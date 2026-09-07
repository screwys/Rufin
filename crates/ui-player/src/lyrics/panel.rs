use super::search::{
    clear_list_box, lyrics_result_subtitle_markup, lyrics_result_title_markup,
    lyrics_search_response_matches_query, lyrics_search_result_can_save,
    lyrics_search_result_has_content, submit_lyrics_search,
};
use super::{lyrics_popup_content_height, lyrics_popup_content_width};
use crate::PlayerUi;
use crate::lyrics::search::LyricsSearchDialog;
use crate::lyrics::view::LyricsPane;
use crate::state::{current_playback_media_id, current_playback_track_id};
use adw::prelude::*;
use gtk::glib;
use localization::{result_count_text, tr};
use lyrics::{LyricsSearchResult, release_japanese_reader};
use rufin_core::lyrics::{CurrentLyrics, CurrentLyricsContent};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use tracing::debug;
use ui_shared::popup::present_light_dismiss_dialog;

const LYRICS_SEARCH_DEBOUNCE_MILLIS: u64 = 600;

fn take_dirty_lyrics_pane(dirty: &std::cell::Cell<bool>, visible: bool) -> bool {
    visible && dirty.replace(false)
}

fn highlight_lrc_timestamps(buffer: &gtk::TextBuffer) {
    let Some(line_time) = buffer.tag_table().lookup("line-time") else {
        return;
    };
    let Some(word_time) = buffer.tag_table().lookup("word-time") else {
        return;
    };
    let (start, end) = buffer.bounds();
    buffer.remove_tag(&line_time, &start, &end);
    buffer.remove_tag(&word_time, &start, &end);
    let mut cursor = start;
    while !cursor.is_end() {
        let open = cursor.char();
        if !matches!(open, '[' | '<') {
            cursor.forward_char();
            continue;
        }
        let tag_start = cursor;
        cursor.forward_char();
        let close = if open == '[' { ']' } else { '>' };
        while !cursor.is_end() {
            let character = cursor.char();
            cursor.forward_char();
            if matches!(character, '\n') || character == close {
                break;
            }
        }
        buffer.apply_tag(
            if open == '[' { &line_time } else { &word_time },
            &tag_start,
            &cursor,
        );
    }
}

impl PlayerUi {
    pub fn apply_japanese_dictionary_status(
        self: &Rc<Self>,
        status: lyrics::JapaneseDictionaryStatus,
    ) {
        use lyrics::JapaneseDictionaryStatus;

        let previous = self.lyrics.dictionary_toast.borrow_mut().take();
        if let Some(toast) = previous {
            toast.dismiss();
        }
        let id = match status {
            JapaneseDictionaryStatus::Loading => "loading",
            JapaneseDictionaryStatus::Failed => "failed",
            JapaneseDictionaryStatus::Ready(_) => {
                self.render_lyrics_panel();
                return;
            }
            JapaneseDictionaryStatus::Idle => return,
        };
        let resource = crate::ui_resource::LYRICS_DICTIONARY_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        let toast: adw::Toast = ui_shared::ui_resource::object(&builder, resource, id);
        let shell = Rc::downgrade(self);
        toast.connect_button_clicked(move |_| {
            if let Some(shell) = shell.upgrade() {
                shell.lyrics_service.japanese_dictionary(true);
            }
        });
        if let Some(overlay) = self.toast_overlay.upgrade() {
            overlay.add_toast(toast.clone());
        }
        *self.lyrics.dictionary_toast.borrow_mut() = Some(toast);
    }

    pub fn render_lyrics_panel(self: &Rc<Self>) {
        self.mark_lyrics_panes_dirty();
        self.render_lyrics_contents();
        self.update_lyrics_highlight();
    }

    pub fn render_lyrics_presentation(self: &Rc<Self>) {
        self.mark_lyrics_panes_dirty();
        self.render_lyrics_contents();
        self.refocus_current_lyrics_highlight();
        self.update_lyrics_highlight();
    }

    pub fn sync_visible_lyrics_surfaces(self: &Rc<Self>) {
        self.render_lyrics_contents();
        self.refocus_current_lyrics_highlight();
        self.update_lyrics_highlight();
    }

