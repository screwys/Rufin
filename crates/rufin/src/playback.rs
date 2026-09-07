//! Rufin crossings for compact Playback, Database Queue persistence, streams, and Activity.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, Sender as EventSender, TrySendError};
use library::{Database, ListenWrite, ReadCancellation};
use playback::{
    OccurrenceId, PlayRequest, Playback, PlaybackBackend, PlaybackProjection, PlaybackUpdate,
    PreparedStream, QueueCommandPort, QueueReorderRequest, RadioCommandPort, RadioPlayRequest,
    RandomPlayRequest, RepeatMode, RunId, SessionCommand, SessionEffect, StreamRequest,
    TransportCommandPort,
};
use scrobbling::Scrobbler;
use tracing::{debug, warn};
use ui::runtime::{PlaybackPublication, VisualizerPublication};

use crate::loudness::LoudnessAnalysisOwner;
use crate::settings::SettingsFile;
use crate::source::SourceOwner;
use crate::waveform::{WaveformMedia, WaveformOwner};
use lyrics::{LyricsContext, LyricsService};

#[derive(Clone)]
struct ActivePlayback {
    instance: u64,
    playback: Playback,
}

struct OutputSelection {
    selected: playback::PlaybackOutput,
    prepared: Option<Box<dyn PlaybackBackend>>,
}

pub(crate) struct PlaybackOwner {
    database: Arc<Database>,
    settings: SettingsFile,
    runtime: tokio::runtime::Handle,
    events: EventSender<PlaybackPublication>,
    event_drain: Receiver<PlaybackPublication>,
    visualizer_events: EventSender<ui::runtime::VisualizerPublication>,
    visualizer_drain: Receiver<VisualizerPublication>,
    waveform: Arc<WaveformOwner>,
    loudness: Arc<LoudnessAnalysisOwner>,
    lyrics: Arc<LyricsService>,
    discord: Arc<desktop_integration::Discord>,
    scrobbler: Arc<Scrobbler>,
    source: Mutex<std::sync::Weak<SourceOwner>>,
    active: Mutex<Option<ActivePlayback>>,
    stream_tasks: Mutex<std::collections::HashMap<RunId, tokio::task::JoinHandle<()>>>,
    update_sender: async_channel::Sender<PlaybackWork>,
    pending_queue: Mutex<Option<playback::QueuePersistence>>,
    store_sender: async_channel::Sender<PlaybackStoreWork>,
    monotonic_origin: Instant,
    play_id_prefix: String,
    next_instance: AtomicU64,
    start_backend: Box<dyn Fn() -> Result<Box<dyn PlaybackBackend>, String> + Send + Sync>,
    cast: playback_cast::CastManager,
    output: Mutex<OutputSelection>,
}

struct PlaybackWork {
    flush: Option<std::sync::mpsc::SyncSender<()>>,
    instance: u64,
    update: PlaybackUpdate,
}

enum PlaybackStoreWork {
    Artwork {
        playback: Playback,
        media_uris: Vec<String>,
    },
    Activity(Box<playback::ActivityListen>),
    Flush(std::sync::mpsc::SyncSender<()>),
    Read {
        playback: Playback,
        id: u64,
        request: library::QueueReadRequest,
    },
    Settings(playback::QueuePersistence),
    Progress {
        current: Option<OccurrenceId>,
        progress: u64,
    },
}

impl PlaybackOwner {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new<StartBackend>(
        database: Arc<Database>,
        settings: SettingsFile,
        runtime: tokio::runtime::Handle,
        events: EventSender<PlaybackPublication>,
        event_drain: Receiver<PlaybackPublication>,
        visualizer_events: EventSender<ui::runtime::VisualizerPublication>,
        visualizer_drain: Receiver<VisualizerPublication>,
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
        let ui = settings.load().ui;
        let (update_sender, update_receiver) = async_channel::bounded(64);
        let (store_sender, store_receiver) = async_channel::unbounded();
        let artwork_settings = settings.clone();
        let cast_artwork = artwork.clone();
        let cast_artwork_path = move |stream: &playback::PreparedStream| {
            cached_cast_artwork(
                &cast_artwork,
                &artwork_settings,
                &stream.occurrence.as_ref()?.item,
            )
        };
        let owner = Arc::new(Self {
            database,
            settings,
            runtime: runtime.clone(),
            events,
            event_drain,
            visualizer_events,
            visualizer_drain,
            waveform,
            loudness: LoudnessAnalysisOwner::new(runtime),
            lyrics,
            discord,
            scrobbler,
            source: Mutex::new(std::sync::Weak::new()),
            active: Mutex::new(None),
            stream_tasks: Mutex::new(std::collections::HashMap::new()),
            update_sender,
            pending_queue: Mutex::new(None),
            store_sender,
            monotonic_origin: Instant::now(),
            play_id_prefix: random_identity(),
            next_instance: AtomicU64::new(1),
            start_backend: Box::new(start_backend),
            cast: playback_cast::CastManager::new(
                ui.cast_proxy_enabled,
                ui.cast_network_interface,
                cast_artwork_path,
            ),
            output: Mutex::new(OutputSelection {
                selected: playback::PlaybackOutput::Local,
                prepared: None,
            }),
        });
        let weak = Arc::downgrade(&owner);
        owner.runtime.spawn(async move {
            while let Ok(work) = update_receiver.recv().await {
                let Some(owner) = weak.upgrade() else { break };
                if let Some(flush) = work.flush {
                    let _ = owner.store_sender.try_send(PlaybackStoreWork::Flush(flush));
                } else {
                    owner.consume_update(work.instance, work.update);
                }
            }
        });
        let weak = Arc::downgrade(&owner);
        owner.runtime.spawn(async move {
            while let Ok(work) = store_receiver.recv().await {
                let Some(owner) = weak.upgrade() else { break };
                owner.consume_store(work).await;
            }
        });
        owner.update_discord_settings();
        owner
    }

