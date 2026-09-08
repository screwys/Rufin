use std::rc::Rc;
use ui_player::outputs::{select_next_audio_output, select_previous_audio_output};

use adw::prelude::*;
use app_identity::{APP_ID, DISPLAY_NAME};
use gtk::{gio, glib};
use playback::{PlaybackTransitionMode, QueuePlacement};

use crate::player::play_saved_random;
use crate::preferences::present_preferences_dialog;
use crate::preferences::source::selector::install_source_menu_actions;
#[cfg(any(target_os = "macos", test))]
use localization::tr_with;
use localization::{TRANSLATOR_CREDITS, tr};
use ui_shared::popup::present_light_dismiss_dialog;

use super::{Shell, layout, navigation};

const KEY_SEEK_SECONDS: i32 = 10;
const KEY_VOLUME_STEP: f64 = 0.05;

pub(crate) fn connect_shell_actions(shell: &Rc<Shell>) {
    install_window_actions(shell);
    install_application_actions(shell);
    install_platform_menu(shell);
    navigation::install_mouse_history_buttons(shell);
    layout::connect_shell_layout(shell);
}

pub(crate) fn install_window_actions(shell: &Rc<Shell>) {
    install_source_menu_actions(shell);
    add_window_action(shell, "import-playlist", &[], {
        let shell = Rc::clone(shell);
        move || shell.import_playlist_dialog()
    });

    add_window_action(shell, "new-playlist", &[], {
        let shell = Rc::clone(shell);
        move || shell.new_playlist_dialog()
    });

    add_window_action(shell, "new-smart-playlist", &[], {
        let shell = Rc::clone(shell);
        move || shell.new_smart_playlist_dialog()
    });

    let go_back = gio::SimpleAction::new("go-back", None);
    let go_back_shell = Rc::clone(shell);
    go_back.connect_activate(move |_, _| go_back_shell.go_back());
    shell.chrome.window.add_action(&go_back);

    let go_forward = gio::SimpleAction::new("go-forward", None);
    let go_forward_shell = Rc::clone(shell);
    go_forward.connect_activate(move |_, _| go_forward_shell.go_forward());
    shell.chrome.window.add_action(&go_forward);

    #[cfg(target_os = "macos")]
    let troubleshooting_accels = &["<Alt><Meta>i"][..];
    #[cfg(not(target_os = "macos"))]
    let troubleshooting_accels = &["<Control><Shift>i"][..];
    add_window_action(shell, "troubleshooting", troubleshooting_accels, {
        let shell = Rc::clone(shell);
        move || super::diagnostics::present_diagnostics(&shell)
    });

    #[cfg(target_os = "macos")]
    let left_sidebar_accels = &["<Alt><Meta>s"][..];
    #[cfg(not(target_os = "macos"))]
    let left_sidebar_accels = &["<Control><Alt>s"][..];
    add_window_action(shell, "toggle-left-sidebar", left_sidebar_accels, {
        let shell = Rc::clone(shell);
        move || shell.toggle_active_left_sidebar_size()
    });

    #[cfg(target_os = "macos")]
    let private_mode_accels = &["<Alt><Meta>p"][..];
    #[cfg(not(target_os = "macos"))]
    let private_mode_accels = &["<Control><Alt>p"][..];
    add_window_action(shell, "toggle-private-mode", private_mode_accels, {
        let shell = Rc::clone(shell);
        move || {
            let enabled = !shell.settings.current.borrow().private_mode;
            shell.set_private_mode(enabled);
            if shell.settings.current.borrow().private_mode == enabled {
                shell
                    .control_feedback
                    .show_control_feedback_toast(if enabled {
                        tr("Private mode is on")
                    } else {
                        tr("Private mode is off")
                    });
            }
        }
    });

    let fullscreen = gio::SimpleAction::new("toggle-fullscreen", None);
    let fullscreen_shell = Rc::clone(shell);
    fullscreen.connect_activate(move |_, _| {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            if fullscreen_shell.chrome.window.is_maximized() {
                fullscreen_shell.chrome.window.unmaximize();
            } else {
                fullscreen_shell.chrome.window.maximize();
            }
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            if fullscreen_shell.chrome.window.is_fullscreen() {
                fullscreen_shell.chrome.window.unfullscreen();
            } else {
                fullscreen_shell.chrome.window.fullscreen();
            }
        }
    });
    shell.chrome.window.add_action(&fullscreen);

    #[cfg(target_os = "macos")]
    let play_pause_accels = &[][..];
    #[cfg(not(target_os = "macos"))]
    let play_pause_accels = &["<Control>space"][..];
    add_window_action(shell, "play-pause", play_pause_accels, {
        let transport = shell.products.playback.transport.clone();
        move || transport.play_pause()
    });
    let navigate_sidebar =
        gio::SimpleAction::new("navigate-sidebar", Some(glib::VariantTy::UINT32));
    let navigate_shell = Rc::clone(shell);
    navigate_sidebar.connect_activate(move |_, parameter| {
        let Some(position) = parameter.and_then(|position| position.get::<u32>()) else {
            return;
        };
        if let Some(route) =
            navigation::sidebar_route_at_position(&navigate_shell, position as usize)
        {
            navigate_shell.navigate(route);
        }
    });
    shell.chrome.window.add_action(&navigate_sidebar);
    for position in 1..=10 {
        let target = (position as u32).to_variant();
        let action_name = gio::Action::print_detailed_name("win.navigate-sidebar", Some(&target));
        let accelerator_position = position % 10;
        #[cfg(target_os = "macos")]
        let accelerator = format!("<Meta>{accelerator_position}");
        #[cfg(not(target_os = "macos"))]
        let accelerator = format!("<Control>{accelerator_position}");
        shell
            .chrome
            .application
            .set_accels_for_action(&action_name, &[&accelerator]);
    }
    #[cfg(target_os = "macos")]
    let previous_track_accels = &["<Meta>Left"][..];
    #[cfg(not(target_os = "macos"))]
    let previous_track_accels = &["<Control>b"][..];
    add_window_action(shell, "previous-track", previous_track_accels, {
        let transport = shell.products.playback.transport.clone();
        move || transport.previous()
    });
    #[cfg(target_os = "macos")]
    let next_track_accels = &["<Meta>Right"][..];
    #[cfg(not(target_os = "macos"))]
    let next_track_accels = &["<Control>n"][..];
    add_window_action(shell, "next-track", next_track_accels, {
        let transport = shell.products.playback.transport.clone();
        move || transport.next()
    });
    #[cfg(target_os = "macos")]
    let seek_backward_accels = &["<Shift><Meta>Left"][..];
    #[cfg(not(target_os = "macos"))]
    let seek_backward_accels = &["<Control>Left"][..];
    add_window_action(shell, "seek-backward", seek_backward_accels, {
        let shell = Rc::clone(shell);
        move || seek_by(&shell, -KEY_SEEK_SECONDS)
    });
    #[cfg(target_os = "macos")]
    let seek_forward_accels = &["<Shift><Meta>Right"][..];
    #[cfg(not(target_os = "macos"))]
    let seek_forward_accels = &["<Control>Right"][..];
    add_window_action(shell, "seek-forward", seek_forward_accels, {
        let shell = Rc::clone(shell);
        move || seek_by(&shell, KEY_SEEK_SECONDS)
    });
    #[cfg(target_os = "macos")]
    let shuffle_accels = &["<Meta>s"][..];
    #[cfg(not(target_os = "macos"))]
    let shuffle_accels = &["<Control>s"][..];
    add_window_action(shell, "toggle-shuffle", shuffle_accels, {
        let shell = Rc::clone(shell);
        move || toggle_shuffle_shortcut(&shell)
    });
    #[cfg(target_os = "macos")]
    let repeat_accels = &["<Meta>r"][..];
    #[cfg(not(target_os = "macos"))]
    let repeat_accels = &["<Control>r"][..];
    add_window_action(shell, "cycle-repeat", repeat_accels, {
        let shell = Rc::clone(shell);
        move || cycle_repeat_shortcut(&shell)
    });
    #[cfg(target_os = "macos")]
    let search_accels = &["<Meta>f"][..];
    #[cfg(not(target_os = "macos"))]
    let search_accels = &["<Control>f"][..];
    add_window_action(shell, "focus-search", search_accels, {
        let shell = Rc::clone(shell);
        move || shell.focus_current_route_search()
    });
    #[cfg(target_os = "macos")]
    let navigate_search_accels = &["<Meta>k"][..];
    #[cfg(not(target_os = "macos"))]
    let navigate_search_accels = &["<Control>k"][..];
    add_window_action(shell, "navigate-search", navigate_search_accels, {
        let shell = Rc::clone(shell);
        move || shell.navigate(ui_shared::route::Route::Search)
    });
    #[cfg(target_os = "macos")]
    let cycle_layout_accels = &["<Meta>j"][..];
    #[cfg(not(target_os = "macos"))]
    let cycle_layout_accels = &["<Control>j"][..];
    add_window_action(shell, "cycle-layout", cycle_layout_accels, {
        let shell = Rc::clone(shell);
        move || shell.cycle_current_route_layout()
    });
    add_window_action(shell, "cycle-tabs", &["<Control>Tab"], {
        let shell = Rc::clone(shell);
        move || shell.cycle_current_route_tabs()
    });
    #[cfg(target_os = "macos")]
    let favorite_accels = &["<Meta>l"][..];
    #[cfg(not(target_os = "macos"))]
    let favorite_accels = &["<Control>l"][..];
    add_window_action(shell, "toggle-favorite", favorite_accels, {
        let shell = Rc::clone(shell);
        move || shell.toggle_current_track_favorite()
    });
    #[cfg(target_os = "macos")]
    let auto_dj_accels = &["<Alt>space"][..];
    #[cfg(not(target_os = "macos"))]
    let auto_dj_accels = &["<Control>d"][..];
    add_window_action(shell, "toggle-auto-dj", auto_dj_accels, {
        let shell = Rc::clone(shell);
        move || toggle_auto_dj_shortcut(&shell)
    });
    #[cfg(target_os = "macos")]
    let clear_queue_accels = &["<Meta>period"][..];
    #[cfg(not(target_os = "macos"))]
    let clear_queue_accels = &["<Control>period"][..];
    add_window_action(shell, "clear-queue", clear_queue_accels, {
        let shell = Rc::clone(shell);
        move || shell.player_ui.clear_queue()
    });
    #[cfg(target_os = "macos")]
    let random_accels = &["<Alt><Meta>r"][..];
    #[cfg(not(target_os = "macos"))]
    let random_accels = &["<Control><Shift>r"][..];
    add_window_action(shell, "play-random", random_accels, {
        let shell = Rc::clone(shell);
        move || play_saved_random(&shell, QueuePlacement::Now)
    });
    #[cfg(target_os = "macos")]
    let random_next_accels = &["<Alt><Meta>n"][..];
    #[cfg(not(target_os = "macos"))]
    let random_next_accels = &["<Control><Shift>n"][..];
    add_window_action(shell, "play-random-next", random_next_accels, {
        let shell = Rc::clone(shell);
        move || play_saved_random(&shell, QueuePlacement::Next)
    });
    #[cfg(target_os = "macos")]
    let random_later_accels = &["<Alt><Meta>t"][..];
    #[cfg(not(target_os = "macos"))]
    let random_later_accels = &["<Control><Shift>t"][..];
    add_window_action(shell, "play-random-later", random_later_accels, {
        let shell = Rc::clone(shell);
        move || play_saved_random(&shell, QueuePlacement::Last)
    });
    #[cfg(target_os = "macos")]
    let gapless_accels = &["<Alt><Meta>g"][..];
    #[cfg(not(target_os = "macos"))]
    let gapless_accels = &["<Control><Shift>g"][..];
    add_window_action(shell, "use-gapless", gapless_accels, {
        let shell = Rc::clone(shell);
        move || set_transition_mode_shortcut(&shell, PlaybackTransitionMode::Gapless)
    });
    #[cfg(target_os = "macos")]
    let crossfade_accels = &["<Alt><Meta>c"][..];
    #[cfg(not(target_os = "macos"))]
    let crossfade_accels = &["<Control><Shift>c"][..];
    add_window_action(shell, "use-crossfade", crossfade_accels, {
        let shell = Rc::clone(shell);
        move || set_transition_mode_shortcut(&shell, PlaybackTransitionMode::Crossfade)
    });
    add_window_action(shell, "mute", &[], {
        let shell = Rc::clone(shell);
        move || toggle_mute_shortcut(&shell)
    });
    #[cfg(target_os = "macos")]
    let volume_up_accels = &["<Meta>Up"][..];
    #[cfg(not(target_os = "macos"))]
    let volume_up_accels = &["<Control>Up"][..];
    add_window_action(shell, "volume-up", volume_up_accels, {
        let shell = Rc::clone(shell);
        move || adjust_volume(&shell, KEY_VOLUME_STEP)
    });
    #[cfg(target_os = "macos")]
    let volume_down_accels = &["<Meta>Down"][..];
    #[cfg(not(target_os = "macos"))]
    let volume_down_accels = &["<Control>Down"][..];
    add_window_action(shell, "volume-down", volume_down_accels, {
        let shell = Rc::clone(shell);
        move || adjust_volume(&shell, -KEY_VOLUME_STEP)
    });
    add_window_action(shell, "previous-audio-output", &[], {
        let shell = Rc::clone(shell);
        move || select_previous_audio_output(&shell.player_ui)
    });
    add_window_action(shell, "next-audio-output", &[], {
        let shell = Rc::clone(shell);
        move || select_next_audio_output(&shell.player_ui)
    });
    #[cfg(target_os = "macos")]
    let queue_accels = &["<Alt><Meta>u"][..];
    #[cfg(not(target_os = "macos"))]
    let queue_accels = &["F9"][..];
    add_window_action(shell, "toggle-queue", queue_accels, {
        let shell = Rc::clone(shell);
        move || shell.toggle_right_panel()
    });
    #[cfg(target_os = "macos")]
    let visualizer_panel_accels = &["<Alt><Meta>v"][..];
    #[cfg(not(target_os = "macos"))]
    let visualizer_panel_accels = &["<Control><Shift>v"][..];
    add_window_action(shell, "show-visualizer-panel", visualizer_panel_accels, {
        let shell = Rc::clone(shell);
        move || shell.set_right_panel_media_visibility(false, true)
    });
    #[cfg(target_os = "macos")]
    let lyrics_panel_accels = &["<Alt><Meta>l"][..];
    #[cfg(not(target_os = "macos"))]
    let lyrics_panel_accels = &["<Control><Shift>l"][..];
    add_window_action(shell, "show-lyrics-panel", lyrics_panel_accels, {
        let shell = Rc::clone(shell);
        move || shell.set_right_panel_media_visibility(true, false)
    });
    #[cfg(target_os = "macos")]
    let visualizer_lyrics_panel_accels = &["<Alt><Meta>b"][..];
    #[cfg(not(target_os = "macos"))]
    let visualizer_lyrics_panel_accels = &["<Control><Shift>b"][..];
    add_window_action(
        shell,
        "show-visualizer-lyrics-panel",
        visualizer_lyrics_panel_accels,
        {
            let shell = Rc::clone(shell);
            move || shell.set_right_panel_media_visibility(true, true)
        },
    );
    add_window_action(shell, "toggle-lyrics", &[], {
        let shell = Rc::clone(shell);
        move || shell.toggle_lyrics_panel()
    });
    #[cfg(target_os = "macos")]
    let refresh_library_accels = &["<Control><Meta>r"][..];
    #[cfg(not(target_os = "macos"))]
    let refresh_library_accels = &["F5"][..];
    add_window_action(shell, "refresh-library", refresh_library_accels, {
        let shell = Rc::clone(shell);
        move || refresh_selected_library(&shell)
    });
    #[cfg(target_os = "macos")]
    let fullscreen_player_accels = &["<Control><Meta>f"][..];
    #[cfg(not(target_os = "macos"))]
    let fullscreen_player_accels = &["<Shift>F11"][..];
    add_window_action(
        shell,
        "toggle-fullscreen-player",
        fullscreen_player_accels,
        {
            let shell = Rc::clone(shell);
            move || shell.player_ui.toggle_fullscreen_player()
        },
    );
    #[cfg(target_os = "macos")]
    let primary_menu_accels = &["<Control><Meta>m"][..];
    #[cfg(not(target_os = "macos"))]
    let primary_menu_accels = &["F10"][..];
    add_window_action(shell, "show-primary-menu", primary_menu_accels, {
        let shell = Rc::clone(shell);
        move || navigation::popup_primary_menu(&shell)
    });
    #[cfg(target_os = "macos")]
    let release_notes_accels = &["<Meta>y"][..];
    #[cfg(not(target_os = "macos"))]
    let release_notes_accels = &["<Control>h"][..];
    add_window_action(shell, "show-release-notes", release_notes_accels, {
        let shell = Rc::clone(shell);
        move || shell.present_release_notes()
    });

    #[cfg(target_os = "macos")]
    {
        shell
            .chrome
            .application
            .set_accels_for_action("win.go-back", &["<Meta>bracketleft"]);
        shell
            .chrome
            .application
            .set_accels_for_action("win.go-forward", &["<Meta>bracketright"]);
        shell
            .chrome
            .application
            .set_accels_for_action("win.toggle-fullscreen", &["<Control><Meta>f"]);
    }
    #[cfg(not(target_os = "macos"))]
    {
        shell
            .chrome
            .application
            .set_accels_for_action("win.go-back", &["<Alt>Left"]);
        shell
            .chrome
            .application
            .set_accels_for_action("win.go-forward", &["<Alt>Right"]);
        shell
            .chrome
            .application
            .set_accels_for_action("win.toggle-fullscreen", &["F11"]);
    }
}