    fn mark_lyrics_panes_dirty(&self) {
        if let Some(lyrics) = self.selected_lyrics() {
            lyrics.right_pane_dirty.set(true);
            lyrics.fullscreen_pane_dirty.set(true);
        }
    }

    fn render_lyrics_contents(self: &Rc<Self>) {
        let local_readings_enabled = {
            let settings = self.settings.current.borrow();
            settings.lyrics.show_furigana || settings.lyrics.show_romanization
        };
        if !local_readings_enabled {
            release_japanese_reader();
        }
        self.request_auto_lyrics_if_needed();
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        if take_dirty_lyrics_pane(
            &lyrics.right_pane_dirty,
            self.right_lyrics_surface_visible(),
        ) {
            self.render_lyrics_pane(&lyrics.right_pane);
        }
        if take_dirty_lyrics_pane(
            &lyrics.fullscreen_pane_dirty,
            self.fullscreen_lyrics_surface_visible(),
        ) {
            self.render_lyrics_pane(&lyrics.fullscreen_pane);
        }
    }

    fn render_lyrics_pane(self: &Rc<Self>, pane: &LyricsPane) {
        let settings = self.settings.current.borrow();
        let current_media = current_playback_media_id(self.selected_playback().as_deref());
        let current_track_id = current_playback_track_id(self.selected_playback().as_deref());
        let dictionary_status = if (settings.lyrics.show_furigana
            || settings.lyrics.show_romanization && self.visible_lyrics_pronunciation().is_none())
            && self
                .visible_lyrics()
                .as_deref()
                .is_some_and(|lyrics| lyrics.is_japanese_for_readings())
        {
            self.lyrics_service.japanese_dictionary(false)
        } else {
            lyrics::JapaneseDictionaryStatus::Idle
        };
        if let lyrics::JapaneseDictionaryStatus::Ready(path) = &dictionary_status {
            lyrics::prepare_japanese_reader(path);
        }
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let projection = lyrics.projection.borrow().clone();
        let offset = lyrics.offset_millis.get();
        drop(lyrics);
        let writable = current_media
            .as_ref()
            .is_some_and(|id| self.lyrics_service.current_writable(id));
        let seek_shell = Rc::downgrade(self);
        pane.render_projection(
            &projection,
            current_media.as_ref(),
            current_track_id.as_deref(),
            &settings,
            writable,
            match dictionary_status {
                lyrics::JapaneseDictionaryStatus::Ready(path) => Some(path),
                _ => None,
            },
            offset,
            Rc::new(move |position| {
                if let Some(shell) = seek_shell.upgrade() {
                    shell.seek_to_lyrics_position(position);
                }
            }),
        );
    }

    pub fn save_current_lyrics(self: &Rc<Self>) {
        let Some(current) = self
            .selected_playback()
            .as_deref()
            .and_then(|player| player.transport.current.clone())
        else {
            return;
        };
        let Some(media_id) = current_playback_media_id(self.selected_playback().as_deref()) else {
            return;
        };
        if self.visible_lyrics().is_none() {
            return;
        }
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let offset_millis = lyrics.offset_millis.get();
        drop(lyrics);
        if self.should_save_lyrics_to_source(&media_id) {
            self.lyrics_service
                .save_current_to_source(media_id, offset_millis);
            return;
        }
        let shell = Rc::clone(self);
        gtk::glib::spawn_future_local(async move {
            let dialog = gtk::FileDialog::builder()
                .title(tr("Save Lyrics"))
                .initial_name(lyrics_save_filename(&current.title))
                .build();
            let Some(window) = shell.window.upgrade() else {
                return;
            };
            let Ok(file) = dialog.save_future(Some(&window)).await else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            shell
                .lyrics_service
                .save_current(media_id, offset_millis, path);
        });
    }

    fn should_save_lyrics_to_source(&self, media_id: &playback::CurrentMediaId) -> bool {
        self.settings.current.borrow().lyrics.save_lyrics_to_source
            && self.lyrics_service.current_writable(media_id)
    }

