use std::collections::HashSet;

use localization::msgid;
use serde::{Deserialize, Deserializer, Serialize};
use sources::SourceId;

use super::sidebar::{
    available_detail_track_fields, available_sort_fields, default_descending,
    default_detail_track_fields, default_grid_fields, default_row_fields, default_sort_key,
    ensure_usable_row_field, sanitize_optional_fields, sanitize_required_fields,
};
pub const LIBRARY_LIST_LAYOUT_VERSION: u8 = 9;
pub const DEFAULT_WINDOW_WIDTH: i32 = 1_500;
pub const DEFAULT_WINDOW_HEIGHT: i32 = 900;
pub const MIN_RESTORED_WINDOW_WIDTH: i32 = 450;
pub const MIN_RESTORED_WINDOW_HEIGHT: i32 = 400;
pub const MAX_RESTORED_WINDOW_WIDTH: i32 = 3_400;
pub const MAX_RESTORED_WINDOW_HEIGHT: i32 = 2_000;
pub(super) fn default_true() -> bool {
    true
}
fn default_narrow_layout_enabled() -> bool {
    true
}
fn default_narrow_layout_threshold() -> i32 {
    1_300
}
pub const MIN_NARROW_LAYOUT_THRESHOLD: i32 = 700;
pub const MAX_NARROW_LAYOUT_THRESHOLD: i32 = 3_400;
pub const DEFAULT_LEFT_SIDEBAR_WIDTH: i32 = 230;
pub const MIN_LEFT_SIDEBAR_WIDTH: i32 = 210;
pub const MAX_LEFT_SIDEBAR_WIDTH: i32 = 400;
pub const DEFAULT_RIGHT_SIDEBAR_WIDTH: i32 = 300;
pub const MIN_RIGHT_SIDEBAR_WIDTH: i32 = 250;
pub const MAX_RIGHT_SIDEBAR_WIDTH: i32 = 500;
pub const MIN_TABLE_COLUMN_WIDTH: i32 = 24;
pub const MAX_TABLE_COLUMN_WIDTH: i32 = 4_096;
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FolderViewSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_width: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_column_width: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_column_width: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_column_width: Option<i32>,
}
impl FolderViewSettings {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn sanitize(&mut self) {
        for width in [
            &mut self.name_column_width,
            &mut self.detail_column_width,
            &mut self.duration_column_width,
        ] {
            if let Some(value) = width {
                *value = (*value).clamp(MIN_TABLE_COLUMN_WIDTH, MAX_TABLE_COLUMN_WIDTH);
            }
        }
        if let Some(width) = &mut self.tree_width {
            *width = (*width).clamp(1, MAX_TABLE_COLUMN_WIDTH);
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub enum LeftSidebarMode {
    #[default]
    Full,
    Compact,
    Hidden,
}
impl<'de> Deserialize<'de> for LeftSidebarMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "Full" => Self::Full,
            "Compact" => Self::Compact,
            "Hidden" => Self::Hidden,
            _ => Self::default(),
        })
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum RightSidebarMode {
    Hidden,
    #[default]
    Visible,
}
impl RightSidebarMode {
    pub fn is_visible(self) -> bool {
        !matches!(self, Self::Hidden)
    }
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LayoutProfile {
    #[serde(default)]
    pub left_sidebar: LeftSidebarMode,
    #[serde(default)]
    pub right_sidebar: RightSidebarMode,
}
impl LayoutProfile {
    pub fn new(left_sidebar: LeftSidebarMode, right_sidebar: RightSidebarMode) -> Self {
        Self {
            left_sidebar,
            right_sidebar,
        }
    }
}
impl Default for LayoutProfile {
    fn default() -> Self {
        Self::new(LeftSidebarMode::Full, RightSidebarMode::Visible)
    }
}
fn default_narrow_layout_profile() -> LayoutProfile {
    LayoutProfile::new(LeftSidebarMode::Compact, RightSidebarMode::Visible)
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LayoutSettings {
    #[serde(default)]
    pub default_profile: LayoutProfile,
    #[serde(default = "default_narrow_layout_enabled")]
    pub narrow_enabled: bool,
    #[serde(default = "default_narrow_layout_threshold")]
    pub narrow_threshold: i32,
    #[serde(default = "default_narrow_layout_profile")]
    pub narrow_profile: LayoutProfile,
    pub preferred_left_sidebar_width: i32,
    pub preferred_right_sidebar_width: i32,
}
impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            default_profile: LayoutProfile::default(),
            narrow_enabled: true,
            narrow_threshold: default_narrow_layout_threshold(),
            narrow_profile: default_narrow_layout_profile(),
            preferred_left_sidebar_width: DEFAULT_LEFT_SIDEBAR_WIDTH,
            preferred_right_sidebar_width: DEFAULT_RIGHT_SIDEBAR_WIDTH,
        }
    }
}
impl LayoutSettings {
    pub fn sanitize(&mut self) {
        self.narrow_threshold = self
            .narrow_threshold
            .clamp(MIN_NARROW_LAYOUT_THRESHOLD, MAX_NARROW_LAYOUT_THRESHOLD);
        self.preferred_left_sidebar_width = self
            .preferred_left_sidebar_width
            .clamp(MIN_LEFT_SIDEBAR_WIDTH, MAX_LEFT_SIDEBAR_WIDTH);
        self.preferred_right_sidebar_width = self
            .preferred_right_sidebar_width
            .clamp(MIN_RIGHT_SIDEBAR_WIDTH, MAX_RIGHT_SIDEBAR_WIDTH);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum StoredRightSidebarMode {
    Hidden,
    Compact,
    #[default]
    Default,
    Comfortable,
    Spacious,
}

impl StoredRightSidebarMode {
    fn presentation(self) -> RightSidebarMode {
        if matches!(self, Self::Hidden) {
            RightSidebarMode::Hidden
        } else {
            RightSidebarMode::Visible
        }
    }

    fn preferred_width(self) -> Option<i32> {
        match self {
            Self::Hidden => None,
            Self::Compact => Some(250),
            Self::Default => Some(300),
            Self::Comfortable => Some(400),
            Self::Spacious => Some(500),
        }
    }
}

impl<'de> Deserialize<'de> for StoredRightSidebarMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "Hidden" => Self::Hidden,
            "Compact" => Self::Compact,
            "Comfortable" => Self::Comfortable,
            "Spacious" => Self::Spacious,
            "Visible" | "Shown" | "Default" => Self::Default,
            _ => Self::default(),
        })
    }
}