fn install_application_actions(shell: &Rc<Shell>) {
    let preferences = gio::SimpleAction::new("preferences", None);
    let preferences_shell = Rc::downgrade(shell);
    preferences.connect_activate(move |_, _| {
        if let Some(shell) = preferences_shell.upgrade() {
            present_preferences_dialog(&shell);
        }
    });
    shell.chrome.application.add_action(&preferences);

    let shortcuts = gio::SimpleAction::new("show-shortcuts", None);
    let shortcuts_shell = Rc::downgrade(shell);
    shortcuts.connect_activate(move |_, _| {
        if let Some(shell) = shortcuts_shell.upgrade() {
            show_shortcuts_dialog(&shell);
        }
    });
    shell.chrome.application.add_action(&shortcuts);

    let about = gio::SimpleAction::new("about", None);
    let about_shell = Rc::downgrade(shell);
    about.connect_activate(move |_, _| {
        if let Some(shell) = about_shell.upgrade() {
            show_about_dialog(&shell);
        }
    });
    shell.chrome.application.add_action(&about);

    #[cfg(target_os = "macos")]
    {
        shell
            .chrome
            .application
            .set_accels_for_action("app.preferences", &["<Meta>comma"]);
        shell
            .chrome
            .application
            .set_accels_for_action("app.show-shortcuts", &["<Meta>question"]);
    }
    #[cfg(not(target_os = "macos"))]
    {
        shell
            .chrome
            .application
            .set_accels_for_action("app.preferences", &["<Control>comma"]);
        shell
            .chrome
            .application
            .set_accels_for_action("app.show-shortcuts", &["<Control>question"]);
    }
}

