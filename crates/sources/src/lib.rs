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
mod operations;
mod policy;
mod source;

mod jellyfin;
mod local;
mod remote_http;
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
    JellyfinSettingsInput, JellyfinSetupInput, LocalFolderHostInput, SourceCacheMatch,
    SourceConfiguration, SourceId, SourceSettingsInput, SourceSetupInput,
};
pub use operations::{
    AlbumMetadata, AlbumMetadataEdit, AlbumMetadataMixed, AlbumMetadataValues,
    AlbumMetadataWritable, ArtistMetadata, ArtistMetadataEdit, ArtistMetadataMixed,
    ArtistMetadataValues, ArtistMetadataWritable, ImageBytes, LyricsSearch, NativeLyricAgent,
    NativeLyricAgentRole, NativeLyricCue, NativeLyricCueLine, NativeLyricLine, NativeLyrics,
    NativeLyricsDocument, NativeLyricsRole, SourceMetadataError, TrackMetadata, TrackMetadataEdit,
    TrackMetadataValues, TrackMetadataWritable,
};
pub use source::*;

pub use jellyfin::{DiscoveredJellyfinServer, discover_jellyfin_servers};
pub use local::{LOCAL_LIBRARY_SOURCE_ID, LOCAL_SOURCE_ID, verify_local_media_file};
pub use subsonic::{SubsonicAuthentication, SubsonicFlavor};

use thiserror::Error;

#[derive(Debug, Error)]
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
