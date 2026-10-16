//! User intent and presentation for configured music sources.
//!
//! Provider configuration and lifecycle policy stay in Rufin and Sources. UI
//! sees only form values, settings-derived summaries, and one operation state.

use std::path::PathBuf;

#[cfg(feature = "bindings")]
uniffi::custom_type!(PathBuf, String, {
    remote,
    lower: |path| path.to_string_lossy().into_owned(),
    try_lift: |path| Ok(PathBuf::from(path)),
});
use std::sync::Arc;

use sources::{CredentialPreset, SourceId};

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LocalAccessStatus {
    pub total_track_count: usize,
    pub matched_track_count: usize,
    pub sample_source_path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SourceSummary {
    pub id: SourceId,
    pub kind: String,
    pub name: String,
    pub transcoded_download_bitrate_limit_kbps: Option<u32>,
    pub half_stars_enabled: bool,
    pub supports_playlist_public: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LocalFolder {
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SourceLocalAccess {
    pub source_id: SourceId,
    pub root_path: PathBuf,
    pub server_prefix: Option<String>,
    pub local_prefix: Option<String>,
    pub sample_source_path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SourceLocalAccessSummary {
    pub source_id: SourceId,
    pub access: Option<SourceLocalAccess>,
    pub selected_music_folder_name: Option<String>,
    pub album_count: usize,
    pub track_count: usize,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConfiguredSources {
    pub sources: Vec<SourceSummary>,
    pub selected_source_id: Option<SourceId>,
    pub local_folders: Vec<LocalFolder>,
    pub local_excluded_folders: Vec<PathBuf>,
    pub local_access: Vec<SourceLocalAccessSummary>,
    pub document_roots: Vec<sources::DocumentRoot>,
}

impl ConfiguredSources {
    pub fn half_stars_enabled(&self, media_uri: &str, source_id: Option<&str>) -> bool {
        let uri_source = library::source_entity_parts(media_uri).map(|(source, _, _)| source);
        let source_id = source_id
            .or_else(|| uri_source.as_ref().map(SourceId::as_str))
            .or_else(|| self.selected_source_id.as_ref().map(SourceId::as_str));
        self.sources
            .iter()
            .find(|source| Some(source.id.as_str()) == source_id)
            .is_some_and(|source| source.half_stars_enabled)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct EditableSource {
    pub integration: bool,
    pub plex_settings: Option<sources::PlexSettingsInput>,
    pub file_settings: Option<sources::FileSourceSettings>,
    pub source: SourceSummary,
    pub credentials: CredentialPreset,
    pub use_instant_mix: Option<bool>,
    pub emby_connect: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SourceProgressStage {
    Connecting,
    Albums,
    Tracks,
    Artists,
    Genres,
    Playlists,
    Home,
    Artwork,
    Lyrics,
    Files,
    Finalizing,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SourceProgress {
    pub stage: SourceProgressStage,
    pub completed: usize,
    pub total: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
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
    Downloading {
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
}

pub fn source_progress_text(progress: &SourceProgress) -> String {
    let subject = match progress.stage {
        SourceProgressStage::Connecting => {
            return localization::tr("Connecting to music server...");
        }
        SourceProgressStage::Albums => "albums",
        SourceProgressStage::Tracks => "tracks",
        SourceProgressStage::Artists => "artists",
        SourceProgressStage::Genres => "genres",
        SourceProgressStage::Playlists => "playlists",
        SourceProgressStage::Home => "Home",
        SourceProgressStage::Artwork => "artwork",
        SourceProgressStage::Lyrics => "lyrics",
        SourceProgressStage::Files => "files",
        SourceProgressStage::Finalizing => return localization::tr("Preparing library..."),
    };
    match progress.total {
        Some(total) => format!(
            "Fetching {subject}, {}/{total} fetched...",
            progress.completed
        ),
        None if progress.completed > 0 => {
            format!("Fetching {subject}, {} fetched...", progress.completed)
        }
        None => format!("Fetching {subject}..."),
    }
}

pub fn source_download_text(progress: &SourceProgress) -> String {
    let message = match progress.stage {
        SourceProgressStage::Artwork => {
            localization::msgid("Downloading artwork... {count} checked")
        }
        SourceProgressStage::Lyrics => localization::msgid("Downloading lyrics... {count} checked"),
        _ => return source_progress_text(progress),
    };
    localization::tr_with(message, &[("count", &progress.completed.to_string())])
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn source_operation_text(operation: &SourceOperation) -> Option<String> {
    match operation {
        SourceOperation::Idle => None,
        SourceOperation::Downloading { progress, .. } => Some(source_download_text(progress)),
        SourceOperation::Adding { progress } | SourceOperation::Refreshing { progress, .. } => {
            Some(source_progress_text(progress))
        }
        SourceOperation::Switching { .. } => Some(localization::tr("Switching library...")),
        SourceOperation::Failed { message, .. } => Some(message.clone()),
    }
}

pub use sources::{DiscoveredServer, DiscoveryProvider};

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DiscoveryStatus {
    Idle,
    Searching,
    Empty,
    Found(u64),
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DiscoveryUpdate {
    pub provider: DiscoveryProvider,
    pub servers: Vec<DiscoveredServer>,
    pub status: DiscoveryStatus,
}

#[derive(serde::Serialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum SourceLoginEvent {
    Code {
        code: String,
        url: String,
    },
    OpenBrowser(String),
    Servers(Vec<sources::EmbyConnectServer>),
    #[serde(rename = "authorized")]
    Jellyfin(sources::JellyfinQuickConnectLogin),
    #[serde(rename = "authorized")]
    Plex(Box<sources::PlexLogin>),
    #[serde(rename = "authorized")]
    Files {
        settings: sources::FileSourceSettings,
        credentials: sources::FileCredentials,
    },
}

#[derive(serde::Deserialize)]
#[serde(tag = "method", content = "data", rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum EmbyConnectLoginMethod {
    Password { username: String, password: String },
    Pin,
}

#[derive(serde::Deserialize)]
#[serde(tag = "method", content = "data", rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum PlexLoginMethod {
    Browser,
    Password {
        username: String,
        password: String,
        verification_code: Option<String>,
    },
}

pub enum PlaylistExport {
    Playlist(library::PlaylistKey),
    Smart(library::SmartPlaylistKey),
}

pub type SourceHandle = Arc<crate::source::SourceOwner>;

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

pub fn source_kind_title(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "jellyfin" => localization::msgid("Jellyfin"),
        "emby" => localization::msgid("Emby"),
        "plex" => localization::msgid("Plex"),
        "navidrome" => localization::msgid("Navidrome"),
        "subsonic" => localization::msgid("OpenSubsonic"),
        "local" => localization::msgid("Local"),
        "webdav" => localization::msgid("WebDAV"),
        "smb" => localization::msgid("SMB / Samba"),
        _ => return None,
    })
}

pub fn source_kind_icon_name(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "jellyfin" => "io.github.screwys.Rufin.source.jellyfin",
        "emby" => "io.github.screwys.Rufin.source.emby",
        "plex" => "io.github.screwys.Rufin.source.plex",
        "navidrome" => "io.github.screwys.Rufin.source.navidrome",
        "subsonic" => "io.github.screwys.Rufin.source.opensubsonic",
        "local" => "io.github.screwys.Rufin-symbolic",
        "webdav" => "io.github.screwys.Rufin.source.webdav",
        "smb" => "io.github.screwys.Rufin.source.smb",
        _ => return None,
    })
}