#[cfg(target_os = "macos")]
fn install_platform_menu(shell: &Shell) {
    shell
        .chrome
        .application
        .set_menubar(Some(&macos_menu_model()));
}

#[cfg(any(target_os = "macos", test))]
fn macos_menu_model() -> gio::Menu {
    let menu = gio::Menu::new();

    let edit = gio::Menu::new();
    append_macos_menu_section(
        &edit,
        &[(tr("Undo"), "text.undo"), (tr("Redo"), "text.redo")],
    );
    append_macos_menu_section(
        &edit,
        &[
            (tr("Cut"), "clipboard.cut"),
            (tr("Copy"), "clipboard.copy"),
            (tr("Paste"), "clipboard.paste"),
            (tr("Delete"), "selection.delete"),
            (tr("Select All"), "selection.select-all"),
        ],
    );
    menu.append_submenu(Some(&tr("Edit")), &edit);

    let view = gio::Menu::new();
    append_macos_menu_section(
        &view,
        &[
            (tr("Back"), "win.go-back"),
            (tr("Forward"), "win.go-forward"),
            (tr("Search"), "win.focus-search"),
            (tr("Navigate to Search"), "win.navigate-search"),
            (tr("Switch between layouts"), "win.cycle-layout"),
            (tr("Switch Search/Favorites tabs"), "win.cycle-tabs"),
            (tr("Menu"), "win.show-primary-menu"),
        ],
    );

    let sidebar_routes = gio::Menu::new();
    for position in 1..=10 {
        let position_label = position.to_string();
        let label = tr_with(
            "Sidebar item {position}",
            &[("position", position_label.as_str())],
        );
        let target = (position as u32).to_variant();
        let action = gio::Action::print_detailed_name("win.navigate-sidebar", Some(&target));
        sidebar_routes.append(Some(&label), Some(action.as_str()));
    }
    view.append_submenu(Some(&tr("Sidebar Items")), &sidebar_routes);

    append_macos_menu_section(
        &view,
        &[
            (tr("Collapse/expand sidebar"), "win.toggle-left-sidebar"),
            (tr("Show/hide right sidebar"), "win.toggle-queue"),
            (tr("Show/hide lyrics"), "win.toggle-lyrics"),
            (tr("Private mode"), "win.toggle-private-mode"),
            (tr("Toggle Fullscreen"), "win.toggle-fullscreen"),
        ],
    );
    menu.append_submenu(Some(&tr("View")), &view);

    let playback = gio::Menu::new();
    append_macos_menu_section(
        &playback,
        &[
            (tr("Play/Pause"), "win.play-pause"),
            (tr("Previous Track"), "win.previous-track"),
            (tr("Next Track"), "win.next-track"),
        ],
    );
    append_macos_menu_section(
        &playback,
        &[
            (tr("Seek Backward"), "win.seek-backward"),
            (tr("Seek Forward"), "win.seek-forward"),
        ],
    );
    append_macos_menu_section(
        &playback,
        &[
            (tr("Shuffle"), "win.toggle-shuffle"),
            (tr("Repeat"), "win.cycle-repeat"),
            (tr("Favorite"), "win.toggle-favorite"),
            (tr("Auto DJ"), "win.toggle-auto-dj"),
            (tr("Clear queue"), "win.clear-queue"),
        ],
    );
    append_macos_menu_section(
        &playback,
        &[
            (tr("Mute"), "win.mute"),
            (tr("Volume Up"), "win.volume-up"),
            (tr("Volume Down"), "win.volume-down"),
        ],
    );
    menu.append_submenu(Some(&tr("Playback")), &playback);

    let window = gio::Menu::new();
    window.append(Some(&tr("Close Window")), Some("window.close"));
    let window_item = gio::MenuItem::new_submenu(Some(&tr("Window")), &window);
    window_item.set_attribute_value("gtk-macos-special", Some(&"window-submenu".to_variant()));
    menu.append_item(&window_item);

    let help = gio::Menu::new();
    help.append(Some(&tr("Keyboard Shortcuts")), Some("app.show-shortcuts"));
    help.append(Some(&tr("Version History")), Some("win.show-release-notes"));
    help.append(Some(&tr("Troubleshooting")), Some("win.troubleshooting"));
    menu.append_submenu(Some(&tr("Help")), &help);

    menu
}

