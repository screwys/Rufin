//! The context ids that plays and downloads of a whole collection carry. Queues and download
//! jobs save them, so each spelling stays as callers have always written it: albums and artists
//! go by media URI in some places and by catalog key in others.

use crate::route::Route;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CollectionTarget {
    /// An album by media URI, or by album key where the caller names it that way.
    Album(String),
    /// An artist by media URI or artist key, read as an artist or as an album artist.
    Artist {
        id: String,
        album_artist: bool,
    },
    Genre(library::GenreKey),
    Mood(library::MoodKey),
    Playlist(library::PlaylistKey),
    SmartPlaylist(library::SmartPlaylistKey),
}

impl CollectionTarget {
    /// The collection a detail route shows, named by media URI or key.
    pub fn for_route(route: &Route) -> Option<Self> {
        Some(match route {
            Route::AlbumDetail(uri) => Self::Album(uri.clone()),
            Route::ArtistDetail(uri)
            | Route::ArtistTracks(uri)
            | Route::ArtistFavoriteTracks(uri)
            | Route::ArtistDiscography(uri) => Self::Artist {
                id: uri.clone(),
                album_artist: false,
            },
            Route::AlbumArtistDetail(uri)
            | Route::AlbumArtistTracks(uri)
            | Route::AlbumArtistFavoriteTracks(uri)
            | Route::AlbumArtistDiscography(uri) => Self::Artist {
                id: uri.clone(),
                album_artist: true,
            },
            Route::GenreDetail(key) => Self::Genre(*key),
            Route::MoodDetail(key) => Self::Mood(*key),
            Route::PlaylistDetail(key) => Self::Playlist(*key),
            Route::SmartPlaylistDetail(key) => Self::SmartPlaylist(*key),
            _ => return None,
        })
    }

    pub fn context_id(&self) -> String {
        match self {
            Self::Album(id) => format!("album:{id}"),
            Self::Artist {
                id,
                album_artist: false,
            } => format!("artist:{id}"),
            Self::Artist {
                id,
                album_artist: true,
            } => format!("album-artist:{id}"),
            Self::Genre(key) => format!("genre:{key}"),
            Self::Mood(key) => format!("mood:{key}"),
            Self::Playlist(key) => format!("playlist:{key}"),
            Self::SmartPlaylist(key) => format!("smart-playlist:{key}"),
        }
    }

    /// The whole collection a context id names. Ids decorated with a page's filter or order
    /// name part of a collection and read as nothing.
    pub fn parse(context_id: &str) -> Option<Self> {
        let (kind, id) = context_id.split_once(':')?;
        let key = || id.parse::<i64>().ok();
        Some(match kind {
            "album" => Self::Album(id.to_owned()),
            "artist" | "album-artist" => Self::Artist {
                id: id.to_owned(),
                album_artist: kind == "album-artist",
            },
            "genre" => Self::Genre(library::GenreKey::from_raw(key()?)),
            "mood" => Self::Mood(library::MoodKey::from_raw(key()?)),
            "playlist" => Self::Playlist(library::PlaylistKey::from_raw(key()?)),
            "smart-playlist" => Self::SmartPlaylist(library::SmartPlaylistKey::from_raw(key()?)),
            _ => return None,
        })
    }

    /// The collection a download event marks downloaded or not.
    pub fn downloaded(event: &downloads::DownloadEvent) -> Option<(Self, bool)> {
        let downloads::DownloadEvent::SubjectChanged {
            subject: downloads::DownloadSubject::Prepared { context_id, .. },
            downloaded,
        } = event
        else {
            return None;
        };
        Some((Self::parse(context_id)?, *downloaded))
    }
}
