use std::sync::Arc;

use crate::{QueueRow, RepeatMode, RunId};

pub type QueuePosition = usize;
#[cfg(feature = "bindings")]
uniffi::custom_type!(QueuePosition, u64, {
    remote,
    lower: |value| value as u64,
    try_lift: |value| Ok(usize::try_from(value)?),
});

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
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
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PositionDiscontinuity {
    pub run: RunId,
    pub position_millis: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QueueSummaryView {
    pub revision: u64,
    pub total: QueuePosition,
    pub current_index: Option<QueuePosition>,
    pub next_index: Option<QueuePosition>,
    pub can_next: bool,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CurrentMedia {
    pub stream: Option<CurrentStream>,
    pub id: CurrentMediaId,
    pub row: Arc<QueueRow>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CurrentStream {
    pub local: bool,
    pub format: Option<String>,
    pub transcoded: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CurrentMediaId {
    pub run: Option<RunId>,
    pub media_uri: String,
}

#[derive(Clone, Debug, PartialEq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TransportView {
    pub current: Option<CurrentMedia>,
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

    /// Wanted playback waits while a stream refills. Starts without a refill level do not count.
    pub fn refilling(&self) -> bool {
        self.effective_state() == TransportStatus::Buffering && self.buffering_percent.is_some()
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

#[derive(Clone, Debug, PartialEq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
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
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum RemoteOutputProtocol {
    Upnp,
    GoogleCast,
    PlexCompanion,
    RufinConnect,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RemoteOutput {
    pub id: String,
    pub name: String,
    pub protocol: RemoteOutputProtocol,
}

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
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
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
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

impl PlaybackProjection {
    /// Live state for observers, including playback intent and the latest seek.
    pub fn reporting_transport(&self) -> TransportView {
        let mut transport = self.view.transport.clone();
        transport.state = transport.effective_state();
        let run = transport.current.as_ref().and_then(|media| media.id.run);
        if let Some(position) = self.notices.iter().rev().find_map(|notice| match notice {
            PlaybackNotice::PositionDiscontinuity(seek) if Some(seek.run) == run => {
                Some(seek.position_millis)
            }
            _ => None,
        }) {
            transport.position_millis = position;
        }
        transport
    }
}
