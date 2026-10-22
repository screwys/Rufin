use crate::{Database, LibraryError, LibraryResult};
use sqlx::Connection;

impl Database {
    pub async fn metadata_lookup_cache(
        &self,
        key: &str,
        now: i64,
    ) -> LibraryResult<Option<Vec<u8>>> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(
            "SELECT payload FROM metadata_lookup_cache WHERE key=?1 AND expires_at>?2",
        )
        .bind(key)
        .bind(now)
        .fetch_optional(&mut *reader)
        .await?)
    }

    pub async fn write_metadata_lookup_cache(
        &self,
        key: &str,
        payload: &[u8],
        now: i64,
    ) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        sqlx::query("DELETE FROM metadata_lookup_cache WHERE expires_at<=?1")
            .bind(now)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("INSERT INTO metadata_lookup_cache(key,payload,written_at,expires_at) VALUES(?1,?2,?3,?4) ON CONFLICT(key) DO UPDATE SET payload=excluded.payload,written_at=excluded.written_at,expires_at=excluded.expires_at")
            .bind(key).bind(payload).bind(now).bind(now + 7 * 86400).execute(&mut *transaction).await?;
        sqlx::query("DELETE FROM metadata_lookup_cache WHERE key IN (SELECT key FROM (SELECT key,row_number() OVER(ORDER BY written_at DESC,key DESC) AS position,sum(length(payload)) OVER(ORDER BY written_at DESC,key DESC) AS bytes FROM metadata_lookup_cache) WHERE position>10000 OR bytes>67108864)")
            .execute(&mut *transaction).await?;
        transaction.commit().await?;
        drop(writer);
        self.cache_written();
        Ok(())
    }

    pub async fn link_fingerprint_lookup(&self, uri: &str, key: &str) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        sqlx::query(
            "UPDATE fingerprint_cache SET lookup_key=?2 WHERE media_uri=?1 AND lookup_key<>?2",
        )
        .bind(uri)
        .bind(key)
        .execute(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
        .await?;
        Ok(())
    }

    pub async fn cached_fingerprint_lookups(
        &self,
        uris: &[String],
        now: i64,
    ) -> LibraryResult<Vec<String>> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT fingerprint.media_uri FROM fingerprint_cache fingerprint JOIN metadata_lookup_cache lookup ON lookup.key=fingerprint.lookup_key WHERE lookup.expires_at>?2 AND fingerprint.media_uri IN (SELECT value FROM json_each(?1))")
            .bind(serde_json::to_string(uris)?).bind(now).fetch_all(&mut *reader).await?)
    }
}
