//! Completed waveforms are reusable for the same source audio revision.
use crate::{Database, LibraryError, LibraryResult};
use serde::{Deserialize, Serialize};
use sqlx::{Connection, Row, SqliteConnection};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WaveformCacheRow {
    pub media_uri: String,
    pub analysis_key: [u8; 32],
    pub peaks: Vec<(f64, f64)>,
}

impl Database {
    pub async fn clear_source_waveform_cache(&self, source: &crate::SourceId) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query("DELETE FROM waveform_cache WHERE media_uri IN (SELECT track.media_uri FROM tracks track JOIN sources source USING(source_key) WHERE source.object_id=?1)")
            .bind(source.as_str()).execute(connection).await?;
        Ok(())
    }

    pub async fn audio_analysis_key(&self, uri: &str) -> LibraryResult<Option<[u8; 32]>> {
        let mut connection = self.acquire_reader().await?;
        let key: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT loudness_analysis_key FROM tracks WHERE media_uri=?1")
                .bind(uri)
                .fetch_optional(&mut *connection)
                .await?;
        key.map(|key| {
            key.try_into()
                .map_err(|_| LibraryError::InvalidStore("invalid audio analysis key".into()))
        })
        .transpose()
    }
    pub async fn waveform_cache(&self, uri: &str) -> LibraryResult<Option<WaveformCacheRow>> {
        let mut connection = self.acquire_reader().await?;
        current_record_on(&mut connection, uri)
            .await?
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }
    pub async fn write_waveform_cache(&self, row: &WaveformCacheRow) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let changed = write_on(&mut transaction, row).await?;
        transaction.commit().await?;
        drop(writer);
        if changed {
            self.cache_written();
        }
        Ok(changed)
    }
}

pub(crate) async fn current_record_on(
    connection: &mut SqliteConnection,
    uri: &str,
) -> LibraryResult<Option<serde_json::Value>> {
    let row = sqlx::query("SELECT cache.analysis_key,cache.peaks FROM waveform_cache cache JOIN tracks track USING(media_uri) WHERE cache.media_uri=?1 AND cache.analysis_key=track.loudness_analysis_key")
        .bind(uri).fetch_optional(&mut *connection).await?;
    row.map(|row| {
        let key: Vec<u8> = row.get(0);
        Ok(serde_json::to_value(WaveformCacheRow {
            media_uri: uri.to_owned(),
            analysis_key: key
                .try_into()
                .map_err(|_| LibraryError::InvalidStore("invalid waveform analysis key".into()))?,
            peaks: serde_json::from_str(row.get::<&str, _>(1))?,
        })?)
    })
    .transpose()
}

pub(crate) async fn apply_record_on(
    connection: &mut SqliteConnection,
    uri: &str,
    value: Option<&serde_json::Value>,
) -> LibraryResult<bool> {
    let Some(value) = value else {
        return Ok(false);
    };
    let row: WaveformCacheRow = serde_json::from_value(value.clone())?;
    if row.media_uri != uri {
        return Err(LibraryError::InvalidRequest(
            "waveform identity does not match its audio".into(),
        ));
    }
    write_on(connection, &row).await
}

async fn write_on(
    connection: &mut SqliteConnection,
    row: &WaveformCacheRow,
) -> LibraryResult<bool> {
    if row.peaks.is_empty()
        || row.peaks.iter().any(|(left, right)| {
            !left.is_finite()
                || !right.is_finite()
                || !(0.0..=1.0).contains(left)
                || !(0.0..=1.0).contains(right)
        })
    {
        return Err(LibraryError::InvalidRequest(
            "invalid waveform peaks".into(),
        ));
    }
    let changed=sqlx::query("INSERT INTO waveform_cache(media_uri,analysis_key,peaks) SELECT ?1,?2,?3 WHERE EXISTS(SELECT 1 FROM tracks WHERE media_uri=?1 AND loudness_analysis_key=?2) ON CONFLICT(media_uri) DO UPDATE SET analysis_key=excluded.analysis_key,peaks=excluded.peaks WHERE (analysis_key,peaks) IS NOT(excluded.analysis_key,excluded.peaks)")
        .bind(&row.media_uri).bind(row.analysis_key.as_slice()).bind(serde_json::to_string(&row.peaks)?).execute(&mut *connection).await?.rows_affected()>0;
    if changed {
        crate::connect::mark_current_on(connection, "analysis:waveform", &row.media_uri, false)
            .await?;
    }
    Ok(changed)
}
