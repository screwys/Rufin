//! Owns persisted source gain and EBU R128 facts and selects one missing analysis unit at a time.
//! Current source measurements take precedence over Rufin analysis; parsing, scheduling, and tag writing stay outside Library.

use blake3::Hasher;
use futures_util::TryStreamExt;
use sqlx::{Connection, FromRow, SqliteConnection};

use crate::{
    AlbumKey, Database, LibraryError, LibraryResult, LoudnessInput, ReadCancellation, SourceKey,
    TrackKey,
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
    pub expected_analysis_key: [u8; 32],
    pub media_uri: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlbumLoudnessWork {
    pub album_key: AlbumKey,
    pub expected_analysis_key: [u8; 32],
    pub tracks: Vec<TrackLoudnessWork>,
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
    expected_analysis_key: Vec<u8>,
    media_uri: String,
}

impl TryFrom<TrackWorkScalar> for TrackLoudnessWork {
    type Error = LibraryError;
    fn try_from(value: TrackWorkScalar) -> Result<Self, Self::Error> {
        Ok(Self {
            track_key: value.track_key,
            expected_analysis_key: value.expected_analysis_key.try_into().map_err(|_| {
                LibraryError::InvalidStore(
                    "Track loudness analysis key is not 32 bytes".to_string(),
                )
            })?,
            media_uri: value.media_uri,
        })
    }
}

#[derive(FromRow)]
struct MeasurementScalar {
    entity_kind: String,
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
    pub async fn playback_loudness(
        &self,
        media_uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(Option<LoudnessMeasurement>, Option<LoudnessMeasurement>)> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let rows = sqlx::query_as::<_, MeasurementScalar>(
            "WITH owners AS (
                SELECT 'track' entity_kind,track_key entity_key,source_key,loudness_analysis_key
                FROM tracks WHERE media_uri=?1
                UNION ALL
                SELECT 'album',album.album_key,album.source_key,album.loudness_analysis_key
                FROM tracks track JOIN albums album USING(album_key) WHERE track.media_uri=?1
             ) SELECT owner.entity_kind,owner.loudness_analysis_key analysis_key,
                    loudness.integrated_lufs,loudness.true_peak,replay.gain_db replay_gain_db,
                    replay.peak replay_gain_peak
             FROM owners owner LEFT JOIN loudness_measurements loudness
               ON loudness.source_key=owner.source_key AND loudness.entity_kind=owner.entity_kind
              AND loudness.entity_key=owner.entity_key AND loudness.analysis_key=owner.loudness_analysis_key
             LEFT JOIN replay_gain_measurements replay
               ON replay.source_key=owner.source_key AND replay.entity_kind=owner.entity_kind
              AND replay.entity_key=owner.entity_key AND replay.analysis_key=owner.loudness_analysis_key
             WHERE loudness.entity_key IS NOT NULL OR replay.entity_key IS NOT NULL
             ORDER BY loudness.origin='source' DESC",
        ).bind(media_uri).fetch_all(&mut *connection).await?;
        let (mut track, mut album) = (None, None);
        for row in rows {
            let selected = if row.entity_kind == "track" {
                &mut track
            } else {
                &mut album
            };
            if selected.is_none() {
                *selected = Some(row.try_into()?);
            }
        }
        Ok((track, album))
    }

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
        let result = sqlx::query_as::<_, R128TagWrite>(
            "SELECT track.track_key,track_loudness.integrated_lufs track_lufs,
                (SELECT measurement.integrated_lufs FROM loudness_measurements measurement
                 JOIN albums album ON album.album_key=track.album_key
                 WHERE measurement.source_key=track.source_key AND measurement.entity_kind='album'
                   AND measurement.entity_key=album.album_key AND measurement.analysis_key=album.loudness_analysis_key
                 ORDER BY measurement.origin='source' DESC LIMIT 1) album_lufs
             FROM tracks track JOIN loudness_measurements track_loudness
               ON track_loudness.source_key=track.source_key AND track_loudness.entity_kind='track'
              AND track_loudness.entity_key=track.track_key AND track_loudness.analysis_key=track.loudness_analysis_key
              AND track_loudness.origin='analysis'
             WHERE track.source_key=?1 AND track.track_key>?2 AND NOT EXISTS(
                SELECT 1 FROM loudness_measurements source WHERE source.source_key=track.source_key
                  AND source.entity_kind='track' AND source.entity_key=track.track_key
                  AND source.analysis_key=track.loudness_analysis_key AND source.origin='source')
             ORDER BY track.track_key LIMIT ?3")
            .bind(source)
            .bind(after.map_or(0, TrackKey::raw))
            .bind(limit as i64)
            .fetch_all(&mut *connection)
            .await;
        result.map_err(Into::into)
    }

    pub async fn next_missing_track_loudness(
        &self,
        source: SourceKey,
        after: Option<TrackKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<TrackLoudnessWork>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, TrackWorkScalar>("SELECT track.track_key,track.loudness_analysis_key expected_analysis_key,COALESCE((SELECT access.access_uri FROM local_access_files access WHERE access.source_key=track.source_key AND access.media_uri=track.media_uri ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),track.media_uri) media_uri FROM tracks track WHERE track.source_key=?1 AND track.track_key>?2 AND NOT EXISTS(SELECT 1 FROM loudness_measurements measurement WHERE measurement.source_key=track.source_key AND measurement.entity_kind='track' AND measurement.entity_key=track.track_key AND measurement.analysis_key=track.loudness_analysis_key) ORDER BY track.track_key LIMIT 1")
            .bind(source).bind(after.map_or(0, TrackKey::raw)).fetch_optional(&mut *connection).await;
        result?.map(TryInto::try_into).transpose()
    }

    pub async fn next_missing_album_loudness(
        &self,
        source: SourceKey,
        after: Option<AlbumKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<AlbumLoudnessWork>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let album = sqlx::query_as::<_,(AlbumKey,Vec<u8>)>("SELECT album.album_key,album.loudness_analysis_key FROM albums album WHERE album.source_key=?1 AND album.album_key>?2 AND NOT EXISTS(SELECT 1 FROM loudness_measurements measurement WHERE measurement.source_key=album.source_key AND measurement.entity_kind='album' AND measurement.entity_key=album.album_key AND measurement.analysis_key=album.loudness_analysis_key) AND EXISTS (SELECT 1 FROM tracks track WHERE track.source_key=album.source_key AND track.album_key=album.album_key) ORDER BY album.album_key LIMIT 1")
            .bind(source).bind(after.map_or(0, AlbumKey::raw)).fetch_optional(&mut *transaction).await?;
        let result = if let Some((album_key, expected)) = album {
            let tracks = sqlx::query_as::<_, TrackWorkScalar>("SELECT track.track_key,track.loudness_analysis_key expected_analysis_key,COALESCE((SELECT access.access_uri FROM local_access_files access WHERE access.source_key=track.source_key AND access.media_uri=track.media_uri ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),track.media_uri) media_uri FROM tracks track WHERE track.source_key=?1 AND track.album_key=?2 ORDER BY track.disc_number,track.track_number,track.track_key")
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
        Ok(result)
    }

    pub async fn write_track_analyzed_loudness(
        &self,
        source: SourceKey,
        track: TrackKey,
        measurement: &LoudnessMeasurement,
    ) -> LibraryResult<bool> {
        write_analyzed_loudness(self, source, "track", track.raw(), measurement).await
    }
    pub async fn write_album_analyzed_loudness(
        &self,
        source: SourceKey,
        album: AlbumKey,
        measurement: &LoudnessMeasurement,
    ) -> LibraryResult<bool> {
        write_analyzed_loudness(self, source, "album", album.raw(), measurement).await
    }
}

async fn write_analyzed_loudness(
    database: &Database,
    source: SourceKey,
    kind: &'static str,
    key: i64,
    measurement: &LoudnessMeasurement,
) -> LibraryResult<bool> {
    let input = LoudnessInput {
        integrated_lufs: measurement.integrated_lufs,
        true_peak: measurement.true_peak,
        replay_gain_db: measurement.replay_gain_db,
        replay_gain_peak: measurement.replay_gain_peak,
    };
    if measurement.integrated_lufs.is_none() || !valid_measurement(input) {
        return Err(LibraryError::InvalidRequest(
            "invalid loudness measurement".into(),
        ));
    }
    let mut writer = database.writer().await?;
    let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
    let accepted = sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(format!(
        "SELECT item.loudness_analysis_key=?3 AND NOT EXISTS(
            SELECT 1 FROM loudness_measurements measurement WHERE measurement.source_key=?1
              AND measurement.entity_kind='{kind}' AND measurement.entity_key=?2
              AND measurement.analysis_key=?3 AND measurement.origin='source')
         FROM {kind}s item WHERE item.source_key=?1 AND item.{kind}_key=?2"
    )))
    .bind(source)
    .bind(key)
    .bind(measurement.analysis_key.as_slice())
    .fetch_optional(&mut *connection)
    .await?
    .ok_or_else(|| {
        LibraryError::InvalidRequest("loudness entity does not belong to the source".into())
    })?;
    if accepted {
        sqlx::query(
            "INSERT INTO loudness_measurements(source_key,entity_kind,entity_key,analysis_key,integrated_lufs,true_peak,origin)
             VALUES(?1,?2,?3,?4,?5,?6,'analysis') ON CONFLICT(source_key,entity_kind,entity_key,origin) DO UPDATE SET
                analysis_key=excluded.analysis_key,integrated_lufs=excluded.integrated_lufs,true_peak=excluded.true_peak
             WHERE (analysis_key,integrated_lufs,true_peak) IS NOT (excluded.analysis_key,excluded.integrated_lufs,excluded.true_peak)")
            .bind(source).bind(kind).bind(key).bind(measurement.analysis_key.as_slice())
            .bind(measurement.integrated_lufs).bind(measurement.true_peak).execute(connection).await?;
    }
    Ok(accepted)
}

