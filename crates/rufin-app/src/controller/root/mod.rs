use super::covers;
pub use super::discovery::DiscoveredServer;
pub use super::random::{RandomPlayAction, RandomPlayRequest};
use crate::external_metadata;
use crate::external_scrobbling::{self, ExternalScrobbleState};
use crate::providers::{
    JellyfinLyricsSearch, LoadedProvider, StreamingProvider, login_provider, provider_display_name,
    provider_from_saved,
};
use directories::ProjectDirs;
#[cfg(test)]
use rufin_core::ThemePreference;
use rufin_core::{
    Album, AlbumId, AppSettings, Artist, ArtistId, FolderPathItem, Genre, GenreId, HomeSection,
    HomeSectionKind, ImageRef, LibrarySourceSelection, LocalLibraryFolder, MusicFolder,
    MusicFolderId, PlaybackSettings, Playlist, PlaylistId, QueueEngine, QueueEntry, QueueEntryId,
    QueueSnapshot, RepeatMode, ServerId, ServerIdentity, Track, TrackId,
};
use rufin_playback::{
    FakePlaybackBackend, LazyGStreamerPlaybackBackend, PlaybackBackend, PlaybackCommand,
    PlaybackEvent, PlaybackState, PlaybackTrack, PreparedPlaybackItem, StreamDescriptor,
};
use rufin_provider::{
    FavoriteItemId, FolderDetail, Lyrics, MusicProvider, PagedRequest, PlaybackReport,
    PlaybackReportKind, PlaylistEntry, ProviderSession, SavedProviderSession, SearchResults,
    StreamRequest,
};
#[cfg(test)]
use rufin_provider::{LyricLine, LyricsSource, PlayedFilter};
use rufin_provider_local::{LOCAL_PROVIDER_ID, LocalProvider};
#[cfg(unix)]
use rufin_secrets::SecretServiceStore;
use rufin_secrets::{CachedSecretStore, MemorySecretStore, SecretKey, SecretStore};
#[cfg(test)]
use rufin_store::CoverCacheEntry;
use rufin_store::{
    CachedArtistDetail, CachedGenreDetail, SavedServer, ServerLocalAccess, Store, StoreError,
    SyncState,
};
use rufin_test_support::{FakeProvider, FakeScale};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::Hash;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
#[cfg(test)]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::runtime::Runtime;
use tracing::{debug, info, instrument, warn};

mod app_cache_commands;
mod auto_dj;
mod auto_dj_commands;
mod cached_library_api;
mod cached_reads;
mod controller_bootstrap;
mod controller_settings;
mod controller_startup;
mod folder_search_commands;
mod library_mutations;
mod local_source_commands;
mod lyrics_commands;
mod playback_advance;
mod playback_commands;
mod playback_queue;
mod playback_reporting;
mod playback_runtime;
mod playlist_commands;
mod queue_commands;
mod queue_mutation;
mod queue_state;
mod refresh_commands;
mod server_cache_commands;
mod server_lifecycle_commands;
mod server_local_access_commands;
mod settings_controller;
mod source_selection;
mod sync_command;
mod sync_requests;

#[cfg(test)]
mod cover_playback_tests;
#[cfg(test)]
mod lyrics_local_access_tests;
#[cfg(test)]
mod startup_sync_tests;
#[cfg(test)]
mod test_support;

pub(in crate::controller) use cached_reads::*;
pub(crate) use cached_reads::{grouped_cover_refs_for_items, track_cover_refs_for_items};
pub(in crate::controller) use controller_startup::*;
#[cfg(test)]
pub(in crate::controller) use lyrics_local_access_tests::{
    controller_from_store_for_test, saved_server, unique_test_dir,
};
pub(in crate::controller) use playback_queue::*;
#[cfg(test)]
pub(in crate::controller) use startup_sync_tests::RecordingPlaybackBackend;
pub(in crate::controller) use sync_requests::*;
#[cfg(test)]
pub(in crate::controller) use test_support::*;

