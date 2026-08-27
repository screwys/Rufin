use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

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
    connect_fullscreen_player_controls, connect_player_controls, connect_queue_lyrics_overlay,
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
use localization::{effective_language_preference, set_language_preference, tr};

use super::Shell;
use super::actions::{ControlFeedbackState, connect_shell_actions};
use super::chrome::{
    WindowChrome, WindowControlLayout, build_content_chrome, build_main_area,
    window_drag_handle_with_child,
};
use super::cover::ArtworkState;
use super::events::install_product_event_receivers;
use super::layout::{
    COMPACT_RAIL_WIDTH, MIN_APP_WINDOW_HEIGHT, MIN_APP_WINDOW_WIDTH, NORMAL_SIDEBAR_WIDTH,
    ShellLayoutState,
};
use super::localization::LocalizationState;
use super::navigation::{
    NavigationState, NavigationWidgets, NormalPrimaryMenuWidgets, PrimaryMenuWidgets,
    build_compact_navigation, build_normal_navigation, install_normal_navigation_activation,
    normal_sidebar_header,
};
use super::route::RouteViewport;
use super::selected_ui::SelectedUiState;
use super::startup::StartupState;
use super::window_state::initial_window_size;

fn sidebar_scroll_slot(width: i32, child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let slot = gtk::ScrolledWindow::new();
    slot.set_policy(gtk::PolicyType::Never, gtk::PolicyType::External);
    slot.set_width_request(width);
    slot.set_min_content_width(width);
    slot.set_max_content_width(width);
    slot.set_propagate_natural_width(false);
    slot.set_propagate_natural_height(false);
    slot.set_hexpand(false);
    slot.set_vexpand(true);
    let viewport = gtk::Viewport::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    viewport.set_vscroll_policy(gtk::ScrollablePolicy::Natural);
    viewport.set_child(Some(child));
    slot.set_child(Some(&viewport));
    slot
}

