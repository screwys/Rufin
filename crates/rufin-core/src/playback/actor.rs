use std::sync::{
    Arc, Mutex, Weak,
    mpsc::{self, Receiver, RecvTimeoutError, Sender},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::{
    PlaybackOwner,
    queue::{Sequence, SequenceError},
    state::{
        ClockSample, Command, ExternalPlayback, MaterializationId, MaterializationReservation,
        Player,
    },
};
use playback::{
    Placement, PlayRequest, PlaybackBackend, PlaybackOutput, PlaybackProjection, PlaybackSettings,
    PreparedStream, QueueItem, RunId,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum PlaybackError {
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

type PlaybackResult<T> = Result<T, PlaybackError>;
type Clock = Arc<dyn Fn() -> ClockSample + Send + Sync>;

#[derive(Clone)]
pub(crate) struct Playback(Arc<PlaybackInner>);
#[derive(Clone, Default)]
pub(super) struct WeakPlayback(Weak<PlaybackInner>);
struct PlaybackInner {
    sender: Sender<Message>,
    thread: Mutex<Option<JoinHandle<()>>>,
}
pub(super) struct Message {
    apply: Box<dyn FnOnce(&mut Player, &ClockSample) + Send>,
    shutdown: bool,
}
impl Message {
    pub(crate) fn apply(self, player: &mut Player, sample: &ClockSample) {
        (self.apply)(player, sample);
    }
}

pub(super) fn channel() -> (Playback, Receiver<Message>) {
    let (sender, receiver) = mpsc::channel();
    (
        Playback(Arc::new(PlaybackInner {
            sender,
            thread: Mutex::new(None),
        })),
        receiver,
    )
}

impl WeakPlayback {
    pub(crate) fn upgrade(&self) -> Option<Playback> {
        self.0.upgrade().map(Playback)
    }
    pub(crate) fn post(&self, apply: impl FnOnce(&mut Player, &ClockSample) + Send + 'static) {
        if let Some(playback) = self.upgrade() {
            let _ = playback.0.sender.send(Message {
                apply: Box::new(move |player, sample| {
                    apply(player, sample);
                    player.publish(sample);
                }),
                shutdown: false,
            });
        }
    }
}

impl Playback {
    pub(super) fn downgrade(&self) -> WeakPlayback {
        WeakPlayback(Arc::downgrade(&self.0))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start(
        sequence: Sequence,
        prefix: impl Into<Arc<str>>,
        settings: PlaybackSettings,
        auto_dj: bool,
        threshold: usize,
        output: PlaybackOutput,
        backend: Box<dyn PlaybackBackend>,
        clock: Clock,
        owner: Weak<PlaybackOwner>,
    ) -> PlaybackResult<(Self, PlaybackProjection)> {
        let (playback, receiver) = channel();
        let mut player = Player::new(
            sequence,
            prefix,
            settings,
            output,
            auto_dj,
            threshold,
            backend,
            owner.clone(),
            playback.downgrade(),
        );
        let initial = player.projection();
        owner
            .upgrade()
            .ok_or(PlaybackError::Unavailable)?
            .publish(initial.clone(), true);
        let thread = thread::Builder::new()
            .name("rufin-playback".into())
            .spawn(move || {
                loop {
                    let message = receiver.recv_timeout(Duration::from_millis(33));
                    let Some(_owner) = owner.upgrade() else {
                        let _ = player.shutdown_backend();
                        break;
                    };
                    match message {
                        Ok(message) => {
                            let sample = clock();
                            let shutdown = message.shutdown;
                            message.apply(&mut player, &sample);
                            if shutdown {
                                break;
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => {
                            let sample = clock();
                            player.shutdown(&sample);
                            player.publish(&sample);
                            let _ = player.shutdown_backend();
                            break;
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                    player.drive(clock.as_ref());
                }
            })
            .map_err(|error| PlaybackError::WorkerStart(error.to_string()))?;
        *playback.0.thread.lock().unwrap_or_else(|p| p.into_inner()) = Some(thread);
        Ok((playback, initial))
    }

    fn request<T: Send + 'static>(
        &self,
        apply: impl FnOnce(&mut Player, &ClockSample) -> PlaybackResult<T> + Send + 'static,
    ) -> PlaybackResult<T> {
        self.request_inner(apply, false)
    }
    fn request_inner<T: Send + 'static>(
        &self,
        apply: impl FnOnce(&mut Player, &ClockSample) -> PlaybackResult<T> + Send + 'static,
        shutdown: bool,
    ) -> PlaybackResult<T> {
        let (reply, result) = mpsc::sync_channel(1);
        self.0
            .sender
            .send(Message {
                apply: Box::new(move |player, sample| {
                    let result = apply(player, sample);
                    player.publish(sample);
                    let _ = reply.send(result);
                }),
                shutdown,
            })
            .map_err(|_| PlaybackError::Unavailable)?;
        result.recv().map_err(|_| PlaybackError::WorkerStopped)?
    }

    pub(crate) fn command(&self, command: Command) -> PlaybackResult<()> {
        self.request(move |player, sample| Ok(player.handle_command(command, sample)?))
    }
    pub(crate) fn play(&self, request: PlayRequest) -> PlaybackResult<()> {
        self.request(move |player, sample| Ok(player.play(request, sample)?))
    }
    pub(crate) fn page_entries(
        &self,
        offset: usize,
        limit: usize,
    ) -> PlaybackResult<(Vec<(usize, Option<u64>, library::QueueEntry)>, usize)> {
        self.request(move |player, _| Ok(player.page_entries(offset, limit)))
    }

    pub(crate) fn continuation(&self) -> PlaybackResult<Option<playback::Continuation>> {
        self.request(|player, sample| Ok(player.continuation(sample)))
    }
    pub(crate) fn stop_continuation(
        &self,
        expected: playback::ContinuationHeader,
    ) -> PlaybackResult<Option<playback::ContinuationHeader>> {
        self.request(move |player, sample| Ok(player.stop_continuation(&expected, sample)))
    }
    pub(crate) fn adopt_continuation(
        &self,
        continuation: playback::Continuation,
        stream: PreparedStream,
        backend: Box<dyn PlaybackBackend>,
    ) -> PlaybackResult<()> {
        self.request(move |player, sample| {
            player.adopt_continuation(continuation, stream, backend, sample)
        })
    }
    pub(crate) fn handoff_snapshot(
        &self,
    ) -> PlaybackResult<(
        library::QueueRestore,
        Option<playback::SourceReportFact>,
        String,
    )> {
        self.request(|player, _| {
            let (queue, report) = player.handoff_snapshot();
            Ok((queue, report, player.next_session_identifier()))
        })
    }
    pub(crate) fn observe_external(
        &self,
        output: PlaybackOutput,
        observation: ExternalPlayback,
    ) -> PlaybackResult<()> {
        self.command(Command::ObserveExternal {
            output,
            observation,
        })
    }
    pub(crate) fn adopt_local(
        &self,
        queue: library::QueueRestore,
        stream: PreparedStream,
        playing: bool,
        backend: Box<dyn PlaybackBackend>,
    ) -> PlaybackResult<()> {
        self.request(move |player, _| player.adopt_local(queue, stream, playing, backend))
    }
    pub(crate) fn reserve_materialization(
        &self,
        placement: Placement,
    ) -> PlaybackResult<MaterializationReservation> {
        self.request(move |player, _| Ok(player.reserve_materialization(placement)))
    }
    pub(crate) fn complete_materialization(
        &self,
        id: MaterializationId,
        batch: library::QueueReadRequest,
        placement: Placement,
    ) -> PlaybackResult<bool> {
        self.request(move |player, sample| {
            Ok(player.apply_materialization(id, batch, placement, sample)?)
        })
    }
    pub(crate) fn fail_materialization(
        &self,
        id: MaterializationId,
        placement: Placement,
        message: String,
    ) -> PlaybackResult<bool> {
        self.request(move |player, _| Ok(player.fail_materialization(id, placement, message)))
    }
    pub(crate) fn cancel_materialization(
        &self,
        id: MaterializationId,
        placement: Placement,
    ) -> PlaybackResult<bool> {
        self.request(move |player, _| Ok(player.cancel_materialization(id, placement)))
    }
    pub(crate) fn complete_auto_dj_candidates(
        &self,
        seed_run: Option<RunId>,
        seed_media_uri: String,
        candidates: Vec<QueueItem>,
        requested_count: usize,
    ) -> PlaybackResult<bool> {
        self.request(move |player, sample| {
            Ok(player.complete_auto_dj_candidates(
                seed_run,
                &seed_media_uri,
                candidates,
                requested_count,
                sample,
            )?)
        })
    }
    pub(crate) fn auto_dj_unavailable(
        &self,
        seed_run: Option<RunId>,
        seed_media_uri: String,
        error: Option<String>,
    ) -> PlaybackResult<bool> {
        self.request(move |player, _| {
            Ok(player.auto_dj_unavailable(seed_run, &seed_media_uri, error))
        })
    }
    pub(crate) fn current_media(&self) -> PlaybackResult<Option<Arc<playback::CurrentMedia>>> {
        self.request(|player, _| Ok(player.view().transport.current))
    }
    pub(crate) fn projection(&self) -> PlaybackResult<PlaybackProjection> {
        self.request(|player, _| Ok(player.projection()))
    }
    pub(crate) fn replace_backend(
        &self,
        output: PlaybackOutput,
        backend: Box<dyn PlaybackBackend>,
    ) -> PlaybackResult<()> {
        self.request(move |player, _| {
            player.replace_backend(output, backend);
            Ok(())
        })
    }
    pub(crate) fn shutdown(&self) -> PlaybackResult<()> {
        let result = self.request_inner(
            |player, sample| {
                player.shutdown(sample);
                player
                    .shutdown_backend()
                    .map_err(|error| PlaybackError::BackendShutdown(error.to_string()))
            },
            true,
        );
        if let Some(thread) = self
            .0
            .thread
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            thread.join().map_err(|_| PlaybackError::WorkerStopped)?;
        }
        result
    }
}

#[cfg(test)]
#[path = "actor_tests.rs"]
mod tests;
