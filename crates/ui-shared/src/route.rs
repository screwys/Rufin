use library::{GenreKey, MoodKey, PlaylistKey, SmartPlaylistKey};
use localization::msgid;
use serde::{Deserialize, Serialize};

use rufin_core::settings::layout::SidebarRouteItem;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct FolderPathItem {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
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
    Folders { path: Vec<FolderPathItem> },
    Playlists,
    PlaylistDetail(PlaylistKey),
    SmartPlaylists,
    SmartPlaylistDetail(SmartPlaylistKey),
}

pub struct SidebarRouteDescriptor {
    pub stable_id: &'static str,
    pub title: &'static str,
    pub icon_name: &'static str,
    selected_icon_name: Option<&'static str>,
    pub css_class: &'static str,
    pub root_route: Route,
}

impl SidebarRouteDescriptor {
    fn new(
        stable_id: &'static str,
        title: &'static str,
        icon_name: &'static str,
        selected_icon_name: Option<&'static str>,
        css_class: &'static str,
        root_route: Route,
    ) -> Self {
        Self {
            stable_id,
            title,
            icon_name,
            selected_icon_name,
            css_class,
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
            SidebarRouteItem::Home => SidebarRouteDescriptor::new("Home", msgid("Home"), "rufin-home-symbolic", None, "nav-route-home", Route::Home),
            SidebarRouteItem::Search => SidebarRouteDescriptor::new("Search", msgid("Search"), "rufin-search-symbolic", None, "nav-route-search", Route::Search),
            SidebarRouteItem::Favorites => SidebarRouteDescriptor::new("Favorites", msgid("Favorites"), "rufin-heart-outline-symbolic", Some("rufin-heart-filled-symbolic"), "nav-route-favorites", Route::Favorites),
            SidebarRouteItem::Albums => SidebarRouteDescriptor::new("Albums", msgid("Albums"), "rufin-albums-symbolic", None, "nav-route-albums", Route::Albums),
            SidebarRouteItem::Tracks => SidebarRouteDescriptor::new("Tracks", msgid("Tracks"), "rufin-tracks-symbolic", None, "nav-route-tracks", Route::Tracks),
            SidebarRouteItem::Artists => SidebarRouteDescriptor::new("Artists", msgid("Artists"), "rufin-artists-symbolic", None, "nav-route-artists", Route::Artists),
            SidebarRouteItem::AlbumArtists => SidebarRouteDescriptor::new("AlbumArtists", msgid("Album Artists"), "rufin-album-artists-symbolic", None, "nav-route-album-artists", Route::AlbumArtists),
            SidebarRouteItem::Genres => SidebarRouteDescriptor::new("Genres", msgid("Genres"), "rufin-tag-outline-symbolic", None, "nav-route-genres", Route::Genres),
            SidebarRouteItem::Moods => SidebarRouteDescriptor::new("Moods", msgid("Moods"), "rufin-moods-symbolic", None, "nav-route-moods", Route::Moods),
            SidebarRouteItem::History => SidebarRouteDescriptor::new("History", msgid("History"), "rufin-history-symbolic", None, "nav-route-history", Route::History),
            SidebarRouteItem::Folders => SidebarRouteDescriptor::new("Folders", msgid("Folders"), "rufin-folders-symbolic", Some("rufin-folders-selected-symbolic"), "nav-route-folders", Route::Folders { path: Vec::new() }),
            SidebarRouteItem::Playlists => SidebarRouteDescriptor::new("Playlists", msgid("Playlists"), "rufin-playlists-symbolic", None, "nav-route-playlists", Route::Playlists),
            SidebarRouteItem::SmartPlaylists => SidebarRouteDescriptor::new("SmartPlaylists", msgid("Smart Playlists"), "rufin-smart-playlists-symbolic", None, "nav-route-smart-playlists", Route::SmartPlaylists),
        }
    }

use rufin_core::settings::layout::{LibraryField, LibraryListKey};
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
            Self::Tracks => {
                rufin_core::settings::sidebar::available_sort_fields(LibraryListKey::Tracks)
            }
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
