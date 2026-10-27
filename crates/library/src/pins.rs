//! Sidebar pins: rows read by key, totals from the resident Catalog; smart totals are evaluated
//! separately from row loading.
use crate::{
    AlbumView, ArtistView, Catalog, GenreView, LibraryResult, PlaylistFolderRow, PlaylistRow,
    ReadCancellation, SmartPlaylistRow, SourceId,
};
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, FromRow};
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
    /// come without totals, which take an evaluation.
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
                SidebarPin::SmartPlaylist { .. } => {
                    crate::smart_playlists::load_smart_playlist_headers(
                        &mut connection,
                        &[crate::SmartPlaylistKey::from_raw(key)],
                    )
                    .await?
                    .pop()
                    .map(PinRow::SmartPlaylist)
                }
                SidebarPin::PlaylistFolder { .. } => {
                    #[derive(FromRow)]
                    struct Folder {
                        #[sqlx(flatten)]
                        row: PlaylistFolderRow,
                        cover_id: Option<String>,
                        cover_source: Option<String>,
                        mosaic: Option<String>,
                    }
                    let mut folder: Folder = sqlx::query_as(AssertSqlSafe(format!(
                        "SELECT item.folder_key,item.object_id,item.name,item.smart,{} artwork_binding,0 track_count,0 duration_millis,coalesce(smart.object_id,regular.object_id) cover_id,source.object_id cover_source,CASE WHEN item.smart THEN smart.mosaic ELSE regular.mosaic END mosaic
                        FROM playlist_folders item
                        LEFT JOIN playlists regular ON NOT item.smart AND regular.playlist_key=(SELECT min(child.playlist_key) FROM playlists child WHERE child.playlist_folder_key=item.folder_key AND child.name IS NOT NULL AND (child.owner='rufin' OR child.indexed))
                        LEFT JOIN smart_playlists smart ON item.smart AND smart.smart_playlist_key=(SELECT min(child.smart_playlist_key) FROM smart_playlists child WHERE child.playlist_folder_key=item.folder_key)
                        LEFT JOIN sources source ON source.source_key=regular.source_key
                        WHERE item.folder_key=?1",
                        crate::artwork::preview_sql("CASE WHEN item.smart THEN smart.artwork_binding ELSE regular.artwork_binding END")
                    )))
                    .bind(key)
                    .fetch_one(&mut *connection)
                    .await?;
                    folder.row.artwork_binding = match folder.cover_id {
                        Some(id) => crate::artwork::playlist_artwork(
                            folder.row.artwork_binding.take(),
                            (folder.row.smart, folder.cover_source, id),
                            folder.mosaic.as_deref(),
                        )?,
                        None => None,
                    };
                    Some(PinRow::PlaylistFolder(folder.row))
                }
            };
            if let Some(row) = row {
                rows.push((pin.clone(), row));
            }
        }
        Ok(rows)
    }

    /// Totals of pinned smart playlists and playlist folders, evaluated now. Folders count what
    /// the playlist list counts for them.
    pub async fn pin_totals(
        &self,
        rows: &mut [(SidebarPin, PinRow)],
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<()> {
        let mut smart = Vec::new();
        let mut smart_folders = Vec::new();
        let mut regular_folders = false;
        for (_, row) in rows.iter() {
            match row {
                PinRow::SmartPlaylist(row) => smart.push(row.smart_playlist_key.raw()),
                PinRow::PlaylistFolder(row) if row.smart => {
                    smart_folders.push(row.folder_key.raw())
                }
                PinRow::PlaylistFolder(_) => regular_folders = true,
                _ => {}
            }
        }
        if smart.is_empty() && smart_folders.is_empty() && !regular_folders {
            return Ok(());
        }
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let totals =
            crate::smart_playlists::smart_totals(self, &mut connection, &smart, now).await?;
        let smart_folders =
            crate::smart_playlists::smart_folder_totals(self, &mut connection, &smart_folders, now)
                .await?;
        let folders = if regular_folders {
            crate::playlist_folders::folder_totals(
                self,
                &crate::ListRead::new(crate::ListKind::Playlists, None, crate::CatalogSort::Title),
            )
            .await
        } else {
            HashMap::new()
        };
        for (_, row) in rows {
            match row {
                PinRow::SmartPlaylist(row) => {
                    crate::smart_playlists::set_smart_totals(row, &totals)
                }
                PinRow::PlaylistFolder(row) if row.smart => {
                    (row.track_count, row.duration_millis) = smart_folders
                        .get(&row.folder_key.raw())
                        .copied()
                        .unwrap_or_default();
                }
                PinRow::PlaylistFolder(row) => {
                    if let Some(totals) = folders.get(&row.folder_key.raw()) {
                        row.track_count = i64::from(totals.tracks);
                        row.duration_millis = totals.duration as i64;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Totals of pinned smart playlists and playlist folders, which take an evaluation.
    pub async fn sidebar_pin_totals(
        &self,
        mut rows: Vec<SidebarPinRow>,
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SidebarPinRow>> {
        if !rows.iter().any(|row| {
            matches!(
                row.pin,
                SidebarPin::SmartPlaylist { .. } | SidebarPin::PlaylistFolder { .. }
            )
        }) {
            return Ok(rows);
        }
        self.loaded().await;
        let smart = self
            .snapshot()
            .smart_playlists
            .values()
            .map(|smart| (smart.key, smart.folder))
            .collect::<Vec<_>>();
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let totals = crate::smart_playlists::smart_totals(
            self,
            &mut connection,
            &smart.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            now,
        )
        .await?;
        let snapshot = self.snapshot();
        let context = crate::catalog::views::Context::new(&snapshot, false);
        let mut folders = HashMap::<i64, (i64, i64)>::new();
        for (_, playlist) in snapshot.playlists.iter() {
            if playlist.folder != 0 && context.listed_playlist(playlist) {
                let total = context.playlist_totals(playlist, None);
                let folder = folders.entry(playlist.folder).or_default();
                folder.0 += i64::from(total.tracks);
                folder.1 += total.duration as i64;
            }
        }
        for (key, folder) in &smart {
            if let (Some(folder), Some(total)) = (folder, totals.get(key)) {
                let folder = folders.entry(*folder).or_default();
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
