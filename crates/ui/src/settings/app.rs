use std::path::PathBuf;
use std::rc::Rc;

use desktop_integration::Settings as RichPresenceSettings;
use downloads::{DownloadRules, SourceDownloadSettings};
use library::{GenreId, HomeBlockKind, MusicFolderId, PlayedFilter, SourceId, StreamQuality};
use localization::{default_language_preference, sanitize_language_preference};
use lyrics::Settings as LyricsSettings;
use playback::{
    DEFAULT_AUTO_DJ_REFILL_THRESHOLD, MAX_AUTO_DJ_REFILL_THRESHOLD, MIN_AUTO_DJ_REFILL_THRESHOLD,
    PlaybackSettings, RepeatMode,
};
use secrets::SecretStorageMode;
use serde::{Deserialize, Serialize};

use super::{
    AccentPreference, ContextMenuSettings, ExternalSiteLinkSettings, FolderViewSettings,
    LayoutSettings, LibraryListKey, LibraryListSettings, LibraryListSettingsEntry, SidebarSettings,
    ThemePreference, default_library_list_settings, sanitized_window_size,
};

const DEFAULT_RANDOM_PLAY_LIMIT: usize = 100;
const MIN_RANDOM_PLAY_LIMIT: usize = 1;
const MAX_RANDOM_PLAY_LIMIT: usize = 500;
const MIN_RANDOM_PLAY_YEAR: u16 = 1850;
const MAX_RANDOM_PLAY_YEAR: u16 = 2050;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RandomPlayGenreSelection {
    pub source_id: SourceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub music_folder_id: Option<MusicFolderId>,
    pub genre_id: GenreId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct RandomPlaySettings {
    pub limit: usize,
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
        music_folder_id: Option<&MusicFolderId>,
    ) -> Option<&GenreId> {
        self.genre.as_ref().and_then(|genre| {
            (&genre.source_id == source_id && genre.music_folder_id.as_ref() == music_folder_id)
                .then_some(&genre.genre_id)
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

    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Settings {
    #[serde(default)]
    pub layout: LayoutSettings,
    #[serde(default)]
    pub sidebar: SidebarSettings,
    #[serde(default)]
    pub context_menu: ContextMenuSettings,
    pub theme_preference: ThemePreference,
    #[serde(default)]
    pub accent_preference: AccentPreference,
    #[serde(default = "default_language_preference")]
    pub language: String,
    pub private_mode: bool,
    #[serde(default)]
    pub cast_proxy_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_network_interface: Option<String>,
    pub notifications_enabled: bool,
    #[serde(default = "default_true")]
    pub control_notifications_enabled: bool,
    #[serde(default = "default_true")]
    pub release_notifications_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_notification_seen_version: Option<String>,
    #[serde(default)]
    pub automatic_updates_enabled: bool,
    #[serde(default = "legacy_secret_storage_mode")]
    pub secret_storage_mode: SecretStorageMode,
    #[serde(flatten)]
    pub lyrics: LyricsSettings,
    #[serde(default = "default_true")]
    pub external_metadata_enabled: bool,
    #[serde(default)]
    pub external_site_links: ExternalSiteLinkSettings,
    #[serde(default)]
    pub prefer_server_playlist_covers: bool,
    #[serde(default = "default_true")]
    pub show_downloaded_badges: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub downloads: Vec<SourceDownloadSettings>,
    #[serde(default)]
    pub seekbar_waveform_enabled: bool,
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
    #[serde(default = "default_auto_dj_refill_threshold")]
    pub auto_dj_refill_threshold: u8,
    #[serde(default)]
    pub playback: PlaybackSettings,
    #[serde(default, skip_serializing_if = "RandomPlaySettings::is_default")]
    pub random_play: RandomPlaySettings,
    #[serde(default)]
    pub home_blocks: Vec<HomeBlockKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_width: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_height: Option<i32>,
    #[serde(default = "default_lyrics_panel_visible")]
    pub lyrics_panel_visible: bool,
    #[serde(default)]
    pub visualizer_panel_visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_lyrics_height: Option<i32>,
    #[serde(default)]
    pub library_lists: Vec<LibraryListSettingsEntry>,
    #[serde(default, skip_serializing_if = "FolderViewSettings::is_default")]
    pub folder_view: FolderViewSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            layout: LayoutSettings::default(),
            sidebar: SidebarSettings::default(),
            context_menu: ContextMenuSettings::default(),
            theme_preference: ThemePreference::System,
            accent_preference: AccentPreference::System,
            language: default_language_preference(),
            private_mode: false,
            cast_proxy_enabled: false,
            cast_network_interface: None,
            notifications_enabled: false,
            control_notifications_enabled: true,
            release_notifications_enabled: true,
            release_notification_seen_version: None,
            automatic_updates_enabled: false,
            secret_storage_mode: SecretStorageMode::default(),
            lyrics: LyricsSettings::default(),
            external_metadata_enabled: true,
            external_site_links: ExternalSiteLinkSettings::default(),
            prefer_server_playlist_covers: false,
            show_downloaded_badges: true,
            downloads: Vec::new(),
            seekbar_waveform_enabled: false,
            tray_enabled: false,
            keep_running_after_close: false,
            start_minimized: false,
            type_to_search_enabled: true,
            rich_presence: RichPresenceSettings::default(),
            lastfm_api_key: String::new(),
            auto_dj_enabled: false,
            shuffle_enabled: false,
            repeat_mode: RepeatMode::Off,
            auto_dj_refill_threshold: DEFAULT_AUTO_DJ_REFILL_THRESHOLD,
            playback: PlaybackSettings::default(),
            random_play: RandomPlaySettings::default(),
            home_blocks: default_home_blocks(),
            window_width: None,
            window_height: None,
            lyrics_panel_visible: true,
            visualizer_panel_visible: false,
            queue_lyrics_height: None,
            library_lists: default_library_list_settings(),
            folder_view: FolderViewSettings::default(),
        }
    }
}

impl Settings {
    pub fn allows_notifications(&self) -> bool {
        self.notifications_enabled
    }

    pub fn shows_external_site_links(&self) -> bool {
        self.external_site_links.enabled
    }

    pub fn allows_external_metadata_lookup(&self) -> bool {
        self.external_metadata_enabled && !self.private_mode
    }

    pub fn sanitize(&mut self) {
        self.rich_presence.sanitize();
        self.playback.sanitize();
        self.random_play.sanitize();
        self.lyrics.sanitize();
        self.auto_dj_refill_threshold = self
            .auto_dj_refill_threshold
            .clamp(MIN_AUTO_DJ_REFILL_THRESHOLD, MAX_AUTO_DJ_REFILL_THRESHOLD);
        self.lastfm_api_key = self.lastfm_api_key.trim().to_string();
        self.cast_network_interface = self
            .cast_network_interface
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        self.language = sanitize_language_preference(&self.language);
        self.release_notification_seen_version = self
            .release_notification_seen_version
            .as_deref()
            .map(str::trim)
            .filter(|version| !version.is_empty())
            .map(str::to_string);
        self.layout.sanitize();
        self.sidebar.sanitize();
        self.context_menu.sanitize();
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
        sanitize_home_blocks(&mut self.home_blocks);
        migrate_library_lists(&mut self.library_lists);
        self.folder_view.sanitize();
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

    pub fn download_quality(&self, source_id: &SourceId) -> StreamQuality {
        self.download_settings(source_id).quality
    }

    pub fn download_directory(&self, source_id: &SourceId) -> Option<PathBuf> {
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

    pub fn set_download_quality(&mut self, source_id: SourceId, quality: StreamQuality) -> bool {
        self.update_download_settings(source_id, |settings| settings.quality = quality)
    }

    pub fn set_download_directory(
        &mut self,
        source_id: SourceId,
        directory: Option<PathBuf>,
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

pub trait SettingsPort {
    fn load(&self) -> Settings;
    fn save(&self, settings: &Settings) -> Result<Settings, String>;
}

pub type SettingsHandle = Rc<dyn SettingsPort>;

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
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            entry.directory = None;
        }
    }
    downloads.retain(|entry| !entry.is_default());
    downloads.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    downloads.dedup_by(|left, right| left.source_id == right.source_id);
}
