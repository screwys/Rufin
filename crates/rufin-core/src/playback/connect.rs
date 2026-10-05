//! A Rufin receiver owns playback and its queue; this device observes and controls it.
use super::*;
use crate::connect::{ConnectOwner, Control, casting::ReceiverState};
use playback::{ExternalPlayback, PlaybackOutput, RemoteOutputProtocol};
use std::sync::atomic::AtomicBool;

#[derive(Default)]
struct VolumeRequests {
    value: Option<f64>,
    persist: Option<f64>,
}

pub(super) struct RufinPlayback {
    peer: String,
    connect: std::sync::Weak<ConnectOwner>,
    playback: Playback,
    database: Arc<Database>,
    output: PlaybackOutput,
    commands: tokio::sync::Mutex<()>,
    volume: Mutex<VolumeRequests>,
    volume_changed: Arc<tokio::sync::Notify>,
    revision: Mutex<Option<u64>>,
    content_id: Mutex<String>,
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

    async fn refresh(&self) -> Result<(), String> {
        let mut state = self.connection()?.receiver_state(&self.peer).await?;
        if self.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        let previous = self.playback.projection().map_err(string_error)?.view;
        let current = previous
            .transport
            .current
            .as_ref()
            .map(|media| &media.id.occurrence);
        let changed = *self.revision.lock().unwrap_or_else(|p| p.into_inner())
            != Some(state.revision)
            || current != state.current.as_ref()
            || previous
                .queue_window
                .iter()
                .map(|row| &row.occurrence)
                .ne(state.rows.iter().map(|row| &row.occurrence));
        if changed {
            let uris = state
                .rows
                .iter()
                .map(|row| row.media_uri.clone())
                .collect::<Vec<_>>();
            let bindings = self
                .database
                .queue_artwork_for_uris(&uris)
                .await
                .map_err(string_error)?
                .into_iter()
                .collect::<std::collections::HashMap<_, _>>();
            for row in &mut state.rows {
                row.item.artwork_binding = bindings.get(&row.media_uri).cloned().flatten();
            }
        }
        let restore = changed.then(|| library::QueueRestore {
            entries: state
                .rows
                .iter()
                .map(|row| library::QueueEntry {
                    occurrence: row.occurrence.clone(),
                    media_uri: row.media_uri.clone().into(),
                    playlist_entry_id: row.playlist_entry_id.clone().map(Into::into),
                    provenance: row.provenance.clone(),
                })
                .collect(),
            order: (0..state.rows.len() as u32).collect(),
            current_index: state
                .current
                .as_ref()
                .and_then(|current| state.rows.iter().position(|row| &row.occurrence == current)),
            occurrences: state.rows.iter().cloned().map(Arc::new).collect(),
            progress_millis: state.position.min(i64::MAX as u64) as i64,
            repeat_mode: state.repeat,
            shuffled: state.shuffle,
            next_id: 0,
        });
        self.playback
            .observe_external(
                self.output.clone(),
                ExternalPlayback {
                    queue: restore,
                    queue_total: state.total,
                    queue_offset: state.offset,
                    status: state.state,
                    position_millis: Some(state.position),
                    duration_millis: state.duration,
                    volume: state.volume,
                    muted: state.muted,
                    repeat: state.repeat,
                    shuffle: state.shuffle,
                },
            )
            .map_err(string_error)?;
        self.playback
            .command(SessionCommand::SetAutoDj {
                enabled: state.auto_dj,
                refill_threshold: state.auto_dj_threshold,
            })
            .map_err(string_error)?;
        *self.revision.lock().unwrap_or_else(|p| p.into_inner()) = Some(state.revision);
        *self.content_id.lock().unwrap_or_else(|p| p.into_inner()) = state.content_id;
        Ok(())
    }

    pub(super) async fn snapshot(&self) -> Result<Option<playback::Continuation>, String> {
        let _command = self.commands.lock().await;
        self.connection()?.receiver_queue(&self.peer).await
    }