pub(crate) fn valid_measurement(input: LoudnessInput) -> bool {
    (input.integrated_lufs.is_some() || input.replay_gain_db.is_some())
        && input.integrated_lufs.is_none_or(f64::is_finite)
        && input
            .true_peak
            .is_none_or(|value| value.is_finite() && value >= 0.0)
        && input.replay_gain_db.is_none_or(f64::is_finite)
        && input
            .replay_gain_peak
            .is_none_or(|value| value.is_finite() && value >= 0.0)
}

pub(crate) async fn recompute_album_loudness_key(
    connection: &mut SqliteConnection,
    album: AlbumKey,
    update_source: bool,
) -> LibraryResult<bool> {
    let mut source_hasher = Hasher::new();
    source_hasher.update(b"rufin-album-loudness-v1\0");
    let mut current_hasher = source_hasher.clone();
    let mut rows = sqlx::query_as::<_, (Vec<u8>, Vec<u8>)>(
        "SELECT track.source_loudness_analysis_key,track.loudness_analysis_key
         FROM tracks track JOIN albums album USING(album_key)
         WHERE album.album_key=?1 AND track.source_key=album.source_key AND (NOT ?2 OR track.indexed)
         ORDER BY track.disc_number,track.track_number,track.sort_text,track.object_id")
        .bind(album).bind(update_source).fetch(&mut *connection);
    while let Some((source_key, current_key)) = rows.try_next().await? {
        if update_source {
            source_hasher.update(&source_key);
        }
        current_hasher.update(&current_key);
    }
    drop(rows);
    let source_key = source_hasher.finalize();
    let current_key = current_hasher.finalize();
    Ok(sqlx::query(
        "UPDATE albums SET source_loudness_analysis_key=CASE WHEN ?2 THEN ?3 ELSE source_loudness_analysis_key END,
             loudness_analysis_key=?4 WHERE album_key=?1
         AND (source_loudness_analysis_key,loudness_analysis_key) IS NOT
             (CASE WHEN ?2 THEN ?3 ELSE source_loudness_analysis_key END,?4)")
        .bind(album).bind(update_source).bind(source_key.as_bytes().as_slice())
        .bind(current_key.as_bytes().as_slice()).execute(connection).await?.rows_affected()>0)
}
