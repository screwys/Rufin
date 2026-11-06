//! The read vocabulary: which members or which list a client asks for.
use crate::*;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Collection {
    Source(SourceKey),
    Objects {
        source: SourceKey,
        ids: Arc<[String]>,
    },
    Album(AlbumKey),
    Artist {
        key: ArtistKey,
        album_artist: bool,
    },
    Genre(GenreKey),
    Mood(MoodKey),
    Playlist(PlaylistKey),
    Smart {
        key: SmartPlaylistKey,
        now: i64,
    },
    History {
        source: Option<SourceId>,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemberRead {
    pub collection: Collection,
    pub sort: Option<LibraryField>,
    pub folder: Option<FolderKey>,
    pub filter: String,
    pub descending: bool,
    pub favorites_only: bool,
    pub downloaded_only: bool,
    pub activity: bool,
    /// Keeps only the tracks on the albums this list read shows, as an artist page's header
    /// plays the releases it shows.
    pub within: Option<Box<ListRead>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ListKind {
    Albums,
    ArtistAlbums {
        artist: ArtistKey,
        album_artist: bool,
        release_class: Option<AlbumReleaseClass>,
        filters: Option<Arc<[String; 5]>>,
    },
    Artists {
        album_artist: bool,
    },
    Genres,
    Moods,
    Playlists,
    SmartPlaylists {
        now: i64,
    },
}

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub enum PlaylistScope {
    #[default]
    All,
    Root,
    Folder {
        key: PlaylistFolderKey,
    },
    Folders,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListRead {
    #[serde(default)]
    pub playlist_scope: PlaylistScope,
    pub kind: ListKind,
    pub source: Option<SourceKey>,
    pub folder: Option<FolderKey>,
    pub filter: String,
    pub sort: LibraryField,
    pub descending: bool,
    pub favorites_only: bool,
    pub activity: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CatalogKind {
    Track,
    Album,
    Artist,
    Genre,
    Mood,
    Folder,
    Playlist,
    SmartPlaylist,
    PlaylistFolder,
}

impl Catalog {
    /// Keys of a source's rows of one kind by object id, in the order asked; ids it does not
    /// hold are skipped. Collection object ids are resident; track ones are read.
    pub async fn catalog_keys(
        &self,
        kind: CatalogKind,
        source: Option<SourceKey>,
        objects: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<i64>> {
        if kind == CatalogKind::Track {
            let mut connection = self.acquire_cancellable(cancellation).await?;
            return self
                .track_keys_by_objects(&mut connection, source, objects)
                .await;
        }
        tokio::select! {
            () = self.loaded() => {}
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        }
        let snapshot = self.snapshot();
        let source = match source {
            Some(source) => match snapshot.source_slot(source) {
                Some(slot) => slot,
                None => return Ok(Vec::new()),
            },
            None => crate::catalog::snapshot::NO_SOURCE,
        };
        let objects = objects.iter().map(String::as_str).collect::<Vec<_>>();
        Ok(snapshot
            .object_keys(kind, source, &objects)
            .into_iter()
            .flatten()
            .collect())
    }
}

/// The order an album, artist, genre or mood plays in whole: album order.
pub(crate) const ALBUM_ORDER: LibraryField = LibraryField::TrackNumber;

impl MemberRead {
    pub fn new(collection: Collection) -> Self {
        Self {
            collection,
            sort: None,
            folder: None,
            filter: String::new(),
            descending: false,
            favorites_only: false,
            downloaded_only: false,
            activity: false,
            within: None,
        }
    }
    /// The sort it orders by: its own, else its collection's. Albums, artists, genres and
    /// moods play in album order. A field no list sorts by orders by title.
    pub fn effective_sort(&self) -> LibraryField {
        self.sort
            .unwrap_or(match self.collection {
                Collection::Album(_)
                | Collection::Artist { .. }
                | Collection::Genre(_)
                | Collection::Mood(_) => ALBUM_ORDER,
                Collection::Playlist(_) => LibraryField::RowIndex,
                Collection::History { .. } => LibraryField::LastPlayed,
                _ => LibraryField::Title,
            })
            .sort()
    }
    pub fn selection(self) -> MemberSelection {
        MemberSelection::Members {
            read: self,
            ranges: None,
        }
    }
}

impl ListRead {
    pub fn new(kind: ListKind, source: Option<SourceKey>, sort: LibraryField) -> Self {
        Self {
            kind,
            source,
            playlist_scope: PlaylistScope::All,
            sort,
            folder: None,
            filter: String::new(),
            descending: false,
            favorites_only: false,
            activity: false,
        }
    }

    pub fn selection(self) -> MemberSelection {
        MemberSelection::Parents {
            read: self,
            ranges: None,
        }
    }
}

pub(crate) fn selection_ranges(
    ranges: Option<&[std::ops::Range<usize>]>,
) -> Vec<std::ops::Range<usize>> {
    let mut ranges = ranges
        .map(<[_]>::to_vec)
        .unwrap_or_else(|| std::iter::once(0..usize::MAX).collect());
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
    for range in ranges {
        if range.is_empty() {
            continue;
        }
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
