use std::{cell::Cell, rc::Rc, time::Duration};

use adw::prelude::*;
use gtk::{gio, glib};
use playback::{PlaybackTransitionMode, QueuePlacement};

use crate::localization::{bind_widget_accessible_label, bind_widget_tooltip};
use crate::player::{play_saved_random, select_next_audio_output, select_previous_audio_output};
use crate::preferences::source::selector::install_source_menu_actions;
use crate::preferences::{
    dialogs::popup::present_light_dismiss_dialog, present_preferences_dialog,
};
#[cfg(any(target_os = "macos", test))]
use localization::tr_with;
use localization::{TRANSLATOR_CREDITS, tr};

use super::{Shell, layout, navigation};

pub(crate) const PLAY_ICON: &str = "rufin-play-symbolic";
pub(crate) const PLAY_NEXT_ICON: &str = "rufin-play-next-symbolic";
pub(crate) const PLAY_LATER_ICON: &str = "rufin-play-later-symbolic";
pub(crate) const EDIT_ICON: &str = "rufin-edit-symbolic";
pub(crate) const ADD_ICON: &str = "list-add-bundled-symbolic";
pub(crate) const REMOVE_ICON: &str = "list-remove-bundled-symbolic";
pub(crate) const DELETE_ICON: &str = "process-stop-bundled-symbolic";
pub(crate) const TRASH_ICON: &str = "user-trash-bundled-symbolic";
pub(crate) const MORE_ICON: &str = "rufin-more-symbolic";
const SORT_ORDER_ICON: &str = "rufin-sort-name-symbolic";
const SORT_ORDER_DESCENDING_ICON: &str = "rufin-sort-name-descending-symbolic";

pub(crate) fn sort_order_icon(descending: bool) -> &'static str {
    if descending {
        SORT_ORDER_DESCENDING_ICON
    } else {
        SORT_ORDER_ICON
    }
}

const KEY_SEEK_SECONDS: i32 = 10;
const KEY_VOLUME_STEP: f64 = 0.05;
const CONTROL_TOAST_TIMEOUT: u32 = 2;

pub(crate) struct ControlFeedbackState {
    pub(crate) generation: Rc<Cell<u64>>,
}

pub(crate) fn connect_shell_actions(shell: &Rc<Shell>) {
    install_window_actions(shell);
    install_application_actions(shell);
    install_platform_menu(shell);
    navigation::install_mouse_history_buttons(shell);
    layout::connect_shell_layout(shell);
}

pub(crate) fn install_window_actions(shell: &Rc<Shell>) {
    install_source_menu_actions(shell);

    let go_back = gio::SimpleAction::new("go-back", None);
    let go_back_shell = Rc::clone(shell);
    go_back.connect_activate(move |_, _| go_back_shell.go_back());
    shell.chrome.window.add_action(&go_back);

    let go_forward = gio::SimpleAction::new("go-forward", None);
    let go_forward_shell = Rc::clone(shell);
    go_forward.connect_activate(move |_, _| go_forward_shell.go_forward());
    shell.chrome.window.add_action(&go_forward);

    let troubleshooting = gio::SimpleAction::new("troubleshooting", None);
    let troubleshooting_shell = Rc::clone(shell);
    troubleshooting.connect_activate(move |_, _| {
        super::diagnostics::present_diagnostics(&troubleshooting_shell);
    });
    shell.chrome.window.add_action(&troubleshooting);

    let toggle_left_sidebar = gio::SimpleAction::new("toggle-left-sidebar", None);
    let toggle_left_sidebar_shell = Rc::clone(shell);
    toggle_left_sidebar.connect_activate(move |_, _| {
        toggle_left_sidebar_shell.toggle_active_left_sidebar_size();
    });
    shell.chrome.window.add_action(&toggle_left_sidebar);

    let toggle_private_mode = gio::SimpleAction::new("toggle-private-mode", None);
    let private_mode_shell = Rc::clone(shell);
    toggle_private_mode.connect_activate(move |_, _| {
        let enabled = !private_mode_shell.settings.current.borrow().private_mode;
        private_mode_shell.set_private_mode(enabled);
        if private_mode_shell.settings.current.borrow().private_mode == enabled {
            private_mode_shell.show_control_feedback_toast(if enabled {
                tr("Private mode is on")
            } else {
                tr("Private mode is off")
            });
        }
    });
    shell.chrome.window.add_action(&toggle_private_mode);

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
        move || select_previous_audio_output(&shell)
    });
    add_window_action(shell, "next-audio-output", &[], {
        let shell = Rc::clone(shell);
        move || select_next_audio_output(&shell)
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
            move || shell.toggle_fullscreen_player()
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
    let release_notes = gio::SimpleAction::new("show-release-notes", None);
    let release_notes_shell = Rc::clone(shell);
    release_notes.connect_activate(move |_, _| release_notes_shell.present_release_notes());
    shell.chrome.window.add_action(&release_notes);

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
        let player = shell.selected_playback();
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
        .selected_playback()
        .as_deref()
        .map(|player| (player.controls.volume + delta).clamp(0.0, 1.0))
    else {
        return;
    };
    shell.apply_user_volume(volume);
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
    shell.update_playback_settings(|settings| settings.transition_mode = mode);
    shell.show_control_feedback_toast(match mode {
        PlaybackTransitionMode::Gapless => tr("Gapless"),
        PlaybackTransitionMode::Crossfade => tr("Crossfade"),
    });
}

fn refresh_selected_library(shell: &Shell) {
    let source_id = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.source_id.clone());
    if let Some(source_id) = source_id {
        shell.products.source.refresh_source(source_id);
    }
}

