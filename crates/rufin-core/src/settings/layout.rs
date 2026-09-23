use serde::{Deserialize, Deserializer, Serialize};

pub const LIBRARY_LIST_LAYOUT_VERSION: u8 = 14;
pub const DEFAULT_WINDOW_WIDTH: i32 = 1_500;
pub const DEFAULT_WINDOW_HEIGHT: i32 = 900;
pub const MIN_RESTORED_WINDOW_WIDTH: i32 = 450;
pub const MIN_RESTORED_WINDOW_HEIGHT: i32 = 400;
pub const MAX_RESTORED_WINDOW_WIDTH: i32 = 3_400;
pub const MAX_RESTORED_WINDOW_HEIGHT: i32 = 2_000;
fn default_narrow_layout_enabled() -> bool {
    true
}
fn default_narrow_layout_threshold() -> i32 {
    1_300
}
pub fn sanitized_window_size(width: Option<i32>, height: Option<i32>) -> Option<(i32, i32)> {
    let (width, height) = (width?, height?);
    if width < MIN_RESTORED_WINDOW_WIDTH || height < MIN_RESTORED_WINDOW_HEIGHT {
        return None;
    }
    Some((
        width.clamp(MIN_RESTORED_WINDOW_WIDTH, MAX_RESTORED_WINDOW_WIDTH),
        height.clamp(MIN_RESTORED_WINDOW_HEIGHT, MAX_RESTORED_WINDOW_HEIGHT),
    ))
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ThemePreference {
    System,
    Light,
    Dark,
    Named(String),
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
    pub fn color(self) -> Option<&'static str> {
        match self {
            Self::System => None,
            Self::Blue => Some("#3584e4"),
            Self::Teal => Some("#2190a4"),
            Self::Green => Some("#3a944a"),
            Self::Yellow => Some("#c88800"),
            Self::Orange => Some("#ed5b00"),
            Self::Red => Some("#e62d42"),
            Self::Pink => Some("#d56199"),
            Self::Purple => Some("#9141ac"),
            Self::Slate => Some("#6f8396"),
        }
    }
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
    Folders,
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
    Queue,
}
impl LibraryListKey {
    pub fn all() -> [Self; 19] {
        [
            Self::Albums,
            Self::Artists,
            Self::AlbumArtists,
            Self::Tracks,
            Self::Folders,
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
            Self::Queue,
        ]
    }

    pub fn supports_layout(self, layout: LibraryLayout) -> bool {
        match layout {
            LibraryLayout::Detail => matches!(self, Self::Albums),
            LibraryLayout::Grid => {
                !matches!(self, Self::AlbumDetailTracks | Self::Queue | Self::Folders)
            }
            LibraryLayout::Row => true,
        }
    }

