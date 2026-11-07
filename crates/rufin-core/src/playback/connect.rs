//! A Rufin receiver owns playback and its queue; this device mirrors and controls it.
use super::*;
use crate::connect::{ConnectOwner, Control, Observed, Observing};
use crate::playback::{
    actor::Playback,
    state::{Command, ExternalPlayback},
};
use playback::{PlaybackOutput, RemoteOutputProtocol, TransportStatus};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
struct VolumeRequests {
    value: Option<f64>,
    persist: Option<f64>,
}

pub(super) struct RufinPlayback {
    peer: String,
    connect: std::sync::Weak<ConnectOwner>,
    playback: Playback,
    catalog: Arc<Catalog>,
    output: PlaybackOutput,
    commands: tokio::sync::Mutex<()>,
    volume: Mutex<VolumeRequests>,
    /// The receiver's queue revision and current position the mirrored window was read at,
    /// and the window's offset.
    window: Mutex<Option<(u64, Option<usize>, usize)>>,
    /// Whether the receiver played at its last frame.
    playing: AtomicBool,
    auto_dj_refill_threshold: usize,
    cancelled: AtomicBool,
    observer: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl RufinPlayback {
    fn connection(&self) -> Result<Arc<ConnectOwner>, String> {
        self.connect
            .upgrade()
            .ok_or("Connect is unavailable".into())
    }

    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(task) = self
            .observer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            task.abort();
        }
    }

    /// Shows a frame of the receiver's state here. The queue window around its current entry
    /// is read again when its queue or current entry changed.
    async fn mirror(&self, frame: Observed) -> Result<(), String> {
        let shown = *self.window.lock().unwrap_or_else(|p| p.into_inner());
        let (queue, offset) = match shown {
            Some((revision, current, offset))
                if revision == frame.revision && current == frame.current =>
            {
                (None, offset)
            }
            _ => {
                let (entries, _, offset) = self
                    .connection()?
                    .queue_page(
                        &self.peer,
                        frame.current.unwrap_or_default().saturating_sub(16) as u64,
                        library::QUEUE_CONTEXT_LIMIT as u32,
                        String::new(),
                    )
                    .await?;
                let offset = offset as usize;
                let entries = entries
                    .into_iter()
                    .map(|(_, entry)| entry)
                    .collect::<Vec<_>>();
                let rows = self
                    .catalog
                    .queue_rows_for_entries(
                        &entries.iter().cloned().enumerate().collect::<Vec<_>>(),
                        &ReadCancellation::new(),
                    )
                    .await
                    .map_err(string_error)?;
                let queue = library::QueueRestore {
                    order: (0..entries.len() as u32).collect(),
                    entries: entries.into(),
                    current_index: frame
                        .current
                        .and_then(|position| position.checked_sub(offset)),
                    rows,
                    progress_millis: frame.position_ms.min(i64::MAX as u64) as i64,
                    repeat_mode: frame.repeat,
                    shuffled: frame.shuffle,
                    ..Default::default()
                };
                (Some(queue), offset)
            }
        };
        if self.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.playback
            .observe_external(
                self.output.clone(),
                ExternalPlayback {
                    queue,
                    queue_total: frame.total,
                    queue_offset: offset,
                    status: frame.state,
                    position_millis: Some(frame.position_ms),
                    duration_millis: frame.duration_ms,
                    volume: frame.volume,
                    muted: frame.muted,
                    repeat: frame.repeat,
                    shuffle: frame.shuffle,
                },
            )
            .map_err(string_error)?;
        self.playback
            .command(Command::SetAutoDj {
                enabled: frame.auto_dj,
                refill_threshold: self.auto_dj_refill_threshold,
            })
            .map_err(string_error)?;
        *self.window.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((frame.revision, frame.current, offset));
        self.playing.store(frame.playing, Ordering::Release);
        Ok(())
    }

    /// Mirrors the receiver's frames. When the stream ends it is opened again once; a
    /// receiver that cannot be observed again is left, and its queue continues here.
    fn follow(self: &Arc<Self>, runtime: &tokio::runtime::Handle, mut observing: Observing) {
        let weak = Arc::downgrade(self);
        let task = runtime.spawn(async move {
            let mut reopened = false;
            loop {
                let frame = observing.next().await;
                let Some(receiver) = weak.upgrade() else {
                    break;
                };
                if receiver.cancelled.load(Ordering::Acquire) {
                    break;
                }
                match frame {
                    Ok(Some(frame)) => {
                        reopened = false;
                        if let Err(error) = receiver.mirror(frame).await {
                            let _ = receiver.playback.command(Command::OperationFailed(error));
                        }
                        continue;
                    }
                    Ok(None) => {}
                    Err(error) => debug!(%error, "The Rufin receiver's stream failed"),
                }
                if !reopened {
                    reopened = true;
                    let observed = match receiver.connection() {
                        Ok(connect) => connect.observe(&receiver.peer).await,
                        Err(error) => Err(error),
                    };
                    if let Ok(next) = observed {
                        observing = next;
                        continue;
                    }
                }
                let Ok(connect) = receiver.connection() else {
                    break;
                };
                let owner = Arc::clone(&connect.playback);
                tokio::task::spawn_blocking(move || {
                    if !receiver.cancelled.load(Ordering::Acquire)
                        && let Err(error) = owner.select_playback_output(
                            PlaybackOutput::Local,
                            Arc::new(AtomicBool::new(false)),
                        )
                    {
                        let _ = receiver.playback.command(Command::OperationFailed(error));
                    }
                });
                break;
            }
        });
        *self.observer.lock().unwrap_or_else(|p| p.into_inner()) = Some(task);
    }

    /// Sends a command to the receiver, after the volume requests that wait.
    fn control(self: &Arc<Self>, runtime: &tokio::runtime::Handle, control: Option<Control>) {
        let receiver = Arc::clone(self);
        runtime.spawn(async move {
            let _command = receiver.commands.lock().await;
            let pending =
                std::mem::take(&mut *receiver.volume.lock().unwrap_or_else(|p| p.into_inner()));
            let controls = pending
                .value
                .map(|value| Control::Volume { value })
                .into_iter()
                .chain(
                    pending
                        .persist
                        .map(|value| Control::PersistVolume { value }),
                )
                .chain(control);
            for control in controls {
                let result = match receiver.connection() {
                    Ok(connect) => connect.control(&receiver.peer, control).await,
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    let _ = receiver.playback.command(Command::OperationFailed(error));
                    return;
                }
            }
        });
    }

    async fn input(
        &self,
        request: PlayRequest,
        target: Option<library::QueueReorderTarget>,
    ) -> Result<(), String> {
        let placement = request.placement;
        let shuffled = self
            .playback
            .projection()
            .is_ok_and(|projection| projection.view.controls.shuffle_enabled);
        let page = request
            .prepare(&self.catalog, shuffled, random_u64())
            .await?;
        let queue = library::QueueRestore {
            order: (0..page.entries.len() as u32).collect(),
            current_index: Some(page.current_index),
            entries: page.entries.into(),
            ..Default::default()
        };
        self.connection()?
            .cast(&self.peer, &queue, true, Some((placement, target)))
            .await
    }
}

