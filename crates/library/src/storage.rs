//! Usage and removal of disposable data stored with the library.

use crate::{Catalog, LibraryResult};

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Clone, Copy, Debug, Default)]
pub struct CacheUsage {
    pub entries: u64,
    pub bytes: u64,
}

impl Catalog {
    pub async fn metadata_lookup_cache_usage(&self) -> LibraryResult<CacheUsage> {
        self.cached_table_usage("metadata_lookup_cache").await
    }

    pub async fn clear_metadata_lookup_cache(&self) -> LibraryResult<()> {
        self.clear_cached_table("metadata_lookup_cache").await
    }

    pub async fn fingerprint_cache_usage(&self) -> LibraryResult<CacheUsage> {
        self.cached_table_usage("fingerprint").await
    }

    pub async fn waveform_cache_usage(&self) -> LibraryResult<CacheUsage> {
        self.cached_table_usage("waveform").await
    }

    pub async fn loudness_cache_usage(&self) -> LibraryResult<CacheUsage> {
        let tracks = self.cached_table_usage("loudness").await?;
        let albums = self.cached_table_usage("album_loudness").await?;
        Ok(CacheUsage {
            entries: tracks.entries + albums.entries,
            bytes: tracks.bytes + albums.bytes,
        })
    }

    pub async fn lyrics_cache_usage(&self) -> LibraryResult<CacheUsage> {
        self.cached_table_usage("lyrics_cache").await
    }

    async fn cached_table_usage(&self, table: &str) -> LibraryResult<CacheUsage> {
        let mut reader = self.acquire_reader().await?;
        let (entries, bytes): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT (SELECT count(*) FROM {table}),coalesce(sum(payload),0) FROM dbstat WHERE name=?1"
        ))).bind(table).fetch_one(&mut *reader).await?;
        Ok(CacheUsage {
            entries: entries as u64,
            bytes: bytes as u64,
        })
    }

    pub async fn clear_fingerprint_cache(&self) -> LibraryResult<()> {
        self.clear_cached_table("fingerprint").await
    }

    pub async fn clear_waveform_cache(&self) -> LibraryResult<()> {
        self.clear_cached_table("waveform").await
    }

    pub async fn clear_loudness_cache(&self) -> LibraryResult<()> {
        self.clear_cached_table("loudness").await?;
        self.clear_cached_table("album_loudness").await
    }

    async fn clear_cached_table(&self, table: &str) -> LibraryResult<()> {
        let mut write = self.write().await?;
        if sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table}")))
            .execute(write.connection().await?)
            .await?
            .rows_affected()
            != 0
        {
            write.keep();
        }
        write.commit().await?;
        Ok(())
    }
}
