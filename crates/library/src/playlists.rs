//! Owns Playlist identity, exact duplicate occurrences, ordering, filtering, and edits.
//! Entries keep song references; song metadata belongs to the shared library.

use sqlx::{Connection, FromRow, QueryBuilder, Sqlite, SqliteConnection};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};

use crate::{
    CatalogSort, Database, FolderKey, LibraryError, LibraryResult, PlaylistEntryKey, PlaylistKey,
    ReadCancellation, SourceKey, TrackKey, TrackSelection,
};

#[derive(Clone, Debug, PartialEq, FromRow)]
pub struct PlaylistAccess {
    pub source_key: Option<SourceKey>,
    pub source_id: Option<String>,
    pub object_id: String,
    pub source_owned: bool,
    pub writable: bool,
    pub metadata_writable: bool,
}

pub(crate) const PLAYLIST_ROW_LIMIT: usize = 128;
const PLAYLIST_DELETE_BATCH: usize = 400;

/// Stage a large removal on disk; change the playlist once after checks finish.
pub struct PlaylistEntryRemovals {
    file: BufWriter<std::fs::File>,
    count: usize,
}

impl PlaylistEntryRemovals {
    pub fn new() -> LibraryResult<Self> {
        Ok(Self {
            file: BufWriter::new(tempfile::tempfile()?),
            count: 0,
        })
    }

    pub fn push(&mut self, entry: PlaylistEntryKey) -> LibraryResult<()> {
        self.file.write_all(&entry.raw().to_le_bytes())?;
        self.count += 1;
        Ok(())
    }

    pub async fn apply(
        mut self,
        database: &Database,
        source: Option<SourceKey>,
        playlist: PlaylistKey,
    ) -> LibraryResult<usize> {
        if self.count == 0 {
            return Ok(0);
        }
        self.file.flush()?;
        self.file.seek(SeekFrom::Start(0))?;
        let mut reader = BufReader::new(self.file.get_ref());
        let entries = (0..self.count).map(move |_| {
            let mut bytes = [0; 8];
            reader.read_exact(&mut bytes)?;
            Ok(PlaylistEntryKey::from_raw(i64::from_le_bytes(bytes)))
        });
        database
            .remove_playlist_entry_iter(source, playlist, entries)
            .await
    }
}

