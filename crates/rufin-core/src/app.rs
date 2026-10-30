//! Constructs Rufin's product owners and connects their concrete event lanes.

use std::sync::Arc;

use crate::runtime::{DiagnosticsHandle, ProductHandles, ProductReceivers, RuntimeInputs};
use ::scrobbling::Scrobbler;
use async_channel::{bounded, unbounded};
use playback::PlaybackBackend;
use secrets::SwitchableSecretStore;
use tracing::warn;

use crate::paths::Paths;
use crate::playback::PlaybackOwner;
use crate::release_update::ReleaseUpdateOwner;
use crate::scrobbling::ScrobblingOwner;
use crate::settings::{SettingsFile, SettingsOwner, platform_secret_store};
use crate::source::{SourceOutputs, SourceOwner};
use crate::waveform::WaveformOwner;

/// Runs an application host with Rufin's worker runtime available on its calling thread.
/// The host shuts down playback before returning; workers are stopped afterwards.
pub fn with_runtime<T>(host: impl FnOnce(&tokio::runtime::Runtime) -> T) -> std::io::Result<T> {
    let runtime = worker_runtime()?;
    let result = {
        let _guard = runtime.enter();
        host(&runtime)
    };
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    Ok(result)
}

pub fn worker_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("rufin-async")
        .build()
}

