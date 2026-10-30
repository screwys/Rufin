use crate::{Catalog, LibraryResult};

impl Catalog {
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
        sqlx::query("INSERT INTO fingerprint_cache(media_uri,analysis_key,version,fingerprint,duration_seconds) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(media_uri) DO UPDATE SET analysis_key=excluded.analysis_key,version=excluded.version,fingerprint=excluded.fingerprint,duration_seconds=excluded.duration_seconds,lookup_key=''")
            .bind(uri).bind(key.as_slice()).bind(version).bind(fingerprint).bind(duration as i64).execute(&mut *writer).await?;
        Ok(())
    }
}
