use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use app_identity::DISPLAY_NAME;
use tracing::info;

use crate::interactions::connect_transient_entry_focus_dismissal;
use crate::player::desktop::DesktopState;
use crate::player::desktop::lifecycle::install_application_quit;
use crate::player::lyrics::state::LyricsState;
use crate::player::right_panel::RightPanelWidgets;
use crate::player::state::PlaybackState;
use crate::player::{
    BOTTOM_PLAYER_HEIGHT, PlayerDesktopWidgets, apply_sidebar_media_visibility,
    build_bottom_player, build_fullscreen_player, build_right_panel, build_visualizer,
    connect_fullscreen_player_controls, connect_player_controls, connect_queue_lyrics_split,
    connect_queue_panel_controls, default_audio_output_options, warm_audio_output_cache,
};
use crate::player::{install_desktop_lifecycle, present_initial_window};
use crate::preferences::PreferencesState;
use crate::preferences::dialogs::release_notes::{
    check_for_release_update, schedule_periodic_release_checks,
};
use crate::preferences::source::SourceState;
use crate::runtime::RuntimeInputs;
use crate::settings::SettingsState;

use super::Shell;
use super::actions::{ControlFeedbackState, connect_shell_actions};
use super::chrome::{WindowChrome, WindowControlLayout, build_content_chrome};
use super::cover::ArtworkState;
use super::events::install_product_event_receivers;
use super::layout::{
    COMPACT_RAIL_WIDTH, MIN_APP_WINDOW_HEIGHT, MIN_APP_WINDOW_WIDTH, NORMAL_SIDEBAR_WIDTH,
    ShellLayoutState,
};
use super::navigation::{
    NavigationState, NavigationWidgets, NormalPrimaryMenuWidgets, PrimaryMenuWidgets,
    build_compact_navigation, build_normal_navigation, install_normal_navigation_activation,
    normal_primary_menu_button,
};
use super::route::RouteViewport;
use super::selected_ui::SelectedUiState;
use super::startup::StartupState;
use super::window_state::initial_window_size;

