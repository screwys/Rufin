//! AcoustID fingerprints, keyed by the audio they were computed from.
use crate::{Catalog, LibraryResult};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FingerprintRow {
    pub version: String,
    pub fingerprint: String,
    pub duration_seconds: u64,
}

impl Catalog {
    pub async fn fingerprint(&self, hash: [u8; 32]) -> LibraryResult<Option<FingerprintRow>> {
        let mut reader = self.acquire_reader().await?;
        let row: Option<(String, String, i64)> = sqlx::query_as(
            "SELECT version,fingerprint,duration_seconds FROM fingerprint WHERE audio_hash=?1",
        )
        .bind(hash.as_slice())
        .fetch_optional(&mut *reader)
        .await?;
        Ok(row.map(|(version, fingerprint, duration)| FingerprintRow {
            version,
            fingerprint,
            duration_seconds: duration.max(0) as u64,
        }))
    }

    /// A new fingerprint drops the AcoustID lookup the old one linked.
    pub async fn put_fingerprint(
        &self,
        hash: [u8; 32],
        row: &FingerprintRow,
    ) -> LibraryResult<bool> {
        self.put_analysis(
            hash,
            sqlx::query("INSERT INTO fingerprint(audio_hash,version,fingerprint,duration_seconds) VALUES(?1,?2,?3,?4)
                 ON CONFLICT(audio_hash) DO UPDATE SET version=excluded.version,fingerprint=excluded.fingerprint,duration_seconds=excluded.duration_seconds,lookup_key=''
                 WHERE (version,fingerprint,duration_seconds) IS NOT (excluded.version,excluded.fingerprint,excluded.duration_seconds)")
                .bind(hash.as_slice())
                .bind(&row.version)
                .bind(&row.fingerprint)
                .bind(row.duration_seconds as i64),
        )
        .await
    }
}