    fn default_layout(self) -> LibraryLayout {
        match self {
            Self::Albums => LibraryLayout::Grid,
            Self::Queue
            | Self::Tracks
            | Self::Folders
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
    Tools,
}
impl LibraryField {
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
            | Self::AlbumCount
            | Self::Tools => library::TrackSort::Title,
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
            Self::RowIndex => library::PlaylistSort::Position,
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
                LibraryListKey::Queue
                | LibraryListKey::Artists
                | LibraryListKey::AlbumArtists
                | LibraryListKey::Tracks
                | LibraryListKey::Folders
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
                LibraryListKey::Queue
                | LibraryListKey::Artists
                | LibraryListKey::AlbumArtists
                | LibraryListKey::Tracks
                | LibraryListKey::Folders
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
        if self.layout_version < 10 {
            for fields in [&mut self.row_fields, &mut self.detail_track_fields] {
                for field in fields.iter_mut() {
                    if *field == LibraryField::Favorite {
                        *field = LibraryField::Tools;
                    }
                }
                if key == LibraryListKey::Queue {
                    fields.retain(|field| *field != LibraryField::Tools);
                } else if !fields.contains(&LibraryField::Tools) {
                    fields.push(LibraryField::Tools);
                }
            }
        }
        if self.layout_version < 11
            && key == LibraryListKey::ArtistTracks
            && self.row_fields
                == [
                    LibraryField::RowIndex,
                    LibraryField::TitleMerged,
                    LibraryField::Album,
                    LibraryField::Year,
                    LibraryField::PlayCount,
                    LibraryField::Tools,
                ]
        {
            self.row_fields = default_row_fields(key);
        }
        if self.layout_version < 12
            && matches!(
                key,
                LibraryListKey::Albums
                    | LibraryListKey::ArtistAlbums
                    | LibraryListKey::Artists
                    | LibraryListKey::AlbumArtists
                    | LibraryListKey::Genres
                    | LibraryListKey::Moods
                    | LibraryListKey::Playlists
                    | LibraryListKey::SmartPlaylists
            )
        {
            let defaults = default_row_fields(key);
            if self.row_fields == defaults[1..] {
                self.row_fields = defaults;
            }
        }
        if self.layout_version < 13
            && key == LibraryListKey::History
            && self.row_fields
                == [
                    LibraryField::RowIndex,
                    LibraryField::TitleMerged,
                    LibraryField::Album,
                    LibraryField::LastPlayed,
                    LibraryField::Tools,
                ]
        {
            self.row_fields = default_row_fields(key);
        }
        if self.layout_version < 14
            && key == LibraryListKey::FavoriteTracks
            && self.row_fields
                == [
                    LibraryField::RowIndex,
                    LibraryField::TitleMerged,
                    LibraryField::Album,
                    LibraryField::Year,
                    LibraryField::PlayCount,
                    LibraryField::Tools,
                ]
        {
            self.row_fields = default_row_fields(key);
        }
        if self.layout_version < 14
            && key == LibraryListKey::SmartPlaylists
            && self.row_fields
                == [
                    LibraryField::RowIndex,
                    LibraryField::Image,
                    LibraryField::Title,
                    LibraryField::SongCount,
                    LibraryField::Duration,
                    LibraryField::Tools,
                ]
        {
            self.row_fields = default_row_fields(key);
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
        LibraryListKey::Queue => &[
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Title,
            LibraryField::Artist,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::Duration,
            LibraryField::Tools,
        ],
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
            LibraryField::Tools,
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
            LibraryField::Tools,
        ],
        LibraryListKey::Genres => &[
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::SongCount,
            LibraryField::Tools,
        ],
        LibraryListKey::Moods => &[
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
            LibraryField::Tools,
        ],
        LibraryListKey::Playlists | LibraryListKey::SmartPlaylists => &[
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
            LibraryField::Tools,
        ],
        LibraryListKey::Tracks
        | LibraryListKey::Folders
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
            LibraryField::Tools,
        ],
    }
}
pub fn available_grid_fields(key: LibraryListKey) -> &'static [LibraryField] {
    match key {
        LibraryListKey::Queue | LibraryListKey::Folders => &[],
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

pub fn available_sort_fields(key: LibraryListKey) -> &'static [LibraryField] {
    match key {
        LibraryListKey::Queue => &[LibraryField::RowIndex],
        LibraryListKey::Albums | LibraryListKey::ArtistAlbums => &[
            LibraryField::Title,
            LibraryField::AlbumArtist,
            LibraryField::Year,
            LibraryField::ReleaseDate,
            LibraryField::DateAdded,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::SongCount,
            LibraryField::Duration,
            LibraryField::Favorite,
        ],
        LibraryListKey::Artists | LibraryListKey::AlbumArtists => &[
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::SongCount,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::Favorite,
        ],
        LibraryListKey::Genres => &[
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::SongCount,
        ],
        LibraryListKey::Moods => &[
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::Playlists => &[
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::SmartPlaylists => &[
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::PlaylistTracks => &[
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::Artist,
            LibraryField::Album,
        ],
        LibraryListKey::History => &[LibraryField::LastPlayed],
        LibraryListKey::Tracks
        | LibraryListKey::Folders
        | LibraryListKey::FavoriteTracks
        | LibraryListKey::AlbumDetailTracks
        | LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::SmartPlaylistTracks => &[
            LibraryField::TrackNumber,
            LibraryField::Title,
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
            LibraryField::Duration,
            LibraryField::Favorite,
        ],
    }
}
fn default_row_fields(key: LibraryListKey) -> Vec<LibraryField> {
    match key {
        LibraryListKey::Queue => vec![LibraryField::TitleMerged, LibraryField::Year],
        LibraryListKey::Albums | LibraryListKey::ArtistAlbums => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::PlayCount,
            LibraryField::Year,
            LibraryField::Tools,
        ],
        LibraryListKey::Artists | LibraryListKey::AlbumArtists => vec![
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::Tools,
        ],
        LibraryListKey::Genres => vec![
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::SongCount,
            LibraryField::Tools,
        ],
        LibraryListKey::Moods => vec![
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
            LibraryField::Tools,
        ],
        LibraryListKey::Playlists | LibraryListKey::SmartPlaylists => vec![
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Tools,
        ],
        LibraryListKey::Tracks | LibraryListKey::Folders | LibraryListKey::FavoriteTracks => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::Tools,
        ],
        LibraryListKey::History => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::LastPlayed,
            LibraryField::Tools,
        ],
        LibraryListKey::AlbumDetailTracks => default_detail_track_fields(),
        LibraryListKey::ArtistTracks => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Tools,
        ],
        LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::PlaylistTracks => {
            vec![
                LibraryField::RowIndex,
                LibraryField::TitleMerged,
                LibraryField::Album,
                LibraryField::Duration,
                LibraryField::Tools,
            ]
        }
        LibraryListKey::SmartPlaylistTracks => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::PlayCount,
            LibraryField::Tools,
        ],
    }
}
fn default_grid_fields(key: LibraryListKey) -> Vec<LibraryField> {
    match key {
        LibraryListKey::Queue | LibraryListKey::Folders => Vec::new(),
        LibraryListKey::Albums | LibraryListKey::ArtistAlbums => {
            vec![LibraryField::AlbumArtist, LibraryField::Year]
        }
        LibraryListKey::Artists | LibraryListKey::AlbumArtists => Vec::new(),
        LibraryListKey::Genres => Vec::new(),
        LibraryListKey::Moods => vec![LibraryField::SongCount, LibraryField::Duration],
        LibraryListKey::Playlists => vec![LibraryField::SongCount],
        LibraryListKey::SmartPlaylists => vec![LibraryField::SongCount, LibraryField::Duration],
        LibraryListKey::History => vec![
            LibraryField::Artist,
            LibraryField::Album,
            LibraryField::LastPlayed,
        ],
        LibraryListKey::Tracks
        | LibraryListKey::FavoriteTracks
        | LibraryListKey::AlbumDetailTracks
        | LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::PlaylistTracks
        | LibraryListKey::SmartPlaylistTracks => {
            vec![
                LibraryField::Artist,
                LibraryField::Album,
                LibraryField::Duration,
            ]
        }
    }
}
pub fn available_detail_track_fields() -> &'static [LibraryField] {
    &[
        LibraryField::RowIndex,
        LibraryField::TrackNumber,
        LibraryField::Title,
        LibraryField::Duration,
        LibraryField::Tools,
    ]
}
fn default_detail_track_fields() -> Vec<LibraryField> {
    vec![
        LibraryField::RowIndex,
        LibraryField::Title,
        LibraryField::Duration,
        LibraryField::Tools,
    ]
}
fn default_sort_key(key: LibraryListKey) -> LibraryField {
    match key {
        LibraryListKey::Albums
        | LibraryListKey::Artists
        | LibraryListKey::AlbumArtists
        | LibraryListKey::Genres
        | LibraryListKey::Moods
        | LibraryListKey::ArtistAlbums
        | LibraryListKey::Tracks
        | LibraryListKey::Folders
        | LibraryListKey::FavoriteTracks => LibraryField::Title,
        LibraryListKey::History => LibraryField::LastPlayed,
        LibraryListKey::Queue
        | LibraryListKey::Playlists
        | LibraryListKey::SmartPlaylists
        | LibraryListKey::PlaylistTracks => LibraryField::RowIndex,
        LibraryListKey::AlbumDetailTracks
        | LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::SmartPlaylistTracks => LibraryField::TrackNumber,
    }
}
fn default_descending(key: LibraryListKey) -> bool {
    key == LibraryListKey::History
}
fn sanitize_optional_fields(fields: &mut Vec<LibraryField>, available: &[LibraryField]) {
    let mut seen = Vec::new();
    fields.retain(|field| {
        if !available.contains(field) || seen.contains(field) {
            return false;
        }
        seen.push(*field);
        true
    });
}
fn sanitize_required_fields(
    fields: &mut Vec<LibraryField>,
    available: &[LibraryField],
    fallback: Vec<LibraryField>,
) {
    sanitize_optional_fields(fields, available);
    if fields.is_empty() {
        *fields = fallback;
    }
}
fn ensure_usable_row_field(fields: &mut Vec<LibraryField>, fallback: Vec<LibraryField>) {
    if fields.iter().any(|field| row_field_is_usable(*field)) {
        return;
    }
    if let Some(field) = fallback
        .into_iter()
        .find(|field| row_field_is_usable(*field))
    {
        fields.push(field);
    }
}
fn row_field_is_usable(field: LibraryField) -> bool {
    !matches!(
        field,
        LibraryField::RowIndex
            | LibraryField::Image
            | LibraryField::TrackNumber
            | LibraryField::DiscNumber
            | LibraryField::Favorite
            | LibraryField::Tools
    )
}

