//! Owns Playlist identity, exact duplicate occurrences, ordering, filtering, and edits.
//! Playback queue state is a separate SQLite owner.

use sqlx::sqlite::SqliteRow;
use sqlx::{Connection, FromRow, QueryBuilder, Row, Sqlite, SqliteConnection};
use std::collections::BTreeMap;

use crate::{
    Database, FolderKey, LibraryError, LibraryResult, PlaylistEntryKey, PlaylistKey,
    ReadCancellation, RouteSeedWindow, SourceKey,
};

const PLAYLIST_ROW_LIMIT: usize = 128;
const PLAYLIST_ENTRY_ROW_LIMIT: usize = 256;
const PLAYLIST_DELETE_BATCH: usize = 400;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistSort {
    Position,
    Title,
    TrackCount,
    Duration,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PlaylistEntrySort {
    Position,
    Title,
    Artist,
    Album,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistRow {
    pub playlist_key: PlaylistKey,
    pub source_key: Option<SourceKey>,
    pub object_id: String,
    pub name: String,
    pub artwork_binding: Option<Vec<u8>>,
    pub track_count: i64,
    pub duration_millis: i64,
    pub downloaded_count: i64,
    pub representative_artwork: Vec<Vec<u8>>,
    pub genres: Vec<PlaylistGenreLink>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistGenreLink {
    pub genre_key: crate::GenreKey,
    pub name: String,
}

impl<'row> FromRow<'row, SqliteRow> for PlaylistRow {
    fn from_row(row: &'row SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            playlist_key: row.try_get("playlist_key")?,
            source_key: row.try_get("source_key")?,
            object_id: row.try_get("object_id")?,
            name: row.try_get("name")?,
            artwork_binding: row.try_get("artwork_binding")?,
            track_count: row.try_get("track_count")?,
            duration_millis: row.try_get("duration_millis")?,
            downloaded_count: row.try_get("downloaded_count")?,
            representative_artwork: Vec::new(),
            genres: Vec::new(),
        })
    }
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct PlaylistEntryRow {
    pub playlist_entry_key: PlaylistEntryKey,
    pub position: i64,
    pub media_uri: String,
    pub source_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_media_uri: Option<String>,
    #[sqlx(skip)]
    pub artists: Vec<crate::TrackArtistLink>,
    #[sqlx(skip)]
    pub album_artists: Vec<crate::TrackArtistLink>,
    pub album_display_artist: Option<String>,
    pub artwork_binding: Option<Vec<u8>>,
    pub duration_millis: i64,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub bpm: Option<i64>,
    pub genre: String,
    pub play_count: i64,
    pub last_played: Option<i64>,
    pub source_format: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub favorite: bool,
    pub rating: Option<i64>,
    pub is_downloaded: bool,
}

#[derive(Clone)]
pub struct PlaylistDetailPage {
    pub summary: PlaylistRow,
    pub order: Vec<PlaylistEntryKey>,
    pub first_row_position: usize,
    pub first_rows: Vec<PlaylistEntryRow>,
}

impl Database {
    pub async fn playlist_owner(
        &self,
        playlist: PlaylistKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<(Option<SourceKey>, Option<String>)>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, (Option<SourceKey>, Option<String>)>(
            "SELECT playlist.source_key,source.object_id
             FROM playlists playlist LEFT JOIN sources source USING(source_key)
             WHERE playlist.playlist_key=?1",
        )
        .bind(playlist)
        .fetch_optional(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn playlist_route_page(
        &self,
        source: impl Into<Option<SourceKey>>,
        folder: Option<FolderKey>,
        sort: PlaylistSort,
        descending: bool,
        filter: &str,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(Vec<PlaylistKey>, usize, Vec<PlaylistRow>)> {
        let source = source.into();
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let order =
            Self::load_playlist_order(&mut transaction, source, folder, sort, descending, filter)
                .await?;
        let seed = window.range(order.len());
        let first_row_position = seed.start;
        let first_rows = Self::load_playlist_rows(&mut transaction, &order[seed]).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok((order, first_row_position, first_rows))
    }

    pub async fn playlist_key_by_object(
        &self,
        source: SourceKey,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar(
            "SELECT playlist_key FROM playlists WHERE source_key=?1 AND object_id=?2",
        )
        .bind(source)
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn global_playlist_key_by_object(
        &self,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar(
            "SELECT playlist_key FROM playlists WHERE source_key IS NULL AND object_id=?1",
        )
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn all_playlist_track_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, String>(
            "SELECT track.media_uri FROM tracks track
             WHERE track.source_key=?1
               AND EXISTS (SELECT 1 FROM playlist_entries entry WHERE entry.media_uri=track.media_uri)
               AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?2))
             ORDER BY track.sort_text,track.track_key",
        )
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn playlist_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        sort: PlaylistSort,
        descending: bool,
        filter: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = Self::load_playlist_order(
            &mut connection,
            Some(source),
            folder,
            sort,
            descending,
            filter,
        )
        .await;
        Database::clear_progress(&mut connection).await?;
        result
    }

    async fn load_playlist_order(
        connection: &mut SqliteConnection,
        source: Option<SourceKey>,
        folder: Option<FolderKey>,
        sort: PlaylistSort,
        descending: bool,
        filter: &str,
    ) -> LibraryResult<Vec<PlaylistKey>> {
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        let aggregate = matches!(sort, PlaylistSort::TrackCount | PlaylistSort::Duration);
        let mut query =
            QueryBuilder::<Sqlite>::new("SELECT playlist.playlist_key FROM playlists playlist");
        if aggregate {
            query.push(" LEFT JOIN playlist_entries entry USING(playlist_key) LEFT JOIN tracks track USING(media_uri)");
        }
        query.push(" WHERE (playlist.source_key=").push_bind(source)
            .push(" OR playlist.source_key IS NULL) AND (playlist.source_key IS NULL OR ")
            .push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM playlist_entries scoped_entry JOIN tracks scoped_track USING(media_uri) JOIN track_folders scope USING(track_key) WHERE scoped_entry.playlist_key=playlist.playlist_key AND scope.folder_key=").push_bind(folder).push("))");
        if !filter.is_empty() {
            query
                .push(" AND instr(playlist.normalized_name,")
                .push_bind(filter)
                .push(")>0");
        }
        if aggregate {
            query.push(" AND (playlist.source_key IS NULL OR ").push_bind(folder)
                .push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=")
                .push_bind(folder).push(")) GROUP BY playlist.playlist_key");
        }
        query.push(" ORDER BY ").push(match sort {
            PlaylistSort::Position => "playlist.position",
            PlaylistSort::Title => "playlist.sort_text",
            PlaylistSort::TrackCount => "count(entry.playlist_entry_key)",
            PlaylistSort::Duration => {
                "COALESCE(sum(COALESCE(track.duration_millis,entry.duration_millis)),0)"
            }
        });
        if descending {
            query.push(" DESC");
        }
        query.push(",playlist.sort_text,playlist.playlist_key");
        Ok(query
            .build_query_scalar()
            .persistent(false)
            .fetch_all(connection)
            .await?)
    }

    pub async fn playlist_destinations(
        &self,
        source: Option<SourceKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let order = Self::load_playlist_order(
            &mut transaction,
            source,
            None,
            PlaylistSort::Position,
            false,
            "",
        )
        .await?;
        let mut rows = Vec::with_capacity(order.len());
        for keys in order.chunks(PLAYLIST_ROW_LIMIT) {
            rows.extend(Self::load_playlist_rows(&mut transaction, keys).await?);
        }
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(rows)
    }

    pub async fn playlist_entry_order(
        &self,
        playlist: PlaylistKey,
        folder: Option<FolderKey>,
        sort: PlaylistEntrySort,
        descending: bool,
        filter: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistEntryKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = Self::load_playlist_entry_order(
            &mut connection,
            playlist,
            folder,
            sort,
            descending,
            filter,
        )
        .await;
        Database::clear_progress(&mut connection).await?;
        result
    }

    pub async fn playlist_detail_page(
        &self,
        playlist: PlaylistKey,
        folder: Option<FolderKey>,
        sort: PlaylistEntrySort,
        descending: bool,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistDetailPage>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let summary = Self::load_playlist_rows(&mut transaction, &[playlist])
            .await?
            .pop();
        let page = if let Some(summary) = summary {
            let order = Self::load_playlist_entry_order(
                &mut transaction,
                playlist,
                folder,
                sort,
                descending,
                "",
            )
            .await?;
            let range = window.range(order.len());
            let first_row_position = range.start;
            let first_rows = load_playlist_entry_rows(&mut transaction, &order[range]).await?;
            Some(PlaylistDetailPage {
                summary,
                order,
                first_row_position,
                first_rows,
            })
        } else {
            None
        };
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(page)
    }

    async fn load_playlist_entry_order(
        connection: &mut SqliteConnection,
        playlist: PlaylistKey,
        folder: Option<FolderKey>,
        sort: PlaylistEntrySort,
        descending: bool,
        filter: &str,
    ) -> LibraryResult<Vec<PlaylistEntryKey>> {
        let query = playlist_query(playlist, folder, sort, descending, filter);
        Ok(
            sqlx::query_scalar::<_, PlaylistEntryKey>(sqlx::AssertSqlSafe(
                query.select(&query.entry_key),
            ))
            .fetch_all(connection)
            .await?,
        )
    }

    pub async fn playlist_media_uri_order(
        &self,
        playlist: PlaylistKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, String>(
            "SELECT entry.media_uri FROM playlist_entries entry
             JOIN playlists playlist USING(playlist_key)
             LEFT JOIN tracks track USING(media_uri)
             WHERE playlist.playlist_key=?1
               AND (playlist.source_key IS NULL OR ?2 IS NULL OR EXISTS (
                 SELECT 1 FROM track_folders scope
                 WHERE scope.track_key=track.track_key AND scope.folder_key=?2
               ))
             ORDER BY entry.position,entry.playlist_entry_key",
        )
        .bind(playlist)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn playlist_rows(
        &self,
        keys: &[PlaylistKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = Self::load_playlist_rows(&mut connection, keys).await;
        Database::clear_progress(&mut connection).await?;
        result
    }

    async fn load_playlist_rows(
        connection: &mut SqliteConnection,
        keys: &[PlaylistKey],
    ) -> LibraryResult<Vec<PlaylistRow>> {
        if keys.len() > PLAYLIST_ROW_LIMIT {
            return Err(LibraryError::InvalidRequest(format!(
                "Playlist row reads are limited to {PLAYLIST_ROW_LIMIT} keys"
            )));
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(playlist_key, position) AS (");
        query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
            row.push_bind(*key).push_bind(position as i64);
        });
        query.push(
            ") SELECT playlist.playlist_key, playlist.source_key,
                      playlist.object_id, playlist.name,
                      playlist.artwork_binding,
                      count(entry.playlist_entry_key) AS track_count,
                      COALESCE(sum(COALESCE(track.duration_millis,entry.duration_millis)), 0) AS duration_millis,
                      count(CASE WHEN EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=entry.media_uri AND access.origin='download') THEN 1 END) AS downloaded_count
               FROM requested JOIN playlists AS playlist USING(playlist_key)
               LEFT JOIN playlist_entries AS entry USING(playlist_key)
               LEFT JOIN tracks AS track USING(media_uri)
               GROUP BY playlist.playlist_key ORDER BY requested.position",
        );
        let mut result = query
            .build_query_as::<PlaylistRow>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?;
        let mut artwork_query = QueryBuilder::<Sqlite>::new("WITH requested(playlist_key) AS (");
        artwork_query.push_values(keys, |mut row, key| {
            row.push_bind(*key);
        });
        artwork_query.push(
            "), ranked AS (
               SELECT entry.playlist_key,
                      track.artwork_binding,
                      row_number() OVER (PARTITION BY entry.playlist_key ORDER BY entry.position,entry.playlist_entry_key) artwork_position
               FROM requested JOIN playlist_entries entry USING(playlist_key)
               LEFT JOIN tracks track USING(media_uri)
               WHERE track.artwork_binding IS NOT NULL)
               SELECT playlist_key,artwork_binding FROM ranked WHERE artwork_position<=4 ORDER BY playlist_key,artwork_position",
        );
        let mut artwork = BTreeMap::<PlaylistKey, Vec<Vec<u8>>>::new();
        for (playlist, binding) in artwork_query
            .build_query_as::<(PlaylistKey, Vec<u8>)>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?
        {
            artwork.entry(playlist).or_default().push(binding);
        }

        let mut genre_query = QueryBuilder::<Sqlite>::new("WITH requested(playlist_key) AS (");
        genre_query.push_values(keys, |mut row, key| {
            row.push_bind(*key);
        });
        genre_query.push(
            "), counts AS (
               SELECT entry.playlist_key,genre.genre_key,genre.name,genre.sort_text,count(*) uses
               FROM requested JOIN playlist_entries entry USING(playlist_key)
               JOIN tracks track USING(media_uri) JOIN track_genres relation USING(track_key)
               JOIN genres genre USING(genre_key) GROUP BY entry.playlist_key,genre.genre_key),
               ranked AS (SELECT *,row_number() OVER (PARTITION BY playlist_key ORDER BY uses DESC,sort_text,genre_key) genre_position FROM counts)
               SELECT playlist_key,genre_key,name FROM ranked WHERE genre_position<=2 ORDER BY playlist_key,genre_position",
        );
        let mut genres = BTreeMap::<PlaylistKey, Vec<PlaylistGenreLink>>::new();
        for (playlist, genre_key, name) in genre_query
            .build_query_as::<(PlaylistKey, crate::GenreKey, String)>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?
        {
            genres
                .entry(playlist)
                .or_default()
                .push(PlaylistGenreLink { genre_key, name });
        }
        for row in &mut result {
            row.representative_artwork = artwork.remove(&row.playlist_key).unwrap_or_default();
            row.genres = genres.remove(&row.playlist_key).unwrap_or_default();
        }
        Ok(result)
    }

    pub async fn playlist_entry_rows(
        &self,
        keys: &[PlaylistEntryKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistEntryRow>> {
        if keys.len() > PLAYLIST_ENTRY_ROW_LIMIT {
            return Err(LibraryError::InvalidRequest(format!(
                "Playlist entry reads are limited to {PLAYLIST_ENTRY_ROW_LIMIT} keys"
            )));
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = tokio::select! {
            result = self.acquire_reader() => result?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        let mut transaction = connection.begin().await?;
        let rows = load_playlist_entry_rows(&mut transaction, keys).await?;
        transaction.commit().await?;
        Ok(rows)
    }

    pub async fn playlist_entry_media_uris(
        &self,
        playlist: PlaylistKey,
        entries: &[PlaylistEntryKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        if entries.len() > PLAYLIST_ENTRY_ROW_LIMIT {
            return Err(LibraryError::InvalidRequest(format!(
                "Playlist entry reads are limited to {PLAYLIST_ENTRY_ROW_LIMIT} keys"
            )));
        }
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query =
            QueryBuilder::<Sqlite>::new("WITH requested(playlist_entry_key,ordinal) AS (");
        query.push_values(entries.iter().enumerate(), |mut row, (ordinal, entry)| {
            row.push_bind(*entry).push_bind(ordinal as i64);
        });
        query
            .push(") SELECT entry.media_uri FROM requested JOIN playlist_entries entry USING(playlist_entry_key) WHERE entry.playlist_key=")
            .push_bind(playlist)
            .push(" ORDER BY requested.ordinal");
        let result = query
            .build_query_scalar::<String>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn source_playlist_object_id(
        &self,
        source: SourceKey,
        playlist: PlaylistKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<String>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar(
            "SELECT object_id FROM playlists
             WHERE source_key=?1 AND playlist_key=?2",
        )
        .bind(source)
        .bind(playlist)
        .fetch_optional(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn source_playlist_media_object_ids(
        &self,
        source: SourceKey,
        playlist: Option<PlaylistKey>,
        media_uris: &[String],
        skip_existing: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        if media_uris.len() > PLAYLIST_ENTRY_ROW_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "Playlist provider Track page exceeds 256 entries".to_string(),
            ));
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(media_uri,ordinal) AS (");
        query.push_values(
            media_uris.iter().enumerate(),
            |mut row, (ordinal, media_uri)| {
                row.push_bind(media_uri).push_bind(ordinal as i64);
            },
        );
        query.push(") SELECT track.object_id,(")
            .push_bind(!skip_existing)
            .push(" OR ")
            .push_bind(playlist)
            .push(" IS NULL OR NOT EXISTS(SELECT 1 FROM playlist_entries existing WHERE existing.playlist_key=")
            .push_bind(playlist)
            .push(" AND existing.media_uri=requested.media_uri)) accepted FROM requested LEFT JOIN tracks track ON track.media_uri=requested.media_uri AND track.source_key=")
            .push_bind(source)
            .push(" ORDER BY requested.ordinal");
        let mut result = Vec::with_capacity(media_uris.len());
        for (object_id, accepted) in query
            .build_query_as::<(Option<String>, bool)>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?
        {
            let Some(object_id) = object_id else {
                Database::clear_progress(&mut connection).await?;
                return Err(LibraryError::InvalidRequest(
                    "Playlist Track is no longer current".to_string(),
                ));
            };
            if accepted {
                result.push(object_id);
            }
        }
        Database::clear_progress(&mut connection).await?;
        Ok(result)
    }

    pub async fn source_playlist_entry_object_ids(
        &self,
        source: SourceKey,
        playlist: PlaylistKey,
        entries: &[PlaylistEntryKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        if entries.len() > PLAYLIST_ENTRY_ROW_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "Playlist provider entry page exceeds 256 entries".to_string(),
            ));
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query =
            QueryBuilder::<Sqlite>::new("WITH requested(playlist_entry_key,ordinal) AS (");
        query.push_values(entries.iter().enumerate(), |mut row, (ordinal, entry)| {
            row.push_bind(*entry).push_bind(ordinal as i64);
        });
        query.push(") SELECT entry.object_id FROM requested JOIN playlist_entries entry USING(playlist_entry_key) JOIN playlists playlist USING(playlist_key) WHERE playlist.source_key=")
            .push_bind(source)
            .push(" AND playlist.playlist_key=")
            .push_bind(playlist)
            .push(" ORDER BY requested.ordinal");
        let result = query
            .build_query_scalar::<String>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?;
        Database::clear_progress(&mut connection).await?;
        Ok(result)
    }

    pub async fn create_playlist(
        &self,
        source: Option<SourceKey>,
        name: &str,
        media_uris: &[String],
    ) -> LibraryResult<Option<(PlaylistKey, String)>> {
        let name = name.trim();
        if name.is_empty() {
            return Err(LibraryError::InvalidRequest(
                "Playlist name cannot be empty".to_string(),
            ));
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        if let Some(source) = source
            && !media_uris_exist(&mut transaction, source, media_uris).await?
        {
            transaction.rollback().await?;
            return Ok(None);
        }
        let (playlist, object_id) = sqlx::query_as::<_, (PlaylistKey, String)>(
            "INSERT INTO main.playlists(
                 source_key, object_id, name, normalized_name, sort_text, position
             ) SELECT (SELECT durable.source_key FROM main.source_ids durable JOIN catalog.sources source USING(object_id) WHERE source.source_key=?1),id,?2,lower(?2),lower(?2),
                      (SELECT COALESCE(max(position)+1,0) FROM playlists)
               FROM (SELECT 'rufin:playlist:' || lower(hex(randomblob(16))) id)
               RETURNING playlist_key,object_id",
        )
        .bind(source)
        .bind(name)
        .fetch_one(&mut *transaction)
        .await?;
        insert_playlist_media(&mut transaction, playlist, 0, media_uris, false).await?;
        transaction.commit().await?;
        Ok(Some((playlist, object_id)))
    }

    pub async fn rename_playlist(
        &self,
        source: Option<SourceKey>,
        playlist: PlaylistKey,
        name: &str,
    ) -> LibraryResult<bool> {
        let name = name.trim();
        if name.is_empty() {
            return Err(LibraryError::InvalidRequest(
                "Playlist name cannot be empty".to_string(),
            ));
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query(
            "UPDATE main.playlists SET name=?3, normalized_name=lower(?3), sort_text=lower(?3)
             WHERE ((?1 IS NULL AND source_key IS NULL) OR source_key=(SELECT durable.source_key FROM main.source_ids durable JOIN catalog.sources source USING(object_id) WHERE source.source_key=?1))
               AND playlist_key=?2",
        )
        .bind(source)
        .bind(playlist)
        .bind(name)
        .execute(connection)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn move_playlist(
        &self,
        source: impl Into<Option<SourceKey>>,
        dragged: PlaylistKey,
        target: PlaylistKey,
    ) -> LibraryResult<bool> {
        let source = source.into();
        if dragged == target {
            return Ok(false);
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        sqlx::query("INSERT OR IGNORE INTO main.source_ids(object_id) SELECT object_id FROM catalog.sources")
            .execute(&mut *transaction).await?;
        sqlx::query("INSERT INTO main.playlists(source_key,object_id,position)
          SELECT durable.source_key,observed.object_id,(SELECT COALESCE(max(position)+1,0) FROM main.playlists)+row_number() OVER(ORDER BY observed.playlist_key)-1
          FROM catalog.native_playlists observed JOIN catalog.sources source USING(source_key)
          JOIN main.source_ids durable ON durable.object_id=source.object_id
          WHERE NOT EXISTS(SELECT 1 FROM main.playlists owned WHERE owned.source_key=durable.source_key AND owned.object_id=observed.object_id)")
          .execute(&mut *transaction).await?;
        let visible = sqlx::query_as::<_, (PlaylistKey, PlaylistKey, i64)>(
            "SELECT visible.playlist_key,owned.playlist_key,visible.position FROM playlists visible
             LEFT JOIN catalog.sources source ON source.source_key=visible.source_key
             LEFT JOIN main.source_ids durable ON durable.object_id=source.object_id
             JOIN main.playlists owned ON owned.object_id=visible.object_id AND owned.source_key IS durable.source_key
             WHERE ?1 IS NULL OR visible.source_key=?1 OR visible.source_key IS NULL
             ORDER BY visible.position,visible.playlist_key",
        )
        .bind(source)
        .fetch_all(&mut *transaction)
        .await?;
        let Some(dragged_index) = visible.iter().position(|(key, _, _)| *key == dragged) else {
            transaction.rollback().await?;
            return Ok(false);
        };
        let Some(target_index) = visible.iter().position(|(key, _, _)| *key == target) else {
            transaction.rollback().await?;
            return Ok(false);
        };
        let mut order = visible.iter().map(|(_, key, _)| *key).collect::<Vec<_>>();
        let dragged = order.remove(dragged_index);
        let insertion = if dragged_index < target_index {
            target_index - 1
        } else {
            target_index
        };
        order.insert(insertion, dragged);
        let max_position =
            sqlx::query_scalar::<_, Option<i64>>("SELECT max(position) FROM playlists")
                .fetch_one(&mut *transaction)
                .await?
                .unwrap_or_default();
        let offset = max_position + 1;
        sqlx::query(
            "UPDATE main.playlists SET position=position+?2 WHERE ?1 IS NULL OR source_key=(SELECT durable.source_key FROM main.source_ids durable JOIN catalog.sources source USING(object_id) WHERE source.source_key=?1) OR source_key IS NULL",
        )
        .bind(source)
        .bind(offset)
        .execute(&mut *transaction)
        .await?;
        for (key, (_, _, position)) in order.into_iter().zip(visible) {
            sqlx::query("UPDATE main.playlists SET position=?2 WHERE playlist_key=?1")
                .bind(key)
                .bind(position)
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn delete_playlist(
        &self,
        source: Option<SourceKey>,
        playlist: PlaylistKey,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query(
            "DELETE FROM main.playlists
             WHERE ((?1 IS NULL AND source_key IS NULL) OR source_key=(SELECT durable.source_key FROM main.source_ids durable JOIN catalog.sources source USING(object_id) WHERE source.source_key=?1))
               AND playlist_key=?2",
        )
        .bind(source)
        .bind(playlist)
        .execute(connection)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn add_playlist_media(
        &self,
        source: Option<SourceKey>,
        playlist: PlaylistKey,
        media_uris: &[String],
        skip_existing: bool,
    ) -> LibraryResult<usize> {
        if media_uris.is_empty() {
            return Ok(0);
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        if !editable_playlist_exists(&mut transaction, source, playlist).await?
            || match source {
                Some(source) => !media_uris_exist(&mut transaction, source, media_uris).await?,
                None => false,
            }
        {
            transaction.rollback().await?;
            return Ok(0);
        }
        let next_position = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(max(position) + 1, 0)
             FROM playlist_entries WHERE playlist_key=?1",
        )
        .bind(playlist)
        .fetch_one(&mut *transaction)
        .await?;
        let accepted = insert_playlist_media(
            &mut transaction,
            playlist,
            next_position,
            media_uris,
            skip_existing,
        )
        .await?;
        transaction.commit().await?;
        Ok(accepted)
    }

    pub async fn remove_playlist_entries(
        &self,
        source: Option<SourceKey>,
        playlist: PlaylistKey,
        entries: &[PlaylistEntryKey],
    ) -> LibraryResult<usize> {
        if entries.is_empty() {
            return Ok(0);
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        if !editable_playlist_exists(&mut transaction, source, playlist).await? {
            transaction.rollback().await?;
            return Ok(0);
        }
        let mut removed = 0usize;
        for batch in entries.chunks(PLAYLIST_DELETE_BATCH) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "DELETE FROM main.playlist_entries WHERE playlist_key=",
            );
            query
                .push_bind(playlist)
                .push(" AND playlist_entry_key IN (");
            let mut separated = query.separated(", ");
            for entry in batch {
                separated.push_bind(*entry);
            }
            separated.push_unseparated(")");
            removed += query
                .build()
                .persistent(false)
                .execute(&mut *transaction)
                .await?
                .rows_affected() as usize;
        }
        sqlx::query(
            "WITH positions AS (
                 SELECT playlist_entry_key,
                        row_number() OVER (ORDER BY position) - 1 AS next_position
                 FROM playlist_entries WHERE playlist_key=?1
             ) UPDATE main.playlist_entries SET position=(
                 SELECT next_position FROM positions
                 WHERE positions.playlist_entry_key=playlist_entries.playlist_entry_key
             ) WHERE playlist_key=?1",
        )
        .bind(playlist)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(removed)
    }

    pub async fn move_playlist_entry(
        &self,
        source: Option<SourceKey>,
        playlist: PlaylistKey,
        entry: PlaylistEntryKey,
        new_position: usize,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        if !editable_playlist_exists(&mut transaction, source, playlist).await? {
            transaction.rollback().await?;
            return Ok(false);
        }
        let Some(old_position) = sqlx::query_scalar::<_, i64>(
            "SELECT position FROM playlist_entries
             WHERE playlist_key=?1 AND playlist_entry_key=?2",
        )
        .bind(playlist)
        .bind(entry)
        .fetch_optional(&mut *transaction)
        .await?
        else {
            transaction.rollback().await?;
            return Ok(false);
        };
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM playlist_entries WHERE playlist_key=?1",
        )
        .bind(playlist)
        .fetch_one(&mut *transaction)
        .await?;
        let new_position = i64::try_from(new_position)
            .unwrap_or(i64::MAX)
            .min(count.saturating_sub(1));
        if old_position == new_position {
            transaction.commit().await?;
            return Ok(true);
        }
        let offset = count + 1;
        sqlx::query("UPDATE main.playlist_entries SET position=position+?2 WHERE playlist_key=?1")
            .bind(playlist)
            .bind(offset)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "UPDATE main.playlist_entries SET position=CASE
                 WHEN playlist_entry_key=?2 THEN ?4
                 WHEN ?4 < ?3 AND position-?5 >= ?4 AND position-?5 < ?3
                     THEN position-?5+1
                 WHEN ?4 > ?3 AND position-?5 > ?3 AND position-?5 <= ?4
                     THEN position-?5-1
                 ELSE position-?5 END
             WHERE playlist_key=?1",
        )
        .bind(playlist)
        .bind(entry)
        .bind(old_position)
        .bind(new_position)
        .bind(offset)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(true)
    }
}

async fn editable_playlist_exists(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    source: Option<SourceKey>,
    playlist: PlaylistKey,
) -> LibraryResult<bool> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM playlists
         WHERE ((?1 IS NULL AND source_key IS NULL) OR source_key=?1)
           AND playlist_key=?2",
    )
    .bind(source)
    .bind(playlist)
    .fetch_optional(&mut **transaction)
    .await?
    .is_some())
}

async fn media_uris_exist(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    source: SourceKey,
    media_uris: &[String],
) -> LibraryResult<bool> {
    if media_uris.is_empty() {
        return Ok(true);
    }
    for media_uri in media_uris {
        if sqlx::query_scalar::<_, i64>("SELECT 1 FROM tracks WHERE source_key=?1 AND media_uri=?2")
            .bind(source)
            .bind(media_uri)
            .fetch_optional(&mut **transaction)
            .await?
            .is_none()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

// Every branch is driven by the bounded requested URI set before the UNION. A
// LEFT JOIN of the public compound view can otherwise materialize every entry.
pub(crate) const PLAYLIST_URI_SNAPSHOTS: &str = r#"
playlist_snapshot_candidates AS (
    SELECT entry.* FROM requested
    JOIN main.playlist_entries entry ON entry.playlist_entry_key=(
        SELECT playlist_entry_key FROM main.playlist_entries WHERE media_uri=requested.media_uri
        ORDER BY title IS NULL,snapshot_at DESC,playlist_entry_key DESC LIMIT 1)
    UNION ALL
    SELECT -entry.playlist_entry_key,-entry.playlist_key,entry.object_id,entry.media_uri,
           entry.title,entry.artist,entry.album,entry.album_display_artist,entry.snapshot_at,
           entry.duration_millis,entry.disc_number,entry.track_number,entry.year,entry.release_date,
           entry.source_format,entry.musicbrainz_recording_id,entry.musicbrainz_release_track_id,entry.position
    FROM requested JOIN catalog.native_playlist_entries entry ON entry.playlist_entry_key=(
        SELECT playlist_entry_key FROM catalog.native_playlist_entries WHERE media_uri=requested.media_uri
        ORDER BY title IS NULL,snapshot_at DESC,playlist_entry_key ASC LIMIT 1)
),
playlist_snapshot_ranked AS (
    SELECT *,row_number() OVER(PARTITION BY media_uri ORDER BY title IS NULL,snapshot_at DESC,playlist_entry_key DESC) snapshot_rank
    FROM playlist_snapshot_candidates
),
playlist_snapshots AS (SELECT * FROM playlist_snapshot_ranked WHERE snapshot_rank=1)
"#;

#[cfg(test)]
mod point_projection_tests {
    use super::*;
    use sqlx::Row;

    #[tokio::test]
    async fn latest_snapshots_probe_both_physical_uri_indexes_before_union() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("library.sqlite3"))
            .await
            .unwrap();
        let mut connection = database.acquire_reader().await.unwrap();
        let sql = format!(
            "EXPLAIN QUERY PLAN WITH requested(media_uri) AS (VALUES(?1)), {PLAYLIST_URI_SNAPSHOTS} SELECT * FROM playlist_snapshots"
        );
        let plan = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
            .bind("https://example.test/known")
            .fetch_all(&mut *connection)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect::<Vec<_>>();
        assert_eq!(
            plan.iter()
                .filter(|detail| detail.contains("SEARCH entry USING INTEGER PRIMARY KEY"))
                .count(),
            2
        );
        assert!(
            plan.iter()
                .any(|detail| detail.contains("playlist_entries_media_idx")
                    && detail.contains("media_uri=?"))
        );
        assert!(
            plan.iter()
                .any(|detail| detail.contains("native_playlist_entries_media")
                    && detail.contains("media_uri=?"))
        );
        assert!(
            !plan
                .iter()
                .any(|detail| detail.contains("LAST TERM OF ORDER BY")),
            "snapshot ties must also come directly from the URI indexes"
        );
    }
}

pub(crate) async fn load_playlist_entry_rows(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    keys: &[PlaylistEntryKey],
) -> LibraryResult<Vec<PlaylistEntryRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(playlist_entry_key,ordinal) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (ordinal, key)| {
        row.push_bind(*key).push_bind(ordinal as i64);
    });
    query.push("),entries AS (
        SELECT requested.ordinal,entry.* FROM requested JOIN main.playlist_entries entry
          ON entry.playlist_entry_key=requested.playlist_entry_key WHERE requested.playlist_entry_key>0
        UNION ALL
        SELECT requested.ordinal,-entry.playlist_entry_key,-entry.playlist_key,entry.object_id,entry.media_uri,
          entry.title,entry.artist,entry.album,entry.album_display_artist,entry.snapshot_at,
          entry.duration_millis,entry.disc_number,entry.track_number,entry.year,entry.release_date,
          entry.source_format,entry.musicbrainz_recording_id,entry.musicbrainz_release_track_id,entry.position
        FROM requested JOIN catalog.native_playlist_entries entry
          ON entry.playlist_entry_key=-requested.playlist_entry_key WHERE requested.playlist_entry_key<0
    ) SELECT ");
    query.push(crate::tracks::TRACK_LINK_COLUMNS);
    query.push("entry.playlist_entry_key,entry.media_uri,entry.position,source.object_id source_id,
                      COALESCE(track.title,entry.title,'') title,
                      COALESCE(track.display_artist,entry.artist,'') artist,
                      COALESCE(track.display_album,entry.album,'') album,
                      COALESCE(album.display_artist,entry.album_display_artist) album_display_artist,
                      track.artwork_binding,COALESCE(track.duration_millis,entry.duration_millis,0) duration_millis,
                      COALESCE(track.disc_number,entry.disc_number) disc_number,
                      COALESCE(track.track_number,entry.track_number) track_number,
                      COALESCE(track.year,entry.year) year,
                      COALESCE(track.release_date,entry.release_date) release_date,
                      track.date_added,track.bpm,
                      (SELECT COALESCE(group_concat(name, ', '),'') FROM (
                         SELECT genre.name FROM track_genres credit JOIN genres genre USING(genre_key)
                         WHERE credit.track_key=track.track_key ORDER BY credit.position
                      )) genre,
                      COALESCE((SELECT baseline.play_count FROM activity_baseline baseline
                                WHERE baseline.source_key=track.source_key
                                  AND baseline.track_object_id=track.object_id
                                  AND baseline.period='lifetime' AND baseline.item_kind='track'),0)
                        +(SELECT count(*) FROM listens listen WHERE listen.media_uri=entry.media_uri) play_count,
                      (SELECT max(value) FROM (
                         SELECT baseline.last_played_at value FROM activity_baseline baseline
                         WHERE baseline.source_key=track.source_key
                           AND baseline.track_object_id=track.object_id
                           AND baseline.period='lifetime' AND baseline.item_kind='track'
                         UNION ALL SELECT (SELECT listen.started_at FROM listens listen
                           WHERE listen.media_uri=entry.media_uri
                           ORDER BY listen.started_at DESC,listen.listen_key DESC LIMIT 1)
                      )) last_played,
                      COALESCE(track.source_format,entry.source_format) source_format,
                      COALESCE(track.musicbrainz_recording_id,entry.musicbrainz_recording_id) musicbrainz_recording_id,
                      COALESCE(track.musicbrainz_release_track_id,entry.musicbrainz_release_track_id) musicbrainz_release_track_id,
                      COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=entry.media_uri),track.source_favorite,0) favorite,
                      COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=entry.media_uri),track.source_rating)/10 rating,
                      EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=entry.media_uri AND access.origin='download') is_downloaded
               FROM entries entry
               LEFT JOIN tracks track USING(media_uri)
               LEFT JOIN sources source ON source.source_key=track.source_key
               LEFT JOIN albums album ON album.album_key=track.album_key
               ORDER BY entry.ordinal");
    query
        .build()
        .persistent(false)
        .fetch_all(&mut **transaction)
        .await?
        .iter()
        .map(|row| {
            let mut entry = PlaylistEntryRow::from_row(row)?;
            entry.artists = serde_json::from_str(row.try_get("artists")?)?;
            entry.album_artists = serde_json::from_str(row.try_get("album_artists")?)?;
            Ok(entry)
        })
        .collect()
}

async fn insert_playlist_media(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    playlist: PlaylistKey,
    start: i64,
    media_uris: &[String],
    skip_existing: bool,
) -> LibraryResult<usize> {
    let initial_max = start - 1;
    let mut accepted = 0;
    for media_uri in media_uris {
        let inserted = sqlx::query(
            "INSERT INTO main.playlist_entries(
                 playlist_key,object_id,media_uri,title,artist,
                 album,album_display_artist,duration_millis,
                 disc_number,track_number,year,release_date,source_format,
                 musicbrainz_recording_id,musicbrainz_release_track_id,
                 position
             ) SELECT ?1,'rufin:entry:'||lower(hex(randomblob(16))),?2,
                      track.title,track.display_artist,track.display_album,
                      album.display_artist,track.duration_millis,track.disc_number,
                      track.track_number,track.year,track.release_date,track.source_format,
                      track.musicbrainz_recording_id,track.musicbrainz_release_track_id,?3
               FROM (SELECT ?2 media_uri) requested
               LEFT JOIN tracks track USING(media_uri) LEFT JOIN albums album USING(album_key)
               WHERE ?4=0 OR NOT EXISTS (
                   SELECT 1 FROM playlist_entries existing
                   WHERE existing.playlist_key=?1 AND existing.media_uri=?2
                     AND existing.position<=?5
                 )",
        )
        .bind(playlist)
        .bind(media_uri)
        .bind(start + accepted as i64)
        .bind(skip_existing)
        .bind(initial_max)
        .execute(&mut **transaction)
        .await?
        .rows_affected();
        accepted += inserted as usize;
    }
    Ok(accepted)
}