#[cfg(any(target_os = "macos", test))]
fn append_macos_menu_section(menu: &gio::Menu, actions: &[(String, &str)]) {
    let section = gio::Menu::new();
    for (label, action) in actions {
        section.append(Some(label), Some(action));
    }
    menu.append_section(None, &section);
}

#[cfg(not(target_os = "macos"))]
fn install_platform_menu(_shell: &Shell) {}

fn add_window_action(
    shell: &Rc<Shell>,
    name: &str,
    accels: &[&str],
    activate: impl Fn() + 'static,
) {
    let action = gio::SimpleAction::new(name, None);
    action.connect_activate(move |_, _| activate());
    shell.chrome.window.add_action(&action);
    if !accels.is_empty() {
        shell
            .chrome
            .application
            .set_accels_for_action(&format!("win.{name}"), accels);
    }
}

fn seek_by(shell: &Shell, delta_seconds: i32) {
    let Some(seconds) = ({
        let player = shell.player_ui.selected_playback();
        let Some(player) = player.as_ref() else {
            return;
        };
        let duration_seconds =
            (player.transport.duration_millis / 1_000).min(u64::from(u32::MAX)) as u32;
        if player.transport.current.is_none() || duration_seconds == 0 {
            None
        } else {
            let position_seconds =
                (player.transport.position_millis / 1_000).min(u64::from(u32::MAX)) as u32;
            let target = position_seconds as i32 + delta_seconds;
            Some(target.clamp(0, duration_seconds as i32) as u32)
        }
    }) else {
        return;
    };
    shell.products.playback.transport.seek_seconds(seconds);
}

