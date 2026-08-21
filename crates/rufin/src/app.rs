//! Constructs Rufin's product owners and connects their concrete event lanes.

use std::sync::Arc;

use ::scrobbling::Scrobbler;
use async_channel::unbounded;
use playback::{PlaybackBackend, PlaybackHandles};
use playback_gstreamer::GStreamerPlaybackBackend;
use secrets::SwitchableSecretStore;
use tracing::warn;
use ui::runtime::{DiagnosticsHandle, ProductHandles, ProductReceivers, RuntimeInputs};

use crate::paths;
use crate::playback::PlaybackOwner;
use crate::release_update::ReleaseUpdateOwner;
use crate::scrobbling::ScrobblingOwner;
use crate::settings::{
    SettingsFile, SettingsUiPort, platform_secret_store, startup_scrobbling_settings,
};
use crate::source::{SourceBootstrap, SourceOutputs, SourceOwner};
use crate::waveform::WaveformOwner;

pub(crate) fn runtime_inputs(
    diagnostics: DiagnosticsHandle,
    take_previous_update_result: bool,
) -> Result<RuntimeInputs, String> {
    if let Err(error) = paths::prepare() {
        warn!(%error, "could not prepare every Rufin data directory");
    }
    let runtime = tokio::runtime::Handle::current();
    let settings = SettingsFile::open(paths::settings_file()).unwrap_or_else(|error| {
        warn!(%error, "could not use saved settings; startup will continue with defaults");
        SettingsFile::memory()
    });
    let stored = settings.load();
    let secrets = Arc::new(SwitchableSecretStore::new(platform_secret_store(&stored)));
    let (library, repair) = match library::Libraries::open_with_repair(paths::store_file()) {
        Ok(opened) => opened,
        Err(error) => {
            warn!(%error, "could not use the saved Store; startup will continue in memory");
            (library::Libraries::memory().map_err(string_error)?, None)
        }
    };
    if let Some(repair) = repair {
        warn!(
            preserved_store_path = %repair.preserved_store.display(),
            recovered_rows = repair.recovered_rows,
            skipped_rows = repair.skipped_rows,
            unreadable_families = ?repair.unreadable_families,
            "repaired the Rufin Store; source facts will be rebuilt"
        );
    }
    let scrobbler = Arc::new(Scrobbler::new(
        library.clone(),
        startup_scrobbling_settings(&settings, &secrets),
        stored.ui.private_mode,
    )?);

    let (source_events, source_receiver) = unbounded();
    let (playback_events, playback_receiver) = unbounded();
    let (download_events, download_receiver) = unbounded();
    let (discovery_events, discovery_receiver) = unbounded();
    let (waveform_events, waveform_receiver) = unbounded();
    let (lyrics_events, lyrics_receiver) = unbounded();
    let (release_update_events, release_update_receiver) = unbounded();
    let artwork = match artwork::Artwork::new(paths::artwork_dir(), runtime.clone()) {
        Ok(artwork) => artwork,
        Err(error) => {
            warn!(%error, "could not use the artwork cache; startup will continue with a temporary cache");
            artwork::Artwork::new(temporary_artwork_dir(), runtime.clone()).map_err(string_error)?
        }
    };
    let downloads = downloads::Downloads::new(
        paths::downloads_dir(),
        runtime.clone(),
        download_events,
        stored.ui.downloads.clone(),
    );
    let discord = Arc::new(desktop_integration::Discord::new());
    let release_updates = ReleaseUpdateOwner::new(
        settings.clone(),
        runtime.clone(),
        release_update_events,
        paths::release_history_file(),
        take_previous_update_result,
    );
    let release_history = release_updates.initial_history();

    let SourceBootstrap {
        owner: source,
        configured,
        operation,
    } = SourceOwner::open_dormant(
        artwork.clone(),
        library.clone(),
        downloads.clone(),
        settings.clone(),
        Arc::clone(&secrets),
        Arc::clone(&scrobbler),
        runtime.clone(),
        SourceOutputs {
            events: source_events.clone(),
            discovery: discovery_events,
        },
    );
    let waveform = WaveformOwner::new(
        runtime.clone(),
        waveform_events,
        paths::playback_dir(),
        stored.ui.seekbar_waveform_enabled,
    );
    let lyrics = lyrics::LyricsService::new(
        library.clone(),
        runtime.clone(),
        stored.ui.lyrics.clone(),
        stored.ui.private_mode,
        lyrics_events,
    );
    let playback = PlaybackOwner::new(
        library.clone(),
        settings.clone(),
        runtime.clone(),
        playback_events,
        source.acceptance_sender(),
        artwork.clone(),
        Arc::clone(&waveform),
        Arc::clone(&lyrics),
        Arc::clone(&discord),
        Arc::clone(&scrobbler),
        || {
            GStreamerPlaybackBackend::new()
                .map(|backend| Box::new(backend) as Box<dyn PlaybackBackend>)
                .map_err(|error| error.to_string())
        },
    );
    let scrobbling = ScrobblingOwner::new(
        settings.clone(),
        Arc::clone(&secrets),
        runtime.clone(),
        scrobbler,
        Arc::clone(&playback),
    );

    source.attach_playback(&playback);

    let settings_playback = Arc::clone(&playback);
    let settings_lyrics = Arc::clone(&lyrics);
    let settings_source = Arc::clone(&source);
    let settings_downloads = downloads.clone();
    let settings_scrobbling = Arc::clone(&scrobbling);
    let settings_handle = SettingsUiPort::new(settings, move |previous, current| {
        if previous.ui.rich_presence != current.ui.rich_presence
            || previous.ui.private_mode != current.ui.private_mode
            || previous.ui.lastfm_api_key != current.ui.lastfm_api_key
        {
            settings_playback.update_discord_settings();
        }
        if previous.ui.seekbar_waveform_enabled != current.ui.seekbar_waveform_enabled {
            settings_playback.waveform_setting_changed(current.ui.seekbar_waveform_enabled);
        }
        if previous.ui.playback != current.ui.playback {
            settings_playback.playback_settings_changed(current.ui.playback.clone());
        }
        if previous.ui.cast_proxy_enabled != current.ui.cast_proxy_enabled {
            settings_playback.cast_proxy_setting_changed(current.ui.cast_proxy_enabled);
        }
        if previous.ui.cast_network_interface != current.ui.cast_network_interface {
            settings_playback
                .cast_network_setting_changed(current.ui.cast_network_interface.clone());
        }
        if previous.ui.auto_dj_refill_threshold != current.ui.auto_dj_refill_threshold {
            settings_playback.auto_dj_threshold_changed(
                current.ui.auto_dj_enabled,
                current.ui.auto_dj_refill_threshold,
            );
        }
        if previous.ui.private_mode != current.ui.private_mode {
            settings_scrobbling.private_mode_changed(current.ui.private_mode);
        }
        if previous.ui.lyrics != current.ui.lyrics
            || previous.ui.private_mode != current.ui.private_mode
        {
            settings_lyrics.settings_changed(current.ui.lyrics.clone(), current.ui.private_mode);
        }
        if previous.ui.allows_external_metadata_lookup()
            != current.ui.allows_external_metadata_lookup()
        {
            settings_source
                .album_release_settings_changed(current.ui.allows_external_metadata_lookup());
        }
        if previous.ui.downloads != current.ui.downloads {
            settings_downloads.settings_changed(current.ui.downloads.clone());
        }
    });

    source.start()?;
    let source_handle: ui::runtime::SourceHandle = source.clone();
    let transport: playback::TransportHandle = playback.clone();
    let queue: playback::QueueHandle = playback.clone();
    let radio: playback::RadioHandle = playback;

    Ok(RuntimeInputs {
        diagnostics,
        products: ProductHandles {
            source: source_handle,
            downloads,
            playback: PlaybackHandles {
                transport,
                queue,
                radio,
            },
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
            playback: playback_receiver,
            waveform: waveform_receiver,
            lyrics: lyrics_receiver,
            release_updates: release_update_receiver,
        },
        configured_sources: configured,
        source_operation: operation,
        release_history,
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
