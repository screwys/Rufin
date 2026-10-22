use std::path::PathBuf;
use std::sync::Arc;

use downloads::{DownloadRules, SourceDownloadSettings};
use library::PlayedFilter;
use localization::{default_language_preference, sanitize_language_preference};
use lyrics::Settings as LyricsSettings;
use playback::{
    DEFAULT_AUTO_DJ_REFILL_THRESHOLD, MAX_AUTO_DJ_REFILL_THRESHOLD, MIN_AUTO_DJ_REFILL_THRESHOLD,
    PlaybackSettings, RepeatMode,
};
use presence::Settings as RichPresenceSettings;
use secrets::SecretStorageMode;
use serde::{Deserialize, Serialize};
use sources::SourceId;

use super::{
    AccentPreference, ContextMenuSettings, LayoutSettings, LibraryListKey, LibraryListSettings,
    LibraryListSettingsEntry, SettingsError, SettingsOwner, SidebarRouteItem, SidebarSettings,
    ThemePreference, default_library_list_settings, sanitized_window_size,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivityOverviewSettings {
    pub autoplay_first_track: bool,
    pub dynamic_background: bool,
    pub background_image: bool,
    pub tracks: bool,
    pub artists: bool,
    pub albums: bool,
    pub genres: bool,
    pub result_count: super::visualizer::PreferenceIndex,
    pub show_comparison: bool,
    pub show_rufin_in_headline: bool,
    pub monthly_enabled: bool,
    pub yearly_enabled: bool,
    pub monthly_days_before_end: u32,
    pub monthly_days_after_end: u32,
    pub yearly_days_before_end: u32,
    pub yearly_days_after_end: u32,
    pub opened_month: Option<String>,
    pub opened_year: Option<i32>,
}

impl Default for ActivityOverviewSettings {
    fn default() -> Self {
        Self {
            autoplay_first_track: true,
            dynamic_background: true,
            background_image: true,
            tracks: true,
            artists: true,
            albums: true,
            genres: true,
            result_count: 3,
            show_comparison: true,
            show_rufin_in_headline: true,
            monthly_enabled: true,
            yearly_enabled: true,
            monthly_days_before_end: 0,
            monthly_days_after_end: 5,
            yearly_days_before_end: 6,
            yearly_days_after_end: 7,
            opened_month: None,
            opened_year: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ExternalSiteLinkSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub lastfm: bool,
    #[serde(default = "default_true")]
    pub musicbrainz: bool,
    #[serde(default = "default_true")]
    pub server: bool,
}

impl Default for ExternalSiteLinkSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            lastfm: true,
            musicbrainz: true,
            server: true,
        }
    }
}