/// Rufin-authored names and global/native rank are durable; native observations are not.
#[derive(Debug, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct PlaylistIdentity {
    pub source_id: Option<String>,
    pub object_id: String,
    pub name: Option<String>,
    pub position: i64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct PlaylistEntryWrite {
    pub object_id: String,
    pub media_uri: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_display_artist: Option<String>,
    pub snapshot_at: i64,
    pub duration_millis: Option<i64>,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub source_format: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub position: i64,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "record")]
enum PlaylistRecord {
    Playlist(PlaylistIdentity),
}

pub(crate) async fn write_playlist_identity(
    connection: &mut SqliteConnection,
    identity: &PlaylistIdentity,
) -> LibraryResult<PlaylistKey> {
    if identity.object_id.is_empty() || identity.position < 0 {
        return Err(LibraryError::InvalidRequest(
            "invalid playlist identity or order".into(),
        ));
    }
    let source = if let Some(source_id) = &identity.source_id {
        Some(crate::db::write_source_identity(connection, source_id).await?)
    } else {
        None
    };
    // Reimporting an exact identity retains its current global rank and pins.
    let existing = sqlx::query_scalar::<_, PlaylistKey>(
        "SELECT playlist_key FROM main.playlists WHERE object_id=?1 AND source_key IS ?2",
    )
    .bind(&identity.object_id)
    .bind(source)
    .fetch_optional(&mut *connection)
    .await?;
    if let Some(key) = existing {
        sqlx::query("UPDATE main.playlists SET name=?2,normalized_name=lower(?2),sort_text=lower(?2) WHERE playlist_key=?1")
            .bind(key).bind(&identity.name).execute(&mut *connection).await?;
        return Ok(key);
    }
    Ok(sqlx::query_scalar("INSERT INTO main.playlists(source_key,object_id,name,normalized_name,sort_text,position)
        VALUES(?1,?2,?3,lower(?3),lower(?3),CASE WHEN EXISTS(SELECT 1 FROM main.playlists WHERE position=?4)
        THEN (SELECT COALESCE(max(position)+1,0) FROM main.playlists) ELSE ?4 END) RETURNING playlist_key")
        .bind(source).bind(&identity.object_id).bind(&identity.name).bind(identity.position)
        .fetch_one(connection).await?)
}

pub(crate) async fn write_playlist_entry(
    connection: &mut SqliteConnection,
    playlist: PlaylistKey,
    entry: &PlaylistEntryWrite,
) -> LibraryResult<()> {
    if entry.object_id.is_empty() || entry.media_uri.is_empty() || entry.position < 0 {
        return Err(LibraryError::InvalidRequest(
            "invalid playlist occurrence identity or order".into(),
        ));
    }
    sqlx::query("INSERT INTO main.playlist_entries(playlist_key,object_id,media_uri,title,artist,album,
        album_display_artist,snapshot_at,duration_millis,disc_number,track_number,year,release_date,
        source_format,musicbrainz_recording_id,musicbrainz_release_track_id,position)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
        ON CONFLICT(playlist_key,object_id) DO UPDATE SET media_uri=excluded.media_uri,title=excluded.title,
        artist=excluded.artist,album=excluded.album,album_display_artist=excluded.album_display_artist,
        snapshot_at=excluded.snapshot_at,duration_millis=excluded.duration_millis,disc_number=excluded.disc_number,
        track_number=excluded.track_number,year=excluded.year,release_date=excluded.release_date,
        source_format=excluded.source_format,musicbrainz_recording_id=excluded.musicbrainz_recording_id,
        musicbrainz_release_track_id=excluded.musicbrainz_release_track_id,position=excluded.position")
        .bind(playlist).bind(&entry.object_id).bind(&entry.media_uri).bind(&entry.title).bind(&entry.artist)
        .bind(&entry.album).bind(&entry.album_display_artist).bind(entry.snapshot_at).bind(entry.duration_millis)
        .bind(entry.disc_number).bind(entry.track_number).bind(entry.year).bind(&entry.release_date)
        .bind(&entry.source_format).bind(&entry.musicbrainz_recording_id).bind(&entry.musicbrainz_release_track_id)
        .bind(entry.position).execute(connection).await?;
    Ok(())
}

pub(crate) async fn export_playlist_order_jsonl_on(
    connection: &mut SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<u64> {
    use futures_util::TryStreamExt;
    let mut rows=sqlx::query_as::<_,PlaylistIdentity>("SELECT source.object_id source_id,playlist.object_id,playlist.name,playlist.position FROM main.playlists playlist LEFT JOIN main.source_ids source USING(source_key) ORDER BY playlist.position").fetch(connection);
    let mut count = 0;
    while let Some(row) = rows.try_next().await? {
        serde_json::to_writer(&mut output, &PlaylistRecord::Playlist(row))?;
        output.write_all(b"\n")?;
        count += 1;
    }
    Ok(count)
}
pub(crate) async fn import_playlists_jsonl_on(
    connection: &mut SqliteConnection,
    input: impl std::io::BufRead,
) -> LibraryResult<u64> {
    let mut count = 0;
    for line in input.lines() {
        let PlaylistRecord::Playlist(identity) = serde_json::from_str(&line?)?;
        write_playlist_identity(connection, &identity).await?;
        count += 1;
    }
    Ok(count)
}

pub(crate) fn playlist_query(
    key: PlaylistKey,
    folder: Option<FolderKey>,
    sort: PlaylistEntrySort,
    descending: bool,
    filter: &str,
) -> crate::source_window::SourceQuery {
    let native = key.raw() < 0;
    let mut query = crate::source_window::SourceQuery {
        from: format!(
            "{} entry JOIN {} playlist USING(playlist_key)",
            if native {
                "catalog.native_playlist_entries"
            } else {
                "main.playlist_entries"
            },
            if native {
                "catalog.native_playlists"
            } else {
                "main.playlists"
            }
        ),
        predicate: format!("entry.playlist_key={}", key.raw().abs()),
        uri: "entry.media_uri".into(),
        key: "entry.playlist_entry_key".into(),
        entry_key: if native {
            "-entry.playlist_entry_key"
        } else {
            "entry.playlist_entry_key"
        }
        .into(),
        order: vec![(
            match sort {
                PlaylistEntrySort::Position => "entry.position",
                PlaylistEntrySort::Title => "lower(entry.title)",
                PlaylistEntrySort::Artist => "lower(entry.artist)",
                PlaylistEntrySort::Album => "lower(entry.album)",
            }
            .into(),
            descending,
        )],
    };
    if sort != PlaylistEntrySort::Position {
        query.order.push(("entry.position".into(), false));
    }
    if let Some(folder) = folder {
        query.predicate.push_str(&format!(" AND (playlist.source_key IS NULL OR EXISTS(SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.media_uri=entry.media_uri AND scope.folder_key={}))",folder.raw()));
    }
    let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
    if !filter.is_empty() {
        let filter = crate::source_window::quote(&filter);
        query.predicate.push_str(&format!(" AND (instr(lower(entry.title||' '||entry.artist||' '||entry.album),{filter})>0 OR CAST(entry.year AS TEXT)={filter})"));
    }
    query
}
