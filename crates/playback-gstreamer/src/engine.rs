use super::audio::{SharedQueuedLoudness, audio_output_is_available, passthrough_allowed};
use super::pipeline::{AboutToFinishAction, PlayerPipeline, SourceClock};
#[cfg(test)]
use super::visualizer::visualizer_pipeline_is_live;
use super::visualizer::{VisualizerAnalyzer, VisualizerTap};
use super::*;
use std::collections::HashMap;
use std::sync::mpsc::{SyncSender, sync_channel};

const GAPLESS_BUFFERING_IGNORE_REMAINING_MS: u64 = 5_000;
const STATUS_FADE_DURATION: Duration = Duration::from_millis(300);

struct AudioWarning {
    category: String,
    element: Option<String>,
    message: String,
    captured_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TelemetryKind {
    Position,
    Duration,
    Buffering,
    Visualizer,
}

struct PendingTelemetry {
    sequence: u64,
    event: BackendEvent,
}

/// Everything the playback worker reacts to, in arrival order.
pub(super) enum WorkerInput {
    Command(Box<BackendCommand>),
    Processing(ProcessingRequest),
    Bus(Slot, PipelineId, gst::Message),
    Wake,
    Shutdown,
}

#[derive(Default)]
pub(super) struct EventMailbox {
    next_sequence: u64,
    ready: VecDeque<BackendEvent>,
    latest: HashMap<(RunId, TelemetryKind), PendingTelemetry>,
    processing_meter: Option<BackendEvent>,
    waker: Option<EventWaker>,
}

impl EventMailbox {
    fn is_empty(&self) -> bool {
        self.ready.is_empty() && self.latest.is_empty() && self.processing_meter.is_none()
    }

    /// Returns the waker when this event makes the mailbox non-empty.
    fn push(&mut self, event: BackendEvent) -> Option<EventWaker> {
        let waker = self.is_empty().then(|| self.waker.clone()).flatten();
        self.insert(event);
        waker
    }

    fn insert(&mut self, event: BackendEvent) {
        if let BackendEvent::ProcessingMeter {
            output,
            mut peak_db,
            spectrum_db,
        } = event
        {
            if let Some(BackendEvent::ProcessingMeter {
                output: old_output,
                peak_db: old_peak,
                spectrum_db: old_spectrum,
            }) = self.processing_meter.as_mut()
                && *old_output == output
            {
                if peak_db.len() == old_peak.len() {
                    for (peak, old) in peak_db.iter_mut().zip(old_peak.iter()) {
                        *peak = peak.max(*old);
                    }
                }
                if !peak_db.is_empty() {
                    *old_peak = peak_db;
                }
                if !spectrum_db.is_empty() {
                    *old_spectrum = spectrum_db;
                }
            } else {
                self.processing_meter = Some(BackendEvent::ProcessingMeter {
                    output,
                    peak_db,
                    spectrum_db,
                });
            }
            return;
        }
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        if let Some(key) = telemetry_key(&event) {
            self.latest
                .insert(key, PendingTelemetry { sequence, event });
        } else {
            self.flush_telemetry();
            self.ready.push_back(event);
        }
    }

    fn drain(&mut self) -> Vec<BackendEvent> {
        self.flush_telemetry();
        if let Some(meter) = self.processing_meter.take() {
            self.ready.push_back(meter);
        }
        self.ready.drain(..).collect()
    }

    fn flush_telemetry(&mut self) {
        let mut telemetry = self
            .latest
            .drain()
            .map(|(_, event)| event)
            .collect::<Vec<_>>();
        telemetry.sort_unstable_by_key(|event| event.sequence);
        self.ready
            .extend(telemetry.into_iter().map(|event| event.event));
    }
}

fn telemetry_key(event: &BackendEvent) -> Option<(RunId, TelemetryKind)> {
    match event {
        BackendEvent::Position { run, .. } => Some((*run, TelemetryKind::Position)),
        BackendEvent::Duration { run, .. } => Some((*run, TelemetryKind::Duration)),
        BackendEvent::Buffering { run, .. } => Some((*run, TelemetryKind::Buffering)),
        BackendEvent::Visualizer { run, levels } if !levels.is_empty() => {
            Some((*run, TelemetryKind::Visualizer))
        }
        BackendEvent::Started { .. }
        | BackendEvent::State { .. }
        | BackendEvent::Seekable { .. }
        | BackendEvent::Ended { .. }
        | BackendEvent::Transitioned { .. }
        | BackendEvent::Bitstream { .. }
        | BackendEvent::NextNeeded { .. }
        | BackendEvent::NextPreparationFailed { .. }
        | BackendEvent::AudioApplied { .. }
        | BackendEvent::ProcessingApplied { .. }
        | BackendEvent::ProcessingFailed { .. }
        | BackendEvent::ProcessingMeter { .. }
        | BackendEvent::Visualizer { .. }
        | BackendEvent::OperationFailed { .. }
        | BackendEvent::TransportRejected { .. }
        | BackendEvent::TransportObserved { .. }
        | BackendEvent::Error { .. } => None,
    }
}

pub struct GStreamerPlaybackBackend {
    commands: Option<Sender<WorkerInput>>,
    events: Arc<Mutex<EventMailbox>>,
    thread: Option<thread::JoinHandle<()>>,
    processing: tokio::task::JoinHandle<()>,
}
impl GStreamerPlaybackBackend {
    pub fn new(
        mut processing: tokio::sync::watch::Receiver<ProcessingRequest>,
    ) -> Result<Self, BackendError> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| BackendError::Backend(error.to_string()))?;
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let thread_events = Arc::clone(&events);
        let initial = processing.borrow_and_update().clone();
        let (ready_sender, ready_receiver) = sync_channel(1);
        let thread = thread::Builder::new()
            .name("rufin-gstreamer-playback".to_string())
            .spawn(move || run_gstreamer_thread(thread_events, ready_sender, initial))
            .map_err(|error| BackendError::Backend(error.to_string()))?;
        match ready_receiver.recv() {
            Ok(Ok(commands)) => Ok(Self {
                processing: runtime.spawn({
                    let commands = commands.clone();
                    async move {
                        while processing.changed().await.is_ok() {
                            let request = processing.borrow_and_update().clone();
                            if commands.send(WorkerInput::Processing(request)).is_err() {
                                break;
                            }
                        }
                    }
                }),
                commands: Some(commands),
                events,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(BackendError::Backend(error))
            }
            Err(_) => {
                let _ = thread.join();
                Err(BackendError::ChannelClosed)
            }
        }
    }
}
impl PlaybackBackend for GStreamerPlaybackBackend {
    fn send(&mut self, command: BackendCommand) -> Result<(), BackendError> {
        self.commands
            .as_ref()
            .ok_or(BackendError::ChannelClosed)?
            .send(WorkerInput::Command(Box::new(command)))
            .map_err(|_| BackendError::ChannelClosed)
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        lock_recover(&self.events).drain()
    }

    fn set_event_waker(&mut self, waker: EventWaker) {
        let pending = {
            let mut events = lock_recover(&self.events);
            events.waker = Some(Arc::clone(&waker));
            !events.is_empty()
        };
        if pending {
            waker();
        }
    }

    fn shutdown(&mut self) -> Result<(), BackendError> {
        self.processing.abort();
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(WorkerInput::Shutdown);
        }
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        thread
            .join()
            .map_err(|_| BackendError::Backend("GStreamer playback worker panicked".to_string()))
    }
}

