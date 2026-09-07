use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use thiserror::Error;

use crate::{
    BackendEvent, BackendFailure, Batch, ClockSample, MaterializationId,
    MaterializationReservation, Placement, PlayRequest, PlaybackBackend, PlaybackNotice,
    PlaybackOutput as SelectedPlaybackOutput, PlaybackProjection, PlaybackSession,
    PlaybackSettings, PreparedStream, QueueItem, RunId, Sequence, SequenceError, SessionCommand,
    SessionEffect, SessionUpdate,
};

const BACKEND_POLL_INTERVAL: Duration = Duration::from_millis(33);

#[derive(Clone, Debug)]
pub struct QueuePersistence {
    revision: u64,
    state: library::QueueRestore,
    kind: crate::QueuePersistenceKind,
}

impl QueuePersistence {
    pub(crate) fn capture(sequence: &Sequence, kind: crate::QueuePersistenceKind) -> Self {
        Self {
            revision: sequence.revision(),
            state: sequence.snapshot(),
            kind,
        }
    }
    pub fn coalesce(&mut self, newer: Self) {
        if newer.revision >= self.revision {
            let kind = self.kind.max(newer.kind);
            *self = newer;
            self.kind = kind;
        }
    }
    pub fn state(&self) -> &library::QueueRestore {
        &self.state
    }
    pub fn kind(&self) -> crate::QueuePersistenceKind {
        self.kind
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub fn current(&self) -> Option<&crate::OccurrenceId> {
        self.state.current()
    }
    pub fn progress_millis(&self) -> u64 {
        self.state.progress_millis.max(0) as u64
    }
    pub const fn repeat_mode(&self) -> crate::RepeatMode {
        self.state.repeat_mode
    }
    pub const fn shuffled(&self) -> bool {
        self.state.shuffled
    }
}

#[derive(Debug, Error)]
pub enum PlaybackError {
    #[error("playback runtime state is unavailable")]
    Unavailable,
    #[error("could not start the playback worker: {0}")]
    WorkerStart(String),
    #[error("the playback worker stopped unexpectedly")]
    WorkerStopped,
    #[error("could not stop the playback backend: {0}")]
    BackendShutdown(String),
    #[error(transparent)]
    Sequence(#[from] SequenceError),
}

pub type PlaybackResult<T> = Result<T, PlaybackError>;

#[derive(Debug, Default)]
pub struct PlaybackUpdate {
    pub queue_persistence: Option<QueuePersistence>,
    pub projection: Option<PlaybackProjection>,
    pub effects: Vec<SessionEffect>,
    pub current_media_changed: bool,
    pub queue_changed: bool,
    pub visualizer: Option<(RunId, Vec<f64>)>,
}

impl PlaybackUpdate {
    pub fn is_empty(&self) -> bool {
        self.queue_persistence.is_none()
            && self.projection.is_none()
            && self.effects.is_empty()
            && !self.current_media_changed
            && !self.queue_changed
            && self.visualizer.is_none()
    }

