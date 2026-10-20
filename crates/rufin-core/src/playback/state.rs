use super::{
    PlaybackOwner,
    actor::{PlaybackError, WeakPlayback},
    queue::{OccurrenceId, Sequence, SequenceError},
};
use crate::playback::{QueueReorderTarget, QueueSelection};
use playback::{
    BackendFailure, ControlsView, CurrentMedia, CurrentMediaId, PlaybackBackend, PlaybackNotice,
    PlaybackProjection, PlaybackView, PositionDiscontinuity, SourceReportFact, SourceReportPhase,
    TransportStatus, TransportView,
};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};
use tokio::task::JoinHandle;

use crate::playback::Placement;
use playback::{
    BackendCommand, BackendEvent, BackendState, NextTransition, PlaybackOutput, PlaybackSettings,
    PlaybackTransitionMode, PreparedNext, PreparedStream, Provenance, QueueRow, RepeatMode,
    RunEndReason, RunId, StreamRequest, manual_end_is_skip, qualified_play_threshold_millis,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClockSample {
    pub monotonic_millis: u64,
    pub unix_seconds: i64,
    pub local_period: String,
}

/// A receiver observation, applied without issuing transport commands or reporting
/// another player's listen as Rufin activity. Queue metadata remains windowed.
#[derive(Clone, Debug, PartialEq)]
pub struct ExternalPlayback {
    pub queue: Option<library::QueueRestore>,
    pub queue_total: usize,
    pub queue_offset: usize,
    pub status: TransportStatus,
    pub position_millis: Option<u64>,
    pub duration_millis: u64,
    pub volume: f64,
    pub muted: bool,
    pub repeat: RepeatMode,
    pub shuffle: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoDjRequest {
    pub seed_run: Option<RunId>,
    pub seed_media_uri: String,
    pub requested_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Radio(crate::playback::RadioPlayRequest),
    Random(crate::playback::RandomPlayRequest),
    OperationFailed(String),
    ObserveExternal {
        output: PlaybackOutput,
        observation: ExternalPlayback,
    },
    ApplyQueue(crate::playback::PlayRequest),
    Activate(usize),
    ActivateOccurrence(u64),
    Remove(usize),
    RemoveMany(QueueSelection),
    Forget(Vec<library::TrackKey>),
    Reorder {
        selection: QueueSelection,
        target: crate::playback::QueueReorderTarget,
    },
    Insert {
        input: library::QueueLoad,
        target: crate::playback::QueueReorderTarget,
    },
    MoveAfterCurrent(usize),
    MoveOccurrenceAfterCurrent(u64),
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
    PersistVolume(f64),
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
    RekeyTracks(Arc<[(library::TrackKey, library::TrackKey)]>),
}

impl Command {
    pub(super) fn addresses_occurrence(&self) -> bool {
        matches!(
            self,
            Self::ActivateOccurrence(_)
                | Self::MoveOccurrenceAfterCurrent(_)
                | Self::RemoveMany(QueueSelection::Occurrences(_))
                | Self::Reorder {
                    selection: QueueSelection::Occurrences(_),
                    ..
                }
                | Self::Reorder {
                    target: QueueReorderTarget::BeforeOccurrence(_)
                        | QueueReorderTarget::AfterOccurrence(_),
                    ..
                }
                | Self::Insert {
                    target: QueueReorderTarget::BeforeOccurrence(_)
                        | QueueReorderTarget::AfterOccurrence(_),
                    ..
                }
        )
    }
}

#[derive(Clone, Debug)]
struct RunContext {
    id: RunId,
    play_id: String,
    row: Arc<QueueRow>,
    status: TransportStatus,
    duration_millis: u64,
    audible_millis: u64,
    started_at_unix_seconds: Option<i64>,
    local_period: Option<String>,
    last_monotonic_millis: Option<u64>,
    position_observed_at_millis: u64,
    qualified: bool,
    last_progress_bucket: Option<u64>,
    desired_playing: bool,
    resolved_stream: Option<PreparedStream>,
    backend_loaded: bool,
    seekable: bool,
}

impl RunContext {
    fn resolving(id: RunId, play_id: String, entry: &Arc<QueueRow>) -> Self {
        Self {
            id,
            play_id,
            row: entry.clone(),
            duration_millis: entry.track.metadata.duration_millis.max(0) as u64,
            status: TransportStatus::Resolving,
            audible_millis: 0,
            started_at_unix_seconds: None,
            local_period: None,
            last_monotonic_millis: None,
            position_observed_at_millis: 0,
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

#[derive(Clone, Debug)]
struct FailedRun {
    desired_playing: bool,
}

#[derive(Clone, Debug)]
enum QueueCompletion {
    Replace,
    Insert { target: QueueReorderTarget },
}

pub(super) struct Player {
    owner: Weak<PlaybackOwner>,
    playback: WeakPlayback,
    backend: Box<dyn PlaybackBackend>,
    backend_errors: Vec<BackendEvent>,
    stream_tasks: HashMap<RunId, JoinHandle<()>>,
    dirty: bool,
    current_media_changed: bool,
    notices: Vec<PlaybackNotice>,
    external: Option<ExternalPlayback>,
    external_clock_millis: u64,
    sequence: Sequence,
    pending_queue: Option<(u64, QueueCompletion, JoinHandle<()>)>,
    pending_hydration: Option<JoinHandle<()>>,
    queue_loading: bool,
    next_queue_request: u64,
    queue_transport: Option<TransportStatus>,
    deferred_queue: VecDeque<Command>,
    play_id_prefix: Arc<str>,
    current_run: Option<RunContext>,
    failed_run: Option<FailedRun>,
    next_plan: Option<NextPlan>,
    restored_paused: bool,
    next_run_number: u64,
    candidate_tasks: HashMap<u64, (Placement, JoinHandle<()>)>,
    settings: PlaybackSettings,
    playback_output: PlaybackOutput,
    output_volume: f64,
    output_muted: bool,
    auto_dj_enabled: bool,
    auto_dj_refill_threshold: usize,
    auto_dj_in_flight: Option<AutoDjRequest>,
    auto_dj_waiting_for_continuation: bool,
    buffering_percent: Option<u8>,
    last_error: Option<String>,
}

impl Player {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        sequence: Sequence,
        play_id_prefix: impl Into<Arc<str>>,
        mut settings: PlaybackSettings,
        playback_output: PlaybackOutput,
        auto_dj_enabled: bool,
        auto_dj_refill_threshold: usize,
        backend: Box<dyn PlaybackBackend>,
        owner: Weak<PlaybackOwner>,
        playback: WeakPlayback,
    ) -> Self {
        settings.sanitize();
        let output_volume = settings.volume;
        let output_muted = settings.muted;
        let restored_paused = sequence.selected().is_some();
        Self {
            owner,
            playback,
            backend,
            backend_errors: Vec::new(),
            stream_tasks: HashMap::new(),
            dirty: false,
            current_media_changed: false,
            notices: Vec::new(),
            external: None,
            external_clock_millis: 0,
            sequence,
            pending_queue: None,
            pending_hydration: None,
            queue_loading: false,
            next_queue_request: 1,
            queue_transport: None,
            deferred_queue: VecDeque::new(),
            play_id_prefix: play_id_prefix.into(),
            current_run: None,
            failed_run: None,
            next_plan: None,
            restored_paused,
            next_run_number: 1,
            candidate_tasks: HashMap::new(),
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

    pub(super) fn stream_runs(&self) -> [Option<RunId>; 2] {
        [
            self.current_run.as_ref().map(|run| run.id),
            self.next_plan.as_ref().map(|plan| plan.next_run),
        ]
    }

    pub(super) fn sequence(&self) -> &Sequence {
        &self.sequence
    }

    pub(super) fn page_entries(
        &self,
        offset: usize,
        limit: usize,
    ) -> (Vec<(usize, Option<u64>, library::QueueEntry)>, usize) {
        let rows = self
            .sequence
            .page_entries(offset, limit)
            .into_iter()
            .map(|(position, entry)| {
                let id = self
                    .external
                    .is_none()
                    .then(|| self.sequence.id_at(position).map(|id| id.0))
                    .flatten();
                (position, id, entry)
            })
            .collect();
        (rows, self.sequence.total())
    }

    pub(super) fn continuation(&mut self, sample: &ClockSample) -> Option<playback::Continuation> {
        if self.status() == TransportStatus::Stopped {
            return None;
        }
        self.advance_external_clock(sample);
        if let Some(current) = self.current_run.as_mut() {
            current.advance_clock(sample.monotonic_millis);
        }
        let current = self.sequence.selected_index()?;
        Some(playback::Continuation {
            header: playback::ContinuationHeader {
                total: self.sequence.total(),
                current,
                position_millis: self.sequence.progress_millis(),
                repeat: self.sequence.repeat_mode(),
                shuffled: self.sequence.shuffle_enabled(),
                auto_dj: self.auto_dj_enabled,
                auto_dj_refill_threshold: self.auto_dj_refill_threshold,
                playback_rate: self.settings.playback_rate,
                desired_playing: self.desired_playing(),
                listen: self
                    .current_run
                    .as_ref()
                    .map(|run| playback::ContinuedListen {
                        play_id: run.play_id.clone(),
                        started_at_unix_seconds: run.started_at_unix_seconds,
                        local_period: run.local_period.clone(),
                        audible_millis: run.audible_millis,
                        qualified: run.qualified,
                    }),
            },
            queue: self.sequence.snapshot(),
        })
    }

    pub(super) fn stop_continuation(
        &mut self,
        expected: &playback::ContinuationHeader,
        sample: &ClockSample,
    ) -> Option<playback::ContinuationHeader> {
        let Some(current) = self.continuation(sample) else {
            return None;
        };
        if current.header.current != expected.current
            || current.header.listen.as_ref().map(|listen| &listen.play_id)
                != expected.listen.as_ref().map(|listen| &listen.play_id)
        {
            return None;
        }

        self.qualify_current();
        let header = self.continuation(sample).map(|state| state.header);
        self.sequence.set_selected_duration(self.duration_millis());
        self.stop(sample, false);

        header
    }

    pub(super) fn adopt_continuation(
        &mut self,
        continuation: playback::Continuation,
        stream: PreparedStream,
        backend: Box<dyn PlaybackBackend>,
        sample: &ClockSample,
    ) -> Result<(), PlaybackError> {
        let playback::Continuation { header, mut queue } = continuation;
        queue.current_index = Some(header.current);
        queue.progress_millis = header.position_millis.min(i64::MAX as u64) as i64;
        queue.repeat_mode = header.repeat;
        queue.shuffled = header.shuffled;
        let sequence =
            Sequence::from_window(queue, self.sequence.revision() + 1, self.sequence.next_id())?;
        if sequence.selected().is_none() {
            return Err(SequenceError::MissingSelectedEntry.into());
        }

        self.swap_backend(backend)?;
        self.finish_current(RunEndReason::Replaced, sample);
        self.cancel_queue_read();
        self.cancel_candidate_tasks(false);
        self.deferred_queue.clear();
        self.queue_loading = false;
        self.queue_transport = None;
        self.settings.playback_rate = header.playback_rate;
        self.auto_dj_enabled = header.auto_dj;
        self.auto_dj_refill_threshold = header.auto_dj_refill_threshold;
        self.adopt_sequence(sequence, stream, header.desired_playing);
        if let (Some(current), Some(listen)) = (self.current_run.as_mut(), header.listen) {
            current.play_id = listen.play_id;
            current.started_at_unix_seconds = listen.started_at_unix_seconds;
            current.local_period = listen.local_period;
            current.audible_millis = listen.audible_millis;
            current.qualified = listen.qualified;
            current.last_monotonic_millis = Some(sample.monotonic_millis);
        }

        Ok(())
    }
    pub(super) fn external_queue_extent(&self) -> Option<(usize, usize)> {
        self.external
            .as_ref()
            .map(|external| (external.queue_total, external.queue_offset))
    }

    pub(super) fn handoff_snapshot(&self) -> (library::QueueRestore, Option<SourceReportFact>) {
        if self.external.is_some() {
            let mut queue = self.sequence.snapshot();
            queue.ids = Arc::from([]);
            return (queue, None);
        }
        let restored = self.sequence.selected().map(|entry| {
            let run = RunId::new(self.next_run_number);
            let mut current = RunContext::resolving(run, self.play_id(run), entry);
            current.status = TransportStatus::Paused;
            current.desired_playing = false;
            current
        });
        (
            self.sequence.snapshot(),
            self.current_run
                .as_ref()
                .or(restored.as_ref())
                .map(|run| self.source_report(run, SourceReportPhase::Progress, false)),
        )
    }
    pub(super) fn next_session_identifier(&self) -> String {
        self.play_id(RunId::new(self.next_run_number))
    }

    fn observe_external(
        &mut self,
        output: PlaybackOutput,
        mut observation: ExternalPlayback,
        sample: &ClockSample,
    ) -> Result<(), SequenceError> {
        self.advance_external_clock(sample);
        let already_external = self.external.is_some();
        self.external_clock_millis = sample.monotonic_millis;
        // Hold the same entry's playhead for ignored previews and buffering
        // reports, including when queue metadata is refreshed at the same time.
        if already_external
            && (observation.position_millis.is_none()
                || observation.status == TransportStatus::Buffering)
            && observation.queue.as_ref().is_none_or(|queue| {
                let current_offset = self.external_queue_extent().map_or(0, |(_, offset)| offset);
                queue
                    .current_index
                    .map(|position| position + observation.queue_offset)
                    == self
                        .sequence
                        .selected_index()
                        .map(|position| position + current_offset)
                    && queue
                        .current_index
                        .and_then(|position| queue.order.get(position))
                        .and_then(|member| queue.entries.get(*member as usize))
                        .map(|entry| entry.track_key)
                        == self
                            .sequence
                            .selected_index()
                            .and_then(|position| self.sequence.entry_at(position))
                            .map(|entry| entry.track_key)
            })
        {
            observation.position_millis = Some(self.sequence.progress_millis());
        }
        // Validate the captured queue before retiring the local run.
        let sequence = observation
            .queue
            .take()
            .map(|queue| {
                Sequence::from_window(queue, self.sequence.revision() + 1, self.sequence.next_id())
            })
            .transpose()?;

        if self.external.is_none() {
            if let Some(run) = self.current_run() {
                self.send_backend(BackendCommand::Stop { run });
            }
            self.finish_current(RunEndReason::Stopped, sample);
            self.cancel_queue_read();
            self.queue_loading = false;
            self.deferred_queue.clear();
            self.cancel_candidate_tasks(false);
            self.auto_dj_in_flight = None;
        }
        let queue_changed = sequence.is_some();
        if let Some(sequence) = sequence {
            self.sequence = sequence;
            self.dirty = true;
            self.current_media_changed = true;
        }
        self.dirty |= queue_changed
            || self.external.as_ref() != Some(&observation)
            || self.playback_output != output;
        if let Some(position) = observation.position_millis {
            self.sequence.set_progress_millis(position);
        }
        self.sequence.set_repeat_mode(observation.repeat);
        self.sequence.observe_shuffle(observation.shuffle);
        self.output_volume = observation.volume;
        self.output_muted = observation.muted;
        self.playback_output = output;
        self.external = Some(observation);
        self.last_error = None;
        self.failed_run = None;
        if already_external && queue_changed {
            self.maybe_request_auto_dj();
        }
        Ok(())
    }

    pub(super) fn advance_external_clock(&mut self, sample: &ClockSample) {
        let Some(external) = self.external.as_ref() else {
            return;
        };
        let elapsed = sample
            .monotonic_millis
            .saturating_sub(self.external_clock_millis);
        self.external_clock_millis = sample.monotonic_millis;
        if external.status != TransportStatus::Playing || self.sequence.selected_index().is_none() {
            return;
        }
        let previous = self.sequence.progress_millis();
        let mut position = previous.saturating_add(elapsed);
        let duration = self.duration_millis();
        if duration > 0 {
            position = position.min(duration);
        }
        self.sequence.set_progress_millis(position);
        self.dirty |= position != previous;
    }

    pub(super) fn adopt_local(
        &mut self,
        queue: library::QueueRestore,
        stream: PreparedStream,
        playing: bool,
        backend: Box<dyn PlaybackBackend>,
    ) -> Result<(), PlaybackError> {
        let sequence =
            Sequence::from_window(queue, self.sequence.revision() + 1, self.sequence.next_id())?;
        if sequence.selected().is_none() {
            return Err(SequenceError::MissingSelectedEntry.into());
        }
        let _ = self.swap_backend(backend);
        self.adopt_sequence(sequence, stream, playing);
        Ok(())
    }

    fn adopt_sequence(&mut self, sequence: Sequence, stream: PreparedStream, playing: bool) {
        self.external = None;
        self.sequence = sequence;
        self.sequence.persist_membership();
        self.playback_output = PlaybackOutput::Local;
        self.output_volume = self.settings.volume;
        self.output_muted = self.settings.muted;
        self.current_run = None;
        self.failed_run = None;
        self.next_plan = None;
        self.auto_dj_in_flight = None;
        self.auto_dj_waiting_for_continuation = false;
        self.dirty = true;
        if let Some(entry) = self.sequence.selected().cloned() {
            let run = self.next_run_id();
            let mut current = RunContext::resolving(run, self.play_id(run), &entry);
            current.desired_playing = playing;
            self.current_run = Some(current);
            self.replan_next_if_changed();

            self.current_stream_resolved(run, stream);
            self.current_media_changed = true;
            self.sequence.persist_state();
        }
    }
    pub(super) fn take_queue_persistence(&mut self) -> Option<library::QueueUpdate> {
        let dirty = self.sequence.take_persistence();
        if self.external.is_some() { None } else { dirty }
    }
    pub(super) fn accepts_queue(&self, id: u64) -> bool {
        self.pending_queue
            .as_ref()
            .is_some_and(|(pending, _, _)| *pending == id)
    }

    pub(super) fn playback_output(&self) -> &PlaybackOutput {
        &self.playback_output
    }

    pub(super) fn output_volume(&self) -> f64 {
        self.output_volume
    }

    pub(super) fn output_muted(&self) -> bool {
        self.output_muted
    }

    pub(super) fn status(&self) -> TransportStatus {
        if let Some(external) = &self.external {
            return external.status;
        }
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

    pub(super) fn desired_playing(&self) -> bool {
        if let Some(external) = &self.external {
            return matches!(
                external.status,
                TransportStatus::Playing | TransportStatus::Buffering
            );
        }
        self.current_run
            .as_ref()
            .is_some_and(|run| run.desired_playing)
    }

    pub(super) fn current_run(&self) -> Option<RunId> {
        self.current_run.as_ref().map(|run| run.id)
    }

    pub(super) fn can_seek(&self) -> bool {
        match self.current_run.as_ref() {
            Some(run) => run.seekable,
            None => self.duration_millis() > 0,
        }
    }

    pub(super) fn position_millis(&self) -> u64 {
        self.sequence.progress_millis()
    }

    pub(super) fn position_observed_at_millis(&self) -> u64 {
        if self.external.is_some() {
            self.external_clock_millis
        } else {
            self.current_run
                .as_ref()
                .map(|run| run.position_observed_at_millis)
                .unwrap_or_default()
        }
    }

    pub(super) fn duration_millis(&self) -> u64 {
        if let Some(external) = &self.external {
            if external.duration_millis > 0 {
                return external.duration_millis;
            }
        }
        self.current_run
            .as_ref()
            .map(|run| run.duration_millis)
            .or_else(|| {
                self.sequence
                    .selected()
                    .map(|entry| entry.track.metadata.duration_millis.max(0) as u64)
            })
            .unwrap_or_default()
    }

    pub(super) fn buffering_percent(&self) -> Option<u8> {
        self.buffering_percent
    }

    pub(super) fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub(super) fn settings(&self) -> &PlaybackSettings {
        &self.settings
    }

    pub(super) fn replace_output(&mut self, output: PlaybackOutput) {
        self.external = None;
        self.playback_output = output;
        self.last_error = None;
        if self.playback_output.is_local() {
            self.output_volume = self.settings.volume;
            self.output_muted = self.settings.muted;
        }
        let Some(current) = self.current_run.as_mut() else {
            self.dirty = true;
            if let Some(failed) = self.failed_run.take() {
                if failed.desired_playing {
                    self.begin_selected_run();
                } else {
                    self.restored_paused = true;
                }
            }
            return;
        };
        let Some(stream) = current.resolved_stream.clone() else {
            self.dirty = true;
            return;
        };
        let run = current.id;
        let desired_playing = current.desired_playing;
        current.backend_loaded = false;
        if !desired_playing {
            current.status = TransportStatus::Paused;
            self.dirty = true;
            return;
        }
        current.status = TransportStatus::Buffering;
        current.backend_loaded = true;
        let next = self.prepared_next(run);

        if self.playback_output.is_local() {
            self.send_backend(BackendCommand::ConfigureAudio(self.settings.clone().into()));
        }
        self.send_backend(BackendCommand::Start {
            run,
            current: stream,
            next,
            start_position_millis: self.sequence.progress_millis(),
            playback_rate: self.settings.playback_rate,
        });
        self.dirty = true;
    }

    pub(super) fn auto_dj_enabled(&self) -> bool {
        self.auto_dj_enabled
    }

    fn play_radio(&mut self, request: crate::playback::RadioPlayRequest) {
        let owner = self.owner();
        let database = owner.database.clone();
        let source = owner
            .source
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let selected = source.upgrade().and_then(|owner| owner.selected_library());
        self.start_candidates(request.placement, Provenance::Radio, async move {
            crate::radio::radio_candidates(
                &database,
                source,
                selected,
                request.seed,
                20,
                &library::ReadCancellation::new(),
            )
            .await
        });
    }

    fn play_random(&mut self, request: crate::playback::RandomPlayRequest) {
        let owner = self.owner();
        let Some(selected) = owner
            .source_owner()
            .and_then(|owner| owner.selected_library())
        else {
            return;
        };
        let database = owner.database.clone();
        let excluded = self
            .sequence
            .selected()
            .map(|entry| entry.track.track_key)
            .into_iter()
            .collect::<Vec<_>>();
        self.start_candidates(request.placement, Provenance::Random, async move {
            database
                .random_candidates(
                    selected.source_key,
                    selected.music_folder_key,
                    &request.criteria,
                    &excluded,
                    request.requested,
                    &library::ReadCancellation::new(),
                )
                .await
                .map_err(super::string_error)
        });
    }

    fn start_candidates(
        &mut self,
        placement: Placement,
        provenance: Provenance,
        candidates: impl std::future::Future<Output = Result<Vec<library::TrackKey>, String>>
        + Send
        + 'static,
    ) {
        if matches!(placement, Placement::Replace { .. }) {
            self.cancel_candidate_tasks(false);
            self.auto_dj_in_flight = None;
            self.auto_dj_waiting_for_continuation = false;
        }
        let was_loading = self.queue_loading();
        let id = self.next_queue_request;
        self.next_queue_request = id.wrapping_add(1);
        let playback = self.playback.clone();
        let task = self.owner().runtime.spawn(async move {
            let result = candidates.await;
            playback.post(move |player, sample| {
                if let Err(error) = player.complete_candidates(id, result, provenance, sample) {
                    player.report_error(error.to_string());
                }
            });
        });
        self.candidate_tasks.insert(id, (placement, task));
        self.dirty |= self.queue_loading() != was_loading;
    }

    fn complete_candidates(
        &mut self,
        id: u64,
        result: Result<Vec<library::TrackKey>, String>,
        provenance: Provenance,
        sample: &ClockSample,
    ) -> Result<(), SequenceError> {
        let was_loading = self.queue_loading();
        let Some((placement, _)) = self.candidate_tasks.remove(&id) else {
            return Ok(());
        };
        self.dirty |= self.queue_loading() != was_loading;
        match result {
            Ok(candidates) if !candidates.is_empty() => self.apply_batch(
                crate::playback::PlayRequest::from_input(
                    library::QueueLoad::track_keys(candidates, provenance),
                    placement,
                ),
                sample,
            ),
            Ok(_) => Ok(()),
            Err(error) => Ok(self.report_error(error)),
        }
    }

    fn cancel_candidate_tasks(&mut self, replacement_only: bool) {
        let was_loading = self.queue_loading();
        self.candidate_tasks.retain(|_, (placement, task)| {
            if !replacement_only || matches!(placement, Placement::Replace { .. }) {
                task.abort();
                false
            } else {
                true
            }
        });
        self.dirty |= self.queue_loading() != was_loading;
    }

    pub(super) fn play(
        &mut self,
        request: crate::playback::PlayRequest,
        sample: &ClockSample,
    ) -> Result<(), SequenceError> {
        if let Some((context, track_key, rank)) =
            request.activation_context(self.sequence.shuffle_enabled())
            && self.activate_context(&context, track_key, rank, sample)
        {
            return Ok(());
        }
        if matches!(request.placement, Placement::Replace { .. }) {
            self.cancel_candidate_tasks(false);
            self.auto_dj_in_flight = None;
            self.auto_dj_waiting_for_continuation = false;
        }
        self.apply_batch(request, sample)
    }

    pub(super) fn handle_command(
        &mut self,
        command: Command,
        sample: &ClockSample,
    ) -> Result<(), SequenceError> {
        if self.external.is_some() && command.addresses_occurrence() {
            return Ok(());
        }
        if self.pending_queue.is_some()
            && matches!(
                &command,
                Command::Insert { .. } | Command::SetShuffle { .. }
            )
        {
            self.deferred_queue.push_back(command);
            return Ok(());
        }
        match command {
            Command::Radio(request) => Ok(self.play_radio(request)),
            Command::Random(request) => Ok(self.play_random(request)),
            Command::OperationFailed(message) => {
                self.last_error = Some(message.clone());
                Ok({
                    self.report_error(message);
                    self.dirty = true;
                })
            }
            Command::ObserveExternal {
                output,
                observation,
            } => self.observe_external(output, observation, sample),
            Command::ApplyQueue(request) => self.apply_batch(request, sample),
            Command::Activate(position) => Ok(self.activate(&position, sample)),
            Command::ActivateOccurrence(id) => Ok({
                if let Some(position) = self.sequence.index_of(OccurrenceId(id)) {
                    self.activate_index(position, sample);
                }
            }),
            Command::Remove(position) => Ok(self.remove(&position, sample)),
            Command::RemoveMany(selection) => Ok(self.remove_many(&selection, sample)),
            Command::Forget(positions) => Ok(self.forget(&positions, sample)),
            Command::Reorder { selection, target } => Ok(self.reorder(&selection, &target, sample)),
            Command::Insert { input, target } => Ok(self.insert(input, &target)),
            Command::MoveAfterCurrent(position) => Ok(self.move_after_current(&position, sample)),
            Command::MoveOccurrenceAfterCurrent(id) => Ok({
                let target = self
                    .sequence
                    .selected_index()
                    .map_or(QueueReorderTarget::End, QueueReorderTarget::After);
                self.reorder(&QueueSelection::Occurrences(vec![id]), &target, sample);
            }),
            Command::Clear { include_current } => Ok(self.clear(include_current, sample)),
            Command::PlayPause => Ok(self.play_pause()),
            Command::Play => Ok(self.set_playing(true)),
            Command::Pause => Ok(self.set_playing(false)),
            Command::Stop => Ok(self.stop(sample, true)),
            Command::Next => Ok(self.next(sample)),
            Command::Previous => Ok(self.previous(sample)),
            Command::Seek(position_millis) => Ok(self.seek(position_millis)),
            Command::SetVolume(volume) => Ok(self.set_volume(volume)),
            Command::SetMuted(muted) => Ok(self.set_muted(muted)),
            Command::PersistVolume(volume) => {
                self.set_volume(volume);
                if self.playback_output.is_local() {
                    self.owner().persist_output(
                        self.settings.volume,
                        self.settings.muted,
                        self.settings.audio_output.clone(),
                    );
                }
                Ok(())
            }
            Command::SetRepeat(repeat) => Ok(self.set_repeat(repeat)),
            Command::SetShuffle { enabled, seed } => Ok(self.set_shuffle(enabled, seed)),
            Command::SetAutoDj {
                enabled,
                refill_threshold,
            } => Ok(self.set_auto_dj(enabled, refill_threshold)),
            Command::UpdateSettings(settings) => Ok(self.update_settings(settings)),
            Command::SetVisualizerEnabled(enabled) => Ok({
                self.send_backend(BackendCommand::SetVisualizerEnabled(enabled));
            }),
            Command::StreamInputsChanged => Ok(self.stream_inputs_changed()),
            Command::RekeyTracks(pairs) => Ok({
                let mut changed = false;
                self.edit_queue(|sequence| changed = sequence.rekey_tracks(&pairs));
                self.dirty |= changed;
            }),
            Command::CatalogChanged => Ok(self.sequence.refresh_metadata()),
        }
    }

    pub(super) fn stream_resolved(&mut self, run: RunId, stream: impl Into<PreparedStream>) {
        let mut stream = stream.into();
        if stream.loudness.is_none()
            && self.settings.loudness_normalization != playback::LoudnessNormalization::Off
        {
            self.resolve_loudness(run, stream);
            return;
        }
        self.stream_tasks.remove(&run);
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
            return;
        };
        stream.row = self.sequence.at_id(plan.occurrence).cloned();
        plan.resolution = NextResolution::Ready(stream.clone());

        if self.current_run.as_ref().is_some_and(|current| {
            matches!(
                current.status,
                TransportStatus::Buffering | TransportStatus::Playing | TransportStatus::Paused
            )
        }) {
            let command = BackendCommand::PrepareNext {
                current_run: plan.current_run,
                next: Some(PreparedNext::new(plan.next_run, stream, plan.transition)),
            };
            self.send_backend(command);
        }
    }

    pub(super) fn loudness_resolved(&mut self, run: RunId, stream: PreparedStream) {
        self.stream_tasks.remove(&run);
        if let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
            && let Some(prepared) = current.resolved_stream.as_mut()
        {
            prepared.loudness = stream.loudness.clone();
            self.send_backend(BackendCommand::UpdateLoudness {
                run,
                loudness: stream.loudness.unwrap_or_default(),
            });
        } else {
            self.stream_resolved(run, stream);
        }
    }

    fn resolve_loudness(&mut self, run: RunId, stream: PreparedStream) {
        let task = self
            .owner()
            .resolve_loudness(self.playback.clone(), run, stream);
        if let Some(previous) = self.stream_tasks.insert(run, task) {
            previous.abort();
        }
    }

    pub(super) fn stream_failed(&mut self, run: RunId, error: String, sample: &ClockSample) {
        self.stream_tasks.remove(&run);
        if self
            .current_run
            .as_ref()
            .is_some_and(|current| current.id == run)
        {
            self.dirty = true;
            self.finish_current(RunEndReason::Failed, sample);
            self.last_error = Some(error.clone());
            self.buffering_percent = None;
            self.report_error(error);
            return;
        }
        if self
            .next_plan
            .as_ref()
            .is_some_and(|plan| plan.next_run == run)
        {
            self.next_plan = None;
        }
    }

    pub(super) fn handle_backend(&mut self, event: BackendEvent, sample: &ClockSample) {
        match event {
            BackendEvent::Started { run } => self.accept_started(run, sample),
            BackendEvent::State { run, state } => self.accept_state(run, state, sample),
            BackendEvent::TransportObserved { run, state } => {
                let Some(current) = self
                    .current_run
                    .as_mut()
                    .filter(|current| current.id == run)
                else {
                    return;
                };
                if state == BackendState::Stopped {
                    self.dirty = true;
                    self.finish_current(RunEndReason::Stopped, sample);
                    return;
                }
                let desired_playing = match state {
                    BackendState::Playing => true,
                    BackendState::Paused => false,
                    BackendState::Buffering => current.desired_playing,
                    BackendState::Stopped => unreachable!(),
                };
                let intent_changed = current.desired_playing != desired_playing;
                current.desired_playing = desired_playing;
                self.accept_state(run, state, sample);
                self.dirty |= intent_changed;
            }
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
                    self.replan_next_if_changed();
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
                    self.report_error(error.message().to_string());
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
                    return;
                }
                self.output_volume = volume;
                self.output_muted = muted;
                if local {
                    self.settings.volume = volume;
                    self.settings.muted = muted;
                    self.settings.audio_output = output.clone();
                }
                if audio_output_changed {
                    self.owner().persist_output(volume, muted, output);
                }
                self.dirty = true;
            }
            BackendEvent::Visualizer { run, levels } => {
                if self
                    .current_run
                    .as_ref()
                    .is_some_and(|current| current.id == run)
                {
                    self.owner().publish_visualizer(run, levels);
                }
            }
            BackendEvent::Error { run, error } => {
                let Some(current) = self
                    .current_run
                    .as_ref()
                    .filter(|current| current.id == run)
                else {
                    return;
                };
                let failed = FailedRun {
                    desired_playing: current.desired_playing,
                };
                self.dirty = true;
                self.finish_current(RunEndReason::Failed, sample);
                self.failed_run = Some(failed);
                self.last_error = Some(error.message().to_string());
                self.report_error(error.message().to_string());
            }
            BackendEvent::OperationFailed { run, error } => {
                if run.is_some() && run != self.current_run() {
                    return;
                }
                self.report_error(error.message().to_string());
            }
            BackendEvent::TransportRejected {
                run,
                requested_playing,
                error,
            } => {
                let Some(current) = self
                    .current_run
                    .as_mut()
                    .filter(|current| current.id == run)
                else {
                    return;
                };
                let observed_playing = matches!(
                    current.status,
                    TransportStatus::Playing | TransportStatus::Buffering
                );
                let view_changed = current.desired_playing == requested_playing
                    && current.desired_playing != observed_playing;
                if view_changed {
                    current.desired_playing = observed_playing;
                }

                self.report_error(error.message().to_string());
                self.dirty |= view_changed;
            }
        }
    }

    pub(super) fn complete_auto_dj(
        &mut self,
        seed_run: Option<RunId>,
        seed_media_uri: &str,
        load: library::QueueLoad,
        sample: &ClockSample,
    ) -> Result<bool, SequenceError> {
        if self.external.is_some() {
            return Ok(false);
        }
        if !self.auto_dj_in_flight.as_ref().is_some_and(|request| {
            request.seed_run == seed_run && request.seed_media_uri == seed_media_uri
        }) {
            return Ok(false);
        }
        self.auto_dj_in_flight = None;
        if !self.auto_dj_enabled
            || self.sequence.selected_index().is_none()
            || self.sequence.remaining_after_selected() >= self.auto_dj_refill_threshold
        {
            self.dirty = true;
            return Ok(true);
        }
        self.apply_batch(
            crate::playback::PlayRequest::from_input(load, Placement::End),
            sample,
        )?;
        Ok(true)
    }

    pub(super) fn complete_auto_dj_candidates(
        &mut self,
        seed_run: Option<RunId>,
        seed_media_uri: &str,
        mut candidates: Vec<library::TrackKey>,
        requested_count: usize,
        sample: &ClockSample,
    ) -> Result<bool, SequenceError> {
        candidates.truncate(requested_count);
        if candidates.is_empty() {
            return Ok(self.auto_dj_unavailable(seed_run, seed_media_uri, None));
        }
        self.complete_auto_dj(
            seed_run,
            seed_media_uri,
            library::QueueLoad::track_keys(candidates, Provenance::AutoDj),
            sample,
        )
    }

    pub(super) fn auto_dj_unavailable(
        &mut self,
        seed_run: Option<RunId>,
        seed_media_uri: &str,
        error: Option<String>,
    ) -> bool {
        if !self
            .auto_dj_in_flight
            .as_ref()
            .is_some_and(|key| key.seed_run == seed_run && key.seed_media_uri == seed_media_uri)
        {
            return false;
        }
        self.auto_dj_in_flight = None;
        self.auto_dj_waiting_for_continuation = false;
        if let Some(error) = error {
            self.report_error(error);
        }
        self.dirty = true;
        true
    }

    fn apply_batch(
        &mut self,
        request: crate::playback::PlayRequest,
        _sample: &ClockSample,
    ) -> Result<(), SequenceError> {
        let replacing = matches!(request.placement, Placement::Replace { .. });
        let loading_changed = !self.queue_loading;
        self.queue_loading = true;
        if self.pending_queue.is_some() && !replacing {
            self.deferred_queue.push_back(Command::ApplyQueue(request));
            self.dirty |= loading_changed;
            return Ok(());
        }
        if replacing {
            self.deferred_queue.clear();
            self.queue_transport = None;
        }
        let completion = match request.placement {
            Placement::Replace { .. } => QueueCompletion::Replace,
            Placement::AfterCurrent => QueueCompletion::Insert {
                target: self
                    .sequence
                    .selected()
                    .map_or(QueueReorderTarget::End, |row| {
                        QueueReorderTarget::AfterOccurrence(
                            row.occurrence_id.expect("local queue entry"),
                        )
                    }),
            },
            Placement::End => QueueCompletion::Insert {
                target: QueueReorderTarget::End,
            },
        };
        self.request_queue(request, completion);
        Ok(())
    }

    pub(super) fn queue_loading(&self) -> bool {
        self.queue_loading || !self.candidate_tasks.is_empty()
    }

    fn request_queue(
        &mut self,
        request: crate::playback::PlayRequest,
        completion: QueueCompletion,
    ) {
        let id = self.next_queue_request;
        self.next_queue_request = id.wrapping_add(1);
        self.queue_loading = true;
        self.cancel_queue_read();
        let owner = self.owner();
        let database = owner.database.clone();
        let playback = self.playback.clone();
        let shuffled = self.sequence.shuffle_enabled();
        let task = owner.runtime.spawn(async move {
            let result = request
                .prepare(&database, shuffled, super::random_u64())
                .await
                .map_err(super::string_error);
            playback.post(move |player, sample| {
                if let Err(error) = player.complete_queue(id, result, sample) {
                    player.report_error(error.to_string());
                }
            });
        });
        self.pending_queue = Some((id, completion, task));
        self.dirty = true;
    }

    pub(super) fn hydrate_queue(&mut self) {
        if (self.external.is_some() && !self.sequence.refreshing_metadata())
            || self.pending_queue.is_some()
            || self.pending_hydration.is_some()
        {
            return;
        }
        let Some(entries) = self.sequence.read_request() else {
            self.dirty |= self.queue_loading;
            self.queue_loading = false;
            return;
        };
        let owner = self.owner();
        let database = owner.database.clone();
        let playback = self.playback.clone();
        self.pending_hydration = Some(owner.runtime.spawn(async move {
            let requested = entries
                .iter()
                .enumerate()
                .map(|(position, (_, entry))| (position, entry.clone()))
                .collect::<Vec<_>>();
            let result = database
                .queue_rows_for_entries(&requested, &library::ReadCancellation::new())
                .await
                .map(|rows| {
                    entries
                        .into_iter()
                        .map(|(id, _)| id)
                        .zip(rows.into_iter().map(|row| row.track.clone()))
                        .collect()
                })
                .map_err(super::string_error);
            playback.post(move |player, sample| player.complete_queue_metadata(result, sample));
        }));
    }

    fn complete_queue_metadata(
        &mut self,
        result: Result<Vec<(OccurrenceId, library::TrackView)>, String>,
        _sample: &ClockSample,
    ) {
        self.pending_hydration = None;
        match result {
            Ok(rows) => self.dirty |= self.sequence.hydrate(rows),
            Err(error) => self.report_error(error),
        }
        if self.external.is_some() {
            return;
        }
        if matches!(self.pending_queue, Some((_, QueueCompletion::Replace, _))) {
            return;
        }
        if self.current_run.is_none()
            && self.queue_transport != Some(TransportStatus::Stopped)
            && !self.restored_paused
        {
            self.begin_selected_run();
            if self.queue_transport == Some(TransportStatus::Paused) {
                self.set_playing(false);
            }
        } else {
            self.replan_next_if_changed();
        }
        if self.current_run.is_some() {
            self.queue_transport = None;
        }
    }

    fn finish_queue_run(&mut self, sample: &ClockSample, reason: RunEndReason) {
        if let Some(run) = &self.current_run {
            self.send_backend(BackendCommand::Stop { run: run.id });
        }
        self.finish_current(reason, sample);
    }

    fn edit_queue(&mut self, edit: impl FnOnce(&mut Sequence)) {
        edit(&mut self.sequence);
        if let (Some(run), Some(row)) = (self.current_run.as_mut(), self.sequence.selected()) {
            run.row = row.clone();
            if let Some(stream) = &mut run.resolved_stream {
                stream.row = Some(row.clone());
            }
        }
    }

    fn changed_queue(&mut self, changed: bool, stop: bool, sample: &ClockSample) {
        self.dirty = true;
        if changed {
            if stop {
                self.queue_transport = Some(TransportStatus::Stopped);
            }
            if let Some(run) = &self.current_run {
                self.send_backend(BackendCommand::Stop { run: run.id });
            }
            self.finish_current(
                if stop {
                    RunEndReason::Stopped
                } else {
                    RunEndReason::ManualSkip
                },
                sample,
            );
            self.restored_paused = false;
            if !stop && self.queue_transport != Some(TransportStatus::Stopped) {
                self.begin_selected_run();
                if self.queue_transport == Some(TransportStatus::Paused) {
                    self.set_playing(false);
                }
            }
            self.current_media_changed = true;
        } else {
            self.replan_next_if_changed();
        }
        self.sequence.persist_state();
        if self.sequence.selected_index().is_none() {
            self.auto_dj_in_flight = None;
            self.auto_dj_waiting_for_continuation = false;
        }
        self.maybe_request_auto_dj();
    }

    pub(super) fn complete_queue(
        &mut self,
        id: u64,
        result: Result<super::commands::QueueBatch, String>,
        sample: &ClockSample,
    ) -> Result<bool, SequenceError> {
        if !self.accepts_queue(id) {
            return Ok(false);
        }
        let (_, completion, task) = self.pending_queue.take().unwrap();
        task.abort();
        match result {
            Err(error) => {
                self.queue_loading = false;

                self.report_error(error);
                self.dirty = true;
            }
            Ok(page) => match completion {
                QueueCompletion::Replace => {
                    self.finish_queue_run(sample, RunEndReason::ManualSkip);
                    self.sequence.add_page(page, QueueReorderTarget::End, true);
                    self.restored_paused = false;
                    self.changed_queue(true, false, sample);
                }
                QueueCompletion::Insert { target } => {
                    let advance =
                        self.auto_dj_waiting_for_continuation && self.current_run.is_none();
                    let had_current = self.sequence.selected_index().is_some();
                    self.edit_queue(|sequence| sequence.add_page(page, target, false));
                    if advance {
                        self.sequence.advance_index(false);
                        self.auto_dj_waiting_for_continuation = false;
                    }
                    self.changed_queue(advance || !had_current, false, sample)
                }
            },
        };
        if self.current_run.is_some() {
            self.queue_transport = None;
        }
        while self.pending_queue.is_none()
            && let Some(command) = self.deferred_queue.pop_front()
        {
            self.handle_command(command, sample)?;
        }
        Ok(true)
    }

    pub(super) fn activate_context(
        &mut self,
        context_id: &str,
        track_key: library::TrackKey,
        source_rank: usize,
        sample: &ClockSample,
    ) -> bool {
        let Some(index) = self
            .sequence
            .context_index(context_id, track_key, source_rank)
        else {
            return false;
        };
        self.activate_index(index, sample);
        true
    }

    fn activate(&mut self, position: &usize, sample: &ClockSample) {
        self.activate_index(*position, sample)
    }

    fn activate_index(&mut self, index: usize, sample: &ClockSample) {
        if self
            .current_run
            .as_ref()
            .is_some_and(|_| self.sequence.selected_index() == Some(index))
            || index >= self.sequence.total()
        {
            return;
        }
        self.cancel_candidate_tasks(true);
        self.restored_paused = false;
        self.dirty = true;
        if let Some(run) = self.current_run.as_ref() {
            self.send_backend(BackendCommand::Stop { run: run.id });
        }
        self.finish_current(RunEndReason::ManualSkip, sample);
        if !self.sequence.activate_index(index) {
            return;
        }
        self.begin_selected_run();
    }

    fn remove(&mut self, id: &usize, sample: &ClockSample) {
        self.remove_many(&QueueSelection::Positions(vec![*id]), sample)
    }
    fn remove_many(&mut self, selection: &QueueSelection, sample: &ClockSample) {
        let previous = self
            .sequence
            .selected_index()
            .and_then(|i| self.sequence.id_at(i));
        let removing_current = previous.is_some_and(|id| match selection {
            QueueSelection::Positions(positions) => positions
                .iter()
                .any(|&i| self.sequence.id_at(i) == Some(id)),
            QueueSelection::Occurrences(ids) => ids.contains(&id.0),
        });
        if removing_current {
            self.finish_queue_run(sample, RunEndReason::ManualSkip);
        }
        self.edit_queue(|sequence| sequence.remove(selection));
        self.changed_queue(removing_current, false, sample);
    }
    fn forget(&mut self, keys: &[library::TrackKey], sample: &ClockSample) {
        let keys = keys
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let ids = (0..self.sequence.total())
            .filter(|&position| {
                self.sequence
                    .entry_at(position)
                    .is_some_and(|entry| keys.contains(&entry.track_key))
            })
            .collect::<Vec<_>>();
        let previous = self.sequence.selected_index();
        if previous.as_ref().is_some_and(|id| ids.contains(id)) {
            self.finish_queue_run(sample, RunEndReason::Stopped)
        }
        self.edit_queue(|sequence| sequence.remove(&QueueSelection::Positions(ids.clone())));
        self.changed_queue(
            previous.is_some_and(|position| ids.contains(&position)),
            true,
            sample,
        );
    }
    fn reorder(
        &mut self,
        selection: &QueueSelection,
        target: &QueueReorderTarget,
        sample: &ClockSample,
    ) {
        self.edit_queue(|sequence| sequence.reorder(selection, target));
        self.changed_queue(false, false, sample)
    }
    fn insert(&mut self, input: library::QueueLoad, target: &QueueReorderTarget) {
        if self.current_run.is_none() {
            self.queue_transport.get_or_insert(TransportStatus::Stopped);
        }
        self.request_queue(
            crate::playback::PlayRequest::from_input(input, Placement::End),
            QueueCompletion::Insert {
                target: match target {
                    QueueReorderTarget::Before(i) => self
                        .sequence
                        .id_at(*i)
                        .map_or(QueueReorderTarget::End, |id| {
                            QueueReorderTarget::BeforeOccurrence(id.0)
                        }),
                    QueueReorderTarget::After(i) => self
                        .sequence
                        .id_at(*i)
                        .map_or(QueueReorderTarget::End, |id| {
                            QueueReorderTarget::AfterOccurrence(id.0)
                        }),
                    target => target.clone(),
                },
            },
        )
    }
    fn move_after_current(&mut self, id: &usize, sample: &ClockSample) {
        let target = self
            .sequence
            .selected()
            .map_or(QueueReorderTarget::End, |row| {
                QueueReorderTarget::AfterOccurrence(row.occurrence_id.expect("local queue entry"))
            });
        self.reorder(&QueueSelection::Positions(vec![*id]), &target, sample)
    }
    fn clear(&mut self, include_current: bool, sample: &ClockSample) {
        let previous = self.sequence.selected_index();
        if include_current {
            self.finish_queue_run(sample, RunEndReason::Stopped)
        }
        self.cancel_queue_read();
        self.queue_loading = false;
        self.deferred_queue.clear();
        let removing_current = include_current || self.current_run.is_none();
        self.edit_queue(|sequence| sequence.clear(removing_current));
        self.changed_queue(removing_current && previous.is_some(), true, sample);
    }

    fn play_pause(&mut self) {
        let playing = self
            .current_run
            .as_ref()
            .is_some_and(|run| run.desired_playing);
        self.set_playing(!playing)
    }

    fn set_playing(&mut self, desired_playing: bool) {
        if self.pending_queue.is_some() || self.pending_hydration.is_some() {
            self.queue_transport = Some(if desired_playing {
                TransportStatus::Playing
            } else {
                TransportStatus::Paused
            });
        }
        let Some(run) = self.current_run.as_mut() else {
            if !desired_playing {
                if let Some(failed) = self.failed_run.as_mut() {
                    failed.desired_playing = false;
                }
                return;
            }
            self.restored_paused = false;
            self.dirty = true;
            self.begin_selected_run();
            return;
        };
        let command = match run.status {
            TransportStatus::Resolving => {
                if run.desired_playing == desired_playing {
                    return;
                }
                run.desired_playing = desired_playing;
                self.dirty = true;
                return;
            }
            TransportStatus::Paused => {
                if !desired_playing {
                    if !run.desired_playing {
                        return;
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

                        if self.playback_output.is_local() {
                            self.send_backend(BackendCommand::ConfigureAudio(
                                self.settings.clone().into(),
                            ));
                        }
                        self.send_backend(BackendCommand::Start {
                            run: run_id,
                            current: stream,
                            next,
                            start_position_millis: self.sequence.progress_millis(),
                            playback_rate: self.settings.playback_rate,
                        });

                        self.dirty = true;
                        return;
                    }
                    BackendCommand::Play { run: run.id }
                }
            }
            TransportStatus::Playing | TransportStatus::Buffering => {
                if run.desired_playing == desired_playing {
                    return;
                }
                run.desired_playing = desired_playing;
                if desired_playing {
                    BackendCommand::Play { run: run.id }
                } else {
                    BackendCommand::Pause { run: run.id }
                }
            }
            TransportStatus::Stopped | TransportStatus::Failed => {
                return;
            }
        };

        self.send_backend(command);
        self.dirty = true;
    }

    fn stop(&mut self, sample: &ClockSample, reset_position: bool) {
        self.failed_run = None;
        if self.pending_queue.is_some() || self.pending_hydration.is_some() {
            self.queue_transport = Some(TransportStatus::Stopped);
        }
        let Some(run) = self.current_run.as_ref().map(|run| run.id) else {
            if !reset_position || (self.sequence.progress_millis() == 0 && !self.restored_paused) {
                return;
            }
            self.restored_paused = false;
            self.sequence.set_progress_millis(0);

            self.sequence.persist_state();
            self.dirty = true;
            return;
        };
        self.cancel_candidate_tasks(true);
        self.dirty = true;
        self.send_backend(BackendCommand::Stop { run });
        self.finish_current(RunEndReason::Stopped, sample);
        if reset_position {
            self.sequence.set_progress_millis(0);
        }
        self.sequence.persist_state();
    }

    pub(super) fn shutdown(&mut self, sample: &ClockSample) {
        self.cancel_candidate_tasks(false);
        self.auto_dj_in_flight = None;
        self.auto_dj_waiting_for_continuation = false;
        self.dirty = true;
        if let Some(run) = self.current_run.as_ref() {
            self.send_backend(BackendCommand::Stop { run: run.id });
        }
        self.finish_current(RunEndReason::Stopped, sample);
        self.sequence.persist_state();
    }

    fn next(&mut self, sample: &ClockSample) {
        self.restored_paused = false;
        self.dirty = true;
        let old = self.current_run.as_ref().map(|run| run.id);
        let reserved = self.next_plan.clone();
        self.cancel_candidate_tasks(true);
        if let Some(run) = old {
            self.send_backend(BackendCommand::Stop { run });
        }
        self.finish_current(RunEndReason::ManualSkip, sample);
        let next_position = self.sequence.advance_index(false);
        let Some(next_position) = next_position else {
            self.auto_dj_waiting_for_continuation = true;
            self.maybe_request_auto_dj();
            return;
        };
        self.next_plan =
            reserved.filter(|plan| Some(plan.occurrence) == self.sequence.id_at(next_position));
        self.promote_or_begin(next_position, true);
    }

    fn previous(&mut self, sample: &ClockSample) {
        if self.position_millis() > 10_000 {
            return self.seek(0);
        }
        let Some(index) = self.sequence.previous_index() else {
            return self.seek(0);
        };
        self.restored_paused = false;
        self.dirty = true;
        self.cancel_candidate_tasks(true);
        if let Some(run) = self.current_run.as_ref() {
            self.send_backend(BackendCommand::Stop { run: run.id });
        }
        self.finish_current(RunEndReason::ManualSkip, sample);
        self.sequence.activate_index(index);
        self.begin_selected_run();
    }

    fn seek(&mut self, position_millis: u64) {
        let Some(run) = self.current_run.as_ref() else {
            self.sequence.set_progress_millis(position_millis);

            self.sequence.persist_state();
            self.dirty = true;
            return;
        };
        if !run.seekable && position_millis != 0 {
            return;
        }
        let run = run.id;

        self.send_backend(BackendCommand::Seek {
            run,
            position_millis,
        });
        self.notices.push(PlaybackNotice::PositionDiscontinuity(
            PositionDiscontinuity {
                run,
                position_millis,
            },
        ));
    }

    fn accept_seekable(&mut self, run: RunId, seekable: bool) {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return;
        };
        if current.seekable == seekable {
            return;
        }
        current.seekable = seekable;
        self.dirty = true;
    }

    fn set_volume(&mut self, volume: f64) {
        let volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        if self.output_volume == volume && !self.output_muted {
            return;
        }
        self.output_volume = volume;
        self.output_muted = false;
        if self.playback_output.is_local() {
            self.settings.volume = volume;
            self.settings.muted = false;
        }

        self.send_backend(BackendCommand::SetOutputVolume {
            volume,
            volume_scale: self.settings.volume_scale,
            muted: false,
        });
        self.dirty = true;
    }

    fn set_muted(&mut self, muted: bool) {
        if self.output_muted == muted {
            return;
        }
        self.output_muted = muted;
        if self.playback_output.is_local() {
            self.settings.muted = muted;
        }
        self.send_backend(BackendCommand::SetOutputVolume {
            volume: self.output_volume,
            volume_scale: self.settings.volume_scale,
            muted,
        });
        if self.playback_output.is_local() {
            self.owner().persist_output(
                self.settings.volume,
                muted,
                self.settings.audio_output.clone(),
            );
        }

        self.dirty = true;
    }

    fn set_repeat(&mut self, repeat: RepeatMode) {
        if self.sequence.repeat_mode() == repeat {
            return;
        }
        self.sequence.set_repeat_mode(repeat);
        self.dirty = true;
        self.replan_next_if_changed();
        self.maybe_request_auto_dj();
    }

    fn set_shuffle(&mut self, enabled: bool, seed: u64) {
        if self.sequence.shuffle_enabled() == enabled {
            return;
        }
        self.edit_queue(|sequence| sequence.shuffle(enabled, seed));
        self.dirty = true;
        self.replan_next_if_changed();
    }

    fn set_auto_dj(&mut self, enabled: bool, refill_threshold: usize) {
        let refill_threshold = refill_threshold.max(1);
        if self.auto_dj_enabled == enabled && self.auto_dj_refill_threshold == refill_threshold {
            return;
        }
        self.auto_dj_enabled = enabled;
        self.auto_dj_refill_threshold = refill_threshold;
        if !enabled {
            self.auto_dj_in_flight = None;
            self.auto_dj_waiting_for_continuation = false;
        }
        self.dirty = true;
        self.maybe_request_auto_dj();
    }

    fn update_settings(&mut self, mut settings: PlaybackSettings) {
        settings.sanitize();
        if settings == self.settings {
            return;
        }
        let stream_changed = settings.stream_quality != self.settings.stream_quality;
        let normalization_enabled = self.settings.loudness_normalization
            == playback::LoudnessNormalization::Off
            && settings.loudness_normalization != playback::LoudnessNormalization::Off;
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
            || settings.audio_fade_on_status_change != self.settings.audio_fade_on_status_change
            || settings.visualizer_ignore_volume != self.settings.visualizer_ignore_volume;
        self.settings = settings.clone();
        self.dirty = true;
        if self.playback_output.is_local() {
            self.output_volume = settings.volume;
            self.output_muted = settings.muted;
            if settings.loudness_normalization != playback::LoudnessNormalization::Off
                && let Some(current) = self.current_run.as_ref()
                && let Some(stream) = current
                    .resolved_stream
                    .as_ref()
                    .filter(|stream| stream.loudness.is_none())
                && !self.stream_tasks.contains_key(&current.id)
            {
                self.resolve_loudness(current.id, stream.clone());
            }
        }
        if self.playback_output.is_local() && audio_configuration_changed {
            self.send_backend(BackendCommand::ConfigureAudio(self.settings.clone().into()));
        } else if self.playback_output.is_local() && output_changed {
            self.send_backend(BackendCommand::SetOutputVolume {
                volume: settings.volume,
                volume_scale: settings.volume_scale,
                muted: settings.muted,
            });
        }
        if self.playback_output.is_local() && playback_rate_changed {
            self.send_backend(BackendCommand::SetPlaybackRate(playback_rate));
        }
        if stream_changed || normalization_enabled {
            self.replan_next(true);
        } else {
            self.replan_next_if_changed();
        }
    }

    fn stream_inputs_changed(&mut self) {
        self.replan_next(true);
    }

    fn current_stream_resolved(&mut self, run: RunId, mut stream: PreparedStream) {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return;
        };
        stream.row = Some(current.row.clone());
        if !current.desired_playing {
            current.status = TransportStatus::Paused;
            current.resolved_stream = Some(stream);
            self.dirty = true;
            return;
        }
        current.status = TransportStatus::Buffering;
        current.resolved_stream = Some(stream.clone());
        current.backend_loaded = true;
        let next = self.prepared_next(run);

        if self.playback_output.is_local() {
            self.send_backend(BackendCommand::ConfigureAudio(self.settings.clone().into()));
        }
        self.send_backend(BackendCommand::Start {
            run,
            current: stream,
            next,
            start_position_millis: self.sequence.progress_millis(),
            playback_rate: self.settings.playback_rate,
        });

        self.dirty = true;
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

    fn accept_started(&mut self, run: RunId, sample: &ClockSample) {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return;
        };
        current.backend_loaded = true;
        if current.started_at_unix_seconds.is_some() {
            if current.status != TransportStatus::Buffering {
                return;
            }
            current.status = TransportStatus::Playing;
            current.last_monotonic_millis = Some(sample.monotonic_millis);
            self.dirty = true;
            return;
        }
        self.dirty = true;
        self.mark_started(sample);
    }

    fn accept_state(&mut self, run: RunId, state: BackendState, sample: &ClockSample) {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return;
        };
        current.advance_clock(sample.monotonic_millis);
        let status = match state {
            BackendState::Stopped => TransportStatus::Stopped,
            BackendState::Buffering => TransportStatus::Buffering,
            BackendState::Paused => TransportStatus::Paused,
            BackendState::Playing => TransportStatus::Playing,
        };
        if current.status == status {
            return;
        }
        current.status = status;
        self.dirty = true;
        let progress_reported = self.emit_progress_facts();
        if matches!(status, TransportStatus::Playing | TransportStatus::Paused)
            && !progress_reported
            && let Some(current) = self.current_run.as_ref()
            && current.started_at_unix_seconds.is_some()
        {
            self.owner().report_source(self.source_report(
                current,
                SourceReportPhase::Progress,
                false,
            ));
        }
        if status == TransportStatus::Paused && !progress_reported {
            self.sequence.persist_state();
        }
        if matches!(status, TransportStatus::Paused | TransportStatus::Stopped) {}
    }

    fn accept_position(&mut self, run: RunId, millis: u64, sample: &ClockSample) {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return;
        };
        let clock_changed = current.position_observed_at_millis != sample.monotonic_millis;
        current.position_observed_at_millis = sample.monotonic_millis;
        current.advance_clock(sample.monotonic_millis);
        let playhead_millis = if current.duration_millis == 0 {
            millis
        } else {
            millis.min(current.duration_millis)
        };
        let changed = self.sequence.progress_millis() != playhead_millis;
        if !changed && current.status == TransportStatus::Paused {
            return;
        }
        self.sequence.set_progress_millis(playhead_millis);
        self.dirty |= changed || clock_changed;
        self.emit_progress_facts();
    }

    fn accept_duration(&mut self, run: RunId, millis: u64) {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return;
        };
        if millis == 0 || current.duration_millis == millis {
            return;
        }
        current.duration_millis = millis;
        self.dirty = true;
    }

    fn accept_buffering(&mut self, run: RunId, percent: u8) {
        if !self
            .current_run
            .as_ref()
            .is_some_and(|current| current.id == run)
        {
            return;
        }
        self.buffering_percent = Some(percent.min(100));
        self.dirty = true;
    }

    fn accept_ended(&mut self, run: RunId, sample: &ClockSample) {
        if !self
            .current_run
            .as_ref()
            .is_some_and(|current| current.id == run)
        {
            return;
        }
        let desired_playing = self
            .current_run
            .as_ref()
            .is_some_and(|current| current.desired_playing);
        self.dirty = true;
        let reserved = self.next_plan.clone();
        self.finish_current(RunEndReason::Completed, sample);
        let next = self.sequence.advance_index(true);
        if let Some(next) = next {
            self.next_plan =
                reserved.filter(|plan| Some(plan.occurrence) == self.sequence.id_at(next));
            self.promote_or_begin(next, desired_playing);
        } else {
            self.auto_dj_waiting_for_continuation = true;
            self.maybe_request_auto_dj();
        }
    }

    fn accept_transitioned(&mut self, old_run: RunId, new_run: RunId, sample: &ClockSample) {
        let current_run = self.current_run.as_ref().map(|current| current.id);
        if current_run == Some(new_run) {
            return;
        }
        if current_run != Some(old_run) {
            self.send_backend(BackendCommand::Stop { run: new_run });
            return;
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
            self.send_backend(BackendCommand::Stop { run: new_run });
            self.accept_ended(old_run, sample);
            return;
        }
        let Some(position) = self
            .next_plan
            .as_ref()
            .and_then(|plan| self.sequence.index_of(plan.occurrence))
        else {
            return;
        };
        let transitioned_stream = self.next_plan.as_ref().and_then(|plan| {
            let NextResolution::Ready(stream) = &plan.resolution else {
                return None;
            };
            Some(stream.clone())
        });
        self.dirty = true;
        self.finish_current(RunEndReason::Completed, sample);
        if !self.sequence.activate_backend(position) {
            return;
        }
        self.install_reserved_run(new_run, position, desired_playing);
        if let Some(current) = self.current_run.as_mut() {
            current.resolved_stream = transitioned_stream;
            current.backend_loaded = true;
        }
        self.mark_started(sample);
        if !desired_playing {
            self.send_backend(BackendCommand::Pause { run: new_run });
        }
    }

    fn promote_or_begin(&mut self, position: usize, desired_playing: bool) {
        let plan = self
            .next_plan
            .take()
            .filter(|plan| Some(plan.occurrence) == self.sequence.id_at(position));
        let Some(plan) = plan else {
            self.begin_selected_run();
            if !desired_playing && let Some(current) = self.current_run.as_mut() {
                current.desired_playing = false;
            }
            return;
        };
        self.install_reserved_run(plan.next_run, position, desired_playing);
        match plan.resolution {
            NextResolution::Ready(stream) if desired_playing => {
                if let Some(current) = self.current_run.as_mut() {
                    current.status = TransportStatus::Buffering;
                    current.resolved_stream = Some(stream.clone());
                    current.backend_loaded = true;
                }
                self.send_backend(BackendCommand::Start {
                    run: plan.next_run,
                    current: stream,
                    next: None,
                    start_position_millis: 0,
                    playback_rate: self.settings.playback_rate,
                });
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

    fn install_reserved_run(&mut self, run: RunId, position: usize, desired_playing: bool) {
        let Some(entry) = self
            .sequence
            .selected()
            .filter(|entry| entry.position == position)
            .cloned()
        else {
            return;
        };
        let mut current = RunContext::resolving(run, self.play_id(run), &entry);
        current.desired_playing = desired_playing;
        self.current_run = Some(current);
        self.failed_run = None;
        self.current_media_changed = true;
        self.next_plan = None;
        self.buffering_percent = None;
        self.last_error = None;
        self.sequence.persist_state();
        self.replan_next_if_changed();
    }

    fn begin_selected_run(&mut self) {
        if self.queue_transport == Some(TransportStatus::Stopped) {
            self.queue_transport = None;
        }
        self.failed_run = None;
        let Some(entry) = self.sequence.selected().cloned() else {
            self.sequence.need_metadata();
            self.current_run = None;
            self.next_plan = None;
            return;
        };
        let run = self.next_run_id();
        self.current_run = Some(RunContext::resolving(run, self.play_id(run), &entry));
        self.current_media_changed = true;
        self.next_plan = None;
        self.buffering_percent = None;
        self.last_error = None;
        self.resolve_current(run, &entry);
        self.replan_next_if_changed();
        self.sequence.persist_state();
    }

    fn replan_next_if_changed(&mut self) {
        self.replan_next(false);
    }

    fn replan_next(&mut self, force: bool) {
        let Some(current) = self.current_run.as_ref() else {
            self.next_plan = None;
            return;
        };
        let current_run = current.id;
        let current_occurrence = current.row.occurrence_id;
        let Some(_) = self
            .sequence
            .selected()
            .filter(|entry| entry.occurrence_id == current_occurrence)
        else {
            self.next_plan = None;
            return;
        };
        let next = self.sequence.peek_next_eos().cloned();
        let Some(next) = next else {
            if self.next_plan.take().is_some() {
                self.send_backend(BackendCommand::PrepareNext {
                    current_run,
                    next: None,
                });
            }
            return;
        };
        let request = StreamRequest::new(
            (&next.track).media_uri.clone(),
            self.settings.stream_quality,
        );
        let transition = decided_transition(
            &self.settings,
            self.sequence.selected().map(|entry| &entry.track.metadata),
            &next.track.metadata,
        );
        if !force
            && self.next_plan.as_ref().is_some_and(|plan| {
                plan.current_run == current_run
                    && Some(plan.occurrence.0) == next.occurrence_id
                    && plan.request == request
                    && plan.transition == transition
            })
        {
            return;
        }
        if self.next_plan.take().is_some() {
            self.send_backend(BackendCommand::PrepareNext {
                current_run,
                next: None,
            });
        }
        let next_run = self.next_run_id();
        self.next_plan = Some(NextPlan {
            current_run,
            next_run,
            occurrence: OccurrenceId(next.occurrence_id.expect("local queue entry")),
            request: request.clone(),
            transition,
            resolution: NextResolution::Resolving,
        });
        let mut request = request;
        request.session_identifier = Some(self.play_id(next_run));
        self.resolve_stream(next_run, next, request);
    }

    fn resolve_current(&mut self, run: RunId, entry: &std::sync::Arc<QueueRow>) {
        let mut request = StreamRequest::new(
            (&entry.track).media_uri.clone(),
            self.settings.stream_quality,
        );
        request.session_identifier = Some(
            self.current_run
                .as_ref()
                .filter(|current| current.id == run)
                .map_or_else(|| self.play_id(run), |current| current.play_id.clone()),
        );
        self.resolve_stream(run, entry.clone(), request);
    }

    fn mark_started(&mut self, sample: &ClockSample) {
        let (run, row) = {
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
            (current.id, current.row.clone())
        };
        self.notices.push(PlaybackNotice::RunStarted(run));
        self.owner().scrobbler.now_playing(&row.track.metadata);
        if let Some(current) = self.current_run.as_ref() {
            self.owner().report_source(self.source_report(
                current,
                SourceReportPhase::Started,
                false,
            ));
        }
        self.emit_progress_facts();
    }

    fn emit_progress_facts(&mut self) -> bool {
        let playhead_millis = self.sequence.progress_millis();
        let bucket_changed = {
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
            bucket_changed
        };
        self.qualify_current();
        if bucket_changed {
            self.sequence.persist_state();
            if let Some(current) = self.current_run.as_ref() {
                self.owner().report_source(self.source_report(
                    current,
                    SourceReportPhase::Progress,
                    false,
                ));
            }
        }
        bucket_changed
    }

    fn qualify_current(&mut self) {
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
                    current.row.clone(),
                    current.started_at_unix_seconds,
                    current.audible_millis,
                    current.local_period.clone(),
                ))
            } else {
                None
            }
        };
        let activity_qualified = activity.is_some();
        if let Some((play_id, row, Some(started_at), listened_millis, Some(local_period))) =
            activity
        {
            let track = library::TrackReference::from(row.track.clone());
            self.owner().record_activity(
                row.track.track_key,
                Box::new(playback::ActivityListen {
                    play_id: play_id.clone(),
                    item: track.clone(),
                    started_at_unix_seconds: started_at,
                    local_period,
                    listened_millis,
                    skipped: false,
                }),
            );
        }

        if activity_qualified && let Some(current) = self.current_run.as_ref() {
            self.owner().report_source(self.source_report(
                current,
                SourceReportPhase::QualifiedPlay,
                false,
            ));
        }
    }

    fn finish_current(&mut self, reason: RunEndReason, sample: &ClockSample) {
        self.failed_run = None;
        let Some(current) = self.current_run.as_mut() else {
            self.next_plan = None;
            return;
        };
        current.advance_clock(sample.monotonic_millis);
        self.qualify_current();
        let Some(current) = self.current_run.take() else {
            return;
        };
        let track = library::TrackReference::from(current.row.track.clone());
        self.current_media_changed = true;
        if current.started_at_unix_seconds.is_some() {
            self.owner().report_source(self.source_report(
                &current,
                SourceReportPhase::Ended,
                reason == RunEndReason::Failed,
            ));
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
                self.owner().record_activity(
                    current.row.track.track_key,
                    Box::new(playback::ActivityListen {
                        play_id: current.play_id,
                        item: track,
                        started_at_unix_seconds: started_at,
                        local_period: period,
                        listened_millis: current.audible_millis,
                        skipped: true,
                    }),
                );
            }
        }
        if reason == RunEndReason::Completed {
            self.sequence.set_progress_millis(0);
        }
        self.sequence.persist_state();
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
            transcoded: current
                .resolved_stream
                .as_ref()
                .is_some_and(|stream| stream.transcoded()),
            run: current.id,
            session_identifier: current.play_id.clone(),
            queue_id: None,
            queue_item_id: None,
            media_uri: current.row.track.media_uri.clone(),
            phase,
            started_at_unix_seconds: current.started_at_unix_seconds.unwrap_or_default(),
            position_millis: self.sequence.progress_millis(),
            duration_millis: (current.duration_millis > 0).then_some(current.duration_millis),
            paused: !current.desired_playing,
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

    fn maybe_request_auto_dj(&mut self) {
        if matches!(&self.playback_output, PlaybackOutput::Remote(output) if output.protocol == playback::RemoteOutputProtocol::RufinConnect)
        {
            return;
        }
        let remaining = self.external.as_ref().map_or_else(
            || self.sequence.remaining_after_selected(),
            |external| {
                external.queue_total.saturating_sub(
                    external.queue_offset
                        + self.sequence.selected_index().map_or(0, |index| index + 1),
                )
            },
        );
        if !self.auto_dj_enabled
            || remaining >= self.auto_dj_refill_threshold
            || self.auto_dj_in_flight.is_some()
        {
            return;
        }
        let Some(seed) = self.sequence.selected() else {
            return;
        };
        let request = AutoDjRequest {
            seed_run: self.current_run(),
            seed_media_uri: seed.track.media_uri.clone(),
            requested_count: 5,
        };
        self.auto_dj_in_flight = Some(request.clone());
        self.owner().request_auto_dj(self.playback.clone(), request);
    }

    fn next_run_id(&mut self) -> RunId {
        let run = RunId::new(self.next_run_number);
        self.next_run_number = self.next_run_number.wrapping_add(1).max(1);
        run
    }
}

