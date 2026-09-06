use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::{
    BackendCommand, BackendEvent, BackendState, Batch, BatchItem, ListeningFact, NextTransition,
    OccurrenceId, Placement, PlaybackOutput, PlaybackSettings, PlaybackTransitionMode,
    PreparedNext, PreparedStream, Provenance, QueueItem, QueueOccurrence, RepeatMode, RunEndReason,
    RunId, Sequence, SequenceError, StreamRequest, manual_end_is_skip,
    qualified_play_threshold_millis,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MaterializationId(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializationReservation {
    pub id: MaterializationId,
    pub current_media_uri: Option<String>,
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
    pub media_uri: String,
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
    pub seed_occurrence: OccurrenceId,
    pub seed_media_uri: String,
    pub requested_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionEffect {
    RefreshArtwork(Vec<String>),
    CancelStream(RunId),
    Queue {
        id: u64,
        edit: library::QueueEdit,
        current: Option<OccurrenceId>,
        progress_millis: u64,
        repeat: RepeatMode,
        shuffled: bool,
    },
    ResolveStream {
        run: RunId,
        occurrence: std::sync::Arc<QueueOccurrence>,
        request: StreamRequest,
    },
    Backend(BackendCommand),
    PersistProgress {
        revision: u64,
        occurrence: Option<OccurrenceId>,
        progress_millis: u64,
    },
    PersistState {
        revision: u64,
        occurrence: Option<OccurrenceId>,
        progress_millis: u64,
    },
    PersistOutputState {
        volume: f64,
        muted: bool,
        audio_output: Option<String>,
    },
    FlushPersistence,
    Listening(ListeningFact),
    Activity(Box<crate::ActivityListen>),
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
    QueuePrepared {
        id: u64,
        window: Box<library::QueueRestore>,
    },
    ApplyBatch {
        batch: Batch,
        placement: Placement,
    },
    QueueComplete {
        id: u64,
        result: Box<Result<library::QueueRestore, String>>,
    },
    Activate(OccurrenceId),
    Remove(OccurrenceId),
    RemoveMany(Vec<OccurrenceId>),
    Forget(Vec<OccurrenceId>),
    Reorder {
        occurrences: Vec<OccurrenceId>,
        target: crate::QueueReorderTarget,
    },
    Insert {
        input: library::QueueInput,
        target: crate::QueueReorderTarget,
    },
    MoveAfterCurrent(OccurrenceId),
    Clear {
        include_current: bool,
    },
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
    StreamInputsChanged,
    CatalogChanged,
    ArtworkRefreshed(Vec<(String, Option<Vec<u8>>)>),
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
}

#[derive(Clone, Debug)]
struct RunContext {
    id: RunId,
    play_id: String,
    occurrence: OccurrenceId,
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
    fn resolving(id: RunId, play_id: String, entry: &QueueOccurrence) -> Self {
        Self {
            id,
            play_id,
            occurrence: entry.occurrence.clone(),
            duration_millis: entry.duration_millis.max(0) as u64,
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
    seed_occurrence: OccurrenceId,
}

#[derive(Clone, Copy, Debug)]
enum QueueCompletion {
    Preserve,
    Replace,
    Select,
    Forget,
    Clear,
    Refill,
}

#[derive(Clone, Debug)]
pub(crate) struct PlaybackSession {
    sequence: Sequence,
    pending_queue: Option<(u64, QueueCompletion, Option<OccurrenceId>)>,
    next_queue_request: u64,
    queue_transport: Option<TransportStatus>,
    deferred_queue: VecDeque<SessionCommand>,
    play_id_prefix: Arc<str>,
    current_run: Option<RunContext>,
    next_plan: Option<NextPlan>,
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
        play_id_prefix: impl Into<Arc<str>>,
        mut settings: PlaybackSettings,
        playback_output: PlaybackOutput,
        auto_dj_enabled: bool,
        auto_dj_refill_threshold: usize,
    ) -> Self {
        settings.sanitize();
        let output_volume = settings.volume;
        let output_muted = settings.muted;
        let restored_paused = sequence.selected().is_some();
        Self {
            sequence,
            pending_queue: None,
            next_queue_request: 1,
            queue_transport: None,
            deferred_queue: VecDeque::new(),
            play_id_prefix: play_id_prefix.into(),
            current_run: None,
            next_plan: None,
            restored_paused,
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

    pub(crate) fn stream_runs(&self) -> [Option<RunId>; 2] {
        [
            self.current_run.as_ref().map(|run| run.id),
            self.next_plan.as_ref().map(|plan| plan.next_run),
        ]
    }

    pub fn sequence(&self) -> &Sequence {
        &self.sequence
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
                self.sequence
                    .selected()
                    .map(|entry| entry.duration_millis.max(0) as u64)
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
            current_media_uri: self
                .sequence
                .selected()
                .map(|entry| entry.media_uri.clone()),
        }
    }

    pub fn apply_materialization(
        &mut self,
        id: MaterializationId,
        batch: Batch,
        placement: Placement,
        sample: &ClockSample,
    ) -> Result<Option<SessionUpdate>, SequenceError> {
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
        self.apply_batch(batch, placement, sample).map(Some)
    }

    pub fn fail_materialization(
        &mut self,
        id: MaterializationId,
        placement: Placement,
        message: String,
    ) -> Option<SessionUpdate> {
        self.cancel_materialization(id, placement)
            .then(|| SessionUpdate {
                effects: vec![SessionEffect::NonfatalError(message)],
                ..SessionUpdate::default()
            })
    }

    pub fn cancel_materialization(&mut self, id: MaterializationId, placement: Placement) -> bool {
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
        let replaces = matches!(
            &command,
            SessionCommand::ApplyBatch {
                placement: Placement::Replace { .. },
                ..
            }
        );
        let bounded_navigation = match &command {
            SessionCommand::Activate(occurrence) => {
                self.sequence.occurrence_index(occurrence).is_some()
            }
            SessionCommand::Next => self
                .sequence
                .next_index(false)
                .is_none_or(|index| self.sequence.at(index).is_some()),
            SessionCommand::Previous => self
                .sequence
                .previous_index()
                .is_none_or(|index| self.sequence.at(index).is_some()),
            _ => false,
        };
        if self.pending_queue.is_some()
            && !replaces
            && !bounded_navigation
            && matches!(
                &command,
                SessionCommand::ApplyBatch { .. }
                    | SessionCommand::Activate(_)
                    | SessionCommand::Remove(_)
                    | SessionCommand::RemoveMany(_)
                    | SessionCommand::Forget(_)
                    | SessionCommand::Reorder { .. }
                    | SessionCommand::Insert { .. }
                    | SessionCommand::MoveAfterCurrent(_)
                    | SessionCommand::Clear { .. }
                    | SessionCommand::Next
                    | SessionCommand::Previous
                    | SessionCommand::SetRepeat(_)
                    | SessionCommand::SetShuffle { .. }
            )
        {
            self.deferred_queue.push_back(command);
            return Ok(SessionUpdate::default());
        }
        match command {
            SessionCommand::QueuePrepared { id, window } => self.prepare_queue(id, *window, sample),
            SessionCommand::ApplyBatch { batch, placement } => {
                self.apply_batch(batch, placement, sample)
            }
            SessionCommand::QueueComplete { id, result } => {
                self.complete_queue(id, *result, sample)
            }
            SessionCommand::Activate(occurrence) => Ok(self.activate(&occurrence, sample)),
            SessionCommand::Remove(occurrence) => Ok(self.remove(&occurrence, sample)),
            SessionCommand::RemoveMany(occurrences) => Ok(self.remove_many(&occurrences, sample)),
            SessionCommand::Forget(occurrences) => Ok(self.forget(&occurrences, sample)),
            SessionCommand::Reorder {
                occurrences,
                target,
            } => Ok(self.reorder(&occurrences, &target)),
            SessionCommand::Insert { input, target } => Ok(self.insert(input, &target)),
            SessionCommand::MoveAfterCurrent(occurrence) => {
                Ok(self.move_after_current(&occurrence))
            }
            SessionCommand::Clear { include_current } => Ok(self.clear(include_current, sample)),
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
            SessionCommand::StreamInputsChanged => Ok(self.stream_inputs_changed()),
            SessionCommand::CatalogChanged => Ok(SessionUpdate {
                effects: vec![SessionEffect::RefreshArtwork(self.sequence.artwork_uris())],
                ..SessionUpdate::default()
            }),
            SessionCommand::ArtworkRefreshed(bindings) => Ok(SessionUpdate {
                view_changed: self.sequence.refresh_artwork(&bindings),
                ..SessionUpdate::default()
            }),
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
                        SessionEffect::FlushPersistence,
                    ],
                    ..SessionUpdate::changed()
                }
            }
        }
    }

    pub fn complete_auto_dj(
        &mut self,
        seed_occurrence: &OccurrenceId,
        batch: Batch,
        sample: &ClockSample,
    ) -> Result<Option<SessionUpdate>, SequenceError> {
        let key = AutoDjKey {
            seed_occurrence: seed_occurrence.clone(),
        };
        if self.auto_dj_in_flight.as_ref() != Some(&key) {
            return Ok(None);
        }
        self.auto_dj_in_flight = None;
        if !self.auto_dj_enabled
            || self.sequence.selected().map(|entry| &entry.occurrence) != Some(seed_occurrence)
            || self.sequence.occurrence(seed_occurrence).is_none()
            || self.sequence.remaining_after_selected() >= self.auto_dj_refill_threshold
        {
            return Ok(None);
        }
        let update = self.apply_batch(batch, Placement::End, sample)?;
        Ok(Some(update))
    }

    pub fn complete_auto_dj_candidates(
        &mut self,
        seed_occurrence: &OccurrenceId,
        candidates: Vec<QueueItem>,
        requested_count: usize,
        shuffle_seed: u64,
        sample: &ClockSample,
    ) -> Result<Option<SessionUpdate>, SequenceError> {
        let items = candidates
            .into_iter()
            .take(requested_count)
            .map(|media| BatchItem::direct(media, Provenance::AutoDj))
            .collect::<Vec<_>>();
        if items.is_empty() {
            return Ok(self.auto_dj_unavailable(seed_occurrence, None));
        }
        self.complete_auto_dj(
            seed_occurrence,
            Batch::new(items).with_shuffle_intent(shuffle_seed, false),
            sample,
        )
    }

    pub fn auto_dj_unavailable(
        &mut self,
        seed_occurrence: &OccurrenceId,
        error: Option<String>,
    ) -> Option<SessionUpdate> {
        let key = AutoDjKey {
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
        sample: &ClockSample,
    ) -> Result<SessionUpdate, SequenceError> {
        let replacing = matches!(placement, Placement::Replace { .. });
        if self.pending_queue.is_some() && !replacing {
            self.deferred_queue
                .push_back(SessionCommand::ApplyBatch { batch, placement });
            return Ok(SessionUpdate::default());
        }
        if replacing {
            self.deferred_queue.clear();
            self.queue_transport = None;
        }
        let completion = if replacing {
            QueueCompletion::Replace
        } else {
            QueueCompletion::Preserve
        };
        let mut update = self.request_queue(
            library::QueueEdit::Apply {
                input: batch.input,
                placement,
                shuffle_seed: (self.sequence.shuffle_enabled() || batch.random_start)
                    .then_some(batch.shuffle_seed),
                random_start: batch.random_start,
                identity: batch.identity,
            },
            completion,
        );
        if replacing && let Some(window) = batch.window {
            let id = self.pending_queue.as_ref().unwrap().0;
            let ready = self.prepare_queue(id, *window, sample)?;
            update.effects.extend(ready.effects);
            update.view_changed |= ready.view_changed;
            update.queue_changed |= ready.queue_changed;
        }
        Ok(update)
    }

    fn prepare_queue(
        &mut self,
        id: u64,
        mut window: library::QueueRestore,
        sample: &ClockSample,
    ) -> Result<SessionUpdate, SequenceError> {
        let Some((pending, QueueCompletion::Replace, _)) = self.pending_queue.as_ref() else {
            return Ok(SessionUpdate::default());
        };
        if *pending != id {
            return Ok(SessionUpdate::default());
        }
        let mut update = SessionUpdate {
            queue_changed: true,
            ..SessionUpdate::changed()
        };
        if let Some(run) = self.current_run.as_ref() {
            update
                .effects
                .push(SessionEffect::Backend(BackendCommand::Stop { run: run.id }));
        }
        self.finish_current(RunEndReason::Replaced, sample, &mut update.effects);
        window.repeat_mode = self.sequence.repeat_mode();
        self.pending_queue = Some((
            id,
            QueueCompletion::Preserve,
            window.current_occurrence.clone(),
        ));
        self.sequence = self
            .sequence
            .replace_window(window, self.sequence.revision() + 1)?;
        self.sequence.mark_prepared();
        self.restored_paused = false;
        if self.queue_transport != Some(TransportStatus::Stopped) {
            self.begin_selected_run(&mut update.effects);
            if self.queue_transport == Some(TransportStatus::Paused) {
                update.effects.extend(self.set_playing(false).effects);
            }
        }
        update.effects.push(self.state_effect());
        Ok(update)
    }

    fn request_queue(
        &mut self,
        edit: library::QueueEdit,
        completion: QueueCompletion,
    ) -> SessionUpdate {
        if !self.sequence.is_durable()
            && self.pending_queue.is_none()
            && !matches!(completion, QueueCompletion::Replace)
        {
            return SessionUpdate {
                effects: vec![SessionEffect::NonfatalError(
                    "The playing Queue was not saved; start a new Queue before editing it".into(),
                )],
                ..SessionUpdate::default()
            };
        }
        let id = self.next_queue_request;
        self.next_queue_request = id.wrapping_add(1);
        let current = self
            .sequence
            .selected()
            .map(|entry| entry.occurrence.clone());
        self.pending_queue = Some((id, completion, current.clone()));
        SessionUpdate {
            effects: vec![SessionEffect::Queue {
                id,
                edit,
                current,
                progress_millis: self.sequence.progress_millis(),
                repeat: self.sequence.repeat_mode(),
                shuffled: self.sequence.shuffle_enabled(),
            }],
            ..SessionUpdate::default()
        }
    }

    pub(crate) fn refill_queue(&mut self) -> Option<SessionEffect> {
        if self.pending_queue.is_some()
            || !self.sequence.is_durable()
            || !self.sequence.needs_refill()
        {
            return None;
        }
        let index = self.sequence.selected_index()?;
        self.request_queue(
            library::QueueEdit::SelectIndex(index),
            QueueCompletion::Refill,
        )
        .effects
        .pop()
    }

    fn complete_queue(
        &mut self,
        id: u64,
        result: Result<library::QueueRestore, String>,
        sample: &ClockSample,
    ) -> Result<SessionUpdate, SequenceError> {
        let Some((pending, completion, requested_current)) = self.pending_queue.take() else {
            return Ok(SessionUpdate::default());
        };
        if id != pending {
            self.pending_queue = Some((pending, completion, requested_current));
            return Ok(SessionUpdate::default());
        }
        let mut update = SessionUpdate::changed();
        match result {
            Err(error) => update.effects.push(SessionEffect::NonfatalError(error)),
            Ok(mut window) => {
                let live = self
                    .sequence
                    .selected()
                    .map(|entry| entry.occurrence.clone());
                let preserve = matches!(
                    completion,
                    QueueCompletion::Preserve
                        | QueueCompletion::Refill
                        | QueueCompletion::Forget
                        | QueueCompletion::Clear
                );
                if preserve && let Some(live_id) = live.as_ref() {
                    if let Some(local) = window
                        .occurrences
                        .iter()
                        .position(|entry| &entry.occurrence == live_id)
                    {
                        window.current_occurrence = Some(live_id.clone());
                        window.current_index = Some(window.window_start + local);
                        window.progress_millis = self.sequence.progress_millis() as i64;
                    } else if live != requested_current {
                        let edit = window
                            .removed_successors
                            .iter()
                            .find(|(removed, _)| removed == live_id)
                            .map_or_else(
                                || library::QueueEdit::Select(live_id.clone()),
                                |(_, next)| library::QueueEdit::SelectOptional(next.clone()),
                            );
                        self.sequence.mark_durable();
                        let mut update = self.request_queue(edit, completion);
                        update.view_changed = true;
                        return Ok(update);
                    }
                }
                let selected = window.current_occurrence.clone();
                let changed = live != selected
                    || matches!(
                        completion,
                        QueueCompletion::Replace | QueueCompletion::Select
                    );
                if changed {
                    if let Some(run) = self.current_run.as_ref() {
                        update
                            .effects
                            .push(SessionEffect::Backend(BackendCommand::Stop { run: run.id }));
                    }
                    self.finish_current(
                        if matches!(completion, QueueCompletion::Replace) {
                            RunEndReason::Replaced
                        } else if matches!(
                            completion,
                            QueueCompletion::Clear | QueueCompletion::Forget
                        ) {
                            RunEndReason::Stopped
                        } else {
                            RunEndReason::ManualSkip
                        },
                        sample,
                        &mut update.effects,
                    );
                }
                self.sequence = self.sequence.replace_window(
                    window,
                    self.sequence.revision()
                        + u64::from(!matches!(completion, QueueCompletion::Refill)),
                )?;
                update.queue_changed = !matches!(completion, QueueCompletion::Refill);
                update.effects.push(self.state_effect());
                if changed {
                    self.restored_paused = false;
                    let intent = self.queue_transport.take();
                    if !matches!(completion, QueueCompletion::Forget)
                        && intent != Some(TransportStatus::Stopped)
                    {
                        self.begin_selected_run(&mut update.effects);
                        if intent == Some(TransportStatus::Paused) {
                            update.effects.extend(self.set_playing(false).effects);
                        }
                    }
                    update.effects.push(SessionEffect::CurrentMediaChanged);
                } else {
                    self.replan_next_if_changed(&mut update.effects);
                }
                if self.auto_dj_waiting_for_continuation
                    && self.current_run.is_none()
                    && self.sequence.advance_manual().is_some()
                {
                    self.auto_dj_waiting_for_continuation = false;
                    self.begin_selected_run(&mut update.effects);
                }
                self.maybe_request_auto_dj(&mut update.effects);
            }
        }
        self.queue_transport = None;
        while self.pending_queue.is_none()
            && let Some(command) = self.deferred_queue.pop_front()
        {
            let next = self.handle_command(command, sample)?;
            update.effects.extend(next.effects);
            update.view_changed |= next.view_changed;
            update.queue_changed |= next.queue_changed;
        }
        Ok(update)
    }

    pub fn activate_context(
        &mut self,
        context_id: &str,
        media_uri: &str,
        source_rank: usize,
        sample: &ClockSample,
    ) -> Option<SessionUpdate> {
        let index = self
            .sequence
            .context_index(context_id, media_uri, source_rank)?;
        let occurrence = self.sequence.at(index)?.occurrence.clone();
        Some(self.activate_index(index, occurrence, sample))
    }

    fn activate(&mut self, occurrence: &OccurrenceId, sample: &ClockSample) -> SessionUpdate {
        let Some(index) = self.sequence.occurrence_index(occurrence) else {
            return self.request_queue(
                library::QueueEdit::Select(occurrence.clone()),
                QueueCompletion::Select,
            );
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

    fn remove(&mut self, occurrence: &OccurrenceId, _sample: &ClockSample) -> SessionUpdate {
        self.request_queue(
            library::QueueEdit::Remove(vec![occurrence.clone()]),
            QueueCompletion::Preserve,
        )
    }
    fn remove_many(
        &mut self,
        occurrences: &[OccurrenceId],
        _sample: &ClockSample,
    ) -> SessionUpdate {
        self.request_queue(
            library::QueueEdit::Remove(occurrences.to_vec()),
            QueueCompletion::Preserve,
        )
    }
    fn forget(&mut self, occurrences: &[OccurrenceId], _sample: &ClockSample) -> SessionUpdate {
        self.request_queue(
            library::QueueEdit::Remove(occurrences.to_vec()),
            QueueCompletion::Forget,
        )
    }
    fn reorder(
        &mut self,
        occurrences: &[OccurrenceId],
        target: &crate::QueueReorderTarget,
    ) -> SessionUpdate {
        self.request_queue(
            library::QueueEdit::Reorder {
                occurrences: occurrences.to_vec(),
                target: target.clone(),
            },
            QueueCompletion::Preserve,
        )
    }
    fn insert(
        &mut self,
        input: library::QueueInput,
        target: &crate::QueueReorderTarget,
    ) -> SessionUpdate {
        self.request_queue(
            library::QueueEdit::Insert {
                input,
                target: target.clone(),
            },
            QueueCompletion::Preserve,
        )
    }
    fn move_after_current(&mut self, occurrence: &OccurrenceId) -> SessionUpdate {
        self.request_queue(
            library::QueueEdit::MoveAfterCurrent(occurrence.clone()),
            QueueCompletion::Preserve,
        )
    }
    fn clear(&mut self, include_current: bool, _sample: &ClockSample) -> SessionUpdate {
        self.request_queue(
            library::QueueEdit::Clear {
                include_current: include_current || self.current_run.is_none(),
            },
            QueueCompletion::Clear,
        )
    }

    fn play_pause(&mut self) -> SessionUpdate {
        let playing = self
            .current_run
            .as_ref()
            .is_some_and(|run| run.desired_playing);
        self.set_playing(!playing)
    }

    fn set_playing(&mut self, desired_playing: bool) -> SessionUpdate {
        if self.pending_queue.is_some() {
            self.queue_transport = Some(if desired_playing {
                TransportStatus::Playing
            } else {
                TransportStatus::Paused
            });
        }
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
        if self.pending_queue.is_some() {
            self.queue_transport = Some(TransportStatus::Stopped);
        }
        let Some(run) = self.current_run.as_ref() else {
            if self.sequence.progress_millis() == 0 && !self.restored_paused {
                return SessionUpdate::default();
            }
            self.restored_paused = false;
            self.sequence.set_progress_millis(0);
            return SessionUpdate {
                effects: vec![self.progress_effect(), SessionEffect::FlushPersistence],
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
        update.effects.push(SessionEffect::FlushPersistence);
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
        update.effects.push(self.progress_effect());
        update.effects.push(SessionEffect::FlushPersistence);
        update
    }

    fn next(&mut self, sample: &ClockSample) -> SessionUpdate {
        if let Some(index) = self.sequence.next_index(false)
            && self.sequence.at(index).is_none()
        {
            return self.request_queue(
                library::QueueEdit::SelectIndex(index),
                QueueCompletion::Select,
            );
        }
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
        if let Some(index) = self.sequence.previous_index()
            && self.sequence.at(index).is_none()
        {
            return self.request_queue(
                library::QueueEdit::SelectIndex(index),
                QueueCompletion::Select,
            );
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
        self.request_queue(
            library::QueueEdit::Shuffle { enabled, seed },
            QueueCompletion::Preserve,
        )
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
            update.effects.push(SessionEffect::FlushPersistence);
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
        if let Some(index) = self.sequence.next_index_eos()
            && self.sequence.at(index).is_none()
        {
            let mut update = SessionUpdate::changed();
            self.finish_current(RunEndReason::Completed, sample, &mut update.effects);
            if self.pending_queue.is_some() {
                self.deferred_queue.push_back(SessionCommand::Next);
            } else {
                update.effects.extend(
                    self.request_queue(
                        library::QueueEdit::SelectIndex(index),
                        QueueCompletion::Select,
                    )
                    .effects,
                );
            }
            return update;
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
        let mut current = RunContext::resolving(run, self.play_id(run), &entry);
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
        let run = self.next_run_id();
        self.current_run = Some(RunContext::resolving(run, self.play_id(run), &entry));
        effects.push(SessionEffect::CurrentMediaChanged);
        self.next_plan = None;
        self.buffering_percent = None;
        self.last_error = None;
        effects.push(self.resolve_effect(run, &entry));
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
        let request = StreamRequest::for_item(&next.item, self.settings.stream_quality);
        let transition = decided_transition(
            &self.settings,
            self.sequence.selected().map(|entry| &entry.item),
            &next.item,
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
            occurrence: next,
            request,
        });
    }

    fn resolve_effect(&self, run: RunId, entry: &std::sync::Arc<QueueOccurrence>) -> SessionEffect {
        SessionEffect::ResolveStream {
            run,
            occurrence: entry.clone(),
            request: StreamRequest::for_item(&entry.item, self.settings.stream_quality),
        }
    }

    fn mark_started(&mut self, sample: &ClockSample, effects: &mut Vec<SessionEffect>) {
        let (run, occurrence) = {
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
            (current.id, current.occurrence.clone())
        };
        let track = self
            .sequence
            .occurrence(&occurrence)
            .expect("Playback run occurrence")
            .item
            .clone();
        effects.push(SessionEffect::Listening(ListeningFact::Started {
            run,
            started_at_unix_seconds: sample.unix_seconds,
            local_period: sample.local_period.clone(),
            item: Box::new(track),
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
                    current.occurrence.clone(),
                    current.started_at_unix_seconds,
                    current.audible_millis,
                    current.local_period.clone(),
                ))
            } else {
                None
            }
        };
        let activity_qualified = activity.is_some();
        if let Some((play_id, occurrence, Some(started_at), listened_millis, Some(local_period))) =
            activity
        {
            let track = self
                .sequence
                .occurrence(&occurrence)
                .expect("Playback run occurrence")
                .item
                .clone();
            effects.push(SessionEffect::Activity(Box::new(crate::ActivityListen {
                play_id: play_id.clone(),
                item: track.clone(),
                started_at_unix_seconds: started_at,
                local_period,
                listened_millis,
                skipped: false,
            })));
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
        let track = self
            .sequence
            .occurrence(&current.occurrence)
            .expect("Playback run occurrence")
            .item
            .clone();
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
                effects.push(SessionEffect::Activity(Box::new(crate::ActivityListen {
                    play_id: current.play_id,
                    item: track,
                    started_at_unix_seconds: started_at,
                    local_period: period,
                    listened_millis: current.audible_millis,
                    skipped: true,
                })));
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
            media_uri: self
                .sequence
                .occurrence(&current.occurrence)
                .expect("Playback run occurrence")
                .media_uri
                .clone(),
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
        let key = AutoDjKey {
            seed_occurrence: seed.occurrence.clone(),
        };
        self.auto_dj_in_flight = Some(key.clone());
        effects.push(SessionEffect::RequestAutoDj(AutoDjRequest {
            seed_occurrence: key.seed_occurrence,
            seed_media_uri: seed.media_uri.clone(),
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
    current: Option<&QueueItem>,
    next: &QueueItem,
) -> NextTransition {
    match settings.transition_mode {
        PlaybackTransitionMode::Gapless => NextTransition::Gapless,
        PlaybackTransitionMode::Crossfade
            if settings.skip_same_album_crossfade
                && current.is_some_and(|current| {
                    !current.album.is_empty()
                        && current.album == next.album
                        && current.album_display_artist == next.album_display_artist
                }) =>
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

    async fn seeded(items: Vec<BatchItem>) -> (tempfile::TempDir, library::Database, Sequence) {
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("queue.sqlite3"))
            .await
            .unwrap();
        let window = database
            .edit_queue_with_preview(
                library::QueueEdit::Apply {
                    input: Batch::new(items).input,
                    placement: Placement::Now,
                    shuffle_seed: None,
                    random_start: false,
                    identity: None,
                },
                None,
                RepeatMode::Off,
                false,
                0,
                |_| {},
            )
            .await
            .unwrap();
        (
            directory,
            database,
            Sequence::from_window(window, 1).unwrap(),
        )
    }
    async fn finish_queue(
        database: &library::Database,
        session: &mut PlaybackSession,
        mut update: SessionUpdate,
    ) -> SessionUpdate {
        let sample = ClockSample {
            monotonic_millis: 0,
            unix_seconds: 0,
            local_period: "1970-01".into(),
        };
        loop {
            let Some(index) = update
                .effects
                .iter()
                .position(|effect| matches!(effect, SessionEffect::Queue { .. }))
            else {
                return update;
            };
            let SessionEffect::Queue {
                id,
                edit,
                current,
                progress_millis,
                repeat,
                shuffled,
            } = update.effects.remove(index)
            else {
                unreachable!()
            };
            let window = database
                .edit_queue_with_preview(
                    edit,
                    current.as_ref(),
                    repeat,
                    shuffled,
                    progress_millis as i64,
                    |_| {},
                )
                .await
                .unwrap();
            let next = session
                .handle_command(
                    SessionCommand::QueueComplete {
                        id,
                        result: Box::new(Ok(window)),
                    },
                    &sample,
                )
                .unwrap();
            update.view_changed |= next.view_changed;
            update.queue_changed |= next.queue_changed;
            update.queue_persistence_changed |= next.queue_persistence_changed;
            update.effects.extend(next.effects);
        }
    }

    fn restored_item() -> QueueItem {
        let mut item = QueueItem::direct(
            "rufin://source/track/track-1",
            "Track",
            "Artist",
            "Album",
            180_000,
        );
        item.disc_number = Some(1);
        item.track_number = Some(1);
        item
    }

    fn batch_item(track: i64, provenance: Provenance) -> BatchItem {
        let mut item = restored_item();
        item.media_uri = format!("rufin://source/track/track-{track}");
        BatchItem::direct(item, provenance)
    }

    async fn active_two_track_session() -> (tempfile::TempDir, library::Database, PlaybackSession) {
        let (_directory, _database, sequence) = seeded(vec![
            batch_item(1, Provenance::Manual),
            batch_item(2, Provenance::Manual),
        ])
        .await;
        let mut session = PlaybackSession::new(
            sequence,
            "clear-queue-law",
            PlaybackSettings::default(),
            PlaybackOutput::Local,
            false,
            3,
        );
        session.set_playing(true);
        assert!(session.current_run().is_some());
        (_directory, _database, session)
    }

    #[tokio::test]
    async fn transport_and_prepared_stream_share_queue_facts_and_release_them() {
        use std::sync::Arc;

        let (_directory, _database, session) = active_two_track_session().await;
        let entry = session.sequence().selected().expect("selected occurrence");
        let weak = Arc::downgrade(entry);
        let view = session.view();
        let current = view.transport.current.as_ref().expect("Current");
        assert!(Arc::ptr_eq(&current.occurrence, entry));
        let run = session.current_run().expect("run");
        let SessionEffect::ResolveStream { occurrence, .. } = session.resolve_effect(run, entry)
        else {
            panic!("stream preparation");
        };
        assert!(Arc::ptr_eq(&occurrence, entry));
        let stream = PreparedStream::new(
            crate::ResolvedStream::new("https://example.test/audio.flac"),
            crate::TrackLoudness::default(),
        )
        .with_occurrence(occurrence, None);
        for _ in 0..1_000 {
            let projection = session.view();
            assert!(Arc::ptr_eq(
                &projection.transport.current.unwrap().occurrence,
                entry
            ));
        }
        drop(view);
        drop(session);
        assert!(
            weak.upgrade().is_some(),
            "prepared stream retains its addressed occurrence"
        );
        drop(stream);
        assert!(
            weak.upgrade().is_none(),
            "no projection retains retired Queue facts"
        );
    }

    #[tokio::test]
    async fn captured_queue_starts_and_skips_before_persistence_without_restarting_on_completion() {
        let (_directory, database, mut session) = active_two_track_session().await;
        let input = library::QueueInput::Items(
            (0..240)
                .map(|position| {
                    (
                        QueueItem::direct(
                            format!("https://example.test/{position}"),
                            position.to_string(),
                            "",
                            "",
                            120_000,
                        ),
                        Provenance::Manual,
                    )
                })
                .collect(),
        );
        let window = database
            .prepare_queue_window(&input, 120, None, false, "ready")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(window.occurrences.len(), 96);
        let sample = ClockSample {
            monotonic_millis: 1_000,
            unix_seconds: 1,
            local_period: "1970-01".into(),
        };
        let mut pending = session
            .handle_command(
                SessionCommand::ApplyBatch {
                    batch: Batch::from_input(input)
                        .with_prepared_window("ready".into(), Some(window)),
                    placement: Placement::Replace { anchor_index: 120 },
                },
                &sample,
            )
            .unwrap();
        assert!(
            pending
                .effects
                .iter()
                .any(|effect| matches!(effect, SessionEffect::ResolveStream { .. }))
        );
        pending
            .effects
            .retain(|effect| matches!(effect, SessionEffect::Queue { .. }));
        assert_eq!(session.sequence.selected().unwrap().title, "120");
        assert_eq!(session.sequence.total(), 240);
        let prepared = session.view().prepared_queue.unwrap();
        assert!(prepared.len() <= library::QUEUE_CONTEXT_LIMIT);
        assert!(
            prepared
                .iter()
                .all(|entry| entry.occurrence.as_str().starts_with("ready:"))
        );
        assert!(
            prepared
                .iter()
                .any(|entry| Arc::ptr_eq(entry, session.sequence.selected().unwrap()))
        );
        for (command, expected) in [
            (SessionCommand::Next, "121"),
            (SessionCommand::Previous, "120"),
        ] {
            let update = session.handle_command(command, &sample).unwrap();
            assert!(
                !update
                    .effects
                    .iter()
                    .any(|effect| matches!(effect, SessionEffect::Queue { .. }))
            );
            assert_eq!(session.sequence.selected().unwrap().title, expected);
        }
        let run = session.current_run().unwrap();
        session.handle_backend(BackendEvent::Ended { run }, &sample);
        assert_eq!(session.sequence.selected().unwrap().title, "121");
        let run = session.current_run().unwrap();
        session.handle_backend(
            BackendEvent::Seekable {
                run,
                seekable: true,
            },
            &sample,
        );
        let seek = session
            .handle_command(SessionCommand::Seek(12_000), &sample)
            .unwrap();
        assert!(seek.effects.iter().any(|effect| matches!(
            effect,
            SessionEffect::Backend(BackendCommand::Seek {
                position_millis: 12_000,
                ..
            })
        )));
        session.handle_backend(
            BackendEvent::Position {
                run,
                millis: 12_000,
            },
            &sample,
        );
        session
            .handle_command(SessionCommand::Pause, &sample)
            .unwrap();
        let current_run = session.current_run();
        let current = session.sequence.selected().unwrap().occurrence.clone();
        let next_run = session.next_plan.as_ref().map(|plan| plan.next_run);
        let completed = finish_queue(&database, &mut session, pending).await;
        assert!(!completed.effects.iter().any(|effect| matches!(
            effect,
            SessionEffect::ResolveStream { .. }
                | SessionEffect::Backend(BackendCommand::Stop { .. })
        )));
        assert_eq!(session.current_run(), current_run);
        assert_eq!(
            session.next_plan.as_ref().map(|plan| plan.next_run),
            next_run
        );
        assert_eq!(session.sequence.selected().unwrap().occurrence, current);
        assert_eq!(session.position_millis(), 12_000);
        assert!(!session.current_run.as_ref().unwrap().desired_playing);
        assert_eq!(database.restore_queue().await.unwrap().total, 240);
        assert!(session.view().prepared_queue.is_none());
        assert!(session.sequence.is_durable());
    }

    #[tokio::test]
    async fn prepared_queue_wrap_selection_survives_the_durable_window_handoff() {
        let (_directory, database, mut session) = active_two_track_session().await;
        session.sequence.set_repeat_mode(RepeatMode::All);
        let batch = Batch::new(
            (0..240)
                .map(|index| batch_item(index, Provenance::Manual))
                .collect(),
        );
        let window = database
            .prepare_queue_window(batch.input(), 0, None, false, "wrap")
            .await
            .unwrap();
        let sample = ClockSample {
            monotonic_millis: 1_000,
            unix_seconds: 1,
            local_period: "1970-01".into(),
        };
        let pending = session
            .handle_command(
                SessionCommand::ApplyBatch {
                    batch: batch.with_prepared_window("wrap".into(), window),
                    placement: Placement::Replace { anchor_index: 0 },
                },
                &sample,
            )
            .unwrap();
        session
            .handle_command(SessionCommand::Previous, &sample)
            .unwrap();
        assert_eq!(
            session.sequence.selected().unwrap().occurrence.as_str(),
            "wrap:239"
        );
        let run = session.current_run();
        let completed = finish_queue(&database, &mut session, pending).await;
        assert!(!completed.effects.iter().any(|effect| matches!(effect, SessionEffect::Backend(BackendCommand::Stop { run: stopped }) if Some(*stopped) == run)));
        assert_eq!(session.current_run(), run);
        assert_eq!(
            session.sequence.selected().unwrap().occurrence.as_str(),
            "wrap:239"
        );
        assert!(session.view().prepared_queue.is_none());
    }

    #[tokio::test]
    async fn failed_queue_write_keeps_the_prepared_run_and_rows_until_a_successful_replacement() {
        let (_directory, database, mut session) = active_two_track_session().await;
        let stored = database.restore_queue().await.unwrap();
        let sample = ClockSample {
            monotonic_millis: 1_000,
            unix_seconds: 1,
            local_period: "1970-01".into(),
        };
        let mut invalid = batch_item(20, Provenance::Manual);
        invalid.item.disc_number = Some(-1);
        let batch = Batch::new(vec![batch_item(10, Provenance::Manual), invalid]);
        let prepared = database
            .prepare_queue_window(batch.input(), 0, None, false, "failed")
            .await
            .unwrap()
            .unwrap();
        let mut pending = session
            .handle_command(
                SessionCommand::ApplyBatch {
                    batch: batch.with_prepared_window("failed".into(), Some(prepared)),
                    placement: Placement::Replace { anchor_index: 0 },
                },
                &sample,
            )
            .unwrap();
        let request = pending
            .effects
            .iter()
            .position(|effect| matches!(effect, SessionEffect::Queue { .. }))
            .unwrap();
        let SessionEffect::Queue {
            id,
            edit,
            current,
            progress_millis,
            repeat,
            shuffled,
        } = pending.effects.remove(request)
        else {
            unreachable!()
        };
        let error = database
            .edit_queue_with_preview(
                edit,
                current.as_ref(),
                repeat,
                shuffled,
                progress_millis as i64,
                |_| {},
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("CHECK constraint failed"));
        let run = session.current_run();
        let prepared = session.view().prepared_queue.unwrap();
        let failed = session
            .handle_command(
                SessionCommand::QueueComplete {
                    id,
                    result: Box::new(Err(error.to_string())),
                },
                &sample,
            )
            .unwrap();
        assert!(
            failed
                .effects
                .iter()
                .any(|effect| matches!(effect, SessionEffect::NonfatalError(_)))
        );
        assert_eq!(session.current_run(), run);
        assert_eq!(session.view().prepared_queue.as_ref(), Some(&prepared));
        let rows = database.prepared_queue_page(&prepared, "").await.unwrap();
        assert!(
            rows.iter()
                .all(|row| row.occurrence.as_str().starts_with("failed:"))
        );
        assert!(session.refill_queue().is_none());
        let current = session.sequence.selected().unwrap().occurrence.clone();
        for command in [
            SessionCommand::Remove(current.clone()),
            SessionCommand::Activate(OccurrenceId::new("old-missing")),
        ] {
            let edit = session.handle_command(command, &sample).unwrap();
            assert!(
                edit.effects
                    .iter()
                    .any(|effect| matches!(effect, SessionEffect::NonfatalError(_)))
            );
            assert!(
                !edit
                    .effects
                    .iter()
                    .any(|effect| matches!(effect, SessionEffect::Queue { .. }))
            );
            assert_eq!(session.current_run(), run);
        }
        session
            .handle_command(
                SessionCommand::Activate(prepared[1].occurrence.clone()),
                &sample,
            )
            .unwrap();
        assert_eq!(
            session.sequence.selected().unwrap().occurrence,
            prepared[1].occurrence
        );
        session
            .handle_command(SessionCommand::Previous, &sample)
            .unwrap();
        assert_eq!(session.sequence.selected().unwrap().occurrence, current);
        assert_eq!(database.restore_queue().await.unwrap(), stored);
        let batch = Batch::new(vec![batch_item(30, Provenance::Manual)]);
        let prepared = database
            .prepare_queue_window(batch.input(), 0, None, false, "recovered")
            .await
            .unwrap();
        let pending = session
            .handle_command(
                SessionCommand::ApplyBatch {
                    batch: batch.with_prepared_window("recovered".into(), prepared),
                    placement: Placement::Replace { anchor_index: 0 },
                },
                &sample,
            )
            .unwrap();
        finish_queue(&database, &mut session, pending).await;
        assert!(session.view().prepared_queue.is_none());
        let edit = session
            .handle_command(SessionCommand::Remove(current), &sample)
            .unwrap();
        assert!(
            edit.effects
                .iter()
                .any(|effect| matches!(effect, SessionEffect::Queue { .. }))
        );
    }

    #[tokio::test]
    async fn newer_captured_play_supersedes_pending_completion_and_stop_is_preserved() {
        let (_directory, database, mut session) = active_two_track_session().await;
        let sample = ClockSample {
            monotonic_millis: 1_000,
            unix_seconds: 1,
            local_period: "1970-01".into(),
        };
        let mut updates = Vec::new();
        for identity in ["first", "latest"] {
            let input = library::QueueInput::Items(vec![(
                QueueItem::direct(
                    format!("https://example.test/{identity}"),
                    identity,
                    "",
                    "",
                    120_000,
                ),
                Provenance::Manual,
            )]);
            let window = database
                .prepare_queue_window(&input, 0, None, false, identity)
                .await
                .unwrap();
            updates.push(
                session
                    .handle_command(
                        SessionCommand::ApplyBatch {
                            batch: Batch::from_input(input)
                                .with_prepared_window(identity.into(), window),
                            placement: Placement::Now,
                        },
                        &sample,
                    )
                    .unwrap(),
            );
            assert_eq!(session.sequence.selected().unwrap().title, identity);
        }
        let run = session.current_run();
        finish_queue(&database, &mut session, updates.remove(0)).await;
        assert_eq!(session.current_run(), run);
        assert_eq!(session.sequence.selected().unwrap().title, "latest");
        session
            .handle_command(SessionCommand::Stop, &sample)
            .unwrap();
        finish_queue(&database, &mut session, updates.remove(0)).await;
        assert_eq!(session.current_run(), None);
        assert_eq!(session.sequence.selected().unwrap().title, "latest");
    }

    #[tokio::test]
    async fn artwork_publication_preserves_run_progress_prepared_next_and_occurrences() {
        let (_directory, _database, mut session) = active_two_track_session().await;
        let sample = ClockSample {
            monotonic_millis: 1_000,
            unix_seconds: 1,
            local_period: "1970-01".into(),
        };
        let before = session.view();
        let run = session.current_run();
        let next_run = session.next_plan.as_ref().map(|plan| plan.next_run);
        let uris = session.sequence.artwork_uris();
        let update = session
            .handle_command(SessionCommand::CatalogChanged, &sample)
            .unwrap();
        assert_eq!(
            update.effects,
            vec![SessionEffect::RefreshArtwork(uris.clone())]
        );
        let bindings = uris
            .into_iter()
            .map(|uri| (uri, Some(vec![1, 2, 3])))
            .collect::<Vec<_>>();
        let update = session
            .handle_command(SessionCommand::ArtworkRefreshed(bindings.clone()), &sample)
            .unwrap();
        assert!(update.view_changed);
        assert!(!update.queue_changed);
        assert!(
            update.effects.is_empty(),
            "presentation does not restart or reprepare audio"
        );
        let after = session.view();
        assert_eq!(after.queue, before.queue);
        assert_eq!(
            after.transport.position_millis,
            before.transport.position_millis
        );
        assert_eq!(
            after.transport.current.as_ref().unwrap().id,
            before.transport.current.as_ref().unwrap().id
        );
        assert_eq!(session.current_run(), run);
        assert_eq!(
            session.next_plan.as_ref().map(|plan| plan.next_run),
            next_run
        );
        assert_eq!(
            after.transport.current.unwrap().artwork_binding,
            Some(vec![1, 2, 3])
        );
        assert_eq!(
            session
                .handle_command(SessionCommand::ArtworkRefreshed(bindings), &sample)
                .unwrap(),
            SessionUpdate::default()
        );
    }

    #[tokio::test]
    async fn clearing_queue_without_current_keeps_the_active_run() {
        let (_directory, _database, mut session) = active_two_track_session().await;
        let run = session.current_run().expect("active run");

        let update = session
            .handle_command(
                SessionCommand::Clear {
                    include_current: false,
                },
                &ClockSample {
                    monotonic_millis: 0,
                    unix_seconds: 0,
                    local_period: "1970-01".to_string(),
                },
            )
            .expect("clear upcoming Queue");
        let update = finish_queue(&_database, &mut session, update).await;

        assert_eq!(session.sequence().entries().len(), 1);
        assert_eq!(session.current_run(), Some(run));
        assert!(session.view().transport.current.is_some());
        assert!(
            !update.effects.iter().any(|effect| matches!(
                effect,
                SessionEffect::Backend(BackendCommand::Stop { .. })
            ))
        );
    }

    #[tokio::test]
    async fn stream_access_changes_and_unchanged_settings_leave_the_audible_run_untouched() {
        let (_directory, _database, mut session) = active_two_track_session().await;
        let sample = ClockSample {
            monotonic_millis: 10_000,
            unix_seconds: 10,
            local_period: "1970-01".into(),
        };
        let run = session.current_run().unwrap();
        session.stream_resolved(
            run,
            crate::ResolvedStream::new("https://music.example/track.flac"),
        );
        session.handle_backend(BackendEvent::Started { run }, &sample);
        let occurrence = session.sequence.selected().unwrap().occurrence.clone();
        session.sequence.set_progress_millis(42_000);
        for command in [
            SessionCommand::StreamInputsChanged,
            SessionCommand::UpdateSettings(session.settings.clone()),
        ] {
            let update = session.handle_command(command, &sample).unwrap();
            assert!(update.effects.iter().all(|effect| !matches!(effect,
                SessionEffect::Backend(command) if !matches!(command, BackendCommand::PrepareNext { .. })
            )), "background access changes may only replace prepared-next");
            assert_eq!(session.current_run(), Some(run));
            assert_eq!(session.sequence.selected().unwrap().occurrence, occurrence);
            assert_eq!(session.sequence.progress_millis(), 42_000);
        }
    }

    #[tokio::test]
    async fn clearing_queue_with_current_stops_the_run_and_empties_the_session() {
        let (_directory, _database, mut session) = active_two_track_session().await;
        let run = session.current_run().expect("active run");

        let update = session
            .handle_command(
                SessionCommand::Clear {
                    include_current: true,
                },
                &ClockSample {
                    monotonic_millis: 0,
                    unix_seconds: 0,
                    local_period: "1970-01".to_string(),
                },
            )
            .expect("clear complete Queue");
        let update = finish_queue(&_database, &mut session, update).await;

        assert!(session.sequence().entries().is_empty());
        assert_eq!(session.current_run(), None);
        assert!(session.sequence().selected().is_none());
        assert_eq!(session.status(), TransportStatus::Stopped);
        assert!(session.view().transport.current.is_none());
        assert!(update.queue_changed);
        assert_eq!(_database.restore_queue().await.unwrap().total, 0);
        assert!(update.effects.iter().any(|effect| matches!(
            effect,
            SessionEffect::Backend(BackendCommand::Stop { run: stopped }) if *stopped == run
        )));
    }

    #[tokio::test]
    async fn restored_media_stays_paused_until_first_play_starts_one_resolution() {
        let (_directory, _database, mut sequence) =
            seeded(vec![batch_item(1, Provenance::Manual)]).await;
        sequence.set_progress_millis(42_000);
        let mut session = PlaybackSession::new(
            sequence,
            "restore-law",
            PlaybackSettings::default(),
            PlaybackOutput::Local,
            false,
            3,
        );
        assert_eq!(session.current_run(), None);
        assert_eq!(session.status(), TransportStatus::Paused);
        assert_eq!(session.position_millis(), 42_000);
        assert!(session.view().transport.current.is_some());

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
        assert_eq!(
            session.view().transport.effective_state(),
            TransportStatus::Resolving
        );
    }

    #[tokio::test]
    async fn playback_modes_publish_their_state_without_an_active_run() {
        let _directory = tempfile::tempdir().unwrap();
        let _database = library::Database::open(_directory.path().join("queue.sqlite3"))
            .await
            .unwrap();
        let mut session = PlaybackSession::new(
            Sequence::new(),
            "mode-law",
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

        let shuffle = session
            .handle_command(
                SessionCommand::SetShuffle {
                    enabled: true,
                    seed: 7,
                },
                &sample,
            )
            .expect("enable shuffle");
        let shuffle = finish_queue(&_database, &mut session, shuffle).await;
        assert!(shuffle.view_changed);
        assert!(session.view().controls.shuffle_enabled);

        let repeat = session
            .handle_command(SessionCommand::SetRepeat(RepeatMode::One), &sample)
            .expect("enable repeat");
        assert!(repeat.view_changed);
        assert_eq!(session.view().controls.repeat_mode, RepeatMode::One);

        let auto_dj = session
            .handle_command(
                SessionCommand::SetAutoDj {
                    enabled: true,
                    refill_threshold: 3,
                },
                &sample,
            )
            .expect("enable Auto DJ");
        assert!(auto_dj.view_changed);
        assert!(session.view().controls.auto_dj_enabled);
    }

    #[tokio::test]
    async fn collection_starts_and_selected_rows_keep_distinct_shuffle_anchors() {
        let sample = ClockSample {
            monotonic_millis: 0,
            unix_seconds: 0,
            local_period: "1970-01".to_string(),
        };
        for shuffled in [false, true] {
            for (collection_start, anchor) in [(true, 0), (false, 0), (false, 1)] {
                let items = (0..4)
                    .map(|rank| {
                        batch_item(
                            rank as i64 + 1,
                            Provenance::Context {
                                context_id: "collection".into(),
                                source_rank: rank,
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                let (_directory, database, mut sequence) = seeded(items.clone()).await;
                sequence.set_progress_millis(42_000);
                let mut session = PlaybackSession::new(
                    sequence,
                    "collection-start-law",
                    PlaybackSettings::default(),
                    PlaybackOutput::Local,
                    false,
                    3,
                );
                let update = session
                    .handle_command(
                        SessionCommand::SetShuffle {
                            enabled: shuffled,
                            seed: 7,
                        },
                        &sample,
                    )
                    .unwrap();
                finish_queue(&database, &mut session, update).await;
                session
                    .handle_command(SessionCommand::Play, &sample)
                    .unwrap();
                let previous_run = session.current_run();
                assert_eq!(session.sequence().selected().unwrap().canonical_position, 0);

                let request = if collection_start {
                    crate::PlayRequest::ordered
                } else {
                    crate::PlayRequest::captured
                };
                let request = request(
                    Batch::new(items).input,
                    anchor,
                    Placement::Now,
                    collection_start && shuffled,
                );
                assert_eq!(request.activation_context().is_none(), collection_start);
                let window = database
                    .prepare_queue_window(
                        request.batch.input(),
                        anchor,
                        shuffled.then_some(2),
                        request.shuffled_start,
                        "restarted",
                    )
                    .await
                    .unwrap();
                let (batch, placement) = request.compact_batch(2);
                let reservation = session.reserve_materialization(placement);
                let update = session
                    .apply_materialization(
                        reservation.id,
                        batch.with_prepared_window("restarted".into(), window),
                        placement,
                        &sample,
                    )
                    .unwrap()
                    .unwrap();
                let expected = if collection_start && shuffled {
                    2
                } else {
                    anchor
                };
                assert_eq!(
                    session.sequence().selected().unwrap().canonical_position,
                    expected
                );
                assert_eq!(session.position_millis(), 0);
                assert_ne!(session.current_run(), previous_run);
                let update = finish_queue(&database, &mut session, update).await;
                let selected = session.sequence().selected().unwrap();
                assert_eq!(selected.canonical_position, expected);
                assert_eq!(session.view().controls.shuffle_enabled, shuffled);
                let restored = database.restore_queue().await.unwrap();
                assert_eq!(
                    restored.current_occurrence.as_ref(),
                    Some(&selected.occurrence)
                );
                assert_eq!(restored.shuffled, shuffled);
                assert!(update.effects.iter().any(|effect| matches!(
                    effect,
                    SessionEffect::ResolveStream { occurrence, .. }
                        if occurrence.occurrence == selected.occurrence
                )));
            }
        }
    }

    #[tokio::test]
    async fn gapless_preparation_resolves_exactly_the_next_occurrence() {
        let (_directory, _database, sequence) = seeded(vec![
            batch_item(1, Provenance::Manual),
            batch_item(2, Provenance::Manual),
        ])
        .await;
        let current = sequence.entries()[0].occurrence.clone();
        let next = sequence.entries()[1].occurrence.clone();
        let mut session = PlaybackSession::new(
            sequence,
            "gapless-law",
            PlaybackSettings::default(),
            PlaybackOutput::Local,
            false,
            3,
        );
        let current_update = session.set_playing(true);
        let current_run = current_update
            .effects
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::ResolveStream {
                    run, occurrence, ..
                } if occurrence.occurrence == current => Some(*run),
                _ => None,
            })
            .expect("current resolution");
        session.stream_resolved(
            current_run,
            crate::ResolvedStream::new("file:///current.flac"),
        );
        let next_run = current_update
            .effects
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::ResolveStream {
                    run, occurrence, ..
                } if occurrence.occurrence == next => Some(*run),
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

    #[tokio::test]
    async fn one_track_auto_dj_completion_publishes_appended_queue_entries() {
        let (_directory, _database, sequence) =
            seeded(vec![batch_item(1, Provenance::Manual)]).await;
        let mut session = PlaybackSession::new(
            sequence,
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
                &request.seed_occurrence,
                vec![
                    batch_item(2, Provenance::AutoDj).item,
                    batch_item(3, Provenance::AutoDj).item,
                ],
                request.requested_count,
                7,
                &sample,
            )
            .expect("complete AutoDJ")
            .expect("accepted AutoDJ completion");
        let completed = finish_queue(&_database, &mut session, completed).await;
        assert!(completed.queue_changed);
        assert_eq!(_database.restore_queue().await.unwrap().total, 3);
        assert_eq!(session.view().queue.total, 3);
    }

    #[tokio::test]
    async fn failed_stream_preserves_selected_media_and_player_controls() {
        let (_directory, _database, sequence) =
            seeded(vec![batch_item(1, Provenance::Manual)]).await;
        let mut session = PlaybackSession::new(
            sequence,
            "failure-law",
            PlaybackSettings::default(),
            PlaybackOutput::Local,
            false,
            3,
        );
        let update = session.set_playing(true);
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

    #[tokio::test]
    async fn playback_crosses_window_edges_and_wraps_without_truncating_the_queue() {
        let (_directory, database, sequence) = seeded(
            (0..500)
                .map(|rank| batch_item(rank, Provenance::Manual))
                .collect(),
        )
        .await;
        let mut session = PlaybackSession::new(
            sequence,
            "full-traversal",
            PlaybackSettings::default(),
            PlaybackOutput::Local,
            false,
            3,
        );
        let sample = ClockSample {
            monotonic_millis: 0,
            unix_seconds: 0,
            local_period: "1970-01".into(),
        };
        session
            .handle_command(SessionCommand::SetRepeat(RepeatMode::All), &sample)
            .unwrap();
        session.set_playing(true);
        for rank in 0..500 {
            assert_eq!(
                session.sequence().selected().unwrap().media_uri,
                format!("rufin://source/track/track-{rank}")
            );
            assert_eq!(session.sequence().total(), 500);
            assert!(session.sequence().entries().len() <= 96);
            let next = session
                .handle_command(SessionCommand::Next, &sample)
                .unwrap();
            finish_queue(&database, &mut session, next).await;
            if let Some(effect) = session.refill_queue() {
                finish_queue(
                    &database,
                    &mut session,
                    SessionUpdate {
                        effects: vec![effect],
                        ..SessionUpdate::default()
                    },
                )
                .await;
            }
        }
        assert_eq!(session.sequence().selected_index(), Some(0));
        let previous = session
            .handle_command(SessionCommand::Previous, &sample)
            .unwrap();
        finish_queue(&database, &mut session, previous).await;
        assert_eq!(session.sequence().selected_index(), Some(499));
        assert_eq!(
            session.sequence().selected().unwrap().media_uri,
            "rufin://source/track/track-499"
        );
    }
    #[tokio::test]
    async fn backend_advancing_during_removal_uses_the_exact_surviving_successor() {
        for removed_rank in [1, 2] {
            let (_directory, database, sequence) = seeded(
                (0..3)
                    .map(|rank| batch_item(rank, Provenance::Manual))
                    .collect(),
            )
            .await;
            let mut session = PlaybackSession::new(
                sequence,
                "removal-transition",
                PlaybackSettings::default(),
                PlaybackOutput::Local,
                false,
                3,
            );
            let sample = ClockSample {
                monotonic_millis: 0,
                unix_seconds: 0,
                local_period: "1970-01".into(),
            };
            session.sequence.activate_index(removed_rank - 1);
            session.set_playing(true);
            let removed = session
                .sequence
                .at(removed_rank)
                .unwrap()
                .occurrence
                .clone();
            let request = session
                .handle_command(SessionCommand::Remove(removed), &sample)
                .unwrap();
            let old_run = session.current_run().unwrap();
            session.accept_ended(old_run, &sample);
            assert_eq!(session.sequence.selected_index(), Some(removed_rank));
            finish_queue(&database, &mut session, request).await;
            if removed_rank == 1 {
                assert_eq!(
                    session.sequence.selected().unwrap().media_uri,
                    "rufin://source/track/track-2"
                );
            } else {
                assert!(session.sequence.selected().is_none());
                assert!(session.current_run().is_none());
            }
        }
    }

    #[tokio::test]
    async fn pause_and_stop_during_queue_preparation_apply_to_the_requested_track() {
        for stop in [false, true] {
            let (_directory, database, mut session) = active_two_track_session().await;
            let sample = ClockSample {
                monotonic_millis: 0,
                unix_seconds: 0,
                local_period: "1970-01".into(),
            };
            let request = session
                .apply_batch(
                    Batch::new(vec![batch_item(99, Provenance::Manual)]),
                    Placement::Now,
                    &sample,
                )
                .unwrap();
            if stop {
                session.stop(&sample);
            } else {
                session.set_playing(false);
            }
            finish_queue(&database, &mut session, request).await;
            assert_eq!(
                session.sequence.selected().unwrap().media_uri,
                "rufin://source/track/track-99"
            );
            assert_eq!(
                session.status(),
                if stop {
                    TransportStatus::Stopped
                } else {
                    TransportStatus::Paused
                }
            );
        }
    }
}