    pub(super) fn content_id(&self) -> String {
        self.content_id
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub(super) async fn stop_continuation(
        &self,
        expected: &playback::ContinuationHeader,
    ) -> Result<Option<playback::ContinuationHeader>, String> {
        let header = self
            .connection()?
            .stop_receiver_continuation(&self.peer)
            .await?;
        Ok((header.current == expected.current
            && header.listen.as_ref().map(|listen| &listen.play_id)
                == expected.listen.as_ref().map(|listen| &listen.play_id))
        .then_some(header))
    }

    async fn input(
        &self,
        input: library::QueueInput,
        title: Option<Arc<library::QueueContextTitle>>,
        anchor: usize,
        shuffled: Option<u64>,
        placement: library::QueuePlacement,
        target: Option<library::QueueReorderTarget>,
    ) -> Result<(), String> {
        let page = self
            .database
            .read_queue(library::QueueReadRequest::Capture {
                input: Box::new(input),
                context_title: title,
                anchor_index: anchor,
                random_start: None,
                shuffled,
            })
            .await
            .map_err(string_error)?;
        if page.entries.is_empty() {
            return Ok(());
        }
        let namespace = page.entries[0]
            .occurrence
            .as_str()
            .rsplit_once(':')
            .map(|(prefix, _)| format!("{prefix}:"));
        let queue = library::QueueRestore {
            order: (0..page.entries.len() as u32).collect(),
            entries: page.entries.into(),
            occurrences: page.occurrences,
            current_index: Some(page.current_index),
            ..Default::default()
        };
        let result = self
            .connection()?
            .send_cast_input(&self.peer, queue, placement, target)
            .await;
        if let Some(namespace) = namespace {
            let _ = self.database.discard_queue_capture(&namespace).await;
        }
        result?;
        self.refresh().await
    }
}

impl PlaybackOwner {
    pub(crate) fn send_cast_auto_dj(&self, enabled: bool) {
        let _ = self.settings.update(|stored| {
            stored.ui.auto_dj_enabled = enabled;
            Ok(())
        });
        self.send(SessionCommand::SetAutoDj {
            enabled,
            refill_threshold: usize::from(self.settings.load().ui.auto_dj_refill_threshold),
        });
    }

