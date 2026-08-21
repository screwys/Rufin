//! Playback composition for the selected source.
//!
//! The `playback` crate owns queue, transport, run, and backend ordering. This
//! module consumes its one ordered output stream and performs the Rufin-owned
//! crossings: durable Library writes, stream resolution, accepted activity,
//! source reporting, AutoDJ, settings, and UI publication. It never mirrors
//! the queue or current-media state.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_channel::Sender as EventSender;
use library::{
    AcceptedPlay, AcceptedSkip, Libraries, Library, PlayableFile, PlaybackLoad,
    PlaybackOccurrenceId, PlaybackProgressUpdate, PlaybackStateUpdate, ResolvedStream, SourceId,
    StreamRequest, Track, TrackId,
};
use playback::{
    LoadedPlayRequest, OccurrenceId, Playback, PlaybackBackend, PlaybackProjection, PlaybackUpdate,
    PreparedStream, QueueCommandPort, QueuePage, QueuePageQuery, QueueReorderRequest,
    RadioCommandPort, RadioPlayRequest, RandomPlayRequest, RepeatMode, RunId, SessionCommand,
    SessionEffect, SourceReportFact, SourceReportPhase, SourceSessionEpoch, TransportCommandPort,
};
use scrobbling::Scrobbler;
use sources::{NativeSourceResult, Source};
use tracing::{debug, warn};
use ui::runtime::PlaybackPublication;

use crate::loudness::LoudnessAnalysisOwner;
use crate::settings::SettingsFile;
use crate::source::{
    ActiveSource, SelectedSourceState, SourceAcceptanceSender, WeakActiveSource,
    source_access_unavailable,
};
use crate::waveform::{WaveformMedia, WaveformOwner};
use lyrics::{LyricsContext, LyricsService};

const MAX_PENDING_QUEUE_INTENTS: usize = 32;

#[derive(Clone)]
struct ActivePlayback {
    instance: u64,
    source_id: SourceId,
    source_session_epoch: SourceSessionEpoch,
    selected: Arc<ActiveSource>,
    playback: Playback,
}

impl ActivePlayback {
    fn selected(&self) -> Option<Arc<SelectedSourceState>> {
        self.selected.resolve()
    }

    fn weak_selected(&self) -> WeakActiveSource {
        self.selected.downgrade()
    }
}

enum QueueIntentWork {
    Loaded {
        playback: Playback,
        request: LoadedPlayRequest,
    },
    Radio {
        source_session_epoch: SourceSessionEpoch,
        selected: WeakActiveSource,
        playback: Playback,
        request: RadioPlayRequest,
        #[cfg(test)]
        started: Option<SyncSender<Option<tokio::task::JoinHandle<()>>>>,
    },
    Random {
        source_session_epoch: SourceSessionEpoch,
        selected: WeakActiveSource,
        playback: Playback,
        request: RandomPlayRequest,
        #[cfg(test)]
        started: Option<SyncSender<Option<tokio::task::JoinHandle<()>>>>,
    },
    Fence(SyncSender<()>),
    Shutdown,
}

/// One FIFO edge for queue intents that require preparation before Playback.
///
/// Playback remains the only reservation and queue owner. Loaded selections
/// finish their bounded preparation here; provider work leaves only after its
/// reservation, so a slow server cannot hold a newer click.
struct QueueIntentWorker {
    sender: SyncSender<QueueIntentWork>,
    selected_epoch: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}

impl QueueIntentWorker {
    fn new(runtime: tokio::runtime::Handle) -> Self {
        let (sender, receiver) = mpsc::sync_channel(MAX_PENDING_QUEUE_INTENTS);
        let selected_epoch = Arc::new(AtomicU64::new(0));
        let worker_epoch = Arc::clone(&selected_epoch);
        let worker = thread::Builder::new()
            .name("rufin-queue-intent".to_string())
            .spawn(move || {
                while let Ok(work) = receiver.recv() {
                    match work {
                        QueueIntentWork::Loaded { playback, request } => {
                            if worker_epoch.load(Ordering::Acquire)
                                != request.source_session_epoch.get()
                            {
                                continue;
                            }
                            play_loaded_selection(&playback, request);
                        }
                        QueueIntentWork::Radio {
                            source_session_epoch,
                            selected,
                            playback,
                            request,
                            #[cfg(test)]
                            started,
                        } => {
                            if worker_epoch.load(Ordering::Acquire) != source_session_epoch.get() {
                                continue;
                            }
                            let task = crate::radio::play_radio(
                                runtime.clone(),
                                selected,
                                playback,
                                request,
                            );
                            #[cfg(not(test))]
                            drop(task);
                            #[cfg(test)]
                            if let Some(started) = started {
                                let _ = started.send(task);
                            }
                        }
                        QueueIntentWork::Random {
                            source_session_epoch,
                            selected,
                            playback,
                            request,
                            #[cfg(test)]
                            started,
                        } => {
                            if worker_epoch.load(Ordering::Acquire) != source_session_epoch.get() {
                                continue;
                            }
                            let task = crate::radio::play_random(
                                runtime.clone(),
                                selected,
                                playback,
                                request,
                            );
                            #[cfg(not(test))]
                            drop(task);
                            #[cfg(test)]
                            if let Some(started) = started {
                                let _ = started.send(task);
                            }
                        }
                        QueueIntentWork::Fence(crossed) => {
                            let _ = crossed.send(());
                        }
                        QueueIntentWork::Shutdown => break,
                    }
                }
            })
            .expect("could not start queue intent worker");
        Self {
            sender,
            selected_epoch,
            worker: Some(worker),
        }
    }

    fn select(&self, source_session_epoch: SourceSessionEpoch) {
        self.selected_epoch
            .store(source_session_epoch.get(), Ordering::Release);
    }

    fn retire(&self) {
        self.selected_epoch.store(0, Ordering::Release);
        self.drain();
    }

    fn submit_loaded(&self, playback: Playback, request: LoadedPlayRequest) {
        if self
            .sender
            .send(QueueIntentWork::Loaded { playback, request })
            .is_err()
        {
            warn!("queue intent worker stopped");
        }
    }

    fn submit_radio(
        &self,
        source_session_epoch: SourceSessionEpoch,
        selected: WeakActiveSource,
        playback: Playback,
        request: RadioPlayRequest,
    ) {
        if self
            .sender
            .send(QueueIntentWork::Radio {
                source_session_epoch,
                selected,
                playback,
                request,
                #[cfg(test)]
                started: None,
            })
            .is_err()
        {
            warn!("queue intent worker stopped");
        }
    }

    fn submit_random(
        &self,
        source_session_epoch: SourceSessionEpoch,
        selected: WeakActiveSource,
        playback: Playback,
        request: RandomPlayRequest,
    ) {
        if self
            .sender
            .send(QueueIntentWork::Random {
                source_session_epoch,
                selected,
                playback,
                request,
                #[cfg(test)]
                started: None,
            })
            .is_err()
        {
            warn!("queue intent worker stopped");
        }
    }

    fn drain(&self) {
        let (fence, crossed) = mpsc::sync_channel(0);
        if self.sender.send(QueueIntentWork::Fence(fence)).is_ok() {
            let _ = crossed.recv();
        }
    }

    #[cfg(test)]
    fn thread_id(&self) -> thread::ThreadId {
        self.worker
            .as_ref()
            .expect("queue intent worker must be running")
            .thread()
            .id()
    }

    #[cfg(test)]
    fn hold(&self) -> mpsc::Receiver<()> {
        let (fence, crossed) = mpsc::sync_channel(0);
        self.sender
            .send(QueueIntentWork::Fence(fence))
            .expect("queue intent worker must be running");
        crossed
    }

    #[cfg(test)]
    fn submit_random_observed(
        &self,
        source_session_epoch: SourceSessionEpoch,
        selected: WeakActiveSource,
        playback: Playback,
        request: RandomPlayRequest,
    ) -> mpsc::Receiver<Option<tokio::task::JoinHandle<()>>> {
        let (started, task) = mpsc::sync_channel(0);
        self.sender
            .send(QueueIntentWork::Random {
                source_session_epoch,
                selected,
                playback,
                request,
                started: Some(started),
            })
            .expect("queue intent worker must be running");
        task
    }
}

impl Drop for QueueIntentWorker {
    fn drop(&mut self) {
        self.drain();
        let _ = self.sender.send(QueueIntentWork::Shutdown);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            warn!("queue intent worker stopped unexpectedly");
        }
    }
}

fn play_loaded_selection(playback: &Playback, request: LoadedPlayRequest) {
    let request = match request.prepare() {
        Ok(Some(request)) => request,
        Ok(None) => return,
        Err(error) => {
            warn!(%error, "could not prepare loaded Play selection");
            return;
        }
    };
    let placement = request.placement();
    match playback.admit_loaded(&request) {
        Ok(None) => {}
        Ok(Some(reservation)) => match request.materialize_batch(random_u64()) {
            Ok((batch, placement)) => {
                if let Err(error) = playback.complete_materialization(
                    reservation.id,
                    reservation.source_id,
                    batch,
                    placement,
                ) {
                    warn!(%error, "could not complete loaded Play selection");
                }
            }
            Err(error) => {
                warn!(%error, "could not materialize loaded Play selection");
                cancel_loaded_materialization(playback, reservation, placement);
            }
        },
        Err(error) => warn!(%error, "could not admit loaded Play selection"),
    }
}

fn cancel_loaded_materialization(
    playback: &Playback,
    reservation: playback::MaterializationReservation,
    placement: playback::Placement,
) {
    if let Err(error) =
        playback.cancel_materialization(reservation.id, reservation.source_id, placement)
    {
        warn!(%error, "could not close loaded Play materialization");
    }
}

pub(crate) struct PreparedTrackRefresh {
    active: ActivePlayback,
    track_ids: Vec<TrackId>,
}

/// A target Playback session whose fallible construction finished before a
/// selected-source cutover.
///
/// Dropping an unused preparation shuts its workers down. Installation only
/// publishes the already-started session after the previous one has stopped.
pub(crate) struct PreparedPlayback {
    active: Option<ActivePlayback>,
    projection: Option<PlaybackProjection>,
    activated: Arc<AtomicBool>,
}

impl Drop for PreparedPlayback {
    fn drop(&mut self) {
        if let Some(active) = self.active.take()
            && let Err(error) = active.playback.shutdown()
        {
            warn!(%error, "could not discard prepared Playback");
        }
    }
}

/// Proof that the previous Playback left the active slot and its persistence
/// work finished before a prepared target is installed.
pub(crate) struct PlaybackCutover;

const PERSISTENCE_CAPACITY: usize = 64;