    fn merge(&mut self, mut newer: Self) {
        if let Some(next) = newer.queue_persistence.take() {
            if let Some(current) = self.queue_persistence.as_mut() {
                current.coalesce(next);
            } else {
                self.queue_persistence = Some(next);
            }
        }
        match (&mut self.projection, newer.projection.take()) {
            (Some(current), Some(mut next)) => {
                let mut notices = std::mem::take(&mut current.notices);
                notices.append(&mut next.notices);
                next.notices = notices;
                self.projection = Some(next);
            }
            (None, Some(next)) => self.projection = Some(next),
            _ => {}
        }
        self.effects.append(&mut newer.effects);
        self.current_media_changed |= newer.current_media_changed;
        self.queue_changed |= newer.queue_changed;
        if newer.visualizer.is_some() {
            self.visualizer = newer.visualizer;
        }
    }
}

/// Playback's serialized command edge and ordered output stream.
///
/// The session and backend are kept on one thread so a stream completion
/// cannot publish ahead of an earlier GTK command. Rufin consumes each
/// [`PlaybackUpdate`] in this order and applies persistence, Source, and UI
/// effects without creating a second playback-state owner.
#[derive(Clone)]
pub struct Playback {
    inner: Arc<PlaybackInner>,
}

struct PlaybackInner {
    commands: SyncSender<RuntimeCommand>,
    threads: Mutex<Option<(JoinHandle<()>, JoinHandle<()>)>>,
}

type Reply<T> = SyncSender<PlaybackResult<T>>;
type Clock = Arc<dyn Fn() -> ClockSample + Send + Sync>;

enum RuntimeCommand {
    Session {
        command: SessionCommand,
        reply: Reply<()>,
    },
    CompleteQueue {
        id: u64,
        result: Box<Result<library::QueueReadPage, String>>,
        reply: Reply<bool>,
    },
    AdmitPlay {
        activation: Option<(String, String, usize)>,
        placement: Placement,
        reply: Reply<Option<MaterializationReservation>>,
    },
    ReserveMaterialization {
        placement: Placement,
        reply: Reply<MaterializationReservation>,
    },
    CompleteMaterialization {
        id: MaterializationId,
        batch: Batch,
        placement: Placement,
        reply: Reply<bool>,
    },
    FailMaterialization {
        id: MaterializationId,
        placement: Placement,
        message: String,
        reply: Reply<bool>,
    },
    CancelMaterialization {
        id: MaterializationId,
        placement: Placement,
        reply: Reply<bool>,
    },
    ResolveStream {
        run: RunId,
        stream: Result<PreparedStream, String>,
        reply: Reply<()>,
    },
    CompleteAutoDj {
        seed_occurrence: crate::OccurrenceId,
        candidates: Vec<QueueItem>,
        requested_count: usize,
        shuffle_seed: u64,
        reply: Reply<bool>,
    },
    AutoDjUnavailable {
        seed_occurrence: crate::OccurrenceId,
        error: Option<String>,
        reply: Reply<bool>,
    },
    CurrentMedia {
        reply: Reply<Option<Arc<crate::CurrentMedia>>>,
    },
    Projection {
        reply: Reply<PlaybackProjection>,
    },
    ReplaceBackend {
        output: SelectedPlaybackOutput,
        backend: Box<dyn PlaybackBackend>,
        reply: Reply<()>,
    },
    Retire {
        reply: Reply<()>,
    },
    Shutdown {
        reply: Reply<()>,
    },
}

#[expect(
    clippy::large_enum_variant,
    reason = "playback updates stay inline so the frequent output path does not allocate"
)]
enum PlaybackOutput {
    Update(PlaybackUpdate),
    Fence(SyncSender<()>),
    Shutdown,
}

