//! Owns Playlist identity, exact duplicate occurrences, ordering, filtering, and edits.
//! Entries keep song references; song metadata belongs to the shared library.

use sqlx::{Connection, FromRow, SqliteConnection};

use crate::catalog::snapshot::{Entry, METADATA_WRITABLE, Packed, SOURCE_OWNED, WRITABLE};
use crate::catalog::write::{Scope, Write};
use crate::{
    Catalog, FolderKey, LibraryError, LibraryResult, MemberSelection, PlaylistEntryKey,
    PlaylistKey, ReadCancellation, SourceKey, TrackKey,
};

#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistAccess {
    pub source_key: Option<SourceKey>,
    pub source_id: Option<String>,
    /// The server's id of a playlist its source owns.
    pub object_id: Option<String>,
    pub source_owned: bool,
    pub writable: bool,
    pub metadata_writable: bool,
}

const PLAYLIST_DELETE_BATCH: usize = 400;

#[derive(Clone, Debug, PartialEq)]
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
    pub genres: Vec<PlaylistGenreLink>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlaylistGenreLink {
    pub genre_key: crate::GenreKey,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistEditEntry {
    pub playlist_entry_key: PlaylistEntryKey,
    pub track_key: TrackKey,
    pub object_id: String,
    /// The entry's index in the playlist's order.
    pub position: usize,
}

impl Catalog {
    /// Who owns a playlist and what may change, from its resident record. Only a playlist its
    /// source owns reads its server id.
    pub async fn playlist_access(
        &self,
        playlist: PlaylistKey,
        _cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistAccess>> {
        self.loaded().await;
        let snapshot = self.snapshot();
        let Some(record) = snapshot.playlists.get(playlist.raw()) else {
            return Ok(None);
        };
        let source_key = snapshot.source_key(record.source);
        let source_owned = record.flags & SOURCE_OWNED != 0;
        Ok(Some(PlaylistAccess {
            source_key,
            source_id: source_key
                .and_then(|key| snapshot.source(key))
                .map(|source| source.object.clone()),
            object_id: source_owned.then(|| snapshot.text(record.object).to_owned()),
            source_owned,
            writable: record.flags & WRITABLE != 0,
            metadata_writable: record.flags & METADATA_WRITABLE != 0,
        }))
    }

    /// A playlist's name from its resident record. An unnamed playlist has none.
    pub async fn playlist_name(&self, playlist: PlaylistKey) -> Option<String> {
        self.loaded().await;
        let snapshot = self.snapshot();
        let name = snapshot.text(snapshot.playlists.get(playlist.raw())?.name);
        (!name.is_empty()).then(|| name.to_owned())
    }

    /// Whether a playlist holds a track its source no longer lists, from its resident entries.
    pub fn playlist_holds_hidden(&self, playlist: PlaylistKey) -> bool {
        let snapshot = self.snapshot();
        snapshot.playlists.get(playlist.raw()).is_some_and(|record| {
            record.entries.iter().any(|entry| {
                snapshot
                    .tracks
                    .live(entry.track)
                    .is_some_and(|track| !track.indexed())
            })
        })
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
                .flat_map(|(_, playlist)| playlist.entries.iter().map(|entry| entry.track))
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
        let mut connection = self.acquire_cancellable(cancellation).await?;
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
        let order = self
            .order(
                &crate::Read::List(crate::ListRead::new(
                    crate::ListKind::Playlists,
                    source,
                    crate::LibraryField::RowIndex,
                )),
                cancellation,
            )
            .await?;
        let writable = {
            let snapshot = self.snapshot();
            order
                .items(0..order.len())
                .into_iter()
                .filter(|item| match item {
                    crate::Item::Playlist(key) => snapshot
                        .playlists
                        .get(key.raw())
                        .is_some_and(|playlist| playlist.flags & WRITABLE != 0),
                    _ => false,
                })
                .collect::<Vec<_>>()
        };
        let mut connection = self.acquire_loaded(cancellation).await?;
        Ok(self
            .rows_of_on(
                &mut connection,
                &writable,
                &crate::read::Shape::of(None),
                true,
            )
            .await?
            .into_iter()
            .filter_map(|row| match row {
                crate::Row::Playlist(crate::PlaylistListRow::Playlist(row)) => Some(row),
                _ => None,
            })
            .collect())
    }

    pub async fn playlist_edit_entries(
        &self,
        playlist: PlaylistKey,
        selection: &MemberSelection,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistEditEntry>> {
        let mut connection = self.acquire_loaded(cancellation).await?;
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
        let found: Vec<(PlaylistEntryKey, TrackKey, String)> = sqlx::query_as(
            "SELECT entry.playlist_entry_key,entry.track_key,entry.object_id
             FROM json_each(?1) requested JOIN playlist_entries entry ON entry.playlist_entry_key=requested.value
             WHERE entry.playlist_key=?2"
        ).bind(serde_json::to_string(&keys)?).bind(playlist).fetch_all(&mut *connection).await?;
        drop(connection);
        let positions = self.entry_positions(playlist);
        let mut entries = found
            .into_iter()
            .filter_map(|(playlist_entry_key, track_key, object_id)| {
                Some(PlaylistEditEntry {
                    position: *positions.get(&playlist_entry_key.raw())?,
                    playlist_entry_key,
                    track_key,
                    object_id,
                })
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.position);
        Ok(entries)
    }

    /// Each entry's index in its playlist's order.
    fn entry_positions(&self, playlist: PlaylistKey) -> std::collections::HashMap<i64, usize> {
        self.snapshot()
            .playlists
            .get(playlist.raw())
            .map(|record| {
                record
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(position, entry)| (entry.key, position))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A page of a playlist's entries and their URIs, in order, from `offset`.
    pub async fn playlist_entry_uri_page(
        &self,
        playlist: PlaylistKey,
        offset: usize,
    ) -> LibraryResult<Vec<(PlaylistEntryKey, String)>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as(
            "SELECT playlist_entry_key,media_uri FROM playlist_entries JOIN tracks USING(track_key)
            WHERE playlist_key=?1 ORDER BY rank,playlist_entry_key LIMIT ?3 OFFSET ?2",
        )
        .bind(playlist)
        .bind(offset as i64)
        .bind(256_i64)
        .fetch_all(&mut *connection)
        .await?)
    }

    /// The object ids of a playlist's tracks from an index of its order to the end.
    pub async fn playlist_track_ids_from(
        &self,
        playlist: PlaylistKey,
        position: usize,
    ) -> LibraryResult<Vec<String>> {
        let keys = self
            .snapshot()
            .playlists
            .get(playlist.raw())
            .map(|record| {
                record.entries[position.min(record.entries.len())..]
                    .iter()
                    .map(|entry| entry.key)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT track.object_id FROM json_each(?1) requested JOIN playlist_entries entry ON entry.playlist_entry_key=requested.value JOIN tracks track USING(track_key) ORDER BY CAST(requested.key AS INTEGER)")
            .bind(serde_json::to_string(&keys)?)
            .fetch_all(&mut *connection)
            .await?)
    }

    /// Object ids of a source's named playlists that Rufin owns or the source lists, by name.
    pub async fn source_playlist_object_ids(
        &self,
        source: SourceKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        tokio::select! {
            () = self.loaded() => {}
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        }
        let snapshot = self.snapshot();
        let Some(owner) = snapshot.source_slot(source) else {
            return Ok(Vec::new());
        };
        let mut playlists = snapshot
            .playlists
            .iter()
            .map(|(_, record)| record)
            .filter(|record| {
                record.source == owner
                    && !snapshot.text(record.name).is_empty()
                    && (record.flags & SOURCE_OWNED == 0 || record.indexed())
            })
            .map(|record| {
                (
                    snapshot.text(record.name).to_ascii_lowercase(),
                    record.key,
                    snapshot.text(record.object),
                )
            })
            .collect::<Vec<_>>();
        playlists.sort_unstable();
        Ok(playlists
            .into_iter()
            .map(|(.., object)| object.to_owned())
            .collect())
    }

    /// The selected tracks of a source with their object ids, as the source names them.
    pub async fn source_playlist_track_ids(
        &self,
        source: SourceKey,
        playlist: Option<PlaylistKey>,
        songs: &MemberSelection,
        skip_existing: bool,
    ) -> LibraryResult<Vec<(TrackKey, String)>> {
        let mut connection = self.acquire_reader().await?;
        let mut transaction = connection.begin().await?;
        let tracks = playlist_tracks_on(self, &mut transaction, Some(source), songs)
            .await?
            .ok_or_else(|| {
                LibraryError::InvalidRequest("Playlist Track is no longer current".into())
            })?;
        let ids = sqlx::query_as("SELECT track.track_key,track.object_id FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value
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
        let (playlist, object_id) = create_playlist_identity(&mut write, name).await?;
        insert_playlist_tracks(self, &mut write, playlist, &tracks, false).await?;
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
        let binding = match &revision {
            Some(revision) => {
                let (source_id, object_id): (Option<String>, String) = sqlx::query_as("SELECT source.object_id,playlist.object_id FROM playlists playlist LEFT JOIN sources source USING(source_key) WHERE playlist_key=?1").bind(playlist).fetch_one(&mut *transaction).await?;
                Some(crate::artwork::playlist_binding(
                    false,
                    source_id,
                    object_id,
                    revision.clone(),
                ))
            }
            None => None,
        };
        let row: Option<crate::catalog::write::PlaylistRow> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "UPDATE main.playlists SET name=COALESCE(?2,name),
             artwork_bytes=CASE WHEN ?3 THEN ?4 ELSE artwork_bytes END,
             artwork_mime=CASE WHEN ?3 THEN ?5 ELSE artwork_mime END,
             artwork_revision=CASE WHEN ?3 THEN ?6 ELSE artwork_revision END,
             artwork_binding=CASE WHEN ?3 THEN ?7 ELSE artwork_binding END
             WHERE owner='rufin' AND playlist_key=?1 AND metadata_writable
               AND ((?2 IS NOT NULL AND name IS NOT ?2) OR (?3 AND artwork_revision IS NOT ?6))
             RETURNING {}",
                crate::catalog::write::PLAYLIST_COLUMNS
            )))
            .bind(playlist)
            .bind(name)
            .bind(artwork.is_some())
            .bind(image.map(|(bytes, _)| bytes))
            .bind(image.and_then(|(_, mime)| mime))
            .bind(revision)
            .bind(binding)
            .fetch_optional(&mut *transaction)
            .await?;
        let changed = row.is_some();
        if changed && name.is_some() {
            crate::playlist_links::mark_dirty_on(self, transaction, playlist).await?;
        }
        if let Some(row) = row {
            write.put_playlist(row);
            // The image itself is read by key.
            write.changed(Scope::Playlist {
                key: playlist,
                entries: false,
            });
        }
        write.commit().await?;
        Ok(changed)
    }

    /// Deletes Rufin playlists in one Write, and returns how many went.
    pub async fn delete_playlists(&self, playlists: &[PlaylistKey]) -> LibraryResult<usize> {
        if playlists.is_empty() {
            return Ok(0);
        }
        let mut write = self.write().await?;
        let deleted: Vec<i64> = sqlx::query_scalar(
            "DELETE FROM playlists WHERE owner='rufin' AND playlist_key IN(SELECT value FROM json_each(?1)) RETURNING playlist_key",
        )
        .bind(serde_json::to_string(playlists)?)
        .fetch_all(write.connection().await?)
        .await?;
        for key in &deleted {
            write.remove_playlist(*key);
        }
        write.commit().await?;
        Ok(deleted.len())
    }

    pub async fn add_playlist_tracks(
        &self,
        playlist: PlaylistKey,
        songs: &MemberSelection,
        skip_existing: bool,
    ) -> LibraryResult<usize> {
        let mut write = self.write().await?;
        if !self.editable_playlist(playlist) {
            return Ok(0);
        }
        remember_uris(&mut write, songs).await?;
        let connection = write.connection().await?;
        let Some(tracks) = playlist_tracks_on(self, connection, None, songs).await? else {
            return Ok(0);
        };
        let accepted =
            insert_playlist_tracks(self, &mut write, playlist, &tracks, skip_existing).await?;
        if accepted > 0 {
            crate::playlist_links::mark_dirty_on(self, write.connection().await?, playlist).await?;
        }
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
        let mut write = self.write().await?;
        if !self.editable_playlist(playlist) {
            return Ok(0);
        }
        let count = remove_entries_on(&mut write, playlist, entries).await?;
        if count > 0 {
            crate::playlist_links::mark_dirty_on(self, write.connection().await?, playlist).await?;
        }
        write.commit().await?;
        Ok(count)
    }

    /// Moves an entry to an index of the playlist's order. Only the moved entry is written: it
    /// gets a rank between its new neighbours.
    pub async fn move_playlist_entry(
        &self,
        playlist: PlaylistKey,
        entry: PlaylistEntryKey,
        new_position: usize,
    ) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        if !self.editable_playlist(playlist) {
            return Ok(false);
        }
        if !move_entry_on(self, &mut write, playlist, entry, new_position).await? {
            return Ok(false);
        }
        crate::playlist_links::mark_dirty_on(self, write.connection().await?, playlist).await?;
        write.commit().await?;
        Ok(true)
    }

    /// Writes an entry edit the server has applied, from the resident entries, in one Write.
    /// The playlist's revision is cleared, so the next sync reads this playlist once.
    pub async fn edit_source_playlist(
        &self,
        playlist: PlaylistKey,
        edit: SourceEntryEdit,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let owned = self
            .snapshot()
            .playlists
            .get(playlist.raw())
            .is_some_and(|record| record.flags & SOURCE_OWNED != 0);
        if !owned {
            return Err(LibraryError::InvalidRequest(
                "The playlist does not belong to a source".into(),
            ));
        }
        match edit {
            SourceEntryEdit::Add { tracks } => {
                insert_playlist_tracks(self, &mut write, playlist, &tracks, false).await?;
            }
            SourceEntryEdit::Move { entry, position } => {
                move_entry_on(self, &mut write, playlist, entry, position).await?;
            }
            SourceEntryEdit::Remove { entries } => {
                remove_entries_on(&mut write, playlist, &entries).await?;
            }
        }
        sqlx::query("UPDATE playlists SET provider_revision=NULL WHERE playlist_key=?1 AND provider_revision IS NOT NULL")
            .bind(playlist)
            .execute(write.connection().await?)
            .await?;
        write.commit().await?;
        Ok(())
    }

    /// Whether the user can change the playlist's entries: a writable Rufin playlist.
    fn editable_playlist(&self, playlist: PlaylistKey) -> bool {
        self.snapshot()
            .playlists
            .get(playlist.raw())
            .is_some_and(|record| record.flags & (SOURCE_OWNED | WRITABLE) == WRITABLE)
    }
}

/// An entry edit of a server playlist, as the server applied it.
#[derive(Clone, Debug, PartialEq)]
pub enum SourceEntryEdit {
    /// Tracks appended at the end.
    Add {
        tracks: Vec<TrackKey>,
    },
    /// An entry moved to an index of the order.
    Move {
        entry: PlaylistEntryKey,
        position: usize,
    },
    Remove {
        entries: Vec<PlaylistEntryKey>,
    },
}

async fn remove_entries_on(
    write: &mut Write<'_>,
    playlist: PlaylistKey,
    entries: &[PlaylistEntryKey],
) -> LibraryResult<usize> {
    let mut removed = Vec::new();
    for batch in entries.chunks(PLAYLIST_DELETE_BATCH) {
        removed.extend(
            sqlx::query_scalar::<_, i64>(
                "DELETE FROM playlist_entries WHERE playlist_key=?1 AND playlist_entry_key IN(SELECT value FROM json_each(?2)) RETURNING playlist_entry_key",
            )
            .bind(playlist)
            .bind(serde_json::to_string(batch)?)
            .fetch_all(write.connection().await?)
            .await?,
        );
    }
    let count = removed.len();
    write.remove_entries(playlist.raw(), removed);
    Ok(count)
}

/// Gives the entry a rank between its neighbours at the new index. Returns whether the
/// playlist holds the entry.
async fn move_entry_on(
    catalog: &Catalog,
    write: &mut Write<'_>,
    playlist: PlaylistKey,
    entry: PlaylistEntryKey,
    new_position: usize,
) -> LibraryResult<bool> {
    let (rank, track) = {
        let snapshot = catalog.snapshot();
        let Some(record) = snapshot.playlists.get(playlist.raw()) else {
            return Ok(false);
        };
        let mut order = record.entries.iter().collect::<Vec<_>>();
        let Some(old_position) = order.iter().position(|other| other.key == entry.raw()) else {
            return Ok(false);
        };
        let new_position = new_position.min(order.len() - 1);
        if old_position == new_position {
            return Ok(true);
        }
        let moved = order.remove(old_position);
        let low = new_position.checked_sub(1).map(|index| &*order[index].rank);
        let high = order.get(new_position).map(|other| &*other.rank);
        let Some(track) = snapshot.tracks.key_of(moved.track) else {
            return Ok(false);
        };
        (
            crate::rank::distinct(crate::rank::between(low, high), high),
            TrackKey::from_raw(track),
        )
    };
    sqlx::query("UPDATE playlist_entries SET rank=?2 WHERE playlist_entry_key=?1")
        .bind(entry)
        .bind(&rank)
        .execute(write.connection().await?)
        .await?;
    write.put_entries(
        playlist.raw(),
        vec![Entry {
            key: entry.raw(),
            rank: rank.into(),
            track,
        }],
    );
    Ok(true)
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
                let references = uris
                    .iter()
                    .map(|uri| crate::TrackReference {
                        media_uri: uri.clone(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>();
                write.put_references(&references).await?;
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

pub(crate) async fn create_playlist_identity(
    write: &mut Write<'_>,
    name: &str,
) -> LibraryResult<(PlaylistKey, String)> {
    let name = name.trim();
    if name.is_empty() {
        return Err(LibraryError::InvalidRequest(
            "Playlist name cannot be empty".into(),
        ));
    }
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO main.playlists(object_id,name)
         VALUES('rufin:playlist:'||lower(hex(randomblob(16))),?1)
         RETURNING {}",
        crate::catalog::write::PLAYLIST_COLUMNS
    )))
    .bind(name)
    .fetch_one(write.connection().await?)
    .await?;
    let header = crate::catalog::write::PlaylistRow::from_row(&row)?;
    let object_id = header.10.clone();
    let key = PlaylistKey::from_raw(header.0);
    write.put_playlist(header);
    Ok((key, object_id))
}

/// Appends tracks after the playlist's last rank, in one INSERT. With `skip_existing`, tracks the
/// playlist already holds are left out.
async fn insert_playlist_tracks(
    catalog: &Catalog,
    write: &mut Write<'_>,
    playlist: PlaylistKey,
    tracks: &[TrackKey],
    skip_existing: bool,
) -> LibraryResult<usize> {
    let (tracks, last) = {
        let snapshot = catalog.snapshot();
        let entries = snapshot
            .playlists
            .get(playlist.raw())
            .map_or(&[][..], |record| record.entries.as_slice());
        let held = entries
            .iter()
            .filter(|_| skip_existing)
            .map(|entry| entry.track)
            .collect::<std::collections::HashSet<_>>();
        let tracks = tracks
            .iter()
            .copied()
            .filter(|track| {
                snapshot
                    .tracks
                    .slot(track.raw())
                    .is_none_or(|slot| !held.contains(&slot))
            })
            .collect::<Vec<_>>();
        (tracks, entries.last().map(|entry| entry.rank.clone()))
    };
    if tracks.is_empty() {
        return Ok(0);
    }
    let ranks = crate::rank::spread(last.as_deref(), None, tracks.len());
    let rows = tracks
        .iter()
        .zip(ranks.iter().enumerate())
        .map(|(track, (index, rank))| {
            let rank = crate::rank::distinct(rank.clone(), ranks.get(index + 1).map(Vec::as_slice));
            (track.raw(), crate::rank::hex(&rank))
        })
        .collect::<Vec<_>>();
    let inserted: Vec<(i64, Vec<u8>, i64)> = sqlx::query_as(
        "INSERT INTO playlist_entries(playlist_key,object_id,track_key,rank)
        SELECT ?1,'rufin:entry:'||lower(hex(randomblob(16))),value->>0,unhex(value->>1)
        FROM json_each(?2) ORDER BY CAST(key AS INTEGER) RETURNING playlist_entry_key,rank,track_key",
    )
    .bind(playlist)
    .bind(serde_json::to_string(&rows)?)
    .fetch_all(write.connection().await?)
    .await?;
    let count = inserted.len();
    write.put_entries(
        playlist.raw(),
        inserted
            .into_iter()
            .map(|(key, rank, track)| Entry {
                key,
                rank: rank.into(),
                track: TrackKey::from_raw(track),
            })
            .collect(),
    );
    Ok(count)
}

/// Makes a playlist's entries the wanted object ids and tracks, in order, and writes only what
/// differs. An entry is kept for a wanted one with its object id, else for the next one with its
/// track. Kept entries that are already in order keep their ranks; the rest get ranks between
/// their new neighbours. With `keep_ids`, kept entries keep their object ids, as a playlist file
/// or a Subsonic server names tracks rather than entries; other sources' ids are the source's.
/// An empty wanted id names no entry: a new entry gets its own.
pub(crate) async fn write_entries(
    write: &mut Write<'_>,
    playlist: i64,
    wanted: &[(String, TrackKey)],
    keep_ids: bool,
) -> LibraryResult<bool> {
    let existing: Vec<(i64, String, TrackKey, Vec<u8>)> = sqlx::query_as(
        "SELECT playlist_entry_key,object_id,track_key,rank FROM playlist_entries WHERE playlist_key=?1 ORDER BY rank,playlist_entry_key",
    )
    .bind(playlist)
    .fetch_all(write.connection().await?)
    .await?;
    let mut by_object = std::collections::HashMap::new();
    let mut by_track =
        std::collections::HashMap::<TrackKey, std::collections::VecDeque<usize>>::new();
    for (index, (_, object, track, _)) in existing.iter().enumerate() {
        by_object.insert(object.as_str(), index);
        by_track.entry(*track).or_default().push_back(index);
    }
    let mut used = vec![false; existing.len()];
    let mut matched = wanted
        .iter()
        .map(|(object, _)| {
            let index = by_object.get(object.as_str()).copied()?;
            used[index] = true;
            Some(index)
        })
        .collect::<Vec<_>>();
    for (slot, (_, track)) in matched.iter_mut().zip(wanted) {
        if slot.is_none() {
            let unused = by_track.get_mut(track).and_then(|indexes| {
                while let Some(index) = indexes.pop_front() {
                    if !used[index] {
                        return Some(index);
                    }
                }
                None
            });
            if let Some(index) = unused {
                used[index] = true;
                *slot = Some(index);
            }
        }
    }
    let kept = in_order(&matched);
    let mut ranks: Vec<Option<Vec<u8>>> = matched
        .iter()
        .zip(&kept)
        .map(|(index, kept)| kept.then(|| existing[index.unwrap()].3.clone()))
        .collect();
    let mut start = 0;
    while start < ranks.len() {
        if ranks[start].is_some() {
            start += 1;
            continue;
        }
        let end = (start..ranks.len())
            .find(|index| ranks[*index].is_some())
            .unwrap_or(ranks.len());
        let low = start.checked_sub(1).and_then(|index| ranks[index].clone());
        let high = ranks.get(end).cloned().flatten();
        for (index, rank) in (start..end).zip(crate::rank::spread(
            low.as_deref(),
            high.as_deref(),
            end - start,
        )) {
            ranks[index] = Some(rank);
        }
        start = end;
    }
    let removed = existing
        .iter()
        .zip(&used)
        .filter(|(_, used)| !**used)
        .map(|((key, ..), _)| *key)
        .collect::<Vec<_>>();
    // Object ids stay unique in a playlist: a repeated wanted id gets a new one.
    let mut taken = matched
        .iter()
        .flatten()
        .filter(|_| keep_ids)
        .map(|index| existing[*index].1.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut changed = Vec::new();
    let mut inserted = Vec::new();
    for ((object, track), (index, rank)) in wanted.iter().zip(matched.iter().zip(ranks)) {
        let rank = rank.expect("every wanted entry has a rank");
        let object = match index {
            Some(index) if keep_ids => Some(existing[*index].1.clone()),
            _ => (!object.is_empty() && taken.insert(object.as_str())).then(|| object.clone()),
        };
        match index {
            Some(index) => {
                let (key, old_object, old_track, old_rank) = &existing[*index];
                if (Some(old_object), old_track, old_rank) != (object.as_ref(), track, &rank) {
                    changed.push((*key, object, *track, rank));
                }
            }
            None => inserted.push((object, *track, rank)),
        }
    }
    if removed.is_empty() && changed.is_empty() && inserted.is_empty() {
        return Ok(false);
    }
    let connection = write.connection().await?;
    if !removed.is_empty() {
        sqlx::query(
            "DELETE FROM playlist_entries WHERE playlist_entry_key IN(SELECT value FROM json_each(?1))",
        )
        .bind(serde_json::to_string(&removed)?)
        .execute(&mut *connection)
        .await?;
    }
    if !changed.is_empty() {
        let rows = changed
            .iter()
            .map(|(key, object, track, rank)| (key, object, track, crate::rank::hex(rank)))
            .collect::<Vec<_>>();
        sqlx::query("UPDATE playlist_entries SET object_id=coalesce(input.value->>1,'rufin:entry:'||lower(hex(randomblob(16)))),track_key=input.value->>2,rank=unhex(input.value->>3) FROM json_each(?1) input WHERE playlist_entry_key=input.value->>0")
            .bind(serde_json::to_string(&rows)?)
            .execute(&mut *connection)
            .await?;
    }
    let added: Vec<(i64, Vec<u8>, TrackKey)> = if inserted.is_empty() {
        Vec::new()
    } else {
        let rows = inserted
            .iter()
            .map(|(object, track, rank)| (object, track, crate::rank::hex(rank)))
            .collect::<Vec<_>>();
        sqlx::query_as(
            "INSERT INTO playlist_entries(playlist_key,object_id,track_key,rank)
            SELECT ?1,coalesce(value->>0,'rufin:entry:'||lower(hex(randomblob(16)))),value->>1,unhex(value->>2) FROM json_each(?2) WHERE true
            RETURNING playlist_entry_key,rank,track_key",
        )
        .bind(playlist)
        .bind(serde_json::to_string(&rows)?)
        .fetch_all(&mut *connection)
        .await?
    };
    write.remove_entries(playlist, removed);
    write.put_entries(
        playlist,
        changed
            .into_iter()
            .map(|(key, _, track, rank)| (key, rank, track))
            .chain(added)
            .map(|(key, rank, track)| Entry {
                key,
                rank: rank.into(),
                track,
            })
            .collect(),
    );
    Ok(true)
}

/// Which matched entries keep their place: the longest run of them already in order.
fn in_order(matched: &[Option<usize>]) -> Vec<bool> {
    // Patience sorting: tails[length] ends the best increasing run of that length + 1.
    let mut tails: Vec<usize> = Vec::new();
    let mut previous = vec![None; matched.len()];
    for (position, index) in matched.iter().enumerate() {
        let Some(index) = index else {
            continue;
        };
        let length = tails.partition_point(|tail| matched[*tail].unwrap() < *index);
        previous[position] = length.checked_sub(1).map(|length| tails[length]);
        if length == tails.len() {
            tails.push(position);
        } else {
            tails[length] = position;
        }
    }
    let mut kept = vec![false; matched.len()];
    let mut next = tails.last().copied();
    while let Some(position) = next {
        kept[position] = true;
        next = previous[position];
    }
    kept
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
    pub(crate) source_id: Option<String>,
    pub(crate) object_id: String,
    #[serde(default)]
    pub(crate) bytes: Option<Vec<u8>>,
    pub(crate) content_type: Option<String>,
    #[serde(default)]
    pub(crate) artwork_binding: Option<Vec<u8>>,
    #[serde(default)]
    pub(crate) mosaic: Option<String>,
}

/// An entry of a Rufin playlist by object ids, with its track's URI and copy.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistEntryCopy {
    pub playlist: String,
    pub object_id: String,
    pub rank: Vec<u8>,
    pub track: crate::TrackReference,
}

impl Catalog {
    /// The entries of Rufin playlists in rank order: those of `playlists` or of all of them,
    /// and only those of `entries` when given.
    pub async fn playlist_entry_copies(
        &self,
        playlists: Option<&[PlaylistKey]>,
        entries: Option<&[String]>,
    ) -> LibraryResult<Vec<PlaylistEntryCopy>> {
        let mut connection = self.acquire_reader().await?;
        let rows: Vec<(String, String, Vec<u8>, String, String, String, String, i64)> =
            sqlx::query_as(
                "SELECT playlist.object_id,entry.object_id,entry.rank,track.media_uri,track.title,track.display_artist,track.display_album,track.duration_millis
                 FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) JOIN tracks track USING(track_key)
                 WHERE playlist.owner='rufin' AND (?1 IS NULL OR playlist.playlist_key IN(SELECT value FROM json_each(?1)))
                   AND (?2 IS NULL OR entry.object_id IN(SELECT value FROM json_each(?2)))
                 ORDER BY entry.playlist_key,entry.rank,entry.playlist_entry_key",
            )
            .bind(playlists.map(serde_json::to_string).transpose()?)
            .bind(entries.map(serde_json::to_string).transpose()?)
            .fetch_all(&mut *connection)
            .await?;
        Ok(rows
            .into_iter()
            .map(
                |(playlist, object_id, rank, uri, title, artist, album, duration)| {
                    PlaylistEntryCopy {
                        playlist,
                        object_id,
                        rank,
                        track: crate::TrackReference::direct(uri, title, artist, album, duration),
                    }
                },
            )
            .collect())
    }

    /// Writes Rufin playlist entries by object id with their ranks and tracks, in one Write.
    /// A track the Store does not know is remembered from the entry's copy; an entry whose
    /// playlist is not here is left out. Returns how many entries changed.
    pub async fn put_playlist_entries(&self, entries: &[PlaylistEntryCopy]) -> LibraryResult<u64> {
        if entries.is_empty() {
            return Ok(0);
        }
        let mut write = self.write().await?;
        let references = entries
            .iter()
            .map(|entry| entry.track.clone())
            .collect::<Vec<_>>();
        let tracks = write.put_references(&references).await?;
        let rows = entries
            .iter()
            .zip(tracks)
            .map(|(entry, track)| {
                (
                    &entry.playlist,
                    &entry.object_id,
                    track,
                    crate::rank::hex(&entry.rank),
                )
            })
            .collect::<Vec<_>>();
        let written: Vec<(PlaylistKey, i64, Vec<u8>, TrackKey)> = sqlx::query_as(
            "INSERT INTO playlist_entries(playlist_key,object_id,track_key,rank)
             SELECT playlist.playlist_key,input.value->>1,input.value->>2,unhex(input.value->>3)
             FROM json_each(?1) input JOIN playlists playlist ON playlist.object_id=input.value->>0 AND playlist.source_key IS NULL AND playlist.owner='rufin'
             WHERE true
             ON CONFLICT(playlist_key,object_id) DO UPDATE SET track_key=excluded.track_key,rank=excluded.rank
             WHERE (track_key,rank) IS NOT (excluded.track_key,excluded.rank)
             RETURNING playlist_key,playlist_entry_key,rank,track_key",
        )
        .bind(serde_json::to_string(&rows)?)
        .fetch_all(write.connection().await?)
        .await?;
        let count = written.len() as u64;
        let mut by_playlist = std::collections::BTreeMap::<PlaylistKey, Vec<_>>::new();
        for (playlist, key, rank, track) in written {
            by_playlist.entry(playlist).or_default().push(Entry {
                key,
                rank: rank.into(),
                track,
            });
        }
        for (playlist, entries) in by_playlist {
            write.put_entries(playlist.raw(), entries);
            crate::playlist_links::mark_dirty_on(self, write.connection().await?, playlist).await?;
        }
        write.commit().await?;
        Ok(count)
    }

    /// Removes Rufin playlist entries by object id, in one Write.
    pub async fn remove_playlist_entries_by_object(&self, ids: &[String]) -> LibraryResult<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut write = self.write().await?;
        let mut by_playlist = std::collections::BTreeMap::<PlaylistKey, Vec<i64>>::new();
        for batch in ids.chunks(PLAYLIST_DELETE_BATCH) {
            let removed: Vec<(PlaylistKey, i64)> = sqlx::query_as(
                "DELETE FROM playlist_entries WHERE object_id IN(SELECT value FROM json_each(?1))
                 AND playlist_key IN(SELECT playlist_key FROM playlists WHERE owner='rufin')
                 RETURNING playlist_key,playlist_entry_key",
            )
            .bind(serde_json::to_string(batch)?)
            .fetch_all(write.connection().await?)
            .await?;
            for (playlist, key) in removed {
                by_playlist.entry(playlist).or_default().push(key);
            }
        }
        let mut count = 0;
        for (playlist, keys) in by_playlist {
            count += keys.len() as u64;
            write.remove_entries(playlist.raw(), keys);
            crate::playlist_links::mark_dirty_on(self, write.connection().await?, playlist).await?;
        }
        write.commit().await?;
        Ok(count)
    }

    /// Rufin playlists by object id, all of them or those of `playlists`.
    pub async fn playlist_copies(
        &self,
        playlists: Option<&[PlaylistKey]>,
    ) -> LibraryResult<Vec<PlaylistCopy>> {
        let mut connection = self.acquire_reader().await?;
        let rows: Vec<(String, String, Option<String>, i64, Option<String>)> = sqlx::query_as(
            "SELECT playlist.object_id,coalesce(playlist.name,''),folder.object_id,playlist.created_at,playlist.mosaic
             FROM playlists playlist LEFT JOIN playlist_folders folder ON folder.folder_key=playlist.playlist_folder_key
             WHERE playlist.owner='rufin' AND playlist.source_key IS NULL
               AND (?1 IS NULL OR playlist.playlist_key IN(SELECT value FROM json_each(?1)))
             ORDER BY playlist.object_id",
        )
        .bind(playlists.map(serde_json::to_string).transpose()?)
        .fetch_all(&mut *connection)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(object_id, name, folder, created_at, mosaic)| PlaylistCopy {
                    object_id,
                    name,
                    folder,
                    created_at,
                    mosaic,
                },
            )
            .collect())
    }

    /// Writes Rufin playlists by object id, in one Write. A folder that is not here leaves the
    /// playlist outside folders; a missing mosaic keeps the stored one.
    pub async fn put_playlist_copies(&self, playlists: &[PlaylistCopy]) -> LibraryResult<()> {
        if playlists.is_empty() {
            return Ok(());
        }
        let rows = playlists
            .iter()
            .map(|playlist| {
                (
                    &playlist.object_id,
                    &playlist.name,
                    &playlist.folder,
                    playlist.created_at,
                    &playlist.mosaic,
                )
            })
            .collect::<Vec<_>>();
        let mut write = self.write().await?;
        let written: Vec<crate::catalog::write::PlaylistRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "INSERT INTO playlists(owner,object_id,name,created_at,playlist_folder_key,mosaic)
             SELECT 'rufin',input.value->>0,input.value->>1,input.value->>3,
                 (SELECT folder_key FROM playlist_folders WHERE object_id=input.value->>2 AND NOT smart),input.value->>4
             FROM json_each(?1) input WHERE true
             ON CONFLICT(object_id) WHERE source_key IS NULL DO UPDATE SET name=excluded.name,created_at=excluded.created_at,
                 playlist_folder_key=excluded.playlist_folder_key,mosaic=coalesce(excluded.mosaic,mosaic)
             WHERE owner='rufin' AND (name,created_at,playlist_folder_key,mosaic) IS NOT
                 (excluded.name,excluded.created_at,excluded.playlist_folder_key,coalesce(excluded.mosaic,mosaic))
             RETURNING {}",
            crate::catalog::write::PLAYLIST_COLUMNS
        )))
        .bind(serde_json::to_string(&rows)?)
        .fetch_all(write.connection().await?)
        .await?;
        for row in written {
            let key = PlaylistKey::from_raw(row.0);
            crate::playlist_links::mark_dirty_on(self, write.connection().await?, key).await?;
            write.put_playlist(row);
        }
        write.commit().await?;
        Ok(())
    }

    /// The images people chose for Rufin and smart playlists, all of them or those of
    /// `objects`, by object id. Bytes are read only when asked for.
    pub async fn playlist_images(
        &self,
        objects: Option<&[String]>,
        bytes: bool,
    ) -> LibraryResult<Vec<PlaylistImage>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as(
            "SELECT object_id,artwork_mime mime,artwork_revision revision,CASE WHEN ?2 THEN artwork_bytes END bytes
             FROM playlists WHERE owner='rufin' AND source_key IS NULL AND artwork_bytes IS NOT NULL
               AND (?1 IS NULL OR object_id IN(SELECT value FROM json_each(?1)))
             UNION ALL SELECT object_id,NULL,artwork_revision,CASE WHEN ?2 THEN artwork_bytes END
             FROM smart_playlists WHERE artwork_bytes IS NOT NULL
               AND (?1 IS NULL OR object_id IN(SELECT value FROM json_each(?1)))
             ORDER BY 1",
        )
        .bind(objects.map(serde_json::to_string).transpose()?)
        .bind(bytes)
        .fetch_all(&mut *connection)
        .await?)
    }

    /// Sets or clears the chosen images of Rufin and smart playlists by object id, in one
    /// Write.
    pub async fn put_playlist_images(
        &self,
        images: Vec<(String, Option<(Vec<u8>, Option<String>)>)>,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        for (object_id, image) in images {
            let (bytes, content_type) = image.unzip();
            write_playlist_artwork(
                &mut write,
                &PlaylistArtworkRecord {
                    source_id: None,
                    object_id,
                    bytes,
                    content_type: content_type.flatten(),
                    artwork_binding: None,
                    mosaic: None,
                },
            )
            .await?;
        }
        write.commit().await?;
        Ok(())
    }
}