enum PersistenceWork {
    Checkpoint(playback::PlaybackCheckpointRevision),
    State(PlaybackStateUpdate),
    Progress(PlaybackProgressUpdate),
    Activity {
        loaded: Arc<Library>,
        source_id: SourceId,
        source_session_epoch: SourceSessionEpoch,
        outcome: playback::ListeningOutcome,
    },
    CompletedScrobble(playback::CompletedScrobble),
    OutputState {
        volume: f64,
        muted: bool,
        audio_output: Option<String>,
    },
    Fence(SyncSender<()>),
}

fn apply_checkpoint_revision(
    library: &Libraries,
    revision: playback::PlaybackCheckpointRevision,
) -> library::LibraryResult<library::PlaybackWriteOutcome> {
    let full_fallback = revision.clone();
    match revision.materialize_checkpoint() {
        playback::PlaybackCheckpointMaterialization::Full(checkpoint) => {
            library.replace_playback(checkpoint)
        }
        playback::PlaybackCheckpointMaterialization::Traversal(traversal) => {
            match library.replace_playback_traversal(traversal)? {
                library::PlaybackWriteOutcome::Applied => {
                    Ok(library::PlaybackWriteOutcome::Applied)
                }
                library::PlaybackWriteOutcome::Stale => {
                    library.replace_playback(full_fallback.materialize_full_checkpoint())
                }
            }
        }
    }
}

struct PersistenceTarget {
    library: Libraries,
    settings: SettingsFile,
    acceptance: SourceAcceptanceSender,
    scrobbler: Arc<Scrobbler>,
}

impl PersistenceTarget {
    fn apply(&self, work: PersistenceWork) {
        match work {
            PersistenceWork::Checkpoint(revision) => {
                if let Err(error) = apply_checkpoint_revision(&self.library, revision) {
                    warn!(%error, "could not save Playback state");
                }
            }
            PersistenceWork::State(state) => {
                if let Err(error) = self.library.update_playback_state(state) {
                    warn!(%error, "could not save Playback state");
                }
            }
            PersistenceWork::Progress(progress) => {
                if let Err(error) = self.library.update_playback_progress(progress) {
                    warn!(%error, "could not save Playback state");
                }
            }
            PersistenceWork::OutputState {
                volume,
                muted,
                audio_output,
            } => {
                if let Err(error) = self.settings.update(|settings| {
                    settings.ui.playback.volume = volume;
                    settings.ui.playback.muted = muted;
                    settings.ui.playback.audio_output = audio_output;
                    Ok(())
                }) {
                    warn!(%error, "could not save Playback output settings");
                }
            }
            PersistenceWork::Activity {
                loaded,
                source_id,
                source_session_epoch,
                outcome,
            } => self.apply_activity(loaded, source_id, source_session_epoch, outcome),
            PersistenceWork::CompletedScrobble(completed) => {
                if let Err(error) = self.scrobbler.completed_play(&completed) {
                    warn!(%error, "could not save external scrobbling work");
                }
            }
            PersistenceWork::Fence(_) => {
                unreachable!("persistence control work is handled by the worker")
            }
        }
    }

    fn apply_activity(
        &self,
        loaded: Arc<Library>,
        source_id: SourceId,
        source_session_epoch: SourceSessionEpoch,
        outcome: playback::ListeningOutcome,
    ) {
        if outcome.qualified_plays > 0
            && let Some(played_at) = outcome.last_played_at_unix_seconds
        {
            match loaded.record_play(AcceptedPlay {
                play_id: outcome.play_id.clone(),
                track_id: outcome.track_id.clone(),
                played_at,
                month: outcome.local_period.clone(),
            }) {
                Ok(Some(update)) => self.acceptance.publish_activity(
                    source_id.clone(),
                    source_session_epoch,
                    update,
                ),
                Ok(None) => {}
                Err(error) => warn!(%error, "could not record accepted play"),
            }
        }
        if outcome.skips > 0 {
            match loaded.record_skip(AcceptedSkip {
                track_id: outcome.track_id,
            }) {
                Ok(update) => {
                    self.acceptance
                        .publish_activity(source_id, source_session_epoch, update)
                }
                Err(error) => warn!(%error, "could not record accepted skip"),
            }
        }
    }
}

enum PersistenceToken {
    Checkpoint(SourceId),
    Work(PersistenceWork),
    Shutdown,
}

struct PlaybackPersistence {
    sender: Mutex<SyncSender<PersistenceToken>>,
    checkpoints: Arc<Mutex<HashMap<SourceId, playback::PlaybackCheckpointRevision>>>,
    worker: Option<JoinHandle<()>>,
}

impl PlaybackPersistence {
    fn new(target: PersistenceTarget) -> Self {
        Self::start(move |work| target.apply(work))
    }

    fn start(mut apply: impl FnMut(PersistenceWork) + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::sync_channel(PERSISTENCE_CAPACITY);
        let checkpoints = Arc::new(Mutex::new(HashMap::<
            SourceId,
            playback::PlaybackCheckpointRevision,
        >::new()));
        let worker_checkpoints = Arc::clone(&checkpoints);
        let worker = thread::Builder::new()
            .name("rufin-playback-persistence".to_string())
            .spawn(move || {
                while let Ok(token) = receiver.recv() {
                    match token {
                        PersistenceToken::Checkpoint(source_id) => {
                            let checkpoint = worker_checkpoints
                                .lock()
                                .ok()
                                .and_then(|mut pending| pending.remove(&source_id));
                            if let Some(checkpoint) = checkpoint {
                                apply(PersistenceWork::Checkpoint(checkpoint));
                            }
                        }
                        PersistenceToken::Work(PersistenceWork::Fence(crossed)) => {
                            let _ = crossed.send(());
                        }
                        PersistenceToken::Work(work) => apply(work),
                        PersistenceToken::Shutdown => break,
                    }
                }
            })
            .expect("could not start Playback persistence");
        Self {
            sender: Mutex::new(sender),
            checkpoints,
            worker: Some(worker),
        }
    }

    fn enqueue(&self, work: PersistenceWork) {
        let stopped = match self.sender.lock() {
            Ok(sender) => sender.send(PersistenceToken::Work(work)).is_err(),
            Err(_) => true,
        };
        if stopped {
            warn!("Playback persistence worker stopped");
        }
    }

    fn enqueue_checkpoint(&self, mut checkpoint: playback::PlaybackCheckpointRevision) {
        let source_id = checkpoint.source_id().clone();
        let Ok(sender) = self.sender.lock() else {
            warn!("Playback persistence worker stopped");
            return;
        };
        let Ok(mut pending) = self.checkpoints.lock() else {
            warn!("Playback persistence worker stopped");
            return;
        };
        if let Some(mut older) = pending.remove(&source_id) {
            older.coalesce(checkpoint);
            checkpoint = older;
        } else if sender
            .send(PersistenceToken::Checkpoint(source_id.clone()))
            .is_err()
        {
            warn!("Playback persistence worker stopped");
        }
        pending.insert(source_id, checkpoint);
    }

    fn enqueue_output_state(&self, volume: f64, muted: bool, audio_output: Option<String>) {
        self.enqueue(PersistenceWork::OutputState {
            volume,
            muted,
            audio_output,
        });
    }

    fn drain(&self) {
        let (fence, crossed) = mpsc::sync_channel(0);
        let sent = self.sender.lock().is_ok_and(|sender| {
            sender
                .send(PersistenceToken::Work(PersistenceWork::Fence(fence)))
                .is_ok()
        });
        if sent {
            let _ = crossed.recv();
        }
    }
}

impl Drop for PlaybackPersistence {
    fn drop(&mut self) {
        self.drain();
        if let Ok(sender) = self.sender.lock() {
            let _ = sender.send(PersistenceToken::Shutdown);
        }
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            warn!("Playback persistence worker stopped unexpectedly");
        }
    }
}

// Native provider reporting is transient but ordered. One Playback-owned drain
// keeps play transitions in order while repeated progress can be replaced or
// discarded when a slow server fills the bounded queue.
const SOURCE_REPORT_CAPACITY: usize = 64;

#[derive(Clone, Copy, Eq, PartialEq)]
struct SourceReportKey {
    playback_instance: u64,
    run: RunId,
}

struct PendingSourceReport<T> {
    key: SourceReportKey,
    phase: SourceReportPhase,
    payload: T,
}

impl<T> PendingSourceReport<T> {
    fn progress(&self) -> bool {
        self.phase == SourceReportPhase::Progress
    }
}

struct PendingSourceReports<T> {
    capacity: usize,
    items: VecDeque<PendingSourceReport<T>>,
}

impl<T> PendingSourceReports<T> {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            items: VecDeque::with_capacity(capacity),
        }
    }

    fn push(&mut self, pending: PendingSourceReport<T>) -> Result<(), PendingSourceReport<T>> {
        if pending.progress()
            && let Some(index) = self
                .items
                .iter()
                .position(|queued| queued.progress() && queued.key == pending.key)
        {
            self.items.remove(index);
            self.items.push_back(pending);
            return Ok(());
        }
        if self.items.len() < self.capacity {
            self.items.push_back(pending);
            return Ok(());
        }
        if !pending.progress()
            && let Some(index) = self.items.iter().position(PendingSourceReport::progress)
        {
            self.items.remove(index);
            self.items.push_back(pending);
            return Ok(());
        }
        Err(pending)
    }

    fn pop(&mut self) -> Option<PendingSourceReport<T>> {
        self.items.pop_front()
    }
}

struct SourceReportMailboxState<T> {
    pending: PendingSourceReports<T>,
    draining: bool,
}

struct SourceReportMailbox<T> {
    state: Mutex<SourceReportMailboxState<T>>,
}

impl<T> SourceReportMailbox<T> {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(SourceReportMailboxState {
                pending: PendingSourceReports::new(capacity),
                draining: false,
            }),
        }
    }

    fn admit(&self, report: PendingSourceReport<T>) -> Result<bool, PendingSourceReport<T>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending.push(report)?;
        if state.draining {
            return Ok(false);
        }
        state.draining = true;
        Ok(true)
    }

    fn next(&self) -> Option<PendingSourceReport<T>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let next = state.pending.pop();
        if next.is_none() {
            state.draining = false;
        }
        next
    }
}

struct SourceReportJob(Arc<Source>, SourceReportFact);

impl SourceReportMailbox<SourceReportJob> {
    async fn drain(self: Arc<Self>) {
        while let Some(pending) = self.next() {
            let run = pending.key.run;
            let phase = pending.phase;
            let SourceReportJob(source, report) = pending.payload;
            if let Err(error) = source.report_playback(report).await {
                warn!(%error, %run, ?phase, "source playback report failed");
            }
        }
    }
}

