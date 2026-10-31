//! Sidebar pins: keys and totals from the resident Catalog, pictures from one statement.
use crate::{Catalog, Item, LibraryResult, PlaylistListRow, ReadCancellation, Row, SourceId};
use serde::{Deserialize, Serialize};

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

impl Catalog {
    /// The item of a pinned row, when the Catalog still holds it.
    fn pin_item(&self, pin: &SidebarPin) -> Option<Item> {
        let snapshot = self.snapshot();
        let find = |kind: crate::CatalogKind, source: Option<&SourceId>, object: &str| {
            let source = match source {
                Some(id) => snapshot.source_slot(
                    snapshot
                        .sources
                        .values()
                        .find(|source| source.object == id.as_str())?
                        .key,
                )?,
                None => crate::catalog::snapshot::NO_SOURCE,
            };
            snapshot.object_keys(kind, source, &[object])[0]
        };
        match pin {
            SidebarPin::Album {
                source_id,
                album_id,
            } => find(crate::CatalogKind::Album, Some(source_id), album_id)
                .map(|key| Item::Album(crate::AlbumKey::from_raw(key))),
            SidebarPin::Artist {
                source_id,
                artist_id,
                album_artist,
            } => find(crate::CatalogKind::Artist, Some(source_id), artist_id).map(|key| {
                Item::Artist {
                    key: crate::ArtistKey::from_raw(key),
                    album_artist: *album_artist,
                }
            }),
            SidebarPin::Genre {
                source_id,
                genre_id,
            } => find(crate::CatalogKind::Genre, Some(source_id), genre_id)
                .map(|key| Item::Genre(crate::GenreKey::from_raw(key))),
            SidebarPin::Playlist {
                source_id,
                playlist_id,
            } => find(
                crate::CatalogKind::Playlist,
                source_id.as_ref(),
                playlist_id,
            )
            .map(|key| Item::Playlist(crate::PlaylistKey::from_raw(key))),
            SidebarPin::SmartPlaylist { playlist_id } => snapshot
                .smart_playlists
                .values()
                .find(|smart| smart.object == *playlist_id)
                .map(|smart| Item::Smart(crate::SmartPlaylistKey::from_raw(smart.key))),
            SidebarPin::PlaylistFolder { folder_id, .. } => snapshot
                .playlist_folders
                .values()
                .find(|folder| folder.object == *folder_id)
                .map(|folder| Item::PlaylistFolder(crate::PlaylistFolderKey::from_raw(folder.key))),
        }
    }

    /// Rows of the pins the Catalog still holds, in pin order, with their totals: smart
    /// playlists and their folders evaluated at `now`, playlist folders over every source.
    pub async fn pin_rows(
        &self,
        pins: &[SidebarPin],
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(SidebarPin, Row)>> {
        tokio::select! {
            () = self.loaded() => {}
            () = cancellation.cancelled() => return Err(crate::LibraryError::ReadCancelled),
        }
        let pinned = pins
            .iter()
            .filter_map(|pin| Some((pin, self.pin_item(pin)?)))
            .collect::<Vec<_>>();
        let items = pinned.iter().map(|(_, item)| *item).collect::<Vec<_>>();
        let mut connection = self.acquire_cancellable(cancellation).await?;
        let shape = crate::read::Shape {
            now,
            ..crate::read::Shape::of(None)
        };
        let mut built = std::collections::HashMap::new();
        for row in self
            .rows_of_on(&mut connection, &items, &shape, true)
            .await?
        {
            let item = match &row {
                Row::Album(row) => Item::Album(row.album_key),
                Row::Artist(row) => Item::Artist {
                    key: row.artist_key,
                    album_artist: row.album_artist,
                },
                Row::Tag(row) => Item::Genre(crate::GenreKey::from_raw(row.key)),
                Row::Playlist(PlaylistListRow::Playlist(row)) => Item::Playlist(row.playlist_key),
                Row::Playlist(PlaylistListRow::SmartPlaylist(row)) => {
                    Item::Smart(row.smart_playlist_key)
                }
                Row::Playlist(PlaylistListRow::Folder(row)) => Item::PlaylistFolder(row.folder_key),
                _ => continue,
            };
            built.insert(item, row);
        }
        let rows = pinned
            .into_iter()
            .filter_map(|(pin, item)| Some((pin.clone(), built.remove(&item)?)))
            .collect();
        Ok(rows)
    }
}