    pub(crate) fn install_source_owner(&self, source: &Arc<SourceOwner>) {
        *self
            .source
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::downgrade(source);
    }

    pub(crate) async fn start(self: &Arc<Self>) -> Result<PlaybackProjection, String> {
        let stored = self.settings.load();
        let sequence = match self.database.restore_queue().await {
            Ok(mut restore) => {
                if restore.occurrences.is_empty() {
                    restore.repeat_mode = stored.ui.repeat_mode;
                    restore.shuffled = stored.ui.shuffle_enabled;
                }
                playback::Sequence::from_window(restore, 0).map_err(string_error)
            }
            Err(error) => Err(string_error(error)),
        }.unwrap_or_else(|error| {
            warn!(%error, "could not restore optional Queue; starting with an empty playback window");
            playback::Sequence::new()
        });
        let instance = self.next_instance.fetch_add(1, Ordering::AcqRel);
        let owner = Arc::downgrade(self);
        let clock_owner = Arc::downgrade(self);
        let backend_owner = Arc::clone(self);
        let (playback_output, backend) =
            tokio::task::spawn_blocking(move || backend_owner.take_selected_backend())
                .await
                .map_err(string_error)??;
        let (playback, _) = Playback::start(
            sequence,
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
            move |update| {
                if let Some(owner) = owner.upgrade() {
                    owner.queue_update(instance, update);
                }
            },
        )
        .map_err(string_error)?;
        *self.active.lock().unwrap_or_else(|p| p.into_inner()) = Some(ActivePlayback {
            instance,
            playback: playback.clone(),
        });
        let projection = playback.projection().map_err(string_error)?;
        self.publish_selected_products(&projection);
        self.publish_projection(PlaybackPublication {
            projection: projection.clone(),
        });
        Ok(projection)
    }