pub(crate) struct PlaybackOwner {
    library: Libraries,
    settings: SettingsFile,
    runtime: tokio::runtime::Handle,
    events: EventSender<PlaybackPublication>,
    artwork: artwork::Artwork,
    waveform: Arc<WaveformOwner>,
    loudness: Arc<LoudnessAnalysisOwner>,
    lyrics: Arc<LyricsService>,
    discord: Arc<desktop_integration::Discord>,
    active: Mutex<Option<ActivePlayback>>,
    queue_intents: QueueIntentWorker,
    scrobbler: Arc<Scrobbler>,
    persistence: PlaybackPersistence,
    source_reports: Arc<SourceReportMailbox<SourceReportJob>>,
    monotonic_origin: Instant,
    play_id_prefix: String,
    next_instance: AtomicU64,
    start_backend: Box<dyn Fn() -> Result<Box<dyn PlaybackBackend>, String> + Send + Sync>,
    cast: playback_cast::CastManager,
    output: Mutex<OutputSelection>,
}

struct OutputSelection {
    selected: playback::PlaybackOutput,
    prepared: Option<Box<dyn PlaybackBackend>>,
}

impl PlaybackOwner {
    pub(crate) fn new<StartBackend>(
        library: Libraries,
        settings: SettingsFile,
        runtime: tokio::runtime::Handle,
        events: EventSender<PlaybackPublication>,
        acceptance: SourceAcceptanceSender,
        artwork: artwork::Artwork,
        waveform: Arc<WaveformOwner>,
        lyrics: Arc<LyricsService>,
        discord: Arc<desktop_integration::Discord>,
        scrobbler: Arc<Scrobbler>,
        start_backend: StartBackend,
    ) -> Arc<Self>
    where
        StartBackend: Fn() -> Result<Box<dyn PlaybackBackend>, String> + Send + Sync + 'static,
    {
        let persistence = PlaybackPersistence::new(PersistenceTarget {
            library: library.clone(),
            settings: settings.clone(),
            acceptance,
            scrobbler: Arc::clone(&scrobbler),
        });
        let queue_intents = QueueIntentWorker::new(runtime.clone());
        let loudness = LoudnessAnalysisOwner::new(runtime.clone());
        let cast_settings = settings.load().ui;
        let owner = Arc::new(Self {
            scrobbler,
            library,
            settings,
            runtime,
            events,
            artwork,
            waveform,
            loudness,
            lyrics,
            discord,
            active: Mutex::new(None),
            queue_intents,
            persistence,
            source_reports: Arc::new(SourceReportMailbox::new(SOURCE_REPORT_CAPACITY)),
            monotonic_origin: Instant::now(),
            play_id_prefix: random_identity(),
            next_instance: AtomicU64::new(1),
            start_backend: Box::new(start_backend),
            cast: playback_cast::CastManager::new(
                cast_settings.cast_proxy_enabled,
                cast_settings.cast_network_interface,
            ),
            output: Mutex::new(OutputSelection {
                selected: playback::PlaybackOutput::Local,
                prepared: None,
            }),
        });
        owner.update_discord_settings();
        owner
    }

    pub(crate) fn prepare_selected(
        self: &Arc<Self>,
        session: Arc<ActiveSource>,
        selected: Arc<SelectedSourceState>,
    ) -> Result<PreparedPlayback, String> {
        let stored = self.settings.load();
        let sequence = match self
            .library
            .load_playback(selected.source_id())
            .map_err(string_error)?
        {
            PlaybackLoad::Missing | PlaybackLoad::DiscardedCorrupt => {
                let mut sequence = playback::Sequence::new(selected.source_id().clone());
                sequence.set_repeat_mode(stored.ui.repeat_mode);
                sequence.set_shuffle_seed(stored.ui.shuffle_enabled, random_u64());
                sequence
            }
            PlaybackLoad::Ready(checkpoint) => playback::restore_checkpoint(
                &checkpoint,
                Some(&selected.library),
                stored.ui.repeat_mode,
                stored.ui.shuffle_enabled,
                random_u64(),
            )
            .map_err(|error| error.to_string())?,
        };
        let source_id = selected.source_id().clone();
        let source_session_epoch = selected.source_session_epoch;
        let instance = self.next_instance.fetch_add(1, Ordering::AcqRel);
        let owner = Arc::downgrade(self);
        let clock_owner = Arc::downgrade(self);
        let output_session = session.clone();
        let activated = Arc::new(AtomicBool::new(false));
        let output_activated = Arc::clone(&activated);
        let (playback_output, backend) = self.take_selected_backend()?;
        let (playback, projection) = Playback::start(
            sequence,
            source_session_epoch,
            format!("{}:{instance}", self.play_id_prefix),
            stored.ui.playback,
            stored.ui.auto_dj_enabled,
            usize::from(stored.ui.auto_dj_refill_threshold),
            playback_output,
            backend,
            Arc::new(move || {
                clock_owner
                    .upgrade()
                    .map_or_else(empty_clock_sample, |owner| owner.clock_sample())
            }),
            {
                let source_id = source_id.clone();
                move |update| {
                    if output_activated.load(Ordering::Acquire)
                        && let Some(owner) = owner.upgrade()
                    {
                        owner.consume_update(
                            instance,
                            source_id.clone(),
                            source_session_epoch,
                            &output_session,
                            update,
                        );
                    }
                }
            },
        )
        .map_err(string_error)?;
        Ok(PreparedPlayback {
            active: Some(ActivePlayback {
                instance,
                source_id,
                source_session_epoch,
                selected: session,
                playback,
            }),
            projection: Some(projection),
            activated,
        })
    }