const DEFAULT_RANDOM_PLAY_LIMIT: usize = 100;
const MIN_RANDOM_PLAY_LIMIT: usize = 1;
const MAX_RANDOM_PLAY_LIMIT: usize = 500;
const MIN_RANDOM_PLAY_YEAR: u16 = 1850;
const MAX_RANDOM_PLAY_YEAR: u16 = 2050;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RandomPlayGenreSelection {
    pub source_id: SourceId,
    #[serde(default)]
    pub music_folder_id: Option<String>,
    pub genre_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RandomPlaySettings {
    pub limit: super::visualizer::PreferenceIndex,
    pub min_year: Option<u16>,
    pub max_year: Option<u16>,
    pub genre: Option<RandomPlayGenreSelection>,
    pub played_filter: PlayedFilter,
}

impl Default for RandomPlaySettings {
    fn default() -> Self {
        Self {
            limit: DEFAULT_RANDOM_PLAY_LIMIT,
            min_year: None,
            max_year: None,
            genre: None,
            played_filter: PlayedFilter::All,
        }
    }
}

impl RandomPlaySettings {
    pub fn selected_genre_id(
        &self,
        source_id: &SourceId,
        music_folder_id: Option<&str>,
    ) -> Option<&str> {
        self.genre.as_ref().and_then(|genre| {
            (&genre.source_id == source_id && genre.music_folder_id.as_deref() == music_folder_id)
                .then_some(genre.genre_id.as_str())
        })
    }

    fn sanitize(&mut self) {
        self.limit = self
            .limit
            .clamp(MIN_RANDOM_PLAY_LIMIT, MAX_RANDOM_PLAY_LIMIT);
        self.min_year = self
            .min_year
            .map(|year| year.clamp(MIN_RANDOM_PLAY_YEAR, MAX_RANDOM_PLAY_YEAR));
        self.max_year = self
            .max_year
            .map(|year| year.clamp(MIN_RANDOM_PLAY_YEAR, MAX_RANDOM_PLAY_YEAR));
    }
}

pub use library::{HomeBlockKind, HomeSectionKind};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum RecentSearchKind {
    Track,
    Album,
    Artist,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RecentSearchResult {
    pub kind: RecentSearchKind,
    pub media_uri: String,
    pub title: String,
    pub subtitle: String,
    pub artwork_binding: Option<Vec<u8>>,
}

macro_rules! preference_group {
    ($(#[$record_attribute:meta])* $name:ident, $group:ident, { $( $(#[$attribute:meta])* $field:ident: $ty:ty = $default:expr $(=> $setter:ident)?; )* }) => {
        #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
        $(#[$record_attribute])*
        pub struct $name { $( $(#[$attribute])* pub $field: $ty, )* }
        impl Default for $name {
            fn default() -> Self { Self { $( $field: $default, )* } }
        }
        #[cfg_attr(feature = "bindings", uniffi::export)]
        impl SettingsOwner {
            $( $(pub fn $setter(&self, value: $ty) -> Result<(), SettingsError> {
                self.update_preferences(move |ui| {
                    ui.$group.$field = value;
                    Ok(())
                }).map_err(Into::into)
            })? )*
        }
    };
}

preference_group!(#[cfg_attr(feature = "bindings", derive(uniffi::Record))] ApplicationPreferences, application, {
    #[serde(default = "default_starting_page")]
    starting_page: SidebarRouteItem = default_starting_page() => set_starting_page;
    private_mode: bool = false => set_private_mode;
    #[serde(default)]
    cast_proxy_enabled: bool = false => set_cast_proxy_enabled;
    #[serde(default = "default_true")]
    external_metadata_enabled: bool = true => set_external_metadata_enabled;
    #[serde(default)]
    prefer_distinct_track_covers: bool = false => set_prefer_distinct_track_covers;
    notifications_enabled: bool = false => set_notifications_enabled;
    #[serde(default = "default_true")]
    control_notifications_enabled: bool = true => set_control_notifications_enabled;
    #[serde(default = "default_true")]
    release_notifications_enabled: bool = true => set_release_notifications_enabled;
    #[serde(default = "default_release_check_interval_hours")]
    release_check_interval_hours: u32 = default_release_check_interval_hours() => set_release_check_interval_hours;
    #[serde(default)]
    reduce_motion: bool = false => set_reduce_motion;
    #[serde(default = "default_true")]
    show_downloaded_badges: bool = true => set_show_downloaded_badges;
    #[serde(default = "default_true")]
    new_playlist_current: bool = true => set_new_playlist_current;
    #[serde(default = "default_auto_dj_refill_threshold")]
    auto_dj_refill_threshold: u8 = DEFAULT_AUTO_DJ_REFILL_THRESHOLD => set_auto_dj_refill_threshold;
    #[serde(default)]
    external_site_links: ExternalSiteLinkSettings = ExternalSiteLinkSettings::default() => set_external_site_links;
    #[serde(default)]
    home_blocks: Vec<HomeBlockKind> = default_home_blocks() => set_home_blocks;
    #[serde(default)]
    context_menu: ContextMenuSettings = ContextMenuSettings::default() => set_context_menu;
    #[serde(default)]
    activity_overview: ActivityOverviewSettings = ActivityOverviewSettings::default();
    #[serde(default)]
    backup: crate::runtime::BackupSettings = crate::runtime::BackupSettings::default();
    #[serde(default = "legacy_secret_storage_mode")]
    secret_storage_mode: SecretStorageMode = SecretStorageMode::default();
});

preference_group!(#[cfg_attr(feature = "bindings", derive(uniffi::Record))] PlayerDisplayPreferences, player_display, {
    #[serde(default)]
    clear_queue_includes_current: bool = false => set_clear_queue_includes_current;
    #[serde(default = "default_true")]
    fullscreen_dynamic_background: bool = true => set_fullscreen_dynamic_background;
    #[serde(default = "default_lyrics_panel_visible")]
    fullscreen_background_image: bool = true => set_fullscreen_background_image;
    #[serde(default = "default_lyrics_panel_visible")]
    fullscreen_lyrics_visible: bool = true => set_fullscreen_lyrics_visible;
    #[serde(default = "default_lyrics_panel_visible")]
    fullscreen_current_lyrics_line_visible: bool = true => set_fullscreen_current_lyrics_line_visible;
    #[serde(default = "default_lyrics_panel_visible")]
    fullscreen_visualizer_visible: bool = true => set_fullscreen_visualizer_visible;
    #[serde(default)]
    seekbar_waveform_enabled: bool = false => set_seekbar_waveform_enabled;
});

preference_group!(#[derive(Eq)] AppearancePreferences, appearance, {
    theme_preference: ThemePreference = ThemePreference::System => set_theme;
    #[serde(default)]
    accent_preference: AccentPreference = AccentPreference::System => set_accent;
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    theme_accents: std::collections::BTreeMap<String, String> = Default::default();
    #[serde(default = "default_language_preference")]
    language: String = default_language_preference() => set_language;
});

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Settings {
    #[serde(flatten)]
    pub appearance: AppearancePreferences,
    #[serde(flatten)]
    pub application: ApplicationPreferences,
    #[serde(flatten)]
    pub player_display: PlayerDisplayPreferences,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_search_results: Vec<RecentSearchResult>,
    #[serde(default)]
    pub web_controller: crate::settings::ControllerSettings,
    #[serde(default)]
    pub layout: LayoutSettings,
    #[serde(default)]
    pub sidebar: SidebarSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_network_interface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_notification_seen_version: Option<String>,
    #[serde(default)]
    pub automatic_updates_enabled: bool,
    #[serde(flatten)]
    pub lyrics: LyricsSettings,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub downloads: Vec<SourceDownloadSettings>,
    #[serde(default)]
    pub show_bottom_bar_rating: bool,
    #[serde(default)]
    pub tray_enabled: bool,
    #[serde(default, alias = "exit_to_tray")]
    pub keep_running_after_close: bool,
    #[serde(default)]
    pub start_minimized: bool,
    #[serde(default = "default_true")]
    pub type_to_search_enabled: bool,
    #[serde(flatten)]
    pub rich_presence: RichPresenceSettings,
    #[serde(default)]
    pub lastfm_api_key: String,
    #[serde(default)]
    pub auto_dj_enabled: bool,
    #[serde(default)]
    pub shuffle_enabled: bool,
    #[serde(default)]
    pub repeat_mode: RepeatMode,
    #[serde(default)]
    pub playback: PlaybackSettings,
    #[serde(default)]
    pub random_play: RandomPlaySettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_width: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_height: Option<i32>,
    #[serde(default = "default_lyrics_panel_visible")]
    pub lyrics_panel_visible: bool,
    #[serde(default)]
    pub visualizer_panel_visible: bool,
    #[serde(default)]
    pub visualizer: super::visualizer::VisualizerSettings,
    #[serde(default)]
    pub queue_lyrics_height: Option<i32>,
    #[serde(default)]
    pub right_panel: super::right_panel::RightPanelSettings,
    #[serde(default)]
    pub library_lists: Vec<LibraryListSettingsEntry>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: AppearancePreferences::default(),
            application: ApplicationPreferences::default(),
            player_display: PlayerDisplayPreferences::default(),
            recent_search_results: Vec::new(),
            web_controller: crate::settings::ControllerSettings::default(),
            layout: LayoutSettings::default(),
            sidebar: SidebarSettings::default(),
            cast_network_interface: None,
            release_notification_seen_version: None,
            automatic_updates_enabled: false,
            lyrics: LyricsSettings::default(),
            downloads: Vec::new(),
            show_bottom_bar_rating: false,
            tray_enabled: false,
            keep_running_after_close: false,
            start_minimized: false,
            type_to_search_enabled: true,
            rich_presence: RichPresenceSettings::default(),
            lastfm_api_key: String::new(),
            auto_dj_enabled: false,
            shuffle_enabled: false,
            repeat_mode: RepeatMode::Off,
            playback: PlaybackSettings::default(),
            random_play: RandomPlaySettings::default(),
            window_width: None,
            window_height: None,
            lyrics_panel_visible: true,
            visualizer_panel_visible: false,
            visualizer: super::visualizer::VisualizerSettings::default(),
            queue_lyrics_height: None,
            right_panel: super::right_panel::RightPanelSettings::default(),
            library_lists: default_library_list_settings(),
        }
    }
}

impl Settings {
    /// Keep opened or played results in recency order, identified by their media URI.
    pub fn remember_search_result(&mut self, result: RecentSearchResult) -> bool {
        if self.recent_search_results.first() == Some(&result) {
            return false;
        }
        self.recent_search_results
            .retain(|recent| recent.media_uri != result.media_uri || recent.kind != result.kind);
        self.recent_search_results.insert(0, result);
        self.recent_search_results.truncate(20);
        true
    }

    pub fn allows_notifications(&self) -> bool {
        self.application.notifications_enabled
    }

    pub fn shows_external_site_links(&self) -> bool {
        self.application.external_site_links.enabled
    }

    pub fn allows_external_metadata_lookup(&self) -> bool {
        self.application.external_metadata_enabled && !self.application.private_mode
    }

    pub fn sanitize(&mut self) {
        self.recent_search_results.truncate(20);
        if ![1, 6, 12, 24].contains(&self.application.release_check_interval_hours) {
            self.application.release_check_interval_hours = default_release_check_interval_hours();
        }
        self.rich_presence.sanitize();
        self.playback.sanitize();
        self.random_play.sanitize();
        self.lyrics.sanitize();
        self.application.auto_dj_refill_threshold = self
            .application
            .auto_dj_refill_threshold
            .clamp(MIN_AUTO_DJ_REFILL_THRESHOLD, MAX_AUTO_DJ_REFILL_THRESHOLD);
        self.lastfm_api_key = self.lastfm_api_key.trim().to_string();
        self.cast_network_interface = self
            .cast_network_interface
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        self.appearance.language = sanitize_language_preference(&self.appearance.language);
        self.release_notification_seen_version = self
            .release_notification_seen_version
            .as_deref()
            .map(str::trim)
            .filter(|version| !version.is_empty())
            .map(str::to_string);
        self.layout.sanitize();
        self.sidebar.sanitize();
        self.right_panel.sanitize();
        self.application.context_menu.sanitize();
        if self.keep_running_after_close {
            self.tray_enabled = true;
        }
        if !self.tray_enabled {
            self.start_minimized = false;
        }
        if let Some((width, height)) = sanitized_window_size(self.window_width, self.window_height)
        {
            self.window_width = Some(width);
            self.window_height = Some(height);
        } else {
            self.window_width = None;
            self.window_height = None;
        }
        sanitize_home_blocks(&mut self.application.home_blocks);
        migrate_library_lists(&mut self.library_lists);
        sanitize_downloads(&mut self.downloads);
    }

    pub fn library_list(&self, key: LibraryListKey) -> LibraryListSettings {
        self.library_lists
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.settings.clone())
            .unwrap_or_else(|| LibraryListSettings::for_key(key))
    }

    pub fn download_rules(&self, source_id: &SourceId) -> DownloadRules {
        self.download_settings(source_id).rules
    }

    pub fn download_directory(&self, source_id: &SourceId) -> Option<PathBuf> {
        self.download_settings(source_id)
            .directory
            .and_then(|directory| directory.native().map(PathBuf::from))
    }

    pub fn download_location(&self, source_id: &SourceId) -> Option<downloads::DownloadDirectory> {
        self.download_settings(source_id).directory
    }

    pub fn download_settings(&self, source_id: &SourceId) -> SourceDownloadSettings {
        self.downloads
            .iter()
            .find(|entry| &entry.source_id == source_id)
            .cloned()
            .unwrap_or_else(|| SourceDownloadSettings::for_source(source_id.clone()))
    }

    pub fn set_download_rules(&mut self, source_id: SourceId, rules: DownloadRules) -> bool {
        self.update_download_settings(source_id, |settings| settings.rules = rules)
    }

    pub fn set_download_quality(
        &mut self,
        source_id: SourceId,
        quality: sources::DownloadQuality,
    ) -> bool {
        self.update_download_settings(source_id, |settings| settings.quality = quality)
    }

    pub fn set_download_directory(
        &mut self,
        source_id: SourceId,
        directory: Option<PathBuf>,
    ) -> bool {
        self.set_download_location(
            source_id,
            directory.map(downloads::DownloadDirectory::Native),
        )
    }

    pub fn set_download_location(
        &mut self,
        source_id: SourceId,
        directory: Option<downloads::DownloadDirectory>,
    ) -> bool {
        self.update_download_settings(source_id, |settings| settings.directory = directory)
    }

    fn update_download_settings(
        &mut self,
        source_id: SourceId,
        update: impl FnOnce(&mut SourceDownloadSettings),
    ) -> bool {
        let previous = self.download_settings(&source_id);
        let mut next = previous.clone();
        update(&mut next);
        if previous == next {
            return false;
        }
        self.downloads.retain(|entry| entry.source_id != source_id);
        if !next.is_default() {
            self.downloads.push(next);
            self.downloads
                .sort_by(|left, right| left.source_id.cmp(&right.source_id));
        }
        true
    }
}

pub type SettingsHandle = Arc<super::SettingsOwner>;

pub fn default_home_blocks() -> Vec<HomeBlockKind> {
    vec![
        HomeBlockKind::Showcase,
        HomeBlockKind::Explore,
        HomeBlockKind::MostPlayed,
        HomeBlockKind::NewlyAdded,
        HomeBlockKind::RecentlyPlayed,
        HomeBlockKind::RecentlyReleased,
        HomeBlockKind::Genres,
    ]
}

fn legacy_secret_storage_mode() -> SecretStorageMode {
    SecretStorageMode::ConfigFile
}

fn default_starting_page() -> SidebarRouteItem {
    SidebarRouteItem::Home
}

fn default_release_check_interval_hours() -> u32 {
    6
}

fn default_true() -> bool {
    true
}

fn default_lyrics_panel_visible() -> bool {
    true
}

fn default_auto_dj_refill_threshold() -> u8 {
    DEFAULT_AUTO_DJ_REFILL_THRESHOLD
}

fn sanitize_home_blocks(blocks: &mut Vec<HomeBlockKind>) {
    let mut seen = Vec::new();
    blocks.retain(|block| {
        if seen.contains(block) {
            false
        } else {
            seen.push(*block);
            true
        }
    });
    if blocks.is_empty() {
        *blocks = default_home_blocks();
    }
}

fn migrate_library_lists(lists: &mut Vec<LibraryListSettingsEntry>) {
    if lists.is_empty() {
        *lists = default_library_list_settings();
    }
    for key in LibraryListKey::all() {
        if !lists.iter().any(|entry| entry.key == key) {
            lists.push(LibraryListSettingsEntry {
                key,
                settings: LibraryListSettings::for_key(key),
            });
        }
    }
    lists.retain(|entry| LibraryListKey::all().contains(&entry.key));
    lists.sort_by_key(|entry| {
        LibraryListKey::all()
            .iter()
            .position(|key| *key == entry.key)
            .unwrap_or(usize::MAX)
    });
    for entry in lists {
        entry.settings.sanitize(entry.key);
    }
}

fn sanitize_downloads(downloads: &mut Vec<SourceDownloadSettings>) {
    for entry in downloads.iter_mut() {
        if entry
            .directory
            .as_ref()
            .is_some_and(|directory| match directory {
                downloads::DownloadDirectory::Native(path) => path.as_os_str().is_empty(),
                downloads::DownloadDirectory::Document { uri } => uri.is_empty(),
            })
        {
            entry.directory = None;
        }
    }
    downloads.retain(|entry| !entry.is_default());
    downloads.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    downloads.dedup_by(|left, right| left.source_id == right.source_id);
}

#[cfg(test)]
mod tests {
    use crate::settings::SidebarRouteItem;

    use super::{
        ApplicationPreferences, PlayerDisplayPreferences, RecentSearchKind, RecentSearchResult,
        Settings,
    };

    #[test]
    fn recent_search_results_keep_exact_items_in_recency_order_and_round_trip() {
        let mut settings = Settings::default();
        let artist = RecentSearchResult {
            kind: RecentSearchKind::Artist,
            media_uri: "provider://one/artist/1".into(),
            title: "Björk".into(),
            subtitle: String::new(),
            artwork_binding: None,
        };
        let album = RecentSearchResult {
            kind: RecentSearchKind::Album,
            media_uri: "provider://one/album/1".into(),
            ..artist.clone()
        };
        let other_source = RecentSearchResult {
            media_uri: "provider://two/artist/1".into(),
            ..artist.clone()
        };
        assert!(settings.remember_search_result(artist.clone()));
        assert!(!settings.remember_search_result(artist.clone()));
        settings.remember_search_result(album.clone());
        settings.remember_search_result(other_source.clone());
        settings.remember_search_result(artist.clone());
        assert_eq!(
            settings.recent_search_results,
            [artist.clone(), other_source, album]
        );
        for index in 0..25 {
            settings.remember_search_result(RecentSearchResult {
                kind: RecentSearchKind::Track,
                media_uri: format!("provider://one/track/{index}"),
                ..artist.clone()
            });
        }
        assert_eq!(settings.recent_search_results.len(), 20);
        assert_eq!(
            settings.recent_search_results.first().unwrap().media_uri,
            "provider://one/track/24"
        );
        assert_eq!(
            settings.recent_search_results.last().unwrap().media_uri,
            "provider://one/track/5"
        );
        let restored: Settings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(
            restored.recent_search_results,
            settings.recent_search_results
        );
        let mut saved = serde_json::to_value(settings).unwrap();
        saved
            .as_object_mut()
            .unwrap()
            .remove("recent_search_results");
        saved
            .as_object_mut()
            .unwrap()
            .insert("recent_searches".into(), serde_json::json!(["old query"]));
        assert!(
            serde_json::from_value::<Settings>(saved)
                .unwrap()
                .recent_search_results
                .is_empty()
        );
    }

    #[test]
    fn fullscreen_display_defaults_and_choices_survive_settings_reload() {
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        for key in [
            "fullscreen_lyrics_visible",
            "fullscreen_visualizer_visible",
            "fullscreen_dynamic_background",
            "fullscreen_background_image",
        ] {
            old.as_object_mut().unwrap().remove(key);
        }
        let restored: Settings = serde_json::from_value(old).unwrap();
        assert!(restored.player_display.fullscreen_lyrics_visible);
        assert!(restored.player_display.fullscreen_visualizer_visible);
        assert!(restored.player_display.fullscreen_dynamic_background);
        for choice in 0..16 {
            let settings = Settings {
                player_display: PlayerDisplayPreferences {
                    fullscreen_lyrics_visible: choice & 1 != 0,
                    fullscreen_visualizer_visible: choice & 2 != 0,
                    fullscreen_dynamic_background: choice & 4 != 0,
                    fullscreen_background_image: choice & 8 != 0,
                    ..Default::default()
                },
                ..Settings::default()
            };
            let restored: Settings =
                serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
            assert_eq!(
                (
                    restored.player_display.fullscreen_lyrics_visible,
                    restored.player_display.fullscreen_visualizer_visible,
                    restored.player_display.fullscreen_dynamic_background,
                    restored.player_display.fullscreen_background_image
                ),
                (
                    settings.player_display.fullscreen_lyrics_visible,
                    settings.player_display.fullscreen_visualizer_visible,
                    settings.player_display.fullscreen_dynamic_background,
                    settings.player_display.fullscreen_background_image
                )
            );
        }
    }

    #[test]
    fn general_preferences_default_for_existing_settings_and_round_trip_choices() {
        let mut saved = serde_json::to_value(Settings::default()).unwrap();
        saved
            .as_object_mut()
            .unwrap()
            .remove("release_check_interval_hours");
        saved.as_object_mut().unwrap().remove("starting_page");
        let previous: Settings = serde_json::from_value(saved).unwrap();
        assert_eq!(previous.application.release_check_interval_hours, 6);
        assert_eq!(previous.application.starting_page, SidebarRouteItem::Home);

        for page in SidebarRouteItem::all() {
            let mut settings = Settings::default();
            settings.application.starting_page = page;
            settings.sanitize();
            let saved = serde_json::to_vec(&settings).unwrap();
            let restored: Settings = serde_json::from_slice(&saved).unwrap();
            assert_eq!(restored.application.starting_page, page);
        }

        for hours in [1, 6, 12, 24] {
            let mut settings = Settings {
                application: ApplicationPreferences {
                    release_check_interval_hours: hours,
                    ..Default::default()
                },
                ..Settings::default()
            };
            settings.sanitize();
            let saved = serde_json::to_vec(&settings).unwrap();
            let restored: Settings = serde_json::from_slice(&saved).unwrap();
            assert_eq!(restored.application.release_check_interval_hours, hours);
        }
    }
}
