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
        self.put_analysis_copies(&[crate::AnalysisCopy {
            hash,
            loudness: None,
            waveform: None,
            fingerprint: Some(row.clone()),
        }])
        .await
    }
}
