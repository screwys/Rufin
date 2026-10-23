//! Typed collection windows and the ordered relations used by member actions.
use crate::*;
use sqlx::{Arguments, Connection, SqliteConnection, sqlite::SqliteArguments};
use std::{ops::Range, sync::Arc};

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
    pub(crate) fn sql(self) -> (&'static str, &'static str) {
        match self {
            Self::Track => ("tracks", "track"),
            Self::Album => ("albums", "album"),
            Self::Artist => ("artists", "artist"),
            Self::Genre => ("genres", "genre"),
            Self::Mood => ("moods", "mood"),
            Self::Folder => ("folders", "folder"),
            Self::Playlist => ("playlists", "playlist"),
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

impl Database {
    pub async fn catalog_identity(
        &self,
        kind: CatalogKind,
        uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<CatalogIdentity>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let (table, kind) = kind.sql();
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {kind}_key key,source_key,object_id,media_uri FROM {table} WHERE media_uri=?1"
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
        let (table, kind) = kind.sql();
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT entity.{kind}_key FROM json_each(?1) requested JOIN {table} entity ON entity.object_id=requested.value WHERE entity.source_key IS ?2 ORDER BY CAST(requested.key AS INTEGER)"))).bind(serde_json::to_string(objects)?).bind(source).fetch_all(&mut *connection).await?)
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
    pub fn activity_sensitive(&self) -> bool {
        self.activity
            || self.effective_sort().activity_sensitive()
            || matches!(
                self.collection,
                Collection::Smart { .. } | Collection::History { .. }
            )
    }
    pub fn selection(self) -> MemberSelection {
        MemberSelection::Members {
            read: self,
            ranges: None,
        }
    }
    pub async fn count(
        &self,
        database: &Database,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<usize> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let query = member_query_on(&mut connection, self).await?;
        member_count(&mut connection, self, &query).await
    }
    pub async fn rows(
        &self,
        database: &Database,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<MemberRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let query = member_query_on(&mut transaction, self).await?;
        let rows = member_rows(&mut transaction, self, &query, offset, limit.min(256)).await?;
        transaction.commit().await?;
        Ok(rows)
    }
    pub async fn page(
        &self,
        database: &Database,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Page<MemberRow>> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let query = member_query_on(&mut transaction, self).await?;
        let count = member_count(&mut transaction, self, &query).await?;
        let range = window.range(count);
        let rows = member_rows(&mut transaction, self, &query, range.start, range.len()).await?;
        transaction.commit().await?;
        Ok(Page {
            count,
            first_row_position: range.start,
            rows,
        })
    }
    pub async fn sections(
        &self,
        database: &Database,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ScrollSection>> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        member_query_on(&mut connection, self)
            .await?
            .sections(&mut connection)
            .await
    }
    pub async fn disc_sections(
        &self,
        database: &Database,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(u32, i64)>> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let query = member_query_on(&mut connection, self).await?;
        crate::collections::album_disc_sections(
            &query,
            self.effective_sort(),
            self.descending,
            &mut connection,
        )
        .await
    }
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
    fn window_limit(&self, limit: usize) -> usize {
        limit.min(if matches!(self.kind, ListKind::ArtistAlbums { .. }) {
            128
        } else {
            256
        })
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
        database: &Database,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<[usize; 6]> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let query = list_query_on(&mut connection, self).await?;
        let rows = sqlx::query_as_with::<_, (i64, i64), _>(
            sqlx::AssertSqlSafe(format!(
                "SELECT album.release_class,count(*) FROM {} WHERE {} GROUP BY album.release_class",
                query.from, query.predicate
            )),
            query.arguments,
        )
        .fetch_all(&mut *connection)
        .await?;
        let mut counts = [0; 6];
        for (class, count) in rows {
            counts[class as usize] = count as usize;
        }
        Ok(counts)
    }
    pub async fn count(
        &self,
        database: &Database,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<usize> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let query = list_query_on(&mut connection, self).await?;
        list_count(&mut connection, self, &query).await
    }
    pub async fn sections(
        &self,
        database: &Database,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ScrollSection>> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        list_query_on(&mut connection, self)
            .await?
            .sections(&mut connection)
            .await
    }
}

// Every typed window projects the bounded requested relation in the same statement.
macro_rules! list_windows {
    ($rows:ident, $page:ident, $row:ty, $connection:ident, $read:ident, $requested:ident => $projection:expr) => {
        impl ListRead {
            pub async fn $rows(
                &self,
                database: &Database,
                offset: usize,
                limit: usize,
                cancellation: &ReadCancellation,
            ) -> LibraryResult<Vec<$row>> {
                let (_permit, mut connection) = database.acquire_general(cancellation).await?;
                let mut transaction = connection.begin().await?;
                let query = list_query_on(&mut transaction, self).await?;
                let rows = {
                    let $connection = &mut *transaction;
                    let $read = self;
                    let $requested = query.requested_keys(offset, self.window_limit(limit));
                    ($projection).await?
                };
                transaction.commit().await?;
                Ok(rows)
            }
            pub async fn $page(
                &self,
                database: &Database,
                window: RouteSeedWindow,
                cancellation: &ReadCancellation,
            ) -> LibraryResult<Page<$row>> {
                let (_permit, mut connection) = database.acquire_general(cancellation).await?;
                let mut transaction = connection.begin().await?;
                let query = list_query_on(&mut transaction, self).await?;
                let count = list_count(&mut transaction, self, &query).await?;
                let range = window.range(count);
                let rows = {
                    let $connection = &mut *transaction;
                    let $read = self;
                    let $requested = query.requested_keys(range.start, range.len());
                    ($projection).await?
                };
                transaction.commit().await?;
                Ok(Page {
                    count,
                    first_row_position: range.start,
                    rows,
                })
            }
        }
    };
}

list_windows!(albums, page_albums, AlbumView, connection, read, requested => {
    crate::collections::load_album_requested(connection, requested, read.activity)
});
list_windows!(artists, page_artists, ArtistView, connection, read, requested => {
    let ListKind::Artists { album_artist } = read.kind else {
        unreachable!()
    };
    crate::collections::load_artist_requested(connection, requested, album_artist, read.activity)
});
list_windows!(genres, page_genres, GenreView, connection, read, requested => {
    crate::collections::load_genre_requested(connection, requested, read.folder)
});
list_windows!(moods, page_moods, MoodView, connection, read, requested => {
    crate::collections::load_mood_requested(connection, requested, read.folder)
});
list_windows!(playlists, page_playlists, PlaylistRow, connection, _read, requested => {
    Database::load_playlist_requested(connection, requested)
});
list_windows!(playlist_items, page_playlist_items, PlaylistListRow, connection, read, requested => {
    crate::playlist_folders::load_items_on(connection, read, requested)
});
list_windows!(smart_playlists, page_smart_playlists, SmartPlaylistRow, connection, read, requested => {
    let ListKind::SmartPlaylists { now } = read.kind else {
        unreachable!()
    };
    crate::smart_playlists::load_smart_playlist_requested(
        connection, requested, now,
    )
});

async fn member_count(
    connection: &mut SqliteConnection,
    read: &MemberRead,
    query: &OrderedQuery,
) -> LibraryResult<usize> {
    if read.filter.trim().is_empty() && !read.favorites_only && !read.downloaded_only {
        let stored = match read.collection {
            Collection::Source(source) => Some(if let Some(folder) = read.folder {
                format!(
                    "SELECT COALESCE(totals.track_count,0) FROM folders folder {} WHERE folder.source_key={} AND folder.folder_key={}",
                    crate::collections::global_totals_join("folder", false),
                    source.raw(),
                    folder.raw()
                )
            } else {
                format!(
                    "SELECT track_count FROM sources WHERE source_key={}",
                    source.raw()
                )
            }),
            Collection::Album(key) => Some(if let Some(folder) = read.folder {
                format!(
                    "SELECT track_count FROM collection_album_index WHERE kind='folder' AND entity_key={} AND role=0 AND album_key={} AND coalesce(folder_key,'')=''",
                    folder.raw(),
                    key.raw()
                )
            } else {
                format!(
                    "SELECT track_count FROM albums WHERE album_key={}",
                    key.raw()
                )
            }),
            Collection::Artist { key, album_artist } => Some(stored_member_count(
                "artist",
                key.raw(),
                album_artist,
                read.folder,
            )),
            Collection::Genre(key) => {
                Some(stored_member_count("genre", key.raw(), false, read.folder))
            }
            Collection::Mood(key) => {
                Some(stored_member_count("mood", key.raw(), false, read.folder))
            }
            Collection::Playlist(key) => Some(if let Some(folder) = read.folder {
                format!(
                    "SELECT COALESCE(totals.track_count,0) FROM playlists playlist LEFT JOIN collection_totals totals ON totals.kind='playlist' AND totals.entity_key=playlist.playlist_key AND totals.role=0 AND coalesce(totals.folder_key,'')=CASE WHEN playlist.source_key IS NULL THEN '' ELSE {} END WHERE playlist.playlist_key={}",
                    folder.raw(),
                    key.raw()
                )
            } else {
                stored_member_count("playlist", key.raw(), false, None)
            }),
            _ => None,
        };
        if let Some(sql) = stored {
            return Ok(sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql))
                .fetch_optional(connection)
                .await?
                .unwrap_or(0)
                .max(0) as usize);
        }
    }
    Ok(query.count(connection).await?.max(0) as usize)
}
fn stored_member_count(kind: &str, key: i64, role: bool, folder: Option<FolderKey>) -> String {
    let folder = folder.map_or("''".into(), |folder| folder.raw().to_string());
    format!(
        "SELECT track_count FROM collection_totals WHERE kind='{kind}' AND entity_key={key} AND role={} AND coalesce(folder_key,'')={folder}",
        i32::from(role)
    )
}

async fn list_count(
    connection: &mut SqliteConnection,
    read: &ListRead,
    query: &OrderedQuery,
) -> LibraryResult<usize> {
    if read.kind == ListKind::Albums
        && read.folder.is_none()
        && read.filter.trim().is_empty()
        && !read.favorites_only
        && !read.downloaded_only
        && let Some(source) = read.source
    {
        return Ok(sqlx::query_scalar::<_, i64>(
            "SELECT album_count FROM sources WHERE source_key=?1",
        )
        .bind(source)
        .fetch_optional(connection)
        .await?
        .unwrap_or(0)
        .max(0) as usize);
    }
    Ok(query.count(connection).await?.max(0) as usize)
}
async fn member_rows(
    connection: &mut SqliteConnection,
    read: &MemberRead,
    query: &OrderedQuery,
    offset: usize,
    limit: usize,
) -> LibraryResult<Vec<MemberRow>> {
    if matches!(read.collection, Collection::History { .. }) {
        return Ok(
            crate::activity::load_history_page(connection, query, offset, limit)
                .await?
                .into_iter()
                .map(MemberRow::Track)
                .collect(),
        );
    }
    let requested = query.requested_keys(offset, limit);
    if matches!(read.collection, Collection::Playlist(_)) {
        Ok(
            crate::playlists::load_playlist_entry_requested(connection, requested)
                .await?
                .into_iter()
                .map(MemberRow::Entry)
                .collect(),
        )
    } else {
        Ok(crate::tracks::load_track_requested(connection, requested)
            .await?
            .into_iter()
            .map(MemberRow::Track)
            .collect())
    }
}

pub(crate) async fn list_query_on(
    connection: &mut SqliteConnection,
    read: &ListRead,
) -> LibraryResult<OrderedQuery> {
    match read.kind {
        ListKind::Playlists | ListKind::SmartPlaylists { .. } => {
            crate::playlist_folders::playlist_query_on(connection, read).await
        }
        _ => Ok(crate::collections::collection_query(read)),
    }
}

pub(crate) async fn member_query_on(
    connection: &mut SqliteConnection,
    read: &MemberRead,
) -> LibraryResult<OrderedQuery> {
    let sort = read.effective_sort();
    let mut query = match &read.collection {
        Collection::Smart { key, now, .. } => {
            crate::smart_playlists::smart_query(
                connection,
                *key,
                &read.filter,
                read.sort,
                read.descending,
                *now,
            )
            .await?
        }
        Collection::History { source } => {
            crate::activity::history_query(source.as_ref(), &read.filter, read.descending)
        }
        Collection::Playlist(key) => {
            crate::playlists::playlist_query(*key, read.folder, sort, read.descending, &read.filter)
        }
        collection => {
            let mut arguments = SqliteArguments::default();
            // Bounded collections drive their key relation before track ordering.
            let (from, predicate) = match collection {
                Collection::Source(source) => {
                    let from = if let Some(folder) = read.folder {
                        format!("track_folders membership CROSS JOIN tracks track ON track.track_key=membership.track_key AND membership.folder_key={}", folder.raw())
                    } else { "tracks track".into() };
                    (from, format!("track.source_key={}", source.raw()))
                }
                Collection::Objects { source, ids } => {
                    arguments.add(serde_json::to_string(ids)?).map_err(sqlx::Error::Encode)?;
                    (format!("(SELECT DISTINCT value FROM json_each(?1)) requested CROSS JOIN tracks track ON track.source_key={} AND track.object_id=requested.value",source.raw()), "1".into())
                }
                Collection::Album(key) => ("tracks track INDEXED BY tracks_album_order_idx".into(), format!("track.album_key={}",key.raw())),
                Collection::Artist { key, album_artist: false } => ("track_artists membership CROSS JOIN tracks track ON track.track_key=membership.track_key".into(),format!("membership.artist_key={}",key.raw())),
                Collection::Artist { key, album_artist: true } => (format!("({}) membership CROSS JOIN tracks track ON track.track_key=membership.track_key",crate::collections::album_artist_keys(&key.raw().to_string())),"1".into()),
                Collection::Genre(key) => (format!("({}) membership CROSS JOIN tracks track ON track.track_key=membership.track_key",crate::collections::genre_keys(&key.raw().to_string())),"1".into()),
                Collection::Mood(key) => ("track_moods membership CROSS JOIN tracks track ON track.track_key=membership.track_key".into(),format!("membership.mood_key={}",key.raw())),
                _ => unreachable!(),
            };
            let mut query = OrderedQuery {
                key: "track.track_key".into(),
                text: sort.section_text(false),
                from,
                predicate: format!("track.indexed=1 AND ({predicate})"),
                order: sort.order_terms(read.descending, false),
                entry_key: "NULL".into(),
                arguments,
            };
            crate::tracks::track_filter(&mut query, read.folder, &read.filter, false);
            query
        }
    };
    if read.favorites_only {
        crate::tracks::track_filter(&mut query, None, "", true);
    }
    if read.downloaded_only {
        crate::tracks::downloaded_filter(&mut query);
    }
    Ok(query)
}

pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) struct RequestedKeys {
    pub sql: String,
    pub arguments: SqliteArguments,
}
impl RequestedKeys {
    pub fn batch(keys: &[impl serde::Serialize]) -> LibraryResult<Self> {
        let mut arguments = SqliteArguments::default();
        arguments
            .add(serde_json::to_string(keys)?)
            .map_err(sqlx::Error::Encode)?;
        Ok(Self {
            sql: "SELECT value,CAST(key AS INTEGER) ordinal FROM json_each(?1)".into(),
            arguments,
        })
    }
    pub fn keys(kind: CatalogKind, keys: &[impl serde::Serialize]) -> LibraryResult<Self> {
        let (table, name) = kind.sql();
        let batch = Self::batch(keys)?;
        Ok(Self {
            sql: format!(
                "SELECT entity.{name}_key value,requested.ordinal FROM ({}) requested CROSS JOIN {table} entity ON entity.{name}_key=requested.value ORDER BY requested.ordinal LIMIT 256",
                batch.sql
            ),
            arguments: batch.arguments,
        })
    }
    pub fn uris(kind: CatalogKind, uris: &[impl serde::Serialize]) -> LibraryResult<Self> {
        let (table, name) = kind.sql();
        let batch = Self::batch(uris)?;
        Ok(Self {
            sql: format!(
                "SELECT entity.{name}_key value,requested.ordinal FROM ({}) requested CROSS JOIN {table} entity ON entity.media_uri=requested.value ORDER BY requested.ordinal LIMIT 256",
                batch.sql
            ),
            arguments: batch.arguments,
        })
    }
}

