use library::{AlbumKey, ArtistKey, GenreKey, MoodKey, PlaylistKey, SmartPlaylistKey};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(crate) struct FolderPathItem {
    pub(crate) id: String,
    pub(crate) name: String,
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
