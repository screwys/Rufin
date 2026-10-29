//! The read vocabulary: which members or which list a client asks for.
use crate::*;
use sqlx::SqliteConnection;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Collection {
    Source(SourceKey),
    Objects {
        source: SourceKey,
        ids: Arc<[String]>,
    },
    Album(AlbumKey),
    Artist {
        key: ArtistKey,
        album_artist: bool,
    },
    Genre(GenreKey),
    Mood(MoodKey),
    Playlist(PlaylistKey),
    Smart {
        key: SmartPlaylistKey,
        now: i64,
    },
    History {
        source: Option<SourceId>,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemberRead {
    pub collection: Collection,
    pub sort: Option<LibraryField>,
    pub folder: Option<FolderKey>,
    pub filter: String,
    pub descending: bool,
    pub favorites_only: bool,
    pub downloaded_only: bool,
    pub activity: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ListKind {
    Albums,
    ArtistAlbums {
        artist: ArtistKey,
        album_artist: bool,
        release_class: Option<AlbumReleaseClass>,
        filters: Option<Arc<[String; 5]>>,
    },
    Artists {
        album_artist: bool,
    },
    Genres,
    Moods,
    Playlists,
    SmartPlaylists {
        now: i64,
    },
}

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub enum PlaylistScope {
    #[default]
    All,
    Root,
    Folder {
        key: PlaylistFolderKey,
    },
    Folders,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListRead {
    #[serde(default)]
    pub playlist_scope: PlaylistScope,
    pub kind: ListKind,
    pub source: Option<SourceKey>,
    pub folder: Option<FolderKey>,
    pub filter: String,
    pub sort: LibraryField,
    pub descending: bool,
    pub favorites_only: bool,
    pub downloaded_only: bool,
    pub activity: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CatalogKind {
    Track,
    Album,
    Artist,
    Genre,
    Mood,
    Folder,
    Playlist,
}

impl CatalogKind {
    /// The table, key column and kind predicate of rows of this kind.
    fn sql(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Track => ("tracks", "track_key", "1"),
            Self::Album => ("albums", "album_key", "1"),
            Self::Artist => ("artists", "artist_key", "1"),
            Self::Genre => ("tags", "tag_key", "kind=0"),
            Self::Mood => ("tags", "tag_key", "kind=1"),
            Self::Folder => ("folders", "folder_key", "1"),
            Self::Playlist => ("playlists", "playlist_key", "1"),
        }
    }
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct CatalogIdentity {
    pub key: i64,
    pub source_key: Option<SourceKey>,
    pub object_id: Option<String>,
    pub media_uri: String,
}

impl Catalog {
    pub async fn catalog_identity(
        &self,
        kind: CatalogKind,
        uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<CatalogIdentity>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let (table, key, scope) = kind.sql();
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {key} key,source_key,object_id,media_uri FROM {table} WHERE media_uri=?1 AND {scope}"
        )))
        .bind(uri)
        .fetch_optional(&mut *connection)
        .await?)
    }
    pub async fn catalog_keys(
        &self,
        kind: CatalogKind,
        source: Option<SourceKey>,
        objects: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<i64>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let (table, key, scope) = kind.sql();
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT entity.{key} FROM json_each(?1) requested JOIN {table} entity ON entity.object_id=requested.value WHERE entity.source_key IS ?2 AND {scope} ORDER BY CAST(requested.key AS INTEGER)"))).bind(serde_json::to_string(objects)?).bind(source).fetch_all(&mut *connection).await?)
    }
}

impl MemberRead {
    pub fn new(collection: Collection) -> Self {
        Self {
            collection,
            sort: None,
            folder: None,
            filter: String::new(),
            descending: false,
            favorites_only: false,
            downloaded_only: false,
            activity: false,
        }
    }
    /// The sort it orders by: its own, else its collection's. A field no list sorts by
    /// orders by title.
    pub fn effective_sort(&self) -> LibraryField {
        self.sort
            .unwrap_or(match self.collection {
                Collection::Album(_) => LibraryField::TrackNumber,
                Collection::Playlist(_) => LibraryField::RowIndex,
                Collection::History { .. } => LibraryField::LastPlayed,
                _ => LibraryField::Title,
            })
            .sort()
    }
    /// Whether plays change the rows an open page shows. An open Smart Playlist keeps the list
    /// it opened with; plays change its members on the next visit.
    pub fn activity_sensitive(&self) -> bool {
        match self.collection {
            Collection::Smart { .. } => false,
            Collection::History { .. } => true,
            _ => self.activity || self.effective_sort().activity_sensitive(),
        }
    }
    pub fn selection(self) -> MemberSelection {
        MemberSelection::Members {
            read: self,
            ranges: None,
        }
    }
}

impl ListRead {
    pub fn new(kind: ListKind, source: Option<SourceKey>, sort: LibraryField) -> Self {
        Self {
            kind,
            source,
            playlist_scope: PlaylistScope::All,
            sort,
            folder: None,
            filter: String::new(),
            descending: false,
            favorites_only: false,
            downloaded_only: false,
            activity: false,
        }
    }

    pub fn activity_sensitive(&self) -> bool {
        self.activity
            || self.sort.activity_sensitive()
            || matches!(self.kind, ListKind::SmartPlaylists { .. })
    }
    pub fn selection(self) -> MemberSelection {
        MemberSelection::Parents {
            read: self,
            ranges: None,
        }
    }
}

/// Track keys by media URI, in the order asked. URIs the Store does not hold are skipped.
pub(crate) async fn track_keys_by_uris(
    connection: &mut SqliteConnection,
    uris: &[String],
) -> LibraryResult<Vec<TrackKey>> {
    Ok(sqlx::query_scalar("SELECT track.track_key FROM json_each(?1) requested JOIN tracks track ON track.media_uri=requested.value ORDER BY CAST(requested.key AS INTEGER)")
        .bind(serde_json::to_string(uris)?)
        .fetch_all(connection)
        .await?)
}

pub(crate) fn selection_ranges(
    ranges: Option<&[std::ops::Range<usize>]>,
) -> Vec<std::ops::Range<usize>> {
    let mut ranges = ranges
        .map(<[_]>::to_vec)
        .unwrap_or_else(|| std::iter::once(0..usize::MAX).collect());
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
    for range in ranges {
        if range.is_empty() {
            continue;
        }
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
