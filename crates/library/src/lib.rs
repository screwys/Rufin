//! Rufin's SQLite-owned music library and its concrete persistence operations.
//! Provider acquisition, playback transport, and presentation remain outside this crate.

mod activity;
mod artwork;
mod collections;
mod db;
mod favorites;
mod home;
mod keys;
mod local;
mod loudness;
mod lyrics;
mod playlists;
mod queue;
mod radio;
mod recovery;
mod scan;
mod schema;
mod search;
mod smart_playlists;
mod tracks;

pub use activity::{
    ActivityBaseline, ActivityHistoryRow, ActivityPeriod, ActivityTrackRow, ListenDeliveryTarget,
    ListenWrite, PendingListenDelivery,
};
pub use artwork::{ArtworkPreparationPage, LocalAlbumArtworkCandidate};
pub use collections::{
    AlbumArtistLink, AlbumDetail, AlbumGenreLink, AlbumMetadataWrite, AlbumReleaseCandidate,
    AlbumReleaseResult, AlbumRow, AlbumSort, ArtistDetail, ArtistMetadataWrite, ArtistRow,
    ArtistSort, FolderRow, GenreDetail, GenreRow, GenreSort, MoodDetail, MoodRow, MoodSort,
};
pub use db::{Database, ReadCancellation};
pub use favorites::{FavoriteTarget, PendingFavorite};
pub use home::{HomeAlbumRow, HomeEntryInput, HomeEntryKind, HomeGenreRow, HomeTrackRow};
pub use keys::{
    AlbumKey, ArtistKey, FolderKey, GenreKey, ListenKey, ListenOutboxKey, LocalAccessFileKey,
    LocalFileKey, MoodKey, PlaylistEntryKey, PlaylistKey, QueueOccurrenceKey, SmartPlaylistKey,
    SourceKey, TrackKey,
};
pub use local::{
    LocalAccessRow, LocalAccessWrite, LocalFileKind, LocalFileRow, LocalFileState, LocalFileWrite,
};
pub use loudness::{AlbumLoudnessTrack, AlbumLoudnessWork, LoudnessMeasurement, TrackLoudnessWork};
pub use lyrics::LyricsCacheRow;
pub use playlists::{PlaylistEntryRow, PlaylistEntrySort, PlaylistRow, PlaylistSort};
pub use queue::{
    QueueCompactOccurrence, QueueCurrentNext, QueueMedia, QueuePageRow, QueueProvenance,
    QueueRepeatMode, QueueRestore, QueueState,
};
pub use radio::{PlayedFilter, RadioSeed, RandomCriteria};
pub use recovery::RecoveryReport;
pub use scan::{CachedSource, Freshness, Publication, Scan, ScanOutcome};
pub use search::{SearchRequest, SearchResults};
pub use smart_playlists::{
    SmartPlaylistActivityPeriod, SmartPlaylistDefinition, SmartPlaylistListSort, SmartPlaylistRow,
    SmartPlaylistRule, SmartPlaylistRuleField, SmartPlaylistRuleOperator, SmartPlaylistRuleValue,
    SmartPlaylistSort,
};
pub use tracks::{
    TrackArtistLink, TrackDetail, TrackGenreLink, TrackMetadataWrite, TrackRow, TrackSort,
};

use thiserror::Error;

/// Failures at the Library persistence boundary.
#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("SQLite failed: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("Library Store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("stored JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported Store (application ID {application_id}, schema {user_version})")]
    UnsupportedStore {
        application_id: i64,
        user_version: i64,
    },
    #[error("invalid Library Store: {0}")]
    InvalidStore(String),
    #[error("scan input is invalid: {0}")]
    InvalidScan(String),
    #[error("Library request is invalid: {0}")]
    InvalidRequest(String),
    #[error("scan staging failed; the candidate cannot be published")]
    ScanFailed,
    #[error("another source scan is already active")]
    ScanInProgress,
    #[error("the fixed Library writer is unavailable")]
    WriterUnavailable,
    #[error("the Library read was cancelled")]
    ReadCancelled,
}

pub type LibraryResult<T> = Result<T, LibraryError>;
