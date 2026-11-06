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

/// What a collection row shows of itself: an album, artist, tag, playlist, smart playlist or
/// playlist folder.
#[derive(Debug, PartialEq)]
pub struct CollectionShown<'a> {
    /// The row's key in its kind.
    pub key: i64,
    pub name: &'a str,
    pub track_count: i64,
    pub duration_millis: i64,
    /// Its picture, or the covers its mosaic picked.
    pub artwork: Vec<&'a [u8]>,
}

impl crate::AlbumRow {
    pub fn pin(&self) -> Option<SidebarPin> {
        let (source_id, _, album_id) = crate::source_entity_parts(&self.media_uri)?;
        Some(SidebarPin::Album {
            source_id,
            album_id,
        })
    }
}

impl crate::ArtistRow {
    pub fn pin(&self) -> Option<SidebarPin> {
        let (source_id, _, artist_id) = crate::source_entity_parts(&self.media_uri)?;
        Some(SidebarPin::Artist {
            source_id,
            artist_id,
            album_artist: self.album_artist,
        })
    }
}

impl crate::TagRow {
    /// A genre's pin; moods have none.
    pub fn pin(&self) -> Option<SidebarPin> {
        (self.kind == crate::TagKind::Genre).then(|| SidebarPin::Genre {
            source_id: SourceId::new(self.source_id.clone()),
            genre_id: self.object_id.clone(),
        })
    }
}

impl crate::PlaylistRow {
    pub fn pin(&self) -> SidebarPin {
        SidebarPin::Playlist {
            source_id: self.source_id.as_ref().map(SourceId::new),
            playlist_id: self.object_id.clone(),
        }
    }
}

impl crate::SmartPlaylistRow {
    pub fn pin(&self) -> SidebarPin {
        SidebarPin::SmartPlaylist {
            playlist_id: self.object_id.clone(),
        }
    }
}

impl crate::PlaylistFolderRow {
    pub fn pin(&self) -> SidebarPin {
        SidebarPin::PlaylistFolder {
            folder_id: self.object_id.clone(),
            smart: self.smart,
        }
    }
}

impl crate::CollectionSummary {
    /// Whether every track of the collection is downloaded; an empty one is not.
    pub fn downloaded(&self) -> bool {
        self.track_count > 0 && self.downloaded_count == self.track_count
    }
}

impl crate::PlaylistRow {
    /// Whether every track of the playlist is downloaded; an empty one is not.
    pub fn downloaded(&self) -> bool {
        self.track_count > 0 && self.downloaded_count == self.track_count
    }
}

impl crate::SmartPlaylistRow {
    /// Whether every track the smart playlist lists is downloaded; an empty one is not.
    pub fn downloaded(&self) -> bool {
        self.track_count > 0 && self.downloaded_count == self.track_count
    }
}

impl PlaylistListRow {
    /// Whether every track of the playlist is downloaded; a folder never is.
    pub fn downloaded(&self) -> bool {
        match self {
            Self::Playlist(playlist) => playlist.downloaded(),
            Self::SmartPlaylist(smart) => smart.downloaded(),
            Self::Folder(_) => false,
        }
    }

    pub fn pin(&self) -> SidebarPin {
        match self {
            Self::Playlist(playlist) => playlist.pin(),
            Self::SmartPlaylist(playlist) => playlist.pin(),
            Self::Folder(folder) => folder.pin(),
        }
    }
}

impl Row {
    /// Whether the row shows as downloaded: a track with its copy, or a collection with
    /// tracks that are all downloaded.
    pub fn downloaded(&self) -> bool {
        match self {
            Self::Track(track) | Self::AlbumTrack { track, .. } => track.is_downloaded,
            Self::Album(album) | Self::AlbumHeading(album) => album.summary.downloaded(),
            Self::Artist(artist) => artist.summary.downloaded(),
            Self::Tag(tag) => tag.summary.downloaded(),
            Self::Playlist(playlist) => playlist.downloaded(),
            Self::Folder(_) => false,
        }
    }

    /// The sidebar pin of a collection row.
    pub fn pin(&self) -> Option<SidebarPin> {
        match self {
            Self::Album(album) | Self::AlbumHeading(album) => album.pin(),
            Self::Artist(artist) => artist.pin(),
            Self::Tag(tag) => tag.pin(),
            Self::Playlist(playlist) => Some(playlist.pin()),
            Self::Track(_) | Self::AlbumTrack { .. } | Self::Folder(_) => None,
        }
    }