impl Drop for GStreamerPlaybackBackend {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Slot {
    Primary,
    Secondary,
}

impl Slot {
    fn index(self) -> usize {
        match self {
            Self::Primary => 0,
            Self::Secondary => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct PipelineId(pub(super) u64);

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PreparedRun {
    pub(super) run: RunId,
    pub(super) stream: PreparedStream,
}

impl PreparedRun {
    fn from_next(next: &PreparedNext) -> Self {
        Self {
            run: next.run,
            stream: next.stream.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IncomingPhase {
    Prerolling,
    Seeking,
    Ready,
}

#[derive(Clone, Debug)]
struct IncomingPipeline {
    id: PipelineId,
    slot: Slot,
    item: PreparedNext,
    phase: IncomingPhase,
}

impl IncomingPipeline {
    fn fade_in(&self) -> Option<Duration> {
        fade_in(&self.item)
    }
}

fn fade_in(item: &PreparedNext) -> Option<Duration> {
    match item.transition {
        NextTransition::Gapless => None,
        NextTransition::Crossfade { duration_millis } => {
            Some(Duration::from_millis(duration_millis))
        }
    }
}

#[derive(Clone, Debug)]
struct PendingHandoff {
    incoming: IncomingPipeline,
    from: Slot,
    old_run: RunId,
}

impl PendingHandoff {
    fn matches(&self, slot: Slot, id: PipelineId) -> bool {
        self.incoming.slot == slot && self.incoming.id == id
    }

    fn item(&self) -> &PreparedNext {
        &self.incoming.item
    }
}

/// What the second player is doing. Only one transition runs at a time.
#[derive(Clone, Debug, Default)]
enum Transition {
    #[default]
    None,
    /// The next item prerolls, paused, on the inactive slot.
    Preparing(IncomingPipeline),
    /// The prepared item was asked to play and commits once it reports Playing.
    Activating(PendingHandoff),
    /// Both players run while `from` fades out under its own gain curve.
    Overlap { from: Slot, old_run: RunId },
}

/// The latest seek target. Only one seek runs at a time; a newer target waits for it.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SeekState {
    /// Physical position of the seek the pipeline is performing.
    in_flight: Option<u64>,
    /// Physical position requested while a seek runs or before the session prerolled.
    queued: Option<u64>,
}

#[derive(Debug)]
pub(super) struct SharedBackendState {
    pub(super) settings: BackendAudioSettings,
    pub(super) processing: Option<ProcessingRequest>,
    pub(super) playback_rate: f64,
    pub(super) current: Option<PreparedRun>,
    pub(super) next: Option<PreparedNext>,
    pub(super) active: Slot,
    pub(super) visualizer_enabled: bool,
    pipeline_ids: [Option<PipelineId>; 2],
}
impl SharedBackendState {
    pub(super) fn new() -> Self {
        let settings = BackendAudioSettings::default();
        Self {
            current: None,
            processing: None,
            next: None,
            active: Slot::Primary,
            visualizer_enabled: false,
            playback_rate: DEFAULT_PLAYBACK_RATE,
            pipeline_ids: [None, None],
            settings,
        }
    }

    fn pipeline_id(&self, slot: Slot) -> Option<PipelineId> {
        self.pipeline_ids[slot.index()]
    }

    fn set_pipeline_id(&mut self, slot: Slot, id: Option<PipelineId>) {
        self.pipeline_ids[slot.index()] = id;
    }

    fn pipeline_is_live(&self, slot: Slot, id: PipelineId) -> bool {
        self.pipeline_id(slot) == Some(id)
    }

    pub(super) fn pipeline_is_current(&self, slot: Slot, id: PipelineId) -> bool {
        self.active == slot && self.pipeline_is_live(slot, id)
    }
}
#[derive(Debug, Default)]
pub(super) struct GaplessPlayback {
    pub(super) pending: Option<PreparedNext>,
    pub(super) about_to_finish_pending: bool,
    pub(super) next_needed: Option<RunId>,
    // A passed-through stream can't overlap another, so crossfades play as gapless.
    pub(super) bitstream: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StatusFadeTarget {
    Pause,
    Playing,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct StatusFade {
    slot: Slot,
    target: StatusFadeTarget,
    muted: bool,
}
impl StatusFade {
    pub(super) fn new(slot: Slot, target: StatusFadeTarget, muted: bool) -> Self {
        Self {
            slot,
            target,
            muted,
        }
    }
}
pub(super) struct GstEngine {
    inputs: Receiver<WorkerInput>,
    input_sender: Sender<WorkerInput>,
    pub(super) primary: PlayerPipeline,
    pub(super) secondary: PlayerPipeline,
    pub(super) shared: Arc<Mutex<SharedBackendState>>,
    events: Arc<Mutex<EventMailbox>>,
    pub(super) visualizer: VisualizerAnalyzer,
    pub(super) last_position_tick: Instant,
    pub(super) seek: SeekState,
    // Positions mean nothing before a session's first preroll.
    session_prerolled: bool,
    // A starting session passes through Paused before it plays.
    awaiting_playing: bool,
    pub(super) status_fade: Option<StatusFade>,
    pub(super) play_command_started_at: Option<Instant>,
    desired_playing: bool,
    ended_run: Option<RunId>,
    next_pipeline_number: u64,
    transition: Transition,
}
impl GstEngine {
    fn new(events: Arc<Mutex<EventMailbox>>) -> Self {
        let shared = Arc::new(Mutex::new(SharedBackendState::new()));
        let (input_sender, inputs) = channel();
        let primary = PlayerPipeline::new(
            "rufin-primary-player",
            Slot::Primary,
            Arc::clone(&shared),
            input_sender.clone(),
        );
        let secondary = PlayerPipeline::new(
            "rufin-secondary-player",
            Slot::Secondary,
            Arc::clone(&shared),
            input_sender.clone(),
        );
        let visualizer = VisualizerAnalyzer::new(Arc::clone(&events), Arc::clone(&shared));
        Self {
            inputs,
            input_sender,
            primary,
            secondary,
            shared,
            events,
            visualizer,
            last_position_tick: Instant::now(),
            seek: SeekState::default(),
            session_prerolled: false,
            awaiting_playing: false,
            status_fade: None,
            play_command_started_at: None,
            desired_playing: false,
            ended_run: None,
            next_pipeline_number: 1,
            transition: Transition::None,
        }
    }

    fn incoming(&self) -> Option<&IncomingPipeline> {
        match &self.transition {
            Transition::Preparing(incoming) => Some(incoming),
            _ => None,
        }
    }

    fn incoming_mut(&mut self) -> Option<&mut IncomingPipeline> {
        match &mut self.transition {
            Transition::Preparing(incoming) => Some(incoming),
            _ => None,
        }
    }

    fn take_incoming(&mut self) -> Option<IncomingPipeline> {
        self.incoming()?;
        let Transition::Preparing(incoming) = std::mem::take(&mut self.transition) else {
            unreachable!();
        };
        Some(incoming)
    }

    fn activating(&self) -> Option<&PendingHandoff> {
        match &self.transition {
            Transition::Activating(pending) => Some(pending),
            _ => None,
        }
    }

    fn take_activating(&mut self) -> Option<PendingHandoff> {
        self.activating()?;
        let Transition::Activating(pending) = std::mem::take(&mut self.transition) else {
            unreachable!();
        };
        Some(pending)
    }

    fn overlap(&self) -> Option<(Slot, RunId)> {
        match self.transition {
            Transition::Overlap { from, old_run } => Some((from, old_run)),
            _ => None,
        }
    }

    /// The players following transport: the current one, and an outgoing crossfade tail.
    fn running_slots(&self) -> impl Iterator<Item = Slot> + use<> {
        std::iter::once(self.active_slot()).chain(self.overlap().map(|(from, _)| from))
    }

    fn next_pipeline_id(&mut self) -> PipelineId {
        let id = PipelineId(self.next_pipeline_number);
        self.next_pipeline_number = self.next_pipeline_number.wrapping_add(1).max(1);
        id
    }

    fn start_pipeline(
        &mut self,
        slot: Slot,
        item: &PreparedRun,
        settings: &BackendAudioSettings,
        volume: f64,
        muted: bool,
        playback_rate: f64,
        startup_state: gst::State,
        fade_in: Option<Duration>,
        bitstream: Option<gst::Caps>,
    ) -> Result<PipelineId, BackendFailure> {
        let id = self.next_pipeline_id();
        let previous_id = lock_recover(&self.shared).pipeline_id(slot);
        lock_recover(&self.shared).set_pipeline_id(slot, Some(id));
        let result = self.pipeline_for_slot_mut(slot).play_item(
            id,
            slot,
            item,
            settings,
            volume,
            muted,
            playback_rate,
            startup_state,
            fade_in,
            bitstream,
        );
        if result.is_err() {
            let mut shared = lock_recover(&self.shared);
            if shared.pipeline_id(slot) == Some(id) {
                shared.set_pipeline_id(slot, previous_id);
            }
        }
        result.map(|()| id)
    }

    fn stop_pipeline(&mut self, slot: Slot) {
        lock_recover(&self.shared).set_pipeline_id(slot, None);
        self.pipeline_for_slot_mut(slot).stop();
    }

    fn clear_incoming(&mut self) {
        if let Some(incoming) = self.take_incoming() {
            self.stop_pipeline(incoming.slot);
        }
    }

    fn cancel_handoff_for_replan(&mut self) -> Option<RunId> {
        self.clear_incoming();
        let pending = self.take_activating()?;
        let ended = self.handoff_outgoing_ended(&pending);
        self.stop_pipeline(pending.incoming.slot);
        if ended {
            self.stop_pipeline(pending.from);
            Some(pending.old_run)
        } else {
            None
        }
    }

    fn handoff_outgoing_ended(&self, pending: &PendingHandoff) -> bool {
        pending.incoming.item.transition == NextTransition::Gapless
    }

    fn prepare_incoming(&mut self, next: &PreparedNext) {
        // The output takes one stream while passed through: no overlap or second player.
        if !next.stream.allows_preloading || self.active_pipeline().is_bitstream() {
            self.clear_incoming();
            return;
        }
        if self
            .incoming()
            .is_some_and(|incoming| incoming.item == *next)
            || self
                .activating()
                .is_some_and(|pending| pending.item() == next)
        {
            return;
        }
        if self.activating().is_some() {
            return;
        }
        let overlapping = self.overlap().is_some();
        let context = (|| {
            let shared = lock_recover(&self.shared);
            if overlapping {
                return None;
            }
            let current = shared.current.as_ref()?;
            if !current.stream.allows_preloading {
                return None;
            }
            let should_prepare = match next.transition {
                NextTransition::Crossfade { .. } => true,
                NextTransition::Gapless => gapless_uses_separate_pipeline(current, next),
            };
            should_prepare.then(|| {
                (
                    inactive_slot(shared.active),
                    shared.settings.clone(),
                    shared.playback_rate,
                    shared.settings.muted,
                )
            })
        })();
        let Some((slot, settings, playback_rate, muted)) = context else {
            self.clear_incoming();
            return;
        };

        self.clear_incoming();
        self.stop_pipeline(slot);
        let item = PreparedRun::from_next(next);
        let fade_in = fade_in(next);
        let id = match self.start_pipeline(
            slot,
            &item,
            &settings,
            settings.output_gain(),
            muted,
            playback_rate,
            gst::State::Paused,
            fade_in,
            None,
        ) {
            Ok(id) => id,
            Err(error) => {
                self.report_next_preparation_failure(next.run, error);
                return;
            }
        };
        self.transition = Transition::Preparing(IncomingPipeline {
            id,
            slot,
            item: next.clone(),
            phase: IncomingPhase::Prerolling,
        });
    }

    fn incoming_matches(&self, slot: Slot, id: PipelineId) -> bool {
        self.incoming()
            .is_some_and(|incoming| incoming.slot == slot && incoming.id == id)
    }

    fn pending_handoff_matches(&self, slot: Slot, id: PipelineId) -> bool {
        self.activating()
            .is_some_and(|pending| pending.matches(slot, id))
    }

    fn handle_incoming_async_done(&mut self, slot: Slot, id: PipelineId) {
        if self.pipeline_for_slot(slot).seek_in_progress() {
            return;
        }
        let needs_initial_rate_seek = self.pipeline_for_slot(slot).needs_initial_rate_seek();
        let time_reopened = self
            .pipeline_for_slot(slot)
            .stream()
            .is_some_and(|stream| stream.time_offset_millis().is_some());
        let restart = self
            .incoming()
            .filter(|incoming| {
                incoming.slot == slot
                    && incoming.id == id
                    && incoming.phase == IncomingPhase::Prerolling
                    && (incoming.item.stream.end_millis().is_some() || needs_initial_rate_seek)
                    && incoming.item.stream.time_seekable()
                    && !time_reopened
                    && self.pipeline_for_slot(slot).native_seekable() == Some(false)
            })
            .map(|incoming| incoming.item.clone());
        if let Some(mut next) = restart {
            let planned = next.clone();
            match next.stream.reopened_at(0) {
                Ok(stream) => *next.stream.stream = stream,
                Err(error) => {
                    self.fail_incoming(slot, id, error.to_string().into());
                    return;
                }
            }
            self.clear_incoming();
            self.prepare_incoming(&next);
            if let Some(incoming) = self.incoming_mut() {
                incoming.item = planned;
            }
            return;
        }
        let Some(incoming) = self
            .incoming_mut()
            .filter(|incoming| incoming.slot == slot && incoming.id == id)
        else {
            return;
        };
        match incoming.phase {
            IncomingPhase::Prerolling
                if (incoming.item.stream.end_millis().is_some() && !time_reopened)
                    || needs_initial_rate_seek =>
            {
                incoming.phase = IncomingPhase::Seeking;
                if let Err(error) = self.pipeline_for_slot(slot).seek_millis(0) {
                    self.fail_incoming(slot, id, error);
                } else if !self.pipeline_for_slot(slot).seek_in_progress()
                    && let Some(incoming) = self.incoming_mut()
                {
                    incoming.phase = IncomingPhase::Ready;
                }
            }
            IncomingPhase::Prerolling | IncomingPhase::Seeking => {
                incoming.phase = IncomingPhase::Ready;
            }
            IncomingPhase::Ready => {}
        }
    }

    fn begin_incoming_handoff(
        &mut self,
        slot: Slot,
        id: PipelineId,
        result: gst::StateChangeSuccess,
    ) -> bool {
        if self.activating().is_some() || !self.desired_playing {
            return false;
        }
        let (from, old_run) = {
            let shared = lock_recover(&self.shared);
            let Some(old_run) = shared.current.as_ref().map(|current| current.run) else {
                return false;
            };
            (shared.active, old_run)
        };
        let ready = self.incoming().is_some_and(|incoming| {
            incoming.slot == slot && incoming.id == id && incoming.phase == IncomingPhase::Ready
        });
        if !ready {
            return false;
        }
        let Some(incoming) = self.take_incoming() else {
            return false;
        };
        self.transition = Transition::Activating(PendingHandoff {
            incoming,
            from,
            old_run,
        });
        match result {
            gst::StateChangeSuccess::Async | gst::StateChangeSuccess::NoPreroll => true,
            gst::StateChangeSuccess::Success => self.confirm_handoff(slot, id),
        }
    }

    fn confirm_handoff(&mut self, slot: Slot, id: PipelineId) -> bool {
        if self.pipeline_for_slot(slot).is_buffering() {
            return false;
        }
        if !self.desired_playing {
            self.cancel_unconfirmed_handoff_for_pause();
            return false;
        }
        let Some(pending) = self
            .activating()
            .filter(|pending| pending.matches(slot, id))
        else {
            return false;
        };
        let (still_current, target_is_live) = {
            let shared = lock_recover(&self.shared);
            let target_is_live = shared.pipeline_is_live(slot, id);
            (
                shared.active == pending.from
                    && shared.current.as_ref().map(|current| current.run) == Some(pending.old_run)
                    && target_is_live
                    && shared.next.as_ref() == Some(&pending.incoming.item),
                target_is_live,
            )
        };
        if !still_current {
            self.transition = Transition::None;
            if target_is_live {
                self.stop_pipeline(slot);
            }
            return false;
        }
        let Some(PendingHandoff {
            incoming,
            from,
            old_run,
        }) = self.take_activating()
        else {
            return false;
        };
        match incoming.item.transition {
            NextTransition::Gapless => {
                self.commit_prepared_gapless_handoff(incoming, from, old_run)
            }
            NextTransition::Crossfade { duration_millis } => {
                self.commit_crossfade_handoff(incoming, from, old_run, duration_millis);
            }
        }
        true
    }

    fn fail_handoff(&mut self, slot: Slot, id: PipelineId, error: BackendFailure) -> bool {
        if !self.pending_handoff_matches(slot, id) {
            return false;
        }
        let Some(pending) = self.take_activating() else {
            return false;
        };
        let ended = self.handoff_outgoing_ended(&pending);
        self.stop_pipeline(pending.incoming.slot);
        self.report_next_preparation_failure(pending.incoming.item.run, error);
        if ended {
            self.emit_ended_once(pending.old_run);
        }
        true
    }

    fn cancel_unconfirmed_handoff_for_pause(&mut self) {
        if self.activating().is_none() {
            return;
        }
        if let Some(old_run) = self.cancel_handoff_for_replan() {
            self.emit_ended_once(old_run);
        }
    }

    fn clear_incoming_candidate_at_end(&mut self, slot: Slot) {
        let target = match &self.transition {
            Transition::Activating(pending) if pending.from == slot => Some(pending.incoming.slot),
            Transition::Preparing(incoming) if self.is_active_slot(slot) => Some(incoming.slot),
            _ => None,
        };
        if let Some(target) = target {
            self.transition = Transition::None;
            self.stop_pipeline(target);
        }
    }

    fn gapless_handoff_waits_for_confirmation(&self, slot: Slot) -> bool {
        self.activating().is_some_and(|pending| {
            pending.incoming.item.transition == NextTransition::Gapless && pending.from == slot
        })
    }

    fn commit_prepared_gapless_handoff(
        &mut self,
        incoming: IncomingPipeline,
        from: Slot,
        old_run: RunId,
    ) {
        let slot = incoming.slot;
        self.stop_pipeline(from);
        let (volume, muted) = self.output_gain_state();
        self.pipeline_for_slot(slot)
            .set_output_volume(volume, muted);
        let visualizer_enabled = {
            let mut shared = lock_recover(&self.shared);
            shared.active = slot;
            shared.current = Some(PreparedRun::from_next(&incoming.item));
            shared.next = None;
            shared.visualizer_enabled
        };
        let new_run = incoming.item.run;
        self.seek = SeekState::default();
        self.session_prerolled = true;
        self.awaiting_playing = false;
        self.ended_run = None;
        self.last_position_tick = Instant::now();
        self.sync_visualizer_taps(visualizer_enabled);
        if visualizer_enabled {
            push_event(
                &self.events,
                BackendEvent::Visualizer {
                    run: new_run,
                    levels: Vec::new(),
                },
            );
        }
        push_event(
            &self.events,
            BackendEvent::Transitioned { old_run, new_run },
        );
        push_event(
            &self.events,
            BackendEvent::Position {
                run: new_run,
                millis: 0,
            },
        );
    }

    fn commit_crossfade_handoff(
        &mut self,
        incoming: IncomingPipeline,
        from: Slot,
        old_run: RunId,
        duration_millis: u64,
    ) {
        let to = incoming.slot;
        let new_run = incoming.item.run;
        self.pipeline_for_slot(from)
            .fade_transition_out(Duration::from_millis(duration_millis));
        let visualizer_enabled = {
            let mut shared = lock_recover(&self.shared);
            shared.next = None;
            shared.active = to;
            shared.current = Some(PreparedRun::from_next(&incoming.item));
            shared.visualizer_enabled
        };
        self.transition = Transition::Overlap { from, old_run };
        self.seek = SeekState::default();
        self.session_prerolled = true;
        self.awaiting_playing = false;
        self.ended_run = None;
        let tap = self.visualizer_tap(to, visualizer_enabled);
        self.pipeline_for_slot_mut(to).set_visualizer_tap(tap);
        push_event(
            &self.events,
            BackendEvent::Transitioned { old_run, new_run },
        );
        push_event(
            &self.events,
            BackendEvent::Position {
                run: new_run,
                millis: 0,
            },
        );
    }

    fn fail_incoming(&mut self, slot: Slot, id: PipelineId, error: BackendFailure) {
        if !self.incoming_matches(slot, id) {
            return;
        }
        let Some(incoming) = self.take_incoming() else {
            return;
        };
        self.stop_pipeline(slot);
        self.report_next_preparation_failure(incoming.item.run, error);
    }

    fn report_next_preparation_failure(&self, next_run: RunId, error: BackendFailure) {
        let current_run = self.timing_run_id();
        warn!(
            %error,
            current_run = current_run.map(RunId::get),
            next_run = %next_run,
            "next-stream preparation failed; retaining the cold-start fallback"
        );
        if let Some(current_run) = current_run {
            push_event(
                &self.events,
                BackendEvent::NextPreparationFailed {
                    current_run,
                    next_run,
                    error: error,
                },
            );
        }
    }

    fn handle_command(&mut self, command: BackendCommand) {
        let command_run = command.run().or_else(|| self.timing_run_id());
        let changes_transport = matches!(
            command,
            BackendCommand::Start { .. }
                | BackendCommand::Play { .. }
                | BackendCommand::Pause { .. }
        );
        let result = match command {
            BackendCommand::Start {
                run,
                current,
                next,
                start_position_millis,
                playback_rate,
            } => {
                lock_recover(&self.shared).playback_rate = sanitize_playback_rate(playback_rate);
                self.play_prepared(
                    PreparedRun {
                        run,
                        stream: current,
                    },
                    next,
                    start_position_millis,
                )
            }
            BackendCommand::PrepareNext { current_run, next } => {
                if self.run_is_current(current_run) {
                    self.prepare_next(next);
                }
                Ok(())
            }
            BackendCommand::UpdateLoudness { run, loudness } => {
                if self.run_is_current(run) {
                    if let Some(current) = lock_recover(&self.shared).current.as_mut() {
                        current.stream.loudness = Some(loudness.clone());
                    }
                    self.active_pipeline_mut().update_loudness(&loudness);
                    self.end_passthrough_if_needed()
                } else {
                    Ok(())
                }
            }
            BackendCommand::ConfigureAudio(mut settings) => {
                debug!(
                    fade_on_play_stop = settings.fade_on_status_change,
                    volume_gain = settings.output_gain(),
                    muted = settings.muted,
                    desired_playing = self.desired_playing,
                    "GStreamer audio configuration requested"
                );
                let previous_settings = self.settings();
                if let Some(selected) = settings.audio_output.as_deref()
                    && !audio_output_is_available(selected)
                {
                    settings.audio_output =
                        if settings.audio_output != previous_settings.audio_output {
                            previous_settings.audio_output.clone()
                        } else {
                            None
                        };
                }
                let previous_output = previous_settings.audio_output.clone();
                let output_changed = previous_output != settings.audio_output;
                if output_changed {
                    // A crossfade tail never moves to the new device.
                    self.finish_overlap();
                }
                let preserve_pitch_changed =
                    previous_settings.preserve_pitch != settings.preserve_pitch;
                if self.desired_playing {
                    self.cancel_status_fade();
                } else {
                    self.status_fade = None;
                }
                let mut ended_run = None;
                let result = (|| -> Result<(), BackendFailure> {
                    if !self.desired_playing {
                        if self.active_pipeline().has_session() {
                            if let Err(error) = self.active_pipeline().set_state(gst::State::Paused)
                            {
                                self.retire_playback();
                                return Err(error);
                            }
                            self.push_state(BackendState::Paused);
                        }
                    }
                    // A passed-through session's output is outside the audio graph.
                    let retargeted = if output_changed
                        && !audio_output_change_requires_restart()
                        && !self.active_pipeline().is_bitstream()
                    {
                        self.active_pipeline_mut()
                            .try_reconfigure_audio(&settings)?
                    } else {
                        false
                    };
                    let restart = if output_changed && !retargeted {
                        let current = lock_recover(&self.shared).current.clone();
                        current.map(|current| (current, self.restart_position_millis()))
                    } else {
                        None
                    };
                    lock_recover(&self.shared).settings = settings.clone();
                    if let Some((current, position_millis)) = restart {
                        ended_run = self.cancel_handoff_for_replan();
                        let target_state = if self.desired_playing {
                            gst::State::Playing
                        } else {
                            gst::State::Paused
                        };
                        self.start_item_session_at_millis(
                            current,
                            position_millis,
                            target_state,
                            None,
                        )?;
                        self.push_logical_position(position_millis);
                        if !self.desired_playing {
                            self.push_state(BackendState::Paused);
                        }
                    } else {
                        self.primary.configure_audio(&settings)?;
                        self.secondary.configure_audio(&settings)?;
                        if preserve_pitch_changed && self.playback_rate() != DEFAULT_PLAYBACK_RATE {
                            self.set_playback_rate(self.playback_rate())?;
                        }
                        self.end_passthrough_if_needed()?;
                    }
                    if settings.visualizer_ignore_volume
                        != previous_settings.visualizer_ignore_volume
                    {
                        self.sync_visualizer_taps(self.visualizer_enabled());
                    }
                    let (gain, muted) = self.output_gain_state();
                    self.apply_output_gain_to_pipelines(gain, muted);
                    push_event(
                        &self.events,
                        BackendEvent::AudioApplied {
                            volume: settings.volume,
                            muted,
                            output: settings.audio_output.clone(),
                        },
                    );
                    Ok(())
                })();
                if result.is_ok() {
                    if let Some(ended_run) = ended_run {
                        self.emit_ended_once(ended_run);
                    } else {
                        self.prepare_reserved_incoming();
                    }
                } else {
                    let _ = self.primary.configure_audio(&previous_settings);
                    let _ = self.secondary.configure_audio(&previous_settings);
                    lock_recover(&self.shared).settings = previous_settings;
                    let (gain, muted) = self.output_gain_state();
                    self.apply_output_gain_to_pipelines(gain, muted);
                }
                result
            }
            BackendCommand::SetOutputVolume {
                volume,
                volume_scale,
                muted,
            } => {
                let volume = if volume.is_finite() {
                    volume.clamp(0.0, 1.0)
                } else {
                    1.0
                };
                {
                    let mut shared = lock_recover(&self.shared);
                    shared.settings.volume = volume;
                    shared.settings.volume_scale = volume_scale;
                    shared.settings.muted = muted;
                }
                let (gain, muted) = self.output_gain_state();
                self.apply_output_gain_to_pipelines(gain, muted);
                push_event(
                    &self.events,
                    BackendEvent::AudioApplied {
                        volume,
                        muted,
                        output: lock_recover(&self.shared).settings.audio_output.clone(),
                    },
                );
                self.end_passthrough_if_needed()
            }
            BackendCommand::SetPlaybackRate(rate) => self.set_playback_rate(rate),
            BackendCommand::SetVisualizerEnabled(enabled) => self.set_visualizer_enabled(enabled),
            BackendCommand::Play { run } => {
                if self.run_is_current(run) {
                    let result = self.start_status_resume();
                    if result.is_ok() {
                        self.prepare_reserved_incoming();
                    }
                    result
                } else {
                    Ok(())
                }
            }
            BackendCommand::Pause { run } => {
                if self.run_is_current(run) {
                    self.start_status_pause()
                } else {
                    Ok(())
                }
            }
            BackendCommand::Stop { run } => {
                if !self.run_is_current(run) {
                    return;
                }
                self.ended_run = None;
                self.retire_playback();
                push_event(&self.events, BackendEvent::Position { run, millis: 0 });
                push_event(
                    &self.events,
                    BackendEvent::State {
                        run,
                        state: BackendState::Stopped,
                    },
                );
                Ok(())
            }
            BackendCommand::Seek {
                run,
                position_millis,
            } => {
                if self.run_is_current(run) {
                    let result = self.start_seek(position_millis);
                    if result.is_ok() {
                        self.prepare_reserved_incoming();
                    }
                    result
                } else {
                    Ok(())
                }
            }
        };

        if let Err(error) = result {
            let run = command_run;
            if let Some(run) = run
                && (changes_transport || !self.active_pipeline().has_session())
            {
                self.fail_playback(Some(run), error);
            } else {
                push_event(&self.events, BackendEvent::OperationFailed { run, error });
            }
        }
    }

    fn play_prepared(
        &mut self,
        item: PreparedRun,
        next: Option<PreparedNext>,
        start_position_millis: u64,
    ) -> Result<(), BackendFailure> {
        self.desired_playing = true;
        let incoming_next = next.clone();
        let command_started_at = Instant::now();
        self.play_command_started_at = Some(command_started_at);
        let _ = self.cancel_status_fade();
        self.ended_run = None;
        self.transition = Transition::None;
        let settings = self.settings();
        let playback_rate = self.playback_rate();
        self.stop_pipeline(Slot::Primary);
        self.stop_pipeline(Slot::Secondary);
        self.secondary.set_visualizer_tap(None);
        let output_gain = settings.output_gain();
        let muted = settings.muted;
        let start_millis =
            SourceClock::from_stream(&item.stream).physical_seek(start_position_millis);
        let visualizer_enabled = {
            let mut shared = lock_recover(&self.shared);
            shared.settings = settings.clone();
            shared.current = Some(item.clone());
            shared.next = next;
            shared.active = Slot::Primary;
            shared.visualizer_enabled
        };
        if visualizer_enabled {
            push_event(
                &self.events,
                BackendEvent::Visualizer {
                    run: item.run,
                    levels: Vec::new(),
                },
            );
        }
        self.push_state(BackendState::Buffering);
        let pipeline_started_at = Instant::now();
        let needs_preroll_seek = start_millis > 0
            || item.stream.end_millis().is_some()
            || playback_rate != DEFAULT_PLAYBACK_RATE;
        let startup_state = if needs_preroll_seek {
            gst::State::Paused
        } else {
            gst::State::Playing
        };
        self.start_pipeline(
            Slot::Primary,
            &item,
            &settings,
            output_gain,
            muted,
            playback_rate,
            startup_state,
            None,
            None,
        )?;
        let primary_tap = self.visualizer_tap(Slot::Primary, visualizer_enabled);
        self.primary.set_visualizer_tap(primary_tap);
        info!(
            run = %item.run,
            uri_scheme = %stream_uri_scheme(item.stream.uri()),
            stream_windowed = item.stream.end_millis().is_some(),
            start_millis,
            playback_rate,
            audio_output = self.primary.audio_output_factory().as_deref().unwrap_or("unknown"),
            elapsed_ms = command_started_at.elapsed().as_millis(),
            pipeline_ms = pipeline_started_at.elapsed().as_millis(),
            "queued GStreamer playback item"
        );
        self.session_started(start_millis, needs_preroll_seek, gst::State::Playing);
        if let Some(duration) = self.primary.fixed_duration() {
            self.push_duration(duration);
        }
        if let Some(next) = incoming_next.as_ref() {
            self.prepare_incoming(next);
        }
        Ok(())
    }

    fn prepare_next(&mut self, next: Option<PreparedNext>) {
        let Some(next) = next else {
            self.clear_prepared_next();
            return;
        };
        if self
            .incoming()
            .is_some_and(|incoming| incoming.item == next)
            || self
                .activating()
                .is_some_and(|pending| pending.item() == &next)
        {
            return;
        }

        let ended_run = self.cancel_handoff_for_replan();
        if let Some(ended_run) = ended_run {
            let mut shared = lock_recover(&self.shared);
            shared.next = Some(next);
            drop(shared);
            self.emit_ended_once(ended_run);
            return;
        }

        {
            let mut shared = lock_recover(&self.shared);
            shared.next = Some(next.clone());
            if let Some(gapless) = self.pipeline_for_slot(shared.active).gapless() {
                lock_recover(gapless).next_needed = None;
            }
        }
        self.prepare_incoming(&next);
    }

    fn clear_prepared_next(&mut self) {
        let ended_run = self.cancel_handoff_for_replan();
        {
            let mut shared = lock_recover(&self.shared);
            shared.next = None;
            if let Some(gapless) = self.pipeline_for_slot(shared.active).gapless() {
                let mut gapless = lock_recover(gapless);
                gapless.about_to_finish_pending = false;
                gapless.next_needed = None;
            }
        }
        if let Some(ended_run) = ended_run {
            self.emit_ended_once(ended_run);
        }
    }

    pub(super) fn start_seek(&mut self, millis: u64) -> Result<(), BackendFailure> {
        self.ended_run = None;
        let _ = self.cancel_status_fade();
        self.finish_overlap();
        // A queued URI, or a drain past a failed one, cannot be taken back by a seek.
        let current_after_gapless_cancel = self.cancel_handoff_for_seek().or_else(|| {
            (self.active_pipeline().has_pending_gapless() || self.active_pipeline().draining())
                .then(|| self.current_item().ok())
                .flatten()
        });
        let target_state = if self.desired_playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        let current_after_gapless_cancel = current_after_gapless_cancel.or_else(|| {
            (self.active_pipeline().native_seekable() == Some(false))
                .then(|| self.current_item().ok())
                .flatten()
                .filter(|item| item.stream.time_seekable())
        });
        // Restarting at 0 avoids an audible skip after AAC priming on some files.
        if let Some(current) = current_after_gapless_cancel
            .or_else(|| (millis == 0).then(|| self.current_item().ok()).flatten())
        {
            // Reopening a bounded source at its end asks for no audio; the track is finished.
            if current.stream.time_seekable()
                && current
                    .stream
                    .end_millis()
                    .is_some_and(|end| current.stream.start_millis().saturating_add(millis) >= end)
            {
                self.emit_ended_once(current.run);
                return Ok(());
            }
            let bitstream = self.active_pipeline().bitstream_format();
            self.start_item_session_at_millis(current, millis, target_state, bitstream)?;
            self.push_logical_position(millis);
            return Ok(());
        }
        if !self.desired_playing
            && let Err(error) = self.active_pipeline().set_state(gst::State::Paused)
        {
            self.retire_playback();
            return Err(error);
        }
        let target = self.active_pipeline().physical_seek_target(millis);
        // Repeating the running target, as repeated lyric taps do, needs no new seek.
        if self.seek.in_flight == Some(target) {
            self.seek.queued = None;
            return Ok(());
        }
        if self.seek.in_flight.is_some() || !self.session_prerolled {
            self.seek.queued = Some(target);
            return Ok(());
        }
        self.send_seek(target).inspect_err(|error| {
            warn!(%error, target_millis = millis, "GStreamer seek request failed");
            if let Some(position) = self.active_pipeline().position() {
                self.push_position(clock_millis(position));
            }
        })
    }

    fn send_seek(&mut self, target: u64) -> Result<(), BackendFailure> {
        let state = if self.desired_playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        // A source that cannot seek natively reopens at the requested source time.
        if self.active_pipeline().native_seekable() == Some(false)
            && let Some(current) = self
                .current_item()
                .ok()
                .filter(|item| item.stream.time_seekable())
        {
            let logical = self.active_pipeline().logical_position(target);
            let bitstream = self.active_pipeline().bitstream_format();
            self.start_item_session_at_millis(current, logical, state, bitstream)?;
            self.push_logical_position(logical);
            return Ok(());
        }
        self.active_pipeline().seek_physical_millis(target)?;
        self.seek.in_flight = self.active_pipeline().seek_in_progress().then_some(target);
        if let Err(error) = self.active_pipeline().set_state(state) {
            self.retire_playback();
            return Err(error);
        }
        Ok(())
    }

    /// Seek bookkeeping for a session just started at `start_millis`.
    fn session_started(&mut self, start_millis: u64, needs_preroll_seek: bool, state: gst::State) {
        self.seek = SeekState {
            in_flight: None,
            queued: needs_preroll_seek.then_some(start_millis),
        };
        self.session_prerolled = false;
        self.awaiting_playing = state == gst::State::Playing;
    }

    fn cancel_gapless_pending(&self) -> Option<(PreparedRun, PreparedNext)> {
        let shared = lock_recover(&self.shared);
        let gapless = self.pipeline_for_slot(shared.active).gapless()?;
        let mut gapless = lock_recover(gapless);
        let pending = gapless.pending.take()?;
        let current = shared.current.clone()?;
        gapless.about_to_finish_pending = false;
        Some((current, pending))
    }

    fn cancel_handoff_for_seek(&mut self) -> Option<PreparedRun> {
        let pending = self.take_activating()?;
        let ended = self.handoff_outgoing_ended(&pending);
        self.stop_pipeline(pending.incoming.slot);
        if ended {
            lock_recover(&self.shared).current.clone()
        } else {
            None
        }
    }

    fn prepare_reserved_incoming(&mut self) {
        let next = lock_recover(&self.shared).next.clone();
        if let Some(next) = next {
            self.prepare_incoming(&next);
        }
    }

    fn current_item(&self) -> Result<PreparedRun, BackendFailure> {
        lock_recover(&self.shared)
            .current
            .clone()
            .ok_or_else(|| "No current playback item is active".into())
    }

    fn start_item_session_at_millis(
        &mut self,
        mut item: PreparedRun,
        position_millis: u64,
        target_state: gst::State,
        bitstream: Option<gst::Caps>,
    ) -> Result<(), BackendFailure> {
        let (settings, volume, muted, playback_rate, visualizer_enabled, slot) =
            self.session_context();
        if item.stream.time_seekable()
            && (item.stream.time_offset_millis().is_some()
                || self.active_pipeline().native_seekable() == Some(false))
        {
            *item.stream.stream = item
                .stream
                .reopened_at(position_millis)
                .map_err(|error| error.to_string())?;
        }
        let start_millis = SourceClock::from_stream(&item.stream).physical_seek(position_millis);
        let run = item.run;
        let active = bitstream.is_some();
        let needs_preroll_seek = start_millis > 0
            || (item.stream.end_millis().is_some() && item.stream.time_offset_millis().is_none())
            || (playback_rate != DEFAULT_PLAYBACK_RATE
                && item.stream.time_offset_millis().is_none());
        let startup_state = if needs_preroll_seek {
            gst::State::Paused
        } else {
            target_state
        };
        self.start_pipeline(
            slot,
            &item,
            &settings,
            volume,
            muted,
            playback_rate,
            startup_state,
            None,
            bitstream,
        )?;
        let tap = self.visualizer_tap(slot, visualizer_enabled);
        self.pipeline_for_slot_mut(slot).set_visualizer_tap(tap);
        self.session_started(start_millis, needs_preroll_seek, target_state);
        push_event(&self.events, BackendEvent::Bitstream { run, active });
        Ok(())
    }

    /// The logical position a restarted session continues from.
    fn restart_position_millis(&self) -> u64 {
        self.seek
            .queued
            .or(self.seek.in_flight)
            .or_else(|| self.active_pipeline().position().map(clock_millis))
            .map(|position| self.active_pipeline().logical_position(position))
            .unwrap_or_default()
    }

    /// The decoded session's first stream can reach the output undecoded.
    fn start_passthrough(&mut self, format: gst::Caps) {
        let Ok(current) = self.current_item() else {
            return;
        };
        let position = self.restart_position_millis();
        let state = if self.desired_playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        info!(run = %current.run, %format, "passing compressed audio to the output");
        // A crossfade prepared before the format was known can't mix with it.
        self.clear_incoming();
        if let Err(error) =
            self.start_item_session_at_millis(current, position, state, Some(format))
        {
            self.fail_playback(self.timing_run_id(), error);
        }
    }

    /// A setting that changes samples continues the current track decoded.
    fn end_passthrough_if_needed(&mut self) -> Result<(), BackendFailure> {
        let Some(format) = self.active_pipeline().bitstream_format() else {
            return Ok(());
        };
        let current = self.current_item()?;
        let allowed = {
            let shared = lock_recover(&self.shared);
            passthrough_allowed(
                &format,
                &shared.settings,
                shared.playback_rate,
                shared.processing.as_ref(),
                current.stream.loudness.as_ref(),
            )
        };
        if allowed {
            return Ok(());
        }
        let position = self.restart_position_millis();
        let state = if self.desired_playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        self.start_item_session_at_millis(current, position, state, None)?;
        self.push_logical_position(position);
        if !self.desired_playing {
            self.push_state(BackendState::Paused);
        }
        self.prepare_reserved_incoming();
        Ok(())
    }

    fn session_context(&self) -> (BackendAudioSettings, f64, bool, f64, bool, Slot) {
        let shared = lock_recover(&self.shared);
        (
            shared.settings.clone(),
            shared.settings.output_gain(),
            shared.settings.muted,
            shared.playback_rate,
            shared.visualizer_enabled,
            shared.active,
        )
    }

    fn set_processing(&mut self, request: ProcessingRequest) {
        if let Err(message) =
            audio_processing::effects::EffectsBin::validate_available(&request.config)
        {
            push_event(
                &self.events,
                BackendEvent::ProcessingFailed {
                    output: request.output,
                    message,
                },
            );
            return;
        }
        lock_recover(&self.shared).processing = Some(request.clone());
        let mut applied = true;
        for slot in [Slot::Primary, Slot::Secondary] {
            if let Err(message) = self
                .pipeline_for_slot_mut(slot)
                .configure_processing(&request)
            {
                applied = false;
                push_event(
                    &self.events,
                    BackendEvent::ProcessingFailed {
                        output: request.output.clone(),
                        message: message.to_string(),
                    },
                );
            }
        }
        self.sync_processing_taps();
        if let Err(error) = self.end_passthrough_if_needed() {
            self.fail_playback(self.timing_run_id(), error);
        }
        // Without an audio graph in use, nothing else reports neutral processing applied.
        let graph_unused = self.active_pipeline().is_bitstream();
        if applied && (graph_unused || lock_recover(&self.shared).current.is_none()) {
            let rate = 48_000;
            let preamp_db = request.config.equalizer.as_ref().map_or(0.0, |p| {
                audio_processing::effects::effective_preamp_db(p, rate)
            });
            push_event(
                &self.events,
                BackendEvent::ProcessingApplied {
                    output: request.output,
                    config: request.config,
                    sample_rate: None,
                    preamp_db,
                },
            );
        }
    }

    fn poll_processing(&mut self) {
        for slot in [Slot::Primary, Slot::Secondary] {
            match self.pipeline_for_slot_mut(slot).poll_processing() {
                Ok(Some((config, sample_rate, preamp_db))) if self.is_active_slot(slot) => {
                    if let Some(output) = self.pipeline_for_slot(slot).processing_output() {
                        push_event(
                            &self.events,
                            BackendEvent::ProcessingApplied {
                                output,
                                config,
                                sample_rate,
                                preamp_db,
                            },
                        );
                    }
                }
                Err(message) => {
                    if let Some(output) = self.pipeline_for_slot(slot).processing_output() {
                        push_event(
                            &self.events,
                            BackendEvent::ProcessingFailed { output, message },
                        );
                    }
                }
                _ => {}
            }
        }
    }

    /// Handles every input already queued. Returns false after a shutdown request.
    #[cfg(test)]
    fn poll_bus(&mut self) -> bool {
        let inputs = self.inputs.try_iter().collect();
        self.handle_inputs(inputs)
    }

    fn handle_inputs(&mut self, inputs: VecDeque<WorkerInput>) -> bool {
        let mut inputs = inputs.into_iter().peekable();
        while let Some(input) = inputs.next() {
            match input {
                WorkerInput::Command(command) => self.handle_command(*command),
                WorkerInput::Processing(request) => self.set_processing(request),
                WorkerInput::Bus(slot, id, message) => {
                    // Use the latest consecutive refill update to avoid an unnecessary pause.
                    let superseded = message.type_() == gst::MessageType::Buffering
                        && inputs.peek().is_some_and(|next| {
                            matches!(next, WorkerInput::Bus(next_slot, _, next)
                                if *next_slot == slot
                                    && next.type_() == gst::MessageType::Buffering
                                    && next.src() == message.src())
                        });
                    if !superseded {
                        self.handle_message(slot, id, &message);
                    }
                }
                WorkerInput::Wake => {}
                WorkerInput::Shutdown => return false,
            }
        }
        true
    }

    /// The next time the worker must act without a new input.
    fn next_deadline(&self) -> Option<Duration> {
        const STEP: Duration = Duration::from_millis(50);
        if self.status_fade.is_some() {
            return Some(STEP);
        }
        if !self.desired_playing || !self.active_pipeline().has_session() {
            return None;
        }
        if self
            .active_pipeline()
            .gapless()
            .is_some_and(|gapless| lock_recover(gapless).about_to_finish_pending)
        {
            return Some(STEP);
        }
        let report = Duration::from_millis(500).saturating_sub(self.last_position_tick.elapsed());
        Some(
            self.crossfade_start_wait()
                .map_or(report, |wait| wait.min(report)),
        )
    }

    fn crossfade_start_wait(&self) -> Option<Duration> {
        let NextTransition::Crossfade { duration_millis } =
            lock_recover(&self.shared).next.as_ref()?.transition
        else {
            return None;
        };
        let pipeline = self.active_pipeline();
        let remaining = pipeline.logical_remaining(
            clock_millis(pipeline.position()?),
            clock_millis(pipeline.duration()?),
        );
        let start = crossfade_start_remaining_millis(duration_millis, self.playback_rate());
        // Past the start point, readiness messages drive the start instead.
        let wait = remaining.checked_sub(start).filter(|wait| *wait > 0)?;
        Some(Duration::from_millis(
            (wait as f64 / self.playback_rate()).ceil() as u64,
        ))
    }

    fn log_audio_warning(&self, warning: AudioWarning) {
        let slot = [Slot::Primary, Slot::Secondary].into_iter().find(|slot| {
            warning
                .element
                .as_deref()
                .is_some_and(|name| self.pipeline_for_slot(*slot).owns_audio_output(name))
        });
        let pipeline = slot.map(|slot| self.pipeline_for_slot(slot));
        let run = slot.and_then(|slot| self.run_for_slot(slot));
        let position_millis = pipeline.and_then(|pipeline| {
            pipeline
                .position()
                .map(|position| pipeline.logical_position(clock_millis(position)))
        });
        let duration_millis = pipeline.and_then(|pipeline| {
            pipeline
                .duration()
                .map(|duration| pipeline.logical_duration(clock_millis(duration)))
        });
        let role = match slot {
            Some(slot) if slot == self.active_slot() => "current",
            Some(_) if run.is_some() => "outgoing",
            Some(_) => "prepared",
            None => "unassigned",
        };
        warn!(
            category = %warning.category, element = ?warning.element, message = %warning.message,
            handling_delay_ms = warning.captured_at.elapsed().as_millis(),
            ?slot, run = run.map(RunId::get), role,
            ?position_millis, ?duration_millis,
            buffering_percent = pipeline.and_then(PlayerPipeline::buffering_percent),
            "GStreamer audio output warning"
        );
    }

    fn handle_message(&mut self, slot: Slot, id: PipelineId, message: &gst::Message) {
        if !lock_recover(&self.shared).pipeline_is_live(slot, id) {
            return;
        }
        if audio_processing::effects::EffectsBin::is_change_message(message) {
            self.poll_processing();
            return;
        }
        use gst::MessageView;
        if self.is_active_slot(slot)
            && let Some(peak_db) = self.pipeline_for_slot(slot).processing_peak(message)
            && let Some(output) = self.pipeline_for_slot(slot).processing_output()
        {
            push_event(
                &self.events,
                BackendEvent::ProcessingMeter {
                    output,
                    peak_db,
                    spectrum_db: Vec::new(),
                },
            );
        }

        let preroll_message = matches!(message.view(), MessageView::AsyncDone(_))
            || matches!(message.view(), MessageView::StateChanged(state)
                if state.current() == gst::State::Paused
                    && state.pending() == gst::State::VoidPending);
        if preroll_message
            && self.message_source_is_pipeline(slot, message)
            && let Err(error) = self.pipeline_for_slot(slot).complete_seek()
        {
            if self.incoming_matches(slot, id) {
                self.fail_incoming(slot, id, error);
            } else if self.is_active_slot(slot) {
                self.seek = SeekState::default();
                push_event(
                    &self.events,
                    BackendEvent::OperationFailed {
                        run: self.timing_run_id(),
                        error,
                    },
                );
                let target = if self.desired_playing {
                    gst::State::Playing
                } else {
                    gst::State::Paused
                };
                if let Err(error) = self.active_pipeline().set_state(target) {
                    self.fail_playback(self.timing_run_id(), error);
                } else if let Some(position) = self.active_pipeline().position() {
                    self.push_position(clock_millis(position));
                }
            } else if self.pending_handoff_matches(slot, id) {
                self.fail_handoff(slot, id, error);
            } else if self.error_is_relevant_slot(slot) {
                self.fail_playback(self.run_for_slot(slot), error);
            }
            return;
        }

        match message.view() {
            MessageView::StreamStart(_) if self.message_source_is_pipeline(slot, message) => {
                let initial = !self.pipeline_for_slot(slot).initial_stream_started;
                self.pipeline_for_slot_mut(slot).initial_stream_started = true;
                // Queuing the next URI can precede the current item's first message.
                if self.is_active_slot(slot) && !initial {
                    self.handle_stream_start();
                }
                return;
            }
            MessageView::Buffering(buffering) => {
                self.handle_buffering(
                    slot,
                    id,
                    buffering.percent().clamp(0, 100) as u8,
                    buffering.buffering_stats().0,
                );
                return;
            }
            MessageView::SegmentDone(_) => {
                self.pipeline_for_slot(slot).segment_done(message.seqnum())
            }
            MessageView::NewClock(clock) => {
                debug!(?slot, clock = ?clock.clock().map(|clock| clock.name()), "GStreamer playback clock selected");
            }
            // The documented recovery: pausing and resuming lets the pipeline select a clock.
            MessageView::ClockLost(_) => {
                if let Err(error) = self.pipeline_for_slot(slot).recover_clock() {
                    warn!(?slot, %error, "could not recover the GStreamer playback clock");
                }
            }
            MessageView::Latency(_) => self.pipeline_for_slot(slot).recalculate_latency(),
            MessageView::Warning(warning) => {
                warn!(?slot, message = %warning.error(), details = ?warning.debug(), "GStreamer playback warning");
            }
            _ => {}
        }

        if self.pending_handoff_matches(slot, id) {
            match message.view() {
                MessageView::Error(error) => {
                    let output = self.pipeline_for_slot(slot).audio_output_factory();
                    let details = gstreamer_playback_error(
                        message,
                        "prepared playback handoff",
                        output.as_deref(),
                    )
                    .unwrap_or_else(|| BackendFailure::new(error.error().to_string()));
                    self.fail_handoff(slot, id, details);
                }
                MessageView::StateChanged(state)
                    if self.message_source_is_pipeline(slot, message)
                        && state.current() == gst::State::Playing
                        && state.pending() == gst::State::VoidPending =>
                {
                    self.confirm_handoff(slot, id);
                }
                _ => {}
            }
            return;
        }

        match message.view() {
            _ if preroll_message
                && self.incoming_matches(slot, id)
                && self.message_source_is_pipeline(slot, message) =>
            {
                self.handle_incoming_async_done(slot, id);
                return;
            }
            MessageView::Error(error) if self.incoming_matches(slot, id) => {
                let output = self.pipeline_for_slot(slot).audio_output_factory();
                let details =
                    gstreamer_playback_error(message, "prepared playback", output.as_deref())
                        .unwrap_or_else(|| BackendFailure::new(error.error().to_string()));
                self.fail_incoming(slot, id, details);
                return;
            }
            _ => {}
        }

        match message.view() {
            MessageView::StateChanged(state)
                if self.message_source_is_pipeline(slot, message) && self.is_active_slot(slot) =>
            {
                // Synchronous prerolls and live sources post no ASYNC_DONE for a queued seek.
                if preroll_message
                    && self.active_pipeline().has_prerolled()
                    && !self.active_pipeline().seek_in_progress()
                    && self.seek.queued.is_some()
                {
                    self.handle_async_done();
                    return;
                }
                if let Some(started_at) = self.play_command_started_at {
                    let run = self.timing_run_id();
                    debug!(
                        run = run.map(RunId::get).unwrap_or_default(),
                        ?slot,
                        old = ?state.old(),
                        current = ?state.current(),
                        pending = ?state.pending(),
                        elapsed_ms = started_at.elapsed().as_millis(),
                        "GStreamer startup state changed"
                    );
                }
                let playback_state = match state.current() {
                    gst::State::Null | gst::State::Ready => BackendState::Stopped,
                    gst::State::Paused => BackendState::Paused,
                    gst::State::Playing => BackendState::Playing,
                    gst::State::VoidPending => BackendState::Buffering,
                };
                self.handle_state_changed(playback_state);
            }
            MessageView::AsyncDone(_)
                if self.is_active_slot(slot) && self.message_source_is_pipeline(slot, message) =>
            {
                if let Some(started_at) = self.play_command_started_at {
                    let run = self.timing_run_id();
                    debug!(
                        run = run.map(RunId::get).unwrap_or_default(),
                        ?slot,
                        elapsed_ms = started_at.elapsed().as_millis(),
                        "GStreamer startup async done"
                    );
                }
                self.handle_async_done();
            }
            MessageView::Tag(tag) if self.is_active_slot(slot) => {
                self.log_stream_diagnostics(slot, &tag.tags());
            }
            MessageView::DurationChanged(_) if self.is_active_slot(slot) => {
                if !self.seek_busy()
                    && let Some(duration) = self.active_pipeline().duration()
                {
                    self.push_physical_duration(clock_millis(duration));
                }
            }
            MessageView::Eos(_) => self.handle_end(slot),
            MessageView::Error(error_message) => {
                let output = self.pipeline_for_slot(slot).audio_output_factory();
                let error = gstreamer_playback_error(message, "playback", output.as_deref())
                    .unwrap_or_else(|| BackendFailure::new(error_message.error().to_string()));
                let source = message
                    .src()
                    .map(|source| source.name().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let active_slot = self.active_slot();
                let relevant = self.error_is_relevant_slot(slot);
                error!(
                    message = %error,
                    %source,
                    ?slot,
                    ?active_slot,
                    relevant,
                    "GStreamer playback error"
                );
                if relevant
                    && (self.handle_gapless_preload_error(slot, error_message, &error)
                        || self.handle_crossfade_next_error(slot, &error))
                {
                    return;
                }
                if relevant {
                    let run = self.run_for_slot(slot).or_else(|| self.timing_run_id());
                    self.fail_playback(run, error);
                }
            }
            _ => {}
        }
    }

    fn log_stream_diagnostics(&self, slot: Slot, tags: &gst::TagListRef) {
        let codec = tags
            .get::<gst::tags::AudioCodec>()
            .map(|value| value.get().to_string())
            .or_else(|| {
                tags.get::<gst::tags::Codec>()
                    .map(|value| value.get().to_string())
            })
            .or_else(|| {
                tags.get::<gst::tags::ContainerFormat>()
                    .map(|value| value.get().to_string())
            });
        let bitrate = tags
            .get::<gst::tags::Bitrate>()
            .map(|value| value.get())
            .or_else(|| {
                tags.get::<gst::tags::NominalBitrate>()
                    .map(|value| value.get())
            })
            .map(|bits_per_second| bits_per_second / 1_000);
        if codec.is_none() && bitrate.is_none() {
            return;
        }
        let run = self.run_for_slot(slot).or_else(|| self.timing_run_id());
        debug!(
            run = run.map(RunId::get).unwrap_or_default(),
            codec = codec.as_deref().unwrap_or("unknown"),
            reported_bitrate_kbps = bitrate,
            "received GStreamer stream metadata"
        );
    }

    fn handle_gapless_preload_error(
        &mut self,
        slot: Slot,
        message: &gst::message::Error,
        error: &BackendFailure,
    ) -> bool {
        if !self.is_active_slot(slot) {
            return false;
        }
        let Some(uri) = message
            .details()
            .and_then(|details| details.get::<&str>("uri").ok())
        else {
            return false;
        };
        {
            let shared = lock_recover(&self.shared);
            // A pending item alone cannot identify the failing stream, including repeat.
            if shared
                .current
                .as_ref()
                .is_none_or(|item| item.stream.uri() == uri)
            {
                return false;
            }
            // The failed item can report again while its drain is under way.
            if self.pipeline_for_slot(slot).draining() {
                return true;
            }
            let gapless = lock_recover(self.pipeline_for_slot(slot).gapless().unwrap());
            if gapless
                .pending
                .as_ref()
                .is_none_or(|item| item.stream.uri() != uri)
            {
                return false;
            }
        }
        let Some((current, pending)) = self.cancel_gapless_pending() else {
            return false;
        };
        warn!(
            next_run = %pending.run,
            error = %error,
            "gapless next stream failed before commit; finishing the current stream"
        );
        push_event(
            &self.events,
            BackendEvent::NextPreparationFailed {
                current_run: current.run,
                next_run: pending.run,
                error: error.clone(),
            },
        );
        // The current run ends through the drained EOS, so core skips the failed item.
        self.pipeline_for_slot(slot).drain();
        true
    }

    fn handle_crossfade_next_error(&mut self, slot: Slot, error: &BackendFailure) -> bool {
        let Some((from, old_run)) = self.overlap().filter(|(from, _)| *from == slot) else {
            return false;
        };
        warn!(%error, old_run = %old_run, ?from, "outgoing crossfade tail failed");
        self.finish_overlap();
        true
    }

    fn handle_stream_start(&mut self) {
        let started = (|| {
            let mut shared = lock_recover(&self.shared);
            let gapless = self.pipeline_for_slot(shared.active).gapless()?;
            let mut gapless = lock_recover(gapless);
            let item = gapless.pending.take()?;
            let old_run = shared.current.as_ref()?.run;
            if shared.next.as_ref() == Some(&item) {
                shared.next = None;
            }
            shared.current = Some(PreparedRun::from_next(&item));
            Some((old_run, item.run, item.stream))
        })();
        self.handle_stream_started_run(started);
    }

    fn handle_stream_started_run(&mut self, started: Option<(RunId, RunId, PreparedStream)>) {
        let Some((old_run, new_run, stream)) = started else {
            return;
        };
        info!(
            old_run = %old_run,
            new_run = %new_run,
            "gapless stream started"
        );
        self.seek = SeekState::default();
        self.ended_run = None;
        self.last_position_tick = Instant::now();
        self.pipeline_for_slot_mut(self.active_slot())
            .activate_stream(&stream);
        let visualizer_enabled = self.visualizer_enabled();
        self.sync_visualizer_taps(visualizer_enabled);
        if visualizer_enabled {
            push_event(
                &self.events,
                BackendEvent::Visualizer {
                    run: new_run,
                    levels: Vec::new(),
                },
            );
        }
        push_event(
            &self.events,
            BackendEvent::Transitioned { old_run, new_run },
        );
        if self.active_pipeline().is_bitstream() {
            push_event(
                &self.events,
                BackendEvent::Bitstream {
                    run: new_run,
                    active: true,
                },
            );
        }
        push_event(
            &self.events,
            BackendEvent::Position {
                run: new_run,
                millis: 0,
            },
        );
        self.push_seekable();
    }

    pub(super) fn handle_state_changed(&mut self, state: BackendState) {
        if state == BackendState::Playing {
            self.awaiting_playing = false;
        }
        // Starting sessions and flushing seeks pass through Paused on their way to playing.
        if self.desired_playing
            && matches!(state, BackendState::Stopped | BackendState::Paused)
            && (self.awaiting_playing || self.seek_busy())
        {
            return;
        }
        if state == BackendState::Paused && !self.desired_playing {
            self.sample_pause_position();
        }
        self.push_state(state);
    }

    fn seek_busy(&self) -> bool {
        self.seek.in_flight.is_some()
            || self.seek.queued.is_some()
            || self.active_pipeline().seek_in_progress()
    }

    fn handle_buffering(
        &mut self,
        slot: Slot,
        id: PipelineId,
        percent: u8,
        mode: gst::BufferingMode,
    ) {
        if self.is_active_slot(slot) && percent < 100 && self.gapless_preload_near_end() {
            debug!(
                percent,
                "ignoring buffering while gapless handoff is pending near end"
            );
            return;
        }
        if let Err(error) = self.pipeline_for_slot(slot).set_buffering(percent, mode) {
            if self.fail_handoff(slot, id, error.clone())
                || self.handle_crossfade_next_error(slot, &error)
            {
                return;
            }
            if self.incoming_matches(slot, id) {
                self.fail_incoming(slot, id, error);
                return;
            }
            self.fail_playback(self.run_for_slot(slot), error);
            return;
        }
        // Both halves of a crossfade wait while either one refills.
        if let Some((from, _)) = self.overlap()
            && (slot == from || self.is_active_slot(slot))
        {
            let other = if slot == from {
                self.active_slot()
            } else {
                from
            };
            let held = self.pipeline_for_slot(slot).is_buffering();
            if let Err(error) = self.pipeline_for_slot(other).set_held(held)
                && !self.handle_crossfade_next_error(other, &error)
            {
                self.fail_playback(self.timing_run_id(), error);
                return;
            }
        }
        if matches!(percent, 0 | 100) {
            debug!(
                ?slot,
                pipeline_id = id.0,
                percent,
                ?mode,
                "GStreamer buffering"
            );
        }
        if !self.is_active_slot(slot) {
            return;
        }
        if let Some(run) = self.timing_run_id() {
            push_event(&self.events, BackendEvent::Buffering { run, percent });
        }
        if self.desired_playing && self.active_pipeline().is_buffering() {
            self.push_state(BackendState::Buffering);
        }
    }

    fn gapless_preload_near_end(&self) -> bool {
        if !self.active_pipeline().has_pending_gapless() {
            return false;
        }
        let Some(position) = self.active_pipeline().position() else {
            return false;
        };
        let Some(duration) = self.active_pipeline().duration() else {
            return false;
        };
        let position_ms = clock_millis(position);
        let duration_ms = clock_millis(duration);
        let remaining_ms = self
            .active_pipeline()
            .logical_remaining(position_ms, duration_ms);
        duration_ms > 0 && position_ms > 0 && remaining_ms < GAPLESS_BUFFERING_IGNORE_REMAINING_MS
    }

    /// A preroll finished: send the queued target, or finish the seek that ran.
    fn handle_async_done(&mut self) {
        self.push_seekable();
        self.session_prerolled = true;
        if self.active_pipeline().seek_in_progress() {
            return;
        }
        self.seek.in_flight = None;
        if let Some(target) = self.seek.queued.take() {
            let run = self.timing_run_id();
            let Err(error) = self.send_seek(target) else {
                return;
            };
            if !self.active_pipeline().has_session() {
                if let Some(run) = run {
                    push_event(&self.events, BackendEvent::Error { run, error });
                }
                return;
            }
            warn!(%error, target_millis = target, "queued seek failed; playing from the current position");
            let state = if self.desired_playing {
                gst::State::Playing
            } else {
                gst::State::Paused
            };
            if let Err(error) = self.active_pipeline().set_state(state) {
                self.fail_playback(run, error);
                return;
            }
        }
        if let Some(position) = self.active_pipeline().position() {
            self.push_position(clock_millis(position));
        }
    }

    fn push_state(&mut self, state: BackendState) {
        let state = if self.desired_playing
            && self.active_pipeline().is_buffering()
            && matches!(state, BackendState::Playing | BackendState::Paused)
        {
            BackendState::Buffering
        } else {
            state
        };
        let run = self.timing_run_id();
        if state == BackendState::Playing
            && let Some(started_at) = self.play_command_started_at.take()
        {
            info!(
                run = run.map(RunId::get).unwrap_or_default(),
                elapsed_ms = started_at.elapsed().as_millis(),
                "GStreamer playback reached playing"
            );
            if let Some(run) = run {
                push_event(&self.events, BackendEvent::Started { run });
            }
        }
        if let Some(run) = run {
            push_event(&self.events, BackendEvent::State { run, state });
        }
    }

    fn handle_end(&mut self, slot: Slot) {
        let pipeline_id = lock_recover(&self.shared).pipeline_id(slot);
        debug!(
            ?slot,
            pipeline_id = pipeline_id.map(|id| id.0),
            run = self.run_for_slot(slot).map(RunId::get),
            "GStreamer end of stream"
        );
        if self.overlap().is_some_and(|(from, _)| from == slot) {
            self.finish_overlap();
            return;
        }
        if self.gapless_handoff_waits_for_confirmation(slot) {
            return;
        }
        if self.desired_playing && self.is_active_slot(slot) && self.promote_prepared_gapless() {
            return;
        }
        if self.is_active_slot(slot) {
            self.clear_incoming_candidate_at_end(slot);
            let run = self.timing_run_id();
            info!(
                run = run.map(RunId::get).unwrap_or_default(),
                "playback reached end"
            );
            if let Some(run) = run {
                self.emit_ended_once(run);
            }
        }
    }

    fn start_status_pause(&mut self) -> Result<(), BackendFailure> {
        if !self.desired_playing {
            return Ok(());
        }
        self.desired_playing = false;
        let previous_fade = self.status_fade.take();
        self.cancel_unconfirmed_handoff_for_pause();
        let (volume, muted, enabled) = self.status_fade_gain_settings();
        debug!(
            fade_on_play_stop = enabled,
            volume_gain = volume,
            muted,
            "GStreamer pause requested"
        );
        if !self.active_pipeline().has_session() {
            self.push_state(BackendState::Paused);
            return Ok(());
        }
        if !enabled
            || muted
            || volume <= 0.0
            || self.active_pipeline().is_buffering()
            || self.seek_busy()
        {
            self.pause_current_pipeline()?;
            return Ok(());
        }
        let slot = self.active_slot();
        self.status_fade = Some(StatusFade::new(slot, StatusFadeTarget::Pause, muted));
        let start = previous_fade.is_none().then_some(volume);
        self.pipeline_for_slot(slot)
            .fade_output(start, 0.0, STATUS_FADE_DURATION);
        self.with_tail(|tail| {
            tail.fade_output(start, 0.0, STATUS_FADE_DURATION);
            Ok(())
        });
        Ok(())
    }

    /// Lets a crossfade tail follow transport; its failure retires only the tail.
    fn with_tail(&mut self, apply: impl FnOnce(&PlayerPipeline) -> Result<(), BackendFailure>) {
        let Some((from, _)) = self.overlap() else {
            return;
        };
        if let Err(error) = apply(self.pipeline_for_slot(from)) {
            self.handle_crossfade_next_error(from, &error);
        }
    }

    fn start_status_resume(&mut self) -> Result<(), BackendFailure> {
        self.desired_playing = true;
        let previous_fade = self.status_fade.take();
        let (volume, muted, enabled) = self.status_fade_gain_settings();
        debug!(
            fade_on_play_stop = enabled,
            volume_gain = volume,
            muted,
            "GStreamer resume requested"
        );
        if self.seek_busy() {
            self.active_pipeline().set_output_volume(volume, muted);
            if self.active_pipeline().seek_in_progress() {
                self.active_pipeline().set_state(gst::State::Playing)?;
            }
            self.push_state(BackendState::Buffering);
            return Ok(());
        }
        if !self.active_pipeline().has_session() {
            return if self.current_item().is_ok() {
                Err("No active GStreamer session to resume".into())
            } else {
                Ok(())
            };
        }
        if self.active_pipeline().is_buffering() || !enabled || muted || volume <= 0.0 {
            self.with_tail(|tail| {
                tail.set_output_volume(volume, muted);
                tail.set_state(gst::State::Playing).map(drop)
            });
        }
        if self.active_pipeline().is_buffering() {
            self.active_pipeline().set_output_volume(volume, muted);
            self.active_pipeline().set_state(gst::State::Playing)?;
            self.push_state(BackendState::Buffering);
            return Ok(());
        }
        if !enabled || muted || volume <= 0.0 {
            self.active_pipeline().set_output_volume(volume, muted);
            return self
                .active_pipeline()
                .set_state(gst::State::Playing)
                .map(|result| {
                    if result != gst::StateChangeSuccess::Async {
                        self.handle_state_changed(BackendState::Playing);
                    }
                });
        }
        let slot = self.active_slot();
        let start = previous_fade.is_none().then_some(0.0);
        self.pipeline_for_slot(slot)
            .fade_output(start, volume, STATUS_FADE_DURATION);
        self.with_tail(|tail| {
            tail.fade_output(start, volume, STATUS_FADE_DURATION);
            tail.set_state(gst::State::Playing).map(drop)
        });
        self.pipeline_for_slot(slot)
            .set_state(gst::State::Playing)
            .map(|result| {
                if result != gst::StateChangeSuccess::Async {
                    self.handle_state_changed(BackendState::Playing);
                }
                self.status_fade = Some(StatusFade::new(slot, StatusFadeTarget::Playing, muted));
            })
    }

    fn update_status_fade(&mut self) {
        let Some(fade) = self.status_fade else {
            return;
        };
        let pipeline = self.pipeline_for_slot(fade.slot);
        if !pipeline.is_buffering()
            && self.ended_run != self.timing_run_id()
            && !pipeline.fade_finished()
        {
            return;
        }

        self.status_fade = None;
        match fade.target {
            StatusFadeTarget::Pause => {
                self.pipeline_for_slot(fade.slot)
                    .set_output_volume(0.0, fade.muted);
                if let Err(error) = self.pause_current_pipeline() {
                    self.fail_playback(self.timing_run_id(), error);
                    return;
                }
            }
            StatusFadeTarget::Playing => {
                let (volume, muted) = self.output_gain_state();
                self.pipeline_for_slot(fade.slot)
                    .set_output_volume(volume, muted);
                self.with_tail(|tail| {
                    tail.set_output_volume(volume, muted);
                    Ok(())
                });
            }
        }
    }

    fn pause_current_pipeline(&mut self) -> Result<(), BackendFailure> {
        self.sample_pause_position();
        self.with_tail(|tail| tail.set_state(gst::State::Paused).map(drop));
        if self.active_pipeline().set_state(gst::State::Paused)? != gst::StateChangeSuccess::Async {
            self.handle_state_changed(BackendState::Paused);
        }
        Ok(())
    }

    fn sample_pause_position(&mut self) {
        if let Some(position) = self.active_pipeline().position() {
            self.push_position(clock_millis(position));
        }
    }

    fn cancel_status_fade(&mut self) -> Option<StatusFade> {
        let fade = self.status_fade.take();
        if let Some(fade) = fade {
            let (volume, muted) = self.output_gain_state();
            let volume = if self.desired_playing { volume } else { 0.0 };
            self.pipeline_for_slot(fade.slot)
                .set_output_volume(volume, muted);
            self.with_tail(|tail| {
                tail.set_output_volume(volume, muted);
                Ok(())
            });
        }
        fade
    }

    fn status_fade_gain_settings(&self) -> (f64, bool, bool) {
        // Compressed audio has no gain to fade.
        let bitstream = self.active_pipeline().is_bitstream();
        let shared = lock_recover(&self.shared);
        (
            shared.settings.output_gain(),
            shared.settings.muted,
            shared.settings.fade_on_status_change && !bitstream,
        )
    }

    fn tick(&mut self) {
        if let Some(format) = self.active_pipeline().bitstream_request() {
            self.start_passthrough(format);
        }
        self.sync_processing_taps();
        if self.desired_playing
            && self.active_pipeline().is_playing()
            && !self.active_pipeline().is_buffering()
            && !self.seek_busy()
            && !self.active_pipeline().has_pending_gapless()
            && self.active_pipeline().take_segment_done()
        {
            self.continue_segment();
        }
        if self.active_pipeline().gapless().is_some_and(|gapless| {
            let gapless = lock_recover(gapless);
            gapless.about_to_finish_pending && gapless.pending.is_none()
        }) && self.desired_playing
            && self.active_pipeline().is_playing()
            && !self.active_pipeline().is_buffering()
            && !self.seek_busy()
            && self
                .active_pipeline()
                .position()
                .is_some_and(|position| position > gst::ClockTime::ZERO)
        {
            let deferred_preload = {
                let mut shared = lock_recover(&self.shared);
                self.pipeline_for_slot(shared.active)
                    .gapless()
                    .and_then(|gapless| {
                        let mut gapless = lock_recover(gapless);
                        if !gapless.about_to_finish_pending || shared.next.is_none() {
                            return None;
                        }
                        match about_to_finish_action(&mut shared, &mut gapless) {
                            AboutToFinishAction::Preload(next) => Some(*next),
                            AboutToFinishAction::Ignore => None,
                        }
                    })
            };
            if let Some(item) = deferred_preload {
                info!(
                    next_run = %item.run,
                    uri = %item.stream.redacted_uri(),
                    "preloading late gapless next stream"
                );
                if let Err(error) = self.active_pipeline_mut().set_stream(&item.stream) {
                    self.cancel_gapless_pending();
                    self.report_next_preparation_failure(item.run, error);
                }
            }
        }
        let next_needed = self
            .active_pipeline()
            .gapless()
            .and_then(|gapless| lock_recover(gapless).next_needed.take());
        if let Some(run) = next_needed {
            push_event(&self.events, BackendEvent::NextNeeded { run });
        }
        self.update_status_fade();
        if self.status_fade.is_some() {
            return;
        }
        self.maybe_start_crossfade();
        // A tail whose fade has rendered is silent; its EOS may still be far off.
        if self
            .overlap()
            .is_some_and(|(from, _)| self.pipeline_for_slot(from).transition_finished())
        {
            self.finish_overlap();
        }

        if self.last_position_tick.elapsed() >= Duration::from_millis(500) {
            self.last_position_tick = Instant::now();
            if self.seek_busy() || !self.session_prerolled {
                return;
            }
            debug!(
                run = self.timing_run_id().map(RunId::get),
                "querying GStreamer playback timing"
            );
            if tracing::enabled!(tracing::Level::DEBUG) {
                self.active_pipeline().log_output_timing();
            }
            let position = self.active_pipeline().position();
            if let Some(position) = position {
                self.push_position(clock_millis(position));
            }
            let duration = self.active_pipeline().duration();
            if let Some(duration) = duration {
                self.push_physical_duration(clock_millis(duration));
            }
            debug!(
                run = self.timing_run_id().map(RunId::get),
                position_millis = position.map(clock_millis),
                duration_millis = duration.map(clock_millis),
                buffering_percent = self.active_pipeline().buffering_percent(),
                pipeline_playing = self.active_pipeline().is_playing(),
                "GStreamer playback timing"
            );
        }
    }

    /// Positions wait for running seeks and the first preroll, so none are stale.
    pub(super) fn push_position(&mut self, millis: u64) {
        if self.seek_busy() || !self.session_prerolled {
            return;
        }
        let logical_millis = self.active_pipeline().logical_position(millis);
        if let Some(run) = self.timing_run_id() {
            if self.ended_run == Some(run) {
                return;
            }
            self.push_logical_position(logical_millis);
        }
    }

    fn promote_prepared_gapless(&mut self) -> bool {
        if !self.desired_playing {
            return false;
        }
        let Some(incoming) = self.incoming().filter(|incoming| {
            incoming.phase == IncomingPhase::Ready
                && incoming.item.transition == NextTransition::Gapless
        }) else {
            return false;
        };
        let slot = incoming.slot;
        let id = incoming.id;
        let (volume, muted) = self.output_gain_state();
        self.pipeline_for_slot(slot)
            .set_output_volume(volume, muted);
        match self.pipeline_for_slot(slot).set_state(gst::State::Playing) {
            Ok(result) => self.begin_incoming_handoff(slot, id, result),
            Err(error) => {
                self.fail_incoming(slot, id, error);
                false
            }
        }
    }

    fn continue_segment(&mut self) {
        let bitstream = self.active_pipeline().is_bitstream();
        let next = {
            let shared = lock_recover(&self.shared);
            let Some(gapless) = self.pipeline_for_slot(shared.active).gapless() else {
                return;
            };
            let next = shared
                .next
                .as_ref()
                .filter(|next| {
                    (next.transition == NextTransition::Gapless || bitstream)
                        && next.stream.allows_preloading
                        && shared.current.as_ref().is_some_and(|current| {
                            current.stream.allows_preloading
                                && current.stream.uri() == next.stream.uri()
                        })
                })
                .cloned();
            if let Some(next) = next.as_ref() {
                lock_recover(gapless).pending = Some(next.clone());
            }
            next
        };
        if let Err(error) = self
            .active_pipeline()
            .continue_segment(next.as_ref().map(|next| &next.stream))
        {
            if let Some(next) = next {
                self.cancel_gapless_pending();
                self.report_next_preparation_failure(next.run, error);
            }
            self.handle_end(self.active_slot());
        }
    }

    fn push_logical_position(&self, millis: u64) {
        if let Some(run) = self.timing_run_id() {
            push_event(&self.events, BackendEvent::Position { run, millis });
        }
    }

    pub(super) fn push_duration(&self, millis: u64) {
        if let Some(run) = self.duration_run_id() {
            push_event(&self.events, BackendEvent::Duration { run, millis });
        }
    }

    fn push_seekable(&self) {
        let Some(run) = self.timing_run_id() else {
            return;
        };
        let Some(seekable) = self.active_pipeline().seekable() else {
            return;
        };
        push_event(&self.events, BackendEvent::Seekable { run, seekable });
    }

    fn push_physical_duration(&self, millis: u64) {
        self.push_duration(self.active_pipeline().logical_duration(millis));
    }

    fn emit_ended_once(&mut self, run: RunId) {
        if self.ended_run == Some(run) {
            return;
        }
        self.ended_run = Some(run);
        if self.run_is_current(run) {
            if !self.active_pipeline().has_pending_gapless() {
                if let Some(position) = self.active_pipeline().position() {
                    self.push_logical_position(
                        self.active_pipeline()
                            .logical_position(clock_millis(position)),
                    );
                }
                if let Some(duration) = self.active_pipeline().duration() {
                    self.push_physical_duration(clock_millis(duration));
                }
            }
            self.retire_playback();
        }
        push_event(&self.events, BackendEvent::Ended { run });
    }

    fn timing_run_id(&self) -> Option<RunId> {
        lock_recover(&self.shared)
            .current
            .as_ref()
            .map(|item| item.run)
    }

    fn duration_run_id(&self) -> Option<RunId> {
        let shared = lock_recover(&self.shared);
        if self.pipeline_for_slot(shared.active).has_pending_gapless() {
            return None;
        }
        shared.current.as_ref().map(|item| item.run)
    }

    fn run_is_current(&self, run: RunId) -> bool {
        self.timing_run_id() == Some(run)
    }

    fn maybe_start_crossfade(&mut self) {
        if !self.desired_playing || self.seek_busy() || self.overlap().is_some() {
            return;
        }
        let request = (|| {
            let shared = lock_recover(&self.shared);
            let next = shared.next.clone()?;
            let NextTransition::Crossfade {
                duration_millis: crossfade_ms,
            } = next.transition
            else {
                return None;
            };
            Some((next, crossfade_ms))
        })();

        let Some((next, crossfade_ms)) = request else {
            return;
        };
        let crossfade_start_remaining =
            crossfade_start_remaining_millis(crossfade_ms, self.playback_rate());
        let Some(position) = self.active_pipeline().position() else {
            return;
        };
        let Some(duration) = self.active_pipeline().duration() else {
            return;
        };
        let position_ms = clock_millis(position);
        let duration_ms = clock_millis(duration);
        let logical_position = self.active_pipeline().logical_position(position_ms);
        let logical_duration = self.active_pipeline().logical_duration(duration_ms);
        let remaining = self
            .active_pipeline()
            .logical_remaining(position_ms, duration_ms);
        if logical_duration == 0
            || logical_position >= logical_duration
            || remaining > crossfade_start_remaining
            || logical_duration <= crossfade_start_remaining.saturating_add(1_000)
        {
            return;
        }
        let Some((to, id)) = self
            .incoming()
            .filter(|incoming| incoming.item == next && incoming.phase == IncomingPhase::Ready)
            .map(|incoming| (incoming.slot, incoming.id))
        else {
            return;
        };
        match self.pipeline_for_slot(to).set_state(gst::State::Playing) {
            Ok(result) => {
                self.begin_incoming_handoff(to, id, result);
            }
            Err(error) => self.fail_incoming(to, id, error),
        }
    }

    /// Retires the crossfade tail. The current player keeps its own fade-in curve.
    fn finish_overlap(&mut self) {
        let Some((from, _)) = self.overlap() else {
            return;
        };
        self.transition = Transition::None;
        self.stop_pipeline(from);
        if let Err(error) = self.active_pipeline().set_held(false) {
            self.fail_playback(self.timing_run_id(), error);
            return;
        }
        self.prepare_reserved_incoming();
    }

    fn settings(&self) -> BackendAudioSettings {
        lock_recover(&self.shared).settings.clone()
    }

    fn playback_rate(&self) -> f64 {
        lock_recover(&self.shared).playback_rate
    }

    fn set_playback_rate(&mut self, rate: f64) -> Result<(), BackendFailure> {
        let rate = sanitize_playback_rate(rate);
        if self.active_pipeline().is_bitstream() {
            lock_recover(&self.shared).playback_rate = rate;
            return self.end_passthrough_if_needed();
        }
        // Rebuilding players under a crossfade would lose its curves; the change ends it.
        self.finish_overlap();
        let (active, settings) = {
            let shared = lock_recover(&self.shared);
            (shared.active, shared.settings.clone())
        };
        let handoff_in_progress = self.activating().is_some();
        let reprepare = self
            .incoming()
            .filter(|incoming| incoming.phase == IncomingPhase::Seeking)
            .map(|incoming| incoming.item.clone());
        if reprepare.is_some() {
            self.clear_incoming();
        }

        for slot in [active, inactive_slot(active)] {
            let incoming_phase = self
                .incoming()
                .filter(|incoming| incoming.slot == slot)
                .map(|incoming| incoming.phase);
            let seek_current_position = slot == active
                || handoff_in_progress
                || matches!(
                    incoming_phase,
                    Some(IncomingPhase::Seeking | IncomingPhase::Ready)
                );
            // Before its first preroll the session keeps its queued target for later.
            let seek_position = if slot == active && !self.session_prerolled {
                None
            } else {
                let pending = (slot == active)
                    .then_some(self.seek.queued.or(self.seek.in_flight))
                    .flatten();
                seek_current_position
                    .then(|| pending.or(self.pipeline_for_slot(slot).position().map(clock_millis)))
                    .flatten()
            };
            let time_stream = self
                .pipeline_for_slot(slot)
                .stream()
                .filter(|stream| {
                    stream.time_seekable()
                        && self.pipeline_for_slot(slot).native_seekable() != Some(true)
                })
                .cloned();
            if let Some(mut stream) = time_stream {
                if let Some(position) = seek_position {
                    let logical = self.pipeline_for_slot(slot).logical_position(position);
                    stream.stream = Box::new(
                        stream
                            .reopened_at(logical)
                            .map_err(|error| error.to_string())?,
                    );
                    let incoming = self
                        .incoming()
                        .or(self.activating().map(|handoff| &handoff.incoming))
                        .filter(|incoming| incoming.slot == slot);
                    let fade_in = incoming.and_then(IncomingPipeline::fade_in);
                    let run = self
                        .run_for_slot(slot)
                        .or(incoming.map(|incoming| incoming.item.run))
                        .ok_or("No playback item is active")?;
                    let previous_rate = self.playback_rate();
                    lock_recover(&self.shared).playback_rate = rate;
                    let state = if self.desired_playing && (slot == active || handoff_in_progress) {
                        gst::State::Playing
                    } else {
                        gst::State::Paused
                    };
                    let id = match self.start_pipeline(
                        slot,
                        &PreparedRun { run, stream },
                        &settings,
                        settings.output_gain(),
                        settings.muted,
                        rate,
                        state,
                        fade_in,
                        None,
                    ) {
                        Ok(id) => id,
                        Err(error) => {
                            lock_recover(&self.shared).playback_rate = previous_rate;
                            return Err(error);
                        }
                    };
                    let tap = self.visualizer_tap(slot, self.visualizer_enabled());
                    self.pipeline_for_slot_mut(slot).set_visualizer_tap(tap);
                    match &mut self.transition {
                        Transition::Preparing(incoming) if incoming.slot == slot => {
                            incoming.id = id;
                            incoming.phase = IncomingPhase::Prerolling;
                        }
                        Transition::Activating(handoff) if handoff.incoming.slot == slot => {
                            handoff.incoming.id = id;
                        }
                        _ => {}
                    }
                    if slot == active {
                        self.session_started(0, false, state);
                        self.push_logical_position(logical);
                    }
                    continue;
                } else if slot == active {
                    let target = self
                        .seek
                        .queued
                        .unwrap_or_else(|| self.active_pipeline().physical_seek_target(0));
                    self.seek.queued = Some(target);
                }
            }
            let seek_started = self.pipeline_for_slot_mut(slot).set_playback_rate(
                rate,
                seek_position,
                &settings,
            )?;
            let seeking = self.pipeline_for_slot(slot).seek_in_progress();
            if slot == active {
                lock_recover(&self.shared).playback_rate = rate;
                if seek_started {
                    self.seek = SeekState {
                        in_flight: seek_position.filter(|_| seeking),
                        queued: None,
                    };
                }
            }
            if seek_started
                && let Some(incoming) = self.incoming_mut().filter(|incoming| incoming.slot == slot)
            {
                incoming.phase = if seeking {
                    IncomingPhase::Seeking
                } else {
                    IncomingPhase::Ready
                };
            }
        }
        if let Some(next) = reprepare {
            self.prepare_incoming(&next);
        }
        Ok(())
    }

    fn output_gain_state(&self) -> (f64, bool) {
        let shared = lock_recover(&self.shared);
        (shared.settings.output_gain(), shared.settings.muted)
    }

    fn apply_output_gain_to_pipelines(&mut self, volume: f64, muted: bool) {
        let running: Vec<Slot> = self.running_slots().collect();
        let fade = self.status_fade.as_mut().map(|fade| {
            fade.muted = muted;
            fade.target
        });
        for slot in [Slot::Primary, Slot::Secondary] {
            let pipeline = self.pipeline_for_slot(slot);
            match fade {
                Some(target) if running.contains(&slot) => pipeline.retarget_output(
                    if target == StatusFadeTarget::Pause {
                        0.0
                    } else {
                        volume
                    },
                    muted,
                ),
                _ => pipeline.set_output_volume(volume, muted),
            }
        }
    }

    fn visualizer_enabled(&self) -> bool {
        lock_recover(&self.shared).visualizer_enabled
    }

    fn visualizer_tap(&self, slot: Slot, enabled: bool) -> Option<VisualizerTap> {
        if !enabled {
            return None;
        }
        let (pipeline_id, run, ignore_volume) = (|| {
            let shared = lock_recover(&self.shared);
            if shared.active != slot {
                return None;
            }
            Some((
                shared.pipeline_id(slot)?,
                shared.current.as_ref()?.run,
                shared.settings.visualizer_ignore_volume,
            ))
        })()?;
        Some(self.visualizer.tap(slot, pipeline_id, run, ignore_volume))
    }

    fn sync_processing_taps(&mut self) {
        let output = lock_recover(&self.shared)
            .processing
            .as_ref()
            .filter(|request| request.spectrum)
            .map(|request| request.output.clone());
        for slot in [Slot::Primary, Slot::Secondary] {
            let tap = output
                .as_ref()
                .filter(|output| {
                    self.pipeline_for_slot(slot).processing_output().as_ref() == Some(*output)
                })
                .and_then(|output| {
                    let shared = lock_recover(&self.shared);
                    let pipeline_id = shared.pipeline_id(slot)?;
                    let run = shared.current.as_ref()?.run;
                    Some(
                        self.visualizer
                            .tap(slot, pipeline_id, run, true)
                            .measurement(output.clone()),
                    )
                });
            self.pipeline_for_slot_mut(slot).set_processing_tap(tap);
        }
    }

    fn sync_visualizer_taps(&mut self, enabled: bool) {
        let primary_tap = self.visualizer_tap(Slot::Primary, enabled);
        let secondary_tap = self.visualizer_tap(Slot::Secondary, enabled);
        self.primary.set_visualizer_tap(primary_tap);
        self.secondary.set_visualizer_tap(secondary_tap);
    }

    fn set_visualizer_enabled(&mut self, enabled: bool) -> Result<(), BackendFailure> {
        let changed = {
            let mut shared = lock_recover(&self.shared);
            let changed = shared.visualizer_enabled != enabled;
            shared.visualizer_enabled = enabled;
            changed
        };
        if changed && let Some(run) = self.timing_run_id() {
            push_event(
                &self.events,
                BackendEvent::Visualizer {
                    run,
                    levels: Vec::new(),
                },
            );
        }
        if enabled {
            self.sync_visualizer_taps(true);
        } else if changed {
            self.sync_visualizer_taps(false);
        }
        Ok(())
    }

    fn active_pipeline(&self) -> &PlayerPipeline {
        self.pipeline_for_slot(self.active_slot())
    }

    fn active_pipeline_mut(&mut self) -> &mut PlayerPipeline {
        let slot = self.active_slot();
        self.pipeline_for_slot_mut(slot)
    }

    fn pipeline_for_slot(&self, slot: Slot) -> &PlayerPipeline {
        match slot {
            Slot::Primary => &self.primary,
            Slot::Secondary => &self.secondary,
        }
    }

    fn pipeline_for_slot_mut(&mut self, slot: Slot) -> &mut PlayerPipeline {
        match slot {
            Slot::Primary => &mut self.primary,
            Slot::Secondary => &mut self.secondary,
        }
    }

    fn active_slot(&self) -> Slot {
        lock_recover(&self.shared).active
    }

    fn is_active_slot(&self, slot: Slot) -> bool {
        self.active_slot() == slot
    }

    fn error_is_relevant_slot(&self, slot: Slot) -> bool {
        self.running_slots().any(|running| running == slot)
    }

    fn run_for_slot(&self, slot: Slot) -> Option<RunId> {
        if let Some((from, old_run)) = self.overlap()
            && from == slot
        {
            return Some(old_run);
        }
        let shared = lock_recover(&self.shared);
        (shared.active == slot)
            .then(|| shared.current.as_ref().map(|current| current.run))
            .flatten()
    }

    fn fail_playback(&mut self, run: Option<RunId>, error: BackendFailure) {
        self.retire_playback();
        if let Some(run) = run {
            push_event(&self.events, BackendEvent::Error { run, error });
        }
    }

    fn retire_playback(&mut self) {
        self.desired_playing = false;
        self.seek = SeekState::default();
        self.session_prerolled = false;
        self.awaiting_playing = false;
        self.status_fade = None;
        self.play_command_started_at = None;
        self.transition = Transition::None;
        self.stop_pipeline(Slot::Primary);
        self.stop_pipeline(Slot::Secondary);
        {
            let mut shared = lock_recover(&self.shared);
            shared.current = None;
            shared.next = None;
            shared.active = Slot::Primary;
        }
    }

    fn message_source_is_pipeline(&self, slot: Slot, message: &gst::Message) -> bool {
        self.pipeline_for_slot(slot)
            .message_source_is_pipeline(message)
    }

    fn shutdown(&mut self) {
        self.transition = Transition::None;
        self.stop_pipeline(Slot::Primary);
        self.stop_pipeline(Slot::Secondary);
    }
}

#[cfg(target_os = "linux")]
fn audio_output_change_requires_restart() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

#[cfg(not(target_os = "linux"))]
fn audio_output_change_requires_restart() -> bool {
    false
}
#[instrument(skip(events))]
fn run_gstreamer_thread(
    events: Arc<Mutex<EventMailbox>>,
    ready: SyncSender<Result<Sender<WorkerInput>, String>>,
    processing: ProcessingRequest,
) {
    let startup_started_at = Instant::now();
    if let Err(error) = ensure_gstreamer_initialized() {
        let _ = ready.send(Err(format!("GStreamer init failed: {error}")));
        return;
    }

    // These clock corrections and device underruns are debug-log warnings, not
    // bus messages. Queue them so diagnostic file/terminal I/O cannot block audio.
    let (audio_warning_sender, audio_warnings) = sync_channel(32);
    let mut engine = GstEngine::new(Arc::clone(&events));
    let warning_wake = engine.input_sender.clone();
    let audio_log = gst::log::add_log_function(move |category, level, _, _, _, object, message| {
        if level <= gst::DebugLevel::Warning
            && matches!(
                category.name(),
                "audiobasesink" | "pulse" | "opensles_ringbuffer"
            )
            && let Some(message) = message.get()
        {
            if audio_warning_sender
                .try_send(AudioWarning {
                    category: category.name().to_string(),
                    element: object.map(|object| object.to_string()),
                    message: message.to_string(),
                    captured_at: Instant::now(),
                })
                .is_ok()
            {
                let _ = warning_wake.send(WorkerInput::Wake);
            }
        }
    });
    for category in ["audiobasesink", "pulse", "opensles_ringbuffer"] {
        gst::log::set_threshold_for_name(category, gst::DebugLevel::Warning);
    }

    engine.set_processing(processing);
    if ready.send(Ok(engine.input_sender.clone())).is_err() {
        gst::log::remove_log_function(audio_log);
        return;
    }
    info!(
        elapsed_ms = startup_started_at.elapsed().as_millis(),
        "GStreamer playback backend is ready"
    );

    loop {
        let first = match engine.next_deadline() {
            Some(wait) => engine.inputs.recv_timeout(wait).ok(),
            None => engine.inputs.recv().ok(),
        };
        let inputs = first.into_iter().chain(engine.inputs.try_iter()).collect();
        if !engine.handle_inputs(inputs) {
            break;
        }
        for warning in audio_warnings.try_iter() {
            engine.log_audio_warning(warning);
        }
        engine.tick();
    }
    engine.shutdown();
    gst::log::remove_log_function(audio_log);
}
pub(super) fn handle_about_to_finish(
    pipeline: &gst::Element,
    shared: &Arc<Mutex<SharedBackendState>>,
    gapless: &Arc<Mutex<GaplessPlayback>>,
    queued_loudness: &SharedQueuedLoudness,
    trust_invalid_certificate: &AtomicBool,
    slot: Slot,
    id: PipelineId,
) {
    if !about_to_finish_may_query(&lock_recover(shared), slot, id) {
        return;
    }
    let has_position = pipeline
        .query_position::<gst::ClockTime>()
        .is_some_and(|position| position > gst::ClockTime::ZERO);
    let can_preload = has_position || {
        let (_, current, pending) = pipeline.state(gst::ClockTime::ZERO);
        current == gst::State::Playing || pending == gst::State::Playing
    };
    let action = about_to_finish_action_for_pipeline(
        &mut lock_recover(shared),
        &mut lock_recover(gapless),
        slot,
        id,
        can_preload,
    );

    match action {
        AboutToFinishAction::Preload(next) => {
            info!(
                next_run = %next.run,
                uri = %next.stream.redacted_uri(),
                "preloading gapless next stream"
            );
            *queued_loudness
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = next.stream.loudness.clone();
            trust_invalid_certificate
                .store(next.stream.trust_invalid_certificate(), Ordering::SeqCst);
            pipeline.set_property("uri", next.stream.uri());
        }
        AboutToFinishAction::Ignore => {}
    }
}

fn about_to_finish_may_query(shared: &SharedBackendState, slot: Slot, id: PipelineId) -> bool {
    shared.pipeline_is_current(slot, id)
        && shared
            .current
            .as_ref()
            .is_some_and(|current| current.stream.allows_preloading)
}

fn about_to_finish_action_for_pipeline(
    shared: &mut SharedBackendState,
    gapless: &mut GaplessPlayback,
    slot: Slot,
    id: PipelineId,
    can_preload: bool,
) -> AboutToFinishAction {
    if !shared.pipeline_is_current(slot, id) {
        return AboutToFinishAction::Ignore;
    }
    if !can_preload {
        gapless.about_to_finish_pending = true;
        return AboutToFinishAction::Ignore;
    }
    about_to_finish_action(shared, gapless)
}

pub(super) fn about_to_finish_action(
    shared: &mut SharedBackendState,
    gapless: &mut GaplessPlayback,
) -> AboutToFinishAction {
    if gapless.pending.is_some() {
        // The next source can finish reading before its stream-start arrives.
        gapless.about_to_finish_pending = true;
        return AboutToFinishAction::Ignore;
    }

    let Some(next) = shared.next.as_ref() else {
        gapless.about_to_finish_pending = true;
        gapless.next_needed = shared.current.as_ref().map(|current| current.run);
        return AboutToFinishAction::Ignore;
    };

    if !gapless_preload_should_run(shared, next, gapless.bitstream) {
        gapless.about_to_finish_pending = false;
        return AboutToFinishAction::Ignore;
    }

    if next.stream.end_millis().is_some() || !gapless_preload_source_is_supported(next.stream.uri())
    {
        debug!(
            next_run = %next.run,
            uri = %next.stream.redacted_uri(),
            "skipping gapless preload for non-local stream"
        );
        gapless.about_to_finish_pending = false;
        return AboutToFinishAction::Ignore;
    }

    gapless.pending = Some(next.clone());
    gapless.about_to_finish_pending = false;
    AboutToFinishAction::Preload(Box::new(next.clone()))
}

fn gapless_preload_should_run(
    shared: &SharedBackendState,
    next: &PreparedNext,
    bitstream: bool,
) -> bool {
    let Some(current) = shared.current.as_ref() else {
        return false;
    };
    (next.transition == NextTransition::Gapless || bitstream)
        && next.stream.allows_preloading
        && current.stream.allows_preloading
        && current.stream.window().is_none()
        && !gapless_uses_separate_pipeline(current, next)
}

pub(super) fn gapless_preload_source_is_supported(uri: &str) -> bool {
    uri.starts_with("file://") || uri.starts_with("http://") || uri.starts_with("https://")
}
fn inactive_slot(slot: Slot) -> Slot {
    match slot {
        Slot::Primary => Slot::Secondary,
        Slot::Secondary => Slot::Primary,
    }
}

fn gapless_uses_separate_pipeline(current: &PreparedRun, next: &PreparedNext) -> bool {
    (current.stream.window().is_some() || next.stream.window().is_some())
        && (current.stream.window().is_none() || current.stream.uri() != next.stream.uri())
}

fn crossfade_start_remaining_millis(crossfade_millis: u64, playback_rate: f64) -> u64 {
    ((crossfade_millis as f64) * sanitize_playback_rate(playback_rate))
        .round()
        .clamp(0.0, u64::MAX as f64) as u64
}

pub(super) fn push_event(events: &Arc<Mutex<EventMailbox>>, event: BackendEvent) {
    let waker = lock_recover(events).push(event);
    if let Some(waker) = waker {
        waker();
    }
}
fn clock_millis(clock_time: gst::ClockTime) -> u64 {
    clock_time.mseconds()
}
fn stream_uri_scheme(uri: &str) -> &str {
    uri.split_once(':')
        .map(|(scheme, _)| scheme)
        .unwrap_or("unknown")
}
#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;

    use library::LoudnessMeasurement;

    use super::*;

    const ACTIVE_PIPELINE: PipelineId = PipelineId(7);

    #[test]
    fn crossfade_trigger_tracks_playback_rate() {
        assert_eq!(crossfade_start_remaining_millis(5_000, 0.5), 2_500);
        assert_eq!(crossfade_start_remaining_millis(5_000, 1.0), 5_000);
        assert_eq!(crossfade_start_remaining_millis(5_000, 1.5), 7_500);
        assert_eq!(crossfade_start_remaining_millis(5_000, 2.0), 10_000);
    }

    const INCOMING_PIPELINE: PipelineId = PipelineId(8);

    struct HandoffFixture {
        engine: GstEngine,
        events: Arc<Mutex<EventMailbox>>,
        old_run: RunId,
        next_run: RunId,
    }

    impl HandoffFixture {
        fn separate(transition: NextTransition) -> Self {
            let events = Arc::new(Mutex::new(EventMailbox::default()));
            let mut engine = GstEngine::new(Arc::clone(&events));
            let old_run = RunId::new(1);
            let next_run = RunId::new(2);
            let (current_stream, next_stream) = if transition == NextTransition::Gapless {
                (
                    ResolvedStream::new("file:///music/current.flac").with_window(0, 30_000),
                    ResolvedStream::new("file:///music/next.flac").with_window(30_000, 60_000),
                )
            } else {
                (
                    ResolvedStream::new("file:///music/current.flac"),
                    ResolvedStream::new("file:///music/next.flac"),
                )
            };
            let next = PreparedNext::new(next_run, next_stream, transition);
            {
                let mut shared = lock_recover(&engine.shared);
                shared.current = Some(PreparedRun {
                    run: old_run,
                    stream: current_stream.into(),
                });
                shared.next = Some(next.clone());
                shared.active = Slot::Primary;
                shared.set_pipeline_id(Slot::Primary, Some(ACTIVE_PIPELINE));
                shared.set_pipeline_id(Slot::Secondary, Some(INCOMING_PIPELINE));
            }
            engine.transition = Transition::Preparing(IncomingPipeline {
                id: INCOMING_PIPELINE,
                slot: Slot::Secondary,
                item: next.clone(),
                phase: IncomingPhase::Ready,
            });
            engine.desired_playing = true;
            Self {
                engine,
                events,
                old_run,
                next_run,
            }
        }

        fn begin_separate(&mut self) {
            assert!(self.engine.begin_incoming_handoff(
                Slot::Secondary,
                INCOMING_PIPELINE,
                gst::StateChangeSuccess::Async,
            ));
        }

        fn current_run(&self) -> Option<RunId> {
            lock_recover(&self.engine.shared)
                .current
                .as_ref()
                .map(|item| item.run)
        }

        fn drain(&self) -> Vec<BackendEvent> {
            lock_recover(&self.events).drain()
        }
    }

    #[test]
    fn gapless_normalization_can_share_the_audio_graph() {
        let direct_loudness = TrackLoudness {
            track: Some(Box::new(LoudnessMeasurement {
                analysis_key: [1; 32],
                integrated_lufs: None,
                true_peak: None,
                replay_gain_db: Some(-4.0),
                replay_gain_peak: Some(0.8),
            })),
            album: None,
        };
        let current = PreparedRun {
            run: RunId::new(1),
            stream: PreparedStream::new(
                ResolvedStream::new("file:///music/current.flac"),
                Some(direct_loudness.clone()),
            ),
        };
        let direct_next = PreparedNext::new(
            RunId::new(2),
            PreparedStream::new(
                ResolvedStream::new("file:///music/direct-next.flac"),
                Some(direct_loudness),
            ),
            NextTransition::Gapless,
        );
        let native_next = PreparedNext::new(
            RunId::new(3),
            PreparedStream::from(ResolvedStream::new("file:///music/native-next.flac")),
            NextTransition::Gapless,
        );

        assert!(!gapless_uses_separate_pipeline(&current, &direct_next));
        assert!(!gapless_uses_separate_pipeline(&current, &native_next));
    }

    #[test]
    fn repeated_stream_is_preloaded_on_the_current_pipeline() {
        let mut gapless = GaplessPlayback::default();
        let pipeline = PipelineId(5);
        let current_run = RunId::new(10);
        let repeated = PreparedStream::from(ResolvedStream::new("file:///music/repeated.flac"));
        let next = PreparedNext::new(RunId::new(11), repeated.clone(), NextTransition::Gapless);
        let mut shared = SharedBackendState::new();
        shared.active = Slot::Primary;
        shared.current = Some(PreparedRun {
            run: current_run,
            stream: repeated,
        });
        shared.next = Some(next.clone());
        shared.set_pipeline_id(Slot::Primary, Some(pipeline));

        assert!(!gapless_uses_separate_pipeline(
            shared.current.as_ref().expect("current stream"),
            &next,
        ));
        let distinct = PreparedNext::new(
            RunId::new(12),
            ResolvedStream::new("file:///music/distinct.flac"),
            NextTransition::Gapless,
        );
        assert!(!gapless_uses_separate_pipeline(
            shared.current.as_ref().expect("current stream"),
            &distinct,
        ));
        assert_eq!(
            about_to_finish_action_for_pipeline(
                &mut shared,
                &mut gapless,
                Slot::Primary,
                pipeline,
                true
            ),
            AboutToFinishAction::Preload(Box::new(next.clone()))
        );
        assert_eq!(shared.next, Some(next.clone()));
        assert_eq!(gapless.pending, Some(next));
    }

    #[test]
    fn fresh_playback_outputs_the_selected_gain_and_mute() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let directory = tempfile::tempdir().expect("playback fixture directory");
        let path = directory.path().join("initial-volume.wav");
        write_silent_wave(&path);
        let mut wav = std::fs::read(&path).unwrap();
        for bytes in wav[44..].chunks_exact_mut(2) {
            bytes.copy_from_slice(&8192_i16.to_le_bytes());
        }
        std::fs::write(&path, wav).unwrap();
        let uri = gst::glib::filename_to_uri(&path, None).expect("playback fixture URI");

        for (volume_scale, muted, expected_gain) in [
            (VolumeScale::Perceptual, false, 0.056_234_132_519_034_91),
            (VolumeScale::Linear, false, 0.5),
            (VolumeScale::Perceptual, true, 0.056_234_132_519_034_91),
        ] {
            let events = Arc::new(Mutex::new(EventMailbox::default()));
            let mut engine = GstEngine::new(events);
            let settings = BackendAudioSettings {
                volume: 0.5,
                volume_scale,
                muted,
                audio_output: Some("appsink".to_string()),
                ..BackendAudioSettings::default()
            };
            lock_recover(&engine.shared).settings = settings;

            engine
                .play_prepared(
                    PreparedRun {
                        run: RunId::new(1),
                        stream: ResolvedStream::new(uri.as_str()).into(),
                    },
                    None,
                    0,
                )
                .expect("start fresh playback");

            let sink = engine
                .primary
                .audio_output()
                .unwrap()
                .downcast::<gstreamer_app::AppSink>()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let sample = loop {
                engine.poll_bus();
                engine.tick();
                if let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                    break sample;
                }
                assert!(Instant::now() < deadline, "no audio output after startup");
                std::thread::sleep(Duration::from_millis(1));
            };
            let map = sample.buffer().unwrap().map_readable().unwrap();
            let expected = if muted { 0.0 } else { 0.25 * expected_gain };
            assert!(!map.is_empty());
            for bytes in map.as_slice().chunks_exact(4) {
                let actual = f32::from_le_bytes(bytes.try_into().unwrap());
                assert!(
                    (f64::from(actual) - expected).abs() < 0.0001,
                    "first audio sample {actual}, expected {expected}"
                );
            }
            engine.shutdown();
        }
    }

    #[test]
    fn startup_seek_waits_for_preroll_before_playing() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let directory = tempfile::tempdir().expect("playback fixture directory");
        let path = directory.path().join("startup-seek.wav");
        write_long_silent_wave(&path);
        let uri = gst::glib::filename_to_uri(&path, None).expect("playback fixture URI");
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        lock_recover(&engine.shared).settings = BackendAudioSettings {
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };

        engine
            .play_prepared(
                PreparedRun {
                    run: RunId::new(1),
                    stream: ResolvedStream::new(uri.as_str()).into(),
                },
                None,
                5_000,
            )
            .expect("start playback with a saved position");
        assert!(engine.primary.has_or_targets_state(gst::State::Paused));
        assert!(lock_recover(&events).drain().iter().any(|event| matches!(
            event,
            BackendEvent::State {
                state: BackendState::Buffering,
                ..
            }
        )));
        assert_eq!(engine.seek.queued, Some(5_000));
        engine
            .primary
            .audio_output()
            .unwrap()
            .set_property("sync", true);
        engine.push_position(5_000);
        assert!(!engine.primary.is_playing());
        let deadline = Instant::now() + Duration::from_secs(3);
        while !engine.primary.is_playing() && Instant::now() < deadline {
            engine.poll_bus();
            engine.tick();
            thread::sleep(Duration::from_millis(10));
        }
        engine.poll_bus();
        assert!(engine.primary.is_playing());
        assert!(lock_recover(&events).drain().iter().any(|event| matches!(
            event,
            BackendEvent::State {
                state: BackendState::Playing,
                ..
            }
        )));
        assert!(engine.primary.position().unwrap().mseconds() >= 5_000);
        engine.shutdown();
    }

    #[test]
    fn visualizer_enablement_resets_the_current_run() {
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        let run = RunId::new(1);
        {
            let mut shared = lock_recover(&engine.shared);
            shared.current = Some(PreparedRun {
                run,
                stream: ResolvedStream::new("file:///music/current.flac").into(),
            });
            shared.set_pipeline_id(Slot::Primary, Some(PipelineId(7)));
        }

        engine
            .set_visualizer_enabled(true)
            .expect("enable visualizer");

        assert!(lock_recover(&engine.shared).visualizer_enabled);
        assert!(matches!(
            lock_recover(&events).drain().as_slice(),
            [BackendEvent::Visualizer {
                run: emitted_run,
                levels,
            }] if *emitted_run == run && levels.is_empty()
        ));
    }

    #[test]
    fn seeks_wait_for_the_running_seek_and_keep_only_the_latest_target() {
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(events);
        lock_recover(&engine.shared).current = Some(PreparedRun {
            run: RunId::new(1),
            stream: ResolvedStream::new("file:///music/current.flac").into(),
        });
        engine.desired_playing = true;
        engine.session_prerolled = true;
        engine.seek.in_flight = Some(1_000);

        engine.start_seek(2_000).unwrap();
        engine.start_seek(3_000).unwrap();
        assert_eq!(engine.seek.queued, Some(3_000));
        // Repeating the running target, as repeated lyric taps do, drops the queued one.
        engine.start_seek(1_000).unwrap();
        assert_eq!(engine.seek.queued, None);
        assert_eq!(engine.seek.in_flight, Some(1_000));
    }

    #[test]
    fn device_change_restores_playback_from_a_physically_paused_output() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let directory = tempfile::tempdir().expect("playback fixture directory");
        let path = directory.path().join("audio-device-change.wav");
        write_long_silent_wave(&path);
        let uri = gst::glib::filename_to_uri(&path, None).expect("playback fixture URI");
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        let current = PreparedRun {
            run: RunId::new(1),
            stream: ResolvedStream::new(uri.as_str()).into(),
        };
        let settings = BackendAudioSettings {
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        let pipeline_id = engine
            .start_pipeline(
                Slot::Primary,
                &current,
                &settings,
                settings.volume,
                settings.muted,
                DEFAULT_PLAYBACK_RATE,
                gst::State::Paused,
                None,
                None,
            )
            .expect("start paused output");
        {
            let mut shared = lock_recover(&engine.shared);
            shared.settings = settings.clone();
            shared.current = Some(current);
            shared.set_pipeline_id(Slot::Primary, Some(pipeline_id));
        }
        engine.desired_playing = true;

        let mut changed = settings;
        changed.audio_output = Some("appsink".to_string());
        engine.handle_command(BackendCommand::ConfigureAudio(changed));

        let applied = lock_recover(&events).drain();
        assert!(engine.desired_playing);
        assert_eq!(
            engine.primary.audio_output_factory().as_deref(),
            Some("appsink")
        );
        engine.handle_state_changed(BackendState::Playing);

        let resumed_events = lock_recover(&events).drain();
        assert!(
            resumed_events.iter().any(|event| matches!(
                event,
                BackendEvent::State {
                    run,
                    state: BackendState::Playing,
                } if *run == RunId::new(1)
            )),
            "audio configuration events: {applied:?}; resume events: {resumed_events:?}"
        );
        engine.shutdown();
    }

    #[test]
    fn unavailable_device_selection_keeps_the_working_output() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let directory = tempfile::tempdir().expect("playback fixture directory");
        let path = directory.path().join("unavailable-audio-device.wav");
        write_long_silent_wave(&path);
        let uri = gst::glib::filename_to_uri(&path, None).expect("playback fixture URI");
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        let current = PreparedRun {
            run: RunId::new(1),
            stream: ResolvedStream::new(uri.as_str()).into(),
        };
        let settings = BackendAudioSettings {
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        let pipeline_id = engine
            .start_pipeline(
                Slot::Primary,
                &current,
                &settings,
                settings.volume,
                settings.muted,
                DEFAULT_PLAYBACK_RATE,
                gst::State::Paused,
                None,
                None,
            )
            .expect("start paused output");
        {
            let mut shared = lock_recover(&engine.shared);
            shared.settings = settings.clone();
            shared.current = Some(current);
            shared.set_pipeline_id(Slot::Primary, Some(pipeline_id));
        }
        engine.desired_playing = true;

        let mut changed = settings;
        changed.audio_output = Some("gst-device:unavailable".to_string());
        engine.handle_command(BackendCommand::ConfigureAudio(changed));

        assert!(engine.desired_playing);
        assert_eq!(
            lock_recover(&engine.shared)
                .settings
                .audio_output
                .as_deref(),
            Some("fakesink")
        );
        assert_eq!(
            engine.primary.audio_output_factory().as_deref(),
            Some("fakesink")
        );
        let applied = lock_recover(&events).drain();
        assert!(applied.iter().any(|event| matches!(
            event,
            BackendEvent::AudioApplied {
                output: Some(output),
                ..
            } if output == "fakesink"
        )));
        assert!(
            applied
                .iter()
                .all(|event| !matches!(event, BackendEvent::Error { .. }))
        );
        engine.shutdown();
    }

    #[test]
    fn preserve_pitch_change_at_normal_speed_preserves_the_current_pipeline() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let directory = tempfile::tempdir().expect("playback fixture directory");
        let path = directory.path().join("preserve-pitch-change.wav");
        write_long_silent_wave(&path);
        let uri = gst::glib::filename_to_uri(&path, None).expect("playback fixture URI");
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(events);
        let current = PreparedRun {
            run: RunId::new(1),
            stream: ResolvedStream::new(uri.as_str()).into(),
        };
        let settings = BackendAudioSettings {
            audio_output: Some("fakesink".to_string()),
            preserve_pitch: true,
            ..BackendAudioSettings::default()
        };
        let pipeline_id = engine
            .start_pipeline(
                Slot::Primary,
                &current,
                &settings,
                settings.volume,
                settings.muted,
                DEFAULT_PLAYBACK_RATE,
                gst::State::Paused,
                None,
                None,
            )
            .expect("start paused output");
        {
            let mut shared = lock_recover(&engine.shared);
            shared.settings = settings.clone();
            shared.current = Some(current);
            shared.set_pipeline_id(Slot::Primary, Some(pipeline_id));
        }

        let mut changed = settings;
        changed.preserve_pitch = false;
        engine.handle_command(BackendCommand::ConfigureAudio(changed));

        let shared = lock_recover(&engine.shared);
        assert!(!shared.settings.preserve_pitch);
        assert_eq!(shared.pipeline_id(Slot::Primary), Some(pipeline_id));
        drop(shared);
        engine.shutdown();
    }

    #[test]
    fn unconfirmed_gapless_activation_keeps_the_old_run_until_playing() {
        for result in [
            gst::StateChangeSuccess::Async,
            gst::StateChangeSuccess::NoPreroll,
        ] {
            let mut fixture = HandoffFixture::separate(NextTransition::Gapless);
            assert!(fixture.engine.begin_incoming_handoff(
                Slot::Secondary,
                INCOMING_PIPELINE,
                result,
            ));

            assert_eq!(fixture.current_run(), Some(fixture.old_run));
            assert!(fixture.engine.incoming().is_none());
            assert!(matches!(
                fixture.engine.activating(),
                Some(PendingHandoff {
                    incoming,
                    from: Slot::Primary,
                    old_run,
                }) if incoming.id == INCOMING_PIPELINE
                    && incoming.item.run == fixture.next_run
                    && *old_run == fixture.old_run
            ));
            assert!(fixture.drain().is_empty());
        }
    }

    #[test]
    fn matching_playing_confirmation_commits_gapless_handoff_once() {
        let mut fixture = HandoffFixture::separate(NextTransition::Gapless);
        fixture.begin_separate();

        assert!(
            fixture
                .engine
                .confirm_handoff(Slot::Secondary, INCOMING_PIPELINE)
        );
        assert!(
            !fixture
                .engine
                .confirm_handoff(Slot::Secondary, INCOMING_PIPELINE)
        );

        let shared = lock_recover(&fixture.engine.shared);
        assert_eq!(
            shared.current.as_ref().map(|item| item.run),
            Some(fixture.next_run)
        );
        assert_eq!(shared.active, Slot::Secondary);
        drop(shared);
        assert!(matches!(fixture.engine.transition, Transition::None));
        assert_eq!(
            fixture
                .drain()
                .into_iter()
                .filter(|event| matches!(event, BackendEvent::Transitioned { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn crossfade_overlap_starts_only_after_the_incoming_pipeline_is_playing() {
        let mut fixture = HandoffFixture::separate(NextTransition::Crossfade {
            duration_millis: 5_000,
        });
        fixture.begin_separate();
        assert!(fixture.engine.overlap().is_none());
        assert!(fixture.drain().is_empty());

        assert!(
            fixture
                .engine
                .confirm_handoff(Slot::Secondary, INCOMING_PIPELINE)
        );

        assert_eq!(
            fixture.engine.overlap(),
            Some((Slot::Primary, fixture.old_run))
        );
        let shared = lock_recover(&fixture.engine.shared);
        assert_eq!(shared.active, Slot::Secondary);
        assert_eq!(
            shared.current.as_ref().map(|item| item.run),
            Some(fixture.next_run)
        );
        drop(shared);
        assert!(fixture.drain().iter().any(|event| matches!(
            event,
            BackendEvent::Transitioned {
                old_run: transitioned_old,
                new_run: transitioned_new,
            } if *transitioned_old == fixture.old_run
                && *transitioned_new == fixture.next_run
        )));
    }

    #[test]
    fn failed_asynchronous_gapless_activation_falls_back_after_the_old_run_ends() {
        let mut fixture = HandoffFixture::separate(NextTransition::Gapless);
        fixture.begin_separate();

        fixture.engine.fail_handoff(
            Slot::Secondary,
            INCOMING_PIPELINE,
            "incoming activation failed".into(),
        );

        let shared = lock_recover(&fixture.engine.shared);
        assert!(shared.current.is_none());
        assert!(shared.next.is_none());
        drop(shared);
        assert!(matches!(
            fixture.drain().as_slice(),
            [
                BackendEvent::NextPreparationFailed {
                    current_run,
                    next_run: failed_next,
                    ..
                },
                BackendEvent::Ended { run: ended_run }
            ] if *current_run == fixture.old_run
                && *failed_next == fixture.next_run
                && *ended_run == fixture.old_run
        ));
    }

    #[test]
    fn volume_scale_change_preserves_an_unconfirmed_handoff() {
        let mut fixture = HandoffFixture::separate(NextTransition::Crossfade {
            duration_millis: 5_000,
        });
        fixture.begin_separate();

        fixture
            .engine
            .handle_command(BackendCommand::SetOutputVolume {
                volume: 0.5,
                volume_scale: VolumeScale::Perceptual,
                muted: false,
            });

        assert!(fixture.engine.activating().is_some());
        let (gain, muted) = fixture.engine.output_gain_state();
        assert!((gain - VolumeScale::Perceptual.gain(0.5)).abs() < f64::EPSILON);
        assert!(!muted);
        assert!(
            !fixture
                .drain()
                .iter()
                .any(|event| matches!(event, BackendEvent::Ended { .. }))
        );
    }

    #[test]
    fn pause_cancels_an_unconfirmed_crossfade_and_late_playing_cannot_commit_it() {
        let mut fixture = HandoffFixture::separate(NextTransition::Crossfade {
            duration_millis: 5_000,
        });
        fixture.begin_separate();

        fixture.engine.handle_command(BackendCommand::Pause {
            run: fixture.old_run,
        });

        assert!(matches!(fixture.engine.transition, Transition::None));
        assert!(
            !fixture
                .engine
                .confirm_handoff(Slot::Secondary, INCOMING_PIPELINE)
        );
        assert_eq!(fixture.current_run(), Some(fixture.old_run));
        assert!(fixture.drain().iter().all(|event| !matches!(
            event,
            BackendEvent::Transitioned { .. } | BackendEvent::Ended { .. }
        )));
    }

    #[test]
    fn late_backend_states_after_pause_cannot_authorize_a_transition() {
        for late_state in [BackendState::Buffering, BackendState::Playing] {
            let mut crossfade = HandoffFixture::separate(NextTransition::Crossfade {
                duration_millis: 5_000,
            });
            crossfade.engine.handle_command(BackendCommand::Pause {
                run: crossfade.old_run,
            });
            crossfade.engine.handle_state_changed(late_state);
            crossfade.engine.maybe_start_crossfade();

            assert!(crossfade.engine.incoming().is_some());

            let mut gapless = HandoffFixture::separate(NextTransition::Gapless);
            gapless.engine.handle_command(BackendCommand::Pause {
                run: gapless.old_run,
            });
            gapless.engine.handle_state_changed(late_state);
            gapless.engine.handle_end(Slot::Primary);

            assert_eq!(gapless.current_run(), None);
            assert!(gapless.engine.activating().is_none());
            assert!(gapless.drain().iter().all(|event| !matches!(
                event,
                BackendEvent::Transitioned { .. } | BackendEvent::NextPreparationFailed { .. }
            )));
        }
    }

    #[test]
    fn pause_intent_survives_handoff_audio_reconfiguration_and_late_playing_state() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let directory = tempfile::tempdir().expect("playback fixture directory");
        let path = directory.path().join("pause-fade.wav");
        write_silent_wave(&path);
        let uri = gst::glib::filename_to_uri(&path, None).expect("playback fixture URI");
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        let old_run = RunId::new(1);
        let new_run = RunId::new(2);
        let current = PreparedRun {
            run: old_run,
            stream: ResolvedStream::new(uri.as_str()).into(),
        };
        let next = PreparedNext::new(
            new_run,
            ResolvedStream::new(uri.as_str()),
            NextTransition::Gapless,
        );
        let mut settings = BackendAudioSettings::default();
        settings.audio_output = Some("fakesink".to_string());
        let pipeline_id = engine
            .start_pipeline(
                Slot::Primary,
                &current,
                &settings,
                settings.volume,
                settings.muted,
                DEFAULT_PLAYBACK_RATE,
                gst::State::Paused,
                None,
                None,
            )
            .expect("start inert GStreamer session");
        {
            let mut shared = lock_recover(&engine.shared);
            shared.settings = settings.clone();
            shared.current = Some(current);
            lock_recover(engine.primary.gapless().unwrap()).pending = Some(next);
            shared.active = Slot::Primary;
            shared.set_pipeline_id(Slot::Primary, Some(pipeline_id));
        }
        engine.desired_playing = true;

        engine.handle_command(BackendCommand::Pause { run: old_run });
        let original_fade = engine.status_fade.expect("pause fade");
        engine.handle_stream_start();
        assert_eq!(engine.timing_run_id(), Some(new_run));

        engine.handle_command(BackendCommand::Pause { run: new_run });

        let preserved_fade = engine.status_fade.expect("preserved pause fade");
        assert_eq!(preserved_fade.slot, original_fade.slot);
        assert_eq!(preserved_fade.target, StatusFadeTarget::Pause);

        lock_recover(&events).drain();
        let mut unavailable_settings = settings.clone();
        unavailable_settings.audio_output = Some("rufin-test-missing-output".to_string());
        engine.handle_command(BackendCommand::ConfigureAudio(unavailable_settings));
        assert!(!engine.desired_playing);
        assert!(engine.status_fade.is_none());
        let failed_events = lock_recover(&events).drain();
        let paused = failed_events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    BackendEvent::State {
                        run,
                        state: BackendState::Paused,
                    } if *run == new_run
                )
            })
            .expect("paused acknowledgement before reconfiguration");
        let error = failed_events
            .iter()
            .position(|event| matches!(event, BackendEvent::OperationFailed { run: Some(run), .. } if *run == new_run))
            .expect("audio reconfiguration error");
        assert!(paused < error);

        let mut configured_settings = settings;
        configured_settings.fade_on_status_change = false;
        engine.handle_command(BackendCommand::ConfigureAudio(configured_settings));
        assert!(!engine.desired_playing);
        assert!(engine.status_fade.is_none());
        assert!(lock_recover(&events).drain().iter().any(|event| matches!(
            event,
            BackendEvent::State {
                run,
                state: BackendState::Paused,
            } if *run == new_run
        )));

        engine.handle_command(BackendCommand::Play { run: new_run });
        assert!(engine.desired_playing);
        assert!(engine.status_fade.is_none());
        engine.handle_command(BackendCommand::Pause { run: new_run });
        assert!(!engine.desired_playing);

        engine.handle_state_changed(BackendState::Playing);
        engine.start_seek(0).expect("seek while paused");
        assert!(!engine.session_prerolled && !engine.awaiting_playing);
        assert!(!engine.desired_playing);
        engine.shutdown();
    }

    fn write_silent_wave(path: &std::path::Path) {
        write_silent_mono_wave(path, 800);
    }

    #[test]
    fn repeats_and_cue_windows_preserve_samples_on_one_pipeline() {
        ensure_gstreamer_initialized().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("segments.wav");
        write_silent_mono_wave(&path, 16_000);
        let source = (0..16_000)
            .map(|i| ((i % 1000) as i16 - 500) * 16)
            .collect::<Vec<_>>();
        let mut wav = std::fs::read(&path).unwrap();
        for (sample, bytes) in source.iter().zip(wav[44..].chunks_exact_mut(2)) {
            bytes.copy_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(&path, wav).unwrap();
        let uri = gst::glib::filename_to_uri(&path, None).unwrap();
        for (first, second) in [
            (None, None),
            (Some((250, 750)), Some((250, 750))),
            (Some((250, 750)), Some((750, 1250))),
        ] {
            let stream = |window: Option<(u64, u64)>| {
                let stream = ResolvedStream::new(uri.as_str());
                window.map_or(stream.clone(), |(start, end)| {
                    stream.with_window(start, end)
                })
            };
            let events = Arc::new(Mutex::new(EventMailbox::default()));
            let mut engine = GstEngine::new(Arc::clone(&events));
            engine.handle_command(BackendCommand::ConfigureAudio(BackendAudioSettings {
                audio_output: Some("appsink".into()),
                fade_on_status_change: false,
                ..BackendAudioSettings::default()
            }));
            engine.handle_command(BackendCommand::Start {
                run: RunId::new(1),
                current: stream(first).into(),
                next: Some(PreparedNext::new(
                    RunId::new(2),
                    stream(second),
                    NextTransition::Gapless,
                )),
                start_position_millis: 0,
                playback_rate: 1.0,
            });
            let sink = engine
                .active_pipeline()
                .audio_output()
                .unwrap()
                .downcast::<gstreamer_app::AppSink>()
                .unwrap();
            let mut samples = Vec::new();
            let mut transitions = 0;
            let mut ended = false;
            let deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < deadline && !ended {
                engine.poll_bus();
                engine.tick();
                while let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                    let map = sample.buffer().unwrap().map_readable().unwrap();
                    samples.extend(
                        map.as_slice()
                            .chunks_exact(4)
                            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())),
                    );
                }
                for event in lock_recover(&events).drain() {
                    match event {
                        BackendEvent::Transitioned { old_run, new_run } => {
                            assert_eq!((old_run, new_run), (RunId::new(1), RunId::new(2)));
                            transitions += 1;
                            assert!(!engine.secondary.has_session());
                        }
                        BackendEvent::Ended { run } => {
                            assert_eq!(run, RunId::new(2));
                            ended = true;
                        }
                        BackendEvent::Error { error, .. } => panic!("{error:?}"),
                        _ => {}
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            engine.shutdown();
            assert!(ended, "no EOS for {first:?} -> {second:?}");
            assert_eq!(transitions, 1);
            let expected = [first, second]
                .into_iter()
                .flat_map(|window| {
                    let (start, end) = window.unwrap_or((0, 2000));
                    source[start as usize * 8..end as usize * 8]
                        .iter()
                        .map(|sample| f32::from(*sample) / 32768.0)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                samples.len(),
                expected.len(),
                "sample count for {first:?} -> {second:?}"
            );
            assert!(
                samples
                    .iter()
                    .zip(&expected)
                    .all(|(actual, expected)| (actual - expected).abs() < 0.00001),
                "samples changed at {first:?} -> {second:?}"
            );
        }
    }

    #[test]
    fn accepted_compressed_audio_reaches_the_output_undecoded_and_switches_at_format_changes() {
        ensure_gstreamer_initialized().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let wave = directory.path().join("decoded.wav");
        write_silent_mono_wave(&wave, 4_000);
        let wave = ResolvedStream::new(gst::glib::filename_to_uri(&wave, None).unwrap().as_str());
        let fixture = |name: &str| {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name);
            ResolvedStream::new(gst::glib::filename_to_uri(path, None).unwrap().as_str())
        };
        // 12 E-AC3 5.1 frames each.
        let (first, second) = (fixture("surround-a.eac3"), fixture("surround-b.eac3"));
        let crossfade = NextTransition::Crossfade {
            duration_millis: 1_000,
        };

        #[derive(Debug, Default)]
        struct Rendered {
            formats: Vec<String>,
            frames: usize,
            samples: usize,
            transitions: usize,
            ended: Vec<RunId>,
            largest_gap: Duration,
            passed_through: Vec<RunId>,
        }
        // Plays until `last` ends. A core that receives `Ended` starts the queued item cold.
        let render = |volume: f64,
                      current: &ResolvedStream,
                      next: Option<(&ResolvedStream, NextTransition)>,
                      last: RunId| {
            let events = Arc::new(Mutex::new(EventMailbox::default()));
            let mut engine = GstEngine::new(Arc::clone(&events));
            engine.handle_command(BackendCommand::ConfigureAudio(BackendAudioSettings {
                audio_output: Some("appsink".into()),
                fade_on_status_change: false,
                volume,
                ..BackendAudioSettings::default()
            }));
            engine.handle_command(BackendCommand::Start {
                run: RunId::new(1),
                current: current.clone().into(),
                next: next.map(|(stream, transition)| {
                    PreparedNext::new(RunId::new(2), stream.clone(), transition)
                }),
                start_position_millis: 0,
                playback_rate: 1.0,
            });
            let mut rendered = Rendered::default();
            let mut end = None::<gst::ClockTime>;
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline && !rendered.ended.contains(&last) {
                engine.poll_bus();
                engine.tick();
                assert!(
                    !(engine.active_pipeline().is_bitstream() && engine.secondary.has_session()),
                    "a second player opened beside a passed-through stream"
                );
                let sink = engine
                    .active_pipeline()
                    .audio_output()
                    .and_then(|output| output.downcast::<gstreamer_app::AppSink>().ok());
                while let Some(sample) = sink
                    .as_ref()
                    .and_then(|sink| sink.try_pull_sample(gst::ClockTime::ZERO))
                {
                    let format = sample
                        .caps()
                        .unwrap()
                        .structure(0)
                        .unwrap()
                        .name()
                        .to_string();
                    let buffer = sample.buffer().unwrap();
                    if format == "audio/x-eac3" {
                        rendered.frames += 1;
                        let segment = sample.segment().unwrap();
                        let segment = segment.downcast_ref::<gst::ClockTime>().unwrap();
                        let start = segment.to_running_time(buffer.pts().unwrap()).unwrap();
                        if let Some(end) = end {
                            rendered.largest_gap = rendered.largest_gap.max(Duration::from_nanos(
                                start.nseconds().abs_diff(end.nseconds()),
                            ));
                        }
                        end = Some(start + buffer.duration().unwrap());
                    } else {
                        rendered.samples += buffer.size() / 4;
                    }
                    if rendered.formats.last() != Some(&format) {
                        rendered.formats.push(format);
                    }
                }
                let drained = lock_recover(&events).drain();
                for event in drained {
                    match event {
                        BackendEvent::Transitioned { .. } => rendered.transitions += 1,
                        BackendEvent::Bitstream { run, active: true } => {
                            rendered.passed_through.push(run);
                        }
                        BackendEvent::Ended { run } => {
                            rendered.ended.push(run);
                            if run == RunId::new(1)
                                && let Some((stream, _)) = next
                                && rendered.transitions == 0
                            {
                                engine.handle_command(BackendCommand::Start {
                                    run: RunId::new(2),
                                    current: stream.clone().into(),
                                    next: None,
                                    start_position_millis: 0,
                                    playback_rate: 1.0,
                                });
                            }
                        }
                        BackendEvent::Error { error, .. } => panic!("{error:?}"),
                        _ => {}
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            engine.shutdown();
            assert!(rendered.ended.contains(&last), "{rendered:?}");
            rendered
        };

        let passed = render(1.0, &first, None, RunId::new(1));
        assert_eq!(
            (passed.formats, passed.frames),
            (vec!["audio/x-eac3".into()], 12)
        );
        // Volume below 100% changes samples, so the track stays decoded.
        let decoded = render(0.5, &first, None, RunId::new(1));
        assert_eq!(decoded.formats, ["audio/x-raw"]);
        assert!(decoded.samples > 0 && decoded.frames == 0, "{decoded:?}");
        assert!(decoded.passed_through.is_empty(), "{decoded:?}");
        // A passed-through album stays one gapless stream; a crossfade can't mix it.
        let album = render(1.0, &first, Some((&second, crossfade)), RunId::new(2));
        assert_eq!((album.frames, album.transitions), (24, 1), "{album:?}");
        assert!(album.largest_gap < Duration::from_micros(1), "{album:?}");
        // Both runs are labelled, including the one that started gaplessly.
        assert_eq!(
            album.passed_through,
            [RunId::new(1), RunId::new(2)],
            "{album:?}"
        );
        // A format change finishes the current track, then starts the next one in its mode.
        for (current, next, formats) in [
            (&wave, &second, ["audio/x-raw", "audio/x-eac3"]),
            (&first, &wave, ["audio/x-eac3", "audio/x-raw"]),
        ] {
            let changed = render(
                1.0,
                current,
                Some((next, NextTransition::Gapless)),
                RunId::new(2),
            );
            assert_eq!(changed.formats, formats, "{changed:?}");
            assert_eq!(
                (changed.frames, changed.samples, changed.transitions),
                (12, 4_000, 0),
                "{changed:?}"
            );
            assert_eq!(changed.ended, [RunId::new(1), RunId::new(2)]);
        }
    }

    fn write_long_silent_wave(path: &std::path::Path) {
        write_silent_mono_wave(path, 80_000);
    }

    #[test]
    fn aac_priming_seeks_render_audio_and_end_at_the_real_duration() {
        ensure_gstreamer_initialized().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("priming.m4a");
        // Generated 440 Hz AAC: no edit list, iTunSMPB delay 2112 and padding 204.
        std::fs::write(
            &path,
            include_bytes!("../tests/fixtures/aac-gapless-priming.m4a"),
        )
        .unwrap();
        let uri = gst::glib::filename_to_uri(&path, None).unwrap();
        for paused_seek in [false, true] {
            let events = Arc::new(Mutex::new(EventMailbox::default()));
            let mut engine = GstEngine::new(Arc::clone(&events));
            engine.handle_command(BackendCommand::ConfigureAudio(BackendAudioSettings {
                audio_output: Some("appsink".into()),
                fade_on_status_change: false,
                ..BackendAudioSettings::default()
            }));
            let run = RunId::new(1);
            engine.handle_command(BackendCommand::Start {
                run,
                current: ResolvedStream::new(uri.as_str()).into(),
                next: None,
                start_position_millis: 700,
                playback_rate: 1.0,
            });
            let sink = engine
                .primary
                .audio_output()
                .unwrap()
                .downcast::<gstreamer_app::AppSink>()
                .unwrap();
            let mut frames = [0_usize; 2];
            let mut peak = [0.0_f32; 2];
            let mut sought = false;
            let mut ended = false;
            let mut last_audio = None;
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline && !ended {
                engine.poll_bus();
                engine.tick();
                while let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                    let segment = sample
                        .segment()
                        .unwrap()
                        .downcast_ref::<gst::ClockTime>()
                        .unwrap();
                    assert!(
                        segment.stop().unwrap().mseconds() <= 2_967,
                        "seek extended the AAC segment past its real end: {segment:?}"
                    );
                    let buffer = sample.buffer().unwrap();
                    let map = buffer.map_readable().unwrap();
                    last_audio = Some(Instant::now());
                    let index = usize::from(sought);
                    frames[index] += map.len() / 4;
                    for bytes in map.as_slice().chunks_exact(4) {
                        peak[index] =
                            peak[index].max(f32::from_le_bytes(bytes.try_into().unwrap()).abs());
                    }
                }
                for event in lock_recover(&events).drain() {
                    match event {
                        BackendEvent::Ended { run: finished } => {
                            assert_eq!(finished, run);
                            assert!(
                                last_audio.unwrap().elapsed() < Duration::from_millis(400),
                                "AAC EOS stalled after the last rendered buffer"
                            );
                            ended = true;
                        }
                        BackendEvent::Error { error, .. }
                        | BackendEvent::OperationFailed { error, .. } => panic!("{error:?}"),
                        _ => {}
                    }
                }
                if !sought && frames[0] >= 1_600 {
                    assert!(engine.primary.position().unwrap().mseconds() >= 850);
                    if paused_seek {
                        engine.handle_command(BackendCommand::Pause { run });
                        let paused = engine.primary.position().unwrap();
                        std::thread::sleep(Duration::from_millis(100));
                        engine.poll_bus();
                        engine.tick();
                        assert!(!engine.primary.is_playing());
                        assert!(
                            engine
                                .primary
                                .position()
                                .unwrap()
                                .absdiff(paused)
                                .mseconds()
                                <= 2
                        );
                    }
                    engine.handle_command(BackendCommand::Seek {
                        run,
                        position_millis: 1_700,
                    });
                    if paused_seek {
                        engine.handle_command(BackendCommand::Play { run });
                    }
                    sought = true;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            engine.shutdown();
            assert!(
                sought && ended,
                "AAC seek never resumed or reached EOS, paused={paused_seek}"
            );
            assert!(frames[1] >= 9_000, "seek lost the decoded tail: {frames:?}");
            assert!(
                peak.iter().all(|peak| *peak > 0.1),
                "Playing produced silence: {peak:?}"
            );
        }
    }

    #[test]
    fn aac_priming_repeat_one_renders_each_run_without_stalls_or_lost_audio() {
        ensure_gstreamer_initialized().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("repeat-priming.m4a");
        std::fs::write(
            &path,
            include_bytes!("../tests/fixtures/aac-gapless-priming.m4a"),
        )
        .unwrap();
        let uri = gst::glib::filename_to_uri(&path, None).unwrap();
        let mut reference = Vec::new();
        for runs in [1_u64, 4] {
            let events = Arc::new(Mutex::new(EventMailbox::default()));
            let mut engine = GstEngine::new(Arc::clone(&events));
            engine.handle_command(BackendCommand::ConfigureAudio(BackendAudioSettings {
                audio_output: Some("appsink".into()),
                fade_on_status_change: false,
                ..BackendAudioSettings::default()
            }));
            engine.handle_command(BackendCommand::Start {
                run: RunId::new(1),
                current: ResolvedStream::new(uri.as_str()).into(),
                next: None,
                start_position_millis: 0,
                playback_rate: 1.0,
            });
            let sink = engine
                .primary
                .audio_output()
                .unwrap()
                .downcast::<gstreamer_app::AppSink>()
                .unwrap();
            let mut samples = Vec::new();
            let mut transitions = 0;
            let mut positions = [0_u64; 4];
            let mut ended = false;
            let mut last_audio = None;
            let deadline = Instant::now() + Duration::from_secs(runs * 3 + 5);
            while Instant::now() < deadline && !ended {
                engine.poll_bus();
                engine.tick();
                while let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                    let map = sample.buffer().unwrap().map_readable().unwrap();
                    last_audio = Some(Instant::now());
                    samples.extend(
                        map.as_slice()
                            .chunks_exact(4)
                            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())),
                    );
                }
                for event in lock_recover(&events).drain() {
                    match event {
                        BackendEvent::NextNeeded { run } => {
                            engine.handle_command(BackendCommand::PrepareNext {
                                current_run: run,
                                next: (run.get() < runs).then(|| {
                                    PreparedNext::new(
                                        RunId::new(run.get() + 1),
                                        ResolvedStream::new(uri.as_str()),
                                        NextTransition::Gapless,
                                    )
                                }),
                            })
                        }
                        BackendEvent::Transitioned { old_run, new_run } => {
                            assert!(
                                last_audio.unwrap().elapsed() < Duration::from_millis(400),
                                "repeat stalled after the last rendered buffer"
                            );
                            transitions += 1;
                            assert_eq!(old_run.get(), transitions);
                            assert_eq!(new_run.get(), transitions + 1);
                        }
                        BackendEvent::Position { run, millis } => {
                            let previous = &mut positions[(run.get() - 1) as usize];
                            assert!(
                                millis.saturating_add(20) >= *previous,
                                "repeat position oscillated for {run}: {previous} -> {millis}"
                            );
                            *previous = millis;
                        }
                        BackendEvent::Ended { run } => {
                            assert_eq!(run.get(), runs);
                            assert!(
                                last_audio.unwrap().elapsed() < Duration::from_millis(400),
                                "final repeat stalled before EOS"
                            );
                            ended = true;
                        }
                        BackendEvent::Error { error, .. }
                        | BackendEvent::OperationFailed { error, .. } => panic!("{error:?}"),
                        _ => {}
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            engine.shutdown();
            assert!(
                ended,
                "repeat playback stalled after {transitions} transitions"
            );
            assert_eq!(transitions, runs - 1);
            assert!(
                positions[..runs as usize]
                    .iter()
                    .all(|position| *position > 2_000),
                "a repeated run never advanced its clock: {positions:?}"
            );
            if runs == 1 {
                assert!(samples.len() > 23_000);
                assert!(samples.iter().any(|sample| sample.abs() > 0.1));
                reference = samples;
            } else {
                assert!(
                    samples.len().abs_diff(reference.len() * runs as usize) <= runs as usize,
                    "repeat lost or inserted audio: {} vs {}",
                    samples.len(),
                    reference.len() * runs as usize
                );
                let width = reference.len() / 4;
                for chunk in samples.chunks(width) {
                    let energy = chunk.iter().map(|sample| sample * sample).sum::<f32>()
                        / chunk.len() as f32;
                    assert!(energy > 0.01, "repeat rendered a silent span");
                }
            }
        }
    }

    #[test]
    fn pause_resume_fades_render_smoothly_without_position_reversal() {
        ensure_gstreamer_initialized().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fade-tone.wav");
        write_long_silent_wave(&path);
        let mut wav = std::fs::read(&path).unwrap();
        for sample in wav[44..].chunks_exact_mut(2) {
            sample.copy_from_slice(&8192_i16.to_le_bytes());
        }
        std::fs::write(&path, wav).unwrap();
        for (fade_enabled, disable_after_pause) in [(true, false), (false, false), (true, true)] {
            let events = Arc::new(Mutex::new(EventMailbox::default()));
            let mut engine = GstEngine::new(Arc::clone(&events));
            let settings = BackendAudioSettings {
                audio_output: Some("appsink".into()),
                fade_on_status_change: fade_enabled,
                volume: 0.5,
                volume_scale: VolumeScale::Linear,
                ..BackendAudioSettings::default()
            };
            let expected_level = 0.25 * settings.output_gain() as f32;
            engine.handle_command(BackendCommand::ConfigureAudio(settings.clone()));
            let run = RunId::new(1);
            let current = PreparedRun {
                run,
                stream: ResolvedStream::new(
                    gst::glib::filename_to_uri(&path, None).unwrap().as_str(),
                )
                .into(),
            };
            engine
                .start_pipeline(
                    Slot::Primary,
                    &current,
                    &settings,
                    settings.output_gain(),
                    settings.muted,
                    1.0,
                    gst::State::Null,
                    None,
                    None,
                )
                .unwrap();
            let output = engine.primary.audio_output().unwrap();
            let output_bin = output.parent().unwrap().downcast::<gst::Bin>().unwrap();
            let gain = output_bin.by_name("rufin-output-gain").unwrap();
            let downstream = gain
                .static_pad("src")
                .unwrap()
                .peer()
                .unwrap()
                .parent_element()
                .unwrap();
            let queue = gst::ElementFactory::make("queue")
                .property("max-size-time", 400_000_000_u64)
                .property("max-size-bytes", 0_u32)
                .property("max-size-buffers", 0_u32)
                .build()
                .unwrap();
            output_bin.add(&queue).unwrap();
            gain.unlink(&downstream);
            gst::Element::link_many([&gain, &queue, &downstream]).unwrap();
            engine.handle_command(BackendCommand::Start {
                run,
                current: current.stream,
                next: None,
                start_position_millis: 3_000,
                playback_rate: 1.0,
            });
            let sink = engine
                .primary
                .audio_output()
                .unwrap()
                .downcast::<gstreamer_app::AppSink>()
                .unwrap();
            let mut down = Vec::new();
            let mut up = Vec::new();
            let mut frames = 0;
            let mut pause_requested = false;
            let mut resumed = false;
            let mut paused_position = 0;
            let mut last_position = 0;
            let deadline = Instant::now() + Duration::from_secs(6);
            while Instant::now() < deadline
                && (up.len() < 60
                    || !engine.primary.fade_finished()
                    || up
                        .last()
                        .is_none_or(|level: &f32| *level < expected_level * 0.996))
            {
                engine.poll_bus();
                engine.tick();
                while let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                    let map = sample.buffer().unwrap().map_readable().unwrap();
                    let values = map
                        .as_slice()
                        .chunks_exact(4)
                        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
                        .collect::<Vec<_>>();
                    frames += values.len();
                    let level = values.iter().sum::<f32>() / values.len() as f32;
                    if resumed {
                        up.push(level);
                    } else if pause_requested {
                        down.push(level);
                    }
                }
                for event in lock_recover(&events).drain() {
                    match event {
                        BackendEvent::Position { millis, .. } => {
                            assert!(
                                millis.saturating_add(20) >= last_position,
                                "pause/resume reversed playback position: {last_position} -> {millis}"
                            );
                            last_position = millis;
                        }
                        BackendEvent::Error { error, .. }
                        | BackendEvent::OperationFailed { error, .. } => panic!("{error:?}"),
                        _ => {}
                    }
                }
                if !pause_requested
                    && frames >= 3_200
                    && queue.property::<u64>("current-level-time") >= 300_000_000
                {
                    engine.handle_command(BackendCommand::Pause { run });
                    pause_requested = true;
                } else if pause_requested && !resumed && !engine.primary.is_playing() {
                    let position = engine.primary.position().unwrap();
                    paused_position = position.mseconds();
                    std::thread::sleep(Duration::from_millis(120));
                    engine.poll_bus();
                    engine.tick();
                    assert!(
                        engine
                            .primary
                            .position()
                            .unwrap()
                            .absdiff(position)
                            .mseconds()
                            <= 2
                    );
                    assert!(sink.try_pull_sample(gst::ClockTime::ZERO).is_none());
                    if disable_after_pause {
                        let mut changed = settings.clone();
                        changed.fade_on_status_change = false;
                        engine.handle_command(BackendCommand::ConfigureAudio(changed));
                    }
                    if !fade_enabled {
                        engine.handle_command(BackendCommand::ConfigureAudio(settings.clone()));
                        engine.handle_command(BackendCommand::Seek {
                            run,
                            position_millis: paused_position,
                        });
                        let preroll_deadline = Instant::now() + Duration::from_secs(3);
                        while engine.primary.seek_in_progress() || !engine.primary.has_prerolled() {
                            engine.poll_bus();
                            engine.tick();
                            assert!(
                                Instant::now() < preroll_deadline,
                                "paused seek did not preroll"
                            );
                            std::thread::sleep(Duration::from_millis(1));
                        }
                    }
                    engine.handle_command(BackendCommand::Play { run });
                    resumed = true;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let final_position = engine.primary.position().unwrap().mseconds();
            engine.shutdown();
            assert!(
                pause_requested,
                "fade test did not queue enough PCM ahead of playback"
            );
            assert!(
                resumed && up.len() >= 30,
                "resume did not render sustained audio"
            );
            assert!(final_position >= paused_position + 300);
            if fade_enabled {
                assert!(
                    down.len() >= 10
                        && down[0] > expected_level * 0.8
                        && *down.last().unwrap() < expected_level * 0.2,
                    "pause did not render a fade: {down:?}"
                );
            }
            if fade_enabled && !disable_after_pause {
                assert!(
                    up[0] < expected_level * 0.2 && *up.last().unwrap() > expected_level * 0.96,
                    "resume did not render a fade: {up:?}"
                );
                for levels in [&down, &up] {
                    assert!(
                        levels
                            .windows(2)
                            .all(|pair| (pair[1] - pair[0]).abs() < expected_level * 0.16),
                        "fade contained an audible amplitude jump: {levels:?}"
                    );
                }
            } else {
                let levels = if disable_after_pause {
                    &up[up.len().saturating_sub(30)..]
                } else {
                    &up[..]
                };
                assert!(
                    levels
                        .iter()
                        .all(|level| (*level - expected_level).abs() < 0.00001),
                    "fade-off playback stayed muted after queued PCM drained: {up:?}"
                );
            }
        }
    }

    fn write_silent_mono_wave(path: &std::path::Path, frames: u32) {
        const SAMPLE_RATE: u32 = 8_000;
        const CHANNELS: u16 = 1;
        const BITS_PER_SAMPLE: u16 = 16;
        let bytes_per_frame = u32::from(CHANNELS) * u32::from(BITS_PER_SAMPLE / 8);
        let data_len = frames * bytes_per_frame;
        let mut file = File::create(path).expect("create playback fixture");
        file.write_all(b"RIFF").expect("write RIFF");
        file.write_all(&(36 + data_len).to_le_bytes())
            .expect("write RIFF size");
        file.write_all(b"WAVEfmt ").expect("write WAVE format");
        file.write_all(&16_u32.to_le_bytes())
            .expect("write format size");
        file.write_all(&1_u16.to_le_bytes())
            .expect("write PCM format");
        file.write_all(&CHANNELS.to_le_bytes())
            .expect("write channel count");
        file.write_all(&SAMPLE_RATE.to_le_bytes())
            .expect("write sample rate");
        file.write_all(&(SAMPLE_RATE * bytes_per_frame).to_le_bytes())
            .expect("write byte rate");
        file.write_all(&(bytes_per_frame as u16).to_le_bytes())
            .expect("write block alignment");
        file.write_all(&BITS_PER_SAMPLE.to_le_bytes())
            .expect("write sample size");
        file.write_all(b"data").expect("write data tag");
        file.write_all(&data_len.to_le_bytes())
            .expect("write data size");
        for _ in 0..frames {
            file.write_all(&0_i16.to_le_bytes()).expect("write sample");
        }
    }

    #[test]
    fn seek_cancels_each_unconfirmed_handoff_and_late_confirmation_is_inert() {
        let mut separate = HandoffFixture::separate(NextTransition::Gapless);
        separate.begin_separate();

        let restored = separate
            .engine
            .cancel_handoff_for_seek()
            .expect("separate gapless handoff restores the current run");

        assert_eq!(restored.run, separate.old_run);
        assert!(separate.engine.activating().is_none());
        assert!(
            !separate
                .engine
                .confirm_handoff(Slot::Secondary, INCOMING_PIPELINE)
        );
        assert_eq!(separate.current_run(), Some(separate.old_run));
        assert!(separate.drain().is_empty());
    }

    #[test]
    fn failed_incoming_pipeline_keeps_the_current_and_reserved_next() {
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        let current_run = RunId::new(1);
        let next = PreparedNext::new(
            RunId::new(2),
            ResolvedStream::new("https://music.example/next.flac"),
            NextTransition::Crossfade {
                duration_millis: 5_000,
            },
        );
        let incoming_id = PipelineId(8);
        {
            let mut shared = lock_recover(&engine.shared);
            shared.current = Some(PreparedRun {
                run: current_run,
                stream: ResolvedStream::new("https://music.example/current.flac").into(),
            });
            shared.next = Some(next.clone());
            shared.set_pipeline_id(Slot::Secondary, Some(incoming_id));
        }
        engine.transition = Transition::Preparing(IncomingPipeline {
            id: incoming_id,
            slot: Slot::Secondary,
            item: next.clone(),
            phase: IncomingPhase::Prerolling,
        });

        engine.fail_incoming(Slot::Secondary, incoming_id, "next stream failed".into());

        let shared = lock_recover(&engine.shared);
        assert_eq!(
            shared.current.as_ref().map(|item| item.run),
            Some(current_run)
        );
        assert_eq!(shared.next, Some(next));
        assert_eq!(shared.pipeline_id(Slot::Secondary), None);
        drop(shared);
        assert!(matches!(
            lock_recover(&events).drain().as_slice(),
            [BackendEvent::NextPreparationFailed {
                current_run: failed_current,
                next_run: failed_next,
                ..
            }] if *failed_current == current_run && *failed_next == RunId::new(2)
        ));
    }

    #[test]
    fn synchronous_next_preparation_failure_is_not_a_current_playback_error() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        let current_run = RunId::new(1);
        let current_pipeline = PipelineId(7);
        let next = PreparedNext::new(
            RunId::new(2),
            ResolvedStream::new("https://music.example/next.flac"),
            NextTransition::Crossfade {
                duration_millis: 5_000,
            },
        );
        {
            let mut shared = lock_recover(&engine.shared);
            shared.current = Some(PreparedRun {
                run: current_run,
                stream: ResolvedStream::new("https://music.example/current.flac").into(),
            });
            shared.set_pipeline_id(Slot::Primary, Some(current_pipeline));
            shared.settings.audio_output = Some("rufin-test-missing-output".to_string());
        }

        engine.handle_command(BackendCommand::PrepareNext {
            current_run,
            next: Some(next.clone()),
        });

        let shared = lock_recover(&engine.shared);
        assert_eq!(
            shared.current.as_ref().map(|item| item.run),
            Some(current_run)
        );
        assert_eq!(shared.next, Some(next));
        assert_eq!(shared.pipeline_id(Slot::Primary), Some(current_pipeline));
        drop(shared);
        let events = lock_recover(&events).drain();
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, BackendEvent::Error { .. }))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            BackendEvent::NextPreparationFailed {
                current_run: failed_current,
                next_run: failed_next,
                ..
            } if *failed_current == current_run && *failed_next == RunId::new(2)
        )));
    }

    #[test]
    fn failed_uri_preload_does_not_claim_current_or_unattributed_errors() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let current_uri = "https://music.example/current.flac";
        let next_uri = "https://music.example/next.flac";
        for (failed_uri, pending_uri, next_failed) in [
            (Some(next_uri), next_uri, true),
            (Some(current_uri), next_uri, false),
            (Some(current_uri), current_uri, false),
            (None, next_uri, false),
        ] {
            let events = Arc::new(Mutex::new(EventMailbox::default()));
            let mut engine = GstEngine::new(Arc::clone(&events));
            let current_run = RunId::new(1);
            let current = PreparedRun {
                run: current_run,
                stream: ResolvedStream::new(current_uri).into(),
            };
            let settings = BackendAudioSettings {
                audio_output: Some("fakesink".to_string()),
                ..BackendAudioSettings::default()
            };
            let pipeline = engine
                .start_pipeline(
                    Slot::Primary,
                    &current,
                    &settings,
                    settings.volume,
                    settings.muted,
                    DEFAULT_PLAYBACK_RATE,
                    gst::State::Null,
                    None,
                    None,
                )
                .expect("create inactive session");
            let next = PreparedNext::new(
                RunId::new(2),
                ResolvedStream::new(pending_uri),
                NextTransition::Gapless,
            );
            {
                let mut shared = lock_recover(&engine.shared);
                shared.current = Some(current);
                lock_recover(engine.primary.gapless().unwrap()).pending = Some(next);
            }
            let details = gst::Structure::builder("message-details")
                .field_if_some("uri", failed_uri)
                .build();
            let message = gst::message::Error::builder(gst::ResourceError::Read, "stream failed")
                .details(details)
                .build();
            engine.handle_message(Slot::Primary, pipeline, &message);

            let reported = lock_recover(&events).drain();
            if next_failed {
                // The current run keeps its session and ends through the drained EOS.
                assert!(engine.primary.has_session() && engine.primary.draining());
                assert!(matches!(
                    reported.as_slice(),
                    [BackendEvent::NextPreparationFailed {
                        current_run: failed_current,
                        next_run: failed_next,
                        ..
                    }] if *failed_current == current_run && *failed_next == RunId::new(2)
                ));
                engine.handle_message(Slot::Primary, pipeline, &message);
                assert!(lock_recover(&events).drain().is_empty());
            } else {
                assert!(!engine.primary.has_session());
                let events = reported;
                assert!(matches!(
                    events.as_slice(),
                    [BackendEvent::Error { run, .. }] if *run == current_run
                ));
            }
        }
    }

    #[test]
    fn source_clock_maps_one_cue_window_everywhere() {
        let stream = ResolvedStream::new("file:///music/cue.flac").with_window(60_000, 90_000);
        let clock = SourceClock::from_stream(&stream);

        assert_eq!(clock.physical_seek(12_345), 72_345);
        assert_eq!(clock.physical_seek(40_000), 90_000);
        assert_eq!(clock.logical_position(72_345), 12_345);
        assert_eq!(clock.logical_position(95_000), 30_000);
        assert_eq!(clock.logical_duration(180_000), 30_000);
        assert_eq!(clock.remaining(72_345, 180_000), 17_655);
    }

    #[test]
    fn replaced_pipeline_cannot_consume_next_or_relabel_visualizer_work() {
        let old = PipelineId(4);
        let current = PipelineId(5);
        let current_run = RunId::new(10);
        let next = PreparedNext::new(
            RunId::new(11),
            ResolvedStream::new("file:///music/next.flac"),
            NextTransition::Gapless,
        );
        let mut shared = SharedBackendState::new();
        shared.active = Slot::Primary;
        shared.current = Some(PreparedRun {
            run: current_run,
            stream: ResolvedStream::new("file:///music/current.flac").into(),
        });
        shared.next = Some(next.clone());
        shared.visualizer_enabled = true;
        shared.set_pipeline_id(Slot::Primary, Some(current));

        assert_eq!(
            about_to_finish_action_for_pipeline(
                &mut shared,
                &mut GaplessPlayback::default(),
                Slot::Primary,
                old,
                true
            ),
            AboutToFinishAction::Ignore
        );
        assert_eq!(shared.next, Some(next));

        let shared = Arc::new(Mutex::new(shared));
        assert!(!visualizer_pipeline_is_live(
            &shared,
            Slot::Primary,
            old,
            current_run,
        ));
        assert!(visualizer_pipeline_is_live(
            &shared,
            Slot::Primary,
            current,
            current_run,
        ));
    }

    #[test]
    fn about_to_finish_before_playback_does_not_consume_the_next_track() {
        let mut gapless = GaplessPlayback::default();
        let pipeline = PipelineId(5);
        let current_run = RunId::new(10);
        let next = PreparedNext::new(
            RunId::new(11),
            ResolvedStream::new("file:///music/next.flac"),
            NextTransition::Gapless,
        );
        let mut shared = SharedBackendState::new();
        shared.active = Slot::Primary;
        shared.current = Some(PreparedRun {
            run: current_run,
            stream: ResolvedStream::new("file:///music/current.mod").into(),
        });
        shared.next = Some(next.clone());
        shared.set_pipeline_id(Slot::Primary, Some(pipeline));

        assert!(about_to_finish_may_query(&shared, Slot::Primary, pipeline));
        assert_eq!(
            about_to_finish_action_for_pipeline(
                &mut shared,
                &mut gapless,
                Slot::Primary,
                pipeline,
                false
            ),
            AboutToFinishAction::Ignore
        );
        assert_eq!(shared.next, Some(next.clone()));
        assert!(gapless.pending.is_none());

        assert!(matches!(
            about_to_finish_action_for_pipeline(&mut shared, &mut gapless, Slot::Primary, pipeline, true),
            AboutToFinishAction::Preload(preloaded) if *preloaded == next
        ));
    }

    #[test]
    fn streams_without_preloading_keep_the_next_reserved() {
        let mut gapless = GaplessPlayback::default();
        let pipeline = PipelineId(5);
        let current_run = RunId::new(10);
        let next = PreparedNext::new(
            RunId::new(11),
            ResolvedStream::new("file:///music/next.flac"),
            NextTransition::Gapless,
        );
        let mut shared = SharedBackendState::new();
        shared.active = Slot::Primary;
        shared.current = Some(PreparedRun {
            run: current_run,
            stream: PreparedStream::from(ResolvedStream::new("file:///music/current.mod"))
                .without_preloading(),
        });
        shared.next = Some(next.clone());
        shared.set_pipeline_id(Slot::Primary, Some(pipeline));

        assert!(!about_to_finish_may_query(&shared, Slot::Primary, pipeline));
        assert_eq!(
            about_to_finish_action_for_pipeline(
                &mut shared,
                &mut gapless,
                Slot::Primary,
                pipeline,
                true
            ),
            AboutToFinishAction::Ignore
        );
        assert_eq!(shared.next, Some(next));
        assert!(gapless.pending.is_none());
    }

    #[test]
    fn processing_changes_and_new_events_wake_their_owners() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .unwrap();
        let _runtime = runtime.enter();
        let (requests, receiver) = tokio::sync::watch::channel(ProcessingRequest::default());
        let mut backend = GStreamerPlaybackBackend::new(receiver).unwrap();
        let (woke, wakes) = std::sync::mpsc::channel();
        backend.set_event_waker(Arc::new(move || {
            let _ = woke.send(());
        }));
        backend.drain_events();
        while wakes.try_recv().is_ok() {}

        requests.send_modify(|request| request.meter = !request.meter);
        wakes
            .recv_timeout(Duration::from_secs(5))
            .expect("a processing change reaches the idle worker and wakes core");
        assert!(
            backend
                .drain_events()
                .iter()
                .any(|event| matches!(event, BackendEvent::ProcessingApplied { .. }))
        );
        backend.shutdown().unwrap();
    }

    #[test]
    fn event_mailbox_coalesces_telemetry_without_crossing_ordered_events() {
        let first = RunId::new(1);
        let second = RunId::new(2);
        let mut mailbox = EventMailbox::default();

        mailbox.push(BackendEvent::Position {
            run: first,
            millis: 10,
        });
        mailbox.push(BackendEvent::Position {
            run: first,
            millis: 20,
        });
        mailbox.push(BackendEvent::Duration {
            run: first,
            millis: 90,
        });
        mailbox.push(BackendEvent::Position {
            run: second,
            millis: 30,
        });
        mailbox.push(BackendEvent::State {
            run: first,
            state: BackendState::Playing,
        });
        mailbox.push(BackendEvent::Position {
            run: first,
            millis: 40,
        });
        mailbox.push(BackendEvent::Position {
            run: first,
            millis: 50,
        });
        mailbox.push(BackendEvent::Visualizer {
            run: first,
            levels: vec![0.5],
        });
        mailbox.push(BackendEvent::Visualizer {
            run: first,
            levels: vec![0.75],
        });
        mailbox.push(BackendEvent::Visualizer {
            run: first,
            levels: Vec::new(),
        });
        mailbox.push(BackendEvent::Buffering {
            run: first,
            percent: 10,
        });
        mailbox.push(BackendEvent::Buffering {
            run: first,
            percent: 100,
        });
        mailbox.push(BackendEvent::Ended { run: first });

        assert_eq!(
            mailbox.drain(),
            vec![
                BackendEvent::Position {
                    run: first,
                    millis: 20,
                },
                BackendEvent::Duration {
                    run: first,
                    millis: 90,
                },
                BackendEvent::Position {
                    run: second,
                    millis: 30,
                },
                BackendEvent::State {
                    run: first,
                    state: BackendState::Playing,
                },
                BackendEvent::Position {
                    run: first,
                    millis: 50,
                },
                BackendEvent::Visualizer {
                    run: first,
                    levels: vec![0.75],
                },
                BackendEvent::Visualizer {
                    run: first,
                    levels: Vec::new(),
                },
                BackendEvent::Buffering {
                    run: first,
                    percent: 100,
                },
                BackendEvent::Ended { run: first },
            ]
        );
    }

    fn constant_tone(directory: &std::path::Path, name: &str, frames: u32) -> ResolvedStream {
        let path = directory.join(name);
        write_silent_mono_wave(&path, frames);
        let mut wav = std::fs::read(&path).unwrap();
        for sample in wav[44..].chunks_exact_mut(2) {
            sample.copy_from_slice(&8192_i16.to_le_bytes());
        }
        std::fs::write(&path, wav).unwrap();
        ResolvedStream::new(gst::glib::filename_to_uri(&path, None).unwrap().as_str())
    }

    /// Records levels on the streaming thread as buffers render. Reading them from the
    /// test loop lets the appsink queue fill, and retiring a pipeline flushes it unread.
    fn record_levels(
        pipeline: &PlayerPipeline,
        levels: &Arc<Mutex<Vec<f32>>>,
        recording: &mut Option<gstreamer_app::AppSink>,
    ) {
        let Some(sink) = pipeline
            .audio_output()
            .and_then(|output| output.downcast::<gstreamer_app::AppSink>().ok())
        else {
            return;
        };
        if recording.as_ref() == Some(&sink) {
            return;
        }
        let drain = {
            let levels = Arc::clone(levels);
            move |sink: &gstreamer_app::AppSink| {
                let mut levels = lock_recover(&levels);
                while let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                    let map = sample.buffer().unwrap().map_readable().unwrap();
                    let values = map
                        .as_slice()
                        .chunks_exact(4)
                        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
                        .collect::<Vec<_>>();
                    levels.push(values.iter().sum::<f32>() / values.len() as f32);
                }
            }
        };
        drain(&sink);
        sink.set_callbacks(
            gstreamer_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    drain(sink);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        *recording = Some(sink);
    }

    #[test]
    fn crossfade_envelopes_follow_rendered_audio_through_a_refill() {
        ensure_gstreamer_initialized().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        engine.handle_command(BackendCommand::ConfigureAudio(BackendAudioSettings {
            audio_output: Some("appsink".into()),
            volume: 1.0,
            volume_scale: VolumeScale::Linear,
            fade_on_status_change: false,
            ..BackendAudioSettings::default()
        }));
        engine.handle_command(BackendCommand::Start {
            run: RunId::new(1),
            current: constant_tone(directory.path(), "current.wav", 24_000).into(),
            next: Some(PreparedNext::new(
                RunId::new(2),
                constant_tone(directory.path(), "next.wav", 32_000),
                NextTransition::Crossfade {
                    duration_millis: 1_000,
                },
            )),
            start_position_millis: 0,
            playback_rate: 1.0,
        });
        let full = 0.25_f32;
        let outgoing = Arc::new(Mutex::new(Vec::new()));
        let incoming = Arc::new(Mutex::new(Vec::new()));
        let (mut outgoing_sink, mut incoming_sink) = (None, None);
        let rendered = || lock_recover(&outgoing).len() + lock_recover(&incoming).len();
        let mut held: Option<(Instant, PipelineId, usize)> = None;
        let mut released = false;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline
            && !(released
                && engine.overlap().is_none()
                && lock_recover(&incoming)
                    .last()
                    .is_some_and(|level| *level >= full * 0.99))
        {
            record_levels(&engine.primary, &outgoing, &mut outgoing_sink);
            record_levels(&engine.secondary, &incoming, &mut incoming_sink);
            engine.poll_bus();
            engine.tick();
            for event in lock_recover(&events).drain() {
                if let BackendEvent::Error { error, .. } = event {
                    panic!("{error:?}");
                }
            }
            if held.is_none() && engine.overlap().is_some() && lock_recover(&incoming).len() >= 10 {
                let id = lock_recover(&engine.shared)
                    .pipeline_id(Slot::Secondary)
                    .unwrap();
                // A refilling incoming stream holds both halves of the crossfade.
                engine.handle_buffering(Slot::Secondary, id, 50, gst::BufferingMode::Stream);
                held = Some((Instant::now(), id, rendered()));
            }
            if let Some((started, id, before)) = held
                && !released
                && started.elapsed() >= Duration::from_millis(400)
            {
                assert!(rendered() <= before + 20, "a held crossfade kept rendering");
                engine.handle_buffering(Slot::Secondary, id, 100, gst::BufferingMode::Stream);
                released = true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        engine.shutdown();
        assert!(released, "the crossfade never started");
        let outgoing: Vec<f32> = lock_recover(&outgoing).clone();
        let incoming: Vec<f32> = lock_recover(&incoming).clone();
        let smooth = |levels: &[f32], falling: bool| {
            levels.windows(2).all(|pair| {
                let step = if falling {
                    pair[0] - pair[1]
                } else {
                    pair[1] - pair[0]
                };
                (-0.001..full * 0.08).contains(&step)
            })
        };
        assert!(
            smooth(&outgoing, true) && *outgoing.last().unwrap() < full * 0.15,
            "outgoing fade jumped or ended loud: {outgoing:?}"
        );
        assert!(
            smooth(&incoming, false) && incoming[0] < full * 0.15,
            "incoming fade jumped or started loud: {incoming:?}"
        );
    }

    #[test]
    fn failed_queued_uri_drains_the_current_track_and_ends_it() {
        ensure_gstreamer_initialized().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        engine.handle_command(BackendCommand::ConfigureAudio(BackendAudioSettings {
            audio_output: Some("appsink".into()),
            ..BackendAudioSettings::default()
        }));
        let current_run = RunId::new(1);
        let missing = directory.path().join("missing.wav");
        engine.handle_command(BackendCommand::Start {
            run: current_run,
            current: constant_tone(directory.path(), "current.wav", 48_000).into(),
            next: Some(PreparedNext::new(
                RunId::new(2),
                ResolvedStream::new(gst::glib::filename_to_uri(&missing, None).unwrap().as_str()),
                NextTransition::Gapless,
            )),
            start_position_millis: 0,
            playback_rate: 1.0,
        });
        let rendered = Arc::new(Mutex::new(Vec::new()));
        let mut rendering = None;
        let mut rendered_at_failure = None;
        let mut ended = false;
        let deadline = Instant::now() + Duration::from_secs(12);
        while Instant::now() < deadline && !ended {
            record_levels(&engine.primary, &rendered, &mut rendering);
            engine.poll_bus();
            engine.tick();
            for event in lock_recover(&events).drain() {
                match event {
                    BackendEvent::NextPreparationFailed { error, .. } => {
                        assert_eq!(error.kind(), FailureKind::UnavailableMedia);
                        rendered_at_failure = Some(lock_recover(&rendered).len());
                    }
                    BackendEvent::Ended { run } => {
                        assert_eq!(run, current_run);
                        ended = true;
                    }
                    BackendEvent::Error { error, .. } => panic!("{error:?}"),
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        engine.shutdown();
        let rendered_at_failure = rendered_at_failure.expect("the queued URI did not fail");
        assert!(ended, "the drained track never ended");
        assert!(
            lock_recover(&rendered).len() > rendered_at_failure,
            "no queued audio played after the failure"
        );
    }

    #[test]
    fn clearing_preloaded_next_prevents_its_audio_from_playing() {
        ensure_gstreamer_initialized().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let paths = [
            directory.path().join("current.wav"),
            directory.path().join("removed.wav"),
        ];
        for (index, path) in paths.iter().enumerate() {
            write_silent_mono_wave(path, 16_000);
            let mut wav = std::fs::read(path).unwrap();
            for bytes in wav[44..].chunks_exact_mut(2) {
                bytes.copy_from_slice(
                    &(if index == 0 { -8192_i16 } else { 8192_i16 }).to_le_bytes(),
                );
            }
            std::fs::write(path, wav).unwrap();
        }
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        engine.handle_command(BackendCommand::ConfigureAudio(BackendAudioSettings {
            audio_output: Some("appsink".into()),
            fade_on_status_change: false,
            ..BackendAudioSettings::default()
        }));
        engine.handle_command(BackendCommand::Start {
            run: RunId::new(1),
            current: ResolvedStream::new(
                gst::glib::filename_to_uri(&paths[0], None)
                    .unwrap()
                    .as_str(),
            )
            .into(),
            next: Some(PreparedNext::new(
                RunId::new(2),
                ResolvedStream::new(
                    gst::glib::filename_to_uri(&paths[1], None)
                        .unwrap()
                        .as_str(),
                ),
                NextTransition::Gapless,
            )),
            start_position_millis: 0,
            playback_rate: 1.0,
        });
        let sink = engine
            .active_pipeline()
            .audio_output()
            .unwrap()
            .downcast::<gstreamer_app::AppSink>()
            .unwrap();
        let mut cleared = false;
        let mut ended = false;
        let mut samples = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !ended {
            engine.poll_bus();
            engine.tick();
            if !cleared && engine.primary.has_pending_gapless() && !samples.is_empty() {
                engine.handle_command(BackendCommand::PrepareNext {
                    current_run: RunId::new(1),
                    next: None,
                });
                cleared = true;
            }
            while let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                let map = sample.buffer().unwrap().map_readable().unwrap();
                samples.extend(
                    map.as_slice()
                        .chunks_exact(4)
                        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())),
                );
            }
            for event in lock_recover(&events).drain() {
                match event {
                    BackendEvent::Transitioned { .. } => {
                        assert!(cleared, "next track started before the test issued removal");
                        panic!("removed track started");
                    }
                    BackendEvent::Ended { run } => {
                        assert_eq!(run, RunId::new(1));
                        ended = true;
                    }
                    BackendEvent::Error { error, .. } => panic!("{error:?}"),
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        engine.shutdown();
        assert!(cleared, "next track was not preloaded");
        assert!(ended, "current track did not finish");
        assert_eq!(samples.len(), 16_000);
        assert!(
            samples
                .iter()
                .all(|sample| (*sample + 0.25).abs() < 0.00001)
        );
    }

    #[test]
    fn gapless_preload_cannot_relabel_the_next_duration_as_current() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        let current_run = RunId::new(1);
        let next_run = RunId::new(2);
        let current = PreparedRun {
            run: current_run,
            stream: ResolvedStream::new("file:///music/current.flac").into(),
        };
        let settings = BackendAudioSettings {
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        engine
            .start_pipeline(
                Slot::Primary,
                &current,
                &settings,
                settings.volume,
                settings.muted,
                DEFAULT_PLAYBACK_RATE,
                gst::State::Null,
                None,
                None,
            )
            .expect("create inactive session");
        {
            let mut shared = lock_recover(&engine.shared);
            shared.current = Some(current);
            lock_recover(engine.primary.gapless().unwrap()).pending = Some(PreparedNext::new(
                next_run,
                ResolvedStream::new("file:///music/next.flac"),
                NextTransition::Gapless,
            ));
        }

        engine.push_duration(240_000);
        assert!(lock_recover(&events).drain().is_empty());

        engine.handle_stream_start();
        lock_recover(&events).drain();
        engine.push_duration(240_000);
        assert_eq!(
            lock_recover(&events).drain(),
            vec![BackendEvent::Duration {
                run: next_run,
                millis: 240_000,
            }]
        );
        engine.shutdown();
    }
}
