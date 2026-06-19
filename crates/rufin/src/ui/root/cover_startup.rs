use crate::i18n::tr_with;

use super::*;

const MOUSE_BACK_BUTTON: u32 = 8;
const MOUSE_FORWARD_BUTTON: u32 = 9;
const SLOW_EVENT_BATCH_MS: u64 = 100;
const SLOW_LIBRARY_SYNC_STATUS_MS: u64 = 100;
const SLOW_PLAYBACK_EVENT_POLL_MS: u64 = 100;
const TRANSLATOR_CREDITS: &str = include_str!(concat!(env!("OUT_DIR"), "/translator_credits.txt"));
const KEY_SEEK_SECONDS: i32 = 10;
const KEY_VOLUME_STEP: f64 = 0.05;
const CONTROL_TOAST_TIMEOUT: u32 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum FullscreenPlaybackRefresh {
    None,
    Visualizer,
    Static,
}

pub(in crate::ui) fn fullscreen_playback_refresh(
    previous: &PlaybackSnapshot,
    next: &PlaybackSnapshot,
) -> FullscreenPlaybackRefresh {
    if previous.current_server_id != next.current_server_id || previous.current != next.current {
        FullscreenPlaybackRefresh::Static
    } else if previous.state != next.state {
        FullscreenPlaybackRefresh::Visualizer
    } else {
        FullscreenPlaybackRefresh::None
    }
}

pub(in crate::ui) fn connect_shell_actions(shell: &Rc<Shell>, main_menu: gtk::MenuButton) {
    install_window_actions(shell);
    install_mouse_history_buttons(shell);
    install_main_menu_shortcut(shell, main_menu);
    connect_layout_resize(shell);
}

fn install_mouse_history_buttons(shell: &Rc<Shell>) {
    let click = gtk::GestureClick::new();
    click.set_button(0);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);

    let history_shell = Rc::clone(shell);
    click.connect_pressed(move |click, _, _, _| match click.current_button() {
        MOUSE_BACK_BUTTON => {
            click.set_state(gtk::EventSequenceState::Claimed);
            history_shell.go_back();
        }
        MOUSE_FORWARD_BUTTON => {
            click.set_state(gtk::EventSequenceState::Claimed);
            history_shell.go_forward();
        }
        _ => {}
    });

    shell.window.add_controller(click);
}