fn toggle_shuffle_shortcut(shell: &Shell) {
    let Some(enabled) = shell
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
    shell.show_control_feedback_toast(title);
}

fn cycle_repeat_shortcut(shell: &Shell) {
    let Some(repeat_mode) = shell
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
    shell.show_control_feedback_toast(title);
}

fn toggle_auto_dj_shortcut(shell: &Shell) {
    let Some(enabled) = shell
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
    shell.show_control_feedback_toast(title);
}

pub(crate) fn toggle_mute_shortcut(shell: &Rc<Shell>) {
    let Some(muted) = shell
        .selected_playback()
        .as_deref()
        .map(|player| !player.controls.muted)
    else {
        return;
    };
    shell.apply_user_muted(muted);
    let title = if muted { tr("Muted") } else { tr("Unmuted") };
    shell.show_control_feedback_toast(title);
}

fn show_shortcuts_dialog(shell: &Shell) {
    let dialog = adw::ShortcutsDialog::builder()
        .title(tr("Keyboard Shortcuts"))
        .build();

    let section = adw::ShortcutsSection::new(Some(&tr("General")));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Menu"),
        "win.show-primary-menu",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Search"),
        "win.focus-search",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Preferences"),
        "app.preferences",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Keyboard Shortcuts"),
        "app.show-shortcuts",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Resync Library"),
        "win.refresh-library",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Close app window"),
        "window.close",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Quit Rufin"),
        "app.quit",
    ));
    dialog.add(section);

    let section = adw::ShortcutsSection::new(Some(&tr("Playback")));
    #[cfg(target_os = "macos")]
    section.add(adw::ShortcutsItem::new(&tr("Play/Pause"), "space"));
    #[cfg(not(target_os = "macos"))]
    section.add(adw::ShortcutsItem::new(
        &tr("Play/Pause"),
        "space <Control>space",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Previous"),
        "win.previous-track",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Next"),
        "win.next-track",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Seek Backward"),
        "win.seek-backward",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Seek Forward"),
        "win.seek-forward",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Favorite"),
        "win.toggle-favorite",
    ));
    dialog.add(section);

    let section = adw::ShortcutsSection::new(Some(&tr("Queue")));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Play random"),
        "win.play-random",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Play random (play next)"),
        "win.play-random-next",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Play random (play later)"),
        "win.play-random-later",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Shuffle"),
        "win.toggle-shuffle",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Repeat"),
        "win.cycle-repeat",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Auto DJ"),
        "win.toggle-auto-dj",
    ));
    dialog.add(section);

    let section = adw::ShortcutsSection::new(Some(&tr("Audio")));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Gapless mode"),
        "win.use-gapless",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Crossfade mode"),
        "win.use-crossfade",
    ));
    section.add(adw::ShortcutsItem::new(&tr("Mute"), "m"));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Volume Up"),
        "win.volume-up",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Volume Down"),
        "win.volume-down",
    ));
    #[cfg(target_os = "macos")]
    {
        section.add(adw::ShortcutsItem::new(
            &tr("Previous audio device"),
            "<Alt><Meta>Up",
        ));
        section.add(adw::ShortcutsItem::new(
            &tr("Next audio device"),
            "<Alt><Meta>Down",
        ));
    }
    #[cfg(not(target_os = "macos"))]
    {
        section.add(adw::ShortcutsItem::new(
            &tr("Previous audio device"),
            "<Control><Shift>Up",
        ));
        section.add(adw::ShortcutsItem::new(
            &tr("Next audio device"),
            "<Control><Shift>Down",
        ));
    }
    dialog.add(section);

    let section = adw::ShortcutsSection::new(Some(&tr("View")));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Show/hide right sidebar"),
        "win.toggle-queue",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Show visualizer in the right panel"),
        "win.show-visualizer-panel",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Show lyrics in the right panel"),
        "win.show-lyrics-panel",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Show visualizer and lyrics in the right panel"),
        "win.show-visualizer-lyrics-panel",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Toggle Fullscreen"),
        "win.toggle-fullscreen",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Open fullscreen player"),
        "win.toggle-fullscreen-player",
    ));
    dialog.add(section);

    let section = adw::ShortcutsSection::new(Some(&tr("Navigation")));
    #[cfg(target_os = "macos")]
    {
        section.add(adw::ShortcutsItem::new(
            &tr("Back"),
            "Back <Meta>bracketleft",
        ));
        section.add(adw::ShortcutsItem::new(
            &tr("Forward"),
            "Forward <Meta>bracketright",
        ));
        section.add(adw::ShortcutsItem::new(
            &tr("Sidebar route by position"),
            "<Meta>1...9 <Meta>0",
        ));
    }
    #[cfg(not(target_os = "macos"))]
    {
        section.add(adw::ShortcutsItem::new(&tr("Back"), "Back <Alt>Left"));
        section.add(adw::ShortcutsItem::new(
            &tr("Forward"),
            "Forward <Alt>Right",
        ));
        section.add(adw::ShortcutsItem::new(
            &tr("Sidebar route by position"),
            "<Control>1...9 <Control>0",
        ));
    }
    section.add(adw::ShortcutsItem::new(
        &tr("Navigate page items"),
        "Up Down Left Right",
    ));
    dialog.add(section);

    present_light_dismiss_dialog(&dialog, &shell.chrome.window);
}

