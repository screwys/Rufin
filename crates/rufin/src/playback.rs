//! Rufin crossings for compact Playback, Database Queue persistence, streams, and Activity.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, Sender as EventSender, TrySendError};
use library::{
    Database, ListenWrite, QueueCompactOccurrence, QueueProvenance, QueueRepeatMode,
    ReadCancellation,
};
use playback::{
    LoadedPlayRequest, OccurrenceId, Playback, PlaybackBackend, PlaybackProjection, PlaybackUpdate,
    PreparedStream, QueueCommandPort, QueueReorderRequest, RadioCommandPort, RadioPlayRequest,
    RandomPlayRequest, RepeatMode, RunId, SessionCommand, SessionEffect, SourceSessionEpoch,
    StreamRequest, TransportCommandPort,
};
use scrobbling::Scrobbler;
use sources::Source;
use tracing::{debug, warn};
use ui::runtime::{PlaybackPublication, VisualizerPublication};

use crate::loudness::LoudnessAnalysisOwner;
use crate::settings::SettingsFile;
use crate::source::{ActiveSource, SelectedSourceState, WeakActiveSource};
use crate::waveform::{WaveformMedia, WaveformOwner};
use lyrics::{LyricsContext, LyricsService};

#[derive(Clone)]
struct ActivePlayback {
    instance: u64,
    source_key: library::SourceKey,
    epoch: SourceSessionEpoch,
    selected: Arc<ActiveSource>,
    playback: Playback,
}

impl ActivePlayback {
    fn selected(&self) -> Option<Arc<SelectedSourceState>> {
        self.selected.resolve()
    }
    fn weak_selected(&self) -> WeakActiveSource {
        self.selected.downgrade()
    }
}

pub(crate) struct PreparedPlayback {
    active: Option<ActivePlayback>,
    projection: Option<PlaybackProjection>,
    activated: Arc<AtomicBool>,
}

impl Drop for PreparedPlayback {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            let _ = active.playback.shutdown();
        }
    }
}

pub(crate) struct PlaybackCutover;

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
    active: Mutex<Option<ActivePlayback>>,
    update_sender: async_channel::Sender<PlaybackWork>,
    pending_queue: Mutex<Option<playback::QueuePersistence>>,
    monotonic_origin: Instant,
    play_id_prefix: String,
    next_instance: AtomicU64,
    start_backend: Box<dyn Fn() -> Result<Box<dyn PlaybackBackend>, String> + Send + Sync>,
    cast: playback_cast::CastManager,
    output: Mutex<OutputSelection>,
}

