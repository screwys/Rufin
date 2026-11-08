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
mod field;
pub use field::LibraryField;
pub use query::{CatalogKind, Collection, ListKind, ListRead, MemberRead, PlaylistScope};
mod read;
pub use read::{
    AlbumRow, AlbumSpan, ArtistRow, Fetch, Fetched, FolderRead, FolderRow, Header, HeaderKey, Item,
    Order, PlaylistEntry, Read, Row, TagRow, TrackRow,
};
mod db;
mod downloads;
mod favorites;
mod fingerprint;
mod home;
mod keys;
mod local;
mod loudness;
mod lyrics;
mod lyrics_search;
mod lyrics_tokenizer;
mod migration;
mod mosaics;
mod playlist_folders;
mod playlist_format;
mod playlist_links;
pub use playlist_folders::{
    PlaylistFolderCopy, PlaylistFolderMember, PlaylistFolderRow, PlaylistListRow,
};
mod local_access;
mod pins;
pub use local_access::{
    DocumentLookup, LocalAccess, MappedFile, install_document_lookup, project_local_access_path,
    reported_path_is_absolute,
};
mod playable;
pub use playable::{Gains, Playable, PlayableDownload, PlayableFile, PlayableNames};
mod playlists;
pub use pins::{CollectionShown, SidebarPin};
mod queue;
pub mod rank;
mod waveform;
pub use downloads::{DownloadFile, media_encoding_key};
mod catalog;
mod radio;
mod scroll_sections;
pub use scroll_sections::ScrollSection;
mod schema;
mod search;
mod selection;
pub use selection::MemberSelection;
mod smart_playlists;
mod source_kind;
pub use source_kind::SourceKind;
mod storage;
pub use storage::CacheUsage;
mod tags;
pub use tags::TagKind;
mod tracks;
pub use tracks::AudioProperties;

pub use activity::{
    ActivityCsvFormat, ActivityRecord, CalendarActivityPeriod, LegacyActivityKey,
    LegacyActivityRecord, ListenDeliveryTarget, ListenWrite, PendingListenDelivery,
};
pub use artwork::{
    AlbumCover, ArtworkCacheRetention, ArtworkCandidate, ArtworkImage, RepresentativeArtworkScope,
    authored_playlist_artwork, automatic_playlist_artwork, normalize_artwork_binding,
};
pub use backup::{BackupRestoreReport, StateGroups};
pub use catalog::{
    AlbumObservation, ArtistObservation, CachedSource, CatalogBatch, Change, DownloadChange,
    FileBatch, FolderObservation, Freshness, LocalArtworkCandidate, Observation,
    PlaylistEntryObservation, PlaylistObservation, Scope, ShowsState, SourcePhase, StateChange,
    StoredFile, TagObservation, TrackObservation, walk_key,
};
pub use collections::{
    AlbumArtistLink, AlbumGenreLink, AlbumLinks, AlbumReleaseCandidate, AlbumReleaseClass,
    AlbumReleaseResult, AlbumView, ArtistView, CollectionSummary, FolderChoice, GenreChoice,
    normalize_release_types,
};
pub use db::{Catalog, ReadCancellation};
pub use favorites::{Delivery, DeliveryKind, FavoriteTarget, NativeUserState, UserMediaStateWrite};
pub use fingerprint::FingerprintRow;
pub use home::{
    HomeAlbumRow, HomeBlockKind, HomeEntryInput, HomeEntryKind, HomePage, HomeProviderSection,
    HomeSectionKind, HomeSectionRows, HomeShowcase, HomeTrackRow,
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
    Downloads, FileEditRow, LocalFileKind, LocalFileState, LocalFileWrite, LocalLocatorWrite,
    LocalMediaLocation, MappingTrackRow,
};
pub use loudness::{AnalysisCopy, LoudnessMeasurement, R128TagWrite};
pub use lyrics::LyricsCacheRow;
pub use mosaics::MosaicOwner;
pub use playlist_format::{
    PlaylistFile, PlaylistFileEntry, PlaylistFormat, PlaylistImportReport, PlaylistPathMode,
    playlist_host_path, playlist_locator,
};
pub use playlist_links::{ObservedPlaylistFile, PlaylistFileLink};
pub use playlists::{
    PlaylistAccess, PlaylistCopy, PlaylistEditEntry, PlaylistEntryCopy, PlaylistEntryWrite,
    PlaylistGenreLink, PlaylistIdentity, PlaylistImage, PlaylistRow, SourceEntryEdit,
};
pub use queue::{
    QUEUE_CONTEXT_LIMIT, QueueContextTitle, QueueEntries, QueueEntry, QueueLoad, QueuePlacement,
    QueueProvenance, QueueRank, QueueReorderTarget, QueueRepeatMode, QueueRestore, QueueRow,
    QueueUpdate, shuffle_order,
};
pub use radio::{PlayedFilter, RadioSeed, RandomCriteria};
pub use search::{LyricsSnippet, LyricsSnippetPart, SearchMode, SearchRequest, SearchResults};
pub use smart_playlists::{
    SmartPlaylistActivityPeriod, SmartPlaylistCopy, SmartPlaylistDefinition, SmartPlaylistRow,
    SmartPlaylistRule, SmartPlaylistRuleField, SmartPlaylistRuleOperator, SmartPlaylistRuleValue,
    SmartPlaylistRuleValueKind, SmartPlaylistSort, SmartPlaylistValueSuggestions,
    SmartPlaylistWrite,
};
pub use tracks::{TrackAccess, TrackArtistLink, TrackGenreLink, TrackLinks, TrackState, TrackView};

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
    /// Rows from and up to, read as they are in place of the aligned window.
    rows: Option<(usize, usize)>,
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
            rows: None,
        }
    }

    pub fn position(position: usize) -> Self {
        Self {
            position: Some(position),
            ..Self::top()
        }
    }

    pub fn rows(rows: std::ops::Range<usize>) -> Self {
        Self {
            rows: Some((rows.start, rows.end)),
            ..Self::top()
        }
    }

    pub fn range(self, len: usize) -> std::ops::Range<usize> {
        if let Some((start, end)) = self.rows {
            return start.min(len)..end.min(len);
        }
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
