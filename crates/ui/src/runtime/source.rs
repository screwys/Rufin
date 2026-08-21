//! User intent and presentation for configured music sources.
//!
//! Provider configuration and lifecycle policy stay in Rufin and Sources. UI
//! sees only form values, settings-derived summaries, and one operation state.

use std::path::PathBuf;
use std::sync::Arc;

use async_channel::Receiver;
pub use library::LocalAccessStatus;
use library::{
    FavoriteItemId, FolderContents, FolderId, HomeSectionKind, MetadataDraft, MetadataEdit,
    MetadataError, MetadataItemId, MetadataValues, MusicFolderId, PlaylistEdit, PlaylistTrackAdd,
    SearchRequest as LibrarySearchRequest, SearchResults, SmartPlaylistBuiltin,
    SmartPlaylistDefinition, SmartPlaylistId, SourceId,
};
use secrets::SecretStorageMode;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSummary {
    pub id: SourceId,
    pub kind: String,
    pub name: String,
    pub transcoded_download_bitrate_limit_kbps: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalFolder {
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLocalAccess {
    pub source_id: SourceId,
    pub root_path: PathBuf,
    pub server_prefix: Option<String>,
    pub local_prefix: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLocalAccessSummary {
    pub source_id: SourceId,
    pub access: Option<SourceLocalAccess>,
    pub status: LocalAccessStatus,
    pub selected_music_folder_name: Option<String>,
    pub album_count: usize,
    pub track_count: usize,
}

#[derive(Clone, Debug, Default)]
pub struct ConfiguredSources {
    pub sources: Arc<[SourceSummary]>,
    pub selected_source_id: Option<SourceId>,
    pub local_folders: Arc<[LocalFolder]>,
    pub local_access: Arc<[SourceLocalAccessSummary]>,
    pub first_run: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenSubsonicKind {
    Navidrome,
    OpenSubsonic,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OpenSubsonicAuthentication {
    #[default]
    Password,
    ApiKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialInput {
    pub source_name: Option<String>,
    pub server_url: String,
    pub username: String,
    pub secret: String,
    pub trust_invalid_cert: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceSetup {
    Jellyfin {
        credentials: CredentialInput,
        use_instant_mix: bool,
    },
    OpenSubsonic {
        kind: OpenSubsonicKind,
        authentication: OpenSubsonicAuthentication,
        credentials: CredentialInput,
    },
    Local {
        roots: Vec<PathBuf>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialPreset {
    pub source_name: String,
    pub server_url: String,
    pub username: String,
    pub trust_invalid_cert: bool,
    pub open_subsonic_authentication: Option<OpenSubsonicAuthentication>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditableSource {
    pub source: SourceSummary,
    pub credentials: CredentialPreset,
    pub jellyfin_use_instant_mix: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceSettingsChange {
    Jellyfin {
        source_id: SourceId,
        credentials: CredentialInput,
        use_instant_mix: bool,
    },
    OpenSubsonic {
        source_id: SourceId,
        kind: OpenSubsonicKind,
        authentication: OpenSubsonicAuthentication,
        credentials: CredentialInput,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceProgressStage {
    Connecting,
    Albums,
    Tracks,
    Artists,
    Genres,
    Playlists,
    Home,
    Artwork,
    Files,
    Finalizing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceProgress {
    pub stage: SourceProgressStage,
    pub completed: usize,
    pub total: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceOperation {
    Idle,
    Adding {
        progress: SourceProgress,
    },
    Switching {
        target: SourceId,
        progress: SourceProgress,
    },
    Refreshing {
        source_id: SourceId,
        progress: SourceProgress,
    },
    Failed {
        source_id: Option<SourceId>,
        message: String,
        add_form: bool,
    },
}

impl SourceOperation {
    pub fn blocks_library(&self) -> bool {
        matches!(self, Self::Adding { .. } | Self::Switching { .. })
    }

    pub fn add_form_active(&self) -> bool {
        matches!(
            self,
            Self::Adding { .. } | Self::Failed { add_form: true, .. }
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredServer {
    pub name: String,
    pub address: String,
    pub id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryStatus {
    Idle,
    Searching,
    Empty,
    Found(u64),
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryUpdate {
    pub servers: Arc<[DiscoveredServer]>,
    pub status: DiscoveryStatus,
}

pub trait SourcePort: Send + Sync {
    fn configured_source(&self, source_id: &SourceId) -> Result<Option<EditableSource>, String>;
    fn discover_servers(&self);
    fn configure_source(&self, input: SourceSetup);
    fn update_source(&self, input: SourceSettingsChange);
    fn select_source(&self, source_id: SourceId);
    fn change_secret_storage(&self, mode: SecretStorageMode) -> Receiver<Result<(), String>>;
    fn add_local_folder(&self, path: PathBuf);
    fn replace_local_folder(&self, current: String, replacement: PathBuf);
    fn remove_local_folder(&self, path: String);
    fn refresh_source(&self, source_id: SourceId);
    fn check_for_source_changes(&self);
    fn save_local_access(&self, input: SourceLocalAccess) -> Receiver<Result<(), String>>;
    fn clear_local_access(&self, source_id: SourceId);
    fn forget_source(&self, source_id: SourceId);
}

/// Commands whose validity is owned by one selected source session.
///
/// Rufin embeds this handle in the corresponding [`SelectedLibrary`](super::SelectedLibrary), so
/// callers cannot pair an operation with a different source ID or session.
pub trait SelectedSourcePort: Send + Sync {
    fn selected_library_revealed(&self);
    fn refresh_library(&self);
    fn refresh_home(&self, kind: HomeSectionKind);
    fn set_music_folder(&self, folder_id: Option<MusicFolderId>);
    fn set_favorite(&self, item: FavoriteItemId, favorite: bool);
    fn set_rating(&self, item: FavoriteItemId, rating: Option<u8>);
    fn add_playlist_tracks(&self, request: PlaylistTrackAdd) -> usize;
    fn edit_playlist(&self, edit: PlaylistEdit);
    fn folder(
        &self,
        folder_id: Option<FolderId>,
        music_folder_id: Option<MusicFolderId>,
    ) -> Receiver<Result<FolderContents, String>>;
    /// Dedicated live query route.
    fn search(&self, request: LibrarySearchRequest) -> Receiver<Result<SearchResults, String>>;
    fn metadata_editing_available(&self, item_id: &MetadataItemId) -> bool;
    fn metadata(&self, item_id: MetadataItemId) -> Receiver<Result<MetadataDraft, MetadataError>>;
    fn edit_metadata(&self, edit: MetadataEdit) -> Receiver<Result<(), MetadataError>>;
    fn identify_metadata(
        &self,
        item_id: MetadataItemId,
        editing: library::MetadataEditing,
        values: MetadataValues,
    ) -> Receiver<Result<Option<library::MetadataIdentification>, String>>;
    fn save_metadata_local_access(
        &self,
        input: SourceLocalAccess,
        item_id: MetadataItemId,
    ) -> Receiver<Result<(), String>>;
    fn create_smart_playlist(&self, name: String, definition: SmartPlaylistDefinition);
    fn update_smart_playlist(
        &self,
        id: SmartPlaylistId,
        name: String,
        definition: SmartPlaylistDefinition,
    );
    fn delete_smart_playlist(&self, id: SmartPlaylistId);
    fn restore_builtin_smart_playlist(&self, builtin: SmartPlaylistBuiltin);
    fn move_smart_playlist(&self, dragged: SmartPlaylistId, target: SmartPlaylistId, after: bool);
}

pub type SourceHandle = Arc<dyn SourcePort>;
pub type SelectedSourceHandle = Arc<dyn SelectedSourcePort>;

#[cfg(test)]
mod tests {
    use super::*;

    fn progress() -> SourceProgress {
        SourceProgress {
            stage: SourceProgressStage::Connecting,
            completed: 0,
            total: None,
        }
    }

    #[test]
    fn adding_and_switching_gate_the_selected_library() {
        assert!(
            SourceOperation::Adding {
                progress: progress()
            }
            .blocks_library()
        );
        assert!(
            SourceOperation::Switching {
                target: SourceId::new("target"),
                progress: progress(),
            }
            .blocks_library()
        );
        assert!(
            !SourceOperation::Refreshing {
                source_id: SourceId::new("selected"),
                progress: progress(),
            }
            .blocks_library()
        );
    }
}
