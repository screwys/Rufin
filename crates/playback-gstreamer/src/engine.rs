use super::audio::{SharedLoudnessTags, apply_shared_loudness, audio_output_is_available};
use super::pipeline::{AboutToFinishAction, PlayerPipeline, SourceClock};
#[cfg(test)]
use super::waveform::visualizer_pipeline_is_live;
use super::waveform::{VisualizerAnalyzer, VisualizerTap};
use super::*;
use std::collections::HashMap;
use std::sync::mpsc::{SyncSender, sync_channel};

const GAPLESS_BUFFERING_IGNORE_REMAINING_MS: u64 = 5_000;
const STATUS_FADE_DURATION: Duration = Duration::from_millis(300);

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

#[derive(Default)]
pub(super) struct EventMailbox {
    next_sequence: u64,
    ready: VecDeque<BackendEvent>,
    latest: HashMap<(RunId, TelemetryKind), PendingTelemetry>,
}

impl EventMailbox {
    fn push(&mut self, event: BackendEvent) {
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
        | BackendEvent::NextNeeded { .. }
        | BackendEvent::NextPreparationFailed { .. }
        | BackendEvent::AudioApplied { .. }
        | BackendEvent::Visualizer { .. }
        | BackendEvent::Error { .. } => None,
    }
}

