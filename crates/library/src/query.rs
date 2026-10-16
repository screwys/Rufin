//! One ordered relation for catalog counts, windows, sections and actions.
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CatalogSubject {
    PlaylistEntries {
        keys: Arc<[PlaylistEntryKey]>,
    },
    Uris {
        kind: CatalogKind,
        uris: Arc<[String]>,
        album_artist: bool,
    },
    Keys {
        kind: CatalogKind,
        source: Option<SourceKey>,
        keys: Arc<[i64]>,
        album_artist: bool,
    },
    Tracks {
        scope: TrackScope,
        sort: Option<CatalogSort>,
    },
    Albums {
        source: SourceKey,
        sort: CatalogSort,
        inline_tracks: Option<usize>,
    },
    Artists {
        source: SourceKey,
        sort: CatalogSort,
        album_artist: bool,
    },
    Genres {
        source: SourceKey,
        sort: CatalogSort,
    },
    Moods {
        source: SourceKey,
        sort: CatalogSort,
    },
    Playlists {
        source: Option<SourceKey>,
        sort: CatalogSort,
    },
    SmartPlaylists {
        source: Option<SourceKey>,
        sort: CatalogSort,
        now: i64,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CatalogQuery {
    pub subject: CatalogSubject,
    pub folder: Option<FolderKey>,
    pub filter: String,
    pub descending: bool,
    pub favorites_only: bool,
    pub downloaded_only: bool,
    pub activity: bool,
}

#[derive(Clone)]
pub enum CatalogRows {
    Folders(Vec<FolderView>),
    Tracks(Vec<TrackView>),
    PlaylistEntries(Vec<PlaylistEntryView>),
    Albums(Vec<AlbumView>),
    AlbumDetails(Vec<AlbumDetailRow>),
    Artists(Vec<ArtistView>),
    Genres(Vec<GenreView>),
    Moods(Vec<MoodView>),
    Playlists(Vec<PlaylistRow>),
    SmartPlaylists(Vec<SmartPlaylistRow>),
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
    SmartPlaylist,
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
            Self::SmartPlaylist => ("smart_playlists", "smart_playlist"),
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

#[derive(Clone)]
pub struct CatalogPage {
    pub query: CatalogQuery,
    pub count: usize,
    pub first_row_position: usize,
    pub rows: CatalogRows,
    pub disc_sections: Vec<(u32, i64)>,
}

impl CatalogQuery {
    pub fn activity_sensitive(&self) -> bool {
        self.activity
            || match &self.subject {
                CatalogSubject::SmartPlaylists { .. }
                | CatalogSubject::Keys {
                    kind: CatalogKind::SmartPlaylist,
                    ..
                }
                | CatalogSubject::Uris {
                    kind: CatalogKind::SmartPlaylist,
                    ..
                }
                | CatalogSubject::Tracks {
                    scope: TrackScope::Smart { .. } | TrackScope::History { .. },
                    ..
                } => true,
                CatalogSubject::Tracks { sort, .. } => {
                    sort.is_some_and(CatalogSort::activity_sensitive)
                }
                CatalogSubject::Albums { sort, .. }
                | CatalogSubject::Artists { sort, .. }
                | CatalogSubject::Genres { sort, .. }
                | CatalogSubject::Moods { sort, .. }
                | CatalogSubject::Playlists { sort, .. } => sort.activity_sensitive(),
                _ => false,
            }
    }

    pub fn playlist_entries(keys: Vec<PlaylistEntryKey>) -> Self {
        Self {
            subject: CatalogSubject::PlaylistEntries { keys: keys.into() },
            folder: None,
            filter: String::new(),
            descending: false,
            favorites_only: false,
            downloaded_only: false,
            activity: false,
        }
    }

    pub fn uris(kind: CatalogKind, uris: Vec<String>) -> Self {
        Self {
            subject: CatalogSubject::Uris {
                kind,
                uris: uris.into(),
                album_artist: false,
            },
            folder: None,
            filter: String::new(),
            descending: false,
            favorites_only: false,
            downloaded_only: false,
            activity: false,
        }
    }
    pub fn keys(kind: CatalogKind, source: Option<SourceKey>, keys: Vec<i64>) -> Self {
        Self {
            subject: CatalogSubject::Keys {
                kind,
                source,
                keys: keys.into(),
                album_artist: false,
            },
            folder: None,
            filter: String::new(),
            descending: false,
            favorites_only: false,
            downloaded_only: false,
            activity: false,
        }
    }

    pub fn tracks(scope: TrackScope) -> Self {
        Self {
            subject: CatalogSubject::Tracks { scope, sort: None },
            folder: None,
            filter: String::new(),
            descending: false,
            favorites_only: false,
            downloaded_only: false,
            activity: false,
        }
    }
    pub fn selection(self) -> TrackSelection {
        TrackSelection::Query {
            query: self,
            ranges: None,
        }
    }
    pub fn track_sort_option(&self) -> Option<CatalogSort> {
        match self.subject {
            CatalogSubject::Tracks { sort, .. } => sort,
            _ => None,
        }
    }
    pub fn track_sort(&self) -> CatalogSort {
        let CatalogSubject::Tracks { scope, sort } = &self.subject else {
            return CatalogSort::Title;
        };
        sort.unwrap_or(match scope {
            TrackScope::Album(_) => CatalogSort::TrackNumber,
            TrackScope::Playlist(_) => CatalogSort::Position,
            TrackScope::History { .. } => CatalogSort::LastPlayed,
            _ => CatalogSort::Title,
        })
    }
    pub async fn count(
        &self,
        database: &Database,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<usize> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        Ok(count_on(&mut connection, self).await?.max(0) as usize)
    }
    pub async fn rows(
        &self,
        database: &Database,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<CatalogRows> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let result = rows_on(&mut transaction, self, offset, limit.min(256)).await?;
        transaction.commit().await?;
        Ok(result)
    }
    pub async fn page(
        &self,
        database: &Database,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<CatalogPage> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let count = count_on(&mut transaction, self).await?.max(0) as usize;
        let range = window.range(count);
        let rows = rows_on(&mut transaction, self, range.start, range.len()).await?;
        let disc_sections = if matches!(
            self.subject,
            CatalogSubject::Tracks {
                scope: TrackScope::Album(_),
                ..
            }
        ) {
            let query = compile_on(&mut transaction, self).await?;
            crate::collections::album_disc_sections(
                &query,
                self.track_sort(),
                self.descending,
                &mut transaction,
            )
            .await?
        } else {
            Vec::new()
        };
        transaction.commit().await?;
        Ok(CatalogPage {
            query: self.clone(),
            count,
            first_row_position: range.start,
            rows,
            disc_sections,
        })
    }
    pub async fn sections(
        &self,
        database: &Database,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ScrollSection>> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let query = compile_on(&mut connection, self).await?;
        let Some(text) = &query.text else {
            return Ok(Vec::new());
        };
        let sql = query.section_sql(text);
        crate::scroll_sections::decode_sections(
            sqlx::query_scalar_with(sqlx::AssertSqlSafe(sql), query.arguments)
                .fetch_all(&mut *connection)
                .await?,
        )
    }
    pub async fn disc_sections(
        &self,
        database: &Database,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(u32, i64)>> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let query = compile_on(&mut connection, self).await?;
        crate::collections::album_disc_sections(
            &query,
            self.track_sort(),
            self.descending,
            &mut connection,
        )
        .await
    }
}

pub(crate) async fn compile_on(
    connection: &mut SqliteConnection,
    query: &CatalogQuery,
) -> LibraryResult<OrderedQuery> {
    match query.subject {
        CatalogSubject::PlaylistEntries { ref keys } => {
            let mut arguments = SqliteArguments::default();
            arguments
                .add(serde_json::to_string(keys)?)
                .map_err(sqlx::Error::Encode)?;
            Ok(OrderedQuery {albumless:false,key:"entry.playlist_entry_key".into(),text:None,from:"json_each(?1) requested JOIN playlist_entries entry ON entry.playlist_entry_key=requested.value JOIN tracks track USING(track_key)".into(),predicate:"1".into(),order:vec!["CAST(requested.key AS INTEGER)".into()],uri:"track.media_uri".into(),entry_key:"entry.playlist_entry_key".into(),arguments})
        }

        CatalogSubject::Uris { kind, ref uris, .. } => {
            let (table, name) = kind.sql();
            let alias = if kind == CatalogKind::Track {
                "track"
            } else {
                "entity"
            };
            let mut arguments = SqliteArguments::default();
            arguments
                .add(serde_json::to_string(uris)?)
                .map_err(sqlx::Error::Encode)?;
            Ok(OrderedQuery {
                albumless: false,
                key: format!("{alias}.{name}_key"),
                text: None,
                from: format!(
                    "json_each(?1) requested JOIN {table} {alias} ON {alias}.media_uri=requested.value"
                ),
                predicate: "1".into(),
                order: vec!["CAST(requested.key AS INTEGER)".into()],
                uri: format!("{alias}.media_uri"),
                entry_key: "NULL".into(),
                arguments,
            })
        }
        CatalogSubject::Keys {
            kind,
            source,
            ref keys,
            ..
        } => {
            let (table, name) = kind.sql();
            let alias = if kind == CatalogKind::Track {
                "track"
            } else {
                "entity"
            };
            let mut arguments = SqliteArguments::default();
            arguments
                .add(serde_json::to_string(keys)?)
                .map_err(sqlx::Error::Encode)?;
            Ok(OrderedQuery {
                albumless: false,
                key: format!("{alias}.{name}_key"),
                text: None,
                from: format!(
                    "json_each(?1) requested JOIN {table} {alias} ON {alias}.{name}_key=requested.value"
                ),
                predicate: source.map_or_else(
                    || "1".into(),
                    |key| format!("{alias}.source_key={}", key.raw()),
                ),
                order: vec!["CAST(requested.key AS INTEGER)".into()],
                uri: format!("{alias}.media_uri"),
                entry_key: "NULL".into(),
                arguments,
            })
        }
        CatalogSubject::Tracks { .. } => track_query_on(connection, query).await,
        CatalogSubject::SmartPlaylists { source, sort, now } => {
            crate::smart_playlists::smart_list_query(
                connection,
                source,
                query.folder,
                sort,
                query.descending,
                &query.filter,
                now,
            )
            .await
        }
        _ => Ok(crate::collections::collection_query(query)),
    }
}

async fn count_on(connection: &mut SqliteConnection, query: &CatalogQuery) -> LibraryResult<i64> {
    let relation = compile_on(connection, query).await?;
    if let CatalogSubject::Albums {
        inline_tracks: Some(inline),
        ..
    } = query.subject
    {
        return Ok(sqlx::query_scalar_with(
            sqlx::AssertSqlSafe(format!(
                "SELECT COALESCE(sum(1+max({}-{},0)),0) FROM {} WHERE {}",
                crate::collections::album_detail_track_count(query.folder),
                inline,
                relation.from,
                relation.predicate
            )),
            relation.arguments,
        )
        .fetch_one(connection)
        .await?);
    }
    relation.count(connection).await
}

async fn rows_on(
    connection: &mut SqliteConnection,
    input: &CatalogQuery,
    offset: usize,
    limit: usize,
) -> LibraryResult<CatalogRows> {
    let query = compile_on(connection, input).await?;
    if matches!(
        input.subject,
        CatalogSubject::Tracks {
            scope: TrackScope::History { .. },
            ..
        }
    ) {
        return Ok(CatalogRows::Tracks(
            crate::activity::load_history_page(connection, &query, offset, limit).await?,
        ));
    }
    if let CatalogSubject::Albums {
        source,
        sort,
        inline_tracks: Some(inline),
    } = input.subject
    {
        return Ok(CatalogRows::AlbumDetails(
            crate::collections::load_album_details(
                connection, input, source, sort, inline, offset, limit,
            )
            .await?,
        ));
    }
    let range = offset..offset.saturating_add(limit);
    let keys: Vec<i64> =
        selected_values_on(connection, &query, &query.key, std::slice::from_ref(&range)).await?;
    load_keys_on(connection, input, keys).await
}

pub(crate) async fn load_keys_on(
    connection: &mut SqliteConnection,
    input: &CatalogQuery,
    keys: Vec<i64>,
) -> LibraryResult<CatalogRows> {
    use CatalogSubject as S;
    let (kind, role, source, now) = match input.subject {
        S::Keys {
            kind,
            source,
            album_artist,
            ..
        } => (kind, album_artist, source, 0),
        S::Uris {
            kind, album_artist, ..
        } => (kind, album_artist, None, 0),
        S::PlaylistEntries { .. }
        | S::Tracks {
            scope: TrackScope::Playlist(_),
            ..
        } => {
            return Ok(CatalogRows::PlaylistEntries(
                crate::playlists::load_playlist_entry_rows(
                    connection,
                    &keys
                        .into_iter()
                        .map(PlaylistEntryKey::from_raw)
                        .collect::<Vec<_>>(),
                )
                .await?,
            ));
        }
        S::Tracks { .. } => (CatalogKind::Track, false, None, 0),
        S::Albums { .. } => (CatalogKind::Album, false, None, 0),
        S::Artists { album_artist, .. } => (CatalogKind::Artist, album_artist, None, 0),
        S::Genres { .. } => (CatalogKind::Genre, false, None, 0),
        S::Moods { .. } => (CatalogKind::Mood, false, None, 0),
        S::Playlists { .. } => (CatalogKind::Playlist, false, None, 0),
        S::SmartPlaylists { source, now, .. } => (CatalogKind::SmartPlaylist, false, source, now),
    };
    Ok(match kind {
        CatalogKind::Track => {
            let rows = crate::tracks::load_track_rows(
                connection,
                &keys.into_iter().map(TrackKey::from_raw).collect::<Vec<_>>(),
            )
            .await?;
            CatalogRows::Tracks(rows)
        }
        CatalogKind::Album => CatalogRows::Albums(
            crate::collections::load_album_rows(
                connection,
                &keys.into_iter().map(AlbumKey::from_raw).collect::<Vec<_>>(),
                input.activity,
            )
            .await?,
        ),
        CatalogKind::Artist => CatalogRows::Artists(
            crate::collections::load_artist_rows(
                connection,
                &keys
                    .into_iter()
                    .map(ArtistKey::from_raw)
                    .collect::<Vec<_>>(),
                role,
                input.activity,
            )
            .await?,
        ),
        CatalogKind::Genre => CatalogRows::Genres(
            crate::collections::load_genre_rows(
                connection,
                &keys.into_iter().map(GenreKey::from_raw).collect::<Vec<_>>(),
                input.folder,
            )
            .await?,
        ),
        CatalogKind::Mood => CatalogRows::Moods(
            crate::collections::load_mood_rows(
                connection,
                &keys.into_iter().map(MoodKey::from_raw).collect::<Vec<_>>(),
                input.folder,
            )
            .await?,
        ),
        CatalogKind::Folder => CatalogRows::Folders(
            crate::collections::load_folder_rows(
                connection,
                &keys
                    .into_iter()
                    .map(FolderKey::from_raw)
                    .collect::<Vec<_>>(),
            )
            .await?,
        ),
        CatalogKind::Playlist => CatalogRows::Playlists(
            Database::load_playlist_rows(
                connection,
                &keys
                    .into_iter()
                    .map(PlaylistKey::from_raw)
                    .collect::<Vec<_>>(),
            )
            .await?,
        ),
        CatalogKind::SmartPlaylist => CatalogRows::SmartPlaylists(
            crate::smart_playlists::load_smart_playlist_rows(
                connection,
                source,
                &keys
                    .into_iter()
                    .map(SmartPlaylistKey::from_raw)
                    .collect::<Vec<_>>(),
                input.folder,
                now,
            )
            .await?,
        ),
    })
}

// Shared ordering and bounded reads.
use crate::*;
use sqlx::Connection;
use sqlx::{Arguments, SqliteConnection, sqlite::SqliteArguments};
pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) struct OrderedQuery {
    pub key: String,
    pub text: Option<String>,
    pub albumless: bool,
    pub from: String,
    pub predicate: String,
    pub order: Vec<String>,
    pub uri: String,
    pub entry_key: String,
    pub arguments: SqliteArguments,
}

impl OrderedQuery {
    pub async fn count(&self, connection: &mut SqliteConnection) -> LibraryResult<i64> {
        let base = format!(
            "SELECT count(*) FROM {} WHERE {}",
            self.from, self.predicate
        );
        let sql = if self.albumless {
            format!(
                "SELECT ({base})+(SELECT count(*) FROM tracks track WHERE track.album_key IS NULL AND {})",
                self.predicate
            )
        } else {
            base
        };
        Ok(
            sqlx::query_scalar_with(sqlx::AssertSqlSafe(sql), self.arguments.clone())
                .fetch_one(connection)
                .await?,
        )
    }
    fn order_aliases(&self) -> Vec<String> {
        self.order
            .iter()
            .enumerate()
            .map(|(index, term)| {
                let expression = order_expression(term);
                format!(
                    "__order{index}{}",
                    term.strip_prefix(expression)
                        .expect("order expression prefix")
                )
            })
            .collect()
    }
    pub fn select(&self, columns: &str) -> String {
        if !self.albumless {
            return format!(
                "SELECT {columns} FROM {} WHERE {} ORDER BY {}",
                self.from,
                self.predicate,
                self.order.join(",")
            );
        }
        let expressions = self
            .order
            .iter()
            .map(|term| order_expression(term))
            .collect::<Vec<_>>();
        let extra = expressions
            .iter()
            .enumerate()
            .map(|(index, term)| format!("{term} __order{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let missing = expressions
            .iter()
            .map(|term| {
                term.replace("ordering.album_artist_sort", "NULL")
                    .replace("ordering.title_sort", "NULL")
                    .replace("ordering.year_sort", "NULL")
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "SELECT {columns},{extra} FROM {} WHERE {} UNION ALL SELECT {columns},{missing} FROM tracks track WHERE track.album_key IS NULL AND {} ORDER BY {}",
            self.from,
            self.predicate,
            self.predicate,
            self.order_aliases().join(",")
        )
    }
    fn section_sql(&self, text: &str) -> String {
        if self.albumless {
            let input = self.select(&format!("{text} section_text"));
            crate::scroll_sections::section_sql(
                &format!(
                    "SELECT section_text FROM ({input}) ordered ORDER BY {}",
                    self.order_aliases().join(",")
                ),
                "section_text",
            )
        } else {
            crate::scroll_sections::section_sql(&self.select(&self.key), text)
        }
    }
}

fn order_expression(term: &str) -> &str {
    let term = term.strip_suffix(" NULLS LAST").unwrap_or(term);
    term.strip_suffix(" ASC")
        .or_else(|| term.strip_suffix(" DESC"))
        .unwrap_or(term)
}

pub(crate) async fn selected_values_on<T>(
    connection: &mut SqliteConnection,
    query: &OrderedQuery,
    column: &str,
    ranges: &[std::ops::Range<usize>],
) -> LibraryResult<Vec<T>>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> + Send + Unpin,
{
    Ok(
        selected_rows_on::<(T,)>(connection, query, column, Some(ranges), None)
            .await?
            .into_iter()
            .map(|(value,)| value)
            .collect(),
    )
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

pub(crate) async fn track_query_on(
    connection: &mut SqliteConnection,
    input: &CatalogQuery,
) -> LibraryResult<OrderedQuery> {
    let sort = input.track_sort();
    let crate::CatalogSubject::Tracks { scope, .. } = &input.subject else {
        unreachable!()
    };
    let mut query = match scope {
        TrackScope::Smart { key, source, now } => {
            crate::smart_playlists::smart_query(
                connection,
                *source,
                *key,
                input.folder,
                &input.filter,
                input.track_sort_option(),
                input.descending,
                *now,
            )
            .await?
        }
        TrackScope::History { source } => {
            crate::activity::history_query(source.as_ref(), &input.filter, input.descending)
        }
        TrackScope::Playlist(key) => crate::playlists::playlist_query(
            *key,
            input.folder,
            sort,
            input.descending,
            &input.filter,
        ),
        scope => {
            let predicate = match scope {
                TrackScope::Tracks(key) => format!("track.source_key={}", key.raw()),
                TrackScope::Objects { source, .. } => format!(
                    "track.source_key={} AND track.object_id IN (SELECT value FROM json_each(?1))",
                    source.raw(),
                ),
                TrackScope::Album(key) => format!("track.album_key={}", key.raw()),
                TrackScope::Artist { key, album_artist } => {
                    if *album_artist {
                        crate::collections::album_artist_member("track", &key.raw().to_string())
                    } else {
                        format!(
                            "track.track_key IN (SELECT track_key FROM track_artists WHERE artist_key={})",
                            key.raw()
                        )
                    }
                }
                TrackScope::Genre(key) => {
                    crate::collections::genre_member("track", &key.raw().to_string())
                }
                TrackScope::Mood(key) => format!(
                    "track.track_key IN (SELECT track_key FROM track_moods WHERE mood_key={})",
                    key.raw()
                ),
                _ => unreachable!(),
            };
            let mut arguments = sqlx::sqlite::SqliteArguments::default();
            if let TrackScope::Objects { ids, .. } = scope {
                arguments
                    .add(serde_json::to_string(ids)?)
                    .map_err(sqlx::Error::Encode)?;
            }
            let mut query = OrderedQuery {
                albumless: false,
                from: "tracks track".into(),
                predicate: format!("track.indexed=1 AND ({predicate})"),
                order: sort.order_terms(input.descending, false),
                key: "track.track_key".into(),
                text: crate::scroll_sections::text_sort(sort).then(|| {
                    if sort == CatalogSort::Title {
                        "track.title".into()
                    } else {
                        crate::scroll_sections::order_text(
                            &sort.order_terms(input.descending, false)[0],
                        )
                        .into()
                    }
                }),
                uri: "track.media_uri".into(),
                entry_key: "NULL".into(),
                arguments,
            };
            crate::tracks::track_filter(&mut query, input.folder, &input.filter, false);
            query
        }
    };
    if matches!(sort, CatalogSort::Album | CatalogSort::AlbumArtist)
        && matches!(
            scope,
            TrackScope::Tracks(_)
                | TrackScope::Objects { .. }
                | TrackScope::Album(_)
                | TrackScope::Artist { .. }
                | TrackScope::Genre(_)
                | TrackScope::Mood(_)
        )
    {
        query.from="album_order ordering CROSS JOIN tracks track ON track.album_key=ordering.album_key AND track.source_key=ordering.source_key".into();
        query.albumless = true;
        for term in &mut query.order {
            *term = term
                .replace(
                    "(SELECT album_artist_sort FROM album_order WHERE album_key=track.album_key)",
                    "ordering.album_artist_sort",
                )
                .replace(
                    "(SELECT title_sort FROM album_order WHERE album_key=track.album_key)",
                    "ordering.title_sort",
                )
                .replace(
                    "(SELECT year_sort FROM album_order WHERE album_key=track.album_key)",
                    "ordering.year_sort",
                );
        }
    }
    if input.favorites_only {
        crate::tracks::track_filter(&mut query, None, "", true);
    }
    if input.downloaded_only {
        crate::tracks::downloaded_filter(&mut query);
    }
    Ok(query)
}