    pub(crate) fn install_prepared(
        &self,
        mut prepared: PreparedPlayback,
        _cutover: PlaybackCutover,
    ) -> PlaybackProjection {
        let active = prepared
            .active
            .take()
            .expect("a prepared Playback retains its session until installation");
        let projection = prepared
            .projection
            .take()
            .expect("a prepared Playback retains its projection until installation");
        let source_session_epoch = active.source_session_epoch;
        let loudness_selected = Arc::clone(&active.selected);
        let mut current = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            current.is_none(),
            "the previous Playback session must stop before target installation"
        );
        *current = Some(active);
        drop(current);
        prepared.activated.store(true, Ordering::Release);
        self.queue_intents.select(source_session_epoch);
        self.loudness.settings_changed(
            self.settings.load().ui.playback.loudness_normalization,
            Some(loudness_selected),
        );
        projection
    }

    pub(crate) fn stop_for_source_switch(&self) -> PlaybackCutover {
        let active = self.take_active();
        if let Some(active) = active
            && let Err(error) = active.playback.retire()
        {
            warn!(%error, "could not retire Playback");
        }
        self.persistence.drain();
        PlaybackCutover
    }

    fn take_active(&self) -> Option<ActivePlayback> {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        self.queue_intents.retire();
        self.loudness.cancel();
        self.publish_current_media(None);
        self.observe_discord(None, false);
        active
    }

    fn take_selected_backend(
        &self,
    ) -> Result<(playback::PlaybackOutput, Box<dyn PlaybackBackend>), String> {
        let (selected, prepared) = {
            let mut output = self
                .output
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (output.selected.clone(), output.prepared.take())
        };
        let backend = match prepared {
            Some(backend) => backend,
            None => self.start_output_backend(&selected)?,
        };
        Ok((selected, backend))
    }

    fn start_output_backend(
        &self,
        output: &playback::PlaybackOutput,
    ) -> Result<Box<dyn PlaybackBackend>, String> {
        match output {
            playback::PlaybackOutput::Local => (self.start_backend)(),
            playback::PlaybackOutput::Remote(output) => self
                .cast
                .connect(output)
                .map(|backend| Box::new(backend) as Box<dyn PlaybackBackend>),
        }
    }

    fn select_output(&self, selected: playback::PlaybackOutput) -> Result<(), String> {
        if self
            .output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .selected
            == selected
        {
            return Ok(());
        }
        let backend = self.start_output_backend(&selected)?;
        if let Some(active) = self.active() {
            active
                .playback
                .replace_backend(selected.clone(), backend)
                .map_err(string_error)?;
            let mut output = self
                .output
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            output.selected = selected;
            output.prepared = None;
        } else {
            let mut output = self
                .output
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            output.selected = selected;
            output.prepared = Some(backend);
        }
        Ok(())
    }

    pub(crate) fn refresh_accepted_tracks(
        &self,
        source_id: &SourceId,
        source_session_epoch: SourceSessionEpoch,
        tracks: Vec<Track>,
    ) -> Result<(), String> {
        let Some(active) = self.active() else {
            return Err("Playback is not active".to_string());
        };
        if &active.source_id != source_id || active.source_session_epoch != source_session_epoch {
            return Err("the accepted Tracks belong to another Playback session".to_string());
        }
        let projection = active
            .playback
            .refresh_tracks(source_session_epoch, tracks)
            .map_err(string_error)?;
        self.refresh_loudness_analysis(&active);
        self.publish_projection(active.source_id, active.source_session_epoch, projection);
        Ok(())
    }

    pub(crate) fn prepare_track_refresh(
        &self,
        source_session_epoch: SourceSessionEpoch,
    ) -> Result<PreparedTrackRefresh, String> {
        let Some(active) = self.active() else {
            return Err("Playback is not active".to_string());
        };
        if active.source_session_epoch != source_session_epoch {
            return Err("the Library replacement belongs to another Playback session".to_string());
        }
        let (source_id, epoch, ids) = active.playback.queued_track_ids().map_err(string_error)?;
        if source_id != active.source_id || epoch != source_session_epoch {
            return Err("the queued Tracks belong to another Playback session".to_string());
        }
        Ok(PreparedTrackRefresh {
            active,
            track_ids: ids,
        })
    }

    pub(crate) fn apply_track_refresh(
        &self,
        prepared: PreparedTrackRefresh,
        loaded: &Arc<Library>,
    ) -> Result<(), String> {
        let PreparedTrackRefresh { active, track_ids } = prepared;
        if !self.is_active(
            active.instance,
            &active.source_id,
            active.source_session_epoch,
        ) {
            return Err("Playback belongs to another source session".to_string());
        }
        if loaded.source_id() != &active.source_id {
            return Err("the Library replacement belongs to another Playback session".to_string());
        }
        let tracks = loaded.resolve_tracks(track_ids).map_err(string_error)?;
        let mut projection = active
            .playback
            .refresh_tracks(active.source_session_epoch, tracks)
            .map_err(string_error)?;
        projection.queue_page = Some(
            active
                .playback
                .queue_page(QueuePageQuery::current())
                .map_err(string_error)?,
        );
        self.refresh_loudness_analysis(&active);
        self.publish_projection(active.source_id, active.source_session_epoch, projection);
        Ok(())
    }

    pub(crate) fn waveform_setting_changed(&self, waveform_enabled: bool) {
        let waveform_media = waveform_enabled
            .then(|| self.current_media())
            .flatten()
            .and_then(|media| self.waveform_media(media));
        self.waveform
            .settings_changed(waveform_enabled, waveform_media);
    }

    pub(crate) fn playback_settings_changed(&self, settings: playback::PlaybackSettings) {
        let loudness_mode = settings.loudness_normalization;
        self.send(SessionCommand::UpdateSettings(settings));
        self.loudness
            .settings_changed(loudness_mode, self.active().map(|active| active.selected));
    }

    pub(crate) fn cast_proxy_setting_changed(&self, enabled: bool) {
        self.cast.set_proxy_media(enabled);
    }

    pub(crate) fn cast_network_setting_changed(&self, network_interface: Option<String>) {
        self.cast.set_network_interface(network_interface);
    }

    pub(crate) fn auto_dj_threshold_changed(&self, enabled: bool, refill_threshold: u8) {
        self.send(SessionCommand::SetAutoDj {
            enabled,
            refill_threshold: usize::from(refill_threshold),
        });
    }

    pub(crate) fn stream_inputs_changed(
        &self,
        source_id: &SourceId,
        source_session_epoch: SourceSessionEpoch,
    ) -> Result<(), String> {
        let active = self
            .active()
            .filter(|active| {
                &active.source_id == source_id
                    && active.source_session_epoch == source_session_epoch
            })
            .ok_or_else(|| "Playback belongs to another source session".to_string())?;
        self.send_to(&active, SessionCommand::StreamInputsChanged);
        Ok(())
    }

    pub(crate) fn current_media(&self) -> Option<Arc<playback::CurrentMedia>> {
        self.active()
            .and_then(|active| active.playback.current_media().ok().flatten())
    }

    fn consume_update(
        &self,
        instance: u64,
        source_id: SourceId,
        source_session_epoch: SourceSessionEpoch,
        selected: &Arc<ActiveSource>,
        update: PlaybackUpdate,
    ) {
        let current_media_changed = update.current_media_changed;
        if let Some(checkpoint) = update.checkpoint {
            self.persistence.enqueue_checkpoint(checkpoint);
        }
        for effect in update.effects {
            self.consume_effect(instance, &source_id, source_session_epoch, selected, effect);
        }
        if let Some(projection) = update.projection
            && self.is_active(instance, &source_id, source_session_epoch)
        {
            if current_media_changed {
                self.publish_current_media(projection.view.transport.current.clone());
            }
            self.observe_discord(
                Some(&projection),
                projection.notices.iter().any(|notice| {
                    matches!(notice, playback::PlaybackNotice::PositionDiscontinuity(_))
                }),
            );
            let _ = self.events.try_send(PlaybackPublication {
                source_id,
                source_session_epoch,
                projection,
            });
        }
    }

    fn consume_effect(
        &self,
        instance: u64,
        source_id: &SourceId,
        source_session_epoch: SourceSessionEpoch,
        selected: &Arc<ActiveSource>,
        effect: SessionEffect,
    ) {
        match effect {
            SessionEffect::ResolveStream { run, request, .. } => {
                self.resolve_stream(
                    instance,
                    source_id.clone(),
                    source_session_epoch,
                    run,
                    request,
                );
            }
            SessionEffect::PersistProgress {
                source_id,
                revision,
                occurrence: Some(occurrence),
                progress_millis,
            } => {
                self.persistence
                    .enqueue(PersistenceWork::Progress(PlaybackProgressUpdate {
                        source_id,
                        revision,
                        occurrence: PlaybackOccurrenceId::new(occurrence.as_str()),
                        progress_millis,
                    }));
            }
            SessionEffect::PersistProgress {
                occurrence: None, ..
            } => {}
            SessionEffect::PersistState {
                source_id,
                revision,
                occurrence,
                progress_millis,
                ..
            } => {
                self.persistence
                    .enqueue(PersistenceWork::State(PlaybackStateUpdate {
                        source_id,
                        revision,
                        selected: occurrence
                            .map(|occurrence| PlaybackOccurrenceId::new(occurrence.as_str())),
                        progress_millis,
                    }));
            }
            SessionEffect::PersistOutputState {
                volume,
                muted,
                audio_output,
            } => {
                self.persistence
                    .enqueue_output_state(volume, muted, audio_output);
            }
            SessionEffect::FlushPersistence { .. } => {
                self.persistence.drain();
            }
            SessionEffect::Listening(fact) => {
                if let playback::ListeningFact::Started { track, .. } = &fact {
                    self.scrobbler.now_playing(track);
                }
            }
            SessionEffect::ExternalScrobble(completed) => {
                self.persistence
                    .enqueue(PersistenceWork::CompletedScrobble(completed));
            }
            SessionEffect::Activity(outcome) => {
                self.record_activity(selected, source_id, source_session_epoch, outcome);
            }
            SessionEffect::SourceReport(report) => {
                if let Some(source) = selected
                    .resolve()
                    .and_then(|selected| selected.source.clone())
                {
                    self.report_source(instance, source, report);
                }
            }
            SessionEffect::RequestAutoDj(request) => {
                if let Some(active) =
                    self.active_matching(instance, source_id, source_session_epoch)
                {
                    crate::radio::request_auto_dj(
                        self.runtime.clone(),
                        active.weak_selected(),
                        active.playback,
                        request,
                    );
                }
            }
            SessionEffect::NonfatalError(error) => {
                debug!(%error, "Playback operation was not available");
            }
            SessionEffect::FatalError(error) => warn!(%error, "Playback session failed"),
            SessionEffect::Backend(_)
            | SessionEffect::CurrentMediaChanged
            | SessionEffect::PositionDiscontinuity(_)
            | SessionEffect::Visualizer { .. } => {
                // Playback consumes backend effects and turns presentation
                // effects into PlaybackProjection notices before this edge.
            }
        }
    }

    fn resolve_stream(
        &self,
        instance: u64,
        source_id: SourceId,
        source_session_epoch: SourceSessionEpoch,
        run: playback::RunId,
        request: StreamRequest,
    ) {
        let active = self.active_matching(instance, &source_id, source_session_epoch);
        let Some(active) = active else {
            return;
        };
        let Some(selected) = active.selected() else {
            return;
        };
        let loaded = Arc::clone(&selected.library);
        let source = selected.source.clone();
        let track_id = request.track_id.clone();
        let track = loaded.track(&track_id).ok().flatten();
        let source_format = track.as_ref().and_then(|track| track.source_format.clone());
        let artwork_path = track.as_ref().and_then(|track| {
            let stored = self.settings.load();
            let binding = artwork::ArtworkBindings::new(&loaded)
                .track(track)
                .into_binding();
            let external = artwork::ExternalPolicy::new(
                stored.ui.external_metadata_enabled,
                stored.ui.allows_external_metadata_lookup(),
                stored.ui.lastfm_api_key,
            );
            [512, 256, 96].into_iter().find_map(|size| {
                let request = artwork::ArtworkRequest::new(binding.clone(), size, size)
                    .with_external(external.clone());
                self.artwork.cache_only_file(&source_id, &request)
            })
        });
        debug!(
            %source_id,
            %track_id,
            %run,
            source_format = source_format.as_deref().unwrap_or("unknown"),
            quality = ?request.quality,
            "resolving playback stream"
        );
        let runtime = self.runtime.clone();
        let playback = active.playback;
        let is_transcoded_stream = request.quality.max_bitrate_kbps().is_some();
        runtime.spawn(async move {
            let loudness = loaded
                .loudness_for_track(&track_id)
                .unwrap_or_else(|error| {
                    warn!(%source_id, %track_id, %error, "could not read stored loudness");
                    library::TrackLoudness::default()
                });
            let result = prepare_stream(Some(Arc::clone(&loaded)), source, request)
                .await
                .map(|stream| {
                    let prepared = prepare_for_source_format(
                        PreparedStream::new(stream, loudness),
                        source_format.as_deref(),
                    );
                    match track {
                        Some(track) => prepared
                            .with_media(
                                track,
                                if is_transcoded_stream {
                                    Some("audio/mpeg".to_string())
                                } else {
                                    source_format.as_deref().and_then(audio_mime)
                                },
                            )
                            .with_artwork_path(artwork_path),
                        None => prepared,
                    }
                });
            match &result {
                Ok(stream) => debug!(
                    %source_id,
                    %track_id,
                    %run,
                    transport = stream
                        .uri()
                        .split_once(':')
                        .map(|(scheme, _)| scheme)
                        .unwrap_or("unknown"),
                    "resolved playback stream"
                ),
                Err(error) => warn!(
                    %source_id,
                    %track_id,
                    %run,
                    %error,
                    "could not resolve playback stream"
                ),
            }
            let _ = tokio::task::spawn_blocking(move || playback.resolve_stream(run, result)).await;
        });
    }

    fn record_activity(
        &self,
        selected: &Arc<ActiveSource>,
        source_id: &SourceId,
        source_session_epoch: SourceSessionEpoch,
        outcome: playback::ListeningOutcome,
    ) {
        let Some(selected) = selected.resolve() else {
            return;
        };
        if selected.library.source_id() != source_id || &outcome.source_id != source_id {
            return;
        }
        self.persistence.enqueue(PersistenceWork::Activity {
            loaded: Arc::clone(&selected.library),
            source_id: source_id.clone(),
            source_session_epoch,
            outcome,
        });
    }

    fn report_source(&self, instance: u64, source: Arc<Source>, fact: SourceReportFact) {
        let run = fact.run;
        let phase = fact.phase;
        let pending = PendingSourceReport {
            key: SourceReportKey {
                playback_instance: instance,
                run,
            },
            phase,
            payload: SourceReportJob(source, fact),
        };
        match self.source_reports.admit(pending) {
            Ok(true) => {
                drop(self.runtime.spawn(Arc::clone(&self.source_reports).drain()));
            }
            Ok(false) => {}
            Err(_) => {
                warn!(%run, ?phase, "source playback reporting is busy; report was dropped");
            }
        }
    }

    fn clock_sample(&self) -> playback::ClockSample {
        let now = SystemTime::now();
        let unix_seconds = unix_seconds(now);
        playback::ClockSample {
            monotonic_millis: self
                .monotonic_origin
                .elapsed()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
            unix_seconds,
            local_period: local_calendar_period(unix_seconds),
        }
    }

    fn active(&self) -> Option<ActivePlayback> {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn refresh_loudness_analysis(&self, active: &ActivePlayback) {
        self.loudness.library_changed(
            self.settings.load().ui.playback.loudness_normalization,
            Some(Arc::clone(&active.selected)),
        );
    }

    fn active_matching(
        &self,
        instance: u64,
        source_id: &SourceId,
        source_session_epoch: SourceSessionEpoch,
    ) -> Option<ActivePlayback> {
        self.active().filter(|active| {
            active.instance == instance
                && &active.source_id == source_id
                && active.source_session_epoch == source_session_epoch
        })
    }

    fn active_for_media(&self, media: &playback::CurrentMedia) -> Option<ActivePlayback> {
        self.active().filter(|active| {
            active.source_id == media.id.source_id
                && active.source_session_epoch == media.id.source_session_epoch
        })
    }

    fn is_active(
        &self,
        instance: u64,
        source_id: &SourceId,
        source_session_epoch: SourceSessionEpoch,
    ) -> bool {
        self.active_matching(instance, source_id, source_session_epoch)
            .is_some()
    }

    fn publish_current_media(&self, media: Option<Arc<playback::CurrentMedia>>) {
        let active = media
            .as_ref()
            .and_then(|media| self.active_for_media(media));
        let waveform = media
            .as_ref()
            .and_then(|media| self.waveform_media(Arc::clone(media)));
        self.waveform.current_changed(waveform);
        let Some(media) = media else {
            self.lyrics.set_current(None);
            return;
        };
        let Some(active) = active else {
            self.lyrics.set_current(None);
            return;
        };
        let Some(selected) = active.selected() else {
            self.lyrics.set_current(None);
            return;
        };
        let input = match selected.configuration.input_identity() {
            Ok(input) => input,
            Err(error) => {
                warn!(%error, "could not identify the current source for lyrics");
                self.lyrics.set_current(None);
                return;
            }
        };
        self.lyrics.set_current(Some(LyricsContext {
            media,
            input,
            source: selected.source.clone(),
            loaded: Arc::clone(&selected.library),
        }));
    }

    fn waveform_media(&self, media: Arc<playback::CurrentMedia>) -> Option<WaveformMedia> {
        let active = self.active_for_media(&media)?;
        let selected = active.selected()?;
        Some(WaveformMedia {
            request: StreamRequest::new(
                media.track.id.clone(),
                self.settings.playback_stream_quality(),
            ),
            media,
            loaded: Arc::clone(&selected.library),
            source: selected.source.clone(),
        })
    }

    pub(crate) fn publish_selected_products(&self, projection: &PlaybackProjection) {
        self.publish_current_media(projection.view.transport.current.clone());
        self.observe_discord(Some(projection), false);
    }

    fn publish_projection(
        &self,
        source_id: SourceId,
        source_session_epoch: SourceSessionEpoch,
        projection: PlaybackProjection,
    ) {
        self.publish_selected_products(&projection);
        let _ = self.events.try_send(PlaybackPublication {
            source_id,
            source_session_epoch,
            projection,
        });
    }

    pub(crate) fn update_discord_settings(&self) {
        let stored = self.settings.load();
        let projection = self
            .active()
            .and_then(|active| active.playback.projection().ok());
        self.discord.update(
            stored.ui.rich_presence.clone(),
            !stored.ui.private_mode,
            &stored.ui.lastfm_api_key,
            projection.as_ref().map(|projection| &projection.view),
        );
    }

    fn observe_discord(
        &self,
        projection: Option<&PlaybackProjection>,
        position_discontinuity: bool,
    ) {
        self.discord.observe(
            projection.map(|projection| &projection.view),
            position_discontinuity,
        );
    }

    fn send(&self, command: SessionCommand) {
        let Some(active) = self.active() else {
            return;
        };
        self.send_to(&active, command);
    }

    fn send_to(&self, active: &ActivePlayback, command: SessionCommand) {
        if let Err(error) = active.playback.command(command) {
            warn!(%error, "Playback command failed");
        }
    }
}