    pub(crate) fn remove_waveform_cache(&self, source: &sources::SourceId) -> Result<(), String> {
        self.waveform
            .remove_source_cache(source)
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn forget_source(&self, source: library::SourceKey) -> Result<(), String> {
        let occurrences = self
            .database
            .queue_occurrences_for_source(source)
            .await
            .map_err(string_error)?
            .into_iter()
            .map(OccurrenceId::new)
            .collect::<Vec<_>>();
        if occurrences.is_empty() {
            return Ok(());
        }
        if let Some(active) = self.active() {
            active
                .playback
                .command(SessionCommand::Forget(occurrences))
                .map_err(string_error)?;
        }
        Ok(())
    }

    fn take_active(&self) -> Option<ActivePlayback> {
        let active = self.active.lock().unwrap_or_else(|p| p.into_inner()).take();
        self.loudness.cancel();
        for (_, task) in self
            .stream_tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
        {
            task.abort();
        }
        self.publish_current_media(None);
        self.observe_discord(None, false);
        active
    }

    fn queue_update(self: &Arc<Self>, instance: u64, mut update: PlaybackUpdate) {
        if let Some(persistence) = update.queue_persistence.take() {
            let mut pending = self.pending_queue.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(current) = pending.as_mut() {
                current.coalesce(persistence);
            } else {
                *pending = Some(persistence);
            }
        }
        if let Some((run, levels)) = update.visualizer.take() {
            let frame = VisualizerPublication { run, levels };
            if let Err(TrySendError::Full(frame)) = self.visualizer_events.try_send(frame) {
                let _ = self.visualizer_drain.try_recv();
                let _ = self.visualizer_events.try_send(frame);
            }
        }
        if update.is_empty() {
            return;
        }
        if self
            .update_sender
            .send_blocking(PlaybackWork {
                instance,
                update,
                flush: None,
            })
            .is_err()
        {
            warn!("Playback update consumer stopped");
        }
    }

    fn consume_update(&self, instance: u64, mut update: PlaybackUpdate) {
        let Some(active) = self.active_matching(instance) else {
            return;
        };
        let queue_changed = update.queue_changed;
        let queue_projection = update.projection.as_ref().and_then(|projection| {
            queue_changed.then(|| PlaybackProjection {
                view: projection.view.clone(),
                notices: Vec::new(),
            })
        });
        if let Some(projection) = update.projection.take() {
            if update.current_media_changed {
                self.publish_current_media(projection.view.transport.current.clone());
            }
            self.observe_discord(
                Some(&projection),
                projection.notices.iter().any(|notice| {
                    matches!(notice, playback::PlaybackNotice::PositionDiscontinuity(_))
                }),
            );
            self.publish_projection(PlaybackPublication { projection });
        }
        let persistence = self
            .pending_queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(persistence) = persistence {
            let _ = self
                .store_sender
                .try_send(PlaybackStoreWork::Settings(persistence));
        }
        for effect in update.effects {
            self.consume_effect(&active, effect);
        }
        if let Some(projection) = queue_projection {
            // Queue pages read the durable occurrence owner. Re-publish only
            // after its matching snapshot commits so an early UI request can
            // never leave a newly populated Queue showing stale empty rows.
            self.publish_projection(PlaybackPublication { projection });
        }
    }

    fn publish_projection(&self, publication: PlaybackPublication) {
        let Err(TrySendError::Full(mut publication)) = self.events.try_send(publication) else {
            return;
        };
        if let Ok(previous) = self.event_drain.try_recv() {
            prepend_playback_notices(
                &mut publication.projection.notices,
                previous.projection.notices,
            );
        }
        let _ = self.events.try_send(publication);
    }

    async fn consume_store(&self, work: PlaybackStoreWork) {
        match work {
            PlaybackStoreWork::Artwork {
                playback,
                media_uris,
            } => match self.database.queue_artwork_for_uris(&media_uris).await {
                Ok(bindings) => {
                    let _ = playback.command(SessionCommand::ArtworkRefreshed(bindings));
                }
                Err(error) => warn!(%error, "could not refresh playback artwork"),
            },
            PlaybackStoreWork::Activity(activity) => self.record_activity(*activity).await,
            PlaybackStoreWork::Flush(reply) => {
                let _ = reply.send(());
            }
            PlaybackStoreWork::Read {
                playback,
                id,
                request,
            } => {
                let result = self
                    .database
                    .read_queue(request)
                    .await
                    .map_err(string_error);
                let _ = playback.command(SessionCommand::QueueComplete {
                    id,
                    result: Box::new(result),
                });
            }
            PlaybackStoreWork::Settings(state) => {
                if let Err(error) = self.database.save_queue(state.state()).await {
                    warn!(%error,"could not persist Queue settings");
                }
            }
            PlaybackStoreWork::Progress { current, progress } => {
                if let Err(error) = self
                    .database
                    .persist_queue_progress(current.as_ref(), progress as i64)
                    .await
                {
                    warn!(%error,"could not persist Queue progress");
                }
            }
        }
    }

    fn consume_effect(&self, active: &ActivePlayback, effect: SessionEffect) {
        match effect {
            SessionEffect::RefreshArtwork(media_uris) => {
                let _ = self.store_sender.try_send(PlaybackStoreWork::Artwork {
                    playback: active.playback.clone(),
                    media_uris,
                });
            }
            SessionEffect::CancelStream(run) => {
                if let Some(task) = self
                    .stream_tasks
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&run)
                {
                    task.abort();
                }
            }
            SessionEffect::Queue { id, request } => {
                let _ = self.store_sender.try_send(PlaybackStoreWork::Read {
                    playback: active.playback.clone(),
                    id,
                    request,
                });
            }
            SessionEffect::ResolveStream {
                run,
                occurrence,
                request,
                ..
            } => self.resolve_stream(active.clone(), run, occurrence, request),
            SessionEffect::PersistProgress {
                occurrence,
                progress_millis,
                ..
            }
            | SessionEffect::PersistState {
                occurrence,
                progress_millis,
                ..
            } => {
                let _ = self.store_sender.try_send(PlaybackStoreWork::Progress {
                    current: occurrence,
                    progress: progress_millis,
                });
            }
            SessionEffect::PersistOutputState {
                volume,
                muted,
                audio_output,
            } => {
                let _ = self.settings.update(|stored| {
                    stored.ui.playback.volume = volume;
                    stored.ui.playback.muted = muted;
                    stored.ui.playback.audio_output = audio_output;
                    Ok(())
                });
            }
            SessionEffect::Listening(fact) => {
                if let playback::ListeningFact::Started { item, .. } = &fact {
                    self.scrobbler.now_playing(item);
                }
            }
            SessionEffect::Activity(activity) => {
                let _ = self
                    .store_sender
                    .try_send(PlaybackStoreWork::Activity(activity));
            }
            SessionEffect::SourceReport(report)
                if external_source_reporting_enabled(self.settings.load().ui.private_mode) =>
            {
                if let Some((source_id, kind, _)) = library::source_entity_parts(&report.media_uri)
                    && kind == "track"
                {
                    let owner = self.source_owner();
                    self.runtime.spawn(async move {
                        if let Ok(Some(source)) =
                            tokio::task::spawn_blocking(move || owner?.client(&source_id).ok())
                                .await
                        {
                            let _ = source.report_playback(&report).await;
                        }
                    });
                }
            }
            SessionEffect::SourceReport(_) => {}
            SessionEffect::RequestAutoDj(request) => crate::radio::request_auto_dj(
                self.runtime.clone(),
                Arc::clone(&self.database),
                self.source_owner()
                    .map(|source| Arc::downgrade(&source))
                    .unwrap_or_default(),
                active.playback.clone(),
                request,
            ),
            SessionEffect::NonfatalError(error) => {
                debug!(%error, "Playback operation was not available")
            }
            SessionEffect::FatalError(error) => warn!(%error, "Playback session failed"),
            SessionEffect::FlushPersistence { .. }
            | SessionEffect::Backend(_)
            | SessionEffect::CurrentMediaChanged
            | SessionEffect::PositionDiscontinuity(_)
            | SessionEffect::Visualizer { .. } => {}
        }
    }

