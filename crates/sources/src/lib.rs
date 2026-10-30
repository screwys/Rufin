//! Native music providers and file access.
//!
//! Providers own authentication, HTTP or filesystem work, paging, wire
//! translation, and bounded Library updates. Rufin owns source lifecycle,
//! artwork preparation, and publication of committed catalog changes.

#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!();

#[cfg(test)]
extern crate self as sources;

mod download;
pub use download::{DownloadQuality, ResolvedDownload, downloaded_codec};
mod peer;
pub use peer::{PEER_REVISION_HEADER, PeerAccess, PeerAudio, PeerMedia, peer_revision};
mod config;
mod detail_links;
pub mod discovery;
mod operations;
mod plex;
mod policy;
mod source;

mod file;
mod jellyfin_emby;
pub use jellyfin_emby::ServerKind;
mod remote_http;
mod remote_json;
mod subsonic;

/// What a test's Catalog committed since the last call, merged as the one publication clients
/// would see: the first source whose shown rows changed, with everything it changed.
#[cfg(test)]
pub(crate) fn published(
    changes: &mut tokio::sync::broadcast::Receiver<std::sync::Arc<library::Change>>,
) -> Option<(library::SourceKey, Shown)> {
    let mut published: Option<(library::SourceKey, Shown)> = None;
    let mut unsourced = Shown::default();
    while let Ok(committed) = changes.try_recv() {
        for (source, part) in Shown::of(&committed) {
            let Some(source) = source else {
                unsourced |= part;
                continue;
            };
            let (first, merged) = published.get_or_insert((source, Shown::default()));
            if *first == source {
                *merged |= part;
            }
        }
    }
    if let Some((_, merged)) = &mut published {
        *merged |= unsourced;
    }
    published
}

/// Every row of a list, read in the windows a client scrolls through.
#[cfg(test)]
pub(crate) async fn listed(catalog: &library::Catalog, read: library::Read) -> Vec<library::Row> {
    let cancel = library::ReadCancellation::new();
    let order = catalog.order(&read, &cancel).await.unwrap();
    let mut rows = Vec::new();
    for start in (0..order.len()).step_by(128) {
        rows.extend(
            catalog
                .rows(&order, start..(start + 128).min(order.len()), true, &cancel)
                .await
                .unwrap(),
        );
    }
    rows
}

/// The track rows of a member list.
#[cfg(test)]
pub(crate) async fn listed_tracks(
    catalog: &library::Catalog,
    read: library::MemberRead,
) -> Vec<library::TrackRow> {
    listed(catalog, library::Read::Members(read))
        .await
        .into_iter()
        .filter_map(|row| match row {
            library::Row::Track(track) | library::Row::AlbumTrack { track, .. } => Some(track),
            _ => None,
        })
        .collect()
}

/// The album rows of a list.
#[cfg(test)]
pub(crate) async fn listed_albums(
    catalog: &library::Catalog,
    read: library::ListRead,
) -> Vec<library::AlbumRow> {
    listed(catalog, library::Read::List(read))
        .await
        .into_iter()
        .filter_map(|row| match row {
            library::Row::Album(album) => Some(album),
            _ => None,
        })
        .collect()
}

/// The artist rows of a list.
#[cfg(test)]
pub(crate) async fn listed_artists(
    catalog: &library::Catalog,
    read: library::ListRead,
) -> Vec<library::ArtistRow> {
    listed(catalog, library::Read::List(read))
        .await
        .into_iter()
        .filter_map(|row| match row {
            library::Row::Artist(artist) => Some(artist),
            _ => None,
        })
        .collect()
}

/// The playlist rows of a list.
#[cfg(test)]
pub(crate) async fn listed_playlists(
    catalog: &library::Catalog,
    read: library::ListRead,
) -> Vec<library::PlaylistRow> {
    listed(catalog, library::Read::List(read))
        .await
        .into_iter()
        .filter_map(|row| match row {
            library::Row::Playlist(library::PlaylistListRow::Playlist(playlist)) => Some(playlist),
            _ => None,
        })
        .collect()
}

/// The playable of a track by its URI.
#[cfg(test)]
pub(crate) async fn playable(catalog: &library::Catalog, uri: &str) -> library::Playable {
    let key = catalog
        .track_keys(&[uri.to_owned()], &library::ReadCancellation::new())
        .await
        .unwrap()[0];
    catalog.playable(key, "original").await.unwrap().unwrap()
}

/// Every stored file row of a source, in walk order.
#[cfg(test)]
pub(crate) async fn stored_files(
    catalog: &library::Catalog,
    source: &str,
) -> Vec<library::LocalFileWrite> {
    catalog
        .observe(source)
        .await
        .unwrap()
        .file_rows(&[], &[], None, false, 8192)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.file)
        .collect()
}

/// The key of a source playlist by its object id.
#[cfg(test)]
pub(crate) async fn playlist_key(
    catalog: &library::Catalog,
    source: library::SourceKey,
    object: &str,
) -> Option<library::PlaylistKey> {
    catalog
        .catalog_keys(
            library::CatalogKind::Playlist,
            Some(source),
            &[object.to_owned()],
            &library::ReadCancellation::new(),
        )
        .await
        .unwrap()
        .first()
        .map(|key| library::PlaylistKey::from_raw(*key))
}

#[cfg(test)]
mod shown {
    /// What clients refresh for after a test's committed Writes: the catalog rows of a source,
    /// its playlists, its plays, its Home rows or favorites and ratings.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct Shown(u8);

