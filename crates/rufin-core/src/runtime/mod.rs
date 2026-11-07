//! Commands, updates, and startup data exchanged between application clients and Rufin.
//!
//! `app` constructs these handles; the crates behind them implement the behavior.

use std::sync::Arc;

mod diagnostics;
mod events;
mod inputs;
pub mod release_update;
pub mod scrobbling;
pub mod source;
mod storage;
mod waveform;
pub use diagnostics::DiagnosticsHandle;
pub use events::ProductReceivers;
pub use inputs::{RuntimeInputs, SelectedLibrary, SourceEvent, VisualizerPublication};
pub use release_update::{ReleaseHistory, ReleaseNote, ReleaseUpdate, ReleaseUpdateHandle};
pub use scrobbling::{
    LastFmPreferences, LibreFmPreferences, ListenBrainzPreferences, ScrobblingConnection,
    ScrobblingConnectionEvent, ScrobblingHandle, ScrobblingPreferences,
};
pub use source::SourceHandle;
pub use storage::{CacheKind, CacheSummary, CacheUsage, StorageSummary};
pub use waveform::WaveformProjection;

#[derive(Clone)]
pub struct ProductHandles {
    pub settings: crate::SettingsHandle,
    pub appearance: tokio::sync::watch::Sender<std::collections::BTreeMap<String, String>>,
    pub backup: Arc<crate::backup::BackupOwner>,
    pub connect: Arc<crate::connect::ConnectOwner>,
    pub library: Arc<library::Catalog>,
    pub runtime: tokio::runtime::Handle,
    pub source: SourceHandle,
    pub downloads: crate::downloads::Downloads,
    pub playback: std::sync::Arc<crate::playback::PlaybackOwner>,
    pub artwork: artwork::Artwork,
    pub lyrics: crate::lyrics::LyricsHandle,
    pub release_updates: ReleaseUpdateHandle,
    pub scrobbling: ScrobblingHandle,
}
