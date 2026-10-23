//! Rufin's SQLite-owned music library and its concrete persistence operations.
//! Provider acquisition, playback transport, and presentation remain outside this crate.

#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!();

pub type LibraryIndex = usize;
#[cfg(feature = "bindings")]
uniffi::custom_type!(LibraryIndex, u64, {
    remote,
    lower: |value| value as u64,
    try_lift: |value| Ok(usize::try_from(value)?),
});

mod audio;
pub use audio::*;
mod activity;
mod activity_overview;
pub use activity_overview::{ActivityOverview, ActivityTotals, GenreActivity};
mod artwork;
mod backup;
mod metadata;
mod metadata_lookup;
mod query;
pub use metadata::*;
mod collections;
pub use query::{
    CatalogIdentity, CatalogKind, Collection, ListKind, ListRead, MemberRead, MemberRow, Page,
    PlaylistScope,
};
pub mod connect;
pub mod profile;
pub use connect::connect_current_kind;
mod connect_media;
mod db;
mod favorites;
mod fingerprint;
mod home;
mod keys;
mod local;
mod loudness;
mod lyrics;
mod migration;
mod playlist_folders;
mod playlist_format;
mod playlist_links;
pub use playlist_folders::{PlaylistFolderMember, PlaylistFolderRow, PlaylistListRow};
mod pins;
mod playlists;
pub use pins::{SidebarPin, SidebarPinRow};
mod queue;
mod waveform;
pub use connect_media::{
    CatalogFile, CatalogFileDependency, CatalogFileStamp, ConnectMediaFile, media_encoding_key,
};
mod catalog;
mod radio;
mod scroll_sections;
pub use scroll_sections::ScrollSection;
mod schema;
mod search;
mod selection;
pub use selection::MemberSelection;
mod smart_playlists;
mod storage;
pub use storage::CacheUsage;
mod tracks;
pub use tracks::AudioProperties;

pub use activity::{
    ActivityCsvFormat, ActivityImportReport, ActivityRecord, CalendarActivityPeriod,
    ListenDeliveryTarget, ListenWrite, PendingListenDelivery,
};
pub use artwork::{
    AlbumCover, ArtworkCacheRetention, ArtworkCandidate, ArtworkImage, PlaylistArtworkManifest,
    RepresentativeArtworkScope, authored_playlist_artwork, automatic_playlist_artwork,
    merge_playlist_artwork, normalize_artwork_binding,
};
pub use backup::{BackupRestoreReport, StateGroups};
pub use catalog::{
    AlbumObservation, ArtistObservation, CachedSource, Catalog, CatalogBatch, CatalogUpdate,
    FolderObservation, Freshness, GenreObservation, LocalArtworkCandidate, MoodObservation,
    PlaylistEntryObservation, PlaylistObservation, TrackObservation,
};
pub use collections::{
    AlbumArtistLink, AlbumDetailIndex, AlbumDetailRow, AlbumGenreLink, AlbumLinks,
    AlbumReleaseCandidate, AlbumReleaseClass, AlbumReleaseResult, AlbumView, ArtistView,
    CollectionSummary, FolderChoice, FolderView, GenreChoice, GenreView, MoodView,
};
pub use connect::{CONNECT_PAGE_SIZE, ConnectRecord, ConnectRoot, RecordStamp, StampedRecord};
pub use db::{Database, ReadCancellation};
pub use favorites::{FavoriteTarget, NativeUserState, UserMediaStateWrite};
pub use home::{
    HomeAlbumRow, HomeBlockKind, HomeEntryInput, HomeEntryKind, HomePage, HomeProviderSection,
    HomeSectionKind, HomeSectionRows, HomeTrackRow,
};
pub use keys::{
    AlbumKey, ArtistKey, FolderKey, GenreKey, ListenKey, ListenOutboxKey, LocalAccessFileKey,
    LocalFileKey, MoodKey, PlaylistEntryKey, PlaylistFolderKey, PlaylistKey, SmartPlaylistKey,
    SourceId, SourceKey, TrackKey, cue_media_parts, cue_media_uri, document_access_uri,
    document_fragment_parts, document_locator_fragment, document_locator_parts, document_media_id,
    document_media_uri, file_media_path, normalize_direct_media_uri, source_entity_parts,
    source_entity_uri, source_id_for_uri,
};
pub use local::{
    DownloadMetadata, FileEditRow, LocalAccessOrigin, LocalAccessWrite, LocalFileKind,
    LocalFileRow, LocalFileState, LocalFileWrite, LocalLocatorWrite, LocalMediaLocation,
    MappingTrackRow, ObservedMediaFile,
};
pub use loudness::{AlbumLoudnessWork, LoudnessMeasurement, R128TagWrite, TrackLoudnessWork};
pub use lyrics::LyricsCacheRow;
pub use playlist_format::{
    PlaylistFile, PlaylistFileEntry, PlaylistFormat, PlaylistImportReport, PlaylistPathMode,
    playlist_host_path, playlist_locator,
};
pub use playlist_links::{ObservedPlaylistFile, PlaylistFileLink};
pub use playlists::{
    PlaylistAccess, PlaylistEditEntry, PlaylistEntryRemovals, PlaylistEntryView,
    PlaylistEntryWrite, PlaylistGenreLink, PlaylistIdentity, PlaylistRow,
};
pub use queue::{
    QUEUE_CONTEXT_LIMIT, QueueContextTitle, QueueEntries, QueueEntry, QueueLoad, QueueOrderUpdate,
    QueuePlacement, QueueProvenance, QueueReorderTarget, QueueRepeatMode, QueueRestore, QueueRow,
    QueueTransferPage, QueueTransferRow, QueueUpdate, shuffle_order,
};
pub use radio::{PlayedFilter, RadioSeed, RandomCriteria};
pub use search::{SearchRequest, SearchResults};
pub use smart_playlists::{
    SmartPlaylistActivityPeriod, SmartPlaylistDefinition, SmartPlaylistRow, SmartPlaylistRule,
    SmartPlaylistRuleField, SmartPlaylistRuleOperator, SmartPlaylistRuleValue,
    SmartPlaylistRuleValueKind, SmartPlaylistSort, SmartPlaylistValueSuggestions,
    SmartPlaylistWrite,
};
pub use tracks::{
    CatalogSort, TrackAccess, TrackArtistLink, TrackGenreLink, TrackLinks, TrackState, TrackView,
};
pub use waveform::WaveformCacheRow;