fn show_about_dialog(shell: &Shell) {
    let dialog = adw::AboutDialog::builder()
        .application_name("Rufin")
        .application_icon("io.github.screwys.Rufin")
        .developer_name("screwy")
        .developers(["screwy <screwygit@proton.me>"])
        .translator_credits(TRANSLATOR_CREDITS)
        .version(env!("CARGO_PKG_VERSION"))
        .website("https://github.com/screwys/Rufin")
        .issue_url("https://github.com/screwys/Rufin/issues")
        .copyright("© 2026 screwy")
        .license_type(gtk::License::Custom)
        .license(
            "This application comes with absolutely no warranty and is licensed under GNU General Public Licence, version 3 or later.",
        )
        .comments(tr(
            "Thank you for trying out Rufin! If you have problems or suggestions, please open an issue in Github.",
        ))
        .build();
    dialog.add_link(&tr("Support"), "https://github.com/sponsors/screwys");
    present_light_dismiss_dialog(&dialog, &shell.chrome.window);
}

pub(crate) fn set_active_class(widget: &impl IsA<gtk::Widget>, active: bool) {
    if active {
        widget.add_css_class("active-toggle");
    } else {
        widget.remove_css_class("active-toggle");
    }
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
            "win.show-primary-menu",
            "win.navigate-sidebar",
            "win.toggle-queue",
            "win.toggle-lyrics",
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
            "win.mute",
            "win.volume-up",
            "win.volume-down",
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

pub(crate) fn icon_button(icon_name: &str, label: &str) -> gtk::Button {
    let button = base_icon_button(icon_name);
    bind_widget_tooltip(&button, label);
    button
}

pub(crate) fn icon_button_without_tooltip(icon_name: &str, label: &str) -> gtk::Button {
    let button = base_icon_button(icon_name);
    bind_widget_accessible_label(&button, label);
    button
}

fn base_icon_button(icon_name: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon_name);
    button.add_css_class("icon-button");
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.set_valign(gtk::Align::Center);
    button
}

#[derive(Clone, Copy)]
pub(crate) enum ActionButtonVariant {
    CoverSideTransport,
    CoverPrimaryTransport,
    CoverCornerMenu,
    CoverCornerFavorite,
    DetailAction,
    DetailPrimary,
    DetailFavorite,
}

pub(crate) const COVER_SIDE_ACTION_SIZE: i32 = 34;
pub(crate) const COVER_PRIMARY_ACTION_SIZE: i32 = 54;

