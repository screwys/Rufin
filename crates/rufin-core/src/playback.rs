//! Playback controls, source jobs, and persistent playback facts.
pub(crate) mod actor;
mod connect;
mod continuation;
mod plex;
mod queue;
pub(crate) mod state;
pub(crate) use continuation::PreparedContinuation;
#[cfg(test)]
mod queue_tests;

pub fn seek_preview_matches_position(target_millis: u64, position_millis: u64) -> bool {
    const TOLERANCE_MILLIS: u64 = 1_500;
    (target_millis.saturating_sub(TOLERANCE_MILLIS)
        ..=target_millis.saturating_add(TOLERANCE_MILLIS))
        .contains(&position_millis)
}

use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::runtime::VisualizerPublication;
use actor::{Playback, WeakPlayback};
use async_channel::{Receiver, Sender as EventSender, TrySendError};
use library::QueueUpdate;
use library::{Database, ListenWrite, ReadCancellation};
use playback::{
    PlayRequest, PlaybackBackend, PlaybackProjection, PreparedStream, QueueCommandPort,
    QueueReorderRequest, RadioCommandPort, RadioPlayRequest, RandomPlayRequest, RepeatMode, RunId,
    StreamRequest, TransportCommandPort,
};
use queue::Sequence;
use scrobbling::Scrobbler;
use state::{AutoDjRequest, ClockSample, Command};
use tracing::{debug, warn};

use crate::loudness::LoudnessAnalysisOwner;
use crate::lyrics::{LyricsContext, LyricsService};
use crate::settings::SettingsFile;
use crate::source::SourceOwner;
use crate::waveform::{WaveformMedia, WaveformOwner};

struct OutputSelection {
    selected: playback::PlaybackOutput,
    prepared: Option<Box<dyn PlaybackBackend>>,
}

pub(crate) struct PlaybackOwner {
    database: Arc<Database>,
    settings: SettingsFile,
    runtime: tokio::runtime::Handle,
    pub(crate) updates: playback::PlaybackUpdates,
    visualizer_events: EventSender<crate::runtime::VisualizerPublication>,
    visualizer_drain: Receiver<VisualizerPublication>,
    waveform: Arc<WaveformOwner>,
    loudness: Arc<LoudnessAnalysisOwner>,
    pub(crate) lyrics: Arc<LyricsService>,
    observe_playback: Arc<dyn Fn(Option<&PlaybackProjection>, bool) + Send + Sync>,
    audio_outputs: fn() -> Vec<playback::AudioOutput>,
    scrobbler: Arc<Scrobbler>,
    source: Mutex<std::sync::Weak<SourceOwner>>,
    connect: Mutex<std::sync::Weak<crate::connect::ConnectOwner>>,
    active: Mutex<Option<Playback>>,
    pending_queue: Mutex<Option<QueueUpdate>>,
    store_sender: async_channel::Sender<PlaybackStoreWork>,
    monotonic_origin: Instant,
    start_backend: Box<dyn Fn() -> Result<Box<dyn PlaybackBackend>, String> + Send + Sync>,
    cast: playback_cast::CastManager,
    output: Mutex<OutputSelection>,
    plex_targets: Mutex<std::collections::HashMap<String, playback_cast::plex::PlexPlayer>>,
    plex: Mutex<Option<Arc<plex::PlexPlayback>>>,
    rufin: Mutex<Option<Arc<connect::RufinPlayback>>>,
}

