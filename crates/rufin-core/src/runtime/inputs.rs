use crate::SettingsHandle;
use library::{FolderChoice, FolderKey, PlaylistKey, SourceKey};
use std::sync::Arc;

use super::source::{ConfiguredSources, SourceOperation};
use super::{DiagnosticsHandle, ProductHandles, ProductReceivers};

#[derive(Clone)]
pub struct SelectedLibrary {
    pub source_id: sources::SourceId,
    pub source_key: SourceKey,
    pub music_folder_key: Option<FolderKey>,
    pub music_folder_object_id: Option<String>,
    pub music_folders: Arc<[FolderChoice]>,
}

pub enum SourceEvent {
    Configured(ConfiguredSources),
    Selected {
        configured: ConfiguredSources,
        selected: SelectedLibrary,
    },
    Operation(SourceOperation),
    CatalogPublished(CatalogPublication),
    Notice(SourceNotice),
    PlaylistSourceMismatch(sources::SourceId),
    ReleaseSelected,
}

pub struct VisualizerPublication {
    pub run: playback::RunId,
    /// Linear amplitudes on the canonical ERB axis; map through the current visualizer preset.
    pub levels: Vec<f64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogPublication {
    pub source_key: Option<SourceKey>,
    pub favorite: Option<FavoriteSettlement>,
    pub change: CatalogChange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum CatalogChange {
    Activity,
    Scope,
    Broad,
    Home,
    Playlists(Option<PlaylistKey>),
    Album(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CatalogRevision {
    pub catalog: u64,
    pub activity: u64,
    pub change: Option<CatalogChange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
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
    pub secret_storage_fallbacks: async_channel::Receiver<crate::settings::KeyringSecretStore>,
    pub diagnostics: DiagnosticsHandle,
    pub products: ProductHandles,
    pub settings: SettingsHandle,
    pub receivers: ProductReceivers,
    pub configured_sources: ConfiguredSources,
    pub source_operation: SourceOperation,
    pub release_history: super::ReleaseHistory,
}
