//! Rufin application composition and concrete product operations.
mod album_release;
pub mod app;
pub mod backup;
pub mod connect;
pub mod diagnostics;
mod loudness;
pub mod paths;
pub mod playback;
pub mod radio;
pub mod release_update;
pub mod runtime;
pub mod scrobbling;
pub mod settings;
pub mod source;
pub mod themes;
mod waveform;
pub use settings::SettingsHandle;

pub mod lyrics;

pub mod artwork;

pub mod metadata;
pub mod playlist_files;
pub mod playlists;