pub(crate) struct OrderedQuery {
    pub key: String,
    pub text: Option<String>,
    pub from: String,
    pub predicate: String,
    pub order: Vec<String>,
    pub entry_key: String,
    pub arguments: SqliteArguments,
}
impl OrderedQuery {
    pub async fn count(&self, connection: &mut SqliteConnection) -> LibraryResult<i64> {
        Ok(sqlx::query_scalar_with(
            sqlx::AssertSqlSafe(format!(
                "SELECT count(*) FROM {} WHERE {}",
                self.from, self.predicate
            )),
            self.arguments.clone(),
        )
        .fetch_one(connection)
        .await?)
    }
    pub fn select(&self, columns: &str) -> String {
        format!(
            "SELECT {columns} FROM {} WHERE {} ORDER BY {}",
            self.from,
            self.predicate,
            self.order.join(",")
        )
    }
    pub fn requested_keys(&self, offset: usize, limit: usize) -> RequestedKeys {
        RequestedKeys {
            sql: format!(
                "SELECT value,row_number() OVER ()-1 ordinal FROM ({} LIMIT {} OFFSET {})",
                self.select(&format!("{} value", self.key)),
                limit.min(i64::MAX as usize),
                offset.min(i64::MAX as usize)
            ),
            arguments: self.arguments.clone(),
        }
    }
    pub fn requested_ranges(&self, ranges: Option<&[Range<usize>]>) -> RequestedKeys {
        let sql = selection_ranges(ranges)
            .into_iter()
            .map(|range| {
                let selected = self.requested_keys(range.start, range.len());
                format!(
                    "SELECT value,ordinal+{} ordinal FROM ({})",
                    range.start, selected.sql
                )
            })
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        RequestedKeys {
            sql: if sql.is_empty() {
                self.requested_keys(0, 0).sql
            } else {
                sql
            },
            arguments: self.arguments.clone(),
        }
    }
    pub async fn sections(
        &self,
        connection: &mut SqliteConnection,
    ) -> LibraryResult<Vec<ScrollSection>> {
        let Some(text) = &self.text else {
            return Ok(Vec::new());
        };
        let input = format!(
            "SELECT {text} section_text,row_number() OVER(ORDER BY {})-1 section_position FROM {} WHERE {}",
            self.order.join(","),
            self.from,
            self.predicate
        );
        crate::scroll_sections::decode_sections(
            sqlx::query_scalar_with(
                sqlx::AssertSqlSafe(crate::scroll_sections::section_sql(&input)),
                self.arguments.clone(),
            )
            .fetch_all(connection)
            .await?,
        )
    }
}

