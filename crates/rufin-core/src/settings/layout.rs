pub use library::LibraryField;
use serde::{Deserialize, Serialize};

pub const LIBRARY_LIST_LAYOUT_VERSION: u8 = 17;
pub const DEFAULT_WINDOW_WIDTH: i32 = 1_500;
pub const DEFAULT_WINDOW_HEIGHT: i32 = 900;
/// A window as small as the mini player reopens at its size.
pub const MIN_RESTORED_WINDOW_WIDTH: i32 = 200;
pub const MIN_RESTORED_WINDOW_HEIGHT: i32 = 100;
pub const MAX_RESTORED_WINDOW_WIDTH: i32 = 3_400;
pub const MAX_RESTORED_WINDOW_HEIGHT: i32 = 2_000;
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
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum LeftSidebarMode {
    #[default]
    Full,
    Compact,
    Hidden,
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct LayoutSettings {
    pub default_profile: LayoutProfile,
    pub narrow_enabled: bool,
    pub narrow_threshold: i32,
    pub narrow_profile: LayoutProfile,
    pub preferred_left_sidebar_width: i32,
    pub preferred_right_sidebar_width: i32,
}
impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            default_profile: LayoutProfile::default(),
            narrow_enabled: true,
            narrow_threshold: 1_300,
            narrow_profile: LayoutProfile::new(LeftSidebarMode::Compact, RightSidebarMode::Visible),
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThemePreference {
    System,
    Light,
    Dark,
    Named(String),
}

impl ThemePreference {
    /// The theme this preference shows, if any: Light and Dark are the built-in themes; System
    /// follows the platform.
    pub fn selected<'a>(
        &self,
        themes: &'a [crate::themes::Theme],
    ) -> Option<&'a crate::themes::Theme> {
        let id = match self {
            Self::System => return None,
            Self::Light => "builtin:light",
            Self::Dark => "builtin:dark",
            Self::Named(id) => id,
        };
        themes.iter().find(|theme| theme.id == id)
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
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
    /// Its name, untranslated.
    pub const fn title(self) -> &'static str {
        match self {
            Self::System => localization::msgid("System"),
            Self::Blue => localization::msgid("Blue"),
            Self::Teal => localization::msgid("Teal"),
            Self::Green => localization::msgid("Green"),
            Self::Yellow => localization::msgid("Yellow"),
            Self::Orange => localization::msgid("Orange"),
            Self::Red => localization::msgid("Red"),
            Self::Pink => localization::msgid("Pink"),
            Self::Purple => localization::msgid("Purple"),
            Self::Slate => localization::msgid("Slate"),
        }
    }

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
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum LibraryLayout {
    Row,
    Grid,
    Detail,
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
    FullscreenTracks,
}
impl LibraryListKey {
    pub fn all() -> [Self; 20] {
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
            Self::FullscreenTracks,
        ]
    }

    pub fn supports_layout(self, layout: LibraryLayout) -> bool {
        match layout {
            LibraryLayout::Detail => matches!(self, Self::Albums),
            LibraryLayout::Grid => !matches!(
                self,
                Self::AlbumDetailTracks | Self::Queue | Self::FullscreenTracks | Self::Folders
            ),
            LibraryLayout::Row => true,
        }
    }

    fn default_layout(self) -> LibraryLayout {
        match self {
            Self::Albums => LibraryLayout::Grid,
            Self::Queue
            | Self::FullscreenTracks
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
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DisplaySize {
    Compact,
    #[default]
    Default,
    Large,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DisplaySettings {
    pub size: DisplaySize,
    pub grid_spacing: DisplaySize,
    pub show_header: bool,
    pub hover_highlight: bool,
    pub alternate_rows: bool,
    pub row_borders: bool,
    pub column_borders: bool,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            size: DisplaySize::Default,
            grid_spacing: DisplaySize::Default,
            show_header: true,
            hover_highlight: true,
            alternate_rows: false,
            row_borders: false,
            column_borders: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LibraryListSettings {
    #[serde(default)]
    pub display: DisplaySettings,
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
            display: DisplaySettings::default(),
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

    /// The fields `layout` shows: row columns, grid labels or detail track columns.
    pub fn fields(&self, layout: LibraryLayout) -> &[LibraryField] {
        match layout {
            LibraryLayout::Row => &self.row_fields,
            LibraryLayout::Grid => &self.grid_fields,
            LibraryLayout::Detail => &self.detail_track_fields,
        }
    }

    fn fields_mut(&mut self, layout: LibraryLayout) -> &mut Vec<LibraryField> {
        match layout {
            LibraryLayout::Row => &mut self.row_fields,
            LibraryLayout::Grid => &mut self.grid_fields,
            LibraryLayout::Detail => &mut self.detail_track_fields,
        }
    }

    /// Whether `field` can be shown or hidden in `layout`. A row keeps one column, where
    /// Tools counts as one.
    pub fn can_toggle_field(&self, layout: LibraryLayout, field: LibraryField) -> bool {
        let fields = self.fields(layout);
        layout == LibraryLayout::Grid
            || !fields.contains(&field)
            || !row_column(field)
            || fields.iter().filter(|field| row_column(**field)).count() > 1
    }

    /// Shows or hides `field` in `layout`. A shown field goes before Tools; the queue's row
    /// index goes first.
    pub fn show_field(
        &mut self,
        key: LibraryListKey,
        layout: LibraryLayout,
        field: LibraryField,
        shown: bool,
    ) {
        if !self.can_toggle_field(layout, field) {
            return;
        }
        let fields = self.fields_mut(layout);
        if !shown {
            fields.retain(|candidate| *candidate != field);
        } else if key == LibraryListKey::Queue && field == LibraryField::RowIndex {
            fields.retain(|candidate| *candidate != field);
            fields.insert(0, field);
        } else if !fields.contains(&field) {
            let position = fields.len() - usize::from(fields.last() == Some(&LibraryField::Tools));
            fields.insert(position, field);
        }
    }

    /// Moves a shown `field` one place earlier (`delta` < 0) or later in `layout`.
    pub fn move_field(&mut self, layout: LibraryLayout, field: LibraryField, delta: isize) {
        let fields = self.fields_mut(layout);
        let Some(index) = fields.iter().position(|candidate| *candidate == field) else {
            return;
        };
        let target = if delta < 0 {
            index.saturating_sub(1)
        } else {
            (index + 1).min(fields.len() - 1)
        };
        fields.swap(index, target);
    }

    /// Moves a shown `source` field before or `after` the shown `target` field in `layout`.
    pub fn reorder_field(
        &mut self,
        layout: LibraryLayout,
        source: LibraryField,
        target: LibraryField,
        after: bool,
    ) {
        let fields = self.fields_mut(layout);
        let Some(source_index) = fields.iter().position(|field| *field == source) else {
            return;
        };
        let field = fields.remove(source_index);
        let Some(target_index) = fields.iter().position(|field| *field == target) else {
            fields.insert(source_index.min(fields.len()), field);
            return;
        };
        fields.insert((target_index + usize::from(after)).min(fields.len()), field);
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
                self.row_fields = combined_row_fields(key);
            }
            if self.grid_fields == [LibraryField::SongCount, LibraryField::Duration] {
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
                if matches!(
                    key,
                    LibraryListKey::Queue | LibraryListKey::FullscreenTracks
                ) {
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
            self.row_fields = combined_row_fields(key);
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
            let defaults = combined_row_fields(key);
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
            self.row_fields = combined_row_fields(key);
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
            self.row_fields = combined_row_fields(key);
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
            self.row_fields = combined_row_fields(key);
        }
        if self.layout_version < 15 {
            let removed = match key {
                LibraryListKey::Albums | LibraryListKey::ArtistAlbums => {
                    Some((2, LibraryField::PlayCount))
                }
                LibraryListKey::Artists | LibraryListKey::AlbumArtists => {
                    Some((3, LibraryField::AlbumCount))
                }
                LibraryListKey::Genres => Some((2, LibraryField::AlbumCount)),
                LibraryListKey::Moods => Some((3, LibraryField::Duration)),
                LibraryListKey::Playlists | LibraryListKey::SmartPlaylists => {
                    Some((3, LibraryField::SongCount))
                }
                LibraryListKey::Tracks
                | LibraryListKey::Folders
                | LibraryListKey::FavoriteTracks => Some((3, LibraryField::Year)),
                LibraryListKey::GenreTracks
                | LibraryListKey::MoodTracks
                | LibraryListKey::PlaylistTracks => Some((3, LibraryField::Duration)),
                LibraryListKey::SmartPlaylistTracks => Some((3, LibraryField::PlayCount)),
                _ => None,
            };
            if let Some((index, field)) = removed {
                let defaults = combined_row_fields(key);
                let mut previous = defaults.clone();
                previous.insert(index, field);
                if self.row_fields == previous
                    || (self.layout_version < 12
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
                        && self.row_fields == previous[1..])
                {
                    self.row_fields = defaults;
                }
            }
        }
        if self.layout_version < 16
            && key == LibraryListKey::Albums
            && self.detail_track_fields
                == [
                    LibraryField::RowIndex,
                    LibraryField::Title,
                    LibraryField::Duration,
                    LibraryField::Tools,
                ]
        {
            self.detail_track_fields = default_detail_track_fields();
        }
        if self.layout_version < 17 {
            for fields in [&mut self.row_fields, &mut self.detail_track_fields] {
                if available_row_fields(key).contains(&LibraryField::Favorite)
                    && !fields.contains(&LibraryField::Favorite)
                    && let Some(position) = fields
                        .iter()
                        .position(|field| *field == LibraryField::Tools)
                {
                    fields.insert(position, LibraryField::Favorite);
                }
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
/// The labels under the title of a Home album card.
pub const HOME_ALBUM_FIELDS: [LibraryField; 2] = [LibraryField::AlbumArtist, LibraryField::Year];
/// The labels under the title of a Home track row.
pub const HOME_TRACK_FIELDS: [LibraryField; 2] = [LibraryField::Artist, LibraryField::Album];
/// The labels under the name of a Home genre row.
pub const HOME_GENRE_FIELDS: [LibraryField; 2] =
    [LibraryField::AlbumCount, LibraryField::SongCount];
/// The fields `layout` can show for `key`.
pub fn available_fields(key: LibraryListKey, layout: LibraryLayout) -> &'static [LibraryField] {
    match layout {
        LibraryLayout::Row => available_row_fields(key),
        LibraryLayout::Grid => available_grid_fields(key),
        LibraryLayout::Detail => available_detail_track_fields(),
    }
}
pub fn available_row_fields(key: LibraryListKey) -> &'static [LibraryField] {
    match key {
        LibraryListKey::Queue | LibraryListKey::FullscreenTracks => &[
            LibraryField::RowIndex,
            LibraryField::AlbumArtist,
            LibraryField::DiscNumber,
            LibraryField::TrackNumber,
            LibraryField::ReleaseDate,
            LibraryField::Bitrate,
            LibraryField::SampleRate,
            LibraryField::BitDepth,
            LibraryField::Channels,
            LibraryField::Format,
            LibraryField::FilePath,
            LibraryField::Source,
            LibraryField::TitleMerged,
            LibraryField::Title,
            LibraryField::Artist,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::Duration,
            LibraryField::Favorite,
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
            LibraryField::Favorite,
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
            LibraryField::Favorite,
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
            LibraryField::Bitrate,
            LibraryField::SampleRate,
            LibraryField::BitDepth,
            LibraryField::Channels,
            LibraryField::Format,
            LibraryField::FilePath,
            LibraryField::Source,
            LibraryField::DiscNumber,
            LibraryField::TrackNumber,
            LibraryField::Duration,
            LibraryField::Favorite,
            LibraryField::Tools,
        ],
    }
}
pub fn available_grid_fields(key: LibraryListKey) -> &'static [LibraryField] {
    match key {
        LibraryListKey::Queue | LibraryListKey::FullscreenTracks | LibraryListKey::Folders => &[],
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
    const TRACK_SORT_FIELDS: &[LibraryField] = &[
        LibraryField::RowIndex,
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
    ];
    match key {
        LibraryListKey::Queue | LibraryListKey::FullscreenTracks => &[LibraryField::RowIndex],
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
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::SmartPlaylists => &[
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::PlaylistTracks => TRACK_SORT_FIELDS,
        LibraryListKey::History => &[LibraryField::LastPlayed],
        LibraryListKey::Tracks
        | LibraryListKey::Folders
        | LibraryListKey::FavoriteTracks
        | LibraryListKey::AlbumDetailTracks
        | LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::SmartPlaylistTracks => &TRACK_SORT_FIELDS[1..],
    }
}
fn default_row_fields(key: LibraryListKey) -> Vec<LibraryField> {
    let mut fields = combined_row_fields(key);
    if available_row_fields(key).contains(&LibraryField::Favorite)
        && let Some(position) = fields
            .iter()
            .position(|field| *field == LibraryField::Tools)
    {
        fields.insert(position, LibraryField::Favorite);
    }
    // Phones open a row's menu by holding or swiping it, so their rows leave the ⋯ column out
    // until it is shown.
    if cfg!(target_os = "android") {
        fields.retain(|field| *field != LibraryField::Tools);
    }
    fields
}

fn combined_row_fields(key: LibraryListKey) -> Vec<LibraryField> {
    match key {
        LibraryListKey::Queue => vec![LibraryField::TitleMerged, LibraryField::Year],
        LibraryListKey::FullscreenTracks => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Duration,
            LibraryField::Tools,
        ],
        LibraryListKey::Albums | LibraryListKey::ArtistAlbums => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Year,
            LibraryField::Tools,
        ],
        LibraryListKey::Artists | LibraryListKey::AlbumArtists => vec![
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::Tools,
        ],
        LibraryListKey::Genres => vec![
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Tools,
        ],
        LibraryListKey::Moods => vec![
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Tools,
        ],
        LibraryListKey::Playlists | LibraryListKey::SmartPlaylists => vec![
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::Tools,
        ],
        LibraryListKey::Tracks | LibraryListKey::Folders | LibraryListKey::FavoriteTracks => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Tools,
        ],
        LibraryListKey::History => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::LastPlayed,
            LibraryField::Tools,
        ],
        LibraryListKey::AlbumDetailTracks => vec![
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::Duration,
            LibraryField::Tools,
        ],
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
                LibraryField::Tools,
            ]
        }
        LibraryListKey::SmartPlaylistTracks => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Tools,
        ],
    }
}
fn default_grid_fields(key: LibraryListKey) -> Vec<LibraryField> {
    match key {
        LibraryListKey::Queue | LibraryListKey::FullscreenTracks | LibraryListKey::Folders => {
            Vec::new()
        }
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
        LibraryField::AlbumArtist,
        LibraryField::DiscNumber,
        LibraryField::ReleaseDate,
        LibraryField::Bitrate,
        LibraryField::SampleRate,
        LibraryField::BitDepth,
        LibraryField::Channels,
        LibraryField::Format,
        LibraryField::FilePath,
        LibraryField::Source,
        LibraryField::RowIndex,
        LibraryField::TrackNumber,
        LibraryField::Title,
        LibraryField::Duration,
        LibraryField::Favorite,
        LibraryField::Tools,
    ]
}
fn default_detail_track_fields() -> Vec<LibraryField> {
    vec![
        LibraryField::RowIndex,
        LibraryField::Title,
        LibraryField::Duration,
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
        | LibraryListKey::FavoriteTracks
        | LibraryListKey::Playlists
        | LibraryListKey::SmartPlaylists => LibraryField::Title,
        LibraryListKey::History => LibraryField::LastPlayed,
        LibraryListKey::Queue
        | LibraryListKey::FullscreenTracks
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
    if fields.iter().any(|field| row_column(*field)) {
        return;
    }
    if let Some(field) = fallback.into_iter().find(|field| row_column(*field)) {
        fields.push(field);
    }
}

/// Whether `field` is a column a row can keep alone; Tools counts as one.
fn row_column(field: LibraryField) -> bool {
    !matches!(
        field,
        LibraryField::RowIndex
            | LibraryField::Image
            | LibraryField::TrackNumber
            | LibraryField::DiscNumber
            | LibraryField::Favorite
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
    fn history_row_fields_default_to_index_title_last_played_favorite_and_tools() {
        let defaults = default_row_fields(LibraryListKey::History);
        assert_eq!(
            defaults,
            [
                LibraryField::RowIndex,
                LibraryField::TitleMerged,
                LibraryField::LastPlayed,
                LibraryField::Favorite,
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
                LibraryField::Favorite,
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