fn module_music_format(source_format: &str) -> bool {
    matches!(
        source_format.trim().to_ascii_lowercase().as_str(),
        "669"
            | "amf"
            | "ams"
            | "dbm"
            | "digi"
            | "dmf"
            | "dsm"
            | "far"
            | "gdm"
            | "imf"
            | "it"
            | "j2b"
            | "mdl"
            | "med"
            | "mod"
            | "mptm"
            | "mt2"
            | "mtm"
            | "okt"
            | "psm"
            | "ptm"
            | "s3m"
            | "stm"
            | "stx"
            | "ult"
            | "umx"
            | "xm"
    )
}

fn audio_mime(source_format: &str) -> Option<String> {
    let mime = match source_format.trim().to_ascii_lowercase().as_str() {
        "mp3" | "mp2" | "mpeg" => "audio/mpeg",
        "flac" => "audio/flac",
        "m4a" | "mp4" => "audio/mp4",
        "aac" => "audio/aac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" | "wave" => "audio/wav",
        "webm" => "audio/webm",
        _ => return None,
    };
    Some(mime.to_string())
}

fn prepare_for_source_format(
    prepared: PreparedStream,
    source_format: Option<&str>,
) -> PreparedStream {
    if source_format.is_some_and(module_music_format) {
        prepared.without_preloading().without_timing_queries()
    } else {
        prepared
    }
}

impl QueueCommandPort for PlaybackOwner {
    fn play_loaded(&self, request: LoadedPlayRequest) {
        let Some(active) = self.active().filter(|active| {
            active.source_id == request.source_id
                && active.source_session_epoch == request.source_session_epoch
        }) else {
            return;
        };
        self.queue_intents.submit_loaded(active.playback, request);
    }

    fn remove(&self, occurrence: OccurrenceId) {
        self.send(SessionCommand::Remove(occurrence));
    }

    fn activate(&self, occurrence: OccurrenceId) {
        self.send(SessionCommand::Activate(occurrence));
    }

    fn move_after_current(&self, occurrence: OccurrenceId) {
        self.send(SessionCommand::MoveAfterCurrent(occurrence));
    }

    fn reorder(&self, request: QueueReorderRequest) {
        self.send(SessionCommand::Reorder {
            occurrence: request.occurrence,
            target_index: request.target_index,
            after: request.after,
        });
    }

    fn clear(&self) {
        self.send(SessionCommand::ClearUpcoming);
    }

    fn request_page(&self, query: QueuePageQuery) -> Option<QueuePage> {
        self.active()
            .and_then(|active| active.playback.queue_page(query).ok())
    }
}

impl RadioCommandPort for PlaybackOwner {
    fn play_random(&self, request: RandomPlayRequest) {
        let Some(active) = self.active() else {
            return;
        };
        self.queue_intents.submit_random(
            active.source_session_epoch,
            active.weak_selected(),
            active.playback,
            request,
        );
    }

    fn play_radio(&self, request: RadioPlayRequest) {
        let Some(active) = self.active() else {
            return;
        };
        self.queue_intents.submit_radio(
            active.source_session_epoch,
            active.weak_selected(),
            active.playback,
            request,
        );
    }
}

impl TransportCommandPort for PlaybackOwner {
    fn play_pause(&self) {
        self.send(SessionCommand::PlayPause);
    }

    fn play(&self) {
        self.send(SessionCommand::Play);
    }

    fn pause(&self) {
        self.send(SessionCommand::Pause);
    }

    fn stop(&self) {
        self.send(SessionCommand::Stop);
    }

    fn next(&self) {
        self.send(SessionCommand::Next);
    }

    fn previous(&self) {
        self.send(SessionCommand::Previous);
    }

    fn seek_seconds(&self, seconds: u32) {
        self.seek_millis(u64::from(seconds).saturating_mul(1_000));
    }

    fn seek_millis(&self, millis: u64) {
        self.send(SessionCommand::Seek(millis));
    }

    fn set_volume(&self, volume: f64) {
        self.send(SessionCommand::SetVolume(volume));
    }

    fn persist_volume(&self, volume: f64) {
        self.send(SessionCommand::SetVolume(volume));
        self.send(SessionCommand::PersistOutputState);
    }

    fn set_muted(&self, muted: bool) {
        self.send(SessionCommand::SetMuted(muted));
    }

    fn toggle_shuffle(&self) {
        let enabled = match self.settings.update(|stored| {
            stored.ui.shuffle_enabled = !stored.ui.shuffle_enabled;
            Ok(stored.ui.shuffle_enabled)
        }) {
            Ok(enabled) => enabled,
            Err(error) => {
                warn!(%error, "could not save shuffle setting");
                return;
            }
        };
        self.send(SessionCommand::SetShuffle {
            enabled,
            seed: random_u64(),
        });
    }

    fn set_shuffle(&self, enabled: bool) {
        if let Err(error) = self.settings.update(|stored| {
            stored.ui.shuffle_enabled = enabled;
            Ok(())
        }) {
            warn!(%error, "could not save shuffle setting");
            return;
        }
        self.send(SessionCommand::SetShuffle {
            enabled,
            seed: random_u64(),
        });
    }

    fn cycle_repeat(&self) {
        let repeat = match self.settings.update(|stored| {
            stored.ui.repeat_mode = next_repeat(stored.ui.repeat_mode);
            Ok(stored.ui.repeat_mode)
        }) {
            Ok(repeat) => repeat,
            Err(error) => {
                warn!(%error, "could not save repeat setting");
                return;
            }
        };
        self.send(SessionCommand::SetRepeat(repeat));
    }

    fn set_repeat(&self, repeat: RepeatMode) {
        if let Err(error) = self.settings.update(|stored| {
            stored.ui.repeat_mode = repeat;
            Ok(())
        }) {
            warn!(%error, "could not save repeat setting");
            return;
        }
        self.send(SessionCommand::SetRepeat(repeat));
    }

    fn toggle_auto_dj(&self) {
        let (enabled, refill_threshold) = match self.settings.update(|stored| {
            stored.ui.auto_dj_enabled = !stored.ui.auto_dj_enabled;
            Ok((
                stored.ui.auto_dj_enabled,
                stored.ui.auto_dj_refill_threshold,
            ))
        }) {
            Ok(settings) => settings,
            Err(error) => {
                warn!(%error, "could not save AutoDJ setting");
                return;
            }
        };
        self.send(SessionCommand::SetAutoDj {
            enabled,
            refill_threshold: usize::from(refill_threshold),
        });
    }

    fn set_visualizer_enabled(&self, enabled: bool) {
        self.send(SessionCommand::SetVisualizerEnabled(enabled));
    }

    fn available_audio_outputs(&self) -> Vec<playback::AudioOutput> {
        playback_gstreamer::available_audio_outputs()
    }

    fn available_cast_networks(&self) -> Vec<playback::CastNetwork> {
        match self.cast.available_networks() {
            Ok(networks) => networks,
            Err(error) => {
                warn!(%error, "could not list casting networks");
                Vec::new()
            }
        }
    }

    fn playback_output(&self) -> playback::PlaybackOutput {
        self.output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .selected
            .clone()
    }

    fn discover_remote_outputs(&self) -> Result<Vec<playback::RemoteOutput>, String> {
        self.cast.discover()
    }

