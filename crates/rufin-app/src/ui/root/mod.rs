#[path = "../artist.rs"]
mod artist;
#[path = "../cards.rs"]
mod cards;
#[path = "../chrome.rs"]
mod chrome;
#[path = "../discord.rs"]
mod discord;
#[path = "../favorites.rs"]
mod favorites;
#[path = "../folders.rs"]
mod folders;
#[path = "../fullscreen_player.rs"]
mod fullscreen_player;
#[path = "../home.rs"]
mod home;
#[path = "../layout.rs"]
mod layout;
#[path = "../library/mod.rs"]
mod library;
#[path = "../local_access_mapping.rs"]
mod local_access_mapping;
#[path = "../login.rs"]
mod login;
#[cfg(unix)]
#[path = "../mpris.rs"]
mod mpris;
#[path = "../navigation.rs"]
mod navigation;
#[path = "../paging.rs"]
mod paging;
#[path = "../player.rs"]
mod player;
#[path = "../player_icons.rs"]
mod player_icons;
#[path = "../preferences.rs"]
mod preferences;
#[path = "../queue.rs"]
mod queue;
#[path = "../random_play.rs"]
mod random_play;
#[path = "../right_panel.rs"]
mod right_panel;
#[path = "../settings_persistence.rs"]
mod settings_persistence;
#[path = "../source_selector.rs"]
mod source_selector;

use crate::controller::{
    AppController, ControllerEvent, DiscoveredServer, LibrarySnapshot, LyricsSearchResult,
    PlaybackSnapshot, grouped_cover_refs_for_items, track_cover_refs_for_items,
};
use crate::external_metadata;
use crate::i18n::{self, tr};
use crate::lyrics::{LyricsPane, next_lyrics_line_start_after};
use adw::prelude::*;
use chrome::{build_content_chrome, build_main_area};
use discord::DiscordPresence;
use favorites::{
    FavoriteControlKey, FavoriteControls, album_favorite_key, artist_favorite_key,
    clear_favorite_controls, favorite_change_needs_route_render, favorite_control_key,
    merge_favorite_snapshot, register_favorite_control, track_favorite_key,
    unregister_favorite_control, update_favorite_controls,
};
use fullscreen_player::{
    FullscreenPlayerParts, build_fullscreen_player, connect_fullscreen_player_controls,
};
use gdk_pixbuf::Pixbuf;
use gtk::gdk::prelude::GdkCairoContextExt;
use gtk::gio;
use gtk::glib;
use layout::{
    COMPACT_RAIL_WIDTH, HOME_ALBUM_GAP, NORMAL_SIDEBAR_WIDTH, PRIMARY_ROUTE_MARGIN_END,
    PRIMARY_ROUTE_MARGIN_START, ResolvedLayout, resolve_layout, route_content_width,
};
#[cfg(unix)]
use mpris::install_mpris;
#[cfg(unix)]
use mpris_server::Player as MprisPlayer;
use navigation::{
    build_compact_navigation, build_normal_navigation, rebuild_navigation, sidebar_history_button,
    update_navigation_selection,
};
use paging::{PagedGridCursor, connect_paged_grid_loader, finish_grid_page};
use player::{PlayerControls, build_bottom_player, connect_player_controls};
use preferences::{present_library_preferences_dialog, present_preferences_dialog};
use queue::connect_queue_panel_controls;
use right_panel::{apply_lyrics_panel_visibility, build_right_panel, connect_queue_lyrics_split};
use rufin_core::{
    Album, AlbumId, AppSettings, Artist, ArtistId, DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH,
    FolderPathItem, Genre, HomeSection, HomeSectionKind, ImageRef, LeftSidebarMode, LibraryField,
    LibraryLayout, LibraryListKey, LibraryListSettings, Playlist, PlaylistId, QueueEntry,
    QueueSnapshot, RightSidebarMode, Route, RouteStack, SearchKind, SidebarRouteItem, Track,
    TrackId, TrackSortKey, TrackTableColumn, TrackTableSettings, format_duration,
    sanitized_window_size,
};
use rufin_playback::PlaybackState;
use rufin_provider::{FavoriteItemId, FolderDetail, Lyrics, LyricsSource, PlaylistEntry};
use rufin_store::{CachedGenreDetail, image_cache_key};
use rufin_test_support::FakeScale;
use source_selector::{ServerSelector, build_server_selector};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