fn adjust_volume(shell: &Rc<Shell>, delta: f64) {
    let Some(volume) = shell
        .player_ui
        .selected_playback()
        .as_deref()
        .map(|player| (player.controls.volume + delta).clamp(0.0, 1.0))
    else {
        return;
    };
    shell.player_ui.apply_user_volume(volume);
}

fn set_transition_mode_shortcut(shell: &Rc<Shell>, mode: PlaybackTransitionMode) {
    if !shell
        .products
        .playback
        .transport
        .playback_output()
        .is_local()
    {
        return;
    }
    shell
        .player_ui
        .update_playback_settings(|settings| settings.transition_mode = mode);
    shell
        .control_feedback
        .show_control_feedback_toast(match mode {
            PlaybackTransitionMode::Gapless => tr("Gapless"),
            PlaybackTransitionMode::Crossfade => tr("Crossfade"),
        });
}

fn refresh_selected_library(shell: &Shell) {
    if let Some(source) = shell.selected_source_operations() {
        source.refresh_library(rufin_core::runtime::LibraryRefreshTrigger::GlobalAction);
    }
}

fn toggle_shuffle_shortcut(shell: &Shell) {
    let Some(enabled) = shell
        .player_ui
        .selected_playback()
        .as_deref()
        .map(|player| !player.controls.shuffle_enabled)
    else {
        return;
    };
    shell.products.playback.transport.toggle_shuffle();
    let title = if enabled {
        tr("Shuffle on")
    } else {
        tr("Shuffle off")
    };
    shell.control_feedback.show_control_feedback_toast(title);
}