    fn select_playback_output(&self, output: playback::PlaybackOutput) -> Result<(), String> {
        self.select_output(output)
    }

    fn shutdown(&self) {
        if let Some(mut backend) = self
            .output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .prepared
            .take()
        {
            let _ = backend.shutdown();
        }
        if let Some(active) = self.take_active()
            && let Err(error) = active.playback.shutdown()
        {
            warn!(%error, "could not shut down Playback");
        }
        self.persistence.drain();
    }
}

pub(crate) async fn prepare_stream(
    loaded: Option<Arc<library::Library>>,
    source: Option<Arc<Source>>,
    request: StreamRequest,
) -> Result<ResolvedStream, String> {
    let mut local_error = None;
    let local_file = loaded
        .map(|loaded| {
            loaded
                .playable_file(&request.track_id)
                .map_err(string_error)
        })
        .transpose()?
        .flatten();
    if let Some(file) = local_file {
        match prepared_local_stream(file) {
            Ok(stream) => return Ok(stream),
            Err(error) if source.is_none() => return Err(error),
            Err(error) => {
                debug!(%error, "verified local playback file is no longer available");
                local_error = Some(error);
            }
        }
    }
    let source = source.ok_or_else(source_access_unavailable)?;
    source_stream(&source, request, local_error).await
}

async fn source_stream(
    source: &Source,
    request: StreamRequest,
    unavailable_error: Option<String>,
) -> Result<ResolvedStream, String> {
    match source.stream(&request).await.map_err(string_error)? {
        NativeSourceResult::Available(stream) => Ok(stream),
        NativeSourceResult::Unavailable => Err(unavailable_error
            .unwrap_or_else(|| "the selected source cannot resolve this track".to_string())),
    }
}

fn prepared_local_stream(file: PlayableFile) -> Result<ResolvedStream, String> {
    let path = file.path();
    if !path.is_file() {
        return Err(format!(
            "the local playback file is missing: {}",
            path.display()
        ));
    }
    let url = url::Url::from_file_path(path)
        .map_err(|()| format!("could not create a file URI for {}", path.display()))?;
    Ok(match file {
        PlayableFile::File { .. } => ResolvedStream::new(url.to_string()),
        PlayableFile::Cue {
            start_millis,
            end_millis,
            ..
        } => ResolvedStream::new(url.to_string()).with_window(start_millis, end_millis),
    })
}

const fn next_repeat(repeat: RepeatMode) -> RepeatMode {
    match repeat {
        RepeatMode::Off => RepeatMode::All,
        RepeatMode::All => RepeatMode::One,
        RepeatMode::One => RepeatMode::Off,
    }
}

pub(crate) fn random_u64() -> u64 {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes)
        .map(|()| u64::from_le_bytes(bytes))
        .unwrap_or_else(|_| unix_seconds(SystemTime::now()) as u64)
}

fn random_identity() -> String {
    format!("{:016x}{:016x}", random_u64(), random_u64())
}

fn unix_seconds(now: SystemTime) -> i64 {
    now.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

fn local_calendar_period(unix_seconds: i64) -> String {
    glib::DateTime::from_unix_local(unix_seconds)
        .or_else(|_| glib::DateTime::from_unix_utc(unix_seconds))
        .and_then(|date| date.format("%Y-%m"))
        .map(|period| period.to_string())
        .unwrap_or_else(|_| "1970-01".to_string())
}

fn empty_clock_sample() -> playback::ClockSample {
    playback::ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".to_string(),
    }
}

fn string_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use library::{TrackData, TrackRelations};
    use playback::{
        BackendCommand, BackendError, BackendEvent, Batch, BatchItem, ClockSample, ListeningTrack,
        Placement, PlaybackBackend, PlaybackSettings, Provenance, Sequence,
    };

    #[derive(Default)]
    struct AcceptingBackend;

    impl PlaybackBackend for AcceptingBackend {
        fn send(&mut self, _command: BackendCommand) -> Result<(), BackendError> {
            Ok(())
        }

        fn drain_events(&mut self) -> Vec<BackendEvent> {
            Vec::new()
        }
    }

    #[derive(Debug, Eq, PartialEq)]
    enum AppliedWork {
        Checkpoint(SourceId, u64),
        Scrobble(String),
    }

    fn completed_scrobble(play_id: &str, source_id: &SourceId) -> PersistenceWork {
        PersistenceWork::CompletedScrobble(playback::CompletedScrobble {
            play_id: play_id.to_string(),
            track: ListeningTrack {
                source_id: source_id.clone(),
                track_id: TrackId::fake(1),
                recording_id: None,
                title: "Track".to_string(),
                artists: vec!["Artist".to_string()],
                album: Some("Album".to_string()),
                track_number: Some(1),
                disc_number: Some(1),
                duration_millis: 180_000,
            },
            started_at_unix_seconds: 1,
        })
    }

    fn checkpoint_revision(source_id: &SourceId) -> playback::PlaybackCheckpointRevision {
        let mut sequence = Sequence::new(source_id.clone());
        sequence
            .apply_batch(
                Batch::new(vec![BatchItem::new(
                    Track::new(TrackData {
                        id: TrackId::fake(1),
                        album_id: None,
                        title: "Track".to_string(),
                        artist: "Artist".to_string(),
                        album: "Album".to_string(),
                        album_artwork: None,
                        year: 2026,
                        release_date: None,
                        date_added: None,
                        last_played: None,
                        play_count: None,
                        user_rating: None,
                        duration_seconds: 180,
                        favorite: false,
                        disc_number: 1,
                        track_number: 1,
                        image_ref: None,
                        local_artwork: None,
                        musicbrainz_recording_id: None,
                        musicbrainz_release_track_id: None,
                        source_path: None,
                        cue: None,
                        source_format: None,
                        comment: None,
                        skip_count: None,
                        bpm: None,
                        relations: TrackRelations::default(),
                    }),
                    Provenance::Manual,
                )]),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("seed checkpoint revision");
        let (captured, receiver) = mpsc::channel();
        let (playback, _) = Playback::start(
            sequence,
            SourceSessionEpoch::new(1),
            "persistence-test",
            PlaybackSettings::default(),
            false,
            2,
            playback::PlaybackOutput::Local,
            Box::<AcceptingBackend>::default(),
            Arc::new(|| ClockSample {
                monotonic_millis: 0,
                unix_seconds: 0,
                local_period: "1970-01".to_string(),
            }),
            move |update| {
                if let Some(checkpoint) = update.checkpoint {
                    captured
                        .send(checkpoint)
                        .expect("capture checkpoint revision");
                }
            },
        )
        .expect("start checkpoint Playback");
        playback
            .command(SessionCommand::SetShuffle {
                enabled: true,
                seed: 7,
            })
            .expect("change checkpoint traversal");
        let checkpoint = receiver.recv().expect("checkpoint revision");
        playback.shutdown().expect("stop checkpoint Playback");
        checkpoint
    }

    #[test]
    fn blocked_worker_keeps_one_pending_checkpoint_per_source() {
        let first_source = SourceId::fake(1);
        let second_source = SourceId::fake(2);
        let first_checkpoint = checkpoint_revision(&first_source);
        let second_checkpoint = checkpoint_revision(&second_source);
        let revision = first_checkpoint.revision();
        let (entered, worker_entered) = mpsc::sync_channel(0);
        let (release, worker_release) = mpsc::sync_channel(0);
        let applied = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&applied);
        let mut first = true;
        let persistence = PlaybackPersistence::start(move |work| {
            if first {
                first = false;
                entered.send(()).expect("report blocked persistence worker");
                worker_release
                    .recv()
                    .expect("release blocked persistence worker");
            }
            let applied = match work {
                PersistenceWork::Checkpoint(checkpoint) => {
                    AppliedWork::Checkpoint(checkpoint.source_id().clone(), checkpoint.revision())
                }
                PersistenceWork::CompletedScrobble(completed) => {
                    AppliedWork::Scrobble(completed.play_id)
                }
                _ => return,
            };
            observed
                .lock()
                .expect("record applied persistence work")
                .push(applied);
        });

        persistence.enqueue_checkpoint(first_checkpoint.clone());
        worker_entered
            .recv()
            .expect("persistence worker entered first write");
        for _ in 0..256 {
            persistence.enqueue_checkpoint(first_checkpoint.clone());
            persistence.enqueue_checkpoint(second_checkpoint.clone());
        }
        persistence.enqueue(completed_scrobble("first", &first_source));
        persistence.enqueue(completed_scrobble("second", &first_source));

        {
            let pending = persistence
                .checkpoints
                .lock()
                .expect("inspect pending persistence work");
            assert_eq!(pending.len(), 2);
        }

        release.send(()).expect("release persistence worker");
        persistence.drain();
        assert_eq!(
            *applied.lock().expect("inspect applied persistence work"),
            [
                AppliedWork::Checkpoint(first_source.clone(), revision),
                AppliedWork::Checkpoint(first_source, revision),
                AppliedWork::Checkpoint(second_source, revision),
                AppliedWork::Scrobble("first".to_string()),
                AppliedWork::Scrobble("second".to_string()),
            ]
        );
    }

    #[test]
    fn traversal_without_its_durable_base_promotes_to_a_full_checkpoint() {
        let directory = tempfile::tempdir().expect("temporary Playback Store");
        let libraries =
            Libraries::open(directory.path().join("library.db")).expect("open Playback Store");
        let source_id = SourceId::fake(1);
        let checkpoint = checkpoint_revision(&source_id);
        let revision = checkpoint.revision();

        assert_eq!(
            apply_checkpoint_revision(&libraries, checkpoint).expect("persist promoted checkpoint"),
            library::PlaybackWriteOutcome::Applied
        );
        let library::PlaybackLoad::Ready(saved) = libraries
            .load_playback(&source_id)
            .expect("load promoted checkpoint")
        else {
            panic!("promoted checkpoint must be durable");
        };
        assert_eq!(saved.revision, revision);
        assert_eq!(saved.queue.occurrences.len(), 1);
    }
}

#[cfg(test)]
mod source_report_tests {
    use super::*;

    fn report(
        playback_instance: u64,
        run: u64,
        phase: SourceReportPhase,
        payload: u8,
    ) -> PendingSourceReport<u8> {
        PendingSourceReport {
            key: SourceReportKey {
                playback_instance,
                run: RunId::new(run),
            },
            phase,
            payload,
        }
    }

    fn drain(reports: &mut PendingSourceReports<u8>) -> Vec<u8> {
        let mut payloads = Vec::new();
        while let Some(report) = reports.pop() {
            payloads.push(report.payload);
        }
        payloads
    }

