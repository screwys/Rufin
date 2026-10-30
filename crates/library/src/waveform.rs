//! Seekbar waveforms, keyed by the audio they were drawn from.
use crate::{Catalog, LibraryError, LibraryResult};

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
        if peaks.is_empty()
            || peaks
                .iter()
                .any(|(left, right)| !(0.0..=1.0).contains(left) || !(0.0..=1.0).contains(right))
        {
            return Err(LibraryError::InvalidRequest(
                "invalid waveform peaks".into(),
            ));
        }
        self.put_analysis(
            hash,
            sqlx::query("INSERT INTO waveform(audio_hash,peaks) VALUES(?1,?2) ON CONFLICT(audio_hash) DO UPDATE SET peaks=excluded.peaks WHERE peaks IS NOT excluded.peaks")
                .bind(hash.as_slice())
                .bind(serde_json::to_string(peaks)?),
        )
        .await
    }
}
