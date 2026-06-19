#[path = "../artist.rs"]
mod artist;
#[path = "../cards.rs"]
mod cards;
#[path = "../chrome.rs"]
mod chrome;
#[path = "../discord.rs"]
mod discord;
#[path = "../equalizer.rs"]
mod equalizer;
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
#[path = "../release_kind.rs"]
mod release_kind;
#[path = "../right_panel.rs"]
mod right_panel;
#[path = "../settings_persistence.rs"]
mod settings_persistence;
#[path = "../source_selector.rs"]
mod source_selector;
#[cfg(test)]
#[path = "../style_contract.rs"]
mod style_contract;
#[cfg(unix)]
mod tray;

use crate::controller::{
    AppController, ControllerEvent, DiscoveredServer, LibrarySnapshot, LibrarySyncStatus,
    LyricsSearchResult, PlaybackSnapshot, SearchRequestKey, ServerDiscoveryStatus,
    grouped_cover_refs_for_items, smart_playlist_definition_fingerprint,
    track_cover_refs_for_items,
};
use crate::external_metadata;
use crate::i18n::{self, tr};
use crate::lyrics::{LyricsPane, next_lyrics_line_start_after};
use ::library::{CachedGenreDetail, LibraryDelta, image_cache_key};
#[cfg(feature = "dev-tools")]
use ::test_support::FakeScale;
use adw::prelude::*;
use chrome::{build_content_chrome, build_main_area};
use discord::DiscordPresence;
use domain::{
    Album, AlbumId, AppSettings, Artist, ArtistId, ArtistTrackScope, DEFAULT_WINDOW_HEIGHT,
    DEFAULT_WINDOW_WIDTH, ExternalLyricsProvider, FolderPathItem, Genre, HomeBlockKind,
    HomeSection, HomeSectionKind, ImageRef, LibraryField, LibraryLayout, LibraryListKey,
    LibraryListSettings, MusicFolderId, PlaySourceDescriptor, Playlist,
    PlaylistEntrySortDescriptor, PlaylistId, QueueEntry, QueueSnapshot, RightSidebarMode, Route,
    RouteStack, SearchKind, ServerId, SidebarRouteItem, SmartPlaylist, SmartPlaylistBuiltin,
    SmartPlaylistDefinition, SmartPlaylistId, SmartPlaylistMatchMode, SmartPlaylistRule,
    SmartPlaylistRuleField, SmartPlaylistRuleGroup, SmartPlaylistRuleNode,
    SmartPlaylistRuleOperator, SmartPlaylistRuleValue, SmartPlaylistSortField, Track, TrackId,
    TrackSortKey, TrackTableSettings, format_duration, sanitized_window_size,
};
use favorites::{
    FavoriteControlKey, FavoriteControls, album_favorite_key, apply_search_favorite_change,
    artist_favorite_key, clear_favorite_controls, favorite_change_needs_route_render,
    favorite_control_key, merge_favorite_snapshot, register_favorite_control,
    update_favorite_controls,
};
use fullscreen_player::{
    FullscreenPlayerParts, build_fullscreen_player, connect_fullscreen_player_controls,
};
use gdk_pixbuf::{Colorspace, InterpType, Pixbuf};
use gtk::gdk::prelude::GdkCairoContextExt;
use gtk::gio;
use gtk::glib;
use layout::{
    COMPACT_RAIL_WIDTH, DETAIL_GRADIENT_MARGIN_END, DETAIL_ROUTE_SCROLL_GUTTER, HOME_ALBUM_GAP,
    MIN_APP_WINDOW_HEIGHT, MIN_APP_WINDOW_WIDTH, NORMAL_SIDEBAR_WIDTH, PRIMARY_ROUTE_MARGIN_END,
    PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN, ResolvedLayout, ResolvedLeftSidebarMode,
    SidebarWidths, configure_exact_width_clip, configure_fill_width_clip, detail_route_inner_width,
    detail_showcase_cover_size, large_popup_content_height, large_popup_content_width,
    resolve_layout_with_sidebar_widths, route_content_width,
};
#[cfg(unix)]
use mpris::install_mpris;
#[cfg(unix)]
use mpris_server::Player as MprisPlayer;
use navigation::{
    build_compact_navigation, build_normal_navigation, rebuild_navigation,
    update_navigation_selection,
};
use paging::{PagedGridCursor, connect_paged_grid_loader, finish_grid_page};
use playback::PlaybackState;
use player::{PlayerControls, build_bottom_player, connect_player_controls};
use preferences::{present_library_preferences_dialog, present_preferences_dialog};
use queue::connect_queue_panel_controls;
use release_kind::album_release_kind_label;
use right_panel::{apply_lyrics_panel_visibility, build_right_panel, connect_queue_lyrics_split};
use source::{FavoriteItemId, FolderDetail, Lyrics, LyricsSource, PlaylistEntry, SearchResults};
use source_selector::{ServerSelector, build_server_selector};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, error, info, warn};

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
mod new_smart_playlist_dialog;
mod playlist_detail_view;
mod playlist_rename_dialog;
mod release_notes;
mod responsive_layout_state;
mod responsive_route_render;
mod route_navigation;
mod route_rendering;
mod search_route_state;
mod search_view;
mod shell_navigation;
mod sidebar_route_controls;
mod startup_reveal;

