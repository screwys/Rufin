//! Owns Playlist identity, exact duplicate occurrences, ordering, filtering, and edits.
//! Playback queue state is a separate SQLite owner.

use std::collections::BTreeMap;

use sqlx::sqlite::SqliteRow;
use sqlx::{Connection, FromRow, QueryBuilder, Row, Sqlite};

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
    pub ownership: String,
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
            ownership: row.try_get("ownership")?,
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
    playlist_key: PlaylistKey,
    object_id: String,
    track_key: Option<TrackKey>,
    track_object_id: String,
    position: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistEntryRow {
    pub playlist_entry_key: PlaylistEntryKey,
    pub playlist_key: PlaylistKey,
    pub object_id: String,
    pub track_key: Option<TrackKey>,
    pub track_object_id: String,
    pub position: i64,
    pub track: Option<TrackRow>,
}

impl Database {
    pub async fn playlist_projection_playback(
        &self,
        source: SourceKey,
        playlist: PlaylistKey,
        entries: &[PlaylistEntryKey],
        anchor: PlaylistEntryKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<(Vec<TrackKey>, usize, TrackRow)>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut order = Vec::with_capacity(entries.len());
        let mut anchor_position = None;
        let mut anchor_track = None;
        for batch in entries.chunks(PLAYLIST_ENTRY_ROW_LIMIT) {
            let mut query =
                QueryBuilder::<Sqlite>::new("WITH requested(playlist_entry_key,ordinal) AS (");
            query.push_values(batch.iter().enumerate(), |mut row, (ordinal, entry)| {
                row.push_bind(*entry).push_bind(ordinal as i64);
            });
            query.push(") SELECT entry.playlist_entry_key,entry.track_key FROM requested JOIN playlist_entries entry USING(playlist_entry_key) JOIN playlists playlist USING(playlist_key) WHERE playlist.source_key=").push_bind(source).push(" AND playlist.playlist_key=").push_bind(playlist).push(" AND entry.track_key IS NOT NULL AND (").push_bind(folder).push(" IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=entry.track_key AND scope.folder_key=").push_bind(folder).push(")) ORDER BY requested.ordinal");
            for (entry, track) in query
                .build_query_as::<(PlaylistEntryKey, TrackKey)>()
                .persistent(false)
                .fetch_all(&mut *connection)
                .await?
            {
                if entry == anchor {
                    anchor_position = Some(order.len());
                    anchor_track = Some(track);
                }
                order.push(track);
            }
        }
        let Some((anchor_position, anchor_track)) = anchor_position.zip(anchor_track) else {
            Database::clear_progress(&mut connection).await?;
            return Ok(None);
        };
        let rows = load_track_rows(&mut connection, source, &[anchor_track]).await?;
        Database::clear_progress(&mut connection).await?;
        Ok(rows
            .into_iter()
            .next()
            .map(|row| (order, anchor_position, row)))
    }

