//! Usage and removal of disposable data stored with the library.

use crate::{Database, LibraryError, LibraryResult};

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Clone, Copy, Debug, Default)]
pub struct CacheUsage {
    pub entries: u64,
    pub bytes: u64,
}

impl Database {
    pub async fn waveform_cache_usage(&self) -> LibraryResult<CacheUsage> {
        let mut connection = self.acquire_reader().await?;
        let (entries, bytes): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM waveform_cache), coalesce(sum(payload), 0) FROM dbstat WHERE name='waveform_cache'",
        ).fetch_one(&mut *connection).await?;
        Ok(CacheUsage {
            entries: entries as u64,
            bytes: bytes as u64,
        })
    }

    pub async fn clear_waveform_cache(&self) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query("DELETE FROM waveform_cache")
            .execute(connection)
            .await?;
        Ok(())
    }

    pub async fn loudness_cache_usage(&self) -> LibraryResult<CacheUsage> {
        let mut connection = self.acquire_reader().await?;
        let (entries, bytes): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM loudness_measurements), coalesce(sum(payload), 0) FROM dbstat WHERE name='loudness_measurements'",
        )
        .fetch_one(&mut *connection)
        .await?;
        Ok(CacheUsage {
            entries: entries as u64,
            bytes: bytes as u64,
        })
    }

    pub async fn lyrics_cache_usage(&self) -> LibraryResult<CacheUsage> {
        let mut connection = self.acquire_reader().await?;
        let (entries, bytes): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM lyrics_cache), coalesce(sum(payload), 0) FROM dbstat WHERE name='lyrics_cache'",
        )
        .fetch_one(&mut *connection)
        .await?;
        Ok(CacheUsage {
            entries: entries as u64,
            bytes: bytes as u64,
        })
    }

    pub async fn clear_loudness_cache(&self) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query("DELETE FROM loudness_measurements")
            .execute(connection)
            .await?;
        Ok(())
    }
}
