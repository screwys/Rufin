use std::cell::{Cell, RefCell};
use std::rc::Rc;
use ui_player::outputs::{default_audio_output_options, warm_audio_output_cache};
use ui_player::right_panel::build_right_panel;
use ui_player::right_panel::{apply_sidebar_media_visibility, connect_queue_lyrics_split};
use ui_player::{
    PlayerDesktopWidgets,
    bottom::{BOTTOM_PLAYER_HEIGHT, build_bottom_player},
    fullscreen::build_fullscreen_player,
    visualizer::build_visualizer,
};
use ui_player::{
    fullscreen::connect_fullscreen_player_controls, queue::connect_queue_panel_controls,
};

use adw::prelude::*;
use app_identity::DISPLAY_NAME;
use tracing::info;

use crate::player::connect_player_controls;
use crate::player::desktop::DesktopState;
use crate::player::desktop::lifecycle::install_application_quit;
use crate::player::right_panel::RightPanelWidgets;
use crate::player::{install_desktop_lifecycle, present_initial_window};
use crate::preferences::PreferencesState;
use crate::preferences::dialogs::release_notes::{
    check_for_release_update, schedule_periodic_release_checks,
};
use crate::preferences::source::SourceState;
use rufin_core::runtime::RuntimeInputs;
use ui_player::lyrics::state::LyricsState;
use ui_player::state::PlaybackState;
use ui_shared::settings::SettingsState;

use super::Shell;
use super::actions::connect_shell_actions;
use ui_shared::feedback::ControlFeedbackState;

use super::chrome::{WindowChrome, WindowControlLayout, build_content_chrome};
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
use ui_shared::artwork::ArtworkState;