#[cfg(test)]
mod shell_tests;

pub(in crate::ui) use build::*;
pub(in crate::ui::root) use cover::*;
pub(in crate::ui) use cover::{
    CoverBinding, CoverDecodeJob, DecodedCover, DecodedCoverOrderEntry, cover_artwork_id_for_key,
    cover_request_id_for_key,
};
pub(in crate::ui) use cover_startup::*;
pub(in crate::ui) use equalizer::{
    build_equalizer_preset_dropdown, connect_equalizer_scale_commit, equalizer_band_label_parts,
    equalizer_band_title, equalizer_default_preset_bands, equalizer_preset_bands,
    equalizer_preset_button_label, equalizer_preset_name_at, equalizer_preset_position,
    equalizer_selected_preset, install_equalizer_scroll,
};
pub(in crate::ui) use home_refresh::*;
pub(in crate::ui) use layout_rendering::*;
#[cfg(test)]
pub(in crate::ui) use playlist_detail_view::{
    playlist_cover_size, playlist_detail_compact_for_width, playlist_route_margin,
    playlist_sort_width,
};
pub(in crate::ui) use release_notes::*;
pub(in crate::ui) use shell_navigation::*;

pub(in crate::ui) const GRID_ROUTE_PAGE_SIZE: usize = 16;
pub(in crate::ui) const TRACK_ROUTE_PAGE_SIZE: usize = 64;
pub(in crate::ui) const GRID_COVER_SIZE: u32 = 256;
pub(in crate::ui) const DETAIL_COVER_SIZE: u32 = 512;
pub(in crate::ui) const THUMB_COVER_SIZE: u32 = 96;
pub(in crate::ui) const IMAGE_TAG_UNTAGGED: &str = "untagged";
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::ui) struct PlaybackArtworkPath {
    key: String,
    path: PathBuf,
}
pub(in crate::ui) const DECODED_COVER_CACHE_LIMIT: usize = 3_072;
pub(in crate::ui) const DECODED_COVER_CACHE_SOFT_BYTES: usize = 256 * 1024 * 1024;
pub(in crate::ui) const COVER_WARM_BATCH_SIZE: usize = 3;
pub(in crate::ui) const COVER_LOOKUP_LIMIT: usize = 12;
pub(in crate::ui) const COVER_WARM_INTERVAL_MS: u64 = 32;
pub(in crate::ui) const COVER_WARM_SCROLL_PAUSE_MS: u64 = 1_500;
pub(in crate::ui) const COVER_VISIBLE_SCROLL_PAUSE_MS: u64 = 160;
pub(in crate::ui) const COVER_VISIBLE_REQUEST_DELAY_MS: u64 = 48;
pub(in crate::ui) const COVER_DECODE_MAX_IN_FLIGHT: usize = 8;
pub(in crate::ui) const COVER_DECODE_LIMIT: usize = 16;
pub(in crate::ui) const SLOW_COVER_CALLBACK_MS: u64 = 100;
pub(in crate::ui) const SLOW_ROUTE_PAGE_LOAD_MS: u64 = 100;
pub(in crate::ui) const STARTUP_ROUTE_REVEAL_MAX_MS: u64 = 3_000;
pub(in crate::ui) const STARTUP_ROUTE_REVEAL_POLL_MS: u64 = 32;
pub(in crate::ui) const STARTUP_STALL_MONITOR_INTERVAL_MS: u64 = 100;
pub(in crate::ui) const STARTUP_STALL_MONITOR_WINDOW_MS: u64 = 60_000;
pub(in crate::ui) const STARTUP_STALL_WARN_MS: u64 = 100;
pub(in crate::ui) const STARTUP_HOME_SECTION_LIMIT: usize = 3;
pub(in crate::ui) const STARTUP_HOME_SECTION_COVER_LIMIT: usize = 4;
pub(in crate::ui) const STARTUP_CACHED_COVER_PRIME_LIMIT: usize = 3_072;
pub(in crate::ui) const PRIME_TIMEOUT_MS: u64 = 3_000;
pub(in crate::ui) const PRIME_POLL_MS: u64 = 33;
pub(in crate::ui) const FIRST_RUN_HOME_SECTION_LIMIT: usize = 3;
pub(in crate::ui) const HOME_COVER_LIMIT: usize = 4;
pub(in crate::ui) const GRID_COVER_LIMIT: usize = 192;
pub(in crate::ui) const WARM_SETTLE_MS: u64 = RESPONSIVE_RENDER_DELAY_MS * 5;
pub(in crate::ui) const LIBRARY_SYNC_COMPLETE_STATUS: &str = "Library sync complete";
pub(in crate::ui) const LIBRARY_PREPARING_STATUS: &str = "Preparing library…";
pub(in crate::ui) const FAVORITE_EMPTY_GLYPH: &str = "♡";
pub(in crate::ui) const FAVORITE_FILLED_GLYPH: &str = "♥";
pub(in crate::ui) const PLAYLIST_ENTRY_NUMBER_WIDTH: i32 = 24;
pub(in crate::ui) const PLAYLIST_ENTRY_COVER_WIDTH: i32 = 36;
pub(in crate::ui) const PLAYLIST_ENTRY_COLUMN_GAP: i32 = 8;
pub(in crate::ui) const PLAYLIST_ENTRY_TITLE_MAX_CHARS: i32 = 44;
pub(in crate::ui) const PLAY_NEXT_ICON: &str = "view-sort-ascending-symbolic";
pub(in crate::ui) const PLAY_LATER_ICON: &str = "view-sort-descending-symbolic";
pub(in crate::ui) const RESPONSIVE_RENDER_DELAY_MS: u64 = 16;
pub(in crate::ui) const RESPONSIVE_ROUTE_SETTLE_MS: u64 = 180;
static HOME_SHOWCASE_COUNTER: AtomicU64 = AtomicU64::new(0);
pub(in crate::ui) fn route_resize_diagnostics_enabled() -> bool {
    matches!(
        std::env::var("RUFIN_RESIZE_DEBUG")
            .unwrap_or_default()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub(in crate::ui) fn smart_playlist_display_name(playlist: &SmartPlaylist) -> String {
    playlist
        .builtin
        .map(|builtin| tr(builtin.title()))
        .unwrap_or_else(|| playlist.name.clone())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum RouteResizePolicy {
    Stable,
    LayoutSignature,
    SettledWidth,
}
#[derive(Clone, Debug, Default)]
pub struct AppOptions {
    #[cfg(feature = "dev-tools")]
    pub fake_scale: Option<FakeScale>,
}
#[derive(Clone)]
pub(in crate::ui) struct AddServerDialogHandle {
    toolbar: adw::ToolbarView,
    on_connect_started: Option<Rc<dyn Fn()>>,
}
pub(in crate::ui) struct AppState {
    routes: RefCell<RouteStack>,
    settings: RefCell<AppSettings>,
    resolved_left_sidebar: Cell<ResolvedLeftSidebarMode>,
    resolved_right_sidebar: Cell<RightSidebarMode>,
    resolved_right_sidebar_width: Cell<i32>,
    main_content_width: Cell<i32>,
    library: RefCell<LibrarySnapshot>,
    track_index: RefCell<HashMap<TrackId, usize>>,
    queue: RefCell<Option<QueueSnapshot>>,
    player: RefCell<PlaybackSnapshot>,
    lyrics: RefCell<Option<Lyrics>>,
    lyrics_track_id: RefCell<Option<domain::TrackId>>,
    lyrics_loading_track_id: RefCell<Option<domain::TrackId>>,
    lyrics_auto_search_attempted: RefCell<HashSet<domain::TrackId>>,
    lyrics_search_dialog: RefCell<Option<LyricsSearchDialog>>,
    type_to_search: RefCell<Option<gtk::SearchEntry>>,
    context_playlist_picker: RefCell<Option<PlaylistPickerHandle>>,
    preferences_dialog: RefCell<Option<adw::Dialog>>,
    reconnect_toasts_shown: RefCell<HashSet<ServerId>>,
    library_sync_toast: RefCell<Option<adw::Toast>>,
    library_sync_toast_suppressed: Cell<bool>,
    control_feedback_toast: RefCell<Option<adw::Toast>>,
    lyrics_timing_generation: Cell<u64>,
    lyrics_timing_source: RefCell<Option<glib::SourceId>>,
    #[cfg(unix)]
    mpris_player: RefCell<Option<Rc<MprisPlayer>>>,
    #[cfg(unix)]
    mpris_metadata_key: RefCell<Option<String>>,
    #[cfg(unix)]
    mpris_position_state: RefCell<Option<mpris::MprisPositionState>>,
    #[cfg(unix)]
    mpris_update_generation: Rc<Cell<u64>>,
    #[cfg(unix)]
    tray_handle: RefCell<Option<tray::TrayHandle>>,
    #[cfg(unix)]
    tray_command_source: RefCell<Option<glib::SourceId>>,
    discord_presence: RefCell<DiscordPresence>,
    updating_player_controls: Cell<bool>,
    seek_preview_seconds: Cell<Option<u32>>,
    seek_generation: Cell<u64>,
    queue_filter: RefCell<String>,
    queue_render_queued: Cell<bool>,
    queue_sidebar_render_state: RefCell<Option<queue::QueuePanelModelState>>,
    queue_fullscreen_render_state: RefCell<Option<queue::QueuePanelModelState>>,
    lyrics_panel_visible: Cell<bool>,
    fullscreen_player_visible: Cell<bool>,
    responsive_render_queued: Cell<bool>,
    responsive_render_generation: Cell<u64>,
    current_route_resize_policy: Cell<RouteResizePolicy>,
    responsive_render_signature: Cell<i32>,
    current_route_boundary: RefCell<Option<gtk::Widget>>,
    column_view_width_fits: RefCell<Vec<library::ColumnViewWidthFit>>,
    collection_grid_columns: Cell<usize>,
    home_section_state: RefCell<HashMap<HomeSectionKind, HomeSectionState>>,
    home_section_views: RefCell<HashMap<HomeSectionKind, HomeSectionView>>,
    prefetched_explore: RefCell<Option<PrefetchedHomeSection>>,
    route_track_refs: RefCell<Vec<Option<ImageRef>>>,
    smart_playlists: RefCell<Vec<SmartPlaylist>>,
    smart_playlists_loaded: Cell<bool>,
    home_showcase_seed: Cell<u64>,
    route_load_generation: Cell<u64>,
    pending_sync_route_refresh: RefCell<Option<(Route, u64)>>,
    pending_sync_route_delta: RefCell<Option<LibraryDelta>>,
    startup_route_revealed: Cell<bool>,
    startup_route_render_pending: Cell<bool>,
    startup_route_content_prepared: Cell<bool>,
    source_switch_preparing: Cell<bool>,
    local_source_preparing: Cell<bool>,
    local_source_sync_seen: Cell<bool>,
    startup_cover_prime_generation: Cell<u64>,
    startup_cover_prime_pending: RefCell<HashSet<String>>,
    first_run_connection_pending: Cell<bool>,
    first_run_connection_ready: Cell<bool>,
    first_run_cover_prime_generation: Cell<u64>,
    first_run_cover_prime_pending: RefCell<HashSet<String>>,
    cover_warm_token: Cell<u64>,
    cover_warm_pending: RefCell<Option<(ServerId, u64)>>,
    cover_warm_started: RefCell<Option<ServerId>>,
    discovered_servers: RefCell<Vec<DiscoveredServer>>,
    server_discovery_status: RefCell<ServerDiscoveryStatus>,
    server_discovery_running: Cell<bool>,
    server_discovery_started: Cell<bool>,
    add_server_dialog: RefCell<Option<AddServerDialogHandle>>,
    locale_bindings: RefCell<Vec<Box<dyn Fn()>>>,
    cover_bindings: RefCell<HashMap<String, Vec<CoverBinding>>>,
    cover_unavailable: RefCell<HashSet<String>>,
    cover_path_cache: RefCell<HashMap<String, PathBuf>>,
    cover_path_lookups: cover::CoverPathLookups,
    cover_fetches: RefCell<HashSet<String>>,
    cover_visible_requests: cover::CoverVisibleRequests,
    cover_decodes: RefCell<HashMap<String, cover::CoverDecodePriority>>,
    cover_decode_queue: RefCell<VecDeque<CoverDecodeJob>>,
    cover_warm_generation: Cell<u64>,
    cover_warm_paused_until: Cell<Option<Instant>>,
    cover_visible_paused_until: Cell<Option<Instant>>,
    cover_decode_resume_queued: Cell<bool>,
    decoded_covers: RefCell<HashMap<String, DecodedCover>>,
    decoded_cover_order: RefCell<VecDeque<DecodedCoverOrderEntry>>,
    decoded_cover_bytes: Cell<usize>,
    decoded_cover_touch: Cell<u64>,
    favorite_controls: FavoriteControls,
    folder_request_generation: Cell<u64>,
    folder_state: RefCell<FolderRouteState>,
    search_request_generation: Cell<u64>,
    search_state: RefCell<SearchRouteState>,
}
#[derive(Clone)]
pub(in crate::ui) struct LyricsSearchDialog {
    dialog: adw::Dialog,
    track_id: domain::TrackId,
    artist_entry: gtk::Entry,
    title_entry: gtk::Entry,
    search_debounce_source: Rc<RefCell<Option<glib::SourceId>>>,
    list: gtk::ListBox,
    status: gtk::Label,
}
#[derive(Clone)]
pub(in crate::ui) struct ArtworkTile {
    area: gtk::DrawingArea,
    size: Rc<Cell<i32>>,
    seed: Rc<Cell<u32>>,
    pixbuf: Rc<RefCell<Option<Pixbuf>>>,
    expects_image: Rc<Cell<bool>>,
    artwork_id: Rc<RefCell<Option<String>>>,
    request_key: Rc<RefCell<Option<String>>>,
    generation: Rc<Cell<u64>>,
}
#[derive(Clone)]
pub(in crate::ui) struct ArtworkTileWeak {
    area: glib::WeakRef<gtk::DrawingArea>,
    size: Rc<Cell<i32>>,
    seed: Rc<Cell<u32>>,
    pixbuf: Rc<RefCell<Option<Pixbuf>>>,
    expects_image: Rc<Cell<bool>>,
    artwork_id: Rc<RefCell<Option<String>>>,
    request_key: Rc<RefCell<Option<String>>>,
    generation: Rc<Cell<u64>>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum ArtworkBindAction {
    Request,
    Replace,
    Retain,
    RetainAndRequest,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) struct ArtworkBindOutcome {
    pub(in crate::ui) generation: u64,
    pub(in crate::ui) request_needed: bool,
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
    server_id: domain::ServerId,
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
#[derive(Clone, Default)]
pub(in crate::ui) struct SearchRouteState {
    key: Option<SearchRequestKey>,
    loading: bool,
    results: SearchResults,
    error: Option<String>,
}
pub(in crate::ui) struct GroupedDetailData {
    title: String,
    artwork: crate::cover_art_policy::SelectedArtwork,
    seed: u32,
    summary_items: Vec<(&'static str, String)>,
    tracks: Vec<Track>,
    table_context: &'static str,
    source_descriptor: Option<PlaySourceDescriptor>,
}
pub(in crate::ui) struct Shell {
    state: AppState,
    controller: AppController,
    application: adw::Application,
    window: adw::ApplicationWindow,
    toast_overlay: adw::ToastOverlay,
    quick_toast_overlay: adw::ToastOverlay,
    root_stack: gtk::Stack,
    app_root_overlay: gtk::Overlay,
    app_root: gtk::Box,
    app_content_stack: gtk::Stack,
    login_host: gtk::Box,
    startup_loading_host: gtk::Box,
    split_view: adw::OverlaySplitView,
    normal_nav_slot: gtk::ScrolledWindow,
    compact_nav_slot: gtk::ScrolledWindow,
    tiny_nav_button: gtk::Button,
    normal_nav: gtk::Box,
    compact_nav: gtk::Box,
    server_selector: ServerSelector,
    route_host: gtk::Box,
    main_menu: gtk::MenuButton,
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

pub(in crate::ui) fn track_index_for(tracks: &[Track]) -> HashMap<TrackId, usize> {
    let mut index = HashMap::with_capacity(tracks.len());
    for (position, track) in tracks.iter().enumerate() {
        index.insert(track.id.clone(), position);
    }
    index
}

impl Shell {
    pub(in crate::ui) fn replace_library_snapshot(&self, snapshot: LibrarySnapshot) {
        let track_index = track_index_for(&snapshot.tracks);
        *self.state.library.borrow_mut() = snapshot;
        *self.state.track_index.borrow_mut() = track_index;
    }

    pub(in crate::ui) fn rebuild_track_index(&self) {
        let index = track_index_for(&self.state.library.borrow().tracks);
        *self.state.track_index.borrow_mut() = index;
    }
}

fn sidebar_scroll_slot(width: i32, child: &gtk::Box) -> gtk::ScrolledWindow {
    let slot = gtk::ScrolledWindow::new();
    slot.set_policy(gtk::PolicyType::Never, gtk::PolicyType::External);
    slot.set_width_request(width);
    slot.set_min_content_width(width);
    slot.set_max_content_width(width);
    slot.set_propagate_natural_width(false);
    slot.set_propagate_natural_height(false);
    slot.set_hexpand(false);
    slot.set_vexpand(true);
    slot.set_child(Some(child));
    slot
}
pub fn build(app: &adw::Application, _options: AppOptions) {
    install_css();

    let loaded_at = std::time::Instant::now();
    #[cfg(feature = "dev-tools")]
    let using_fake_library = _options.fake_scale.is_some();
    #[cfg(not(feature = "dev-tools"))]
    let using_fake_library = false;
    let bootstrapped = {
        #[cfg(feature = "dev-tools")]
        {
            if let Some(scale) = _options.fake_scale {
                Ok(AppController::bootstrap_with_fake(scale))
            } else {
                AppController::bootstrap()
            }
        }
        #[cfg(not(feature = "dev-tools"))]
        {
            AppController::bootstrap()
        }
    };
    let (controller, events, library, queue, player) = match bootstrapped {
        Ok(bootstrapped) => bootstrapped,
        Err(error) => {
            error!(%error, "failed to start Rufin");
            app.quit();
            return;
        }
    };
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
    let defer_initial_route = !first_run;
    let language_preference = i18n::effective_language_preference(&settings.language);
    i18n::set_language_preference(&language_preference);
    let prefetched_explore = prefetched_explore_from_snapshot(&library);
    let track_index = track_index_for(&library.tracks);

    let state = AppState {
        routes: RefCell::new(RouteStack::new(Route::Home)),
        settings: RefCell::new(settings.clone()),
        resolved_left_sidebar: Cell::new(ResolvedLeftSidebarMode::Full),
        resolved_right_sidebar: Cell::new(RightSidebarMode::Hidden),
        resolved_right_sidebar_width: Cell::new(0),
        main_content_width: Cell::new(1),
        library: RefCell::new(library),
        track_index: RefCell::new(track_index),
        queue: RefCell::new(queue),
        player: RefCell::new(player),
        lyrics: RefCell::new(None),
        lyrics_track_id: RefCell::new(None),
        lyrics_loading_track_id: RefCell::new(None),
        lyrics_auto_search_attempted: RefCell::new(HashSet::new()),
        lyrics_search_dialog: RefCell::new(None),
        type_to_search: RefCell::new(None),
        context_playlist_picker: RefCell::new(None),
        preferences_dialog: RefCell::new(None),
        reconnect_toasts_shown: RefCell::new(HashSet::new()),
        library_sync_toast: RefCell::new(None),
        library_sync_toast_suppressed: Cell::new(false),
        control_feedback_toast: RefCell::new(None),
        lyrics_timing_generation: Cell::new(0),
        lyrics_timing_source: RefCell::new(None),
        #[cfg(unix)]
        mpris_player: RefCell::new(None),
        #[cfg(unix)]
        mpris_metadata_key: RefCell::new(None),
        #[cfg(unix)]
        mpris_position_state: RefCell::new(None),
        #[cfg(unix)]
        mpris_update_generation: Rc::new(Cell::new(0)),
        #[cfg(unix)]
        tray_handle: RefCell::new(None),
        #[cfg(unix)]
        tray_command_source: RefCell::new(None),
        discord_presence: RefCell::new(DiscordPresence::new()),
        updating_player_controls: Cell::new(false),
        seek_preview_seconds: Cell::new(None),
        seek_generation: Cell::new(0),
        queue_filter: RefCell::new(String::new()),
        queue_render_queued: Cell::new(false),
        queue_sidebar_render_state: RefCell::new(None),
        queue_fullscreen_render_state: RefCell::new(None),
        lyrics_panel_visible: Cell::new(settings.lyrics_panel_visible),
        fullscreen_player_visible: Cell::new(false),
        responsive_render_queued: Cell::new(false),
        responsive_render_generation: Cell::new(0),
        current_route_resize_policy: Cell::new(RouteResizePolicy::Stable),
        responsive_render_signature: Cell::new(0),
        current_route_boundary: RefCell::new(None),
        column_view_width_fits: RefCell::new(Vec::new()),
        collection_grid_columns: Cell::new(0),
        home_section_state: RefCell::new(HashMap::new()),
        home_section_views: RefCell::new(HashMap::new()),
        prefetched_explore: RefCell::new(prefetched_explore),
        route_track_refs: RefCell::new(Vec::new()),
        smart_playlists: RefCell::new(Vec::new()),
        smart_playlists_loaded: Cell::new(false),
        home_showcase_seed: Cell::new(next_home_showcase_seed()),
        route_load_generation: Cell::new(0),
        pending_sync_route_refresh: RefCell::new(None),
        pending_sync_route_delta: RefCell::new(None),
        startup_route_revealed: Cell::new(!defer_initial_route),
        startup_route_render_pending: Cell::new(false),
        startup_route_content_prepared: Cell::new(!defer_initial_route),
        source_switch_preparing: Cell::new(false),
        local_source_preparing: Cell::new(false),
        local_source_sync_seen: Cell::new(false),
        startup_cover_prime_generation: Cell::new(0),
        startup_cover_prime_pending: RefCell::new(HashSet::new()),
        first_run_connection_pending: Cell::new(false),
        first_run_connection_ready: Cell::new(false),
        first_run_cover_prime_generation: Cell::new(0),
        first_run_cover_prime_pending: RefCell::new(HashSet::new()),
        cover_warm_token: Cell::new(0),
        cover_warm_pending: RefCell::new(None),
        cover_warm_started: RefCell::new(None),
        discovered_servers: RefCell::new(Vec::new()),
        server_discovery_status: RefCell::new(ServerDiscoveryStatus::Idle),
        server_discovery_running: Cell::new(false),
        server_discovery_started: Cell::new(false),
        add_server_dialog: RefCell::new(None),
        locale_bindings: RefCell::new(Vec::new()),
        cover_bindings: RefCell::new(HashMap::new()),
        cover_unavailable: RefCell::new(HashSet::new()),
        cover_path_cache: RefCell::new(HashMap::new()),
        cover_path_lookups: cover::CoverPathLookups::new(),
        cover_fetches: RefCell::new(HashSet::new()),
        cover_visible_requests: cover::CoverVisibleRequests::new(),
        cover_decodes: RefCell::new(HashMap::new()),
        cover_decode_queue: RefCell::new(VecDeque::new()),
        cover_warm_generation: Cell::new(0),
        cover_warm_paused_until: Cell::new(None),
        cover_visible_paused_until: Cell::new(None),
        cover_decode_resume_queued: Cell::new(false),
        decoded_covers: RefCell::new(HashMap::new()),
        decoded_cover_order: RefCell::new(VecDeque::new()),
        decoded_cover_bytes: Cell::new(0),
        decoded_cover_touch: Cell::new(0),
        favorite_controls: RefCell::new(HashMap::new()),
        folder_request_generation: Cell::new(0),
        folder_state: RefCell::new(FolderRouteState::default()),
        search_request_generation: Cell::new(0),
        search_state: RefCell::new(SearchRouteState::default()),
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
    root_stack.set_width_request(MIN_APP_WINDOW_WIDTH);
    root_stack.set_height_request(MIN_APP_WINDOW_HEIGHT);
    root_stack.set_hhomogeneous(false);
    root_stack.set_vhomogeneous(false);
    root_stack.set_interpolate_size(false);
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

    let app_content_overlay = gtk::Overlay::new();
    app_content_overlay.set_hexpand(true);
    app_content_overlay.set_vexpand(true);

    let login_host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    login_host.add_css_class("login-root");
    login_host.set_hexpand(true);
    login_host.set_vexpand(true);

    let startup_loading_host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    startup_loading_host.add_css_class("startup-loading-root");
    startup_loading_host.set_hexpand(true);
    startup_loading_host.set_vexpand(true);

    let upper = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    upper.set_hexpand(true);
    upper.set_vexpand(true);

    let normal_nav = gtk::Box::new(gtk::Orientation::Vertical, 4);
    normal_nav.add_css_class("wide-sidebar");
    normal_nav.set_hexpand(false);
    normal_nav.set_width_request(NORMAL_SIDEBAR_WIDTH);
    let normal_nav_slot = sidebar_scroll_slot(NORMAL_SIDEBAR_WIDTH, &normal_nav);
    normal_nav_slot.add_css_class("sidebar-pane");
    normal_nav_slot.add_css_class("wide-sidebar-slot");

    let compact_nav = gtk::Box::new(gtk::Orientation::Vertical, 1);
    compact_nav.add_css_class("compact-rail");
    compact_nav.set_hexpand(false);
    compact_nav.set_width_request(COMPACT_RAIL_WIDTH);
    let compact_nav_slot = sidebar_scroll_slot(COMPACT_RAIL_WIDTH, &compact_nav);
    compact_nav_slot.add_css_class("sidebar-pane");
    compact_nav_slot.add_css_class("compact-rail-slot");
    let server_selector = build_server_selector();

    let main_area_parts = build_main_area();
    let main_area = main_area_parts.root;
    let route_host = main_area_parts.route_host;

    let right_panel_parts = build_right_panel();
    let right_panel = right_panel_parts.root;
    let queue_panel = right_panel_parts.queue_panel;
    let queue_search = right_panel_parts.queue_search;
    let queue_clear_button = right_panel_parts.queue_clear_button;
    let queue_lyrics_split = right_panel_parts.queue_lyrics_split;
    let lyrics_pane = right_panel_parts.lyrics_pane;

    let content_chrome = build_content_chrome(&main_area, &right_panel);
    content_chrome.root.set_margin_start(8);
    let main_menu = content_chrome.main_menu;
    let right_panel_slot = content_chrome.right_panel_slot;
    let tiny_nav_button = gtk::Button::from_icon_name("sidebar-show-symbolic");
    tiny_nav_button.add_css_class("icon-button");
    tiny_nav_button.add_css_class("flat");
    tiny_nav_button.add_css_class("circular");
    tiny_nav_button.add_css_class("tiny-sidebar-button");
    tiny_nav_button.set_tooltip_text(Some(&tr("Show sidebar")));
    tiny_nav_button.update_property(&[gtk::accessible::Property::Label(&tr("Show sidebar"))]);
    tiny_nav_button.set_halign(gtk::Align::Start);
    tiny_nav_button.set_valign(gtk::Align::End);
    tiny_nav_button.set_margin_start(8);
    tiny_nav_button.set_margin_bottom(8);
    tiny_nav_button.set_visible(false);
    content_chrome.root.add_overlay(&tiny_nav_button);
    content_chrome
        .root
        .set_measure_overlay(&tiny_nav_button, false);
    let fullscreen_player = build_fullscreen_player();
    let player_controls = build_bottom_player();

    let content_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content_row.set_hexpand(true);
    content_row.set_vexpand(true);
    content_row.append(&compact_nav_slot);
    content_row.append(&content_chrome.root);

    let split_view = adw::OverlaySplitView::new();
    split_view.set_hexpand(true);
    split_view.set_vexpand(true);
    split_view.set_enable_hide_gesture(false);
    split_view.set_enable_show_gesture(false);
    split_view.set_min_sidebar_width(NORMAL_SIDEBAR_WIDTH as f64);
    split_view.set_max_sidebar_width(NORMAL_SIDEBAR_WIDTH as f64);
    split_view.set_sidebar_width_unit(adw::LengthUnit::Px);
    split_view.set_sidebar(Some(&normal_nav_slot));
    split_view.set_content(Some(&content_row));

    upper.append(&split_view);

    app_content_stack.add_named(&upper, Some("main"));
    app_content_overlay.set_child(Some(&app_content_stack));
    app_content_overlay.add_overlay(&fullscreen_player.root);
    app_content_overlay.set_measure_overlay(&fullscreen_player.root, false);

    app_root.append(&app_content_overlay);
    app_root.append(&player_controls.root);

    root_stack.add_named(&login_host, Some("login"));
    root_stack.add_named(&startup_loading_host, Some("startup-loading"));
    let app_root_overlay = gtk::Overlay::new();
    app_root_overlay.set_hexpand(true);
    app_root_overlay.set_vexpand(true);
    app_root_overlay.set_child(Some(&app_root));

    root_stack.add_named(&app_root_overlay, Some("app"));
    let quick_toast_overlay = adw::ToastOverlay::new();
    quick_toast_overlay.add_css_class("quick-toast-overlay");
    quick_toast_overlay.set_child(Some(&root_stack));
    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.add_css_class("app-toast-overlay");
    toast_overlay.set_child(Some(&quick_toast_overlay));
    window.set_content(Some(&toast_overlay));

    let shell = Rc::new(Shell {
        state,
        controller,
        application: app.clone(),
        window,
        toast_overlay,
        quick_toast_overlay,
        root_stack,
        app_root_overlay,
        app_root,
        app_content_stack,
        login_host,
        startup_loading_host,
        split_view,
        normal_nav_slot,
        compact_nav_slot,
        tiny_nav_button,
        normal_nav,
        compact_nav,
        server_selector,
        route_host,
        main_menu: main_menu.clone(),
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
    shell.install_locale_bindings();
    shell.update_server_selector();
    {
        let split_view = shell.split_view.clone();
        shell
            .tiny_nav_button
            .connect_clicked(move |_| split_view.set_show_sidebar(true));
    }
    connect_shell_actions(&shell, main_menu);
    install_window_state_persistence(&shell);
    #[cfg(unix)]
    tray::install_tray(&shell);
    connect_queue_panel_controls(&shell);
    connect_queue_lyrics_split(&shell);
    shell.connect_type_to_search();
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
    startup_reveal::install_startup_stall_monitor(&shell);
    install_event_pump(&shell, events);
    if settings.seekbar_waveform_enabled {
        shell.controller.request_waveform_for_current();
    }

    if !using_fake_library {
        schedule_startup_sync(&shell);
    }

    #[cfg(unix)]
    tray::present_initial_window(&shell);
    #[cfg(not(unix))]
    shell.window.present();
    schedule_release_toast(&shell);
    if defer_initial_route {
        shell.schedule_startup_route_reveal();
    }
    let layout_shell = Rc::clone(&shell);
    glib::idle_add_local_once(move || {
        layout_shell.update_layout();
    });
    shell.queue_responsive_route_render();
}