#[derive(Deserialize)]
struct StoredLayoutProfile {
    #[serde(default)]
    left_sidebar: LeftSidebarMode,
    #[serde(default)]
    right_sidebar: StoredRightSidebarMode,
    #[serde(default)]
    last_visible_right_sidebar: StoredRightSidebarMode,
}

impl Default for StoredLayoutProfile {
    fn default() -> Self {
        Self {
            left_sidebar: LeftSidebarMode::Full,
            right_sidebar: StoredRightSidebarMode::Default,
            last_visible_right_sidebar: StoredRightSidebarMode::Default,
        }
    }
}

fn default_stored_narrow_layout_profile() -> StoredLayoutProfile {
    StoredLayoutProfile {
        left_sidebar: LeftSidebarMode::Compact,
        ..StoredLayoutProfile::default()
    }
}

#[derive(Deserialize)]
struct StoredLayoutSettings {
    #[serde(default)]
    default_profile: StoredLayoutProfile,
    #[serde(default = "default_narrow_layout_enabled")]
    narrow_enabled: bool,
    #[serde(default = "default_narrow_layout_threshold")]
    narrow_threshold: i32,
    #[serde(default = "default_stored_narrow_layout_profile")]
    narrow_profile: StoredLayoutProfile,
    #[serde(default)]
    preferred_left_sidebar_width: Option<i32>,
    #[serde(default)]
    preferred_right_sidebar_width: Option<i32>,
}

