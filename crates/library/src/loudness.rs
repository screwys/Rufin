//! EBU R128 analysis results, keyed by the audio they measured, and the work that misses them.
//! Source gains in track metadata take precedence; parsing, scheduling and tag writing stay
//! outside Library.

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::{
    AlbumKey, Catalog, LibraryError, LibraryResult, ReadCancellation, Scope, SourceKey, TrackKey,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoudnessMeasurement {
    pub integrated_lufs: Option<f64>,
    pub true_peak: Option<f64>,
    pub replay_gain_db: Option<f64>,
    pub replay_gain_peak: Option<f64>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct R128TagWrite {
    pub track_key: TrackKey,
    pub track_lufs: Option<f64>,
    pub album_lufs: Option<f64>,
}

/// The key of an album's analysed loudness: the 16-hex-character prefixes of its indexed
/// members' audio hashes in hash order, NULL while one is unknown.
pub(crate) fn album_members_sql(album: &str) -> String {
    format!(
        "(SELECT CASE WHEN count(*)=count(analysis_details.audio_hash) THEN group_concat(substr(hex(analysis_details.audio_hash),1,16),'' ORDER BY analysis_details.audio_hash) END
          FROM tracks analysis_member JOIN track_details analysis_details USING(track_key)
          WHERE analysis_member.album_key={album} AND analysis_member.indexed=1)"
    )
}

impl Catalog {
    /// Tracks whose stored analysis Rufin may write as R128 tags: measured, without a source gain.
    pub async fn r128_tag_write_page(
        &self,
        source: SourceKey,
        after: Option<TrackKey>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<R128TagWrite>> {
        if limit == 0 || limit > 128 {
            return Err(LibraryError::InvalidRequest(
                "R128 tag write page limit must be between 1 and 128".to_string(),
            ));
        }
        let members = album_members_sql("track.album_key");
        let mut connection = self.acquire_cancellable(cancellation).await?;
        let result = sqlx::query_as::<_, R128TagWrite>(sqlx::AssertSqlSafe(format!(
            "SELECT track.track_key,measured.integrated_lufs track_lufs,
                coalesce((SELECT json_extract(member_details.metadata,'$.album_loudness.integrated_lufs') FROM tracks member JOIN track_details member_details USING(track_key)
                    WHERE member.album_key=track.album_key AND member.indexed=1 AND json_extract(member_details.metadata,'$.album_loudness.integrated_lufs') IS NOT NULL LIMIT 1),
                  (SELECT integrated_lufs FROM album_loudness WHERE members={members})) album_lufs
             FROM tracks track JOIN track_details details USING(track_key) JOIN loudness measured ON measured.audio_hash=details.audio_hash
             WHERE track.source_key=?1 AND track.indexed=1 AND track.track_key>?2
               AND json_extract(details.metadata,'$.source_loudness.integrated_lufs') IS NULL
               AND (track_lufs IS NOT NULL OR album_lufs IS NOT NULL)
             ORDER BY track.track_key LIMIT ?3"
        )))
        .bind(source)
        .bind(after.map_or(0, TrackKey::raw))
        .bind(limit as i64)
        .fetch_all(&mut *connection)
        .await;
        result.map_err(Into::into)
    }

    /// Indexed tracks without a source gain whose audio hash is unknown or unmeasured.
    pub async fn loudness_work(
        &self,
        source: SourceKey,
        after: Option<TrackKey>,
        limit: usize,
    ) -> LibraryResult<Vec<TrackKey>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(
            "SELECT track.track_key FROM tracks track JOIN track_details details USING(track_key)
             WHERE track.source_key=?1 AND track.indexed=1 AND track.track_key>?2
               AND json_extract(details.metadata,'$.source_loudness.integrated_lufs') IS NULL
               AND NOT EXISTS(SELECT 1 FROM loudness WHERE audio_hash=details.audio_hash)
             ORDER BY track.track_key LIMIT ?3",
        )
        .bind(source)
        .bind(after.map_or(0, TrackKey::raw))
        .bind(limit as i64)
        .fetch_all(&mut *connection)
        .await?)
    }

    /// Albums without a source album gain whose members' loudness is unmeasured, with their
    /// indexed tracks in album order.
    pub async fn album_loudness_work(
        &self,
        source: SourceKey,
        after: Option<AlbumKey>,
        limit: usize,
    ) -> LibraryResult<Vec<(AlbumKey, Vec<TrackKey>)>> {
        let members = album_members_sql("album.album_key");
        let mut connection = self.acquire_reader().await?;
        let rows = sqlx::query_as::<_, (AlbumKey, String)>(sqlx::AssertSqlSafe(format!(
            "SELECT album.album_key,(SELECT json_group_array(track_key) FROM (SELECT member.track_key FROM tracks member
                 WHERE member.album_key=album.album_key AND member.indexed=1 ORDER BY member.disc_number,member.track_number,member.track_key))
             FROM albums album
             WHERE album.source_key=?1 AND album.album_key>?2
               AND EXISTS(SELECT 1 FROM tracks track WHERE track.album_key=album.album_key AND track.indexed=1)
               AND NOT EXISTS(SELECT 1 FROM tracks member JOIN track_details member_details USING(track_key)
                   WHERE member.album_key=album.album_key AND member.indexed=1 AND json_extract(member_details.metadata,'$.album_loudness.integrated_lufs') IS NOT NULL)
               AND NOT EXISTS(SELECT 1 FROM album_loudness WHERE members={members})
             ORDER BY album.album_key LIMIT ?3"
        )))
        .bind(source)
        .bind(after.map_or(0, AlbumKey::raw))
        .bind(limit as i64)
        .fetch_all(&mut *connection)
        .await?;
        rows.into_iter()
            .map(|(album, tracks)| Ok((album, serde_json::from_str(&tracks)?)))
            .collect()
    }

    /// Records the hash of a track's audio while its revision is the one that was hashed.
    pub async fn put_audio_hash(
        &self,
        track: TrackKey,
        revision: [u8; 32],
        hash: [u8; 32],
    ) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let changed = sqlx::query("UPDATE track_details SET audio_hash=?3 WHERE track_key=?1 AND audio_revision=?2 AND audio_hash IS NOT ?3")
            .bind(track)
            .bind(revision.as_slice())
            .bind(hash.as_slice())
            .execute(write.connection().await?)
            .await?
            .rows_affected()
            > 0;
        if changed {
            write.changed(Scope::Analysis { hash });
        }
        write.commit().await?;
        Ok(changed)
    }

    pub async fn put_loudness(
        &self,
        hash: [u8; 32],
        value: &LoudnessMeasurement,
    ) -> LibraryResult<bool> {
        self.put_analysis_copies(&[AnalysisCopy {
            hash,
            loudness: Some((value.integrated_lufs, value.true_peak)),
            waveform: None,
            fingerprint: None,
        }])
        .await
    }

    /// Stores an album's loudness under the audio hashes of the members it measured, keyed as
    /// `album_members_sql` keys them.
    pub async fn put_album_loudness(
        &self,
        members: &[[u8; 32]],
        value: &LoudnessMeasurement,
    ) -> LibraryResult<bool> {
        let mut members = members.to_vec();
        members.sort_unstable();
        let key = members
            .iter()
            .flat_map(|hash| &hash[..8])
            .map(|byte| format!("{byte:02X}"));
        let mut write = self.write().await?;
        let row = (key.collect(), value.integrated_lufs, value.true_peak);
        let changed = write_album_loudness(&mut write, &[row]).await?;
        write.commit().await?;
        Ok(changed)
    }
}

