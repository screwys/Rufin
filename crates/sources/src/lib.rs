//! Concrete music sources behind one source-agnostic application boundary.
//!
//! Providers own authentication, HTTP or filesystem work, paging, wire
//! translation, and preparation of invisible Library candidates. Rufin owns
//! source lifecycle, artwork preparation, and candidate acceptance. Local
//! preparation reads one selected Library's accepted baselines; only Library
//! can accept and persist the resulting replacement.

#[cfg(test)]
extern crate self as sources;

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

#[cfg(test)]
mod local_change_integration_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/local_changes.rs"
    ));
}

pub use config::{
    CredentialHostInput, CredentialHostPreset, CredentialSettingsInput, EditableSource,
    JellyfinEmbySettingsInput, JellyfinEmbySetupInput, LocalFolderHostInput, PlexSettingsInput,
    PlexSetupInput, SourceConfiguration, SourceSettingsInput, SourceSetupInput,
};
pub use file::metadata::read_embedded_lyrics;
pub use file::remote::smb::list_smb_shares;
pub use file::remote::webdav::nextcloud::authorize_nextcloud;
pub use file::remote::{
    FileAuthentication, FileCredentials, FileCredentialsEdit, FileSourceSettings,
};
pub use jellyfin_emby::{
    EmbyConnectLogin, EmbyConnectPin, EmbyConnectServer, JellyfinQuickConnect,
    JellyfinQuickConnectLogin,
};
pub use library::SourceId;
pub use operations::{
    AlbumMetadata, AlbumMetadataEdit, AlbumMetadataMixed, AlbumMetadataValues,
    AlbumMetadataWritable, ArtistMetadata, ArtistMetadataEdit, ArtistMetadataMixed,
    ArtistMetadataValues, ArtistMetadataWritable, ImageBytes, SourceMetadataError, TrackMetadata,
    TrackMetadataEdit, TrackMetadataValues, TrackMetadataWritable,
};
pub use source::*;

pub use discovery::{DiscoveredServer, DiscoveryProvider, discover_servers};
pub use file::local::{
    LOCAL_LIBRARY_SOURCE_ID, LOCAL_SOURCE_ID, read_local_image, verify_local_media_file,
};
pub use plex::{
    PLEX_QUEUE_WINDOW, PlexCompanionContext, PlexCompanionPlayer, PlexQueueItem, PlexQueueMutation,
    PlexQueuePlacement, PlexQueueWindow, plex_queue_write_batches,
};
pub use plex::{PlexBrowserLogin, PlexConnection, PlexLogin, PlexProfile, PlexServer};
pub use subsonic::{SubsonicAuthentication, SubsonicFlavor};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("{error}")]
    IncompleteScan {
        outcome: library::ScanOutcome,
        error: Box<SourceError>,
    },
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