mod album_detail_view;
mod build;
mod cover;
mod cover_startup;
mod empty_states;
mod favorite_controls;
mod folder_route_state;
mod genre_detail_view;
mod grouped_detail_view;
mod home_refresh;
mod home_route_refresh;
mod home_visible_sections;
mod layout_rendering;
mod lyrics_highlight_timers;
mod lyrics_panel;
mod lyrics_playback_state;
mod new_playlist_dialog;
mod perf_recording;
mod playlist_detail_view;
mod playlist_rename_dialog;
mod responsive_layout_state;
mod responsive_route_render;
mod route_navigation;
mod route_rendering;
mod search_view;
mod shell_navigation;
mod sidebar_route_controls;
mod startup_reveal;
mod track_table;
mod track_table_popover;

#[cfg(test)]
mod shell_tests;

pub(in crate::ui) use build::*;
pub(in crate::ui) use cover::{
    CoverBinding, CoverDecodeJob, CoverDecodePriority, CoverPathLookupIntent, CoverWarmJob,
    DecodedCover, DecodedCoverOrderEntry, FirstRunCoverPrimeJob,
};
pub(in crate::ui) use cover_startup::*;
pub(in crate::ui) use home_refresh::*;
pub(in crate::ui) use layout_rendering::*;
pub(in crate::ui) use shell_navigation::*;

