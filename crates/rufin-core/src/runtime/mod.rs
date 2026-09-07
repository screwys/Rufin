//! Commands, updates, and startup data exchanged between the UI and Rufin.
//!
//! `rufin` constructs these handles; the crates behind them implement the behavior.

use std::sync::Arc;

mod backup;
mod diagnostics;
mod events;
mod inputs;
mod release_update;
mod scrobbling;
pub mod source;
mod waveform;

pub use ::playback::PlaybackHandles;
pub use backup::{BackupHandle, BackupPreview, BackupSettings};
pub use diagnostics::DiagnosticsHandle;
pub use downloads::{
    DownloadEvent, DownloadQueueItem, DownloadQueueSnapshot, DownloadQueueState, DownloadSubject,
};
pub use events::ProductReceivers;
pub use inputs::{
    CatalogChange, CatalogPublication, FavoriteSettlement, RuntimeInputs, SelectedLibrary,
    SourceEvent, SourceNotice, SourceNoticeKind, VisualizerPublication,
};
pub use release_update::{ReleaseHistory, ReleaseNote, ReleaseUpdate, ReleaseUpdateHandle};
pub use scrobbling::{
    LastFmPreferences, LibreFmPreferences, ListenBrainzPreferences, ScrobblingConnection,
    ScrobblingConnectionEvent, ScrobblingHandle, ScrobblingPreferences,
};
pub use source::LibraryRefreshTrigger;
pub use source::{SelectedSourceHandle, SourceHandle};
pub use waveform::WaveformProjection;

#[derive(Clone)]
pub struct ProductHandles {
    pub backup: BackupHandle,
    pub library: Arc<library::Database>,
    pub runtime: tokio::runtime::Handle,
    pub source: SourceHandle,
    pub downloads: downloads::Downloads,
    pub playback: PlaybackHandles,
    pub artwork: artwork::Artwork,
    pub lyrics: crate::lyrics::LyricsHandle,
    pub release_updates: ReleaseUpdateHandle,
    pub scrobbling: ScrobblingHandle,
}