impl PlaybackOwner {
    pub(crate) fn send_cast_auto_dj(&self, enabled: bool) {
        if let Err(error) = self.settings.edit(|stored| {
            stored.ui.auto_dj_enabled = enabled;
            Ok(())
        }) {
            warn!(%error, "could not save Auto DJ setting");
        }
        self.send(Command::SetAutoDj {
            enabled,
            refill_threshold: usize::from(
                self.settings.load().ui.application.auto_dj_refill_threshold,
            ),
        });
    }

    /// This device's playback state for a device that casts here, with the position at this
    /// moment.
    pub(crate) fn observed(&self, projection: Option<&PlaybackProjection>) -> Observed {
        let Some(projection) = projection else {
            return Observed::default();
        };
        let view = &projection.view;
        let transport = projection.reporting_transport();
        let seeked = transport.position_millis != view.transport.position_millis;
        let mut position = transport.position_millis;
        if transport.state == TransportStatus::Playing && !seeked {
            let elapsed = self
                .clock_sample()
                .monotonic_millis
                .saturating_sub(transport.position_observed_at_millis);
            position += (elapsed as f64 * transport.playback_rate) as u64;
            if transport.duration_millis > 0 {
                position = position.min(transport.duration_millis);
            }
        }
        Observed {
            current: view.queue.current_index,
            state: transport.state,
            playing: view.transport.desired_playing,
            position_ms: position,
            duration_ms: transport.duration_millis,
            volume: view.controls.volume,
            muted: view.controls.muted,
            repeat: view.controls.repeat_mode,
            shuffle: view.controls.shuffle_enabled,
            auto_dj: view.controls.auto_dj_enabled,
            revision: view.queue.revision,
            total: view.queue.total,
        }
    }

    /// The whole queue in play order as the queue panel shows it: this device's, or that of
    /// the Rufin receiver it casts to.
    pub(crate) async fn queue_entries(&self) -> Result<Vec<library::QueueEntry>, String> {
        let receiver = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(receiver) = receiver {
            return receiver.connection()?.queue(&receiver.peer).await;
        }
        let Some(active) = self.active() else {
            return Ok(Vec::new());
        };
        let (entries, _) = active.page_entries(0, usize::MAX).map_err(string_error)?;
        Ok(entries.into_iter().map(|(.., entry)| entry).collect())
    }