pub(crate) fn configure_action_button(
    button: &gtk::Button,
    variant: ActionButtonVariant,
    icon_name: Option<&str>,
) {
    let is_cover = matches!(
        variant,
        ActionButtonVariant::CoverSideTransport
            | ActionButtonVariant::CoverPrimaryTransport
            | ActionButtonVariant::CoverCornerMenu
            | ActionButtonVariant::CoverCornerFavorite
    );
    if is_cover {
        button.add_css_class("cover-hover-button");
        button.add_css_class("cover-hover-animated");
    } else {
        button.add_css_class("detail-showcase-action-button");
    }

    let nudge_icon = match variant {
        ActionButtonVariant::CoverSideTransport => {
            button.add_css_class("cover-side-button");
            pin_action_button(button, COVER_SIDE_ACTION_SIZE);
            true
        }
        ActionButtonVariant::CoverPrimaryTransport => {
            button.add_css_class("cover-play-button");
            pin_action_button(button, COVER_PRIMARY_ACTION_SIZE);
            true
        }
        ActionButtonVariant::CoverCornerMenu => {
            button.add_css_class("cover-menu-button");
            pin_action_button(button, COVER_SIDE_ACTION_SIZE);
            false
        }
        ActionButtonVariant::CoverCornerFavorite => {
            button.add_css_class("cover-favorite-button");
            pin_action_button(button, COVER_SIDE_ACTION_SIZE);
            false
        }
        ActionButtonVariant::DetailAction => true,
        ActionButtonVariant::DetailPrimary => {
            button.add_css_class("detail-showcase-play-button");
            true
        }
        ActionButtonVariant::DetailFavorite => false,
    };

    if let (true, Some(icon_name)) = (nudge_icon, icon_name) {
        nudge_transport_action_icon(button, icon_name);
    }
    let face_class = if is_cover {
        "cover-hover-face"
    } else {
        "detail-showcase-action-face"
    };
    wrap_button_child_in_action_layers(button, face_class);
}

fn pin_action_button(button: &gtk::Button, size: i32) {
    button.set_size_request(size, size);
    button.set_halign(gtk::Align::Center);
    button.set_valign(gtk::Align::Center);
}

fn nudge_transport_action_icon(button: &gtk::Button, icon_name: &str) {
    let start_margin = if icon_name == PLAY_ICON {
        4
    } else if icon_name == PLAY_NEXT_ICON || icon_name == PLAY_LATER_ICON {
        2
    } else {
        return;
    };
    let Some(child) = button.child() else {
        return;
    };
    if let Ok(image) = child.downcast::<gtk::Image>() {
        image.set_margin_start(start_margin);
    }
}

fn wrap_button_child_in_action_layers(button: &gtk::Button, face_class: &str) {
    let Some(child) = button.child() else {
        return;
    };
    button.set_child(None::<&gtk::Widget>);
    child.set_halign(gtk::Align::Center);
    child.set_valign(gtk::Align::Center);

    let shadow = gtk::CenterBox::new();
    shadow.add_css_class("action-button-shadow");
    shadow.set_can_target(false);

    let face = gtk::CenterBox::new();
    face.add_css_class(face_class);
    face.set_can_target(false);
    face.set_center_widget(Some(&child));
    shadow.set_center_widget(Some(&face));
    button.set_child(Some(&shadow));
}

pub(crate) fn text_button(icon_name: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("pill-button");
    button.add_css_class("pill");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name(icon_name));
    content.append(&gtk::Label::new(Some(&tr(label))));
    button.set_child(Some(&content));
    button
}

impl Shell {
    pub(crate) fn show_control_feedback_toast(&self, title: String) {
        if !self.settings.current.borrow().control_notifications_enabled {
            return;
        }
        self.show_feedback_toast(title);
    }

    pub(crate) fn show_feedback_toast(&self, title: String) {
        let generation = self.control_feedback.generation.get() + 1;
        self.control_feedback.generation.set(generation);
        self.chrome.control_feedback_label.set_text(&title);
        self.chrome.control_feedback_label.set_visible(true);
        let label = self.chrome.control_feedback_label.clone();
        let active_generation = Rc::clone(&self.control_feedback.generation);
        glib::timeout_add_local_once(
            Duration::from_secs(u64::from(CONTROL_TOAST_TIMEOUT)),
            move || {
                if active_generation.get() == generation {
                    label.set_visible(false);
                }
            },
        );
    }
}