    impl Shown {
        pub const CATALOG: Self = Self(1);
        pub const PLAYLIST: Self = Self(1 << 1);
        pub const STATS: Self = Self(1 << 2);
        pub const HOME: Self = Self(1 << 3);
        pub const STATE: Self = Self(1 << 4);

        pub fn intersects(self, other: Self) -> bool {
            self.0 & other.0 != 0
        }

        pub fn contains(self, other: Self) -> bool {
            self.0 & other.0 == other.0
        }

        pub fn is_empty(self) -> bool {
            self.0 == 0
        }

        /// The parts of one Change, by the source they belong to.
        pub fn of(change: &library::Change) -> Vec<(Option<library::SourceKey>, Self)> {
            use library::Scope;
            change
                .scopes
                .iter()
                .filter_map(|scope| match scope {
                    Scope::Tracks { source, .. } => Some((*source, Self::CATALOG)),
                    Scope::Collections { source } | Scope::Source { key: source, .. } => {
                        Some((Some(*source), Self::CATALOG))
                    }
                    Scope::Playlist { .. }
                    | Scope::Playlists
                    | Scope::Smart { .. }
                    | Scope::Folders => Some((None, Self::PLAYLIST)),
                    Scope::Listened { .. } => Some((None, Self::STATS)),
                    Scope::Home { source } => Some((Some(*source), Self::HOME)),
                    Scope::State { .. } => Some((None, Self::STATE)),
                    Scope::Reload => Some((None, Self::CATALOG)),
                    _ => None,
                })
                .collect()
        }
    }

    impl std::ops::BitOr for Shown {
        type Output = Self;

        fn bitor(self, other: Self) -> Self {
            Self(self.0 | other.0)
        }
    }

    impl std::ops::BitOrAssign for Shown {
        fn bitor_assign(&mut self, other: Self) {
            self.0 |= other.0;
        }
    }
}
#[cfg(test)]
pub use shown::Shown;

#[cfg(test)]
mod local_change_integration_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/local_changes.rs"
    ));
}

pub use config::{
    CredentialInput, CredentialPreset, EditableSource, PlexSettingsInput, PlexSetupInput,
    SourceConfiguration, SourceSettingsChange, SourceSetup,
};
pub use file::discovery::DiscoveryWorker;
pub use file::discovery::run_worker as run_discovery_worker;
#[cfg(target_os = "android")]
pub use file::discovery::{DiscoveryHost, DiscoverySession, install_discovery_host};
pub use file::documents::{
    DocumentEntry, DocumentHost, DocumentPage, DocumentReader, DocumentRoot, copy_document,
    create_document, create_document_directories, create_document_directory, delete_document,
    document_identity, document_root, find_document, find_document_child, install_document_host,
    list_documents, open_document_input, rename_document, resolve_document_relative,
    resolve_document_stream, save_document, stat_document,
};
pub use file::documents::{
    read_document_image, read_document_prefix, read_document_track_reference,
};
pub use file::local::media::{local_media_identity_path, read_local_track_reference};
pub use file::metadata::read_embedded_lyrics;
pub use file::playlist::{
    import_local_playlist_file, playlist_file_is_non_audio, playlist_file_revision,
};
pub use file::remote::smb::list_smb_shares;
pub use file::remote::webdav::nextcloud::authorize_nextcloud;
pub use file::remote::{
    FileAuthentication, FileCredentials, FileCredentialsEdit, FileHeader, FileSource,
    FileSourceSettings,
};
pub use jellyfin_emby::{
    EmbyConnectLogin, EmbyConnectPin, EmbyConnectServer, JellyfinQuickConnect,
    JellyfinQuickConnectLogin,
};
pub use library::SourceId;
pub use operations::{
    ArtworkChange, ArtworkEdit, ArtworkEditing, ArtworkStorage, ImageBytes, LyricsStorage,
    MetadataChanges, MetadataDraft, MetadataEdit, MetadataField, MetadataFieldKind,
    SourceMetadataError, artwork_file, finish_metadata_save,
};
pub use source::*;

pub use discovery::{DiscoveredServer, DiscoveryProvider, discover_servers};
pub use file::local::{LOCAL_LIBRARY_SOURCE_ID, LOCAL_SOURCE_ID, read_local_image};
pub use plex::{
    PLEX_QUEUE_WINDOW, PlexCompanionContext, PlexCompanionPlayer, PlexQueueItem, PlexQueueMutation,
    PlexQueuePlacement, PlexQueueWindow, plex_queue_write_batches,
};
pub use plex::{PlexBrowserLogin, PlexConnection, PlexLogin, PlexProfile, PlexServer};
pub use subsonic::{SubsonicAuthentication, SubsonicFlavor};

use thiserror::Error;

#[derive(Debug, Error)]
#[cfg_attr(feature = "bindings", derive(uniffi::Error))]
#[cfg_attr(feature = "bindings", uniffi(flat_error))]
pub enum SourceError {
    #[error(transparent)]
    Library(#[from] library::LibraryError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("source authentication failed: {0}")]
    Auth(String),
    #[error("source TLS validation failed: {0}")]
    Tls(String),
    #[error("source network failed: {0}")]
    Network(String),
    #[error("source server failed with status {status}: {message}")]
    Server { status: u16, message: String },
    #[error("source item was not found")]
    NotFound,
    #[error("source request is invalid: {0}")]
    InvalidRequest(&'static str),
    #[error("source operation was cancelled")]
    Cancelled,
    #[error("saved source configuration is invalid: {0}")]
    InvalidConfig(String),
    #[error("source failed: {0}")]
    Other(String),
}

pub type SourceResult<T> = Result<T, SourceError>;
