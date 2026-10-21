use crate::{Database, LibraryError, LibraryResult};

impl Database {
    pub async fn fingerprint_cache(
        &self,
        uri: &str,
        key: &[u8; 32],
        version: &str,
    ) -> LibraryResult<Option<(String, u64)>> {
        let mut reader = self.acquire_reader().await?;
        let row: Option<(String, i64)> = sqlx::query_as("SELECT cache.fingerprint,cache.duration_seconds FROM fingerprint_cache cache WHERE cache.media_uri=?1 AND cache.analysis_key=?2 AND cache.version=?3")
            .bind(uri).bind(key.as_slice()).bind(version).fetch_optional(&mut *reader).await?;
        Ok(row.map(|(fingerprint, duration)| (fingerprint, duration as u64)))
    }

    pub async fn write_fingerprint_cache(
        &self,
        uri: &str,
        key: &[u8; 32],
        version: &str,
        fingerprint: &str,
        duration: u64,
    ) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query("INSERT INTO fingerprint_cache(media_uri,analysis_key,version,fingerprint,duration_seconds) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(media_uri) DO UPDATE SET analysis_key=excluded.analysis_key,version=excluded.version,fingerprint=excluded.fingerprint,duration_seconds=excluded.duration_seconds,lookup_key=''")
            .bind(uri).bind(key.as_slice()).bind(version).bind(fingerprint).bind(duration as i64).execute(connection).await?;
        drop(writer);
        self.cache_written();
        Ok(())
    }

    pub async fn preserve_fingerprint(
        &self,
        uri: &str,
        previous: &[u8; 32],
        current: &[u8; 32],
    ) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query(
            "UPDATE fingerprint_cache SET analysis_key=?3 WHERE media_uri=?1 AND analysis_key=?2",
        )
        .bind(uri)
        .bind(previous.as_slice())
        .bind(current.as_slice())
        .execute(connection)
        .await?;
        Ok(())
    }

    pub async fn preserve_audio_analysis(
        &self,
        uri: &str,
        previous: &[u8; 32],
    ) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query("UPDATE waveform_cache SET analysis_key=(SELECT loudness_analysis_key FROM tracks WHERE media_uri=?1) WHERE media_uri=?1 AND analysis_key=?2")
            .bind(uri).bind(previous.as_slice()).execute(&mut *connection).await?;
        sqlx::query("UPDATE loudness_measurements AS measurement SET analysis_key=track.loudness_analysis_key FROM tracks track WHERE track.media_uri=?1 AND measurement.source_key=track.source_key AND measurement.entity_kind='track' AND measurement.entity_key=track.track_key AND measurement.analysis_key=?2")
            .bind(uri).bind(previous.as_slice()).execute(connection).await?;
        Ok(())
    }
}
