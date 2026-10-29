//! Sidebar pins: rows read by key, totals from the resident Catalog; smart totals are evaluated
//! separately from row loading.
use crate::{
    AlbumView, ArtistView, Catalog, GenreView, LibraryResult, PlaylistFolderRow, PlaylistRow,
    ReadCancellation, SmartPlaylistRow, SourceId,
};
use serde::{Deserialize, Serialize};
use sqlx::AssertSqlSafe;
use std::collections::HashMap;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarPin {
    Album {
        source_id: SourceId,
        album_id: String,
    },
    Artist {
        source_id: SourceId,
        artist_id: String,
        #[serde(default, skip_serializing_if = "is_false")]
        album_artist: bool,
    },
    Genre {
        source_id: SourceId,
        genre_id: String,
    },
    Playlist {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<SourceId>,
        playlist_id: String,
    },
    SmartPlaylist {
        playlist_id: String,
    },
    PlaylistFolder {
        folder_id: String,
        smart: bool,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A pinned row the Store still holds.
#[derive(Clone, Debug, PartialEq)]
pub enum PinRow {
    Album(Box<AlbumView>),
    Artist(ArtistView),
    Genre(GenreView),
    Playlist(PlaylistRow),
    SmartPlaylist(SmartPlaylistRow),
    PlaylistFolder(PlaylistFolderRow),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SidebarPinRow {
    pub pin: SidebarPin,
    pub key: i64,
    pub name: String,
    pub totals: Option<(i64, i64)>,
    pub artwork: Vec<Vec<u8>>,
}

impl From<(SidebarPin, PinRow)> for SidebarPinRow {
    /// The sidebar's reduced row. Smart playlists and folders wait for `sidebar_pin_totals`.
    fn from((pin, row): (SidebarPin, PinRow)) -> Self {
        let (key, name, totals, artwork) = match row {
            PinRow::Album(album) => (
                album.album_key.raw(),
                album.metadata.title,
                Some((album.summary.track_count, album.summary.duration_millis)),
                album.artwork_binding.into_iter().collect(),
            ),
            PinRow::Artist(artist) => (
                artist.artist_key.raw(),
                artist.metadata.name,
                Some((artist.summary.track_count, artist.summary.duration_millis)),
                artist.artwork_binding.into_iter().collect(),
            ),
            PinRow::Genre(genre) => (
                genre.genre_key.raw(),
                genre.metadata.name,
                Some((genre.summary.track_count, genre.summary.duration_millis)),
                match genre.artwork_binding {
                    Some(binding) => vec![binding],
                    None => genre.representative_artwork,
                },
            ),
            PinRow::Playlist(playlist) => (
                playlist.playlist_key.raw(),
                playlist.name,
                Some((playlist.track_count, playlist.duration_millis)),
                playlist.artwork_binding.into_iter().collect(),
            ),
            PinRow::SmartPlaylist(smart) => (
                smart.smart_playlist_key.raw(),
                smart.name,
                None,
                smart.artwork_binding.into_iter().collect(),
            ),
            PinRow::PlaylistFolder(folder) => (
                folder.folder_key.raw(),
                folder.name,
                None,
                folder.artwork_binding.into_iter().collect(),
            ),
        };
        Self {
            pin,
            key,
            name,
            totals,
            artwork,
        }
    }
}

impl Catalog {
    /// The key of a pinned row, when the Store still holds it.
    async fn pin_key(
        connection: &mut sqlx::SqliteConnection,
        pin: &SidebarPin,
    ) -> LibraryResult<Option<i64>> {
        let (sql, source, object) = match pin {
            SidebarPin::Album {
                source_id,
                album_id,
            } => (
                "SELECT item.album_key FROM albums item JOIN sources source USING(source_key) WHERE source.object_id=?1 AND item.object_id=?2",
                Some(source_id.as_str()),
                album_id,
            ),
            SidebarPin::Artist {
                source_id,
                artist_id,
                ..
            } => (
                "SELECT item.artist_key FROM artists item JOIN sources source USING(source_key) WHERE source.object_id=?1 AND item.object_id=?2",
                Some(source_id.as_str()),
                artist_id,
            ),
            SidebarPin::Genre {
                source_id,
                genre_id,
            } => (
                "SELECT item.tag_key FROM tags item JOIN sources source USING(source_key) WHERE item.kind=0 AND source.object_id=?1 AND item.object_id=?2",
                Some(source_id.as_str()),
                genre_id,
            ),
            SidebarPin::Playlist {
                source_id,
                playlist_id,
            } => (
                "SELECT item.playlist_key FROM playlists item LEFT JOIN sources source USING(source_key) WHERE source.object_id IS ?1 AND (?1 IS NULL OR item.source_key IS NOT NULL) AND item.object_id=?2 AND item.name IS NOT NULL",
                source_id.as_ref().map(SourceId::as_str),
                playlist_id,
            ),
            SidebarPin::SmartPlaylist { playlist_id } => (
                "SELECT smart_playlist_key FROM smart_playlists WHERE ?1 IS NULL AND object_id=?2",
                None,
                playlist_id,
            ),
            SidebarPin::PlaylistFolder { folder_id, .. } => (
                "SELECT folder_key FROM playlist_folders WHERE ?1 IS NULL AND object_id=?2",
                None,
                folder_id,
            ),
        };
        Ok(sqlx::query_scalar(AssertSqlSafe(sql))
            .bind(source)
            .bind(object)
            .fetch_optional(connection)
            .await?)
    }

    /// Rows of the pins the Store still holds, in pin order. Smart playlists and playlist folders
    /// come without totals, which take an evaluation, and smart playlists without their
    /// definitions; `complete_pin_rows` reads both.
    pub async fn pin_rows(
        &self,
        pins: &[SidebarPin],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(SidebarPin, PinRow)>> {
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let mut rows = Vec::with_capacity(pins.len());
        for pin in pins {
            let Some(key) = Self::pin_key(&mut connection, pin).await? else {
                continue;
            };
            let row = match pin {
                SidebarPin::Album { .. } => crate::collections::load_album_rows(
                    self,
                    &mut connection,
                    &[crate::AlbumKey::from_raw(key)],
                    false,
                )
                .await?
                .pop()
                .map(|album| PinRow::Album(Box::new(album))),
                SidebarPin::Artist { album_artist, .. } => crate::collections::load_artist_rows(
                    self,
                    &mut connection,
                    &[crate::ArtistKey::from_raw(key)],
                    *album_artist,
                    false,
                )
                .await?
                .pop()
                .map(PinRow::Artist),
                SidebarPin::Genre { .. } => crate::collections::load_genre_rows(
                    self,
                    &mut connection,
                    &[crate::GenreKey::from_raw(key)],
                )
                .await?
                .pop()
                .map(PinRow::Genre),
                SidebarPin::Playlist { .. } => crate::playlists::load_playlist_rows(
                    self,
                    &mut connection,
                    &[crate::PlaylistKey::from_raw(key)],
                )
                .await?
                .pop()
                .map(PinRow::Playlist),
                SidebarPin::SmartPlaylist { playlist_id } => {
                    // The definition is left unread, so a playlist this build cannot read
                    // still lists.
                    let (name, position, binding, mosaic): (
                        String,
                        i64,
                        Option<Vec<u8>>,
                        Option<String>,
                    ) = sqlx::query_as(AssertSqlSafe(format!(
                        "SELECT name,position,{},mosaic FROM smart_playlists WHERE smart_playlist_key=?1",
                        crate::artwork::preview_sql("artwork_binding")
                    )))
                    .bind(key)
                    .fetch_one(&mut *connection)
                    .await?;
                    Some(PinRow::SmartPlaylist(SmartPlaylistRow {
                        smart_playlist_key: crate::SmartPlaylistKey::from_raw(key),
                        object_id: playlist_id.clone(),
                        name,
                        definition: Default::default(),
                        position,
                        track_count: 0,
                        duration_millis: 0,
                        downloaded_count: 0,
                        artwork_binding: crate::artwork::playlist_artwork(
                            binding,
                            (true, None, playlist_id.clone()),
                            mosaic.as_deref(),
                        )?,
                    }))
                }
                SidebarPin::PlaylistFolder { smart: true, .. } => {
                    crate::smart_playlists::load_smart_folder_headers(&mut connection, &[key])
                        .await?
                        .pop()
                        .map(PinRow::PlaylistFolder)
                }
                SidebarPin::PlaylistFolder { smart: false, .. } => {
                    crate::playlist_folders::load_folder_headers(&mut connection, &[key])
                        .await?
                        .pop()
                        .map(PinRow::PlaylistFolder)
                }
            };
            if let Some(row) = row {
                rows.push((pin.clone(), row));
            }
        }
        Ok(rows)
    }

    /// Reads pinned smart playlists and playlist folders again as the playlist lists show them:
    /// with their definitions and totals, evaluated now.
    pub async fn complete_pin_rows(
        &self,
        rows: &mut [(SidebarPin, PinRow)],
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<()> {
        let mut smart = Vec::new();
        let mut smart_folders = Vec::new();
        let mut folders = Vec::new();
        for (_, row) in rows.iter() {
            match row {
                PinRow::SmartPlaylist(row) => smart.push(row.smart_playlist_key),
                PinRow::PlaylistFolder(row) if row.smart => {
                    smart_folders.push(row.folder_key.raw())
                }
                PinRow::PlaylistFolder(row) => folders.push(row.folder_key.raw()),
                _ => {}
            }
        }
        if smart.is_empty() && smart_folders.is_empty() && folders.is_empty() {
            return Ok(());
        }
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let mut smart =
            crate::smart_playlists::load_smart_playlist_rows(self, &mut connection, &smart, now)
                .await?
                .into_iter()
                .map(|row| (row.smart_playlist_key, row))
                .collect::<HashMap<_, _>>();
        let mut full_folders = HashMap::new();
        if !smart_folders.is_empty() {
            full_folders.extend(
                crate::smart_playlists::load_smart_folder_rows(
                    self,
                    &mut connection,
                    &smart_folders,
                    now,
                )
                .await?
                .into_iter()
                .map(|row| (row.folder_key, row)),
            );
        }
        if !folders.is_empty() {
            let read =
                crate::ListRead::new(crate::ListKind::Playlists, None, crate::CatalogSort::Title);
            full_folders.extend(
                crate::playlist_folders::load_folder_rows(self, &mut connection, &read, &folders)
                    .await?
                    .into_iter()
                    .map(|row| (row.folder_key, row)),
            );
        }
        for (_, row) in rows {
            match row {
                PinRow::SmartPlaylist(row) => {
                    if let Some(full) = smart.remove(&row.smart_playlist_key) {
                        *row = full;
                    }
                }
                PinRow::PlaylistFolder(row) => {
                    if let Some(full) = full_folders.remove(&row.folder_key) {
                        *row = full;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Totals of pinned smart playlists and playlist folders, which take an evaluation. Folders
    /// count the listed playlists of every source.
    pub async fn sidebar_pin_totals(
        &self,
        mut rows: Vec<SidebarPinRow>,
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SidebarPinRow>> {
        let keys = |pinned: fn(&SidebarPin) -> bool| {
            rows.iter()
                .filter(|row| pinned(&row.pin))
                .map(|row| row.key)
                .collect::<Vec<_>>()
        };
        let smart = keys(|pin| matches!(pin, SidebarPin::SmartPlaylist { .. }));
        let pinned_folders = keys(|pin| matches!(pin, SidebarPin::PlaylistFolder { .. }));
        if smart.is_empty() && pinned_folders.is_empty() {
            return Ok(rows);
        }
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let totals =
            crate::smart_playlists::smart_totals(self, &mut connection, &smart, now).await?;
        let mut folders = crate::smart_playlists::smart_folder_totals(
            self,
            &mut connection,
            &pinned_folders,
            now,
        )
        .await?;
        let snapshot = self.snapshot();
        let context = crate::catalog::views::Context::new(&snapshot);
        for (_, playlist) in snapshot.playlists.iter() {
            if pinned_folders.contains(&playlist.folder) && context.listed_playlist(playlist) {
                let total = context.playlist_totals(playlist, None);
                let folder = folders.entry(playlist.folder).or_default();
                folder.0 += i64::from(total.tracks);
                folder.1 += total.duration as i64;
            }
        }
        for row in &mut rows {
            row.totals = match row.pin {
                SidebarPin::SmartPlaylist { .. } => Some(
                    totals
                        .get(&row.key)
                        .map(|total| (i64::from(total.tracks), total.duration as i64))
                        .unwrap_or_default(),
                ),
                SidebarPin::PlaylistFolder { .. } => {
                    Some(folders.get(&row.key).copied().unwrap_or_default())
                }
                _ => row.totals,
            };
        }
        Ok(rows)
    }
}
