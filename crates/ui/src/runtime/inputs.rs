use crate::SettingsHandle;
use library::{Database, FolderKey, FolderRow, PlaylistKey, SourceKey};
use playback::PlaybackProjection;
use std::sync::Arc;

use super::source::{ConfiguredSources, SelectedSourceHandle, SourceOperation, SourceProgress};
use super::{DiagnosticsHandle, ProductHandles, ProductReceivers};

#[derive(Clone)]
pub struct SelectedLibrary {
    pub source_id: sources::SourceId,
    pub source_key: SourceKey,
    pub music_folder_key: Option<FolderKey>,
    pub music_folder_object_id: Option<String>,
    pub music_folders: Arc<[FolderRow]>,
    pub database: Arc<Database>,
    pub runtime: tokio::runtime::Handle,
    pub operations: SelectedSourceHandle,
}

pub enum SourceEvent {
    Configured(ConfiguredSources),
    Selected {
        configured: ConfiguredSources,
        selected: SelectedLibrary,
    },
    CatalogReplaced {
        configured: ConfiguredSources,
        selected: SelectedLibrary,
    },
    Operation(SourceOperation),
    ArtworkPreparation {
        source_key: SourceKey,
        revision: u64,
        progress: Option<SourceProgress>,
    },
    CatalogPublished(CatalogPublication),
    Notice(SourceNotice),
    ReleaseSelected {
        acknowledged: async_channel::Sender<()>,
    },
}

pub struct PlaybackPublication {
    pub projection: PlaybackProjection,
}

pub struct VisualizerPublication {
    pub run: playback::RunId,
    pub levels: Vec<f64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogPublication {
    pub source_key: Option<SourceKey>,
    pub favorite: Option<FavoriteSettlement>,
    pub change: CatalogChange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogChange {
    /// Completed source acquisition; refresh selected folder/count summaries too.
    Acquired,
    Broad,
    Home,
    Playlists(Option<PlaylistKey>),
    Album(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FavoriteSettlement {
    pub target: library::FavoriteTarget,
    pub requested: bool,
    pub effective: bool,
}

#[derive(Clone, Debug)]
pub struct SourceNotice {
    pub source_key: SourceKey,
    pub kind: SourceNoticeKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceNoticeKind {
    ServerUnreachable,
    FavoriteRejected,
}

pub struct RuntimeInputs {
    pub temporary_store: bool,
    pub diagnostics: DiagnosticsHandle,
    pub products: ProductHandles,
    pub settings: SettingsHandle,
    pub receivers: ProductReceivers,
    pub configured_sources: ConfiguredSources,
    pub source_operation: SourceOperation,
    pub release_history: super::ReleaseHistory,
}
