use std::sync::Arc;

use crate::sequence::Sequence;
use crate::{OccurrenceId, PlaybackSession, QueueOccurrence, RepeatMode, RunId, TransportStatus};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueSummaryView {
    pub revision: u64,
    pub total: usize,
    pub current_occurrence: Option<OccurrenceId>,
    pub current_index: Option<usize>,
    pub current_position: Option<usize>,
    pub next_occurrence: Option<OccurrenceId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurrentMedia {
    pub id: CurrentMediaId,
    pub occurrence: Arc<QueueOccurrence>,
}

impl std::ops::Deref for CurrentMedia {
    type Target = QueueOccurrence;

    fn deref(&self) -> &Self::Target {
        &self.occurrence
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CurrentMediaId {
    pub run: Option<RunId>,
    pub occurrence: OccurrenceId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransportView {
    pub current: Option<Arc<CurrentMedia>>,
    pub state: TransportStatus,
    pub desired_playing: bool,
    pub position_millis: u64,
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
    pub queue: QueueSummaryView,
    /// Shared bounded Sequence rows until their replacement is durable in Store.
    pub prepared_queue: Option<Vec<Arc<QueueOccurrence>>>,
    pub transport: TransportView,
    pub controls: ControlsView,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlaybackNotice {
    RunStarted(RunId),
    PositionDiscontinuity(crate::PositionDiscontinuity),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackProjection {
    pub view: PlaybackView,
    pub notices: Vec<PlaybackNotice>,
}

impl Sequence {
    pub fn summary(&self) -> QueueSummaryView {
        let next_index = self.next_index_eos();
        QueueSummaryView {
            revision: self.revision(),
            total: self.total(),
            current_occurrence: self.selected().map(|entry| entry.occurrence.clone()),
            current_index: self.selected_index(),
            current_position: self.selected().map(|entry| entry.canonical_position),
            next_occurrence: next_index
                .and_then(|index| self.at(index))
                .map(|entry| entry.occurrence.clone()),
        }
    }
}

impl PlaybackSession {
    pub fn view(&self) -> PlaybackView {
        let sequence = self.sequence();
        let settings = self.settings();
        PlaybackView {
            queue: sequence.summary(),
            prepared_queue: sequence.prepared_window(),
            transport: TransportView {
                current: sequence.selected().map(|entry| {
                    Arc::new(CurrentMedia {
                        id: CurrentMediaId {
                            run: self.current_run(),
                            occurrence: entry.occurrence.clone(),
                        },
                        occurrence: entry.clone(),
                    })
                }),
                state: self.status(),
                desired_playing: self.desired_playing(),
                position_millis: self.position_millis(),
                duration_millis: self.duration_millis(),
                can_seek: self.can_seek(),
                buffering_percent: self.buffering_percent(),
                error: self.last_error().map(str::to_string),
            },
            controls: ControlsView {
                repeat_mode: sequence.repeat_mode(),
                shuffle_enabled: sequence.shuffle_enabled(),
                auto_dj_enabled: self.auto_dj_enabled(),
                volume: if self.output_muted() {
                    0.0
                } else {
                    self.output_volume()
                },
                muted: self.output_muted(),
                audio_output: settings.audio_output.clone(),
                playback_output: self.playback_output().clone(),
            },
        }
    }
}