#[derive(Clone, Debug, PartialEq, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlaylistRow {
    pub playlist_key: PlaylistKey,
    pub source_key: Option<SourceKey>,
    pub source_id: Option<String>,
    pub source_owned: bool,
    pub object_id: String,
    pub name: String,
    pub writable: bool,
    pub metadata_writable: bool,
    pub artwork_binding: Option<Vec<u8>>,
    pub track_count: i64,
    pub duration_millis: i64,
    pub downloaded_count: i64,
    #[sqlx(skip)]
    pub genres: Vec<PlaylistGenreLink>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlaylistGenreLink {
    pub genre_key: crate::GenreKey,
    pub name: String,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct PlaylistEditEntry {
    pub playlist_entry_key: PlaylistEntryKey,
    pub track_key: TrackKey,
    pub object_id: String,
    #[sqlx(try_from = "i64")]
    pub position: usize,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlaylistEntryView {
    pub playlist_entry_key: PlaylistEntryKey,
    pub position: i64,
    pub source_entry_id: Option<String>,
    pub track: crate::TrackView,
}

impl From<PlaylistEntryView> for crate::TrackReference {
    fn from(entry: PlaylistEntryView) -> Self {
        entry.track.into()
    }
}

impl Database {
    pub async fn playlist_access(
        &self,
        playlist: PlaylistKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistAccess>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_as("SELECT playlist.source_key,source.object_id source_id,playlist.object_id,
            playlist.owner='source' source_owned,playlist.writable,playlist.metadata_writable
            FROM playlists playlist LEFT JOIN sources source USING(source_key) WHERE playlist_key=?1")
            .bind(playlist).fetch_optional(&mut *connection).await?)
    }

    pub async fn playlist_key_by_object(
        &self,
        source: SourceKey,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(
            "SELECT playlist_key FROM playlists WHERE source_key=?1 AND object_id=?2",
        )
        .bind(source)
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn playlist_key_by_identity(
        &self,
        source_id: Option<&crate::SourceId>,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(
            "SELECT playlist_key FROM playlists
             WHERE source_key IS (SELECT source_key FROM sources WHERE object_id=?1)
               AND (?1 IS NULL OR source_key IS NOT NULL) AND object_id=?2 AND name IS NOT NULL",
        )
        .bind(source_id.map(crate::SourceId::as_str))
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn all_playlist_track_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT track.media_uri FROM tracks track
             WHERE track.source_key=?1
               AND EXISTS (SELECT 1 FROM playlist_entries entry WHERE entry.track_key=track.track_key)
               AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?2))
             ORDER BY track.sort_text,track.track_key",
        )
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn playlist_destinations(
        &self,
        source: Option<SourceKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let query = playlist_list_query(source, None, CatalogSort::Position, false, "");
        let order: Vec<PlaylistKey> = sqlx::query_scalar_with(
            sqlx::AssertSqlSafe(query.select(&query.key)),
            query.arguments,
        )
        .fetch_all(&mut *transaction)
        .await?;
        let mut rows = Vec::with_capacity(order.len());
        for keys in order.chunks(PLAYLIST_ROW_LIMIT) {
            rows.extend(Self::load_playlist_rows(&mut transaction, keys).await?);
        }
        rows.retain(|row| row.writable);
        for row in &mut rows {
            row.genres = load_playlist_genres(&mut transaction, row.playlist_key).await?;
        }
        transaction.commit().await?;
        Ok(rows)
    }

    pub async fn playlist_genres(
        &self,
        key: PlaylistKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistGenreLink>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        load_playlist_genres(&mut connection, key).await
    }

    pub(crate) async fn load_playlist_rows(
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
        let mut result = Vec::<PlaylistRow>::with_capacity(keys.len());
        for key in keys {
            let Some(mut row) = sqlx::query_as::<_, PlaylistRow>(sqlx::AssertSqlSafe(format!(
                "SELECT playlist.playlist_key,playlist.source_key,source.object_id source_id,
                        playlist.owner='source' source_owned,playlist.object_id,
                        playlist.name,playlist.writable,playlist.metadata_writable,{} artwork_binding,
                        (SELECT count(*) FROM playlist_entries WHERE playlist_key=playlist.playlist_key) track_count,(SELECT COALESCE(sum(duration_millis),0) FROM playlist_entries JOIN tracks USING(track_key) WHERE playlist_key=playlist.playlist_key) duration_millis,(SELECT count(*) FROM playlist_entries entry JOIN tracks USING(track_key) WHERE entry.playlist_key=playlist.playlist_key AND EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=tracks.media_uri AND origin='download')) downloaded_count
                 FROM playlists playlist
                 LEFT JOIN sources source USING(source_key)
                 WHERE playlist.playlist_key=?1 AND playlist.name IS NOT NULL", crate::artwork::preview_sql("playlist.artwork_binding"))))
            .bind(key).fetch_optional(&mut *connection).await? else {
                continue;
            };
            row.artwork_binding = Some(crate::artwork::playlist_artwork(
                row.artwork_binding,
                (false, row.source_id.clone(), row.object_id.clone()),
                (None, None),
            )?);
            result.push(row);
        }
        Ok(result)
    }

    pub async fn playlist_edit_entries(
        &self,
        playlist: PlaylistKey,
        selection: &TrackSelection,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistEditEntry>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let mut pending = vec![selection];
        let mut entries = Vec::new();
        const COLUMNS: &str =
            "entry.playlist_entry_key,entry.track_key,entry.object_id,entry.position";
        while let Some(selection) = pending.pop() {
            match selection {
                TrackSelection::Concat(parts) => pending.extend(parts.iter().rev()),
                TrackSelection::Query { query, ranges }
                    if matches!(
                        query.subject,
                        crate::CatalogSubject::Tracks {
                            scope: crate::TrackScope::Playlist(_),
                            ..
                        } | crate::CatalogSubject::PlaylistEntries { .. }
                    ) =>
                {
                    let mut query = crate::query::compile_on(&mut transaction, query).await?;
                    query
                        .predicate
                        .push_str(&format!(" AND entry.playlist_key={}", playlist.raw()));
                    entries.extend(
                        crate::query::selected_rows_on::<PlaylistEditEntry>(
                            &mut transaction,
                            &query,
                            COLUMNS,
                            ranges.as_deref(),
                            None,
                        )
                        .await?,
                    );
                }
                _ => {
                    return Err(LibraryError::InvalidRequest(
                        "Selection does not contain playlist entries".into(),
                    ));
                }
            }
        }
        transaction.commit().await?;
        entries.sort_by_key(|entry| entry.position);
        Ok(entries)
    }

    pub async fn playlist_entry_uri_page(
        &self,
        playlist: PlaylistKey,
        after_position: i64,
    ) -> LibraryResult<Vec<(PlaylistEntryKey, i64, String)>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as(
            "SELECT playlist_entry_key,position,media_uri FROM playlist_entries JOIN tracks USING(track_key)
            WHERE playlist_key=?1 AND position>?2 ORDER BY position LIMIT ?3",
        )
        .bind(playlist)
        .bind(after_position)
        .bind(256_i64)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn source_playlist_object_ids(
        &self,
        source: SourceKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(
            "SELECT object_id FROM playlists WHERE source_key=?1 AND name IS NOT NULL AND (owner='rufin' OR indexed) ORDER BY sort_text,playlist_key",
        )
        .bind(source)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn source_playlist_track_ids(
        &self,
        source: SourceKey,
        playlist: Option<PlaylistKey>,
        songs: &TrackSelection,
        skip_existing: bool,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        let mut transaction = connection.begin().await?;
        let tracks = playlist_tracks_on(&mut transaction, Some(source), songs)
            .await?
            .ok_or_else(|| {
                LibraryError::InvalidRequest("Playlist Track is no longer current".into())
            })?;
        let ids = sqlx::query_scalar("SELECT track.object_id FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value
            WHERE NOT ?2 OR ?3 IS NULL OR NOT EXISTS(SELECT 1 FROM playlist_entries existing WHERE existing.playlist_key=?3 AND existing.track_key=track.track_key)
            ORDER BY CAST(requested.key AS INTEGER)")
            .bind(serde_json::to_string(&tracks)?).bind(skip_existing).bind(playlist)
            .fetch_all(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(ids)
    }

    pub async fn create_playlist(
        &self,
        source: Option<SourceKey>,
        name: &str,
        songs: &TrackSelection,
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
        let Some(tracks) = playlist_tracks_on(&mut transaction, source, songs).await? else {
            return Ok(None);
        };
        let (playlist, object_id) =
            create_playlist_identity(&mut transaction, source, name).await?;
        insert_playlist_tracks(&mut transaction, playlist, 0, &tracks, false).await?;
        transaction.commit().await?;
        Ok(Some((playlist, object_id)))
    }

    pub async fn update_playlist(
        &self,
        source: Option<SourceKey>,
        playlist: PlaylistKey,
        name: Option<&str>,
        artwork: Option<Option<(&[u8], Option<&str>)>>,
    ) -> LibraryResult<bool> {
        let name = name.map(str::trim);
        if name.is_some_and(str::is_empty) {
            return Err(LibraryError::InvalidRequest(
                "Playlist name cannot be empty".into(),
            ));
        }
        let image = artwork.flatten();
        let revision = image.map(|(bytes, _)| blake3::hash(bytes).to_hex().to_string());
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let binding = if artwork.is_some() {
            let (source_id, object_id, previous): (Option<String>, String, Option<Vec<u8>>) = sqlx::query_as("SELECT source.object_id,playlist.object_id,playlist.artwork_binding FROM playlists playlist LEFT JOIN sources source USING(source_key) WHERE playlist_key=?1").bind(playlist).fetch_one(&mut *transaction).await?;
            let incoming = revision.as_ref().map(|revision| {
                crate::artwork::playlist_binding(false, source_id, object_id, revision.clone())
            });
            crate::artwork::merge_playlist_artwork(previous.as_deref(), incoming.as_deref())?
        } else {
            None
        };
        let changed = sqlx::query(
            "UPDATE main.playlists SET name=COALESCE(?3,name), normalized_name=COALESCE(lower(?3),normalized_name), sort_text=COALESCE(lower(?3),sort_text),
             artwork_bytes=CASE WHEN ?4 THEN ?5 ELSE artwork_bytes END,
             artwork_mime=CASE WHEN ?4 THEN ?6 ELSE artwork_mime END,
             artwork_revision=CASE WHEN ?4 THEN ?7 ELSE artwork_revision END,
             artwork_binding=CASE WHEN ?4 THEN ?8 ELSE artwork_binding END
             WHERE source_key IS ?1 AND owner='rufin'
               AND playlist_key=?2 AND metadata_writable AND ((?3 IS NOT NULL AND name IS NOT ?3) OR (?4 AND artwork_revision IS NOT ?7))",
        ).bind(source).bind(playlist).bind(name).bind(artwork.is_some())
            .bind(image.map(|(bytes, _)| bytes)).bind(image.and_then(|(_, mime)| mime)).bind(revision).bind(binding)
            .execute(&mut *transaction).await?.rows_affected() == 1;
        if changed && name.is_some() {
            crate::playlist_links::mark_dirty_on(&mut transaction, playlist).await?;
        }
        if changed {
            sync_playlist_header_on(&mut transaction, playlist).await?;
        }
        transaction.commit().await?;
        Ok(changed)
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
        let visible = sqlx::query_as::<_, (PlaylistKey, i64)>(
            "SELECT playlist_key,position FROM playlists
             WHERE name IS NOT NULL AND (owner='rufin' OR indexed) AND (?1 IS NULL OR source_key=?1 OR source_key IS NULL)
             ORDER BY position,playlist_key",
        )
        .bind(source)
        .fetch_all(&mut *transaction)
        .await?;
        let Some(dragged_index) = visible.iter().position(|(key, _)| *key == dragged) else {
            transaction.rollback().await?;
            return Ok(false);
        };
        let Some(target_index) = visible.iter().position(|(key, _)| *key == target) else {
            transaction.rollback().await?;
            return Ok(false);
        };
        let mut order = visible.iter().map(|(key, _)| *key).collect::<Vec<_>>();
        let dragged = order.remove(dragged_index);
        let insertion = if dragged_index < target_index {
            target_index - 1
        } else {
            target_index
        };
        order.insert(insertion, dragged);
        let positions = order
            .into_iter()
            .zip(visible)
            .map(|(key, (_, position))| (key, position))
            .collect::<Vec<_>>();
        write_positions_on(&mut transaction, &positions).await?;
        for (key, _) in positions {
            sync_playlist_header_on(&mut transaction, key).await?;
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
        let mut transaction = connection.begin().await?;
        let identity:Option<String> = sqlx::query_scalar("DELETE FROM playlists WHERE source_key IS ?1 AND owner='rufin' AND playlist_key=?2 RETURNING json_array((SELECT object_id FROM sources WHERE source_key=playlists.source_key),object_id)")
            .bind(source).bind(playlist).fetch_optional(&mut *transaction).await?;
        if let Some(key) = &identity {
            crate::profile::write_local_on(
                &mut transaction,
                &[
                    crate::ConnectRecord {
                        kind: "playlist".into(),
                        key: key.clone(),
                        value: None,
                    },
                    crate::ConnectRecord {
                        kind: "playlist_artwork".into(),
                        key: key.clone(),
                        value: None,
                    },
                ],
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(identity.is_some())
    }

    pub async fn add_playlist_tracks(
        &self,
        source: Option<SourceKey>,
        playlist: PlaylistKey,
        songs: &TrackSelection,
        skip_existing: bool,
    ) -> LibraryResult<usize> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        if !editable_playlist_exists(&mut transaction, source, playlist).await? {
            return Ok(0);
        }
        let Some(tracks) = playlist_tracks_on(&mut transaction, source, songs).await? else {
            return Ok(0);
        };
        let next_position = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(max(position) + 1, 0)
             FROM playlist_entries WHERE playlist_key=?1",
        )
        .bind(playlist)
        .fetch_one(&mut *transaction)
        .await?;
        let accepted = insert_playlist_tracks(
            &mut transaction,
            playlist,
            next_position,
            &tracks,
            skip_existing,
        )
        .await?;
        if accepted > 0 {
            crate::playlist_links::mark_dirty_on(&mut transaction, playlist).await?;
        }
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
        self.remove_playlist_entry_iter(source, playlist, entries.iter().copied().map(Ok))
            .await
    }

    async fn remove_playlist_entry_iter(
        &self,
        source: Option<SourceKey>,
        playlist: PlaylistKey,
        mut entries: impl Iterator<Item = LibraryResult<PlaylistEntryKey>> + Send,
    ) -> LibraryResult<usize> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        if !editable_playlist_exists(&mut transaction, source, playlist).await? {
            transaction.rollback().await?;
            return Ok(0);
        }
        let mut removed = 0usize;
        loop {
            let batch = entries
                .by_ref()
                .take(PLAYLIST_DELETE_BATCH)
                .collect::<LibraryResult<Vec<_>>>()?;
            if batch.is_empty() {
                break;
            }
            let deleted:Vec<(String,i64)> = sqlx::query_as("SELECT json_array((SELECT object_id FROM sources WHERE source_key=playlist.source_key),playlist.object_id,entry.object_id),entry.position FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) JOIN json_each(?2) requested ON entry.playlist_entry_key=requested.value WHERE playlist.playlist_key=?1")
                .bind(playlist).bind(serde_json::to_string(&batch)?).fetch_all(&mut *transaction).await?;
            let tombstones = deleted
                .into_iter()
                .map(|(key, _position)| crate::ConnectRecord {
                    kind: "entry".into(),
                    key,
                    value: None,
                })
                .collect::<Vec<_>>();
            crate::profile::write_local_on(&mut transaction, &tombstones).await?;
            let mut query = QueryBuilder::<Sqlite>::new(
                "DELETE FROM main.playlist_entries WHERE playlist_key=",
            );
            query
                .push_bind(playlist)
                .push(" AND playlist_entry_key IN (");
            let mut separated = query.separated(", ");
            for entry in &batch {
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
        if removed > 0 {
            // Vacate the compact range before assigning positions in row-key order.
            sqlx::query("UPDATE main.playlist_entries SET position=position+(SELECT COALESCE(max(position)+1,0) FROM playlist_entries WHERE playlist_key=?1) WHERE playlist_key=?1")
                .bind(playlist).execute(&mut *transaction).await?;
            sqlx::query(
                "WITH positions AS MATERIALIZED (
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
            crate::playlist_links::mark_dirty_on(&mut transaction, playlist).await?;
        }
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
        crate::playlist_links::mark_dirty_on(&mut transaction, playlist).await?;
        let records =
            playlist_entry_records_on(&mut transaction, playlist, new_position, new_position)
                .await?;
        crate::profile::write_local_on(&mut transaction, &records).await?;
        transaction.commit().await?;
        Ok(true)
    }
}

pub(crate) async fn sync_playlist_header_on(
    connection: &mut SqliteConnection,
    playlist: PlaylistKey,
) -> LibraryResult<()> {
    let active:bool = sqlx::query_scalar("SELECT profile IS NOT NULL AND enabled AND NOT receiving FROM connect_profile WHERE singleton=1").fetch_one(&mut *connection).await?;
    if !active {
        return Ok(());
    }

    let identity: Option<(String, bool)> = sqlx::query_as("SELECT json_array((SELECT object_id FROM sources WHERE source_key=playlist.source_key),playlist.object_id),owner='rufin' FROM playlists playlist WHERE playlist_key=?1")
        .bind(playlist).fetch_optional(&mut *connection).await?;
    let Some((key, authored)) = identity else {
        return Ok(());
    };
    let mut records = Vec::new();
    for kind in if authored {
        &["playlist", "playlist_artwork"][..]
    } else {
        &["playlist_order"][..]
    } {
        records.push(Database::connect_record_on(connection, kind, &key).await?);
    }
    crate::profile::write_local_on(connection, &records).await
}

pub(crate) async fn playlist_entry_records_on(
    connection: &mut SqliteConnection,
    playlist: PlaylistKey,
    start: i64,
    end: i64,
) -> LibraryResult<Vec<crate::ConnectRecord>> {
    let active:bool = sqlx::query_scalar("SELECT profile IS NOT NULL AND enabled AND NOT receiving FROM connect_profile WHERE singleton=1").fetch_one(&mut *connection).await?;
    if !active {
        return Ok(Vec::new());
    }

    let keys: Vec<(String, String, bool)> = sqlx::query_as("SELECT json_array((SELECT object_id FROM sources WHERE source_key=playlist.source_key),playlist.object_id,entry.object_id),track.media_uri,NOT EXISTS(SELECT 1 FROM connect_versions WHERE name='reference:'||track.media_uri AND deleted=0) FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) JOIN tracks track USING(track_key) WHERE playlist.playlist_key=?1 AND playlist.owner='rufin' AND entry.position BETWEEN ?2 AND ?3 ORDER BY entry.position")
        .bind(playlist).bind(start).bind(end).fetch_all(&mut *connection).await?;
    let mut records = Vec::with_capacity(keys.len());
    let mut references = std::collections::HashSet::new();
    for (key, uri, missing) in keys {
        if missing && references.insert(uri.clone()) {
            records.push(Database::connect_record_on(connection, "reference", &uri).await?);
        }
        records.push(Database::connect_record_on(connection, "entry", &key).await?);
    }
    Ok(records)
}

async fn editable_playlist_exists(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    source: Option<SourceKey>,
    playlist: PlaylistKey,
) -> LibraryResult<bool> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM playlists
         WHERE ((?1 IS NULL AND source_key IS NULL) OR source_key=?1)
           AND playlist_key=?2 AND owner='rufin' AND writable",
    )
    .bind(source)
    .bind(playlist)
    .fetch_optional(&mut **transaction)
    .await?
    .is_some())
}

async fn playlist_tracks_on(
    connection: &mut SqliteConnection,
    source: Option<SourceKey>,
    selection: &TrackSelection,
) -> LibraryResult<Option<Vec<TrackKey>>> {
    let mut pending = vec![selection];
    let mut tracks = Vec::new();
    while let Some(selection) = pending.pop() {
        match selection {
            TrackSelection::Concat(parts) => pending.extend(parts.iter().rev()),
            TrackSelection::Query {
                query:
                    crate::CatalogQuery {
                        subject:
                            crate::CatalogSubject::Uris {
                                kind: crate::CatalogKind::Track,
                                uris,
                                ..
                            },
                        ..
                    },
                ranges: None,
            } => {
                let missing:Vec<String>=sqlx::query_scalar("SELECT DISTINCT requested.value FROM json_each(?1) requested WHERE NOT EXISTS(SELECT 1 FROM tracks WHERE media_uri=requested.value)").bind(serde_json::to_string(uris)?).fetch_all(&mut *connection).await?;
                if source.is_some() && !missing.is_empty() {
                    return Ok(None);
                }
                for uri in missing {
                    crate::catalog::remember_track(
                        connection,
                        &crate::TrackReference {
                            media_uri: uri,
                            ..Default::default()
                        },
                    )
                    .await?;
                }
                tracks.extend(
                    crate::selection::selected_tracks_on(connection, selection)
                        .await?
                        .into_iter()
                        .map(|(key, _)| key),
                );
            }
            _ => tracks.extend(
                crate::selection::selected_tracks_on(connection, selection)
                    .await?
                    .into_iter()
                    .map(|(track, _)| track),
            ),
        }
    }
    if let Some(source) = source
        && sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM json_each(?1) requested JOIN tracks ON tracks.track_key=requested.value WHERE tracks.source_key IS NOT ?2)")
            .bind(serde_json::to_string(&tracks)?).bind(source).fetch_one(&mut *connection).await?
    {
        let id: String = sqlx::query_scalar("SELECT object_id FROM sources WHERE source_key=?1").bind(source).fetch_one(&mut *connection).await?;
        return Err(LibraryError::PlaylistSourceMismatch(crate::SourceId::new(id)));
    }
    Ok(Some(tracks))
}

#[cfg(test)]
mod point_projection_tests {
    use super::*;

    #[tokio::test]
    async fn small_native_playlist_reads_stay_bounded_as_unrelated_catalog_grows() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("library.sqlite3"))
            .await
            .unwrap();
        let mut writer = database.writer().await.unwrap();
        let connection = writer.as_mut().unwrap();
        sqlx::raw_sql("INSERT INTO sources(source_key,object_id,display_name,normalized_name,artwork_digest) VALUES (1,'source','Source','source',zeroblob(32));\n          INSERT INTO playlists(playlist_key,source_key,owner,object_id,name,normalized_name,sort_text,position)\n          VALUES(1,1,'source','small','Small','small','small',0),(2,1,'source','large','Large','large','large',1);\n          WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<15)\n          INSERT INTO tracks(track_key,source_key,object_id,media_uri,native_artwork,metadata) SELECT x,1,CAST(x AS TEXT),'track:'||x,X'01',jsonb(json_object('title',COALESCE('Title',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('title',''),'duration_millis',COALESCE(1000,0))) FROM n;\n          INSERT INTO playlist_entries(playlist_key,object_id,track_key,position)\n          SELECT 1,object_id,track_key,track_key FROM tracks;")
            .execute(&mut *connection).await.unwrap();
        for grow in [false, true] {
            if grow {
                sqlx::raw_sql("WITH RECURSIVE n(x) AS (VALUES(16) UNION ALL SELECT x+1 FROM n WHERE x<10000)\n                  INSERT INTO tracks(track_key,source_key,object_id,media_uri,native_artwork,metadata) SELECT x,1,CAST(x AS TEXT),'track:'||x,X'01',jsonb(json_object('title',COALESCE('Title',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('title',''),'duration_millis',COALESCE(1000,0))) FROM n;\n                  INSERT INTO playlist_entries(playlist_key,object_id,track_key,position)\n                  SELECT 2,object_id,track_key,track_key FROM tracks;")
                    .execute(&mut *connection).await.unwrap();
            }
            let work = Arc::new(AtomicUsize::new(0));
            let counter = work.clone();
            connection
                .lock_handle()
                .await
                .unwrap()
                .set_progress_handler(100, move || {
                    counter.fetch_add(100, Ordering::Relaxed);
                    true
                });
            let rows = Database::load_playlist_rows(connection, &[PlaylistKey::from_raw(1)])
                .await
                .unwrap();
            let entry: String = sqlx::query_scalar(
                "SELECT media_uri FROM playlist_entries JOIN tracks USING(track_key) WHERE playlist_entry_key=1",
            )
            .fetch_one(&mut *connection)
            .await
            .unwrap();
            let order: Vec<i64> = sqlx::query_scalar("SELECT playlist_entry_key FROM playlist_entries WHERE playlist_key=1 ORDER BY position,playlist_entry_key")
                .fetch_all(&mut *connection).await.unwrap();
            for sort in [CatalogSort::Artist, CatalogSort::AlbumArtist] {
                let query = playlist_query(PlaylistKey::from_raw(1), None, sort, false, "");
                let ordered = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(
                    query.select(&query.entry_key),
                ))
                .fetch_all(&mut *connection)
                .await
                .unwrap();
                assert_eq!(ordered.len(), 15);
            }
            connection
                .lock_handle()
                .await
                .unwrap()
                .remove_progress_handler();
            assert_eq!(rows[0].track_count, 15);
            assert!(rows[0].artwork_binding.is_some());
            assert_eq!(entry, "track:1");
            assert_eq!(order.len(), 15);
            assert!(
                work.load(Ordering::Relaxed) < 5000,
                "small playlist visited unrelated rows: {} instructions",
                work.load(Ordering::Relaxed)
            );
        }
    }
}

pub(crate) async fn load_playlist_entry_rows(
    connection: &mut SqliteConnection,
    keys: &[PlaylistEntryKey],
) -> LibraryResult<Vec<PlaylistEntryView>> {
    let entries = sqlx::query_as::<_, (PlaylistEntryKey,i64,String,TrackKey)>("SELECT entry.playlist_entry_key,entry.position,entry.object_id,entry.track_key FROM json_each(?1) requested JOIN playlist_entries entry ON playlist_entry_key=requested.value ORDER BY CAST(requested.key AS INTEGER)")
        .bind(serde_json::to_string(keys)?).fetch_all(&mut *connection).await?;
    let tracks = entries.iter().map(|entry| entry.3).collect::<Vec<_>>();
    let songs = crate::tracks::load_track_rows(connection, &tracks).await?;
    Ok(entries
        .into_iter()
        .zip(songs)
        .map(
            |((playlist_entry_key, position, id, _), song)| PlaylistEntryView {
                playlist_entry_key,
                position,
                source_entry_id: Some(id),
                track: song,
            },
        )
        .collect())
}

pub(crate) async fn create_playlist_identity(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    source: Option<SourceKey>,
    name: &str,
) -> LibraryResult<(PlaylistKey, String)> {
    let name = name.trim();
    if name.is_empty() {
        return Err(LibraryError::InvalidRequest(
            "Playlist name cannot be empty".into(),
        ));
    }
    let created: (PlaylistKey, String) = sqlx::query_as(
        "INSERT INTO main.playlists(source_key,object_id,name,normalized_name,sort_text,position)
         SELECT ?1,id,?2,lower(?2),lower(?2),
                (SELECT coalesce(max(position)+1,0) FROM playlists)
         FROM (SELECT 'rufin:playlist:'||lower(hex(randomblob(16))) id)
         RETURNING playlist_key,object_id",
    )
    .bind(source)
    .bind(name)
    .fetch_one(&mut **transaction)
    .await?;
    sync_playlist_header_on(transaction, created.0).await?;
    Ok(created)
}

async fn insert_playlist_tracks(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    playlist: PlaylistKey,
    start: i64,
    tracks: &[TrackKey],
    skip_existing: bool,
) -> LibraryResult<usize> {
    let accepted = sqlx::query(
        "INSERT INTO playlist_entries(playlist_key,object_id,track_key,position)
        SELECT ?1,'rufin:entry:'||lower(hex(randomblob(16))),requested.value,
               ?2+row_number() OVER (ORDER BY CAST(requested.key AS INTEGER))-1
        FROM json_each(?3) requested WHERE NOT ?4 OR NOT EXISTS(
            SELECT 1 FROM playlist_entries existing WHERE existing.playlist_key=?1
                AND existing.track_key=requested.value AND existing.position<?2)
        ORDER BY CAST(requested.key AS INTEGER)",
    )
    .bind(playlist)
    .bind(start)
    .bind(serde_json::to_string(tracks)?)
    .bind(skip_existing)
    .execute(&mut **transaction)
    .await?
    .rows_affected() as usize;
    if accepted > 0 {
        let records = playlist_entry_records_on(transaction, playlist, start, i64::MAX).await?;
        crate::profile::write_local_on(transaction, &records).await?;
    }
    Ok(accepted)
}

/// Playlist identity and rank in backups and exported files.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct PlaylistIdentity {
    #[serde(default)]
    pub source_owned: bool,
    pub source_id: Option<String>,
    pub object_id: String,
    pub name: Option<String>,
    pub position: i64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct PlaylistEntryWrite {
    pub object_id: String,
    pub position: i64,
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub track: crate::TrackReference,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "record")]
enum PlaylistRecord {
    Playlist(PlaylistIdentity),
    File(crate::playlist_links::PlaylistFileRecord),
    Artwork(PlaylistArtworkRecord),
}

#[derive(serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub(crate) struct PlaylistArtworkRecord {
    source_id: Option<String>,
    object_id: String,
    #[serde(default)]
    bytes: Option<Vec<u8>>,
    content_type: Option<String>,
    #[serde(default)]
    artwork_binding: Option<Vec<u8>>,
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
    let source_owned = identity.source_owned || identity.name.is_none();
    if source_owned && let Some(source) = source {
        return crate::catalog::remember_collection(
            connection,
            "playlist",
            SourceKey::from_raw(source),
            &identity.object_id,
            None,
            &serde_json::json!({"position":identity.position}),
            None,
        )
        .await
        .map(PlaylistKey::from_raw);
    }
    let owner = if source_owned { "source" } else { "rufin" };
    let existing = sqlx::query_scalar::<_, PlaylistKey>(
        "SELECT playlist_key FROM playlists WHERE object_id=?1 AND source_key IS ?2",
    )
    .bind(&identity.object_id)
    .bind(source)
    .fetch_optional(&mut *connection)
    .await?;
    if let Some(key) = existing {
        if !source_owned {
            sqlx::query("UPDATE playlists SET name=?2,normalized_name=lower(?2),sort_text=lower(?2) WHERE playlist_key=?1 AND owner='rufin'")
                .bind(key).bind(&identity.name).execute(&mut *connection).await?;
        }
        return Ok(key);
    }
    Ok(sqlx::query_scalar("INSERT INTO playlists(source_key,owner,object_id,name,normalized_name,sort_text,position,indexed)
        VALUES(?1,?2,?3,?4,lower(?4),lower(?4),CASE WHEN EXISTS(SELECT 1 FROM playlists WHERE position=?5)
        THEN (SELECT COALESCE(max(position)+1,0) FROM playlists) ELSE ?5 END,?6) RETURNING playlist_key")
        .bind(source).bind(owner).bind(&identity.object_id).bind(&identity.name).bind(identity.position)
        .bind(!source_owned)
        .fetch_one(connection).await?)
}

pub(crate) async fn write_playlist_entry(
    connection: &mut SqliteConnection,
    playlist: PlaylistKey,
    entry: &PlaylistEntryWrite,
) -> LibraryResult<()> {
    if entry.object_id.is_empty() || entry.track.media_uri.is_empty() || entry.position < 0 {
        return Err(LibraryError::InvalidRequest(
            "invalid playlist occurrence identity or order".into(),
        ));
    }
    let track = remember_playlist_track(connection, entry).await?;
    write_entry_positions_on(
        connection,
        &[(
            playlist,
            entry.object_id.clone(),
            Some(track),
            entry.position,
        )],
    )
    .await?;
    Ok(())
}

/// Store accepted playlist positions.
pub(crate) async fn write_positions_on(
    connection: &mut SqliteConnection,
    positions: &[(PlaylistKey, i64)],
) -> LibraryResult<bool> {
    let mut changed = false;
    for page in positions.chunks(PLAYLIST_ROW_LIMIT) {
        let rows = serde_json::to_string(page)?;
        changed |= sqlx::query(
            "WITH input AS MATERIALIZED(SELECT json_extract(value,'$[0]') id,json_extract(value,'$[1]') position FROM json_each(?1)),
             changed AS MATERIALIZED(SELECT input.* FROM input JOIN playlists old ON old.playlist_key=input.id WHERE old.position<>input.position),
             affected AS MATERIALIZED(SELECT old.playlist_key id,old.position FROM changed input JOIN playlists old ON old.playlist_key=input.id OR old.position=input.position),
             moved AS MATERIALIZED(SELECT id,(SELECT coalesce(max(position),-1)+1 FROM playlists)+row_number() OVER(ORDER BY position) position FROM (SELECT DISTINCT id,position FROM affected))
             UPDATE playlists SET position=moved.position FROM moved WHERE playlists.playlist_key=moved.id"
        ).bind(&rows).execute(&mut *connection).await?.rows_affected()>0;
        changed |= sqlx::query(
            "WITH input AS(SELECT json_extract(value,'$[0]') id,json_extract(value,'$[1]') position FROM json_each(?1))
             UPDATE playlists SET position=input.position FROM input WHERE playlists.playlist_key=input.id AND playlists.position<>input.position"
        ).bind(rows).execute(&mut *connection).await?.rows_affected()>0;
    }
    Ok(changed)
}

/// Store accepted occurrence identities and their final positions.
pub(crate) async fn write_entry_positions_on(
    connection: &mut SqliteConnection,
    entries: &[(PlaylistKey, String, Option<TrackKey>, i64)],
) -> LibraryResult<bool> {
    let mut changed = false;
    let input = "WITH input AS MATERIALIZED(SELECT json_extract(value,'$[0]') playlist_key,json_extract(value,'$[1]') object_id,json_extract(value,'$[2]') track_key,json_extract(value,'$[3]') position,json_extract(value,'$[2]') IS NOT NULL present FROM json_each(?1)) ";
    for page in entries.chunks(PLAYLIST_ROW_LIMIT) {
        let rows = serde_json::to_string(page)?;
        changed |= sqlx::query(sqlx::AssertSqlSafe(format!("{input} DELETE FROM playlist_entries WHERE playlist_entry_key IN(SELECT old.playlist_entry_key FROM input CROSS JOIN playlist_entries old ON old.playlist_key=input.playlist_key AND old.object_id=input.object_id WHERE NOT input.present)")))
            .bind(&rows).execute(&mut *connection).await?.rows_affected()>0;
        sqlx::query(sqlx::AssertSqlSafe(format!("{input},changed AS MATERIALIZED(SELECT input.* FROM input LEFT JOIN playlist_entries old ON old.playlist_key=input.playlist_key AND old.object_id=input.object_id WHERE input.present AND(old.track_key,old.position) IS NOT(input.track_key,input.position)),affected AS MATERIALIZED(SELECT old.playlist_entry_key,old.playlist_key FROM changed input CROSS JOIN playlist_entries old ON old.playlist_key=input.playlist_key AND old.object_id=input.object_id UNION SELECT old.playlist_entry_key,old.playlist_key FROM changed input CROSS JOIN playlist_entries old ON old.playlist_key=input.playlist_key AND old.position=input.position),moved AS MATERIALIZED(SELECT playlist_entry_key,(SELECT coalesce(max(position),-1)+1 FROM playlist_entries WHERE playlist_key=affected.playlist_key)+playlist_entry_key position FROM affected) UPDATE playlist_entries SET position=moved.position FROM moved WHERE playlist_entries.playlist_entry_key=moved.playlist_entry_key")))
            .bind(&rows).execute(&mut *connection).await?;
        changed |= sqlx::query(sqlx::AssertSqlSafe(format!("{input} INSERT INTO playlist_entries(playlist_key,object_id,track_key,position) SELECT playlist_key,object_id,track_key,position FROM input WHERE present ON CONFLICT(playlist_key,object_id) DO UPDATE SET track_key=excluded.track_key,position=excluded.position WHERE(track_key,position) IS NOT(excluded.track_key,excluded.position)")))
            .bind(rows).execute(&mut *connection).await?.rows_affected()>0;
    }
    Ok(changed)
}

pub(crate) async fn remember_playlist_track(
    connection: &mut SqliteConnection,
    entry: &PlaylistEntryWrite,
) -> LibraryResult<crate::TrackKey> {
    crate::catalog::remember_track(connection, &entry.track).await
}

pub(crate) async fn export_playlist_order_jsonl_on(
    connection: &mut SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<u64> {
    use futures_util::TryStreamExt;
    let mut rows=sqlx::query_as::<_,PlaylistIdentity>("SELECT playlist.owner='source' source_owned,source.object_id source_id,playlist.object_id,CASE WHEN playlist.owner='rufin' THEN playlist.name END name,playlist.position FROM main.playlists playlist LEFT JOIN sources source USING(source_key) ORDER BY playlist.position").fetch(&mut *connection);
    let mut count = 0;
    while let Some(row) = rows.try_next().await? {
        serde_json::to_writer(&mut output, &PlaylistRecord::Playlist(row))?;
        output.write_all(b"\n")?;
        count += 1;
    }
    drop(rows);
    let mut images = sqlx::query_as::<_, PlaylistArtworkRecord>("SELECT source.object_id source_id,playlist.object_id,playlist.artwork_bytes bytes,playlist.artwork_mime content_type,playlist.artwork_binding FROM main.playlists playlist LEFT JOIN sources source USING(source_key) WHERE playlist.artwork_binding IS NOT NULL").fetch(&mut *connection);
    while let Some(image) = images.try_next().await? {
        serde_json::to_writer(&mut output, &PlaylistRecord::Artwork(image))?;
        output.write_all(b"\n")?;
    }
    drop(images);

    crate::playlist_links::backup_records_on(connection, |record| {
        serde_json::to_writer(&mut output, &PlaylistRecord::File(record))?;
        output.write_all(b"\n")?;
        Ok(())
    })
    .await?;
    Ok(count)
}
pub(crate) async fn import_playlists_jsonl_on(
    connection: &mut SqliteConnection,
    input: impl std::io::BufRead,
) -> LibraryResult<u64> {
    let mut count = 0;
    for line in input.lines() {
        match serde_json::from_str(&line?)? {
            PlaylistRecord::Playlist(identity) => {
                write_playlist_identity(connection, &identity).await?;
                count += 1;
            }
            PlaylistRecord::Artwork(image) => {
                write_playlist_artwork(connection, &image).await?;
            }
            PlaylistRecord::File(record) => {
                crate::playlist_links::restore_record_on(connection, record).await?
            }
        }
    }
    Ok(count)
}

pub(crate) async fn write_playlist_artwork(
    connection: &mut SqliteConnection,
    image: &PlaylistArtworkRecord,
) -> LibraryResult<bool> {
    let previous: Option<(bool, Option<Vec<u8>>)> = sqlx::query_as("SELECT owner='source',artwork_binding FROM playlists WHERE object_id=?1 AND source_key IS (SELECT source_key FROM sources WHERE object_id=?2)")
        .bind(&image.object_id).bind(&image.source_id).fetch_optional(&mut *connection).await?;
    let revision = image
        .bytes
        .as_ref()
        .map(|bytes| blake3::hash(bytes).to_hex().to_string());
    let incoming = image
        .artwork_binding
        .as_deref()
        .map(crate::normalize_artwork_binding)
        .transpose()?
        .or_else(|| {
            revision.as_ref().map(|revision| {
                crate::artwork::playlist_binding(
                    false,
                    image.source_id.clone(),
                    image.object_id.clone(),
                    revision.clone(),
                )
            })
        });
    let binding = if previous.as_ref().is_some_and(|(native, _)| *native) {
        let current = previous
            .as_ref()
            .and_then(|(_, binding)| binding.as_deref())
            .map(crate::artwork::authored_playlist_artwork)
            .transpose()?
            .flatten();
        crate::artwork::merge_playlist_artwork(incoming.as_deref(), current.as_deref())?
    } else {
        crate::artwork::merge_playlist_artwork(
            previous
                .as_ref()
                .and_then(|(_, binding)| binding.as_deref()),
            incoming.as_deref(),
        )?
    };
    Ok(sqlx::query("UPDATE main.playlists SET artwork_bytes=?3,artwork_mime=?4,artwork_revision=?5,artwork_binding=?6 WHERE object_id=?1 AND source_key IS (SELECT source_key FROM sources WHERE object_id=?2) AND (artwork_bytes,artwork_mime,artwork_revision,artwork_binding) IS NOT (?3,?4,?5,?6)")
        .bind(&image.object_id).bind(&image.source_id).bind(&image.bytes).bind(&image.content_type).bind(revision).bind(binding).execute(&mut *connection).await?.rows_affected()>0)
}

async fn load_playlist_genres(
    connection: &mut SqliteConnection,
    key: PlaylistKey,
) -> LibraryResult<Vec<PlaylistGenreLink>> {
    Ok(sqlx::query_as::<_, (crate::GenreKey, String)>(
        "SELECT genre.genre_key,genre.name FROM playlist_entries entry
         CROSS JOIN track_genres relation USING(track_key)
         JOIN genres genre USING(genre_key)
         WHERE entry.playlist_key=?1 GROUP BY genre.genre_key
         ORDER BY count(*) DESC,genre.sort_text,genre.genre_key LIMIT 2",
    )
    .bind(key)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|(genre_key, name)| PlaylistGenreLink { genre_key, name })
    .collect())
}

pub(crate) fn playlist_query(
    key: PlaylistKey,
    folder: Option<FolderKey>,
    sort: CatalogSort,
    descending: bool,
    filter: &str,
) -> crate::query::OrderedQuery {
    let mut query = crate::query::OrderedQuery {
        albumless:false,
        key: "entry.playlist_entry_key".into(),
        text: crate::scroll_sections::text_sort(sort).then(|| crate::scroll_sections::order_text(&sort.order_terms(descending,true)[0]).to_string()),
        from: "playlist_entries entry JOIN playlists playlist USING(playlist_key) JOIN tracks track USING(track_key)".into(),
        predicate: format!("entry.playlist_key={}", key.raw()),
        uri: "track.media_uri".into(),
        entry_key: "entry.playlist_entry_key".into(),
        arguments: Default::default(),
        order: sort.order_terms(descending, true)
    };
    if sort != CatalogSort::Position {
        query.order.push("entry.position ASC".into());
    }
    if let Some(folder) = folder {
        query.predicate.push_str(&format!(" AND (playlist.source_key IS NULL OR EXISTS(SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.track_key=entry.track_key AND scope.folder_key={}))",folder.raw()));
    }
    crate::tracks::track_filter(&mut query, None, filter, false);
    query
}

pub(crate) fn playlist_list_query(
    source: Option<SourceKey>,
    folder: Option<FolderKey>,
    sort: CatalogSort,
    descending: bool,
    filter: &str,
) -> crate::query::OrderedQuery {
    let source_scope = source.map_or_else(
        || "playlist.source_key IS NULL".into(),
        |source| {
            format!(
                "(playlist.source_key={} OR playlist.source_key IS NULL)",
                source.raw()
            )
        },
    );
    let mut predicate = format!(
        "playlist.name IS NOT NULL AND (playlist.owner='rufin' OR playlist.indexed) AND {source_scope}"
    );
    if let Some(folder) = folder {
        predicate.push_str(&format!(" AND (playlist.source_key IS NULL OR EXISTS(SELECT 1 FROM playlist_entries entry JOIN track_folders scope USING(track_key) WHERE entry.playlist_key=playlist.playlist_key AND scope.folder_key={}))",folder.raw()));
    }
    if !filter.trim().is_empty() {
        predicate.push_str(&format!(
            " AND instr(playlist.normalized_name,{})>0",
            crate::query::quote(&filter.trim().to_lowercase())
        ));
    }
    let field = match sort {
        CatalogSort::Position => "playlist.position",
        CatalogSort::Title => "playlist.sort_text",
        CatalogSort::TrackCount => {
            "(SELECT count(*) FROM playlist_entries WHERE playlist_key=playlist.playlist_key)"
        }
        CatalogSort::Duration => {
            "(SELECT COALESCE(sum(duration_millis),0) FROM playlist_entries JOIN tracks USING(track_key) WHERE playlist_key=playlist.playlist_key)"
        }
        _ => unreachable!("playlist sort field"),
    };
    let direction = if descending { "DESC" } else { "ASC" };
    crate::query::OrderedQuery {
        albumless: false,
        key: "playlist.playlist_key".into(),
        text: (sort == CatalogSort::Title).then(|| "playlist.sort_text".into()),
        from: "playlists playlist".into(),
        predicate,
        order: vec![format!(
            "{field} {direction},playlist.sort_text,playlist.playlist_key"
        )],
        uri: "NULL".into(),
        entry_key: "NULL".into(),
        arguments: Default::default(),
    }
}
