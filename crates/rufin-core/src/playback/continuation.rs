//! Connected continuation uses fresh player state, never portable profile state.
use super::*;

pub(crate) struct PreparedContinuation {
    transfer: String,
    continuation: playback::Continuation,
    stream: PreparedStream,
    backend: Box<dyn PlaybackBackend>,
}

impl PlaybackOwner {
    pub(crate) async fn queue_content_id(&self) -> Result<String, String> {
        let plex = self.plex.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(plex) = plex {
            return Ok(plex.queue_content_id().await);
        }
        self.active()
            .ok_or("Playback is unavailable")?
            .playback
            .queue_content_id()
            .map_err(string_error)
    }

    pub(crate) fn has_continuation(&self) -> bool {
        self.active()
            .and_then(|active| active.playback.projection().ok())
            .is_some_and(|projection| {
                projection.view.transport.current.is_some()
                    && projection.view.transport.state != playback::TransportStatus::Stopped
            })
    }

    pub(crate) fn continuation_snapshot(&self) -> Result<Option<playback::Continuation>, String> {
        let plex = self.plex.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(plex) = plex {
            return self.runtime.block_on(plex.continuation_snapshot());
        }
        self.active()
            .ok_or("Playback is unavailable")?
            .playback
            .continuation()
            .map_err(string_error)
    }

    pub(crate) async fn continuation_page(
        &self,
        snapshot: &playback::Continuation,
        offset: usize,
    ) -> Result<library::QueueTransferPage, String> {
        let end = offset
            .saturating_add(library::QUEUE_CONTEXT_LIMIT)
            .min(snapshot.queue.entries.len());
        let entries = snapshot
            .queue
            .entries
            .get(offset..end)
            .ok_or("Queue transfer offset is out of range")?
            .to_vec();
        let page = self
            .database
            .read_queue(library::QueueReadRequest::Hydrate {
                entries: entries.clone(),
            })
            .await
            .map_err(string_error)?;
        let page = self.import_missing_plex_queue_rows(entries, page).await?;
        Ok(library::QueueTransferPage {
            offset,
            occurrences: page
                .occurrences
                .into_iter()
                .enumerate()
                .map(|(index, row)| {
                    let mut row = (*row).clone();
                    row.canonical_position = offset + index;
                    row
                })
                .collect(),
            order: snapshot.queue.order[offset..end].to_vec(),
        })
    }

    pub(crate) async fn stage_continuation_page(
        &self,
        transfer: &str,
        page: &library::QueueTransferPage,
    ) -> Result<(), String> {
        self.database
            .stage_queue_transfer_page(transfer, page)
            .await
            .map_err(string_error)
    }

    pub(crate) async fn discard_continuation(&self, transfer: &str) -> Result<(), String> {
        self.database
            .discard_queue_transfer(transfer)
            .await
            .map_err(string_error)
    }

    pub(crate) async fn prepare_continuation(
        &self,
        transfer: String,
        header: playback::ContinuationHeader,
    ) -> Result<PreparedContinuation, String> {
        let mut queue = self
            .database
            .prepare_queue_transfer(&transfer, header.total, &header.current)
            .await
            .map_err(string_error)?;
        queue.progress_millis = header.position_millis.min(i64::MAX as u64) as i64;
        queue.repeat_mode = header.repeat;
        queue.shuffled = header.shuffled;
        let occurrence = queue
            .occurrences
            .first()
            .cloned()
            .ok_or("The current track is unavailable")?;
        let mut request =
            StreamRequest::for_item(&occurrence.item, self.settings.playback_stream_quality());
        request.session_identifier = header.listen.as_ref().map(|listen| listen.play_id.clone());
        let connect = self
            .connect
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upgrade();
        if let Some(connect) = connect {
            connect.resolve_media(&occurrence).await?;
        }
        let source = self.source_owner();
        let stream = prepare_stream(&self.database, request, move |source_id| async move {
            tokio::task::spawn_blocking(move || {
                source
                    .ok_or_else(crate::source::source_access_unavailable)?
                    .client(&source_id)
            })
            .await
            .map_err(string_error)?
        })
        .await?;
        let (track, album) = self
            .database
            .playback_loudness(&occurrence.media_uri, &ReadCancellation::new())
            .await
            .map_err(string_error)?;
        let stream = prepare_media_stream(
            stream,
            playback::TrackLoudness {
                track: track.map(Box::new),
                album: album.map(Box::new),
            },
            occurrence,
        );
        let backend = (self.start_backend)()?;
        Ok(PreparedContinuation {
            transfer,
            continuation: playback::Continuation { header, queue },
            stream,
            backend,
        })
    }

    pub(crate) fn stop_continuation(
        &self,
        expected: playback::ContinuationHeader,
    ) -> Result<Option<playback::ContinuationHeader>, String> {
        let plex = self.plex.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(plex) = plex {
            return self.runtime.block_on(plex.stop_continuation(&expected));
        }
        self.active()
            .ok_or("Playback is unavailable")?
            .playback
            .stop_continuation(expected)
            .map_err(string_error)
    }

    pub(crate) async fn apply_continuation(
        &self,
        mut prepared: PreparedContinuation,
        final_header: playback::ContinuationHeader,
    ) -> Result<(), String> {
        if prepared.continuation.header.current != final_header.current
            || prepared
                .continuation
                .header
                .listen
                .as_ref()
                .map(|listen| &listen.play_id)
                != final_header.listen.as_ref().map(|listen| &listen.play_id)
        {
            return Err("The other device changed playback. Try continuing again.".into());
        }
        prepared.continuation.queue.progress_millis =
            final_header.position_millis.min(i64::MAX as u64) as i64;
        prepared.continuation.header = final_header;
        let (reply, result) = tokio::sync::oneshot::channel();
        self.store_sender
            .send(PlaybackStoreWork::Continue {
                prepared: Box::new(prepared),
                reply,
            })
            .await
            .map_err(string_error)?;
        result.await.map_err(string_error)?
    }

    pub(super) async fn commit_continuation(
        &self,
        prepared: PreparedContinuation,
    ) -> Result<(), String> {
        // Queue persistence has one writer. Older local saves complete before
        // transferred metadata becomes the active queue.
        save_pending_queue(&self.database, &self.pending_queue)
            .await
            .map_err(string_error)?;
        let active = self.active().ok_or("Playback is unavailable")?;
        let plex = self.plex.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(plex) = plex {
            plex.stop_for_continuation().await?;
            self.plex.lock().unwrap_or_else(|p| p.into_inner()).take();
        }
        self.database
            .commit_queue_transfer(&prepared.transfer, &prepared.continuation.queue)
            .await
            .map_err(string_error)?;
        tokio::task::spawn_blocking(move || {
            active.playback.adopt_continuation(
                prepared.continuation,
                prepared.stream,
                prepared.backend,
            )
        })
        .await
        .map_err(string_error)?
        .map_err(string_error)?;
        save_pending_queue(&self.database, &self.pending_queue)
            .await
            .map_err(string_error)?;
        self.output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .selected = playback::PlaybackOutput::Local;
        Ok(())
    }
}