pub(in crate::ui) fn connect_lyrics_search_controls(shell: &Rc<Shell>) {
    let lyrics_shell = Rc::clone(shell);
    shell.lyrics_pane.connect_search_clicked(move || {
        if current_playback_track_id(&lyrics_shell.state.player.borrow()).is_none() {
            return;
        }
        lyrics_shell.present_lyrics_search_dialog();
    });
    let lyrics_shell = Rc::clone(shell);
    shell
        .lyrics_pane
        .connect_clear_auto_search_clicked(move || lyrics_shell.suppress_auto_lyrics_for_current());

    let fullscreen_lyrics_shell = Rc::clone(shell);
    shell
        .fullscreen_player
        .lyrics_pane
        .connect_search_clicked(move || {
            if current_playback_track_id(&fullscreen_lyrics_shell.state.player.borrow()).is_none() {
                return;
            }
            fullscreen_lyrics_shell.present_lyrics_search_dialog();
        });
    let fullscreen_lyrics_shell = Rc::clone(shell);
    shell
        .fullscreen_player
        .lyrics_pane
        .connect_clear_auto_search_clicked(move || {
            fullscreen_lyrics_shell.suppress_auto_lyrics_for_current()
        });
}
pub(in crate::ui) fn submit_lyrics_search(shell: &Rc<Shell>) {
    let Some(dialog) = shell.state.lyrics_search_dialog.borrow().clone() else {
        return;
    };
    if let Some(source) = dialog.search_debounce_source.borrow_mut().take() {
        source.remove();
    }
    if current_playback_track_id(&shell.state.player.borrow()).as_ref() != Some(&dialog.track_id) {
        dialog.dialog.close();
        return;
    }
    let artist_name = dialog.artist_entry.text().trim().to_string();
    let track_name = dialog.title_entry.text().trim().to_string();
    if artist_name.is_empty() && track_name.is_empty() {
        dialog.status.set_text(&tr("Enter an artist or song."));
        return;
    }
    clear_list_box(&dialog.list);
    dialog.status.set_text(&tr("Searching…"));
    debug!(
        artist_name = %artist_name,
        track_name = %track_name,
        "submitted manual lyric search"
    );
    shell
        .controller
        .search_lyrics_for_current(artist_name, track_name);
}
pub(in crate::ui) fn auto_lyrics_search_is_suppressed(
    settings: &AppSettings,
    track_id: &domain::TrackId,
) -> bool {
    settings
        .suppressed_auto_lyrics_track_ids
        .iter()
        .any(|stored| stored == track_id.as_str())
}
pub(in crate::ui) fn lyrics_search_response_matches_query(
    received_artist_name: &str,
    received_track_name: &str,
    current_artist_name: &str,
    current_track_name: &str,
) -> bool {
    lyrics_search_text_matches(received_artist_name, current_artist_name)
        && lyrics_search_text_matches(received_track_name, current_track_name)
}
pub(in crate::ui) fn lyrics_search_text_matches(received: &str, current: &str) -> bool {
    received.trim().to_lowercase() == current.trim().to_lowercase()
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum AutoLyricsRequest {
    Default,
    ServerOnly,
}
pub(in crate::ui) fn preferences_login_status_toast_message(status: &str) -> Option<String> {
    let status = status.trim();
    if let Some(provider) = status
        .strip_prefix("Checking ")
        .and_then(|status| status.strip_suffix(" server…"))
        .filter(|provider| !provider.trim().is_empty())
    {
        return Some(tr_with(
            "Checking {provider} server…",
            &[("provider", provider)],
        ));
    }
    match status {
        "Server settings saved." => Some(tr("Server settings saved.")),
        "Server settings saved. Resyncing library…" => {
            Some(tr("Server settings saved. Resyncing library…"))
        }
        "No changes to save." => Some(tr("No changes to save.")),
        "Sync already running." => Some(tr("Sync already running.")),
        _ => None,
    }
}

pub(in crate::ui) fn controller_error_is_user_visible(error: &str) -> bool {
    !error.contains("Element failed to change its state")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum LibrarySyncToastState {
    Progress,
    Complete,
    Clear,
}

pub(in crate::ui) fn library_sync_toast_state(status: &str) -> Option<LibrarySyncToastState> {
    let status = status.trim();
    if status == LIBRARY_SYNC_COMPLETE_STATUS {
        return Some(LibrarySyncToastState::Complete);
    }
    if status == "Cached library ready" || status.starts_with("Library cache ready for ") {
        return Some(LibrarySyncToastState::Clear);
    }
    if status.starts_with("Syncing ") && status.ends_with(" library…") {
        return Some(LibrarySyncToastState::Progress);
    }
    if status.starts_with("Caching library…")
        || status.starts_with("Caching local library…")
        || status.starts_with("Caching library artwork…")
    {
        return Some(LibrarySyncToastState::Progress);
    }
    None
}

pub(in crate::ui) fn library_sync_toast_message(status: &str) -> String {
    let status = status.trim();
    if let Some(provider) = status
        .strip_prefix("Syncing ")
        .and_then(|status| status.strip_suffix(" library…"))
        .filter(|provider| !provider.trim().is_empty())
    {
        return tr_with("Syncing {provider} library…", &[("provider", provider)]);
    }
    if status.starts_with("Caching library… This may take some time.") {
        return tr("Caching library… This may take some time.");
    }
    if status.starts_with("Caching local library… This may take some time.") {
        return tr("Caching local library… This may take some time.");
    }
    if status.starts_with("Caching library artwork…") {
        return tr("Caching library artwork…");
    }
    match status {
        "Cached library ready" => tr("Cached library ready"),
        "Library sync complete" => tr("Library sync complete"),
        _ => status.to_string(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum SnapshotRenderDecision {
    SourceChanged,
    FirstRunFinished,
    PreserveScroll,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum LocalSourceCacheGateAction {
    None,
    Enter,
    Wait,
    Reveal,
    Cancel,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) struct SnapshotEventOutcome {
    pub entered_first_run: bool,
    pub render: SnapshotRenderDecision,
}
#[derive(Clone, Copy, Debug)]
pub(in crate::ui) struct LocalSourceCacheGateInput<'a> {
    pub local_folders_changed: bool,
    pub next_source: &'a Option<domain::LibrarySourceSelection>,
    pub has_local_folders: bool,
    pub has_cached_library: bool,
    pub startup_route_revealed: bool,
    pub preparing: bool,
    pub sync_seen: bool,
    pub sync_status: &'a str,
}
pub(in crate::ui) fn snapshot_event_outcome(
    previous_first_run: bool,
    next_first_run: bool,
    previous_source: &Option<domain::LibrarySourceSelection>,
    next_source: &Option<domain::LibrarySourceSelection>,
    first_run_connection_pending: bool,
    first_run_connection_ready: bool,
) -> SnapshotEventOutcome {
    let first_run_finished =
        first_run_connection_pending && first_run_connection_ready && !next_first_run;
    let render = if first_run_finished {
        SnapshotRenderDecision::FirstRunFinished
    } else if previous_source != next_source {
        SnapshotRenderDecision::SourceChanged
    } else {
        SnapshotRenderDecision::PreserveScroll
    };

    SnapshotEventOutcome {
        entered_first_run: next_first_run && !previous_first_run,
        render,
    }
}
pub(in crate::ui) fn local_source_cache_gate_action(
    input: LocalSourceCacheGateInput<'_>,
) -> LocalSourceCacheGateAction {
    if !library_source_is_local(input.next_source) {
        return if input.preparing {
            LocalSourceCacheGateAction::Cancel
        } else {
            LocalSourceCacheGateAction::None
        };
    }

    let uncached_local_wait = !input.has_cached_library
        && (input.local_folders_changed || local_source_snapshot_is_syncing(input.sync_status));
    let startup_folder_wait = input.local_folders_changed && !input.startup_route_revealed;
    if !input.preparing && input.has_local_folders && (uncached_local_wait || startup_folder_wait) {
        return LocalSourceCacheGateAction::Enter;
    }

    if !input.preparing {
        return LocalSourceCacheGateAction::None;
    }

    if local_source_snapshot_is_syncing(input.sync_status) || !input.sync_seen {
        LocalSourceCacheGateAction::Wait
    } else {
        LocalSourceCacheGateAction::Reveal
    }
}
pub(in crate::ui) fn library_source_is_local(
    source: &Option<domain::LibrarySourceSelection>,
) -> bool {
    matches!(source, Some(domain::LibrarySourceSelection::Local))
}
pub(in crate::ui) fn local_source_snapshot_is_syncing(sync_status: &str) -> bool {
    sync_status == "Syncing library…"
}
pub(in crate::ui) fn queue_source_waits_for_snapshot(
    queue: Option<&QueueSnapshot>,
    active_server_id: Option<&domain::ServerId>,
) -> bool {
    queue.is_some_and(|queue| active_server_id != Some(&queue.server_id))
}
pub(in crate::ui) fn queue_source_matches_library(
    queue: Option<&QueueSnapshot>,
    library: &LibrarySnapshot,
) -> bool {
    let Some(queue) = queue else {
        return false;
    };
    library
        .server
        .as_ref()
        .is_some_and(|server| server.id == queue.server_id)
}
pub(in crate::ui) fn auto_lyrics_request_for_settings(
    settings: &AppSettings,
    track_id: &domain::TrackId,
    lyrics_surface_visible: bool,
) -> Option<AutoLyricsRequest> {
    if !lyrics_surface_visible {
        return None;
    }
    if !crate::external_activity::external_lyrics_lookup(settings)
        || auto_lyrics_search_is_suppressed(settings, track_id)
    {
        Some(AutoLyricsRequest::ServerOnly)
    } else {
        Some(AutoLyricsRequest::Default)
    }
}
pub(in crate::ui) fn auto_lyrics_skip_action_enabled(
    settings: &AppSettings,
    track_id: Option<&domain::TrackId>,
    lyrics: Option<&Lyrics>,
) -> bool {
    let Some((track_id, lyrics)) = track_id.zip(lyrics) else {
        return false;
    };
    if lyrics.source != LyricsSource::Remote {
        return false;
    }
    crate::external_activity::external_lyrics_lookup(settings)
        && !auto_lyrics_search_is_suppressed(settings, track_id)
}
pub(in crate::ui) fn clear_list_box(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}
pub(in crate::ui) fn lyrics_search_result_has_content(result: &LyricsSearchResult) -> bool {
    result.provider != ExternalLyricsProvider::Lrclib
        || result
            .synced_lyrics
            .as_deref()
            .is_some_and(|lyrics| !lyrics.trim().is_empty())
        || result
            .plain_lyrics
            .as_deref()
            .is_some_and(|lyrics| !lyrics.trim().is_empty())
}
pub(in crate::ui) fn lyrics_result_title(result: &LyricsSearchResult) -> String {
    format!("{} - {}", result.artist_name, result.track_name)
}
pub(in crate::ui) fn lyrics_result_title_markup(result: &LyricsSearchResult) -> glib::GString {
    glib::markup_escape_text(&lyrics_result_title(result))
}
pub(in crate::ui) fn lyrics_result_subtitle(result: &LyricsSearchResult) -> String {
    let mut subtitle = result.provider.title().to_string();
    if !result.album_name.trim().is_empty() {
        if !subtitle.is_empty() {
            subtitle.push_str(" - ");
        }
        subtitle.push_str(&result.album_name);
    }
    if result.duration_seconds > 0 {
        if !subtitle.is_empty() {
            subtitle.push_str(" - ");
        }
        subtitle.push_str(&format_duration(result.duration_seconds));
    }
    if !subtitle.is_empty() {
        subtitle.push_str(" - ");
    }
    if result
        .synced_lyrics
        .as_deref()
        .is_some_and(|lyrics| !lyrics.trim().is_empty())
    {
        subtitle.push_str(&tr("Synced lyrics"));
    } else if result
        .plain_lyrics
        .as_deref()
        .is_some_and(|lyrics| !lyrics.trim().is_empty())
    {
        subtitle.push_str(&tr("Plain lyrics"));
    } else if result.provider != ExternalLyricsProvider::Lrclib {
        subtitle.push_str(&tr("Remote lyrics"));
    } else {
        subtitle.push_str(&tr("No lyrics"));
    }
    subtitle
}
pub(in crate::ui) fn lyrics_result_subtitle_markup(result: &LyricsSearchResult) -> glib::GString {
    glib::markup_escape_text(&lyrics_result_subtitle(result))
}
pub(in crate::ui) fn initial_window_size(width: Option<i32>, height: Option<i32>) -> (i32, i32) {
    sanitized_window_size(width, height).unwrap_or((DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT))
}
pub(in crate::ui) fn install_window_state_persistence(shell: &Rc<Shell>) {
    let save_shell = Rc::clone(shell);
    shell.application.connect_shutdown(move |_| {
        save_shell.save_window_state();
    });
}
pub(in crate::ui) fn connect_layout_resize(shell: &Rc<Shell>) {
    let resize_shell = Rc::clone(shell);
    shell
        .window
        .connect_notify_local(Some("width"), move |_, _| {
            resize_shell.update_layout();
            resize_shell.queue_responsive_route_render();
        });
    let resize_shell = Rc::clone(shell);
    shell
        .window
        .connect_notify_local(Some("height"), move |_, _| {
            resize_shell.update_layout();
            resize_shell.queue_responsive_route_render();
        });

    let window = shell.window.clone();
    let resize_shell = Rc::clone(shell);
    window.connect_realize(move |window| {
        if let Some(surface) = window.surface() {
            let surface_resize_shell = Rc::clone(&resize_shell);
            surface.connect_width_notify(move |_| {
                surface_resize_shell.update_layout();
                surface_resize_shell.queue_responsive_route_render();
            });
            let surface_resize_shell = Rc::clone(&resize_shell);
            surface.connect_height_notify(move |_| {
                surface_resize_shell.update_layout();
                surface_resize_shell.queue_responsive_route_render();
            });
        }
        resize_shell.update_layout();
        resize_shell.queue_responsive_route_render();
    });

    let route_shell = Rc::clone(shell);
    shell
        .route_host
        .connect_notify_local(Some("width"), move |_, _| {
            route_shell.queue_responsive_route_render();
        });
}
pub(in crate::ui) fn install_window_actions(shell: &Rc<Shell>) {
    let go_back = gio::SimpleAction::new("go-back", None);
    let go_back_shell = Rc::clone(shell);
    go_back.connect_activate(move |_, _| go_back_shell.go_back());
    shell.window.add_action(&go_back);

    let go_forward = gio::SimpleAction::new("go-forward", None);
    let go_forward_shell = Rc::clone(shell);
    go_forward.connect_activate(move |_, _| go_forward_shell.go_forward());
    shell.window.add_action(&go_forward);

    let preferences = gio::SimpleAction::new("preferences", None);
    let preferences_shell = Rc::clone(shell);
    preferences.connect_activate(move |_, _| present_preferences_dialog(&preferences_shell));
    shell.window.add_action(&preferences);

    let shortcuts = gio::SimpleAction::new("show-shortcuts", None);
    let shortcuts_shell = Rc::clone(shell);
    shortcuts.connect_activate(move |_, _| show_shortcuts_dialog(&shortcuts_shell));
    shell.window.add_action(&shortcuts);

    let fullscreen = gio::SimpleAction::new("toggle-fullscreen", None);
    let fullscreen_shell = Rc::clone(shell);
    fullscreen.connect_activate(move |_, _| {
        if fullscreen_shell.window.is_fullscreen() {
            fullscreen_shell.window.unfullscreen();
        } else {
            fullscreen_shell.window.fullscreen();
        }
    });
    shell.window.add_action(&fullscreen);

    add_window_action(shell, "play-pause", &["<Control>space"], {
        let controller = shell.controller.clone();
        move || controller.play_pause()
    });
    add_window_action(shell, "previous-track", &["<Control>b"], {
        let controller = shell.controller.clone();
        move || controller.previous_track()
    });
    add_window_action(shell, "next-track", &["<Control>n"], {
        let controller = shell.controller.clone();
        move || controller.next_track()
    });
    add_window_action(shell, "seek-backward", &["<Control>Left"], {
        let shell = Rc::clone(shell);
        move || seek_by(&shell, -KEY_SEEK_SECONDS)
    });
    add_window_action(shell, "seek-forward", &["<Control>Right"], {
        let shell = Rc::clone(shell);
        move || seek_by(&shell, KEY_SEEK_SECONDS)
    });
    add_window_action(shell, "toggle-shuffle", &["<Control>s"], {
        let shell = Rc::clone(shell);
        move || toggle_shuffle_shortcut(&shell)
    });
    add_window_action(shell, "cycle-repeat", &["<Control>r"], {
        let shell = Rc::clone(shell);
        move || cycle_repeat_shortcut(&shell)
    });
    add_window_action(shell, "toggle-favorite", &["<Control>f"], {
        let shell = Rc::clone(shell);
        move || toggle_favorite_shortcut(&shell)
    });
    add_window_action(shell, "toggle-auto-dj", &["<Control>d"], {
        let shell = Rc::clone(shell);
        move || toggle_auto_dj_shortcut(&shell)
    });
    add_window_action(shell, "mute", &["<Control>m"], {
        let shell = Rc::clone(shell);
        move || toggle_mute_shortcut(&shell)
    });
    add_window_action(shell, "volume-up", &["<Control>plus", "<Control>equal"], {
        let shell = Rc::clone(shell);
        move || adjust_volume(&shell, KEY_VOLUME_STEP)
    });
    add_window_action(shell, "volume-down", &["<Control>minus"], {
        let shell = Rc::clone(shell);
        move || adjust_volume(&shell, -KEY_VOLUME_STEP)
    });
    add_window_action(shell, "toggle-queue", &["F9"], {
        let shell = Rc::clone(shell);
        move || shell.toggle_right_panel()
    });
    add_window_action(shell, "toggle-lyrics", &["<Control>l"], {
        let shell = Rc::clone(shell);
        move || shell.toggle_lyrics_panel()
    });
    let about = gio::SimpleAction::new("about", None);
    let about_shell = Rc::clone(shell);
    about.connect_activate(move |_, _| show_about_dialog(&about_shell));
    shell.window.add_action(&about);

    let release_notes = gio::SimpleAction::new("show-release-notes", None);
    let release_notes_shell = Rc::clone(shell);
    release_notes.connect_activate(move |_, _| release_notes_shell.present_release_notes());
    shell.window.add_action(&release_notes);

    shell
        .application
        .set_accels_for_action("win.go-back", &["<Alt>Left"]);
    shell
        .application
        .set_accels_for_action("win.go-forward", &["<Alt>Right"]);
    shell
        .application
        .set_accels_for_action("win.preferences", &["<Control>comma"]);
    shell
        .application
        .set_accels_for_action("win.show-shortcuts", &["<Control>question"]);
    shell
        .application
        .set_accels_for_action("win.toggle-fullscreen", &["F11"]);
}

fn add_window_action(
    shell: &Rc<Shell>,
    name: &str,
    accels: &[&str],
    activate: impl Fn() + 'static,
) {
    let action = gio::SimpleAction::new(name, None);
    action.connect_activate(move |_, _| activate());
    shell.window.add_action(&action);
    if !accels.is_empty() {
        shell
            .application
            .set_accels_for_action(&format!("win.{name}"), accels);
    }
}

fn seek_by(shell: &Shell, delta_seconds: i32) {
    let Some(seconds) = ({
        let player = shell.state.player.borrow();
        if player.current.is_none() || player.duration_seconds == 0 {
            None
        } else {
            let target = player.position_seconds as i32 + delta_seconds;
            Some(target.clamp(0, player.duration_seconds as i32) as u32)
        }
    }) else {
        return;
    };
    shell.controller.seek(seconds);
}

fn adjust_volume(shell: &Shell, delta: f64) {
    let volume = {
        let player = shell.state.player.borrow();
        (player.volume + delta).clamp(0.0, 1.0)
    };
    shell.controller.set_volume(volume);
}

fn toggle_shuffle_shortcut(shell: &Shell) {
    let enabled = !shell.state.player.borrow().shuffle_enabled;
    shell.controller.toggle_shuffle();
    let title = if enabled {
        tr("Shuffle on")
    } else {
        tr("Shuffle off")
    };
    shell.show_control_feedback_toast(title);
}

fn cycle_repeat_shortcut(shell: &Shell) {
    let title = match shell.state.player.borrow().repeat_mode {
        domain::RepeatMode::Off => tr("Repeat all"),
        domain::RepeatMode::All => tr("Repeat one"),
        domain::RepeatMode::One => tr("Repeat off"),
    };
    shell.controller.cycle_repeat();
    shell.show_control_feedback_toast(title);
}

fn toggle_auto_dj_shortcut(shell: &Shell) {
    let enabled = !shell.state.player.borrow().auto_dj_enabled;
    shell.controller.toggle_auto_dj();
    let title = if enabled {
        tr("Auto DJ on")
    } else {
        tr("Auto DJ off")
    };
    shell.show_control_feedback_toast(title);
}

fn toggle_mute_shortcut(shell: &Shell) {
    let muted = !shell.state.player.borrow().muted;
    shell.controller.toggle_mute();
    let title = if muted { tr("Muted") } else { tr("Unmuted") };
    shell.show_control_feedback_toast(title);
}

fn toggle_favorite_shortcut(shell: &Rc<Shell>) {
    let Some(entry) = shell.state.player.borrow().current.clone() else {
        return;
    };
    shell.set_favorite_with_feedback(
        source::FavoriteItemId::Track(entry.track_id),
        !entry.favorite,
        Some(&shell.player_controls.favorite_button),
    );
}
pub(in crate::ui) fn install_main_menu_shortcut(shell: &Rc<Shell>, main_menu: gtk::MenuButton) {
    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed(move |_, key, _, state| {
        if key == gtk::gdk::Key::F10 && !state.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
            main_menu.popup();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    shell.window.add_controller(key_controller);
}
pub(in crate::ui) fn show_shortcuts_dialog(shell: &Shell) {
    let dialog = adw::ShortcutsDialog::builder()
        .title(tr("Keyboard Shortcuts"))
        .build();
    let section = adw::ShortcutsSection::new(Some(&tr("General")));
    section.add(adw::ShortcutsItem::new(&tr("Back"), "Back <Alt>Left"));
    section.add(adw::ShortcutsItem::new(
        &tr("Forward"),
        "Forward <Alt>Right",
    ));
    section.add(adw::ShortcutsItem::new(&tr("Main Menu"), "F10"));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Preferences"),
        "win.preferences",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Keyboard Shortcuts"),
        "win.show-shortcuts",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Toggle Fullscreen"),
        "win.toggle-fullscreen",
    ));
    dialog.add(section);

    let section = adw::ShortcutsSection::new(Some(&tr("Playback")));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Play/Pause"),
        "win.play-pause",
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
        &tr("Shuffle"),
        "win.toggle-shuffle",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Repeat"),
        "win.cycle-repeat",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Favorite"),
        "win.toggle-favorite",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Auto DJ"),
        "win.toggle-auto-dj",
    ));
    section.add(adw::ShortcutsItem::from_action(&tr("Mute"), "win.mute"));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Volume Up"),
        "win.volume-up",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Volume Down"),
        "win.volume-down",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Show/hide right sidebar"),
        "win.toggle-queue",
    ));
    section.add(adw::ShortcutsItem::from_action(
        &tr("Show/hide lyrics"),
        "win.toggle-lyrics",
    ));
    dialog.add(section);
    dialog.present(Some(&shell.window));
}
pub(in crate::ui) fn show_about_dialog(shell: &Shell) {
    let dialog = adw::AboutDialog::builder()
        .application_name("Rufin")
        .application_icon("io.github.screwys.Rufin")
        .developer_name("screwys")
        .developers(["screwy https://github.com/screwys"])
        .translator_credits(TRANSLATOR_CREDITS)
        .version(env!("CARGO_PKG_VERSION"))
        .website("https://github.com/screwys/Rufin")
        .issue_url("https://github.com/screwys/Rufin/issues")
        .copyright("© 2026 screwy")
        .license_type(gtk::License::Custom)
        .license(
            "This application comes with absolutely no warranty and is licensed under GNU General Public Licence, version 3 or later.",
        )
        .release_notes_version(format!("v{}", env!("CARGO_PKG_VERSION")))
        .release_notes(about_release_notes())
        .comments(tr(
            "Thank you for trying out Rufin! If you have problems or suggestions, please open an issue in Github.",
        ))
        .build();
    dialog.present(Some(&shell.window));
}

pub(in crate::ui) fn schedule_startup_sync(shell: &Rc<Shell>) {
    let Some(delay_ms) = shell.controller.startup_sync_delay_ms() else {
        return;
    };

    let shell = Rc::clone(shell);
    glib::timeout_add_local_once(Duration::from_millis(delay_ms), move || {
        debug!(delay_ms, "starting deferred background sync");
        shell.controller.start_background_sync_for_active();
    });
}
pub(in crate::ui) fn apply_library_sync_status(
    library: &mut LibrarySnapshot,
    status: LibrarySyncStatus,
) -> bool {
    let Some(server_id) = library.server.as_ref().map(|server| server.id.clone()) else {
        return false;
    };
    if server_id != status.server_id {
        return false;
    }

    invalidate_sync_snapshot_pages(library, &status.delta);
    library.sync_status = status.sync_status;
    library.last_error = status.last_error;
    if login_status_marks_sync_complete(&library.sync_status) {
        library.first_run = false;
    }
    library.cached_album_count = status.counts.albums;
    library.cached_track_count = status.counts.tracks;
    library.cached_artist_count = status.counts.artists;
    library.cached_album_artist_count = status.counts.album_artists;
    library.cached_genre_count = status.counts.genres;
    library.cached_playlist_count = status.counts.playlists;
    if let Some(home) = status.home {
        library.home_sections = home.sections;
        library.prefetched_explore = home.prefetched_explore;
    }
    if let Some(source) = library
        .server_local_access
        .iter_mut()
        .find(|source| source.server_id == server_id)
    {
        source.sync_status = library.sync_status.clone();
        source.cached_album_count = library.cached_album_count;
        source.cached_track_count = library.cached_track_count;
    }
    true
}

fn invalidate_sync_snapshot_pages(library: &mut LibrarySnapshot, delta: &LibraryDelta) {
    if delta.is_empty() {
        return;
    }
    if delta.reset.is_some() {
        library.tracks.clear();
        library.albums.clear();
        library.artists.clear();
        library.album_artists.clear();
        library.genres.clear();
        library.playlists.clear();
        library.favorites.clear();
        library.search = source::SearchResults::default();
        return;
    }
    if !delta.tracks.is_empty() {
        library.tracks.clear();
        library.search = source::SearchResults::default();
    }
    if !delta.albums.is_empty() {
        library.albums.clear();
        library.search = source::SearchResults::default();
    }
    if !delta.artists.is_empty() {
        library.artists.clear();
        library.search = source::SearchResults::default();
    }
    if !delta.album_artists.is_empty() {
        library.album_artists.clear();
        library.search = source::SearchResults::default();
    }
    if !delta.genres.is_empty() {
        library.genres.clear();
        library.search = source::SearchResults::default();
    }
    if playlist_snapshot_changed(delta) {
        library.playlists.clear();
        library.search = source::SearchResults::default();
    }
}

fn playlist_snapshot_changed(delta: &LibraryDelta) -> bool {
    !delta.playlists.added.is_empty()
        || !delta.playlists.deleted.is_empty()
        || !delta.playlists.fields.is_empty()
        || !delta.playlists.entries.is_empty()
        || !delta.playlists.cover_refs.is_empty()
}

pub(in crate::ui) fn install_event_pump(shell: &Rc<Shell>, receiver: Receiver<ControllerEvent>) {
    let shell = Rc::clone(shell);
    glib::timeout_add_local(Duration::from_millis(33), move || {
        let batch_started = Instant::now();
        let playback_poll_started = Instant::now();
        shell.controller.poll_playback_events();
        let playback_poll_ms = playback_poll_started.elapsed().as_millis() as u64;
        if playback_poll_ms >= SLOW_PLAYBACK_EVENT_POLL_MS {
            warn!(playback_poll_ms, "slow playback event poll");
        }
        let mut event_count = 0_u64;
        while let Ok(event) = receiver.try_recv() {
            event_count += 1;
            match event {
                ControllerEvent::Snapshot(snapshot) => {
                    let (snapshot_outcome, local_folders_changed) = {
                        let current = shell.state.library.borrow();
                        (
                            snapshot_event_outcome(
                                current.first_run,
                                snapshot.first_run,
                                &current.selected_source,
                                &snapshot.selected_source,
                                shell.state.first_run_connection_pending.get(),
                                shell.state.first_run_connection_ready.get(),
                            ),
                            current.local_folders != snapshot.local_folders,
                        )
                    };
                    let local_gate_action = if matches!(
                        snapshot_outcome.render,
                        SnapshotRenderDecision::FirstRunFinished
                    ) {
                        LocalSourceCacheGateAction::None
                    } else {
                        local_source_cache_gate_action(LocalSourceCacheGateInput {
                            local_folders_changed,
                            next_source: &snapshot.selected_source,
                            has_local_folders: !snapshot.local_folders.is_empty(),
                            has_cached_library: snapshot
                                .cached_album_count
                                .saturating_add(snapshot.cached_track_count)
                                > 0,
                            startup_route_revealed: shell.state.startup_route_revealed.get(),
                            preparing: shell.state.local_source_preparing.get(),
                            sync_seen: shell.state.local_source_sync_seen.get(),
                            sync_status: &snapshot.sync_status,
                        })
                    };
                    let local_snapshot_syncing =
                        local_source_snapshot_is_syncing(&snapshot.sync_status);
                    let server_id = snapshot.server.as_ref().map(|server| server.id.clone());
                    let prefetched_explore = prefetched_explore_from_snapshot(&snapshot);
                    let sections = snapshot.home_sections.clone();
                    shell.replace_library_snapshot(*snapshot);
                    if snapshot_outcome.entered_first_run {
                        shell.state.server_discovery_started.set(false);
                        shell.state.server_discovery_running.set(false);
                        *shell.state.discovered_servers.borrow_mut() = Vec::new();
                        *shell.state.server_discovery_status.borrow_mut() =
                            ServerDiscoveryStatus::Idle;
                    }
                    shell.update_prefetched_explore_from_snapshot(
                        server_id,
                        prefetched_explore,
                        &sections,
                    );
                    refresh_context_playlist_picker(&shell);
                    *shell.state.folder_state.borrow_mut() = FolderRouteState::default();
                    shell.update_server_selector();
                    match local_gate_action {
                        LocalSourceCacheGateAction::Enter => {
                            shell.state.local_source_preparing.set(true);
                            shell.state.source_switch_preparing.set(false);
                            shell
                                .state
                                .local_source_sync_seen
                                .set(local_snapshot_syncing);
                            shell.state.startup_route_render_pending.set(false);
                            shell.state.startup_route_revealed.set(false);
                            shell.state.startup_route_content_prepared.set(false);
                            shell.prepare_home_route();
                            shell.render_startup_loading_view();
                            continue;
                        }
                        LocalSourceCacheGateAction::Wait => {
                            if local_snapshot_syncing {
                                shell.state.local_source_sync_seen.set(true);
                            }
                            shell.render_startup_loading_view();
                            continue;
                        }
                        LocalSourceCacheGateAction::Reveal => {
                            shell.state.local_source_preparing.set(false);
                            shell.state.local_source_sync_seen.set(false);
                            shell.state.source_switch_preparing.set(false);
                            shell.log_layout_snapshot("local_source_final_snapshot");
                            shell.schedule_startup_route_reveal();
                            continue;
                        }
                        LocalSourceCacheGateAction::Cancel => {
                            shell.state.local_source_preparing.set(false);
                            shell.state.local_source_sync_seen.set(false);
                            shell.state.source_switch_preparing.set(false);
                            shell.state.startup_route_render_pending.set(false);
                            shell.state.startup_route_revealed.set(true);
                            shell.state.startup_route_content_prepared.set(true);
                        }
                        LocalSourceCacheGateAction::None => {}
                    }
                    if shell.state.source_switch_preparing.get() {
                        let queue_matches_library = {
                            let queue = shell.state.queue.borrow();
                            let library = shell.state.library.borrow();
                            queue_source_matches_library(queue.as_ref(), &library)
                        };
                        if queue_matches_library {
                            shell.state.source_switch_preparing.set(false);
                            shell.prepare_home_route();
                            shell.render_queue_panel();
                            shell.render_lyrics_panel();
                            shell.update_bottom_player();
                            shell.update_fullscreen_player();
                            let player = shell.state.player.borrow().clone();
                            #[cfg(unix)]
                            shell.update_mpris_player();
                            shell.update_discord_presence(&player);
                            shell.schedule_startup_route_reveal();
                            continue;
                        }
                    }
                    match snapshot_outcome.render {
                        SnapshotRenderDecision::FirstRunFinished => {
                            shell.state.local_source_preparing.set(false);
                            shell.state.local_source_sync_seen.set(false);
                            shell.state.source_switch_preparing.set(false);
                            shell.log_layout_snapshot("first_run_final_snapshot");
                            shell.schedule_first_run_app_reveal();
                            continue;
                        }
                        SnapshotRenderDecision::SourceChanged => {
                            shell.reset_cover_pipeline();
                            shell.navigate(Route::Home);
                        }
                        SnapshotRenderDecision::PreserveScroll => {
                            shell.render_current_route_preserving_scroll();
                        }
                    }
                }
                ControllerEvent::LibrarySyncStatus(status) => {
                    let event_started = Instant::now();
                    let sync_status = status.sync_status.clone();
                    let delta_empty = status.delta.is_empty();
                    let last_error = status.last_error.clone();
                    let sync_toast_state = library_sync_toast_state(&status.sync_status);
                    let toast_message = sync_toast_state
                        .is_none()
                        .then(|| preferences_login_status_toast_message(&status.sync_status))
                        .flatten();
                    let delta = status.delta.clone();
                    let tracks_changed = delta.reset.is_some() || !delta.tracks.is_empty();
                    let apply_started = Instant::now();
                    let applied = {
                        let mut library = shell.state.library.borrow_mut();
                        apply_library_sync_status(&mut library, *status)
                    };
                    let apply_ms = apply_started.elapsed().as_millis() as u64;
                    if !applied {
                        continue;
                    }
                    if tracks_changed {
                        shell.rebuild_track_index();
                    }
                    let sync_complete = login_status_marks_sync_complete(&sync_status);
                    if sync_complete && shell.state.first_run_connection_pending.get() {
                        shell.state.first_run_connection_ready.set(true);
                    }
                    let selector_started = Instant::now();
                    shell.update_server_selector();
                    let selector_ms = selector_started.elapsed().as_millis() as u64;
                    if let Some(error) = last_error {
                        warn!(%error, "library sync update reported an error");
                        shell.dismiss_library_sync_toast();
                        shell.show_preferences_toast(&error);
                    } else if let Some(sync_toast_state) = sync_toast_state {
                        shell.update_library_sync_toast(sync_toast_state, &sync_status);
                    } else if let Some(message) = toast_message {
                        shell.show_preferences_toast(&message);
                    }
                    let mut delta_ms = 0_u64;
                    if shell.state.local_source_preparing.get() {
                        let syncing = {
                            let library = shell.state.library.borrow();
                            local_source_snapshot_is_syncing(&library.sync_status)
                        };
                        if syncing {
                            shell.render_startup_loading_view();
                        } else {
                            shell.state.local_source_preparing.set(false);
                            shell.state.local_source_sync_seen.set(false);
                            shell.state.source_switch_preparing.set(false);
                            shell.log_layout_snapshot("local_source_status_ready");
                            shell.schedule_startup_route_reveal();
                        }
                    } else {
                        let delta_started = Instant::now();
                        shell.apply_library_delta(delta);
                        delta_ms = delta_started.elapsed().as_millis() as u64;
                        info!(
                            sync_status = %sync_status,
                            delta_empty,
                            apply_ms,
                            selector_ms,
                            delta_ms,
                            total_ms = event_started.elapsed().as_millis() as u64,
                            "handled library sync status"
                        );
                    }
                    if sync_complete && shell.state.first_run_connection_pending.get() {
                        shell.schedule_first_run_app_reveal();
                    }
                    let total_ms = event_started.elapsed().as_millis() as u64;
                    if total_ms >= SLOW_LIBRARY_SYNC_STATUS_MS {
                        warn!(
                            sync_status = %sync_status,
                            delta_empty,
                            apply_ms,
                            selector_ms,
                            delta_ms,
                            total_ms,
                            "slow library sync status handling"
                        );
                    }
                }
                ControllerEvent::LibraryDelta(delta) => {
                    shell.apply_library_delta(*delta);
                }
                ControllerEvent::HomeSectionsUpdated {
                    snapshot,
                    include_explore,
                } => {
                    let previous_sections = shell.state.library.borrow().home_sections.clone();
                    let server_id = snapshot.server.as_ref().map(|server| server.id.clone());
                    let prefetched_explore = prefetched_explore_from_snapshot(&snapshot);
                    let snapshot = *snapshot;
                    let sections = snapshot.home_sections.clone();
                    shell.replace_library_snapshot(snapshot);
                    shell.update_prefetched_explore_from_snapshot(
                        server_id,
                        prefetched_explore,
                        &sections,
                    );
                    if !include_explore {
                        shell.promote_cached_prefetched_explore();
                    }
                    shell.update_server_selector();
                    if matches!(shell.state.routes.borrow().current(), Route::Home)
                        && !shell.state.startup_route_revealed.get()
                    {
                        shell.state.startup_route_content_prepared.set(false);
                        shell.prepare_startup_route_content();
                        return glib::ControlFlow::Continue;
                    }
                    shell.refresh_changed_visible_home_sections(
                        &previous_sections,
                        &sections,
                        include_explore,
                    );
                }
                ControllerEvent::HomeSectionPrefetched { server_id, section } => {
                    let active_server_id = shell
                        .state
                        .library
                        .borrow()
                        .server
                        .as_ref()
                        .map(|server| server.id.clone());
                    if active_server_id.as_ref() == Some(&server_id) {
                        let prefetched = PrefetchedHomeSection { server_id, section };
                        *shell.state.prefetched_explore.borrow_mut() = Some(prefetched);
                        if matches!(shell.state.routes.borrow().current(), Route::Home)
                            && !shell.state.startup_route_revealed.get()
                        {
                            shell.state.startup_route_content_prepared.set(false);
                            shell.prepare_startup_route_content();
                            return glib::ControlFlow::Continue;
                        }
                    }
                }
                ControllerEvent::PlaylistChanged {
                    playlist_id,
                    snapshot,
                } => {
                    shell.replace_library_snapshot(*snapshot);
                    shell.update_server_selector();
                    refresh_context_playlist_picker(&shell);
                    let route = shell.state.routes.borrow().current().clone();
                    let playlist_route_changed = matches!(route, Route::Playlists)
                        || matches!(route, Route::PlaylistDetail(id) if id == playlist_id);
                    if playlist_route_changed {
                        shell.render_current_route_preserving_scroll();
                    }
                }
                ControllerEvent::SmartPlaylistChanged {
                    smart_playlist_id,
                    snapshot,
                } => {
                    shell.replace_library_snapshot(*snapshot);
                    shell.state.smart_playlists.borrow_mut().clear();
                    shell.state.smart_playlists_loaded.set(false);
                    shell.update_server_selector();
                    let route = shell.state.routes.borrow().current().clone();
                    if matches!(route, Route::SmartPlaylists) {
                        shell.navigate(Route::SmartPlaylistDetail(smart_playlist_id));
                    } else if matches!(
                        route,
                        Route::SmartPlaylistDetail(id) if id == smart_playlist_id
                    ) {
                        shell.render_current_route_preserving_scroll();
                    }
                }
                ControllerEvent::FavoriteChanged {
                    item_id,
                    favorite,
                    snapshot,
                } => {
                    shell.apply_favorite_changed(item_id, favorite, *snapshot);
                }
                ControllerEvent::Queue(queue) => {
                    let next_queue = *queue;
                    let waits_for_source_snapshot = {
                        let library = shell.state.library.borrow();
                        queue_source_waits_for_snapshot(
                            next_queue.as_ref(),
                            library.server.as_ref().map(|server| &server.id),
                        )
                    };
                    *shell.state.queue.borrow_mut() = next_queue;
                    if waits_for_source_snapshot {
                        shell.state.source_switch_preparing.set(true);
                        shell.state.startup_route_render_pending.set(false);
                        shell.state.startup_route_revealed.set(false);
                        shell.state.startup_route_content_prepared.set(false);
                        shell.render_startup_loading_view();
                        continue;
                    }
                    shell.schedule_queue_panel_render();
                }
                ControllerEvent::Playback(player) => {
                    let previous_player = shell.state.player.borrow().clone();
                    let previous_track = previous_player
                        .current
                        .as_ref()
                        .map(|entry| entry.track_id.clone());
                    let mut next_snapshot = *player;
                    if let (Some(previous), Some(next)) = (
                        previous_player.current.as_ref(),
                        next_snapshot.current.as_mut(),
                    ) && previous.track_id == next.track_id
                    {
                        next.favorite = previous.favorite;
                    }
                    let next_track = next_snapshot
                        .current
                        .as_ref()
                        .map(|entry| entry.track_id.clone());
                    let lyrics_timing_changed = previous_track != next_track
                        || previous_player.state != next_snapshot.state
                        || previous_player.position_millis != next_snapshot.position_millis;
                    let fullscreen_refresh =
                        fullscreen_playback_refresh(&previous_player, &next_snapshot);
                    let auto_dj_enabled = next_snapshot.auto_dj_enabled;
                    *shell.state.player.borrow_mut() = next_snapshot.clone();
                    shell.maybe_clear_player_seek_preview(
                        &next_snapshot,
                        previous_track != next_track,
                    );
                    shell.update_bottom_player();
                    shell.sync_auto_dj(auto_dj_enabled);
                    if shell.state.source_switch_preparing.get() {
                        if previous_track != next_track {
                            *shell.state.lyrics.borrow_mut() = None;
                            *shell.state.lyrics_track_id.borrow_mut() = next_track.clone();
                            shell.lyrics_pane.clear_follow_scroll_pause();
                            shell
                                .fullscreen_player
                                .lyrics_pane
                                .clear_follow_scroll_pause();
                            shell.cancel_scheduled_lyrics_highlight();
                        }
                        continue;
                    }
                    if previous_track != next_track {
                        *shell.state.lyrics.borrow_mut() = None;
                        *shell.state.lyrics_track_id.borrow_mut() = next_track.clone();
                        shell.lyrics_pane.clear_follow_scroll_pause();
                        shell
                            .fullscreen_player
                            .lyrics_pane
                            .clear_follow_scroll_pause();
                        shell.cancel_scheduled_lyrics_highlight();
                        shell.render_lyrics_panel();
                        shell.notify_now_playing(&next_snapshot);
                    }
                    match fullscreen_refresh {
                        FullscreenPlaybackRefresh::Static => shell.update_fullscreen_player(),
                        FullscreenPlaybackRefresh::Visualizer => {
                            shell.sync_fullscreen_visualizer_state()
                        }
                        FullscreenPlaybackRefresh::None => {}
                    }
                    if lyrics_timing_changed {
                        shell.update_lyrics_highlight();
                    }
                    #[cfg(unix)]
                    shell.update_mpris_player();
                    shell.update_discord_presence(&next_snapshot);
                }
                ControllerEvent::Visualizer(levels) => {
                    shell.apply_fullscreen_visualizer_levels(levels);
                }
                ControllerEvent::Lyrics { track_id, lyrics } => {
                    shell.apply_loaded_lyrics_for_track(track_id, *lyrics);
                }
                ControllerEvent::LyricsSearchResults {
                    track_id,
                    artist_name,
                    track_name,
                    results,
                } => {
                    shell.apply_lyrics_search_results(track_id, artist_name, track_name, results);
                }
                ControllerEvent::LyricsSearchFailed {
                    track_id,
                    artist_name,
                    track_name,
                    error,
                } => {
                    shell.apply_lyrics_search_failed(track_id, artist_name, track_name, error);
                }
                ControllerEvent::SearchLoaded { key, results } => {
                    shell.apply_search_loaded(key, results);
                }
                ControllerEvent::SearchFailed { key, error } => {
                    shell.apply_search_failed(key, error);
                }
                ControllerEvent::LyricsSaved { path, lyrics } => {
                    shell.apply_lyrics_saved(path, lyrics);
                }
                ControllerEvent::FolderLoaded {
                    request_id,
                    path,
                    detail,
                } => {
                    shell.apply_folder_loaded(request_id, path, detail);
                }
                ControllerEvent::FolderLoadFailed {
                    request_id,
                    path,
                    error,
                } => {
                    shell.apply_folder_load_failed(request_id, path, error);
                }
                ControllerEvent::CoverReady { key, path } => {
                    let update_playback_art =
                        shell.current_playback_art_key_matches(&key, THUMB_COVER_SIZE);
                    shell.apply_cover_ready(&key, &path);
                    if update_playback_art {
                        let player = shell.state.player.borrow().clone();
                        shell.notify_now_playing(&player);
                    }
                    #[cfg(unix)]
                    if update_playback_art {
                        shell.update_mpris_player();
                    }
                }
                ControllerEvent::CoverUnavailable {
                    key,
                    external_retry_generation,
                } => {
                    if external_retry_generation
                        .is_some_and(|generation| !shell.controller.cover_retry_status(generation))
                    {
                        continue;
                    }
                    shell.apply_cover_unavailable(&key);
                }
                ControllerEvent::CoverDeferred { key } => {
                    shell.apply_cover_deferred(&key);
                }
                ControllerEvent::ServerDiscovery {
                    servers,
                    status,
                    running,
                } => {
                    *shell.state.discovered_servers.borrow_mut() = servers;
                    *shell.state.server_discovery_status.borrow_mut() = status;
                    shell.state.server_discovery_running.set(running);
                    if shell.state.library.borrow().first_run {
                        shell.render_current_route();
                    }
                    shell.refresh_add_server_dialog();
                }
                ControllerEvent::LoginStatus(status) => {
                    if let Some(sync_toast_state) = library_sync_toast_state(&status) {
                        shell.update_library_sync_toast(sync_toast_state, &status);
                    } else if let Some(message) = preferences_login_status_toast_message(&status) {
                        shell.show_preferences_toast(&message);
                    }
                    let sync_complete = login_status_marks_sync_complete(&status);
                    if sync_complete {
                        shell.state.first_run_connection_ready.set(true);
                        if shell.state.local_source_preparing.get() {
                            shell.state.local_source_sync_seen.set(true);
                        }
                    }
                    let first_run_connection_pending =
                        shell.state.first_run_connection_pending.get();
                    let display_status = if sync_complete && first_run_connection_pending {
                        tr(LIBRARY_PREPARING_STATUS)
                    } else {
                        status
                    };
                    let should_render = {
                        let mut library = shell.state.library.borrow_mut();
                        library.sync_status = display_status;
                        route_displays_sync_status(
                            shell.state.routes.borrow().current(),
                            library.first_run,
                        ) || shell.state.first_run_connection_pending.get()
                            || shell.state.local_source_preparing.get()
                    };
                    if should_render {
                        if shell.state.local_source_preparing.get() {
                            shell.render_startup_loading_view();
                        } else {
                            shell.render_current_route();
                        }
                    }
                }
                ControllerEvent::Error(error) => {
                    if !controller_error_is_user_visible(&error) {
                        debug!(%error, "suppressed controller error");
                        continue;
                    }
                    warn!(%error, "controller error");
                    shell.dismiss_library_sync_toast();
                    shell.show_preferences_toast(&error);
                    shell.state.first_run_connection_pending.set(false);
                    shell.state.first_run_connection_ready.set(false);
                    shell.state.local_source_preparing.set(false);
                    shell.state.local_source_sync_seen.set(false);
                    shell.state.source_switch_preparing.set(false);
                    shell.state.startup_route_render_pending.set(false);
                    shell.state.startup_route_revealed.set(true);
                    shell.state.startup_route_content_prepared.set(true);
                    let mut library = shell.state.library.borrow_mut();
                    library.sync_status = "Action failed".to_string();
                    library.last_error = Some(error);
                    drop(library);
                    shell.render_current_route();
                }
            }
        }
        let batch_ms = batch_started.elapsed().as_millis() as u64;
        if batch_ms >= SLOW_EVENT_BATCH_MS {
            warn!(
                event_count,
                playback_poll_ms, batch_ms, "slow controller event pump"
            );
        }
        glib::ControlFlow::Continue
    });
}

fn login_status_marks_sync_complete(status: &str) -> bool {
    let status = status.trim();
    status == LIBRARY_SYNC_COMPLETE_STATUS
        || status == "Cached library ready"
        || status.starts_with("Library cache ready for ")
}
impl Shell {
    pub(in crate::ui) fn show_control_feedback_toast(&self, title: String) {
        if !self.state.settings.borrow().control_notifications_enabled {
            return;
        }
        if let Some(toast) = self.state.control_feedback_toast.borrow_mut().take() {
            toast.dismiss();
        }
        let toast = adw::Toast::new(&title);
        toast.set_timeout(CONTROL_TOAST_TIMEOUT);
        self.quick_toast_overlay.add_toast(toast.clone());
        *self.state.control_feedback_toast.borrow_mut() = Some(toast);
    }

    pub(in crate::ui) fn show_preferences_toast(&self, message: &str) {
        self.toast_overlay.add_toast(adw::Toast::new(message));
    }

    pub(in crate::ui) fn update_library_sync_toast(
        self: &Rc<Self>,
        state: LibrarySyncToastState,
        message: &str,
    ) {
        match state {
            LibrarySyncToastState::Progress => self.show_or_update_library_sync_toast(message),
            LibrarySyncToastState::Complete | LibrarySyncToastState::Clear => {
                self.dismiss_library_sync_toast()
            }
        }
    }

    fn show_or_update_library_sync_toast(self: &Rc<Self>, status: &str) {
        if status.trim().starts_with("Syncing ") {
            self.state.library_sync_toast_suppressed.set(false);
        } else if self.state.library_sync_toast_suppressed.get() {
            return;
        }

        let message = library_sync_toast_message(status);
        if let Some(toast) = self.state.library_sync_toast.borrow().as_ref() {
            toast.set_title(&message);
            toast.set_timeout(0);
            return;
        }

        let toast = adw::Toast::new(&message);
        toast.set_timeout(0);
        let weak_shell = Rc::downgrade(self);
        let toast_for_signal = toast.clone();
        toast.connect_dismissed(move |_| {
            let Some(shell) = weak_shell.upgrade() else {
                return;
            };
            let mut active = shell.state.library_sync_toast.borrow_mut();
            if active
                .as_ref()
                .is_some_and(|toast| toast == &toast_for_signal)
            {
                active.take();
                shell.state.library_sync_toast_suppressed.set(true);
            }
        });
        self.toast_overlay.add_toast(toast.clone());
        *self.state.library_sync_toast.borrow_mut() = Some(toast);
    }

    pub(in crate::ui) fn dismiss_library_sync_toast(&self) {
        let toast = self.state.library_sync_toast.borrow_mut().take();
        if let Some(toast) = toast {
            toast.dismiss();
        }
    }
}

pub(in crate::ui) fn cover_prime_sizes(
    shell: &Shell,
    key: LibraryListKey,
    settings: &LibraryListSettings,
) -> Option<(u32, i32)> {
    match settings.layout {
        LibraryLayout::Grid => Some((
            GRID_COVER_SIZE,
            shell.collection_card_grid_metrics_for(key, settings).1,
        )),
        LibraryLayout::Detail => Some((GRID_COVER_SIZE, GRID_COVER_SIZE as i32)),
        LibraryLayout::Row if row_layout_uses_cover(settings) => Some((THUMB_COVER_SIZE, 48)),
        LibraryLayout::Row => None,
    }
}

pub(in crate::ui) fn visible_index_range(
    shell: &Shell,
    total: usize,
    key: LibraryListKey,
    settings: &LibraryListSettings,
) -> (usize, usize) {
    if total == 0 {
        return (0, 0);
    }
    let Some(scroller) = find_largest_scrolled_window(&shell.route_host.clone().upcast()) else {
        return (0, initial_visible_count(shell, key, settings).min(total));
    };
    let adjustment = scroller.vadjustment();
    let offset = adjustment.value().max(0.0);
    let page_size = effective_page_size(shell, &scroller, &adjustment);
    let (columns, card_size) = shell.collection_card_grid_metrics_for(key, settings);
    let grid_item_extent = library::collection_grid_item_extent(card_size, settings);
    visible_index_range_from_metrics(
        total,
        settings.layout,
        offset,
        page_size,
        library::LIBRARY_TABLE_ROW_HEIGHT.max(1),
        columns,
        grid_item_extent,
    )
}

fn effective_page_size(
    shell: &Shell,
    scroller: &gtk::ScrolledWindow,
    adjustment: &gtk::Adjustment,
) -> f64 {
    let fallback_height = scroller
        .height()
        .max(shell.route_host.height())
        .max(shell.app_root.height())
        .max(1);
    adjustment.page_size().max(f64::from(fallback_height))
}

pub(in crate::ui) fn visible_index_range_from_metrics(
    total: usize,
    layout: LibraryLayout,
    offset: f64,
    page_size: f64,
    row_height: i32,
    grid_columns: usize,
    grid_item_extent: i32,
) -> (usize, usize) {
    if total == 0 {
        return (0, 0);
    }
    match layout {
        LibraryLayout::Row => {
            let row_height = f64::from(row_height.max(1));
            let raw_start = (offset.max(0.0) / row_height).floor() as usize;
            let count = (page_size.max(1.0) / row_height).ceil().max(1.0) as usize;
            let count = count.min(total);
            let start = raw_start.min(total.saturating_sub(count));
            (start, start.saturating_add(count).min(total))
        }
        LibraryLayout::Grid | LibraryLayout::Detail => {
            let columns = grid_columns.max(1);
            let item_extent = f64::from(grid_item_extent.max(1));
            let first_row = (offset.max(0.0) / item_extent).floor() as usize;
            let rows = (page_size.max(1.0) / item_extent).ceil().max(1.0) as usize + 1;
            let count = rows.saturating_mul(columns).max(columns).min(total);
            let raw_start = first_row.saturating_mul(columns);
            let start = raw_start.min(total.saturating_sub(count));
            (start, start.saturating_add(count).min(total))
        }
    }
}

fn initial_visible_count(
    shell: &Shell,
    key: LibraryListKey,
    settings: &LibraryListSettings,
) -> usize {
    let (columns, card_size) = shell.collection_card_grid_metrics_for(key, settings);
    let grid_item_extent = library::collection_grid_item_extent(card_size, settings);
    initial_visible_count_from_metrics(
        settings.layout,
        shell.route_host.height(),
        shell.app_root.height(),
        columns,
        grid_item_extent,
    )
}

pub(in crate::ui) fn initial_visible_count_from_metrics(
    layout: LibraryLayout,
    route_height: i32,
    app_height: i32,
    grid_columns: usize,
    grid_item_extent: i32,
) -> usize {
    let viewport_height = route_height.max(app_height).max(1);
    match layout {
        LibraryLayout::Row => {
            let row_height = library::LIBRARY_TABLE_ROW_HEIGHT.max(1);
            (viewport_height / row_height).saturating_add(2).max(1) as usize
        }
        LibraryLayout::Grid | LibraryLayout::Detail => {
            let columns = grid_columns.max(1);
            let item_extent = grid_item_extent.max(1);
            let rows = (viewport_height / item_extent).saturating_add(2).max(1) as usize;
            rows.saturating_mul(columns)
        }
    }
}
