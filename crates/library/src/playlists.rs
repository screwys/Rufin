//! Owns Playlist identity, exact duplicate occurrences, ordering, filtering, and edits.
//! Entries keep song references; song metadata belongs to the shared library.

use sqlx::{Connection, FromRow, QueryBuilder, Sqlite, SqliteConnection};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};

use crate::{
    Database, FolderKey, LibraryError, LibraryResult, PlaylistEntryKey, PlaylistKey,
    ReadCancellation, RouteSeedWindow, SongSelection, SourceKey, TrackKey, TrackSort,
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
const PLAYLIST_ENTRY_ROW_LIMIT: usize = 256;
const PLAYLIST_DELETE_BATCH: usize = 400;

pub(crate) async fn install_playlist_facts_on(
    connection: &mut SqliteConnection,
) -> LibraryResult<()> {
    sqlx::raw_sql(r#"
CREATE TRIGGER IF NOT EXISTS playlist_entry_facts_insert AFTER INSERT ON playlist_entries BEGIN
    UPDATE playlists SET track_count=track_count+1,
        duration_millis=duration_millis+COALESCE((SELECT duration_millis FROM tracks WHERE track_key=NEW.track_key),0),
        downloaded_count=downloaded_count+EXISTS(SELECT 1 FROM local_locators WHERE origin='download' AND media_uri=(SELECT media_uri FROM tracks WHERE track_key=NEW.track_key))
    WHERE playlist_key=NEW.playlist_key;
END;
CREATE TRIGGER IF NOT EXISTS playlist_entry_facts_delete AFTER DELETE ON playlist_entries BEGIN
    UPDATE playlists SET track_count=track_count-1,
        duration_millis=duration_millis-COALESCE((SELECT duration_millis FROM tracks WHERE track_key=OLD.track_key),0),
        downloaded_count=downloaded_count-EXISTS(SELECT 1 FROM local_locators WHERE origin='download' AND media_uri=(SELECT media_uri FROM tracks WHERE track_key=OLD.track_key))
    WHERE playlist_key=OLD.playlist_key;
END;
CREATE TRIGGER IF NOT EXISTS playlist_entry_facts_update AFTER UPDATE OF playlist_key,track_key ON playlist_entries
WHEN (OLD.playlist_key,OLD.track_key) IS NOT (NEW.playlist_key,NEW.track_key) BEGIN
    UPDATE playlists SET track_count=track_count-1,
        duration_millis=duration_millis-COALESCE((SELECT duration_millis FROM tracks WHERE track_key=OLD.track_key),0),
        downloaded_count=downloaded_count-EXISTS(SELECT 1 FROM local_locators WHERE origin='download' AND media_uri=(SELECT media_uri FROM tracks WHERE track_key=OLD.track_key))
    WHERE playlist_key=OLD.playlist_key;
    UPDATE playlists SET track_count=track_count+1,
        duration_millis=duration_millis+COALESCE((SELECT duration_millis FROM tracks WHERE track_key=NEW.track_key),0),
        downloaded_count=downloaded_count+EXISTS(SELECT 1 FROM local_locators WHERE origin='download' AND media_uri=(SELECT media_uri FROM tracks WHERE track_key=NEW.track_key))
    WHERE playlist_key=NEW.playlist_key;
END;
CREATE TRIGGER IF NOT EXISTS playlist_song_facts_update AFTER UPDATE OF duration_millis,media_uri ON tracks
WHEN (OLD.duration_millis,OLD.media_uri) IS NOT (NEW.duration_millis,NEW.media_uri) BEGIN
    UPDATE playlists SET duration_millis=duration_millis+(COALESCE(NEW.duration_millis,0)-COALESCE(OLD.duration_millis,0))*(SELECT count(*) FROM playlist_entries WHERE playlist_key=playlists.playlist_key AND track_key=NEW.track_key),
        downloaded_count=downloaded_count+((EXISTS(SELECT 1 FROM local_locators WHERE origin='download' AND media_uri=NEW.media_uri))-(EXISTS(SELECT 1 FROM local_locators WHERE origin='download' AND media_uri=OLD.media_uri)))*(SELECT count(*) FROM playlist_entries WHERE playlist_key=playlists.playlist_key AND track_key=NEW.track_key)
    WHERE playlist_key IN (SELECT playlist_key FROM playlist_entries WHERE track_key=NEW.track_key);
END;
CREATE TRIGGER IF NOT EXISTS playlist_download_facts_insert AFTER INSERT ON local_locators
WHEN NEW.origin='download' AND (SELECT count(*) FROM local_locators WHERE origin='download' AND media_uri=NEW.media_uri)=1 BEGIN
    UPDATE playlists SET downloaded_count=downloaded_count+(SELECT count(*) FROM playlist_entries WHERE playlist_key=playlists.playlist_key AND track_key=(SELECT track_key FROM tracks WHERE media_uri=NEW.media_uri))
    WHERE playlist_key IN (SELECT playlist_key FROM playlist_entries WHERE track_key=(SELECT track_key FROM tracks WHERE media_uri=NEW.media_uri));
END;
CREATE TRIGGER IF NOT EXISTS playlist_download_facts_delete AFTER DELETE ON local_locators
WHEN OLD.origin='download' AND NOT EXISTS(SELECT 1 FROM local_locators WHERE origin='download' AND media_uri=OLD.media_uri) BEGIN
    UPDATE playlists SET downloaded_count=downloaded_count-(SELECT count(*) FROM playlist_entries WHERE playlist_key=playlists.playlist_key AND track_key=(SELECT track_key FROM tracks WHERE media_uri=OLD.media_uri))
    WHERE playlist_key IN (SELECT playlist_key FROM playlist_entries WHERE track_key=(SELECT track_key FROM tracks WHERE media_uri=OLD.media_uri));
END;
CREATE TRIGGER IF NOT EXISTS playlist_download_facts_update AFTER UPDATE OF media_uri,origin ON local_locators
WHEN (OLD.media_uri,OLD.origin) IS NOT (NEW.media_uri,NEW.origin) BEGIN
    UPDATE playlists SET downloaded_count=downloaded_count-(SELECT count(*) FROM playlist_entries WHERE playlist_key=playlists.playlist_key AND track_key=(SELECT track_key FROM tracks WHERE media_uri=OLD.media_uri))
    WHERE OLD.origin='download' AND NOT EXISTS(SELECT 1 FROM local_locators WHERE origin='download' AND media_uri=OLD.media_uri)
        AND playlist_key IN (SELECT playlist_key FROM playlist_entries WHERE track_key=(SELECT track_key FROM tracks WHERE media_uri=OLD.media_uri));
    UPDATE playlists SET downloaded_count=downloaded_count+(SELECT count(*) FROM playlist_entries WHERE playlist_key=playlists.playlist_key AND track_key=(SELECT track_key FROM tracks WHERE media_uri=NEW.media_uri))
    WHERE NEW.origin='download' AND (SELECT count(*) FROM local_locators WHERE origin='download' AND media_uri=NEW.media_uri)=1
        AND playlist_key IN (SELECT playlist_key FROM playlist_entries WHERE track_key=(SELECT track_key FROM tracks WHERE media_uri=NEW.media_uri));
END;
"#).execute(connection).await?;
    Ok(())
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistSort {
    Position,
    Title,
    TrackCount,
    Duration,
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
    pub representative_artwork: Vec<Vec<u8>>,
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

#[derive(Clone, Debug, FromRow, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlaylistEntryRow {
    pub track_key: crate::TrackKey,
    pub audio_properties: crate::AudioProperties,
    pub source_path: Option<String>,
    pub source_name: Option<String>,
    pub playlist_entry_key: PlaylistEntryKey,
    pub position: i64,
    pub media_uri: String,
    pub source_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_media_uri: Option<String>,
    #[sqlx(json)]
    pub artists: Vec<crate::TrackArtistLink>,
    #[sqlx(json)]
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
    pub count: usize,
    pub first_row_position: usize,
    pub first_rows: Vec<PlaylistEntryRow>,
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
        Self::load_playlist_order(
            &mut connection,
            Some(source),
            folder,
            sort,
            descending,
            filter,
        )
        .await
    }

    async fn load_playlist_order(
        connection: &mut SqliteConnection,
        source: Option<SourceKey>,
        folder: Option<FolderKey>,
        sort: PlaylistSort,
        descending: bool,
        filter: &str,
    ) -> LibraryResult<Vec<PlaylistKey>> {
        Ok(
            playlist_order_query(source, folder, sort, descending, filter, false)
                .build_query_scalar()
                .persistent(false)
                .fetch_all(connection)
                .await?,
        )
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
        rows.retain(|row| row.writable);
        for row in &mut rows {
            row.genres = load_playlist_genres(&mut transaction, row.playlist_key).await?;
        }
        transaction.commit().await?;
        Ok(rows)
    }

    pub async fn playlist_detail_page(
        &self,
        input: &crate::SongQuery,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistDetailPage>> {
        let crate::SongScope::Playlist(playlist) = input.scope else {
            return Err(LibraryError::InvalidRequest(
                "Playlist detail needs a playlist query".into(),
            ));
        };
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let summary = Self::load_playlist_rows(&mut transaction, &[playlist])
            .await?
            .pop();
        let page = if let Some(mut summary) = summary {
            summary.genres = load_playlist_genres(&mut transaction, playlist).await?;
            let query = crate::selection::song_query_on(&mut transaction, input).await?;
            let count = query.count(&mut transaction).await?.max(0) as usize;
            let range = window.range(count);
            let first_row_position = range.start;
            let keys = crate::source_window::selected_values_on(
                &mut transaction,
                &query,
                &query.entry_key,
                &[range],
            )
            .await?;
            let first_rows = load_playlist_entry_rows(&mut transaction, &keys).await?;
            Some(PlaylistDetailPage {
                summary,
                count,
                first_row_position,
                first_rows,
            })
        } else {
            None
        };
        transaction.commit().await?;
        Ok(page)
    }

    pub async fn playlist_genres(
        &self,
        key: PlaylistKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistGenreLink>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        load_playlist_genres(&mut connection, key).await
    }

    pub async fn playlist_rows(
        &self,
        keys: &[PlaylistKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Self::load_playlist_rows(&mut connection, keys).await
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
            if result.iter().any(|row| row.playlist_key == *key) {
                continue;
            }
            let Some(mut row) = sqlx::query_as::<_, PlaylistRow>(
                "SELECT playlist.playlist_key,playlist.source_key,source.object_id source_id,
                        playlist.owner='source' source_owned,playlist.object_id,
                        playlist.name,playlist.writable,playlist.metadata_writable,playlist.artwork_binding,
                        playlist.track_count,playlist.duration_millis,playlist.downloaded_count
                 FROM playlists playlist
                 LEFT JOIN sources source USING(source_key)
                 WHERE playlist.playlist_key=?1 AND playlist.name IS NOT NULL
"
            )
            .bind(key).fetch_optional(&mut *connection).await? else {
                continue;
            };
            if row.artwork_binding.is_none() {
                row.representative_artwork =
                    crate::artwork::playlist_representative_artwork_on(connection, *key).await?;
            }
            result.push(row);
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

    pub async fn playlist_edit_entries(
        &self,
        playlist: PlaylistKey,
        selection: &SongSelection,
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
                SongSelection::Concat(parts) => pending.extend(parts.iter().rev()),
                SongSelection::PlaylistEntries(keys) => {
                    entries.extend(sqlx::query_as::<_, PlaylistEditEntry>(sqlx::AssertSqlSafe(format!(
                        "SELECT {COLUMNS} FROM json_each(?1) requested JOIN playlist_entries entry ON entry.playlist_entry_key=requested.value WHERE entry.playlist_key=?2 ORDER BY entry.position"
                    )))
                    .bind(serde_json::to_string(keys)?)
                    .bind(playlist)
                    .persistent(false)
                    .fetch_all(&mut *transaction)
                    .await?);
                }
                SongSelection::Query { query, ranges }
                    if matches!(query.scope, crate::SongScope::Playlist(_)) =>
                {
                    let mut query =
                        crate::selection::song_query_on(&mut transaction, query).await?;
                    query
                        .predicate
                        .push_str(&format!(" AND entry.playlist_key={}", playlist.raw()));
                    entries.extend(
                        crate::source_window::selected_rows_on::<PlaylistEditEntry>(
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
        .bind(PLAYLIST_ENTRY_ROW_LIMIT as i64)
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

    pub async fn source_playlist_song_ids(
        &self,
        source: SourceKey,
        playlist: Option<PlaylistKey>,
        songs: &SongSelection,
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
        songs: &SongSelection,
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
        let changed = sqlx::query(
            "UPDATE main.playlists SET name=COALESCE(?3,name), normalized_name=COALESCE(lower(?3),normalized_name), sort_text=COALESCE(lower(?3),sort_text),
             artwork_bytes=CASE WHEN ?4 THEN ?5 ELSE artwork_bytes END,
             artwork_mime=CASE WHEN ?4 THEN ?6 ELSE artwork_mime END,
             artwork_revision=CASE WHEN ?4 THEN ?7 ELSE artwork_revision END,
             artwork_binding=CASE WHEN ?4 THEN CASE WHEN ?7 IS NOT NULL THEN CAST(json_object('kind','playlist','source_id',(SELECT object_id FROM sources WHERE source_key=playlists.source_key),'object_id',object_id,'revision',?7) AS BLOB) END ELSE artwork_binding END
             WHERE source_key IS ?1 AND owner='rufin'
               AND playlist_key=?2 AND metadata_writable AND ((?3 IS NOT NULL AND name IS NOT ?3) OR (?4 AND artwork_revision IS NOT ?7))",
        ).bind(source).bind(playlist).bind(name).bind(artwork.is_some())
            .bind(image.map(|(bytes, _)| bytes)).bind(image.and_then(|(_, mime)| mime)).bind(revision)
            .execute(&mut *transaction).await?.rows_affected() == 1;
        if changed && name.is_some() {
            crate::playlist_links::mark_dirty_on(&mut transaction, playlist).await?;
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
        let max_position =
            sqlx::query_scalar::<_, Option<i64>>("SELECT max(position) FROM playlists")
                .fetch_one(&mut *transaction)
                .await?
                .unwrap_or_default();
        let offset = max_position + 1;
        sqlx::query(
            "UPDATE main.playlists SET position=position+?2 WHERE name IS NOT NULL AND (owner='rufin' OR indexed) AND (?1 IS NULL OR source_key=?1 OR source_key IS NULL)",
        )
        .bind(source)
        .bind(offset)
        .execute(&mut *transaction)
        .await?;
        for (key, (_, position)) in order.into_iter().zip(visible) {
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
             WHERE source_key IS ?1 AND owner='rufin'
               AND playlist_key=?2",
        )
        .bind(source)
        .bind(playlist)
        .execute(connection)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn add_playlist_songs(
        &self,
        source: Option<SourceKey>,
        playlist: PlaylistKey,
        songs: &SongSelection,
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
    selection: &SongSelection,
) -> LibraryResult<Option<Vec<TrackKey>>> {
    let mut pending = vec![selection];
    let mut tracks = Vec::new();
    while let Some(selection) = pending.pop() {
        match selection {
            SongSelection::Concat(parts) => pending.extend(parts.iter().rev()),
            SongSelection::Uris(uris) => {
                let known: Vec<Option<TrackKey>> = sqlx::query_scalar(
                    "SELECT track.track_key FROM json_each(?1) requested LEFT JOIN tracks track ON track.media_uri=requested.value ORDER BY CAST(requested.key AS INTEGER)",
                )
                .bind(serde_json::to_string(uris)?)
                .fetch_all(&mut *connection)
                .await?;
                if source.is_some() && known.iter().any(Option::is_none) {
                    return Ok(None);
                }
                for (uri, key) in uris.iter().zip(known) {
                    tracks.push(match key {
                        Some(key) => key,
                        None => {
                            crate::tracks::remember_song(connection, uri, Default::default())
                                .await?
                        }
                    });
                }
            }
            _ => tracks.extend(
                crate::selection::selected_songs_on(connection, selection)
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
        sqlx::raw_sql("INSERT INTO sources(source_key,object_id,display_name,normalized_name,artwork_digest) VALUES (1,'source','Source','source',zeroblob(32));
          INSERT INTO playlists(playlist_key,source_key,owner,object_id,name,normalized_name,sort_text,position)
          VALUES(1,1,'source','small','Small','small','small',0),(2,1,'source','large','Large','large','large',1);
          WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<15)
          INSERT INTO tracks(track_key,source_key,object_id,title,normalized_search,display_album,display_artist,sort_text,duration_millis,media_uri,artwork_binding)
          SELECT x,1,CAST(x AS TEXT),'Title','title','','','title',1000,'track:'||x,X'01' FROM n;
          INSERT INTO playlist_entries(playlist_key,object_id,track_key,position)
          SELECT 1,object_id,track_key,track_key FROM tracks;")
            .execute(&mut *connection).await.unwrap();
        for grow in [false, true] {
            if grow {
                sqlx::raw_sql("WITH RECURSIVE n(x) AS (VALUES(16) UNION ALL SELECT x+1 FROM n WHERE x<10000)
                  INSERT INTO tracks(track_key,source_key,object_id,title,normalized_search,display_album,display_artist,sort_text,duration_millis,media_uri,artwork_binding)
                  SELECT x,1,CAST(x AS TEXT),'Title','title','','','title',1000,'track:'||x,X'01' FROM n;
                  INSERT INTO playlist_entries(playlist_key,object_id,track_key,position)
                  SELECT 2,object_id,track_key,track_key FROM tracks;")
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
            for sort in [TrackSort::Artist, TrackSort::AlbumArtist] {
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
            assert!(rows[0].artwork_binding.is_none());
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

pub(crate) fn playlist_entries_query(keys: &[PlaylistEntryKey]) -> QueryBuilder<Sqlite> {
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(playlist_entry_key,ordinal) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (ordinal, key)| {
        row.push_bind(*key).push_bind(ordinal as i64);
    });
    query.push("),entries AS (
        SELECT requested.ordinal,entry.* FROM requested JOIN playlist_entries entry USING(playlist_entry_key)
    ) SELECT ");
    query
}

pub(crate) async fn load_playlist_entry_rows(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    keys: &[PlaylistEntryKey],
) -> LibraryResult<Vec<PlaylistEntryRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = playlist_entries_query(keys);
    query.push(crate::tracks::TRACK_LINK_COLUMNS);
    query.push(format!(concat!("entry.track_key,entry.playlist_entry_key,track.media_uri,entry.position,source.object_id source_id,
                      COALESCE(track.title,'') title,
                      COALESCE(track.display_artist,'') artist,
                      COALESCE(track.display_album,'') album,
                      COALESCE(album.display_artist,track.album_display_artist) album_display_artist,
                      track.artwork_binding,COALESCE(track.duration_millis,0) duration_millis,
                      track.disc_number disc_number,
                      track.track_number track_number,
                      track.year year,
                      track.release_date release_date,
                      COALESCE(track.audio_properties,'{{}}') audio_properties,date(", crate::tracks::added_at!("track"), ",'unixepoch') date_added,track.bpm,track.source_path,source.display_name source_name,
                      (SELECT COALESCE(group_concat(name, ', '),'') FROM (
                         SELECT genre.name FROM track_genres credit JOIN genres genre USING(genre_key)
                         WHERE credit.track_key=track.track_key ORDER BY credit.position
                      )) genre,
                      {plays} play_count,{last} last_played,
                      track.source_format source_format,
                      track.musicbrainz_recording_id musicbrainz_recording_id,
                      track.musicbrainz_release_track_id musicbrainz_release_track_id,
                      COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=track.media_uri),track.source_favorite,0) favorite,
                      COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=track.media_uri),track.source_rating)/10 rating,
                      EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=track.media_uri AND access.origin='download') is_downloaded
               FROM entries entry
               JOIN tracks track USING(track_key)
               LEFT JOIN sources source ON source.source_key=track.source_key
               LEFT JOIN albums album ON album.album_key=track.album_key
               ORDER BY entry.ordinal"), plays=crate::tracks::displayed_stat("play_count"), last=crate::tracks::displayed_stat("last_played")));
    query
        .build_query_as::<PlaylistEntryRow>()
        .persistent(false)
        .fetch_all(&mut **transaction)
        .await
        .map_err(Into::into)
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
    Ok(sqlx::query_as(
        "INSERT INTO main.playlists(source_key,object_id,name,normalized_name,sort_text,position)
         SELECT ?1,id,?2,lower(?2),lower(?2),
                (SELECT coalesce(max(position)+1,0) FROM playlists)
         FROM (SELECT 'rufin:playlist:'||lower(hex(randomblob(16))) id)
         RETURNING playlist_key,object_id",
    )
    .bind(source)
    .bind(name)
    .fetch_one(&mut **transaction)
    .await?)
}

async fn insert_playlist_tracks(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    playlist: PlaylistKey,
    start: i64,
    tracks: &[TrackKey],
    skip_existing: bool,
) -> LibraryResult<usize> {
    Ok(sqlx::query(
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
    .rows_affected() as usize)
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
    pub media_uri: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_display_artist: Option<String>,
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
    File(crate::playlist_links::PlaylistFileRecord),
    Artwork(PlaylistArtworkRecord),
}

#[derive(serde::Serialize, serde::Deserialize, sqlx::FromRow)]
struct PlaylistArtworkRecord {
    source_id: Option<String>,
    object_id: String,
    bytes: Vec<u8>,
    content_type: Option<String>,
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
    if entry.object_id.is_empty() || entry.media_uri.is_empty() || entry.position < 0 {
        return Err(LibraryError::InvalidRequest(
            "invalid playlist occurrence identity or order".into(),
        ));
    }
    let track = remember_playlist_song(connection, entry).await?;
    sqlx::query(
        "INSERT INTO playlist_entries(playlist_key,object_id,track_key,position)
        VALUES(?1,?2,?3,?4) ON CONFLICT(playlist_key,object_id)
        DO UPDATE SET track_key=excluded.track_key,position=excluded.position",
    )
    .bind(playlist)
    .bind(&entry.object_id)
    .bind(track)
    .bind(entry.position)
    .execute(connection)
    .await?;
    Ok(())
}

pub(crate) async fn remember_playlist_song(
    connection: &mut SqliteConnection,
    entry: &PlaylistEntryWrite,
) -> LibraryResult<crate::TrackKey> {
    crate::tracks::remember_song(
        connection,
        &entry.media_uri,
        crate::tracks::SongMetadata {
            title: entry.title.as_deref(),
            artist: entry.artist.as_deref(),
            album: entry.album.as_deref(),
            album_display_artist: entry.album_display_artist.as_deref(),
            duration_millis: entry.duration_millis,
            disc_number: entry.disc_number,
            track_number: entry.track_number,
            year: entry.year,
            release_date: entry.release_date.as_deref(),
            source_format: entry.source_format.as_deref(),
            musicbrainz_recording_id: entry.musicbrainz_recording_id.as_deref(),
            musicbrainz_release_track_id: entry.musicbrainz_release_track_id.as_deref(),
            ..Default::default()
        },
    )
    .await
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
    let mut images = sqlx::query_as::<_, PlaylistArtworkRecord>("SELECT source.object_id source_id,playlist.object_id,playlist.artwork_bytes bytes,playlist.artwork_mime content_type FROM main.playlists playlist LEFT JOIN sources source USING(source_key) WHERE playlist.artwork_bytes IS NOT NULL").fetch(&mut *connection);
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
                let revision = blake3::hash(&image.bytes).to_hex().to_string();
                sqlx::query("UPDATE main.playlists SET artwork_bytes=?3,artwork_mime=?4,artwork_revision=?5,artwork_binding=CAST(json_object('kind','playlist','source_id',?2,'object_id',?1,'revision',?5) AS BLOB) WHERE owner='rufin' AND object_id=?1 AND source_key IS (SELECT source_key FROM sources WHERE object_id=?2)")
                    .bind(image.object_id).bind(image.source_id).bind(image.bytes).bind(image.content_type).bind(revision).execute(&mut *connection).await?;
            }
            PlaylistRecord::File(record) => {
                crate::playlist_links::restore_record_on(connection, record).await?
            }
        }
    }
    Ok(count)
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
    sort: TrackSort,
    descending: bool,
    filter: &str,
) -> crate::source_window::SourceQuery {
    let mut query = crate::source_window::SourceQuery {
        from: "playlist_entries entry JOIN playlists playlist USING(playlist_key) JOIN tracks track USING(track_key)".into(),
        predicate: format!("entry.playlist_key={}", key.raw()),
        uri: "track.media_uri".into(),
        entry_key: "entry.playlist_entry_key".into(),
        arguments: Default::default(),
        order: sort.order_terms(descending, true),
    };
    if sort != TrackSort::Position {
        query.order.push("entry.position ASC".into());
    }
    if let Some(folder) = folder {
        query.predicate.push_str(&format!(" AND (playlist.source_key IS NULL OR EXISTS(SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.track_key=entry.track_key AND scope.folder_key={}))",folder.raw()));
    }
    crate::tracks::track_filter(&mut query, None, filter, false);
    query
}

pub(crate) fn playlist_order_query(
    source: Option<SourceKey>,
    folder: Option<FolderKey>,
    sort: PlaylistSort,
    descending: bool,
    filter: &str,
    count: bool,
) -> QueryBuilder<Sqlite> {
    let filter: String = filter.trim().to_lowercase().chars().take(256).collect();

    let mut query = QueryBuilder::<Sqlite>::new(if count {
        "SELECT count(*) FROM playlists playlist"
    } else {
        "SELECT playlist.playlist_key FROM playlists playlist"
    });
    query.push(" WHERE playlist.name IS NOT NULL AND (playlist.owner='rufin' OR playlist.indexed) AND (playlist.source_key=").push_bind(source)
            .push(" OR playlist.source_key IS NULL) AND (playlist.source_key IS NULL OR ")
            .push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM playlist_entries scoped_entry JOIN tracks scoped_track USING(track_key) JOIN track_folders scope USING(track_key) WHERE scoped_entry.playlist_key=playlist.playlist_key AND scope.folder_key=").push_bind(folder).push("))");
    if !filter.is_empty() {
        query
            .push(" AND instr(playlist.normalized_name,")
            .push_bind(filter)
            .push(")>0");
    }
    if count {
        return query;
    }
    query.push(" ORDER BY ").push(match sort {
        PlaylistSort::Position => "playlist.position",
        PlaylistSort::Title => "playlist.sort_text",
        PlaylistSort::TrackCount => "playlist.track_count",
        PlaylistSort::Duration => "playlist.duration_millis",
    });
    if descending {
        query.push(" DESC");
    }
    if sort != PlaylistSort::Title {
        query.push(",playlist.sort_text");
    }
    query.push(",playlist.playlist_key");
    query
}
