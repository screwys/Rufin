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

use sources::{CredentialPreset, SourceId, SourceProgress, SourceProgressStage};

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
    pub half_stars_enabled: bool,
    /// Whether a playlist can belong to this source, so the owner choice shows; file and peer
    /// sources make Rufin playlists.
    pub owns_playlists: bool,
    pub supports_playlist_public: bool,
    /// Rufin reads the files itself: albums come from tags and plays from Rufin's listens.
    pub file_library: bool,
    /// Whether half stars are a setting of this source.
    pub offers_half_star_setting: bool,
    /// Whether lyrics can be kept beside the music files: a Local source, or a Navidrome or
    /// Subsonic server with a folder mapping.
    pub stores_lyrics_locally: bool,
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

/// Whether the track a URI names, or the given or selected source, rates in half stars.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn source_half_stars_enabled(
    sources: ConfiguredSources,
    media_uri: String,
    source_id: Option<String>,
) -> bool {
    sources.half_stars_enabled(&media_uri, source_id.as_deref())
}

/// Whether lyrics search offers to keep server lyrics: automatic lyrics downloads are off, the
/// offer was not dismissed and a source keeps lyrics on a server.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn shows_lyrics_cache_prompt(
    application: crate::settings::ApplicationPreferences,
    sources: Vec<SourceSummary>,
) -> bool {
    !application.download_lyrics_automatically
        && !application.lyrics_cache_prompt_dismissed
        && sources.iter().any(|source| {
            library::SourceKind::parse(&source.kind) != Some(library::SourceKind::Local)
        })
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

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOperation {
    /// Whether the selected library is being replaced: pages that list a source wait for it.
    pub fn blocks_library(&self) -> bool {
        matches!(self, Self::Adding { .. } | Self::Switching { .. })
    }
}

pub fn source_progress_text(progress: &SourceProgress) -> String {
    let stage = match progress.stage {
        SourceProgressStage::Connecting => {
            return localization::tr("Connecting to music server...");
        }
        SourceProgressStage::Finalizing => return localization::tr("Preparing library..."),
        SourceProgressStage::Albums => localization::tr("Fetching albums"),
        SourceProgressStage::Tracks => localization::tr("Scanning tracks"),
        SourceProgressStage::Artists => localization::tr("Fetching artists"),
        SourceProgressStage::Genres => localization::tr("Fetching genres"),
        SourceProgressStage::Playlists => localization::tr("Fetching playlists"),
        SourceProgressStage::Home => localization::tr("Fetching Home"),
        SourceProgressStage::Artwork => localization::tr("Fetching artwork"),
        SourceProgressStage::Lyrics => localization::tr("Fetching lyrics"),
    };
    let completed = progress.completed.to_string();
    match progress.total {
        Some(total) if total > 0 => localization::tr_with(
            "{stage}: {completed} / {total}",
            &[
                ("stage", &stage),
                ("completed", &completed),
                ("total", &total.to_string()),
            ],
        ),
        _ => localization::tr_with(
            "{stage}: {completed}",
            &[("stage", &stage), ("completed", &completed)],
        ),
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

/// A server that answered on the local network, with the source kind that adds it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FoundServer {
    pub kind: String,
    pub name: String,
    pub address: String,
}

/// The user's music folder, and whether this process can read it. A sandbox knows the path
/// without having the folder, and the system's folder dialog then grants it.
pub fn music_folder() -> Option<(std::path::PathBuf, bool)> {
    let user_dirs = directories::UserDirs::new()?;
    let folder = user_dirs
        .audio_dir()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| user_dirs.home_dir().join("Music"));
    let readable = std::fs::read_dir(&folder).is_ok();
    Some((folder, readable))
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

#[derive(Clone, Copy)]
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

/// The lines under a source's name in the source list: its provider and address, account and
/// music folder, then its cache and local file mapping. A Local source counts its folders.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn source_row_subtitle(
    source: SourceSummary,
    access: Option<SourceLocalAccessSummary>,
    credentials: Option<CredentialPreset>,
    status: Option<LocalAccessStatus>,
    local_folder_count: u64,
) -> String {
    use localization::{tr, tr_with, trn_with};
    let line = |parts: [String; 2]| {
        parts
            .into_iter()
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" - ")
    };
    let cache = access.as_ref().map_or_else(String::new, |access| {
        format!(
            "{}: {}, {}",
            tr("Cached"),
            localization::album_count_text(access.album_count as u64),
            localization::track_count_text(access.track_count as u64)
        )
    });
    let lines = if source.kind == "local" {
        let count = local_folder_count.to_string();
        [
            tr_with("Configured items: {count}", &[("count", &count)]),
            String::new(),
            cache,
        ]
    } else {
        let mapping = match (
            access.as_ref().and_then(|access| access.access.as_ref()),
            status,
        ) {
            (None, _) => tr("No local file mapping"),
            (Some(_), Some(status)) if status.total_track_count > 0 => {
                let matched = status.matched_track_count.to_string();
                let total = status.total_track_count.to_string();
                trn_with(
                    "Local file mapping: {matched} of {total} server path matches",
                    "Local file mapping: {matched} of {total} server paths match",
                    status.total_track_count as u64,
                    &[("matched", &matched), ("total", &total)],
                )
            }
            (Some(_), _) => tr("Local playback saved"),
        };
        let provider = source_kind_title(&source.kind).map_or_else(|| source.kind.clone(), tr);
        let (address, user) = credentials.map_or_else(Default::default, |credentials| {
            let user = Some(credentials.username)
                .filter(|username| !username.trim().is_empty())
                .map(|username| format!("{}: {username}", tr("User")))
                .unwrap_or_default();
            (credentials.server_url, user)
        });
        let folder = access
            .and_then(|access| access.selected_music_folder_name)
            .unwrap_or_else(|| tr("All Music"));
        [
            line([provider, address]),
            line([user, format!("{}: {folder}", tr("Music Folder"))]),
            line([cache, mapping]),
        ]
    };
    lines
        .into_iter()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn source_kind_title(kind: &str) -> Option<&'static str> {
    use library::SourceKind;
    Some(match SourceKind::parse(kind)? {
        SourceKind::Jellyfin => localization::msgid("Jellyfin"),
        SourceKind::Emby => localization::msgid("Emby"),
        SourceKind::Plex => localization::msgid("Plex"),
        SourceKind::Navidrome => localization::msgid("Navidrome"),
        SourceKind::Subsonic => localization::msgid("OpenSubsonic"),
        SourceKind::Local => localization::msgid("Local"),
        SourceKind::Webdav => localization::msgid("WebDAV"),
        SourceKind::Smb => localization::msgid("SMB / Samba"),
        SourceKind::Peer => localization::msgid("Rufin Connect"),
    })
}

pub fn source_kind_icon_name(kind: &str) -> Option<&'static str> {
    use library::SourceKind;
    Some(match SourceKind::parse(kind)? {
        SourceKind::Jellyfin => "io.github.screwys.Rufin.source.jellyfin",
        SourceKind::Emby => "io.github.screwys.Rufin.source.emby",
        SourceKind::Plex => "io.github.screwys.Rufin.source.plex",
        SourceKind::Navidrome => "io.github.screwys.Rufin.source.navidrome",
        SourceKind::Subsonic => "io.github.screwys.Rufin.source.opensubsonic",
        SourceKind::Local => "io.github.screwys.Rufin-symbolic",
        SourceKind::Webdav => "io.github.screwys.Rufin.source.webdav",
        SourceKind::Smb => "io.github.screwys.Rufin.source.smb",
        SourceKind::Peer => "rufin-phonelink-symbolic",
    })
}
