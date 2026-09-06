use library::{GenreKey, MoodKey, PlaylistKey, SmartPlaylistKey};
use localization::msgid;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use crate::SidebarRouteItem;

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

pub(crate) struct SidebarRouteDescriptor {
    pub(crate) stable_id: &'static str,
    pub(crate) title: &'static str,
    pub(crate) icon_name: &'static str,
    selected_icon_name: Option<&'static str>,
    pub(crate) css_class: &'static str,
    pub(crate) root_route: Route,
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

    pub(crate) fn selected_icon_name(&self) -> &'static str {
        self.selected_icon_name.unwrap_or(self.icon_name)
    }
}

impl SidebarRouteItem {
    #[rustfmt::skip]
    pub(crate) fn descriptor(self) -> SidebarRouteDescriptor {
        match self {
            Self::Home => SidebarRouteDescriptor::new("Home", msgid("Home"), "rufin-home-symbolic", None, "nav-route-home", Route::Home),
            Self::Search => SidebarRouteDescriptor::new("Search", msgid("Search"), "rufin-search-symbolic", None, "nav-route-search", Route::Search),
            Self::Favorites => SidebarRouteDescriptor::new("Favorites", msgid("Favorites"), "rufin-heart-outline-symbolic", Some("rufin-heart-filled-symbolic"), "nav-route-favorites", Route::Favorites),
            Self::Albums => SidebarRouteDescriptor::new("Albums", msgid("Albums"), "rufin-albums-symbolic", None, "nav-route-albums", Route::Albums),
            Self::Tracks => SidebarRouteDescriptor::new("Tracks", msgid("Tracks"), "rufin-tracks-symbolic", None, "nav-route-tracks", Route::Tracks),
            Self::Artists => SidebarRouteDescriptor::new("Artists", msgid("Artists"), "rufin-artists-symbolic", None, "nav-route-artists", Route::Artists),
            Self::AlbumArtists => SidebarRouteDescriptor::new("AlbumArtists", msgid("Album Artists"), "rufin-album-artists-symbolic", None, "nav-route-album-artists", Route::AlbumArtists),
            Self::Genres => SidebarRouteDescriptor::new("Genres", msgid("Genres"), "rufin-tag-outline-symbolic", None, "nav-route-genres", Route::Genres),
            Self::Moods => SidebarRouteDescriptor::new("Moods", msgid("Moods"), "rufin-moods-symbolic", None, "nav-route-moods", Route::Moods),
            Self::History => SidebarRouteDescriptor::new("History", msgid("History"), "rufin-history-symbolic", None, "nav-route-history", Route::History),
            Self::Folders => SidebarRouteDescriptor::new("Folders", msgid("Folders"), "rufin-folders-symbolic", Some("rufin-folders-selected-symbolic"), "nav-route-folders", Route::Folders { path: Vec::new() }),
            Self::Playlists => SidebarRouteDescriptor::new("Playlists", msgid("Playlists"), "rufin-playlists-symbolic", None, "nav-route-playlists", Route::Playlists),
            Self::SmartPlaylists => SidebarRouteDescriptor::new("SmartPlaylists", msgid("Smart Playlists"), "rufin-smart-playlists-symbolic", None, "nav-route-smart-playlists", Route::SmartPlaylists),
        }
    }

    pub(crate) fn from_stable_id(stable_id: &str) -> Option<Self> {
        Self::all()
            .into_iter()
            .find(|item| item.descriptor().stable_id == stable_id)
    }
}

impl Serialize for SidebarRouteItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.descriptor().stable_id)
    }
}

impl<'de> Deserialize<'de> for SidebarRouteItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let stable_id = String::deserialize(deserializer)?;
        Self::from_stable_id(&stable_id)
            .ok_or_else(|| D::Error::custom(format!("unknown sidebar route item `{stable_id}`")))
    }
}
