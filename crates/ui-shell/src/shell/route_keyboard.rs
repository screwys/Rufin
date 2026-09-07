use super::Shell;
use super::actions::toggle_mute_shortcut;
use crate::shell::playlist_picker::{
    present_playlist_picker_entries, present_playlist_picker_media_uris,
    present_playlist_picker_selection,
};
use adw::prelude::*;
use gtk::glib;
use playback::QueuePlacement;
use std::rc::Rc;
use ui_player::outputs::{select_next_audio_output, select_previous_audio_output};
use ui_shared::media_menus::{
    download_playlist_entry_selection, download_track_selection, remove_playlist_entry_selection,
};
use ui_shared::selection::TrackSelectionSnapshot;
#[derive(Clone, Copy)]
enum SelectionShortcut {
    Play(QueuePlacement),
    AddToPlaylist,
    Download,
    Delete,
}
impl Shell {
    pub(crate) fn focus_current_route_search(&self) {
        if !self.route_keyboard_available() {
            return;
        }
        if let Some(search) = self.current_route_search() {
            search.focus();
        }
    }

    fn route_keyboard_available(&self) -> bool {
        !self.fullscreen_player_visible() && !self.transient_route_input_active()
    }

    fn playback_keyboard_available(&self) -> bool {
        !self.transient_route_input_active()
    }

    fn transient_route_input_active(&self) -> bool {
        self.preferences.active_dialog().is_some()
            || self.source.add_server.borrow().is_some()
            || self
                .player_ui
                .selected_lyrics()
                .is_some_and(|lyrics| lyrics.search_dialog.borrow().is_some())
    }