fn cycle_repeat_shortcut(shell: &Shell) {
    let Some(repeat_mode) = shell
        .player_ui
        .selected_playback()
        .as_deref()
        .map(|player| player.controls.repeat_mode)
    else {
        return;
    };
    let title = match repeat_mode {
        playback::RepeatMode::Off => tr("Repeat all"),
        playback::RepeatMode::All => tr("Repeat one"),
        playback::RepeatMode::One => tr("Repeat off"),
    };
    shell.products.playback.transport.cycle_repeat();
    shell.control_feedback.show_control_feedback_toast(title);
}

fn toggle_auto_dj_shortcut(shell: &Shell) {
    let Some(enabled) = shell
        .player_ui
        .selected_playback()
        .as_deref()
        .map(|player| !player.controls.auto_dj_enabled)
    else {
        return;
    };
    shell.products.playback.transport.toggle_auto_dj();
    let title = if enabled {
        tr("Auto DJ on")
    } else {
        tr("Auto DJ off")
    };
    shell.control_feedback.show_control_feedback_toast(title);
}

pub(crate) fn toggle_mute_shortcut(shell: &Rc<Shell>) {
    let Some(muted) = shell
        .player_ui
        .selected_playback()
        .as_deref()
        .map(|player| !player.controls.muted)
    else {
        return;
    };
    shell.player_ui.apply_user_muted(muted);
    let title = if muted { tr("Muted") } else { tr("Unmuted") };
    shell.control_feedback.show_control_feedback_toast(title);
}