    pub async fn playlist_entry_playback(
        &self,
        source: SourceKey,
        playlist: PlaylistKey,
        entry: PlaylistEntryKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<(Vec<TrackKey>, usize, TrackRow)>> {
        let order = self
            .playlist_track_order(source, playlist, folder, cancellation)
            .await?;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let anchor = sqlx::query_as::<_, (TrackKey, i64)>(
            "SELECT selected.track_key,
                    (SELECT count(*) FROM playlist_entries prior
                     WHERE prior.playlist_key=selected.playlist_key
                       AND prior.track_key IS NOT NULL
                       AND prior.position<selected.position
                       AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=prior.track_key AND scope.folder_key=?4)))
             FROM playlist_entries selected JOIN playlists playlist USING(playlist_key)
             WHERE playlist.source_key=?1 AND selected.playlist_key=?2
               AND selected.playlist_entry_key=?3 AND selected.track_key IS NOT NULL
               AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=selected.track_key AND scope.folder_key=?4))",
        )
        .bind(source)
        .bind(playlist)
        .bind(entry)
        .bind(folder)
        .fetch_optional(&mut *connection)
        .await?;
        Database::clear_progress(&mut connection).await?;
        let Some((track, position)) = anchor else {
            return Ok(None);
        };
        let rows = self.track_rows(source, &[track], cancellation).await?;
        Ok(rows
            .into_iter()
            .next()
            .map(|row| (order, position.max(0) as usize, row)))
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
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        if sort == PlaylistSort::Title {
            let result=sqlx::query_scalar::<_,PlaylistKey>(if descending {"SELECT playlist.playlist_key FROM playlists playlist WHERE playlist.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM playlist_entries entry JOIN track_folders scope USING(track_key) WHERE entry.playlist_key=playlist.playlist_key AND scope.folder_key=?2)) ORDER BY playlist.sort_text DESC,playlist.playlist_key"} else {"SELECT playlist.playlist_key FROM playlists playlist WHERE playlist.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM playlist_entries entry JOIN track_folders scope USING(track_key) WHERE entry.playlist_key=playlist.playlist_key AND scope.folder_key=?2)) ORDER BY playlist.sort_text,playlist.playlist_key"}).bind(source).bind(folder).fetch_all(&mut *connection).await;
            Database::clear_progress(&mut connection).await?;
            return Ok(result?);
        }
        let result = sqlx::query_scalar::<_, PlaylistKey>(
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
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
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
    ) -> LibraryResult<Vec<PlaylistEntryKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        if sort == PlaylistEntrySort::Position {
            let result=sqlx::query_scalar::<_,PlaylistEntryKey>(if descending {"SELECT entry.playlist_entry_key FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) LEFT JOIN tracks track USING(track_key) WHERE playlist.source_key=?1 AND playlist.playlist_key=?2 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=entry.track_key AND scope.folder_key=?3)) AND (?4 OR instr(track.normalized_search,?5)>0 OR CAST(track.year AS TEXT)=?5) ORDER BY entry.position DESC"} else {"SELECT entry.playlist_entry_key FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) LEFT JOIN tracks track USING(track_key) WHERE playlist.source_key=?1 AND playlist.playlist_key=?2 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=entry.track_key AND scope.folder_key=?3)) AND (?4 OR instr(track.normalized_search,?5)>0 OR CAST(track.year AS TEXT)=?5) ORDER BY entry.position"}).bind(source).bind(playlist).bind(folder).bind(filter.is_empty()).bind(&filter).fetch_all(&mut *connection).await;
            Database::clear_progress(&mut connection).await?;
            return Ok(result?);
        }
        let result = sqlx::query_scalar::<_, PlaylistEntryKey>(
            "SELECT entry.playlist_entry_key FROM playlist_entries entry
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
        Ok(result?)
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
        if keys.len() > PLAYLIST_ROW_LIMIT {
            return Err(LibraryError::InvalidRequest(format!(
                "Playlist row reads are limited to {PLAYLIST_ROW_LIMIT} keys"
            )));
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(playlist_key, position) AS (");
        query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
            row.push_bind(*key).push_bind(position as i64);
        });
        query.push(
            ") SELECT playlist.playlist_key, playlist.source_key,
                      playlist.object_id, playlist.ownership, playlist.name,
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
        for row in &mut result {
            row.representative_artwork = sqlx::query_scalar::<_, Vec<u8>>(
                "SELECT album.artwork_binding FROM playlist_entries entry
                 JOIN tracks track USING(track_key) JOIN albums album USING(album_key)
                 WHERE entry.playlist_key=?1 AND track.source_key=?2
                   AND (?3 IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?3))
                   AND album.artwork_binding IS NOT NULL
                 GROUP BY album.album_key ORDER BY min(entry.position) LIMIT 4",
            )
            .bind(row.playlist_key)
            .bind(source)
            .bind(folder)
            .fetch_all(&mut *connection)
            .await?;
            row.genres = sqlx::query_as::<_, (crate::GenreKey, String)>(
                "SELECT genre.genre_key,genre.name FROM playlist_entries entry
                 JOIN tracks track USING(track_key) JOIN track_genres relation USING(track_key)
                 JOIN genres genre USING(genre_key)
                 WHERE entry.playlist_key=?1 AND track.source_key=?2
                   AND (?3 IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?3))
                 GROUP BY genre.genre_key ORDER BY count(*) DESC,genre.sort_text,genre.genre_key LIMIT 2",
            )
            .bind(row.playlist_key)
            .bind(source)
            .bind(folder)
            .fetch_all(&mut *connection)
            .await?
            .into_iter()
            .map(|(genre_key, name)| PlaylistGenreLink { genre_key, name })
            .collect();
        }
        Database::clear_progress(&mut connection).await?;
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
            ") SELECT entry.playlist_entry_key, entry.playlist_key,
                      entry.object_id, entry.track_key, entry.track_object_id,
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
                playlist_key: scalar.playlist_key,
                object_id: scalar.object_id,
                track_key: scalar.track_key,
                track_object_id: scalar.track_object_id,
                position: scalar.position,
                track,
            });
        }
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(rows)
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