    #[test]
    fn newer_progress_replaces_and_repositions_the_same_play() {
        let mut reports = PendingSourceReports::new(4);
        assert!(
            reports
                .push(report(1, 1, SourceReportPhase::Started, 1))
                .is_ok()
        );
        assert!(
            reports
                .push(report(1, 1, SourceReportPhase::Progress, 2))
                .is_ok()
        );
        assert!(
            reports
                .push(report(1, 1, SourceReportPhase::QualifiedPlay, 3))
                .is_ok()
        );
        assert!(
            reports
                .push(report(1, 1, SourceReportPhase::Progress, 4))
                .is_ok()
        );

        assert_eq!(drain(&mut reports), [1, 3, 4]);
    }

    #[test]
    fn equal_run_numbers_from_different_playback_sessions_do_not_coalesce() {
        let mut reports = PendingSourceReports::new(2);
        assert!(
            reports
                .push(report(1, 1, SourceReportPhase::Progress, 1))
                .is_ok()
        );
        assert!(
            reports
                .push(report(2, 1, SourceReportPhase::Progress, 2))
                .is_ok()
        );

        assert_eq!(drain(&mut reports), [1, 2]);
    }

    #[test]
    fn a_transition_evicts_progress_but_not_an_older_transition() {
        let mut reports = PendingSourceReports::new(3);
        assert!(
            reports
                .push(report(1, 1, SourceReportPhase::Started, 1))
                .is_ok()
        );
        assert!(
            reports
                .push(report(1, 1, SourceReportPhase::Progress, 2))
                .is_ok()
        );
        assert!(
            reports
                .push(report(1, 2, SourceReportPhase::Started, 3))
                .is_ok()
        );
        assert!(
            reports
                .push(report(1, 1, SourceReportPhase::Ended, 4))
                .is_ok()
        );

        assert_eq!(drain(&mut reports), [1, 3, 4]);

        let mut transitions = PendingSourceReports::new(2);
        assert!(
            transitions
                .push(report(1, 1, SourceReportPhase::Started, 5))
                .is_ok()
        );
        assert!(
            transitions
                .push(report(1, 1, SourceReportPhase::QualifiedPlay, 6))
                .is_ok()
        );
        assert!(
            transitions
                .push(report(1, 1, SourceReportPhase::Ended, 7))
                .is_err()
        );
        assert_eq!(drain(&mut transitions), [5, 6]);
    }

    #[test]
    fn one_lazy_drain_owns_the_mailbox_until_it_is_empty() {
        let mailbox = SourceReportMailbox::new(2);
        assert!(matches!(
            mailbox.admit(report(1, 1, SourceReportPhase::Started, 1)),
            Ok(true)
        ));
        assert!(matches!(
            mailbox.admit(report(1, 1, SourceReportPhase::Progress, 2)),
            Ok(false)
        ));
        assert_eq!(mailbox.next().map(|report| report.payload), Some(1));
        assert_eq!(mailbox.next().map(|report| report.payload), Some(2));
        assert!(mailbox.next().is_none());
        assert!(matches!(
            mailbox.admit(report(1, 2, SourceReportPhase::Started, 3)),
            Ok(true)
        ));
    }
}

#[cfg(test)]
mod playback_format_tests {
    use super::{module_music_format, prepare_for_source_format};
    use library::ResolvedStream;
    use playback::PreparedStream;

    #[test]
    fn every_openmpt_family_uses_the_isolated_playback_path() {
        for format in ["mod", "s3m", "xm", "it", "mptm", "669", "okt", "umx"] {
            assert!(module_music_format(format), "{format}");
        }
        assert!(!module_music_format("flac"));
        assert!(!module_music_format("vgm"));
    }

    #[test]
    fn module_music_keeps_safe_files_recoverable() {
        let module = prepare_for_source_format(
            PreparedStream::from(ResolvedStream::new("file:///music/test.it")),
            Some("it"),
        );
        assert!(!module.allows_preloading);
        assert!(!module.allows_timing_queries);

        let flac = prepare_for_source_format(
            PreparedStream::from(ResolvedStream::new("file:///music/test.flac")),
            Some("flac"),
        );
        assert!(flac.allows_preloading);
        assert!(flac.allows_timing_queries);
    }
}

#[cfg(test)]
mod loaded_play_tests {
    use library::{
        CandidateBatch, CandidateFinish, CandidateHeader, HomeFacts, HomeSnapshot, LocalFile,
        LocalFileKind, LocalFileState, Playlist, PlaylistEntry, PlaylistId, PlaylistSnapshot,
        RadioSeed, TrackData, TrackRelations,
    };
    use playback::{
        BackendCommand, BackendError, BackendEvent, ClockSample, PlaybackBackend, PlaybackSettings,
        Provenance, QueuePlacement, Sequence,
    };
    use sources::SourceConfiguration;

    use super::*;

    #[derive(Default)]
    struct AcceptingBackend;

    impl PlaybackBackend for AcceptingBackend {
        fn send(&mut self, _command: BackendCommand) -> Result<(), BackendError> {
            Ok(())
        }

        fn drain_events(&mut self) -> Vec<BackendEvent> {
            Vec::new()
        }
    }

    fn loaded_play_fixture() -> (
        tempfile::TempDir,
        SourceId,
        Arc<Library>,
        LoadedPlayRequest,
        Playback,
    ) {
        let directory = tempfile::tempdir().expect("temporary loaded Play Store");
        let library =
            Libraries::open(directory.path().join("library.db")).expect("open loaded Play Store");
        let source_id = SourceId::fake(1);
        let mut candidate = library
            .begin_source_candidate(CandidateHeader {
                source_id: source_id.clone(),
                input_digest: [1; 32],
            })
            .expect("begin loaded Play candidate");
        candidate
            .write(CandidateBatch::Tracks(vec![
                track(1, "Alpha"),
                track(2, "Beta"),
            ]))
            .expect("write loaded Play Tracks");
        let playlist_id = PlaylistId::fake(1);
        candidate
            .write(CandidateBatch::Playlists(vec![PlaylistSnapshot {
                playlist: Playlist {
                    id: playlist_id.clone(),
                    name: "Repeated order".to_string(),
                    image_ref: None,
                },
                entries: vec![
                    PlaylistEntry {
                        occurrence_id: "beta-first".to_string(),
                        track_id: TrackId::fake(2),
                    },
                    PlaylistEntry {
                        occurrence_id: "alpha".to_string(),
                        track_id: TrackId::fake(1),
                    },
                    PlaylistEntry {
                        occurrence_id: "beta-last".to_string(),
                        track_id: TrackId::fake(2),
                    },
                ],
            }]))
            .expect("write loaded Play Playlist");
        let loaded = candidate
            .finish(
                CandidateFinish {
                    freshness: None,
                    home: HomeFacts::RufinDefined,
                    accepted_at: 1,
                },
                None,
            )
            .and_then(|prepared| prepared.accept())
            .expect("accept loaded Play candidate")
            .library;
        let request = LoadedPlayRequest::context(
            source_id.clone(),
            SourceSessionEpoch::new(1),
            loaded.playlist_track_selection(&playlist_id),
            0,
            QueuePlacement::Now,
            "tracks",
            false,
        )
        .expect("loaded Play request");

        let (playback, _) = Playback::start(
            Sequence::new(source_id.clone()),
            SourceSessionEpoch::new(1),
            "loaded-play-test",
            PlaybackSettings::default(),
            false,
            2,
            playback::PlaybackOutput::Local,
            Box::<AcceptingBackend>::default(),
            Arc::new(|| ClockSample {
                monotonic_millis: 0,
                unix_seconds: 0,
                local_period: "1970-01".to_string(),
            }),
            |_| {},
        )
        .expect("start Playback");
        (directory, source_id, loaded, request, playback)
    }

    fn selected_runtime(
        configuration: SourceConfiguration,
        loaded: Arc<Library>,
    ) -> SelectedSourceState {
        SelectedSourceState {
            configuration,
            source: None,
            source_session_epoch: SourceSessionEpoch::new(1),
            library: loaded,
            home: Arc::new(HomeSnapshot::default()),
            music_folder_id: None,
        }
    }

    fn replacement_loaded(directory: &tempfile::TempDir, source_id: &SourceId) -> Arc<Library> {
        let library =
            Libraries::open(directory.path().join("library.db")).expect("reopen loaded Play Store");
        let mut candidate = library
            .begin_source_candidate(CandidateHeader {
                source_id: source_id.clone(),
                input_digest: [2; 32],
            })
            .expect("begin replacement candidate");
        candidate
            .write(CandidateBatch::Tracks(vec![track(3, "Gamma")]))
            .expect("write replacement Track");
        candidate
            .finish(
                CandidateFinish {
                    freshness: None,
                    home: HomeFacts::RufinDefined,
                    accepted_at: 2,
                },
                None,
            )
            .and_then(|prepared| prepared.accept())
            .expect("accept replacement candidate")
            .library
    }

    #[test]
    fn deferred_loaded_order_is_admitted_and_materialized_off_the_caller_thread() {
        let (_directory, source_id, loaded, request, playback) = loaded_play_fixture();
        assert!(matches!(
            &request.tracks,
            playback::LoadedTrackSelection::Shallow(_)
        ));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("queue intent runtime");
        let worker = QueueIntentWorker::new(runtime.handle().clone());
        worker.select(SourceSessionEpoch::new(1));
        assert_ne!(worker.thread_id(), thread::current().id());
        worker.submit_loaded(playback.clone(), request);
        worker.drain();

        let page = playback
            .queue_page(QueuePageQuery::current())
            .expect("materialized queue");
        assert_eq!(page.total, 3);
        assert_eq!(page.current_absolute_index, Some(0));
        assert_eq!(
            page.rows
                .iter()
                .map(|row| row.entry.track.id.clone())
                .collect::<Vec<_>>(),
            [TrackId::fake(2), TrackId::fake(1), TrackId::fake(2)]
        );
        assert!(matches!(
            page.rows[0].entry.provenance,
            Provenance::Context {
                ref context_id,
                source_rank: 0
            } if context_id == "tracks"
        ));
        assert!(matches!(
            page.rows[1].entry.provenance,
            Provenance::Context {
                ref context_id,
                source_rank: 1
            } if context_id == "tracks"
        ));
        assert!(matches!(
            page.rows[2].entry.provenance,
            Provenance::Context {
                ref context_id,
                source_rank: 2
            } if context_id == "tracks"
        ));
        let occurrences = page
            .rows
            .iter()
            .map(|row| row.entry.occurrence.clone())
            .collect::<Vec<_>>();
        playback
            .command(SessionCommand::Activate(occurrences[1].clone()))
            .expect("activate another occurrence");
        let replay = LoadedPlayRequest::context(
            source_id,
            SourceSessionEpoch::new(1),
            loaded.playlist_track_selection(&PlaylistId::fake(1)),
            0,
            QueuePlacement::Now,
            "tracks",
            false,
        )
        .expect("replay loaded context");
        worker.submit_loaded(playback.clone(), replay);
        worker.drain();
        let replayed = playback
            .queue_page(QueuePageQuery::current())
            .expect("reactivated queue");
        assert_eq!(
            replayed
                .rows
                .iter()
                .map(|row| row.entry.occurrence.clone())
                .collect::<Vec<_>>(),
            occurrences
        );
        assert_eq!(replayed.current_absolute_index, Some(0));

        drop(worker);
        playback.shutdown().expect("stop Playback");
    }