fn show_shortcuts_dialog(shell: &Shell) {
    let resource = crate::ui_resource::SHORTCUTS_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    let dialog: adw::ShortcutsDialog =
        ui_shared::ui_resource::object(&builder, resource, "shortcuts_dialog");
    #[cfg(target_os = "macos")]
    let platform_accelerators = [
        ("shortcut_play_pause", "space"),
        ("shortcut_multi_selection", "<Meta>Pointer_Button1"),
        ("shortcut_select_all", "<Meta>a"),
        ("shortcut_selection_play_next", "<Meta>Return"),
        ("shortcut_selection_play_later", "<Meta><Shift>Return"),
        ("shortcut_add_to_playlist", "<Meta><Shift>p"),
        ("shortcut_download", "<Meta><Shift>d"),
        ("shortcut_previous_output", "<Alt><Meta>Up"),
        ("shortcut_next_output", "<Alt><Meta>Down"),
        ("shortcut_back", "Back <Meta>bracketleft"),
        ("shortcut_forward", "Forward <Meta>bracketright"),
        ("shortcut_sidebar_position", "<Meta>1...9 <Meta>0"),
        ("shortcut_source_position", "<Meta><Alt>1...9 <Meta><Alt>0"),
    ];
    #[cfg(not(target_os = "macos"))]
    let platform_accelerators = [
        ("shortcut_play_pause", "space <Control>space"),
        ("shortcut_multi_selection", "<Control>Pointer_Button1"),
        ("shortcut_select_all", "<Control>a"),
        ("shortcut_selection_play_next", "<Control>Return"),
        ("shortcut_selection_play_later", "<Control><Shift>Return"),
        ("shortcut_add_to_playlist", "<Control><Shift>p"),
        ("shortcut_download", "<Control><Shift>d"),
        ("shortcut_previous_output", "<Control><Shift>Up"),
        ("shortcut_next_output", "<Control><Shift>Down"),
        ("shortcut_back", "Back <Alt>Left"),
        ("shortcut_forward", "Forward <Alt>Right"),
        ("shortcut_sidebar_position", "<Control>1...9 <Control>0"),
        (
            "shortcut_source_position",
            "<Control><Alt>1...9 <Control><Alt>0",
        ),
    ];
    for (id, accelerator) in platform_accelerators {
        let item: adw::ShortcutsItem = ui_shared::ui_resource::object(&builder, resource, id);
        item.set_accelerator(accelerator);
    }

    dialog.connect_map(|dialog| {
        let dialog = dialog.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(dialog) = dialog.upgrade() {
                replace_pointer_shortcut_labels(dialog.upcast_ref());
            }
        });
    });
    drop(builder);
    present_light_dismiss_dialog(&dialog, &shell.chrome.window);
}

