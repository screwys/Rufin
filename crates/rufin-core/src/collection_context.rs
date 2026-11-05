//! The context ids that plays and downloads of a whole collection carry. Queues and download
//! jobs save them; albums and artists go by media URI, the other collections by key.

use crate::route::Route;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CollectionTarget {
    /// An album by media URI.
    Album(String),
    /// An artist by media URI, read as an artist or as an album artist.
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

    /// The collection `collection` reads, naming albums and artists by the URI `catalog`
    /// holds for them. Sources, folder objects and history are no collection here.
    pub fn of(catalog: &library::Catalog, collection: &library::Collection) -> Option<Self> {
        Some(match *collection {
            library::Collection::Album(key) => Self::Album(catalog.uri(library::Item::Album(key))?),
            library::Collection::Artist { key, album_artist } => Self::Artist {
                id: catalog.uri(library::Item::Artist { key, album_artist })?,
                album_artist,
            },
            library::Collection::Genre(key) => Self::Genre(key),
            library::Collection::Mood(key) => Self::Mood(key),
            library::Collection::Playlist(key) => Self::Playlist(key),
            library::Collection::Smart { key, .. } => Self::SmartPlaylist(key),
            library::Collection::Source(_)
            | library::Collection::Objects { .. }
            | library::Collection::History { .. } => return None,
        })
    }

    /// What the collection is, as a queue names where it plays from.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Album(_) => localization::msgid("Album"),
            Self::Artist { .. } => localization::msgid("Artist"),
            Self::Genre(_) => localization::msgid("Genre"),
            Self::Mood(_) => localization::msgid("Mood"),
            Self::Playlist(_) | Self::SmartPlaylist(_) => localization::msgid("Playlist"),
        }
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
}

/// Whether what plays, in context `playing`, is the collection `context_id` names: the same id,
/// or that id decorated after `|` (a page's filter and order), or after `:` for a playlist.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn plays_context(context_id: &str, playing: &str) -> bool {
    let decoration = if context_id.starts_with("playlist:") {
        ':'
    } else {
        '|'
    };
    playing == context_id
        || playing
            .strip_prefix(context_id)
            .is_some_and(|suffix| suffix.starts_with(decoration))
}
