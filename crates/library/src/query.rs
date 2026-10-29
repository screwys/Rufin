//! Typed member and list reads. Orders, counts and sections come from the resident Catalog;
//! only the rows of the window a client shows are read from the Store, by key.
use crate::catalog::views::Member;
use crate::*;
use sqlx::SqliteConnection;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemberRead {
    pub collection: Collection,
    pub sort: Option<CatalogSort>,
    pub folder: Option<FolderKey>,
    pub filter: String,
    pub descending: bool,
    pub favorites_only: bool,
    pub downloaded_only: bool,
    pub activity: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PlaylistScope {
    #[default]
    All,
    Root,
    Folder {
        key: PlaylistFolderKey,
    },
    Folders,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListRead {
    #[serde(default)]
    pub playlist_scope: PlaylistScope,
    pub kind: ListKind,
    pub source: Option<SourceKey>,
    pub folder: Option<FolderKey>,
    pub filter: String,
    pub sort: CatalogSort,
    pub descending: bool,
    pub favorites_only: bool,
    pub downloaded_only: bool,
    pub activity: bool,
}

#[derive(Clone, Debug)]
pub struct Page<T> {
    pub count: usize,
    pub first_row_position: usize,
    pub rows: Vec<T>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MemberRow {
    Track(TrackView),
    Entry(PlaylistEntryView),
}

impl MemberRow {
    pub fn track(&self) -> &TrackView {
        match self {
            Self::Track(track) => track,
            Self::Entry(entry) => &entry.track,
        }
    }
    pub fn into_track(self) -> TrackView {
        match self {
            Self::Track(track) => track,
            Self::Entry(entry) => entry.track,
        }
    }
    pub fn into_entry(self) -> Option<PlaylistEntryView> {
        match self {
            Self::Entry(entry) => Some(entry),
            Self::Track(_) => None,
        }
    }
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
    pub fn effective_sort(&self) -> CatalogSort {
        self.sort.unwrap_or(match self.collection {
            Collection::Album(_) => CatalogSort::TrackNumber,
            Collection::Playlist(_) => CatalogSort::Position,
            Collection::History { .. } => CatalogSort::LastPlayed,
            _ => CatalogSort::Title,
        })
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
    pub async fn count(
        &self,
        catalog: &Catalog,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<usize> {
        if let Collection::History { source } = &self.collection {
            let (_permit, mut connection) = catalog.acquire_general(cancellation).await?;
            return Ok(Catalog::history(
                &mut connection,
                source.as_ref(),
                &self.filter,
                self.descending,
            )
            .await?
            .len());
        }
        let (_permit, mut connection) = catalog.acquire_loaded(cancellation).await?;
        Ok(catalog.member_order(&mut connection, self).await?.len())
    }
    pub async fn rows(
        &self,
        catalog: &Catalog,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<MemberRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let range = offset..offset.saturating_add(limit.min(256));
        if let Collection::History { source } = &self.collection {
            let (_permit, mut connection) = catalog.acquire_general(cancellation).await?;
            let order = Catalog::history(
                &mut connection,
                source.as_ref(),
                &self.filter,
                self.descending,
            )
            .await?;
            return catalog.history_rows(&mut connection, &order, range).await;
        }
        let (_permit, mut connection) = catalog.acquire_loaded(cancellation).await?;
        let order = catalog.member_order(&mut connection, self).await?;
        member_rows(catalog, &mut connection, self, &order, range).await
    }
    pub async fn page(
        &self,
        catalog: &Catalog,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Page<MemberRow>> {
        if let Collection::History { source } = &self.collection {
            let (_permit, mut connection) = catalog.acquire_general(cancellation).await?;
            let order = Catalog::history(
                &mut connection,
                source.as_ref(),
                &self.filter,
                self.descending,
            )
            .await?;
            let range = window.range(order.len());
            return Ok(Page {
                count: order.len(),
                first_row_position: range.start,
                rows: catalog.history_rows(&mut connection, &order, range).await?,
            });
        }
        let (_permit, mut connection) = catalog.acquire_loaded(cancellation).await?;
        let order = catalog.member_order(&mut connection, self).await?;
        let range = window.range(order.len());
        Ok(Page {
            count: order.len(),
            first_row_position: range.start,
            rows: member_rows(catalog, &mut connection, self, &order, range).await?,
        })
    }
    pub async fn sections(
        &self,
        catalog: &Catalog,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ScrollSection>> {
        if matches!(self.collection, Collection::History { .. }) {
            return Ok(Vec::new());
        }
        let order = {
            let (_permit, mut connection) = catalog.acquire_loaded(cancellation).await?;
            catalog.member_order(&mut connection, self).await?
        };
        let snapshot = catalog.snapshot();
        Ok(crate::catalog::views::Context::new(&snapshot, false).member_sections(self, &order))
    }
    pub async fn disc_sections(
        &self,
        catalog: &Catalog,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(u32, i64)>> {
        if matches!(self.collection, Collection::History { .. }) {
            return Ok(Vec::new());
        }
        let order = {
            let (_permit, mut connection) = catalog.acquire_loaded(cancellation).await?;
            catalog.member_order(&mut connection, self).await?
        };
        let snapshot = catalog.snapshot();
        Ok(crate::catalog::views::Context::new(&snapshot, false).disc_sections(self, &order))
    }
}

/// Rows of a window of a member order: playlist entries, else tracks.
async fn member_rows(
    catalog: &Catalog,
    connection: &mut sqlx::SqliteConnection,
    read: &MemberRead,
    order: &[Member],
    range: std::ops::Range<usize>,
) -> LibraryResult<Vec<MemberRow>> {
    let window = &order[range.start.min(order.len())..range.end.min(order.len())];
    let keys = catalog.member_keys(window);
    if matches!(read.collection, Collection::Playlist(_)) {
        let entries = keys
            .iter()
            .filter_map(|(_, entry)| *entry)
            .collect::<Vec<_>>();
        return Ok(
            crate::playlists::load_playlist_entry_rows(catalog, connection, &entries)
                .await?
                .into_iter()
                .map(MemberRow::Entry)
                .collect(),
        );
    }
    let tracks = keys.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    Ok(crate::tracks::load_track_rows(catalog, connection, &tracks)
        .await?
        .into_iter()
        .map(MemberRow::Track)
        .collect())
}

impl ListRead {
    pub fn new(kind: ListKind, source: Option<SourceKey>, sort: CatalogSort) -> Self {
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
    fn window(&self, offset: usize, limit: usize, len: usize) -> std::ops::Range<usize> {
        let limit = limit.min(if matches!(self.kind, ListKind::ArtistAlbums { .. }) {
            128
        } else {
            256
        });
        offset.min(len)..offset.saturating_add(limit).min(len)
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
    pub async fn artist_release_counts(
        &self,
        catalog: &Catalog,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<[usize; 6]> {
        let (_permit, mut connection) = catalog.acquire_loaded(cancellation).await?;
        let order = catalog.list_order(&mut connection, self).await?;
        let albums = {
            let snapshot = catalog.snapshot();
            order
                .iter()
                .filter_map(|key| snapshot.albums.get(*key))
                .map(|album| {
                    (
                        i64::from(album.key),
                        album.flags & crate::catalog::snapshot::COMPILATION != 0,
                    )
                })
                .collect::<Vec<_>>()
        };
        let mut counts = [0; 6];
        for class in catalog
            .release_classes(&mut connection, &albums)
            .await?
            .into_values()
        {
            counts[crate::catalog::views::release_code(class)] += 1;
        }
        Ok(counts)
    }
    pub async fn count(
        &self,
        catalog: &Catalog,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<usize> {
        let mut read = self.clone();
        if matches!(read.kind, ListKind::SmartPlaylists { .. }) {
            // Counting needs no totals.
            read.sort = CatalogSort::Title;
        }
        let (_permit, mut connection) = catalog.acquire_loaded(cancellation).await?;
        Ok(catalog.list_order(&mut connection, &read).await?.len())
    }
    pub async fn sections(
        &self,
        catalog: &Catalog,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ScrollSection>> {
        let order = {
            let (_permit, mut connection) = catalog.acquire_loaded(cancellation).await?;
            catalog.list_order(&mut connection, self).await?
        };
        let snapshot = catalog.snapshot();
        Ok(crate::catalog::views::Context::new(&snapshot, false).list_sections(self, &order))
    }

    /// The keys of a window of this list, from the cached order.
    async fn window_keys(
        &self,
        catalog: &Catalog,
        connection: &mut sqlx::SqliteConnection,
        offset: usize,
        limit: usize,
    ) -> LibraryResult<Vec<i64>> {
        let order = catalog.list_order(connection, self).await?;
        Ok(order[self.window(offset, limit, order.len())].to_vec())
    }

    async fn page_keys(
        &self,
        catalog: &Catalog,
        connection: &mut sqlx::SqliteConnection,
        window: RouteSeedWindow,
    ) -> LibraryResult<(usize, usize, Vec<i64>)> {
        let order = catalog.list_order(connection, self).await?;
        let range = window.range(order.len());
        Ok((order.len(), range.start, order[range].to_vec()))
    }
}

// Every typed window reads its rows by key from the cached order.
macro_rules! list_windows {
    ($rows:ident, $($page:ident)?, $row:ty, $catalog:ident, $connection:ident, $read:ident, $keys:ident => $load:expr) => {
        impl ListRead {
            pub async fn $rows(
                &self,
                catalog: &Catalog,
                offset: usize,
                limit: usize,
                cancellation: &ReadCancellation,
            ) -> LibraryResult<Vec<$row>> {
                let (_permit, mut connection) = catalog.acquire_loaded(cancellation).await?;
                let keys = self.window_keys(catalog, &mut connection, offset, limit).await?;
                let ($catalog, $connection, $read, $keys) = (catalog, &mut *connection, self, &keys[..]);
                $load.await
            }
            $(pub async fn $page(
                &self,
                catalog: &Catalog,
                window: RouteSeedWindow,
                cancellation: &ReadCancellation,
            ) -> LibraryResult<Page<$row>> {
                let (_permit, mut connection) = catalog.acquire_loaded(cancellation).await?;
                let (count, first_row_position, keys) =
                    self.page_keys(catalog, &mut connection, window).await?;
                let ($catalog, $connection, $read, $keys) = (catalog, &mut *connection, self, &keys[..]);
                Ok(Page {
                    count,
                    first_row_position,
                    rows: $load.await?,
                })
            })?
        }
    };
}

list_windows!(albums, page_albums, AlbumView, catalog, connection, read, keys => async {
    let keys = keys.iter().copied().map(AlbumKey::from_raw).collect::<Vec<_>>();
    crate::collections::load_album_rows(catalog, connection, &keys, read.activity).await
});
list_windows!(artists, page_artists, ArtistView, catalog, connection, read, keys => async {
    let ListKind::Artists { album_artist } = read.kind else {
        unreachable!()
    };
    let keys = keys.iter().copied().map(ArtistKey::from_raw).collect::<Vec<_>>();
    crate::collections::load_artist_rows(catalog, connection, &keys, album_artist, read.activity).await
});
list_windows!(genres, page_genres, GenreView, catalog, connection, _read, keys => async {
    let keys = keys.iter().copied().map(GenreKey::from_raw).collect::<Vec<_>>();
    crate::collections::load_genre_rows(catalog, connection, &keys).await
});
list_windows!(moods, page_moods, MoodView, catalog, connection, _read, keys => async {
    let keys = keys.iter().copied().map(MoodKey::from_raw).collect::<Vec<_>>();
    crate::collections::load_mood_rows(catalog, connection, &keys).await
});
list_windows!(playlists, , PlaylistRow, catalog, connection, read, keys => async {
    let _ = read;
    let keys = keys.iter().copied().filter(|key| *key >= 0).map(PlaylistKey::from_raw).collect::<Vec<_>>();
    crate::playlists::load_playlist_rows(catalog, connection, &keys).await
});
list_windows!(playlist_items, page_playlist_items, PlaylistListRow, catalog, connection, read, keys => {
    crate::playlist_folders::load_items(catalog, connection, read, keys)
});
list_windows!(smart_playlists, , SmartPlaylistRow, catalog, connection, read, keys => async {
    let ListKind::SmartPlaylists { now } = read.kind else {
        unreachable!()
    };
    let keys = keys.iter().copied().filter(|key| *key >= 0).map(SmartPlaylistKey::from_raw).collect::<Vec<_>>();
    crate::smart_playlists::load_smart_playlist_rows(catalog, connection, &keys, now).await
});

impl Catalog {
    pub async fn track(
        &self,
        uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<TrackView>> {
        Ok(self
            .tracks_by_uris(&[uri.to_owned()], cancellation)
            .await?
            .pop())
    }
    pub async fn tracks(
        &self,
        keys: &[TrackKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackView>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        crate::tracks::load_track_rows(self, &mut connection, &keys[..keys.len().min(256)]).await
    }
    pub async fn tracks_by_uris(
        &self,
        uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackView>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let keys = keys_by_uris(
            &mut connection,
            "tracks",
            "track_key",
            &uris[..uris.len().min(256)],
        )
        .await?
        .into_iter()
        .map(TrackKey::from_raw)
        .collect::<Vec<_>>();
        crate::tracks::load_track_rows(self, &mut connection, &keys).await
    }
    pub async fn playlists(
        &self,
        keys: &[PlaylistKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistRow>> {
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        crate::playlists::load_playlist_rows(self, &mut connection, &keys[..keys.len().min(256)])
            .await
    }
    pub async fn playlist_entries(
        &self,
        keys: &[PlaylistEntryKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistEntryView>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        crate::playlists::load_playlist_entry_rows(
            self,
            &mut connection,
            &keys[..keys.len().min(256)],
        )
        .await
    }
}

/// Keys of rows by media URI, in the order asked. URIs the Store does not hold are skipped.
pub(crate) async fn keys_by_uris(
    connection: &mut SqliteConnection,
    table: &str,
    key: &str,
    uris: &[String],
) -> LibraryResult<Vec<i64>> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT entity.{key} FROM json_each(?1) requested JOIN {table} entity ON entity.media_uri=requested.value ORDER BY CAST(requested.key AS INTEGER)")))
        .bind(serde_json::to_string(uris)?)
        .fetch_all(connection)
        .await?)
}

macro_rules! collection_points {
    ($point:ident,$batch:ident,$table:literal,$column:literal,$key:ty,$row:ty,$load:ident, $( $argument:ident : $argument_type:ty ),*) => {
        impl Catalog {
            pub async fn $point(&self, uri: &str, $( $argument:$argument_type, )* cancellation: &ReadCancellation) -> LibraryResult<Option<$row>> {
                let (_permit,mut connection)=self.acquire_loaded(cancellation).await?;
                let keys=keys_by_uris(&mut connection,$table,$column,&[uri.to_owned()]).await?.into_iter().map(<$key>::from_raw).collect::<Vec<_>>();
                Ok(crate::collections::$load(self,&mut connection,&keys, $( $argument, )*).await?.pop())
            }
            pub async fn $batch(&self, keys: &[$key], $( $argument:$argument_type, )* cancellation: &ReadCancellation) -> LibraryResult<Vec<$row>> {
                let (_permit,mut connection)=self.acquire_loaded(cancellation).await?;
                crate::collections::$load(self,&mut connection,&keys[..keys.len().min(256)], $( $argument, )*).await
            }
        }
    };
}
collection_points!(album,albums,"albums","album_key",AlbumKey,AlbumView,load_album_rows,activity:bool);
collection_points!(artist,artists,"artists","artist_key",ArtistKey,ArtistView,load_artist_rows,album_artist:bool,activity:bool);
collection_points!(
    genre,
    genres,
    "tags",
    "tag_key",
    GenreKey,
    GenreView,
    load_genre_rows,
);
collection_points!(
    mood,
    moods,
    "tags",
    "tag_key",
    MoodKey,
    MoodView,
    load_mood_rows,
);

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
