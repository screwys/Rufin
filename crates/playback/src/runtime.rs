use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use library::{SourceKey, TrackKey};
use thiserror::Error;

use crate::{
    BackendEvent, BackendFailure, Batch, ClockSample, LoadedPlayRequest, MaterializationId,
    MaterializationReservation, Placement, PlaybackBackend, PlaybackNotice,
    PlaybackOutput as SelectedPlaybackOutput, PlaybackProjection, PlaybackSession,
    PlaybackSettings, PreparedStream, RunId, Sequence, SequenceEntry, SequenceError,
    SessionCommand, SessionEffect, SessionUpdate, SourceSessionEpoch,
};

const BACKEND_POLL_INTERVAL: Duration = Duration::from_millis(33);
const QUEUE_PERSISTENCE_PAGE_LIMIT: usize = 128;

#[derive(Clone, Debug)]
pub struct QueuePersistence {
    source_key: SourceKey,
    revision: u64,
    total: usize,
    current: Option<crate::OccurrenceId>,
    prepared_next: Option<crate::OccurrenceId>,
    progress_millis: u64,
    repeat_mode: crate::RepeatMode,
    shuffled: bool,
}

impl QueuePersistence {
    pub(crate) fn capture(sequence: &Sequence) -> Self {
        Self {
            source_key: sequence.source_key(),
            revision: sequence.revision(),
            total: sequence.entries().len(),
            current: sequence.selected().map(|entry| entry.occurrence.clone()),
            prepared_next: sequence
                .peek_next_eos()
                .map(|entry| entry.occurrence.clone()),
            progress_millis: sequence.progress_millis(),
            repeat_mode: sequence.repeat_mode(),
            shuffled: sequence.shuffle_enabled(),
        }
    }
    pub fn coalesce(&mut self, newer: Self) {
        if self.source_key != newer.source_key || newer.revision < self.revision {
            return;
        }
        *self = newer;
    }
    pub const fn source_key(&self) -> SourceKey {
        self.source_key
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub const fn total(&self) -> usize {
        self.total
    }
    pub fn current(&self) -> Option<&crate::OccurrenceId> {
        self.current.as_ref()
    }
    pub fn prepared_next(&self) -> Option<&crate::OccurrenceId> {
        self.prepared_next.as_ref()
    }
    pub const fn progress_millis(&self) -> u64 {
        self.progress_millis
    }
    pub const fn repeat_mode(&self) -> crate::RepeatMode {
        self.repeat_mode
    }
    pub const fn shuffled(&self) -> bool {
        self.shuffled
    }
}

#[derive(Debug, Error)]
pub enum PlaybackError {
    #[error("playback runtime state is unavailable")]
    Unavailable,
    #[error("the loaded music selection belongs to an inactive source session")]
    InactiveSourceSession,
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
    AdmitLoaded {
        source_id: SourceKey,
        source_session_epoch: SourceSessionEpoch,
        activation: Option<(String, library::TrackKey, usize)>,
        placement: Placement,
        reply: Reply<Option<MaterializationReservation>>,
    },
    ReserveMaterialization {
        placement: Placement,
        reply: Reply<MaterializationReservation>,
    },
    CompleteMaterialization {
        id: MaterializationId,
        source_id: SourceKey,
        batch: Batch,
        placement: Placement,
        anchor: Box<Option<crate::PlaybackMedia>>,
        reply: Reply<bool>,
    },
    FailMaterialization {
        id: MaterializationId,
        source_id: SourceKey,
        placement: Placement,
        message: String,
        reply: Reply<bool>,
    },
    CancelMaterialization {
        id: MaterializationId,
        source_id: SourceKey,
        placement: Placement,
        reply: Reply<bool>,
    },
    ResolveStream {
        run: RunId,
        stream: Result<PreparedStream, String>,
        reply: Reply<()>,
    },
    CompleteAutoDj {
        source_id: SourceKey,
        seed_occurrence: crate::OccurrenceId,
        candidates: Vec<TrackKey>,
        requested_count: usize,
        shuffle_seed: u64,
        reply: Reply<bool>,
    },
    AutoDjUnavailable {
        source_id: SourceKey,
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
    QueuePersistencePage {
        revision: u64,
        offset: usize,
        limit: usize,
        reply: Reply<Option<Vec<SequenceEntry>>>,
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
        source_session_epoch: SourceSessionEpoch,
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
            source_session_epoch,
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

    pub fn admit_loaded(
        &self,
        request: &LoadedPlayRequest,
    ) -> PlaybackResult<Option<MaterializationReservation>> {
        self.request(|reply| RuntimeCommand::AdmitLoaded {
            source_id: request.source_key,
            source_session_epoch: request.source_session_epoch,
            activation: request.activation_context(),
            placement: request.placement(),
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
        source_id: SourceKey,
        batch: Batch,
        placement: Placement,
        anchor: Option<crate::PlaybackMedia>,
    ) -> PlaybackResult<bool> {
        self.request(|reply| RuntimeCommand::CompleteMaterialization {
            id,
            source_id,
            batch,
            placement,
            anchor: Box::new(anchor),
            reply,
        })
    }

    pub fn fail_materialization(
        &self,
        id: MaterializationId,
        source_id: SourceKey,
        placement: Placement,
        message: String,
    ) -> PlaybackResult<bool> {
        self.request(|reply| RuntimeCommand::FailMaterialization {
            id,
            source_id,
            placement,
            message,
            reply,
        })
    }

    pub fn cancel_materialization(
        &self,
        id: MaterializationId,
        source_id: SourceKey,
        placement: Placement,
    ) -> PlaybackResult<bool> {
        self.request(|reply| RuntimeCommand::CancelMaterialization {
            id,
            source_id,
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
        source_id: SourceKey,
        seed_occurrence: crate::OccurrenceId,
        candidates: Vec<TrackKey>,
        requested_count: usize,
        shuffle_seed: u64,
    ) -> PlaybackResult<bool> {
        self.request(|reply| RuntimeCommand::CompleteAutoDj {
            source_id,
            seed_occurrence,
            candidates,
            requested_count,
            shuffle_seed,
            reply,
        })
    }

    pub fn auto_dj_unavailable(
        &self,
        source_id: SourceKey,
        seed_occurrence: crate::OccurrenceId,
        error: Option<String>,
    ) -> PlaybackResult<bool> {
        self.request(|reply| RuntimeCommand::AutoDjUnavailable {
            source_id,
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

    pub fn queue_persistence_page(
        &self,
        revision: u64,
        offset: usize,
        limit: usize,
    ) -> PlaybackResult<Option<Vec<SequenceEntry>>> {
        self.request(|reply| RuntimeCommand::QueuePersistencePage {
            revision,
            offset,
            limit,
            reply,
        })
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
        RuntimeCommand::AdmitLoaded {
            source_id,
            source_session_epoch,
            activation,
            placement,
            reply,
        } => {
            let value = runtime
                .admit_loaded(
                    &source_id,
                    source_session_epoch,
                    activation,
                    placement,
                    &sample,
                )
                .and_then(|(reservation, update)| {
                    publish_optional_update(outputs, update)?;
                    Ok(reservation)
                });
            let _ = reply.send(value);
        }
        RuntimeCommand::ReserveMaterialization { placement, reply } => {
            let _ = reply.send(runtime.reserve_materialization(placement));
        }
        RuntimeCommand::CompleteMaterialization {
            id,
            source_id,
            batch,
            placement,
            anchor,
            reply,
        } => {
            let value = runtime
                .complete_materialization(id, &source_id, batch, placement, *anchor, &sample)
                .and_then(|update| publish_optional_update(outputs, update));
            let _ = reply.send(value);
        }
        RuntimeCommand::FailMaterialization {
            id,
            source_id,
            placement,
            message,
            reply,
        } => {
            let value = runtime
                .fail_materialization(id, &source_id, placement, message, &sample)
                .and_then(|update| publish_optional_update(outputs, update));
            let _ = reply.send(value);
        }
        RuntimeCommand::CancelMaterialization {
            id,
            source_id,
            placement,
            reply,
        } => {
            let _ = reply.send(runtime.cancel_materialization(id, &source_id, placement));
        }
        RuntimeCommand::ResolveStream { run, stream, reply } => {
            reply_update(runtime.resolve_stream(run, stream, &sample), outputs, reply);
        }
        RuntimeCommand::CompleteAutoDj {
            source_id,
            seed_occurrence,
            candidates,
            requested_count,
            shuffle_seed,
            reply,
        } => {
            let value = runtime
                .complete_auto_dj_candidates(
                    &source_id,
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
            source_id,
            seed_occurrence,
            error,
            reply,
        } => {
            let value = runtime
                .auto_dj_unavailable(&source_id, &seed_occurrence, error, &sample)
                .and_then(|update| publish_optional_update(outputs, update));
            let _ = reply.send(value);
        }
        RuntimeCommand::CurrentMedia { reply } => {
            let _ = reply.send(runtime.current_media());
        }
        RuntimeCommand::Projection { reply } => {
            let _ = reply.send(Ok(runtime.initial_projection()));
        }
        RuntimeCommand::QueuePersistencePage {
            revision,
            offset,
            limit,
            reply,
        } => {
            let sequence = runtime.session.sequence();
            let page = (sequence.revision() == revision).then(|| {
                sequence
                    .entries()
                    .iter()
                    .skip(offset)
                    .take(limit.clamp(1, QUEUE_PERSISTENCE_PAGE_LIMIT))
                    .cloned()
                    .collect()
            });
            let _ = reply.send(Ok(page));
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
    use crate::{BatchItem, Placement, Provenance};

    #[test]
    fn traversal_coalesces_into_one_pending_structural_order() {
        let mut sequence = Sequence::new(SourceKey::from_raw(1));
        sequence
            .apply_batch_with_change(
                Batch::new(
                    (1..=4)
                        .map(|key| BatchItem::new(TrackKey::from_raw(key), Provenance::Manual))
                        .collect(),
                ),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("batch");
        let mut pending = QueuePersistence::capture(&sequence);
        sequence.set_shuffle_seed(true, 7);
        let newer = QueuePersistence::capture(&sequence);
        let expected_revision = newer.revision();
        pending.coalesce(newer);
        assert_eq!(pending.revision(), expected_revision);
        assert_eq!(pending.total(), 4);
        assert!(pending.shuffled());
    }
}

struct PlaybackRuntime {
    session: PlaybackSession,
    backend: Box<dyn PlaybackBackend>,
}

impl PlaybackRuntime {
    fn new(
        sequence: Sequence,
        source_session_epoch: SourceSessionEpoch,
        play_id_prefix: impl Into<Arc<str>>,
        settings: PlaybackSettings,
        auto_dj_enabled: bool,
        auto_dj_refill_threshold: usize,
        playback_output: SelectedPlaybackOutput,
        backend: Box<dyn PlaybackBackend>,
    ) -> Self {
        Self {
            session: PlaybackSession::new(
                sequence,
                source_session_epoch,
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

    fn admit_loaded(
        &mut self,
        source_id: &SourceKey,
        source_session_epoch: SourceSessionEpoch,
        activation: Option<(String, library::TrackKey, usize)>,
        placement: Placement,
        sample: &ClockSample,
    ) -> PlaybackResult<(Option<MaterializationReservation>, Option<PlaybackUpdate>)> {
        if self.session.sequence().source_key() != *source_id
            || self.session.source_session_epoch() != source_session_epoch
        {
            return Err(PlaybackError::InactiveSourceSession);
        }
        if let Some((context_id, track_id, source_rank)) = activation
            && let Some(update) =
                self.session
                    .activate_context(&context_id, &track_id, source_rank, sample)
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
        source_id: &SourceKey,
        batch: Batch,
        placement: Placement,
        anchor: Option<crate::PlaybackMedia>,
        sample: &ClockSample,
    ) -> PlaybackResult<Option<PlaybackUpdate>> {
        let update = self
            .session
            .apply_materialization(id, source_id, batch, placement, anchor, sample)?
            .map(|update| self.finish(update, sample))
            .transpose()?;
        Ok(update)
    }

    fn fail_materialization(
        &mut self,
        id: MaterializationId,
        source_id: &SourceKey,
        placement: Placement,
        message: String,
        sample: &ClockSample,
    ) -> PlaybackResult<Option<PlaybackUpdate>> {
        self.session
            .fail_materialization(id, source_id, placement, message)
            .map(|update| self.finish(update, sample))
            .transpose()
    }

    fn cancel_materialization(
        &mut self,
        id: MaterializationId,
        source_id: &SourceKey,
        placement: Placement,
    ) -> PlaybackResult<bool> {
        Ok(self
            .session
            .cancel_materialization(id, source_id, placement))
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
        source_id: &SourceKey,
        seed_occurrence: &crate::OccurrenceId,
        candidates: Vec<TrackKey>,
        requested_count: usize,
        shuffle_seed: u64,
        sample: &ClockSample,
    ) -> PlaybackResult<Option<PlaybackUpdate>> {
        let update = self
            .session
            .complete_auto_dj_candidates(
                source_id,
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
        source_id: &SourceKey,
        seed_occurrence: &crate::OccurrenceId,
        error: Option<String>,
        sample: &ClockSample,
    ) -> PlaybackResult<Option<PlaybackUpdate>> {
        let update = self
            .session
            .auto_dj_unavailable(source_id, seed_occurrence, error)
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

    fn commit(&self, update: SessionUpdate) -> PlaybackUpdate {
        let queue_persistence = update
            .queue_persistence_changed
            .then(|| QueuePersistence::capture(self.session.sequence()));
        let mut notices = Vec::new();
        let mut effects = Vec::new();
        let mut current_media_changed = false;
        let mut visualizer = None;
        for effect in update.effects {
            match effect {
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
        let projection = (update.view_changed || !notices.is_empty()).then(|| PlaybackProjection {
            view: self.session.view(),
            notices,
        });
        PlaybackUpdate {
            queue_persistence,
            projection,
            effects,
            current_media_changed,
            queue_changed: update.queue_changed,
            visualizer,
        }
    }
}
