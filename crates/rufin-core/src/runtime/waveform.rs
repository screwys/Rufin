use std::sync::Arc;

use playback::CurrentMediaId;

/// Peaks prepared for one exact current-media publication.
///
/// Rufin owns selection, cancellation, and caching. The UI only compares this
/// identity with the current Playback view before choosing the waveform over
/// the ordinary seekbar.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct WaveformProjection {
    pub media_id: Option<CurrentMediaId>,
    pub peaks: Option<Arc<Vec<(f64, f64)>>>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl WaveformProjection {
    pub fn media_id(&self) -> Option<CurrentMediaId> {
        self.media_id.clone()
    }
    pub fn amplitudes(&self) -> Vec<f64> {
        self.peaks
            .as_ref()
            .map(|peaks| {
                peaks
                    .iter()
                    .map(|(low, high)| low.abs().max(high.abs()))
                    .collect()
            })
            .unwrap_or_default()
    }
}
