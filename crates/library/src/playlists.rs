//! Owns Playlist identity, exact duplicate occurrences, ordering, filtering, and edits.
//! Entries keep song references; song metadata belongs to the shared library.

use sqlx::{Connection, FromRow, QueryBuilder, Sqlite, SqliteConnection};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};

use crate::catalog::snapshot::Packed;
use crate::catalog::write::Scope;
use crate::{
    Catalog, CatalogSort, FolderKey, LibraryError, LibraryResult, MemberSelection,
    PlaylistEntryKey, PlaylistKey, ReadCancellation, SourceKey, TrackKey,
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

    pub async fn apply(mut self, catalog: &Catalog, playlist: PlaylistKey) -> LibraryResult<usize> {
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
        catalog.remove_playlist_entry_iter(playlist, entries).await
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
    #[sqlx(json)]
    pub genres: Vec<PlaylistGenreLink>,
    /// An entry's track is hidden: its file or server item is gone.
    pub holds_hidden: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
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

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlaylistEntryView {
    pub playlist_entry_key: PlaylistEntryKey,
    pub position: i64,
    pub source_entry_id: Option<String>,
    #[sqlx(flatten)]
    pub track: crate::TrackView,
}

impl From<PlaylistEntryView> for crate::TrackReference {
    fn from(entry: PlaylistEntryView) -> Self {
        entry.track.into()
    }
}

impl Catalog {
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

    /// URIs of a source's tracks that some playlist holds, by title.
    pub async fn all_playlist_track_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        self.loaded().await;
        let keys = {
            let snapshot = self.snapshot();
            let Some(owner) = snapshot.source_slot(source) else {
                return Ok(Vec::new());
            };
            let folder = match folder {
                Some(folder) => match snapshot.folders.slot(folder.raw()) {
                    Some(slot) => Some(slot),
                    None => return Ok(Vec::new()),
                },
                None => None,
            };
            let held = snapshot
                .playlists
                .iter()
                .flat_map(|(_, playlist)| playlist.entries.iter().map(|(_, slot)| *slot))
                .collect::<std::collections::BTreeSet<_>>();
            let mut tracks = held
                .into_iter()
                .filter_map(|slot| snapshot.tracks.live(slot))
                .filter(|record| {
                    record.source == owner
                        && folder
                            .is_none_or(|folder| snapshot.track_runs(record)[3].contains(&folder))
                })
                .map(|record| (snapshot.track_sort(record), record.key))
                .collect::<Vec<_>>();
            tracks.sort();
            tracks.into_iter().map(|(_, key)| key).collect::<Vec<_>>()
        };
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar("SELECT track.media_uri FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value ORDER BY CAST(requested.key AS INTEGER)")
            .bind(serde_json::to_string(&keys)?)
            .fetch_all(&mut *connection)
            .await?)
    }

    /// Playlists the user can add tracks to, as the playlist list orders them.
    pub async fn playlist_destinations(
        &self,
        source: Option<SourceKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistRow>> {
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let order = self
            .list_order(
                &mut connection,
                &crate::ListRead::new(crate::ListKind::Playlists, source, CatalogSort::Position),
            )
            .await?;
        let keys = order
            .iter()
            .copied()
            .map(PlaylistKey::from_raw)
            .collect::<Vec<_>>();
        let mut rows = Vec::with_capacity(keys.len());
        for keys in keys.chunks(PLAYLIST_ROW_LIMIT) {
            rows.extend(load_playlist_rows(self, &mut connection, keys).await?);
        }
        rows.retain(|row| row.writable);
        Ok(rows)
    }

    pub async fn playlist_edit_entries(
        &self,
        playlist: PlaylistKey,
        selection: &MemberSelection,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistEditEntry>> {
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let selected =
            crate::selection::selected_tracks_on(self, &mut connection, selection).await?;
        let keys = selected
            .into_iter()
            .map(|(_, entry)| {
                entry.ok_or_else(|| {
                    LibraryError::InvalidRequest(
                        "Selection does not contain playlist entries".into(),
                    )
                })
            })
            .collect::<LibraryResult<Vec<_>>>()?;
        let mut entries = sqlx::query_as::<_, PlaylistEditEntry>(
            "SELECT entry.playlist_entry_key,entry.track_key,entry.object_id,entry.position
             FROM json_each(?1) requested JOIN playlist_entries entry ON entry.playlist_entry_key=requested.value
             WHERE entry.playlist_key=?2 ORDER BY entry.position"
        ).bind(serde_json::to_string(&keys)?).bind(playlist).fetch_all(&mut *connection).await?;
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
            "SELECT object_id FROM playlists WHERE source_key=?1 AND name IS NOT NULL AND (owner='rufin' OR indexed) ORDER BY lower(name),playlist_key",
        )
        .bind(source)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn source_playlist_track_ids(
        &self,
        source: SourceKey,
        playlist: Option<PlaylistKey>,
        songs: &MemberSelection,
        skip_existing: bool,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        let mut transaction = connection.begin().await?;
        let tracks = playlist_tracks_on(self, &mut transaction, Some(source), songs)
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

    /// A new Rufin playlist. Rufin playlists hold tracks of any source.
    pub async fn create_playlist(
        &self,
        name: &str,
        songs: &MemberSelection,
    ) -> LibraryResult<Option<(PlaylistKey, String)>> {
        let name = name.trim();
        if name.is_empty() {
            return Err(LibraryError::InvalidRequest(
                "Playlist name cannot be empty".to_string(),
            ));
        }
        let mut write = self.write().await?;
        remember_uris(&mut write, songs).await?;
        let connection = write.connection().await?;
        let Some(tracks) = playlist_tracks_on(self, connection, None, songs).await? else {
            return Ok(None);
        };
        let (playlist, object_id) = create_playlist_identity(connection, name).await?;
        insert_playlist_tracks(connection, playlist, 0, &tracks, false).await?;
        write.touch_playlist(playlist.raw());
        write.commit().await?;
        Ok(Some((playlist, object_id)))
    }

    pub async fn update_playlist(
        &self,
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
        let mut write = self.write().await?;
        let transaction = write.connection().await?;
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
            "UPDATE main.playlists SET name=COALESCE(?2,name),
             artwork_bytes=CASE WHEN ?3 THEN ?4 ELSE artwork_bytes END,
             artwork_mime=CASE WHEN ?3 THEN ?5 ELSE artwork_mime END,
             artwork_revision=CASE WHEN ?3 THEN ?6 ELSE artwork_revision END,
             artwork_binding=CASE WHEN ?3 THEN ?7 ELSE artwork_binding END
             WHERE owner='rufin' AND playlist_key=?1 AND metadata_writable
               AND ((?2 IS NOT NULL AND name IS NOT ?2) OR (?3 AND artwork_revision IS NOT ?6))",
        )
        .bind(playlist)
        .bind(name)
        .bind(artwork.is_some())
        .bind(image.map(|(bytes, _)| bytes))
        .bind(image.and_then(|(_, mime)| mime))
        .bind(revision)
        .bind(binding)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            == 1;
        if changed && name.is_some() {
            crate::playlist_links::mark_dirty_on(transaction, playlist).await?;
        }
        if changed {
            write.touch_playlist(playlist.raw());
            // The image itself is read by key.
            write.changed(Scope::Playlist {
                key: playlist,
                entries: false,
            });
        }
        write.commit().await?;
        Ok(changed)
    }

    pub async fn delete_playlist(&self, playlist: PlaylistKey) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let deleted = sqlx::query("DELETE FROM playlists WHERE owner='rufin' AND playlist_key=?1")
            .bind(playlist)
            .execute(write.connection().await?)
            .await?
            .rows_affected()
            > 0;
        write.touch_playlist(playlist.raw());
        write.commit().await?;
        Ok(deleted)
    }

    pub async fn add_playlist_tracks(
        &self,
        playlist: PlaylistKey,
        songs: &MemberSelection,
        skip_existing: bool,
    ) -> LibraryResult<usize> {
        let mut write = self.write().await?;
        if !editable_playlist_exists(write.connection().await?, playlist).await? {
            return Ok(0);
        }
        remember_uris(&mut write, songs).await?;
        let connection = write.connection().await?;
        let Some(tracks) = playlist_tracks_on(self, connection, None, songs).await? else {
            return Ok(0);
        };
        let next_position = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(max(position) + 1, 0)
             FROM playlist_entries WHERE playlist_key=?1",
        )
        .bind(playlist)
        .fetch_one(&mut *connection)
        .await?;
        let accepted =
            insert_playlist_tracks(connection, playlist, next_position, &tracks, skip_existing)
                .await?;
        if accepted > 0 {
            crate::playlist_links::mark_dirty_on(connection, playlist).await?;
        }
        write.touch_playlist(playlist.raw());
        write.commit().await?;
        Ok(accepted)
    }

    pub async fn remove_playlist_entries(
        &self,
        playlist: PlaylistKey,
        entries: &[PlaylistEntryKey],
    ) -> LibraryResult<usize> {
        if entries.is_empty() {
            return Ok(0);
        }
        self.remove_playlist_entry_iter(playlist, entries.iter().copied().map(Ok))
            .await
    }

    async fn remove_playlist_entry_iter(
        &self,
        playlist: PlaylistKey,
        mut entries: impl Iterator<Item = LibraryResult<PlaylistEntryKey>> + Send,
    ) -> LibraryResult<usize> {
        let mut write = self.write().await?;
        let transaction = write.connection().await?;
        if !editable_playlist_exists(transaction, playlist).await? {
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
            crate::playlist_links::mark_dirty_on(transaction, playlist).await?;
        }
        write.touch_playlist(playlist.raw());
        write.commit().await?;
        Ok(removed)
    }

    pub async fn move_playlist_entry(
        &self,
        playlist: PlaylistKey,
        entry: PlaylistEntryKey,
        new_position: usize,
    ) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let transaction = write.connection().await?;
        if !editable_playlist_exists(transaction, playlist).await? {
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
        crate::playlist_links::mark_dirty_on(transaction, playlist).await?;
        write.touch_playlist(playlist.raw());
        write.commit().await?;
        Ok(true)
    }
}

