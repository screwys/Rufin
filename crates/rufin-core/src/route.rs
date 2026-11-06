pub mod detail_links;

use library::{GenreKey, MoodKey, PlaylistKey, SmartPlaylistKey};
use localization::msgid;
use serde::{Deserialize, Serialize};

use crate::settings::SidebarRouteItem;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FolderPathItem {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum CollectionCategory {
    #[default]
    Tracks,
    Albums,
    Artists,
}

impl CollectionCategory {
    pub const ALL: [Self; 3] = [Self::Tracks, Self::Albums, Self::Artists];

    pub const fn next(self) -> Self {
        match self {
            Self::Tracks => Self::Albums,
            Self::Albums => Self::Artists,
            Self::Artists => Self::Tracks,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Route {
    Home,
    Search,
    Favorites,
    History,
    Albums,
    AlbumDetail(String),
    Tracks,
    Artists,
    ArtistDetail(String),
    ArtistDiscography(String),
    ArtistTracks(String),
    ArtistFavoriteTracks(String),
    AlbumArtists,
    AlbumArtistDetail(String),
    AlbumArtistDiscography(String),
    AlbumArtistTracks(String),
    AlbumArtistFavoriteTracks(String),
    Genres,
    GenreDetail(GenreKey),
    Moods,
    MoodDetail(MoodKey),
    Folders {
        path: Vec<FolderPathItem>,
    },
    Playlists,
    PlaylistDetail(PlaylistKey),
    SmartPlaylists,
    SmartPlaylistDetail(SmartPlaylistKey),
    PlaylistFolder {
        key: library::PlaylistFolderKey,
        smart: bool,
    },
}

impl Route {
    /// The page a collection row opens; None for tracks and child folders.
    pub fn of_row(row: &library::Row) -> Option<Self> {
        use library::{PlaylistListRow, Row, TagKind};
        Some(match row {
            Row::Album(album) | Row::AlbumHeading(album) => Self::of_album(album),
            Row::Artist(artist) => Self::of_artist(artist),
            Row::Tag(tag) if tag.kind == TagKind::Genre => {
                Self::GenreDetail(GenreKey::from_raw(tag.key))
            }
            Row::Tag(tag) => Self::MoodDetail(MoodKey::from_raw(tag.key)),
            Row::Playlist(PlaylistListRow::Playlist(playlist)) => {
                Self::PlaylistDetail(playlist.playlist_key)
            }
            Row::Playlist(PlaylistListRow::SmartPlaylist(smart)) => {
                Self::SmartPlaylistDetail(smart.smart_playlist_key)
            }
            Row::Playlist(PlaylistListRow::Folder(folder)) => Self::PlaylistFolder {
                key: folder.folder_key,
                smart: folder.smart,
            },
            Row::Track(_) | Row::AlbumTrack { .. } | Row::Folder(_) => return None,
        })
    }

    /// The page an album row opens.
    pub fn of_album(album: &library::AlbumRow) -> Self {
        Self::AlbumDetail(album.media_uri.clone())
    }

    /// The page an artist row opens, in the role it was read with.
    pub fn of_artist(artist: &library::ArtistRow) -> Self {
        if artist.album_artist {
            Self::AlbumArtistDetail(artist.media_uri.clone())
        } else {
            Self::ArtistDetail(artist.media_uri.clone())
        }
    }

    /// Whether a page lists the selected source and shows nothing without one; a lyrics search
    /// reads every source, and in a selected source its music folder.
    pub fn requires_selected_source(&self, search_mode: library::SearchMode) -> bool {
        match self {
            Self::Search => search_mode == library::SearchMode::Title,
            _ => matches!(
                self,
                Self::Home
                    | Self::Favorites
                    | Self::Albums
                    | Self::Tracks
                    | Self::Artists
                    | Self::AlbumArtists
                    | Self::Genres
                    | Self::Moods
                    | Self::Folders { .. }
            ),
        }
    }
}

/// Whether a page on `route` shows nothing without a selected source, as
/// [`Route::requires_selected_source`] decides.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn route_requires_selected_source(route: &Route, search_mode: library::SearchMode) -> bool {
    route.requires_selected_source(search_mode)
}

pub struct SidebarRouteDescriptor {
    pub stable_id: &'static str,
    pub title: &'static str,
    pub icon_name: &'static str,
    selected_icon_name: Option<&'static str>,
    pub root_route: Route,
}

impl SidebarRouteDescriptor {
    fn new(
        stable_id: &'static str,
        title: &'static str,
        icon_name: &'static str,
        selected_icon_name: Option<&'static str>,
        root_route: Route,
    ) -> Self {
        Self {
            stable_id,
            title,
            icon_name,
            selected_icon_name,
            root_route,
        }
    }

    pub fn selected_icon_name(&self) -> &'static str {
        self.selected_icon_name.unwrap_or(self.icon_name)
    }
}

#[rustfmt::skip]
    pub fn sidebar_route_descriptor(item: SidebarRouteItem) -> SidebarRouteDescriptor {
        match item {
            SidebarRouteItem::Home => SidebarRouteDescriptor::new("Home", msgid("Home"), "rufin-home-symbolic", None, Route::Home),
            SidebarRouteItem::Search => SidebarRouteDescriptor::new("Search", msgid("Search"), "rufin-search-symbolic", None, Route::Search),
            SidebarRouteItem::Favorites => SidebarRouteDescriptor::new("Favorites", msgid("Favorites"), "rufin-heart-outline-symbolic", Some("rufin-heart-filled-symbolic"), Route::Favorites),
            SidebarRouteItem::Albums => SidebarRouteDescriptor::new("Albums", msgid("Albums"), "rufin-albums-symbolic", None, Route::Albums),
            SidebarRouteItem::Tracks => SidebarRouteDescriptor::new("Tracks", msgid("Tracks"), "rufin-tracks-symbolic", None, Route::Tracks),
            SidebarRouteItem::Artists => SidebarRouteDescriptor::new("Artists", msgid("Artists"), "rufin-artists-symbolic", None, Route::Artists),
            SidebarRouteItem::AlbumArtists => SidebarRouteDescriptor::new("AlbumArtists", msgid("Album Artists"), "rufin-album-artists-symbolic", None, Route::AlbumArtists),
            SidebarRouteItem::Genres => SidebarRouteDescriptor::new("Genres", msgid("Genres"), "rufin-tag-outline-symbolic", None, Route::Genres),
            SidebarRouteItem::Moods => SidebarRouteDescriptor::new("Moods", msgid("Moods"), "rufin-moods-symbolic", None, Route::Moods),
            SidebarRouteItem::History => SidebarRouteDescriptor::new("History", msgid("History"), "rufin-history-symbolic", None, Route::History),
            SidebarRouteItem::Folders => SidebarRouteDescriptor::new("Folders", msgid("Folders"), "rufin-folders-symbolic", Some("rufin-folders-selected-symbolic"), Route::Folders { path: Vec::new() }),
            SidebarRouteItem::Playlists => SidebarRouteDescriptor::new("Playlists", msgid("Playlists"), "rufin-playlists-symbolic", None, Route::Playlists),
            SidebarRouteItem::SmartPlaylists => SidebarRouteDescriptor::new("SmartPlaylists", msgid("Smart Playlists"), "rufin-smart-playlists-symbolic", None, Route::SmartPlaylists),
        }
    }

use crate::settings::layout::{LibraryField, LibraryListKey};
impl CollectionCategory {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tracks => "tracks",
            Self::Albums => "albums",
            Self::Artists => "artists",
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name {
            "albums" => Self::Albums,
            "artists" => Self::Artists,
            _ => Self::Tracks,
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Tracks => msgid("Tracks"),
            Self::Albums => msgid("Albums"),
            Self::Artists => msgid("Artists"),
        }
    }

    pub const fn icon_name(self) -> &'static str {
        match self {
            Self::Tracks => "rufin-tracks-symbolic",
            Self::Albums => "rufin-albums-symbolic",
            Self::Artists => "rufin-artists-symbolic",
        }
    }

    pub const fn key(self) -> LibraryListKey {
        match self {
            Self::Tracks => LibraryListKey::Tracks,
            Self::Albums => LibraryListKey::Albums,
            Self::Artists => LibraryListKey::Artists,
        }
    }

    pub fn sort_fields(self) -> &'static [LibraryField] {
        match self {
            Self::Tracks => crate::settings::available_sort_fields(LibraryListKey::Tracks),
            Self::Albums => &[
                LibraryField::Title,
                LibraryField::AlbumArtist,
                LibraryField::Year,
                LibraryField::ReleaseDate,
                LibraryField::DateAdded,
                LibraryField::LastPlayed,
                LibraryField::PlayCount,
                LibraryField::UserRating,
                LibraryField::Favorite,
            ],
            Self::Artists => &[
                LibraryField::Title,
                LibraryField::LastPlayed,
                LibraryField::PlayCount,
                LibraryField::UserRating,
                LibraryField::Favorite,
            ],
        }
    }
}