use thiserror::Error;

/// Failures at the Library persistence boundary.
#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("Connect catalog metadata is still arriving")]
    ConnectPending,
    #[error("SQLite failed: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("Library Store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("stored JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("known Store migration failed: {0}")]
    Migration(String),
    #[error("invalid Library Store: {0}")]
    InvalidStore(String),
    #[error("Library request is invalid: {0}")]
    InvalidRequest(String),
    #[error("Playlist only accepts Tracks from Source {0}")]
    PlaylistSourceMismatch(SourceId),
    #[error("the fixed Library writer is unavailable")]
    WriterUnavailable,
    #[error("the Library read was cancelled")]
    ReadCancelled,
}

pub type LibraryResult<T> = Result<T, LibraryError>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RouteSeedWindow {
    relative: f64,
    position: Option<usize>,
}

impl RouteSeedWindow {
    const LIMIT: usize = 64;

    pub fn top() -> Self {
        Self::relative(0.0)
    }

    pub fn relative(relative: f64) -> Self {
        Self {
            relative: if relative.is_finite() {
                relative.clamp(0.0, 1.0)
            } else {
                0.0
            },
            position: None,
        }
    }

    pub fn position(position: usize) -> Self {
        Self {
            relative: 0.0,
            position: Some(position),
        }
    }

    pub fn range(self, len: usize) -> std::ops::Range<usize> {
        let position = self
            .position
            .unwrap_or_else(|| ((len.saturating_sub(1)) as f64 * self.relative).round() as usize)
            .min(len.saturating_sub(1));
        let start = position / Self::LIMIT * Self::LIMIT;
        start.min(len)..start.saturating_add(Self::LIMIT).min(len)
    }
}

impl LibraryError {
    pub fn is_store_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Io(_) | Self::Sqlite(sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut)
        ) || matches!(self, Self::Sqlite(sqlx::Error::Database(error))
                if error.code().as_deref().and_then(|code|code.parse::<i32>().ok())
                    .is_some_and(|code|matches!(code & 0xff, 3 | 5 | 6 | 7 | 8 | 10 | 13 | 14)))
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
        assert_eq!(RouteSeedWindow::position(750).range(1_000), 704..768);
        assert_eq!(RouteSeedWindow::position(750).range(500), 448..500);
        assert_eq!(RouteSeedWindow::position(750).range(0), 0..0);
    }
}