    pub(crate) async fn receiver_state(&self) -> Result<ReceiverState, String> {
        let active = self.active().ok_or("Playback is unavailable")?;
        let view = active.playback.projection().map_err(string_error)?.view;
        let offset = view
            .queue
            .current_index
            .unwrap_or_default()
            .saturating_sub(16);
        let page = self
            .receiver_queue_page(
                offset as u64,
                library::QUEUE_CONTEXT_LIMIT as u32,
                String::new(),
            )
            .await?;
        let rows = page
            .rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let mut row = (*row).clone();
                row.canonical_position = offset + index;
                row
            })
            .collect();
        Ok(ReceiverState {
            revision: view.queue.revision,
            total: view.queue.total,
            offset,
            current: view.queue.current_occurrence,
            rows,
            state: view.transport.effective_state(),
            position: view.transport.position_millis,
            duration: view.transport.duration_millis,
            volume: view.controls.volume,
            muted: view.controls.muted,
            repeat: view.controls.repeat_mode,
            shuffle: view.controls.shuffle_enabled,
            auto_dj: view.controls.auto_dj_enabled,
            auto_dj_threshold: usize::from(self.settings.load().ui.auto_dj_refill_threshold),
            content_id: active.playback.queue_content_id().map_err(string_error)?,
        })
    }

    pub(crate) async fn receiver_queue_page(
        &self,
        offset: u64,
        limit: u32,
        filter: String,
    ) -> Result<playback::QueuePage, String> {
        let limit = limit.min(library::QUEUE_CONTEXT_LIMIT as u32);
        let receiver = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(receiver) = receiver {
            let _command = receiver.commands.lock().await;
            let mut page = receiver
                .connection()?
                .receiver_queue_page(&receiver.peer, offset, limit, filter)
                .await?;
            let uris = page
                .rows
                .iter()
                .map(|row| row.media_uri.clone())
                .collect::<Vec<_>>();
            let bindings = self
                .database
                .queue_artwork_for_uris(&uris)
                .await
                .map_err(string_error)?
                .into_iter()
                .collect::<std::collections::HashMap<_, _>>();
            for row in &mut page.rows {
                Arc::make_mut(row).item.artwork_binding =
                    bindings.get(&row.media_uri).cloned().flatten();
            }
            return Ok(page);
        }
        let Some(active) = self.active() else {
            return Ok(playback::QueuePage {
                rows: Vec::new(),
                total: 0,
                window_offset: 0,
            });
        };
        let (matches, filtered_total) = if filter.trim().is_empty() {
            (None, None)
        } else {
            let (matches, total) = self
                .database
                .saved_queue_search_page(&filter, offset, limit)
                .await
                .map_err(string_error)?;
            (Some(matches), Some(total))
        };
        let (entries, total, window_offset) = active
            .playback
            .queue_members(
                offset.min(usize::MAX as u64) as usize,
                limit as usize,
                matches,
            )
            .map_err(string_error)?;
        let page = self
            .database
            .read_queue(library::QueueReadRequest::Hydrate {
                entries: entries.clone(),
            })
            .await
            .map_err(string_error)?;
        let page = self.import_missing_plex_queue_rows(entries, page).await?;
        Ok(playback::QueuePage {
            rows: page.occurrences,
            total: filtered_total.unwrap_or(total as u64),
            window_offset: window_offset as u64,
        })
    }

    pub(crate) async fn cast_queue_snapshot(
        &self,
    ) -> Result<Option<playback::Continuation>, String> {
        let receiver = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(receiver) = receiver {
            return receiver.snapshot().await;
        }
        let plex = self.plex.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(plex) = plex {
            return plex.cast_queue_snapshot().await;
        }
        let active = self.active().ok_or("Playback is unavailable")?;
        if let Some(snapshot) = active.playback.continuation().map_err(string_error)? {
            return Ok(Some(snapshot));
        }
        let queue = active.playback.handoff_snapshot().map_err(string_error)?.0;
        let Some(current) = queue.current().cloned().or_else(|| {
            queue
                .order
                .first()
                .and_then(|index| queue.entries.get(*index as usize))
                .map(|entry| entry.occurrence.clone())
        }) else {
            return Ok(None);
        };
        let settings = self.settings.load().ui;
        Ok(Some(playback::Continuation {
            header: playback::ContinuationHeader {
                total: queue.entries.len(),
                current,
                position_millis: queue.progress_millis.max(0) as u64,
                repeat: queue.repeat_mode,
                shuffled: queue.shuffled,
                auto_dj: settings.auto_dj_enabled,
                auto_dj_refill_threshold: usize::from(settings.auto_dj_refill_threshold),
                playback_rate: settings.playback.playback_rate,
                desired_playing: false,
                listen: None,
            },
            queue,
        }))
    }

    pub(super) fn discover_rufin(&self) -> Vec<playback::RemoteOutput> {
        let Some(connect) = self
            .connect
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upgrade()
        else {
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
        let connect = self
            .connect
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upgrade()
            .ok_or("Connect is unavailable")?;
        let active = self.active().ok_or("Playback is unavailable")?;
        let volume_changed = Arc::new(tokio::sync::Notify::new());
        let volume_wake = Arc::clone(&volume_changed);
        let receiver = Arc::new(RufinPlayback {
            peer: remote.id.clone(),
            connect: Arc::downgrade(&connect),
            playback: active.playback.clone(),
            database: self.database.clone(),
            output: output.clone(),
            commands: tokio::sync::Mutex::new(()),
            volume: Mutex::new(VolumeRequests::default()),
            volume_changed,
            revision: Mutex::new(None),
            content_id: Mutex::new(String::new()),
            cancelled: AtomicBool::new(false),
            observer: Mutex::new(None),
        });
        let previous = self.runtime.block_on(async {
            connect
                .select_receiver(
                    &receiver.peer,
                    active
                        .playback
                        .projection()
                        .map_err(string_error)?
                        .view
                        .queue
                        .total
                        > 0,
                )
                .await?;
            if cancelled.load(Ordering::Acquire) {
                return Err("Output selection cancelled".into());
            }
            let previous = self
                .rufin
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .replace(receiver.clone());
            if let Err(error) = receiver.refresh().await {
                *self.rufin.lock().unwrap_or_else(|p| p.into_inner()) = previous;
                return Err(error);
            }
            Ok(previous)
        })?;
        if let Some(previous) = previous {
            previous.cancel();
        }
        if let Some(plex) = self.plex.lock().unwrap_or_else(|p| p.into_inner()).take() {
            plex.cancel();
        }
        self.output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .selected = output;
        let weak = Arc::downgrade(&receiver);
        let task = self.runtime.spawn(async move {
            loop {
                let volume_changed = tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => false,
                    _ = volume_wake.notified() => true,
                };
                let Some(receiver) = weak.upgrade() else {
                    break;
                };
                if receiver.cancelled.load(Ordering::Acquire) {
                    break;
                }
                let _command = if volume_changed {
                    receiver.commands.lock().await
                } else {
                    let Ok(command) = receiver.commands.try_lock() else {
                        continue;
                    };
                    command
                };
                let result = async {
                    if volume_changed {
                        let pending = std::mem::take(
                            &mut *receiver.volume.lock().unwrap_or_else(|p| p.into_inner()),
                        );
                        let connect = receiver.connection()?;
                        if let Some(value) = pending.value {
                            connect
                                .receiver_control(&receiver.peer, Control::Volume { value })
                                .await?;
                        }
                        if let Some(value) = pending.persist {
                            connect
                                .receiver_control(&receiver.peer, Control::PersistVolume { value })
                                .await?;
                        }
                    }
                    receiver.refresh().await
                }
                .await;
                if let Err(error) = result {
                    let _ = receiver
                        .playback
                        .command(SessionCommand::OperationFailed(error));
                }
            }
        });
        *receiver.observer.lock().unwrap_or_else(|p| p.into_inner()) = Some(task);
        Ok(())
    }

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
        self.runtime.block_on(async {
            let _command = receiver.commands.lock().await;
            receiver
                .connection()?
                .continue_from_with_state(&receiver.peer, None)
                .await
        })?;
        receiver.cancel();
        self.rufin.lock().unwrap_or_else(|p| p.into_inner()).take();
        self.output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .selected = PlaybackOutput::Local;
        Ok(())
    }

    pub(super) fn play_rufin(&self, request: &PlayRequest) -> bool {
        let Some(receiver) = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return false;
        };
        let mut request = request.clone();
        request.shuffled_start &= request.placement == library::QueuePlacement::Now
            && receiver
                .playback
                .projection()
                .is_ok_and(|projection| projection.view.controls.shuffle_enabled);
        let anchor = request.anchor_index;
        let (batch, placement) = request.compact_batch(random_u64());
        self.runtime.spawn(async move {
            let _command = receiver.commands.lock().await;
            if let Err(error) = receiver
                .input(
                    batch.input().clone(),
                    batch.context_title().cloned(),
                    anchor,
                    batch.shuffled_seed(),
                    placement,
                    None,
                )
                .await
            {
                let _ = receiver
                    .playback
                    .command(SessionCommand::OperationFailed(error));
            }
        });
        true
    }

    pub(super) fn radio_rufin(&self, request: &RadioPlayRequest) -> bool {
        let Some(receiver) = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return false;
        };
        let source = self
            .source
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let request = request.clone();
        self.runtime.spawn(async move {
            let result = async {
                let candidates = crate::radio::radio_candidates(
                    &receiver.database,
                    source,
                    request.seed,
                    20,
                    &ReadCancellation::new(),
                )
                .await?;
                let _command = receiver.commands.lock().await;
                receiver
                    .input(
                        library::QueueInput::MediaUris {
                            order: candidates.into(),
                            provenance: playback::Provenance::Radio,
                        },
                        None,
                        0,
                        None,
                        request.placement,
                        None,
                    )
                    .await
            }
            .await;
            if let Err(error) = result {
                let _ = receiver
                    .playback
                    .command(SessionCommand::OperationFailed(error));
            }
        });
        true
    }

    pub(super) fn random_rufin(&self, request: &RandomPlayRequest) -> bool {
        let Some(receiver) = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return false;
        };
        let Some(selected) = self
            .source_owner()
            .and_then(|source| source.current_session())
            .and_then(|session| session.resolve())
        else {
            return true;
        };
        let excluded = self
            .current_media()
            .map(|media| vec![media.media_uri.clone()])
            .unwrap_or_default();
        let request = request.clone();
        self.runtime.spawn(async move {
            let result = async {
                let candidates = selected
                    .database
                    .random_candidates(
                        selected.source_key,
                        selected.music_folder_key,
                        &request.criteria,
                        &excluded,
                        request.requested,
                        &ReadCancellation::new(),
                    )
                    .await
                    .map_err(string_error)?;
                let _command = receiver.commands.lock().await;
                receiver
                    .input(
                        library::QueueInput::MediaUris {
                            order: candidates.into(),
                            provenance: playback::Provenance::Random,
                        },
                        None,
                        0,
                        None,
                        request.placement,
                        None,
                    )
                    .await
            }
            .await;
            if let Err(error) = result {
                let _ = receiver
                    .playback
                    .command(SessionCommand::OperationFailed(error));
            }
        });
        true
    }

    pub(super) fn send_rufin(&self, command: SessionCommand) -> Option<SessionCommand> {
        let Some(receiver) = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return Some(command);
        };
        if let SessionCommand::Insert { input, target } = command {
            self.runtime.spawn(async move {
                let _command = receiver.commands.lock().await;
                if let Err(error) = receiver
                    .input(
                        input,
                        None,
                        0,
                        None,
                        library::QueuePlacement::End,
                        Some(target),
                    )
                    .await
                {
                    let _ = receiver
                        .playback
                        .command(SessionCommand::OperationFailed(error));
                }
            });
            return None;
        }
        let control = match command {
            SessionCommand::PlayPause => Control::PlayPause,
            SessionCommand::Play => Control::Play,
            SessionCommand::Pause => Control::Pause,
            SessionCommand::Stop => Control::Stop,
            SessionCommand::Next => Control::Next,
            SessionCommand::Previous => Control::Previous,
            SessionCommand::Seek(millis) => Control::Seek { millis },
            SessionCommand::SetVolume(value) => {
                receiver
                    .volume
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .value = Some(value);
                receiver.volume_changed.notify_one();
                return None;
            }
            SessionCommand::PersistVolume(value) => {
                receiver
                    .volume
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .persist = Some(value);
                receiver.volume_changed.notify_one();
                return None;
            }
            SessionCommand::SetMuted(muted) => Control::Mute { muted },
            SessionCommand::SetRepeat(mode) => Control::Repeat { mode },
            SessionCommand::SetShuffle { enabled, .. } => Control::Shuffle { enabled },
            SessionCommand::SetAutoDj { enabled, .. } => Control::AutoDj { enabled },
            SessionCommand::Activate(occurrence) => Control::Activate { occurrence },
            SessionCommand::Remove(occurrence) => Control::Remove {
                occurrences: vec![occurrence],
            },
            SessionCommand::RemoveMany(occurrences) | SessionCommand::Forget(occurrences) => {
                Control::Remove { occurrences }
            }
            SessionCommand::Reorder {
                occurrences,
                target,
            } => Control::Reorder {
                occurrences,
                target,
            },
            SessionCommand::MoveAfterCurrent(occurrence) => Control::MoveNext { occurrence },
            SessionCommand::Clear { include_current } => Control::Clear { include_current },
            command => return Some(command),
        };
        self.runtime.spawn(async move {
            let _command = receiver.commands.lock().await;
            let result = async {
                receiver
                    .connection()?
                    .receiver_control(&receiver.peer, control)
                    .await?;
                receiver.refresh().await
            }
            .await;
            if let Err(error) = result {
                let _ = receiver
                    .playback
                    .command(SessionCommand::OperationFailed(error));
            }
        });
        None
    }
}
