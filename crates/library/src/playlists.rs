//! Owns Playlist identity, exact duplicate occurrences, ordering, filtering, and edits.
//! Entries keep song references; song metadata belongs to the shared library.

use sqlx::{Connection, FromRow, SqliteConnection};

use crate::catalog::snapshot::{Entry, METADATA_WRITABLE, Packed, SOURCE_OWNED, WRITABLE};
use crate::catalog::write::{Scope, Write};
use crate::{
    Catalog, CatalogSort, FolderKey, LibraryError, LibraryResult, MemberSelection,
    PlaylistEntryKey, PlaylistKey, ReadCancellation, SourceKey, TrackKey,
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

pub(crate) const PLAYLIST_ROW_LIMIT: usize = 128;
const PLAYLIST_DELETE_BATCH: usize = 400;

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

#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistEditEntry {
    pub playlist_entry_key: PlaylistEntryKey,
    pub track_key: TrackKey,
    pub object_id: String,
    /// The entry's index in the playlist's order.
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
    /// Who owns a playlist and what may change, from its resident record. Only a playlist its
    /// source owns reads its server id.
    pub async fn playlist_access(
        &self,
        playlist: PlaylistKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistAccess>> {
        self.loaded().await;
        let Some((source_key, flags)) = ({
            let snapshot = self.snapshot();
            snapshot
                .playlists
                .get(playlist.raw())
                .map(|record| (snapshot.source_key(record.source), record.flags))
        }) else {
            return Ok(None);
        };
        let source_owned = flags & SOURCE_OWNED != 0;
        let object_id = if source_owned {
            let (_permit, mut connection) = self.acquire_general(cancellation).await?;
            sqlx::query_scalar("SELECT object_id FROM playlists WHERE playlist_key=?1")
                .bind(playlist)
                .fetch_optional(&mut *connection)
                .await?
        } else {
            None
        };
        Ok(Some(PlaylistAccess {
            source_key,
            source_id: source_key
                .and_then(|key| self.source_id(key))
                .map(|id| id.as_str().to_owned()),
            object_id,
            source_owned,
            writable: flags & WRITABLE != 0,
            metadata_writable: flags & METADATA_WRITABLE != 0,
        }))
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

    pub async fn delete_playlist(&self, playlist: PlaylistKey) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let deleted = sqlx::query("DELETE FROM playlists WHERE owner='rufin' AND playlist_key=?1")
            .bind(playlist)
            .execute(write.connection().await?)
            .await?
            .rows_affected()
            > 0;
        if deleted {
            write.remove_playlist(playlist.raw());
        }
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
        if count > 0 {
            crate::playlist_links::mark_dirty_on(self, write.connection().await?, playlist).await?;
        }
        write.remove_entries(playlist.raw(), removed);
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
        let (rank, track) = {
            let snapshot = self.snapshot();
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
            (crate::rank::between(low, high), TrackKey::from_raw(track))
        };
        sqlx::query("UPDATE playlist_entries SET rank=?2 WHERE playlist_entry_key=?1")
            .bind(entry)
            .bind(&rank)
            .execute(write.connection().await?)
            .await?;
        crate::playlist_links::mark_dirty_on(self, write.connection().await?, playlist).await?;
        write.put_entries(
            playlist.raw(),
            vec![Entry {
                key: entry.raw(),
                rank: rank.into(),
                track,
            }],
        );
        write.commit().await?;
        Ok(true)
    }

    /// Whether the user can change the playlist's entries: a writable Rufin playlist.
    fn editable_playlist(&self, playlist: PlaylistKey) -> bool {
        self.snapshot()
            .playlists
            .get(playlist.raw())
            .is_some_and(|record| record.flags & (SOURCE_OWNED | WRITABLE) == WRITABLE)
    }
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
                for entry in &record.entries {
                    let Some(track) = snapshot.tracks.live(entry.track) else {
                        continue;
                    };
                    holds_hidden |= !track.indexed();
                    for tag in snapshot.track_runs(track)[2] {
                        if snapshot
                            .tags
                            .live(*tag)
                            .is_some_and(|tag| tag.kind() == crate::TagKind::Genre)
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
                            i64::from(tag.key),
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
        mosaic: Option<String>,
        #[sqlx(flatten)]
        playlist: PlaylistRow,
    }
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT CAST(requested.key AS INTEGER) ordinal,playlist.mosaic,playlist.playlist_key,playlist.source_key,source.object_id source_id,
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
                 mosaic,
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
                playlist.artwork_binding = crate::artwork::playlist_artwork(
                    playlist.artwork_binding.take(),
                    (
                        false,
                        playlist.source_id.clone(),
                        playlist.object_id.clone(),
                    ),
                    mosaic.as_deref(),
                )?;
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
    let found: Vec<(PlaylistEntryKey, PlaylistKey, String, TrackKey)> = sqlx::query_as("SELECT entry.playlist_entry_key,entry.playlist_key,entry.object_id,entry.track_key FROM json_each(?1) requested JOIN playlist_entries entry ON entry.playlist_entry_key=requested.value ORDER BY CAST(requested.key AS INTEGER)")
        .bind(serde_json::to_string(entries)?)
        .fetch_all(&mut *connection)
        .await?;
    let mut positions = std::collections::HashMap::new();
    for (_, playlist, ..) in &found {
        positions
            .entry(*playlist)
            .or_insert_with(|| catalog.entry_positions(*playlist));
    }
    let keys = found.iter().map(|(.., track)| *track).collect::<Vec<_>>();
    let tracks = crate::tracks::load_track_rows(catalog, connection, &keys)
        .await?
        .into_iter()
        .map(|track| (track.track_key, track))
        .collect::<std::collections::HashMap<_, _>>();
    Ok(found
        .into_iter()
        .filter_map(|(playlist_entry_key, playlist, source_entry_id, track)| {
            Some(PlaylistEntryView {
                playlist_entry_key,
                position: *positions[&playlist].get(&playlist_entry_key.raw())? as i64,
                source_entry_id: Some(source_entry_id),
                track: tracks.get(&track)?.clone(),
            })
        })
        .collect())
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
         RETURNING {},object_id",
        crate::catalog::write::PLAYLIST_COLUMNS
    )))
    .bind(name)
    .fetch_one(write.connection().await?)
    .await?;
    let object_id = sqlx::Row::try_get(&row, 10)?;
    let header = crate::catalog::write::PlaylistRow::from_row(&row)?;
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
    let rows = tracks
        .iter()
        .zip(crate::rank::spread(last.as_deref(), None, tracks.len()))
        .map(|(track, rank)| (track.raw(), crate::rank::hex(&rank)))
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
/// names tracks rather than entries; a source's ids are the source's.
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
            _ => taken.insert(object.as_str()).then(|| object.clone()),
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
    sqlx::query(
        "DELETE FROM playlist_entries WHERE playlist_entry_key IN(SELECT value FROM json_each(?1))",
    )
    .bind(serde_json::to_string(&removed)?)
    .execute(&mut *connection)
    .await?;
    let rows = changed
        .iter()
        .map(|(key, object, track, rank)| (key, object, track, crate::rank::hex(rank)))
        .collect::<Vec<_>>();
    // Object ids move aside first, since kept entries may trade them.
    sqlx::query("UPDATE playlist_entries SET object_id='moving:'||playlist_entry_key WHERE playlist_entry_key IN(SELECT value->>0 FROM json_each(?1)) AND object_id IS NOT (SELECT value->>1 FROM json_each(?1) WHERE value->>0=playlist_entry_key)")
        .bind(serde_json::to_string(&rows)?)
        .execute(&mut *connection)
        .await?;
    sqlx::query("UPDATE playlist_entries SET object_id=coalesce(input.value->>1,'rufin:entry:'||lower(hex(randomblob(16)))),track_key=input.value->>2,rank=unhex(input.value->>3) FROM json_each(?1) input WHERE playlist_entry_key=input.value->>0")
        .bind(serde_json::to_string(&rows)?)
        .execute(&mut *connection)
        .await?;
    let rows = inserted
        .iter()
        .map(|(object, track, rank)| (object, track, crate::rank::hex(rank)))
        .collect::<Vec<_>>();
    let added: Vec<(i64, Vec<u8>, TrackKey)> = sqlx::query_as(
        "INSERT INTO playlist_entries(playlist_key,object_id,track_key,rank)
        SELECT ?1,coalesce(value->>0,'rufin:entry:'||lower(hex(randomblob(16)))),value->>1,unhex(value->>2) FROM json_each(?2) WHERE true
        RETURNING playlist_entry_key,rank,track_key",
    )
    .bind(playlist)
    .bind(serde_json::to_string(&rows)?)
    .fetch_all(&mut *connection)
    .await?;
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
    source_id: Option<String>,
    object_id: String,
    #[serde(default)]
    bytes: Option<Vec<u8>>,
    content_type: Option<String>,
    #[serde(default)]
    artwork_binding: Option<Vec<u8>>,
    #[serde(default)]
    mosaic: Option<String>,
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
    let revision = image
        .bytes
        .as_ref()
        .map(|bytes| blake3::hash(bytes).to_hex().to_string());
    let binding = image
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
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE main.playlists SET artwork_bytes=?3,artwork_mime=?4,artwork_revision=?5,artwork_binding=?6,mosaic=coalesce(?7,mosaic) WHERE {matched} AND (artwork_bytes,artwork_mime,artwork_revision,artwork_binding,mosaic) IS NOT (?3,?4,?5,?6,coalesce(?7,mosaic))")))
        .bind(&image.object_id).bind(&image.source_id).bind(&image.bytes).bind(&image.content_type).bind(revision).bind(binding).bind(&image.mosaic).execute(&mut *connection).await?.rows_affected()>0)
}
