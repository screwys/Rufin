//! Rufin application composition and concrete product operations.
#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!();

pub mod activity;
mod album_release;
pub mod app;
pub mod backup;
pub mod collection_context;
pub mod connect;
pub mod detail_links;
pub mod diagnostics;
pub mod downloads;
mod features;
mod loudness;
pub mod paths;
pub mod playback;
pub mod presence;
pub mod radio;
pub mod release_update;
pub mod route;
pub mod runtime;
pub mod scrobbling;
pub mod settings;
pub mod source;
pub mod themes;
pub mod view;
mod waveform;
pub use settings::SettingsHandle;

pub mod lyrics;
pub mod menu;

pub mod metadata;
pub mod open;
pub mod playlist_files;
pub mod playlists;

pub use playback::equalizer::{
    EqCastFormat, EqEdit, EqPresetFormat, EqPresetSummary, EqSubscription, EqView,
    EqualizerPreferences,
};

pub(crate) fn string_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