    async fn record_activity(&self, activity: playback::ActivityListen) {
        let deliveries = if activity.skipped {
            Vec::new()
        } else {
            self.scrobbler.listen_delivery_targets(&activity.item)
        };
        let listen = ListenWrite {
            external_id: Some(activity.play_id),
            media_uri: activity.item.media_uri,
            title: activity.item.title,
            artist: activity.item.artist,
            album: activity.item.album,
            duration_millis: activity.item.duration_millis,
            disc_number: activity.item.disc_number,
            track_number: activity.item.track_number,
            year: activity.item.year,
            release_date: activity.item.release_date,
            source_format: activity.item.source_format,
            musicbrainz_recording_id: activity.item.musicbrainz_recording_id,
            musicbrainz_release_track_id: activity.item.musicbrainz_release_track_id,
            started_at: activity.started_at_unix_seconds,
            local_period: activity.local_period,
            listened_millis: i64::try_from(activity.listened_millis).unwrap_or(i64::MAX),
            skipped: activity.skipped,
        };
        match self.database.record_listen(&listen, &deliveries).await {
            Ok(_) => self.scrobbler.listen_recorded(deliveries.len()),
            Err(error) => warn!(%error, "could not record Rufin Activity"),
        }
    }

    fn resolve_stream(
        &self,
        active: ActivePlayback,
        run: RunId,
        occurrence: Arc<playback::QueueOccurrence>,
        request: StreamRequest,
    ) {
        let database = Arc::clone(&self.database);
        let source_owner = self.source_owner();
        let playback = active.playback;
        let task = self.runtime.spawn(async move {
            let (track, album) = database
                .playback_loudness(&occurrence.item.media_uri, &ReadCancellation::new())
                .await
                .unwrap_or_default();
            let loudness = playback::TrackLoudness {
                track: track.map(Box::new),
                album: album.map(Box::new),
            };
            let result = prepare_stream(&database, request, move |source_id| {
                source_owner
                    .ok_or_else(crate::source::source_access_unavailable)?
                    .client(source_id)
            })
            .await
            .map(|stream| prepare_media_stream(stream, loudness, occurrence));
            let _ = tokio::task::spawn_blocking(move || playback.resolve_stream(run, result)).await;
        });
        if let Some(previous) = self
            .stream_tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(run, task)
        {
            previous.abort();
        }
    }

