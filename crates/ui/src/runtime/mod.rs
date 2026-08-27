//! Commands, updates, and startup data exchanged between the UI and Rufin.
//!
//! `rufin` constructs these handles; the crates behind them implement the behavior.

mod diagnostics;
mod events;
mod inputs;
mod release_update;
mod scrobbling;
pub mod source;
mod waveform;

pub use ::playback::PlaybackHandles;
pub use diagnostics::{DiagnosticsHandle, DiagnosticsPort};
pub use downloads::{
    DownloadEvent, DownloadQueueItem, DownloadQueueSnapshot, DownloadQueueState, DownloadSubject,
};
pub use events::ProductReceivers;
pub use inputs::{
    CatalogChange, CatalogPublication, FavoriteSettlement, PlaybackPublication, RuntimeInputs,
    SelectedLibrary, SourceEvent, SourceNotice, SourceNoticeKind, VisualizerPublication,
};
pub use release_update::{
    ReleaseHistory, ReleaseNote, ReleaseUpdate, ReleaseUpdateHandle, ReleaseUpdatePort,
};
pub use scrobbling::{
    LastFmPreferences, LibreFmPreferences, ListenBrainzPreferences, ScrobblingConnection,
    ScrobblingConnectionEvent, ScrobblingHandle, ScrobblingPort, ScrobblingPreferences,
};
pub use source::LibraryRefreshTrigger;
pub use source::{SelectedSourceHandle, SelectedSourcePort, SourceHandle, SourcePort};
pub use waveform::WaveformProjection;

#[derive(Clone)]
pub struct ProductHandles {
    pub source: SourceHandle,
    pub downloads: downloads::Downloads,
    pub playback: PlaybackHandles,
    pub artwork: artwork::Artwork,
    pub lyrics: lyrics::LyricsHandle,
    pub release_updates: ReleaseUpdateHandle,
    pub scrobbling: ScrobblingHandle,
}