    pub(crate) async fn receiver_queue_page(
        &self,
        offset: u64,
        limit: u32,
        filter: String,
    ) -> Result<crate::playback::QueuePage, String> {
        let limit = limit.min(library::QUEUE_CONTEXT_LIMIT as u32);
        let receiver = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(receiver) = receiver {
            let (entries, total, window_offset) = receiver
                .connection()?
                .queue_page(&receiver.peer, offset, limit, filter)
                .await?;
            let rows = self
                .catalog
                .queue_rows_for_entries(&entries, &ReadCancellation::new())
                .await
                .map_err(string_error)?;
            return Ok(crate::playback::QueuePage {
                rows,
                total,
                window_offset,
            });
        }
        let Some(active) = self.active() else {
            return Ok(crate::playback::QueuePage {
                rows: Vec::new(),
                total: 0,
                window_offset: 0,
            });
        };
        let (requested, total) = if filter.trim().is_empty() {
            let (entries, total) = active
                .page_entries(offset as usize, limit as usize)
                .map_err(string_error)?;
            (entries, total as u64)
        } else {
            let queue = active.handoff_snapshot().map_err(string_error)?.0;
            let (positions, total) = self
                .catalog
                .queue_search_page(&queue, &filter, offset, limit)
                .await
                .map_err(string_error)?;
            let entries = positions
                .into_iter()
                .map(|position| {
                    let index = queue.order[position] as usize;
                    (
                        position,
                        queue.ids.get(index).copied(),
                        queue.entries[index].clone(),
                    )
                })
                .collect();
            (entries, total)
        };
        let entries = requested
            .iter()
            .map(|(position, _, entry)| (*position, entry.clone()))
            .collect::<Vec<_>>();
        let mut rows = self
            .catalog
            .queue_rows_for_entries(&entries, &ReadCancellation::new())
            .await
            .map_err(string_error)?;
        // Rows of tracks the Store no longer holds are missing, so match by position.
        for row in &mut rows {
            if let Some((_, id, _)) = requested
                .iter()
                .find(|(position, ..)| *position == row.position)
            {
                Arc::make_mut(row).occurrence_id = *id;
            }
        }
        Ok(crate::playback::QueuePage {
            rows,
            total,
            window_offset: offset.min(total),
        })
    }

    pub(super) fn discover_rufin(&self) -> Vec<playback::RemoteOutput> {
        let Some(connect) = self.source.connect() else {
            return Vec::new();
        };
        let status = connect.status();
        if !status.settings.enabled {
            return Vec::new();
        }
        status
            .devices
            .into_iter()
            .filter(|device| device.enrolled && device.reachable)
            .map(|device| playback::RemoteOutput {
                id: device.id,
                name: device.name,
                protocol: RemoteOutputProtocol::RufinConnect,
            })
            .collect()
    }