/// Playback's one live queue, transport session, and physical backend.
///
/// Callers send typed operations through this handle. Playback owns its clock,
/// backend polling cadence, and ordered output worker; callers cannot mutate
/// the session or drive the backend through a second path.
impl Playback {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        sequence: Sequence,
        play_id_prefix: impl Into<Arc<str>>,
        settings: PlaybackSettings,
        auto_dj_enabled: bool,
        auto_dj_refill_threshold: usize,
        playback_output: SelectedPlaybackOutput,
        backend: Box<dyn PlaybackBackend>,
        clock: Clock,
        consume: impl FnMut(PlaybackUpdate) + Send + 'static,
    ) -> PlaybackResult<(Self, PlaybackProjection)> {
        let runtime = PlaybackRuntime::new(
            sequence,
            play_id_prefix,
            settings,
            auto_dj_enabled,
            auto_dj_refill_threshold,
            playback_output,
            backend,
        );
        let initial_projection = runtime.initial_projection();
        let (commands, command_receiver) = sync_channel(0);
        let (outputs, output_receiver) = sync_channel(0);
        let output_thread = thread::Builder::new()
            .name("rufin-playback-output".to_string())
            .spawn(move || run_playback_outputs(output_receiver, consume))
            .map_err(|error| PlaybackError::WorkerStart(error.to_string()))?;
        let actor_thread = match thread::Builder::new()
            .name("rufin-playback".to_string())
            .spawn(move || run_playback(runtime, command_receiver, outputs, clock))
        {
            Ok(thread) => thread,
            Err(error) => {
                let _ = output_thread.join();
                return Err(PlaybackError::WorkerStart(error.to_string()));
            }
        };
        Ok((
            Self {
                inner: Arc::new(PlaybackInner {
                    commands,
                    threads: Mutex::new(Some((actor_thread, output_thread))),
                }),
            },
            initial_projection,
        ))
    }

    pub fn command(&self, command: SessionCommand) -> PlaybackResult<()> {
        self.request(|reply| RuntimeCommand::Session { command, reply })
    }
    /// Acknowledges the read after its accepted state reaches the output consumer.
    pub fn complete_queue(
        &self,
        id: u64,
        result: Result<library::QueueReadPage, String>,
    ) -> PlaybackResult<bool> {
        self.request(|reply| RuntimeCommand::CompleteQueue {
            id,
            result: Box::new(result),
            reply,
        })
    }

    pub fn admit_play(
        &self,
        request: &PlayRequest,
    ) -> PlaybackResult<Option<MaterializationReservation>> {
        self.request(|reply| RuntimeCommand::AdmitPlay {
            activation: request.activation_context(),
            placement: request.placement,
            reply,
        })
    }

    pub fn reserve_materialization(
        &self,
        placement: Placement,
    ) -> PlaybackResult<MaterializationReservation> {
        self.request(|reply| RuntimeCommand::ReserveMaterialization { placement, reply })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn complete_materialization(
        &self,
        id: MaterializationId,
        batch: Batch,
        placement: Placement,
    ) -> PlaybackResult<bool> {
        self.request(|reply| RuntimeCommand::CompleteMaterialization {
            id,
            batch,
            placement,
            reply,
        })
    }

    pub fn fail_materialization(
        &self,
        id: MaterializationId,
        placement: Placement,
        message: String,
    ) -> PlaybackResult<bool> {
        self.request(|reply| RuntimeCommand::FailMaterialization {
            id,
            placement,
            message,
            reply,
        })
    }

    pub fn cancel_materialization(
        &self,
        id: MaterializationId,
        placement: Placement,
    ) -> PlaybackResult<bool> {
        self.request(|reply| RuntimeCommand::CancelMaterialization {
            id,
            placement,
            reply,
        })
    }

    pub fn resolve_stream(
        &self,
        run: RunId,
        stream: Result<PreparedStream, String>,
    ) -> PlaybackResult<()> {
        self.request(|reply| RuntimeCommand::ResolveStream { run, stream, reply })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn complete_auto_dj_candidates(
        &self,
        seed_occurrence: crate::OccurrenceId,
        candidates: Vec<QueueItem>,
        requested_count: usize,
        shuffle_seed: u64,
    ) -> PlaybackResult<bool> {
        self.request(|reply| RuntimeCommand::CompleteAutoDj {
            seed_occurrence,
            candidates,
            requested_count,
            shuffle_seed,
            reply,
        })
    }

    pub fn auto_dj_unavailable(
        &self,
        seed_occurrence: crate::OccurrenceId,
        error: Option<String>,
    ) -> PlaybackResult<bool> {
        self.request(|reply| RuntimeCommand::AutoDjUnavailable {
            seed_occurrence,
            error,
            reply,
        })
    }

    pub fn current_media(&self) -> PlaybackResult<Option<Arc<crate::CurrentMedia>>> {
        self.request(|reply| RuntimeCommand::CurrentMedia { reply })
    }

    pub fn projection(&self) -> PlaybackResult<PlaybackProjection> {
        self.request(|reply| RuntimeCommand::Projection { reply })
    }

    pub fn replace_backend(
        &self,
        output: SelectedPlaybackOutput,
        backend: Box<dyn PlaybackBackend>,
    ) -> PlaybackResult<()> {
        self.request(|reply| RuntimeCommand::ReplaceBackend {
            output,
            backend,
            reply,
        })
    }

    /// Ends the logical session and lets Playback finish shutting down its backend.
    pub fn retire(&self) -> PlaybackResult<()> {
        self.request(|reply| RuntimeCommand::Retire { reply })
    }

    pub fn shutdown(&self) -> PlaybackResult<()> {
        let result = self.request(|reply| RuntimeCommand::Shutdown { reply });
        let joined = self.inner.join_threads();
        result.and(joined)
    }

    fn request<T>(&self, command: impl FnOnce(Reply<T>) -> RuntimeCommand) -> PlaybackResult<T> {
        let (reply, response) = sync_channel(0);
        self.inner
            .commands
            .send(command(reply))
            .map_err(|_| PlaybackError::Unavailable)?;
        response.recv().map_err(|_| PlaybackError::WorkerStopped)?
    }
}

impl PlaybackInner {
    fn join_threads(&self) -> PlaybackResult<()> {
        let Some((actor, output)) = self
            .threads
            .lock()
            .map_err(|_| PlaybackError::Unavailable)?
            .take()
        else {
            return Ok(());
        };
        actor.join().map_err(|_| PlaybackError::WorkerStopped)?;
        output.join().map_err(|_| PlaybackError::WorkerStopped)?;
        Ok(())
    }
}

fn run_playback(
    mut runtime: PlaybackRuntime,
    commands: Receiver<RuntimeCommand>,
    outputs: SyncSender<PlaybackOutput>,
    clock: Clock,
) {
    loop {
        match commands.recv_timeout(BACKEND_POLL_INTERVAL) {
            Ok(command) => {
                if !apply_runtime_command(&mut runtime, command, &outputs, &clock) {
                    break;
                }
                let sample = clock();
                if runtime
                    .poll(&sample)
                    .and_then(|update| publish_update(&outputs, update))
                    .is_err()
                {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let sample = clock();
                if runtime
                    .poll(&sample)
                    .and_then(|update| publish_update(&outputs, update))
                    .is_err()
                {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                let sample = clock();
                if let Ok(update) = runtime.shutdown(&sample) {
                    let _ = publish_update(&outputs, update);
                }
                let _ = outputs.send(PlaybackOutput::Shutdown);
                break;
            }
        }
    }
}

fn apply_runtime_command(
    runtime: &mut PlaybackRuntime,
    command: RuntimeCommand,
    outputs: &SyncSender<PlaybackOutput>,
    clock: &Clock,
) -> bool {
    let sample = clock();
    match command {
        RuntimeCommand::Session { command, reply } => {
            reply_update(runtime.command(command, &sample), outputs, reply);
        }
        RuntimeCommand::CompleteQueue { id, result, reply } => {
            let accepted = runtime.session.accepts_queue(id);
            let value = if accepted {
                runtime
                    .command(SessionCommand::QueueComplete { id, result }, &sample)
                    .and_then(|update| {
                        publish_update(outputs, update)?;
                        Ok(true)
                    })
            } else {
                Ok(false)
            };
            let value = value.and_then(|accepted| {
                fence_outputs(outputs)?;
                Ok(accepted)
            });
            let _ = reply.send(value);
        }
        RuntimeCommand::AdmitPlay {
            activation,
            placement,
            reply,
        } => {
            let value = runtime.admit_play(activation, placement, &sample).and_then(
                |(reservation, update)| {
                    publish_optional_update(outputs, update)?;
                    Ok(reservation)
                },
            );
            let _ = reply.send(value);
        }
        RuntimeCommand::ReserveMaterialization { placement, reply } => {
            let _ = reply.send(runtime.reserve_materialization(placement));
        }
        RuntimeCommand::CompleteMaterialization {
            id,
            batch,
            placement,
            reply,
        } => {
            let value = runtime
                .complete_materialization(id, batch, placement, &sample)
                .and_then(|update| publish_optional_update(outputs, update));
            let _ = reply.send(value);
        }
        RuntimeCommand::FailMaterialization {
            id,
            placement,
            message,
            reply,
        } => {
            let value = runtime
                .fail_materialization(id, placement, message, &sample)
                .and_then(|update| publish_optional_update(outputs, update));
            let _ = reply.send(value);
        }
        RuntimeCommand::CancelMaterialization {
            id,
            placement,
            reply,
        } => {
            let _ = reply.send(runtime.cancel_materialization(id, placement));
        }
        RuntimeCommand::ResolveStream { run, stream, reply } => {
            reply_update(runtime.resolve_stream(run, stream, &sample), outputs, reply);
        }
        RuntimeCommand::CompleteAutoDj {
            seed_occurrence,
            candidates,
            requested_count,
            shuffle_seed,
            reply,
        } => {
            let value = runtime
                .complete_auto_dj_candidates(
                    &seed_occurrence,
                    candidates,
                    requested_count,
                    shuffle_seed,
                    &sample,
                )
                .and_then(|update| publish_optional_update(outputs, update));
            let _ = reply.send(value);
        }
        RuntimeCommand::AutoDjUnavailable {
            seed_occurrence,
            error,
            reply,
        } => {
            let value = runtime
                .auto_dj_unavailable(&seed_occurrence, error, &sample)
                .and_then(|update| publish_optional_update(outputs, update));
            let _ = reply.send(value);
        }
        RuntimeCommand::CurrentMedia { reply } => {
            let _ = reply.send(runtime.current_media());
        }
        RuntimeCommand::Projection { reply } => {
            let _ = reply.send(Ok(runtime.initial_projection()));
        }
        RuntimeCommand::ReplaceBackend {
            output,
            backend,
            reply,
        } => {
            reply_update(
                runtime.replace_backend(output, backend, &sample),
                outputs,
                reply,
            );
        }
        RuntimeCommand::Retire { reply } => {
            let mut value = runtime
                .retire(&sample)
                .and_then(|update| publish_update(outputs, update));
            if outputs.send(PlaybackOutput::Shutdown).is_err() && value.is_ok() {
                value = Err(PlaybackError::Unavailable);
            }
            let _ = reply.send(value);
            let _ = runtime.shutdown_backend();
            return false;
        }
        RuntimeCommand::Shutdown { reply } => {
            let mut value = runtime
                .shutdown(&sample)
                .and_then(|update| publish_update(outputs, update));
            if outputs.send(PlaybackOutput::Shutdown).is_err() && value.is_ok() {
                value = Err(PlaybackError::Unavailable);
            }
            let _ = reply.send(value);
            return false;
        }
    }
    true
}

fn reply_update(
    value: PlaybackResult<PlaybackUpdate>,
    outputs: &SyncSender<PlaybackOutput>,
    reply: Reply<()>,
) {
    let _ = reply.send(value.and_then(|update| publish_update(outputs, update)));
}

fn publish_optional_update(
    outputs: &SyncSender<PlaybackOutput>,
    update: Option<PlaybackUpdate>,
) -> PlaybackResult<bool> {
    let Some(update) = update else {
        return Ok(false);
    };
    publish_update(outputs, update)?;
    Ok(true)
}

fn publish_update(
    outputs: &SyncSender<PlaybackOutput>,
    update: PlaybackUpdate,
) -> PlaybackResult<()> {
    if update.is_empty() {
        return Ok(());
    }
    let flushes_persistence = update
        .effects
        .iter()
        .any(|effect| matches!(effect, SessionEffect::FlushPersistence { .. }));
    outputs
        .send(PlaybackOutput::Update(update))
        .map_err(|_| PlaybackError::Unavailable)?;
    if flushes_persistence {
        fence_outputs(outputs)?;
    }
    Ok(())
}

fn fence_outputs(outputs: &SyncSender<PlaybackOutput>) -> PlaybackResult<()> {
    let (fence, crossed) = sync_channel(0);
    outputs
        .send(PlaybackOutput::Fence(fence))
        .map_err(|_| PlaybackError::Unavailable)?;
    crossed.recv().map_err(|_| PlaybackError::WorkerStopped)
}

fn run_playback_outputs(
    outputs: Receiver<PlaybackOutput>,
    mut consume: impl FnMut(PlaybackUpdate),
) {
    while let Ok(output) = outputs.recv() {
        match output {
            PlaybackOutput::Update(update) => consume(update),
            PlaybackOutput::Fence(crossed) => {
                let _ = crossed.send(());
            }
            PlaybackOutput::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;

    struct IdleBackend;

    impl PlaybackBackend for IdleBackend {
        fn send(&mut self, _: crate::BackendCommand) -> Result<(), crate::BackendError> {
            Ok(())
        }

        fn drain_events(&mut self) -> Vec<BackendEvent> {
            Vec::new()
        }
    }

    async fn published_queue_snapshot(
        playback: &Playback,
        updates: &Receiver<PlaybackUpdate>,
        database: &library::Database,
    ) -> QueuePersistence {
        loop {
            let update = updates.recv_timeout(Duration::from_secs(5)).unwrap();
            for effect in update.effects {
                if let SessionEffect::Queue { id, request } = effect {
                    let result = database
                        .read_queue(request)
                        .await
                        .map_err(|error| error.to_string());
                    assert!(playback.complete_queue(id, result).unwrap());
                }
            }
            if let Some(snapshot) = update.queue_persistence {
                return snapshot;
            }
        }
    }

    fn replace_queue(playback: &Playback, prefix: &str, count: usize) {
        let request = PlayRequest::ordered(
            library::QueueInput::Items(
                (0..count)
                    .map(|index| {
                        (
                            QueueItem::direct(
                                format!("https://example.test/{prefix}/{index}"),
                                format!("{prefix} {index}"),
                                "Artist",
                                "Album",
                                180_000,
                            ),
                            crate::Provenance::Manual,
                        )
                    })
                    .collect(),
            ),
            0,
            crate::QueuePlacement::Now,
            false,
        );
        let reservation = playback.admit_play(&request).unwrap().unwrap();
        let (batch, placement) = request.compact_batch(7);
        assert!(
            playback
                .complete_materialization(reservation.id, batch, placement)
                .unwrap()
        );
    }

    #[tokio::test]
    async fn runtime_queue_edits_publish_restorable_bounded_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("queue.sqlite3"))
            .await
            .unwrap();
        let (sender, updates) = std::sync::mpsc::channel();
        let (playback, _) = Playback::start(
            Sequence::new(),
            "persistence-test",
            PlaybackSettings::default(),
            false,
            1,
            SelectedPlaybackOutput::Local,
            Box::new(IdleBackend),
            Arc::new(|| ClockSample {
                monotonic_millis: 0,
                unix_seconds: 0,
                local_period: "1970-01".into(),
            }),
            move |update| {
                let _ = sender.send(update);
            },
        )
        .unwrap();

        replace_queue(&playback, "first", 150);
        let mut pending = published_queue_snapshot(&playback, &updates, &database).await;
        assert_eq!(pending.state().entries.len(), 150);
        assert!(pending.state().occurrences.len() <= library::QUEUE_CONTEXT_LIMIT);
        database.save_queue(pending.state()).await.unwrap();
        let restored = database.restore_queue().await.unwrap();
        assert_eq!(restored.occurrences.len(), library::QUEUE_CONTEXT_LIMIT);

        replace_queue(&playback, "replacement", 3);
        pending.coalesce(published_queue_snapshot(&playback, &updates, &database).await);
        database.save_queue(pending.state()).await.unwrap();
        let restored = database.restore_queue().await.unwrap();
        assert_eq!(restored.occurrences.len(), 3);
        assert!(
            restored
                .occurrences
                .iter()
                .all(|row| row.media_uri.contains("/replacement/"))
        );
        assert_eq!(restored.current_index, Some(0));

        playback
            .command(SessionCommand::Remove(
                restored.occurrences[2].occurrence.clone(),
            ))
            .unwrap();
        let snapshot = published_queue_snapshot(&playback, &updates, &database).await;
        database.save_queue(snapshot.state()).await.unwrap();
        assert_eq!(database.restore_queue().await.unwrap().occurrences.len(), 2);

        playback
            .command(SessionCommand::SetShuffle {
                enabled: true,
                seed: 19,
            })
            .unwrap();
        let snapshot = published_queue_snapshot(&playback, &updates, &database).await;
        database.save_queue(snapshot.state()).await.unwrap();
        assert!(database.restore_queue().await.unwrap().shuffled);

        playback.command(SessionCommand::SetVolume(0.137)).unwrap();
        let update = updates.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(update.projection.unwrap().view.controls.volume, 0.137);
        assert!(update.queue_persistence.is_none());

        playback
            .command(SessionCommand::Clear {
                include_current: true,
            })
            .unwrap();
        let snapshot = published_queue_snapshot(&playback, &updates, &database).await;
        database.save_queue(snapshot.state()).await.unwrap();
        let restored = database.restore_queue().await.unwrap();
        assert!(restored.occurrences.is_empty());
        playback.shutdown().unwrap();
    }

    #[tokio::test]
    async fn persistence_coalesces_latest_queue_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("queue.sqlite3"))
            .await
            .unwrap();
        let page = database
            .read_queue(library::QueueReadRequest::Capture {
                input: Box::new(library::QueueInput::Uris {
                    order: (1..=4)
                        .map(|key| format!("https://example.test/{key}"))
                        .collect(),
                    context_id: "test".into(),
                    source_start: 0,
                }),
                anchor_index: 0,
                random_start: None,
            })
            .await
            .unwrap();
        let mut sequence = Sequence::new();
        sequence.add_page(page, library::QueueReorderTarget::End, true, None);
        let mut pending =
            QueuePersistence::capture(&sequence, crate::QueuePersistenceKind::Membership);
        sequence.set_repeat_mode(crate::RepeatMode::All);
        sequence.set_progress_millis(42000);
        sequence.shuffle(true, 7);
        let newer = QueuePersistence::capture(&sequence, crate::QueuePersistenceKind::Membership);
        pending.coalesce(newer);
        assert_eq!(pending.revision(), 3);
        assert_eq!(pending.progress_millis(), 42000);
        assert_eq!(pending.repeat_mode(), crate::RepeatMode::All);
        assert!(pending.shuffled());
    }
}

struct PlaybackRuntime {
    requested_streams: std::collections::HashSet<RunId>,
    session: PlaybackSession,
    backend: Box<dyn PlaybackBackend>,
}

impl PlaybackRuntime {
    fn new(
        sequence: Sequence,
        play_id_prefix: impl Into<Arc<str>>,
        settings: PlaybackSettings,
        auto_dj_enabled: bool,
        auto_dj_refill_threshold: usize,
        playback_output: SelectedPlaybackOutput,
        backend: Box<dyn PlaybackBackend>,
    ) -> Self {
        Self {
            requested_streams: std::collections::HashSet::new(),
            session: PlaybackSession::new(
                sequence,
                play_id_prefix,
                settings,
                playback_output,
                auto_dj_enabled,
                auto_dj_refill_threshold,
            ),
            backend,
        }
    }

    fn initial_projection(&self) -> PlaybackProjection {
        PlaybackProjection {
            view: self.session.view(),
            notices: Vec::new(),
        }
    }

    fn command(
        &mut self,
        command: SessionCommand,
        sample: &ClockSample,
    ) -> PlaybackResult<PlaybackUpdate> {
        let update = self.session.handle_command(command, sample)?;
        self.finish(update, sample)
    }

    fn admit_play(
        &mut self,
        activation: Option<(String, String, usize)>,
        placement: Placement,
        sample: &ClockSample,
    ) -> PlaybackResult<(Option<MaterializationReservation>, Option<PlaybackUpdate>)> {
        if let Some((context_id, media_uri, source_rank)) = activation
            && let Some(update) =
                self.session
                    .activate_context(&context_id, &media_uri, source_rank, sample)
        {
            return Ok((None, Some(self.finish(update, sample)?)));
        }
        Ok((Some(self.session.reserve_materialization(placement)), None))
    }

    fn reserve_materialization(
        &mut self,
        placement: Placement,
    ) -> PlaybackResult<MaterializationReservation> {
        Ok(self.session.reserve_materialization(placement))
    }

    fn complete_materialization(
        &mut self,
        id: MaterializationId,
        batch: Batch,
        placement: Placement,
        sample: &ClockSample,
    ) -> PlaybackResult<Option<PlaybackUpdate>> {
        let update = self
            .session
            .apply_materialization(id, batch, placement, sample)?
            .map(|update| self.finish(update, sample))
            .transpose()?;
        Ok(update)
    }

    fn fail_materialization(
        &mut self,
        id: MaterializationId,
        placement: Placement,
        message: String,
        sample: &ClockSample,
    ) -> PlaybackResult<Option<PlaybackUpdate>> {
        self.session
            .fail_materialization(id, placement, message)
            .map(|update| self.finish(update, sample))
            .transpose()
    }

    fn cancel_materialization(
        &mut self,
        id: MaterializationId,
        placement: Placement,
    ) -> PlaybackResult<bool> {
        Ok(self.session.cancel_materialization(id, placement))
    }

    fn resolve_stream(
        &mut self,
        run: RunId,
        result: Result<PreparedStream, String>,
        sample: &ClockSample,
    ) -> PlaybackResult<PlaybackUpdate> {
        let update = match result {
            Ok(stream) => self.session.stream_resolved(run, stream),
            Err(error) => self.session.stream_failed(run, error, sample),
        };
        self.finish(update, sample)
    }

    fn complete_auto_dj_candidates(
        &mut self,
        seed_occurrence: &crate::OccurrenceId,
        candidates: Vec<QueueItem>,
        requested_count: usize,
        shuffle_seed: u64,
        sample: &ClockSample,
    ) -> PlaybackResult<Option<PlaybackUpdate>> {
        let update = self
            .session
            .complete_auto_dj_candidates(
                seed_occurrence,
                candidates,
                requested_count,
                shuffle_seed,
                sample,
            )?
            .map(|update| self.finish(update, sample))
            .transpose()?;
        Ok(update)
    }

    fn auto_dj_unavailable(
        &mut self,
        seed_occurrence: &crate::OccurrenceId,
        error: Option<String>,
        sample: &ClockSample,
    ) -> PlaybackResult<Option<PlaybackUpdate>> {
        let update = self
            .session
            .auto_dj_unavailable(seed_occurrence, error)
            .map(|update| self.finish(update, sample))
            .transpose()?;
        Ok(update)
    }

    fn poll(&mut self, sample: &ClockSample) -> PlaybackResult<PlaybackUpdate> {
        let events = self.backend.drain_events();
        let mut output = PlaybackUpdate::default();
        for event in events {
            let update = self.session.handle_backend(event, sample);
            output.merge(self.finish(update, sample)?);
        }
        Ok(output)
    }

    fn current_media(&self) -> PlaybackResult<Option<std::sync::Arc<crate::CurrentMedia>>> {
        Ok(self.session.view().transport.current)
    }

    fn replace_backend(
        &mut self,
        output: SelectedPlaybackOutput,
        backend: Box<dyn PlaybackBackend>,
        sample: &ClockSample,
    ) -> PlaybackResult<PlaybackUpdate> {
        let mut previous = std::mem::replace(&mut self.backend, backend);
        let mut errors = Vec::new();
        if let Some(run) = self.session.current_run()
            && let Err(error) = previous.send(crate::BackendCommand::Stop { run })
        {
            errors.push(error.to_string());
        }
        previous.drain_events();
        if let Err(error) = previous.shutdown() {
            errors.push(error.to_string());
        }
        let session_update = self.session.replace_output(output);
        let mut update = self.finish(session_update, sample)?;
        update
            .effects
            .extend(errors.into_iter().map(SessionEffect::NonfatalError));
        Ok(update)
    }

    fn retire(&mut self, sample: &ClockSample) -> PlaybackResult<PlaybackUpdate> {
        let session_update = self.session.shutdown(sample);
        self.finish(session_update, sample)
    }

    fn shutdown_backend(&mut self) -> PlaybackResult<()> {
        self.backend
            .shutdown()
            .map_err(|error| PlaybackError::BackendShutdown(error.to_string()))
    }

    fn shutdown(&mut self, sample: &ClockSample) -> PlaybackResult<PlaybackUpdate> {
        let update = self.retire(sample)?;
        self.shutdown_backend()?;
        Ok(update)
    }

    fn finish(
        &mut self,
        update: SessionUpdate,
        sample: &ClockSample,
    ) -> PlaybackResult<PlaybackUpdate> {
        let mut output = self.commit(update);
        if let Some(effect) = self.session.hydrate_queue() {
            output.effects.push(effect);
        }
        let mut backend_failures = Vec::new();
        for effect in std::mem::take(&mut output.effects) {
            match effect {
                SessionEffect::Backend(command) => {
                    let run = command.run();
                    if let Err(error) = self.backend.send(command) {
                        backend_failures.push((run, error.to_string()));
                    }
                }
                effect => output.effects.push(effect),
            }
        }
        for (run, error) in backend_failures {
            if let Some(run) = run {
                let failed = self.session.handle_backend(
                    BackendEvent::Error {
                        run,
                        error: BackendFailure::new(error),
                    },
                    sample,
                );
                output.merge(self.commit(failed));
            } else {
                output.effects.push(SessionEffect::NonfatalError(error));
            }
        }
        Ok(output)
    }

    fn commit(&mut self, update: SessionUpdate) -> PlaybackUpdate {
        let mut persistence = self.session.take_queue_persistence();
        let mut notices = Vec::new();
        let mut effects = Vec::new();
        let mut current_media_changed = false;
        let mut visualizer = None;
        for effect in update.effects {
            match effect {
                SessionEffect::PersistState { .. } | SessionEffect::PersistProgress { .. } => {
                    persistence.get_or_insert(crate::QueuePersistenceKind::State);
                }
                effect @ SessionEffect::Listening(crate::ListeningFact::Started { run, .. }) => {
                    notices.push(PlaybackNotice::RunStarted(run));
                    effects.push(effect);
                }
                SessionEffect::PositionDiscontinuity(discontinuity) => {
                    notices.push(PlaybackNotice::PositionDiscontinuity(discontinuity));
                }
                SessionEffect::Visualizer { run, levels } => {
                    visualizer = Some((run, levels));
                }
                SessionEffect::CurrentMediaChanged => {
                    current_media_changed = true;
                }
                _ => effects.push(effect),
            }
        }
        let live = self.session.stream_runs();
        effects.retain(|effect| match effect {
            SessionEffect::ResolveStream { run, .. } => live.contains(&Some(*run)),
            _ => true,
        });
        self.requested_streams.retain(|run| {
            if live.contains(&Some(*run)) {
                true
            } else {
                effects.push(SessionEffect::CancelStream(*run));
                false
            }
        });
        for effect in &effects {
            if let SessionEffect::ResolveStream { run, .. } = effect {
                self.requested_streams.insert(*run);
            }
        }
        let projection = (update.view_changed || !notices.is_empty()).then(|| PlaybackProjection {
            view: self.session.view(),
            notices,
        });
        PlaybackUpdate {
            queue_persistence: persistence
                .map(|kind| QueuePersistence::capture(self.session.sequence(), kind)),
            projection,
            effects,
            current_media_changed,
            queue_changed: update.queue_changed,
            visualizer,
        }
    }
}