pub struct GStreamerPlaybackBackend {
    commands: Option<Sender<BackendCommand>>,
    events: Arc<Mutex<EventMailbox>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl GStreamerPlaybackBackend {
    pub fn new() -> Result<Self, BackendError> {
        let (commands, receiver) = channel();
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let thread_events = Arc::clone(&events);
        let (ready_sender, ready_receiver) = sync_channel(1);
        let thread = thread::Builder::new()
            .name("rufin-gstreamer-playback".to_string())
            .spawn(move || run_gstreamer_thread(receiver, thread_events, ready_sender))
            .map_err(|error| BackendError::Backend(error.to_string()))?;
        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
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
            .send(command)
            .map_err(|_| BackendError::ChannelClosed)
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        lock_recover(&self.events).drain()
    }

    fn shutdown(&mut self) -> Result<(), BackendError> {
        self.commands.take();
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

#[derive(Clone, Debug)]
pub(super) struct CrossfadeState {
    pub(super) from: Slot,
    pub(super) to: Slot,
    pub(super) old_run: RunId,
    pub(super) started_at: Instant,
    pub(super) duration: Duration,
}

impl CrossfadeState {
    fn progress_at(&self, now: Instant) -> f64 {
        (now.saturating_duration_since(self.started_at).as_secs_f64() / self.duration.as_secs_f64())
            .clamp(0.0, 1.0)
    }

    fn output_levels_at(&self, volume: f64, now: Instant) -> [f64; 2] {
        let progress = self.progress_at(now);
        let mut levels = [volume; 2];
        levels[self.from.index()] = (progress * FRAC_PI_2).cos() * volume;
        levels[self.to.index()] = (progress * FRAC_PI_2).sin() * volume;
        levels
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

#[derive(Clone, Debug)]
enum PendingHandoff {
    Separate {
        incoming: IncomingPipeline,
        from: Slot,
        old_run: RunId,
    },
    AdjacentWindow {
        slot: Slot,
        id: PipelineId,
        old_run: RunId,
        item: PreparedNext,
        confirmation_after: gst::Seqnum,
    },
}

impl PendingHandoff {
    fn matches(&self, slot: Slot, id: PipelineId) -> bool {
        match self {
            Self::Separate { incoming, .. } => incoming.slot == slot && incoming.id == id,
            Self::AdjacentWindow {
                slot: pending_slot,
                id: pending_id,
                ..
            } => *pending_slot == slot && *pending_id == id,
        }
    }

    fn item(&self) -> &PreparedNext {
        match self {
            Self::Separate { incoming, .. } => &incoming.item,
            Self::AdjacentWindow { item, .. } => item,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct PendingSeek {
    target_millis: u64,
    expires_at: Instant,
    logical_state: BackendState,
    kind: PendingSeekKind,
    pub(super) retry_on_async_done: bool,
    pub(super) resume_after_seek: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingSeekKind {
    Interactive,
    Startup,
    TrackStart,
}
impl PendingSeek {
    pub(super) fn interactive(
        target_millis: u64,
        logical_state: BackendState,
        now: Instant,
    ) -> Self {
        Self {
            target_millis,
            expires_at: now + SEEK_SETTLE_WINDOW,
            logical_state,
            kind: PendingSeekKind::Interactive,
            retry_on_async_done: false,
            resume_after_seek: false,
        }
    }

    pub(super) fn startup(target_millis: u64, logical_state: BackendState, now: Instant) -> Self {
        Self::startup_with_resume(target_millis, logical_state, now, true)
    }

    pub(super) fn startup_with_resume(
        target_millis: u64,
        logical_state: BackendState,
        now: Instant,
        resume_after_seek: bool,
    ) -> Self {
        Self {
            target_millis,
            expires_at: now + STARTUP_SEEK_SETTLE_WINDOW,
            logical_state,
            kind: PendingSeekKind::Startup,
            retry_on_async_done: true,
            resume_after_seek,
        }
    }

    pub(super) fn track_start(now: Instant) -> Self {
        Self {
            target_millis: 0,
            expires_at: now + TRACK_START_SETTLE_WINDOW,
            logical_state: BackendState::Buffering,
            kind: PendingSeekKind::TrackStart,
            retry_on_async_done: false,
            resume_after_seek: false,
        }
    }

    pub(super) fn accepts_position(&self, millis: u64, now: Instant) -> bool {
        now >= self.expires_at || seek_position_matches_target(self.target_millis, millis)
    }

    pub(super) fn suppresses_state(&self, state: BackendState, now: Instant) -> bool {
        if now >= self.expires_at || state == self.logical_state {
            return false;
        }

        match self.kind {
            PendingSeekKind::Interactive => matches!(
                state,
                BackendState::Stopped
                    | BackendState::Buffering
                    | BackendState::Paused
                    | BackendState::Playing
            ),
            PendingSeekKind::Startup => matches!(
                state,
                BackendState::Stopped | BackendState::Paused | BackendState::Playing
            ),
            PendingSeekKind::TrackStart => {
                matches!(state, BackendState::Stopped | BackendState::Paused)
            }
        }
    }

    pub(super) fn suppresses_buffering(&self, now: Instant) -> bool {
        now < self.expires_at
            && matches!(
                self.kind,
                PendingSeekKind::Interactive | PendingSeekKind::Startup
            )
    }

    pub(super) fn is_track_start(&self) -> bool {
        self.kind == PendingSeekKind::TrackStart
    }

    pub(super) fn blocks_timing_query(&self) -> bool {
        self.kind == PendingSeekKind::TrackStart
    }

    fn set_desired_playing(&mut self, playing: bool) {
        self.logical_state = if playing {
            BackendState::Playing
        } else {
            BackendState::Paused
        };
        if self.kind == PendingSeekKind::Startup {
            self.resume_after_seek = playing;
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct RepeatedSeekGuard {
    target_millis: u64,
    quiet_until: Instant,
}

impl RepeatedSeekGuard {
    fn new(target_millis: u64, now: Instant) -> Self {
        Self {
            target_millis,
            quiet_until: now + SEEK_SETTLE_WINDOW,
        }
    }

    fn suppresses(&mut self, target_millis: u64, now: Instant) -> bool {
        if self.target_millis != target_millis || now >= self.quiet_until {
            return false;
        }
        self.quiet_until = now + SEEK_SETTLE_WINDOW;
        true
    }
}

#[derive(Debug)]
pub(super) struct SharedBackendState {
    pub(super) settings: BackendAudioSettings,
    pub(super) playback_rate: f64,
    pub(super) current: Option<PreparedRun>,
    pub(super) next: Option<PreparedNext>,
    pub(super) gapless_pending: Option<PreparedNext>,
    pub(super) about_to_finish_pending: bool,
    pub(super) next_needed: Option<RunId>,
    pub(super) active: Slot,
    pub(super) crossfade: Option<CrossfadeState>,
    pub(super) visualizer_enabled: bool,
    pipeline_ids: [Option<PipelineId>; 2],
}
impl SharedBackendState {
    pub(super) fn new() -> Self {
        let settings = BackendAudioSettings::default();
        Self {
            current: None,
            next: None,
            gapless_pending: None,
            about_to_finish_pending: false,
            next_needed: None,
            active: Slot::Primary,
            crossfade: None,
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
pub(super) struct PreparedNextClear {
    pub(super) gapless_current: Option<(Slot, PreparedRun)>,
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
    started_at: Instant,
    duration: Duration,
    start_volume: f64,
    end_volume: f64,
    muted: bool,
}
impl StatusFade {
    pub(super) fn new(
        slot: Slot,
        target: StatusFadeTarget,
        start_volume: f64,
        end_volume: f64,
        muted: bool,
        now: Instant,
    ) -> Self {
        Self {
            slot,
            target,
            started_at: now,
            duration: STATUS_FADE_DURATION,
            start_volume: start_volume.clamp(0.0, 1.0),
            end_volume: end_volume.clamp(0.0, 1.0),
            muted,
        }
    }

    pub(super) fn volume_at(&self, now: Instant) -> f64 {
        let progress = (now.saturating_duration_since(self.started_at).as_secs_f64()
            / self.duration.as_secs_f64())
        .clamp(0.0, 1.0);
        self.start_volume + (self.end_volume - self.start_volume) * progress
    }

    fn is_finished(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started_at) >= self.duration
    }
}
pub(super) struct GstEngine {
    pub(super) primary: PlayerPipeline,
    pub(super) secondary: PlayerPipeline,
    pub(super) shared: Arc<Mutex<SharedBackendState>>,
    events: Arc<Mutex<EventMailbox>>,
    pub(super) visualizer: VisualizerAnalyzer,
    pub(super) last_position_tick: Instant,
    pub(super) state: BackendState,
    pub(super) pending_seek: Option<PendingSeek>,
    repeated_seek_guard: Option<RepeatedSeekGuard>,
    pub(super) status_fade: Option<StatusFade>,
    pub(super) restore_output_on_playing: bool,
    pub(super) play_command_started_at: Option<Instant>,
    desired_playing: bool,
    ended_run: Option<RunId>,
    next_pipeline_number: u64,
    incoming: Option<IncomingPipeline>,
    pending_handoff: Option<PendingHandoff>,
}
impl GstEngine {
    fn new(events: Arc<Mutex<EventMailbox>>) -> Self {
        let shared = Arc::new(Mutex::new(SharedBackendState::new()));
        let primary = PlayerPipeline::new("rufin-primary-player", Arc::clone(&shared));
        let secondary = PlayerPipeline::new("rufin-secondary-player", Arc::clone(&shared));
        let visualizer = VisualizerAnalyzer::new(Arc::clone(&events), Arc::clone(&shared));
        Self {
            primary,
            secondary,
            shared,
            events,
            visualizer,
            last_position_tick: Instant::now(),
            state: BackendState::Stopped,
            pending_seek: None,
            repeated_seek_guard: None,
            status_fade: None,
            restore_output_on_playing: false,
            play_command_started_at: None,
            desired_playing: false,
            ended_run: None,
            next_pipeline_number: 1,
            incoming: None,
            pending_handoff: None,
        }
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
    ) -> Result<PipelineId, String> {
        let id = self.next_pipeline_id();
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
        );
        if result.is_err() {
            let mut shared = lock_recover(&self.shared);
            if shared.pipeline_id(slot) == Some(id) {
                shared.set_pipeline_id(slot, None);
            }
        }
        result.map(|()| id)
    }

    fn stop_pipeline(&mut self, slot: Slot) {
        lock_recover(&self.shared).set_pipeline_id(slot, None);
        self.pipeline_for_slot_mut(slot).stop();
    }

    fn clear_incoming(&mut self) {
        if let Some(incoming) = self.incoming.take() {
            self.stop_pipeline(incoming.slot);
        }
    }

    fn cancel_handoff_for_replan(&mut self) -> Option<RunId> {
        self.clear_incoming();
        match self.pending_handoff.take()? {
            PendingHandoff::Separate {
                incoming,
                from,
                old_run,
            } => {
                self.stop_pipeline(incoming.slot);
                if incoming.item.transition != NextTransition::Gapless {
                    return None;
                }
                self.stop_pipeline(from);
                Some(old_run)
            }
            PendingHandoff::AdjacentWindow { slot, old_run, .. } => {
                self.stop_pipeline(slot);
                Some(old_run)
            }
        }
    }

    fn prepare_incoming(&mut self, next: &PreparedNext) {
        if !next.stream.allows_preloading {
            self.clear_incoming();
            return;
        }
        if self
            .incoming
            .as_ref()
            .is_some_and(|incoming| incoming.item == *next)
            || self
                .pending_handoff
                .as_ref()
                .is_some_and(|pending| pending.item() == next)
        {
            return;
        }
        if self.pending_handoff.is_some() {
            return;
        }
        let context = (|| {
            let shared = lock_recover(&self.shared);
            if shared.crossfade.is_some() {
                return None;
            }
            let current = shared.current.as_ref()?;
            if !current.stream.allows_preloading {
                return None;
            }
            let should_prepare = match next.transition {
                NextTransition::Crossfade { .. } => true,
                NextTransition::Gapless => {
                    gapless_uses_separate_pipeline(&shared.settings, current, next)
                }
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
        let id = match self.start_pipeline(
            slot,
            &item,
            &settings,
            0.0,
            muted,
            playback_rate,
            gst::State::Paused,
        ) {
            Ok(id) => id,
            Err(error) => {
                self.report_next_preparation_failure(next.run, error);
                return;
            }
        };
        self.incoming = Some(IncomingPipeline {
            id,
            slot,
            item: next.clone(),
            phase: IncomingPhase::Prerolling,
        });
    }

    fn incoming_matches(&self, slot: Slot, id: PipelineId) -> bool {
        self.incoming
            .as_ref()
            .is_some_and(|incoming| incoming.slot == slot && incoming.id == id)
    }

    fn pending_handoff_matches(&self, slot: Slot, id: PipelineId) -> bool {
        self.pending_handoff
            .as_ref()
            .is_some_and(|pending| pending.matches(slot, id))
    }

    fn pending_handoff_accepts_async_done(
        &self,
        slot: Slot,
        id: PipelineId,
        seqnum: gst::Seqnum,
    ) -> bool {
        self.pending_handoff
            .as_ref()
            .is_some_and(|pending| match pending {
                PendingHandoff::AdjacentWindow {
                    slot: pending_slot,
                    id: pending_id,
                    confirmation_after,
                    ..
                } => *pending_slot == slot && *pending_id == id && seqnum > *confirmation_after,
                PendingHandoff::Separate { .. } => false,
            })
    }

    fn handle_incoming_async_done(&mut self, slot: Slot, id: PipelineId) {
        let needs_initial_rate_seek = self.pipeline_for_slot(slot).needs_initial_rate_seek();
        let Some(incoming) = self
            .incoming
            .as_mut()
            .filter(|incoming| incoming.slot == slot && incoming.id == id)
        else {
            return;
        };
        match incoming.phase {
            IncomingPhase::Prerolling
                if incoming.item.stream.end_millis().is_some() || needs_initial_rate_seek =>
            {
                incoming.phase = IncomingPhase::Seeking;
                if let Err(error) = self.pipeline_for_slot(slot).seek_millis(0) {
                    self.fail_incoming(slot, id, error);
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
        if self.pending_handoff.is_some() || !self.desired_playing {
            return false;
        }
        let (from, old_run) = {
            let shared = lock_recover(&self.shared);
            let Some(old_run) = shared.current.as_ref().map(|current| current.run) else {
                return false;
            };
            (shared.active, old_run)
        };
        let ready = self.incoming.as_ref().is_some_and(|incoming| {
            incoming.slot == slot && incoming.id == id && incoming.phase == IncomingPhase::Ready
        });
        if !ready {
            return false;
        }
        let Some(incoming) = self.incoming.take() else {
            return false;
        };
        self.pending_handoff = Some(PendingHandoff::Separate {
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
        if !self.desired_playing {
            self.cancel_unconfirmed_handoff_for_pause();
            return false;
        }
        let Some(pending) = self
            .pending_handoff
            .as_ref()
            .filter(|pending| pending.matches(slot, id))
        else {
            return false;
        };
        let (still_current, stop_target_if_stale) = {
            let shared = lock_recover(&self.shared);
            match pending {
                PendingHandoff::Separate {
                    incoming,
                    from,
                    old_run,
                } => {
                    let target_is_live = shared.pipeline_is_live(slot, id);
                    (
                        shared.active == *from
                            && shared.current.as_ref().map(|current| current.run) == Some(*old_run)
                            && target_is_live
                            && shared.next.as_ref() == Some(&incoming.item),
                        target_is_live,
                    )
                }
                PendingHandoff::AdjacentWindow { old_run, item, .. } => (
                    shared.pipeline_is_current(slot, id)
                        && shared.current.as_ref().map(|current| current.run) == Some(*old_run)
                        && shared.next.as_ref() == Some(item)
                        && self.pending_seek.is_none(),
                    false,
                ),
            }
        };
        if !still_current {
            self.pending_handoff = None;
            if stop_target_if_stale {
                self.stop_pipeline(slot);
            }
            return false;
        }
        let Some(pending) = self.pending_handoff.take() else {
            return false;
        };
        match pending {
            PendingHandoff::Separate {
                incoming,
                from,
                old_run,
            } => match incoming.item.transition {
                NextTransition::Gapless => {
                    self.commit_prepared_gapless_handoff(incoming, from, old_run)
                }
                NextTransition::Crossfade { duration_millis } => {
                    self.commit_crossfade_handoff(incoming, from, old_run, duration_millis)
                }
            },
            PendingHandoff::AdjacentWindow {
                slot,
                old_run,
                item,
                ..
            } => {
                self.commit_adjacent_window_handoff(slot, old_run, item);
            }
        }
        true
    }

    fn begin_adjacent_window_handoff(
        &mut self,
        slot: Slot,
        id: PipelineId,
        old_run: RunId,
        item: PreparedNext,
        confirmation_after: gst::Seqnum,
    ) -> bool {
        if self.pending_handoff.is_some() || self.pending_seek.is_some() || !self.desired_playing {
            return false;
        }
        let matches = {
            let shared = lock_recover(&self.shared);
            shared.pipeline_is_current(slot, id)
                && shared.current.as_ref().map(|current| current.run) == Some(old_run)
                && shared.next.as_ref() == Some(&item)
        };
        if !matches {
            return false;
        }
        self.clear_incoming();
        self.pending_handoff = Some(PendingHandoff::AdjacentWindow {
            slot,
            id,
            old_run,
            item,
            confirmation_after,
        });
        true
    }

    fn commit_adjacent_window_handoff(&mut self, slot: Slot, old_run: RunId, item: PreparedNext) {
        let new_run = item.run;
        self.pipeline_for_slot_mut(slot)
            .set_source_clock(&item.stream);
        let visualizer_enabled = {
            let mut shared = lock_recover(&self.shared);
            shared.current = Some(PreparedRun::from_next(&item));
            shared.next = None;
            shared.gapless_pending = None;
            shared.about_to_finish_pending = false;
            shared.next_needed = None;
            shared.visualizer_enabled
        };
        self.pending_seek = None;
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

    fn fail_handoff(&mut self, slot: Slot, id: PipelineId, error: String) -> bool {
        if !self.pending_handoff_matches(slot, id) {
            return false;
        }
        let Some(pending) = self.pending_handoff.take() else {
            return false;
        };
        let (next_run, ended_run) = match pending {
            PendingHandoff::Separate {
                incoming, old_run, ..
            } => {
                self.stop_pipeline(incoming.slot);
                (
                    incoming.item.run,
                    (incoming.item.transition == NextTransition::Gapless).then_some(old_run),
                )
            }
            PendingHandoff::AdjacentWindow {
                slot,
                old_run,
                item,
                ..
            } => {
                self.stop_pipeline(slot);
                (item.run, Some(old_run))
            }
        };
        self.report_next_preparation_failure(next_run, error);
        if let Some(ended_run) = ended_run {
            self.emit_ended_once(ended_run);
        }
        true
    }

    fn cancel_unconfirmed_handoff_for_pause(&mut self) {
        let Some(pending) = self.pending_handoff.take() else {
            return;
        };
        match pending {
            PendingHandoff::Separate {
                incoming,
                from,
                old_run,
            } => {
                self.stop_pipeline(incoming.slot);
                if incoming.item.transition == NextTransition::Gapless {
                    self.stop_pipeline(from);
                    self.emit_ended_once(old_run);
                }
            }
            PendingHandoff::AdjacentWindow { slot, old_run, .. } => {
                self.stop_pipeline(slot);
                self.emit_ended_once(old_run);
            }
        }
    }

    fn clear_incoming_candidate_at_end(&mut self, slot: Slot) {
        let pending_target = self
            .pending_handoff
            .as_ref()
            .and_then(|pending| match pending {
                PendingHandoff::Separate { incoming, from, .. } if *from == slot => {
                    Some(incoming.slot)
                }
                _ => None,
            });
        if let Some(target) = pending_target {
            self.pending_handoff = None;
            self.stop_pipeline(target);
        }
        let target = self
            .incoming
            .as_ref()
            .and_then(|incoming| self.is_active_slot(slot).then_some(incoming.slot));
        if let Some(target) = target {
            self.incoming = None;
            self.stop_pipeline(target);
        }
    }

    fn gapless_handoff_waits_for_confirmation(&self, slot: Slot) -> bool {
        self.pending_handoff
            .as_ref()
            .is_some_and(|pending| match pending {
                PendingHandoff::Separate { incoming, from, .. } => {
                    incoming.item.transition == NextTransition::Gapless && *from == slot
                }
                PendingHandoff::AdjacentWindow {
                    slot: pending_slot, ..
                } => *pending_slot == slot,
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
            shared.gapless_pending = None;
            shared.about_to_finish_pending = false;
            shared.visualizer_enabled
        };
        let new_run = incoming.item.run;
        self.pending_seek = None;
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
        let (volume, muted) = self.output_gain_state();
        let visualizer_enabled = {
            let mut shared = lock_recover(&self.shared);
            shared.next = None;
            shared.active = to;
            shared.current = Some(PreparedRun::from_next(&incoming.item));
            shared.crossfade = Some(CrossfadeState {
                from,
                to,
                old_run,
                started_at: Instant::now(),
                duration: Duration::from_millis(duration_millis),
            });
            shared.visualizer_enabled
        };
        let mut output_levels = [volume; 2];
        output_levels[from.index()] = volume;
        output_levels[to.index()] = 0.0;
        self.set_pipeline_output_levels(output_levels, muted);
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

    fn fail_incoming(&mut self, slot: Slot, id: PipelineId, error: String) {
        if !self.incoming_matches(slot, id) {
            return;
        }
        let Some(incoming) = self.incoming.take() else {
            return;
        };
        self.stop_pipeline(slot);
        self.report_next_preparation_failure(incoming.item.run, error);
    }

    fn report_next_preparation_failure(&self, next_run: RunId, error: String) {
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
                    error: BackendFailure::new(error),
                },
            );
        }
    }

    fn handle_command(&mut self, command: BackendCommand) {
        let command_run = command.run();
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
            BackendCommand::ConfigureAudio(mut settings) => {
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
                let previous_output = previous_settings.audio_output;
                let output_changed = previous_output != settings.audio_output;
                let preserve_pitch_changed =
                    previous_settings.preserve_pitch != settings.preserve_pitch;
                if self.desired_playing {
                    self.cancel_status_fade();
                } else {
                    self.status_fade = None;
                }
                let ended_run = self.cancel_handoff_for_replan();
                let result = (|| -> Result<(), String> {
                    let visualizer_enabled = self.visualizer_enabled();
                    if !self.desired_playing {
                        self.set_pipeline_output_levels([0.0; 2], settings.muted);
                        if self.active_pipeline().has_session() {
                            self.active_pipeline().set_state(gst::State::Paused)?;
                            self.push_state(BackendState::Paused);
                        }
                    }
                    let retargeted = if output_changed && !audio_output_change_requires_restart() {
                        self.active_pipeline_mut()
                            .try_reconfigure_audio(&settings)?
                    } else {
                        false
                    };
                    let restart = if preserve_pitch_changed || (output_changed && !retargeted) {
                        let current = lock_recover(&self.shared).current.clone();
                        current.map(|current| {
                            let position_millis = self
                                .active_pipeline()
                                .position()
                                .map(clock_millis)
                                .map(|position| self.active_pipeline().logical_position(position))
                                .unwrap_or_default();
                            (current, position_millis)
                        })
                    } else {
                        None
                    };
                    lock_recover(&self.shared).settings = settings.clone();
                    if let Some((current, position_millis)) = restart {
                        let logical_state = if self.desired_playing {
                            BackendState::Playing
                        } else {
                            BackendState::Paused
                        };
                        let target_state = if self.desired_playing {
                            gst::State::Playing
                        } else {
                            gst::State::Paused
                        };
                        let (start_millis, needs_preroll_seek) = self
                            .start_item_session_at_millis(current, position_millis, target_state)?;
                        self.pending_seek = pending_seek_for_session_restart(
                            start_millis,
                            position_millis,
                            logical_state,
                            target_state,
                            needs_preroll_seek,
                            Instant::now(),
                        );
                        self.push_logical_position(position_millis);
                        if !self.desired_playing {
                            self.push_state(BackendState::Paused);
                        }
                    } else {
                        self.primary.configure_audio(&settings)?;
                        self.secondary.configure_audio(&settings)?;
                    }
                    self.sync_visualizer_taps(visualizer_enabled);
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
                Ok(())
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
                self.desired_playing = false;
                let _ = self.cancel_status_fade();
                self.pending_seek = None;
                self.repeated_seek_guard = None;
                self.ended_run = None;
                self.incoming = None;
                self.pending_handoff = None;
                self.stop_pipeline(Slot::Primary);
                self.stop_pipeline(Slot::Secondary);
                {
                    let mut shared = lock_recover(&self.shared);
                    shared.current = None;
                    shared.next = None;
                    shared.gapless_pending = None;
                    shared.about_to_finish_pending = false;
                    shared.next_needed = None;
                    shared.crossfade = None;
                    shared.active = Slot::Primary;
                }
                self.primary.set_visualizer_tap(None);
                self.secondary.set_visualizer_tap(None);
                push_event(&self.events, BackendEvent::Position { run, millis: 0 });
                self.state = BackendState::Stopped;
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

        if let Err(error) = result
            && let Some(run) = command_run.or_else(|| self.timing_run_id())
        {
            push_event(
                &self.events,
                BackendEvent::Error {
                    run,
                    error: BackendFailure::new(error),
                },
            );
        }
    }

    fn play_prepared(
        &mut self,
        item: PreparedRun,
        next: Option<PreparedNext>,
        start_position_millis: u64,
    ) -> Result<(), String> {
        self.desired_playing = true;
        let incoming_next = next.clone();
        let command_started_at = Instant::now();
        self.play_command_started_at = Some(command_started_at);
        let _ = self.cancel_status_fade();
        self.pending_seek = None;
        self.repeated_seek_guard = None;
        self.ended_run = None;
        self.clear_incoming();
        self.pending_handoff = None;
        self.restore_output_on_playing = false;
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
            shared.gapless_pending = None;
            shared.about_to_finish_pending = false;
            shared.crossfade = None;
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
        )?;
        self.restore_output_on_playing = true;
        let primary_tap = self.visualizer_tap(Slot::Primary, visualizer_enabled);
        self.primary.set_visualizer_tap(primary_tap);
        info!(
            run = %item.run,
            uri_scheme = %stream_uri_scheme(item.stream.uri()),
            stream_windowed = item.stream.end_millis().is_some(),
            start_millis,
            audio_output = self.primary.audio_output_factory().as_deref().unwrap_or("unknown"),
            elapsed_ms = command_started_at.elapsed().as_millis(),
            pipeline_ms = pipeline_started_at.elapsed().as_millis(),
            "queued GStreamer playback item"
        );
        if needs_preroll_seek {
            self.start_playback_seek(start_millis);
        } else {
            self.pending_seek = Some(PendingSeek::track_start(Instant::now()));
        }
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
            .incoming
            .as_ref()
            .is_some_and(|incoming| incoming.item == next)
            || self
                .pending_handoff
                .as_ref()
                .is_some_and(|pending| pending.item() == &next)
        {
            return;
        }

        let ended_run = self.cancel_handoff_for_replan();
        if let Some(ended_run) = ended_run {
            let mut shared = lock_recover(&self.shared);
            shared.next = Some(next);
            shared.gapless_pending = None;
            shared.about_to_finish_pending = false;
            shared.next_needed = None;
            drop(shared);
            self.emit_ended_once(ended_run);
            return;
        }

        let late_preload = {
            let mut shared = lock_recover(&self.shared);
            let mut late_preload = None;
            shared.next = Some(next.clone());
            shared.next_needed = None;
            if shared.about_to_finish_pending && gapless_preload_should_run(&shared, &next) {
                if next.stream.end_millis().is_none()
                    && gapless_preload_source_is_supported(next.stream.uri())
                    && let Some(item) = shared.next.take()
                {
                    shared.gapless_pending = Some(item.clone());
                    shared.about_to_finish_pending = false;
                    late_preload = Some(item);
                }
                shared.about_to_finish_pending = false;
            }
            if shared.about_to_finish_pending && !gapless_preload_should_run(&shared, &next) {
                shared.about_to_finish_pending = false;
            }
            late_preload
        };
        if let Some(item) = late_preload {
            info!(
                next_run = %item.run,
                uri = %item.stream.redacted_uri(),
                "preloading late gapless next stream"
            );
            if let Err(error) = self.active_pipeline_mut().set_stream(&item.stream) {
                let _ = cancel_gapless_pending(&mut lock_recover(&self.shared));
                self.report_next_preparation_failure(item.run, error);
            }
        }
        self.prepare_incoming(&next);
    }

    fn clear_prepared_next(&mut self) {
        let ended_run = self.cancel_handoff_for_replan();
        let clear = clear_prepared_next_state(&mut lock_recover(&self.shared));
        if let Some((slot, current)) = clear.gapless_current {
            debug!(
                run = %current.run,
                "cleared pending gapless next stream"
            );
            if let Err(error) = self.pipeline_for_slot_mut(slot).set_stream(&current.stream) {
                warn!(
                    %error,
                    run = %current.run,
                    "failed to restore current stream after clearing pending gapless next"
                );
            }
        }
        if let Some(ended_run) = ended_run {
            self.emit_ended_once(ended_run);
        }
    }

    pub(super) fn start_seek(&mut self, millis: u64) -> Result<(), String> {
        let logical_state = if self.desired_playing {
            BackendState::Playing
        } else {
            BackendState::Paused
        };
        self.ended_run = None;
        let _ = self.cancel_status_fade();
        self.finish_crossfade_for_seek();
        let current_after_gapless_cancel = self
            .cancel_handoff_for_seek()
            .or_else(|| self.cancel_gapless_pending_for_seek());
        let target_state = if self.desired_playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        if let Some(current) = current_after_gapless_cancel {
            let (start_millis, needs_preroll_seek) =
                self.start_item_session_at_millis(current, millis, target_state)?;
            self.pending_seek = pending_seek_for_session_restart(
                start_millis,
                millis,
                logical_state,
                target_state,
                needs_preroll_seek,
                Instant::now(),
            );
            self.push_logical_position(millis);
            return Ok(());
        }
        if millis == 0 {
            let current = self.current_item()?;
            let (start_millis, needs_preroll_seek) =
                self.start_item_session_at_millis(current, 0, target_state)?;
            self.pending_seek = pending_seek_for_session_restart(
                start_millis,
                0,
                logical_state,
                target_state,
                needs_preroll_seek,
                Instant::now(),
            );
            self.push_logical_position(0);
            return Ok(());
        }
        let now = Instant::now();
        let physical_target = self.active_pipeline().physical_seek_target(millis);
        if self
            .repeated_seek_guard
            .as_mut()
            .is_some_and(|guard| guard.suppresses(physical_target, now))
        {
            debug!(
                target_millis = millis,
                "ignored repeated seek until same-target input settles"
            );
            return Ok(());
        }
        if logical_state == BackendState::Paused {
            self.active_pipeline().set_state(gst::State::Paused)?;
        }
        if let Err(error) = self.active_pipeline().seek_millis(millis) {
            warn!(
                %error,
                target_millis = millis,
                "GStreamer seek request failed"
            );
            if let Some(position) = self.active_pipeline().position() {
                self.push_position(clock_millis(position));
            }
            return Ok(());
        }
        self.repeated_seek_guard = Some(RepeatedSeekGuard::new(physical_target, Instant::now()));
        self.pending_seek = Some(PendingSeek::interactive(
            physical_target,
            logical_state,
            Instant::now(),
        ));
        Ok(())
    }

    fn start_playback_seek(&mut self, millis: u64) {
        debug!(
            target_millis = millis,
            "deferring startup seek until GStreamer preroll completes"
        );
        self.pending_seek = Some(PendingSeek::startup(millis, self.state, Instant::now()));
    }

    fn cancel_gapless_pending_for_seek(&mut self) -> Option<PreparedRun> {
        cancel_gapless_pending(&mut lock_recover(&self.shared)).map(|(current, _pending)| current)
    }

    fn cancel_handoff_for_seek(&mut self) -> Option<PreparedRun> {
        let current = lock_recover(&self.shared).current.clone();
        match self.pending_handoff.take()? {
            PendingHandoff::Separate { incoming, .. } => {
                self.stop_pipeline(incoming.slot);
                if incoming.item.transition == NextTransition::Gapless {
                    current
                } else {
                    None
                }
            }
            PendingHandoff::AdjacentWindow { slot, .. } => {
                self.stop_pipeline(slot);
                current
            }
        }
    }

    fn prepare_reserved_incoming(&mut self) {
        let next = lock_recover(&self.shared).next.clone();
        if let Some(next) = next {
            self.prepare_incoming(&next);
        }
    }

    fn current_item(&self) -> Result<PreparedRun, String> {
        lock_recover(&self.shared)
            .current
            .clone()
            .ok_or_else(|| "No current playback item is active".to_string())
    }

    fn start_item_session_at_millis(
        &mut self,
        item: PreparedRun,
        position_millis: u64,
        target_state: gst::State,
    ) -> Result<(u64, bool), String> {
        self.repeated_seek_guard = None;
        let (settings, volume, muted, playback_rate, visualizer_enabled, slot) =
            self.session_context();
        let start_millis = SourceClock::from_stream(&item.stream).physical_seek(position_millis);
        let needs_preroll_seek = start_millis > 0
            || item.stream.end_millis().is_some()
            || playback_rate != DEFAULT_PLAYBACK_RATE;
        let startup_state = if needs_preroll_seek {
            gst::State::Paused
        } else {
            target_state
        };
        self.stop_pipeline(slot);
        self.start_pipeline(
            slot,
            &item,
            &settings,
            volume,
            muted,
            playback_rate,
            startup_state,
        )?;
        let tap = self.visualizer_tap(slot, visualizer_enabled);
        self.pipeline_for_slot_mut(slot).set_visualizer_tap(tap);
        Ok((start_millis, needs_preroll_seek))
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

    fn poll_bus(&mut self) {
        while let Some((id, message)) = self.primary.pop_bus_message() {
            self.handle_message(Slot::Primary, id, &message);
        }
        while let Some((id, message)) = self.secondary.pop_bus_message() {
            self.handle_message(Slot::Secondary, id, &message);
        }
    }

    fn handle_message(&mut self, slot: Slot, id: PipelineId, message: &gst::Message) {
        if !lock_recover(&self.shared).pipeline_is_live(slot, id) {
            return;
        }
        use gst::MessageView;

        if self.pending_handoff_matches(slot, id) {
            match message.view() {
                MessageView::Error(error) => {
                    let output = self.pipeline_for_slot(slot).audio_output_factory();
                    let details = gstreamer_error_details(
                        message,
                        "prepared playback handoff",
                        output.as_deref(),
                    )
                    .unwrap_or_else(|| error.error().to_string());
                    self.fail_handoff(slot, id, details);
                }
                MessageView::StateChanged(state)
                    if matches!(
                        self.pending_handoff.as_ref(),
                        Some(PendingHandoff::Separate { .. })
                    ) && self.message_source_is_pipeline(slot, message)
                        && state.current() == gst::State::Playing
                        && state.pending() == gst::State::VoidPending =>
                {
                    self.confirm_handoff(slot, id);
                }
                MessageView::AsyncDone(_)
                    if self.message_source_is_pipeline(slot, message)
                        && self.pending_handoff_accepts_async_done(slot, id, message.seqnum()) =>
                {
                    self.confirm_handoff(slot, id);
                }
                _ => {}
            }
            return;
        }

        match message.view() {
            MessageView::AsyncDone(_)
                if self.incoming_matches(slot, id)
                    && self.message_source_is_pipeline(slot, message) =>
            {
                self.handle_incoming_async_done(slot, id);
                return;
            }
            MessageView::Error(error) if self.incoming_matches(slot, id) => {
                let output = self.pipeline_for_slot(slot).audio_output_factory();
                let details =
                    gstreamer_error_details(message, "prepared playback", output.as_deref())
                        .unwrap_or_else(|| error.error().to_string());
                self.fail_incoming(slot, id, details);
                return;
            }
            _ => {}
        }

        match message.view() {
            MessageView::StateChanged(state)
                if self.message_source_is_pipeline(slot, message) && self.is_active_slot(slot) =>
            {
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
            MessageView::AsyncDone(_) if self.is_active_slot(slot) => {
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
            MessageView::StreamStart(_) if self.is_active_slot(slot) => {
                if let Some(started_at) = self.play_command_started_at {
                    let run = self.timing_run_id();
                    debug!(
                        run = run.map(RunId::get).unwrap_or_default(),
                        ?slot,
                        elapsed_ms = started_at.elapsed().as_millis(),
                        "GStreamer startup stream start"
                    );
                }
                self.handle_stream_start();
            }
            MessageView::Tag(tag) if self.is_active_slot(slot) => {
                self.log_stream_diagnostics(slot, &tag.tags());
            }
            MessageView::DurationChanged(_) if self.is_active_slot(slot) => {
                if self.pending_seek.is_none()
                    && let Some(duration) = self.active_pipeline().duration()
                {
                    self.push_physical_duration(clock_millis(duration));
                }
            }
            MessageView::Buffering(buffering) if self.is_active_slot(slot) => {
                let percent = buffering.percent().min(100) as u8;
                if matches!(percent, 1 | 25 | 50 | 75 | 100)
                    && let Some(started_at) = self.play_command_started_at
                {
                    let run = self.timing_run_id();
                    debug!(
                        run = run.map(RunId::get).unwrap_or_default(),
                        ?slot,
                        percent,
                        elapsed_ms = started_at.elapsed().as_millis(),
                        "GStreamer startup buffering"
                    );
                }
                self.handle_buffering(percent);
            }
            MessageView::SegmentDone(_) => self.handle_end(slot, true),
            MessageView::Eos(_) => self.handle_end(slot, false),
            MessageView::Error(error_message) => {
                let output = self.pipeline_for_slot(slot).audio_output_factory();
                let error = gstreamer_error_details(message, "playback", output.as_deref())
                    .unwrap_or_else(|| error_message.error().to_string());
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
                if relevant && self.handle_transition_error(slot, &error) {
                    return;
                }
                if relevant {
                    let run = self.run_for_slot(slot).or_else(|| self.timing_run_id());
                    self.stop_after_playback_error();
                    if let Some(run) = run {
                        push_event(
                            &self.events,
                            BackendEvent::Error {
                                run,
                                error: BackendFailure::new(error),
                            },
                        );
                        self.state = BackendState::Stopped;
                        push_event(
                            &self.events,
                            BackendEvent::State {
                                run,
                                state: BackendState::Stopped,
                            },
                        );
                    }
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

    fn handle_transition_error(&mut self, slot: Slot, error: &str) -> bool {
        self.handle_gapless_preload_error(slot, error)
            || self.handle_crossfade_next_error(slot, error)
    }

    fn handle_gapless_preload_error(&mut self, slot: Slot, error: &str) -> bool {
        let reset = (|| {
            let mut shared = lock_recover(&self.shared);
            if shared.active != slot {
                return None;
            }
            cancel_gapless_pending(&mut shared)
        })();
        let Some((current, pending)) = reset else {
            return false;
        };
        warn!(
            next_run = %pending.run,
            error = %error,
            "gapless next stream failed before commit"
        );
        self.stop_pipeline(slot);
        self.state = BackendState::Stopped;
        push_event(
            &self.events,
            BackendEvent::NextPreparationFailed {
                current_run: current.run,
                next_run: pending.run,
                error: BackendFailure::new(error),
            },
        );
        self.emit_ended_once(current.run);
        true
    }

    fn handle_crossfade_next_error(&mut self, slot: Slot, error: &str) -> bool {
        let crossfade = lock_recover(&self.shared).crossfade.clone();
        let Some(crossfade) = crossfade else {
            return false;
        };
        if slot == crossfade.from {
            warn!(%error, old_run = %crossfade.old_run, "outgoing crossfade tail failed");
            self.finish_crossfade(crossfade);
            return true;
        }
        false
    }

    fn handle_stream_start(&mut self) {
        let started = (|| {
            let mut shared = lock_recover(&self.shared);
            let item = shared.gapless_pending.take()?;
            let old_run = shared.current.as_ref()?.run;
            shared.current = Some(PreparedRun::from_next(&item));
            shared.about_to_finish_pending = false;
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
        self.pending_seek = None;
        self.ended_run = None;
        self.last_position_tick = Instant::now();
        self.pipeline_for_slot_mut(self.active_slot())
            .set_source_clock(&stream);
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
        let now = Instant::now();
        if self
            .pending_seek
            .as_ref()
            .is_some_and(|pending| pending.suppresses_state(state, now))
        {
            return;
        }
        if self
            .pending_seek
            .as_ref()
            .is_some_and(|pending| now >= pending.expires_at)
        {
            self.pending_seek = None;
        }
        if state == BackendState::Playing
            && self
                .pending_seek
                .as_ref()
                .is_some_and(PendingSeek::is_track_start)
        {
            self.pending_seek = None;
        }
        if state == BackendState::Playing
            && self.status_fade.is_none()
            && self.restore_output_on_playing
        {
            let (volume, muted) = self.output_gain_state();
            self.active_pipeline().set_output_volume(volume, muted);
            self.restore_output_on_playing = false;
        }
        self.push_state(state);
    }

    fn handle_buffering(&mut self, percent: u8) {
        if percent < 100 && self.gapless_preload_near_end() {
            debug!(
                percent,
                "ignoring buffering while gapless handoff is pending near end"
            );
            return;
        }
        let now = Instant::now();
        if self
            .pending_seek
            .as_ref()
            .is_some_and(|pending| pending.suppresses_buffering(now))
        {
            return;
        }
        if self
            .pending_seek
            .as_ref()
            .is_some_and(|pending| now >= pending.expires_at)
        {
            self.pending_seek = None;
        }
        self.state = BackendState::Buffering;
        if let Some(run) = self.timing_run_id() {
            push_event(&self.events, BackendEvent::Buffering { run, percent });
        }
    }

    fn gapless_preload_near_end(&self) -> bool {
        if lock_recover(&self.shared).gapless_pending.is_none() {
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

    fn handle_async_done(&mut self) {
        self.push_seekable();
        if self.retry_pending_seek() {
            return;
        }
        if self
            .pending_seek
            .as_ref()
            .is_some_and(PendingSeek::blocks_timing_query)
        {
            return;
        }
        if let Some(position) = self.active_pipeline().position() {
            self.push_position(clock_millis(position));
        }
    }

    fn retry_pending_seek(&mut self) -> bool {
        let Some(pending) = self.pending_seek.as_mut() else {
            return false;
        };
        if !pending.retry_on_async_done {
            return false;
        }
        let now = Instant::now();
        if now >= pending.expires_at {
            let resume_after_seek = pending.resume_after_seek;
            self.pending_seek = None;
            if resume_after_seek {
                self.resume_after_startup_seek();
            }
            return false;
        }
        let target_millis = pending.target_millis;
        pending.retry_on_async_done = false;
        pending.expires_at = now + STARTUP_SEEK_SETTLE_WINDOW;
        let resume_after_seek = pending.resume_after_seek;
        let seek_result = self.active_pipeline().seek_physical_millis(target_millis);
        if let Err(error) = seek_result {
            warn!(
                %error,
                target_millis,
                "deferred startup seek failed; resuming from current position"
            );
            self.pending_seek = None;
            if resume_after_seek {
                self.resume_after_startup_seek();
            }
            if let Some(position) = self.active_pipeline().position() {
                self.push_position(clock_millis(position));
            }
        } else {
            debug!(target_millis, "deferred startup seek started");
        }
        true
    }

    fn resume_after_startup_seek(&mut self) {
        if self
            .active_pipeline()
            .set_state(gst::State::Playing)
            .is_ok()
        {
            if self.restore_output_on_playing {
                let (volume, muted) = self.output_gain_state();
                self.active_pipeline().set_output_volume(volume, muted);
                self.restore_output_on_playing = false;
            }
            self.push_state(BackendState::Playing);
        }
    }

    fn push_state(&mut self, state: BackendState) {
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
        self.state = state;
        if let Some(run) = run {
            push_event(&self.events, BackendEvent::State { run, state });
        }
    }

    fn handle_end(&mut self, slot: Slot, stream_window: bool) {
        if self.finish_crossfade_if_needed(slot) {
            return;
        }
        if self.gapless_handoff_waits_for_confirmation(slot) {
            return;
        }
        if stream_window && self.is_active_slot(slot) && self.pending_seek.is_some() {
            debug!(
                ?slot,
                "ignoring stream-window completion while a seek is pending"
            );
            return;
        }
        if self.desired_playing
            && stream_window
            && self.is_active_slot(slot)
            && self.promote_adjacent_stream_window()
        {
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
                stream_window, "playback reached end"
            );
            if let Some(run) = run {
                self.emit_ended_once(run);
            }
        }
    }

    fn start_status_pause(&mut self) -> Result<(), String> {
        if !self.desired_playing {
            return Ok(());
        }
        self.desired_playing = false;
        let _ = self.cancel_status_fade();
        self.cancel_unconfirmed_handoff_for_pause();
        if let Some(pending) = self.pending_seek.as_mut() {
            pending.set_desired_playing(false);
        }
        self.state = BackendState::Paused;
        self.finish_crossfade_for_visible_current();
        let (volume, muted, enabled) = self.status_fade_gain_settings();
        if !self.active_pipeline().has_session() {
            self.push_state(BackendState::Paused);
            return Ok(());
        }
        if !enabled || muted || volume <= 0.0 {
            self.active_pipeline().set_state(gst::State::Paused)?;
            self.push_state(BackendState::Paused);
            return Ok(());
        }
        let slot = self.active_slot();
        self.status_fade = Some(StatusFade::new(
            slot,
            StatusFadeTarget::Pause,
            volume,
            0.0,
            muted,
            Instant::now(),
        ));
        self.pipeline_for_slot(slot)
            .set_output_volume(volume, muted);
        Ok(())
    }

    fn start_status_resume(&mut self) -> Result<(), String> {
        self.desired_playing = true;
        let _ = self.cancel_status_fade();
        let waiting_for_preroll = if let Some(pending) = self.pending_seek.as_mut() {
            pending.set_desired_playing(true);
            pending.retry_on_async_done
        } else {
            false
        };
        if waiting_for_preroll {
            self.push_state(BackendState::Buffering);
            return Ok(());
        }
        if !self.active_pipeline().has_session() {
            return if self.current_item().is_ok() {
                Err("No active GStreamer session to resume".to_string())
            } else {
                Ok(())
            };
        }
        let (volume, muted, enabled) = self.status_fade_gain_settings();
        if !enabled || muted || volume <= 0.0 {
            return self
                .active_pipeline()
                .set_state(gst::State::Playing)
                .map(|_| {
                    self.push_state(BackendState::Playing);
                });
        }
        let slot = self.active_slot();
        self.pipeline_for_slot(slot).set_output_volume(0.0, muted);
        self.pipeline_for_slot(slot)
            .set_state(gst::State::Playing)
            .map(|_| {
                self.push_state(BackendState::Playing);
                self.status_fade = Some(StatusFade::new(
                    slot,
                    StatusFadeTarget::Playing,
                    0.0,
                    volume,
                    muted,
                    Instant::now(),
                ));
            })
    }

    fn update_status_fade(&mut self) {
        let Some(fade) = self.status_fade else {
            return;
        };
        let now = Instant::now();
        self.pipeline_for_slot(fade.slot)
            .set_output_volume(fade.volume_at(now), fade.muted);
        if !fade.is_finished(now) {
            return;
        }

        self.status_fade = None;
        match fade.target {
            StatusFadeTarget::Pause => {
                if let Err(error) = self
                    .pipeline_for_slot(fade.slot)
                    .set_state(gst::State::Paused)
                {
                    if let Some(run) = self.timing_run_id() {
                        push_event(
                            &self.events,
                            BackendEvent::Error {
                                run,
                                error: BackendFailure::new(error),
                            },
                        );
                    }
                    return;
                }
                self.push_state(BackendState::Paused);
                let (volume, muted) = self.output_gain_state();
                self.pipeline_for_slot(fade.slot)
                    .set_output_volume(volume, muted);
            }
            StatusFadeTarget::Playing => {
                let (volume, muted) = self.output_gain_state();
                self.pipeline_for_slot(fade.slot)
                    .set_output_volume(volume, muted);
            }
        }
    }

    fn cancel_status_fade(&mut self) -> Option<StatusFade> {
        let fade = self.status_fade.take();
        if let Some(fade) = fade {
            let (volume, muted) = self.output_gain_state();
            self.pipeline_for_slot(fade.slot)
                .set_output_volume(volume, muted);
        }
        fade
    }

    fn status_fade_gain_settings(&self) -> (f64, bool, bool) {
        let shared = lock_recover(&self.shared);
        (
            shared.settings.output_gain(),
            shared.settings.muted,
            shared.settings.fade_on_status_change,
        )
    }

    fn tick(&mut self) {
        let next_needed = lock_recover(&self.shared).next_needed.take();
        if let Some(run) = next_needed {
            push_event(&self.events, BackendEvent::NextNeeded { run });
        }
        self.update_status_fade();
        if self.status_fade.is_some() {
            return;
        }
        self.maybe_start_crossfade();
        self.update_crossfade();

        if self.last_position_tick.elapsed() >= Duration::from_millis(500) {
            self.last_position_tick = Instant::now();
            if matches!(
                self.pending_handoff.as_ref(),
                Some(PendingHandoff::AdjacentWindow { .. })
            ) || self
                .pending_seek
                .as_ref()
                .is_some_and(PendingSeek::blocks_timing_query)
            {
                return;
            }
            if self.current_allows_timing_queries() {
                if let Some(position) = self.active_pipeline().position() {
                    self.push_position(clock_millis(position));
                }
                if self.pending_seek.is_none()
                    && let Some(duration) = self.active_pipeline().duration()
                {
                    self.push_physical_duration(clock_millis(duration));
                }
            } else if self.state == BackendState::Playing
                && let Some(position) = self.active_pipeline().running_time()
            {
                self.push_logical_position(clock_millis(position));
            }
        }
    }

    pub(super) fn push_position(&mut self, millis: u64) {
        let now = Instant::now();
        if let Some(pending) = self.pending_seek.as_ref() {
            if !pending.accepts_position(millis, now) {
                return;
            }
            let resume_after_seek = pending.resume_after_seek;
            self.pending_seek = None;
            if resume_after_seek {
                self.resume_after_startup_seek();
            }
        }
        let logical_millis = self.active_pipeline().logical_position(millis);
        if let Some(run) = self.timing_run_id() {
            if self.ended_run == Some(run) {
                return;
            }
            self.push_logical_position(logical_millis);
        }
    }

    fn promote_adjacent_stream_window(&mut self) -> bool {
        if !self.desired_playing || self.pending_seek.is_some() {
            return false;
        }
        let candidate = (|| {
            let shared = lock_recover(&self.shared);
            let current = shared.current.as_ref()?;
            if !adjacent_window_can_reuse_pipeline(&shared.settings) {
                return None;
            }
            let boundary = current.stream.end_millis()?;
            let next = shared.next.as_ref()?;
            if next.transition != NextTransition::Gapless
                || next.stream.uri() != current.stream.uri()
                || next.stream.start_millis() != boundary
            {
                return None;
            }
            Some((
                shared.active,
                shared.pipeline_id(shared.active)?,
                current.run,
                next.clone(),
            ))
        })();
        let Some((slot, id, old_run, next)) = candidate else {
            return false;
        };

        let confirmation_after = match self
            .pipeline_for_slot_mut(slot)
            .rearm_stream_window(&next.stream)
        {
            Ok(confirmation_after) => confirmation_after,
            Err(error) => {
                push_event(
                    &self.events,
                    BackendEvent::NextPreparationFailed {
                        current_run: old_run,
                        next_run: next.run,
                        error: BackendFailure::new(error),
                    },
                );
                return false;
            }
        };
        self.begin_adjacent_window_handoff(slot, id, old_run, next, confirmation_after)
    }

    fn promote_prepared_gapless(&mut self) -> bool {
        if !self.desired_playing {
            return false;
        }
        let Some(incoming) = self.incoming.as_ref().filter(|incoming| {
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
        if !self.current_allows_timing_queries() {
            push_event(
                &self.events,
                BackendEvent::Seekable {
                    run,
                    seekable: false,
                },
            );
            return;
        }
        let Some(seekable) = self.active_pipeline().seekable() else {
            return;
        };
        push_event(&self.events, BackendEvent::Seekable { run, seekable });
    }

    fn current_allows_timing_queries(&self) -> bool {
        lock_recover(&self.shared)
            .current
            .as_ref()
            .is_some_and(|current| current.stream.allows_timing_queries)
    }

    fn push_physical_duration(&self, millis: u64) {
        self.push_duration(self.active_pipeline().logical_duration(millis));
    }

    fn emit_ended_once(&mut self, run: RunId) {
        if self.ended_run == Some(run) {
            return;
        }
        self.ended_run = Some(run);
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
        if shared.gapless_pending.is_some() {
            return None;
        }
        shared.current.as_ref().map(|item| item.run)
    }

    fn run_is_current(&self, run: RunId) -> bool {
        self.timing_run_id() == Some(run)
    }

    fn maybe_start_crossfade(&mut self) {
        if !self.desired_playing || self.pending_seek.is_some() {
            return;
        }
        let request = (|| {
            let shared = lock_recover(&self.shared);
            if shared.crossfade.is_some() {
                return None;
            }
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
        if !self
            .incoming
            .as_ref()
            .is_some_and(|incoming| incoming.item == next && incoming.phase == IncomingPhase::Ready)
        {
            return;
        }
        let Some((to, id)) = self
            .incoming
            .as_ref()
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

    fn update_crossfade(&mut self) {
        let Some(crossfade) = lock_recover(&self.shared).crossfade.clone() else {
            return;
        };
        let now = Instant::now();
        let progress = crossfade.progress_at(now);
        let (volume, muted) = self.output_gain_state();
        self.set_pipeline_output_levels(crossfade.output_levels_at(volume, now), muted);
        if progress >= 1.0 {
            self.finish_crossfade(crossfade);
        }
    }

    fn finish_crossfade_if_needed(&mut self, eos_slot: Slot) -> bool {
        let crossfade = lock_recover(&self.shared).crossfade.clone();
        if let Some(crossfade) = crossfade
            && crossfade.from == eos_slot
        {
            self.finish_crossfade(crossfade);
            return true;
        }
        false
    }

    pub(super) fn finish_crossfade_for_seek(&mut self) {
        self.finish_crossfade_for_visible_current();
    }

    fn finish_crossfade_for_visible_current(&mut self) {
        let crossfade = lock_recover(&self.shared).crossfade.clone();
        if let Some(crossfade) = crossfade {
            self.finish_crossfade(crossfade);
        }
    }

    fn finish_crossfade(&mut self, crossfade: CrossfadeState) {
        self.pending_seek = None;
        self.stop_pipeline(crossfade.from);
        let (volume, muted) = self.output_gain_state();
        self.pipeline_for_slot(crossfade.to)
            .set_output_volume(volume, muted);
        let retained_next = {
            let mut shared = lock_recover(&self.shared);
            shared.active = crossfade.to;
            shared.crossfade = None;
            shared.gapless_pending = None;
            shared.about_to_finish_pending = false;
            shared.next.clone()
        };
        if let Some(next) = retained_next {
            self.prepare_incoming(&next);
        }
    }

    fn settings(&self) -> BackendAudioSettings {
        lock_recover(&self.shared).settings.clone()
    }

    fn playback_rate(&self) -> f64 {
        lock_recover(&self.shared).playback_rate
    }

    fn set_playback_rate(&mut self, rate: f64) -> Result<(), String> {
        let rate = sanitize_playback_rate(rate);
        let (active, crossfading, settings) = {
            let mut shared = lock_recover(&self.shared);
            shared.playback_rate = rate;
            (
                shared.active,
                shared.crossfade.is_some(),
                shared.settings.clone(),
            )
        };
        let handoff_in_progress = self.pending_handoff.is_some();
        let reprepare = self
            .incoming
            .as_ref()
            .filter(|incoming| incoming.phase == IncomingPhase::Seeking)
            .map(|incoming| incoming.item.clone());
        if reprepare.is_some() {
            self.clear_incoming();
        }

        for slot in [Slot::Primary, Slot::Secondary] {
            let incoming_phase = self
                .incoming
                .as_ref()
                .filter(|incoming| incoming.slot == slot)
                .map(|incoming| incoming.phase);
            let seek_current_position = slot == active
                || crossfading
                || handoff_in_progress
                || matches!(
                    incoming_phase,
                    Some(IncomingPhase::Seeking | IncomingPhase::Ready)
                );
            let seek_started = self.pipeline_for_slot_mut(slot).set_playback_rate(
                rate,
                seek_current_position,
                &settings,
            )?;
            if seek_started
                && let Some(incoming) = self
                    .incoming
                    .as_mut()
                    .filter(|incoming| incoming.slot == slot)
            {
                incoming.phase = IncomingPhase::Seeking;
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

    fn output_levels_at(&self, volume: f64, now: Instant) -> [f64; 2] {
        if let Some(crossfade) = lock_recover(&self.shared).crossfade.clone() {
            return crossfade.output_levels_at(volume, now);
        }

        let crossfade_target = self
            .incoming
            .as_ref()
            .and_then(|incoming| {
                matches!(incoming.item.transition, NextTransition::Crossfade { .. })
                    .then_some(incoming.slot)
            })
            .or_else(|| {
                self.pending_handoff
                    .as_ref()
                    .and_then(|pending| match pending {
                        PendingHandoff::Separate { incoming, .. }
                            if matches!(
                                incoming.item.transition,
                                NextTransition::Crossfade { .. }
                            ) =>
                        {
                            Some(incoming.slot)
                        }
                        _ => None,
                    })
            });
        let mut levels = [volume; 2];
        if let Some(slot) = crossfade_target {
            levels[slot.index()] = 0.0;
        }
        levels
    }

    fn set_pipeline_output_levels(&self, levels: [f64; 2], muted: bool) {
        self.primary
            .set_output_volume(levels[Slot::Primary.index()], muted);
        self.secondary
            .set_output_volume(levels[Slot::Secondary.index()], muted);
    }

    fn apply_output_gain_to_pipelines(&mut self, volume: f64, muted: bool) {
        self.set_pipeline_output_levels(self.output_levels_at(volume, Instant::now()), muted);
    }

    fn visualizer_enabled(&self) -> bool {
        lock_recover(&self.shared).visualizer_enabled
    }

    fn visualizer_tap(&self, slot: Slot, enabled: bool) -> Option<VisualizerTap> {
        if !enabled {
            return None;
        }
        let (pipeline_id, run) = (|| {
            let shared = lock_recover(&self.shared);
            if shared.active != slot {
                return None;
            }
            Some((shared.pipeline_id(slot)?, shared.current.as_ref()?.run))
        })()?;
        Some(self.visualizer.tap(slot, pipeline_id, run))
    }

    fn sync_visualizer_taps(&mut self, enabled: bool) {
        let primary_tap = self.visualizer_tap(Slot::Primary, enabled);
        let secondary_tap = self.visualizer_tap(Slot::Secondary, enabled);
        self.primary.set_visualizer_tap(primary_tap);
        self.secondary.set_visualizer_tap(secondary_tap);
    }

    fn set_visualizer_enabled(&mut self, enabled: bool) -> Result<(), String> {
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
        if self.is_active_slot(slot) {
            return true;
        }
        lock_recover(&self.shared)
            .crossfade
            .clone()
            .is_some_and(|crossfade| crossfade.from == slot || crossfade.to == slot)
    }

    fn run_for_slot(&self, slot: Slot) -> Option<RunId> {
        let shared = lock_recover(&self.shared);
        if let Some(crossfade) = shared.crossfade.as_ref()
            && crossfade.from == slot
        {
            return Some(crossfade.old_run);
        }
        (shared.active == slot)
            .then(|| shared.current.as_ref().map(|current| current.run))
            .flatten()
    }

    fn stop_after_playback_error(&mut self) {
        self.pending_seek = None;
        self.incoming = None;
        self.pending_handoff = None;
        self.stop_pipeline(Slot::Primary);
        self.stop_pipeline(Slot::Secondary);
        {
            let mut shared = lock_recover(&self.shared);
            shared.next = None;
            shared.gapless_pending = None;
            shared.about_to_finish_pending = false;
            shared.crossfade = None;
            shared.active = Slot::Primary;
        }
    }

    fn message_source_is_pipeline(&self, slot: Slot, message: &gst::Message) -> bool {
        self.pipeline_for_slot(slot)
            .message_source_is_pipeline(message)
    }

    fn shutdown(&mut self) {
        self.incoming = None;
        self.pending_handoff = None;
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
#[instrument(skip(receiver, events))]
fn run_gstreamer_thread(
    receiver: Receiver<BackendCommand>,
    events: Arc<Mutex<EventMailbox>>,
    ready: SyncSender<Result<(), String>>,
) {
    let startup_started_at = Instant::now();
    if let Err(error) = ensure_gstreamer_initialized() {
        let _ = ready.send(Err(format!("GStreamer init failed: {error}")));
        return;
    }

    let mut engine = GstEngine::new(Arc::clone(&events));
    if ready.send(Ok(())).is_err() {
        return;
    }
    info!(
        elapsed_ms = startup_started_at.elapsed().as_millis(),
        "GStreamer playback backend is ready"
    );

    loop {
        engine.poll_bus();
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(command) => engine.handle_command(command),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        engine.tick();
    }
    engine.shutdown();
}
pub(super) fn handle_about_to_finish(
    pipeline: &gst::Element,
    shared: &Arc<Mutex<SharedBackendState>>,
    loudness_tags: &SharedLoudnessTags,
    trust_invalid_certificate: &AtomicBool,
    slot: Slot,
    id: PipelineId,
) {
    if !about_to_finish_may_query(&lock_recover(shared), slot, id) {
        return;
    }
    let position_millis = pipeline
        .query_position::<gst::ClockTime>()
        .map(clock_millis)
        .unwrap_or_default();
    let action =
        about_to_finish_action_for_pipeline(&mut lock_recover(shared), slot, id, position_millis);

    match action {
        AboutToFinishAction::Preload(next) => {
            info!(
                next_run = %next.run,
                uri = %next.stream.redacted_uri(),
                "preloading gapless next stream"
            );
            apply_shared_loudness(loudness_tags, &next.stream.loudness);
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
    slot: Slot,
    id: PipelineId,
    position_millis: u64,
) -> AboutToFinishAction {
    if !shared.pipeline_is_current(slot, id) || position_millis == 0 {
        return AboutToFinishAction::Ignore;
    }
    about_to_finish_action(shared)
}

pub(super) fn about_to_finish_action(shared: &mut SharedBackendState) -> AboutToFinishAction {
    if shared.gapless_pending.is_some() {
        return AboutToFinishAction::Ignore;
    }

    let Some(next) = shared.next.as_ref() else {
        shared.about_to_finish_pending = true;
        shared.next_needed = shared.current.as_ref().map(|current| current.run);
        return AboutToFinishAction::Ignore;
    };

    if !gapless_preload_should_run(shared, next) {
        shared.about_to_finish_pending = false;
        return AboutToFinishAction::Ignore;
    }

    if next.stream.end_millis().is_some() || !gapless_preload_source_is_supported(next.stream.uri())
    {
        debug!(
            next_run = %next.run,
            uri = %next.stream.redacted_uri(),
            "skipping gapless preload for non-local stream"
        );
        shared.about_to_finish_pending = false;
        return AboutToFinishAction::Ignore;
    }

    let Some(next) = shared.next.take() else {
        shared.about_to_finish_pending = false;
        return AboutToFinishAction::Ignore;
    };
    shared.gapless_pending = Some(next.clone());
    shared.about_to_finish_pending = false;
    AboutToFinishAction::Preload(Box::new(next))
}

pub(super) fn cancel_gapless_pending(
    shared: &mut SharedBackendState,
) -> Option<(PreparedRun, PreparedNext)> {
    let pending = shared.gapless_pending.take()?;
    let current = shared.current.clone()?;
    if shared.next.is_none() {
        shared.next = Some(pending.clone());
    }
    shared.about_to_finish_pending = false;
    Some((current, pending))
}

pub(super) fn clear_prepared_next_state(shared: &mut SharedBackendState) -> PreparedNextClear {
    let gapless_current = shared.gapless_pending.take().and_then(|_| {
        shared
            .current
            .clone()
            .map(|current| (shared.active, current))
    });
    shared.next = None;
    shared.about_to_finish_pending = false;
    PreparedNextClear { gapless_current }
}

fn gapless_preload_should_run(shared: &SharedBackendState, next: &PreparedNext) -> bool {
    let Some(current) = shared.current.as_ref() else {
        return false;
    };
    next.transition == NextTransition::Gapless
        && next.stream.allows_preloading
        && current.stream.allows_preloading
        && !gapless_uses_separate_pipeline(&shared.settings, current, next)
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

fn gapless_uses_separate_pipeline(
    settings: &BackendAudioSettings,
    current: &PreparedRun,
    next: &PreparedNext,
) -> bool {
    current.stream.stream == next.stream.stream
        || (next.stream.window().is_some()
            && (!adjacent_window_can_reuse_pipeline(settings)
                || !streams_are_adjacent_windows(&current.stream, &next.stream)))
}

fn streams_are_adjacent_windows(current: &ResolvedStream, next: &ResolvedStream) -> bool {
    current
        .end_millis()
        .is_some_and(|boundary| current.uri() == next.uri() && next.start_millis() == boundary)
}

fn adjacent_window_can_reuse_pipeline(settings: &BackendAudioSettings) -> bool {
    settings.loudness_normalization == LoudnessNormalizationMode::Off
}

fn crossfade_start_remaining_millis(crossfade_millis: u64, playback_rate: f64) -> u64 {
    ((crossfade_millis as f64) * sanitize_playback_rate(playback_rate))
        .round()
        .clamp(0.0, u64::MAX as f64) as u64
}

pub(super) fn push_event(events: &Arc<Mutex<EventMailbox>>, event: BackendEvent) {
    lock_recover(events).push(event);
}
fn clock_millis(clock_time: gst::ClockTime) -> u64 {
    clock_time.mseconds()
}
fn seek_position_matches_target(target_millis: u64, millis: u64) -> bool {
    let lower = target_millis.saturating_sub(SEEK_POSITION_TOLERANCE_MILLIS);
    let upper = target_millis.saturating_add(SEEK_POSITION_TOLERANCE_MILLIS);
    (lower..=upper).contains(&millis)
}
fn pending_seek_for_session_restart(
    absolute_start_millis: u64,
    logical_position_millis: u64,
    logical_state: BackendState,
    target_state: gst::State,
    needs_preroll_seek: bool,
    now: Instant,
) -> Option<PendingSeek> {
    if needs_preroll_seek {
        return Some(PendingSeek::startup_with_resume(
            absolute_start_millis,
            logical_state,
            now,
            target_state == gst::State::Playing,
        ));
    }
    if target_state == gst::State::Playing {
        return Some(PendingSeek::track_start(now));
    }
    Some(PendingSeek::interactive(
        logical_position_millis,
        logical_state,
        now,
    ))
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
        next: PreparedNext,
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
            engine.incoming = Some(IncomingPipeline {
                id: INCOMING_PIPELINE,
                slot: Slot::Secondary,
                item: next.clone(),
                phase: IncomingPhase::Ready,
            });
            engine.desired_playing = true;
            engine.state = BackendState::Playing;
            Self {
                engine,
                events,
                old_run,
                next_run,
                next,
            }
        }

        fn adjacent() -> Self {
            let events = Arc::new(Mutex::new(EventMailbox::default()));
            let mut engine = GstEngine::new(Arc::clone(&events));
            let old_run = RunId::new(1);
            let next_run = RunId::new(2);
            let next = PreparedNext::new(
                next_run,
                ResolvedStream::new("file:///music/cue.flac").with_window(30_000, 60_000),
                NextTransition::Gapless,
            );
            {
                let mut shared = lock_recover(&engine.shared);
                shared.current = Some(PreparedRun {
                    run: old_run,
                    stream: ResolvedStream::new("file:///music/cue.flac")
                        .with_window(0, 30_000)
                        .into(),
                });
                shared.next = Some(next.clone());
                shared.active = Slot::Primary;
                shared.set_pipeline_id(Slot::Primary, Some(ACTIVE_PIPELINE));
            }
            engine.desired_playing = true;
            Self {
                engine,
                events,
                old_run,
                next_run,
                next,
            }
        }

        fn begin_separate(&mut self) {
            assert!(self.engine.begin_incoming_handoff(
                Slot::Secondary,
                INCOMING_PIPELINE,
                gst::StateChangeSuccess::Async,
            ));
        }

        fn begin_adjacent(&mut self) {
            self.begin_adjacent_after(gst::Seqnum::next());
        }

        fn begin_adjacent_after(&mut self, confirmation_after: gst::Seqnum) {
            assert!(self.engine.begin_adjacent_window_handoff(
                Slot::Primary,
                ACTIVE_PIPELINE,
                self.old_run,
                self.next.clone(),
                confirmation_after,
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
    fn normalized_cue_tracks_use_fresh_gain_state() {
        let mut settings = BackendAudioSettings::default();
        assert!(adjacent_window_can_reuse_pipeline(&settings));

        settings.loudness_normalization = LoudnessNormalizationMode::Track;
        assert!(!adjacent_window_can_reuse_pipeline(&settings));

        settings.loudness_normalization = LoudnessNormalizationMode::Album;
        assert!(!adjacent_window_can_reuse_pipeline(&settings));
    }

    #[test]
    fn repeated_stream_uses_the_separate_gapless_pipeline() {
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

        assert!(gapless_uses_separate_pipeline(
            &shared.settings,
            shared.current.as_ref().expect("current stream"),
            &next,
        ));
        let distinct = PreparedNext::new(
            RunId::new(12),
            ResolvedStream::new("file:///music/distinct.flac"),
            NextTransition::Gapless,
        );
        assert!(!gapless_uses_separate_pipeline(
            &shared.settings,
            shared.current.as_ref().expect("current stream"),
            &distinct,
        ));
        assert_eq!(
            about_to_finish_action_for_pipeline(&mut shared, Slot::Primary, pipeline, 1),
            AboutToFinishAction::Ignore
        );
        assert_eq!(shared.next, Some(next));
        assert!(shared.gapless_pending.is_none());
    }

    #[test]
    fn fresh_playback_applies_effective_gain_before_processing_state_changes() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let directory = tempfile::tempdir().expect("playback fixture directory");
        let path = directory.path().join("initial-volume.wav");
        write_silent_wave(&path);
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
                audio_output: Some("fakesink".to_string()),
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

            let (gain, pipeline_muted) = engine
                .primary
                .output_volume_state()
                .expect("active primary pipeline");
            assert!((gain - expected_gain).abs() < 1e-12);
            assert_eq!(pipeline_muted, muted);
            engine.shutdown();
        }
    }

    #[test]
    fn startup_seek_stays_silent_until_position_confirmation() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let directory = tempfile::tempdir().expect("playback fixture directory");
        let path = directory.path().join("startup-seek.wav");
        write_long_silent_wave(&path);
        let uri = gst::glib::filename_to_uri(&path, None).expect("playback fixture URI");
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(events);
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
        assert_eq!(engine.state, BackendState::Buffering);
        assert!(
            engine
                .pending_seek
                .as_ref()
                .is_some_and(|pending| pending.resume_after_seek)
        );
        engine.handle_state_changed(BackendState::Playing);
        assert_eq!(engine.state, BackendState::Buffering);

        engine.push_position(5_000);
        assert_eq!(engine.state, BackendState::Playing);
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
    fn repeated_seek_guard_stays_active_until_input_quiets() {
        let started_at = Instant::now();
        let mut guard = RepeatedSeekGuard::new(1_000, started_at);
        let first_deadline = guard.quiet_until;

        assert!(guard.suppresses(1_000, started_at + Duration::from_millis(118)));
        assert!(guard.quiet_until > first_deadline);
        assert!(guard.suppresses(1_000, first_deadline));
        assert!(!guard.suppresses(1_500, first_deadline));
        assert!(!guard.suppresses(1_000, guard.quiet_until));
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
            )
            .expect("start paused output");
        {
            let mut shared = lock_recover(&engine.shared);
            shared.settings = settings.clone();
            shared.current = Some(current);
            shared.set_pipeline_id(Slot::Primary, Some(pipeline_id));
        }
        engine.desired_playing = true;
        engine.state = BackendState::Playing;

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
            )
            .expect("start paused output");
        {
            let mut shared = lock_recover(&engine.shared);
            shared.settings = settings.clone();
            shared.current = Some(current);
            shared.set_pipeline_id(Slot::Primary, Some(pipeline_id));
        }
        engine.desired_playing = true;
        engine.state = BackendState::Playing;

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
    fn preserve_pitch_change_restarts_the_current_pipeline() {
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
            )
            .expect("start paused output");
        {
            let mut shared = lock_recover(&engine.shared);
            shared.settings = settings.clone();
            shared.current = Some(current);
            shared.set_pipeline_id(Slot::Primary, Some(pipeline_id));
        }
        engine.state = BackendState::Paused;

        let mut changed = settings;
        changed.preserve_pitch = false;
        engine.handle_command(BackendCommand::ConfigureAudio(changed));

        let shared = lock_recover(&engine.shared);
        assert!(!shared.settings.preserve_pitch);
        assert_ne!(shared.pipeline_id(Slot::Primary), Some(pipeline_id));
        assert!(shared.pipeline_id(Slot::Primary).is_some());
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
            assert!(fixture.engine.incoming.is_none());
            assert!(matches!(
                fixture.engine.pending_handoff.as_ref(),
                Some(PendingHandoff::Separate {
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
        assert!(fixture.engine.incoming.is_none());
        assert!(fixture.engine.pending_handoff.is_none());
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
    fn crossfade_clock_starts_only_after_the_incoming_pipeline_is_playing() {
        let mut fixture = HandoffFixture::separate(NextTransition::Crossfade {
            duration_millis: 5_000,
        });
        fixture.begin_separate();
        assert!(lock_recover(&fixture.engine.shared).crossfade.is_none());
        assert!(fixture.drain().is_empty());

        assert!(
            fixture
                .engine
                .confirm_handoff(Slot::Secondary, INCOMING_PIPELINE)
        );

        let shared = lock_recover(&fixture.engine.shared);
        let crossfade = shared.crossfade.as_ref().expect("confirmed crossfade");
        assert_eq!(crossfade.from, Slot::Primary);
        assert_eq!(crossfade.to, Slot::Secondary);
        assert_eq!(crossfade.old_run, fixture.old_run);
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
    fn crossfade_output_roles_keep_the_incoming_pipeline_silent_through_commit() {
        let mut fixture = HandoffFixture::separate(NextTransition::Crossfade {
            duration_millis: 5_000,
        });
        let volume = 0.8;

        assert_eq!(
            fixture.engine.output_levels_at(volume, Instant::now()),
            [volume, 0.0]
        );

        fixture.begin_separate();
        assert_eq!(
            fixture.engine.output_levels_at(volume, Instant::now()),
            [volume, 0.0]
        );

        assert!(
            fixture
                .engine
                .confirm_handoff(Slot::Secondary, INCOMING_PIPELINE)
        );
        let crossfade = lock_recover(&fixture.engine.shared)
            .crossfade
            .clone()
            .expect("committed crossfade");
        assert_eq!(
            crossfade.output_levels_at(volume, crossfade.started_at),
            [volume, 0.0]
        );

        let reversed = CrossfadeState {
            from: Slot::Secondary,
            to: Slot::Primary,
            old_run: fixture.old_run,
            started_at: Instant::now(),
            duration: Duration::from_secs(5),
        };
        assert_eq!(
            reversed.output_levels_at(volume, reversed.started_at),
            [0.0, volume]
        );
    }

    #[test]
    fn failed_asynchronous_gapless_activation_falls_back_after_the_old_run_ends() {
        let mut fixture = HandoffFixture::separate(NextTransition::Gapless);
        fixture.begin_separate();

        fixture.engine.fail_handoff(
            Slot::Secondary,
            INCOMING_PIPELINE,
            "incoming activation failed".to_string(),
        );

        let shared = lock_recover(&fixture.engine.shared);
        assert_eq!(
            shared.current.as_ref().map(|item| item.run),
            Some(fixture.old_run)
        );
        assert_eq!(shared.next, Some(fixture.next.clone()));
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
    fn accepted_adjacent_window_seek_waits_for_async_done() {
        let mut fixture = HandoffFixture::adjacent();
        fixture.begin_adjacent();

        let shared = lock_recover(&fixture.engine.shared);
        assert_eq!(
            shared.current.as_ref().map(|item| item.run),
            Some(fixture.old_run)
        );
        assert_eq!(shared.next, Some(fixture.next.clone()));
        drop(shared);
        assert!(matches!(
            fixture.engine.pending_handoff.as_ref(),
            Some(PendingHandoff::AdjacentWindow {
                slot: Slot::Primary,
                id: ACTIVE_PIPELINE,
                old_run,
                item,
                ..
            }) if *old_run == fixture.old_run && item.run == fixture.next_run
        ));
        assert!(fixture.drain().is_empty());
    }

    #[test]
    fn adjacent_window_handoff_stays_behind_seek_and_pipeline_identity_boundaries() {
        let mut fixture = HandoffFixture::adjacent();
        fixture.engine.pending_seek = Some(PendingSeek::interactive(
            10_000,
            BackendState::Playing,
            Instant::now(),
        ));

        assert!(!fixture.engine.begin_adjacent_window_handoff(
            Slot::Primary,
            ACTIVE_PIPELINE,
            fixture.old_run,
            fixture.next.clone(),
            gst::Seqnum::next(),
        ));
        assert!(fixture.engine.pending_handoff.is_none());

        fixture.engine.pending_seek = None;
        fixture.begin_adjacent();
        assert!(
            !fixture
                .engine
                .confirm_handoff(Slot::Primary, PipelineId(ACTIVE_PIPELINE.0 - 1))
        );
        assert!(fixture.engine.pending_handoff.is_some());
        assert!(
            fixture
                .engine
                .confirm_handoff(Slot::Primary, ACTIVE_PIPELINE)
        );
        assert_eq!(fixture.current_run(), Some(fixture.next_run));
    }

    #[test]
    fn adjacent_window_handoff_rejects_async_done_from_before_its_seek() {
        let stale = gst::Seqnum::next();
        let confirmation_after = gst::Seqnum::next();
        let current = gst::Seqnum::next();
        let mut fixture = HandoffFixture::adjacent();
        fixture.begin_adjacent_after(confirmation_after);

        assert!(!fixture.engine.pending_handoff_accepts_async_done(
            Slot::Primary,
            ACTIVE_PIPELINE,
            stale,
        ));
        assert!(fixture.engine.pending_handoff_accepts_async_done(
            Slot::Primary,
            ACTIVE_PIPELINE,
            current,
        ));
        assert!(fixture.engine.pending_handoff.is_some());
        assert_eq!(fixture.current_run(), Some(fixture.old_run));
    }

    #[test]
    fn audio_reconfiguration_cancels_an_unconfirmed_adjacent_window_handoff() {
        let mut fixture = HandoffFixture::adjacent();
        fixture.begin_adjacent();

        fixture
            .engine
            .handle_command(BackendCommand::ConfigureAudio(
                BackendAudioSettings::default(),
            ));

        assert!(fixture.engine.pending_handoff.is_none());
        assert_eq!(
            lock_recover(&fixture.engine.shared).pipeline_id(Slot::Primary),
            None
        );
        assert!(fixture.drain().iter().any(|event| matches!(
            event,
            BackendEvent::Ended { run } if *run == fixture.old_run
        )));
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

        assert!(fixture.engine.pending_handoff.is_some());
        let (gain, muted) = fixture.engine.output_gain_state();
        assert!((gain - VolumeScale::Perceptual.gain(0.5)).abs() < f64::EPSILON);
        assert!(!muted);
        assert_eq!(
            fixture.engine.output_levels_at(gain, Instant::now()),
            [gain, 0.0]
        );
        assert!(
            !fixture
                .drain()
                .iter()
                .any(|event| matches!(event, BackendEvent::Ended { .. }))
        );
    }

    #[test]
    fn matching_async_done_commits_adjacent_window_handoff_once() {
        let mut fixture = HandoffFixture::adjacent();
        fixture.begin_adjacent();

        assert!(
            fixture
                .engine
                .confirm_handoff(Slot::Primary, ACTIVE_PIPELINE)
        );
        assert!(
            !fixture
                .engine
                .confirm_handoff(Slot::Primary, ACTIVE_PIPELINE)
        );

        let shared = lock_recover(&fixture.engine.shared);
        assert_eq!(
            shared.current.as_ref().map(|item| item.run),
            Some(fixture.next_run)
        );
        assert!(shared.next.is_none());
        drop(shared);
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
    fn failed_adjacent_window_seek_uses_the_cold_fallback() {
        let mut fixture = HandoffFixture::adjacent();
        fixture.begin_adjacent();

        assert!(fixture.engine.fail_handoff(
            Slot::Primary,
            ACTIVE_PIPELINE,
            "seek failed".to_string(),
        ));

        let shared = lock_recover(&fixture.engine.shared);
        assert_eq!(
            shared.current.as_ref().map(|item| item.run),
            Some(fixture.old_run)
        );
        assert_eq!(shared.next, Some(fixture.next.clone()));
        drop(shared);
        assert!(fixture.engine.pending_handoff.is_none());
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
    fn pause_cancels_an_unconfirmed_crossfade_and_late_playing_cannot_commit_it() {
        let mut fixture = HandoffFixture::separate(NextTransition::Crossfade {
            duration_millis: 5_000,
        });
        fixture.begin_separate();

        fixture.engine.handle_command(BackendCommand::Pause {
            run: fixture.old_run,
        });

        assert!(fixture.engine.incoming.is_none());
        assert!(fixture.engine.pending_handoff.is_none());
        assert!(
            !fixture
                .engine
                .confirm_handoff(Slot::Secondary, INCOMING_PIPELINE)
        );
        let shared = lock_recover(&fixture.engine.shared);
        assert_eq!(
            shared.current.as_ref().map(|item| item.run),
            Some(fixture.old_run)
        );
        assert!(shared.crossfade.is_none());
        drop(shared);
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

            assert!(crossfade.engine.incoming.is_some());
            assert!(crossfade.engine.pending_handoff.is_none());
            assert!(lock_recover(&crossfade.engine.shared).crossfade.is_none());

            let mut gapless = HandoffFixture::separate(NextTransition::Gapless);
            gapless.engine.handle_command(BackendCommand::Pause {
                run: gapless.old_run,
            });
            gapless.engine.handle_state_changed(late_state);
            gapless.engine.handle_end(Slot::Primary, false);

            assert_eq!(gapless.current_run(), Some(gapless.old_run));
            assert!(gapless.engine.pending_handoff.is_none());
            assert!(gapless.drain().iter().all(|event| !matches!(
                event,
                BackendEvent::Transitioned { .. } | BackendEvent::NextPreparationFailed { .. }
            )));

            let mut adjacent = HandoffFixture::adjacent();
            adjacent.engine.handle_command(BackendCommand::Pause {
                run: adjacent.old_run,
            });
            adjacent.engine.handle_state_changed(late_state);
            adjacent.engine.handle_end(Slot::Primary, true);

            assert_eq!(adjacent.current_run(), Some(adjacent.old_run));
            assert!(adjacent.engine.pending_handoff.is_none());
            assert!(adjacent.drain().iter().all(|event| !matches!(
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
            )
            .expect("start inert GStreamer session");
        {
            let mut shared = lock_recover(&engine.shared);
            shared.settings = settings.clone();
            shared.current = Some(current);
            shared.gapless_pending = Some(next);
            shared.active = Slot::Primary;
            shared.set_pipeline_id(Slot::Primary, Some(pipeline_id));
        }
        engine.desired_playing = true;
        engine.state = BackendState::Playing;

        engine.handle_command(BackendCommand::Pause { run: old_run });
        let original_fade = engine.status_fade.expect("pause fade");
        engine.handle_stream_start();
        assert_eq!(engine.timing_run_id(), Some(new_run));

        engine.handle_command(BackendCommand::Pause { run: new_run });

        let preserved_fade = engine.status_fade.expect("preserved pause fade");
        assert_eq!(preserved_fade.started_at, original_fade.started_at);
        assert_eq!(preserved_fade.start_volume, original_fade.start_volume);
        assert_eq!(preserved_fade.end_volume, original_fade.end_volume);
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
            .position(|event| matches!(event, BackendEvent::Error { run, .. } if *run == new_run))
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
        let pending_seek = engine.pending_seek.as_ref().expect("pending seek");
        assert_eq!(pending_seek.logical_state, BackendState::Paused);
        assert!(!pending_seek.resume_after_seek);
        engine.shutdown();
    }

    fn write_silent_wave(path: &std::path::Path) {
        write_silent_mono_wave(path, 800);
    }

    fn write_long_silent_wave(path: &std::path::Path) {
        write_silent_mono_wave(path, 80_000);
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
        assert!(separate.engine.pending_handoff.is_none());
        assert!(
            !separate
                .engine
                .confirm_handoff(Slot::Secondary, INCOMING_PIPELINE)
        );
        assert_eq!(separate.current_run(), Some(separate.old_run));
        assert!(separate.drain().is_empty());

        let mut adjacent = HandoffFixture::adjacent();
        adjacent.begin_adjacent();

        let restored = adjacent
            .engine
            .cancel_handoff_for_seek()
            .expect("adjacent window handoff restores the current run");

        assert_eq!(restored.run, adjacent.old_run);
        assert!(adjacent.engine.pending_handoff.is_none());
        assert!(
            !adjacent
                .engine
                .confirm_handoff(Slot::Primary, ACTIVE_PIPELINE)
        );
        assert_eq!(adjacent.current_run(), Some(adjacent.old_run));
        assert!(adjacent.drain().is_empty());
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
        engine.incoming = Some(IncomingPipeline {
            id: incoming_id,
            slot: Slot::Secondary,
            item: next.clone(),
            phase: IncomingPhase::Prerolling,
        });

        engine.fail_incoming(
            Slot::Secondary,
            incoming_id,
            "next stream failed".to_string(),
        );

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
    fn failed_uri_preload_ends_the_current_run_without_replaying_it() {
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        let current_run = RunId::new(1);
        let current_pipeline = PipelineId(7);
        let next = PreparedNext::new(
            RunId::new(2),
            ResolvedStream::new("https://music.example/next.flac"),
            NextTransition::Gapless,
        );
        {
            let mut shared = lock_recover(&engine.shared);
            shared.current = Some(PreparedRun {
                run: current_run,
                stream: ResolvedStream::new("https://music.example/current.flac").into(),
            });
            shared.gapless_pending = Some(next.clone());
            shared.set_pipeline_id(Slot::Primary, Some(current_pipeline));
        }
        engine.state = BackendState::Playing;

        assert!(engine.handle_gapless_preload_error(Slot::Primary, "next stream failed"));

        let shared = lock_recover(&engine.shared);
        assert_eq!(
            shared.current.as_ref().map(|item| item.run),
            Some(current_run)
        );
        assert_eq!(shared.next, Some(next));
        assert!(shared.gapless_pending.is_none());
        assert_eq!(shared.pipeline_id(Slot::Primary), None);
        drop(shared);
        assert!(matches!(
            lock_recover(&events).drain().as_slice(),
            [
                BackendEvent::NextPreparationFailed {
                    current_run: failed_current,
                    next_run: failed_next,
                    ..
                },
                BackendEvent::Ended { run: ended_run }
            ] if *failed_current == current_run
                && *failed_next == RunId::new(2)
                && *ended_run == current_run
        ));
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
            about_to_finish_action_for_pipeline(&mut shared, Slot::Primary, old, 1),
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
            about_to_finish_action_for_pipeline(&mut shared, Slot::Primary, pipeline, 0),
            AboutToFinishAction::Ignore
        );
        assert_eq!(shared.next, Some(next.clone()));
        assert!(shared.gapless_pending.is_none());

        assert!(matches!(
            about_to_finish_action_for_pipeline(&mut shared, Slot::Primary, pipeline, 1),
            AboutToFinishAction::Preload(preloaded) if *preloaded == next
        ));
    }

    #[test]
    fn module_music_never_preloads_the_next_stream() {
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
            about_to_finish_action_for_pipeline(&mut shared, Slot::Primary, pipeline, 1),
            AboutToFinishAction::Ignore
        );
        assert_eq!(shared.next, Some(next));
        assert!(shared.gapless_pending.is_none());
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

    #[test]
    fn gapless_preload_cannot_relabel_the_next_duration_as_current() {
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        let current_run = RunId::new(1);
        let next_run = RunId::new(2);
        {
            let mut shared = lock_recover(&engine.shared);
            shared.current = Some(PreparedRun {
                run: current_run,
                stream: ResolvedStream::new("file:///music/current.flac").into(),
            });
            shared.gapless_pending = Some(PreparedNext::new(
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
    }
}
