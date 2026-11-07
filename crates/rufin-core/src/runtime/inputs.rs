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

impl SelectedLibrary {
    /// Whether reads in `a` read what they read in `b`: the same source and, when `folder`,
    /// the same music folder. The folders offered don't count.
    pub fn same(a: Option<&Self>, b: Option<&Self>, folder: bool) -> bool {
        match (a, b) {
            (Some(a), Some(b)) => {
                a.source_key == b.source_key
                    && (!folder
                        || (a.music_folder_key == b.music_folder_key
                            && a.music_folder_object_id == b.music_folder_object_id))
            }
            (a, b) => a.is_none() && b.is_none(),
        }
    }
}

pub enum SourceEvent {
    /// A server refused a favorite, so the source's own favorite shows again.
    FavoriteRejected,
    PlaylistSourceMismatch(sources::SourceId),
}

pub struct VisualizerPublication {
    pub run: playback::RunId,
    /// Linear amplitudes on the canonical ERB axis; map through the current visualizer preset.
    pub levels: Vec<f64>,
}

pub struct RuntimeInputs {
    pub temporary_store: bool,
    pub secret_storage_fallbacks: async_channel::Receiver<crate::settings::KeyringSecretStore>,
    pub diagnostics: DiagnosticsHandle,
    pub products: ProductHandles,
    pub receivers: ProductReceivers,
    pub release_history: super::ReleaseHistory,
}
