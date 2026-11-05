//! Constructs Rufin's product owners and connects their concrete event lanes.

use crate::string_error;
use std::sync::Arc;

use crate::runtime::{DiagnosticsHandle, ProductHandles, ProductReceivers, RuntimeInputs};
use crate::scrobbling::Scrobbler;
use async_channel::{bounded, unbounded};
use playback::PlaybackBackend;
use secrets::SwitchableSecretStore;
use tracing::warn;

use crate::paths::Paths;
use crate::playback::PlaybackOwner;
use crate::release_update::ReleaseUpdateOwner;
use crate::scrobbling::ScrobblingOwner;
use crate::settings::{ApplicationPreferences, SettingsFile, SettingsOwner, platform_secret_store};
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
    // Source credentials saved before they were sealed are sealed once; the keyring may wait.
    let (legacy_settings, legacy_secrets) = (settings.clone(), Arc::clone(&secrets));
    runtime.spawn_blocking(move || {
        if let Err(error) =
            crate::settings::migrate_source_credentials(&legacy_settings, legacy_secrets.as_ref())
        {
            warn!(%error, "could not migrate saved source credentials");
        }
    });
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
    // The Catalog shows rows by these switches; it cannot read the settings itself.
    let display = |library: &library::Catalog, application: &ApplicationPreferences| {
        library.set_distinct_track_covers(application.prefer_distinct_track_covers);
        library.set_cover_lookup(application.external_metadata_enabled);
        library.set_use_sort_tags(application.use_sort_tags);
    };
    display(&library, &stored.ui.application);
    crate::settings::follow(
        &runtime,
        &settings,
        Arc::downgrade(&library),
        move |library, previous, current| {
            if previous.ui.application != current.ui.application {
                display(&library, &current.ui.application);
            }
            std::future::ready(())
        },
    );
    let scrobbler = Arc::new(Scrobbler::new(
        library.as_ref().clone(),
        runtime.clone(),
        settings.clone(),
    ));

    let (source_events, source_receiver) = unbounded();
    let (visualizer_events, visualizer_receiver) = bounded(1);
    let (download_events, download_receiver) = unbounded();
    let (waveform_events, waveform_receiver) = unbounded();
    let (lyrics_events, lyrics_receiver) = unbounded();
    let (release_update_events, release_update_receiver) = unbounded();
    let release_updates = ReleaseUpdateOwner::new(
        settings.clone(),
        runtime.clone(),
        release_update_events,
        paths.release_history_file(),
        take_previous_update_result,
    );
    let release_history = release_updates.initial_history();

    let source = SourceOwner::open_dormant(
        paths.artwork_dir(),
        library.clone(),
        paths.downloads_dir(),
        settings.clone(),
        Arc::clone(&secrets),
        runtime.clone(),
        SourceOutputs {
            events: source_events.clone(),
            downloads: download_events,
        },
    )?;
    let artwork = source.artwork().clone();
    // Recent search results show the pictures their rows have once the Catalog is loaded.
    if !stored.ui.recent_search_results.is_empty() {
        let (library, settings) = (Arc::clone(&library), settings.clone());
        runtime.spawn(async move {
            library.loaded().await;
            let uris = settings
                .load()
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
        });
    }
    let waveform = WaveformOwner::new(
        runtime.clone(),
        waveform_events,
        &paths.playback_dir(),
        &settings,
    );
    let lyrics = crate::lyrics::LyricsService::new(
        library.as_ref().clone(),
        Arc::clone(&source),
        runtime.clone(),
        &settings,
        lyrics_events,
        paths.data_dir().join("japanese-readings"),
    );
    let playback = PlaybackOwner::new(
        library.clone(),
        settings.clone(),
        runtime.clone(),
        visualizer_events,
        visualizer_receiver.clone(),
        Arc::clone(&waveform),
        Arc::clone(&lyrics),
        audio_outputs,
        Arc::clone(&scrobbler),
        Arc::clone(&source),
        start_backend,
    );
    playback.start();
    crate::presence::follow(&runtime, &settings, &playback);
    let scrobbling = ScrobblingOwner::new(
        settings.clone(),
        Arc::clone(&secrets),
        runtime.clone(),
        Arc::clone(&scrobbler),
    );
    scrobbling.start();

    let mut features = source.features();
    features.push(Arc::new(crate::loudness::Normalization(Arc::downgrade(
        &playback,
    ))));
    features.push(Arc::new(crate::scrobbling::Listens(Arc::downgrade(
        &scrobbler,
    ))));
    crate::features::run(&runtime, &settings, &library, features);

    let settings_handle = SettingsOwner::new(settings.clone(), Arc::clone(&secrets));
    let backup = crate::backup::BackupOwner::new(
        Arc::clone(&library),
        settings_handle.as_ref().clone(),
        Arc::clone(&source),
        Arc::clone(&playback),
        runtime.clone(),
        paths.data_dir().join("backups"),
    );
    backup.start();

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
        settings_handle.as_ref().clone(),
        Arc::clone(&source),
        Arc::clone(&playback),
        lyrics.handle(),
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
            downloads: source.downloads().clone(),
            source: source_handle,
            playback,
            artwork,
            lyrics: lyrics.handle(),
            release_updates,
            scrobbling,
        },
        receivers: ProductReceivers {
            source: source_receiver,
            downloads: download_receiver,
            visualizer: visualizer_receiver,
            waveform: waveform_receiver,
            lyrics: lyrics_receiver,
            release_updates: release_update_receiver,
        },
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