    /// What a collection row shows of itself; None for tracks and child folders.
    pub fn collection(&self) -> Option<CollectionShown<'_>> {
        fn picture(binding: &Option<Vec<u8>>) -> Vec<&[u8]> {
            binding.iter().map(Vec::as_slice).collect()
        }
        Some(match self {
            Self::Album(album) | Self::AlbumHeading(album) => CollectionShown {
                key: album.album_key.raw(),
                name: &album.title,
                track_count: album.summary.track_count,
                duration_millis: album.summary.duration_millis,
                artwork: picture(&album.artwork_binding),
            },
            Self::Artist(artist) => CollectionShown {
                key: artist.artist_key.raw(),
                name: &artist.name,
                track_count: artist.summary.track_count,
                duration_millis: artist.summary.duration_millis,
                artwork: picture(&artist.artwork_binding),
            },
            Self::Tag(tag) => CollectionShown {
                key: tag.key,
                name: &tag.name,
                track_count: tag.summary.track_count,
                duration_millis: tag.summary.duration_millis,
                artwork: match &tag.artwork_binding {
                    Some(binding) => vec![binding.as_slice()],
                    None => tag
                        .representative_artwork
                        .iter()
                        .map(Vec::as_slice)
                        .collect(),
                },
            },
            Self::Playlist(PlaylistListRow::Playlist(playlist)) => CollectionShown {
                key: playlist.playlist_key.raw(),
                name: &playlist.name,
                track_count: playlist.track_count,
                duration_millis: playlist.duration_millis,
                artwork: picture(&playlist.artwork_binding),
            },
            Self::Playlist(PlaylistListRow::SmartPlaylist(smart)) => CollectionShown {
                key: smart.smart_playlist_key.raw(),
                name: &smart.name,
                track_count: smart.track_count,
                duration_millis: smart.duration_millis,
                artwork: picture(&smart.artwork_binding),
            },
            Self::Playlist(PlaylistListRow::Folder(folder)) => CollectionShown {
                key: folder.folder_key.raw(),
                name: &folder.name,
                track_count: folder.track_count,
                duration_millis: folder.duration_millis,
                artwork: picture(&folder.artwork_binding),
            },
            Self::Track(_) | Self::AlbumTrack { .. } | Self::Folder(_) => return None,
        })
    }
}

impl Catalog {
    /// The items of pinned rows the Catalog still holds, in pin order.
    fn pin_items(&self, pins: &[SidebarPin]) -> Vec<Option<Item>> {
        use crate::CatalogKind as Kind;
        let snapshot = self.snapshot();
        let slot = |source: Option<&SourceId>| match source {
            Some(id) => snapshot.source_slot(
                snapshot
                    .sources
                    .values()
                    .find(|source| source.object == id.as_str())?
                    .key,
            ),
            None => Some(crate::catalog::snapshot::NO_SOURCE),
        };
        let wanted = pins.iter().map(|pin| {
            let (kind, source, object) = match pin {
                SidebarPin::Album {
                    source_id,
                    album_id,
                } => (Kind::Album, Some(source_id), album_id),
                SidebarPin::Artist {
                    source_id,
                    artist_id,
                    ..
                } => (Kind::Artist, Some(source_id), artist_id),
                SidebarPin::Genre {
                    source_id,
                    genre_id,
                } => (Kind::Genre, Some(source_id), genre_id),
                SidebarPin::Playlist {
                    source_id,
                    playlist_id,
                } => (Kind::Playlist, source_id.as_ref(), playlist_id),
                SidebarPin::SmartPlaylist { playlist_id } => {
                    (Kind::SmartPlaylist, None, playlist_id)
                }
                SidebarPin::PlaylistFolder { folder_id, .. } => {
                    (Kind::PlaylistFolder, None, folder_id)
                }
            };
            Some((kind, slot(source)?, object.as_str()))
        });
        let mut items = snapshot.items(wanted);
        for (pin, item) in pins.iter().zip(&mut items) {
            if let (
                SidebarPin::Artist { album_artist, .. },
                Some(Item::Artist {
                    album_artist: role, ..
                }),
            ) = (pin, item)
            {
                *role = *album_artist;
            }
        }
        items
    }

    /// Rows of the pins the Catalog still holds, in pin order, with their totals: smart
    /// playlists and their folders evaluated at `now` in music folder `folder`, playlist
    /// folders over every source.
    pub async fn pin_rows(
        &self,
        pins: &[SidebarPin],
        now: i64,
        folder: Option<crate::FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(SidebarPin, Row)>> {
        tokio::select! {
            () = self.loaded() => {}
            () = cancellation.cancelled() => return Err(crate::LibraryError::ReadCancelled),
        }
        let pinned = (pins.iter().zip(self.pin_items(pins)))
            .filter_map(|(pin, item)| Some((pin, item?)))
            .collect::<Vec<_>>();
        let items = pinned.iter().map(|(_, item)| *item).collect::<Vec<_>>();
        let mut connection = self.acquire_cancellable(cancellation).await?;
        let shape = crate::read::Shape {
            now,
            folder,
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