    pub(crate) fn waveform_setting_changed(&self, enabled: bool) {
        self.waveform.settings_changed(
            enabled,
            self.current_media().map(|media| self.waveform_media(media)),
        );
    }
    pub(crate) fn playback_settings_changed(&self, settings: playback::PlaybackSettings) {
        self.loudness.settings_changed(
            settings.loudness_normalization,
            settings.loudness_normalization_scope,
            settings.write_ebu_r128_tags,
            self.source_owner()
                .and_then(|source| source.current_session()),
        );
        self.send(SessionCommand::UpdateSettings(settings));
    }
    pub(crate) fn cast_proxy_setting_changed(&self, enabled: bool) {
        self.cast.set_proxy_media(enabled);
    }
    pub(crate) fn cast_network_setting_changed(&self, network: Option<String>) {
        self.cast.set_network_interface(network);
    }
    pub(crate) fn auto_dj_threshold_changed(&self, enabled: bool, threshold: u8) {
        self.send(SessionCommand::SetAutoDj {
            enabled,
            refill_threshold: usize::from(threshold),
        });
    }
    pub(crate) fn stream_inputs_changed(&self) -> Result<(), String> {
        let active = self
            .active()
            .ok_or_else(|| "Playback is unavailable".to_string())?;
        active
            .playback
            .command(SessionCommand::StreamInputsChanged)
            .map_err(string_error)
    }
    pub(crate) fn catalog_changed(&self) {
        if self.active().is_some() {
            self.send(SessionCommand::CatalogChanged);
            let playback = self.settings.load().ui.playback;
            self.loudness.library_changed(
                playback.loudness_normalization,
                playback.loudness_normalization_scope,
                playback.write_ebu_r128_tags,
                self.source_owner()
                    .and_then(|source| source.current_session()),
            );
        }
    }
    pub(crate) fn current_media(&self) -> Option<Arc<playback::CurrentMedia>> {
        self.active()
            .and_then(|active| active.playback.current_media().ok().flatten())
    }

    fn active(&self) -> Option<ActivePlayback> {
        self.active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn source_owner(&self) -> Option<Arc<SourceOwner>> {
        self.source
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .upgrade()
    }
    fn active_matching(&self, instance: u64) -> Option<ActivePlayback> {
        self.active()
            .filter(|active| instance == 0 || active.instance == instance)
    }
    fn publish_current_media(&self, media: Option<Arc<playback::CurrentMedia>>) {
        let waveform = media
            .as_ref()
            .map(|media| self.waveform_media(Arc::clone(media)));
        self.waveform.current_changed(waveform);
        let Some(media) = media else {
            self.lyrics.set_current(None);
            return;
        };
        let source_owner = self.source_owner();
        let input_digest = library::source_entity_parts(&media.media_uri)
            .and_then(|(source_id, _, _)| source_owner.as_ref()?.configuration(&source_id))
            .and_then(|configuration| configuration.input_identity().ok())
            .map_or([0; 32], |input| input.digest);
        let source_owner = source_owner
            .map(|source| Arc::downgrade(&source))
            .unwrap_or_default();
        self.lyrics.set_current(Some(LyricsContext {
            media,
            input_digest,
            source: Arc::new(move |source_id| source_owner.upgrade()?.client(source_id).ok()),
            database: self.database.as_ref().clone(),
        }));
    }

    fn waveform_media(&self, media: Arc<playback::CurrentMedia>) -> WaveformMedia {
        WaveformMedia {
            request: StreamRequest::for_item(&media.item, self.settings.playback_stream_quality()),
            media,
            source: self
                .source_owner()
                .map(|source| Arc::downgrade(&source))
                .unwrap_or_default(),
            database: Arc::clone(&self.database),
        }
    }

    pub(crate) fn publish_selected_products(&self, projection: &PlaybackProjection) {
        self.publish_current_media(projection.view.transport.current.clone());
        self.observe_discord(Some(projection), false);
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
            projection.as_ref().map(|p| &p.view),
        );
    }
    fn observe_discord(&self, projection: Option<&PlaybackProjection>, discontinuity: bool) {
        self.discord
            .observe(projection.map(|p| &p.view), discontinuity);
    }
    fn send(&self, command: SessionCommand) {
        if let Some(active) = self.active() {
            if let Err(error) = active.playback.command(command) {
                warn!(%error,"Playback command failed");
            }
        }
    }

