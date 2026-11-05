use library::{FolderChoice, FolderKey, SourceKey};
use std::sync::Arc;

use super::{DiagnosticsHandle, ProductHandles, ProductReceivers};

#[derive(Clone, PartialEq)]
pub struct SelectedLibrary {
    pub source_id: sources::SourceId,
    pub source_key: SourceKey,
    pub music_folder_key: Option<FolderKey>,
    pub music_folder_object_id: Option<String>,
    pub music_folders: Arc<[FolderChoice]>,
}

pub enum SourceEvent {
    Notice(SourceNotice),
    PlaylistSourceMismatch(sources::SourceId),
}

pub struct VisualizerPublication {
    pub run: playback::RunId,
    /// Linear amplitudes on the canonical ERB axis; map through the current visualizer preset.
    pub levels: Vec<f64>,
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
    pub receivers: ProductReceivers,
    pub release_history: super::ReleaseHistory,
}
