//! Playback controls, source jobs, and persistent playback facts.
pub(crate) mod actor;
mod commands;
pub mod equalizer;
pub use equalizer::{
    EqCastFormat, EqEdit, EqPresetFormat, EqPresetSummary, EqSubscription, EqView,
    EqualizerPreferences,
};
mod connect;
mod continuation;
mod plex;
mod queue;
pub(crate) mod state;
pub use commands::*;
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

use crate::features::follow;
use crate::runtime::VisualizerPublication;
use actor::{Playback, WeakPlayback};
use async_channel::{Receiver, Sender as EventSender, TrySendError};
use library::QueueUpdate;
use library::{Catalog, Change, ListenWrite, ReadCancellation};
use playback::{
    PlaybackBackend, PlaybackProjection, PreparedStream, RepeatMode, RunId, StreamRequest,
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

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct PlaybackOwner {
    catalog: Arc<Catalog>,
    settings: SettingsFile,
    runtime: tokio::runtime::Handle,
    pub updates: playback::PlaybackUpdates,
    visualizer_events: EventSender<crate::runtime::VisualizerPublication>,
    visualizer_drain: Receiver<VisualizerPublication>,
    waveform: Arc<WaveformOwner>,
    pub(crate) loudness: Arc<LoudnessAnalysisOwner>,
    pub(crate) lyrics: Arc<LyricsService>,
    observe_playback: Arc<dyn Fn(Option<&PlaybackProjection>) + Send + Sync>,
    audio_outputs: fn() -> Vec<playback::AudioOutput>,
    scrobbler: Arc<Scrobbler>,
    source: Mutex<std::sync::Weak<SourceOwner>>,
    /// Takes commands from construction on; they wait until the restored queue is in place.
    active: Mutex<Option<Playback>>,
    pending_queue: Mutex<Option<QueueUpdate>>,
    equalizer: Mutex<equalizer::EqualizerState>,
    store_sender: async_channel::Sender<PlaybackStoreWork>,
    report_sender: async_channel::Sender<(sources::SourceId, playback::SourceReportFact)>,
    monotonic_origin: Instant,
    start_backend: Box<
        dyn Fn(
                tokio::sync::watch::Receiver<playback::ProcessingRequest>,
            ) -> Result<Box<dyn PlaybackBackend>, String>
            + Send
            + Sync,
    >,
    cast: playback_cast::CastManager,
    output: Mutex<playback::PlaybackOutput>,
    plex_targets: Mutex<std::collections::HashMap<String, playback_cast::plex::PlexPlayer>>,
    plex: Mutex<Option<Arc<plex::PlexPlayback>>>,
    rufin: Mutex<Option<Arc<connect::RufinPlayback>>>,
    /// The playables resolved last, current and next among them, for the lyrics of the track
    /// that becomes current.
    playables: Arc<Mutex<std::collections::VecDeque<Arc<library::Playable>>>>,
}

/// How many resolved playables the owner keeps.
const KEPT_PLAYABLES: usize = 4;

fn keep_playable(
    kept: &Mutex<std::collections::VecDeque<Arc<library::Playable>>>,
    playable: &Arc<library::Playable>,
) {
    let mut kept = kept.lock().unwrap_or_else(|p| p.into_inner());
    kept.retain(|kept| kept.track != playable.track);
    if kept.len() == KEPT_PLAYABLES {
        kept.pop_front();
    }
    kept.push_back(Arc::clone(playable));
}

enum PlaybackStoreWork {
    Activity(library::TrackKey, Box<ListenWrite>),
    Flush(std::sync::mpsc::SyncSender<()>),
    Output {
        volume: f64,
        muted: bool,
        audio_output: Option<String>,
    },
    Settings,
    Equalizer {
        snapshot: EqualizerPreferences,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
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
    catalog: &Catalog,
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
    if let Err(error) = catalog.apply_queue_update(&update).await {
        *failed = Some(update);
        return Err(error);
    }
    Ok(())
}

impl PlaybackOwner {
    pub(crate) fn insert_load(&self, input: library::QueueLoad, target: QueueReorderTarget) {
        self.send(Command::Insert { input, target });
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new<StartBackend>(
        catalog: Arc<Catalog>,
        settings: SettingsFile,
        runtime: tokio::runtime::Handle,
        visualizer_events: EventSender<crate::runtime::VisualizerPublication>,
        visualizer_drain: Receiver<VisualizerPublication>,
        artwork: artwork::Artwork,
        waveform: Arc<WaveformOwner>,
        lyrics: Arc<LyricsService>,
        observe_playback: Arc<dyn Fn(Option<&PlaybackProjection>) + Send + Sync>,
        audio_outputs: fn() -> Vec<playback::AudioOutput>,
        scrobbler: Arc<Scrobbler>,
        start_backend: StartBackend,
    ) -> Arc<Self>
    where
        StartBackend: Fn(
                tokio::sync::watch::Receiver<playback::ProcessingRequest>,
            ) -> Result<Box<dyn PlaybackBackend>, String>
            + Send
            + Sync
            + 'static,
    {
        let stored = settings.load();
        let eq = equalizer::EqualizerState::new(
            stored.equalizer,
            stored.ui.playback.audio_output.as_deref(),
        );
        let ui = stored.ui;
        let (store_sender, store_receiver) = async_channel::unbounded();
        let (report_sender, report_receiver) = async_channel::unbounded();
        let artwork_settings = settings.clone();
        let cast_artwork = artwork.clone();
        let cast_artwork_path = move |stream: &playback::PreparedStream| {
            cached_cast_artwork(
                &cast_artwork,
                &artwork_settings,
                stream.row.as_ref()?.track.artwork_binding.as_deref(),
            )
        };
        let owner = Arc::new(Self {
            catalog,
            loudness: LoudnessAnalysisOwner::new(runtime.clone(), settings.clone()),
            settings,
            runtime: runtime.clone(),
            updates: playback::PlaybackUpdates::default(),
            visualizer_events,
            visualizer_drain,
            waveform,
            lyrics,
            observe_playback,
            audio_outputs,
            scrobbler,
            source: Mutex::new(std::sync::Weak::new()),
            active: Mutex::new(None),
            pending_queue: Mutex::new(None),
            equalizer: Mutex::new(eq),
            store_sender,
            report_sender,
            monotonic_origin: Instant::now(),
            start_backend: Box::new(start_backend),
            cast: playback_cast::CastManager::new(
                ui.application.cast_proxy_enabled,
                ui.cast_network_interface,
                cast_artwork_path,
            ),
            output: Mutex::new(playback::PlaybackOutput::Local),
            plex_targets: Mutex::new(std::collections::HashMap::new()),
            plex: Mutex::new(None),
            rufin: Mutex::new(None),
            playables: Arc::default(),
        });
        let weak = Arc::downgrade(&owner);
        // The queue restores after the Catalog loaded, so the first full pass has nothing to
        // read again; later ones follow a lag.
        let mut restored = false;
        follow(
            &owner.runtime,
            &owner.catalog,
            weak.clone(),
            move |owner, changes| {
                if std::mem::replace(&mut restored, true) || changes.is_some() {
                    owner.library_changed(changes);
                }
                std::future::ready(())
            },
        );
        follow(
            &owner.runtime,
            &owner.catalog,
            weak.clone(),
            crate::loudness::follow,
        );
        owner.runtime.spawn(async move {
            while let Ok((source_id, report)) = report_receiver.recv().await {
                let Some(owner) = weak.upgrade() else { break };
                if owner.settings.load().ui.application.private_mode {
                    continue;
                }
                let source = owner.source_owner();
                if let Ok(Some(source)) =
                    tokio::task::spawn_blocking(move || source?.client(&source_id).ok()).await
                    && let Err(error) = source.report_playback(&report).await
                {
                    debug!(%error, "could not report playback to source");
                }
            }
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

    /// Takes commands at once; they wait until the saved queue is restored after the Catalog
    /// loaded, so the first rows come first. The backend starts then, or for the first play.
    pub(crate) fn start(self: &Arc<Self>) {
        let (playback, receiver) = actor::channel();
        *self.active.lock().unwrap_or_else(|p| p.into_inner()) = Some(playback.clone());
        let owner = Arc::downgrade(self);
        let catalog = Arc::clone(&self.catalog);
        self.runtime.spawn(async move {
            catalog.loaded().await;
            let Some(strong) = owner.upgrade() else {
                return;
            };
            let stored = strong.settings.load();
            let sequence = match strong.catalog.restore_queue().await {
                Ok(mut restore) => {
                    if restore.entries.is_empty() {
                        restore.repeat_mode = stored.ui.repeat_mode;
                        restore.shuffled = stored.ui.shuffle_enabled;
                    }
                    Sequence::from_window(restore, 0, 0).map_err(string_error)
                }
                Err(error) => Err(string_error(error)),
            }
            .unwrap_or_else(|error| {
                warn!(%error, "could not restore optional Queue; starting with an empty playback window");
                Sequence::new()
            });
            let clock_owner = owner.clone();
            let output = strong.playback_output();
            drop(strong);
            if let Err(error) = playback.run(
                receiver,
                sequence,
                random_identity(),
                stored.ui.playback,
                stored.ui.auto_dj_enabled,
                usize::from(stored.ui.application.auto_dj_refill_threshold),
                output,
                None,
                Arc::new(move || {
                    clock_owner
                        .upgrade()
                        .map_or_else(empty_clock_sample, |owner| owner.clock_sample())
                }),
                owner,
            ) {
                warn!(%error, "could not start playback");
                return;
            }
            playback
                .downgrade()
                .post(|player, _| player.start_backend());
        });
    }

    pub async fn waveform_cache_usage(&self) -> Result<library::CacheUsage, String> {
        self.catalog
            .waveform_cache_usage()
            .await
            .map_err(string_error)
    }

    pub async fn clear_waveform_cache(&self) -> Result<(), String> {
        self.catalog
            .clear_waveform_cache()
            .await
            .map_err(string_error)
    }

    fn take_active(&self) -> Option<Playback> {
        let active = self.active.lock().unwrap_or_else(|p| p.into_inner()).take();
        self.loudness.cancel();
        self.publish_current_media(None);
        (self.observe_playback)(None);
        self.updates.clear();
        active
    }

    fn publish(&self, projection: PlaybackProjection, current_media_changed: bool) {
        if current_media_changed {
            self.publish_current_media(projection.view.transport.current.clone().map(Arc::new));
        }
        (self.observe_playback)(Some(&projection));
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
            PlaybackStoreWork::Equalizer { snapshot, reply } => {
                let result = self.settings.update(|stored| {
                    stored.equalizer = snapshot;
                    Ok(())
                });
                self.equalizer_write_finished(&result);
                let _ = reply.send(result);
            }
            PlaybackStoreWork::Activity(key, activity) => self.store_activity(key, *activity).await,
            PlaybackStoreWork::Flush(reply) => {
                if let Err(error) =
                    save_pending_queue(&self.catalog, &self.pending_queue, failed_queue_save).await
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
                    save_pending_queue(&self.catalog, &self.pending_queue, failed_queue_save).await
                {
                    warn!(%error, "could not persist Queue");
                }
            }
        }
    }

    fn record_activity(&self, key: library::TrackKey, activity: Box<ListenWrite>) {
        let _ = self
            .store_sender
            .try_send(PlaybackStoreWork::Activity(key, activity));
    }

    fn report_source(&self, report: playback::SourceReportFact) {
        if external_source_reporting_enabled(self.settings.load().ui.application.private_mode)
            && let Some((source_id, kind, _)) = library::source_entity_parts(&report.media_uri)
            && kind == "track"
        {
            let _ = self.report_sender.try_send((source_id, report));
        }
    }

    fn request_auto_dj(&self, playback: WeakPlayback, request: AutoDjRequest) {
        if self.request_plex_auto_dj(&request) {
            return;
        }
        let catalog = self.catalog.clone();
        let source = self.source_owner();
        let selected = source.as_ref().and_then(|owner| owner.selected_library());
        let source = source.as_ref().map(Arc::downgrade).unwrap_or_default();
        self.runtime.spawn(async move {
            let candidates = crate::radio::radio_candidates(
                &catalog,
                source,
                selected,
                library::RadioSeed::Track(request.seed_media_uri.clone()),
                request.requested_count,
                &library::ReadCancellation::new(),
            )
            .await;
            playback.post(move |player, sample| match candidates {
                Ok(candidates) => {
                    if let Err(error) = player.complete_auto_dj_candidates(
                        request.seed_run,
                        &request.seed_media_uri,
                        candidates,
                        request.requested_count,
                        sample,
                    ) {
                        player.report_error(error.to_string());
                    }
                }
                Err(error) => {
                    player.auto_dj_unavailable(
                        request.seed_run,
                        &request.seed_media_uri,
                        Some(error),
                    );
                }
            });
        });
    }

    async fn store_activity(&self, current_track: library::TrackKey, listen: ListenWrite) {
        let deliveries = self.scrobbler.listen_delivery_targets(&listen);
        if let Err(error) = self
            .catalog
            .record_listen(&listen, Some(current_track), &deliveries)
            .await
        {
            warn!(%error, "could not record Rufin Activity");
        }
    }

    fn resolve_stream(
        &self,
        playback: WeakPlayback,
        run: RunId,
        occurrence: Arc<playback::QueueRow>,
        request: StreamRequest,
    ) -> tokio::task::JoinHandle<()> {
        let catalog = Arc::clone(&self.catalog);
        let playables = Arc::clone(&self.playables);
        let lyrics = Arc::clone(&self.lyrics);
        let source_owner = self.source_owner();
        let local = self.settings.local_configuration();
        self.runtime.spawn(async move {
            let playable = match catalog.playable(occurrence.track.track_key).await {
                Ok(Some(playable)) => Arc::new(playable),
                Ok(None) => {
                    let kind = playback::FailureKind::UnavailableMedia;
                    let error = playback::BackendFailure::with_kind(kind, kind.summary());
                    playback.post(move |player, sample| player.stream_failed(run, error, sample));
                    return;
                }
                Err(error) => {
                    let error = playback::BackendFailure::new(error.to_string());
                    playback.post(move |player, sample| player.stream_failed(run, error, sample));
                    return;
                }
            };
            keep_playable(&playables, &playable);
            lyrics.playable_resolved(Arc::clone(&playable));
            let downloads = source_owner.as_ref().map(|owner| owner.downloads().clone());
            let result = prepare_stream(
                &playable,
                request,
                local.as_ref(),
                downloads.as_ref(),
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
            .map(|stream| prepare_media_stream(stream, track_loudness(&playable), occurrence));
            playback.post(move |player, sample| match result {
                Ok(stream) => player.stream_resolved(run, stream),
                Err(error) => player.stream_failed(run, error, sample),
            });
        })
    }

    fn kept_playable(&self, track: library::TrackKey) -> Option<Arc<library::Playable>> {
        self.playables
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|kept| kept.track == track)
            .cloned()
    }

    pub(crate) fn waveform_setting_changed(&self, enabled: bool) {
        self.waveform.settings_changed(
            enabled,
            self.current_media().map(|media| self.waveform_media(media)),
        );
    }
    pub(crate) fn playback_settings_changed(
        &self,
        previous: &playback::PlaybackSettings,
        settings: playback::PlaybackSettings,
    ) {
        self.equalizer_output_changed(settings.audio_output.as_deref());
        if (
            previous.loudness_normalization,
            previous.loudness_normalization_scope,
            previous.write_ebu_r128_tags,
        ) != (
            settings.loudness_normalization,
            settings.loudness_normalization_scope,
            settings.write_ebu_r128_tags,
        ) {
            self.loudness.cancel();
            if self.ebu_r128() {
                self.loudness.start(self.source_owner());
            }
        }
        self.send(Command::UpdateSettings(settings));
    }
    pub(crate) fn cancel_loudness_analysis(&self) {
        self.loudness.cancel();
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
    /// Committed Changes the queue hears, or None after a lag.
    pub(crate) fn library_changed(&self, changes: Option<Vec<Arc<Change>>>) {
        if let Some(active) = self.active() {
            active.downgrade().post(move |player, sample| {
                player.library_changed(changes.as_deref(), sample);
            });
        }
    }

    /// A source was selected: the queue's rows read again, and loudness analysis turns to it.
    pub(crate) fn source_selected(&self) {
        self.library_changed(None);
        if self.ebu_r128() {
            self.loudness.library_changed(self.source_owner());
        }
    }

    pub(crate) fn ebu_r128(&self) -> bool {
        self.settings.load().ui.playback.loudness_normalization
            == playback::LoudnessNormalization::EbuR128
    }
    pub(crate) fn current_media(&self) -> Option<Arc<playback::CurrentMedia>> {
        self.updates
            .current()
            .and_then(|view| view.transport.current.clone())
            .map(Arc::new)
    }

    fn active(&self) -> Option<Playback> {
        self.active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub(crate) fn source_owner(&self) -> Option<Arc<SourceOwner>> {
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
        let playable = self.kept_playable(media.row.track.track_key);
        self.lyrics
            .set_current(Some(LyricsContext { media, playable }));
    }

    fn waveform_media(&self, media: Arc<playback::CurrentMedia>) -> WaveformMedia {
        WaveformMedia {
            request: StreamRequest::new(
                (&media.row.track).media_uri.clone(),
                self.settings.playback_stream_quality(),
            ),
            media,
            source: self
                .source_owner()
                .map(|source| Arc::downgrade(&source))
                .unwrap_or_default(),
            catalog: Arc::clone(&self.catalog),
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
            active.downgrade().post(move |player, sample| {
                if let Err(error) = player.handle_command(command, sample) {
                    player.report_error(error.to_string());
                }
            });
        }
    }

    fn start_output_backend(
        &self,
        output: &playback::PlaybackOutput,
    ) -> Result<Box<dyn PlaybackBackend>, String> {
        match output {
            playback::PlaybackOutput::Local => {
                (self.start_backend)(self.processing_receiver(output))
            }
            playback::PlaybackOutput::Remote(output) => self
                .cast
                .connect(
                    output,
                    self.processing_receiver(&playback::PlaybackOutput::Remote(output.clone())),
                )
                .map(|backend| Box::new(backend) as Box<dyn PlaybackBackend>),
        }
    }
    fn select_output(
        &self,
        selected: playback::PlaybackOutput,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        if *self.output.lock().unwrap_or_else(|p| p.into_inner()) == selected {
            return Ok(());
        }
        if matches!(&selected, playback::PlaybackOutput::Remote(output) if output.protocol == playback::RemoteOutputProtocol::RufinConnect)
        {
            self.select_rufin(selected, cancelled)?;
            self.equalizer_output_changed(self.settings.load().ui.playback.audio_output.as_deref());
            return Ok(());
        }
        if self
            .rufin
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
        {
            self.leave_rufin(cancelled.clone())?;
            if selected.is_local() {
                self.equalizer_output_changed(
                    self.settings.load().ui.playback.audio_output.as_deref(),
                );
                return Ok(());
            }
        }
        if matches!(&selected, playback::PlaybackOutput::Remote(output) if output.protocol == playback::RemoteOutputProtocol::PlexCompanion)
        {
            self.select_plex(selected, cancelled)?;
            self.equalizer_output_changed(self.settings.load().ui.playback.audio_output.as_deref());
            return Ok(());
        }
        if self
            .plex
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
        {
            self.leave_plex(cancelled)?;
            if selected.is_local() {
                self.equalizer_output_changed(
                    self.settings.load().ui.playback.audio_output.as_deref(),
                );
                return Ok(());
            }
        }
        let active = self
            .active()
            .ok_or_else(|| "Playback is unavailable".to_string())?;
        let backend = self.start_output_backend(&selected)?;
        active
            .replace_backend(selected.clone(), backend)
            .map_err(string_error)?;
        *self.output.lock().unwrap_or_else(|p| p.into_inner()) = selected;
        self.equalizer_output_changed(self.settings.load().ui.playback.audio_output.as_deref());
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

impl PlaybackOwner {
    pub fn snapshot(&self) -> Result<library::QueueRestore, String> {
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

    pub fn play_selection(&self, request: PlayRequest) {
        if self.play_rufin(&request) {
            return;
        }
        if self.play_plex(&request) {
            return;
        }
        let Some(active) = self.active() else {
            return;
        };
        active.downgrade().post(move |player, sample| {
            if let Err(error) = player.play(request, sample) {
                player.report_error(error.to_string());
            }
        });
    }
    pub fn insert(&self, input: library::MemberSelection, target: QueueReorderTarget) {
        self.insert_load(library::QueueLoad::tracks(input), target);
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl PlaybackOwner {
    pub fn remove(&self, position: usize) {
        self.send(Command::Remove(position));
    }
    pub fn remove_many(&self, selection: QueueSelection) {
        self.send(Command::RemoveMany(selection));
    }
    pub fn activate(&self, position: usize) {
        self.send(Command::Activate(position));
    }
    pub fn activate_occurrence(&self, id: u64) {
        self.send(Command::ActivateOccurrence(id));
    }
    pub fn move_occurrence_after_current(&self, id: u64) {
        self.send(Command::MoveOccurrenceAfterCurrent(id));
    }
    pub fn move_after_current(&self, position: usize) {
        self.send(Command::MoveAfterCurrent(position));
    }
    pub fn reorder(&self, selection: QueueSelection, target: QueueReorderTarget) {
        self.send(Command::Reorder { selection, target });
    }
    pub fn clear(&self, include_current: bool) {
        self.send(Command::Clear { include_current });
    }
}

impl PlaybackOwner {
    pub fn candidates(
        &self,
        seed: library::RadioSeed,
        requested: usize,
        cancellation: library::ReadCancellation,
    ) -> impl std::future::Future<Output = Result<Vec<library::TrackKey>, String>> + Send + 'static + use<>
    {
        let catalog = self.catalog.clone();
        let source = self
            .source
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let selected = source.upgrade().and_then(|owner| owner.selected_library());
        async move {
            crate::radio::radio_candidates(
                &catalog,
                source,
                selected,
                seed,
                requested,
                &cancellation,
            )
            .await
        }
    }

    pub fn play_random(&self, request: RandomPlayRequest) {
        if self.random_rufin(&request) {
            return;
        }
        if self.random_plex(&request) {
            return;
        }
        self.send(Command::Random(request));
    }
    pub fn play_radio(&self, request: RadioPlayRequest) {
        if self.radio_rufin(&request) {
            return;
        }
        if self.radio_plex(&request) {
            return;
        }
        self.send(Command::Radio(request));
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl PlaybackOwner {
    pub fn play_pause(&self) {
        self.send(Command::PlayPause)
    }
    pub fn play(&self) {
        self.send(Command::Play)
    }
    pub fn pause(&self) {
        self.send(Command::Pause)
    }
    pub fn stop(&self) {
        self.send(Command::Stop)
    }
    pub fn next(&self) {
        self.send(Command::Next)
    }
    pub fn previous(&self) {
        self.send(Command::Previous)
    }
    pub fn seek_seconds(&self, seconds: u32) {
        self.seek_millis(u64::from(seconds) * 1000)
    }
    pub fn seek_millis(&self, millis: u64) {
        self.send(Command::Seek(millis))
    }
    pub fn set_volume(&self, volume: f64) {
        self.send(Command::SetVolume(volume))
    }
    pub fn persist_volume(&self, volume: f64) {
        self.send(Command::PersistVolume(volume))
    }
    pub fn set_muted(&self, muted: bool) {
        self.send(Command::SetMuted(muted))
    }
    pub fn set_shuffle(&self, enabled: bool) {
        if let Err(error) = self.settings.edit(|stored| {
            stored.ui.shuffle_enabled = enabled;
            Ok(())
        }) {
            warn!(%error, "could not save shuffle setting");
        }
        self.send(Command::SetShuffle {
            enabled,
            seed: random_u64(),
        })
    }
    pub fn cycle_repeat(&self) {
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
        match self.settings.edit(|stored| {
            stored.ui.repeat_mode = next_repeat(stored.ui.repeat_mode);
            Ok(stored.ui.repeat_mode)
        }) {
            Ok(repeat) => self.send(Command::SetRepeat(repeat)),
            Err(error) => {
                warn!(%error, "could not save repeat setting");
                self.set_repeat(RepeatMode::Off);
            }
        }
    }
    pub fn set_repeat(&self, repeat: RepeatMode) {
        if let Err(error) = self.settings.edit(|stored| {
            stored.ui.repeat_mode = repeat;
            Ok(())
        }) {
            warn!(%error, "could not save repeat setting");
        }
        self.send(Command::SetRepeat(repeat))
    }
    pub fn toggle_auto_dj(&self) {
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
            .edit(|stored| {
                stored.ui.auto_dj_enabled = !stored.ui.auto_dj_enabled;
                Ok((
                    stored.ui.auto_dj_enabled,
                    stored.ui.application.auto_dj_refill_threshold,
                ))
            })
            .unwrap_or_else(|error| {
                warn!(%error, "could not save Auto DJ setting");
                (false, 1)
            });
        self.send(Command::SetAutoDj {
            enabled: value,
            refill_threshold: usize::from(threshold),
        })
    }
    pub fn set_visualizer_enabled(&self, enabled: bool) {
        if let Some(active) = self.active() {
            active.downgrade().post(move |player, sample| {
                let _ = player.handle_command(Command::SetVisualizerEnabled(enabled), sample);
            });
        }
    }
}

impl PlaybackOwner {
    pub fn available_audio_outputs(&self) -> Vec<playback::AudioOutput> {
        (self.audio_outputs)()
    }
    pub fn available_cast_networks(&self) -> Vec<playback::CastNetwork> {
        self.cast.available_networks().unwrap_or_default()
    }
    pub fn playback_output(&self) -> playback::PlaybackOutput {
        self.output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    pub fn discover_remote_outputs(&self) -> Result<Vec<playback::RemoteOutput>, String> {
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
    pub fn select_playback_output(
        &self,
        output: playback::PlaybackOutput,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        self.select_output(output, cancelled)
    }
    pub fn shutdown(&self) {
        if let Some(receiver) = self.rufin.lock().unwrap_or_else(|p| p.into_inner()).take() {
            receiver.cancel();
        }
        if let Some(plex) = self.plex.lock().unwrap_or_else(|p| p.into_inner()).take() {
            plex.cancel();
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

/// Opens a playable from the first usable of its download, its mapped file and its own local
/// file, then its source's remote file or server stream. A local copy is usable when it is a
/// regular file with the size it should have; one that cannot be looked at is skipped. A
/// managed download gone from its folder or the wrong size is forgotten, so its rules download
/// it again. A local track with no usable copy is unavailable.
pub(crate) async fn prepare_stream<F>(
    playable: &library::Playable,
    request: StreamRequest,
    local: Option<&sources::SourceConfiguration>,
    downloads: Option<&downloads::Downloads>,
    source: impl FnOnce(sources::SourceId) -> F + Send,
) -> Result<audio_processing::ResolvedStream, playback::BackendFailure>
where
    F: std::future::Future<Output = Result<Arc<sources::Source>, String>> + Send,
{
    let failure = playback::BackendFailure::new;
    let unavailable = || {
        let kind = playback::FailureKind::UnavailableMedia;
        playback::BackendFailure::with_kind(kind, kind.summary())
    };
    let granted_uri = |uri: &str| {
        let path = library::file_media_path(uri)?;
        let (_, access) = local?.local_file_location(&path)?;
        url::Url::from_file_path(access).ok().map(String::from)
    };
    let granted = |uri: String| granted_uri(&uri).unwrap_or(uri);
    let cue = library::cue_media_parts(&request.media_uri);
    let file_uri = cue
        .as_ref()
        .map_or(request.media_uri.as_str(), |(_, uri, _, _)| uri);
    let granted_file = granted_uri(file_uri);
    let file_uri = granted_file.as_deref().unwrap_or(file_uri);
    let local_media =
        url::Url::parse(file_uri).is_ok_and(|uri| matches!(uri.scheme(), "file" | "content"));
    let download = playable
        .download
        .clone()
        .map(|download| library::PlayableDownload {
            uri: granted(download.uri),
            ..download
        });
    let own = playable
        .local_uri()
        .map(granted)
        .or_else(|| local_media.then(|| file_uri.to_owned()));
    let candidates = Candidates {
        download: download.clone(),
        mapped: playable.mapped.clone(),
        own,
        failed: request.failed_local.clone(),
    };
    let (chosen, broken) = tokio::task::spawn_blocking(move || candidates.choose())
        .await
        .map_err(|error| failure(error.to_string()))?;
    if let (Some(broken), Some(downloads)) = (broken, downloads) {
        warn!(media_uri = %playable.media_uri, "forgetting a download that is gone or the wrong size");
        downloads.forget(playable.media_uri.clone(), broken);
    }
    let from_download = chosen
        .as_ref()
        .zip(download.as_ref())
        .is_some_and(|(chosen, download)| *chosen == download.uri);
    // Downloads names its owned file using the actual transcoded extension.
    // Mapped and original files still use their parsed source format.
    let content_type = match &chosen {
        Some(uri) if from_download => {
            let path = if uri.starts_with("content:") {
                let uri = uri.clone();
                Some(
                    tokio::task::spawn_blocking(move || sources::stat_document(&uri))
                        .await
                        .map_err(|error| failure(error.to_string()))?
                        .map_err(|error| failure(error.to_string()))?
                        .name
                        .into(),
                )
            } else {
                library::file_media_path(uri)
            };
            path.and_then(|path: std::path::PathBuf| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .and_then(audio_mime)
            })
        }
        _ => None,
    };
    // A local track has nothing else to play from.
    if chosen.is_none() && (local_media || playable.source_kind.as_deref() == Some("local")) {
        return Err(unavailable());
    }
    if let Some((start, end)) = playable
        .window
        .filter(|_| chosen.is_some() || cue.is_some())
    {
        return Ok(direct_stream(chosen.unwrap_or_else(|| file_uri.to_owned()))
            .await
            .map_err(failure)?
            .with_content_type(content_type)
            .with_window(start, end));
    }
    if let Some(uri) = chosen.or_else(|| library::normalize_direct_media_uri(file_uri)) {
        return Ok(direct_stream(uri)
            .await
            .map_err(failure)?
            .with_content_type(content_type));
    }
    let source_id = playable
        .source_id
        .clone()
        .ok_or_else(|| failure(crate::source::source_access_unavailable()))?;
    let source = source(source_id).await.map_err(failure)?;
    source
        .stream(playable, request)
        .await
        .map_err(|error| failure(error.to_string()))
}

/// A playable's local copies in the order playback tries them.
struct Candidates {
    download: Option<library::PlayableDownload>,
    mapped: Option<library::MappedFile>,
    own: Option<String>,
    /// The copy that failed to open or decode: it and the ones before it are passed over.
    failed: Option<String>,
}

impl Candidates {
    /// The first usable copy, and the path of a managed download that is gone from its folder
    /// or the wrong size. This blocks on the file system or the document provider.
    fn choose(self) -> (Option<String>, Option<String>) {
        let mut passed = self.failed.is_some();
        let mut broken = None;
        let mut pass = |uri: &str| {
            let skip = passed;
            if self.failed.as_deref() == Some(uri) {
                passed = false;
            }
            skip
        };
        if let Some(download) = &self.download
            && !pass(&download.uri)
        {
            match look(&download.uri, download.size) {
                Found::Usable => return (Some(download.uri.clone()), None),
                // A folder that is gone (an unmounted drive) keeps its downloads.
                Found::Missing if download.managed && folder_present(download) => {
                    broken = Some(download.uri.clone());
                }
                Found::Missing | Found::Unusable => {}
            }
        }
        if let Some(uri) = self
            .mapped
            .as_ref()
            .and_then(library::MappedFile::locate_playable)
            .map(|location| match location {
                library::LocalMediaLocation::File(path) => url::Url::from_file_path(path)
                    .map(String::from)
                    .unwrap_or_default(),
                library::LocalMediaLocation::Document(uri) => uri,
            })
            && !pass(&uri)
        {
            return (Some(uri), broken);
        }
        if let Some(uri) = &self.own
            && !pass(uri)
            && look(uri, None) == Found::Usable
        {
            return (Some(uri.clone()), broken);
        }
        (None, broken)
    }
}

#[derive(Debug, PartialEq)]
enum Found {
    Usable,
    /// Not there, or not the size it should be.
    Missing,
    /// Not a regular file, or it could not be looked at.
    Unusable,
}

/// Whether the folder a download was saved under is there.
fn folder_present(download: &library::PlayableDownload) -> bool {
    match download.root.as_deref() {
        Some(root) if root.starts_with("content:") => {
            sources::stat_document(root).is_ok_and(|entry| entry.directory)
        }
        Some(root) => std::path::Path::new(root).is_dir(),
        None => library::file_media_path(&download.uri)
            .and_then(|path| path.parent().map(std::path::Path::is_dir))
            .unwrap_or(false),
    }
}

fn look(uri: &str, size: Option<u64>) -> Found {
    let (file, actual) = if url::Url::parse(uri).is_ok_and(|uri| uri.scheme() == "content") {
        match sources::stat_document(uri) {
            Ok(entry) => (!entry.directory, entry.size),
            Err(sources::SourceError::NotFound) => return Found::Missing,
            Err(_) => return Found::Unusable,
        }
    } else {
        let Some(path) = library::file_media_path(uri) else {
            return Found::Unusable;
        };
        match std::fs::metadata(path) {
            Ok(metadata) => (metadata.is_file(), Some(metadata.len())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Found::Missing,
            Err(_) => return Found::Unusable,
        }
    };
    if !file {
        Found::Unusable
    } else if size.is_some_and(|size| actual.is_some_and(|actual| actual != size)) {
        Found::Missing
    } else {
        Found::Usable
    }
}

async fn direct_stream(uri: String) -> Result<audio_processing::ResolvedStream, String> {
    if library::document_media_id(&uri).is_some() {
        return Err(sources::SourceError::NotFound.to_string());
    }
    if url::Url::parse(&uri).is_ok_and(|uri| uri.scheme() == "content") {
        sources::resolve_document_stream(&uri)
            .await
            .map_err(string_error)
    } else {
        Ok(audio_processing::ResolvedStream::new(uri))
    }
}

/// The gains a playable carries, as the backend takes them.
pub(crate) fn track_loudness(playable: &library::Playable) -> playback::TrackLoudness {
    playback::TrackLoudness {
        track: playable.gains.track_loudness().map(Box::new),
        album: playable.gains.album_loudness().map(Box::new),
    }
}

fn prepare_media_stream(
    stream: audio_processing::ResolvedStream,
    loudness: playback::TrackLoudness,
    occurrence: Arc<playback::QueueRow>,
) -> PreparedStream {
    let content_type = occurrence
        .track
        .metadata
        .source_format
        .as_deref()
        .and_then(audio_mime);
    PreparedStream::new(stream, Some(loudness)).with_row(occurrence, content_type)
}

fn cached_cast_artwork(
    artwork: &artwork::Artwork,
    settings: &SettingsFile,
    binding: Option<&[u8]>,
) -> Option<std::path::PathBuf> {
    let stored = settings.load();
    let binding = artwork::ArtworkBinding::opaque(binding?);
    let external = artwork::ExternalPolicy::new(
        stored.ui.application.external_metadata_enabled,
        stored.ui.allows_external_metadata_lookup(),
        stored.ui.lastfm_api_key,
    );
    artwork.cache_only_file(&artwork::ArtworkRequest::new(binding, 512).with_external(external))
}

pub(crate) fn audio_mime(source_format: &str) -> Option<String> {
    let mime = match source_format.trim().to_ascii_lowercase().as_str() {
        "mp3" | "mp2" | "mpeg" => "audio/mpeg",
        "flac" => "audio/flac",
        "m4a" | "mp4" => "audio/mp4",
        "aac" => "audio/aac",
        "opus" => "audio/ogg; codecs=opus",
        "vorbis" => "audio/ogg; codecs=vorbis",
        "ogg" | "oga" => "audio/ogg",
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

    fn test_media(source_format: &str) -> playback::TrackReference {
        playback::TrackReference {
            media_uri: library::source_entity_uri(
                &library::SourceId::new("source"),
                "track",
                "track",
            ),
            artwork_binding: None,
            metadata: library::TrackMetadata {
                audio_properties: Default::default(),
                title: "Track".to_string(),
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                album_display_artist: None,
                duration_millis: 180_000,
                disc_number: 0,
                track_number: 0,
                year: None,
                release_date: None,
                source_format: Some(source_format.to_string()),
                musicbrainz_recording_id: None,
                musicbrainz_release_track_id: None,
                musicbrainz_album_id: None,
                musicbrainz_release_group_id: None,
                primary_artist_musicbrainz_id: None,
                ..Default::default()
            },
        }
    }

    fn test_occurrence(source_format: &str) -> playback::QueueRow {
        playback::QueueRow {
            occurrence_id: None,
            position: 0,
            provenance: playback::Provenance::Manual,
            track: {
                let reference = test_media(source_format);
                library::TrackView {
                    track_key: library::TrackKey::from_raw(1),
                    source_key: None,
                    source_id: None,
                    object_id: None,
                    media_uri: reference.media_uri,
                    metadata: reference.metadata,
                    artwork_binding: reference.artwork_binding,
                    state: Default::default(),
                    access: Default::default(),
                    links: Default::default(),
                }
            },
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
            audio_processing::ResolvedStream::new("https://provider.example/stream?id=track"),
            playback::TrackLoudness::default(),
            test_occurrence("flac").into(),
        );

        assert_eq!(prepared.content_type.as_deref(), Some("audio/flac"));
    }

    #[test]
    fn provider_transcode_is_published_as_mpeg() {
        let prepared = prepare_media_stream(
            audio_processing::ResolvedStream::new("https://provider.example/stream?id=track")
                .with_content_type(Some("audio/mpeg".to_string())),
            playback::TrackLoudness::default(),
            test_occurrence("flac").into(),
        );

        assert_eq!(prepared.content_type.as_deref(), Some("audio/mpeg"));
    }

    #[tokio::test]
    async fn downloaded_transcode_keeps_its_actual_format_and_canonical_occurrence() {
        let folder = tempfile::tempdir().unwrap();
        let catalog = library::Catalog::open(folder.path().join("library.sqlite3"))
            .await
            .unwrap();
        let occurrence = test_occurrence("flac");
        catalog
            .queue_entries(
                library::QueueLoad::items(vec![(
                    occurrence.track.clone().into(),
                    library::QueueProvenance::Manual,
                )]),
                None,
            )
            .await
            .unwrap();
        let path = folder.path().join("track.opus");
        std::fs::write(&path, b"downloaded audio").unwrap();
        let uri = url::Url::from_file_path(&path).unwrap().to_string();
        catalog
            .save_download(&library::DownloadFile {
                media_uri: occurrence.track.media_uri.clone(),
                encoding: "mp3".into(),
                path: uri.clone(),
                storage_root: folder.path().to_string_lossy().into_owned(),
                relative_path: "track.opus".into(),
                managed: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let key = catalog
            .track_keys(
                std::slice::from_ref(&occurrence.track.media_uri),
                &library::ReadCancellation::new(),
            )
            .await
            .unwrap()[0];
        let playable = catalog.playable(key).await.unwrap().unwrap();
        let stream = super::prepare_stream(
            &playable,
            playback::StreamRequest::new(
                occurrence.track.media_uri.clone(),
                playback::StreamQuality::Original,
            ),
            None,
            None,
            |_| async { panic!("a completed download must resolve without contacting its source") },
        )
        .await
        .unwrap();
        let canonical = occurrence.track.media_uri.clone();
        let prepared = prepare_media_stream(
            stream,
            playback::TrackLoudness::default(),
            occurrence.into(),
        );
        assert_eq!(prepared.uri(), uri);
        assert_eq!(
            prepared.content_type.as_deref(),
            Some("audio/ogg; codecs=opus")
        );
        assert_eq!(prepared.row.as_ref().unwrap().track.media_uri, canonical);
    }

    #[test]
    fn a_download_is_forgotten_only_when_its_folder_is_there() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("downloads");
        std::fs::create_dir(&root).unwrap();
        let uri = url::Url::from_file_path(root.join("track.flac"))
            .unwrap()
            .to_string();
        let candidates = || super::Candidates {
            download: Some(library::PlayableDownload {
                uri: uri.clone(),
                size: Some(16),
                managed: true,
                root: Some(root.to_string_lossy().into_owned()),
            }),
            mapped: None,
            own: None,
            failed: None,
        };
        assert_eq!(candidates().choose(), (None, Some(uri.clone())));
        // An unmounted drive: the folder is gone, and so is no download.
        std::fs::rename(&root, folder.path().join("unmounted")).unwrap();
        assert_eq!(candidates().choose(), (None, None));
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Error))]
#[cfg_attr(feature = "bindings", uniffi(flat_error))]
#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    #[error("{0}")]
    Message(String),
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PreparedQueuePage {
    pub revision: u64,
    pub rows: Vec<Arc<library::QueueRow>>,
    pub total: u64,
    pub window_offset: u64,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl PlaybackOwner {
    pub async fn selection(
        self: Arc<Self>,
    ) -> Result<Arc<library::MemberSelection>, PlaybackError> {
        let runtime = self.runtime.clone();
        runtime
            .spawn_blocking(move || {
                let queue = self.snapshot().map_err(PlaybackError::Message)?;
                Ok(Arc::new(library::MemberSelection::Keys {
                    source: None,
                    keys: queue
                        .order
                        .iter()
                        .map(|index| queue.entries[*index as usize].track_key)
                        .collect::<Vec<_>>()
                        .into(),
                }))
            })
            .await
            .map_err(|error| PlaybackError::Message(error.to_string()))?
    }

    pub async fn queue_page(
        self: Arc<Self>,
        offset: u64,
        limit: u32,
        filter: String,
    ) -> Result<PreparedQueuePage, PlaybackError> {
        let runtime = self.runtime.clone();
        tokio_util::task::AbortOnDropHandle::new(runtime.spawn(async move {
            let revision = self.updates.current().map_or(0, |view| view.queue.revision);
            let page = self
                .receiver_queue_page(offset, limit, filter)
                .await
                .map_err(PlaybackError::Message)?;
            Ok(PreparedQueuePage {
                revision,
                rows: page.rows,
                total: page.total,
                window_offset: page.window_offset,
            })
        }))
        .await
        .map_err(|error| PlaybackError::Message(error.to_string()))?
    }

    pub async fn remote_outputs(
        self: Arc<Self>,
    ) -> Result<Vec<playback::RemoteOutput>, PlaybackError> {
        let runtime = self.runtime.clone();
        runtime
            .spawn_blocking(move || {
                self.discover_remote_outputs()
                    .map_err(PlaybackError::Message)
            })
            .await
            .map_err(|error| PlaybackError::Message(error.to_string()))?
    }

    pub async fn choose_output(
        self: Arc<Self>,
        output: playback::PlaybackOutput,
    ) -> Result<(), PlaybackError> {
        let runtime = self.runtime.clone();
        runtime
            .spawn_blocking(move || {
                self.select_playback_output(
                    output,
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                )
                .map_err(PlaybackError::Message)
            })
            .await
            .map_err(|error| PlaybackError::Message(error.to_string()))?
    }
}