impl<'de> Deserialize<'de> for LayoutSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let stored = StoredLayoutSettings::deserialize(deserializer)?;
        let legacy_right_width = stored
            .default_profile
            .right_sidebar
            .preferred_width()
            .or_else(|| {
                stored
                    .default_profile
                    .last_visible_right_sidebar
                    .preferred_width()
            })
            .unwrap_or(DEFAULT_RIGHT_SIDEBAR_WIDTH);
        let mut settings = Self {
            default_profile: LayoutProfile::new(
                stored.default_profile.left_sidebar,
                stored.default_profile.right_sidebar.presentation(),
            ),
            narrow_enabled: stored.narrow_enabled,
            narrow_threshold: stored.narrow_threshold,
            narrow_profile: LayoutProfile::new(
                stored.narrow_profile.left_sidebar,
                stored.narrow_profile.right_sidebar.presentation(),
            ),
            preferred_left_sidebar_width: stored
                .preferred_left_sidebar_width
                .unwrap_or(DEFAULT_LEFT_SIDEBAR_WIDTH),
            preferred_right_sidebar_width: stored
                .preferred_right_sidebar_width
                .unwrap_or(legacy_right_width),
        };
        settings.sanitize();
        Ok(settings)
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum SidebarRouteItem {
    Home,
    Search,
    Favorites,
    Albums,
    Tracks,
    Artists,
    AlbumArtists,
    Genres,
    Moods,
    Folders,
    Playlists,
    SmartPlaylists,
    History,
}
impl SidebarRouteItem {
    pub fn all() -> [Self; 13] {
        [
            Self::Home,
            Self::Search,
            Self::Favorites,
            Self::Albums,
            Self::Tracks,
            Self::Artists,
            Self::AlbumArtists,
            Self::Genres,
            Self::Moods,
            Self::History,
            Self::Folders,
            Self::Playlists,
            Self::SmartPlaylists,
        ]
    }

    fn default_visible(self) -> bool {
        !matches!(self, Self::Search | Self::Moods)
    }
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SidebarRouteItemSettings {
    pub item: SidebarRouteItem,
    pub visible: bool,
}
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
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
        source_id: SourceId,
        playlist_id: String,
    },
    SmartPlaylist {
        source_id: SourceId,
        playlist_id: String,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}
