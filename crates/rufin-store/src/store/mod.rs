use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use rufin_core::{
    Album, AlbumId, Artist, ArtistCredit, ArtistId, Genre, GenreId, HomeSection, HomeSectionKind,
    ImageRef, LibraryField, MusicFolder, MusicFolderId, Playlist, PlaylistId, QueueSnapshot,
    ServerId, ServerIdentity, Track, TrackId,
};
use rufin_provider::{Lyrics, PagedResponse, PlaylistDetail, PlaylistEntry, SearchResults};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use thiserror::Error;

const SCHEMA_VERSION: i64 = 10;
const CACHE_KEY_PART_MAX_LEN: usize = 180;
const CACHE_KEY_HASH_LEN: usize = 16;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported store schema version: {0}")]
    UnsupportedSchemaVersion(i64),
    #[error("incomplete store schema version: {0}")]
    IncompleteSchemaVersion(i64),
}

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedServer {
    pub server: ServerIdentity,
    pub user_id: String,
    pub username: String,
    pub trust_invalid_cert: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerLocalAccess {
    pub server_id: ServerId,
    pub root_path: String,
    pub path_replace_from: Option<String>,
    pub path_replace_to: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncState {
    pub server_id: ServerId,
    pub generation: i64,
    pub status: String,
    pub last_started_at: Option<String>,
    pub last_completed_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverCacheEntry {
    pub server_id: ServerId,
    pub item_id: String,
    pub image_tag: String,
    pub size: u32,
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachedArtistDetail {
    pub artist: Artist,
    pub albums: Vec<Album>,
    pub appears_on: Vec<Album>,
    pub tracks: Vec<Track>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachedGenreDetail {
    pub genre: Genre,
    pub albums: Vec<Album>,
    pub tracks: Vec<Track>,
}

pub struct Store {
    connection: Connection,
}

mod library_auxiliary_cache;
mod library_cache_reads;
mod library_cache_writes;
mod library_counts;
mod library_metadata;
mod library_search_helpers;
mod library_track_sort;
mod servers;
mod store_lifecycle_schema;

pub use servers::{image_cache_key, lyrics_cache_key};

#[cfg(test)]
mod library_relationship_tests;
#[cfg(test)]
mod schema_cache_tests;
#[cfg(test)]
mod sync_search_cover_tests;
#[cfg(test)]
mod test_support;