async fn editable_playlist_exists(
    connection: &mut SqliteConnection,
    playlist: PlaylistKey,
) -> LibraryResult<bool> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM playlists WHERE playlist_key=?1 AND owner='rufin' AND writable",
    )
    .bind(playlist)
    .fetch_optional(&mut *connection)
    .await?
    .is_some())
}

/// Selected URIs no track has yet become reference tracks.
async fn remember_uris(
    write: &mut crate::catalog::write::Write<'_>,
    selection: &MemberSelection,
) -> LibraryResult<()> {
    let mut pending = vec![selection];
    while let Some(selection) = pending.pop() {
        match selection {
            MemberSelection::Concat(parts) => pending.extend(parts.iter().rev()),
            MemberSelection::Uris(uris) => {
                for uri in uris.iter() {
                    write
                        .put_reference(&crate::TrackReference {
                            media_uri: uri.clone(),
                            ..Default::default()
                        })
                        .await?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

async fn playlist_tracks_on(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    source: Option<SourceKey>,
    selection: &MemberSelection,
) -> LibraryResult<Option<Vec<TrackKey>>> {
    let mut pending = vec![selection];
    let mut tracks = Vec::new();
    while let Some(selection) = pending.pop() {
        match selection {
            MemberSelection::Concat(parts) => pending.extend(parts.iter().rev()),
            MemberSelection::Uris(uris) => {
                let missing:Vec<String>=sqlx::query_scalar("SELECT DISTINCT requested.value FROM json_each(?1) requested WHERE NOT EXISTS(SELECT 1 FROM tracks WHERE media_uri=requested.value)").bind(serde_json::to_string(uris)?).fetch_all(&mut *connection).await?;
                if source.is_some() && !missing.is_empty() {
                    return Ok(None);
                }
                tracks.extend(
                    crate::selection::selected_tracks_on(catalog, connection, selection)
                        .await?
                        .into_iter()
                        .map(|(key, _)| key),
                );
            }
            _ => tracks.extend(
                crate::selection::selected_tracks_on(catalog, connection, selection)
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

/// The resident part of a playlist row: totals of its entries and its two main genres.
struct Resident {
    totals: crate::catalog::views::Totals,
    genres: Vec<i64>,
    holds_hidden: bool,
}

impl Catalog {
    fn resident_playlists(&self, keys: &[PlaylistKey]) -> Vec<Option<Resident>> {
        let snapshot = self.snapshot();
        let context = crate::catalog::views::Context::new(&snapshot, false);
        keys.iter()
            .map(|key| {
                let record = snapshot.playlists.get(key.raw())?;
                let mut genres = std::collections::HashMap::<u32, u32>::new();
                let mut holds_hidden = false;
                for (_, slot) in &record.entries {
                    let Some(track) = snapshot.tracks.live(*slot) else {
                        continue;
                    };
                    holds_hidden |= !track.indexed();
                    for tag in snapshot.track_runs(track)[2] {
                        if snapshot
                            .tags
                            .live(*tag)
                            .is_some_and(|tag| tag.kind == crate::TagKind::Genre)
                        {
                            *genres.entry(*tag).or_default() += 1;
                        }
                    }
                }
                let mut genres = genres
                    .into_iter()
                    .map(|(slot, count)| {
                        let tag = &snapshot.tags.records[slot as usize];
                        (
                            std::cmp::Reverse(count),
                            snapshot.name_sort(tag.name, tag.sort),
                            tag.key,
                        )
                    })
                    .collect::<Vec<_>>();
                genres.sort();
                Some(Resident {
                    totals: context.playlist_totals(record, None),
                    genres: genres.into_iter().take(2).map(|(.., key)| key).collect(),
                    holds_hidden,
                })
            })
            .collect()
    }
}

/// Playlist rows by key, in the order asked. Unnamed playlists are skipped.
pub(crate) async fn load_playlist_rows(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    keys: &[PlaylistKey],
) -> LibraryResult<Vec<PlaylistRow>> {
    // Playlist rows show totals, which wait for the Catalog.
    catalog.loaded().await;
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let resident = catalog.resident_playlists(keys);
    #[derive(FromRow)]
    struct Row {
        ordinal: i64,
        #[sqlx(flatten)]
        playlist: PlaylistRow,
    }
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT CAST(requested.key AS INTEGER) ordinal,playlist.playlist_key,playlist.source_key,source.object_id source_id,
            playlist.owner='source' source_owned,playlist.object_id,playlist.name,playlist.writable,playlist.metadata_writable,
            {} artwork_binding,0 track_count,0 duration_millis,0 downloaded_count,'[]' genres,0 holds_hidden
         FROM json_each(?1) requested JOIN playlists playlist ON playlist.playlist_key=requested.value
         LEFT JOIN sources source ON source.source_key=playlist.source_key
         WHERE playlist.name IS NOT NULL ORDER BY CAST(requested.key AS INTEGER)",
        crate::artwork::preview_sql("playlist.artwork_binding")
    )))
    .bind(serde_json::to_string(keys)?)
    .fetch_all(&mut *connection)
    .await?;
    let genres = resident
        .iter()
        .flatten()
        .flat_map(|resident| resident.genres.iter().copied())
        .collect::<Vec<_>>();
    let names: std::collections::HashMap<i64, String> = if genres.is_empty() {
        Default::default()
    } else {
        sqlx::query_as(
            "SELECT tag_key,name FROM tags WHERE tag_key IN (SELECT value FROM json_each(?1))",
        )
        .bind(serde_json::to_string(&genres)?)
        .fetch_all(&mut *connection)
        .await?
        .into_iter()
        .collect()
    };
    rows.into_iter()
        .map(
            |Row {
                 ordinal,
                 mut playlist,
             }| {
                if let Some(resident) = resident.get(ordinal as usize).and_then(Option::as_ref) {
                    let summary = resident.totals.summary();
                    playlist.track_count = summary.track_count;
                    playlist.duration_millis = summary.duration_millis;
                    playlist.downloaded_count = summary.downloaded_count;
                    playlist.holds_hidden = resident.holds_hidden;
                    playlist.genres = resident
                        .genres
                        .iter()
                        .filter_map(|key| {
                            Some(PlaylistGenreLink {
                                genre_key: crate::GenreKey::from_raw(*key),
                                name: names.get(key)?.clone(),
                            })
                        })
                        .collect();
                }
                playlist.artwork_binding = Some(crate::artwork::playlist_artwork(
                    playlist.artwork_binding.take(),
                    (
                        false,
                        playlist.source_id.clone(),
                        playlist.object_id.clone(),
                    ),
                    (None, None),
                )?);
                Ok(playlist)
            },
        )
        .collect()
}

/// Playlist entry rows by entry key, in the order asked.
pub(crate) async fn load_playlist_entry_rows(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    entries: &[PlaylistEntryKey],
) -> LibraryResult<Vec<PlaylistEntryView>> {
    let found: Vec<(PlaylistEntryKey, i64, String, TrackKey)> = sqlx::query_as("SELECT entry.playlist_entry_key,entry.position,entry.object_id,entry.track_key FROM json_each(?1) requested JOIN playlist_entries entry ON entry.playlist_entry_key=requested.value ORDER BY CAST(requested.key AS INTEGER)")
        .bind(serde_json::to_string(entries)?)
        .fetch_all(&mut *connection)
        .await?;
    let keys = found.iter().map(|(.., track)| *track).collect::<Vec<_>>();
    let tracks = crate::tracks::load_track_rows(catalog, connection, &keys)
        .await?
        .into_iter()
        .map(|track| (track.track_key, track))
        .collect::<std::collections::HashMap<_, _>>();
    Ok(found
        .into_iter()
        .filter_map(|(playlist_entry_key, position, source_entry_id, track)| {
            Some(PlaylistEntryView {
                playlist_entry_key,
                position,
                source_entry_id: Some(source_entry_id),
                track: tracks.get(&track)?.clone(),
            })
        })
        .collect())
}

pub(crate) async fn create_playlist_identity(
    connection: &mut SqliteConnection,
    name: &str,
) -> LibraryResult<(PlaylistKey, String)> {
    let name = name.trim();
    if name.is_empty() {
        return Err(LibraryError::InvalidRequest(
            "Playlist name cannot be empty".into(),
        ));
    }
    Ok(sqlx::query_as(
        "INSERT INTO main.playlists(object_id,name)
         VALUES('rufin:playlist:'||lower(hex(randomblob(16))),?1)
         RETURNING playlist_key,object_id",
    )
    .bind(name)
    .fetch_one(&mut *connection)
    .await?)
}

async fn insert_playlist_tracks(
    connection: &mut SqliteConnection,
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
    .execute(&mut *connection)
    .await?
    .rows_affected() as usize)
}

/// Playlist identity in backups and exported files. Backups keep the position field of their
/// format; restoring does not read it, since the list sorts by creation.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct PlaylistIdentity {
    #[serde(default)]
    pub source_owned: bool,
    pub source_id: Option<String>,
    pub object_id: String,
    pub name: Option<String>,
    #[serde(default)]
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
    write: &mut crate::catalog::write::Write<'_>,
    identity: &PlaylistIdentity,
) -> LibraryResult<PlaylistKey> {
    if identity.object_id.is_empty() {
        return Err(LibraryError::InvalidRequest(
            "invalid playlist identity".into(),
        ));
    }
    let source_owned = identity.source_owned || identity.name.is_none();
    let source = match &identity.source_id {
        Some(source_id) if source_owned => Some(write.source_identity(source_id).await?),
        _ => None,
    };
    let connection = write.connection().await?;
    if let Some(source) = source {
        let key = source_playlist_identity(connection, source, &identity.object_id).await?;
        write.touch_playlist(key.raw());
        return Ok(key);
    }
    // Rufin playlists are global, whatever source a backup scoped them to.
    let existing = sqlx::query_scalar::<_, PlaylistKey>(
        "SELECT playlist_key FROM playlists WHERE object_id=?1 AND source_key IS NULL",
    )
    .bind(&identity.object_id)
    .fetch_optional(&mut *connection)
    .await?;
    let key = match existing {
        Some(key) => {
            if !source_owned {
                sqlx::query("UPDATE playlists SET name=?2 WHERE playlist_key=?1 AND owner='rufin' AND name IS NOT ?2")
                    .bind(key).bind(&identity.name).execute(&mut *connection).await?;
            }
            key
        }
        None => sqlx::query_scalar("INSERT INTO playlists(owner,object_id,name,indexed) VALUES (?1,?2,?3,?4) RETURNING playlist_key")
            .bind(if source_owned { "source" } else { "rufin" }).bind(&identity.object_id).bind(&identity.name)
            .bind(!source_owned)
            .fetch_one(connection).await?,
    };
    write.touch_playlist(key.raw());
    Ok(key)
}
/// A playlist a source owns, known by identity before the source describes it.
async fn source_playlist_identity(
    connection: &mut SqliteConnection,
    source: SourceKey,
    object_id: &str,
) -> LibraryResult<PlaylistKey> {
    let key: Option<PlaylistKey> = sqlx::query_scalar("INSERT INTO playlists(source_key,owner,object_id,indexed) VALUES (?1,'source',?2,0) ON CONFLICT(source_key,object_id) DO NOTHING RETURNING playlist_key")
        .bind(source).bind(object_id).fetch_optional(&mut *connection).await?;
    match key {
        Some(key) => Ok(key),
        None => Ok(sqlx::query_scalar(
            "SELECT playlist_key FROM playlists WHERE source_key=?1 AND object_id=?2",
        )
        .bind(source)
        .bind(object_id)
        .fetch_one(connection)
        .await?),
    }
}

pub(crate) async fn export_playlist_order_jsonl_on(
    connection: &mut SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<u64> {
    use futures_util::TryStreamExt;
    let mut rows=sqlx::query_as::<_,PlaylistIdentity>("SELECT playlist.owner='source' source_owned,source.object_id source_id,playlist.object_id,CASE WHEN playlist.owner='rufin' THEN playlist.name END name,row_number() OVER(ORDER BY playlist.created_at,playlist.playlist_key)-1 position FROM main.playlists playlist LEFT JOIN sources source USING(source_key) ORDER BY playlist.created_at,playlist.playlist_key").fetch(&mut *connection);
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
    write: &mut crate::catalog::write::Write<'_>,
    input: impl std::io::BufRead,
) -> LibraryResult<u64> {
    let mut count = 0;
    for line in input.lines() {
        match serde_json::from_str(&line?)? {
            PlaylistRecord::Playlist(identity) => {
                write_playlist_identity(write, &identity).await?;
                count += 1;
            }
            PlaylistRecord::Artwork(image) => {
                write_playlist_artwork(write.connection().await?, &image).await?;
                write.keep();
            }
            PlaylistRecord::File(record) => {
                crate::playlist_links::restore_record_on(write.connection().await?, record).await?;
                write.keep();
            }
        }
    }
    Ok(count)
}

pub(crate) async fn write_playlist_artwork(
    connection: &mut SqliteConnection,
    image: &PlaylistArtworkRecord,
) -> LibraryResult<bool> {
    let matched = "object_id=?1 AND (owner='rufin' OR source_key IS (SELECT source_key FROM sources WHERE object_id=?2))";
    let previous: Option<(bool, Option<Vec<u8>>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT owner='source',artwork_binding FROM playlists WHERE {matched}"
    )))
    .bind(&image.object_id)
    .bind(&image.source_id)
    .fetch_optional(&mut *connection)
    .await?;
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
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE main.playlists SET artwork_bytes=?3,artwork_mime=?4,artwork_revision=?5,artwork_binding=?6 WHERE {matched} AND (artwork_bytes,artwork_mime,artwork_revision,artwork_binding) IS NOT (?3,?4,?5,?6)")))
        .bind(&image.object_id).bind(&image.source_id).bind(&image.bytes).bind(&image.content_type).bind(revision).bind(binding).execute(&mut *connection).await?.rows_affected()>0)
}