#[cfg(test)]
mod disc_tests {
    use super::*;
    #[test]
    fn album_detail_uses_rows_when_grid_was_saved() {
        let mut settings = LibraryListSettings::for_key(LibraryListKey::AlbumDetailTracks);
        settings.layout = LibraryLayout::Grid;
        let fields = settings.row_fields.clone();
        settings.sanitize(LibraryListKey::AlbumDetailTracks);
        assert_eq!(settings.layout, LibraryLayout::Row);
        assert_eq!(settings.row_fields, fields);
        assert!(!LibraryListKey::AlbumDetailTracks.supports_layout(LibraryLayout::Grid));
        assert!(LibraryListKey::Tracks.supports_layout(LibraryLayout::Grid));
        assert!(LibraryListKey::Albums.supports_layout(LibraryLayout::Grid));
    }

    #[test]
    fn history_row_fields_default_to_index_title_last_played_and_tools() {
        let defaults = default_row_fields(LibraryListKey::History);
        assert_eq!(
            defaults,
            [
                LibraryField::RowIndex,
                LibraryField::TitleMerged,
                LibraryField::LastPlayed,
                LibraryField::Tools,
            ]
        );
    }

    #[test]
    fn history_migrates_album_column_out_of_defaults() {
        let mut settings = LibraryListSettings::for_key(LibraryListKey::History);
        settings.row_fields = vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::LastPlayed,
            LibraryField::Tools,
        ];
        settings.layout_version = 12;
        settings.sanitize(LibraryListKey::History);
        assert_eq!(
            settings.row_fields,
            [
                LibraryField::RowIndex,
                LibraryField::TitleMerged,
                LibraryField::LastPlayed,
                LibraryField::Tools,
            ]
        );
    }

    #[test]
    fn favorite_tracks_row_fields_match_tracks_route_defaults() {
        assert_eq!(
            default_row_fields(LibraryListKey::FavoriteTracks),
            default_row_fields(LibraryListKey::Tracks)
        );
    }

    #[test]
    fn favorite_tracks_migrates_play_count_column_out_of_defaults() {
        let mut settings = LibraryListSettings::for_key(LibraryListKey::FavoriteTracks);
        settings.row_fields = vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::PlayCount,
            LibraryField::Tools,
        ];
        settings.layout_version = 13;
        settings.sanitize(LibraryListKey::FavoriteTracks);
        assert_eq!(
            settings.row_fields,
            default_row_fields(LibraryListKey::Tracks)
        );
    }

    #[test]
    fn default_row_fields_do_not_exceed_five_columns() {
        for key in LibraryListKey::all() {
            let defaults = default_row_fields(key);
            assert!(
                defaults.len() <= 5,
                "key {:?} has {} default row columns: {:?}",
                key,
                defaults.len(),
                defaults
            );
        }
    }
}
