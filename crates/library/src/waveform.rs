//! Seekbar waveforms, keyed by the audio they were drawn from.
use crate::{Catalog, LibraryResult};

impl Catalog {
    pub async fn waveform(&self, hash: [u8; 32]) -> LibraryResult<Option<Vec<(f64, f64)>>> {
        let mut connection = self.acquire_reader().await?;
        let peaks: Option<String> =
            sqlx::query_scalar("SELECT peaks FROM waveform WHERE audio_hash=?1")
                .bind(hash.as_slice())
                .fetch_optional(&mut *connection)
                .await?;
        Ok(peaks
            .map(|peaks| serde_json::from_str(&peaks))
            .transpose()?)
    }

    pub async fn put_waveform(&self, hash: [u8; 32], peaks: &[(f64, f64)]) -> LibraryResult<bool> {
        self.put_analysis_copies(&[crate::AnalysisCopy {
            hash,
            loudness: None,
            waveform: Some(peaks.to_vec()),
            fingerprint: None,
        }])
        .await
    }
}