pub(in crate::ui) const GRID_ROUTE_PAGE_SIZE: usize = 16;
pub(in crate::ui) const TRACK_ROUTE_PAGE_SIZE: usize = 64;
pub(in crate::ui) const CONTEXT_MENU_PLAYLIST_LIMIT: usize = 100;
pub(in crate::ui) const GRID_COVER_SIZE: u32 = 256;
pub(in crate::ui) const DETAIL_COVER_SIZE: u32 = 512;
pub(in crate::ui) const THUMB_COVER_SIZE: u32 = 96;
pub(in crate::ui) const IMAGE_TAG_UNTAGGED: &str = "untagged";
pub(in crate::ui) const DECODED_COVER_CACHE_LIMIT: usize = 3_072;
pub(in crate::ui) const DECODED_COVER_CACHE_SOFT_BYTES: usize = 128 * 1024 * 1024;
pub(in crate::ui) const COVER_WARM_BATCH_SIZE: usize = 3;
pub(in crate::ui) const COVER_PATH_LOOKUP_MAX_IN_FLIGHT: usize = 3;
pub(in crate::ui) const COVER_WARM_INITIAL_DELAY_MS: u64 = 250;
pub(in crate::ui) const COVER_WARM_INTERVAL_MS: u64 = 32;
pub(in crate::ui) const COVER_WARM_SCROLL_PAUSE_MS: u64 = 1_500;
pub(in crate::ui) const COVER_DECODE_MAX_IN_FLIGHT: usize = 3;
pub(in crate::ui) const STARTUP_COVER_WARM_DELAY_MS: u64 = 1_500;
pub(in crate::ui) const STARTUP_COVER_WARM_BATCH_SIZE: usize = 1;
pub(in crate::ui) const STARTUP_COVER_WARM_INTERVAL_MS: u64 = 80;
pub(in crate::ui) const UI_PERF_MANUAL_SCROLL_IDLE_MS: u64 = 750;
pub(in crate::ui) const UI_PERF_TRACK_ROW_BIND_SLOW_US: u64 = 4_000;
pub(in crate::ui) const STARTUP_ROUTE_REVEAL_MIN_MS: u64 = 320;
pub(in crate::ui) const STARTUP_ROUTE_REVEAL_MAX_MS: u64 = 3_000;
pub(in crate::ui) const STARTUP_ROUTE_REVEAL_POLL_MS: u64 = 32;
pub(in crate::ui) const STARTUP_HOME_SECTION_COVER_LIMIT: usize = 4;
pub(in crate::ui) const STARTUP_GRID_COVER_LIMIT: usize = 48;
pub(in crate::ui) const STARTUP_VISIBLE_TRACK_COVER_LIMIT: usize = TRACK_ROUTE_PAGE_SIZE * 4;
pub(in crate::ui) const STARTUP_CACHED_COVER_PRIME_LIMIT: usize = 3_072;
pub(in crate::ui) const FIRST_RUN_COVER_PRIME_TIMEOUT_MS: u64 = 8_000;
pub(in crate::ui) const FIRST_RUN_COVER_PRIME_POLL_MS: u64 = 33;
pub(in crate::ui) const FIRST_RUN_HOME_SECTION_COVER_LIMIT: usize = 8;
pub(in crate::ui) const FIRST_RUN_GRID_COVER_PRIME_LIMIT: usize = 192;
pub(in crate::ui) const LIBRARY_SYNC_COMPLETE_STATUS: &str = "Library sync complete";
pub(in crate::ui) const LIBRARY_PREPARING_STATUS: &str = "Preparing library...";
pub(in crate::ui) const FAVORITE_EMPTY_GLYPH: &str = "♡";
pub(in crate::ui) const FAVORITE_FILLED_GLYPH: &str = "♥";
pub(in crate::ui) const PLAYLIST_ENTRY_DRAG_WIDTH: i32 = 18;
pub(in crate::ui) const PLAYLIST_ENTRY_NUMBER_WIDTH: i32 = 24;
pub(in crate::ui) const PLAYLIST_ENTRY_COVER_WIDTH: i32 = 36;
pub(in crate::ui) const PLAYLIST_ENTRY_DURATION_WIDTH: i32 = 64;
pub(in crate::ui) const PLAYLIST_ENTRY_REMOVE_WIDTH: i32 = 34;
pub(in crate::ui) const PLAYLIST_ENTRY_COLUMN_GAP: i32 = 8;
pub(in crate::ui) const PLAYLIST_ENTRY_TEXT_COLUMN_GAP: i32 = 16;
pub(in crate::ui) const PLAYLIST_ENTRY_ALBUM_COLUMN_WIDTH: i32 = 120;
pub(in crate::ui) const PLAYLIST_ENTRY_NUMBER_XALIGN: f32 = 0.35;
pub(in crate::ui) const PLAYLIST_ENTRY_TITLE_MAX_CHARS: i32 = 44;
pub(in crate::ui) const PLAYLIST_ENTRY_ALBUM_MAX_CHARS: i32 = 18;
pub(in crate::ui) const PLAY_NEXT_ICON: &str = "view-sort-ascending-symbolic";
pub(in crate::ui) const PLAY_LATER_ICON: &str = "view-sort-descending-symbolic";
pub(in crate::ui) const RESPONSIVE_RENDER_DELAY_MS: u64 = 16;
static HOME_SHOWCASE_COUNTER: AtomicU64 = AtomicU64::new(0);
#[derive(Clone, Debug)]
pub struct AppOptions {
    pub fake_scale: Option<FakeScale>,
    pub smoke_exit_ms: Option<u64>,
    pub ui_perf_run: bool,
    pub ui_perf_observe: bool,
    pub ui_perf_max_gap_ms: u64,
    pub ui_perf_route_ms: u64,
    pub ui_perf_duration_ms: u64,
    pub ui_perf_asset_ms: u64,
    pub ui_perf_output: Option<PathBuf>,
}
pub(in crate::ui) struct AppState {
    routes: RefCell<RouteStack>,
    settings: RefCell<AppSettings>,
    resolved_left_sidebar: Cell<LeftSidebarMode>,
    resolved_right_sidebar: Cell<RightSidebarMode>,
    resolved_right_sidebar_width: Cell<i32>,
    main_content_width: Cell<i32>,
    library: RefCell<LibrarySnapshot>,
    queue: RefCell<Option<QueueSnapshot>>,
    player: RefCell<PlaybackSnapshot>,
    lyrics: RefCell<Option<Lyrics>>,
    lyrics_track_id: RefCell<Option<rufin_core::TrackId>>,
    lyrics_auto_search_attempted: RefCell<HashSet<rufin_core::TrackId>>,
    lyrics_search_dialog: RefCell<Option<LyricsSearchDialog>>,
    lyrics_timing_generation: Cell<u64>,
    lyrics_timing_source: RefCell<Option<glib::SourceId>>,
    #[cfg(unix)]
    mpris_player: RefCell<Option<Rc<MprisPlayer>>>,
    discord_presence: RefCell<DiscordPresence>,
    updating_player_controls: Cell<bool>,
    seek_preview_seconds: Cell<Option<u32>>,
    seek_generation: Cell<u64>,
    queue_filter: RefCell<String>,
    lyrics_panel_visible: Cell<bool>,
    fullscreen_player_visible: Cell<bool>,
    queue_lyrics_position_save_suppressed: Rc<Cell<u32>>,
    responsive_render_queued: Cell<bool>,
    responsive_route_render_width: Cell<i32>,
    card_grid_columns: Cell<usize>,
    home_section_state: RefCell<HashMap<HomeSectionKind, HomeSectionState>>,
    home_section_views: RefCell<HashMap<HomeSectionKind, HomeSectionView>>,
    prefetched_explore: RefCell<Option<PrefetchedHomeSection>>,
    home_refresh_started_for_visit: Cell<bool>,
    playlist_refresh_started_for_visit: Cell<bool>,
    home_showcase_seed: Cell<u64>,
    startup_route_revealed: Cell<bool>,
    startup_cover_prime_generation: Cell<u64>,
    startup_cover_prime_pending: RefCell<HashSet<String>>,
    first_run_connection_pending: Cell<bool>,
    first_run_connection_ready: Cell<bool>,
    first_run_cover_prime_generation: Cell<u64>,
    first_run_cover_prime_pending: RefCell<HashSet<String>>,
    discovered_servers: RefCell<Vec<DiscoveredServer>>,
    server_discovery_status: RefCell<String>,
    server_discovery_running: Cell<bool>,
    server_discovery_started: Cell<bool>,
    cover_bindings: RefCell<HashMap<String, Vec<CoverBinding>>>,
    cover_path_lookups: RefCell<HashMap<String, CoverPathLookupIntent>>,
    cover_decodes: RefCell<HashSet<String>>,
    cover_decode_queue: RefCell<VecDeque<CoverDecodeJob>>,
    cover_warm_generation: Cell<u64>,
    cover_warm_paused_until: Cell<Option<Instant>>,
    startup_cover_warm_generation: Cell<u64>,
    route_cover_gate_started: RefCell<HashMap<&'static str, Instant>>,
    route_cover_gate_queued: RefCell<HashSet<&'static str>>,
    route_cover_gate_timed_out: RefCell<HashSet<&'static str>>,
    decoded_covers: RefCell<HashMap<String, DecodedCover>>,
    decoded_cover_order: RefCell<VecDeque<DecodedCoverOrderEntry>>,
    decoded_cover_bytes: Cell<usize>,
    decoded_cover_touch: Cell<u64>,
    favorite_controls: FavoriteControls,
    folder_request_generation: Cell<u64>,
    folder_state: RefCell<FolderRouteState>,
    perf: Option<Rc<UiPerfMonitor>>,
}
#[derive(Clone)]
pub(in crate::ui) struct LyricsSearchDialog {
    dialog: adw::Dialog,
    track_id: rufin_core::TrackId,
    artist_entry: gtk::Entry,
    title_entry: gtk::Entry,
    search_button: gtk::Button,
    list: gtk::ListBox,
    status: gtk::Label,
}
#[derive(Clone)]
pub(in crate::ui) struct ArtworkTile {
    area: gtk::DrawingArea,
    size: Rc<Cell<i32>>,
    seed: Rc<Cell<u32>>,
    pixbuf: Rc<RefCell<Option<Pixbuf>>>,
    generation: Rc<Cell<u64>>,
}
#[derive(Clone)]
pub(in crate::ui) struct ArtworkTileWeak {
    area: glib::WeakRef<gtk::DrawingArea>,
    size: Rc<Cell<i32>>,
    seed: Rc<Cell<u32>>,
    pixbuf: Rc<RefCell<Option<Pixbuf>>>,
    generation: Rc<Cell<u64>>,
}
pub(in crate::ui) struct HomeSectionState {
    page_start: usize,
    page_size: usize,
}
#[derive(Clone)]
pub(in crate::ui) struct HomeSectionView {
    root: gtk::Widget,
    row: gtk::Box,
    previous: gtk::Button,
    next: gtk::Button,
}
#[derive(Clone)]
pub(in crate::ui) struct PrefetchedHomeSection {
    server_id: rufin_core::ServerId,
    section: HomeSection,
}
#[derive(Clone, Default)]
pub(in crate::ui) struct FolderRouteState {
    request_id: u64,
    path: Vec<FolderPathItem>,
    loading: bool,
    detail: Option<FolderDetail>,
    error: Option<String>,
}
#[derive(Clone, Copy)]
pub(in crate::ui) struct TrackTableOptions {
    paging: Option<(usize, usize)>,
    expand: bool,
    max_visible_rows: Option<usize>,
    favorite_first: bool,
}
pub(in crate::ui) struct UiPerfOptions {
    max_gap_ms: u64,
    route_ms: u64,
    duration_ms: u64,
    asset_ms: u64,
    require_assets: bool,
    terminal_events: bool,
    observe_scroll: bool,
    output: Option<PathBuf>,
}
pub(in crate::ui) struct UiPerfMonitor {
    options: UiPerfOptions,
    started_at: Instant,
    inner: RefCell<UiPerfInner>,
}
#[derive(Default)]
pub(in crate::ui) struct UiPerfInner {
    ticks: usize,
    max_gap_ms: u64,
    max_idle_gap_ms: u64,
    over_budget_ticks: usize,
    over_budget_idle_ticks: usize,
    route_renders: Vec<UiPerfRouteRender>,
    route_scrolls: Vec<UiPerfRouteScroll>,
    active_scroll: Option<UiPerfActiveScroll>,
    last_route_hint: Option<String>,
    gap_samples: Vec<UiPerfGapSample>,
    cover_pending: HashMap<String, Instant>,
    cover_path_ready: HashMap<String, u64>,
    cover_decode_started: HashMap<String, u64>,
    cover_latencies: Vec<UiPerfAssetLatency>,
    max_cover_latency_ms: u64,
    over_budget_assets: usize,
    coverless_tiles: usize,
    cover_bind_requests: usize,
    cover_cache_hits: usize,
    cover_ready_events: usize,
    cover_decode_ok: usize,
    cover_decode_error: usize,
    cover_stale_ignored: usize,
    track_row_binds: HashMap<&'static str, UiPerfBindStats>,
    track_row_contracts: Vec<UiPerfTrackRowContractSample>,
    tracks_row_contract_samples: usize,
    tracks_row_contract_failures: usize,
}
#[derive(Default)]
pub(in crate::ui) struct UiPerfBindStats {
    samples: usize,
    total_us: u64,
    max_us: u64,
    slow_samples: usize,
}
pub(in crate::ui) struct UiPerfTrackRowContract {
    scenario: &'static str,
    visible_start: usize,
    visible_end: usize,
    ready: usize,
    coverless: usize,
    pending: usize,
    missing: usize,
}
pub(in crate::ui) struct UiPerfTrackRowContractSample {
    scenario: &'static str,
    visible_start: usize,
    visible_end: usize,
    ready: usize,
    coverless: usize,
    pending: usize,
    missing: usize,
    failed: bool,
}
pub(in crate::ui) struct UiPerfActiveScroll {
    route: String,
    scenario: &'static str,
    started_at: Instant,
    last_step_at: Instant,
    steps: usize,
    max_gap_ms: u64,
    over_budget_ticks: usize,
    max_adjustment: f64,
    min_value: f64,
    max_value: f64,
    covers_ready_at_start: usize,
    decodes_at_start: usize,
}
pub(in crate::ui) struct UiPerfRouteRender {
    route: String,
    elapsed_ms: u64,
}
pub(in crate::ui) struct UiPerfRouteScroll {
    route: String,
    scenario: &'static str,
    elapsed_ms: u64,
    steps: usize,
    max_gap_ms: u64,
    over_budget_ticks: usize,
    max_adjustment: f64,
    min_value: f64,
    max_value: f64,
    covers_ready: usize,
    decoded_covers: usize,
}
pub(in crate::ui) struct UiPerfGapSample {
    phase: &'static str,
    route: String,
    scenario: &'static str,
    elapsed_ms: u64,
    gap_ms: u64,
}
pub(in crate::ui) struct UiPerfAssetLatency {
    key: String,
    elapsed_ms: u64,
    path_ready_ms: Option<u64>,
    queue_wait_ms: Option<u64>,
    decode_ms: Option<u64>,
}
#[derive(Clone, Copy)]
pub(in crate::ui) enum UiPerfScenario {
    HumanScroll,
    FastScroll,
    FullSweep,
    DragSweep,
}
impl UiPerfScenario {
    fn name(self) -> &'static str {
        match self {
            Self::HumanScroll => "human_scroll",
            Self::FastScroll => "fast_scroll",
            Self::FullSweep => "full_sweep",
            Self::DragSweep => "drag_sweep",
        }
    }
}
pub(in crate::ui) struct GroupedDetailData {
    title: String,
    image_ref: Option<ImageRef>,
    cover_refs: Vec<ImageRef>,
    seed: u32,
    summary: String,
    tracks: Vec<Track>,
    table_context: &'static str,
}
pub(in crate::ui) struct Shell {
    state: AppState,
    controller: AppController,
    application: adw::Application,
    window: adw::ApplicationWindow,
    root_stack: gtk::Stack,
    app_root: gtk::Box,
    app_content_stack: gtk::Stack,
    login_host: gtk::Box,
    normal_nav: gtk::Box,
    compact_nav: gtk::Box,
    server_selector: ServerSelector,
    route_title: adw::WindowTitle,
    route_host: gtk::Box,
    main_menu: gtk::MenuButton,
    normal_back_button: gtk::Button,
    normal_forward_button: gtk::Button,
    compact_back_button: gtk::Button,
    compact_forward_button: gtk::Button,
    right_panel_slot: gtk::ScrolledWindow,
    right_panel: gtk::Box,
    queue_panel: gtk::Box,
    queue_search: gtk::SearchEntry,
    queue_clear_button: gtk::Button,
    queue_lyrics_split: gtk::Paned,
    lyrics_pane: LyricsPane,
    fullscreen_player: FullscreenPlayerParts,
    player_controls: PlayerControls,
}
pub fn build(app: &adw::Application, options: AppOptions) {
    install_css();

    let loaded_at = std::time::Instant::now();
    let (controller, events, library, queue, player) = AppController::bootstrap(options.fake_scale);
    let settings = controller.load_settings();
    info!(
        cached_albums = library.cached_album_count,
        cached_tracks = library.cached_track_count,
        preloaded_albums = library.albums.len(),
        preloaded_tracks = library.tracks.len(),
        first_run = library.first_run,
        elapsed_ms = loaded_at.elapsed().as_millis(),
        "loaded cached music library snapshot"
    );
    let first_run = library.first_run;
    let perf_observe = options.ui_perf_observe && !options.ui_perf_run;
    let perf_enabled = options.ui_perf_run || perf_observe;
    let defer_initial_route = !first_run;
    let language_preference = i18n::effective_language_preference(&settings.language);
    i18n::set_language_preference(&language_preference);
    let perf_requires_assets =
        perf_enabled && options.fake_scale.is_none() && library_has_image_refs(&library);
    let prefetched_explore = prefetched_explore_from_snapshot(&library);

    let state = AppState {
        routes: RefCell::new(RouteStack::new(Route::Home)),
        settings: RefCell::new(settings.clone()),
        resolved_left_sidebar: Cell::new(LeftSidebarMode::Full),
        resolved_right_sidebar: Cell::new(RightSidebarMode::Hidden),
        resolved_right_sidebar_width: Cell::new(0),
        main_content_width: Cell::new(1),
        library: RefCell::new(library),
        queue: RefCell::new(queue),
        player: RefCell::new(player),
        lyrics: RefCell::new(None),
        lyrics_track_id: RefCell::new(None),
        lyrics_auto_search_attempted: RefCell::new(HashSet::new()),
        lyrics_search_dialog: RefCell::new(None),
        lyrics_timing_generation: Cell::new(0),
        lyrics_timing_source: RefCell::new(None),
        #[cfg(unix)]
        mpris_player: RefCell::new(None),
        discord_presence: RefCell::new(DiscordPresence::new()),
        updating_player_controls: Cell::new(false),
        seek_preview_seconds: Cell::new(None),
        seek_generation: Cell::new(0),
        queue_filter: RefCell::new(String::new()),
        lyrics_panel_visible: Cell::new(settings.lyrics_panel_visible),
        fullscreen_player_visible: Cell::new(false),
        queue_lyrics_position_save_suppressed: Rc::new(Cell::new(0)),
        responsive_render_queued: Cell::new(false),
        responsive_route_render_width: Cell::new(0),
        card_grid_columns: Cell::new(0),
        home_section_state: RefCell::new(HashMap::new()),
        home_section_views: RefCell::new(HashMap::new()),
        prefetched_explore: RefCell::new(prefetched_explore),
        home_refresh_started_for_visit: Cell::new(false),
        playlist_refresh_started_for_visit: Cell::new(false),
        home_showcase_seed: Cell::new(next_home_showcase_seed()),
        startup_route_revealed: Cell::new(!defer_initial_route),
        startup_cover_prime_generation: Cell::new(0),
        startup_cover_prime_pending: RefCell::new(HashSet::new()),
        first_run_connection_pending: Cell::new(false),
        first_run_connection_ready: Cell::new(false),
        first_run_cover_prime_generation: Cell::new(0),
        first_run_cover_prime_pending: RefCell::new(HashSet::new()),
        discovered_servers: RefCell::new(Vec::new()),
        server_discovery_status: RefCell::new("Searching will start automatically".to_string()),
        server_discovery_running: Cell::new(false),
        server_discovery_started: Cell::new(false),
        cover_bindings: RefCell::new(HashMap::new()),
        cover_path_lookups: RefCell::new(HashMap::new()),
        cover_decodes: RefCell::new(HashSet::new()),
        cover_decode_queue: RefCell::new(VecDeque::new()),
        cover_warm_generation: Cell::new(0),
        cover_warm_paused_until: Cell::new(None),
        startup_cover_warm_generation: Cell::new(0),
        route_cover_gate_started: RefCell::new(HashMap::new()),
        route_cover_gate_queued: RefCell::new(HashSet::new()),
        route_cover_gate_timed_out: RefCell::new(HashSet::new()),
        decoded_covers: RefCell::new(HashMap::new()),
        decoded_cover_order: RefCell::new(VecDeque::new()),
        decoded_cover_bytes: Cell::new(0),
        decoded_cover_touch: Cell::new(0),
        favorite_controls: RefCell::new(HashMap::new()),
        folder_request_generation: Cell::new(0),
        folder_state: RefCell::new(FolderRouteState::default()),
        perf: perf_enabled.then(|| {
            Rc::new(UiPerfMonitor::new(UiPerfOptions {
                max_gap_ms: options.ui_perf_max_gap_ms,
                route_ms: options.ui_perf_route_ms,
                duration_ms: options.ui_perf_duration_ms.max(1_000),
                asset_ms: options.ui_perf_asset_ms,
                require_assets: perf_requires_assets,
                terminal_events: options.ui_perf_run,
                observe_scroll: perf_observe,
                output: options.ui_perf_output.clone().or_else(|| {
                    if perf_observe {
                        default_ui_perf_output_path("rufin-ui-observe")
                    } else {
                        default_ui_perf_output_path("rufin-ui-perf")
                    }
                }),
            }))
        }),
    };

    let (window_width, window_height) =
        initial_window_size(settings.window_width, settings.window_height);
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Rufin")
        .default_width(window_width)
        .default_height(window_height)
        .build();

    let root_stack = gtk::Stack::new();
    root_stack.add_css_class("app-root");
    root_stack.set_hexpand(true);
    root_stack.set_vexpand(true);

    let app_root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    app_root.add_css_class("app-root");
    app_root.set_hexpand(true);
    app_root.set_vexpand(true);

    let app_content_stack = gtk::Stack::new();
    app_content_stack.add_css_class("app-content-stack");
    app_content_stack.set_hhomogeneous(false);
    app_content_stack.set_vhomogeneous(false);
    app_content_stack.set_interpolate_size(false);
    app_content_stack.set_hexpand(true);
    app_content_stack.set_vexpand(true);

    let login_host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    login_host.add_css_class("login-root");
    login_host.set_hexpand(true);
    login_host.set_vexpand(true);

    let upper = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    upper.set_hexpand(true);
    upper.set_vexpand(true);

    let normal_nav = gtk::Box::new(gtk::Orientation::Vertical, 10);
    normal_nav.add_css_class("wide-sidebar");
    normal_nav.set_hexpand(false);
    normal_nav.set_width_request(NORMAL_SIDEBAR_WIDTH);

    let compact_nav = gtk::Box::new(gtk::Orientation::Vertical, 3);
    compact_nav.add_css_class("compact-rail");
    compact_nav.set_hexpand(false);
    compact_nav.set_width_request(COMPACT_RAIL_WIDTH);
    let server_selector = build_server_selector();

    let normal_back_button = sidebar_history_button("go-previous-symbolic", "Back");
    let normal_forward_button = sidebar_history_button("go-next-symbolic", "Forward");
    let compact_back_button = sidebar_history_button("go-previous-symbolic", "Back");
    let compact_forward_button = sidebar_history_button("go-next-symbolic", "Forward");
    let main_area_parts = build_main_area();
    let main_area = main_area_parts.root;
    let route_title = main_area_parts.route_title;
    let route_host = main_area_parts.route_host;

    let right_panel_parts = build_right_panel();
    let right_panel = right_panel_parts.root;
    let queue_panel = right_panel_parts.queue_panel;
    let queue_search = right_panel_parts.queue_search;
    let queue_clear_button = right_panel_parts.queue_clear_button;
    let queue_lyrics_split = right_panel_parts.queue_lyrics_split;
    let lyrics_pane = right_panel_parts.lyrics_pane;

    let content_chrome = build_content_chrome(&main_area, &right_panel);
    let main_menu = content_chrome.main_menu;
    let right_panel_slot = content_chrome.right_panel_slot;
    let fullscreen_player = build_fullscreen_player();
    let player_controls = build_bottom_player();

    upper.append(&normal_nav);
    upper.append(&compact_nav);
    upper.append(&content_chrome.root);

    app_content_stack.add_named(&upper, Some("main"));
    app_content_stack.add_named(&fullscreen_player.root, Some("fullscreen-player"));

    app_root.append(&app_content_stack);
    app_root.append(&player_controls.root);

    root_stack.add_named(&login_host, Some("login"));
    root_stack.add_named(&app_root, Some("app"));
    window.set_content(Some(&root_stack));

    let shell = Rc::new(Shell {
        state,
        controller,
        application: app.clone(),
        window,
        root_stack,
        app_root,
        app_content_stack,
        login_host,
        normal_nav,
        compact_nav,
        server_selector,
        route_title,
        route_host,
        main_menu: main_menu.clone(),
        normal_back_button,
        normal_forward_button,
        compact_back_button,
        compact_forward_button,
        right_panel_slot,
        right_panel,
        queue_panel,
        queue_search,
        queue_clear_button,
        queue_lyrics_split,
        lyrics_pane,
        fullscreen_player,
        player_controls,
    });

    build_normal_navigation(&shell);
    build_compact_navigation(&shell);
    shell.update_server_selector();
    connect_shell_actions(&shell, main_menu);
    install_window_state_persistence(&shell);
    connect_queue_panel_controls(&shell);
    connect_queue_lyrics_split(&shell);
    connect_lyrics_search_controls(&shell);
    connect_fullscreen_player_controls(&shell);
    connect_player_controls(&shell);
    #[cfg(unix)]
    install_mpris(&shell);
    shell.update_layout();
    if defer_initial_route {
        shell.render_startup_loading_view();
    } else {
        shell.render_current_route();
        shell.refresh_home_for_current_visit();
    }
    shell.render_queue_panel();
    shell.render_lyrics_panel();
    shell.update_bottom_player();
    shell.update_fullscreen_player();
    shell.update_discord_presence(&shell.state.player.borrow());
    shell.update_right_panel_button();
    shell.update_lyrics_panel_button();
    if !shell.state.lyrics_panel_visible.get() {
        apply_lyrics_panel_visibility(Rc::clone(&shell), false);
    }
    shell.request_initial_lyrics_if_needed();
    install_event_pump(&shell, events);

    if options.fake_scale.is_none() && !options.ui_perf_run {
        schedule_startup_sync(&shell);
    }

    if let Some(delay_ms) = options.smoke_exit_ms {
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(delay_ms), move || {
            info!(delay_ms, "smoke exit requested");
            app.quit();
        });
    }

    shell.window.present();
    if defer_initial_route {
        shell.schedule_startup_route_reveal();
    }
    let layout_shell = Rc::clone(&shell);
    glib::idle_add_local_once(move || {
        layout_shell.update_layout();
    });
    shell.queue_responsive_route_render();

    if options.ui_perf_run {
        start_ui_perf_run(&shell, app);
    } else if perf_observe {
        start_ui_perf_observe(&shell, app);
    }
}
