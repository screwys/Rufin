//! User intent and presentation for configured music sources.
//!
//! Provider configuration and lifecycle policy stay in Rufin and Sources. UI
//! sees only form values, settings-derived summaries, and one operation state.

use std::path::PathBuf;
use std::sync::Arc;

use sources::SourceId;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalAccessStatus {
    pub total_track_count: usize,
    pub matched_track_count: usize,
    pub sample_source_path: Option<String>,
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

#[derive(Clone, Debug, Default)]
pub struct ConfiguredSources {
    pub sources: Arc<[SourceSummary]>,
    pub selected_source_id: Option<SourceId>,
    pub local_folders: Arc<[LocalFolder]>,
    pub local_access: Arc<[SourceLocalAccessSummary]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenSubsonicKind {
    Navidrome,
    OpenSubsonic,
}

pub use sources::SubsonicAuthentication as OpenSubsonicAuthentication;

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
pub struct CredentialInput {
    pub source_name: Option<String>,
    pub server_url: String,
    pub username: String,
    pub secret: String,
    pub trust_invalid_cert: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum SourceSetup {
    EmbyConnect {
        server: sources::EmbyConnectServer,
        source_name: Option<String>,
        use_instant_mix: bool,
    },
    JellyfinQuickConnect {
        login: sources::JellyfinQuickConnectLogin,
        source_name: Option<String>,
        use_instant_mix: bool,
    },
    Plex(sources::PlexSetupInput),
    WebDav {
        name: String,
        settings: sources::FileSourceSettings,
        credentials: sources::FileCredentials,
    },
    Smb {
        name: String,
        settings: sources::FileSourceSettings,
        credentials: sources::FileCredentials,
    },
    JellyfinEmby {
        kind: sources::ServerKind,
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
    pub plex_settings: Option<sources::PlexSettingsInput>,
    pub file_settings: Option<sources::FileSourceSettings>,
    pub source: SourceSummary,
    pub credentials: CredentialPreset,
    pub use_instant_mix: Option<bool>,
    pub emby_connect: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum SourceSettingsChange {
    EmbyConnect {
        source_id: SourceId,
        server: sources::EmbyConnectServer,
        source_name: Option<String>,
        use_instant_mix: bool,
    },
    JellyfinQuickConnect {
        source_id: SourceId,
        login: sources::JellyfinQuickConnectLogin,
        source_name: Option<String>,
        use_instant_mix: bool,
    },
    Plex {
        source_id: SourceId,
        settings: sources::PlexSettingsInput,
    },
    Files {
        source_id: SourceId,
        name: String,
        settings: sources::FileSourceSettings,
        credentials: sources::FileCredentialsEdit,
    },
    JellyfinEmby {
        connect_manually: bool,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct SourceProgress {
    pub stage: SourceProgressStage,
    pub completed: usize,
    pub total: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
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
}

pub use sources::{DiscoveredServer, DiscoveryProvider};

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
    pub provider: DiscoveryProvider,
    pub servers: Arc<[DiscoveredServer]>,
    pub status: DiscoveryStatus,
}

#[derive(serde::Serialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum NextcloudLoginEvent {
    OpenBrowser(String),
    Authorized {
        settings: sources::FileSourceSettings,
        credentials: sources::FileCredentials,
    },
}

#[derive(serde::Deserialize)]
#[serde(tag = "method", content = "data", rename_all = "snake_case")]
pub enum EmbyConnectLoginMethod {
    Password { username: String, password: String },
    Pin,
}

#[derive(serde::Serialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum EmbyConnectLoginEvent {
    Code { code: String, url: String },
    Servers(Vec<sources::EmbyConnectServer>),
}

#[derive(serde::Serialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum JellyfinQuickConnectEvent {
    Code { code: String, url: String },
    Authorized(sources::JellyfinQuickConnectLogin),
}

#[derive(serde::Deserialize)]
#[serde(tag = "method", content = "data", rename_all = "snake_case")]
pub enum PlexLoginMethod {
    Browser,
    Password {
        username: String,
        password: String,
        verification_code: Option<String>,
    },
}

#[derive(serde::Serialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum PlexLoginEvent {
    OpenBrowser(String),
    Authorized(Box<sources::PlexLogin>),
}

pub enum PlaylistExport {
    Playlist(library::PlaylistKey),
    Smart(library::SmartPlaylistKey),
}

pub type SourceHandle = Arc<crate::source::SourceOwner>;

/// Commands whose validity is owned by one selected source session.
///
/// Rufin embeds this handle in the corresponding [`SelectedLibrary`](super::SelectedLibrary), so
/// callers cannot pair an operation with a different source or session.
pub type SelectedSourceHandle = Arc<crate::source::ActiveSource>;

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