fn sidebar_resize_handle() -> gtk::Box {
    let handle = gtk::Box::new(gtk::Orientation::Vertical, 0);
    handle.add_css_class("sidebar-resize-handle");
    handle.set_width_request(8);
    handle.set_halign(gtk::Align::Start);
    handle.set_valign(gtk::Align::Fill);
    handle.set_vexpand(true);
    handle.set_focusable(false);
    handle.set_cursor_from_name(Some("col-resize"));
    let label = tr("Hold and drag to resize");
    handle.update_property(&[gtk::accessible::Property::Label(&label)]);
    handle
}

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
    let language_preference = effective_language_preference(&settings.language);
    set_language_preference(&language_preference);
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
        dialog: RefCell::new(None),
        release_history: RefCell::new(release_history),
        release_history_list: RefCell::new(None),
        release_notification_toast: RefCell::new(None),
        release_updating: RefCell::new(None),
    };
    let downloads = crate::downloads::DownloadsState::default();
    let control_feedback = ControlFeedbackState {
        generation: Rc::new(Cell::new(0)),
    };
    let localization = LocalizationState {
        bindings: RefCell::new(Vec::new()),
    };
    let desktop = DesktopState::new(app, products.playback.transport.clone());
    let artwork = ArtworkState {
        startup_prime: Default::default(),
        textures: RefCell::new(Default::default()),
    };
    let (window_width, window_height) =
        initial_window_size(settings.window_width, settings.window_height);
    let window_bar_platform = crate::application::platform_window_bar(window_bar_preview);
    let window_controls = WindowControlLayout::new(window_bar_platform.is_some());

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
    startup_loading_host.set_visible(false);

    let upper = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    upper.set_hexpand(true);
    upper.set_vexpand(true);

    let normal_nav = gtk::Box::new(gtk::Orientation::Vertical, 4);
    normal_nav.add_css_class("wide-sidebar");
    normal_nav.set_hexpand(true);
    normal_nav.set_vexpand(true);
    normal_nav.set_width_request(1);
    let normal_nav_routes = adw::Sidebar::new();
    normal_nav_routes.set_mode(adw::SidebarMode::Sidebar);
    normal_nav_routes.set_vexpand(false);
    let normal_nav_pins = gtk::Box::new(gtk::Orientation::Vertical, 4);
    normal_nav_pins.set_vexpand(false);
    normal_nav.append(&normal_nav_routes);
    normal_nav.append(&normal_nav_pins);
    let normal_nav_handle = window_drag_handle_with_child("sidebar-drag-handle", &normal_nav);
    normal_nav_handle.set_vexpand(true);
    normal_nav_handle.set_valign(gtk::Align::Fill);
    let normal_nav_scroller = sidebar_scroll_slot(NORMAL_SIDEBAR_WIDTH, &normal_nav_handle);
    normal_nav_scroller.set_width_request(1);
    normal_nav_scroller.set_min_content_width(1);
    normal_nav_scroller.set_max_content_width(-1);
    let normal_nav_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    normal_nav_panel.set_width_request(1);
    normal_nav_panel.set_vexpand(true);
    normal_nav_panel.add_css_class("sidebar-pane");
    normal_nav_panel.add_css_class("wide-sidebar-slot");
    normal_nav_panel.append(&normal_nav_scroller);

    let compact_nav = gtk::Box::new(gtk::Orientation::Vertical, 8);
    compact_nav.add_css_class("compact-rail");
    compact_nav.set_hexpand(false);
    compact_nav.set_vexpand(true);
    compact_nav.set_width_request(COMPACT_RAIL_WIDTH);
    let compact_nav_handle = window_drag_handle_with_child("sidebar-drag-handle", &compact_nav);
    compact_nav_handle.set_vexpand(true);
    compact_nav_handle.set_valign(gtk::Align::Fill);
    let compact_nav_slot = sidebar_scroll_slot(COMPACT_RAIL_WIDTH, &compact_nav_handle);
    compact_nav_slot.add_css_class("sidebar-pane");
    compact_nav_slot.add_css_class("compact-rail-slot");
    let normal_main_menu = gtk::MenuButton::new();
    let compact_main_menu = gtk::Button::new();

    let main_area_parts = build_main_area();
    let main_area = main_area_parts.root;
    let route_host = main_area_parts.route_host;

    let visualizer = build_visualizer();
    let queue_window_controls = window_controls.end_width_reservation();
    let right_panel_parts = build_right_panel(&queue_window_controls, &visualizer.sidebar_area);
    let right_panel = right_panel_parts.root;
    let queue_panel = right_panel_parts.queue_panel;
    let queue_search = right_panel_parts.queue_search;
    let queue_clear_button = right_panel_parts.queue_clear_button;
    let queue_lyrics_overlay = right_panel_parts.queue_lyrics_overlay;
    let lyrics_surface = right_panel_parts.lyrics_surface;
    let lyrics_resize_handle = right_panel_parts.lyrics_resize_handle;
    let lyrics_host = right_panel_parts.lyrics_host;

    let content_chrome = build_content_chrome(&main_area, &right_panel);
    let right_split = content_chrome.right_split;
    let right_panel_slot = content_chrome.right_panel_slot;
    let right_resize_handle = content_chrome.right_resize_handle;
    let tiny_nav_button = gtk::Button::from_icon_name("rufin-sidebar-show-symbolic");
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
    let fullscreen_hero_window_controls = window_controls.start_width_reservation();
    let fullscreen_inline_window_controls = window_controls.start_width_reservation();
    let fullscreen_player = build_fullscreen_player(
        &fullscreen_hero_window_controls,
        &fullscreen_inline_window_controls,
        &visualizer.fullscreen_area,
    );
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
    split_view.set_sidebar(Some(&normal_nav_panel));
    split_view.set_content(Some(&content_row));

    let shell_layout = gtk::Overlay::new();
    shell_layout.set_hexpand(true);
    shell_layout.set_vexpand(true);
    shell_layout.set_child(Some(&split_view));
    let left_resize_handle = sidebar_resize_handle();
    shell_layout.add_overlay(&left_resize_handle);
    shell_layout.set_measure_overlay(&left_resize_handle, false);
    upper.append(&shell_layout);

    app_content_stack.add_named(&upper, Some("main"));
    app_content_overlay.set_child(Some(&app_content_stack));
    app_content_overlay.add_overlay(&fullscreen_player.root);
    app_content_overlay.set_measure_overlay(&fullscreen_player.root, false);

    app_root.append(&app_content_overlay);
    let bottom_player_handle =
        window_drag_handle_with_child("bottom-player-drag-handle", &player_controls.root);
    app_root.append(&bottom_player_handle);

    let app_root_overlay = gtk::Overlay::new();
    app_root_overlay.set_hexpand(true);
    app_root_overlay.set_vexpand(true);
    app_root_overlay.set_child(Some(&app_root));
    let control_feedback_label = gtk::Label::new(None);
    control_feedback_label.add_css_class("control-feedback-toast");
    control_feedback_label.set_halign(gtk::Align::Center);
    control_feedback_label.set_valign(gtk::Align::End);
    control_feedback_label.set_visible(false);
    app_root_overlay.add_overlay(&control_feedback_label);
    app_root_overlay.set_measure_overlay(&control_feedback_label, false);
    let source_refresh_feedback = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    source_refresh_feedback.add_css_class("source-refresh-feedback");
    source_refresh_feedback.set_halign(gtk::Align::Center);
    source_refresh_feedback.set_valign(gtk::Align::End);
    source_refresh_feedback.set_margin_bottom(BOTTOM_PLAYER_HEIGHT + 2);
    source_refresh_feedback.set_visible(false);
    source_refresh_feedback.set_can_target(false);
    let source_refresh_icon = gtk::Image::from_icon_name("rufin-view-refresh-symbolic");
    source_refresh_icon.set_pixel_size(16);
    source_refresh_feedback.append(&source_refresh_icon);
    let source_refresh_content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let source_refresh_feedback_label = gtk::Label::new(None);
    source_refresh_feedback_label.add_css_class("caption-heading");
    source_refresh_feedback_label.set_xalign(0.0);
    source_refresh_feedback_label.set_max_width_chars(36);
    source_refresh_feedback_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    source_refresh_feedback_label.set_single_line_mode(true);
    source_refresh_content.append(&source_refresh_feedback_label);
    let source_refresh_feedback_progress = gtk::ProgressBar::new();
    source_refresh_feedback_progress.add_css_class("source-refresh-progress");
    source_refresh_feedback_progress.set_size_request(250, -1);
    source_refresh_content.append(&source_refresh_feedback_progress);
    source_refresh_feedback.append(&source_refresh_content);
    app_root_overlay.add_overlay(&source_refresh_feedback);
    app_root_overlay.set_measure_overlay(&source_refresh_feedback, false);
    let operation_feedback = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    operation_feedback.add_css_class("operation-feedback");
    operation_feedback.set_halign(gtk::Align::Center);
    operation_feedback.set_valign(gtk::Align::End);
    operation_feedback.set_margin_bottom(BOTTOM_PLAYER_HEIGHT);
    operation_feedback.set_visible(false);
    operation_feedback.set_focusable(true);
    let operation_feedback_artwork = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    operation_feedback_artwork.set_size_request(48, 48);
    operation_feedback.append(&operation_feedback_artwork);
    let operation_feedback_text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    operation_feedback_text.set_valign(gtk::Align::Center);
    operation_feedback_text.set_hexpand(true);
    let operation_feedback_title = gtk::Label::new(None);
    operation_feedback_title.add_css_class("heading");
    operation_feedback_title.set_xalign(0.0);
    operation_feedback_title.set_max_width_chars(34);
    operation_feedback_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    operation_feedback_title.set_single_line_mode(true);
    let operation_feedback_subtitle = gtk::Label::new(None);
    operation_feedback_subtitle.add_css_class("dim-label");
    operation_feedback_subtitle.set_xalign(0.0);
    operation_feedback_subtitle.set_max_width_chars(34);
    operation_feedback_subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
    operation_feedback_subtitle.set_single_line_mode(true);
    operation_feedback_text.append(&operation_feedback_title);
    operation_feedback_text.append(&operation_feedback_subtitle);
    operation_feedback.append(&operation_feedback_text);
    let operation_feedback_close = gtk::Button::from_icon_name("rufin-window-close-symbolic");
    operation_feedback_close.add_css_class("flat");
    operation_feedback_close.set_valign(gtk::Align::Center);
    operation_feedback_close.set_tooltip_text(Some(&tr("Close")));
    operation_feedback.append(&operation_feedback_close);
    app_root_overlay.add_overlay(&operation_feedback);
    app_root_overlay.set_measure_overlay(&operation_feedback, false);
    app_root_overlay.add_overlay(&startup_loading_host);
    app_root_overlay.set_measure_overlay(&startup_loading_host, false);

    root_stack.add_named(&login_host, Some("login"));
    root_stack.add_named(&app_root_overlay, Some("app"));
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
        operation_feedback_close,
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
    let route_viewport = RouteViewport::new(route_host);
    let right_panel = RightPanelWidgets {
        right_split,
        right_panel_slot,
        right_resize_handle,
        root: right_panel,
        queue_panel,
        queue_search,
        queue_clear_button,
        queue_lyrics_overlay,
        lyrics_surface,
        lyrics_resize_handle,
        lyrics_host,
        visualizer_visible: Cell::new(settings.visualizer_panel_visible),
    };
    let player_view = PlayerDesktopWidgets {
        fullscreen_player,
        player_controls,
        visualizer,
    };

    let shell = Rc::new(Shell {
        quitting,
        home_variation: Cell::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos() as i64),
        ),
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
        localization,
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
    {
        let release_updates = Arc::clone(&shell.products.release_updates);
        let was_active = Cell::new(shell.chrome.window.is_active());
        shell.chrome.window.connect_is_active_notify(move |window| {
            let active = window.is_active();
            let previous = was_active.replace(active);
            if active && !previous {
                release_updates.check();
            }
        });
    }
    shell
        .navigation_view
        .normal_nav_panel
        .prepend(&normal_sidebar_header(
            &shell,
            &shell.chrome.window_controls.start_width_reservation(),
        ));
    install_normal_navigation_activation(&shell);
    build_normal_navigation(&shell);
    build_compact_navigation(&shell);
    shell.install_locale_bindings();
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
    connect_queue_lyrics_overlay(&shell);
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
    shell.update_fullscreen_player();
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