    fn take_selected_backend(
        &self,
    ) -> Result<(playback::PlaybackOutput, Box<dyn PlaybackBackend>), String> {
        let (selected, prepared) = {
            let mut output = self.output.lock().unwrap_or_else(|p| p.into_inner());
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
            .unwrap_or_else(|p| p.into_inner())
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
            let mut output = self.output.lock().unwrap_or_else(|p| p.into_inner());
            output.selected = selected;
            output.prepared = None;
        } else {
            let mut output = self.output.lock().unwrap_or_else(|p| p.into_inner());
            output.selected = selected;
            output.prepared = Some(backend);
        }
        Ok(())
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
}

fn prepend_playback_notices(
    current: &mut Vec<playback::PlaybackNotice>,
    mut previous: Vec<playback::PlaybackNotice>,
) {
    previous.append(current);
    *current = previous;
}

impl QueueCommandPort for PlaybackOwner {
    fn play(&self, mut request: PlayRequest) {
        let Some(active) = self.active() else {
            return;
        };
        let playback = active.playback;
        self.runtime.spawn(async move {
            let shuffled = playback
                .projection()
                .is_ok_and(|projection| projection.view.controls.shuffle_enabled);
            request.shuffled_start &=
                shuffled && request.placement == playback::QueuePlacement::Now;
            let Some(reservation) = playback.admit_play(&request).ok().flatten() else {
                return;
            };
            let seed = random_u64();
            let (batch, placement) = request.compact_batch(seed);
            let _ = playback.complete_materialization(reservation.id, batch, placement);
        });
    }
    fn insert(&self, input: library::QueueInput, target: playback::QueueReorderTarget) {
        self.send(SessionCommand::Insert { input, target });
    }
    fn remove(&self, occurrence: OccurrenceId) {
        self.send(SessionCommand::Remove(occurrence));
    }
    fn remove_many(&self, occurrences: Vec<OccurrenceId>) {
        self.send(SessionCommand::RemoveMany(occurrences));
    }
    fn activate(&self, occurrence: OccurrenceId) {
        self.send(SessionCommand::Activate(occurrence));
    }
    fn move_after_current(&self, occurrence: OccurrenceId) {
        self.send(SessionCommand::MoveAfterCurrent(occurrence));
    }
    fn reorder(&self, request: QueueReorderRequest) {
        self.send(SessionCommand::Reorder {
            occurrences: request.occurrences,
            target: request.target,
        });
    }
    fn clear(&self, include_current: bool) {
        self.send(SessionCommand::Clear { include_current });
    }
}

impl RadioCommandPort for PlaybackOwner {
    fn play_random(&self, request: RandomPlayRequest) {
        if let (Some(active), Some(selected)) = (
            self.active(),
            self.source_owner()
                .and_then(|source| source.current_session()),
        ) {
            let _ = crate::radio::play_random(
                self.runtime.clone(),
                selected.downgrade(),
                active.playback,
                request,
            );
        }
    }
    fn play_radio(&self, request: RadioPlayRequest) {
        if let (Some(active), Some(selected)) = (
            self.active(),
            self.source_owner()
                .and_then(|source| source.current_session()),
        ) {
            let _ = crate::radio::play_radio(
                self.runtime.clone(),
                selected.downgrade(),
                active.playback,
                request,
            );
        }
    }
}

impl TransportCommandPort for PlaybackOwner {
    fn play_pause(&self) {
        self.send(SessionCommand::PlayPause)
    }
    fn play(&self) {
        self.send(SessionCommand::Play)
    }
    fn pause(&self) {
        self.send(SessionCommand::Pause)
    }
    fn stop(&self) {
        self.send(SessionCommand::Stop)
    }
    fn next(&self) {
        self.send(SessionCommand::Next)
    }
    fn previous(&self) {
        self.send(SessionCommand::Previous)
    }
    fn seek_seconds(&self, seconds: u32) {
        self.seek_millis(u64::from(seconds) * 1000)
    }
    fn seek_millis(&self, millis: u64) {
        self.send(SessionCommand::Seek(millis))
    }
    fn set_volume(&self, volume: f64) {
        self.send(SessionCommand::SetVolume(volume))
    }
    fn persist_volume(&self, volume: f64) {
        self.set_volume(volume);
        self.send(SessionCommand::PersistOutputState)
    }
    fn set_muted(&self, muted: bool) {
        self.send(SessionCommand::SetMuted(muted))
    }
    fn toggle_shuffle(&self) {
        let enabled = self
            .settings
            .update(|stored| {
                stored.ui.shuffle_enabled = !stored.ui.shuffle_enabled;
                Ok(stored.ui.shuffle_enabled)
            })
            .unwrap_or(false);
        self.set_shuffle(enabled)
    }
    fn set_shuffle(&self, enabled: bool) {
        let _ = self.settings.update(|stored| {
            stored.ui.shuffle_enabled = enabled;
            Ok(())
        });
        self.send(SessionCommand::SetShuffle {
            enabled,
            seed: random_u64(),
        })
    }
    fn cycle_repeat(&self) {
        let repeat = self
            .settings
            .update(|stored| {
                stored.ui.repeat_mode = next_repeat(stored.ui.repeat_mode);
                Ok(stored.ui.repeat_mode)
            })
            .unwrap_or(RepeatMode::Off);
        self.set_repeat(repeat)
    }
    fn set_repeat(&self, repeat: RepeatMode) {
        let _ = self.settings.update(|stored| {
            stored.ui.repeat_mode = repeat;
            Ok(())
        });
        self.send(SessionCommand::SetRepeat(repeat))
    }
    fn toggle_auto_dj(&self) {
        let (value, threshold) = self
            .settings
            .update(|stored| {
                stored.ui.auto_dj_enabled = !stored.ui.auto_dj_enabled;
                Ok((
                    stored.ui.auto_dj_enabled,
                    stored.ui.auto_dj_refill_threshold,
                ))
            })
            .unwrap_or((false, 1));
        self.send(SessionCommand::SetAutoDj {
            enabled: value,
            refill_threshold: usize::from(threshold),
        })
    }
    fn set_visualizer_enabled(&self, enabled: bool) {
        self.send(SessionCommand::SetVisualizerEnabled(enabled))
    }
    fn available_audio_outputs(&self) -> Vec<playback::AudioOutput> {
        playback_gstreamer::available_audio_outputs()
    }
    fn available_cast_networks(&self) -> Vec<playback::CastNetwork> {
        self.cast.available_networks().unwrap_or_default()
    }
    fn playback_output(&self) -> playback::PlaybackOutput {
        self.output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
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
            .unwrap_or_else(|p| p.into_inner())
            .prepared
            .take()
        {
            let _ = backend.shutdown();
        }
        if let Some(active) = self.active() {
            let _ = active.playback.shutdown();
        }
        let (reply, done) = std::sync::mpsc::sync_channel(0);
        if self
            .update_sender
            .send_blocking(PlaybackWork {
                instance: 0,
                update: PlaybackUpdate::default(),
                flush: Some(reply),
            })
            .is_ok()
        {
            let _ = done.recv();
        }
        self.take_active();
    }
}

pub(crate) async fn prepare_stream(
    database: &Database,
    request: StreamRequest,
    source: impl FnOnce(&sources::SourceId) -> Result<Arc<sources::Source>, String> + Send + 'static,
) -> Result<playback::ResolvedStream, String> {
    let access = database
        .playback_access(&request.media_uri)
        .await
        .ok()
        .flatten();
    // Downloads names its owned file using the actual transcoded extension.
    // Mapped and original files still use their parsed source format.
    let content_type = access
        .as_ref()
        .filter(|(_, origin)| *origin == library::LocalAccessOrigin::Download)
        .and_then(|(uri, _)| library::file_media_path(uri))
        .and_then(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .and_then(audio_mime)
        });
    let access_uri = access.map(|(uri, _)| uri);
    if let Some((_, file_uri, start, end)) = library::cue_media_parts(&request.media_uri) {
        return Ok(
            playback::ResolvedStream::new(access_uri.unwrap_or(file_uri))
                .with_content_type(content_type)
                .with_window(
                    u64::try_from(start).map_err(string_error)?,
                    u64::try_from(end).map_err(string_error)?,
                ),
        );
    }
    if let Some(uri) =
        access_uri.or_else(|| library::normalize_direct_media_uri(&request.media_uri))
    {
        return Ok(playback::ResolvedStream::new(uri).with_content_type(content_type));
    }
    let (source_id, kind, _) = library::source_entity_parts(&request.media_uri)
        .ok_or_else(crate::source::source_access_unavailable)?;
    if kind != "track" {
        return Err(crate::source::source_access_unavailable());
    }
    let source = tokio::task::spawn_blocking(move || source(&source_id))
        .await
        .map_err(string_error)??;
    source.stream(database, request).await.map_err(string_error)
}

