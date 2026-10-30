use std::sync::{
    Arc, Mutex, Weak,
    mpsc::{self, Receiver, RecvTimeoutError, Sender},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::{
    PlaybackOwner,
    queue::{Sequence, SequenceError},
    state::{ClockSample, Command, ExternalPlayback, Player},
};
use playback::{
    PlaybackBackend, PlaybackOutput, PlaybackProjection, PlaybackSettings, PreparedStream, RunId,
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

    /// Starts the worker for this handle. What was posted to it before waits for the worker.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run(
        &self,
        receiver: Receiver<Message>,
        sequence: Sequence,
        prefix: impl Into<Arc<str>>,
        settings: PlaybackSettings,
        auto_dj: bool,
        threshold: usize,
        output: PlaybackOutput,
        backend: Option<Box<dyn PlaybackBackend>>,
        clock: Clock,
        owner: Weak<PlaybackOwner>,
    ) -> PlaybackResult<PlaybackProjection> {
        let playback = self;
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
        let strong = owner.upgrade().ok_or(PlaybackError::Unavailable)?;
        strong.publish(initial.clone(), true);
        // The worker starts backends, which run their tasks on Rufin's runtime.
        let runtime = strong.runtime.clone();
        drop(strong);
        let thread = thread::Builder::new()
            .name("rufin-playback".into())
            .spawn(move || {
                let _runtime = runtime.enter();
                loop {
                    let message = if player.external_clock_running() {
                        receiver.recv_timeout(Duration::from_millis(33))
                    } else {
                        receiver.recv().map_err(|_| RecvTimeoutError::Disconnected)
                    };
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
        Ok(initial)
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
    pub(crate) fn page_entries(
        &self,
        offset: usize,
        limit: usize,
    ) -> PlaybackResult<(Vec<(usize, Option<u64>, library::QueueEntry)>, usize)> {
        self.request(move |player, _| Ok(player.page_entries(offset, limit)))
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
        self.request(move |player, sample| {
            player.adopt_local(queue, stream, playing, backend, sample)
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
        self.request(|player, _| Ok(player.view().transport.current.map(Arc::new)))
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
