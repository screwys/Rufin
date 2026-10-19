//! Owns persisted source gain and EBU R128 facts and selects one missing analysis unit at a time.
//! Current source measurements take precedence over Rufin analysis; parsing, scheduling, and tag writing stay outside Library.

use serde::{Deserialize, Serialize};
use sqlx::{Connection, FromRow, SqliteConnection};

use crate::{
    AlbumKey, Database, LibraryError, LibraryResult, NativeGain, ReadCancellation, SourceKey,
    TrackKey,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    pub track_lufs: Option<f64>,
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
        let mut connection = tokio::select! {
            connection = self.acquire_reader() => connection?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        let rows = sqlx::query_as::<_, MeasurementScalar>(sqlx::AssertSqlSafe(format!(
            "WITH owner AS(SELECT track.*,album.source_loudness_analysis_key album_source_token,album.loudness_analysis_key album_token,album.accepted_audio FROM tracks track LEFT JOIN albums album USING(album_key) WHERE track.media_uri=?1),
             native AS(SELECT 'track' entity_kind,track_key entity_key,source_key,loudness_analysis_key analysis_key,CASE WHEN source_loudness_analysis_key=loudness_analysis_key THEN json_extract(metadata,'$.source_loudness') END gain FROM owner
             UNION ALL SELECT 'album',album_key,source_key,album_token,{} FROM owner),
             chosen AS(SELECT native.entity_kind,native.analysis_key,json_extract(native.gain,'$.integrated_lufs') integrated_lufs,json_extract(native.gain,'$.true_peak') true_peak,json_extract(native.gain,'$.replay_gain_db') replay_gain_db,json_extract(native.gain,'$.replay_gain_peak') replay_gain_peak,0 priority FROM native WHERE native.gain IS NOT NULL AND native.gain<>'null'
             UNION ALL SELECT native.entity_kind,native.analysis_key,measurement.integrated_lufs,measurement.true_peak,NULL,NULL,1 FROM native JOIN loudness_measurements measurement ON measurement.source_key=native.source_key AND measurement.entity_kind=native.entity_kind AND measurement.entity_key=native.entity_key AND measurement.analysis_key=native.analysis_key)
             SELECT entity_kind,analysis_key,integrated_lufs,true_peak,replay_gain_db,replay_gain_peak FROM chosen ORDER BY priority",native_album_sql("owner"))))
            .bind(media_uri).fetch_all(&mut *connection).await?;
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
        let native = native_album_sql("owner");
        let result = sqlx::query_as::<_, R128TagWrite>(sqlx::AssertSqlSafe(format!(
            "SELECT track.track_key,track_loudness.integrated_lufs track_lufs,
                coalesce((SELECT json_extract({native},'$.integrated_lufs') FROM
                  (SELECT album.*,source_loudness_analysis_key album_source_token,loudness_analysis_key album_token FROM albums album WHERE album.album_key=track.album_key) owner),
                (SELECT measurement.integrated_lufs FROM loudness_measurements measurement
                 JOIN albums album ON album.album_key=track.album_key
                 WHERE measurement.source_key=track.source_key AND measurement.entity_kind='album'
                   AND measurement.entity_key=album.album_key AND measurement.analysis_key=album.loudness_analysis_key
                 LIMIT 1)) album_lufs
             FROM tracks track JOIN loudness_measurements track_loudness
               ON track_loudness.source_key=track.source_key AND track_loudness.entity_kind='track'
              AND track_loudness.entity_key=track.track_key AND track_loudness.analysis_key=track.loudness_analysis_key
             WHERE track.source_key=?1 AND track.indexed=1 AND track.track_key>?2
               AND (track_loudness.integrated_lufs IS NOT NULL OR album_lufs IS NOT NULL)
               AND NOT(track.source_loudness_analysis_key=track.loudness_analysis_key AND json_extract(track.metadata,'$.source_loudness.integrated_lufs') IS NOT NULL)
             ORDER BY track.track_key LIMIT ?3")))
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
        let result = sqlx::query_as::<_, TrackWorkScalar>("SELECT track.track_key,track.loudness_analysis_key expected_analysis_key,COALESCE((SELECT access.access_uri FROM local_access_files access WHERE access.source_key=track.source_key AND access.media_uri=track.media_uri ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),track.media_uri) media_uri FROM tracks track WHERE track.source_key=?1 AND track.indexed=1 AND track.track_key>?2 AND NOT(track.source_loudness_analysis_key=track.loudness_analysis_key AND json_extract(track.metadata,'$.source_loudness.integrated_lufs') IS NOT NULL) AND NOT EXISTS(SELECT 1 FROM loudness_measurements measurement WHERE measurement.source_key=track.source_key AND measurement.entity_kind='track' AND measurement.entity_key=track.track_key AND measurement.analysis_key=track.loudness_analysis_key) ORDER BY track.track_key LIMIT 1")
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
        let native = native_album_sql("album");
        let album = sqlx::query_as::<_,(AlbumKey,Vec<u8>)>(sqlx::AssertSqlSafe(format!("WITH album AS(SELECT albums.*,source_loudness_analysis_key album_source_token,loudness_analysis_key album_token FROM albums) SELECT album.album_key,album.loudness_analysis_key FROM album WHERE album.source_key=?1 AND album.album_key>?2 AND json_extract({native},'$.integrated_lufs') IS NULL AND NOT EXISTS(SELECT 1 FROM loudness_measurements measurement WHERE measurement.source_key=album.source_key AND measurement.entity_kind='album' AND measurement.entity_key=album.album_key AND measurement.analysis_key=album.loudness_analysis_key) AND EXISTS (SELECT 1 FROM tracks track WHERE track.source_key=album.source_key AND track.album_key=album.album_key AND track.indexed=1) ORDER BY album.album_key LIMIT 1")))
            .bind(source).bind(after.map_or(0, AlbumKey::raw)).fetch_optional(&mut *transaction).await?;
        let result = if let Some((album_key, expected)) = album {
            let tracks = sqlx::query_as::<_, TrackWorkScalar>("SELECT track.track_key,track.loudness_analysis_key expected_analysis_key,COALESCE((SELECT access.access_uri FROM local_access_files access WHERE access.source_key=track.source_key AND access.media_uri=track.media_uri ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),track.media_uri) media_uri FROM tracks track WHERE track.source_key=?1 AND track.album_key=?2 AND track.indexed=1 ORDER BY track.disc_number,track.track_number,track.track_key")
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
    let mut writer = database.writer().await?;
    let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
    let mut transaction = connection.begin().await?;
    let accepted = write_measurement_on(&mut transaction, source, kind, key, measurement).await?;
    transaction.commit().await?;
    drop(writer);
    if accepted {
        database.cache_written();
    }
    Ok(accepted)
}

async fn write_measurement_on(
    connection: &mut SqliteConnection,
    source: SourceKey,
    kind: &str,
    key: i64,
    measurement: &LoudnessMeasurement,
) -> LibraryResult<bool> {
    let input = NativeGain {
        integrated_lufs: measurement.integrated_lufs,
        true_peak: measurement.true_peak,
        replay_gain_db: measurement.replay_gain_db,
        replay_gain_peak: measurement.replay_gain_peak,
    };
    if !valid_measurement(input) {
        return Err(LibraryError::InvalidRequest(
            "invalid loudness measurement".into(),
        ));
    }
    let native = if kind == "track" {
        "CASE WHEN item.source_loudness_analysis_key=item.loudness_analysis_key THEN json_extract(item.metadata,'$.source_loudness') END".to_owned()
    } else {
        native_album_sql("item")
    };
    let accepted = sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(format!(
        "WITH item AS(SELECT {kind}s.*,source_loudness_analysis_key album_source_token,loudness_analysis_key album_token FROM {kind}s WHERE source_key=?1 AND {kind}_key=?2) SELECT item.loudness_analysis_key=?3 AND json_extract({native},'$.integrated_lufs') IS NULL FROM item"
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
        let changed = sqlx::query(
            "INSERT INTO loudness_measurements(source_key,entity_kind,entity_key,analysis_key,integrated_lufs,true_peak)
             VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(source_key,entity_kind,entity_key) DO UPDATE SET
                analysis_key=excluded.analysis_key,integrated_lufs=excluded.integrated_lufs,true_peak=excluded.true_peak
             WHERE (analysis_key,integrated_lufs,true_peak) IS NOT (excluded.analysis_key,excluded.integrated_lufs,excluded.true_peak)")
            .bind(source).bind(kind).bind(key).bind(measurement.analysis_key.as_slice())
            .bind(measurement.integrated_lufs).bind(measurement.true_peak).execute(&mut *connection).await?.rows_affected() > 0;
        if changed {
            let identity: Option<(String,String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT source.object_id,item.object_id FROM {kind}s item JOIN sources source USING(source_key) WHERE item.source_key=?1 AND item.{kind}_key=?2")))
                .bind(source).bind(key).fetch_optional(&mut *connection).await?;
            if let Some((source, object)) = identity {
                crate::connect::mark_current_on(
                    connection,
                    "analysis:loudness",
                    &serde_json::to_string(&(source, kind, object))?,
                    false,
                )
                .await?;
            }
        }
        return Ok(changed);
    }
    Ok(false)
}

pub(crate) fn valid_measurement(input: NativeGain) -> bool {
    (input.integrated_lufs.is_some() || input.true_peak.is_some())
        && input.integrated_lufs.is_none_or(f64::is_finite)
        && input
            .true_peak
            .is_none_or(|value| value.is_finite() && value >= 0.0)
        && input.replay_gain_db.is_none_or(f64::is_finite)
        && input
            .replay_gain_peak
            .is_none_or(|value| value.is_finite() && value >= 0.0)
}

pub(crate) async fn invalidate_album_audio(
    connection: &mut SqliteConnection,
    albums: &[AlbumKey],
    update_source: bool,
) -> LibraryResult<bool> {
    if albums.is_empty() {
        return Ok(false);
    }
    let input = serde_json::to_string(albums)?;
    if update_source {
        sqlx::query("UPDATE albums SET source_loudness_analysis_key=randomblob(32),accepted_audio=NULL WHERE album_key IN(SELECT value FROM json_each(?1))").bind(&input).execute(&mut *connection).await?;
    }
    Ok(sqlx::query("UPDATE albums SET loudness_analysis_key=CASE WHEN EXISTS(SELECT 1 FROM tracks WHERE tracks.album_key=albums.album_key AND indexed=1 AND source_loudness_analysis_key<>loudness_analysis_key) THEN randomblob(32) ELSE source_loudness_analysis_key END WHERE album_key IN(SELECT value FROM json_each(?1))").bind(input).execute(connection).await?.rows_affected()>0)
}

fn native_album_sql(owner: &str) -> String {
    format!(
        "CASE WHEN {owner}.album_source_token={owner}.album_token THEN coalesce((SELECT json_extract(member.metadata,'$.album_loudness') FROM tracks member WHERE member.album_key={owner}.album_key AND member.indexed=1 AND json_type(member.metadata,'$.album_loudness')='object' AND(json_extract(member.metadata,'$.album_loudness.integrated_lufs') IS NOT NULL OR json_extract(member.metadata,'$.album_loudness.replay_gain_db') IS NOT NULL) ORDER BY coalesce(member.cue_path,member.source_path),member.disc_number,member.track_number,member.object_id LIMIT 1),CASE WHEN json_extract({owner}.accepted_audio,'$.token')=hex({owner}.album_source_token) THEN json_extract({owner}.accepted_audio,'$.gain') END) END"
    )
}

pub(crate) async fn current_record_on(
    connection: &mut SqliteConnection,
    key: &str,
) -> LibraryResult<Option<serde_json::Value>> {
    let (source, kind, object): (String, String, String) = serde_json::from_str(key)?;
    if !matches!(kind.as_str(), "track" | "album") {
        return Ok(None);
    }
    let measurement = sqlx::query_as::<_,MeasurementScalar>(sqlx::AssertSqlSafe(format!("SELECT measurement.entity_kind,measurement.analysis_key,measurement.integrated_lufs,measurement.true_peak,NULL replay_gain_db,NULL replay_gain_peak FROM loudness_measurements measurement JOIN {kind}s item ON item.source_key=measurement.source_key AND item.{kind}_key=measurement.entity_key JOIN sources source ON source.source_key=item.source_key WHERE source.object_id=?1 AND item.object_id=?2 AND measurement.entity_kind=?3 AND measurement.analysis_key=item.loudness_analysis_key")))
        .bind(source).bind(object).bind(kind).fetch_optional(&mut *connection).await?;
    measurement
        .map(LoudnessMeasurement::try_from)
        .transpose()?
        .map(serde_json::to_value)
        .transpose()
        .map_err(Into::into)
}

pub(crate) async fn apply_record_on(
    connection: &mut SqliteConnection,
    key: &str,
    value: Option<&serde_json::Value>,
) -> LibraryResult<bool> {
    let Some(value) = value else {
        return Ok(false);
    };
    let (source, kind, object): (String, String, String) = serde_json::from_str(key)?;
    if !matches!(kind.as_str(), "track" | "album") {
        return Ok(false);
    }
    let target: Option<(SourceKey,i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT item.source_key,item.{kind}_key FROM {kind}s item JOIN sources source USING(source_key) WHERE source.object_id=?1 AND item.object_id=?2")))
        .bind(source).bind(object).fetch_optional(&mut *connection).await?;
    let Some((source, entity)) = target else {
        return Ok(false);
    };
    write_measurement_on(
        connection,
        source,
        &kind,
        entity,
        &serde_json::from_value(value.clone())?,
    )
    .await
}
