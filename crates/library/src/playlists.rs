//! Owns Playlist identity, exact duplicate occurrences, ordering, filtering, and edits.
//! Playback queue state is a separate SQLite owner.

use std::{collections::BTreeMap, ops::Deref};

use sqlx::sqlite::SqliteRow;
use sqlx::{Connection, FromRow, QueryBuilder, Row, Sqlite, SqliteConnection};

use crate::{
    Database, FolderKey, LibraryError, LibraryResult, PlaylistEntryKey, PlaylistKey,
    ReadCancellation, SourceKey, TrackKey, TrackRow, tracks::load_track_rows,
};

const PLAYLIST_ROW_LIMIT: usize = 128;
const PLAYLIST_ENTRY_ROW_LIMIT: usize = 256;
const PLAYLIST_DELETE_BATCH: usize = 400;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistSort {
    Title,
    TrackCount,
    Duration,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistEntrySort {
    Position,
    Title,
    Artist,
    Album,
}

impl PlaylistSort {
    const fn code(self) -> i64 {
        match self {
            Self::Title => 0,
            Self::TrackCount => 1,
            Self::Duration => 2,
        }
    }
}
impl PlaylistEntrySort {
    const fn code(self) -> i64 {
        match self {
            Self::Position => 0,
            Self::Title => 1,
            Self::Artist => 2,
            Self::Album => 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistRow {
    pub playlist_key: PlaylistKey,
    pub source_key: SourceKey,
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

#[derive(Clone, Debug, FromRow)]
struct PlaylistEntryScalar {
    playlist_entry_key: PlaylistEntryKey,
    track_key: Option<TrackKey>,
    position: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistEntryRow {
    pub playlist_entry_key: PlaylistEntryKey,
    pub track_key: Option<TrackKey>,
    pub position: i64,
    pub track: Option<TrackRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistEntryOrder {
    pub entries: Vec<PlaylistEntryKey>,
    pub tracks: Vec<TrackKey>,
    pub track_positions: Vec<usize>,
}

impl Deref for PlaylistEntryOrder {
    type Target = [PlaylistEntryKey];

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

fn playlist_entry_order(rows: Vec<(PlaylistEntryKey, Option<TrackKey>)>) -> PlaylistEntryOrder {
    let mut entries = Vec::with_capacity(rows.len());
    let mut tracks = Vec::with_capacity(rows.len());
    let mut track_positions = Vec::with_capacity(rows.len());
    for (position, (entry, track)) in rows.into_iter().enumerate() {
        entries.push(entry);
        if let Some(track) = track {
            tracks.push(track);
            track_positions.push(position);
        }
    }
    PlaylistEntryOrder {
        entries,
        tracks,
        track_positions,
    }
}

impl Database {
    pub async fn playlist_route_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        sort: PlaylistSort,
        descending: bool,
        filter: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(Vec<PlaylistKey>, Vec<PlaylistRow>)> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let order =
            Self::load_playlist_order(&mut transaction, source, folder, sort, descending, filter)
                .await?;
        let first_rows = Self::load_playlist_rows(
            &mut transaction,
            source,
            &order[..order.len().min(64)],
            folder,
        )
        .await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok((order, first_rows))
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

    pub async fn all_playlist_track_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, TrackKey>(
            "SELECT track.track_key FROM tracks track
             WHERE track.source_key=?1
               AND EXISTS (SELECT 1 FROM playlist_entries entry WHERE entry.track_key=track.track_key)
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
        let result =
            Self::load_playlist_order(&mut connection, source, folder, sort, descending, filter)
                .await;
        Database::clear_progress(&mut connection).await?;
        result
    }

    async fn load_playlist_order(
        connection: &mut SqliteConnection,
        source: SourceKey,
        folder: Option<FolderKey>,
        sort: PlaylistSort,
        descending: bool,
        filter: &str,
    ) -> LibraryResult<Vec<PlaylistKey>> {
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        if !filter.is_empty() {
            return Ok(sqlx::query_scalar::<_, PlaylistKey>(
                "SELECT playlist.playlist_key FROM playlists playlist
                 WHERE playlist.source_key=?1 AND instr(playlist.normalized_name,?2)>0
                   AND (?3 IS NULL OR EXISTS (SELECT 1 FROM playlist_entries entry JOIN track_folders scope USING(track_key) WHERE entry.playlist_key=playlist.playlist_key AND scope.folder_key=?3))
                 ORDER BY playlist.sort_text,playlist.playlist_key",
            )
            .bind(source)
            .bind(filter)
            .bind(folder)
            .fetch_all(connection)
            .await?);
        }
        if sort == PlaylistSort::Title {
            return Ok(sqlx::query_scalar::<_, PlaylistKey>(if descending {
                "SELECT playlist.playlist_key FROM playlists playlist WHERE playlist.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM playlist_entries entry JOIN track_folders scope USING(track_key) WHERE entry.playlist_key=playlist.playlist_key AND scope.folder_key=?2)) ORDER BY playlist.sort_text DESC,playlist.playlist_key"
            } else {
                "SELECT playlist.playlist_key FROM playlists playlist WHERE playlist.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM playlist_entries entry JOIN track_folders scope USING(track_key) WHERE entry.playlist_key=playlist.playlist_key AND scope.folder_key=?2)) ORDER BY playlist.sort_text,playlist.playlist_key"
            })
            .bind(source)
            .bind(folder)
            .fetch_all(connection)
            .await?);
        }
        Ok(sqlx::query_scalar::<_, PlaylistKey>(
            "WITH rows AS (SELECT playlist.playlist_key,playlist.sort_text,
               count(entry.playlist_entry_key) track_count,
               COALESCE(sum(track.duration_millis),0) duration
              FROM playlists playlist LEFT JOIN playlist_entries entry ON entry.playlist_key=playlist.playlist_key AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=entry.track_key AND scope.folder_key=?4))
              LEFT JOIN tracks track USING(track_key)
              WHERE playlist.source_key=?1 GROUP BY playlist.playlist_key HAVING ?4 IS NULL OR count(entry.playlist_entry_key)>0)
             SELECT playlist_key FROM rows ORDER BY
              CASE WHEN ?2=0 AND ?3=0 THEN sort_text END ASC,
              CASE WHEN ?2=0 AND ?3=1 THEN sort_text END DESC,
              CASE WHEN ?2=1 AND ?3=0 THEN track_count END ASC,
              CASE WHEN ?2=1 AND ?3=1 THEN track_count END DESC,
              CASE WHEN ?2=2 AND ?3=0 THEN duration END ASC,
              CASE WHEN ?2=2 AND ?3=1 THEN duration END DESC,sort_text,playlist_key",
        )
        .bind(source)
        .bind(sort.code())
        .bind(descending)
        .bind(folder)
        .fetch_all(connection)
        .await?)
    }

    pub async fn playlist_entry_order(
        &self,
        source: SourceKey,
        playlist: PlaylistKey,
        folder: Option<FolderKey>,
        sort: PlaylistEntrySort,
        descending: bool,
        filter: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<PlaylistEntryOrder> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        if sort == PlaylistEntrySort::Position {
            let result=sqlx::query_as::<_,(PlaylistEntryKey,Option<TrackKey>)>(if descending {"SELECT entry.playlist_entry_key,entry.track_key FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) LEFT JOIN tracks track USING(track_key) WHERE playlist.source_key=?1 AND playlist.playlist_key=?2 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=entry.track_key AND scope.folder_key=?3)) AND (?4 OR instr(track.normalized_search,?5)>0 OR CAST(track.year AS TEXT)=?5) ORDER BY entry.position DESC"} else {"SELECT entry.playlist_entry_key,entry.track_key FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) LEFT JOIN tracks track USING(track_key) WHERE playlist.source_key=?1 AND playlist.playlist_key=?2 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=entry.track_key AND scope.folder_key=?3)) AND (?4 OR instr(track.normalized_search,?5)>0 OR CAST(track.year AS TEXT)=?5) ORDER BY entry.position"}).bind(source).bind(playlist).bind(folder).bind(filter.is_empty()).bind(&filter).fetch_all(&mut *connection).await;
            Database::clear_progress(&mut connection).await?;
            return Ok(playlist_entry_order(result?));
        }
        let result = sqlx::query_as::<_, (PlaylistEntryKey, Option<TrackKey>)>(
            "SELECT entry.playlist_entry_key,entry.track_key FROM playlist_entries entry
             JOIN playlists playlist USING(playlist_key) LEFT JOIN tracks track USING(track_key)
             WHERE playlist.source_key=?1 AND playlist.playlist_key=?2
               AND (?5 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=entry.track_key AND scope.folder_key=?5))
               AND (?6 OR instr(track.normalized_search,?7)>0 OR CAST(track.year AS TEXT)=?7) ORDER BY
              CASE WHEN ?3=0 AND ?4=0 THEN entry.position END ASC,
              CASE WHEN ?3=0 AND ?4=1 THEN entry.position END DESC,
              CASE WHEN ?3=1 AND ?4=0 THEN track.sort_text END ASC NULLS LAST,
              CASE WHEN ?3=1 AND ?4=1 THEN track.sort_text END DESC NULLS LAST,
              CASE WHEN ?3=2 AND ?4=0 THEN track.display_artist END ASC NULLS LAST,
              CASE WHEN ?3=2 AND ?4=1 THEN track.display_artist END DESC NULLS LAST,
              CASE WHEN ?3=3 AND ?4=0 THEN track.display_album END ASC NULLS LAST,
              CASE WHEN ?3=3 AND ?4=1 THEN track.display_album END DESC NULLS LAST,
              entry.position",
        )
        .bind(source)
        .bind(playlist)
        .bind(sort.code())
        .bind(descending)
        .bind(folder)
        .bind(filter.is_empty())
        .bind(filter)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(playlist_entry_order(result?))
    }

    pub async fn playlist_track_order(
        &self,
        source: SourceKey,
        playlist: PlaylistKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, TrackKey>(
            "SELECT entry.track_key FROM playlist_entries entry
             JOIN playlists playlist USING(playlist_key)
             WHERE playlist.source_key=?1 AND playlist.playlist_key=?2
               AND entry.track_key IS NOT NULL
               AND (?3 IS NULL OR EXISTS (
                 SELECT 1 FROM track_folders scope
                 WHERE scope.track_key=entry.track_key AND scope.folder_key=?3
               ))
             ORDER BY entry.position,entry.playlist_entry_key",
        )
        .bind(source)
        .bind(playlist)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }
    pub async fn playlist_rows(
        &self,
        source: SourceKey,
        keys: &[PlaylistKey],
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = Self::load_playlist_rows(&mut connection, source, keys, folder).await;
        Database::clear_progress(&mut connection).await?;
        result
    }

    async fn load_playlist_rows(
        connection: &mut SqliteConnection,
        source: SourceKey,
        keys: &[PlaylistKey],
        folder: Option<FolderKey>,
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
                      COALESCE(sum(track.duration_millis), 0) AS duration_millis,
                      count(CASE WHEN EXISTS(SELECT 1 FROM local_access_files access WHERE access.source_key=track.source_key AND access.track_object_id=track.object_id AND access.origin='download') THEN 1 END) AS downloaded_count
               FROM requested JOIN playlists AS playlist USING(playlist_key)
               LEFT JOIN playlist_entries AS entry USING(playlist_key)
               LEFT JOIN tracks AS track USING(track_key)
               WHERE playlist.source_key=",
        );
        query
            .push_bind(source).push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=entry.track_key AND scope.folder_key=").push_bind(folder).push("))")
            .push(" GROUP BY playlist.playlist_key ORDER BY requested.position");
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
                      COALESCE(album.artwork_binding,track.artwork_binding) artwork_binding,
                      row_number() OVER (PARTITION BY entry.playlist_key ORDER BY entry.position,entry.playlist_entry_key) artwork_position
               FROM requested JOIN playlist_entries entry USING(playlist_key)
               JOIN tracks track USING(track_key) JOIN albums album USING(album_key)
               WHERE track.source_key=",
        );
        artwork_query.push_bind(source).push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=").push_bind(folder).push(")) AND COALESCE(album.artwork_binding,track.artwork_binding) IS NOT NULL) SELECT playlist_key,artwork_binding FROM ranked WHERE artwork_position<=4 ORDER BY playlist_key,artwork_position");
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
               JOIN tracks track USING(track_key) JOIN track_genres relation USING(track_key)
               JOIN genres genre USING(genre_key) WHERE track.source_key=",
        );
        genre_query.push_bind(source).push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=").push_bind(folder).push(")) GROUP BY entry.playlist_key,genre.genre_key), ranked AS (SELECT *,row_number() OVER (PARTITION BY playlist_key ORDER BY uses DESC,sort_text,genre_key) genre_position FROM counts) SELECT playlist_key,genre_key,name FROM ranked WHERE genre_position<=2 ORDER BY playlist_key,genre_position");
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
        source: SourceKey,
        keys: &[PlaylistEntryKey],
        folder: Option<FolderKey>,
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
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let mut query =
            QueryBuilder::<Sqlite>::new("WITH requested(playlist_entry_key, ordinal) AS (");
        query.push_values(keys.iter().enumerate(), |mut row, (ordinal, key)| {
            row.push_bind(*key).push_bind(ordinal as i64);
        });
        query.push(
            ") SELECT entry.playlist_entry_key, entry.track_key,
                      entry.position
               FROM requested JOIN playlist_entries AS entry USING(playlist_entry_key)
               JOIN playlists AS playlist USING(playlist_key)
               WHERE playlist.source_key=",
        );
        query.push_bind(source).push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=entry.track_key AND scope.folder_key=").push_bind(folder).push(")) ORDER BY requested.ordinal");
        let result = query
            .build_query_as::<PlaylistEntryScalar>()
            .persistent(false)
            .fetch_all(&mut *transaction)
            .await?;
        let mut track_keys = result
            .iter()
            .filter_map(|row| row.track_key)
            .collect::<Vec<_>>();
        track_keys.sort_unstable();
        track_keys.dedup();
        let tracks = load_track_rows(&mut transaction, source, &track_keys).await?;
        let tracks = tracks
            .into_iter()
            .map(|row| (row.track_key, row))
            .collect::<BTreeMap<_, _>>();
        let mut rows = Vec::with_capacity(result.len());
        for scalar in result {
            let track = scalar.track_key.and_then(|key| tracks.get(&key).cloned());
            rows.push(PlaylistEntryRow {
                playlist_entry_key: scalar.playlist_entry_key,
                track_key: scalar.track_key,
                position: scalar.position,
                track,
            });
        }
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(rows)
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
             WHERE source_key=?1 AND playlist_key=?2 AND ownership='source'",
        )
        .bind(source)
        .bind(playlist)
        .fetch_optional(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn source_playlist_track_object_ids(
        &self,
        source: SourceKey,
        playlist: Option<PlaylistKey>,
        tracks: &[TrackKey],
        skip_existing: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        if tracks.len() > PLAYLIST_ENTRY_ROW_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "Playlist provider Track page exceeds 256 entries".to_string(),
            ));
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(track_key,ordinal) AS (");
        query.push_values(tracks.iter().enumerate(), |mut row, (ordinal, track)| {
            row.push_bind(*track).push_bind(ordinal as i64);
        });
        query.push(") SELECT track.object_id,(")
            .push_bind(!skip_existing)
            .push(" OR ")
            .push_bind(playlist)
            .push(" IS NULL OR NOT EXISTS(SELECT 1 FROM playlist_entries existing WHERE existing.playlist_key=")
            .push_bind(playlist)
            .push(" AND existing.track_key=requested.track_key)) accepted FROM requested LEFT JOIN tracks track ON track.track_key=requested.track_key AND track.source_key=")
            .push_bind(source)
            .push(" ORDER BY requested.ordinal");
        let mut result = Vec::with_capacity(tracks.len());
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
            .push(" AND playlist.ownership='source' ORDER BY requested.ordinal");
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
        source: SourceKey,
        name: &str,
        tracks: &[TrackKey],
    ) -> LibraryResult<Option<PlaylistKey>> {
        let name = name.trim();
        if name.is_empty() {
            return Err(LibraryError::InvalidRequest(
                "Playlist name cannot be empty".to_string(),
            ));
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        if !tracks_exist(&mut transaction, source, tracks).await? {
            transaction.rollback().await?;
            return Ok(None);
        }
        let result = sqlx::query(
            "INSERT INTO playlists(
                 source_key, ownership, object_id, name, normalized_name, sort_text
             ) VALUES (?1, 'user', 'rufin:playlist:' || lower(hex(randomblob(16))),
                       ?2, lower(?2), lower(?2))",
        )
        .bind(source)
        .bind(name)
        .execute(&mut *transaction)
        .await?;
        let playlist = PlaylistKey::from_raw(result.last_insert_rowid());
        insert_playlist_tracks(&mut transaction, source, playlist, 0, tracks).await?;
        transaction.commit().await?;
        Ok(Some(playlist))
    }

    pub async fn rename_playlist(
        &self,
        source: SourceKey,
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
            "UPDATE playlists SET name=?3, normalized_name=lower(?3), sort_text=lower(?3)
             WHERE source_key=?1 AND playlist_key=?2 AND ownership='user'",
        )
        .bind(source)
        .bind(playlist)
        .bind(name)
        .execute(connection)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn delete_playlist(
        &self,
        source: SourceKey,
        playlist: PlaylistKey,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query(
            "DELETE FROM playlists
             WHERE source_key=?1 AND playlist_key=?2 AND ownership='user'",
        )
        .bind(source)
        .bind(playlist)
        .execute(connection)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn add_playlist_tracks(
        &self,
        source: SourceKey,
        playlist: PlaylistKey,
        tracks: &[TrackKey],
        skip_existing: bool,
    ) -> LibraryResult<usize> {
        if tracks.is_empty() {
            return Ok(0);
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        if !tracks_exist(&mut transaction, source, tracks).await?
            || !user_playlist_exists(&mut transaction, source, playlist).await?
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
        let initial_max = next_position - 1;
        let mut accepted = 0usize;
        for track in tracks {
            let position = next_position + accepted as i64;
            let inserted = sqlx::query("INSERT INTO playlist_entries(playlist_key,object_id,track_key,track_object_id,position) SELECT ?1,'rufin:entry:'||lower(hex(randomblob(16))),track_key,object_id,?4 FROM tracks WHERE source_key=?2 AND track_key=?3 AND (?5=0 OR NOT EXISTS (SELECT 1 FROM playlist_entries existing WHERE existing.playlist_key=?1 AND existing.track_key=?3 AND existing.position<=?6))")
                .bind(playlist).bind(source).bind(*track).bind(position).bind(skip_existing).bind(initial_max).execute(&mut *transaction).await?;
            accepted += inserted.rows_affected() as usize;
        }
        transaction.commit().await?;
        Ok(accepted)
    }

    pub async fn remove_playlist_entries(
        &self,
        source: SourceKey,
        playlist: PlaylistKey,
        entries: &[PlaylistEntryKey],
    ) -> LibraryResult<usize> {
        if entries.is_empty() {
            return Ok(0);
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        if !user_playlist_exists(&mut transaction, source, playlist).await? {
            transaction.rollback().await?;
            return Ok(0);
        }
        let mut removed = 0usize;
        for batch in entries.chunks(PLAYLIST_DELETE_BATCH) {
            let mut query =
                QueryBuilder::<Sqlite>::new("DELETE FROM playlist_entries WHERE playlist_key=");
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
             ) UPDATE playlist_entries SET position=(
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
        source: SourceKey,
        playlist: PlaylistKey,
        entry: PlaylistEntryKey,
        new_position: usize,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        if !user_playlist_exists(&mut transaction, source, playlist).await? {
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
        sqlx::query("UPDATE playlist_entries SET position=position+?2 WHERE playlist_key=?1")
            .bind(playlist)
            .bind(offset)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "UPDATE playlist_entries SET position=CASE
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

async fn user_playlist_exists(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    source: SourceKey,
    playlist: PlaylistKey,
) -> LibraryResult<bool> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM playlists
         WHERE source_key=?1 AND playlist_key=?2 AND ownership='user'",
    )
    .bind(source)
    .bind(playlist)
    .fetch_optional(&mut **transaction)
    .await?
    .is_some())
}

async fn tracks_exist(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    source: SourceKey,
    tracks: &[TrackKey],
) -> LibraryResult<bool> {
    if tracks.is_empty() {
        return Ok(true);
    }
    for track in tracks {
        if sqlx::query_scalar::<_, i64>("SELECT 1 FROM tracks WHERE source_key=?1 AND track_key=?2")
            .bind(source)
            .bind(*track)
            .fetch_optional(&mut **transaction)
            .await?
            .is_none()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn insert_playlist_tracks(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    source: SourceKey,
    playlist: PlaylistKey,
    start: i64,
    tracks: &[TrackKey],
) -> LibraryResult<()> {
    for (offset, track) in tracks.iter().enumerate() {
        sqlx::query(
            "INSERT INTO playlist_entries(
                 playlist_key, object_id, track_key, track_object_id, position
             ) SELECT ?1, 'rufin:entry:' || lower(hex(randomblob(16))),
                      track_key, object_id, ?4
               FROM tracks WHERE source_key=?2 AND track_key=?3",
        )
        .bind(playlist)
        .bind(source)
        .bind(*track)
        .bind(start + offset as i64)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}
