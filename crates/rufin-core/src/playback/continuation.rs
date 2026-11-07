//! Another device's queue continues here, as an ordinary queue restore.
use super::*;
use state::ContinuedListen;

/// A queue another player played, to go on here.
pub(super) struct Adoption {
    pub(super) queue: library::QueueRestore,
    /// Its current row.
    pub(super) row: Arc<playback::QueueRow>,
    pub(super) request: StreamRequest,
    /// The client that streams it, keeping its session; None reads the source's.
    pub(super) client: Option<Arc<sources::Source>>,
    pub(super) playing: bool,
    pub(super) listen: Option<ContinuedListen>,
}

impl PlaybackOwner {
    pub(crate) async fn return_empty_local(&self) -> Result<(), String> {
        let active = self.active().ok_or("Playback is unavailable")?;
        let backend =
            (self.start_backend)(self.processing_receiver(&playback::PlaybackOutput::Local))?;
        if let Some(receiver) = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).take() {
            receiver.cancel();
        }
        tokio::task::spawn_blocking(move || {
            active.replace_backend(playback::PlaybackOutput::Local, backend)
        })
        .await
        .map_err(string_error)?
        .map_err(string_error)?;
        *self.output.lock().unwrap_or_else(|p| p.into_inner()) = playback::PlaybackOutput::Local;
        Ok(())
    }

    /// Replaces the queue with `queue` and resumes its current entry at its position on this
    /// device's own output, carrying on `listen` when another device started it.
    pub(crate) async fn continue_queue(
        &self,
        mut queue: library::QueueRestore,
        playing: bool,
        listen: Option<ContinuedListen>,
    ) -> Result<(), String> {
        let current = queue
            .current_index
            .ok_or("The current track is unavailable")?;
        let entry = queue
            .order
            .get(current)
            .and_then(|member| queue.entries.get(*member as usize))
            .cloned()
            .ok_or("The current track is unavailable")?;
        queue.rows = self
            .catalog
            .queue_rows_for_entries(&[(current, entry)], &ReadCancellation::new())
            .await
            .map_err(string_error)?;
        let occurrence = queue
            .rows
            .first()
            .cloned()
            .ok_or("The current track is unavailable")?;
        let mut request = StreamRequest::new(
            (&occurrence.track).media_uri.clone(),
            self.settings.playback_stream_quality(),
        );
        // The stream reports under the listen it continues.
        request.session_identifier = listen.as_ref().map(|listen| listen.play_id.clone());
        let backend =
            (self.start_backend)(self.processing_receiver(&playback::PlaybackOutput::Local))?;
        let active = self.active().ok_or("Playback is unavailable")?;
        if let Some(receiver) = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).take() {
            receiver.cancel();
        }
        let plex = self.plex.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(plex) = plex {
            plex.stop_for_continuation().await?;
            self.plex.lock().unwrap_or_else(|p| p.into_inner()).take();
        }
        self.adopt_here(
            active,
            Adoption {
                queue,
                row: occurrence,
                request,
                client: None,
                playing,
                listen,
            },
            backend,
        )
        .await?;
        *self.output.lock().unwrap_or_else(|p| p.into_inner()) = playback::PlaybackOutput::Local;
        Ok(())
    }

    /// Plays a queue here from its current row: reads the row's playable, prepares its
    /// stream and hands both to `playback`, which plays them on `backend`.
    pub(super) async fn adopt_here(
        &self,
        playback: actor::Playback,
        adoption: Adoption,
        backend: Box<dyn PlaybackBackend>,
    ) -> Result<(), String> {
        let Adoption {
            queue,
            row,
            request,
            client,
            playing,
            listen,
        } = adoption;
        let playable = self
            .catalog
            .playable(row.track.track_key)
            .await
            .map_err(string_error)?
            .ok_or("The current track is unavailable")?;
        let local = self.settings.local_configuration();
        let source = Arc::clone(&self.source);
        // A fixed client keeps its session; otherwise a download or the source plays.
        let downloads = client.is_none().then(|| source.downloads().clone());
        let stream = prepare_stream(
            &playable,
            request,
            local.as_ref(),
            downloads.as_ref(),
            move |source_id| async move {
                match client {
                    Some(client) => Ok(client),
                    None => tokio::task::spawn_blocking(move || source.client(&source_id))
                        .await
                        .map_err(string_error)?,
                }
            },
        )
        .await
        .map_err(string_error)?;
        let stream = prepare_media_stream(stream, super::track_loudness(&playable), row);
        tokio::task::spawn_blocking(move || {
            playback.adopt_local(queue, stream, playing, listen, backend)
        })
        .await
        .map_err(string_error)?
        .map_err(string_error)
    }

    /// The listen of the track this device plays, for a device that continues it.
    pub(crate) async fn current_listen(&self) -> Option<ContinuedListen> {
        let active = self.active()?;
        tokio::task::spawn_blocking(move || active.current_listen())
            .await
            .ok()?
            .ok()
            .flatten()
    }
}