fn prepare_media_stream(
    stream: playback::ResolvedStream,
    loudness: playback::TrackLoudness,
    occurrence: Arc<playback::QueueOccurrence>,
) -> PreparedStream {
    let content_type = occurrence.source_format.as_deref().and_then(audio_mime);
    PreparedStream::new(stream, loudness).with_occurrence(occurrence, content_type)
}

fn cached_cast_artwork(
    artwork: &artwork::Artwork,
    settings: &SettingsFile,
    media: &playback::QueueItem,
) -> Option<std::path::PathBuf> {
    let stored = settings.load();
    let binding = artwork::ArtworkBinding::opaque(media.artwork_binding.as_deref()?);
    let external = artwork::ExternalPolicy::new(
        stored.ui.external_metadata_enabled,
        stored.ui.allows_external_metadata_lookup(),
        stored.ui.lastfm_api_key,
    );
    [512, 256, 96].into_iter().find_map(|size| {
        let request = artwork::ArtworkRequest::new(binding.clone(), size, size)
            .with_external(external.clone());
        artwork.cache_only_file(&request)
    })
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

const fn next_repeat(value: RepeatMode) -> RepeatMode {
    match value {
        RepeatMode::Off => RepeatMode::All,
        RepeatMode::All => RepeatMode::One,
        RepeatMode::One => RepeatMode::Off,
    }
}
pub(crate) fn random_u64() -> u64 {
    let mut bytes = [0; 8];
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
fn external_source_reporting_enabled(private_mode: bool) -> bool {
    !private_mode
}
fn string_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        external_source_reporting_enabled, prepare_media_stream, prepend_playback_notices,
    };

    fn test_media(source_format: &str) -> playback::QueueItem {
        playback::QueueItem {
            media_uri: library::source_entity_uri(
                &library::SourceId::new("source"),
                "track",
                "track",
            ),
            title: "Track".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            album_display_artist: None,
            artwork_binding: None,
            duration_millis: 180_000,
            disc_number: None,
            track_number: None,
            year: None,
            release_date: None,
            source_format: Some(source_format.to_string()),
            musicbrainz_recording_id: None,
            musicbrainz_release_track_id: None,
            musicbrainz_album_id: None,
            musicbrainz_release_group_id: None,
            primary_artist_musicbrainz_id: None,
        }
    }

    fn test_occurrence(source_format: &str) -> playback::QueueOccurrence {
        playback::QueueOccurrence {
            occurrence: playback::OccurrenceId::new("test-occurrence"),
            item: test_media(source_format),
            canonical_position: 0,
            source_index: None,
            playlist_entry_id: None,
            provenance: playback::Provenance::Manual,
        }
    }

    #[test]
    fn private_mode_gates_only_external_source_reporting() {
        assert!(external_source_reporting_enabled(false));
        assert!(!external_source_reporting_enabled(true));
    }

    #[test]
    fn coalesced_playback_publication_keeps_structural_notices_in_order() {
        let mut current = vec![playback::PlaybackNotice::RunStarted(playback::RunId::new(
            2,
        ))];
        prepend_playback_notices(
            &mut current,
            vec![playback::PlaybackNotice::RunStarted(playback::RunId::new(
                1,
            ))],
        );
        assert_eq!(
            current,
            [
                playback::PlaybackNotice::RunStarted(playback::RunId::new(1)),
                playback::PlaybackNotice::RunStarted(playback::RunId::new(2)),
            ]
        );
    }

    #[test]
    fn prepared_original_stream_keeps_cast_format() {
        let prepared = prepare_media_stream(
            playback::ResolvedStream::new("https://provider.example/stream?id=track"),
            playback::TrackLoudness::default(),
            test_occurrence("flac").into(),
        );

        assert_eq!(prepared.content_type.as_deref(), Some("audio/flac"));
    }

    #[test]
    fn provider_transcode_is_published_as_mpeg() {
        let prepared = prepare_media_stream(
            playback::ResolvedStream::new("https://provider.example/stream?id=track")
                .with_content_type(Some("audio/mpeg".to_string())),
            playback::TrackLoudness::default(),
            test_occurrence("flac").into(),
        );

        assert_eq!(prepared.content_type.as_deref(), Some("audio/mpeg"));
    }

    #[tokio::test]
    async fn downloaded_transcode_keeps_its_actual_format_and_canonical_occurrence() {
        let folder = tempfile::tempdir().unwrap();
        let database = library::Database::open(folder.path().join("library.sqlite3"))
            .await
            .unwrap();
        let occurrence = test_occurrence("flac");
        let path = folder.path().join("track.opus");
        let uri = url::Url::from_file_path(&path).unwrap().to_string();
        database
            .upsert_local_access(
                None,
                &library::LocalAccessWrite {
                    media_uri: occurrence.media_uri.clone(),
                    origin: library::LocalAccessOrigin::Download,
                    path: path.to_string_lossy().into_owned(),
                    root: folder.path().to_string_lossy().into_owned(),
                    relative_path: "track.opus".into(),
                    size_bytes: 0,
                    mtime_ns: 0,
                    device_id: None,
                    inode: None,
                    parser_version: 1,
                    title: "Track".into(),
                    album: "Album".into(),
                    artist: "Artist".into(),
                    disc_number: 1,
                    track_number: 1,
                    duration_millis: 180_000,
                    access_uri: uri.clone(),
                    loudness_analysis_key: None,
                },
            )
            .await
            .unwrap();
        let stream = super::prepare_stream(
            &database,
            playback::StreamRequest::new(
                occurrence.media_uri.clone(),
                playback::StreamQuality::Original,
            ),
            |_| panic!("a completed download must resolve without contacting its source"),
        )
        .await
        .unwrap();
        let canonical = occurrence.media_uri.clone();
        let prepared = prepare_media_stream(
            stream,
            playback::TrackLoudness::default(),
            occurrence.into(),
        );
        assert_eq!(prepared.uri(), uri);
        assert_eq!(prepared.content_type.as_deref(), Some("audio/ogg"));
        assert_eq!(prepared.occurrence.as_ref().unwrap().media_uri, canonical);
    }
}
