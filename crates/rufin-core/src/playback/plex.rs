//! Native Companion crossings. PMS owns its queue; observations enter the existing session.
#[cfg(test)]
mod tests;
use super::*;
use playback::{
    ExternalPlayback, PlaybackOutput, RemoteOutput, RemoteOutputProtocol, TransportStatus,
};
use playback_cast::plex::{PlayMedia, PlexClient, PlexPlayer, Timeline};
use sources::{
    PlexCompanionContext, PlexQueueMutation, PlexQueuePlacement, PlexQueueWindow, Source,
};
use std::sync::atomic::AtomicBool;

pub(super) struct PlexPlayback {
    client: PlexClient,
    connection: Mutex<(Arc<Source>, PlexCompanionContext)>,
    source_owner: std::sync::Weak<SourceOwner>,
    settings: SettingsFile,
    output: PlaybackOutput,
    playback: Playback,
    database: Arc<Database>,
    queue: tokio::sync::Mutex<Option<PlexQueueWindow>>,
    commands: tokio::sync::Mutex<()>,
    prior_volume: Mutex<Option<u8>>,
    cancelled: AtomicBool,
    observer: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl PlexPlayback {
    fn connection(&self) -> (Arc<Source>, PlexCompanionContext) {
        self.connection
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    async fn resolve_timeline_source(&self, timeline: &Timeline) -> Result<(), String> {
        let Some(server) = timeline.machine_identifier.as_deref() else {
            return Ok(());
        };
        if self
            .connection
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .1
            .server_id
            == server
        {
            return Ok(());
        }
        let owner = self
            .source_owner
            .upgrade()
            .ok_or("Sources are unavailable")?;
        let stored = self.settings.load();
        let preferred = stored.sources.selected_source_id;
        let mut configured = stored
            .sources
            .configured
            .into_iter()
            .filter(|source| source.configuration.kind == "plex")
            .collect::<Vec<_>>();
        configured
            .sort_by_key(|source| Some(&source.configuration.source_id) != preferred.as_ref());
        for configured in configured {
            let owner = owner.clone();
            let id = configured.configuration.source_id;
            let candidate = tokio::task::spawn_blocking(move || owner.client(&id))
                .await
                .map_err(string_error)?;
            let Ok(source) = candidate else { continue };
            let Ok(context) = source.plex_companion_context().await else {
                continue;
            };
            if context.server_id == server {
                *self.connection.lock().unwrap_or_else(|p| p.into_inner()) = (source, context);
                self.queue.lock().await.take();
                return Ok(());
            }
        }
        Err("The Plex player is playing from an unavailable configured server or profile".into())
    }
    async fn insert(
        &self,
        input: library::QueueInput,
        target: Option<playback::QueueReorderTarget>,
        placement: playback::QueuePlacement,
        anchor: usize,
    ) -> Result<(), String> {
        self.current().await?;
        let page = self
            .database
            .read_queue(library::QueueReadRequest::Capture {
                input: Box::new(input),
                anchor_index: anchor,
                random_start: None,
            })
            .await
            .map_err(string_error)?;
        if let Some(prefix) = page.entries.first().and_then(|entry| {
            entry
                .occurrence
                .as_str()
                .rsplit_once(':')
                .map(|(prefix, _)| format!("{prefix}:"))
        }) {
            self.database
                .discard_queue_capture(&prefix)
                .await
                .map_err(string_error)?;
        }
        let keys = queue_keys(&page.entries, &self.connection().1.source_id)?;
        if keys.is_empty() {
            return Ok(());
        }
        let replacing = matches!(
            placement,
            playback::QueuePlacement::Now | playback::QueuePlacement::Replace { .. }
        ) && target.is_none();
        if replacing {
            let queue = create_queue(&self.connection().0, &keys, &self.cancelled).await?;
            let timeline = queue_timeline(&queue);
            let items = self.all_items(&timeline, &self.cancelled).await?;
            let item = items
                .get(anchor)
                .ok_or("Plex did not retain the selected queue occurrence")?;
            // Selecting a new receiver queue uses skipTo after the new queue is loaded.
            // Existing local handoff selects via the actual local timeline instead.
            let token = self
                .connection()
                .0
                .plex_delegation_token()
                .await
                .map_err(string_error)?;
            let base = url::Url::parse(&self.connection().1.base_url).map_err(string_error)?;
            let server = self.connection().1.server_id.clone();
            let initial = items
                .iter()
                .find(|item| Some(item.occurrence_id) == queue.selected_item_id)
                .or_else(|| items.first())
                .ok_or("Plex returned an empty queue")?;
            let initial_id = initial.occurrence_id;
            let key = initial.key.clone();
            let switching_occurrence = initial_id != item.occurrence_id;
            let expected_server = server.clone();
            let queue_id = queue.id;
            self.control(move |client| {
                client.play_media(&PlayMedia {
                    server_url: &base,
                    machine_identifier: &server,
                    delegation_token: &token,
                    key: &key,
                    play_queue_id: queue_id,
                    offset: 0,
                    paused: switching_occurrence,
                })
            })
            .await?;
            let observed = if switching_occurrence {
                // playMedia acknowledges before its asynchronous stop/load finishes.
                // Wait until the silent initial occurrence is actually installed;
                // skipTo then selects the exact duplicate and starts it itself.
                wait_for_plex_occurrence(
                    &self.client,
                    &expected_server,
                    queue_id,
                    initial_id,
                    Some("paused"),
                    &self.cancelled,
                )
                .await?;
                let key = item.key.clone();
                let id = item.occurrence_id;
                self.control(move |client| client.skip_to(&key, id)).await?;
                let selected = wait_for_plex_occurrence(
                    &self.client,
                    &expected_server,
                    queue_id,
                    id,
                    None,
                    &self.cancelled,
                )
                .await?;
                if selected.state == "paused" {
                    self.control(|client| client.play()).await?;
                    wait_for_plex_occurrence(
                        &self.client,
                        &expected_server,
                        queue_id,
                        id,
                        Some("playing"),
                        &self.cancelled,
                    )
                    .await?
                } else {
                    selected
                }
            } else {
                wait_for_plex_occurrence(
                    &self.client,
                    &expected_server,
                    queue_id,
                    initial_id,
                    Some("playing"),
                    &self.cancelled,
                )
                .await?
            };
            self.observe(observed, true).await?;
        } else {
            let timeline = self.current().await?;
            let queue = timeline
                .play_queue_id
                .ok_or("The Plex player has no queue")?;
            let mut first = None;
            let placement = match target {
                Some(playback::QueueReorderTarget::After(id)) => PlexQueuePlacement::After(
                    plex_occurrence(&id)
                        .ok_or("The queue target is no longer on the Plex player")?,
                ),
                Some(playback::QueueReorderTarget::Before(id)) => {
                    let id = plex_occurrence(&id)
                        .ok_or("The queue target is no longer on the Plex player")?;
                    let before = self
                        .connection()
                        .0
                        .plex_queue_adjacent(queue, id, true, None)
                        .await
                        .map_err(string_error)?;
                    if let Some(previous) = before.items.last() {
                        PlexQueuePlacement::After(previous.occurrence_id)
                    } else {
                        first = Some(id);
                        PlexQueuePlacement::After(id)
                    }
                }
                Some(playback::QueueReorderTarget::End) => PlexQueuePlacement::End,
                None if placement == playback::QueuePlacement::Next => PlexQueuePlacement::Next,
                None => PlexQueuePlacement::End,
            };
            let chunks = keys.chunks(sources::PLEX_QUEUE_WINDOW).collect::<Vec<_>>();
            for index in 0..chunks.len() {
                let batch = chunks[if placement == PlexQueuePlacement::End {
                    index
                } else {
                    chunks.len() - 1 - index
                }];
                self.connection()
                    .0
                    .plex_queue_mutation(PlexQueueMutation::Insert {
                        queue_id: queue,
                        rating_keys: batch.to_vec(),
                        placement,
                    })
                    .await
                    .map_err(string_error)?;
                if let Some(first) = first {
                    let inserted = self
                        .connection()
                        .0
                        .plex_queue_adjacent(queue, first, false, None)
                        .await
                        .map_err(string_error)?;
                    for item in inserted.items.iter().take(batch.len()).rev() {
                        self.connection()
                            .0
                            .plex_queue_mutation(PlexQueueMutation::Move {
                                queue_id: queue,
                                occurrence_id: item.occurrence_id,
                                after: None,
                            })
                            .await
                            .map_err(string_error)?;
                    }
                }
            }
            self.control(move |client| client.refresh_queue(queue))
                .await?;
        }
        Ok(())
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

    async fn control(
        &self,
        action: impl FnOnce(PlexClient) -> Result<(), String> + Send + 'static,
    ) -> Result<(), String> {
        let client = self.client.clone();
        tokio::task::spawn_blocking(move || action(client))
            .await
            .map_err(string_error)?
    }

    async fn current(&self) -> Result<Timeline, String> {
        let timeline = self
            .client
            .timeline_async(false)
            .await?
            .ok_or("The Plex player returned no music timeline")?;
        self.resolve_timeline_source(&timeline).await?;
        Ok(timeline)
    }

    async fn observe(&self, timeline: Timeline, force: bool) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        // Plexamp builds its timeline while the current track is being replaced.
        // An incomplete buffering snapshot is not a cleared queue.
        if timeline.state == "buffering" && timeline.play_queue_item_id.is_none() {
            return Ok(());
        }
        self.resolve_timeline_source(&timeline).await?;
        let mut cached = self.queue.lock().await;
        if timeline.play_queue_id.is_none() {
            let clear = cached.take().is_some()
                || !self
                    .playback
                    .handoff_snapshot()
                    .map_err(string_error)?
                    .0
                    .entries
                    .is_empty();
            return self
                .playback
                .observe_external(
                    self.output.clone(),
                    ExternalPlayback {
                        queue: clear.then(library::QueueRestore::default),
                        queue_total: 0,
                        queue_offset: 0,
                        status: status(&timeline),
                        position_millis: timeline.time,
                        duration_millis: timeline.duration,
                        volume: f64::from(timeline.volume.unwrap_or(100)) / 100.0,
                        muted: self
                            .prior_volume
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .is_some(),
                        repeat: repeat(&timeline),
                        shuffle: timeline.shuffle.unwrap_or(false),
                    },
                )
                .map_err(string_error);
        }
        let changed = force
            || cached.as_ref().is_none_or(|queue| {
                Some(queue.id) != timeline.play_queue_id
                    || Some(queue.version) != timeline.play_queue_version
                    || !queue
                        .items
                        .iter()
                        .any(|item| Some(item.occurrence_id) == timeline.play_queue_item_id)
            });
        let restore = if changed && let Some(id) = timeline.play_queue_id {
            let window = self
                .connection()
                .0
                .plex_queue_window(id, timeline.play_queue_item_id)
                .await
                .map_err(string_error)?;
            let restore = self.restore(&window, &timeline).await?;
            *cached = Some(window);
            Some(restore)
        } else if let Some(window) = cached.as_ref() {
            // A different current occurrence inside the same window still changes the selection.
            let current = self
                .playback
                .handoff_snapshot()
                .map_err(string_error)?
                .0
                .current()
                .cloned();
            if current.as_ref().and_then(plex_occurrence) != timeline.play_queue_item_id {
                Some(self.restore(window, &timeline).await?)
            } else {
                None
            }
        } else {
            None
        };
        let (total, offset) = cached
            .as_ref()
            .map(|queue| (queue.total, queue.offset))
            .unwrap_or_default();
        if self.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.playback
            .observe_external(
                self.output.clone(),
                ExternalPlayback {
                    queue: restore,
                    queue_total: total,
                    queue_offset: offset,
                    status: status(&timeline),
                    position_millis: timeline.time,
                    duration_millis: timeline.duration,
                    volume: f64::from(timeline.volume.unwrap_or(100)) / 100.0,
                    muted: self
                        .prior_volume
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .is_some(),
                    repeat: repeat(&timeline),
                    shuffle: timeline.shuffle.unwrap_or(false),
                },
            )
            .map_err(string_error)
    }

    async fn restore(
        &self,
        window: &PlexQueueWindow,
        timeline: &Timeline,
    ) -> Result<library::QueueRestore, String> {
        let (source, context) = self.connection();
        let entries: Vec<_> = window
            .items
            .iter()
            .map(|item| library::QueueEntry {
                occurrence: receiver_occurrence(&context.source_id, window.id, item.occurrence_id),
                media_uri: item.media_uri.clone().into(),
                playlist_entry_id: None,
                provenance: playback::Provenance::Manual,
            })
            .collect();
        let mut page = self
            .database
            .read_queue(library::QueueReadRequest::Hydrate {
                entries: entries.clone(),
            })
            .await
            .map_err(string_error)?;
        if page.occurrences.len() != entries.len() {
            let keys = window
                .items
                .iter()
                .filter(|item| {
                    !page
                        .occurrences
                        .iter()
                        .any(|row| row.media_uri == item.media_uri)
                })
                .map(|item| item.rating_key.clone())
                .collect::<Vec<_>>();
            source
                .plex_import_items(&self.database, &keys)
                .await
                .map_err(string_error)?;
            page = self
                .database
                .read_queue(library::QueueReadRequest::Hydrate {
                    entries: entries.clone(),
                })
                .await
                .map_err(string_error)?;
        }
        Ok(library::QueueRestore {
            order: (0..entries.len() as u32).collect(),
            entries: entries.into(),
            occurrences: page.occurrences,
            current_index: window
                .items
                .iter()
                .position(|item| Some(item.occurrence_id) == timeline.play_queue_item_id),
            progress_millis: timeline.time.unwrap_or_default().min(i64::MAX as u64) as i64,
            repeat_mode: repeat(timeline),
            shuffled: timeline.shuffle.unwrap_or(false),
            next_id: 0,
        })
    }

    async fn command(&self, command: SessionCommand) -> Result<(), String> {
        let _guard = self.commands.lock().await;
        if self.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        match command {
            SessionCommand::Play => self.control(|client| client.play()).await?,
            SessionCommand::Pause => self.control(|client| client.pause()).await?,
            SessionCommand::PlayPause => {
                let playing = matches!(
                    self.current().await?.state.as_str(),
                    "playing" | "buffering"
                );
                self.control(move |client| {
                    if playing {
                        client.pause()
                    } else {
                        client.play()
                    }
                })
                .await?;
            }
            SessionCommand::Stop => self.control(|client| client.stop()).await?,
            SessionCommand::Next => self.control(|client| client.next()).await?,
            SessionCommand::Previous => self.control(|client| client.previous()).await?,
            SessionCommand::Seek(offset) => self.control(move |client| client.seek(offset)).await?,
            SessionCommand::SetVolume(value) => {
                *self.prior_volume.lock().unwrap_or_else(|p| p.into_inner()) = None;
                self.control(move |client| {
                    client.volume((value.clamp(0.0, 1.0) * 100.0).round() as u8)
                })
                .await?;
            }
            SessionCommand::SetMuted(muted) => {
                let volume = self.current().await?.volume.unwrap_or(100);
                let target = {
                    let mut saved = self.prior_volume.lock().unwrap_or_else(|p| p.into_inner());
                    if muted {
                        saved.get_or_insert(volume);
                        0
                    } else {
                        saved.take().unwrap_or(volume)
                    }
                };
                self.control(move |client| client.volume(target)).await?;
            }
            SessionCommand::SetRepeat(mode) => {
                self.control(move |client| client.repeat(mode)).await?
            }
            SessionCommand::SetShuffle { enabled, .. } => {
                self.control(move |client| client.shuffle(enabled)).await?
            }
            SessionCommand::Activate(id) => {
                let timeline = self.current().await?;
                let id = plex_occurrence(&id)
                    .ok_or("The queue occurrence is no longer on the Plex player")?;
                let window = self
                    .connection()
                    .0
                    .plex_queue_window(
                        timeline
                            .play_queue_id
                            .ok_or("The Plex player has no queue")?,
                        Some(id),
                    )
                    .await
                    .map_err(string_error)?;
                let key = window
                    .items
                    .iter()
                    .find(|item| item.occurrence_id == id)
                    .ok_or("The Plex player no longer has that occurrence")?
                    .key
                    .clone();
                self.control(move |client| client.skip_to(&key, id)).await?;
            }
            SessionCommand::Remove(id) => self.remove(&[id]).await?,
            SessionCommand::RemoveMany(ids) | SessionCommand::Forget(ids) => {
                self.remove(&ids).await?
            }
            SessionCommand::Reorder {
                occurrences,
                target,
            } => self.reorder(&occurrences, &target).await?,
            SessionCommand::MoveAfterCurrent(id) => {
                let timeline = self.current().await?;
                let current = occurrence(
                    timeline
                        .play_queue_id
                        .ok_or("The Plex player has no queue")?,
                    timeline
                        .play_queue_item_id
                        .ok_or("The Plex player has no current occurrence")?,
                );
                self.reorder(&[id], &playback::QueueReorderTarget::After(current))
                    .await?;
            }
            SessionCommand::Insert { input, target } => {
                self.insert(input, Some(target), playback::QueuePlacement::Last, 0)
                    .await?
            }
            SessionCommand::Clear { include_current } => {
                let timeline = self.current().await?;
                let entries = self.all_items(&timeline, &self.cancelled).await?;
                let ids = entries
                    .into_iter()
                    .filter(|item| {
                        include_current || Some(item.occurrence_id) != timeline.play_queue_item_id
                    })
                    .map(|item| occurrence(timeline.play_queue_id.unwrap(), item.occurrence_id))
                    .collect::<Vec<_>>();
                self.remove(&ids).await?;
            }
            SessionCommand::ApplyBatch { batch, placement } => {
                self.insert(batch.input().clone(), None, placement, 0)
                    .await?
            }
            _ => return Ok(()),
        }
        Ok(())
    }

    async fn remove(&self, ids: &[OccurrenceId]) -> Result<(), String> {
        let queue = self
            .current()
            .await?
            .play_queue_id
            .ok_or("The Plex player has no queue")?;
        for id in ids {
            let occurrence_id =
                plex_occurrence(id).ok_or("The occurrence is no longer on the Plex player")?;
            self.connection()
                .0
                .plex_queue_mutation(PlexQueueMutation::Remove {
                    queue_id: queue,
                    occurrence_id,
                })
                .await
                .map_err(string_error)?;
        }
        self.control(move |client| client.refresh_queue(queue))
            .await
    }

    async fn all_items(
        &self,
        timeline: &Timeline,
        cancelled: &AtomicBool,
    ) -> Result<Vec<sources::PlexQueueItem>, String> {
        self.resolve_timeline_source(timeline).await?;
        let id = timeline
            .play_queue_id
            .ok_or("The Plex player has no queue")?;
        check_cancelled(cancelled)?;
        let cached = self
            .queue
            .lock()
            .await
            .as_ref()
            .filter(|queue| queue.id == id && Some(queue.version) == timeline.play_queue_version)
            .cloned();
        let source = self.connection().0;
        let page = match cached {
            Some(page) => page,
            None => source
                .plex_queue_window(id, timeline.play_queue_item_id)
                .await
                .map_err(string_error)?,
        };
        if page.items.len() == page.total {
            return Ok(page.items);
        }
        // Playback needs complete occurrence membership, not complete metadata.
        // Reading that membership once avoids a network round trip per 100 songs.
        let membership = source
            .plex_queue_membership(id, page.total)
            .await
            .map_err(string_error)?;
        check_cancelled(cancelled)?;
        if membership.version != page.version {
            return Err("Plex changed its queue during transfer".into());
        }
        if membership.items.len() != membership.total {
            return Err("Plex returned an incomplete queue during transfer".into());
        }
        Ok(membership.items)
    }

    async fn reorder(
        &self,
        ids: &[OccurrenceId],
        target: &playback::QueueReorderTarget,
    ) -> Result<(), String> {
        let timeline = self.current().await?;
        let queue = timeline
            .play_queue_id
            .ok_or("The Plex player has no queue")?;
        let mut after = match target {
            playback::QueueReorderTarget::After(id) => plex_occurrence(id),
            playback::QueueReorderTarget::Before(id) => {
                let items = self.all_items(&timeline, &self.cancelled).await?;
                let index = items
                    .iter()
                    .position(|item| Some(item.occurrence_id) == plex_occurrence(id))
                    .ok_or("The Plex player no longer has the target occurrence")?;
                items[..index]
                    .iter()
                    .rev()
                    .find(|item| {
                        !ids.iter()
                            .any(|id| plex_occurrence(id) == Some(item.occurrence_id))
                    })
                    .map(|item| item.occurrence_id)
            }
            playback::QueueReorderTarget::End => self
                .all_items(&timeline, &self.cancelled)
                .await?
                .iter()
                .rev()
                .find(|item| {
                    !ids.iter()
                        .any(|id| plex_occurrence(id) == Some(item.occurrence_id))
                })
                .map(|item| item.occurrence_id),
        };
        for id in ids {
            let occurrence_id =
                plex_occurrence(id).ok_or("The occurrence is no longer on the Plex player")?;
            self.connection()
                .0
                .plex_queue_mutation(PlexQueueMutation::Move {
                    queue_id: queue,
                    occurrence_id,
                    after,
                })
                .await
                .map_err(string_error)?;
            after = Some(occurrence_id);
        }
        self.control(move |client| client.refresh_queue(queue))
            .await
    }
}

fn queue_keys(
    entries: &[library::QueueEntry],
    source: &sources::SourceId,
) -> Result<Vec<String>, String> {
    entries
        .iter()
        .map(|entry| {
            let (id, kind, key) = library::source_entity_parts(&entry.media_uri).ok_or(
                "Plex Companion queues require Plex music from one configured server and profile",
            )?;
            if &id != source || kind != "track" {
                return Err(
                    "Plex Companion queues require Plex music from one configured server and profile"
                        .into(),
                );
            }
            key.strip_prefix("plex:track:")
                .map(str::to_owned)
                .ok_or_else(|| {
                    "Plex Companion queues require Plex music from one configured server and profile"
                        .into()
                })
        })
        .collect()
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        Err("Output selection cancelled".into())
    } else {
        Ok(())
    }
}

async fn create_queue(
    source: &Source,
    keys: &[String],
    cancelled: &AtomicBool,
) -> Result<PlexQueueWindow, String> {
    check_cancelled(cancelled)?;
    let mut batches = sources::plex_queue_write_batches(keys).into_iter();
    let first = batches.next().ok_or("There is no music to transfer")?;
    let mut queue = source
        .plex_queue_mutation(PlexQueueMutation::Create {
            rating_keys: first.to_vec(),
        })
        .await
        .map_err(string_error)?;
    for batch in batches {
        check_cancelled(cancelled)?;
        queue = source
            .plex_queue_mutation(PlexQueueMutation::Insert {
                queue_id: queue.id,
                rating_keys: batch.to_vec(),
                placement: PlexQueuePlacement::End,
            })
            .await
            .map_err(string_error)?;
    }
    Ok(queue)
}

#[cfg(test)]
fn stopped_timeline() -> Timeline {
    Timeline {
        state: "stopped".into(),
        time: Some(0),
        duration: 0,
        machine_identifier: None,
        play_queue_id: None,
        play_queue_version: None,
        play_queue_item_id: None,
        rating_key: None,
        key: None,
        volume: None,
        repeat: None,
        shuffle: None,
    }
}

async fn wait_for_plex_occurrence(
    client: &PlexClient,
    server: &str,
    queue: u64,
    item: u64,
    state: Option<&str>,
    cancelled: &AtomicBool,
) -> Result<Timeline, String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        check_cancelled(cancelled)?;
        if let Some(timeline) = client.timeline_async(false).await?
            && timeline.machine_identifier.as_deref() == Some(server)
            && timeline.play_queue_id == Some(queue)
            && timeline.play_queue_item_id == Some(item)
            && state.map_or_else(
                || matches!(timeline.state.as_str(), "paused" | "playing" | "buffering"),
                |state| {
                    timeline.state == state
                        // The occurrence is installed once it is buffering. Waiting
                        // for audio here blocks later controls behind network reads.
                        || (state == "playing" && timeline.state == "buffering")
                },
            )
        {
            return Ok(timeline);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("The Plex player did not finish loading the selected queue".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn queue_timeline(queue: &PlexQueueWindow) -> Timeline {
    Timeline {
        state: "paused".into(),
        time: Some(0),
        duration: 0,
        machine_identifier: None,
        play_queue_id: Some(queue.id),
        play_queue_version: Some(queue.version),
        play_queue_item_id: queue.selected_item_id,
        rating_key: None,
        key: None,
        volume: None,
        repeat: None,
        shuffle: Some(queue.shuffled),
    }
}

fn receiver_occurrence(source: &sources::SourceId, queue: u64, item: u64) -> OccurrenceId {
    OccurrenceId::new(format!("plex:{}:{queue}:{item}", source.as_str()))
}
fn occurrence(queue: u64, item: u64) -> OccurrenceId {
    OccurrenceId::new(format!("plex:{queue}:{item}"))
}
fn plex_occurrence(id: &OccurrenceId) -> Option<u64> {
    id.as_str()
        .strip_prefix("plex:")?
        .rsplit_once(':')?
        .1
        .parse()
        .ok()
}
fn status(timeline: &Timeline) -> TransportStatus {
    match timeline.state.as_str() {
        "playing" => TransportStatus::Playing,
        "paused" => TransportStatus::Paused,
        "buffering" => TransportStatus::Buffering,
        _ => TransportStatus::Stopped,
    }
}
fn repeat(timeline: &Timeline) -> RepeatMode {
    match timeline.repeat {
        Some(1) => RepeatMode::All,
        Some(2) => RepeatMode::One,
        _ => RepeatMode::Off,
    }
}

impl PlaybackOwner {
    /// A pulled native queue retains all lightweight memberships. Resolve only
    /// the missing Plex metadata requested by the existing local queue window.
    pub(super) async fn import_missing_plex_queue_rows(
        &self,
        entries: Vec<library::QueueEntry>,
        page: library::QueueReadPage,
    ) -> Result<library::QueueReadPage, String> {
        let mut missing = std::collections::BTreeMap::<String, Vec<String>>::new();
        for entry in &entries {
            if page
                .occurrences
                .iter()
                .any(|row| row.media_uri == entry.media_uri.as_ref())
            {
                continue;
            }
            if let Some((source, kind, key)) = library::source_entity_parts(&entry.media_uri)
                && kind == "track"
                && let Some(key) = key.strip_prefix("plex:track:")
            {
                missing
                    .entry(source.to_string())
                    .or_default()
                    .push(key.to_owned());
            }
        }
        if missing.is_empty() {
            return Ok(page);
        }
        for (id, mut keys) in missing {
            keys.sort_unstable();
            keys.dedup();
            let owner = self
                .source_owner()
                .ok_or("The Plex source is unavailable")?;
            let source =
                tokio::task::spawn_blocking(move || owner.client(&sources::SourceId::new(id)))
                    .await
                    .map_err(string_error)?
                    .map_err(string_error)?;
            source
                .plex_import_items(&self.database, &keys)
                .await
                .map_err(string_error)?;
        }
        self.database
            .read_queue(library::QueueReadRequest::Hydrate { entries })
            .await
            .map_err(string_error)
    }

    pub(super) fn request_plex_auto_dj(&self, request: &playback::AutoDjRequest) -> bool {
        let Some(plex) = self.plex.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return false;
        };
        let request = request.clone();
        self.runtime.spawn(async move {
            let result = async {
                let (source, context) = {
                    let _guard = plex.commands.lock().await;
                    plex.current().await?;
                    plex.connection()
                };
                if library::source_entity_parts(&request.seed_media_uri)
                    .is_none_or(|(id, _, _)| id != context.source_id)
                {
                    return Ok(());
                }
                let source_key = plex
                    .database
                    .source_identity_key(&context.source_id)
                    .await
                    .map_err(string_error)?
                    .ok_or("Auto DJ source is unavailable")?;
                let candidates = crate::radio::radio_candidates(
                    &plex.database,
                    source_key,
                    Some(&source),
                    library::RadioSeed::Track(request.seed_media_uri),
                    request.requested_count,
                )
                .await?;
                let _guard = plex.commands.lock().await;
                let timeline = plex.current().await?;
                if plex.connection().1.source_id != context.source_id {
                    return Ok(());
                }
                if plex.cancelled.load(Ordering::Acquire)
                    || timeline.play_queue_item_id != plex_occurrence(&request.seed_occurrence)
                {
                    return Ok(());
                }
                plex.insert(
                    library::QueueInput::MediaUris {
                        order: candidates.into(),
                        provenance: playback::Provenance::AutoDj,
                    },
                    None,
                    playback::QueuePlacement::Last,
                    0,
                )
                .await
            }
            .await;
            let _ = plex
                .playback
                .auto_dj_unavailable(request.seed_occurrence, result.err());
        });
        true
    }
    pub(super) fn radio_plex(&self, request: &RadioPlayRequest) -> bool {
        let Some(plex) = self.plex.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return false;
        };
        let Some(selected) = self
            .source_owner()
            .and_then(|owner| owner.current_session())
            .and_then(|session| session.resolve())
        else {
            return true;
        };
        let request = request.clone();
        self.runtime.spawn(async move {
            let result = async {
                let candidates = crate::radio::radio_candidates(
                    &selected.database,
                    selected.source_key,
                    selected.source.as_deref(),
                    request.seed,
                    20,
                )
                .await?;
                let _guard = plex.commands.lock().await;
                plex.insert(
                    library::QueueInput::MediaUris {
                        order: candidates.into(),
                        provenance: playback::Provenance::Radio,
                    },
                    None,
                    request.placement,
                    0,
                )
                .await
            }
            .await;
            if let Err(error) = result {
                let _ = plex
                    .playback
                    .command(SessionCommand::OperationFailed(error));
            }
        });
        true
    }

    pub(super) fn random_plex(&self, request: &RandomPlayRequest) -> bool {
        let Some(plex) = self.plex.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return false;
        };
        let Some(selected) = self
            .source_owner()
            .and_then(|owner| owner.current_session())
            .and_then(|session| session.resolve())
        else {
            return true;
        };
        let request = request.clone();
        self.runtime.spawn(async move {
            let result = async {
                let candidates = selected
                    .database
                    .random_candidates(
                        selected.source_key,
                        selected.music_folder_key,
                        &request.criteria,
                        &[],
                        request.requested,
                        &ReadCancellation::new(),
                    )
                    .await
                    .map_err(string_error)?;
                let _guard = plex.commands.lock().await;
                plex.insert(
                    library::QueueInput::MediaUris {
                        order: candidates.into(),
                        provenance: playback::Provenance::Random,
                    },
                    None,
                    request.placement,
                    0,
                )
                .await
            }
            .await;
            if let Err(error) = result {
                let _ = plex
                    .playback
                    .command(SessionCommand::OperationFailed(error));
            }
        });
        true
    }
    pub(super) fn select_plex(
        &self,
        output: PlaybackOutput,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(), String> {
        check_cancelled(&cancelled)?;
        let PlaybackOutput::Remote(remote) = &output else {
            unreachable!()
        };
        let player = self
            .plex_targets
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&remote.id)
            .cloned()
            .ok_or("The Plex player is no longer available; search for outputs again")?;
        let active = self.active().ok_or("Playback is unavailable")?;
        let (local, _, _) = active.playback.handoff_snapshot().map_err(string_error)?;
        let sources = self.plex_sources()?;
        let transferring = !local.entries.is_empty()
            && self
                .plex
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_none();
        let selected = if transferring {
            let entry = local.entries.first().unwrap();
            let (source, _, _) = library::source_entity_parts(&entry.media_uri).ok_or(
                "Plex Companion queues require Plex music from one configured server and profile",
            )?;
            sources
                .iter()
                .position(|(_, context)| context.source_id == source)
                .ok_or("Plex Companion queues require an available configured Plex source")?
        } else {
            let (_, controller) = sources
                .first()
                .ok_or("Add a Plex source before selecting a Plex player")?;
            let probe = PlexClient::new(
                player.endpoint.as_str(),
                &player.machine_identifier,
                &controller.client_id,
            )?;
            let timeline = probe.timeline(false)?;
            let server = timeline
                .as_ref()
                .and_then(|timeline| timeline.machine_identifier.as_deref());
            let preferred = self.settings.load().sources.selected_source_id;
            let candidates = sources
                .iter()
                .enumerate()
                .filter(|(_, (_, context))| server.is_none_or(|server| context.server_id == server))
                .collect::<Vec<_>>();
            candidates
                .iter()
                .find(|(_, (_, context))| Some(&context.source_id) == preferred.as_ref())
                .or_else(|| candidates.first())
                .map(|(index, _)| *index)
                .ok_or("The Plex player is playing from an unavailable server or profile")?
        };
        let (source, context) = sources
            .get(selected)
            .cloned()
            .ok_or("Add a Plex source before selecting a Plex player")?;
        // Validate every occurrence before sending a command to the receiver.
        let keys = if transferring {
            Some(queue_keys(&local.entries, &context.source_id)?)
        } else {
            None
        };
        let client = PlexClient::new(
            player.endpoint.as_str(),
            &player.machine_identifier,
            &context.client_id,
        )?;
        let plex = Arc::new(PlexPlayback {
            client,
            connection: Mutex::new((source, context)),
            source_owner: self
                .source_owner()
                .map(|owner| Arc::downgrade(&owner))
                .unwrap_or_default(),
            settings: self.settings.clone(),
            output: output.clone(),
            playback: active.playback.clone(),
            database: self.database.clone(),
            queue: tokio::sync::Mutex::new(None),
            commands: tokio::sync::Mutex::new(()),
            prior_volume: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            observer: Mutex::new(None),
        });
        self.runtime.block_on(async {
            if let Some(keys) = keys {
                let ordered = local
                    .order
                    .iter()
                    .map(|index| keys[*index as usize].clone())
                    .collect::<Vec<_>>();
                let queue = create_queue(&plex.connection().0, &ordered, &cancelled).await?;
                let items = plex.all_items(&queue_timeline(&queue), &cancelled).await?;
                if items.len() != ordered.len()
                    || items
                        .iter()
                        .zip(&ordered)
                        .any(|(item, key)| &item.rating_key != key)
                {
                    return Err("Plex did not preserve the complete queue order".into());
                }
                let selected = items
                    .get(local.current_index.unwrap_or(0))
                    .ok_or("Plex did not retain the current queue occurrence")?;
                let token = plex
                    .connection()
                    .0
                    .plex_delegation_token()
                    .await
                    .map_err(string_error)?;
                check_cancelled(&cancelled)?;
                let (fresh, report, _) =
                    active.playback.handoff_snapshot().map_err(string_error)?;
                if fresh.entries != local.entries
                    || fresh.order != local.order
                    || fresh.current() != local.current()
                {
                    return Err(
                        "Local playback changed its queue during transfer; select the Plex player again"
                            .into(),
                    );
                }
                let mut report = report.ok_or("The selected local occurrence is unavailable")?;
                report.queue_id = Some(queue.id);
                report.queue_item_id = Some(selected.occurrence_id);
                plex.connection()
                    .0
                    .report_playback(&report)
                    .await
                    .map_err(string_error)?;
                let base = url::Url::parse(&plex.connection().1.base_url).map_err(string_error)?;
                let server = plex.connection().1.server_id.clone();
                let expected_server = server.clone();
                let key = selected.key.clone();
                let queue_id = queue.id;
                let offset = report.position_millis;
                let paused = report.paused;
                plex.control(move |client| {
                    client.play_media(&PlayMedia {
                        server_url: &base,
                        machine_identifier: &server,
                        delegation_token: &token,
                        key: &key,
                        play_queue_id: queue_id,
                        offset,
                        paused,
                    })
                })
                .await?;
                let observed = wait_for_plex_occurrence(
                    &plex.client,
                    &expected_server,
                    queue.id,
                    selected.occurrence_id,
                    Some(if paused { "paused" } else { "playing" }),
                    &cancelled,
                )
                .await?;
                plex.observe(observed, true).await?;
            } else {
                plex.observe(plex.current().await?, true).await?;
            }
            Ok::<_, String>(())
        })?;
        if let Some(previous) = self
            .plex
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .replace(plex.clone())
        {
            previous.cancel();
        }
        self.output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .selected = output;
        let observed = plex.clone();
        let task = self.runtime.spawn(async move {
            while !observed.cancelled.load(Ordering::Acquire) {
                match observed.client.timeline_async(true).await {
                    Ok(Some(timeline)) => {
                        // A queue load briefly clears Plexamp's current item. Its
                        // next periodic timeline will publish the completed command.
                        let Ok(_guard) = observed.commands.try_lock() else {
                            continue;
                        };
                        if observed.cancelled.load(Ordering::Acquire) {
                            break;
                        }
                        if let Err(error) = observed.observe(timeline, false).await {
                            let _ = observed
                                .playback
                                .command(SessionCommand::OperationFailed(error));
                        }
                    }
                    Ok(None) => {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                    Err(error) => {
                        warn!(%error,"The Plex player timeline unavailable");
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            }
        });
        *plex.observer.lock().unwrap_or_else(|p| p.into_inner()) = Some(task);
        Ok(())
    }

    pub(super) fn leave_plex(&self, cancelled: Arc<AtomicBool>) -> Result<(), String> {
        check_cancelled(&cancelled)?;
        let plex = self
            .plex
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or("The Plex player is not selected")?;
        let backend = (self.start_backend)()?;
        self.runtime.block_on(async {
            let _guard = plex.commands.lock().await;
            let timeline = plex.current().await?;
            let (_, context) = plex.connection();
            if timeline.play_queue_id.is_none() {
                let playback = plex.playback.clone();
                tokio::task::spawn_blocking(move || {
                    playback.replace_backend(PlaybackOutput::Local, backend)
                })
                .await
                .map_err(string_error)?
                .map_err(string_error)?;
                plex.cancel();
                return Ok(());
            }
            let items = plex.all_items(&timeline, &cancelled).await?;
            let window = PlexQueueWindow {
                id: timeline.play_queue_id.unwrap(),
                version: timeline.play_queue_version.unwrap_or(0),
                total: items.len(),
                offset: 0,
                selected_item_id: timeline.play_queue_item_id,
                selected_offset: None,
                shuffled: timeline.shuffle.unwrap_or(false),
                items,
            };
            // Hydration stays around the selected occurrence even for a large transfer.
            let selected = window
                .items
                .iter()
                .position(|item| Some(item.occurrence_id) == timeline.play_queue_item_id)
                .ok_or("The Plex player has no current queue occurrence")?;
            let start = selected.saturating_sub(20);
            let end = (selected + 21).min(window.items.len());
            let nearby = PlexQueueWindow {
                id: window.id,
                version: window.version,
                total: window.total,
                offset: start,
                selected_item_id: window.selected_item_id,
                selected_offset: window.selected_offset,
                shuffled: window.shuffled,
                items: window.items[start..end].to_vec(),
            };
            let mut queue = plex.restore(&nearby, &timeline).await?;
            queue.entries = window
                .items
                .iter()
                .map(|item| library::QueueEntry {
                    occurrence: receiver_occurrence(
                        &context.source_id,
                        window.id,
                        item.occurrence_id,
                    ),
                    media_uri: item.media_uri.clone().into(),
                    playlist_entry_id: None,
                    provenance: playback::Provenance::Manual,
                })
                .collect();
            queue.order = (0..queue.entries.len() as u32).collect();
            queue.current_index = Some(selected);
            let occurrence = queue
                .occurrences
                .iter()
                .find(|row| Some(&row.occurrence) == queue.current())
                .cloned()
                .ok_or("The current Plex track is not available locally")?;
            let mut request =
                StreamRequest::for_item(&occurrence.item, self.settings.playback_stream_quality());
            request.session_identifier =
                Some(plex.playback.handoff_snapshot().map_err(string_error)?.2);
            let source = plex.connection().0.clone();
            let stream = prepare_stream(&self.database, request, move |_| Ok(source)).await?;
            let (track, album) = self
                .database
                .playback_loudness(&occurrence.media_uri, &ReadCancellation::new())
                .await
                .map_err(string_error)?;
            let prepared = prepare_media_stream(
                stream,
                playback::TrackLoudness {
                    track: track.map(Box::new),
                    album: album.map(Box::new),
                },
                occurrence,
            );
            let fresh = plex.current().await?;
            check_cancelled(&cancelled)?;
            if fresh.machine_identifier != timeline.machine_identifier
                || fresh.play_queue_id != timeline.play_queue_id
                || fresh.play_queue_item_id != timeline.play_queue_item_id
                || fresh.play_queue_version != timeline.play_queue_version
            {
                return Err(
                    "The Plex player changed its queue during transfer; select local output again"
                        .into(),
                );
            }
            if let Some(time) = fresh.time {
                queue.progress_millis = time.min(i64::MAX as u64) as i64;
            }
            queue.repeat_mode = repeat(&fresh);
            queue.shuffled = fresh.shuffle.unwrap_or(false);
            let playback = plex.playback.clone();
            tokio::task::spawn_blocking(move || {
                playback.adopt_local(queue, prepared, fresh.state == "playing", backend)
            })
            .await
            .map_err(string_error)?
            .map_err(string_error)?;
            plex.cancel();
            if let Err(error) = plex.control(|client| client.stop()).await {
                let _ = plex
                    .playback
                    .command(SessionCommand::OperationFailed(format!(
                        "Local playback resumed, but the Plex player could not be stopped: {error}"
                    )));
            }
            Ok::<_, String>(())
        })?;
        self.plex.lock().unwrap_or_else(|p| p.into_inner()).take();
        self.output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .selected = PlaybackOutput::Local;
        Ok(())
    }
    fn plex_sources(&self) -> Result<Vec<(Arc<Source>, PlexCompanionContext)>, String> {
        let owner = self.source_owner().ok_or("Sources are unavailable")?;
        self.settings
            .load()
            .sources
            .configured
            .iter()
            .filter(|source| source.configuration.kind == "plex")
            .map(|configured| {
                let source = owner.client(&configured.configuration.source_id)?;
                let context = self
                    .runtime
                    .block_on(source.plex_companion_context())
                    .map_err(string_error)?;
                Ok((source, context))
            })
            .collect()
    }

    pub(super) fn discover_plex(&self) -> Result<Vec<RemoteOutput>, String> {
        let sources = self.plex_sources()?;
        let mut players = self
            .runtime
            .block_on(playback_cast::plex::discover_plex_players(
                std::time::Duration::from_secs(2),
            ))
            .unwrap_or_default();
        for (source, context) in sources {
            for player in self
                .runtime
                .block_on(source.plex_companion_players())
                .unwrap_or_default()
            {
                if let Ok(client) = PlexClient::new(
                    &player.endpoint,
                    &player.machine_identifier,
                    &context.client_id,
                ) && let Ok(player) = client.resources()
                {
                    players.push(player);
                }
            }
        }
        let mut targets = self.plex_targets.lock().unwrap_or_else(|p| p.into_inner());
        targets.clear();
        for player in players
            .into_iter()
            .filter(PlexPlayer::supports_music_control)
        {
            targets.insert(format!("plex:{}", player.machine_identifier), player);
        }
        Ok(targets
            .iter()
            .map(|(id, player)| RemoteOutput {
                id: id.clone(),
                name: player.name.clone(),
                protocol: RemoteOutputProtocol::PlexCompanion,
            })
            .collect())
    }

    pub(super) fn send_plex(&self, command: SessionCommand) -> Option<SessionCommand> {
        let Some(plex) = self.plex.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return Some(command);
        };
        if matches!(
            command,
            SessionCommand::UpdateSettings(_)
                | SessionCommand::SetAutoDj { .. }
                | SessionCommand::SetVisualizerEnabled(_)
                | SessionCommand::ArtworkRefreshed(_)
                | SessionCommand::CatalogChanged
                | SessionCommand::PersistOutputState
        ) {
            return Some(command);
        }
        self.runtime.spawn(async move {
            if let Err(error) = plex.command(command).await {
                let _ = plex
                    .playback
                    .command(SessionCommand::OperationFailed(error));
            }
        });
        None
    }

    pub(super) fn play_plex(&self, request: &PlayRequest) -> bool {
        let Some(plex) = self.plex.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return false;
        };
        let request = request.clone();
        self.runtime.spawn(async move {
            let _guard = plex.commands.lock().await;
            if let Err(error) = plex
                .insert(
                    request.batch.input().clone(),
                    None,
                    request.placement,
                    request.anchor_index,
                )
                .await
            {
                let _ = plex
                    .playback
                    .command(SessionCommand::OperationFailed(error));
            }
        });
        true
    }
}