pub async fn build(
    app: &adw::Application,
    settings: rufin_core::settings::Settings,
    bootstrap: impl std::future::Future<Output = Result<RuntimeInputs, String>>,
    quitting: Rc<Cell<bool>>,
    force_initial_presentation: bool,
    presented: Option<Box<dyn FnOnce()>>,
    window_bar_preview: Option<crate::application::WindowBarPreview>,
) -> Result<(), String> {
    let appearance = crate::application::style::ApplicationAppearance::install();

    appearance.apply(&settings);
    let (window_width, window_height) =
        initial_window_size(settings.window_width, settings.window_height);
    let window_bar_platform = crate::application::platform_window_bar(window_bar_preview);
    let window_controls = WindowControlLayout::new(window_bar_platform.is_some());

    let shell_root_resource = crate::ui_resource::SHELL_ROOT_RESOURCE;
    let shell_root_builder = ui_shared::ui_resource::builder(shell_root_resource);
    ui_shared::objects!(shell_root_builder, shell_root_resource, {
        root_stack: gtk::Stack,
        app_root_overlay: gtk::Overlay,
        app_root: gtk::Box,
        temporary_storage_banner: adw::Banner,
        app_content_overlay: gtk::Overlay,
        app_content_stack: gtk::Stack,
        split_view: adw::OverlaySplitView,
        content_row: gtk::Box,
        left_resize_handle: gtk::Box,
        control_feedback_label: gtk::Label,
        secret_storage_fallback_dialog: adw::AlertDialog,
        source_refresh_feedback: gtk::Box,
        source_refresh_feedback_label: gtk::Label,
        source_refresh_feedback_progress: gtk::ProgressBar,
        operation_feedback: gtk::Box,
        operation_feedback_artwork: gtk::Box,
        operation_feedback_title: gtk::Label,
        operation_feedback_subtitle: gtk::Label,
        operation_feedback_action: gtk::Button,
        startup_loading_host: gtk::Box,
        startup_loading_status: gtk::Label,
    });
    root_stack.set_width_request(MIN_APP_WINDOW_WIDTH);
    root_stack.set_height_request(MIN_APP_WINDOW_HEIGHT);
    left_resize_handle.set_cursor_from_name(Some("col-resize"));

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

    startup_loading_host.set_visible(true);
    let closing_app = app.clone();
    let closing = window.connect_close_request(move |_| {
        closing_app.quit();
        gtk::glib::Propagation::Proceed
    });
    if cfg!(target_os = "macos")
        || force_initial_presentation
        || !settings.tray_enabled
        || !settings.start_minimized
    {
        crate::application::present_window(&window);
    }
    let inputs = match bootstrap.await {
        Ok(inputs) => inputs,
        Err(error) => {
            window.disconnect(closing);
            window.destroy();
            return Err(error);
        }
    };
    window.disconnect(closing);
    let loaded_at = std::time::Instant::now();
    let RuntimeInputs {
        temporary_store,
        secret_storage_fallbacks,
        diagnostics,
        products,
        settings: settings_handle,
        receivers,
        configured_sources,
        source_operation,
        release_history,
    } = inputs;
    temporary_storage_banner.set_revealed(temporary_store);
    let fresh_start_banner: adw::Banner = ui_shared::ui_resource::object(
        &shell_root_builder,
        shell_root_resource,
        "fresh_start_banner",
    );
    fresh_start_banner.set_revealed(products.library.fresh_start());
    let settings = settings_handle.load();
    appearance.apply(&settings);
    info!(
        elapsed_ms = loaded_at.elapsed().as_millis(),
        "loaded music source presentation"
    );
    let defer_initial_route = configured_sources.selected_source_id.is_some();
    let settings_state = Rc::new(SettingsState {
        current: RefCell::new(settings.clone()),
        persistence: settings_handle,
    });
    let navigation = NavigationState::new();
    let selected_ui = SelectedUiState::new();
    let source = SourceState {
        configured: RefCell::new(configured_sources),
        operation: RefCell::new(source_operation),
        discovered_servers: RefCell::new(Vec::new()),
        discovery_status: RefCell::new(rufin_core::runtime::source::DiscoveryStatus::Idle),
        discovery_running: Cell::new(false),
        discovery_started: Cell::new(false),
        discovery_provider: Cell::new(rufin_core::runtime::source::DiscoveryProvider::Jellyfin),
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
        seek_pointer_active: Cell::new(false),
        volume_persist_source: RefCell::new(None),
        audio_output_options: RefCell::new(default_audio_output_options()),
        audio_output_refresh_running: Cell::new(false),
        audio_output_refresh_generation: Cell::new(0),
        audio_output_refreshed_at: Cell::new(None),
        remote_output_options: RefCell::new(Vec::new()),
    };
    let lyrics_state = LyricsState {
        panel_visible: Cell::new(settings.lyrics_panel_visible),
        dictionary_toast: RefCell::new(None),
    };
    let preferences = PreferencesState {
        secret_storage_row: gtk::glib::WeakRef::new(),
        dialog: gtk::glib::WeakRef::new(),
        release_history: RefCell::new(release_history),
        release_history_view: RefCell::new(None),
        release_check_source: RefCell::new(None),
        preview_windows_updates: window_bar_preview
            == Some(crate::application::WindowBarPreview::Windows),
        release_notification_toast: RefCell::new(None),
        release_updating: RefCell::new(None),
    };
    let downloads = ui_shared::downloads::DownloadsState::new(Rc::clone(&settings_state));

    let desktop = DesktopState::new(app, products.playback.transport.clone());

    let navigation_resource = crate::ui_resource::NAVIGATION_RESOURCE;
    let navigation_builder = ui_shared::ui_resource::builder(navigation_resource);
    ui_shared::objects!(navigation_builder, navigation_resource, {
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
        &super::chrome::top_window_drag_handle("fullscreen-player-drag-handle"),
    );
    let player_controls = build_bottom_player();

    content_row.append(&compact_nav_slot);
    content_row.append(&content_chrome.root);

    split_view.set_min_sidebar_width(NORMAL_SIDEBAR_WIDTH as f64);
    split_view.set_max_sidebar_width(NORMAL_SIDEBAR_WIDTH as f64);
    split_view.set_sidebar(Some(&normal_nav_panel));
    app_content_overlay.add_overlay(&fullscreen_player.root);
    app_content_overlay.set_measure_overlay(&fullscreen_player.root, false);
    app_content_overlay.set_clip_overlay(&fullscreen_player.root, true);
    let fullscreen_overlay = fullscreen_player.root.clone();
    let fullscreen_slide_offset = Rc::clone(&fullscreen_player.slide_offset);
    app_content_overlay.connect_get_child_position(move |overlay, child| {
        if child != &fullscreen_overlay {
            return None;
        }
        // The main child already has this layout pass's allocation; the overlay's cached size may
        // still describe the preceding startup pass.
        let content = overlay.child()?;
        let (width, height) = (content.width(), content.height());
        (width > 0 && height > 0)
            .then(|| gtk::gdk::Rectangle::new(0, fullscreen_slide_offset.get(), width, height))
    });

    app_root.append(&player_controls.root);

    source_refresh_feedback.set_margin_bottom(BOTTOM_PLAYER_HEIGHT + 2);
    operation_feedback.set_margin_bottom(BOTTOM_PLAYER_HEIGHT);

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
        startup_loading_host,
        startup_loading_status,
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
    let player_right_panel = ui_player::right_panel::RightPanelWidgets {
        queue_loading: right_panel_parts.queue_loading,
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
    let right_panel = RightPanelWidgets {
        right_split,
        right_panel_slot,
        right_resize_handle,
    };
    let player_view = PlayerDesktopWidgets {
        fullscreen_player,
        player_controls,
        visualizer,
    };

    let control_feedback =
        ControlFeedbackState::new(&chrome.control_feedback_label, Rc::clone(&settings_state));
    let home_variation_seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos() as i64);
    let shell = Rc::new_cyclic(|weak: &std::rc::Weak<Shell>| {
        let artwork = ArtworkState::new(
            products.artwork.clone(),
            Rc::clone(&settings_state),
            {
                let window = chrome.window.downgrade();
                Box::new(move || {
                    window.upgrade().map_or(1.0, |window| {
                        window.surface().map_or_else(
                            || f64::from(window.scale_factor()),
                            |surface| surface.scale(),
                        )
                    })
                })
            },
            {
                let weak = weak.clone();
                Box::new(move || {
                    if let Some(shell) = weak.upgrade() {
                        shell.try_reveal_startup_route();
                    }
                })
            },
            {
                let weak = weak.clone();
                Box::new(move |generation| {
                    if let Some(shell) = weak.upgrade() {
                        shell.try_finish_route_loading(generation);
                    }
                })
            },
            {
                let weak = weak.clone();
                Box::new(move || {
                    if let Some(shell) = weak.upgrade() {
                        let player = shell.player_ui.selected_playback().as_deref().cloned();
                        shell.refresh_now_playing_notification(player.as_ref());
                        shell.update_media_controls();
                    }
                })
            },
        );

        Shell {
            media_menus: super::media_menus::build(weak, &products, Rc::clone(&settings_state)),
            quitting,
            home_showcase_variation: Cell::new(home_variation_seed),
            home_explore_variation: Cell::new(home_variation_seed),
            diagnostics,
            appearance,
            settings: Rc::clone(&settings_state),
            navigation,
            source,
            startup,
            player_ui: Rc::new(ui_player::PlayerUi::new(
                playback_state,
                lyrics_state,
                player_view,
                player_right_panel,
                Rc::clone(&settings_state),
                products.playback.clone(),
                products.lyrics.clone(),
                &chrome.window,
                &chrome.toast_overlay,
                &right_panel.right_panel_slot,
                Rc::clone(&artwork),
                Rc::clone(&control_feedback),
                products.library.clone(),
                products.runtime.clone(),
                {
                    let weak = weak.clone();
                    Rc::new(move |route| {
                        if let Some(shell) = weak.upgrade() {
                            shell.navigate(route);
                        }
                    })
                },
                {
                    let weak = weak.clone();
                    Rc::new(move || {
                        if let Some(shell) = weak.upgrade() {
                            shell.refresh_bottom_player_owner_details();
                        }
                    })
                },
                {
                    let weak = weak.clone();
                    Rc::new(move |target, row, selection, position| {
                        let Some(shell) = weak.upgrade() else {
                            return;
                        };
                        if let Some(selection) = selection {
                            crate::player::queue::present_queue_selection_context_menu(
                                target, &shell, selection, position,
                            );
                        } else {
                            crate::shell::player_menus::present_queue_track_context_menu(
                                target,
                                &shell,
                                row.media_uri,
                                row.occurrence,
                                position,
                            );
                        }
                    })
                },
                {
                    let weak = weak.clone();
                    Rc::new(move |uri, favorite, button| {
                        if let Some(shell) = weak.upgrade() {
                            shell.set_favorite_with_feedback(
                                library::FavoriteTarget::Track(uri),
                                favorite,
                                Some(button),
                            );
                        }
                    })
                },
                &chrome.app_content_stack,
            )),
            preferences,
            downloads,
            download_feedback: Default::default(),
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
        }
    });

    shell.connect_operation_feedback();
    let weak = Rc::downgrade(&shell);
    fresh_start_banner.connect_button_clicked(move |_| {
        if let Some(shell) = weak.upgrade() {
            crate::preferences::backup::import_dialog(&shell);
        }
    });
    normal_primary_menu_button(
        &shell.navigation_view.normal_main_menu.button,
        &shell.navigation_view.normal_main_menu.popover,
        &shell,
    );
    let search_shell = Rc::clone(&shell);
    normal_search.connect_clicked(move |_| {
        search_shell.navigate(ui_shared::route::Route::Search);
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
    connect_queue_panel_controls(&shell.player_ui);
    connect_queue_lyrics_split(&shell.player_ui);
    shell.connect_route_keyboard();
    connect_transient_entry_focus_dismissal(&shell);
    connect_fullscreen_player_controls(&shell.player_ui);
    connect_player_controls(&shell);
    shell.attach_selected_ui_roots();
    warm_audio_output_cache(&shell.player_ui);
    shell.update_layout();
    if defer_initial_route {
        shell.render_startup_loading_view();
    } else {
        shell.render_current_route();
    }
    shell.player_ui.render_queue_panel();
    shell.player_ui.render_lyrics_panel();
    shell.player_ui.update_bottom_player();
    shell.player_ui.update_right_panel_button();
    apply_sidebar_media_visibility(Rc::clone(&shell.player_ui));
    shell.player_ui.request_initial_lyrics_if_needed();
    install_product_event_receivers(&shell, receivers);
    let weak_shell = Rc::downgrade(&shell);
    gtk::glib::spawn_future_local(async move {
        while let Ok(storage) = secret_storage_fallbacks.recv().await {
            let Some(shell) = weak_shell.upgrade() else {
                break;
            };
            if secret_storage_fallback_dialog
                .clone()
                .choose_future(Some(&shell.chrome.window))
                .await
                != "save"
            {
                continue;
            }
            if let Err(error) = shell
                .products
                .scrobbling
                .save_credentials_to_file(storage)
                .recv()
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result)
            {
                tracing::warn!(%error, "could not save credentials to file");
                shell.control_feedback.show_feedback_toast(error);
                continue;
            }
            let mode = shell.settings.persistence.load().secret_storage_mode;
            shell.settings.current.borrow_mut().secret_storage_mode = mode;
            if let Some(row) = shell.preferences.secret_storage_row.upgrade() {
                row.set_selected(match mode {
                    secrets::SecretStorageMode::ConfigFile => 0,
                    secrets::SecretStorageMode::SystemKeyring => 1,
                });
            }
        }
    });

    check_for_release_update(&shell);
    if let Some(presented) = presented {
        if shell.chrome.window.is_mapped() {
            presented();
        } else {
            let presented = Rc::new(RefCell::new(Some(presented)));
            shell.chrome.window.connect_map(move |_| {
                if let Some(presented) = presented.borrow_mut().take() {
                    presented();
                }
            });
        }
    }
    if shell.source.configured.borrow().sources.is_empty() {
        if shell.chrome.window.is_mapped() {
            shell.present_onboarding();
        } else {
            let pending = Cell::new(true);
            let weak = Rc::downgrade(&shell);
            shell.chrome.window.connect_map(move |_| {
                if !pending.replace(false) {
                    return;
                }
                if let Some(shell) = weak.upgrade() {
                    shell.present_onboarding();
                }
            });
        }
    }
    if !shell.chrome.window.is_visible() {
        present_initial_window(&shell, force_initial_presentation);
    }
    schedule_periodic_release_checks(&shell);
    if defer_initial_route && !shell.source.operation.borrow().blocks_library() {
        shell.schedule_startup_route_reveal();
    }
    Ok(())
}

pub(crate) fn connect_transient_entry_focus_dismissal(shell: &Shell) {
    install_focus_dismissal(
        &shell.chrome.window,
        vec![
            shell.player_ui.right_panel.queue_search.clone().upcast(),
            shell.player_ui.right_panel.lyrics_host.clone().upcast(),
            shell
                .player_ui
                .views
                .fullscreen_player
                .lyrics_host
                .clone()
                .upcast(),
        ],
    );
}

fn install_focus_dismissal(window: &gtk::ApplicationWindow, targets: Vec<gtk::Widget>) {
    let click_root = window.clone();
    let click = gtk::GestureClick::new();
    click.set_button(0);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.connect_pressed(move |gesture, _, x, y| {
        gesture.set_state(gtk::EventSequenceState::Denied);
        let Some(focus) = gtk::prelude::RootExt::focus(&click_root) else {
            return;
        };
        let Some(target) = targets
            .iter()
            .find(|target| target.has_focus() || focus.is_ancestor(*target))
        else {
            return;
        };
        if target.compute_bounds(&click_root).is_none_or(|bounds| {
            bounds.contains_point(&gtk::graphene::Point::new(x as f32, y as f32))
        }) {
            return;
        }
        if let Some(root) = target.root() {
            root.set_focus(None::<&gtk::Widget>);
        }
    });
    window.add_controller(click);
}
