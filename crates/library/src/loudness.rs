//! EBU R128 analysis results, keyed by the audio they measured, and the work that misses them.
//! Source gains in track metadata take precedence; parsing, scheduling and tag writing stay
//! outside Library.

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::catalog::families::Query;
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
        self.put_analysis(
            hash,
            sqlx::query("UPDATE track_details SET audio_hash=?3 WHERE track_key=?1 AND audio_revision=?2 AND audio_hash IS NOT ?3")
                .bind(track)
                .bind(revision.as_slice())
                .bind(hash.as_slice()),
        )
        .await
    }

    pub async fn put_loudness(
        &self,
        hash: [u8; 32],
        value: &LoudnessMeasurement,
    ) -> LibraryResult<bool> {
        valid(value)?;
        self.put_analysis(
            hash,
            sqlx::query("INSERT INTO loudness(audio_hash,integrated_lufs,true_peak) VALUES(?1,?2,?3)
                 ON CONFLICT(audio_hash) DO UPDATE SET integrated_lufs=excluded.integrated_lufs,true_peak=excluded.true_peak
                 WHERE (integrated_lufs,true_peak) IS NOT (excluded.integrated_lufs,excluded.true_peak)")
                .bind(hash.as_slice())
                .bind(value.integrated_lufs)
                .bind(value.true_peak),
        )
        .await
    }

    /// Stores an album's loudness under its current members; nothing while a member's hash is
    /// unknown.
    pub async fn put_album_loudness(
        &self,
        album: AlbumKey,
        value: &LoudnessMeasurement,
    ) -> LibraryResult<bool> {
        valid(value)?;
        let members = album_members_sql("?1");
        let mut write = self.write().await?;
        let changed: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "INSERT INTO album_loudness(members,integrated_lufs,true_peak) SELECT members,?2,?3 FROM (SELECT {members} members) WHERE members IS NOT NULL
             ON CONFLICT(members) DO UPDATE SET integrated_lufs=excluded.integrated_lufs,true_peak=excluded.true_peak
             WHERE (integrated_lufs,true_peak) IS NOT (excluded.integrated_lufs,excluded.true_peak) RETURNING members"
        )))
        .bind(album)
        .bind(value.integrated_lufs)
        .bind(value.true_peak)
        .fetch_optional(write.connection().await?)
        .await?;
        if let Some(members) = &changed {
            write.changed(Scope::Analysis {
                hash: *blake3::hash(members.as_bytes()).as_bytes(),
            });
        }
        write.commit().await?;
        Ok(changed.is_some())
    }

    /// One analysis statement in a Write; a changed row raises `Scope::Analysis`.
    pub(crate) async fn put_analysis(
        &self,
        hash: [u8; 32],
        query: Query<'_>,
    ) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let changed = query
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
