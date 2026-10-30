use crate::{Catalog, LibraryResult};

impl Catalog {
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
        let mut write = self.write().await?;
        let transaction = write.connection().await?;
        sqlx::query("DELETE FROM metadata_lookup_cache WHERE expires_at<=?1")
            .bind(now)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("INSERT INTO metadata_lookup_cache(key,payload,written_at,expires_at) VALUES(?1,?2,?3,?4) ON CONFLICT(key) DO UPDATE SET payload=excluded.payload,written_at=excluded.written_at,expires_at=excluded.expires_at")
            .bind(key).bind(payload).bind(now).bind(now + 7 * 86400).execute(&mut *transaction).await?;
        sqlx::query("DELETE FROM metadata_lookup_cache WHERE key IN (SELECT key FROM (SELECT key,row_number() OVER(ORDER BY written_at DESC,key DESC) AS position,sum(length(payload)) OVER(ORDER BY written_at DESC,key DESC) AS bytes FROM metadata_lookup_cache) WHERE position>10000 OR bytes>67108864)")
            .execute(&mut *transaction).await?;
        write.keep();
        write.commit().await?;
        Ok(())
    }

    pub async fn link_fingerprint_lookup(&self, uri: &str, key: &str) -> LibraryResult<()> {
        let mut write = self.write().await?;
        if sqlx::query(
            "UPDATE fingerprint SET lookup_key=?2 WHERE audio_hash=(SELECT details.audio_hash FROM tracks JOIN track_details details USING(track_key) WHERE media_uri=?1) AND lookup_key<>?2",
        )
        .bind(uri)
        .bind(key)
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

    pub async fn cached_fingerprint_lookups(
        &self,
        uris: &[String],
        now: i64,
    ) -> LibraryResult<Vec<String>> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT track.media_uri FROM tracks track JOIN track_details details USING(track_key) JOIN fingerprint ON fingerprint.audio_hash=details.audio_hash JOIN metadata_lookup_cache lookup ON lookup.key=fingerprint.lookup_key WHERE lookup.expires_at>?2 AND track.media_uri IN (SELECT value FROM json_each(?1))")
            .bind(serde_json::to_string(uris)?).bind(now).fetch_all(&mut *reader).await?)
    }
}