    pub(crate) fn connect_route_keyboard(self: &Rc<Self>) {
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let shell = Rc::clone(self);
        key.connect_key_pressed(move |_, key, _, state| {
            let current_focus = GtkWindowExt::focus(&shell.chrome.window);
            if shell.route_keyboard_available()
                && !focus_blocks_selection_shortcut(current_focus.as_ref())
                && let Some(shortcut) = selection_shortcut(key, state)
                && shell.run_selection_shortcut(shortcut, current_focus.as_ref())
            {
                return glib::Propagation::Stop;
            }
            if key_has_no_shortcut_modifiers(state) {
                if key == gtk::gdk::Key::space
                    && shell.playback_keyboard_available()
                    && !focus_blocks_playback_shortcut(current_focus.as_ref())
                {
                    shell.products.playback.transport.play_pause();
                    return glib::Propagation::Stop;
                }
                if key
                    .to_unicode()
                    .is_some_and(|character| character.eq_ignore_ascii_case(&'m'))
                    && shell.playback_keyboard_available()
                    && !focus_blocks_playback_shortcut(current_focus.as_ref())
                {
                    toggle_mute_shortcut(&shell);
                    return glib::Propagation::Stop;
                }
                if shell.route_keyboard_available()
                    && !focus_blocks_page_navigation(current_focus.as_ref())
                    && let Some(direction) = page_navigation_direction(key)
                {
                    return shell.navigate_current_route_items(direction);
                }
            }
            if key_has_audio_output_modifiers(state)
                && shell.playback_keyboard_available()
                && !focus_blocks_playback_shortcut(current_focus.as_ref())
            {
                match key {
                    gtk::gdk::Key::Up => {
                        select_previous_audio_output(&shell.player_ui);
                        return glib::Propagation::Stop;
                    }
                    gtk::gdk::Key::Down => {
                        select_next_audio_output(&shell.player_ui);
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            }
            let Some(search) = shell.current_route_search() else {
                return glib::Propagation::Proceed;
            };
            if !shell.settings.current.borrow().type_to_search_enabled
                || !shell.route_keyboard_available()
                || key_should_bypass_type_to_search(state)
                || focus_blocks_type_to_search(
                    GtkWindowExt::focus(&shell.chrome.window).as_ref(),
                    &search.search,
                )
            {
                return glib::Propagation::Proceed;
            }
            let Some(character) = key.to_unicode().filter(|character| !character.is_control())
            else {
                return glib::Propagation::Proceed;
            };
            if character.is_whitespace() && search.search.text().trim().is_empty() {
                return glib::Propagation::Proceed;
            }
            let mut position = search.search.position();
            if let Some((start, end)) = search.search.selection_bounds() {
                search.search.delete_text(start, end);
                position = start;
            }
            search
                .search
                .insert_text(&character.to_string(), &mut position);
            search.search.set_position(position);
            search.focus();
            glib::Propagation::Stop
        });
        self.chrome.window.add_controller(key);
    }

    fn run_selection_shortcut(
        self: &Rc<Self>,
        shortcut: SelectionShortcut,
        focus: Option<&gtk::Widget>,
    ) -> bool {
        if matches!(shortcut, SelectionShortcut::Delete) {
            return self.remove_focused_selection(focus);
        }
        if self.queue_selection_focused(focus) {
            let Some(queue) = self.player_ui.selected_queue() else {
                return false;
            };
            match shortcut {
                SelectionShortcut::Play(_) => {
                    let Some(occurrence) = queue
                        .selected_occurrences()
                        .and_then(|occurrences| occurrences.first().cloned())
                    else {
                        return false;
                    };
                    self.products.playback.queue.activate(occurrence);
                }
                SelectionShortcut::AddToPlaylist => {
                    let Some(media_uris) = queue.selected_media_uris() else {
                        return false;
                    };
                    present_playlist_picker_media_uris(self, media_uris.iter().cloned());
                }
                SelectionShortcut::Download => return false,
                SelectionShortcut::Delete => unreachable!(),
            }
            return true;
        }
        if !self.queue_selection_focused(focus)
            && let Some(selection) = self.current_playlist_entry_selection_snapshot()
        {
            match shortcut {
                SelectionShortcut::Play(placement) => {
                    selection.play(&self.products.playback.queue, placement)
                }
                SelectionShortcut::AddToPlaylist => {
                    present_playlist_picker_entries(self, selection)
                }
                SelectionShortcut::Download => {
                    return download_playlist_entry_selection(&self.media_menus, selection);
                }
                SelectionShortcut::Delete => unreachable!(),
            }
            return true;
        }
        let Some(selection) = self.focused_track_selection(focus) else {
            return false;
        };
        match shortcut {
            SelectionShortcut::Play(placement) => {
                selection.play(&self.products.playback.queue, placement)
            }
            SelectionShortcut::AddToPlaylist => present_playlist_picker_selection(self, selection),
            SelectionShortcut::Download => {
                return download_track_selection(&self.media_menus, selection);
            }
            SelectionShortcut::Delete => unreachable!(),
        }
        true
    }

    fn remove_focused_selection(self: &Rc<Self>, focus: Option<&gtk::Widget>) -> bool {
        if self.queue_selection_focused(focus) {
            let Some(occurrences) = self
                .player_ui
                .selected_queue()
                .and_then(|queue| queue.selected_occurrences())
            else {
                return false;
            };
            self.products
                .playback
                .queue
                .remove_many(occurrences.to_vec());
            return true;
        }
        self.current_playlist_entry_selection_snapshot()
            .is_some_and(|selection| remove_playlist_entry_selection(&self.media_menus, selection))
    }

    fn focused_track_selection(
        &self,
        _focus: Option<&gtk::Widget>,
    ) -> Option<TrackSelectionSnapshot> {
        self.current_route_track_selection_snapshot()
    }

    fn queue_selection_focused(&self, focus: Option<&gtk::Widget>) -> bool {
        focus.is_some_and(|focus| {
            focus.is_ancestor(&self.player_ui.right_panel.queue_panel)
                || focus.is_ancestor(&self.player_ui.views.fullscreen_player.queue_panel)
        })
    }
}
fn key_should_bypass_type_to_search(state: gtk::gdk::ModifierType) -> bool {
    state.intersects(
        gtk::gdk::ModifierType::ALT_MASK
            | gtk::gdk::ModifierType::CONTROL_MASK
            | gtk::gdk::ModifierType::SUPER_MASK
            | gtk::gdk::ModifierType::HYPER_MASK
            | gtk::gdk::ModifierType::META_MASK,
    )
}

fn key_has_no_shortcut_modifiers(state: gtk::gdk::ModifierType) -> bool {
    !state.intersects(
        gtk::gdk::ModifierType::SHIFT_MASK
            | gtk::gdk::ModifierType::ALT_MASK
            | gtk::gdk::ModifierType::CONTROL_MASK
            | gtk::gdk::ModifierType::SUPER_MASK
            | gtk::gdk::ModifierType::HYPER_MASK
            | gtk::gdk::ModifierType::META_MASK,
    )
}

fn key_has_audio_output_modifiers(state: gtk::gdk::ModifierType) -> bool {
    #[cfg(target_os = "macos")]
    let expected = gtk::gdk::ModifierType::ALT_MASK | gtk::gdk::ModifierType::META_MASK;
    #[cfg(not(target_os = "macos"))]
    let expected = gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK;
    let shortcut_modifiers = gtk::gdk::ModifierType::SHIFT_MASK
        | gtk::gdk::ModifierType::ALT_MASK
        | gtk::gdk::ModifierType::CONTROL_MASK
        | gtk::gdk::ModifierType::SUPER_MASK
        | gtk::gdk::ModifierType::HYPER_MASK
        | gtk::gdk::ModifierType::META_MASK;
    state & shortcut_modifiers == expected
}

fn selection_shortcut(
    key: gtk::gdk::Key,
    state: gtk::gdk::ModifierType,
) -> Option<SelectionShortcut> {
    #[cfg(target_os = "macos")]
    let primary = gtk::gdk::ModifierType::META_MASK;
    #[cfg(not(target_os = "macos"))]
    let primary = gtk::gdk::ModifierType::CONTROL_MASK;
    let shortcut_modifiers = gtk::gdk::ModifierType::SHIFT_MASK
        | gtk::gdk::ModifierType::ALT_MASK
        | gtk::gdk::ModifierType::CONTROL_MASK
        | gtk::gdk::ModifierType::SUPER_MASK
        | gtk::gdk::ModifierType::HYPER_MASK
        | gtk::gdk::ModifierType::META_MASK;
    let modifiers = state & shortcut_modifiers;
    let enter = matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter);
    if enter && modifiers.is_empty() {
        return Some(SelectionShortcut::Play(QueuePlacement::Now));
    }
    if enter && modifiers == primary {
        return Some(SelectionShortcut::Play(QueuePlacement::Next));
    }
    let primary_shift = primary | gtk::gdk::ModifierType::SHIFT_MASK;
    if enter && modifiers == primary_shift {
        return Some(SelectionShortcut::Play(QueuePlacement::Last));
    }
    let delete = matches!(key, gtk::gdk::Key::Delete | gtk::gdk::Key::KP_Delete);
    #[cfg(target_os = "macos")]
    let delete = delete || key == gtk::gdk::Key::BackSpace;
    if delete && modifiers.is_empty() {
        return Some(SelectionShortcut::Delete);
    }
    if modifiers != primary_shift {
        return None;
    }
    match key
        .to_unicode()
        .map(|character| character.to_ascii_lowercase())
    {
        Some('p') => Some(SelectionShortcut::AddToPlaylist),
        Some('d') => Some(SelectionShortcut::Download),
        _ => None,
    }
}

fn page_navigation_direction(key: gtk::gdk::Key) -> Option<gtk::DirectionType> {
    match key {
        gtk::gdk::Key::Up => Some(gtk::DirectionType::Up),
        gtk::gdk::Key::Down => Some(gtk::DirectionType::Down),
        gtk::gdk::Key::Left => Some(gtk::DirectionType::Left),
        gtk::gdk::Key::Right => Some(gtk::DirectionType::Right),
        _ => None,
    }
}

fn focus_blocks_playback_shortcut(focus: Option<&gtk::Widget>) -> bool {
    focus.is_some_and(|focus| focus_is_text_input(focus) || focus_is_in_dialog(focus))
}

fn focus_blocks_selection_shortcut(focus: Option<&gtk::Widget>) -> bool {
    focus.is_some_and(|focus| focus_is_text_input(focus) || focus_is_in_dialog(focus))
}

fn focus_blocks_page_navigation(focus: Option<&gtk::Widget>) -> bool {
    focus.is_some_and(|focus| {
        focus_is_in_dialog(focus)
            || focus_is_text_input(focus)
            || focus.is::<gtk::Range>()
            || focus.ancestor(gtk::Range::static_type()).is_some()
            || focus.is::<gtk::DropDown>()
            || focus.ancestor(gtk::DropDown::static_type()).is_some()
    })
}

fn focus_is_in_dialog(focus: &gtk::Widget) -> bool {
    focus.is::<adw::Dialog>() || focus.ancestor(adw::Dialog::static_type()).is_some()
}

fn focus_is_text_input(focus: &gtk::Widget) -> bool {
    focus.is::<gtk::Editable>()
        || focus.is::<gtk::TextView>()
        || focus.ancestor(gtk::Editable::static_type()).is_some()
        || focus.ancestor(gtk::TextView::static_type()).is_some()
}

fn focus_blocks_type_to_search(focus: Option<&gtk::Widget>, search: &gtk::SearchEntry) -> bool {
    let Some(focus) = focus else {
        return false;
    };
    focus.is_ancestor(search) || focus_is_text_input(focus) || focus_is_in_dialog(focus)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_shortcuts_require_exact_platform_modifiers_and_leave_text_inputs_alone() {
        #[cfg(target_os = "macos")]
        let primary = gtk::gdk::ModifierType::META_MASK;
        #[cfg(not(target_os = "macos"))]
        let primary = gtk::gdk::ModifierType::CONTROL_MASK;
        assert!(matches!(
            selection_shortcut(gtk::gdk::Key::Return, gtk::gdk::ModifierType::empty()),
            Some(SelectionShortcut::Play(playback::QueuePlacement::Now))
        ));
        assert!(matches!(
            selection_shortcut(gtk::gdk::Key::Return, primary),
            Some(SelectionShortcut::Play(playback::QueuePlacement::Next))
        ));
        assert!(matches!(
            selection_shortcut(
                gtk::gdk::Key::p,
                primary | gtk::gdk::ModifierType::SHIFT_MASK
            ),
            Some(SelectionShortcut::AddToPlaylist)
        ));
        assert!(matches!(
            selection_shortcut(gtk::gdk::Key::Delete, gtk::gdk::ModifierType::empty()),
            Some(SelectionShortcut::Delete)
        ));
        #[cfg(target_os = "macos")]
        assert!(matches!(
            selection_shortcut(gtk::gdk::Key::BackSpace, gtk::gdk::ModifierType::empty()),
            Some(SelectionShortcut::Delete)
        ));
        assert!(selection_shortcut(gtk::gdk::Key::Delete, primary).is_none());
        assert!(selection_shortcut(gtk::gdk::Key::p, primary).is_none());
    }
}