/// What analysis holds for one audio hash.
#[derive(Clone, Debug, PartialEq)]
pub struct AnalysisCopy {
    pub hash: [u8; 32],
    /// Integrated loudness and true peak.
    pub loudness: Option<(Option<f64>, Option<f64>)>,
    pub waveform: Option<Vec<(f64, f64)>>,
    pub fingerprint: Option<crate::FingerprintRow>,
}

impl Catalog {
    /// The audio hashes analysis holds any result for.
    pub async fn analysis_hashes(&self) -> LibraryResult<Vec<[u8; 32]>> {
        let mut connection = self.acquire_reader().await?;
        let hashes: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT audio_hash FROM loudness UNION SELECT audio_hash FROM waveform UNION SELECT audio_hash FROM fingerprint",
        )
        .fetch_all(&mut *connection)
        .await?;
        Ok(hashes
            .into_iter()
            .filter_map(|hash| hash.try_into().ok())
            .collect())
    }

    /// The analysis results of these audio hashes; a hash without any is left out.
    pub async fn analysis(&self, hashes: &[[u8; 32]]) -> LibraryResult<Vec<AnalysisCopy>> {
        let mut connection = self.acquire_reader().await?;
        let hex = hashes
            .iter()
            .map(|hash| blake3::Hash::from_bytes(*hash).to_hex().to_string())
            .collect::<Vec<_>>();
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            Vec<u8>,
            Option<bool>,
            Option<f64>,
            Option<f64>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
        )> = sqlx::query_as(
            "SELECT requested.hash,loudness.audio_hash IS NOT NULL,loudness.integrated_lufs,loudness.true_peak,waveform.peaks,
                 fingerprint.version,fingerprint.fingerprint,fingerprint.duration_seconds
             FROM (SELECT DISTINCT unhex(value) hash FROM json_each(?1)) requested
             LEFT JOIN loudness ON loudness.audio_hash=requested.hash
             LEFT JOIN waveform ON waveform.audio_hash=requested.hash
             LEFT JOIN fingerprint ON fingerprint.audio_hash=requested.hash
             WHERE loudness.audio_hash IS NOT NULL OR waveform.audio_hash IS NOT NULL OR fingerprint.audio_hash IS NOT NULL
             ORDER BY requested.hash",
        )
        .bind(serde_json::to_string(&hex)?)
        .fetch_all(&mut *connection)
        .await?;
        let mut copies = Vec::with_capacity(rows.len());
        for (hash, measured, lufs, peak, peaks, version, fingerprint, duration) in rows {
            let Ok(hash) = hash.try_into() else {
                continue;
            };
            copies.push(AnalysisCopy {
                hash,
                loudness: (measured == Some(true)).then_some((lufs, peak)),
                waveform: peaks
                    .map(|peaks| serde_json::from_str(&peaks))
                    .transpose()?,
                fingerprint: version.zip(fingerprint).map(|(version, fingerprint)| {
                    crate::FingerprintRow {
                        version,
                        fingerprint,
                        duration_seconds: duration.unwrap_or_default().max(0) as u64,
                    }
                }),
            });
        }
        Ok(copies)
    }

    /// Writes analysis results, measured here or by another installation, in one Write: one
    /// statement per kind that has any. Whether a result changed.
    pub async fn put_analysis_copies(&self, copies: &[AnalysisCopy]) -> LibraryResult<bool> {
        let hex = |hash: &[u8; 32]| blake3::Hash::from_bytes(*hash).to_hex().to_string();
        let mut loudness = Vec::new();
        let mut waveforms = Vec::new();
        let mut fingerprints = Vec::new();
        for copy in copies {
            if let Some((lufs, peak)) = copy.loudness {
                valid(&LoudnessMeasurement {
                    integrated_lufs: lufs,
                    true_peak: peak,
                    replay_gain_db: None,
                    replay_gain_peak: None,
                })?;
                loudness.push((hex(&copy.hash), lufs, peak));
            }
            if let Some(peaks) = &copy.waveform {
                if peaks.is_empty()
                    || peaks.iter().any(|(left, right)| {
                        !(0.0..=1.0).contains(left) || !(0.0..=1.0).contains(right)
                    })
                {
                    return Err(LibraryError::InvalidRequest(
                        "invalid waveform peaks".into(),
                    ));
                }
                waveforms.push((hex(&copy.hash), serde_json::to_string(peaks)?));
            }
            if let Some(row) = &copy.fingerprint {
                fingerprints.push((
                    hex(&copy.hash),
                    &row.version,
                    &row.fingerprint,
                    row.duration_seconds as i64,
                ));
            }
        }
        let mut write = self.write().await?;
        let mut changed: Vec<Vec<u8>> = Vec::new();
        if !loudness.is_empty() {
            changed.extend(sqlx::query_scalar::<_, Vec<u8>>(
                "INSERT INTO loudness(audio_hash,integrated_lufs,true_peak) SELECT unhex(value->>0),value->>1,value->>2 FROM json_each(?1) WHERE true
                 ON CONFLICT(audio_hash) DO UPDATE SET integrated_lufs=excluded.integrated_lufs,true_peak=excluded.true_peak
                 WHERE (integrated_lufs,true_peak) IS NOT (excluded.integrated_lufs,excluded.true_peak) RETURNING audio_hash",
            )
            .bind(serde_json::to_string(&loudness)?)
            .fetch_all(write.connection().await?)
            .await?);
        }
        if !waveforms.is_empty() {
            changed.extend(
                sqlx::query_scalar::<_, Vec<u8>>(
                    "INSERT INTO waveform(audio_hash,peaks) SELECT unhex(value->>0),value->>1 FROM json_each(?1) WHERE true
                     ON CONFLICT(audio_hash) DO UPDATE SET peaks=excluded.peaks WHERE peaks IS NOT excluded.peaks RETURNING audio_hash",
                )
                .bind(serde_json::to_string(&waveforms)?)
                .fetch_all(write.connection().await?)
                .await?,
            );
        }
        // A new fingerprint drops the AcoustID lookup the old one linked.
        if !fingerprints.is_empty() {
            changed.extend(
                sqlx::query_scalar::<_, Vec<u8>>(
                    "INSERT INTO fingerprint(audio_hash,version,fingerprint,duration_seconds) SELECT unhex(value->>0),value->>1,value->>2,value->>3 FROM json_each(?1) WHERE true
                     ON CONFLICT(audio_hash) DO UPDATE SET version=excluded.version,fingerprint=excluded.fingerprint,duration_seconds=excluded.duration_seconds,lookup_key=''
                     WHERE (version,fingerprint,duration_seconds) IS NOT (excluded.version,excluded.fingerprint,excluded.duration_seconds) RETURNING audio_hash",
                )
                .bind(serde_json::to_string(&fingerprints)?)
                .fetch_all(write.connection().await?)
                .await?,
            );
        }
        let any = !changed.is_empty();
        for hash in changed {
            if let Ok(hash) = hash.try_into() {
                write.changed(Scope::Analysis { hash });
            }
        }
        write.commit().await?;
        Ok(any)
    }

    /// Every stored album loudness by its members key: integrated loudness and true peak.
    pub async fn album_loudness_rows(
        &self,
    ) -> LibraryResult<Vec<(String, Option<f64>, Option<f64>)>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as(
            "SELECT members,integrated_lufs,true_peak FROM album_loudness ORDER BY members",
        )
        .fetch_all(&mut *connection)
        .await?)
    }

    /// Writes album loudness by members key, in one Write.
    pub async fn put_album_loudness_rows(
        &self,
        rows: &[(String, Option<f64>, Option<f64>)],
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        write_album_loudness(&mut write, rows).await?;
        write.commit().await?;
        Ok(())
    }
}