struct PlaybackWork {
    instance: u64,
    source_key: library::SourceKey,
    epoch: SourceSessionEpoch,
    selected: Arc<ActiveSource>,
    update: PlaybackUpdate,
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
        let artwork_settings = settings.clone();
        let cast_artwork = artwork.clone();
        let cast_artwork_path = move |stream: &playback::PreparedStream| {
            cached_cast_artwork(&cast_artwork, &artwork_settings, stream.track.as_deref()?)
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
            active: Mutex::new(None),
            update_sender,
            pending_queue: Mutex::new(None),
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
                owner
                    .consume_update(
                        work.instance,
                        work.source_key,
                        work.epoch,
                        work.selected,
                        work.update,
                    )
                    .await;
            }
        });
        owner.update_discord_settings();
        owner
    }

    pub(crate) async fn prepare_selected(
        self: &Arc<Self>,
        session: Arc<ActiveSource>,
        selected: Arc<SelectedSourceState>,
    ) -> Result<PreparedPlayback, String> {
        let stored = self.settings.load();
        let restore = self
            .database
            .restore_queue(selected.source_key)
            .await
            .map_err(string_error)?;
        let library::QueueRestore {
            occurrences,
            current_occurrence,
            prepared_next_occurrence,
            progress_millis,
            repeat_mode,
            shuffled,
            current,
            prepared_next,
        } = restore;
        let current_occurrence = current_occurrence.map(OccurrenceId::new);
        let queue_empty = occurrences.is_empty();
        let entries = occurrences
            .into_iter()
            .map(|row| playback::SequenceEntry {
                occurrence: OccurrenceId::new(row.object_id),
                track_key: row.track_key,
                canonical_position: usize::try_from(row.canonical_position).unwrap_or_default(),
                provenance: playback_provenance(row.provenance),
            })
            .collect();
        let sequence = if queue_empty {
            let mut sequence = playback::Sequence::new(selected.source_key);
            sequence.set_repeat_mode(stored.ui.repeat_mode);
            sequence.set_shuffle_seed(stored.ui.shuffle_enabled, random_u64());
            sequence
        } else {
            playback::Sequence::restore(
                selected.source_key,
                entries,
                current_occurrence.clone(),
                playback_repeat(repeat_mode),
                shuffled,
                0,
                u64::try_from(progress_millis).unwrap_or_default(),
            )
            .map_err(string_error)?
        };
        let instance = self.next_instance.fetch_add(1, Ordering::AcqRel);
        let epoch = selected.source_session_epoch;
        let source_key = selected.source_key;
        let owner = Arc::downgrade(self);
        let clock_owner = Arc::downgrade(self);
        let selected_session = Arc::clone(&session);
        let activated = Arc::new(AtomicBool::new(false));
        let output_activated = Arc::clone(&activated);
        let backend_owner = Arc::clone(self);
        let (playback_output, backend) =
            tokio::task::spawn_blocking(move || backend_owner.take_selected_backend())
                .await
                .map_err(string_error)??;
        let (playback, _) = Playback::start(
            sequence,
            epoch,
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
                if output_activated.load(Ordering::Acquire)
                    && let Some(owner) = owner.upgrade()
                {
                    owner.queue_update(
                        instance,
                        source_key,
                        epoch,
                        Arc::clone(&selected_session),
                        update,
                    );
                }
            },
        )
        .map_err(string_error)?;
        if let Some(current) = current {
            let occurrence =
                current_occurrence.unwrap_or_else(|| OccurrenceId::new("restore:current"));
            playback
                .command(SessionCommand::MediaResolved {
                    occurrence,
                    media: Box::new(Some(current.into())),
                    prepared: false,
                    start_run: false,
                })
                .map_err(string_error)?;
        }
        if let Some(next) = prepared_next {
            let occurrence = prepared_next_occurrence
                .map(OccurrenceId::new)
                .unwrap_or_else(|| OccurrenceId::new("restore:prepared"));
            playback
                .command(SessionCommand::MediaResolved {
                    occurrence,
                    media: Box::new(Some(next.into())),
                    prepared: true,
                    start_run: false,
                })
                .map_err(string_error)?;
        }
        let projection = playback.projection().map_err(string_error)?;
        Ok(PreparedPlayback {
            active: Some(ActivePlayback {
                instance,
                source_key,
                epoch,
                selected: session,
                playback,
            }),
            projection: Some(projection),
            activated,
        })
    }

    pub(crate) fn install_prepared(
        &self,
        mut prepared: PreparedPlayback,
        _cutover: PlaybackCutover,
    ) -> PlaybackProjection {
        let active = prepared
            .active
            .take()
            .expect("prepared Playback has an active session");
        let projection = prepared
            .projection
            .take()
            .expect("prepared Playback has a projection");
        let selected = Arc::clone(&active.selected);
        *self.active.lock().unwrap_or_else(|p| p.into_inner()) = Some(active);
        prepared.activated.store(true, Ordering::Release);
        let playback = self.settings.load().ui.playback;
        self.loudness.settings_changed(
            playback.loudness_normalization,
            playback.loudness_normalization_scope,
            playback.write_ebu_r128_tags,
            Some(selected),
        );
        self.publish_selected_products(&projection);
        projection
    }

    pub(crate) fn stop_for_source_switch(&self) -> PlaybackCutover {
        if let Some(active) = self.take_active() {
            let _ = active.playback.retire();
        }
        PlaybackCutover
    }

    fn take_active(&self) -> Option<ActivePlayback> {
        let active = self.active.lock().unwrap_or_else(|p| p.into_inner()).take();
        self.loudness.cancel();
        self.publish_current_media(None);
        self.observe_discord(None, false);
        active
    }

    fn queue_update(
        self: &Arc<Self>,
        instance: u64,
        source_key: library::SourceKey,
        epoch: SourceSessionEpoch,
        selected: Arc<ActiveSource>,
        mut update: PlaybackUpdate,
    ) {
        if let Some(persistence) = update.queue_persistence.take() {
            let mut pending = self.pending_queue.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(current) = pending.as_mut() {
                current.coalesce(persistence);
            } else {
                *pending = Some(persistence);
            }
        }
        if let Some((run, levels)) = update.visualizer.take() {
            let frame = VisualizerPublication {
                source_key,
                source_session_epoch: epoch,
                run,
                levels,
            };
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
                source_key,
                epoch,
                selected,
                update,
            })
            .is_err()
        {
            warn!("Playback update consumer stopped");
        }
    }

    async fn consume_update(
        &self,
        instance: u64,
        source_key: library::SourceKey,
        epoch: SourceSessionEpoch,
        selected: Arc<ActiveSource>,
        update: PlaybackUpdate,
    ) {
        let Some(active) = self.active_matching(instance, source_key, epoch) else {
            return;
        };
        let persistence = self
            .pending_queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(persistence) = persistence {
            if let Err(error) = self.persist_queue(&active.playback, &persistence).await {
                warn!(%error, "could not persist compact Queue");
            }
        }
        for effect in update.effects {
            self.consume_effect(&active, &selected, effect).await;
        }
        if let Some(projection) = update.projection {
            if update.current_media_changed {
                self.publish_current_media(projection.view.transport.current.clone());
            }
            self.observe_discord(
                Some(&projection),
                projection.notices.iter().any(|notice| {
                    matches!(notice, playback::PlaybackNotice::PositionDiscontinuity(_))
                }),
            );
            self.publish_projection(PlaybackPublication {
                source_key,
                source_session_epoch: epoch,
                projection,
            });
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

    async fn persist_queue(
        &self,
        playback: &Playback,
        persistence: &playback::QueuePersistence,
    ) -> Result<(), String> {
        let revision = persistence.revision();
        let playback = playback.clone();
        self.database
            .persist_compact_queue(
                persistence.source_key(),
                persistence.total(),
                move |offset, limit| {
                    let playback = playback.clone();
                    async move {
                        let entries = playback
                            .queue_persistence_page(revision, offset, limit)
                            .map_err(|error| {
                                library::LibraryError::InvalidRequest(error.to_string())
                            })?
                            .ok_or_else(|| {
                                library::LibraryError::InvalidRequest(
                                    "Queue changed during persistence".to_string(),
                                )
                            })?;
                        Ok(entries
                            .iter()
                            .enumerate()
                            .map(|(position, entry)| queue_occurrence(entry, offset + position))
                            .collect())
                    }
                },
                persistence.current().map(OccurrenceId::as_str),
                persistence.prepared_next().map(OccurrenceId::as_str),
                persistence.progress_millis() as i64,
                library_repeat(persistence.repeat_mode()),
                persistence.shuffled(),
            )
            .await
            .map_err(string_error)?;
        Ok(())
    }

    async fn consume_effect(
        &self,
        active: &ActivePlayback,
        selected: &Arc<ActiveSource>,
        effect: SessionEffect,
    ) {
        match effect {
            SessionEffect::ResolveMedia {
                occurrence,
                prepared,
                ..
            } => {
                let media = self
                    .database
                    .queue_media_for_occurrence(active.source_key, occurrence.as_str())
                    .await
                    .ok()
                    .flatten();
                let media = playable_queue_media(media).await.map(Into::into);
                let _ = active.playback.command(SessionCommand::MediaResolved {
                    occurrence,
                    media: Box::new(media),
                    prepared,
                    start_run: true,
                });
            }
            SessionEffect::ResolveStream {
                run,
                occurrence,
                media,
                request,
                ..
            } => self.resolve_stream(active.clone(), run, occurrence, *media, request),
            SessionEffect::PersistProgress {
                source_id,
                occurrence,
                progress_millis,
                ..
            }
            | SessionEffect::PersistState {
                source_id,
                occurrence,
                progress_millis,
                ..
            } => {
                if let Err(error) = self
                    .database
                    .persist_queue_progress(
                        source_id,
                        occurrence.as_ref().map(OccurrenceId::as_str),
                        i64::try_from(progress_millis).unwrap_or(i64::MAX),
                    )
                    .await
                {
                    warn!(%error, "could not persist Queue progress");
                }
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
                if let playback::ListeningFact::Started { track, .. } = &fact {
                    self.scrobbler.now_playing(track);
                }
            }
            SessionEffect::Activity(activity) => self.record_activity(activity).await,
            SessionEffect::SourceReport(report)
                if external_source_reporting_enabled(self.settings.load().ui.private_mode) =>
            {
                if let Some(source) = selected.resolve().and_then(|state| state.source.clone()) {
                    self.runtime.spawn(async move {
                        let _ = source.report_playback(&report).await;
                    });
                }
            }
            SessionEffect::SourceReport(_) => {}
            SessionEffect::RequestAutoDj(request) => crate::radio::request_auto_dj(
                self.runtime.clone(),
                active.weak_selected(),
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
        let artist = activity.track.artists.join(", ");
        let deliveries = if activity.skipped {
            Vec::new()
        } else {
            self.scrobbler.listen_delivery_targets(&activity.track)
        };
        let listen = ListenWrite {
            external_id: activity.play_id,
            track_key: activity.track.track_key,
            track_object_id: activity.track.track_object_id,
            track_title: activity.track.title,
            artist_name: artist,
            album_title: activity.track.album.unwrap_or_default(),
            started_at: activity.started_at_unix_seconds,
            local_period: activity.local_period,
            duration_millis: i64::try_from(activity.track.duration_millis).unwrap_or(i64::MAX),
            listened_millis: i64::try_from(activity.listened_millis).unwrap_or(i64::MAX),
            skipped: activity.skipped,
        };
        match self
            .database
            .record_listen(activity.track.source_key, &listen, &deliveries)
            .await
        {
            Ok(_) => self.scrobbler.listen_recorded(deliveries.len()),
            Err(error) => warn!(%error, "could not record Rufin Activity"),
        }
    }

    fn resolve_stream(
        &self,
        active: ActivePlayback,
        run: RunId,
        _occurrence: OccurrenceId,
        media: playback::PlaybackMedia,
        request: StreamRequest,
    ) {
        let Some(selected) = active.selected() else {
            return;
        };
        let database = Arc::clone(&selected.database);
        let source = selected.source.clone();
        let playback = active.playback;
        let quality = request.quality;
        self.runtime.spawn(async move {
            let loudness = playback::TrackLoudness {
                track: match media.track_key {
                    Some(key) => database
                        .track_loudness(selected.source_key, key, &ReadCancellation::new())
                        .await
                        .ok()
                        .flatten()
                        .map(Box::new),
                    None => None,
                },
                album: match media.album_key {
                    Some(key) => database
                        .album_loudness(selected.source_key, key, &ReadCancellation::new())
                        .await
                        .ok()
                        .flatten()
                        .map(Box::new),
                    None => None,
                },
            };
            let result = prepare_stream(source, request)
                .await
                .map(|stream| prepare_media_stream(stream, loudness, media, quality));
            let _ = tokio::task::spawn_blocking(move || playback.resolve_stream(run, result)).await;
        });
    }

    pub(crate) fn waveform_setting_changed(&self, enabled: bool) {
        self.waveform.settings_changed(
            enabled,
            self.current_media()
                .and_then(|media| self.waveform_media(media)),
        );
    }
    pub(crate) fn playback_settings_changed(&self, settings: playback::PlaybackSettings) {
        self.loudness.settings_changed(
            settings.loudness_normalization,
            settings.loudness_normalization_scope,
            settings.write_ebu_r128_tags,
            self.active().map(|active| active.selected),
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
    pub(crate) fn stream_inputs_changed(
        &self,
        source_key: library::SourceKey,
        epoch: SourceSessionEpoch,
    ) -> Result<(), String> {
        let active = self
            .active_matching(0, source_key, epoch)
            .or_else(|| {
                self.active()
                    .filter(|a| a.source_key == source_key && a.epoch == epoch)
            })
            .ok_or_else(|| "selected Playback changed".to_string())?;
        active
            .playback
            .command(SessionCommand::StreamInputsChanged)
            .map_err(string_error)
    }
    pub(crate) fn catalog_changed(&self) {
        if let Some(active) = self.active() {
            let playback = self.settings.load().ui.playback;
            self.loudness.library_changed(
                playback.loudness_normalization,
                playback.loudness_normalization_scope,
                playback.write_ebu_r128_tags,
                Some(active.selected),
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
    fn active_matching(
        &self,
        instance: u64,
        source_key: library::SourceKey,
        epoch: SourceSessionEpoch,
    ) -> Option<ActivePlayback> {
        self.active().filter(|active| {
            (instance == 0 || active.instance == instance)
                && active.source_key == source_key
                && active.epoch == epoch
        })
    }
    fn active_for_media(&self, media: &playback::CurrentMedia) -> Option<ActivePlayback> {
        self.active().filter(|active| {
            active.source_key == media.id.source_key
                && active.epoch == media.id.source_session_epoch
        })
    }

    fn publish_current_media(&self, media: Option<Arc<playback::CurrentMedia>>) {
        let waveform = media
            .as_ref()
            .and_then(|media| self.waveform_media(Arc::clone(media)));
        self.waveform.current_changed(waveform);
        let Some(media) = media else {
            self.lyrics.set_current(None);
            return;
        };
        let Some(selected) = self
            .active_for_media(&media)
            .and_then(|active| active.selected())
        else {
            self.lyrics.set_current(None);
            return;
        };
        let Ok(input) = selected.configuration.input_identity() else {
            self.lyrics.set_current(None);
            return;
        };
        self.lyrics.set_current(Some(LyricsContext {
            media,
            input,
            source: selected.source.clone(),
            database: selected.database.as_ref().clone(),
        }));
    }

    fn waveform_media(&self, media: Arc<playback::CurrentMedia>) -> Option<WaveformMedia> {
        let selected = self.active_for_media(&media)?.selected()?;
        Some(WaveformMedia {
            request: StreamRequest::for_media(
                &media.track,
                self.settings.playback_stream_quality(),
            ),
            media,
            source: selected.source.clone(),
        })
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
    fn play_loaded(&self, request: LoadedPlayRequest) {
        let Some(active) = self.active().filter(|active| {
            active.source_key == request.source_key && active.epoch == request.source_session_epoch
        }) else {
            return;
        };
        let playback = active.playback;
        self.runtime.spawn(async move {
            let Some(reservation) = playback.admit_loaded(&request).ok().flatten() else {
                return;
            };
            let Some((batch, placement, anchor)) = request.compact_batch(random_u64()) else {
                return;
            };
            let _ = playback.complete_materialization(
                reservation.id,
                reservation.source_id,
                batch,
                placement,
                Some(anchor),
            );
        });
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
            occurrence: request.occurrence,
            target: request.target,
        });
    }
    fn clear(&self) {
        self.send(SessionCommand::ClearUpcoming);
    }
}

impl RadioCommandPort for PlaybackOwner {
    fn play_random(&self, request: RandomPlayRequest) {
        if let Some(active) = self.active() {
            let _ = crate::radio::play_random(
                self.runtime.clone(),
                active.weak_selected(),
                active.playback,
                request,
            );
        }
    }
    fn play_radio(&self, request: RadioPlayRequest) {
        if let Some(active) = self.active() {
            let _ = crate::radio::play_radio(
                self.runtime.clone(),
                active.weak_selected(),
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
        if let Some(active) = self.take_active() {
            let _ = active.playback.shutdown();
        }
    }
}

pub(crate) async fn prepare_stream(
    source: Option<Arc<Source>>,
    request: StreamRequest,
) -> Result<playback::ResolvedStream, String> {
    if let Some(uri) = request.media_uri.as_deref() {
        let mut stream = playback::ResolvedStream::new(uri);
        if let (Some(start), Some(end)) = (request.cue_start_millis, request.cue_end_millis) {
            stream = stream.with_window(start, end);
        }
        return Ok(stream);
    }
    let source = source.ok_or_else(crate::source::source_access_unavailable)?;
    source.stream(request).await.map_err(string_error)
}

fn prepare_media_stream(
    stream: playback::ResolvedStream,
    loudness: playback::TrackLoudness,
    media: playback::PlaybackMedia,
    quality: playback::StreamQuality,
) -> PreparedStream {
    let content_type = if quality.max_bitrate_kbps().is_some() {
        Some("audio/mpeg".to_string())
    } else {
        media.source_format.as_deref().and_then(audio_mime)
    };
    PreparedStream::new(stream, loudness).with_media(media, content_type)
}

fn cached_cast_artwork(
    artwork: &artwork::Artwork,
    settings: &SettingsFile,
    media: &playback::PlaybackMedia,
) -> Option<std::path::PathBuf> {
    let stored = settings.load();
    let binding = artwork::ArtworkBinding::opaque(media.artwork_binding.as_deref()?);
    let external = artwork::ExternalPolicy::new(
        stored.ui.external_metadata_enabled,
        stored.ui.allows_external_metadata_lookup(),
        stored.ui.lastfm_api_key,
    );
    let source = sources::SourceId::new(&media.source_id);
    [512, 256, 96].into_iter().find_map(|size| {
        let request = artwork::ArtworkRequest::new(binding.clone(), size, size)
            .with_external(external.clone());
        artwork.cache_only_file(&source, &request)
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

async fn playable_queue_media(
    mut media: Option<library::QueueMedia>,
) -> Option<library::QueueMedia> {
    let media = media.as_mut()?;
    media.media_uri = preferred_media_uri(
        media.download_media_uri.as_deref(),
        media.mapping_media_uri.as_deref(),
        media.source_media_uri.as_deref(),
    )
    .await;
    Some(media.clone())
}

async fn preferred_media_uri(
    download: Option<&str>,
    mapping: Option<&str>,
    source: Option<&str>,
) -> Option<String> {
    if local_media_exists(download).await {
        download.map(str::to_owned)
    } else if local_media_exists(mapping).await {
        mapping.map(str::to_owned)
    } else {
        source.map(str::to_owned)
    }
}

async fn local_media_exists(uri: Option<&str>) -> bool {
    let Some(uri) = uri else {
        return false;
    };
    let Ok(url) = url::Url::parse(uri) else {
        return false;
    };
    let Ok(path) = url.to_file_path() else {
        return false;
    };
    tokio::fs::metadata(path)
        .await
        .is_ok_and(|metadata| metadata.is_file())
}

fn playback_provenance(value: QueueProvenance) -> playback::Provenance {
    match value {
        QueueProvenance::Context {
            context_id,
            source_rank,
        } => playback::Provenance::Context {
            context_id: context_id.into(),
            source_rank: usize::try_from(source_rank).unwrap_or_default(),
        },
        QueueProvenance::Manual => playback::Provenance::Manual,
        QueueProvenance::Random => playback::Provenance::Random,
        QueueProvenance::Radio => playback::Provenance::Radio,
        QueueProvenance::AutoDj => playback::Provenance::AutoDj,
        QueueProvenance::Legacy => playback::Provenance::Legacy,
    }
}
fn library_provenance(value: &playback::Provenance) -> QueueProvenance {
    match value {
        playback::Provenance::Context {
            context_id,
            source_rank,
        } => QueueProvenance::Context {
            context_id: context_id.to_string(),
            source_rank: *source_rank as i64,
        },
        playback::Provenance::Manual => QueueProvenance::Manual,
        playback::Provenance::Random => QueueProvenance::Random,
        playback::Provenance::Radio => QueueProvenance::Radio,
        playback::Provenance::AutoDj => QueueProvenance::AutoDj,
        playback::Provenance::Legacy => QueueProvenance::Legacy,
    }
}
fn queue_occurrence(entry: &playback::SequenceEntry, traversal: usize) -> QueueCompactOccurrence {
    QueueCompactOccurrence {
        object_id: entry.occurrence.as_str().to_string(),
        track_key: entry.track_key,
        canonical_position: entry.canonical_position as i64,
        traversal_position: traversal as i64,
        provenance: library_provenance(&entry.provenance),
    }
}
fn playback_repeat(value: QueueRepeatMode) -> RepeatMode {
    match value {
        QueueRepeatMode::None => RepeatMode::Off,
        QueueRepeatMode::One => RepeatMode::One,
        QueueRepeatMode::All => RepeatMode::All,
    }
}
fn library_repeat(value: RepeatMode) -> QueueRepeatMode {
    match value {
        RepeatMode::Off => QueueRepeatMode::None,
        RepeatMode::One => QueueRepeatMode::One,
        RepeatMode::All => QueueRepeatMode::All,
    }
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
        external_source_reporting_enabled, preferred_media_uri, prepare_media_stream,
        prepend_playback_notices,
    };

    fn test_media(source_format: &str) -> playback::PlaybackMedia {
        playback::PlaybackMedia {
            source_id: "source".to_string(),
            track_key: None,
            track_object_id: "track".to_string(),
            title: "Track".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            album_display_artist: None,
            album_key: None,
            primary_artist_key: None,
            media_uri: None,
            artwork_binding: None,
            duration_millis: 180_000,
            disc_number: None,
            track_number: None,
            year: None,
            release_date: None,
            favorite: None,
            rating: None,
            is_downloaded: false,
            source_format: Some(source_format.to_string()),
            musicbrainz_recording_id: None,
            musicbrainz_release_track_id: None,
            musicbrainz_album_id: None,
            musicbrainz_release_group_id: None,
            primary_artist_musicbrainz_id: None,
            cue_path: None,
            cue_start_millis: None,
            cue_end_millis: None,
            artist_links: Vec::new(),
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
            test_media("flac"),
            playback::StreamQuality::Original,
        );

        assert_eq!(prepared.content_type.as_deref(), Some("audio/flac"));
    }

    #[test]
    fn provider_transcode_is_published_as_mpeg() {
        let prepared = prepare_media_stream(
            playback::ResolvedStream::new("https://provider.example/stream?id=track"),
            playback::TrackLoudness::default(),
            test_media("flac"),
            playback::StreamQuality::MaxBitrateKbps(320),
        );

        assert_eq!(prepared.content_type.as_deref(), Some("audio/mpeg"));
    }

    #[tokio::test]
    async fn stale_download_falls_back_to_mapping_before_provider_streaming() {
        let directory = tempfile::tempdir().expect("temporary playback files");
        let mapping = directory.path().join("mapped.flac");
        std::fs::write(&mapping, b"mapped").expect("write mapped Track");
        let download = url::Url::from_file_path(directory.path().join("stale.flac"))
            .expect("download URI")
            .to_string();
        let mapping = url::Url::from_file_path(mapping)
            .expect("mapping URI")
            .to_string();

        assert_eq!(
            preferred_media_uri(
                Some(&download),
                Some(&mapping),
                Some("https://provider.example/stream")
            )
            .await
            .as_deref(),
            Some(mapping.as_str())
        );
        assert_eq!(
            preferred_media_uri(
                Some(&download),
                Some("file:///also-missing.flac"),
                Some("https://provider.example/stream")
            )
            .await
            .as_deref(),
            Some("https://provider.example/stream")
        );
    }
}