    #[test]
    fn retired_queue_intents_release_the_previous_loaded_library() {
        let (_directory, _source_id, loaded, request, playback) = loaded_play_fixture();
        let previous = Arc::downgrade(&loaded);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("queue intent runtime");
        let worker = QueueIntentWorker::new(runtime.handle().clone());
        worker.select(SourceSessionEpoch::new(1));

        let release = worker.hold();
        worker.submit_loaded(playback.clone(), request);
        drop(loaded);
        assert!(previous.upgrade().is_some());

        worker.selected_epoch.store(0, Ordering::Release);
        release.recv().expect("release queue intent worker");
        worker.drain();
        assert!(
            previous.upgrade().is_none(),
            "retired loaded Play work must not retain the previous Library"
        );

        drop(worker);
        playback.shutdown().expect("stop Playback");
    }

    #[test]
    fn newer_random_click_reserves_after_an_older_loaded_click() {
        let (directory, source_id, loaded, request, playback) = loaded_play_fixture();
        let configuration =
            SourceConfiguration::local(source_id, "Local", vec![directory.path().to_path_buf()])
                .expect("Local source configuration");
        let selected = ActiveSource::fixed_for_test(selected_runtime(configuration, loaded));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("queue intent runtime");
        let worker = QueueIntentWorker::new(runtime.handle().clone());
        worker.select(SourceSessionEpoch::new(1));

        let release = worker.hold();
        worker.submit_loaded(playback.clone(), request);
        let task = worker.submit_random_observed(
            SourceSessionEpoch::new(1),
            selected.downgrade(),
            playback.clone(),
            RandomPlayRequest {
                placement: QueuePlacement::Now,
                criteria: library::RandomCriteria {
                    limit: 1,
                    min_year: None,
                    max_year: None,
                    genre_id: None,
                    genre_name: None,
                    played_filter: library::PlayedFilter::All,
                },
            },
        );
        release.recv().expect("release queue intent worker");
        let task = task
            .recv()
            .expect("random intent reached the worker")
            .expect("random intent reserved queue work");
        runtime.block_on(task).expect("complete random intent");
        worker.drain();

        let page = playback
            .queue_page(QueuePageQuery::current())
            .expect("materialized queue");
        assert_eq!(page.total, 1);
        assert_eq!(page.current_absolute_index, Some(0));
        assert!(matches!(page.rows[0].entry.provenance, Provenance::Random));

        drop(worker);
        playback.shutdown().expect("stop Playback");
    }

    #[test]
    fn add_last_radio_includes_a_selected_track_that_is_not_current() {
        let (directory, source_id, loaded, request, playback) = loaded_play_fixture();
        play_loaded_selection(&playback, request);
        let configuration =
            SourceConfiguration::local(source_id, "Local", vec![directory.path().to_path_buf()])
                .expect("Local source configuration");
        let selected = ActiveSource::fixed_for_test(selected_runtime(configuration, loaded));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("radio runtime");
        let task = crate::radio::play_radio(
            runtime.handle().clone(),
            selected.downgrade(),
            playback.clone(),
            RadioPlayRequest::last(RadioSeed::Track(TrackId::fake(1))),
        )
        .expect("radio intent reserved queue work");
        runtime.block_on(task).expect("complete radio intent");

        let page = playback
            .queue_page(QueuePageQuery::current())
            .expect("queue with appended radio");
        let first_radio = page
            .rows
            .iter()
            .find(|row| matches!(row.entry.provenance, Provenance::Radio))
            .expect("appended radio entries");
        assert_eq!(first_radio.entry.track.id, TrackId::fake(1));

        playback.shutdown().expect("stop Playback");
    }

    #[test]
    fn random_uses_the_current_same_source_library() {
        let (directory, source_id, loaded, request, playback) = loaded_play_fixture();
        drop(request);
        let configuration = SourceConfiguration::local(
            source_id.clone(),
            "Local",
            vec![directory.path().to_path_buf()],
        )
        .expect("Local source configuration");
        let selected = ActiveSource::fixed_for_test(selected_runtime(
            configuration.clone(),
            Arc::clone(&loaded),
        ));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("random runtime");
        let task = crate::radio::play_random(
            runtime.handle().clone(),
            selected.downgrade(),
            playback.clone(),
            RandomPlayRequest {
                placement: QueuePlacement::Now,
                criteria: library::RandomCriteria {
                    limit: 1,
                    min_year: None,
                    max_year: None,
                    genre_id: None,
                    genre_name: None,
                    played_filter: library::PlayedFilter::All,
                },
            },
        )
        .expect("random intent reserved queue work");
        let replacement = replacement_loaded(&directory, &source_id);
        selected.replace_for_test(selected_runtime(configuration, replacement));
        runtime.block_on(task).expect("complete random intent");

        let page = playback
            .queue_page(QueuePageQuery::current())
            .expect("replacement random queue");
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0].entry.track.id, TrackId::fake(3));

        playback.shutdown().expect("stop Playback");
    }

    #[test]
    fn radio_does_not_compose_from_a_replaced_library() {
        let (directory, source_id, loaded, request, playback) = loaded_play_fixture();
        drop(request);
        let configuration = SourceConfiguration::local(
            source_id.clone(),
            "Local",
            vec![directory.path().to_path_buf()],
        )
        .expect("Local source configuration");
        let selected = ActiveSource::fixed_for_test(selected_runtime(
            configuration.clone(),
            Arc::clone(&loaded),
        ));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("radio runtime");
        let task = crate::radio::play_radio(
            runtime.handle().clone(),
            selected.downgrade(),
            playback.clone(),
            RadioPlayRequest::now(RadioSeed::Track(TrackId::fake(1))),
        )
        .expect("radio intent reserved queue work");
        let replacement = replacement_loaded(&directory, &source_id);
        selected.replace_for_test(selected_runtime(configuration, replacement));
        runtime.block_on(task).expect("complete radio intent");

        assert_eq!(
            playback
                .queue_page(QueuePageQuery::current())
                .expect("unchanged radio queue")
                .total,
            0
        );

        playback.shutdown().expect("stop Playback");
    }

    #[test]
    fn pending_random_does_not_retain_a_retired_library() {
        let (directory, source_id, loaded, request, playback) = loaded_play_fixture();
        drop(request);
        let configuration =
            SourceConfiguration::local(source_id, "Local", vec![directory.path().to_path_buf()])
                .expect("Local source configuration");
        let selected =
            ActiveSource::fixed_for_test(selected_runtime(configuration, Arc::clone(&loaded)));
        let retired = Arc::downgrade(&loaded);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("random runtime");
        let task = crate::radio::play_random(
            runtime.handle().clone(),
            selected.downgrade(),
            playback.clone(),
            RandomPlayRequest {
                placement: QueuePlacement::Now,
                criteria: library::RandomCriteria {
                    limit: 1,
                    min_year: None,
                    max_year: None,
                    genre_id: None,
                    genre_name: None,
                    played_filter: library::PlayedFilter::All,
                },
            },
        )
        .expect("random intent reserved queue work");

        drop(selected);
        drop(loaded);
        assert!(retired.upgrade().is_none());
        runtime
            .block_on(task)
            .expect("finish retired random intent");
        assert_eq!(
            playback
                .queue_page(QueuePageQuery::current())
                .expect("unchanged queue")
                .total,
            0
        );

        playback.shutdown().expect("stop Playback");
    }

    #[test]
    fn missing_verified_local_file_survives_an_unavailable_native_stream() {
        let directory = tempfile::tempdir().expect("temporary Local Store");
        let missing = directory.path().join("missing.mp3");
        let missing_text = missing.to_string_lossy().into_owned();
        let source_id = SourceId::fake(9);
        let mut local_track = track(9, "Missing");
        local_track.source_path = Some(missing_text.clone());
        let library =
            Libraries::open(directory.path().join("library.db")).expect("open Local Store");
        let mut candidate = library
            .begin_source_candidate(CandidateHeader {
                source_id: source_id.clone(),
                input_digest: [9; 32],
            })
            .expect("begin Local candidate");
        candidate
            .write(CandidateBatch::Tracks(vec![local_track]))
            .expect("write Local Track");
        candidate
            .write(CandidateBatch::LocalFiles(vec![LocalFile {
                path: missing_text.clone(),
                root: directory.path().to_string_lossy().into_owned(),
                relative_path: "missing.mp3".to_string(),
                kind: LocalFileKind::Media,
                size_bytes: Some(1),
                mtime_ns: 1,
                device_id: None,
                inode: None,
                parse_version: Some(1),
                state: LocalFileState::Accepted,
                dependencies: Vec::new(),
            }]))
            .expect("write Local file");
        let loaded = candidate
            .finish(
                CandidateFinish {
                    freshness: None,
                    home: HomeFacts::RufinDefined,
                    accepted_at: 1,
                },
                None,
            )
            .and_then(|prepared| prepared.accept())
            .expect("accept Local candidate")
            .library;
        let configuration =
            SourceConfiguration::local(source_id, "Local", vec![directory.path().to_path_buf()])
                .expect("Local source configuration");
        let source = Arc::new(Source::open(configuration, None, None).expect("open Local source"));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("stream runtime");
        let error = runtime
            .block_on(prepare_stream(
                Some(loaded),
                Some(source),
                StreamRequest::new(TrackId::fake(9), library::StreamQuality::Original),
            ))
            .expect_err("missing Local file");

        assert_eq!(
            error,
            format!("the local playback file is missing: {}", missing.display())
        );
    }

    fn track(number: u32, title: &str) -> Track {
        Track::new(TrackData {
            id: TrackId::fake(number),
            album_id: None,
            title: title.to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            album_artwork: None,
            year: 2026,
            release_date: None,
            date_added: None,
            last_played: None,
            play_count: None,
            user_rating: None,
            duration_seconds: 180,
            favorite: false,
            disc_number: 1,
            track_number: u16::try_from(number).expect("test Track number fits"),
            image_ref: None,
            local_artwork: None,
            musicbrainz_recording_id: None,
            musicbrainz_release_track_id: None,
            source_path: None,
            cue: None,
            source_format: None,
            comment: None,
            skip_count: None,
            bpm: None,
            relations: TrackRelations::default(),
        })
    }
}
