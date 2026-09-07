use std::sync::Arc;

use playback::CurrentMediaId;

/// Peaks prepared for one exact current-media publication.
///
/// Rufin owns selection, cancellation, and caching. The UI only compares this
/// identity with the current Playback view before choosing the waveform over
/// the ordinary seekbar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WaveformProjection {
    pub media_id: Option<CurrentMediaId>,
    pub peaks: Option<Arc<Vec<(f64, f64)>>>,
}