pub async fn runtime_inputs<StartBackend>(
    diagnostics: DiagnosticsHandle,
    take_previous_update_result: bool,
    settings: SettingsFile,
    paths: Paths,
    start_backend: StartBackend,
    audio_outputs: fn() -> Vec<playback::AudioOutput>,
    observe_playback: Arc<dyn Fn(Option<&playback::PlaybackProjection>) + Send + Sync>,
    observe_settings: Arc<
        dyn Fn(&crate::settings::Settings, Option<&playback::PlaybackProjection>) + Send + Sync,
    >,
) -> Result<RuntimeInputs, String>
where
    StartBackend: Fn(
            tokio::sync::watch::Receiver<playback::ProcessingRequest>,
        ) -> Result<Box<dyn PlaybackBackend>, String>
        + Send
        + Sync
        + 'static,
{
    let runtime = tokio::runtime::Handle::current();
    let secret_storage_fallbacks = settings.secret_storage_fallbacks.1.clone();
    let stored = settings.load();
    let secrets = Arc::new(SwitchableSecretStore::new(platform_secret_store(&settings)));
    let store_path = paths.store_file();
    let (catalog, temporary_store) = match library::Catalog::open_installation(
        &store_path,
        &paths.released_store_file(),
        &paths.catalog_file(),
    )
    .await
    {
        Ok(catalog) => (catalog, false),
        Err(error) if error.is_store_unavailable() => {
            warn!(%error, path=%store_path.display(), "could not access the durable Store; startup will continue with temporary storage");
            let directory = tempfile::Builder::new()
                .prefix("rufin-store-")
                .tempdir()
                .map_err(string_error)?
                .keep();
            (
                library::Catalog::open(directory.join("store.sqlite"))
                    .await
                    .map_err(string_error)?,
                true,
            )
        }
        Err(error) => return Err(string_error(error)),
    };
    let library = Arc::new(catalog);
    library.set_distinct_track_covers(stored.ui.application.prefer_distinct_track_covers);
    library.set_cover_lookup(stored.ui.application.external_metadata_enabled);
    library.set_use_sort_tags(stored.ui.application.use_sort_tags);
    let scrobbling_library = library.as_ref().clone();
    let scrobbling_runtime = runtime.clone();
    let scrobbling_settings = stored.scrobbling_runtime_settings();
    let private_mode = stored.ui.application.private_mode;
    let scrobbler = Arc::new(
        runtime
            .spawn_blocking(move || {
                Scrobbler::new(
                    scrobbling_library,
                    scrobbling_runtime,
                    scrobbling_settings,
                    private_mode,
                )
            })
            .await
            .map_err(string_error)??,
    );

    let (source_events, source_receiver) = unbounded();
    let (visualizer_events, visualizer_receiver) = bounded(1);
    let (download_events, download_receiver) = unbounded();
    let (discovery_events, discovery_receiver) = unbounded();
    let (waveform_events, waveform_receiver) = unbounded();
    let (lyrics_events, lyrics_receiver) = unbounded();
    let (release_update_events, release_update_receiver) = unbounded();
    let artwork = match artwork::Artwork::new(paths.artwork_dir(), runtime.clone()) {
        Ok(artwork) => artwork,
        Err(error) => {
            warn!(%error, "could not use the artwork cache; startup will continue with a temporary cache");
            artwork::Artwork::new(temporary_artwork_dir(), runtime.clone()).map_err(string_error)?
        }
    };
    let downloads = downloads::Downloads::new(
        artwork.clone(),
        paths.downloads_dir(),
        library.as_ref().clone(),
        runtime.clone(),
        download_events,
        stored.ui.downloads.clone(),
    );
    let release_updates = ReleaseUpdateOwner::new(
        settings.clone(),
        runtime.clone(),
        release_update_events,
        paths.release_history_file(),
        take_previous_update_result,
    );
    let release_history = release_updates.initial_history();

    let source = SourceOwner::open_dormant(
        artwork.clone(),
        library.clone(),
        downloads.clone(),
        settings.clone(),
        Arc::clone(&secrets),
        runtime.clone(),
        SourceOutputs {
            events: source_events.clone(),
            discovery: discovery_events,
        },
    );
    artwork.install_catalog(Arc::clone(&library));
    if !stored.ui.recent_search_results.is_empty() {
        let uris = stored
            .ui
            .recent_search_results
            .iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>();
        match library
            .artwork_bindings(&uris, &library::ReadCancellation::new())
            .await
        {
            Ok(bindings) => {
                if let Err(error) = settings.update(|stored| {
                    for (recent, binding) in
                        stored.ui.recent_search_results.iter_mut().zip(bindings)
                    {
                        if binding.is_some() {
                            recent.artwork_binding = binding;
                        }
                    }
                    Ok(())
                }) {
                    warn!(%error, "could not update recent search artwork");
                }
            }
            Err(error) => warn!(%error, "could not read recent search artwork"),
        }
    }
    let artwork_source = Arc::downgrade(&source);
    artwork.install_source_resolver(move |source_id| {
        artwork_source
            .upgrade()
            .and_then(|owner| owner.client(source_id).ok())
    });
    let waveform = WaveformOwner::new(
        runtime.clone(),
        waveform_events,
        &paths.playback_dir(),
        stored.ui.player_display.seekbar_waveform_enabled,
    );
    let lyrics = crate::lyrics::LyricsService::new(
        library.as_ref().clone(),
        Arc::clone(&source),
        runtime.clone(),
        stored.ui.lyrics.clone(),
        stored.ui.application.private_mode,
        lyrics_events,
        paths.data_dir().join("japanese-readings"),
    );
    let playback = PlaybackOwner::new(
        library.clone(),
        settings.clone(),
        runtime.clone(),
        visualizer_events,
        visualizer_receiver.clone(),
        artwork.clone(),
        Arc::clone(&waveform),
        Arc::clone(&lyrics),
        observe_playback,
        audio_outputs,
        Arc::clone(&scrobbler),
        start_backend,
    );
    let observed_playback = Arc::clone(&playback);
    let settings_committed: Arc<dyn Fn(&crate::settings::Settings) + Send + Sync> =
        Arc::new(move |settings| {
            observe_settings(settings, observed_playback.current_projection().as_ref())
        });
    settings_committed(&stored.ui);
    playback.install_source_owner(&source);
    playback.start();
    let scrobbling = ScrobblingOwner::new(
        settings.clone(),
        Arc::clone(&secrets),
        runtime.clone(),
        scrobbler,
        &library,
        Arc::clone(&settings_committed),
    );
    scrobbling.start();

    source.attach_playback(&playback);

    let settings_playback = Arc::clone(&playback);
    let settings_lyrics = Arc::clone(&lyrics);
    let settings_downloads = downloads.clone();
    let settings_source = Arc::clone(&source);
    let settings_scrobbling = Arc::clone(&scrobbling);
    let settings_library = Arc::clone(&library);
    let settings_handle = SettingsOwner::new(
        settings.clone(),
        Arc::clone(&secrets),
        move |previous, current, credentials_changed| {
            settings_library
                .set_distinct_track_covers(current.ui.application.prefer_distinct_track_covers);
            settings_library.set_cover_lookup(current.ui.application.external_metadata_enabled);
            settings_library.set_use_sort_tags(current.ui.application.use_sort_tags);
            if previous.ui.rich_presence != current.ui.rich_presence
                || previous.ui.application.private_mode != current.ui.application.private_mode
                || previous.ui.lastfm_api_key != current.ui.lastfm_api_key
            {
                settings_committed(&current.ui);
            }
            if previous.ui.player_display.seekbar_waveform_enabled
                != current.ui.player_display.seekbar_waveform_enabled
            {
                settings_playback
                    .waveform_setting_changed(current.ui.player_display.seekbar_waveform_enabled);
            }
            if previous.ui.playback != current.ui.playback {
                settings_playback
                    .playback_settings_changed(&previous.ui.playback, current.ui.playback.clone());
            }
            if previous.ui.application.cast_proxy_enabled
                != current.ui.application.cast_proxy_enabled
            {
                settings_playback
                    .cast_proxy_setting_changed(current.ui.application.cast_proxy_enabled);
            }
            if previous.ui.cast_network_interface != current.ui.cast_network_interface {
                settings_playback
                    .cast_network_setting_changed(current.ui.cast_network_interface.clone());
            }
            if previous.ui.application.auto_dj_refill_threshold
                != current.ui.application.auto_dj_refill_threshold
            {
                settings_playback.auto_dj_threshold_changed(
                    current.ui.auto_dj_enabled,
                    current.ui.application.auto_dj_refill_threshold,
                );
            }
            if previous.ui.application.private_mode != current.ui.application.private_mode
                || previous.scrobbling != current.scrobbling
                || credentials_changed
            {
                settings_scrobbling.settings_changed(credentials_changed);
            }
            if previous.ui.lyrics != current.ui.lyrics
                || previous.ui.application.private_mode != current.ui.application.private_mode
            {
                settings_lyrics.settings_changed(
                    current.ui.lyrics.clone(),
                    current.ui.application.private_mode,
                );
            }
            if previous.ui.application.download_artwork_automatically
                != current.ui.application.download_artwork_automatically
                || previous.ui.application.download_lyrics_automatically
                    != current.ui.application.download_lyrics_automatically
            {
                settings_source.asset_downloads_changed();
            }
            if previous.ui.downloads != current.ui.downloads {
                settings_downloads.settings_changed(current.ui.downloads.clone());
                let rules = |downloads: &[crate::settings::SourceDownloadSettings]| {
                    downloads
                        .iter()
                        .map(|source| source.rules.clone())
                        .collect::<Vec<_>>()
                };
                if rules(&previous.ui.downloads) != rules(&current.ui.downloads) {
                    settings_downloads.library_changed(None);
                }
            }
        },
    );

    source.attach_settings(&settings_handle);

    let backup = crate::backup::BackupOwner::new(
        Arc::clone(&library),
        settings.clone(),
        settings_handle.as_ref().clone(),
        Arc::clone(&secrets),
        Arc::clone(&source),
        Arc::clone(&playback),
        runtime.clone(),
        paths.data_dir().join("backups"),
    );
    backup.start();
    let stored = settings.load();

    // Source rows follow the settings once the Catalog is loaded; the first screen does not
    // wait for it.
    let reconciled = Arc::clone(&library);
    let configured = stored.sources.configured.clone();
    runtime.spawn(async move {
        for configured in &configured {
            if let Err(error) = reconciled
                .reconcile_source(
                    &configured.configuration.source_id,
                    &configured.configuration.kind,
                    &configured.configuration.name,
                )
                .await
            {
                warn!(%error, source_id=%configured.configuration.source_id, "could not reconcile configured source mapping");
            }
        }
    });
    let connect = crate::connect::ConnectOwner::new(
        paths.connect_dir(),
        Arc::clone(&library),
        settings.clone(),
        settings_handle.as_ref().clone(),
        Arc::clone(&secrets),
        Arc::clone(&source),
        Arc::clone(&playback),
        runtime.clone(),
    );
    source.install_connect(&connect);
    connect.start();

    source.start()?;
    let source_handle: crate::runtime::SourceHandle = source.clone();

    Ok(RuntimeInputs {
        temporary_store,
        secret_storage_fallbacks,
        diagnostics,
        products: ProductHandles {
            settings: Arc::clone(&settings_handle),
            appearance: tokio::sync::watch::channel(crate::themes::controller_colors(
                &stored.ui,
                &paths.config_dir().join("themes"),
            ))
            .0,
            backup,
            connect,
            library,
            runtime,
            source: source_handle,
            downloads,
            playback,
            artwork,
            lyrics: lyrics.handle(),
            release_updates,
            scrobbling,
        },
        settings: settings_handle,
        receivers: ProductReceivers {
            source: source_receiver,
            source_discovery: discovery_receiver,
            downloads: download_receiver,
            visualizer: visualizer_receiver,
            waveform: waveform_receiver,
            lyrics: lyrics_receiver,
            release_updates: release_update_receiver,
        },
        configured_sources: source.list_sources(),
        source_operation: source.operation().borrow().clone(),
        release_history,
    })
}

pub fn startup_settings(paths: &Paths) -> SettingsFile {
    if let Err(error) = paths.prepare() {
        warn!(%error, "could not prepare every Rufin data directory");
    }
    SettingsFile::open(paths.settings_file()).unwrap_or_else(|error| {
        warn!(%error, "could not use saved settings; startup will continue with defaults");
        SettingsFile::memory_at(paths.config_dir())
    })
}

fn string_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn temporary_artwork_dir() -> std::path::PathBuf {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!("rufin-artwork-{}-{started}", std::process::id()))
}