impl SidebarPin {
    pub fn source_id(&self) -> &SourceId {
        match self {
            Self::Album { source_id, .. }
            | Self::Artist { source_id, .. }
            | Self::Genre { source_id, .. }
            | Self::Playlist { source_id, .. }
            | Self::SmartPlaylist { source_id, .. } => source_id,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SidebarSettings {
    #[serde(default = "default_sidebar_route_items")]
    pub route_items: Vec<SidebarRouteItemSettings>,
    #[serde(default = "default_true")]
    pub pins_visible: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<SidebarPin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub playlist_pin_imported_sources: Vec<SourceId>,
    #[serde(default = "default_true")]
    pub server_visible: bool,
}
impl Default for SidebarSettings {
    fn default() -> Self {
        Self {
            route_items: default_sidebar_route_items(),
            pins_visible: true,
            pins: Vec::new(),
            playlist_pin_imported_sources: Vec::new(),
            server_visible: true,
        }
    }
}
impl SidebarSettings {
    pub fn sanitize(&mut self) {
        let mut sanitized = Vec::with_capacity(SidebarRouteItem::all().len());
        for entry in &self.route_items {
            if !SidebarRouteItem::all().contains(&entry.item)
                || sanitized
                    .iter()
                    .any(|existing: &SidebarRouteItemSettings| existing.item == entry.item)
            {
                continue;
            }
            sanitized.push(entry.clone());
        }
        for item in SidebarRouteItem::all() {
            if !sanitized.iter().any(|entry| entry.item == item) {
                insert_sidebar_route_item_in_default_order(
                    &mut sanitized,
                    SidebarRouteItemSettings {
                        item,
                        visible: item.default_visible(),
                    },
                );
            }
        }
        if !sanitized.iter().any(|entry| entry.visible)
            && let Some(home) = sanitized
                .iter_mut()
                .find(|entry| entry.item == SidebarRouteItem::Home)
        {
            home.visible = true;
        }
        self.route_items = sanitized;
        let mut seen = HashSet::new();
        self.pins.retain(|pin| seen.insert(pin.clone()));
        let mut seen = HashSet::new();
        self.playlist_pin_imported_sources
            .retain(|source_id| seen.insert(source_id.clone()));
    }

    pub fn is_pinned(&self, pin: &SidebarPin) -> bool {
        self.pins.contains(pin)
    }

    pub fn set_pinned(&mut self, pin: SidebarPin, pinned: bool) -> bool {
        if pinned {
            if self.pins.contains(&pin) {
                return false;
            }
            self.pins.push(pin);
            return true;
        }
        let previous_len = self.pins.len();
        self.pins.retain(|stored| stored != &pin);
        self.pins.len() != previous_len
    }

    pub fn import_playlist_pins_once(
        &mut self,
        source_id: SourceId,
        playlist_ids: impl IntoIterator<Item = String>,
    ) -> bool {
        if self.playlist_pin_imported_sources.contains(&source_id) {
            return false;
        }
        for playlist_id in playlist_ids {
            self.set_pinned(
                SidebarPin::Playlist {
                    source_id: source_id.clone(),
                    playlist_id,
                },
                true,
            );
        }
        self.playlist_pin_imported_sources.push(source_id);
        true
    }
}
fn insert_sidebar_route_item_in_default_order(
    items: &mut Vec<SidebarRouteItemSettings>,
    entry: SidebarRouteItemSettings,
) {
    let default_order = SidebarRouteItem::all();
    let Some(entry_index) = default_order.iter().position(|item| *item == entry.item) else {
        items.push(entry);
        return;
    };
    let insert_index = items
        .iter()
        .position(|existing| {
            default_order
                .iter()
                .position(|item| *item == existing.item)
                .is_some_and(|existing_index| existing_index > entry_index)
        })
        .unwrap_or(items.len());
    items.insert(insert_index, entry);
}
fn default_sidebar_route_items() -> Vec<SidebarRouteItemSettings> {
    SidebarRouteItem::all()
        .into_iter()
        .map(|item| SidebarRouteItemSettings {
            item,
            visible: item.default_visible(),
        })
        .collect()
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ThemePreference {
    System,
    Light,
    Dark,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum AccentPreference {
    #[default]
    System,
    Blue,
    Teal,
    Green,
    Yellow,
    Orange,
    Red,
    Pink,
    Purple,
    Slate,
}
impl AccentPreference {
    pub const ALL: [Self; 10] = [
        Self::System,
        Self::Blue,
        Self::Teal,
        Self::Green,
        Self::Yellow,
        Self::Orange,
        Self::Red,
        Self::Pink,
        Self::Purple,
        Self::Slate,
    ];
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub enum LibraryLayout {
    Row,
    Grid,
    Detail,
}
impl<'de> Deserialize<'de> for LibraryLayout {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "Row" | "row" | "Table" | "table" => Self::Row,
            "Detail" | "detail" => Self::Detail,
            "Grid" | "grid" => Self::Grid,
            _ => Self::Grid,
        })
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum LibraryListKey {
    Albums,
    Artists,
    AlbumArtists,
    Tracks,
    FavoriteTracks,
    History,
    Genres,
    Moods,
    Playlists,
    SmartPlaylists,
    AlbumDetailTracks,
    ArtistAlbums,
    ArtistTracks,
    GenreTracks,
    MoodTracks,
    PlaylistTracks,
    SmartPlaylistTracks,
}
impl LibraryListKey {
    pub fn all() -> [Self; 17] {
        [
            Self::Albums,
            Self::Artists,
            Self::AlbumArtists,
            Self::Tracks,
            Self::FavoriteTracks,
            Self::History,
            Self::Genres,
            Self::Moods,
            Self::Playlists,
            Self::SmartPlaylists,
            Self::AlbumDetailTracks,
            Self::ArtistAlbums,
            Self::ArtistTracks,
            Self::GenreTracks,
            Self::MoodTracks,
            Self::PlaylistTracks,
            Self::SmartPlaylistTracks,
        ]
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Albums => msgid("Albums"),
            Self::Artists => msgid("Artists"),
            Self::AlbumArtists => msgid("Album artists"),
            Self::Tracks => msgid("Tracks"),
            Self::FavoriteTracks => msgid("Favorites"),
            Self::History => msgid("History"),
            Self::Genres => msgid("Genres"),
            Self::Moods => msgid("Moods"),
            Self::Playlists => msgid("Playlists"),
            Self::SmartPlaylists => msgid("Smart playlists"),
            Self::AlbumDetailTracks => msgid("Album tracks"),
            Self::ArtistAlbums => msgid("Artist albums"),
            Self::ArtistTracks => msgid("Artist tracks"),
            Self::GenreTracks => msgid("Genre tracks"),
            Self::MoodTracks => msgid("Mood tracks"),
            Self::PlaylistTracks => msgid("Playlist tracks"),
            Self::SmartPlaylistTracks => msgid("Smart playlist tracks"),
        }
    }

    pub fn supports_layout(self, layout: LibraryLayout) -> bool {
        match layout {
            LibraryLayout::Detail => matches!(self, Self::Albums),
            LibraryLayout::Row | LibraryLayout::Grid => true,
        }
    }

    fn default_layout(self) -> LibraryLayout {
        match self {
            Self::Albums => LibraryLayout::Grid,
            Self::Tracks
            | Self::FavoriteTracks
            | Self::History
            | Self::AlbumDetailTracks
            | Self::ArtistTracks
            | Self::GenreTracks
            | Self::MoodTracks
            | Self::PlaylistTracks
            | Self::SmartPlaylistTracks => LibraryLayout::Row,
            Self::Artists
            | Self::AlbumArtists
            | Self::Genres
            | Self::Moods
            | Self::Playlists
            | Self::SmartPlaylists
            | Self::ArtistAlbums => LibraryLayout::Grid,
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum LibraryField {
    RowIndex,
    Image,
    Title,
    TitleMerged,
    Artist,
    AlbumArtist,
    Album,
    Year,
    ReleaseDate,
    DateAdded,
    LastPlayed,
    PlayCount,
    UserRating,
    Genre,
    Bpm,
    TrackNumber,
    DiscNumber,
    SongCount,
    AlbumCount,
    Duration,
    Favorite,
}
impl LibraryField {
    pub fn title(self) -> &'static str {
        match self {
            Self::RowIndex => "#",
            Self::Image => msgid("Image"),
            Self::Title => msgid("Title"),
            Self::TitleMerged => msgid("Title (merged)"),
            Self::Artist => msgid("Artist"),
            Self::AlbumArtist => msgid("Album artist"),
            Self::Album => msgid("Album"),
            Self::Year => msgid("Year"),
            Self::ReleaseDate => msgid("Release date"),
            Self::DateAdded => msgid("Date added"),
            Self::LastPlayed => msgid("Last played"),
            Self::PlayCount => msgid("Plays"),
            Self::UserRating => msgid("Rating"),
            Self::Genre => msgid("Genre"),
            Self::Bpm => msgid("BPM"),
            Self::TrackNumber => msgid("Track"),
            Self::DiscNumber => msgid("Disc"),
            Self::SongCount => msgid("Number of songs"),
            Self::AlbumCount => msgid("Albums"),
            Self::Duration => msgid("Duration"),
            Self::Favorite => msgid("Favorite"),
        }
    }

    pub fn track_sort(self) -> library::TrackSort {
        match self {
            Self::TrackNumber => library::TrackSort::TrackNumber,
            Self::Artist => library::TrackSort::Artist,
            Self::AlbumArtist => library::TrackSort::AlbumArtist,
            Self::Album => library::TrackSort::Album,
            Self::Year => library::TrackSort::Year,
            Self::ReleaseDate => library::TrackSort::ReleaseDate,
            Self::DateAdded => library::TrackSort::DateAdded,
            Self::LastPlayed => library::TrackSort::LastPlayed,
            Self::PlayCount => library::TrackSort::PlayCount,
            Self::UserRating => library::TrackSort::UserRating,
            Self::Genre => library::TrackSort::Genre,
            Self::Bpm => library::TrackSort::Bpm,
            Self::Duration => library::TrackSort::Duration,
            Self::Favorite => library::TrackSort::Favorite,
            Self::RowIndex
            | Self::Image
            | Self::Title
            | Self::TitleMerged
            | Self::DiscNumber
            | Self::SongCount
            | Self::AlbumCount => library::TrackSort::Title,
        }
    }

    pub fn album_sort(self) -> library::AlbumSort {
        match self {
            Self::AlbumArtist | Self::Artist => library::AlbumSort::AlbumArtist,
            Self::Year => library::AlbumSort::Year,
            Self::ReleaseDate => library::AlbumSort::ReleaseDate,
            Self::DateAdded => library::AlbumSort::DateAdded,
            Self::LastPlayed => library::AlbumSort::LastPlayed,
            Self::PlayCount => library::AlbumSort::PlayCount,
            Self::UserRating => library::AlbumSort::Rating,
            Self::SongCount => library::AlbumSort::TrackCount,
            Self::Duration => library::AlbumSort::Duration,
            Self::Favorite => library::AlbumSort::Favorite,
            _ => library::AlbumSort::Title,
        }
    }

    pub fn artist_sort(self) -> library::ArtistSort {
        match self {
            Self::AlbumCount => library::ArtistSort::AlbumCount,
            Self::SongCount => library::ArtistSort::TrackCount,
            Self::LastPlayed => library::ArtistSort::LastPlayed,
            Self::PlayCount => library::ArtistSort::PlayCount,
            Self::UserRating => library::ArtistSort::Rating,
            Self::Favorite => library::ArtistSort::Favorite,
            _ => library::ArtistSort::Title,
        }
    }

    pub fn playlist_sort(self) -> library::PlaylistSort {
        match self {
            Self::SongCount => library::PlaylistSort::TrackCount,
            Self::Duration => library::PlaylistSort::Duration,
            _ => library::PlaylistSort::Title,
        }
    }

    pub fn playlist_entry_sort(self) -> library::PlaylistEntrySort {
        match self {
            Self::Artist => library::PlaylistEntrySort::Artist,
            Self::Album => library::PlaylistEntrySort::Album,
            Self::Title | Self::TitleMerged => library::PlaylistEntrySort::Title,
            _ => library::PlaylistEntrySort::Position,
        }
    }

    pub fn smart_playlist_sort(self) -> library::SmartPlaylistListSort {
        match self {
            Self::RowIndex => library::SmartPlaylistListSort::Position,
            Self::SongCount => library::SmartPlaylistListSort::TrackCount,
            Self::Duration => library::SmartPlaylistListSort::Duration,
            _ => library::SmartPlaylistListSort::Title,
        }
    }

    pub fn genre_sort(self) -> library::GenreSort {
        match self {
            Self::AlbumCount => library::GenreSort::AlbumCount,
            Self::SongCount => library::GenreSort::TrackCount,
            _ => library::GenreSort::Title,
        }
    }

    pub fn mood_sort(self) -> library::MoodSort {
        match self {
            Self::SongCount => library::MoodSort::TrackCount,
            Self::Duration => library::MoodSort::Duration,
            _ => library::MoodSort::Title,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LibraryListSettings {
    pub layout: LibraryLayout,
    pub row_fields: Vec<LibraryField>,
    pub grid_fields: Vec<LibraryField>,
    pub detail_track_fields: Vec<LibraryField>,
    pub sort_key: LibraryField,
    pub descending: bool,
    #[serde(default)]
    pub layout_version: u8,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LibraryListSettingsEntry {
    pub key: LibraryListKey,
    pub settings: LibraryListSettings,
}
impl LibraryListSettings {
    pub fn for_key(key: LibraryListKey) -> Self {
        Self {
            layout: key.default_layout(),
            row_fields: default_row_fields(key),
            grid_fields: default_grid_fields(key),
            detail_track_fields: default_detail_track_fields(),
            sort_key: default_sort_key(key),
            descending: default_descending(key),
            layout_version: LIBRARY_LIST_LAYOUT_VERSION,
        }
    }

    pub fn sanitize(&mut self, key: LibraryListKey) {
        self.migrate_defaults(key);
        if !key.supports_layout(self.layout) {
            self.layout = key.default_layout();
        }
        sanitize_required_fields(
            &mut self.row_fields,
            available_row_fields(key),
            default_row_fields(key),
        );
        ensure_usable_row_field(&mut self.row_fields, default_row_fields(key));
        sanitize_optional_fields(&mut self.grid_fields, available_grid_fields(key));
        sanitize_required_fields(
            &mut self.detail_track_fields,
            available_detail_track_fields(),
            default_detail_track_fields(),
        );
        ensure_usable_row_field(&mut self.detail_track_fields, default_detail_track_fields());
        if !available_sort_fields(key).contains(&self.sort_key) {
            self.sort_key = default_sort_key(key);
            self.descending = default_descending(key);
        }
        self.layout_version = LIBRARY_LIST_LAYOUT_VERSION;
    }

    fn migrate_defaults(&mut self, key: LibraryListKey) {
        if self.layout_version >= LIBRARY_LIST_LAYOUT_VERSION {
            return;
        }

        if key == LibraryListKey::Playlists {
            if self.row_fields
                == [
                    LibraryField::Image,
                    LibraryField::Title,
                    LibraryField::SongCount,
                    LibraryField::Duration,
                ]
            {
                self.row_fields = default_row_fields(key);
            }
            if self.grid_fields == [LibraryField::SongCount, LibraryField::Duration] {
                self.grid_fields = default_grid_fields(key);
            }
        }

        if key == LibraryListKey::SmartPlaylists
            && self.layout_version < 4
            && self.sort_key == LibraryField::Title
        {
            self.sort_key = default_sort_key(key);
        }

        if key.supports_layout(LibraryLayout::Detail)
            && self.layout_version < 5
            && self
                .detail_track_fields
                .iter()
                .any(|field| !available_detail_track_fields().contains(field))
        {
            self.detail_track_fields = default_detail_track_fields();
        }

        if self.layout_version < 6 {
            match key {
                LibraryListKey::Albums | LibraryListKey::ArtistAlbums => {
                    let previous_default = [
                        LibraryField::TitleMerged,
                        LibraryField::Year,
                        LibraryField::Favorite,
                    ];
                    let duplicate_artist_default = [
                        LibraryField::TitleMerged,
                        LibraryField::AlbumArtist,
                        LibraryField::Year,
                        LibraryField::Favorite,
                    ];
                    if self.row_fields == previous_default
                        || self.row_fields == duplicate_artist_default
                    {
                        self.row_fields = default_row_fields(key);
                    }
                }
                LibraryListKey::ArtistTracks => {
                    let previous_default = [
                        LibraryField::RowIndex,
                        LibraryField::TitleMerged,
                        LibraryField::Album,
                        LibraryField::Duration,
                        LibraryField::Favorite,
                    ];
                    if self.row_fields == previous_default {
                        self.row_fields = default_row_fields(key);
                    }
                }
                LibraryListKey::Artists
                | LibraryListKey::AlbumArtists
                | LibraryListKey::Tracks
                | LibraryListKey::FavoriteTracks
                | LibraryListKey::History
                | LibraryListKey::Genres
                | LibraryListKey::Moods
                | LibraryListKey::Playlists
                | LibraryListKey::SmartPlaylists
                | LibraryListKey::AlbumDetailTracks
                | LibraryListKey::GenreTracks
                | LibraryListKey::MoodTracks
                | LibraryListKey::PlaylistTracks
                | LibraryListKey::SmartPlaylistTracks => {}
            }
        }

        if self.layout_version < 7 {
            match key {
                LibraryListKey::Albums => {
                    let previous_default = [
                        LibraryField::TitleMerged,
                        LibraryField::PlayCount,
                        LibraryField::Year,
                        LibraryField::Favorite,
                    ];
                    if self.layout == LibraryLayout::Row && self.row_fields == previous_default {
                        self.layout = key.default_layout();
                    }
                }
                LibraryListKey::FavoriteTracks | LibraryListKey::ArtistTracks => {
                    let previous_default = [
                        LibraryField::TitleMerged,
                        LibraryField::Album,
                        LibraryField::Year,
                        LibraryField::Favorite,
                    ];
                    if self.row_fields == previous_default {
                        self.row_fields = default_row_fields(key);
                    }
                }
                LibraryListKey::Artists
                | LibraryListKey::AlbumArtists
                | LibraryListKey::Tracks
                | LibraryListKey::History
                | LibraryListKey::Genres
                | LibraryListKey::Moods
                | LibraryListKey::Playlists
                | LibraryListKey::SmartPlaylists
                | LibraryListKey::AlbumDetailTracks
                | LibraryListKey::ArtistAlbums
                | LibraryListKey::GenreTracks
                | LibraryListKey::MoodTracks
                | LibraryListKey::PlaylistTracks
                | LibraryListKey::SmartPlaylistTracks => {}
            }
        }

        if self.layout_version < 8 {
            let defaults = default_row_fields(key);
            let standard_track_default = matches!(
                key,
                LibraryListKey::Tracks
                    | LibraryListKey::FavoriteTracks
                    | LibraryListKey::ArtistTracks
            ) && self.row_fields == defaults[1..];
            let album_detail_default = key == LibraryListKey::AlbumDetailTracks
                && self.row_fields
                    == [
                        LibraryField::TrackNumber,
                        LibraryField::Title,
                        LibraryField::Duration,
                    ];
            if standard_track_default || album_detail_default {
                self.row_fields = defaults;
            }

            if self.detail_track_fields
                == [
                    LibraryField::TrackNumber,
                    LibraryField::Title,
                    LibraryField::Duration,
                ]
            {
                self.detail_track_fields = default_detail_track_fields();
            }
        }

        if self.layout_version < 9 && key == LibraryListKey::History {
            if self.row_fields
                == [
                    LibraryField::RowIndex,
                    LibraryField::TitleMerged,
                    LibraryField::Album,
                    LibraryField::Duration,
                    LibraryField::Favorite,
                ]
            {
                self.row_fields = default_row_fields(key);
            }
            if self.grid_fields
                == [
                    LibraryField::Artist,
                    LibraryField::Album,
                    LibraryField::Duration,
                ]
            {
                self.grid_fields = default_grid_fields(key);
            }
        }
    }
}
pub fn default_library_list_settings() -> Vec<LibraryListSettingsEntry> {
    LibraryListKey::all()
        .into_iter()
        .map(|key| LibraryListSettingsEntry {
            key,
            settings: LibraryListSettings::for_key(key),
        })
        .collect()
}
pub fn available_row_fields(key: LibraryListKey) -> &'static [LibraryField] {
    match key {
        LibraryListKey::Albums | LibraryListKey::ArtistAlbums => &[
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::TitleMerged,
            LibraryField::AlbumArtist,
            LibraryField::Year,
            LibraryField::ReleaseDate,
            LibraryField::DateAdded,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::Genre,
            LibraryField::SongCount,
            LibraryField::Duration,
            LibraryField::Favorite,
        ],
        LibraryListKey::Artists | LibraryListKey::AlbumArtists => &[
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::SongCount,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::Favorite,
        ],
        LibraryListKey::Genres => &[
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::SongCount,
        ],
        LibraryListKey::Moods => &[
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::Playlists | LibraryListKey::SmartPlaylists => &[
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::Tracks
        | LibraryListKey::FavoriteTracks
        | LibraryListKey::History
        | LibraryListKey::AlbumDetailTracks
        | LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::PlaylistTracks
        | LibraryListKey::SmartPlaylistTracks => &[
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::TitleMerged,
            LibraryField::Artist,
            LibraryField::AlbumArtist,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::ReleaseDate,
            LibraryField::DateAdded,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::Genre,
            LibraryField::Bpm,
            LibraryField::DiscNumber,
            LibraryField::TrackNumber,
            LibraryField::Duration,
            LibraryField::Favorite,
        ],
    }
}
pub fn available_grid_fields(key: LibraryListKey) -> &'static [LibraryField] {
    match key {
        LibraryListKey::Albums | LibraryListKey::ArtistAlbums => &[
            LibraryField::AlbumArtist,
            LibraryField::Year,
            LibraryField::ReleaseDate,
            LibraryField::DateAdded,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::Genre,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::Artists | LibraryListKey::AlbumArtists => &[
            LibraryField::AlbumCount,
            LibraryField::SongCount,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
        ],
        LibraryListKey::Genres => &[LibraryField::AlbumCount, LibraryField::SongCount],
        LibraryListKey::Moods => &[LibraryField::SongCount, LibraryField::Duration],
        LibraryListKey::Playlists | LibraryListKey::SmartPlaylists => {
            &[LibraryField::SongCount, LibraryField::Duration]
        }
        LibraryListKey::Tracks
        | LibraryListKey::FavoriteTracks
        | LibraryListKey::History
        | LibraryListKey::AlbumDetailTracks
        | LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::PlaylistTracks
        | LibraryListKey::SmartPlaylistTracks => &[
            LibraryField::Artist,
            LibraryField::AlbumArtist,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::ReleaseDate,
            LibraryField::DateAdded,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::Genre,
            LibraryField::Duration,
        ],
    }
}