/// The one album loudness statement, by members key; a changed row raises `Scope::Analysis`.
/// Whether one changed.
async fn write_album_loudness(
    write: &mut crate::catalog::write::Write<'_>,
    rows: &[(String, Option<f64>, Option<f64>)],
) -> LibraryResult<bool> {
    for (_, lufs, peak) in rows {
        valid(&LoudnessMeasurement {
            integrated_lufs: *lufs,
            true_peak: *peak,
            replay_gain_db: None,
            replay_gain_peak: None,
        })?;
    }
    let changed: Vec<String> = sqlx::query_scalar(
        "INSERT INTO album_loudness(members,integrated_lufs,true_peak) SELECT value->>0,value->>1,value->>2 FROM json_each(?1) WHERE true
         ON CONFLICT(members) DO UPDATE SET integrated_lufs=excluded.integrated_lufs,true_peak=excluded.true_peak
         WHERE (integrated_lufs,true_peak) IS NOT (excluded.integrated_lufs,excluded.true_peak) RETURNING members",
    )
    .bind(serde_json::to_string(rows)?)
    .fetch_all(write.connection().await?)
    .await?;
    for members in &changed {
        write.changed(Scope::Analysis {
            hash: *blake3::hash(members.as_bytes()).as_bytes(),
        });
    }
    Ok(!changed.is_empty())
}

fn valid(value: &LoudnessMeasurement) -> LibraryResult<()> {
    let finite = |value: Option<f64>| value.is_none_or(f64::is_finite);
    let peak = |value: Option<f64>| value.is_none_or(|value| value.is_finite() && value >= 0.0);
    if (value.integrated_lufs.is_some() || value.true_peak.is_some())
        && finite(value.integrated_lufs)
        && peak(value.true_peak)
        && finite(value.replay_gain_db)
        && peak(value.replay_gain_peak)
    {
        Ok(())
    } else {
        Err(LibraryError::InvalidRequest(
            "invalid loudness measurement".into(),
        ))
    }
}
