use std::sync::Arc;

use crate::{QueueRow, RepeatMode, RunId};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TransportStatus {
    #[default]
    Stopped,
    Resolving,
    Buffering,
    Playing,
    Paused,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PositionDiscontinuity {
    pub run: RunId,
    pub position_millis: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueSummaryView {
    pub revision: u64,
    pub total: usize,
    pub current_index: Option<usize>,
    pub next_index: Option<usize>,
    pub can_next: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurrentMedia {
    pub stream: Option<CurrentStream>,
    pub id: CurrentMediaId,
    pub row: Arc<QueueRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurrentStream {
    pub local: bool,
    pub format: Option<String>,
    pub transcoded: bool,
}

impl std::ops::Deref for CurrentMedia {
    type Target = QueueRow;

    fn deref(&self) -> &Self::Target {
        &self.row
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CurrentMediaId {
    pub run: Option<RunId>,
    pub media_uri: Arc<str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransportView {
    pub current: Option<Arc<CurrentMedia>>,
    pub state: TransportStatus,
    pub desired_playing: bool,
    pub position_millis: u64,
    pub position_observed_at_millis: u64,
    pub playback_rate: f64,
    pub duration_millis: u64,
    pub can_seek: bool,
    pub buffering_percent: Option<u8>,
    pub error: Option<String>,
}

impl TransportView {
    pub fn effective_state(&self) -> TransportStatus {
        effective_transport_state(self.state, self.desired_playing)
    }
}

fn effective_transport_state(state: TransportStatus, desired_playing: bool) -> TransportStatus {
    match state {
        TransportStatus::Stopped | TransportStatus::Failed => state,
        _ if !desired_playing => TransportStatus::Paused,
        TransportStatus::Paused => TransportStatus::Buffering,
        state => state,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ControlsView {
    pub repeat_mode: RepeatMode,
    pub shuffle_enabled: bool,
    pub auto_dj_enabled: bool,
    pub volume: f64,
    pub muted: bool,
    pub audio_output: Option<String>,
    pub playback_output: PlaybackOutput,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RemoteOutputProtocol {
    Upnp,
    GoogleCast,
    PlexCompanion,
    RufinConnect,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RemoteOutput {
    pub id: String,
    pub name: String,
    pub protocol: RemoteOutputProtocol,
}

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub enum PlaybackOutput {
    #[default]
    Local,
    Remote(RemoteOutput),
}

impl PlaybackOutput {
    pub const fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackView {
    pub queue_loading: bool,
    pub queue: QueueSummaryView,
    /// Retained history, current, and upcoming rows in the bounded playback window.
    pub queue_window: Vec<Arc<QueueRow>>,
    pub transport: TransportView,
    pub controls: ControlsView,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlaybackNotice {
    RunStarted(RunId),
    PositionDiscontinuity(crate::PositionDiscontinuity),
    OperationFailed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackProjection {
    pub view: PlaybackView,
    pub notices: Vec<PlaybackNotice>,
}