    /// Casts this device's queue to a Rufin device, then mirrors that device's playback.
    pub(super) fn select_rufin(
        &self,
        output: PlaybackOutput,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(), String> {
        if cancelled.load(Ordering::Acquire) {
            return Err("Output selection cancelled".into());
        }
        let PlaybackOutput::Remote(remote) = &output else {
            unreachable!();
        };
        let connect = self.source.connect().ok_or("Connect is unavailable")?;
        let active = self.active().ok_or("Playback is unavailable")?;
        let receiver = Arc::new(RufinPlayback {
            peer: remote.id.clone(),
            connect: Arc::downgrade(&connect),
            playback: active.clone(),
            catalog: self.catalog.clone(),
            output: output.clone(),
            commands: tokio::sync::Mutex::new(()),
            volume: Mutex::new(VolumeRequests::default()),
            window: Mutex::new(None),
            playing: AtomicBool::new(false),
            auto_dj_refill_threshold: usize::from(
                self.settings.load().ui.application.auto_dj_refill_threshold,
            ),
            cancelled: AtomicBool::new(false),
            observer: Mutex::new(None),
        });
        // A buffering track that is to play continues playing there.
        let playing = (self.current_projection())
            .is_some_and(|projection| projection.view.transport.desired_playing);
        let observing = self.runtime.block_on(async {
            let queue = active.handoff_snapshot().map_err(string_error)?.0;
            if !queue.entries.is_empty() {
                connect.cast(&receiver.peer, &queue, playing, None).await?;
            }
            if cancelled.load(Ordering::Acquire) {
                return Err("Output selection cancelled".into());
            }
            let mut observing = connect.observe(&receiver.peer).await?;
            let frame = observing.next().await?.ok_or("Device is unreachable")?;
            receiver.mirror(frame).await?;
            Ok::<_, String>(observing)
        })?;
        receiver.follow(&self.runtime, observing);
        *self.rufin.lock().unwrap_or_else(|p| p.into_inner()) = Some(receiver);
        *self.output.lock().unwrap_or_else(|p| p.into_inner()) = output;
        Ok(())
    }

    /// Leaving a Rufin device continues its queue here, playing as it played there; a device
    /// with nothing to continue leaves this device's queue empty.
    pub(super) fn leave_rufin(&self, cancelled: Arc<AtomicBool>) -> Result<(), String> {
        if cancelled.load(Ordering::Acquire) {
            return Err("Output selection cancelled".into());
        }
        let receiver = self
            .rufin
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or("The Rufin device is not selected")?;
        let playing = receiver.playing.load(Ordering::Acquire);
        self.runtime.block_on(async {
            let _command = receiver.commands.lock().await;
            let continued = receiver
                .connection()?
                .continue_from(&receiver.peer, playing)
                .await?;
            if !continued {
                self.return_empty_local().await?;
            }
            Ok(())
        })
    }

    pub(super) fn play_rufin(&self, request: &PlayRequest) -> bool {
        let Some(receiver) = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return false;
        };
        let request = request.clone();
        self.runtime.spawn(async move {
            let _command = receiver.commands.lock().await;
            if let Err(error) = receiver.input(request, None).await {
                let _ = receiver.playback.command(Command::OperationFailed(error));
            }
        });
        true
    }

    pub(super) fn radio_rufin(&self, request: &RadioPlayRequest) -> bool {
        let Some(receiver) = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return false;
        };
        let request = request.clone();
        let candidates = self.candidates(request.seed.clone(), 20, ReadCancellation::new());
        self.runtime.spawn(async move {
            let result = async {
                let candidates = candidates.await?;
                let _command = receiver.commands.lock().await;
                receiver
                    .input(
                        PlayRequest::from_input(
                            library::QueueLoad::track_keys(
                                candidates,
                                playback::QueueProvenance::Radio,
                            ),
                            request.placement,
                        ),
                        None,
                    )
                    .await
            }
            .await;
            if let Err(error) = result {
                let _ = receiver.playback.command(Command::OperationFailed(error));
            }
        });
        true
    }

    pub(super) fn send_rufin(&self, command: Command) -> Option<Command> {
        let Some(receiver) = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return Some(command);
        };
        if let Command::Insert { input, target } = command {
            self.runtime.spawn(async move {
                let _command = receiver.commands.lock().await;
                if let Err(error) = receiver
                    .input(
                        PlayRequest::from_input(input, library::QueuePlacement::End),
                        Some(target),
                    )
                    .await
                {
                    let _ = receiver.playback.command(Command::OperationFailed(error));
                }
            });
            return None;
        }
        let control = match command {
            Command::PlayPause => Control::PlayPause,
            Command::Play => Control::Play,
            Command::Pause => Control::Pause,
            Command::Stop => Control::Stop,
            Command::Next => Control::Next,
            Command::Previous => Control::Previous,
            Command::Seek(millis) => Control::Seek { millis },
            Command::SetVolume(value) => {
                receiver
                    .volume
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .value = Some(value);
                receiver.control(&self.runtime, None);
                return None;
            }
            Command::PersistVolume(value) => {
                receiver
                    .volume
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .persist = Some(value);
                receiver.control(&self.runtime, None);
                return None;
            }
            Command::SetMuted(muted) => Control::Mute { muted },
            Command::SetRepeat(mode) => Control::Repeat { mode },
            Command::SetShuffle { enabled, .. } => Control::Shuffle { enabled },
            Command::SetAutoDj { enabled, .. } => Control::AutoDj { enabled },
            Command::Activate(position) => Control::Activate { position },
            Command::Remove(position) => Control::Remove {
                positions: vec![position],
            },
            Command::RemoveMany(crate::playback::QueueSelection::Positions(positions)) => {
                Control::Remove { positions }
            }
            Command::Reorder {
                selection: crate::playback::QueueSelection::Positions(positions),
                target,
            } => Control::Reorder { positions, target },
            Command::MoveAfterCurrentSelection(crate::playback::QueueSelection::Positions(
                positions,
            )) => Control::MoveNext { positions },
            Command::Clear { include_current } => Control::Clear { include_current },
            command => return Some(command),
        };
        receiver.control(&self.runtime, Some(control));
        None
    }
}
