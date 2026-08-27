//! User intent and presentation for configured music sources.
//!
//! Provider configuration and lifecycle policy stay in Rufin and Sources. UI
//! sees only form values, settings-derived summaries, and one operation state.

use std::path::PathBuf;
use std::sync::Arc;

use async_channel::Receiver;
use library::{AlbumKey, ArtistKey, FavoriteTarget, PlaylistEntryKey, PlaylistKey, TrackKey};
use secrets::SecretStorageMode;
use sources::{
    AlbumMetadata, AlbumMetadataEdit, AlbumMetadataValues, ArtistMetadata, ArtistMetadataEdit,
    ArtistMetadataValues, LiveFolderPage, LiveSearchResults, SourceId, SourceMetadataError,
    TrackMetadata, TrackMetadataEdit, TrackMetadataValues,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalAccessStatus {
    pub total_track_count: usize,
    pub direct_match_count: usize,
    pub prefix_match_count: usize,
    pub metadata_match_count: usize,
    pub unmatched_count: usize,
    pub sample_source_path: Option<String>,
    pub sample_local_path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSummary {
    pub id: SourceId,
    pub kind: String,
    pub name: String,
    pub transcoded_download_bitrate_limit_kbps: Option<u32>,
    pub half_stars_enabled: bool,
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
    pub sample_source_path: Option<String>,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiveSearchCollectionTarget {
    Album(String),
    Artist(String),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LibraryRefreshTrigger {
    GlobalAction,
    NewlyAdded,
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
    fn set_half_stars(&self, source_id: SourceId, enabled: bool);
    fn select_source(&self, source_id: SourceId);
    fn change_secret_storage(&self, mode: SecretStorageMode) -> Receiver<Result<(), String>>;
    fn add_local_folder(&self, path: PathBuf);
    fn replace_local_folder(&self, current: String, replacement: PathBuf);
    fn remove_local_folder(&self, path: String);
    fn refresh_source(&self, source_id: SourceId);
    fn save_local_access(&self, input: SourceLocalAccess) -> Receiver<Result<(), String>>;
    fn clear_local_access(&self, source_id: SourceId);
    fn forget_source(&self, source_id: SourceId);
}

/// Commands whose validity is owned by one selected source session.
///
/// Rufin embeds this handle in the corresponding [`SelectedLibrary`](super::SelectedLibrary), so
/// callers cannot pair an operation with a different source or session.
pub trait SelectedSourcePort: Send + Sync {
    fn selected_library_revealed(&self);
    fn refresh_library(&self, trigger: LibraryRefreshTrigger);
    fn refresh_home(&self, kind: crate::settings::HomeSectionKind);
    fn set_music_folder(&self, folder_object_id: Option<String>);
    fn set_favorite(&self, target: FavoriteTarget, favorite: bool);
    fn set_rating(&self, target: FavoriteTarget, rating: Option<u8>);
    fn create_playlist(&self, name: String, tracks: Vec<TrackKey>);
    fn rename_playlist(&self, playlist: PlaylistKey, name: String);
    fn delete_playlist(&self, playlist: PlaylistKey);
    fn add_playlist_tracks(
        &self,
        playlist: PlaylistKey,
        tracks: Vec<TrackKey>,
        skip_duplicates: bool,
    ) -> usize;
    fn remove_playlist_entries(&self, playlist: PlaylistKey, entries: Vec<PlaylistEntryKey>);
    fn move_playlist_entry(&self, playlist: PlaylistKey, entry: PlaylistEntryKey, position: usize);
    fn folder(
        &self,
        folder_object_id: Option<String>,
        music_folder_object_id: Option<String>,
    ) -> Receiver<Result<LiveFolderPage, String>>;
    /// Dedicated live query route.
    fn search(&self, query: String, limit: usize) -> Receiver<Result<LiveSearchResults, String>>;
    fn play_live_search_collection(
        &self,
        target: LiveSearchCollectionTarget,
        placement: playback::QueuePlacement,
    );
    fn track_metadata(
        &self,
        track: TrackKey,
    ) -> Receiver<Result<TrackMetadata, SourceMetadataError>>;
    fn album_metadata(
        &self,
        album: AlbumKey,
    ) -> Receiver<Result<AlbumMetadata, SourceMetadataError>>;
    fn artist_metadata(
        &self,
        artist: ArtistKey,
    ) -> Receiver<Result<ArtistMetadata, SourceMetadataError>>;
    fn write_reviewed_track_metadata(
        &self,
        track: TrackKey,
        revision: Option<String>,
        application_token: Option<String>,
        edit: TrackMetadataEdit,
    ) -> Receiver<Result<(), SourceMetadataError>>;
    fn write_reviewed_album_metadata(
        &self,
        album: AlbumKey,
        revision: Option<String>,
        application_token: Option<String>,
        edit: AlbumMetadataEdit,
    ) -> Receiver<Result<(), SourceMetadataError>>;
    fn write_reviewed_artist_metadata(
        &self,
        artist: ArtistKey,
        revision: Option<String>,
        application_token: Option<String>,
        edit: ArtistMetadataEdit,
    ) -> Receiver<Result<(), SourceMetadataError>>;
    fn identify_track_metadata(
        &self,
        track: TrackKey,
        values: TrackMetadataValues,
    ) -> Receiver<Result<Option<(TrackMetadataValues, Option<String>)>, String>>;
    fn identify_album_metadata(
        &self,
        album: AlbumKey,
        values: AlbumMetadataValues,
    ) -> Receiver<Result<Option<(AlbumMetadataValues, Option<String>)>, String>>;
    fn identify_artist_metadata(
        &self,
        artist: ArtistKey,
        values: ArtistMetadataValues,
    ) -> Receiver<Result<Option<(ArtistMetadataValues, Option<String>)>, String>>;
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
