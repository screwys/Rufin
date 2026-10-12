//! One ordered song relation for browsing and actions.

use std::{ops::Range, sync::Arc};

use sqlx::{Arguments, Connection, SqliteConnection};

use crate::{
    AlbumKey, ArtistKey, Database, FolderKey, GenreKey, HistoryRow, LibraryResult, MoodKey,
    PlaylistEntryKey, PlaylistEntryRow, PlaylistKey, ReadCancellation, RouteSeedWindow,
    ScrollSection, SmartPlaylistKey, SourceId, SourceKey, TrackKey, TrackRoutePage, TrackRow,
    TrackSort,
    source_window::{SourceQuery, selected_values_on},
};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SongScope {
    Tracks(SourceKey),
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
        source: Option<SourceKey>,
        now: i64,
    },
    History {
        source: Option<SourceId>,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SongQuery {
    pub scope: SongScope,
    pub folder: Option<FolderKey>,
    pub filter: String,
    pub sort: Option<TrackSort>,
    pub descending: bool,
    pub favorites_only: bool,
    pub downloaded_only: bool,
}

impl SongQuery {
    pub fn new(scope: SongScope) -> Self {
        Self {
            scope,
            folder: None,
            filter: String::new(),
            sort: None,
            descending: false,
            favorites_only: false,
            downloaded_only: false,
        }
    }

    pub fn selection(self) -> SongSelection {
        SongSelection::Query {
            query: self,
            ranges: None,
        }
    }

    pub(crate) fn natural_sort(&self) -> TrackSort {
        self.sort.unwrap_or(match self.scope {
            SongScope::Album(_) => TrackSort::TrackNumber,
            SongScope::Playlist(_) => TrackSort::Position,
            SongScope::History { .. } => TrackSort::LastPlayed,
            _ => TrackSort::Title,
        })
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SongSelection {
    Uris(Arc<[String]>),
    Tracks(Arc<[TrackKey]>),
    PlaylistEntries(Arc<[PlaylistEntryKey]>),
    Query {
        query: SongQuery,
        ranges: Option<Vec<Range<usize>>>,
    },
    Concat(Vec<SongSelection>),
}

pub(crate) async fn song_query_on(
    connection: &mut SqliteConnection,
    input: &SongQuery,
) -> LibraryResult<SourceQuery> {
    let sort = input.natural_sort();
    let mut query = match &input.scope {
        SongScope::Smart { key, source, now } => {
            crate::smart_playlists::smart_query(
                connection,
                *source,
                *key,
                input.folder,
                &input.filter,
                input.sort,
                input.descending,
                *now,
            )
            .await?
        }
        SongScope::History { source } => {
            crate::activity::history_query(source.as_ref(), &input.filter, input.descending)
        }
        SongScope::Playlist(key) => crate::playlists::playlist_query(
            *key,
            input.folder,
            sort,
            input.descending,
            &input.filter,
        ),
        scope => {
            let predicate = match scope {
                SongScope::Tracks(key) => format!("track.source_key={}", key.raw()),
                SongScope::Objects { source, .. } => format!(
                    "track.source_key={} AND track.object_id IN (SELECT value FROM json_each(?1))",
                    source.raw(),
                ),
                SongScope::Album(key) => format!("track.album_key={}", key.raw()),
                SongScope::Artist { key, album_artist } => {
                    let (field, table) = if *album_artist {
                        ("album_key", "album_artists")
                    } else {
                        ("track_key", "track_artists")
                    };
                    format!(
                        "track.{field} IN (SELECT {field} FROM {table} WHERE artist_key={})",
                        key.raw()
                    )
                }
                SongScope::Genre(key) => format!(
                    "track.track_key IN (SELECT track_key FROM track_genres WHERE genre_key={0} UNION SELECT member.track_key FROM album_genres credit JOIN tracks member USING(album_key) WHERE credit.genre_key={0})",
                    key.raw()
                ),
                SongScope::Mood(key) => format!(
                    "track.track_key IN (SELECT track_key FROM track_moods WHERE mood_key={})",
                    key.raw()
                ),
                _ => unreachable!(),
            };
            let mut arguments = sqlx::sqlite::SqliteArguments::default();
            if let SongScope::Objects { ids, .. } = scope {
                arguments
                    .add(serde_json::to_string(ids)?)
                    .map_err(sqlx::Error::Encode)?;
            }
            let mut query = SourceQuery {
                from: "tracks track".into(),
                predicate: format!("track.indexed=1 AND ({predicate})"),
                order: sort.order_terms(input.descending, false),
                uri: "track.media_uri".into(),
                entry_key: "NULL".into(),
                arguments,
            };
            crate::tracks::track_filter(&mut query, input.folder, &input.filter, false);
            query
        }
    };
    if input.favorites_only {
        crate::tracks::track_filter(&mut query, None, "", true);
    }
    if input.downloaded_only {
        crate::tracks::downloaded_filter(&mut query);
    }
    Ok(query)
}

async fn count_on(connection: &mut SqliteConnection, input: &SongQuery) -> LibraryResult<i64> {
    if let SongScope::Tracks(source) = input.scope
        && input.folder.is_none()
        && !input.downloaded_only
        && input.filter.trim().is_empty()
        && !input.favorites_only
    {
        return Ok(sqlx::query_scalar(
            "SELECT COALESCE((SELECT track_count FROM sources WHERE source_key=?1),0)",
        )
        .bind(source)
        .fetch_one(connection)
        .await?);
    }
    if input.favorites_only
        && !matches!(
            input.scope,
            SongScope::History { .. } | SongScope::Playlist(_) | SongScope::Smart { .. }
        )
    {
        let query = song_query_on(
            connection,
            &SongQuery {
                favorites_only: false,
                ..input.clone()
            },
        )
        .await?;
        // These sets are disjoint, so each can use its favorite index.
        return Ok(sqlx::query_scalar_with(sqlx::AssertSqlSafe(format!(
            "SELECT (SELECT count(*) FROM tracks track WHERE {} AND track.source_favorite=1
                AND COALESCE((SELECT favorite FROM user_media_state state WHERE state.media_uri=track.media_uri),1)=1)
             + (SELECT count(*) FROM user_media_state state CROSS JOIN tracks track ON track.media_uri=state.media_uri
                WHERE state.favorite=1 AND track.source_favorite=0 AND {})", query.predicate, query.predicate)), query.arguments.clone())
            .fetch_one(connection).await?);
    }
    song_query_on(connection, input)
        .await?
        .count(connection)
        .await
}

pub(crate) async fn selected_songs_on(
    connection: &mut SqliteConnection,
    selection: &SongSelection,
) -> LibraryResult<Vec<(TrackKey, Option<PlaylistEntryKey>)>> {
    selection_rows_on(connection, selection, "track.track_key,$entry", None).await
}

async fn selection_rows_on<T>(
    connection: &mut SqliteConnection,
    selection: &SongSelection,
    columns: &str,
    limit: Option<usize>,
) -> LibraryResult<Vec<T>>
where
    T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Send + Unpin,
{
    let mut pending = vec![selection];
    let mut rows = Vec::new();
    while let Some(selection) = pending.pop() {
        let remaining = limit.map(|limit| limit.saturating_sub(rows.len()));
        if remaining == Some(0) {
            break;
        }
        let (keys, from, entry) = match selection {
            SongSelection::Concat(selections) => {
                pending.extend(selections.iter().rev());
                continue;
            }
            SongSelection::Query { query, ranges } => {
                let query = song_query_on(connection, query).await?;
                rows.extend(
                    crate::source_window::selected_rows_on(
                        connection,
                        &query,
                        &columns.replace("$entry", &query.entry_key),
                        ranges.as_deref(),
                        remaining,
                    )
                    .await?,
                );
                continue;
            }
            SongSelection::Uris(uris) => (
                serde_json::to_string(uris)?,
                "json_each(?1) requested JOIN tracks track ON track.media_uri=requested.value",
                "NULL",
            ),
            SongSelection::Tracks(keys) => (
                serde_json::to_string(keys)?,
                "json_each(?1) requested JOIN tracks track ON track.track_key=requested.value",
                "NULL",
            ),
            SongSelection::PlaylistEntries(keys) => (
                serde_json::to_string(keys)?,
                "json_each(?1) requested JOIN playlist_entries entry ON entry.playlist_entry_key=requested.value JOIN tracks track USING(track_key)",
                "entry.playlist_entry_key",
            ),
        };
        rows.extend(
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT {} FROM {from} ORDER BY requested.key LIMIT ?2",
                columns.replace("$entry", entry)
            )))
            .bind(keys)
            .bind(remaining.map_or(-1, |limit| limit.min(i64::MAX as usize) as i64))
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?,
        );
    }
    Ok(rows)
}

impl Database {
    pub async fn song_count(
        &self,
        query: &SongQuery,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<i64> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        count_on(&mut connection, query).await
    }

    pub async fn song_rows(
        &self,
        query: &SongQuery,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let query = song_query_on(&mut transaction, query).await?;
        let keys = selected_values_on(
            &mut transaction,
            &query,
            "track.track_key",
            &[offset..offset.saturating_add(limit.min(256))],
        )
        .await?;
        let rows = crate::tracks::load_track_rows(&mut transaction, &keys).await?;
        transaction.commit().await?;
        Ok(rows)
    }

    pub async fn song_playlist_rows(
        &self,
        query: &SongQuery,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistEntryRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let query = song_query_on(&mut transaction, query).await?;
        let keys = selected_values_on(
            &mut transaction,
            &query,
            &query.entry_key,
            &[offset..offset.saturating_add(limit.min(256))],
        )
        .await?;
        let rows = crate::playlists::load_playlist_entry_rows(&mut transaction, &keys).await?;
        transaction.commit().await?;
        Ok(rows)
    }

    pub async fn song_history_rows(
        &self,
        query: &SongQuery,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<HistoryRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let query = song_query_on(&mut connection, query).await?;
        crate::activity::load_history_page(&mut connection, &query, offset, limit).await
    }

    pub async fn song_sections(
        &self,
        input: &SongQuery,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ScrollSection>> {
        let sort = input.natural_sort();
        if !crate::scroll_sections::text_sort(sort)
            || matches!(input.scope, SongScope::Smart { .. }) && input.sort.is_none()
        {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let query = song_query_on(&mut connection, input).await?;
        let sql = crate::scroll_sections::section_sql(
            &query.select("track.track_key"),
            if sort == TrackSort::Title {
                "track.title"
            } else {
                crate::scroll_sections::order_text(&query.order[0])
            },
        );
        crate::scroll_sections::decode_sections(
            sqlx::query_scalar_with(sqlx::AssertSqlSafe(sql), query.arguments.clone())
                .persistent(false)
                .fetch_all(&mut *connection)
                .await?,
        )
    }

    pub async fn song_disc_sections(
        &self,
        input: &SongQuery,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(u32, i64)>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let query = song_query_on(&mut connection, input).await?;
        crate::collections::album_disc_sections(
            &query,
            input.natural_sort(),
            input.descending,
            &mut connection,
        )
        .await
    }

    pub async fn song_route_page(
        &self,
        input: &SongQuery,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<TrackRoutePage> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let count = count_on(&mut transaction, input).await?.max(0) as usize;
        let query = song_query_on(&mut transaction, input).await?;
        let range = window.range(count);
        let disc_sections = if matches!(input.scope, SongScope::Album(_)) {
            crate::collections::album_disc_sections(
                &query,
                input.natural_sort(),
                input.descending,
                &mut transaction,
            )
            .await?
        } else {
            Vec::new()
        };
        let keys = selected_values_on(
            &mut transaction,
            &query,
            "track.track_key",
            std::slice::from_ref(&range),
        )
        .await?;
        let first_rows = crate::tracks::load_track_rows(&mut transaction, &keys).await?;
        transaction.commit().await?;
        Ok(TrackRoutePage {
            query: input.clone(),
            count,
            disc_sections,
            first_row_position: range.start,
            first_rows,
        })
    }

    pub async fn selected_track_keys(
        &self,
        selection: &SongSelection,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let rows = selected_songs_on(&mut transaction, selection).await?;
        transaction.commit().await?;
        Ok(rows.into_iter().map(|(key, _)| key).collect())
    }

    pub async fn selected_track_uris(
        &self,
        selection: &SongSelection,
        limit: Option<usize>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        if limit == Some(0) {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let rows =
            selection_rows_on::<(String,)>(&mut transaction, selection, "track.media_uri", limit)
                .await?;
        transaction.commit().await?;
        Ok(rows.into_iter().map(|(uri,)| uri).collect())
    }
}