const PAGE_SIZE: usize = 500;
const SNAPSHOT_GRID_LIMIT: usize = 500;
pub(in crate::controller) const SNAPSHOT_TRACK_LIMIT: usize = 25_000;
const STARTUP_CACHE_STALE_SECONDS: i64 = 24 * 60 * 60;
const GROUPED_COVER_REF_LIMIT: usize = 4;
pub(in crate::controller) const IMAGE_TAG_UNTAGGED: &str = "untagged";
const AUTO_DJ_ITEM_COUNT: usize = 5;
const AUTO_DJ_THRESHOLD: usize = 1;
const AUTO_DJ_LIBRARY_LIMIT: usize = 5_000;
const CACHE_DATABASE_FILE_NAME: &str = "rufin-cache.sqlite";
const SETTINGS_FILE_NAME: &str = "settings.json";
const STORE_DIR_NAME: &str = "store";
const COVER_CACHE_DIR_NAME: &str = "covers";
const LYRICS_CACHE_DIR_NAME: &str = "lyrics";
const PLAYBACK_CACHE_DIR_NAME: &str = "playback";
const TMP_CACHE_DIR_NAME: &str = "tmp";
const LOCAL_SOURCE_SERVER_ID: &str = "local:server:library";
#[derive(Clone, Debug)]
pub struct LibrarySnapshot {
    pub server: Option<ServerIdentity>,
    pub servers: Vec<ServerIdentity>,
    pub selected_source: Option<LibrarySourceSelection>,
    pub local_folders: Vec<LocalLibraryFolder>,
    pub server_local_access: Vec<ServerLocalAccessSnapshot>,
    pub local_access: Option<ServerLocalAccess>,
    pub local_access_status: LocalAccessStatus,
    pub music_folders: Vec<MusicFolder>,
    pub selected_music_folder_id: Option<MusicFolderId>,
    pub username: Option<String>,
    pub first_run: bool,
    pub sync_status: String,
    pub last_error: Option<String>,
    pub cached_album_count: usize,
    pub cached_track_count: usize,
    pub cached_artist_count: usize,
    pub cached_album_artist_count: usize,
    pub cached_genre_count: usize,
    pub cached_playlist_count: usize,
    pub home_sections: Vec<HomeSection>,
    pub prefetched_explore: Option<HomeSection>,
    pub albums: Vec<Album>,
    pub tracks: Vec<Track>,
    pub artists: Vec<Artist>,
    pub album_artists: Vec<Artist>,
    pub genres: Vec<Genre>,
    pub playlists: Vec<Playlist>,
    pub favorites: Vec<Track>,
    pub search: SearchResults,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerLocalAccessSnapshot {
    pub server_id: ServerId,
    pub access: Option<ServerLocalAccess>,
    pub status: LocalAccessStatus,
    pub selected_music_folder_name: Option<String>,
    pub username: Option<String>,
    pub trust_invalid_cert: bool,
    pub sync_status: String,
    pub cached_album_count: usize,
    pub cached_track_count: usize,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalAccessStatus {
    pub sample_server_path: Option<String>,
    pub sample_local_path: Option<String>,
    pub direct_match_count: usize,
    pub prefix_match_count: usize,
    pub metadata_match_count: usize,
    pub unmatched_count: usize,
    pub total_track_count: usize,
}
#[derive(Clone, Debug)]
pub struct PlaybackSnapshot {
    pub current: Option<QueueEntry>,
    pub state: PlaybackState,
    pub position_seconds: u32,
    pub position_millis: u64,
    pub duration_seconds: u32,
    pub volume: f64,
    pub muted: bool,
    pub repeat_mode: RepeatMode,
    pub shuffle_enabled: bool,
    pub auto_dj_enabled: bool,
    pub buffering_percent: Option<u8>,
    pub last_error: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LyricsSearchResult {
    pub id: u64,
    pub track_name: String,
    pub artist_name: String,
    pub album_name: String,
    pub duration_seconds: u32,
    pub synced_lyrics: Option<String>,
    pub plain_lyrics: Option<String>,
}
impl Default for PlaybackSnapshot {
    fn default() -> Self {
        Self {
            current: None,
            state: PlaybackState::Stopped,
            position_seconds: 0,
            position_millis: 0,
            duration_seconds: 0,
            volume: 1.0,
            muted: false,
            repeat_mode: RepeatMode::All,
            shuffle_enabled: false,
            auto_dj_enabled: true,
            buffering_percent: None,
            last_error: None,
        }
    }
}
impl LibrarySnapshot {
    fn first_run() -> Self {
        Self {
            server: None,
            servers: Vec::new(),
            selected_source: None,
            local_folders: Vec::new(),
            server_local_access: Vec::new(),
            local_access: None,
            local_access_status: LocalAccessStatus::default(),
            music_folders: Vec::new(),
            selected_music_folder_id: None,
            username: None,
            first_run: true,
            sync_status: String::new(),
            last_error: None,
            cached_album_count: 0,
            cached_track_count: 0,
            cached_artist_count: 0,
            cached_album_artist_count: 0,
            cached_genre_count: 0,
            cached_playlist_count: 0,
            home_sections: Vec::new(),
            prefetched_explore: None,
            albums: Vec::new(),
            tracks: Vec::new(),
            artists: Vec::new(),
            album_artists: Vec::new(),
            genres: Vec::new(),
            playlists: Vec::new(),
            favorites: Vec::new(),
            search: SearchResults::default(),
        }
    }
}
#[derive(Clone, Debug)]
pub enum ControllerEvent {
    Snapshot(Box<LibrarySnapshot>),
    HomeSectionsUpdated {
        snapshot: Box<LibrarySnapshot>,
        include_explore: bool,
    },
    HomeSectionPrefetched {
        server_id: ServerId,
        section: HomeSection,
    },
    PlaylistChanged {
        playlist_id: PlaylistId,
        snapshot: Box<LibrarySnapshot>,
    },
    FavoriteChanged {
        item_id: FavoriteItemId,
        favorite: bool,
        snapshot: Box<LibrarySnapshot>,
    },
    Queue(Box<Option<QueueSnapshot>>),
    Playback(Box<PlaybackSnapshot>),
    Lyrics(Box<Option<Lyrics>>),
    LyricsSearchResults {
        track_id: TrackId,
        artist_name: String,
        track_name: String,
        results: Vec<LyricsSearchResult>,
    },
    LyricsSaved {
        path: PathBuf,
        lyrics: Lyrics,
    },
    FolderLoaded {
        request_id: u64,
        path: Vec<FolderPathItem>,
        detail: FolderDetail,
    },
    FolderLoadFailed {
        request_id: u64,
        path: Vec<FolderPathItem>,
        error: String,
    },
    CoverReady {
        key: String,
        path: PathBuf,
    },
    ServerDiscovery {
        servers: Vec<DiscoveredServer>,
        status: String,
        running: bool,
    },
    LoginStatus(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub struct LoginRequest {
    pub provider: StreamingProvider,
    pub server_url: String,
    pub username: String,
    pub password: String,
    pub trust_invalid_cert: bool,
    pub local_access_root: Option<PathBuf>,
    pub path_replace_from: Option<String>,
}

#[derive(Clone)]
pub struct AppController {
    pub(in crate::controller) store: StoreHandle,
    pub(in crate::controller) runtime: Arc<Runtime>,
    pub(in crate::controller) secrets: Arc<dyn SecretStore>,
    settings: settings_controller::SettingsController,
    queue: Arc<Mutex<Option<QueueEngine>>>,
    playback: Arc<Mutex<Box<dyn PlaybackBackend>>>,
    playback_snapshot: Arc<Mutex<PlaybackSnapshot>>,
    auto_dj_enabled: Arc<Mutex<bool>>,
    last_progress_snapshot: Arc<Mutex<Option<(ServerId, u32)>>>,
    last_report_snapshot: Arc<Mutex<Option<(TrackId, u32)>>>,
    external_scrobble_state: Arc<Mutex<ExternalScrobbleState>>,
    pub(in crate::controller) events: Sender<ControllerEvent>,
    sync_in_flight: InFlightGuards<ServerId>,
    home_refresh_in_flight: InFlightGuards<ServerId>,
    playlist_refresh_in_flight: InFlightGuards<ServerId>,
    explore_prefetch_in_flight: InFlightGuards<ServerId>,
    pub(in crate::controller) cover_in_flight: Arc<Mutex<HashSet<String>>>,
    pub(in crate::controller) external_cover_prefetch_in_flight: Arc<Mutex<HashSet<ServerId>>>,
    pub(in crate::controller) cover_slots: Arc<(Mutex<usize>, Condvar)>,
    #[cfg(test)]
    _test_permit: Option<ControllerTestPermit>,
}
#[cfg(test)]
#[derive(Clone)]
struct ControllerTestPermit {
    _inner: Arc<ControllerTestPermitInner>,
}
#[cfg(test)]
struct ControllerTestPermitInner;
#[cfg(test)]
static CONTROLLER_TEST_GATE: OnceLock<(Mutex<bool>, Condvar)> = OnceLock::new();
#[cfg(test)]
fn controller_test_permit() -> ControllerTestPermit {
    let (lock, cvar) = CONTROLLER_TEST_GATE.get_or_init(|| (Mutex::new(false), Condvar::new()));
    let mut occupied = lock.lock().expect("controller test gate");
    while *occupied {
        occupied = cvar.wait(occupied).expect("controller test gate");
    }
    *occupied = true;
    ControllerTestPermit {
        _inner: Arc::new(ControllerTestPermitInner),
    }
}
#[cfg(test)]
impl Drop for ControllerTestPermitInner {
    fn drop(&mut self) {
        let (lock, cvar) = CONTROLLER_TEST_GATE.get_or_init(|| (Mutex::new(false), Condvar::new()));
        if let Ok(mut occupied) = lock.lock() {
            *occupied = false;
            cvar.notify_one();
        }
    }
}
#[derive(Clone)]
pub(in crate::controller) struct InFlightGuards<K>
where
    K: Eq + Hash,
{
    name: &'static str,
    inner: Arc<Mutex<HashSet<K>>>,
}
pub(in crate::controller) struct InFlightPermit<K>
where
    K: Eq + Hash,
{
    guards: InFlightGuards<K>,
    key: Option<K>,
}
impl<K> InFlightGuards<K>
where
    K: Eq + Hash,
{
    fn new(name: &'static str) -> Self {
        Self {
            name,
            inner: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    fn contains_or_blocked(&self, key: &K) -> bool {
        self.inner
            .lock()
            .map(|running| running.contains(key))
            .unwrap_or(true)
    }

    fn remove(&self, key: &K) -> Result<bool, String> {
        self.inner
            .lock()
            .map(|mut running| running.remove(key))
            .map_err(|_| self.poisoned_message())
    }

    fn poisoned_message(&self) -> String {
        format!("{} guard lock was poisoned.", self.name)
    }
}
impl<K> InFlightGuards<K>
where
    K: Clone + Eq + Hash,
{
    fn acquire(&self, key: K) -> Result<Option<InFlightPermit<K>>, String> {
        let mut running = self.inner.lock().map_err(|_| self.poisoned_message())?;
        if !running.insert(key.clone()) {
            return Ok(None);
        }
        Ok(Some(InFlightPermit {
            guards: self.clone(),
            key: Some(key),
        }))
    }
}
impl<K> Drop for InFlightPermit<K>
where
    K: Eq + Hash,
{
    fn drop(&mut self) {
        let Some(key) = self.key.take() else {
            return;
        };
        match self.guards.inner.lock() {
            Ok(mut running) => {
                running.remove(&key);
            }
            Err(_) => {
                warn!(
                    guard = self.guards.name,
                    "in-flight guard lock was poisoned during release"
                );
            }
        }
    }
}
pub(in crate::controller) struct HomeRefreshContext {
    store: StoreHandle,
    runtime: Arc<Runtime>,
    secrets: Arc<dyn SecretStore>,
    events: Sender<ControllerEvent>,
    sync_in_flight: InFlightGuards<ServerId>,
    home_refresh_in_flight: InFlightGuards<ServerId>,
}
pub(in crate::controller) struct PlaylistRefreshContext {
    store: StoreHandle,
    runtime: Arc<Runtime>,
    secrets: Arc<dyn SecretStore>,
    events: Sender<ControllerEvent>,
    sync_in_flight: InFlightGuards<ServerId>,
    playlist_refresh_in_flight: InFlightGuards<ServerId>,
}
#[derive(Clone)]
pub(in crate::controller) struct SyncContext {
    store: StoreHandle,
    runtime: Arc<Runtime>,
    secrets: Arc<dyn SecretStore>,
    events: Sender<ControllerEvent>,
    sync_in_flight: InFlightGuards<ServerId>,
    cover_in_flight: Arc<Mutex<HashSet<String>>>,
    external_cover_prefetch_in_flight: Arc<Mutex<HashSet<ServerId>>>,
    cover_slots: Arc<(Mutex<usize>, Condvar)>,
}
pub(in crate::controller) struct ExplorePrefetchContext {
    store: StoreHandle,
    runtime: Arc<Runtime>,
    secrets: Arc<dyn SecretStore>,
    events: Sender<ControllerEvent>,
    sync_in_flight: InFlightGuards<ServerId>,
    explore_prefetch_in_flight: InFlightGuards<ServerId>,
}
#[derive(Clone, Copy, Debug)]
pub(in crate::controller) enum HomeRefreshTarget {
    Section(HomeSectionKind),
}
#[derive(Clone)]
pub(in crate::controller) enum StoreHandle {
    Path {
        cache_database_path: PathBuf,
        settings_path: PathBuf,
    },
    Memory {
        store: Arc<Mutex<Store>>,
        settings: Arc<Mutex<AppSettings>>,
    },
}
impl StoreHandle {
    pub(in crate::controller) fn open_for_app() -> Result<Self, String> {
        if let Some(cache_root) = cache_dir() {
            ensure_app_cache_dirs(&cache_root)?;
        }
        let cache_database_path = app_cache_database_path();
        if let Some(parent) = cache_database_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        Store::open(&cache_database_path).map_err(|error| error.to_string())?;

        let settings_path = app_settings_path();
        let handle = Self::Path {
            cache_database_path,
            settings_path,
        };
        Ok(handle)
    }

    pub(in crate::controller) fn open_memory() -> Result<Self, String> {
        Store::open_memory()
            .map(|store| Self::Memory {
                store: Arc::new(Mutex::new(store)),
                settings: Arc::new(Mutex::new(AppSettings::default())),
            })
            .map_err(|error| error.to_string())
    }

    pub(in crate::controller) fn with_store<T>(
        &self,
        operation: impl FnOnce(&Store) -> Result<T, StoreError>,
    ) -> Result<T, String> {
        match self {
            Self::Path {
                cache_database_path,
                ..
            } => {
                let store = Store::open(cache_database_path).map_err(|error| error.to_string())?;
                operation(&store).map_err(|error| error.to_string())
            }
            Self::Memory { store, .. } => {
                let store = store
                    .lock()
                    .map_err(|_| "store lock was poisoned".to_string())?;
                operation(&store).map_err(|error| error.to_string())
            }
        }
    }

    pub(in crate::controller) fn load_settings(&self) -> Result<AppSettings, String> {
        match self {
            Self::Path { settings_path, .. } => match fs::read_to_string(settings_path) {
                Ok(value) => serde_json::from_str(&value).map_err(|error| error.to_string()),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(AppSettings::default()),
                Err(error) => Err(error.to_string()),
            },
            Self::Memory { settings, .. } => settings
                .lock()
                .map(|settings| settings.clone())
                .map_err(|_| "settings lock was poisoned".to_string()),
        }
    }

    pub(in crate::controller) fn save_settings(
        &self,
        settings: &AppSettings,
    ) -> Result<(), String> {
        match self {
            Self::Path { settings_path, .. } => {
                if let Some(parent) = settings_path.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                let value =
                    serde_json::to_string_pretty(settings).map_err(|error| error.to_string())?;
                let temp_path = settings_path.with_extension("json.tmp");
                fs::write(&temp_path, format!("{value}\n")).map_err(|error| error.to_string())?;
                restrict_settings_file(&temp_path).map_err(|error| error.to_string())?;
                fs::rename(&temp_path, settings_path).map_err(|error| error.to_string())?;
                Ok(())
            }
            Self::Memory {
                settings: stored, ..
            } => {
                let mut stored = stored
                    .lock()
                    .map_err(|_| "settings lock was poisoned".to_string())?;
                *stored = settings.clone();
                Ok(())
            }
        }
    }
}