    pub fn present_lyrics_edit_dialog(self: &Rc<Self>) {
        let Some(media_id) = current_playback_media_id(self.selected_playback().as_deref()) else {
            return;
        };
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let projection = lyrics.projection.borrow();
        let CurrentLyrics::Ready {
            content: Some(CurrentLyricsContent::Document { document, .. }),
            ..
        } = &*projection
        else {
            return;
        };
        let offset_millis = lyrics.offset_millis.get();
        let content = lyrics::lyrics_to_lrc_text(document, offset_millis);
        drop(projection);
        drop(lyrics);

        let resource = crate::ui_resource::LYRICS_EDIT_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        ui_shared::objects!(builder, resource, {
            dialog: adw::Dialog,
            editor: gtk::TextView,
            offset_decrease_100: gtk::Button,
            offset_decrease_50: gtk::Button,
            offset_entry: gtk::Entry,
            offset_increase_50: gtk::Button,
            offset_increase_100: gtk::Button,
            cancel: gtk::Button,
            save: gtk::Button,
        });
        dialog.set_content_width(lyrics_popup_content_width());
        dialog.set_content_height(lyrics_popup_content_height(
            self.window.upgrade().map_or(0, |window| window.height()),
        ));
        let buffer = editor.buffer();
        buffer.create_tag(Some("line-time"), &[("foreground", &"#e8962c")]);
        buffer.create_tag(Some("word-time"), &[("foreground", &"#8ab4f8")]);
        buffer.set_text(&content);
        highlight_lrc_timestamps(&buffer);

        let offset = Rc::new(Cell::new(offset_millis));
        offset_entry.set_text(&format!("{offset_millis} ms"));

        let apply_offset: Rc<dyn Fn(i64)> = Rc::new({
            let offset = Rc::clone(&offset);
            let entry = offset_entry.downgrade();
            let buffer = buffer.clone();
            let shell = Rc::downgrade(self);
            move |value| {
                let previous = offset.replace(value);
                let delta = value.saturating_sub(previous);
                if delta != 0 {
                    let (start, end) = buffer.bounds();
                    let text = buffer.text(&start, &end, false);
                    buffer.set_text(&lyrics::shift_lrc_text_timestamps(&text, delta));
                    if let Some(shell) = shell.upgrade() {
                        shell.set_lyrics_offset_from_text(&value.to_string());
                    }
                }
                if let Some(entry) = entry.upgrade() {
                    entry.set_text(&format!("{value} ms"));
                }
            }
        });
        let commit_offset: Rc<dyn Fn()> = Rc::new({
            let offset = Rc::clone(&offset);
            let entry = offset_entry.downgrade();
            let apply_offset = Rc::clone(&apply_offset);
            move || {
                let Some(entry) = entry.upgrade() else {
                    return;
                };
                match super::state::parse_lyrics_offset_millis(&entry.text()) {
                    Some(value) => apply_offset(value),
                    None => entry.set_text(&format!("{} ms", offset.get())),
                }
            }
        });
        for (button, delta) in [
            (&offset_decrease_100, -100),
            (&offset_decrease_50, -50),
            (&offset_increase_50, 50),
            (&offset_increase_100, 100),
        ] {
            button.connect_clicked({
                let offset = Rc::clone(&offset);
                let apply_offset = Rc::clone(&apply_offset);
                let commit_offset = Rc::clone(&commit_offset);
                move |_| {
                    commit_offset();
                    apply_offset(offset.get().saturating_add(delta));
                }
            });
        }
        offset_entry.connect_activate({
            let commit_offset = Rc::clone(&commit_offset);
            move |_| commit_offset()
        });
        let offset_focus = gtk::EventControllerFocus::new();
        offset_focus.connect_leave({
            let commit_offset = Rc::clone(&commit_offset);
            move |_| commit_offset()
        });
        offset_entry.add_controller(offset_focus);

        let committed = Rc::new(Cell::new(false));
        let close = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(close) = close.upgrade() {
                close.close();
            }
        });
        let restore_shell = Rc::downgrade(self);
        let restore_committed = Rc::clone(&committed);
        let restore_media = media_id.clone();
        dialog.connect_closed(move |_| {
            if !restore_committed.get()
                && let Some(shell) = restore_shell.upgrade()
                && current_playback_media_id(shell.selected_playback().as_deref())
                    == Some(restore_media.clone())
            {
                shell.set_lyrics_offset_from_text(&offset_millis.to_string());
            }
        });
        let save_for_text = save.downgrade();
        buffer.connect_changed(move |buffer| {
            highlight_lrc_timestamps(buffer);
            let (start, end) = buffer.bounds();
            if let Some(save) = save_for_text.upgrade() {
                save.set_sensitive(!buffer.text(&start, &end, false).trim().is_empty());
            }
        });
        let shell = Rc::clone(self);
        let close = dialog.downgrade();
        let buffer = buffer.downgrade();
        save.connect_clicked(move |_| {
            let Some(buffer) = buffer.upgrade() else {
                return;
            };
            commit_offset();
            let (start, end) = buffer.bounds();
            let text = buffer.text(&start, &end, false).to_string();
            committed.set(true);
            shell
                .lyrics_service
                .update_lyrics_text(media_id.clone(), text);
            if let Some(close) = close.upgrade() {
                close.close();
            }
        });
        if let Some(window) = self.window.upgrade() {
            present_light_dismiss_dialog(&dialog, &window);
        }
    }
    pub fn present_lyrics_search_dialog(self: &Rc<Self>) {
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        if let Some(dialog) = lyrics.search_dialog.borrow().as_ref() {
            if let Some(window) = self.window.upgrade() {
                present_light_dismiss_dialog(&dialog.dialog, &window);
            }
            dialog.title_entry.grab_focus();
            return;
        }
        drop(lyrics);

        let Some(current) = self
            .selected_playback()
            .as_deref()
            .and_then(|player| player.transport.current.clone())
        else {
            return;
        };
        let Some(media_id) = current_playback_media_id(self.selected_playback().as_deref()) else {
            return;
        };
        if self.settings.current.borrow().private_mode {
            return;
        }

        let resource = crate::ui_resource::LYRICS_SEARCH_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        ui_shared::objects!(builder, resource, {
            dialog: adw::Dialog,
            close_button: gtk::Button,
            artist_entry: gtk::Entry,
            title_entry: gtk::Entry,
            status: gtk::Label,
            list: gtk::ListBox,
        });
        dialog.set_content_width(lyrics_popup_content_width());
        dialog.set_content_height(lyrics_popup_content_height(
            self.window.upgrade().map_or(0, |window| window.height()),
        ));
        artist_entry.set_text(&current.artist);
        title_entry.set_text(&current.title);
        for entry in [&artist_entry, &title_entry] {
            entry.set_icon_sensitive(gtk::EntryIconPosition::Secondary, !entry.text().is_empty());
            entry.connect_changed(|entry| {
                entry.set_icon_sensitive(
                    gtk::EntryIconPosition::Secondary,
                    !entry.text().is_empty(),
                );
            });
            entry.connect_icon_release(|entry, position| {
                if position == gtk::EntryIconPosition::Secondary {
                    entry.set_text("");
                }
            });
        }
        let search_dialog = LyricsSearchDialog {
            dialog: dialog.clone(),
            media_id,
            artist_entry: artist_entry.clone(),
            title_entry: title_entry.clone(),
            search_debounce_source: Rc::new(RefCell::new(None)),
            list,
            status,
        };
        if let Some(lyrics) = self.selected_lyrics() {
            *lyrics.search_dialog.borrow_mut() = Some(search_dialog.clone());
        }

        let close_shell = Rc::clone(self);
        let close_debounce_source = Rc::clone(&search_dialog.search_debounce_source);
        dialog.connect_closed(move |_| {
            if let Some(source) = close_debounce_source.borrow_mut().take() {
                source.remove();
            }
            if let Some(lyrics) = close_shell.selected_lyrics() {
                lyrics.search_dialog.borrow_mut().take();
            }
        });

        let close_dialog = dialog.downgrade();
        close_button.connect_clicked(move |_| {
            if let Some(dialog) = close_dialog.upgrade() {
                dialog.close();
            }
        });

        let search_shell = Rc::clone(self);
        artist_entry.connect_activate(move |_| submit_lyrics_search(&search_shell));
        let edit_shell = Rc::clone(self);
        artist_entry.connect_changed(move |_| edit_shell.schedule_lyrics_search());

        let search_shell = Rc::clone(self);
        title_entry.connect_activate(move |_| submit_lyrics_search(&search_shell));
        let edit_shell = Rc::clone(self);
        title_entry.connect_changed(move |_| edit_shell.schedule_lyrics_search());

        if let Some(window) = self.window.upgrade() {
            present_light_dismiss_dialog(&dialog, &window);
        }
        search_dialog.title_entry.grab_focus();
        self.schedule_lyrics_search();
    }
    pub fn apply_lyrics_search_results(
        self: &Rc<Self>,
        media_id: playback::CurrentMediaId,
        artist_name: String,
        track_name: String,
        results: Vec<LyricsSearchResult>,
    ) {
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let Some(dialog) = lyrics.search_dialog.borrow().clone() else {
            return;
        };
        drop(lyrics);
        if dialog.media_id != media_id {
            debug!(
                dialog_occurrence = %dialog.media_id.occurrence,
                response_occurrence = %media_id.occurrence,
                "ignored lyric search response for another track"
            );
            return;
        }
        if !lyrics_search_response_matches_query(
            &artist_name,
            &track_name,
            dialog.artist_entry.text().as_str(),
            dialog.title_entry.text().as_str(),
        ) {
            debug!(
                response_artist_name = %artist_name,
                response_track_name = %track_name,
                current_artist_name = %dialog.artist_entry.text(),
                current_track_name = %dialog.title_entry.text(),
                results = results.len(),
                "ignored stale lyric search response"
            );
            return;
        }
        debug!(
            artist_name = %artist_name,
            track_name = %track_name,
            results = results.len(),
            "applying lyric search response"
        );
        clear_list_box(&dialog.list);
        if results.is_empty() {
            dialog.status.set_text(&tr("No lyrics"));
            return;
        }

        dialog
            .status
            .set_text(&result_count_text(results.len() as u64));
        self.render_lyrics_search_result_rows(&dialog, &media_id, &results);
    }
    fn render_lyrics_search_result_rows(
        self: &Rc<Self>,
        dialog: &LyricsSearchDialog,
        media_id: &playback::CurrentMediaId,
        results: &[LyricsSearchResult],
    ) {
        let mut current_provider = None;
        for result in results {
            if current_provider != Some(result.provider) {
                current_provider = Some(result.provider);
                let header = adw::ActionRow::builder()
                    .title(result.provider.title())
                    .activatable(false)
                    .build();
                header.add_css_class("property");
                dialog.list.append(&header);
            }
            let title = lyrics_result_title_markup(result);
            let subtitle = lyrics_result_subtitle_markup(result);
            let has_content = lyrics_search_result_has_content(result);
            let can_save = lyrics_search_result_can_save(result);
            let row = adw::ActionRow::builder()
                .title(title.as_str())
                .subtitle(subtitle.as_str())
                .build();
            row.set_activatable(has_content);
            let button = gtk::Button::with_label(&tr("Save"));
            button.set_valign(gtk::Align::Center);
            button.add_css_class("suggested-action");
            button.set_sensitive(can_save);
            row.add_suffix(&button);

            if has_content {
                let preview_shell = Rc::clone(self);
                let preview_media = media_id.clone();
                let preview_result = result.clone();
                row.connect_activated(move |_| {
                    preview_shell
                        .lyrics_service
                        .preview(preview_media.clone(), preview_result.clone());
                    if let Some(lyrics) = preview_shell.selected_lyrics()
                        && let Some(dialog) = lyrics.search_dialog.borrow().as_ref()
                    {
                        dialog.status.set_text(&tr("Searching..."));
                    }
                });
            }
            let save_shell = Rc::clone(self);
            let save_media = media_id.clone();
            let save_result = result.clone();
            button.connect_clicked(move |_| {
                let shell = Rc::clone(&save_shell);
                let media_id = save_media.clone();
                let result = save_result.clone();
                if shell.should_save_lyrics_to_source(&media_id) {
                    shell.lyrics_service.save_result_to_source(media_id, result);
                    return;
                }
                gtk::glib::spawn_future_local(async move {
                    let dialog = gtk::FileDialog::builder()
                        .title(tr("Save Lyrics"))
                        .initial_name(lyrics_save_filename(&result.track_name))
                        .build();
                    let Some(window) = shell.window.upgrade() else {
                        return;
                    };
                    let Ok(file) = dialog.save_future(Some(&window)).await else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        return;
                    };
                    if let Some(lyrics) = shell.selected_lyrics()
                        && let Some(dialog) = lyrics.search_dialog.borrow().as_ref()
                    {
                        dialog.status.set_text(&tr("Searching..."));
                    }
                    shell.lyrics_service.save_result(media_id, result, path);
                });
            });
            dialog.list.append(&row);
        }
    }
    pub fn apply_lyrics_search_failed(
        self: &Rc<Self>,
        media_id: playback::CurrentMediaId,
        artist_name: String,
        track_name: String,
        error: String,
    ) {
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let Some(dialog) = lyrics.search_dialog.borrow().clone() else {
            return;
        };
        drop(lyrics);
        if dialog.media_id != media_id {
            debug!(
                dialog_occurrence = %dialog.media_id.occurrence,
                response_occurrence = %media_id.occurrence,
                "ignored lyric search failure for another track"
            );
            return;
        }
        if !lyrics_search_response_matches_query(
            &artist_name,
            &track_name,
            dialog.artist_entry.text().as_str(),
            dialog.title_entry.text().as_str(),
        ) {
            debug!(
                response_artist_name = %artist_name,
                response_track_name = %track_name,
                current_artist_name = %dialog.artist_entry.text(),
                current_track_name = %dialog.title_entry.text(),
                %error,
                "ignored stale lyric search failure"
            );
            return;
        }
        debug!(
            artist_name = %artist_name,
            track_name = %track_name,
            %error,
            "applying lyric search failure"
        );
        clear_list_box(&dialog.list);
        dialog.status.set_text(&tr("Couldn't search for lyrics"));
    }
    fn schedule_lyrics_search(self: &Rc<Self>) {
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let Some(dialog) = lyrics.search_dialog.borrow().clone() else {
            return;
        };
        drop(lyrics);
        if let Some(source) = dialog.search_debounce_source.borrow_mut().take() {
            source.remove();
        }
        if dialog.artist_entry.text().trim().is_empty()
            && dialog.title_entry.text().trim().is_empty()
        {
            self.clear_stale_lyrics_search_results();
            return;
        }
        let search_debounce_source = Rc::clone(&dialog.search_debounce_source);
        let search_shell = Rc::clone(self);
        let source = glib::timeout_add_local_once(
            Duration::from_millis(LYRICS_SEARCH_DEBOUNCE_MILLIS),
            move || {
                *search_debounce_source.borrow_mut() = None;
                submit_lyrics_search(&search_shell);
            },
        );
        *dialog.search_debounce_source.borrow_mut() = Some(source);
    }
    fn clear_stale_lyrics_search_results(self: &Rc<Self>) {
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let Some(dialog) = lyrics.search_dialog.borrow().clone() else {
            return;
        };
        clear_list_box(&dialog.list);
        dialog.status.set_text(&tr("Ready"));
    }
    pub fn apply_lyrics_saved(&self, media_id: playback::CurrentMediaId, path: PathBuf) {
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        if let Some(dialog) = lyrics.search_dialog.borrow().as_ref()
            && dialog.media_id == media_id
        {
            dialog
                .status
                .set_text(&format!("{} {}", tr("Saved to"), path.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::take_dirty_lyrics_pane;

    #[test]
    fn hidden_lyrics_pane_keeps_pending_content_without_rebuilding_on_every_open() {
        let dirty = Cell::new(true);

        assert!(!take_dirty_lyrics_pane(&dirty, false));
        assert!(dirty.get());
        assert!(take_dirty_lyrics_pane(&dirty, true));
        assert!(!dirty.get());
        assert!(!take_dirty_lyrics_pane(&dirty, true));
    }
}

fn lyrics_save_filename(track_title: &str) -> String {
    let stem = track_title
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            other if other.is_control() => '_',
            other => other,
        })
        .collect::<String>()
        .trim()
        .trim_end_matches('.')
        .to_string();
    let stem = if stem.is_empty() { "lyrics" } else { &stem };
    format!("{stem}.lrc")
}