fn replace_pointer_shortcut_labels(container: &gtk::Widget) {
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Ok(label) = widget.clone().downcast::<adw::ShortcutLabel>() {
            if label.accelerator().contains("Pointer_Button1") {
                replace_pointer_keycaps(label.upcast_ref());
            }
        }
        replace_pointer_shortcut_labels(&widget);
    }
}

fn replace_pointer_keycaps(container: &gtk::Widget) {
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Ok(label) = widget.clone().downcast::<gtk::Label>()
            && label.has_css_class("keycap")
            && label.text().contains("Pointer")
            && label.text().contains("Button1")
        {
            label.set_text("🖱");
        }
        replace_pointer_keycaps(&widget);
    }
}

fn show_about_dialog(shell: &Shell) {
    let resource = crate::ui_resource::ABOUT_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    let dialog: adw::AboutDialog = ui_shared::ui_resource::object(&builder, resource, "dialog");
    dialog.set_application_name(DISPLAY_NAME);
    dialog.set_application_icon(APP_ID);
    dialog.set_translator_credits(TRANSLATOR_CREDITS);
    dialog.set_version(env!("CARGO_PKG_VERSION"));
    present_light_dismiss_dialog(&dialog, &shell.chrome.window);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use adw::prelude::*;
    use gtk::{gio, glib};

    use super::macos_menu_model;

    #[test]
    fn macos_menu_exposes_shortcut_commands() {
        let menu = macos_menu_model();
        let mut actions = BTreeSet::new();
        collect_actions(menu.upcast_ref(), &mut actions);

        for expected in [
            "text.undo",
            "text.redo",
            "clipboard.cut",
            "clipboard.copy",
            "clipboard.paste",
            "selection.delete",
            "selection.select-all",
            "win.go-back",
            "win.go-forward",
            "win.focus-search",
            "win.navigate-search",
            "win.cycle-layout",
            "win.cycle-tabs",
            "win.show-primary-menu",
            "win.navigate-sidebar",
            "win.toggle-left-sidebar",
            "win.toggle-queue",
            "win.toggle-lyrics",
            "win.toggle-private-mode",
            "win.toggle-fullscreen",
            "win.play-pause",
            "win.previous-track",
            "win.next-track",
            "win.seek-backward",
            "win.seek-forward",
            "win.toggle-shuffle",
            "win.cycle-repeat",
            "win.toggle-favorite",
            "win.toggle-auto-dj",
            "win.clear-queue",
            "win.mute",
            "win.volume-up",
            "win.volume-down",
            "win.troubleshooting",
            "win.show-release-notes",
            "window.close",
            "app.show-shortcuts",
        ] {
            assert!(
                actions.contains(expected),
                "missing macOS menu action {expected}"
            );
        }
    }

    #[test]
    fn macos_menu_leaves_application_and_window_ownership_to_gtk() {
        let menu = macos_menu_model();
        let mut actions = BTreeSet::new();
        collect_actions(menu.upcast_ref(), &mut actions);

        assert!(!actions.contains("app.about"));
        assert!(!actions.contains("app.preferences"));
        assert!(!actions.contains("app.quit"));
        assert_eq!(
            special_attribute(menu.upcast_ref(), "gtk-macos-special"),
            Some("window-submenu".to_string())
        );
    }

    fn collect_actions(model: &gio::MenuModel, actions: &mut BTreeSet<String>) {
        for index in 0..model.n_items() {
            if let Some(action) = model
                .item_attribute_value(index, "action", Some(glib::VariantTy::STRING))
                .and_then(|value| value.str().map(str::to_string))
            {
                actions.insert(action);
            }
            for link in ["section", "submenu"] {
                if let Some(child) = model.item_link(index, link) {
                    collect_actions(&child, actions);
                }
            }
        }
    }

    fn special_attribute(model: &gio::MenuModel, attribute: &str) -> Option<String> {
        (0..model.n_items()).find_map(|index| {
            model
                .item_attribute_value(index, attribute, Some(glib::VariantTy::STRING))
                .and_then(|value| value.str().map(str::to_string))
        })
    }
}
