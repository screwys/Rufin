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
    ActivityAlbumRow, ActivityArtistRow, ActivityGenreRow, ActivityHistoryRow, ActivityTrackRow,
    CalendarActivityPeriod, CalendarActivitySummary, ListenDeliveryTarget, ListenWrite,
    PendingListenDelivery,
};
pub use artwork::{LocalAlbumArtworkInput, RepresentativeArtworkScope};
pub use collections::{
    AlbumArtistLink, AlbumDetail, AlbumGenreLink, AlbumMetadataWrite, AlbumReleaseCandidate,
    AlbumReleaseClass, AlbumReleaseClassification, AlbumReleaseResult, AlbumRow, AlbumSort,
    ArtistDetail, ArtistMetadataWrite, ArtistRow, ArtistSort, FolderRow, GenreDetail, GenreRow,
    GenreSort, MoodDetail, MoodRow, MoodSort,
};
pub use db::{Database, ReadCancellation};
pub use favorites::FavoriteTarget;
pub use home::{
    HomeAlbumRow, HomeEntryInput, HomeEntryKind, HomeGenreRow, HomePage, HomeProviderSection,
    HomeSectionRows, HomeTrackRow,
};
pub use keys::{
    AlbumDetailRouteKey, AlbumKey, ArtistKey, FolderKey, GenreKey, ListenKey, ListenOutboxKey,
    LocalAccessFileKey, LocalFileKey, MoodKey, PlaylistEntryKey, PlaylistKey, QueueOccurrenceKey,
    SmartPlaylistKey, SourceKey, TrackKey,
};
pub use local::{
    LocalAccessOrigin, LocalAccessRow, LocalAccessWrite, LocalFileKind, LocalFileRow,
    LocalFileState, LocalFileWrite, MappingTrackRow,
};
pub use loudness::{
    AlbumLoudnessTrack, AlbumLoudnessWork, LoudnessMeasurement, R128TagWrite, TrackLoudnessWork,
};
pub use lyrics::LyricsCacheRow;
pub use playlists::{
    PlaylistDestination, PlaylistEntryOrder, PlaylistEntryRow, PlaylistEntrySort,
    PlaylistGenreLink, PlaylistRow, PlaylistSort,
};
pub use queue::{
    QueueCompactOccurrence, QueueMedia, QueuePageRow, QueueProvenance, QueueRepeatMode,
    QueueRestore,
};
pub use radio::{PlayedFilter, RadioSeed, RandomCriteria};
pub use scan::{CachedSource, Freshness, Publication, Scan, ScanLink, ScanOutcome};
pub use search::{SearchRequest, SearchResults};
pub use smart_playlists::{
    SmartPlaylistActivityPeriod, SmartPlaylistDefinition, SmartPlaylistListSort, SmartPlaylistRow,
    SmartPlaylistRule, SmartPlaylistRuleField, SmartPlaylistRuleOperator, SmartPlaylistRuleValue,
    SmartPlaylistRuleValueKind, SmartPlaylistSort, SmartPlaylistValueSuggestions,
};
pub use tracks::{
    TrackArtistLink, TrackGenreLink, TrackMetadataWrite, TrackRoutePage, TrackRow, TrackSort,
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RouteSeedWindow {
    relative: f64,
    limit: usize,
}

impl RouteSeedWindow {
    const LIMIT: usize = 64;

    pub fn top() -> Self {
        Self::new(0.0, Self::LIMIT)
    }

    pub fn relative(relative: f64) -> Self {
        Self::new(relative, Self::LIMIT)
    }

    fn new(relative: f64, limit: usize) -> Self {
        Self {
            relative: if relative.is_finite() {
                relative.clamp(0.0, 1.0)
            } else {
                0.0
            },
            limit: limit.max(1),
        }
    }

    pub fn range(self, len: usize) -> std::ops::Range<usize> {
        let position = ((len.saturating_sub(1)) as f64 * self.relative).round() as usize;
        let start = position / self.limit * self.limit;
        start.min(len)..start.saturating_add(self.limit).min(len)
    }
}

impl LibraryError {
    pub fn is_store_path_io(&self) -> bool {
        matches!(self, Self::Io(_) | Self::Sqlite(sqlx::Error::Io(_)))
    }
}

#[cfg(test)]
mod route_seed_tests {
    use super::RouteSeedWindow;

    #[test]
    fn restored_route_window_is_aligned_and_bounded() {
        assert_eq!(RouteSeedWindow::top().range(1_000), 0..64);
        assert_eq!(RouteSeedWindow::relative(0.5).range(1_000), 448..512);
        assert_eq!(RouteSeedWindow::relative(1.0).range(1_000), 960..1_000);
        assert_eq!(RouteSeedWindow::relative(0.5).range(0), 0..0);
    }
}