enum PlaybackStoreWork {
    Continue {
        prepared: Box<PreparedContinuation>,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    Activity(Option<library::TrackKey>, Box<playback::ActivityListen>),
    Flush(std::sync::mpsc::SyncSender<()>),
    Output {
        volume: f64,
        muted: bool,
        audio_output: Option<String>,
    },
    Settings,
}

fn enqueue_queue_save(
    pending: &Mutex<Option<QueueUpdate>>,
    sender: &async_channel::Sender<PlaybackStoreWork>,
    update: QueueUpdate,
) {
    let mut pending = pending.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(current) = pending.as_mut() {
        current.coalesce(update);
    } else {
        *pending = Some(update);
        let _ = sender.try_send(PlaybackStoreWork::Settings);
    }
}

async fn save_pending_queue(
    database: &Database,
    pending: &Mutex<Option<QueueUpdate>>,
    failed: &mut Option<QueueUpdate>,
) -> library::LibraryResult<()> {
    let pending = pending.lock().unwrap_or_else(|p| p.into_inner()).take();
    let update = match (failed.take(), pending) {
        (Some(mut older), Some(newer)) => {
            older.coalesce(newer);
            older
        }
        (Some(update), None) | (None, Some(update)) => update,
        (None, None) => return Ok(()),
    };
    if let Err(error) = database.apply_queue_update(&update).await {
        *failed = Some(update);
        return Err(error);
    }
    Ok(())
}

impl PlaybackOwner {
    pub(crate) fn insert_load(
        &self,
        input: library::QueueReadRequest,
        target: playback::QueueReorderTarget,
    ) {
        self.send(Command::Insert { input, target });
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new<StartBackend>(
        database: Arc<Database>,
        settings: SettingsFile,
        runtime: tokio::runtime::Handle,
        visualizer_events: EventSender<crate::runtime::VisualizerPublication>,
        visualizer_drain: Receiver<VisualizerPublication>,
        artwork: artwork::Artwork,
        waveform: Arc<WaveformOwner>,
        lyrics: Arc<LyricsService>,
        observe_playback: Arc<dyn Fn(Option<&PlaybackProjection>, bool) + Send + Sync>,
        audio_outputs: fn() -> Vec<playback::AudioOutput>,
        scrobbler: Arc<Scrobbler>,
        start_backend: StartBackend,
    ) -> Arc<Self>
    where
        StartBackend: Fn() -> Result<Box<dyn PlaybackBackend>, String> + Send + Sync + 'static,
    {
        let ui = settings.load().ui;
        let (store_sender, store_receiver) = async_channel::unbounded();
        let artwork_settings = settings.clone();
        let cast_artwork = artwork.clone();
        let cast_artwork_path = move |stream: &playback::PreparedStream| {
            cached_cast_artwork(&cast_artwork, &artwork_settings, &stream.row.as_ref()?.item)
        };
        let owner = Arc::new(Self {
            database,
            settings,
            runtime: runtime.clone(),
            updates: playback::PlaybackUpdates::default(),
            visualizer_events,
            visualizer_drain,
            waveform,
            loudness: LoudnessAnalysisOwner::new(runtime),
            lyrics,
            observe_playback,
            audio_outputs,
            scrobbler,
            source: Mutex::new(std::sync::Weak::new()),
            connect: Mutex::new(std::sync::Weak::new()),
            active: Mutex::new(None),
            pending_queue: Mutex::new(None),
            store_sender,
            monotonic_origin: Instant::now(),
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
            plex_targets: Mutex::new(std::collections::HashMap::new()),
            plex: Mutex::new(None),
            rufin: Mutex::new(None),
        });
        let weak = Arc::downgrade(&owner);
        owner.runtime.spawn(async move {
            let mut failed_queue_save = None;
            while let Ok(work) = store_receiver.recv().await {
                let Some(owner) = weak.upgrade() else { break };
                owner.consume_store(work, &mut failed_queue_save).await;
            }
        });
        owner
    }

    pub(crate) fn install_source_owner(&self, source: &Arc<SourceOwner>) {
        *self
            .source
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::downgrade(source);
    }

    pub(crate) fn install_connect(&self, connect: &Arc<crate::connect::ConnectOwner>) {
        *self.connect.lock().unwrap_or_else(|p| p.into_inner()) = Arc::downgrade(connect);
    }

    pub(crate) async fn start(self: &Arc<Self>) -> Result<PlaybackProjection, String> {
        let stored = self.settings.load();
        let sequence = match self.database.restore_queue().await {
            Ok(mut restore) => {
                if restore.entries.is_empty() {
                    restore.repeat_mode = stored.ui.repeat_mode;
                    restore.shuffled = stored.ui.shuffle_enabled;
                }
                Sequence::from_window(restore, 0, 0).map_err(string_error)
            }
            Err(error) => Err(string_error(error)),
        }.unwrap_or_else(|error| {
            warn!(%error, "could not restore optional Queue; starting with an empty playback window");
            Sequence::new()
        });
        let owner = Arc::downgrade(self);
        let clock_owner = Arc::downgrade(self);
        let backend_owner = Arc::clone(self);
        let (playback_output, backend) =
            tokio::task::spawn_blocking(move || backend_owner.take_selected_backend())
                .await
                .map_err(string_error)??;
        let (playback, projection) = Playback::start(
            sequence,
            random_identity(),
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
            owner,
        )
        .map_err(string_error)?;
        *self.active.lock().unwrap_or_else(|p| p.into_inner()) = Some(playback.clone());
        Ok(projection)
    }

    pub(crate) fn remove_waveform_cache(&self, source: &sources::SourceId) -> Result<(), String> {
        self.waveform
            .remove_source_cache(source)
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn forget_source(&self, source: library::SourceKey) -> Result<(), String> {
        let Some(active) = self.active() else {
            return Ok(());
        };
        let queue = active.handoff_snapshot().map_err(string_error)?.0;
        let positions = self
            .database
            .queue_positions_for_source(&queue, source)
            .await
            .map_err(string_error)?;
        if !positions.is_empty() {
            let keys = positions
                .into_iter()
                .map(|position| queue.entries[queue.order[position] as usize].track_key)
                .collect();
            active
                .command(Command::Forget(keys))
                .map_err(string_error)?;
        }
        Ok(())
    }

    fn take_active(&self) -> Option<Playback> {
        let active = self.active.lock().unwrap_or_else(|p| p.into_inner()).take();
        self.loudness.cancel();
        self.publish_current_media(None);
        (self.observe_playback)(None, false);
        self.updates.clear();
        active
    }

    fn publish(&self, projection: PlaybackProjection, current_media_changed: bool) {
        if current_media_changed {
            self.publish_current_media(projection.view.transport.current.clone());
        }
        (self.observe_playback)(
            Some(&projection),
            projection
                .notices
                .iter()
                .any(|notice| matches!(notice, playback::PlaybackNotice::PositionDiscontinuity(_))),
        );
        self.updates.publish(projection);
    }

    fn publish_visualizer(&self, run: RunId, levels: Vec<f64>) {
        let frame = VisualizerPublication { run, levels };
        if let Err(TrySendError::Full(frame)) = self.visualizer_events.try_send(frame) {
            let _ = self.visualizer_drain.try_recv();
            let _ = self.visualizer_events.try_send(frame);
        }
    }

    fn save_queue(&self, state: QueueUpdate) {
        enqueue_queue_save(&self.pending_queue, &self.store_sender, state);
    }

    fn persist_output(&self, volume: f64, muted: bool, audio_output: Option<String>) {
        let _ = self.store_sender.try_send(PlaybackStoreWork::Output {
            volume,
            muted,
            audio_output,
        });
    }

    async fn consume_store(
        &self,
        work: PlaybackStoreWork,
        failed_queue_save: &mut Option<QueueUpdate>,
    ) {
        match work {
            PlaybackStoreWork::Continue { prepared, reply } => {
                let _ = reply.send(self.commit_continuation(*prepared, failed_queue_save).await);
            }
            PlaybackStoreWork::Activity(key, activity) => self.store_activity(key, *activity).await,
            PlaybackStoreWork::Flush(reply) => {
                if let Err(error) =
                    save_pending_queue(&self.database, &self.pending_queue, failed_queue_save).await
                {
                    warn!(%error, "could not persist Queue");
                }
                let _ = reply.send(());
            }
            PlaybackStoreWork::Output {
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
            PlaybackStoreWork::Settings => {
                if let Err(error) =
                    save_pending_queue(&self.database, &self.pending_queue, failed_queue_save).await
                {
                    warn!(%error, "could not persist Queue");
                }
            }
        }
    }

    fn refresh_artwork(&self, playback: WeakPlayback, media_uris: Vec<String>) {
        let database = self.database.clone();
        self.runtime.spawn(async move {
            match database.queue_artwork_for_uris(&media_uris).await {
                Ok(bindings) => playback.post(move |player, _| player.artwork_refreshed(bindings)),
                Err(error) => warn!(%error, "could not refresh playback artwork"),
            }
        });
    }

    fn record_listening(&self, fact: playback::ListeningFact) {
        if let playback::ListeningFact::Started { item, .. } = fact {
            self.scrobbler.now_playing(&item);
        }
    }

    fn record_activity(
        &self,
        key: Option<library::TrackKey>,
        activity: Box<playback::ActivityListen>,
    ) {
        let _ = self
            .store_sender
            .try_send(PlaybackStoreWork::Activity(key, activity));
    }

    fn report_source(&self, report: playback::SourceReportFact) {
        if external_source_reporting_enabled(self.settings.load().ui.private_mode)
            && let Some((source_id, kind, _)) = library::source_entity_parts(&report.media_uri)
            && kind == "track"
        {
            let owner = self.source_owner();
            self.runtime.spawn(async move {
                if let Ok(Some(source)) =
                    tokio::task::spawn_blocking(move || owner?.client(&source_id).ok()).await
                {
                    let _ = source.report_playback(&report).await;
                }
            });
        }
    }

    fn request_auto_dj(&self, playback: WeakPlayback, request: AutoDjRequest) {
        if !self.request_plex_auto_dj(&request)
            && let Some(playback) = playback.upgrade()
        {
            crate::radio::request_auto_dj(
                self.runtime.clone(),
                self.database.clone(),
                self.source_owner()
                    .map(|source| Arc::downgrade(&source))
                    .unwrap_or_default(),
                playback,
                request,
            );
        }
    }

    async fn store_activity(
        &self,
        current_track: Option<library::TrackKey>,
        activity: playback::ActivityListen,
    ) {
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
        match self
            .database
            .record_listen(&listen, current_track, &deliveries)
            .await
        {
            Ok(_) => self.scrobbler.listen_recorded(deliveries.len()),
            Err(error) => warn!(%error, "could not record Rufin Activity"),
        }
    }

    fn resolve_stream(
        &self,
        playback: WeakPlayback,
        run: RunId,
        occurrence: Arc<playback::QueueRow>,
        request: StreamRequest,
    ) -> tokio::task::JoinHandle<()> {
        let database = Arc::clone(&self.database);
        let source_owner = self.source_owner();
        let local = self.settings.local_configuration();
        let connect = self
            .connect
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upgrade();
        self.runtime.spawn(async move {
            let (track, album) = database
                .playback_loudness(&occurrence.item.media_uri, &ReadCancellation::new())
                .await
                .unwrap_or_default();
            let loudness = playback::TrackLoudness {
                track: track.map(Box::new),
                album: album.map(Box::new),
            };
            let result = prepare_stream(
                &database,
                request,
                local.as_ref(),
                connect
                    .as_deref()
                    .map(|connect| (connect, occurrence.as_ref())),
                move |source_id| async move {
                    tokio::task::spawn_blocking(move || {
                        source_owner
                            .ok_or_else(crate::source::source_access_unavailable)?
                            .client(&source_id)
                    })
                    .await
                    .map_err(string_error)?
                },
            )
            .await
            .map(|stream| prepare_media_stream(stream, loudness, occurrence));
            playback.post(move |player, sample| match result {
                Ok(stream) => player.stream_resolved(run, stream),
                Err(error) => player.stream_failed(run, error, sample),
            });
        })
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
        self.send(Command::UpdateSettings(settings));
    }
    pub(crate) fn cast_proxy_setting_changed(&self, enabled: bool) {
        self.cast.set_proxy_media(enabled);
    }
    pub(crate) fn cast_network_setting_changed(&self, network: Option<String>) {
        self.cast.set_network_interface(network);
    }
    pub(crate) fn auto_dj_threshold_changed(&self, enabled: bool, threshold: u8) {
        self.send(Command::SetAutoDj {
            enabled,
            refill_threshold: usize::from(threshold),
        });
    }
    pub(crate) fn stream_inputs_changed(&self) -> Result<(), String> {
        let active = self
            .active()
            .ok_or_else(|| "Playback is unavailable".to_string())?;
        active
            .command(Command::StreamInputsChanged)
            .map_err(string_error)
    }
    pub(crate) fn rekey_songs(&self, pairs: Arc<[(library::TrackKey, library::TrackKey)]>) {
        self.send(Command::RekeySongs(pairs));
    }
    pub(crate) fn catalog_changed(&self) {
        if self.active().is_some() {
            self.send(Command::CatalogChanged);
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
        self.updates
            .current()
            .and_then(|view| view.transport.current.clone())
    }

    fn active(&self) -> Option<Playback> {
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
        self.lyrics.set_current(Some(LyricsContext {
            media,
            input_digest,
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

    pub(crate) fn current_projection(&self) -> Option<PlaybackProjection> {
        self.updates.current().map(|view| PlaybackProjection {
            view: (*view).clone(),
            notices: Vec::new(),
        })
    }
    fn send(&self, command: Command) {
        let command = if command.addresses_occurrence() {
            command
        } else {
            let Some(command) = self.send_rufin(command) else {
                return;
            };
            let Some(command) = self.send_plex(command) else {
                return;
            };
            command
        };
        if let Some(active) = self.active() {
            if let Err(error) = active.command(command) {
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
    fn select_output(
        &self,
        selected: playback::PlaybackOutput,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        if self
            .output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .selected
            == selected
        {
            return Ok(());
        }
        if matches!(&selected, playback::PlaybackOutput::Remote(output) if output.protocol == playback::RemoteOutputProtocol::RufinConnect)
        {
            return self.select_rufin(selected, cancelled);
        }
        if self
            .rufin
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
        {
            self.leave_rufin(cancelled.clone())?;
            if selected.is_local() {
                return Ok(());
            }
        }
        if matches!(&selected, playback::PlaybackOutput::Remote(output) if output.protocol == playback::RemoteOutputProtocol::PlexCompanion)
        {
            return self.select_plex(selected, cancelled);
        }
        if self
            .plex
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
        {
            self.leave_plex(cancelled)?;
            if selected.is_local() {
                return Ok(());
            }
        }
        let backend = self.start_output_backend(&selected)?;
        if let Some(active) = self.active() {
            active
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
    fn clock_sample(&self) -> ClockSample {
        let now = SystemTime::now();
        let unix_seconds = unix_seconds(now);
        ClockSample {
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

impl QueueCommandPort for PlaybackOwner {
    fn page(&self, offset: u64, limit: u32, filter: String) -> Result<playback::QueuePage, String> {
        self.runtime
            .block_on(self.receiver_queue_page(offset, limit, filter))
    }

    fn snapshot(&self) -> Result<library::QueueRestore, String> {
        let receiver = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(receiver) = receiver {
            return self
                .runtime
                .block_on(receiver.snapshot())
                .map(|snapshot| snapshot.map(|snapshot| snapshot.queue).unwrap_or_default());
        }
        let Some(active) = self.active() else {
            return Ok(library::QueueRestore::default());
        };
        active
            .handoff_snapshot()
            .map(|(queue, _, _)| queue)
            .map_err(string_error)
    }

    fn media_uris(&self) -> Result<Vec<String>, String> {
        let queue = self.snapshot()?;
        let keys = queue
            .order
            .iter()
            .map(|index| queue.entries[*index as usize].track_key)
            .collect::<Vec<_>>();
        self.runtime
            .block_on(self.database.track_uris(&keys, &ReadCancellation::new()))
            .map_err(string_error)
    }

    fn play(&self, request: PlayRequest) {
        if self.play_rufin(&request) {
            return;
        }
        if self.play_plex(&request) {
            return;
        }
        let Some(active) = self.active() else {
            return;
        };
        if let Err(error) = active.play(request) {
            warn!(%error, "could not play Queue");
        }
    }
    fn insert(&self, input: library::SongSelection, target: playback::QueueReorderTarget) {
        self.insert_load(library::QueueReadRequest::songs(input), target);
    }
    fn remove(&self, position: usize) {
        self.send(Command::Remove(position));
    }
    fn remove_many(&self, selection: playback::QueueSelection) {
        self.send(Command::RemoveMany(selection));
    }
    fn activate(&self, position: usize) {
        self.send(Command::Activate(position));
    }
    fn activate_occurrence(&self, id: u64) {
        self.send(Command::ActivateOccurrence(id));
    }
    fn move_occurrence_after_current(&self, id: u64) {
        self.send(Command::MoveOccurrenceAfterCurrent(id));
    }
    fn move_after_current(&self, position: usize) {
        self.send(Command::MoveAfterCurrent(position));
    }
    fn reorder(&self, request: QueueReorderRequest) {
        self.send(Command::Reorder {
            selection: request.selection,
            target: request.target,
        });
    }
    fn clear(&self, include_current: bool) {
        self.send(Command::Clear { include_current });
    }
}

impl RadioCommandPort for PlaybackOwner {
    fn candidates(
        &self,
        seed: library::RadioSeed,
        requested: usize,
        cancellation: library::ReadCancellation,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<String>, String>> + Send>>
    {
        let database = self.database.clone();
        let source = self
            .source
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        Box::pin(async move {
            crate::radio::radio_candidates(&database, source, seed, requested, &cancellation).await
        })
    }

    fn play_random(&self, request: RandomPlayRequest) {
        if self.random_rufin(&request) {
            return;
        }
        if self.random_plex(&request) {
            return;
        }
        if let (Some(active), Some(selected)) = (
            self.active(),
            self.source_owner()
                .and_then(|source| source.current_session()),
        ) {
            let _ = crate::radio::play_random(
                self.runtime.clone(),
                selected.downgrade(),
                active,
                request,
            );
        }
    }
    fn play_radio(&self, request: RadioPlayRequest) {
        if self.radio_rufin(&request) {
            return;
        }
        if self.radio_plex(&request) {
            return;
        }
        if let Some(active) = self.active() {
            let _ = crate::radio::play_radio(
                self.runtime.clone(),
                self.database.clone(),
                self.source
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone(),
                active,
                request,
            );
        }
    }
}

impl TransportCommandPort for PlaybackOwner {
    fn play_pause(&self) {
        self.send(Command::PlayPause)
    }
    fn play(&self) {
        self.send(Command::Play)
    }
    fn pause(&self) {
        self.send(Command::Pause)
    }
    fn stop(&self) {
        self.send(Command::Stop)
    }
    fn next(&self) {
        self.send(Command::Next)
    }
    fn previous(&self) {
        self.send(Command::Previous)
    }
    fn seek_seconds(&self, seconds: u32) {
        self.seek_millis(u64::from(seconds) * 1000)
    }
    fn seek_millis(&self, millis: u64) {
        self.send(Command::Seek(millis))
    }
    fn set_volume(&self, volume: f64) {
        self.send(Command::SetVolume(volume))
    }
    fn persist_volume(&self, volume: f64) {
        self.send(Command::PersistVolume(volume))
    }
    fn set_muted(&self, muted: bool) {
        self.send(Command::SetMuted(muted))
    }
    fn set_shuffle(&self, enabled: bool) {
        let _ = self.settings.update(|stored| {
            stored.ui.shuffle_enabled = enabled;
            Ok(())
        });
        self.send(Command::SetShuffle {
            enabled,
            seed: random_u64(),
        })
    }
    fn cycle_repeat(&self) {
        if self
            .rufin
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
            && let Some(projection) = self.current_projection()
        {
            self.set_repeat(next_repeat(projection.view.controls.repeat_mode));
            return;
        }
        match self.settings.update(|stored| {
            stored.ui.repeat_mode = next_repeat(stored.ui.repeat_mode);
            Ok(stored.ui.repeat_mode)
        }) {
            Ok(repeat) => self.send(Command::SetRepeat(repeat)),
            Err(_) => self.set_repeat(RepeatMode::Off),
        }
    }
    fn set_repeat(&self, repeat: RepeatMode) {
        let _ = self.settings.update(|stored| {
            stored.ui.repeat_mode = repeat;
            Ok(())
        });
        self.send(Command::SetRepeat(repeat))
    }
    fn toggle_auto_dj(&self) {
        if self
            .rufin
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
            && let Some(projection) = self.current_projection()
        {
            self.send_cast_auto_dj(!projection.view.controls.auto_dj_enabled);
            return;
        }
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
        self.send(Command::SetAutoDj {
            enabled: value,
            refill_threshold: usize::from(threshold),
        })
    }
    fn set_visualizer_enabled(&self, enabled: bool) {
        self.send(Command::SetVisualizerEnabled(enabled))
    }
    fn available_audio_outputs(&self) -> Vec<playback::AudioOutput> {
        (self.audio_outputs)()
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
        let mut outputs = self.cast.discover().unwrap_or_else(|error| {
            debug!(%error, "generic receiver discovery unavailable");
            Vec::new()
        });
        outputs.extend(self.discover_rufin());
        match self.discover_plex() {
            Ok(plex) => outputs.extend(plex),
            Err(error) if !outputs.is_empty() => {
                debug!(%error, "Plex receiver discovery unavailable")
            }
            Err(error) => return Err(error),
        }
        Ok(outputs)
    }
    fn select_playback_output(
        &self,
        output: playback::PlaybackOutput,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        self.select_output(output, cancelled)
    }
    fn shutdown(&self) {
        if let Some(receiver) = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).take() {
            receiver.cancel();
        }
        if let Some(plex) = self.plex.lock().unwrap_or_else(|p| p.into_inner()).take() {
            plex.cancel();
        }
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
            let _ = active.shutdown();
        }
        let (reply, done) = std::sync::mpsc::sync_channel(0);
        if self
            .store_sender
            .try_send(PlaybackStoreWork::Flush(reply))
            .is_ok()
        {
            let _ = done.recv();
        }
        self.take_active();
    }
}

pub(crate) async fn prepare_stream<F>(
    database: &Database,
    request: StreamRequest,
    local: Option<&sources::SourceConfiguration>,
    file_transfer: Option<(&crate::connect::ConnectOwner, &playback::QueueRow)>,
    source: impl FnOnce(sources::SourceId) -> F + Send,
) -> Result<playback::ResolvedStream, String>
where
    F: std::future::Future<Output = Result<Arc<sources::Source>, String>> + Send,
{
    let mut access = database
        .playback_access(&request.media_uri)
        .await
        .ok()
        .flatten();
    let cue = library::cue_media_parts(&request.media_uri);
    let file_uri = cue
        .as_ref()
        .map_or(request.media_uri.as_str(), |(_, uri, _, _)| uri);
    if access.is_none() && cue.is_some() && library::document_media_id(file_uri).is_some() {
        access = database
            .playback_access(file_uri)
            .await
            .map_err(string_error)?;
    }
    let granted_uri = |uri: &str| {
        let path = library::file_media_path(uri)?;
        let (_, access) = local?.local_file_location(&path)?;
        url::Url::from_file_path(access).ok().map(String::from)
    };
    if let Some((uri, _)) = access.as_mut()
        && let Some(granted) = granted_uri(uri)
    {
        *uri = granted;
    }
    let granted_file = granted_uri(file_uri);
    let file_uri = granted_file.as_deref().unwrap_or(file_uri);
    let mut fetched_uri = None;
    if let Some((connect, occurrence)) = file_transfer
        && connect.is_local_media(&request.media_uri).await?
    {
        let received_document = url::Url::parse(file_uri)
            .is_ok_and(|uri| uri.scheme() == "content")
            && occurrence.received_from_connect;
        if received_document {
            access = None;
        }
        let remote = received_document
            || database
                .file_media_is_remote(&request.media_uri, occurrence.received_from_connect)
                .await
                .map_err(string_error)?;
        let local_access = match access.as_ref() {
            Some((uri, _)) => local_access_available(uri).await.unwrap_or(false),
            None => false,
        };
        // A content URI received from another device is not a grant to this device's provider.
        let available =
            local_access || (!remote && local_access_available(file_uri).await.unwrap_or(false));
        if remote || !available {
            let fetched = connect.fetch_playback_file(occurrence).await?;
            if remote && fetched.is_none() && !local_access {
                return Err(sources::SourceError::NotFound.to_string());
            }
            fetched_uri = fetched
                .map(|location| match location {
                    library::LocalMediaLocation::File(path) => url::Url::from_file_path(path)
                        .map(String::from)
                        .map_err(|_| "Media path must be absolute".to_string()),
                    library::LocalMediaLocation::Document(uri) => Ok(uri),
                })
                .transpose()?;
            if !received_document {
                access = database
                    .playback_access(&request.media_uri)
                    .await
                    .map_err(string_error)?;
            }
        }
    }
    if let Some((uri, library::LocalAccessOrigin::Download)) = access.as_ref()
        && !local_access_available(uri).await?
    {
        access = None;
    }
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
    let access_uri = access.map(|(uri, _)| uri).or(fetched_uri);
    let window = database
        .connect_media_window(&request.media_uri)
        .await
        .map_err(string_error)?;
    if let Some((start, end)) = window.filter(|_| access_uri.is_some() || cue.is_some()) {
        return Ok(
            direct_stream(access_uri.unwrap_or_else(|| file_uri.to_owned()))
                .await?
                .with_content_type(content_type)
                .with_window(start, end),
        );
    }
    if let Some(uri) = access_uri.or_else(|| library::normalize_direct_media_uri(file_uri)) {
        return Ok(direct_stream(uri).await?.with_content_type(content_type));
    }
    let (source_id, kind, _) = library::source_entity_parts(&request.media_uri)
        .ok_or_else(crate::source::source_access_unavailable)?;
    if kind != "track" {
        return Err(crate::source::source_access_unavailable());
    }
    let source = source(source_id).await?;
    source.stream(database, request).await.map_err(string_error)
}

async fn local_access_available(uri: &str) -> Result<bool, String> {
    let location = if url::Url::parse(uri).is_ok_and(|uri| uri.scheme() == "content") {
        library::LocalMediaLocation::Document(uri.to_owned())
    } else if let Some(path) = library::file_media_path(uri) {
        library::LocalMediaLocation::File(path)
    } else {
        return Ok(false);
    };
    sources::local_files_available(vec![vec![location]])
        .await
        .map(|available| available.first() == Some(&true))
        .map_err(string_error)
}

async fn direct_stream(uri: String) -> Result<playback::ResolvedStream, String> {
    if library::document_media_id(&uri).is_some() {
        return Err(sources::SourceError::NotFound.to_string());
    }
    if url::Url::parse(&uri).is_ok_and(|uri| uri.scheme() == "content") {
        sources::resolve_document_stream(&uri)
            .await
            .map_err(string_error)
    } else {
        Ok(playback::ResolvedStream::new(uri))
    }
}

fn prepare_media_stream(
    stream: playback::ResolvedStream,
    loudness: playback::TrackLoudness,
    occurrence: Arc<playback::QueueRow>,
) -> PreparedStream {
    let content_type = occurrence.source_format.as_deref().and_then(audio_mime);
    PreparedStream::new(stream, loudness).with_row(occurrence, content_type)
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
fn empty_clock_sample() -> ClockSample {
    ClockSample {
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
    use super::{external_source_reporting_enabled, prepare_media_stream};

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

    fn test_occurrence(source_format: &str) -> playback::QueueRow {
        playback::QueueRow {
            occurrence_id: None,
            item: test_media(source_format),
            position: 0,
            received_from_connect: false,
            track_key: None,
            provenance: playback::Provenance::Manual,
        }
    }

    #[test]
    fn private_mode_gates_only_external_source_reporting() {
        assert!(external_source_reporting_enabled(false));
        assert!(!external_source_reporting_enabled(true));
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
        std::fs::write(&path, b"downloaded audio").unwrap();
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
            None,
            None,
            |_| async { panic!("a completed download must resolve without contacting its source") },
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
        assert_eq!(prepared.row.as_ref().unwrap().media_uri, canonical);
    }
}
