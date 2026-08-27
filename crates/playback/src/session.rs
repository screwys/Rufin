use std::collections::HashMap;
use std::sync::Arc;

use library::{SourceKey, TrackKey};

use crate::{
    BackendCommand, BackendEvent, BackendState, Batch, BatchItem, ListeningFact, ListeningTrack,
    NextTransition, OccurrenceId, Placement, PlaybackMedia, PlaybackOutput, PlaybackSettings,
    PlaybackTransitionMode, PreparedNext, PreparedStream, Provenance, RepeatMode, RunEndReason,
    RunId, Sequence, SequenceEntry, SequenceError, StreamRequest, manual_end_is_skip,
    qualified_play_threshold_millis,
};

const AUTO_DJ_HISTORY_LIMIT: usize = 10;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MaterializationId(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializationReservation {
    pub id: MaterializationId,
    pub source_id: SourceKey,
    pub current_track_id: Option<TrackKey>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClockSample {
    pub monotonic_millis: u64,
    pub unix_seconds: i64,
    pub local_period: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TransportStatus {
    #[default]
    Stopped,
    Resolving,
    Buffering,
    Playing,
    Paused,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceSessionEpoch(u64);

impl SourceSessionEpoch {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceReportPhase {
    Started,
    Progress,
    QualifiedPlay,
    Ended,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceReportFact {
    pub run: RunId,
    pub source_id: SourceKey,
    pub track_id: Option<TrackKey>,
    pub track_object_id: String,
    pub phase: SourceReportPhase,
    pub started_at_unix_seconds: i64,
    pub position_millis: u64,
    pub paused: bool,
    pub muted: bool,
    pub volume: f64,
    pub shuffle: bool,
    pub repeat_mode: RepeatMode,
    pub failed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PositionDiscontinuity {
    pub run: RunId,
    pub position_millis: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoDjRequest {
    pub source_id: SourceKey,
    pub seed_occurrence: OccurrenceId,
    pub seed_track_id: TrackKey,
    pub requested_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionEffect {
    ResolveMedia {
        source_key: SourceKey,
        occurrence: OccurrenceId,
        prepared: bool,
    },
    ResolveStream {
        run: RunId,
        source_id: SourceKey,
        occurrence: OccurrenceId,
        media: Box<PlaybackMedia>,
        request: StreamRequest,
    },
    Backend(BackendCommand),
    PersistProgress {
        source_id: SourceKey,
        revision: u64,
        occurrence: Option<OccurrenceId>,
        progress_millis: u64,
    },
    PersistState {
        source_id: SourceKey,
        revision: u64,
        occurrence: Option<OccurrenceId>,
        progress_millis: u64,
    },
    PersistOutputState {
        volume: f64,
        muted: bool,
        audio_output: Option<String>,
    },
    FlushPersistence {
        source_id: SourceKey,
    },
    Listening(ListeningFact),
    Activity(crate::ActivityListen),
    SourceReport(SourceReportFact),
    CurrentMediaChanged,
    PositionDiscontinuity(PositionDiscontinuity),
    RequestAutoDj(AutoDjRequest),
    Visualizer {
        run: RunId,
        levels: Vec<f64>,
    },
    NonfatalError(String),
    FatalError(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionCommand {
    Activate(OccurrenceId),
    Remove(OccurrenceId),
    Reorder {
        occurrence: OccurrenceId,
        target: crate::QueueReorderTarget,
    },
    MoveAfterCurrent(OccurrenceId),
    ClearUpcoming,
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    Seek(u64),
    SetVolume(f64),
    SetMuted(bool),
    PersistOutputState,
    SetRepeat(RepeatMode),
    SetShuffle {
        enabled: bool,
        seed: u64,
    },
    SetAutoDj {
        enabled: bool,
        refill_threshold: usize,
    },
    UpdateSettings(PlaybackSettings),
    SetVisualizerEnabled(bool),
    MediaResolved {
        occurrence: OccurrenceId,
        media: Box<Option<PlaybackMedia>>,
        prepared: bool,
        start_run: bool,
    },
    StreamInputsChanged,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionUpdate {
    pub effects: Vec<SessionEffect>,
    pub view_changed: bool,
    pub queue_changed: bool,
    pub(crate) queue_persistence_changed: bool,
}

impl SessionUpdate {
    fn changed() -> Self {
        Self {
            view_changed: true,
            ..Self::default()
        }
    }

    fn structural() -> Self {
        Self {
            view_changed: true,
            queue_changed: true,
            queue_persistence_changed: true,
            ..Self::default()
        }
    }

    fn traversal() -> Self {
        Self {
            view_changed: true,
            queue_persistence_changed: true,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug)]
struct RunContext {
    id: RunId,
    play_id: String,
    occurrence: OccurrenceId,
    track: ListeningTrack,
    status: TransportStatus,
    duration_millis: u64,
    audible_millis: u64,
    started_at_unix_seconds: Option<i64>,
    local_period: Option<String>,
    last_monotonic_millis: Option<u64>,
    qualified: bool,
    last_progress_bucket: Option<u64>,
    desired_playing: bool,
    resolved_stream: Option<PreparedStream>,
    backend_loaded: bool,
    seekable: bool,
}

impl RunContext {
    fn resolving(
        id: RunId,
        play_id: String,
        source_key: SourceKey,
        entry: &SequenceEntry,
        media: &PlaybackMedia,
    ) -> Self {
        let track = ListeningTrack::capture(source_key, media);
        Self {
            id,
            play_id,
            occurrence: entry.occurrence.clone(),
            duration_millis: track.duration_millis,
            track,
            status: TransportStatus::Resolving,
            audible_millis: 0,
            started_at_unix_seconds: None,
            local_period: None,
            last_monotonic_millis: None,
            qualified: false,
            last_progress_bucket: None,
            desired_playing: true,
            resolved_stream: None,
            backend_loaded: false,
            seekable: true,
        }
    }

    fn advance_clock(&mut self, monotonic_millis: u64) {
        if self.status == TransportStatus::Playing
            && let Some(previous) = self.last_monotonic_millis
        {
            self.audible_millis = self
                .audible_millis
                .saturating_add(monotonic_millis.saturating_sub(previous));
        }
        self.last_monotonic_millis = Some(monotonic_millis);
    }
}

#[derive(Clone, Debug)]
enum NextResolution {
    Resolving,
    Ready(PreparedStream),
}

#[derive(Clone, Debug)]
struct NextPlan {
    current_run: RunId,
    next_run: RunId,
    occurrence: OccurrenceId,
    request: StreamRequest,
    transition: NextTransition,
    resolution: NextResolution,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AutoDjKey {
    source_id: SourceKey,
    seed_occurrence: OccurrenceId,
}

#[derive(Clone, Debug)]
pub(crate) struct PlaybackSession {
    sequence: Sequence,
    source_session_epoch: SourceSessionEpoch,
    play_id_prefix: Arc<str>,
    current_run: Option<RunContext>,
    next_plan: Option<NextPlan>,
    current_media: Option<(OccurrenceId, PlaybackMedia)>,
    prepared_media: Option<(OccurrenceId, PlaybackMedia)>,
    restored_paused: bool,
    next_run_number: u64,
    next_materialization_number: u64,
    pending_replacement: Option<MaterializationId>,
    pending_additive: HashMap<MaterializationId, Placement>,
    settings: PlaybackSettings,
    playback_output: PlaybackOutput,
    output_volume: f64,
    output_muted: bool,
    auto_dj_enabled: bool,
    auto_dj_refill_threshold: usize,
    auto_dj_in_flight: Option<AutoDjKey>,
    auto_dj_waiting_for_continuation: bool,
    buffering_percent: Option<u8>,
    last_error: Option<String>,
}

impl PlaybackSession {
    pub fn new(
        sequence: Sequence,
        source_session_epoch: SourceSessionEpoch,
        play_id_prefix: impl Into<Arc<str>>,
        mut settings: PlaybackSettings,
        playback_output: PlaybackOutput,
        auto_dj_enabled: bool,
        auto_dj_refill_threshold: usize,
    ) -> Self {
        settings.sanitize();
        let output_volume = settings.volume;
        let output_muted = settings.muted;
        Self {
            sequence,
            source_session_epoch,
            play_id_prefix: play_id_prefix.into(),
            current_run: None,
            next_plan: None,
            current_media: None,
            prepared_media: None,
            restored_paused: false,
            next_run_number: 1,
            next_materialization_number: 1,
            pending_replacement: None,
            pending_additive: HashMap::new(),
            settings,
            playback_output,
            output_volume,
            output_muted,
            auto_dj_enabled,
            auto_dj_refill_threshold: auto_dj_refill_threshold.max(1),
            auto_dj_in_flight: None,
            auto_dj_waiting_for_continuation: false,
            buffering_percent: None,
            last_error: None,
        }
    }

    pub fn sequence(&self) -> &Sequence {
        &self.sequence
    }

    pub(crate) fn current_media_fact(&self) -> Option<(&OccurrenceId, &PlaybackMedia)> {
        self.current_media
            .as_ref()
            .map(|(occurrence, media)| (occurrence, media))
    }

    pub fn playback_output(&self) -> &PlaybackOutput {
        &self.playback_output
    }

    pub fn output_volume(&self) -> f64 {
        self.output_volume
    }

    pub fn output_muted(&self) -> bool {
        self.output_muted
    }

    pub const fn source_session_epoch(&self) -> SourceSessionEpoch {
        self.source_session_epoch
    }

    pub fn status(&self) -> TransportStatus {
        self.current_run
            .as_ref()
            .map(|run| {
                if run.status == TransportStatus::Resolving && !run.desired_playing {
                    TransportStatus::Paused
                } else {
                    run.status
                }
            })
            .unwrap_or_else(|| {
                if self.restored_paused {
                    TransportStatus::Paused
                } else if self.last_error.is_some() {
                    TransportStatus::Failed
                } else {
                    TransportStatus::Stopped
                }
            })
    }

    pub fn desired_playing(&self) -> bool {
        self.current_run
            .as_ref()
            .is_some_and(|run| run.desired_playing)
    }

    pub fn current_run(&self) -> Option<RunId> {
        self.current_run.as_ref().map(|run| run.id)
    }

    pub fn can_seek(&self) -> bool {
        match self.current_run.as_ref() {
            Some(run) => run.seekable,
            None => self.duration_millis() > 0,
        }
    }

    pub fn position_millis(&self) -> u64 {
        self.sequence.progress_millis()
    }

    pub fn duration_millis(&self) -> u64 {
        self.current_run
            .as_ref()
            .map(|run| run.duration_millis)
            .or_else(|| {
                self.current_media
                    .as_ref()
                    .map(|(_, media)| media.duration_millis.max(0) as u64)
            })
            .unwrap_or_default()
    }

    pub fn buffering_percent(&self) -> Option<u8> {
        self.buffering_percent
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn settings(&self) -> &PlaybackSettings {
        &self.settings
    }

    pub(crate) fn replace_output(&mut self, output: PlaybackOutput) -> SessionUpdate {
        self.playback_output = output;
        self.last_error = None;
        if self.playback_output.is_local() {
            self.output_volume = self.settings.volume;
            self.output_muted = self.settings.muted;
        }
        let Some(current) = self.current_run.as_mut() else {
            return SessionUpdate::changed();
        };
        let Some(stream) = current.resolved_stream.clone() else {
            return SessionUpdate::changed();
        };
        let run = current.id;
        let desired_playing = current.desired_playing;
        current.backend_loaded = false;
        if !desired_playing {
            current.status = TransportStatus::Paused;
            return SessionUpdate::changed();
        }
        current.status = TransportStatus::Buffering;
        current.backend_loaded = true;
        let next = self.prepared_next(run);
        let mut effects = Vec::new();
        if self.playback_output.is_local() {
            effects.push(SessionEffect::Backend(BackendCommand::ConfigureAudio(
                self.settings.clone().into(),
            )));
        }
        effects.push(SessionEffect::Backend(BackendCommand::Start {
            run,
            current: stream,
            next,
            start_position_millis: self.sequence.progress_millis(),
            playback_rate: self.settings.playback_rate,
        }));
        SessionUpdate {
            effects,
            view_changed: true,
            ..SessionUpdate::default()
        }
    }

    pub fn auto_dj_enabled(&self) -> bool {
        self.auto_dj_enabled
    }

    pub fn reserve_materialization(&mut self, placement: Placement) -> MaterializationReservation {
        let id = MaterializationId(self.next_materialization_number);
        self.next_materialization_number = self.next_materialization_number.wrapping_add(1).max(1);
        match placement {
            Placement::Replace { .. } => {
                self.pending_additive.clear();
                self.pending_replacement = Some(id);
                self.auto_dj_in_flight = None;
                self.auto_dj_waiting_for_continuation = false;
            }
            Placement::AfterCurrent | Placement::End => {
                self.pending_additive.insert(id, placement);
            }
        }
        MaterializationReservation {
            id,
            source_id: self.sequence.source_key(),
            current_track_id: self.sequence.selected().and_then(|entry| entry.track_key),
        }
    }

    pub fn apply_materialization(
        &mut self,
        id: MaterializationId,
        source_id: &SourceKey,
        batch: Batch,
        placement: Placement,
        anchor: Option<PlaybackMedia>,
        sample: &ClockSample,
    ) -> Result<Option<SessionUpdate>, SequenceError> {
        if self.sequence.source_key() != *source_id {
            return Ok(None);
        }
        let accepted = match placement {
            Placement::Replace { .. } => self.pending_replacement == Some(id),
            Placement::AfterCurrent | Placement::End => {
                self.pending_additive.get(&id) == Some(&placement)
            }
        };
        if !accepted {
            return Ok(None);
        }
        if matches!(placement, Placement::Replace { .. }) {
            self.pending_replacement = None;
        } else {
            self.pending_additive.remove(&id);
        }
        self.apply_batch(batch, placement, anchor, sample).map(Some)
    }

    pub fn fail_materialization(
        &mut self,
        id: MaterializationId,
        source_id: &SourceKey,
        placement: Placement,
        message: String,
    ) -> Option<SessionUpdate> {
        self.cancel_materialization(id, source_id, placement)
            .then(|| SessionUpdate {
                effects: vec![SessionEffect::NonfatalError(message)],
                ..SessionUpdate::default()
            })
    }

    pub fn cancel_materialization(
        &mut self,
        id: MaterializationId,
        source_id: &SourceKey,
        placement: Placement,
    ) -> bool {
        if self.sequence.source_key() != *source_id {
            return false;
        }
        match placement {
            Placement::Replace { .. } if self.pending_replacement == Some(id) => {
                self.pending_replacement = None;
                true
            }
            Placement::AfterCurrent | Placement::End
                if self.pending_additive.get(&id) == Some(&placement) =>
            {
                self.pending_additive.remove(&id);
                true
            }
            _ => false,
        }
    }

    pub fn handle_command(
        &mut self,
        command: SessionCommand,
        sample: &ClockSample,
    ) -> Result<SessionUpdate, SequenceError> {
        match command {
            SessionCommand::Activate(occurrence) => Ok(self.activate(&occurrence, sample)),
            SessionCommand::Remove(occurrence) => Ok(self.remove(&occurrence, sample)),
            SessionCommand::Reorder { occurrence, target } => {
                Ok(self.reorder(&occurrence, &target))
            }
            SessionCommand::MoveAfterCurrent(occurrence) => {
                Ok(self.move_after_current(&occurrence))
            }
            SessionCommand::ClearUpcoming => Ok(self.clear_upcoming()),
            SessionCommand::PlayPause => Ok(self.play_pause()),
            SessionCommand::Play => Ok(self.set_playing(true)),
            SessionCommand::Pause => Ok(self.set_playing(false)),
            SessionCommand::Stop => Ok(self.stop(sample)),
            SessionCommand::Next => Ok(self.next(sample)),
            SessionCommand::Previous => Ok(self.previous(sample)),
            SessionCommand::Seek(position_millis) => Ok(self.seek(position_millis)),
            SessionCommand::SetVolume(volume) => Ok(self.set_volume(volume)),
            SessionCommand::SetMuted(muted) => Ok(self.set_muted(muted)),
            SessionCommand::PersistOutputState => Ok(SessionUpdate {
                effects: self
                    .playback_output
                    .is_local()
                    .then(|| SessionEffect::PersistOutputState {
                        volume: self.settings.volume,
                        muted: self.settings.muted,
                        audio_output: self.settings.audio_output.clone(),
                    })
                    .into_iter()
                    .collect(),
                ..SessionUpdate::default()
            }),
            SessionCommand::SetRepeat(repeat) => Ok(self.set_repeat(repeat)),
            SessionCommand::SetShuffle { enabled, seed } => Ok(self.set_shuffle(enabled, seed)),
            SessionCommand::SetAutoDj {
                enabled,
                refill_threshold,
            } => Ok(self.set_auto_dj(enabled, refill_threshold)),
            SessionCommand::UpdateSettings(settings) => Ok(self.update_settings(settings)),
            SessionCommand::SetVisualizerEnabled(enabled) => Ok(SessionUpdate {
                effects: vec![SessionEffect::Backend(
                    BackendCommand::SetVisualizerEnabled(enabled),
                )],
                ..SessionUpdate::default()
            }),
            SessionCommand::MediaResolved {
                occurrence,
                media,
                prepared,
                start_run,
            } => Ok(self.media_resolved(occurrence, *media, prepared, start_run)),
            SessionCommand::StreamInputsChanged => Ok(self.stream_inputs_changed()),
        }
    }

    pub fn stream_resolved(
        &mut self,
        run: RunId,
        stream: impl Into<PreparedStream>,
    ) -> SessionUpdate {
        let stream = stream.into();
        if self.current_run.as_ref().is_some_and(|current| {
            current.id == run && current.status == TransportStatus::Resolving
        }) {
            return self.current_stream_resolved(run, stream);
        }
        let Some(plan) = self.next_plan.as_mut().filter(|plan| {
            plan.next_run == run
                && self
                    .current_run
                    .as_ref()
                    .is_some_and(|current| current.id == plan.current_run)
        }) else {
            return SessionUpdate::default();
        };
        plan.resolution = NextResolution::Ready(stream.clone());
        let mut update = SessionUpdate::default();
        if self.current_run.as_ref().is_some_and(|current| {
            matches!(
                current.status,
                TransportStatus::Buffering | TransportStatus::Playing | TransportStatus::Paused
            )
        }) {
            update
                .effects
                .push(SessionEffect::Backend(BackendCommand::PrepareNext {
                    current_run: plan.current_run,
                    next: Some(PreparedNext::new(plan.next_run, stream, plan.transition)),
                }));
        }
        update
    }

    pub fn stream_failed(
        &mut self,
        run: RunId,
        error: String,
        sample: &ClockSample,
    ) -> SessionUpdate {
        if self
            .current_run
            .as_ref()
            .is_some_and(|current| current.id == run)
        {
            let mut update = SessionUpdate::changed();
            self.finish_current(RunEndReason::Failed, sample, &mut update.effects);
            self.last_error = Some(error.clone());
            self.buffering_percent = None;
            update.effects.push(SessionEffect::FatalError(error));
            return update;
        }
        if self
            .next_plan
            .as_ref()
            .is_some_and(|plan| plan.next_run == run)
        {
            self.next_plan = None;
        }
        SessionUpdate::default()
    }

    pub fn handle_backend(&mut self, event: BackendEvent, sample: &ClockSample) -> SessionUpdate {
        match event {
            BackendEvent::Started { run } => self.accept_started(run, sample),
            BackendEvent::State { run, state } => self.accept_state(run, state, sample),
            BackendEvent::Position { run, millis } => self.accept_position(run, millis, sample),
            BackendEvent::Duration { run, millis } => self.accept_duration(run, millis),
            BackendEvent::Seekable { run, seekable } => self.accept_seekable(run, seekable),
            BackendEvent::Buffering { run, percent } => self.accept_buffering(run, percent),
            BackendEvent::Ended { run } => self.accept_ended(run, sample),
            BackendEvent::Transitioned { old_run, new_run } => {
                self.accept_transitioned(old_run, new_run, sample)
            }
            BackendEvent::NextNeeded { run } => {
                if self
                    .current_run
                    .as_ref()
                    .is_some_and(|current| current.id == run)
                    && self.next_plan.is_none()
                {
                    let mut update = SessionUpdate::default();
                    self.plan_next(&mut update.effects);
                    update
                } else {
                    SessionUpdate::default()
                }
            }
            BackendEvent::NextPreparationFailed {
                current_run,
                next_run,
                error,
            } => {
                if self.next_plan.as_ref().is_some_and(|plan| {
                    plan.current_run == current_run && plan.next_run == next_run
                }) {
                    SessionUpdate {
                        effects: vec![SessionEffect::NonfatalError(error.message().to_string())],
                        ..SessionUpdate::default()
                    }
                } else {
                    SessionUpdate::default()
                }
            }
            BackendEvent::AudioApplied {
                volume,
                muted,
                output,
            } => {
                let local = self.playback_output.is_local();
                let audio_output_changed = local && self.settings.audio_output != output;
                let unchanged = self.output_volume == volume
                    && self.output_muted == muted
                    && !audio_output_changed;
                if unchanged {
                    return SessionUpdate::default();
                }
                self.output_volume = volume;
                self.output_muted = muted;
                if local {
                    self.settings.volume = volume;
                    self.settings.muted = muted;
                    self.settings.audio_output = output.clone();
                }
                SessionUpdate {
                    effects: audio_output_changed
                        .then(|| SessionEffect::PersistOutputState {
                            volume,
                            muted,
                            audio_output: output,
                        })
                        .into_iter()
                        .collect(),
                    ..SessionUpdate::changed()
                }
            }
            BackendEvent::Visualizer { run, levels } => {
                if self
                    .current_run
                    .as_ref()
                    .is_some_and(|current| current.id == run)
                {
                    SessionUpdate {
                        effects: vec![SessionEffect::Visualizer { run, levels }],
                        ..SessionUpdate::default()
                    }
                } else {
                    SessionUpdate::default()
                }
            }
            BackendEvent::Error { run, error } => {
                let Some(current) = self
                    .current_run
                    .as_mut()
                    .filter(|current| current.id == run)
                else {
                    return SessionUpdate::default();
                };
                current.advance_clock(sample.monotonic_millis);
                current.status = TransportStatus::Failed;
                current.backend_loaded = false;
                self.last_error = Some(error.message().to_string());
                self.buffering_percent = None;
                SessionUpdate {
                    effects: vec![
                        SessionEffect::FatalError(error.message().to_string()),
                        SessionEffect::FlushPersistence {
                            source_id: self.sequence.source_key().clone(),
                        },
                    ],
                    ..SessionUpdate::changed()
                }
            }
        }
    }

    pub fn complete_auto_dj(
        &mut self,
        source_id: &SourceKey,
        seed_occurrence: &OccurrenceId,
        batch: Batch,
        sample: &ClockSample,
    ) -> Result<Option<SessionUpdate>, SequenceError> {
        let key = AutoDjKey {
            source_id: source_id.clone(),
            seed_occurrence: seed_occurrence.clone(),
        };
        if self.auto_dj_in_flight.as_ref() != Some(&key) {
            return Ok(None);
        }
        self.auto_dj_in_flight = None;
        if !self.auto_dj_enabled
            || self.sequence.source_key() != *source_id
            || self.sequence.occurrence(seed_occurrence).is_none()
            || self.sequence.remaining_after_selected() >= self.auto_dj_refill_threshold
        {
            return Ok(None);
        }
        let continuation = self.auto_dj_waiting_for_continuation;
        self.auto_dj_waiting_for_continuation = false;
        let trimmed = self.sequence.trim_auto_dj_history(AUTO_DJ_HISTORY_LIMIT);
        let mut update = self.apply_batch(batch, Placement::End, None, sample)?;
        if trimmed {
            update.queue_persistence_changed = true;
            update.queue_changed = true;
        }
        if continuation && self.current_run.is_none() && self.sequence.advance_manual().is_some() {
            self.begin_selected_run(&mut update.effects);
        }
        Ok(Some(update))
    }

    pub fn complete_auto_dj_candidates(
        &mut self,
        source_id: &SourceKey,
        seed_occurrence: &OccurrenceId,
        candidates: Vec<TrackKey>,
        requested_count: usize,
        shuffle_seed: u64,
        sample: &ClockSample,
    ) -> Result<Option<SessionUpdate>, SequenceError> {
        let items = candidates
            .into_iter()
            .take(requested_count)
            .map(|track_key| BatchItem::new(track_key, Provenance::AutoDj))
            .collect::<Vec<_>>();
        if items.is_empty() {
            return Ok(self.auto_dj_unavailable(source_id, seed_occurrence, None));
        }
        self.complete_auto_dj(
            source_id,
            seed_occurrence,
            Batch::new(items).with_shuffle_intent(shuffle_seed, false),
            sample,
        )
    }

    pub fn auto_dj_unavailable(
        &mut self,
        source_id: &SourceKey,
        seed_occurrence: &OccurrenceId,
        error: Option<String>,
    ) -> Option<SessionUpdate> {
        let key = AutoDjKey {
            source_id: source_id.clone(),
            seed_occurrence: seed_occurrence.clone(),
        };
        if self.auto_dj_in_flight.as_ref() != Some(&key) {
            return None;
        }
        self.auto_dj_in_flight = None;
        self.auto_dj_waiting_for_continuation = false;
        Some(SessionUpdate {
            effects: error
                .into_iter()
                .map(SessionEffect::NonfatalError)
                .collect(),
            ..SessionUpdate::default()
        })
    }

    fn apply_batch(
        &mut self,
        batch: Batch,
        placement: Placement,
        anchor: Option<PlaybackMedia>,
        sample: &ClockSample,
    ) -> Result<SessionUpdate, SequenceError> {
        let replacing = matches!(placement, Placement::Replace { .. });
        let previous_selected = self
            .sequence
            .selected()
            .map(|entry| entry.occurrence.clone());
        let previous_had_run = self.current_run.is_some();
        let mut update = SessionUpdate::changed();
        if replacing {
            self.pending_replacement = None;
            self.pending_additive.clear();
            self.auto_dj_in_flight = None;
            if let Some(run) = self.current_run.as_ref() {
                update
                    .effects
                    .push(SessionEffect::Backend(BackendCommand::Stop { run: run.id }));
            }
            self.finish_current(RunEndReason::Replaced, sample, &mut update.effects);
        }
        let change = self.sequence.apply_batch_with_change(batch, placement)?;
        if replacing {
            self.current_media = anchor.and_then(|media| {
                self.sequence.selected().and_then(|entry| {
                    (entry.track_key == media.track_key).then(|| (entry.occurrence.clone(), media))
                })
            });
            self.prepared_media = None;
        }
        if change.durable_changed() {
            update.queue_persistence_changed = true;
        }
        let next_selected = self
            .sequence
            .selected()
            .map(|entry| entry.occurrence.clone());
        update.queue_changed = change.rows_changed
            || (replacing && previous_selected.as_ref() != next_selected.as_ref());
        if replacing {
            self.begin_selected_run(&mut update.effects);
            if !previous_had_run && previous_selected.is_some() && next_selected.is_none() {
                update.effects.push(SessionEffect::CurrentMediaChanged);
            }
        } else if previous_selected != next_selected {
            update.effects.push(SessionEffect::CurrentMediaChanged);
            self.plan_next(&mut update.effects);
        } else {
            self.replan_next_if_changed(&mut update.effects);
        }
        self.maybe_request_auto_dj(&mut update.effects);
        Ok(update)
    }

    pub fn activate_context(
        &mut self,
        context_id: &str,
        track_id: &TrackKey,
        source_rank: usize,
        sample: &ClockSample,
    ) -> Option<SessionUpdate> {
        let index = self
            .sequence
            .context_index(context_id, *track_id, source_rank)?;
        let occurrence = self.sequence.entries().get(index)?.occurrence.clone();
        Some(self.activate_index(index, occurrence, sample))
    }

    fn activate(&mut self, occurrence: &OccurrenceId, sample: &ClockSample) -> SessionUpdate {
        let Some(index) = self.sequence.occurrence_index(occurrence) else {
            return SessionUpdate::default();
        };
        self.activate_index(index, occurrence.clone(), sample)
    }

    fn activate_index(
        &mut self,
        index: usize,
        occurrence: OccurrenceId,
        sample: &ClockSample,
    ) -> SessionUpdate {
        if self
            .current_run
            .as_ref()
            .is_some_and(|run| run.occurrence == occurrence)
        {
            return SessionUpdate::default();
        }
        self.pending_replacement = None;
        self.restored_paused = false;
        let mut update = SessionUpdate::changed();
        if let Some(run) = self.current_run.as_ref() {
            update
                .effects
                .push(SessionEffect::Backend(BackendCommand::Stop { run: run.id }));
        }
        self.finish_current(RunEndReason::ManualSkip, sample, &mut update.effects);
        if !self.sequence.activate_index(index) {
            return update;
        }
        self.begin_selected_run(&mut update.effects);
        update
    }

    fn remove(&mut self, occurrence: &OccurrenceId, sample: &ClockSample) -> SessionUpdate {
        let removing_current = self
            .sequence
            .selected()
            .is_some_and(|entry| &entry.occurrence == occurrence);
        let removing_current_run = removing_current && self.current_run.is_some();
        if self.sequence.occurrence(occurrence).is_none() {
            return SessionUpdate::default();
        }
        let mut update = SessionUpdate::structural();
        self.pending_replacement = None;
        if self
            .auto_dj_in_flight
            .as_ref()
            .is_some_and(|key| &key.seed_occurrence == occurrence)
        {
            self.auto_dj_in_flight = None;
        }
        if removing_current {
            if let Some(run) = self.current_run.as_ref() {
                update
                    .effects
                    .push(SessionEffect::Backend(BackendCommand::Stop { run: run.id }));
            }
            self.finish_current(RunEndReason::ManualSkip, sample, &mut update.effects);
        }
        self.sequence.remove(occurrence);
        if removing_current {
            self.begin_selected_run(&mut update.effects);
            if !removing_current_run && self.sequence.selected().is_none() {
                update.effects.push(SessionEffect::CurrentMediaChanged);
            }
        } else {
            self.replan_next_if_changed(&mut update.effects);
        }
        update
    }

    fn reorder(
        &mut self,
        occurrence: &OccurrenceId,
        target: &crate::QueueReorderTarget,
    ) -> SessionUpdate {
        if !self.sequence.reorder(occurrence, target) {
            return SessionUpdate::default();
        }
        self.pending_replacement = None;
        let mut update = SessionUpdate::structural();
        self.replan_next_if_changed(&mut update.effects);
        update
    }

    fn move_after_current(&mut self, occurrence: &OccurrenceId) -> SessionUpdate {
        if !self.sequence.move_after_current(occurrence) {
            return SessionUpdate::default();
        }
        self.pending_replacement = None;
        let mut update = SessionUpdate::structural();
        self.replan_next_if_changed(&mut update.effects);
        update
    }

    fn clear_upcoming(&mut self) -> SessionUpdate {
        let clears_current = self.current_run.is_none() && self.sequence.selected().is_some();
        let changed = if self.current_run.is_some() {
            self.sequence.clear_upcoming()
        } else {
            self.sequence.clear()
        };
        self.pending_replacement = None;
        self.pending_additive.clear();
        self.auto_dj_in_flight = None;
        self.auto_dj_waiting_for_continuation = false;
        if !changed {
            return SessionUpdate::default();
        }
        self.next_plan = None;
        let mut update = SessionUpdate::structural();
        if clears_current {
            update.effects.push(SessionEffect::CurrentMediaChanged);
        }
        if let Some(run) = self.current_run.as_ref() {
            update
                .effects
                .push(SessionEffect::Backend(BackendCommand::PrepareNext {
                    current_run: run.id,
                    next: None,
                }));
        }
        update
    }

    fn play_pause(&mut self) -> SessionUpdate {
        let playing = self
            .current_run
            .as_ref()
            .is_some_and(|run| run.desired_playing);
        self.set_playing(!playing)
    }

    fn set_playing(&mut self, desired_playing: bool) -> SessionUpdate {
        let Some(run) = self.current_run.as_mut() else {
            if !desired_playing {
                return SessionUpdate::default();
            }
            self.restored_paused = false;
            let mut update = SessionUpdate::changed();
            self.begin_selected_run(&mut update.effects);
            return update;
        };
        let command = match run.status {
            TransportStatus::Resolving => {
                if run.desired_playing == desired_playing {
                    return SessionUpdate::default();
                }
                run.desired_playing = desired_playing;
                return SessionUpdate::changed();
            }
            TransportStatus::Paused => {
                if !desired_playing {
                    if !run.desired_playing {
                        return SessionUpdate::default();
                    }
                    run.desired_playing = false;
                    BackendCommand::Pause { run: run.id }
                } else {
                    run.desired_playing = true;
                    if !run.backend_loaded
                        && let Some(stream) = run.resolved_stream.clone()
                    {
                        run.status = TransportStatus::Buffering;
                        run.backend_loaded = true;
                        let run_id = run.id;
                        let next = self.prepared_next(run_id);
                        let mut effects = Vec::new();
                        if self.playback_output.is_local() {
                            effects.push(SessionEffect::Backend(BackendCommand::ConfigureAudio(
                                self.settings.clone().into(),
                            )));
                        }
                        effects.push(SessionEffect::Backend(BackendCommand::Start {
                            run: run_id,
                            current: stream,
                            next,
                            start_position_millis: self.sequence.progress_millis(),
                            playback_rate: self.settings.playback_rate,
                        }));
                        return SessionUpdate {
                            effects,
                            view_changed: true,
                            queue_changed: false,
                            queue_persistence_changed: false,
                        };
                    }
                    BackendCommand::Play { run: run.id }
                }
            }
            TransportStatus::Playing | TransportStatus::Buffering => {
                if run.desired_playing == desired_playing {
                    return SessionUpdate::default();
                }
                run.desired_playing = desired_playing;
                if desired_playing {
                    BackendCommand::Play { run: run.id }
                } else {
                    BackendCommand::Pause { run: run.id }
                }
            }
            TransportStatus::Stopped | TransportStatus::Failed => {
                return SessionUpdate::default();
            }
        };
        SessionUpdate {
            effects: vec![SessionEffect::Backend(command)],
            view_changed: true,
            ..SessionUpdate::default()
        }
    }

    fn stop(&mut self, sample: &ClockSample) -> SessionUpdate {
        let Some(run) = self.current_run.as_ref() else {
            if self.sequence.progress_millis() == 0 && !self.restored_paused {
                return SessionUpdate::default();
            }
            self.restored_paused = false;
            self.sequence.set_progress_millis(0);
            return SessionUpdate {
                effects: vec![
                    self.progress_effect(),
                    SessionEffect::FlushPersistence {
                        source_id: self.sequence.source_key().clone(),
                    },
                ],
                view_changed: true,
                queue_changed: false,
                queue_persistence_changed: false,
            };
        };
        self.pending_replacement = None;
        let mut update = SessionUpdate::changed();
        update
            .effects
            .push(SessionEffect::Backend(BackendCommand::Stop { run: run.id }));
        self.finish_current(RunEndReason::Stopped, sample, &mut update.effects);
        self.sequence.set_progress_millis(0);
        update.effects.push(self.progress_effect());
        update.effects.push(SessionEffect::FlushPersistence {
            source_id: self.sequence.source_key().clone(),
        });
        update
    }

    pub(crate) fn shutdown(&mut self, sample: &ClockSample) -> SessionUpdate {
        self.pending_replacement = None;
        self.pending_additive.clear();
        self.auto_dj_in_flight = None;
        self.auto_dj_waiting_for_continuation = false;
        let mut update = SessionUpdate::changed();
        if let Some(run) = self.current_run.as_ref() {
            update
                .effects
                .push(SessionEffect::Backend(BackendCommand::Stop { run: run.id }));
        }
        self.finish_current(RunEndReason::Stopped, sample, &mut update.effects);
        update.effects.push(SessionEffect::FlushPersistence {
            source_id: self.sequence.source_key().clone(),
        });
        update
    }

    fn next(&mut self, sample: &ClockSample) -> SessionUpdate {
        let mut update = SessionUpdate::changed();
        let old = self.current_run.as_ref().map(|run| run.id);
        let reserved = self.next_plan.clone();
        self.pending_replacement = None;
        if let Some(run) = old {
            update
                .effects
                .push(SessionEffect::Backend(BackendCommand::Stop { run }));
        }
        self.finish_current(RunEndReason::ManualSkip, sample, &mut update.effects);
        let next_occurrence = self
            .sequence
            .advance_manual()
            .map(|entry| entry.occurrence.clone());
        let Some(next_occurrence) = next_occurrence else {
            self.auto_dj_waiting_for_continuation = true;
            self.maybe_request_auto_dj(&mut update.effects);
            return update;
        };
        self.next_plan = reserved.filter(|plan| plan.occurrence == next_occurrence);
        self.promote_or_begin(next_occurrence, true, &mut update.effects);
        update
    }

    fn previous(&mut self, sample: &ClockSample) -> SessionUpdate {
        if self.position_millis() > 10_000 {
            return self.seek(0);
        }
        if self.sequence.peek_previous().is_none() {
            return self.seek(0);
        }
        let mut update = SessionUpdate::changed();
        self.pending_replacement = None;
        if let Some(run) = self.current_run.as_ref() {
            update
                .effects
                .push(SessionEffect::Backend(BackendCommand::Stop { run: run.id }));
        }
        self.finish_current(RunEndReason::ManualSkip, sample, &mut update.effects);
        if self.sequence.previous().is_none() {
            return update;
        }
        self.begin_selected_run(&mut update.effects);
        update
    }

    fn seek(&mut self, position_millis: u64) -> SessionUpdate {
        let Some(run) = self.current_run.as_ref() else {
            self.sequence.set_progress_millis(position_millis);
            return SessionUpdate {
                effects: vec![self.progress_effect()],
                view_changed: true,
                queue_changed: false,
                queue_persistence_changed: false,
            };
        };
        if !run.seekable && position_millis != 0 {
            return SessionUpdate::default();
        }
        SessionUpdate {
            effects: vec![
                SessionEffect::Backend(BackendCommand::Seek {
                    run: run.id,
                    position_millis,
                }),
                SessionEffect::PositionDiscontinuity(PositionDiscontinuity {
                    run: run.id,
                    position_millis,
                }),
            ],
            ..SessionUpdate::default()
        }
    }

    fn accept_seekable(&mut self, run: RunId, seekable: bool) -> SessionUpdate {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return SessionUpdate::default();
        };
        if current.seekable == seekable {
            return SessionUpdate::default();
        }
        current.seekable = seekable;
        SessionUpdate::changed()
    }

    fn set_volume(&mut self, volume: f64) -> SessionUpdate {
        let volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        if self.output_volume == volume && !self.output_muted {
            return SessionUpdate::default();
        }
        self.output_volume = volume;
        self.output_muted = false;
        if self.playback_output.is_local() {
            self.settings.volume = volume;
            self.settings.muted = false;
        }
        SessionUpdate {
            effects: vec![SessionEffect::Backend(BackendCommand::SetOutputVolume {
                volume,
                volume_scale: self.settings.volume_scale,
                muted: false,
            })],
            view_changed: true,
            queue_changed: false,
            queue_persistence_changed: false,
        }
    }

    fn set_muted(&mut self, muted: bool) -> SessionUpdate {
        if self.output_muted == muted {
            return SessionUpdate::default();
        }
        self.output_muted = muted;
        if self.playback_output.is_local() {
            self.settings.muted = muted;
        }
        let mut effects = vec![SessionEffect::Backend(BackendCommand::SetOutputVolume {
            volume: self.output_volume,
            volume_scale: self.settings.volume_scale,
            muted,
        })];
        if self.playback_output.is_local() {
            effects.push(SessionEffect::PersistOutputState {
                volume: self.settings.volume,
                muted,
                audio_output: self.settings.audio_output.clone(),
            });
        }
        SessionUpdate {
            effects,
            view_changed: true,
            queue_changed: false,
            queue_persistence_changed: false,
        }
    }

    fn set_repeat(&mut self, repeat: RepeatMode) -> SessionUpdate {
        if self.sequence.repeat_mode() == repeat {
            return SessionUpdate::default();
        }
        self.sequence.set_repeat_mode(repeat);
        let mut update = SessionUpdate::changed();
        update.queue_persistence_changed = true;
        self.replan_next_if_changed(&mut update.effects);
        self.maybe_request_auto_dj(&mut update.effects);
        update
    }

    fn set_shuffle(&mut self, enabled: bool, seed: u64) -> SessionUpdate {
        if self.sequence.shuffle_enabled() == enabled {
            return SessionUpdate::default();
        }
        let revision = self.sequence.revision();
        self.sequence.set_shuffle_seed(enabled, seed);
        if self.sequence.revision() == revision {
            return SessionUpdate::default();
        }
        self.pending_replacement = None;
        let mut update = SessionUpdate::traversal();
        self.replan_next_if_changed(&mut update.effects);
        update
    }

    fn set_auto_dj(&mut self, enabled: bool, refill_threshold: usize) -> SessionUpdate {
        let refill_threshold = refill_threshold.max(1);
        if self.auto_dj_enabled == enabled && self.auto_dj_refill_threshold == refill_threshold {
            return SessionUpdate::default();
        }
        self.auto_dj_enabled = enabled;
        self.auto_dj_refill_threshold = refill_threshold;
        if !enabled {
            self.auto_dj_in_flight = None;
            self.auto_dj_waiting_for_continuation = false;
        }
        let mut update = SessionUpdate::changed();
        self.maybe_request_auto_dj(&mut update.effects);
        update
    }

    fn update_settings(&mut self, mut settings: PlaybackSettings) -> SessionUpdate {
        settings.sanitize();
        if settings == self.settings {
            return SessionUpdate::default();
        }
        let stream_changed = settings.stream_quality != self.settings.stream_quality;
        let playback_rate_changed = settings.playback_rate != self.settings.playback_rate;
        let playback_rate = settings.playback_rate;
        let output_changed = settings.volume != self.settings.volume
            || settings.volume_scale != self.settings.volume_scale
            || settings.muted != self.settings.muted;
        let audio_configuration_changed = settings.loudness_normalization
            != self.settings.loudness_normalization
            || settings.loudness_normalization_scope != self.settings.loudness_normalization_scope
            || settings.ebu_r128_target_lufs != self.settings.ebu_r128_target_lufs
            || settings.audio_output != self.settings.audio_output
            || settings.equalizer != self.settings.equalizer
            || settings.preserve_pitch != self.settings.preserve_pitch
            || settings.audio_fade_on_status_change != self.settings.audio_fade_on_status_change;
        self.settings = settings.clone();
        let mut update = SessionUpdate::changed();
        if self.playback_output.is_local() {
            self.output_volume = settings.volume;
            self.output_muted = settings.muted;
        }
        if self.playback_output.is_local() && audio_configuration_changed {
            update
                .effects
                .push(SessionEffect::Backend(BackendCommand::ConfigureAudio(
                    settings.into(),
                )));
        } else if self.playback_output.is_local() && output_changed {
            update
                .effects
                .push(SessionEffect::Backend(BackendCommand::SetOutputVolume {
                    volume: settings.volume,
                    volume_scale: settings.volume_scale,
                    muted: settings.muted,
                }));
        }
        if self.playback_output.is_local() && playback_rate_changed {
            update
                .effects
                .push(SessionEffect::Backend(BackendCommand::SetPlaybackRate(
                    playback_rate,
                )));
        }
        if stream_changed {
            self.replan_next(true, &mut update.effects);
        } else {
            self.replan_next_if_changed(&mut update.effects);
        }
        update
    }

    fn media_resolved(
        &mut self,
        occurrence: OccurrenceId,
        media: Option<PlaybackMedia>,
        prepared: bool,
        start_run: bool,
    ) -> SessionUpdate {
        let Some(media) = media else {
            return SessionUpdate {
                effects: vec![SessionEffect::NonfatalError(
                    "Queue media is unavailable".to_string(),
                )],
                ..SessionUpdate::default()
            };
        };
        if prepared {
            self.prepared_media = Some((occurrence, media));
            let mut update = SessionUpdate::default();
            self.replan_next(true, &mut update.effects);
            update
        } else if self
            .sequence
            .selected()
            .is_some_and(|entry| entry.occurrence == occurrence)
        {
            self.current_media = Some((occurrence, media));
            let mut update = SessionUpdate::changed();
            self.restored_paused = !start_run;
            if start_run {
                self.begin_selected_run(&mut update.effects);
            }
            update
        } else {
            SessionUpdate::default()
        }
    }

    fn stream_inputs_changed(&mut self) -> SessionUpdate {
        let mut update = SessionUpdate::default();
        self.replan_next(true, &mut update.effects);
        update
    }

    fn current_stream_resolved(&mut self, run: RunId, stream: PreparedStream) -> SessionUpdate {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return SessionUpdate::default();
        };
        if !current.desired_playing {
            current.status = TransportStatus::Paused;
            current.resolved_stream = Some(stream);
            return SessionUpdate::changed();
        }
        current.status = TransportStatus::Buffering;
        current.resolved_stream = Some(stream.clone());
        current.backend_loaded = true;
        let next = self.prepared_next(run);
        let mut effects = Vec::new();
        if self.playback_output.is_local() {
            effects.push(SessionEffect::Backend(BackendCommand::ConfigureAudio(
                self.settings.clone().into(),
            )));
        }
        effects.push(SessionEffect::Backend(BackendCommand::Start {
            run,
            current: stream,
            next,
            start_position_millis: self.sequence.progress_millis(),
            playback_rate: self.settings.playback_rate,
        }));
        SessionUpdate {
            effects,
            view_changed: true,
            queue_changed: false,
            queue_persistence_changed: false,
        }
    }

    fn prepared_next(&self, current_run: RunId) -> Option<PreparedNext> {
        self.next_plan.as_ref().and_then(|plan| {
            if plan.current_run != current_run {
                return None;
            }
            let NextResolution::Ready(stream) = &plan.resolution else {
                return None;
            };
            Some(PreparedNext::new(
                plan.next_run,
                stream.clone(),
                plan.transition,
            ))
        })
    }

    fn accept_started(&mut self, run: RunId, sample: &ClockSample) -> SessionUpdate {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return SessionUpdate::default();
        };
        current.backend_loaded = true;
        if current.started_at_unix_seconds.is_some() {
            return SessionUpdate::default();
        }
        let mut update = SessionUpdate::changed();
        self.mark_started(sample, &mut update.effects);
        update
    }

    fn accept_state(
        &mut self,
        run: RunId,
        state: BackendState,
        sample: &ClockSample,
    ) -> SessionUpdate {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return SessionUpdate::default();
        };
        current.advance_clock(sample.monotonic_millis);
        let status = match state {
            BackendState::Stopped => TransportStatus::Stopped,
            BackendState::Buffering => TransportStatus::Buffering,
            BackendState::Paused => TransportStatus::Paused,
            BackendState::Playing => TransportStatus::Playing,
        };
        if current.status == status {
            return SessionUpdate::default();
        }
        current.status = status;
        let mut update = SessionUpdate::changed();
        let progress_reported = self.emit_progress_facts(&mut update.effects);
        if matches!(status, TransportStatus::Playing | TransportStatus::Paused)
            && !progress_reported
            && let Some(current) = self.current_run.as_ref()
            && current.started_at_unix_seconds.is_some()
        {
            update
                .effects
                .push(SessionEffect::SourceReport(self.source_report(
                    current,
                    SourceReportPhase::Progress,
                    false,
                )));
        }
        if status == TransportStatus::Paused && !progress_reported {
            update.effects.push(self.progress_effect());
        }
        if matches!(status, TransportStatus::Paused | TransportStatus::Stopped) {
            update.effects.push(SessionEffect::FlushPersistence {
                source_id: self.sequence.source_key().clone(),
            });
        }
        update
    }

    fn accept_position(&mut self, run: RunId, millis: u64, sample: &ClockSample) -> SessionUpdate {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return SessionUpdate::default();
        };
        current.advance_clock(sample.monotonic_millis);
        let playhead_millis = if current.duration_millis == 0 {
            millis
        } else {
            millis.min(current.duration_millis)
        };
        self.sequence.set_progress_millis(playhead_millis);
        let mut update = SessionUpdate::changed();
        self.emit_progress_facts(&mut update.effects);
        update
    }

    fn accept_duration(&mut self, run: RunId, millis: u64) -> SessionUpdate {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return SessionUpdate::default();
        };
        if millis == 0 || current.duration_millis == millis {
            return SessionUpdate::default();
        }
        current.duration_millis = millis;
        SessionUpdate::changed()
    }

    fn accept_buffering(&mut self, run: RunId, percent: u8) -> SessionUpdate {
        if !self
            .current_run
            .as_ref()
            .is_some_and(|current| current.id == run)
        {
            return SessionUpdate::default();
        }
        self.buffering_percent = Some(percent.min(100));
        SessionUpdate::changed()
    }

    fn accept_ended(&mut self, run: RunId, sample: &ClockSample) -> SessionUpdate {
        if !self
            .current_run
            .as_ref()
            .is_some_and(|current| current.id == run)
        {
            return SessionUpdate::default();
        }
        let desired_playing = self
            .current_run
            .as_ref()
            .is_some_and(|current| current.desired_playing);
        let mut update = SessionUpdate::changed();
        let reserved = self.next_plan.clone();
        self.finish_current(RunEndReason::Completed, sample, &mut update.effects);
        let next = self
            .sequence
            .advance_eos()
            .map(|entry| entry.occurrence.clone());
        if let Some(next) = next {
            self.next_plan = reserved.filter(|plan| plan.occurrence == next);
            self.promote_or_begin(next, desired_playing, &mut update.effects);
        } else {
            self.auto_dj_waiting_for_continuation = true;
            self.maybe_request_auto_dj(&mut update.effects);
        }
        update
    }

    fn accept_transitioned(
        &mut self,
        old_run: RunId,
        new_run: RunId,
        sample: &ClockSample,
    ) -> SessionUpdate {
        let current_run = self.current_run.as_ref().map(|current| current.id);
        if current_run == Some(new_run) {
            return SessionUpdate::default();
        }
        if current_run != Some(old_run) {
            return SessionUpdate {
                effects: vec![SessionEffect::Backend(BackendCommand::Stop {
                    run: new_run,
                })],
                ..SessionUpdate::default()
            };
        }
        let desired_playing = self
            .current_run
            .as_ref()
            .is_none_or(|current| current.desired_playing);
        if !self
            .next_plan
            .as_ref()
            .is_some_and(|plan| plan.current_run == old_run && plan.next_run == new_run)
        {
            let mut update = self.accept_ended(old_run, sample);
            update.effects.insert(
                0,
                SessionEffect::Backend(BackendCommand::Stop { run: new_run }),
            );
            return update;
        }
        let Some(occurrence) = self.next_plan.as_ref().map(|plan| plan.occurrence.clone()) else {
            return SessionUpdate::default();
        };
        let transitioned_stream = self.next_plan.as_ref().and_then(|plan| {
            let NextResolution::Ready(stream) = &plan.resolution else {
                return None;
            };
            Some(stream.clone())
        });
        let mut update = SessionUpdate::changed();
        self.finish_current(RunEndReason::Completed, sample, &mut update.effects);
        if !self.sequence.activate_backend(&occurrence) {
            return update;
        }
        self.install_reserved_run(new_run, occurrence, desired_playing, &mut update.effects);
        if let Some(current) = self.current_run.as_mut() {
            current.resolved_stream = transitioned_stream;
            current.backend_loaded = true;
        }
        self.mark_started(sample, &mut update.effects);
        if !desired_playing {
            update
                .effects
                .push(SessionEffect::Backend(BackendCommand::Pause {
                    run: new_run,
                }));
        }
        update
    }

    fn promote_or_begin(
        &mut self,
        occurrence: OccurrenceId,
        desired_playing: bool,
        effects: &mut Vec<SessionEffect>,
    ) {
        let plan = self
            .next_plan
            .take()
            .filter(|plan| plan.occurrence == occurrence);
        let Some(plan) = plan else {
            self.begin_selected_run(effects);
            if !desired_playing && let Some(current) = self.current_run.as_mut() {
                current.desired_playing = false;
            }
            return;
        };
        self.install_reserved_run(plan.next_run, occurrence, desired_playing, effects);
        match plan.resolution {
            NextResolution::Ready(stream) if desired_playing => {
                if let Some(current) = self.current_run.as_mut() {
                    current.status = TransportStatus::Buffering;
                    current.resolved_stream = Some(stream.clone());
                    current.backend_loaded = true;
                }
                effects.push(SessionEffect::Backend(BackendCommand::Start {
                    run: plan.next_run,
                    current: stream,
                    next: None,
                    start_position_millis: 0,
                    playback_rate: self.settings.playback_rate,
                }));
            }
            NextResolution::Ready(stream) => {
                if let Some(current) = self.current_run.as_mut() {
                    current.status = TransportStatus::Paused;
                    current.resolved_stream = Some(stream);
                }
            }
            NextResolution::Resolving => {}
        }
    }

    fn install_reserved_run(
        &mut self,
        run: RunId,
        occurrence: OccurrenceId,
        desired_playing: bool,
        effects: &mut Vec<SessionEffect>,
    ) {
        let Some(entry) = self
            .sequence
            .selected()
            .filter(|entry| entry.occurrence == occurrence)
            .cloned()
        else {
            return;
        };
        if self
            .prepared_media
            .as_ref()
            .is_some_and(|(prepared, _)| prepared == &occurrence)
        {
            self.current_media = self.prepared_media.take();
        }
        let Some(media) = self
            .current_media
            .as_ref()
            .filter(|(current, _)| current == &occurrence)
            .map(|(_, media)| media)
        else {
            effects.push(SessionEffect::ResolveMedia {
                source_key: self.sequence.source_key(),
                occurrence,
                prepared: false,
            });
            return;
        };
        let mut current = RunContext::resolving(
            run,
            self.play_id(run),
            self.sequence.source_key(),
            &entry,
            media,
        );
        current.desired_playing = desired_playing;
        self.current_run = Some(current);
        effects.push(SessionEffect::CurrentMediaChanged);
        self.next_plan = None;
        self.buffering_percent = None;
        self.last_error = None;
        effects.push(self.state_effect());
        self.plan_next(effects);
    }

    fn begin_selected_run(&mut self, effects: &mut Vec<SessionEffect>) {
        let Some(entry) = self.sequence.selected().cloned() else {
            self.current_run = None;
            self.next_plan = None;
            return;
        };
        let Some(media) = self
            .current_media
            .as_ref()
            .filter(|(current, _)| current == &entry.occurrence)
            .map(|(_, media)| media)
            .cloned()
        else {
            effects.push(SessionEffect::ResolveMedia {
                source_key: self.sequence.source_key(),
                occurrence: entry.occurrence,
                prepared: false,
            });
            return;
        };
        let run = self.next_run_id();
        self.current_run = Some(RunContext::resolving(
            run,
            self.play_id(run),
            self.sequence.source_key(),
            &entry,
            &media,
        ));
        effects.push(SessionEffect::CurrentMediaChanged);
        self.next_plan = None;
        self.buffering_percent = None;
        self.last_error = None;
        effects.push(self.resolve_effect(run, &entry, &media));
        self.plan_next(effects);
        effects.push(self.state_effect());
    }

    fn plan_next(&mut self, effects: &mut Vec<SessionEffect>) {
        self.replan_next(false, effects);
    }

    fn replan_next_if_changed(&mut self, effects: &mut Vec<SessionEffect>) {
        self.replan_next(false, effects);
    }

    fn replan_next(&mut self, force: bool, effects: &mut Vec<SessionEffect>) {
        let Some(current) = self.current_run.as_ref() else {
            self.next_plan = None;
            return;
        };
        let current_run = current.id;
        let current_occurrence = current.occurrence.clone();
        let Some(_) = self
            .sequence
            .selected()
            .filter(|entry| entry.occurrence == current_occurrence)
        else {
            self.next_plan = None;
            return;
        };
        let next = self.sequence.peek_next_eos().cloned();
        let Some(next) = next else {
            if self.next_plan.take().is_some() {
                effects.push(SessionEffect::Backend(BackendCommand::PrepareNext {
                    current_run,
                    next: None,
                }));
            }
            return;
        };
        let Some(next_media) = self
            .prepared_media
            .as_ref()
            .filter(|(occurrence, _)| occurrence == &next.occurrence)
            .map(|(_, media)| media.clone())
        else {
            effects.push(SessionEffect::ResolveMedia {
                source_key: self.sequence.source_key(),
                occurrence: next.occurrence,
                prepared: true,
            });
            return;
        };
        let request = StreamRequest::for_media(&next_media, self.settings.stream_quality);
        let transition = decided_transition(
            &self.settings,
            self.current_media.as_ref().map(|(_, media)| media),
            &next_media,
        );
        if !force
            && self.next_plan.as_ref().is_some_and(|plan| {
                plan.current_run == current_run
                    && plan.occurrence == next.occurrence
                    && plan.request == request
                    && plan.transition == transition
            })
        {
            return;
        }
        if self.next_plan.take().is_some() {
            effects.push(SessionEffect::Backend(BackendCommand::PrepareNext {
                current_run,
                next: None,
            }));
        }
        let next_run = self.next_run_id();
        self.next_plan = Some(NextPlan {
            current_run,
            next_run,
            occurrence: next.occurrence.clone(),
            request: request.clone(),
            transition,
            resolution: NextResolution::Resolving,
        });
        effects.push(SessionEffect::ResolveStream {
            run: next_run,
            source_id: self.sequence.source_key(),
            occurrence: next.occurrence,
            media: Box::new(next_media),
            request,
        });
    }

    fn resolve_effect(
        &self,
        run: RunId,
        entry: &SequenceEntry,
        media: &PlaybackMedia,
    ) -> SessionEffect {
        SessionEffect::ResolveStream {
            run,
            source_id: self.sequence.source_key(),
            occurrence: entry.occurrence.clone(),
            media: Box::new(media.clone()),
            request: StreamRequest::for_media(media, self.settings.stream_quality),
        }
    }

    fn mark_started(&mut self, sample: &ClockSample, effects: &mut Vec<SessionEffect>) {
        let (run, track) = {
            let Some(current) = self.current_run.as_mut() else {
                return;
            };
            if current.started_at_unix_seconds.is_some() {
                return;
            }
            current.status = TransportStatus::Playing;
            current.started_at_unix_seconds = Some(sample.unix_seconds);
            current.local_period = Some(sample.local_period.clone());
            current.last_monotonic_millis = Some(sample.monotonic_millis);
            (current.id, current.track.clone())
        };
        effects.push(SessionEffect::Listening(ListeningFact::Started {
            run,
            started_at_unix_seconds: sample.unix_seconds,
            local_period: sample.local_period.clone(),
            track,
        }));
        if let Some(current) = self.current_run.as_ref() {
            effects.push(SessionEffect::SourceReport(self.source_report(
                current,
                SourceReportPhase::Started,
                false,
            )));
        }
        self.emit_progress_facts(effects);
    }

    fn emit_progress_facts(&mut self, effects: &mut Vec<SessionEffect>) -> bool {
        let playhead_millis = self.sequence.progress_millis();
        let (run, audible_millis, bucket_changed) = {
            let Some(current) = self.current_run.as_mut() else {
                return false;
            };
            if current.started_at_unix_seconds.is_none() {
                return false;
            }
            let bucket = playhead_millis / 10_000;
            let bucket_changed = current.last_progress_bucket != Some(bucket);
            if bucket_changed {
                current.last_progress_bucket = Some(bucket);
            }
            (current.id, current.audible_millis, bucket_changed)
        };
        effects.push(SessionEffect::Listening(ListeningFact::Progress {
            run,
            audible_millis,
            playhead_millis,
        }));
        self.qualify_current(effects);
        if bucket_changed {
            effects.push(self.progress_effect());
            if let Some(current) = self.current_run.as_ref() {
                effects.push(SessionEffect::SourceReport(self.source_report(
                    current,
                    SourceReportPhase::Progress,
                    false,
                )));
            }
        }
        bucket_changed
    }

    fn qualify_current(&mut self, effects: &mut Vec<SessionEffect>) {
        let activity = {
            let Some(current) = self.current_run.as_mut() else {
                return;
            };
            if !current.qualified
                && current.started_at_unix_seconds.is_some()
                && current.audible_millis
                    >= qualified_play_threshold_millis(current.duration_millis)
            {
                current.qualified = true;
                Some((
                    current.play_id.clone(),
                    current.track.clone(),
                    current.started_at_unix_seconds,
                    current.audible_millis,
                    current.local_period.clone(),
                ))
            } else {
                None
            }
        };
        let activity_qualified = activity.is_some();
        if let Some((play_id, track, Some(started_at), listened_millis, Some(local_period))) =
            activity
        {
            effects.push(SessionEffect::Activity(crate::ActivityListen {
                play_id: play_id.clone(),
                track: track.clone(),
                started_at_unix_seconds: started_at,
                local_period,
                listened_millis,
                skipped: false,
            }));
        }

        if activity_qualified && let Some(current) = self.current_run.as_ref() {
            effects.push(SessionEffect::SourceReport(self.source_report(
                current,
                SourceReportPhase::QualifiedPlay,
                false,
            )));
        }
    }

    fn finish_current(
        &mut self,
        reason: RunEndReason,
        sample: &ClockSample,
        effects: &mut Vec<SessionEffect>,
    ) {
        let Some(current) = self.current_run.as_mut() else {
            self.next_plan = None;
            return;
        };
        current.advance_clock(sample.monotonic_millis);
        self.qualify_current(effects);
        let Some(current) = self.current_run.take() else {
            return;
        };
        effects.push(SessionEffect::CurrentMediaChanged);
        if current.started_at_unix_seconds.is_some() {
            effects.push(SessionEffect::Listening(ListeningFact::Ended {
                run: current.id,
                reason,
                audible_millis: current.audible_millis,
                playhead_millis: self.sequence.progress_millis(),
            }));
            effects.push(SessionEffect::SourceReport(self.source_report(
                &current,
                SourceReportPhase::Ended,
                reason == RunEndReason::Failed,
            )));
            if !current.qualified
                && manual_end_is_skip(
                    reason,
                    current.duration_millis,
                    current.audible_millis,
                    self.sequence.progress_millis(),
                )
                && let (Some(period), Some(started_at)) = (
                    current.local_period.clone(),
                    current.started_at_unix_seconds,
                )
            {
                effects.push(SessionEffect::Activity(crate::ActivityListen {
                    play_id: current.play_id,
                    track: current.track,
                    started_at_unix_seconds: started_at,
                    local_period: period,
                    listened_millis: current.audible_millis,
                    skipped: true,
                }));
            }
        }
        effects.push(self.progress_effect());
        self.next_plan = None;
        self.buffering_percent = None;
    }

    fn source_report(
        &self,
        current: &RunContext,
        phase: SourceReportPhase,
        failed: bool,
    ) -> SourceReportFact {
        SourceReportFact {
            run: current.id,
            source_id: current.track.source_key,
            track_id: current.track.track_key,
            track_object_id: current.track.track_object_id.clone(),
            phase,
            started_at_unix_seconds: current
                .started_at_unix_seconds
                .expect("source reports require a started Playback run"),
            position_millis: self.sequence.progress_millis(),
            paused: current.status == TransportStatus::Paused,
            muted: self.settings.muted,
            volume: self.settings.volume,
            shuffle: self.sequence.shuffle_enabled(),
            repeat_mode: self.sequence.repeat_mode(),
            failed,
        }
    }

    fn play_id(&self, run: RunId) -> String {
        format!("{}:{}", self.play_id_prefix, run.get())
    }

    fn progress_effect(&self) -> SessionEffect {
        SessionEffect::PersistProgress {
            source_id: self.sequence.source_key().clone(),
            revision: self.sequence.revision(),
            occurrence: self
                .sequence
                .selected()
                .map(|entry| entry.occurrence.clone()),
            progress_millis: self.sequence.progress_millis(),
        }
    }

    fn state_effect(&self) -> SessionEffect {
        SessionEffect::PersistState {
            source_id: self.sequence.source_key().clone(),
            revision: self.sequence.revision(),
            occurrence: self
                .sequence
                .selected()
                .map(|entry| entry.occurrence.clone()),
            progress_millis: self.sequence.progress_millis(),
        }
    }

    fn maybe_request_auto_dj(&mut self, effects: &mut Vec<SessionEffect>) {
        if !self.auto_dj_enabled
            || self.sequence.remaining_after_selected() >= self.auto_dj_refill_threshold
            || self.auto_dj_in_flight.is_some()
        {
            return;
        }
        let Some(seed) = self.sequence.selected() else {
            return;
        };
        let Some(seed_track_id) = seed.track_key else {
            return;
        };
        let key = AutoDjKey {
            source_id: self.sequence.source_key().clone(),
            seed_occurrence: seed.occurrence.clone(),
        };
        self.auto_dj_in_flight = Some(key.clone());
        effects.push(SessionEffect::RequestAutoDj(AutoDjRequest {
            source_id: key.source_id,
            seed_occurrence: key.seed_occurrence,
            seed_track_id,
            requested_count: 5,
        }));
    }

    fn next_run_id(&mut self) -> RunId {
        let run = RunId::new(self.next_run_number);
        self.next_run_number = self.next_run_number.wrapping_add(1).max(1);
        run
    }
}

fn decided_transition(
    settings: &PlaybackSettings,
    current: Option<&PlaybackMedia>,
    next: &PlaybackMedia,
) -> NextTransition {
    match settings.transition_mode {
        PlaybackTransitionMode::Gapless => NextTransition::Gapless,
        PlaybackTransitionMode::Crossfade
            if settings.skip_same_album_crossfade
                && current.is_some_and(|current| current.album_key == next.album_key) =>
        {
            NextTransition::Gapless
        }
        PlaybackTransitionMode::Crossfade => NextTransition::Crossfade {
            duration_millis: u64::from(settings.crossfade_seconds) * 1_000,
        },
    }
}

#[cfg(test)]
mod orchestration_tests {
    use super::*;
    use crate::{BatchItem, Placement, Provenance};

    fn restored_media() -> PlaybackMedia {
        PlaybackMedia {
            source_id: "source".to_string(),
            track_key: Some(TrackKey::from_raw(1)),
            track_object_id: "track".to_string(),
            title: "Track".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            album_display_artist: None,
            album_key: None,
            primary_artist_key: None,
            media_uri: None,
            artwork_binding: None,
            duration_millis: 180_000,
            disc_number: Some(1),
            track_number: Some(1),
            year: None,
            release_date: None,
            favorite: None,
            rating: None,
            is_downloaded: false,
            source_format: None,
            musicbrainz_recording_id: None,
            musicbrainz_release_track_id: None,
            musicbrainz_album_id: None,
            musicbrainz_release_group_id: None,
            primary_artist_musicbrainz_id: None,
            cue_path: None,
            cue_start_millis: None,
            cue_end_millis: None,
            artist_links: Vec::new(),
        }
    }

    #[test]
    fn restored_media_stays_paused_until_first_play_starts_one_resolution() {
        let source = SourceKey::from_raw(1);
        let mut sequence = Sequence::new(source);
        sequence
            .apply_batch_with_change(
                Batch::new(vec![BatchItem::new(
                    TrackKey::from_raw(1),
                    Provenance::Manual,
                )]),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("one Track Queue");
        sequence.set_progress_millis(42_000);
        let occurrence = sequence.selected().expect("selected").occurrence.clone();
        let mut session = PlaybackSession::new(
            sequence,
            SourceSessionEpoch::new(1),
            "restore-law",
            PlaybackSettings::default(),
            PlaybackOutput::Local,
            false,
            3,
        );
        let restored = session.media_resolved(occurrence, Some(restored_media()), false, false);
        assert!(restored.effects.is_empty());
        assert_eq!(session.current_run(), None);
        assert_eq!(session.status(), TransportStatus::Paused);
        assert_eq!(session.position_millis(), 42_000);

        let played = session
            .handle_command(
                SessionCommand::Play,
                &ClockSample {
                    monotonic_millis: 0,
                    unix_seconds: 0,
                    local_period: "1970-01".to_string(),
                },
            )
            .expect("first Play");
        assert_eq!(
            played
                .effects
                .iter()
                .filter(|effect| matches!(effect, SessionEffect::ResolveStream { .. }))
                .count(),
            1
        );
        assert!(session.current_run().is_some());
        assert_eq!(session.position_millis(), 42_000);
    }

    #[test]
    fn shuffled_collection_resolves_the_selected_random_track() {
        let source = SourceKey::from_raw(1);
        let mut session = PlaybackSession::new(
            Sequence::new(source),
            SourceSessionEpoch::new(1),
            "shuffled-collection-law",
            PlaybackSettings::default(),
            PlaybackOutput::Local,
            false,
            3,
        );
        let reservation = session.reserve_materialization(Placement::Replace { anchor_index: 0 });
        let update = session
            .apply_materialization(
                reservation.id,
                &reservation.source_id,
                Batch::new(vec![
                    BatchItem::new(TrackKey::from_raw(1), Provenance::Manual),
                    BatchItem::new(TrackKey::from_raw(2), Provenance::Manual),
                    BatchItem::new(TrackKey::from_raw(3), Provenance::Manual),
                    BatchItem::new(TrackKey::from_raw(4), Provenance::Manual),
                ])
                .with_shuffle_intent(2, true),
                Placement::Replace { anchor_index: 0 },
                Some(restored_media()),
                &ClockSample {
                    monotonic_millis: 0,
                    unix_seconds: 0,
                    local_period: "1970-01".to_string(),
                },
            )
            .expect("valid shuffled collection")
            .expect("accepted shuffled collection");

        let selected = session
            .sequence()
            .selected()
            .expect("selected shuffled Track");
        assert_ne!(selected.track_key, Some(TrackKey::from_raw(1)));
        assert!(session.current_media_fact().is_none());
        assert!(update.effects.iter().any(|effect| matches!(
            effect,
            SessionEffect::ResolveMedia {
                occurrence,
                prepared: false,
                ..
            } if occurrence == &selected.occurrence
        )));
        assert!(
            !update
                .effects
                .iter()
                .any(|effect| matches!(effect, SessionEffect::ResolveStream { .. }))
        );
    }

    #[test]
    fn gapless_preparation_resolves_exactly_the_next_occurrence() {
        let source = SourceKey::from_raw(1);
        let mut sequence = Sequence::new(source);
        sequence
            .apply_batch_with_change(
                Batch::new(vec![
                    BatchItem::new(TrackKey::from_raw(1), Provenance::Manual),
                    BatchItem::new(TrackKey::from_raw(2), Provenance::Manual),
                ]),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("two Track Queue");
        let current = sequence.entries()[0].occurrence.clone();
        let next = sequence.entries()[1].occurrence.clone();
        let mut session = PlaybackSession::new(
            sequence,
            SourceSessionEpoch::new(1),
            "gapless-law",
            PlaybackSettings::default(),
            PlaybackOutput::Local,
            false,
            3,
        );
        let current_update = session.media_resolved(current, Some(restored_media()), false, true);
        let current_run = current_update
            .effects
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::ResolveStream { run, .. } => Some(*run),
                _ => None,
            })
            .expect("current resolution");
        session.stream_resolved(
            current_run,
            crate::ResolvedStream::new("file:///current.flac"),
        );
        let mut next_media = restored_media();
        next_media.track_key = Some(TrackKey::from_raw(2));
        next_media.track_object_id = "next-track".to_string();
        let next_update = session.media_resolved(next.clone(), Some(next_media), true, false);
        let next_run = next_update
            .effects
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::ResolveStream {
                    run, occurrence, ..
                } if occurrence == &next => Some(*run),
                _ => None,
            })
            .expect("next resolution");
        let prepared =
            session.stream_resolved(next_run, crate::ResolvedStream::new("file:///next.flac"));
        assert!(prepared.effects.iter().any(|effect| matches!(
            effect,
            SessionEffect::Backend(BackendCommand::PrepareNext {
                next: Some(PreparedNext {
                    run,
                    transition: NextTransition::Gapless,
                    ..
                }),
                ..
            }) if *run == next_run
        )));
    }

    #[test]
    fn one_track_auto_dj_completion_publishes_appended_queue_entries() {
        let source = SourceKey::from_raw(1);
        let mut sequence = Sequence::new(source);
        sequence
            .apply_batch_with_change(
                Batch::new(vec![BatchItem::new(
                    TrackKey::from_raw(1),
                    Provenance::Manual,
                )]),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("one Track Queue");
        let mut session = PlaybackSession::new(
            sequence,
            SourceSessionEpoch::new(1),
            "autodj-law",
            PlaybackSettings::default(),
            PlaybackOutput::Local,
            false,
            3,
        );
        let sample = ClockSample {
            monotonic_millis: 0,
            unix_seconds: 0,
            local_period: "1970-01".to_string(),
        };
        let requested = session
            .handle_command(
                SessionCommand::SetAutoDj {
                    enabled: true,
                    refill_threshold: 3,
                },
                &sample,
            )
            .expect("enable AutoDJ");
        let request = requested
            .effects
            .into_iter()
            .find_map(|effect| match effect {
                SessionEffect::RequestAutoDj(request) => Some(request),
                _ => None,
            })
            .expect("one Track requests AutoDJ");
        let completed = session
            .complete_auto_dj_candidates(
                &request.source_id,
                &request.seed_occurrence,
                vec![TrackKey::from_raw(2), TrackKey::from_raw(3)],
                request.requested_count,
                7,
                &sample,
            )
            .expect("complete AutoDJ")
            .expect("accepted AutoDJ completion");
        assert!(completed.queue_changed);
        assert!(completed.queue_persistence_changed);
        assert_eq!(session.view().queue.total, 3);
    }

    #[test]
    fn failed_stream_preserves_selected_media_and_player_controls() {
        let source = SourceKey::from_raw(1);
        let mut sequence = Sequence::new(source);
        sequence
            .apply_batch_with_change(
                Batch::new(vec![BatchItem::new(
                    TrackKey::from_raw(1),
                    Provenance::Manual,
                )]),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("one Track Queue");
        let occurrence = sequence.selected().expect("selected").occurrence.clone();
        let mut session = PlaybackSession::new(
            sequence,
            SourceSessionEpoch::new(1),
            "failure-law",
            PlaybackSettings::default(),
            PlaybackOutput::Local,
            false,
            3,
        );
        let update = session.media_resolved(
            occurrence,
            Some(PlaybackMedia {
                source_id: "source".to_string(),
                track_key: Some(TrackKey::from_raw(1)),
                track_object_id: "track".to_string(),
                title: "Track".to_string(),
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                album_display_artist: None,
                album_key: None,
                primary_artist_key: None,
                media_uri: None,
                artwork_binding: None,
                duration_millis: 180_000,
                disc_number: Some(1),
                track_number: Some(1),
                year: None,
                release_date: None,
                favorite: None,
                rating: None,
                is_downloaded: false,
                source_format: None,
                musicbrainz_recording_id: None,
                musicbrainz_release_track_id: None,
                musicbrainz_album_id: None,
                musicbrainz_release_group_id: None,
                primary_artist_musicbrainz_id: None,
                cue_path: None,
                cue_start_millis: None,
                cue_end_millis: None,
                artist_links: Vec::new(),
            }),
            false,
            true,
        );
        let run = update
            .effects
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::ResolveStream { run, .. } => Some(*run),
                _ => None,
            })
            .expect("current stream run");
        let sample = ClockSample {
            monotonic_millis: 0,
            unix_seconds: 0,
            local_period: "1970-01".to_string(),
        };
        session.stream_failed(run, "missing".to_string(), &sample);
        let view = session.view();
        assert!(view.transport.current.is_some());
        assert_eq!(view.queue.total, 1);
        assert!(view.transport.error.is_some());
    }
}