fn decided_transition(
    settings: &PlaybackSettings,
    current: Option<&library::TrackMetadata>,
    next: &library::TrackMetadata,
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

impl Player {
    pub(super) fn view(&self) -> PlaybackView {
        let sequence = self.sequence();
        let settings = self.settings();
        let mut summary = playback::QueueSummaryView {
            revision: sequence.revision(),
            total: sequence.total(),
            current_index: sequence.selected_index(),
            next_index: sequence
                .next_index_eos()
                .filter(|index| *index < sequence.total()),
            can_next: sequence.next_index(false).is_some(),
        };
        let offset = self.external_queue_extent().map_or(0, |(_, offset)| offset);
        let projected_row = |row: &Arc<QueueRow>| {
            let mut row = row.clone();
            if self.external.is_some() {
                let row = Arc::make_mut(&mut row);
                row.position += offset;
                row.occurrence_id = None;
            }
            row
        };
        if let Some((total, offset)) = self.external_queue_extent() {
            summary.total = total;
            summary.current_index = summary.current_index.map(|index| index + offset);
            summary.next_index = summary.next_index.map(|index| index + offset);
            summary.can_next = summary.current_index.is_some_and(|index| index + 1 < total)
                || sequence.repeat_mode() != RepeatMode::Off;
        }
        PlaybackView {
            queue_loading: self.queue_loading(),
            queue: summary,
            queue_window: sequence.entries().iter().map(projected_row).collect(),
            transport: TransportView {
                current: sequence.selected().map(|entry| CurrentMedia {
                    stream: self
                        .current_run
                        .as_ref()
                        .and_then(|run| run.resolved_stream.as_ref())
                        .map(|stream| playback::CurrentStream {
                            local: stream.local_input,
                            format: stream.content_type.clone(),
                            transcoded: stream.transcoded(),
                        }),
                    id: CurrentMediaId {
                        run: self.current_run(),
                        media_uri: entry.track.media_uri.clone(),
                    },
                    row: projected_row(entry),
                }),
                state: self.status(),
                desired_playing: self.desired_playing(),
                position_millis: self.position_millis(),
                position_observed_at_millis: self.position_observed_at_millis(),
                playback_rate: if self.playback_output().is_local() {
                    settings.playback_rate
                } else {
                    1.0
                },
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

impl Player {
    fn owner(&self) -> Arc<PlaybackOwner> {
        self.owner
            .upgrade()
            .expect("playback owner held by actor turn")
    }

    fn send_backend(&mut self, command: BackendCommand) {
        let run = command.run();
        if let Err(error) = self.backend.send(command) {
            let error = BackendFailure::new(error.to_string());
            self.backend_errors.push(match run {
                Some(run) => BackendEvent::Error { run, error },
                None => BackendEvent::OperationFailed { run, error },
            });
        }
    }

    fn resolve_stream(&mut self, run: RunId, row: Arc<QueueRow>, request: StreamRequest) {
        let task = self.owner().resolve_stream(
            self.playback.clone(),
            run,
            row,
            request,
            self.settings.loudness_normalization,
        );
        if let Some(previous) = self.stream_tasks.insert(run, task) {
            previous.abort();
        }
    }

    fn cancel_queue_read(&mut self) {
        if let Some((_, _, task)) = self.pending_queue.take() {
            task.abort();
        }
    }

    pub(super) fn report_error(&mut self, error: String) {
        tracing::warn!(%error, "Playback operation failed");
        self.notices.push(PlaybackNotice::OperationFailed(error));
    }

    pub(super) fn drive(&mut self, clock: &dyn Fn() -> ClockSample) {
        let events = self.backend.drain_events();
        if events.is_empty() && self.external.is_none() {
            return;
        }
        let sample = clock();
        for event in events {
            self.handle_backend(event, &sample);
        }
        self.advance_external_clock(&sample);
        self.publish(&sample);
    }

    pub(super) fn publish(&mut self, sample: &ClockSample) {
        for event in std::mem::take(&mut self.backend_errors) {
            self.handle_backend(event, sample);
        }
        self.hydrate_queue();
        let live = self.stream_runs();
        self.stream_tasks.retain(|run, task| {
            if live.contains(&Some(*run)) {
                true
            } else {
                task.abort();
                false
            }
        });
        let persistence = self.take_queue_persistence();
        if let Some(update) = persistence {
            self.owner().save_queue(update);
        }
        let current_media_changed = std::mem::take(&mut self.current_media_changed);
        if current_media_changed && self.current_run.is_some() {
            self.maybe_request_auto_dj();
        }
        if std::mem::take(&mut self.dirty) || !self.notices.is_empty() || current_media_changed {
            let projection = PlaybackProjection {
                view: self.view(),
                notices: std::mem::take(&mut self.notices),
            };
            self.owner().publish(projection, current_media_changed);
        }
    }

    pub(super) fn projection(&self) -> PlaybackProjection {
        PlaybackProjection {
            view: self.view(),
            notices: Vec::new(),
        }
    }

    pub(super) fn replace_backend(
        &mut self,
        output: PlaybackOutput,
        backend: Box<dyn PlaybackBackend>,
    ) {
        let mut previous = std::mem::replace(&mut self.backend, backend);
        if let Some(run) = self.current_run()
            && let Err(error) = previous.send(BackendCommand::Stop { run })
        {
            self.report_error(error.to_string());
        }
        previous.drain_events();
        if let Err(error) = previous.shutdown() {
            self.report_error(error.to_string());
        }
        self.replace_output(output);
    }

    fn swap_backend(&mut self, backend: Box<dyn PlaybackBackend>) -> Result<(), PlaybackError> {
        let mut previous = std::mem::replace(&mut self.backend, backend);
        let stopped = self
            .current_run()
            .map_or(Ok(()), |run| previous.send(BackendCommand::Stop { run }));
        previous.drain_events();
        let shutdown = previous.shutdown();
        stopped
            .and(shutdown)
            .map_err(|error| PlaybackError::BackendShutdown(error.to_string()))
    }

    pub(super) fn shutdown_backend(&mut self) -> Result<(), playback::BackendError> {
        self.cancel_queue_read();
        self.cancel_candidate_tasks(false);
        if let Some(task) = self.pending_hydration.take() {
            task.abort();
        }
        for (_, task) in self.stream_tasks.drain() {
            task.abort();
        }
        self.backend.shutdown()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.cancel_queue_read();
        self.cancel_candidate_tasks(false);
        if let Some(task) = self.pending_hydration.take() {
            task.abort();
        }
        for (_, task) in self.stream_tasks.drain() {
            task.abort();
        }
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
pub(super) mod tests;