pub(crate) async fn selected_rows_on<T>(
    connection: &mut SqliteConnection,
    query: &OrderedQuery,
    columns: &str,
    ranges: Option<&[std::ops::Range<usize>]>,
    limit: Option<usize>,
) -> LibraryResult<Vec<T>>
where
    T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Send + Unpin,
{
    let first_parameter = query.arguments.len() + 1;
    let merged = selection_ranges(ranges);
    let mut rows = Vec::new();
    for range in merged {
        let remaining = limit.unwrap_or(usize::MAX).saturating_sub(rows.len());
        if remaining == 0 {
            break;
        }
        let sql = format!(
            "{} LIMIT ?{first_parameter} OFFSET ?{}",
            query.select(columns),
            first_parameter + 1
        );
        rows.extend(
            sqlx::query_as_with::<_, T, _>(sqlx::AssertSqlSafe(sql), query.arguments.clone())
                .bind(range.len().min(remaining).min(i64::MAX as usize) as i64)
                .bind(range.start.min(i64::MAX as usize) as i64)
                .persistent(false)
                .fetch_all(&mut *connection)
                .await?,
        );
    }
    Ok(rows)
}

impl Database {
    pub async fn track(
        &self,
        uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<TrackView>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(crate::tracks::load_track_requested(
            &mut connection,
            RequestedKeys::uris(CatalogKind::Track, &[uri])?,
        )
        .await?
        .pop())
    }
    pub async fn tracks(
        &self,
        keys: &[TrackKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackView>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        crate::tracks::load_track_requested(
            &mut connection,
            RequestedKeys::keys(CatalogKind::Track, keys)?,
        )
        .await
    }
    pub async fn tracks_by_uris(
        &self,
        uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackView>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        crate::tracks::load_track_requested(
            &mut connection,
            RequestedKeys::uris(CatalogKind::Track, uris)?,
        )
        .await
    }
    pub async fn playlists(
        &self,
        keys: &[PlaylistKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Self::load_playlist_requested(
            &mut connection,
            RequestedKeys::keys(CatalogKind::Playlist, keys)?,
        )
        .await
    }
    pub async fn playlist(
        &self,
        uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(Self::load_playlist_requested(
            &mut connection,
            RequestedKeys::uris(CatalogKind::Playlist, &[uri])?,
        )
        .await?
        .pop())
    }
    pub async fn playlist_entries(
        &self,
        keys: &[PlaylistEntryKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistEntryView>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let batch = RequestedKeys::batch(keys)?;
        let requested = RequestedKeys {
            sql: format!(
                "SELECT entry.playlist_entry_key value,requested.ordinal FROM ({}) requested CROSS JOIN playlist_entries entry ON entry.playlist_entry_key=requested.value ORDER BY requested.ordinal LIMIT 256",
                batch.sql
            ),
            arguments: batch.arguments,
        };
        crate::playlists::load_playlist_entry_requested(&mut connection, requested).await
    }
}

macro_rules! collection_points {
    ($point:ident,$batch:ident,$kind:ident,$key:ty,$row:ty,$load:ident, $( $argument:ident : $argument_type:ty ),*) => {
        impl Database {
            pub async fn $point(&self, uri: &str, $( $argument:$argument_type, )* cancellation: &ReadCancellation) -> LibraryResult<Option<$row>> {
                let (_permit,mut connection)=self.acquire_general(cancellation).await?;
                Ok(crate::collections::$load(&mut connection,RequestedKeys::uris(CatalogKind::$kind,&[uri])?, $( $argument, )*).await?.pop())
            }
            pub async fn $batch(&self, keys: &[$key], $( $argument:$argument_type, )* cancellation: &ReadCancellation) -> LibraryResult<Vec<$row>> {
                let (_permit,mut connection)=self.acquire_general(cancellation).await?;
                crate::collections::$load(&mut connection,RequestedKeys::keys(CatalogKind::$kind,keys)?, $( $argument, )*).await
            }
        }
    };
}
collection_points!(album,albums,Album,AlbumKey,AlbumView,load_album_requested,activity:bool);
collection_points!(artist,artists,Artist,ArtistKey,ArtistView,load_artist_requested,album_artist:bool,activity:bool);
collection_points!(genre,genres,Genre,GenreKey,GenreView,load_genre_requested,folder:Option<FolderKey>);
collection_points!(mood,moods,Mood,MoodKey,MoodView,load_mood_requested,folder:Option<FolderKey>);
collection_points!(
    folder,
    folders,
    Folder,
    FolderKey,
    FolderView,
    load_folder_requested,
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