/// A Rufin playlist's shared identity: its folder by object id, its creation time and the
/// mosaic's track URIs as a JSON list.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistCopy {
    pub object_id: String,
    pub name: String,
    pub folder: Option<String>,
    pub created_at: i64,
    pub mosaic: Option<String>,
}

/// A playlist or smart playlist image a person chose. Smart playlists keep no MIME type.
#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
pub struct PlaylistImage {
    pub object_id: String,
    pub mime: Option<String>,
    pub revision: String,
    pub bytes: Option<Vec<u8>>,
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
    if let Some(source) = source {
        return source_playlist_identity(write, source, &identity.object_id).await;
    }
    let columns = crate::catalog::write::PLAYLIST_COLUMNS;
    let connection = write.connection().await?;
    // Rufin playlists are global, whatever source a backup scoped them to.
    let existing = sqlx::query_scalar::<_, PlaylistKey>(
        "SELECT playlist_key FROM playlists WHERE object_id=?1 AND source_key IS NULL",
    )
    .bind(&identity.object_id)
    .fetch_optional(&mut *connection)
    .await?;
    let (key, row) = match existing {
        Some(key) if source_owned => (key, None),
        Some(key) => (key, sqlx::query_as::<_, crate::catalog::write::PlaylistRow>(sqlx::AssertSqlSafe(format!("UPDATE playlists SET name=?2 WHERE playlist_key=?1 AND owner='rufin' AND name IS NOT ?2 RETURNING {columns}")))
            .bind(key).bind(&identity.name).fetch_optional(&mut *connection).await?),
        None => {
            let row: crate::catalog::write::PlaylistRow = sqlx::query_as(sqlx::AssertSqlSafe(format!("INSERT INTO playlists(owner,object_id,name,indexed) VALUES (?1,?2,?3,?4) RETURNING {columns}")))
                .bind(if source_owned { "source" } else { "rufin" }).bind(&identity.object_id).bind(&identity.name)
                .bind(!source_owned)
                .fetch_one(connection).await?;
            (PlaylistKey::from_raw(row.0), Some(row))
        }
    };
    if let Some(row) = row {
        write.put_playlist(row);
    }
    Ok(key)
}
/// A playlist a source owns, known by identity before the source describes it.
async fn source_playlist_identity(
    write: &mut Write<'_>,
    source: SourceKey,
    object_id: &str,
) -> LibraryResult<PlaylistKey> {
    let connection = write.connection().await?;
    let row: Option<crate::catalog::write::PlaylistRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!("INSERT INTO playlists(source_key,owner,object_id,indexed) VALUES (?1,'source',?2,0) ON CONFLICT(source_key,object_id) DO NOTHING RETURNING {}", crate::catalog::write::PLAYLIST_COLUMNS)))
        .bind(source).bind(object_id).fetch_optional(&mut *connection).await?;
    match row {
        Some(row) => {
            let key = PlaylistKey::from_raw(row.0);
            write.put_playlist(row);
            Ok(key)
        }
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
    let mut images = sqlx::query_as::<_, PlaylistArtworkRecord>("SELECT source.object_id source_id,playlist.object_id,playlist.artwork_bytes bytes,playlist.artwork_mime content_type,playlist.artwork_binding,playlist.mosaic FROM main.playlists playlist LEFT JOIN sources source USING(source_key) WHERE playlist.artwork_binding IS NOT NULL OR playlist.mosaic IS NOT NULL").fetch(&mut *connection);
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
                write_playlist_artwork(write, &image).await?;
            }
            PlaylistRecord::File(record) => {
                crate::playlist_links::restore_record_on(write.connection().await?, record).await?;
                write.keep();
            }
        }
    }
    Ok(count)
}