pub fn build(
    app: &adw::Application,
    inputs: RuntimeInputs,
    quitting: Rc<Cell<bool>>,
    force_initial_presentation: bool,
    presented: Option<Box<dyn FnOnce()>>,
    window_bar_preview: Option<crate::application::WindowBarPreview>,
) {
    let appearance = crate::application::style::ApplicationAppearance::install();

    let loaded_at = std::time::Instant::now();
    let RuntimeInputs {
        diagnostics,
        products,
        settings: settings_handle,
        receivers,
        configured_sources,
        source_operation,
        release_history,
    } = inputs;
    let settings = settings_handle.load();
    appearance.apply(&settings);
    info!(
        first_run = configured_sources.first_run,
        elapsed_ms = loaded_at.elapsed().as_millis(),
        "loaded music source presentation"
    );
    let first_run = configured_sources.first_run;
    let defer_initial_route = !first_run;
    let settings_state = SettingsState {
        current: RefCell::new(settings.clone()),
        persistence: settings_handle,
    };
    let navigation = NavigationState::new();
    let selected_ui = SelectedUiState::new();
    let source = SourceState {
        configured: RefCell::new(configured_sources),
        operation: RefCell::new(source_operation),
        discovered_servers: RefCell::new(Vec::new()),
        discovery_status: RefCell::new(crate::runtime::source::DiscoveryStatus::Idle),
        discovery_running: Cell::new(false),
        discovery_started: Cell::new(false),
        add_server: RefCell::new(None),
        refresh_feedback_generation: Rc::new(Cell::new(0)),
        artwork_preparation_revision: Cell::new(None),
    };
    let startup = StartupState {
        route_revealed: Cell::new(!defer_initial_route),
        initial_launch: Cell::new(defer_initial_route),
        route_allocated: Cell::new(false),
        reveal_deadline: RefCell::new(None),
    };
    let playback_state = PlaybackState {
        updating_controls: Cell::new(false),
        seek_generation: Cell::new(0),
        volume_persist_source: RefCell::new(None),
        audio_output_options: RefCell::new(default_audio_output_options()),
        audio_output_refresh_running: Cell::new(false),
        audio_output_refresh_generation: Cell::new(0),
        audio_output_refreshed_at: Cell::new(None),
        remote_output_options: RefCell::new(Vec::new()),
    };
    let lyrics_state = LyricsState {
        panel_visible: Cell::new(settings.lyrics_panel_visible),
    };
    let preferences = PreferencesState {
        dialog: gtk::glib::WeakRef::new(),
        release_history: RefCell::new(release_history),
        release_history_view: RefCell::new(None),
        release_notification_toast: RefCell::new(None),
        release_updating: RefCell::new(None),
    };
    let downloads = crate::downloads::DownloadsState::default();
    let control_feedback = ControlFeedbackState {
        generation: Rc::new(Cell::new(0)),
    };
    let desktop = DesktopState::new(app, products.playback.transport.clone());
    let artwork = ArtworkState {
        startup_prime: Default::default(),
        route_prime: Default::default(),
        route_registration_open: Cell::new(false),
        route_registrations: RefCell::new(Vec::new()),
        textures: RefCell::new(Default::default()),
    };
    let (window_width, window_height) =
        initial_window_size(settings.window_width, settings.window_height);
    let window_bar_platform = crate::application::platform_window_bar(window_bar_preview);
    let window_controls = WindowControlLayout::new(window_bar_platform.is_some());

    let shell_root_resource = crate::ui_resource::SHELL_ROOT_RESOURCE;
    let shell_root_builder = crate::ui_resource::builder(shell_root_resource);
    crate::ui_resource::objects!(shell_root_builder, shell_root_resource, {
        root_stack: gtk::Stack,
        login_host: gtk::Box,
        app_root_overlay: gtk::Overlay,
        app_root: gtk::Box,
        app_content_overlay: gtk::Overlay,
        app_content_stack: gtk::Stack,
        split_view: adw::OverlaySplitView,
        content_row: gtk::Box,
        left_resize_handle: gtk::Box,
        control_feedback_label: gtk::Label,
        source_refresh_feedback: gtk::Box,
        source_refresh_feedback_label: gtk::Label,
        source_refresh_feedback_progress: gtk::ProgressBar,
        operation_feedback: gtk::Box,
        operation_feedback_artwork: gtk::Box,
        operation_feedback_title: gtk::Label,
        operation_feedback_subtitle: gtk::Label,
        operation_feedback_action: gtk::Button,
        startup_loading_host: gtk::Box,
    });
    root_stack.set_width_request(MIN_APP_WINDOW_WIDTH);
    root_stack.set_height_request(MIN_APP_WINDOW_HEIGHT);
    left_resize_handle.set_cursor_from_name(Some("col-resize"));

    let navigation_resource = crate::ui_resource::NAVIGATION_RESOURCE;
    let navigation_builder = crate::ui_resource::builder(navigation_resource);
    crate::ui_resource::objects!(navigation_builder, navigation_resource, {
        normal_nav_panel: gtk::Box,
        normal_nav_routes: adw::Sidebar,
        normal_nav_pins: gtk::Box,
        compact_nav_slot: gtk::ScrolledWindow,
        compact_nav: gtk::Box,
        normal_window_controls_host: gtk::Box,
        compact_window_controls_host: gtk::Box,
        normal_search: gtk::Button,
        normal_sidebar_title: gtk::Label,
        normal_main_menu: gtk::MenuButton,
        compact_main_menu: gtk::Button,
        compact_main_menu_label: gtk::Label,
    });
    compact_nav_slot.set_width_request(COMPACT_RAIL_WIDTH);
    compact_nav_slot.set_min_content_width(COMPACT_RAIL_WIDTH);
    compact_nav_slot.set_max_content_width(COMPACT_RAIL_WIDTH);
    compact_nav.set_width_request(COMPACT_RAIL_WIDTH);
    normal_sidebar_title.set_label(DISPLAY_NAME);
    compact_main_menu_label.set_label(&crate::shell::navigation::compact_sidebar_label_text(
        "Menu",
    ));
    normal_window_controls_host.append(&window_controls.start_width_reservation());
    compact_window_controls_host.append(&window_controls.compact_start_reservation());

    let visualizer = build_visualizer();
    let queue_window_controls = window_controls.end_width_reservation();
    let right_panel_parts = build_right_panel(&queue_window_controls, &visualizer.sidebar_area);
    let right_panel = right_panel_parts.root;
    let queue_header_host = right_panel_parts.queue_header_host;
    let queue_panel = right_panel_parts.queue_panel;
    let queue_search = right_panel_parts.queue_search;
    let queue_clear_button = right_panel_parts.queue_clear_button;
    let queue_lyrics_split = right_panel_parts.queue_lyrics_split;
    let lyrics_surface = right_panel_parts.lyrics_surface;
    let lyrics_host = right_panel_parts.lyrics_host;

    let content_chrome = build_content_chrome(&right_panel);
    let route_host = content_chrome.route_host;
    let route_loading = content_chrome.route_loading;
    let right_split = content_chrome.right_split;
    let right_panel_slot = content_chrome.right_panel_slot;
    let right_resize_handle = content_chrome.right_resize_handle;
    let tiny_nav_button = content_chrome.tiny_nav_button;
    let fullscreen_hero_start_controls = window_controls.start_width_reservation();
    let fullscreen_hero_end_controls = window_controls.end_width_reservation();
    let fullscreen_inline_start_controls = window_controls.start_width_reservation();
    let fullscreen_inline_end_controls = window_controls.end_width_reservation();
    let fullscreen_player = build_fullscreen_player(
        &fullscreen_hero_start_controls,
        &fullscreen_hero_end_controls,
        &fullscreen_inline_start_controls,
        &fullscreen_inline_end_controls,
        &visualizer.fullscreen_area,
    );
    let player_controls = build_bottom_player();

    content_row.append(&compact_nav_slot);
    content_row.append(&content_chrome.root);

    split_view.set_min_sidebar_width(NORMAL_SIDEBAR_WIDTH as f64);
    split_view.set_max_sidebar_width(NORMAL_SIDEBAR_WIDTH as f64);
    split_view.set_sidebar(Some(&normal_nav_panel));
    app_content_overlay.add_overlay(&fullscreen_player.root);
    app_content_overlay.set_measure_overlay(&fullscreen_player.root, false);
    let fullscreen_overlay = fullscreen_player.root.clone();
    app_content_overlay.connect_get_child_position(move |overlay, child| {
        if child != &fullscreen_overlay {
            return None;
        }
        // The main child already has this layout pass's allocation; the overlay's cached size may
        // still describe the preceding startup pass.
        let content = overlay.child()?;
        let (width, height) = (content.width(), content.height());
        (width > 0 && height > 0).then(|| gtk::gdk::Rectangle::new(0, 0, width, height))
    });

    app_root.append(&player_controls.root);

    source_refresh_feedback.set_margin_bottom(BOTTOM_PLAYER_HEIGHT + 2);
    operation_feedback.set_margin_bottom(BOTTOM_PLAYER_HEIGHT);
    let layout_state = ShellLayoutState::new(&root_stack);
    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.add_css_class("app-toast-overlay");
    let window_content = window_controls.wrap_content(&layout_state.owner);
    toast_overlay.set_child(Some(&window_content));
    let window = crate::application::application_window(
        app,
        DISPLAY_NAME,
        window_width,
        window_height,
        &toast_overlay,
        window_bar_preview,
    );
    window_controls.bind_window(&window);

    let chrome = WindowChrome {
        application: app.clone(),
        window,
        window_controls,
        toast_overlay,
        control_feedback_label,
        source_refresh_feedback,
        source_refresh_feedback_label,
        source_refresh_feedback_progress,
        operation_feedback,
        operation_feedback_artwork,
        operation_feedback_title,
        operation_feedback_subtitle,
        operation_feedback_action,
        root_stack,
        app_root_overlay,
        app_content_stack,
        login_host,
        startup_loading_host,
    };
    let navigation_view = NavigationWidgets {
        split_view,
        left_resize_handle,
        normal_nav_panel,
        compact_nav_slot,
        tiny_nav_button,
        normal_nav_routes,
        normal_nav_pins,
        compact_nav,
        normal_main_menu: NormalPrimaryMenuWidgets {
            button: normal_main_menu,
            popover: RefCell::new(None),
        },
        compact_main_menu: PrimaryMenuWidgets {
            button: compact_main_menu,
            popover: RefCell::new(None),
        },
    };
    let route_viewport = RouteViewport::new(route_host, route_loading);
    let right_panel = RightPanelWidgets {
        right_split,
        right_panel_slot,
        right_resize_handle,
        root: right_panel,
        queue_header_host,
        queue_panel,
        queue_search,
        queue_clear_button,
        queue_lyrics_split,
        lyrics_surface,
        lyrics_host,
        visualizer_visible: Cell::new(settings.visualizer_panel_visible),
    };
    let player_view = PlayerDesktopWidgets {
        fullscreen_player,
        player_controls,
        visualizer,
    };

    let home_variation_seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos() as i64);
    let shell = Rc::new(Shell {
        quitting,
        home_showcase_variation: Cell::new(home_variation_seed),
        home_explore_variation: Cell::new(home_variation_seed),
        diagnostics,
        appearance,
        settings: settings_state,
        navigation,
        source,
        startup,
        playback: playback_state,
        lyrics: lyrics_state,
        preferences,
        downloads,
        control_feedback,
        desktop,
        artwork,
        selected_ui,
        products,
        chrome,
        layout_state,
        navigation_view,
        route_viewport,
        right_panel,
        player_view,
    });

    shell.connect_operation_feedback();
    normal_primary_menu_button(
        &shell.navigation_view.normal_main_menu.button,
        &shell.navigation_view.normal_main_menu.popover,
        &shell,
    );
    let search_shell = Rc::clone(&shell);
    normal_search.connect_clicked(move |_| {
        search_shell.navigate(crate::routes::route::Route::Search);
    });
    install_normal_navigation_activation(&shell);
    build_normal_navigation(&shell);
    build_compact_navigation(&shell);
    {
        let split_view = shell.navigation_view.split_view.clone();
        shell
            .navigation_view
            .tiny_nav_button
            .connect_clicked(move |_| split_view.set_show_sidebar(true));
    }
    connect_shell_actions(&shell);
    install_application_quit(&shell);
    install_desktop_lifecycle(&shell);
    connect_queue_panel_controls(&shell);
    connect_queue_lyrics_split(&shell);
    shell.connect_route_keyboard();
    connect_transient_entry_focus_dismissal(&shell);
    connect_fullscreen_player_controls(&shell);
    connect_player_controls(&shell);
    warm_audio_output_cache(&shell);
    shell.update_layout();
    if defer_initial_route {
        shell.render_startup_loading_view();
    } else {
        shell.render_current_route();
    }
    shell.render_queue_panel();
    shell.render_lyrics_panel();
    shell.sync_bottom_player_favorite();
    shell.update_bottom_player();
    shell.update_right_panel_button();
    apply_sidebar_media_visibility(Rc::clone(&shell));
    shell.request_initial_lyrics_if_needed();
    install_product_event_receivers(&shell, receivers);

    check_for_release_update(&shell);
    if let Some(presented) = presented {
        let presented = Rc::new(RefCell::new(Some(presented)));
        shell.chrome.window.connect_map(move |_| {
            if let Some(presented) = presented.borrow_mut().take() {
                presented();
            }
        });
    }
    present_initial_window(&shell, force_initial_presentation);
    schedule_periodic_release_checks(&shell);
    if defer_initial_route && !shell.source.operation.borrow().blocks_library() {
        shell.schedule_startup_route_reveal();
    }
}
