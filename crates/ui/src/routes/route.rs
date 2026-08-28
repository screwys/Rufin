use library::{AlbumKey, ArtistKey, GenreKey, MoodKey, PlaylistKey, SmartPlaylistKey};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(crate) struct FolderPathItem {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum CollectionCategory {
    #[default]
    Tracks,
    Albums,
    Artists,
}

impl CollectionCategory {
    pub(crate) const ALL: [Self; 3] = [Self::Tracks, Self::Albums, Self::Artists];

    pub(crate) const fn next(self) -> Self {
        match self {
            Self::Tracks => Self::Albums,
            Self::Albums => Self::Artists,
            Self::Artists => Self::Tracks,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(crate) enum Route {
    Home,
    Search,
    Favorites,
    History,
    Albums,
    AlbumDetail(AlbumKey),
    Tracks,
    Artists,
    ArtistDetail(ArtistKey),
    ArtistDiscography(ArtistKey),
    ArtistTracks(ArtistKey),
    ArtistFavoriteTracks(ArtistKey),
    AlbumArtists,
    AlbumArtistDetail(ArtistKey),
    AlbumArtistDiscography(ArtistKey),
    AlbumArtistTracks(ArtistKey),
    AlbumArtistFavoriteTracks(ArtistKey),
    Genres,
    GenreDetail(GenreKey),
    Moods,
    MoodDetail(MoodKey),
    Folders { path: Vec<FolderPathItem> },
    Playlists,
    PlaylistDetail(PlaylistKey),
    SmartPlaylists,
    SmartPlaylistDetail(SmartPlaylistKey),
}