/// The one image writer of playlists and smart playlists by object id: a playlist of the
/// record's source, else, for a record with no source, the smart playlist. A missing mosaic keeps
/// the stored one.
pub(crate) async fn write_playlist_artwork(
    write: &mut Write<'_>,
    image: &PlaylistArtworkRecord,
) -> LibraryResult<()> {
    let matched = "object_id=?1 AND (owner='rufin' OR source_key IS (SELECT source_key FROM sources WHERE object_id=?2))";
    let revision = image
        .bytes
        .as_ref()
        .map(|bytes| blake3::hash(bytes).to_hex().to_string());
    let stored = image
        .artwork_binding
        .as_deref()
        .map(crate::normalize_artwork_binding)
        .transpose()?;
    let binding = |smart| {
        stored.clone().or_else(|| {
            revision.as_ref().map(|revision| {
                crate::artwork::playlist_binding(
                    smart,
                    image.source_id.clone(),
                    image.object_id.clone(),
                    revision.clone(),
                )
            })
        })
    };
    let row: Option<crate::catalog::write::PlaylistRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!("UPDATE main.playlists SET artwork_bytes=?3,artwork_mime=?4,artwork_revision=?5,artwork_binding=?6,mosaic=coalesce(?7,mosaic) WHERE {matched} AND (artwork_bytes,artwork_mime,artwork_revision,artwork_binding,mosaic) IS NOT (?3,?4,?5,?6,coalesce(?7,mosaic)) RETURNING {}", crate::catalog::write::PLAYLIST_COLUMNS)))
        .bind(&image.object_id).bind(&image.source_id).bind(&image.bytes).bind(&image.content_type).bind(&revision).bind(binding(false)).bind(&image.mosaic).fetch_optional(write.connection().await?).await?;
    if let Some(row) = row {
        let key = PlaylistKey::from_raw(row.0);
        write.put_playlist(row);
        // The image itself is read by key.
        write.changed(Scope::Playlist {
            key,
            entries: false,
        });
        return Ok(());
    }
    if image.source_id.is_some() {
        return Ok(());
    }
    let smart: Option<crate::catalog::write::SmartRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!("UPDATE smart_playlists SET artwork_bytes=?2,artwork_revision=?3,artwork_binding=?4,mosaic=coalesce(?5,mosaic) WHERE object_id=?1 AND (artwork_bytes,artwork_revision,artwork_binding,mosaic) IS NOT (?2,?3,?4,coalesce(?5,mosaic)) RETURNING {}", crate::catalog::write::SMART_COLUMNS)))
        .bind(&image.object_id).bind(&image.bytes).bind(&revision).bind(binding(true)).bind(&image.mosaic).fetch_optional(write.connection().await?).await?;
    if let Some(row) = smart {
        let key = crate::SmartPlaylistKey::from_raw(row.0);
        write.put_smart(row);
        write.changed(Scope::Smart { key });
    }
    Ok(())
}
