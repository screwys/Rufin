//! Owns persisted source gain and EBU R128 facts and selects one missing analysis unit at a time.
//! Accepted source measurements overwrite Rufin analysis; parsing, scheduling, and tag writing stay outside Library.

use blake3::Hasher;
use sqlx::{Connection, FromRow, Sqlite, Transaction};

use crate::{
    AlbumKey, Database, LibraryError, LibraryResult, ReadCancellation, SourceKey, TrackKey,
};

#[derive(Clone, Debug, PartialEq)]
pub struct LoudnessMeasurement {
    pub analysis_key: [u8; 32],
    pub integrated_lufs: Option<f64>,
    pub true_peak: Option<f64>,
    pub replay_gain_db: Option<f64>,
    pub replay_gain_peak: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackLoudnessWork {
    pub track_key: TrackKey,
    pub track_object_id: String,
    pub expected_analysis_key: [u8; 32],
    pub media_uri: Option<String>,
    pub duration_millis: i64,
    pub cue_path: Option<String>,
    pub cue_start_millis: Option<i64>,
    pub cue_end_millis: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlbumLoudnessTrack {
    pub track_key: TrackKey,
    pub track_object_id: String,
    pub expected_analysis_key: [u8; 32],
    pub media_uri: Option<String>,
    pub duration_millis: i64,
    pub cue_path: Option<String>,
    pub cue_start_millis: Option<i64>,
    pub cue_end_millis: Option<i64>,
}

#[derive(FromRow)]
struct AlbumTrackScalar {
    track_key: TrackKey,
    track_object_id: String,
    expected_analysis_key: Vec<u8>,
    media_uri: Option<String>,
    duration_millis: i64,
    cue_path: Option<String>,
    cue_start_millis: Option<i64>,
    cue_end_millis: Option<i64>,
}

impl TryFrom<AlbumTrackScalar> for AlbumLoudnessTrack {
    type Error = LibraryError;

    fn try_from(value: AlbumTrackScalar) -> Result<Self, Self::Error> {
        Ok(Self {
            track_key: value.track_key,
            track_object_id: value.track_object_id,
            expected_analysis_key: value.expected_analysis_key.try_into().map_err(|_| {
                LibraryError::InvalidStore(
                    "Track loudness analysis key is not 32 bytes".to_string(),
                )
            })?,
            media_uri: value.media_uri,
            duration_millis: value.duration_millis,
            cue_path: value.cue_path,
            cue_start_millis: value.cue_start_millis,
            cue_end_millis: value.cue_end_millis,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlbumLoudnessWork {
    pub album_key: AlbumKey,
    pub expected_analysis_key: [u8; 32],
    pub tracks: Vec<AlbumLoudnessTrack>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct R128TagWrite {
    pub track_key: TrackKey,
    pub track_lufs: f64,
    pub album_lufs: Option<f64>,
}

#[derive(FromRow)]
struct TrackWorkScalar {
    track_key: TrackKey,
    track_object_id: String,
    expected_analysis_key: Vec<u8>,
    media_uri: Option<String>,
    duration_millis: i64,
    cue_path: Option<String>,
    cue_start_millis: Option<i64>,
    cue_end_millis: Option<i64>,
}

impl TryFrom<TrackWorkScalar> for TrackLoudnessWork {
    type Error = LibraryError;
    fn try_from(value: TrackWorkScalar) -> Result<Self, Self::Error> {
        Ok(Self {
            track_key: value.track_key,
            track_object_id: value.track_object_id,
            expected_analysis_key: value.expected_analysis_key.try_into().map_err(|_| {
                LibraryError::InvalidStore(
                    "Track loudness analysis key is not 32 bytes".to_string(),
                )
            })?,
            media_uri: value.media_uri,
            duration_millis: value.duration_millis,
            cue_path: value.cue_path,
            cue_start_millis: value.cue_start_millis,
            cue_end_millis: value.cue_end_millis,
        })
    }
}

#[derive(FromRow)]
struct MeasurementScalar {
    analysis_key: Vec<u8>,
    integrated_lufs: Option<f64>,
    true_peak: Option<f64>,
    replay_gain_db: Option<f64>,
    replay_gain_peak: Option<f64>,
}

impl TryFrom<MeasurementScalar> for LoudnessMeasurement {
    type Error = LibraryError;
    fn try_from(value: MeasurementScalar) -> Result<Self, Self::Error> {
        Ok(Self {
            analysis_key: value.analysis_key.try_into().map_err(|_| {
                LibraryError::InvalidStore("loudness analysis key is not 32 bytes".to_string())
            })?,
            integrated_lufs: value.integrated_lufs,
            true_peak: value.true_peak,
            replay_gain_db: value.replay_gain_db,
            replay_gain_peak: value.replay_gain_peak,
        })
    }
}

impl Database {
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
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, R128TagWrite>("SELECT track.track_key,track_loudness.integrated_lufs track_lufs,album_loudness.integrated_lufs album_lufs FROM tracks track JOIN loudness_measurements track_loudness ON track_loudness.source_key=track.source_key AND track_loudness.entity_kind='track' AND track_loudness.entity_key=track.track_key AND track_loudness.analysis_key=track.loudness_analysis_key AND track_loudness.origin='analysis' LEFT JOIN loudness_measurements album_loudness ON album_loudness.source_key=track.source_key AND album_loudness.entity_kind='album' AND album_loudness.entity_key=track.album_key AND album_loudness.analysis_key=(SELECT album.loudness_analysis_key FROM albums album WHERE album.album_key=track.album_key) WHERE track.source_key=?1 AND track.track_key>?2 ORDER BY track.track_key LIMIT ?3")
            .bind(source)
            .bind(after.map_or(0, TrackKey::raw))
            .bind(limit as i64)
            .fetch_all(&mut *connection)
            .await;
        Database::clear_progress(&mut connection).await?;
        result.map_err(Into::into)
    }

    pub async fn track_loudness(
        &self,
        source: SourceKey,
        track: TrackKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<LoudnessMeasurement>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, MeasurementScalar>("SELECT track.loudness_analysis_key analysis_key,loudness.integrated_lufs,loudness.true_peak,replay.gain_db replay_gain_db,replay.peak replay_gain_peak FROM tracks track LEFT JOIN loudness_measurements loudness ON loudness.source_key=track.source_key AND loudness.entity_kind='track' AND loudness.entity_key=track.track_key AND loudness.analysis_key=track.loudness_analysis_key LEFT JOIN replay_gain_measurements replay ON replay.source_key=track.source_key AND replay.entity_kind='track' AND replay.entity_key=track.track_key AND replay.analysis_key=track.loudness_analysis_key WHERE track.source_key=?1 AND track.track_key=?2 AND (loudness.entity_key IS NOT NULL OR replay.entity_key IS NOT NULL)")
            .bind(source).bind(track).fetch_optional(&mut *connection).await;
        Database::clear_progress(&mut connection).await?;
        result?.map(TryInto::try_into).transpose()
    }

    pub async fn album_loudness(
        &self,
        source: SourceKey,
        album: AlbumKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<LoudnessMeasurement>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, MeasurementScalar>("SELECT album.loudness_analysis_key analysis_key,loudness.integrated_lufs,loudness.true_peak,replay.gain_db replay_gain_db,replay.peak replay_gain_peak FROM albums album LEFT JOIN loudness_measurements loudness ON loudness.source_key=album.source_key AND loudness.entity_kind='album' AND loudness.entity_key=album.album_key AND loudness.analysis_key=album.loudness_analysis_key LEFT JOIN replay_gain_measurements replay ON replay.source_key=album.source_key AND replay.entity_kind='album' AND replay.entity_key=album.album_key AND replay.analysis_key=album.loudness_analysis_key WHERE album.source_key=?1 AND album.album_key=?2 AND (loudness.entity_key IS NOT NULL OR replay.entity_key IS NOT NULL)")
            .bind(source).bind(album).fetch_optional(&mut *connection).await;
        Database::clear_progress(&mut connection).await?;
        result?.map(TryInto::try_into).transpose()
    }

    pub async fn next_missing_track_loudness(
        &self,
        source: SourceKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<TrackLoudnessWork>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, TrackWorkScalar>("SELECT track.track_key,track.object_id track_object_id,track.loudness_analysis_key expected_analysis_key,COALESCE((SELECT access.media_uri FROM local_access_files access WHERE access.source_key=track.source_key AND access.track_object_id=track.object_id ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),track.media_uri) media_uri,track.duration_millis,track.cue_path,track.cue_start_millis,track.cue_end_millis FROM tracks track LEFT JOIN loudness_measurements measurement ON measurement.source_key=track.source_key AND measurement.entity_kind='track' AND measurement.entity_key=track.track_key WHERE track.source_key=?1 AND (measurement.entity_key IS NULL OR measurement.analysis_key<>track.loudness_analysis_key) ORDER BY track.track_key LIMIT 1")
            .bind(source).fetch_optional(&mut *connection).await;
        Database::clear_progress(&mut connection).await?;
        result?.map(TryInto::try_into).transpose()
    }

    pub async fn next_missing_album_loudness(
        &self,
        source: SourceKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<AlbumLoudnessWork>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let album = sqlx::query_as::<_,(AlbumKey,Vec<u8>)>("SELECT album.album_key,album.loudness_analysis_key FROM albums album LEFT JOIN loudness_measurements measurement ON measurement.source_key=album.source_key AND measurement.entity_kind='album' AND measurement.entity_key=album.album_key WHERE album.source_key=?1 AND (measurement.entity_key IS NULL OR measurement.analysis_key<>album.loudness_analysis_key) AND EXISTS (SELECT 1 FROM tracks track WHERE track.album_key=album.album_key) ORDER BY album.album_key LIMIT 1")
            .bind(source).fetch_optional(&mut *transaction).await?;
        let result = if let Some((album_key, expected)) = album {
            let tracks = sqlx::query_as::<_, AlbumTrackScalar>("SELECT track.track_key,track.object_id track_object_id,track.loudness_analysis_key expected_analysis_key,COALESCE((SELECT access.media_uri FROM local_access_files access WHERE access.source_key=track.source_key AND access.track_object_id=track.object_id ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),track.media_uri) media_uri,track.duration_millis,track.cue_path,track.cue_start_millis,track.cue_end_millis FROM tracks track WHERE track.source_key=?1 AND track.album_key=?2 ORDER BY track.disc_number,track.track_number,track.track_key")
                .bind(source).bind(album_key).fetch_all(&mut *transaction).await?;
            Some(AlbumLoudnessWork {
                album_key,
                expected_analysis_key: expected.try_into().map_err(|_| {
                    LibraryError::InvalidStore(
                        "Album loudness analysis key is not 32 bytes".to_string(),
                    )
                })?,
                tracks: tracks
                    .into_iter()
                    .map(TryInto::try_into)
                    .collect::<LibraryResult<_>>()?,
            })
        } else {
            None
        };
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(result)
    }

    pub async fn write_track_source_loudness(
        &self,
        source: SourceKey,
        track: TrackKey,
        measurement: &LoudnessMeasurement,
    ) -> LibraryResult<bool> {
        write_loudness(self, source, "track", track.raw(), measurement, true).await
    }
    pub async fn write_album_source_loudness(
        &self,
        source: SourceKey,
        album: AlbumKey,
        measurement: &LoudnessMeasurement,
    ) -> LibraryResult<bool> {
        write_loudness(self, source, "album", album.raw(), measurement, true).await
    }
    pub async fn write_track_analyzed_loudness(
        &self,
        source: SourceKey,
        track: TrackKey,
        measurement: &LoudnessMeasurement,
    ) -> LibraryResult<bool> {
        write_loudness(self, source, "track", track.raw(), measurement, false).await
    }
    pub async fn write_album_analyzed_loudness(
        &self,
        source: SourceKey,
        album: AlbumKey,
        measurement: &LoudnessMeasurement,
    ) -> LibraryResult<bool> {
        write_loudness(self, source, "album", album.raw(), measurement, false).await
    }
}

async fn write_loudness(
    database: &Database,
    source: SourceKey,
    kind: &'static str,
    key: i64,
    measurement: &LoudnessMeasurement,
    source_fact: bool,
) -> LibraryResult<bool> {
    validate_measurement(measurement)?;
    let mut writer = database.writer().await?;
    let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
    let exists = match kind {
        "track" => {
            sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM tracks WHERE source_key=?1 AND track_key=?2",
            )
            .bind(source)
            .bind(key)
            .fetch_optional(&mut *connection)
            .await?
        }
        "album" => {
            sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM albums WHERE source_key=?1 AND album_key=?2",
            )
            .bind(source)
            .bind(key)
            .fetch_optional(&mut *connection)
            .await?
        }
        _ => None,
    };
    if exists.is_none() {
        return Err(LibraryError::InvalidRequest(
            "loudness entity does not belong to the source".to_string(),
        ));
    }
    let sql = match (source_fact, kind) {
        (true, "track") => {
            "INSERT INTO loudness_measurements(source_key,entity_kind,entity_key,analysis_key,integrated_lufs,true_peak,origin) SELECT ?1,?2,?3,?4,?5,?6,'source' FROM tracks WHERE source_key=?1 AND track_key=?3 AND loudness_analysis_key=?4 ON CONFLICT(source_key,entity_kind,entity_key) DO UPDATE SET analysis_key=excluded.analysis_key,integrated_lufs=excluded.integrated_lufs,true_peak=excluded.true_peak,origin='source'"
        }
        (true, "album") => {
            "INSERT INTO loudness_measurements(source_key,entity_kind,entity_key,analysis_key,integrated_lufs,true_peak,origin) SELECT ?1,?2,?3,?4,?5,?6,'source' FROM albums WHERE source_key=?1 AND album_key=?3 AND loudness_analysis_key=?4 ON CONFLICT(source_key,entity_kind,entity_key) DO UPDATE SET analysis_key=excluded.analysis_key,integrated_lufs=excluded.integrated_lufs,true_peak=excluded.true_peak,origin='source'"
        }
        (false, "track") => {
            "INSERT INTO loudness_measurements(source_key,entity_kind,entity_key,analysis_key,integrated_lufs,true_peak,origin) SELECT ?1,?2,?3,?4,?5,?6,'analysis' FROM tracks WHERE source_key=?1 AND track_key=?3 AND loudness_analysis_key=?4 ON CONFLICT(source_key,entity_kind,entity_key) DO UPDATE SET analysis_key=excluded.analysis_key,integrated_lufs=excluded.integrated_lufs,true_peak=excluded.true_peak WHERE loudness_measurements.origin='analysis'"
        }
        (false, "album") => {
            "INSERT INTO loudness_measurements(source_key,entity_kind,entity_key,analysis_key,integrated_lufs,true_peak,origin) SELECT ?1,?2,?3,?4,?5,?6,'analysis' FROM albums WHERE source_key=?1 AND album_key=?3 AND loudness_analysis_key=?4 ON CONFLICT(source_key,entity_kind,entity_key) DO UPDATE SET analysis_key=excluded.analysis_key,integrated_lufs=excluded.integrated_lufs,true_peak=excluded.true_peak WHERE loudness_measurements.origin='analysis'"
        }
        _ => unreachable!(),
    };
    Ok(sqlx::query(sql)
        .bind(source)
        .bind(kind)
        .bind(key)
        .bind(measurement.analysis_key.as_slice())
        .bind(measurement.integrated_lufs)
        .bind(measurement.true_peak)
        .execute(connection)
        .await?
        .rows_affected()
        == 1)
}

fn validate_measurement(measurement: &LoudnessMeasurement) -> LibraryResult<()> {
    if measurement
        .true_peak
        .is_some_and(|value| !value.is_finite() || value < 0.0)
        || measurement
            .integrated_lufs
            .is_some_and(|value| !value.is_finite())
        || measurement
            .replay_gain_db
            .is_some_and(|value| !value.is_finite())
        || measurement
            .replay_gain_peak
            .is_some_and(|value| !value.is_finite() || value < 0.0)
        || measurement.integrated_lufs.is_none()
    {
        return Err(LibraryError::InvalidRequest(
            "invalid loudness measurement".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn source_track_loudness_key(
    media_uri: Option<&str>,
    source_format: Option<&str>,
    duration_millis: i64,
    cue_path: Option<&str>,
    cue_start_millis: Option<i64>,
    cue_end_millis: Option<i64>,
) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(b"rufin-track-loudness-v1\0");
    hash_optional_text(&mut hasher, media_uri);
    hash_optional_text(&mut hasher, source_format);
    hasher.update(&duration_millis.to_le_bytes());
    hash_optional_text(&mut hasher, cue_path);
    hasher.update(&cue_start_millis.unwrap_or(-1).to_le_bytes());
    hasher.update(&cue_end_millis.unwrap_or(-1).to_le_bytes());
    *hasher.finalize().as_bytes()
}

fn hash_optional_text(hasher: &mut Hasher, value: Option<&str>) {
    let value = value.unwrap_or_default().as_bytes();
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

pub(crate) async fn recompute_album_loudness_key(
    transaction: &mut Transaction<'_, Sqlite>,
    album: AlbumKey,
) -> LibraryResult<[u8; 32]> {
    let keys=sqlx::query_scalar::<_,Vec<u8>>("SELECT loudness_analysis_key FROM tracks WHERE album_key=?1 ORDER BY disc_number,track_number,sort_text,track_key")
        .bind(album).fetch_all(&mut **transaction).await?;
    let mut hasher = Hasher::new();
    hasher.update(b"rufin-album-loudness-v1\0");
    for key in keys {
        hasher.update(&key);
    }
    let key = *hasher.finalize().as_bytes();
    sqlx::query("UPDATE albums SET loudness_analysis_key=?2 WHERE album_key=?1")
        .bind(album)
        .bind(key.as_slice())
        .execute(&mut **transaction)
        .await?;
    Ok(key)
}

pub(crate) async fn recompute_album_source_and_current_keys(
    transaction: &mut Transaction<'_, Sqlite>,
    album: AlbumKey,
) -> LibraryResult<()> {
    let keys=sqlx::query_as::<_,(Vec<u8>,Vec<u8>)>("SELECT source_loudness_analysis_key,loudness_analysis_key FROM tracks WHERE album_key=?1 ORDER BY disc_number,track_number,sort_text,track_key")
        .bind(album).fetch_all(&mut **transaction).await?;
    let mut source = Hasher::new();
    source.update(b"rufin-album-loudness-v1\0");
    let mut current = Hasher::new();
    current.update(b"rufin-album-loudness-v1\0");
    for (source_key, current_key) in keys {
        source.update(&source_key);
        current.update(&current_key);
    }
    let source = *source.finalize().as_bytes();
    let current = *current.finalize().as_bytes();
    sqlx::query("UPDATE albums SET source_loudness_analysis_key=?2,loudness_analysis_key=?3 WHERE album_key=?1")
        .bind(album).bind(source.as_slice()).bind(current.as_slice())
        .execute(&mut **transaction).await?;
    Ok(())
}

pub(crate) async fn initialize_recovered_loudness_keys(
    transaction: &mut Transaction<'_, Sqlite>,
) -> LibraryResult<()> {
    let mut after = 0_i64;
    loop {
        let rows=sqlx::query_as::<_,(TrackKey,Option<String>,Option<String>,i64,Option<String>,Option<i64>,Option<i64>)>("SELECT track_key,media_uri,source_format,duration_millis,cue_path,cue_start_millis,cue_end_millis FROM tracks WHERE track_key>?1 ORDER BY track_key LIMIT 128")
            .bind(after).fetch_all(&mut **transaction).await?;
        if rows.is_empty() {
            break;
        }
        let updates = rows
            .into_iter()
            .map(
                |(track, media_uri, source_format, duration, cue_path, cue_start, cue_end)| {
                    let key = source_track_loudness_key(
                        media_uri.as_deref(),
                        source_format.as_deref(),
                        duration,
                        cue_path.as_deref(),
                        cue_start,
                        cue_end,
                    );
                    (track, key)
                },
            )
            .collect::<Vec<_>>();
        after = updates.last().expect("nonempty loudness page").0.raw();
        let mut query =
            sqlx::QueryBuilder::<Sqlite>::new("WITH updated(track_key,analysis_key) AS (");
        query.push_values(&updates, |mut row, (track, key)| {
            row.push_bind(*track).push_bind(key.as_slice());
        });
        query.push(
            ") UPDATE tracks SET
                 source_loudness_analysis_key=(
                     SELECT analysis_key FROM updated WHERE updated.track_key=tracks.track_key
                 ),
                 loudness_analysis_key=(
                     SELECT analysis_key FROM updated WHERE updated.track_key=tracks.track_key
                 )
             WHERE track_key IN (SELECT track_key FROM updated)",
        );
        query.build().execute(&mut **transaction).await?;
    }
    let mut album_after = 0_i64;
    loop {
        let album = sqlx::query_scalar::<_, AlbumKey>(
            "SELECT album_key FROM albums WHERE album_key>?1 ORDER BY album_key LIMIT 1",
        )
        .bind(album_after)
        .fetch_optional(&mut **transaction)
        .await?;
        let Some(album) = album else {
            break;
        };
        album_after = album.raw();
        let key = recompute_album_loudness_key(transaction, album).await?;
        sqlx::query("UPDATE albums SET source_loudness_analysis_key=?2 WHERE album_key=?1")
            .bind(album)
            .bind(key.as_slice())
            .execute(&mut **transaction)
            .await?;
    }
    Ok(())
}
