use super::search::{
    LyricsSearchDialog, clear_list_box, lyrics_result_subtitle_markup, lyrics_result_title_markup,
    lyrics_search_response_matches_query, lyrics_search_result_can_save,
    lyrics_search_result_has_content, submit_lyrics_search,
};
use super::view::{LyricsPane, LyricsPaneContent};
use crate::player::state::{current_playback_media_id, current_playback_track_id};
use crate::preferences::dialogs::popup::present_light_dismiss_dialog;
use crate::shell::Shell;
use crate::shell::actions::icon_button;
use adw::prelude::*;
use gtk::glib;
use localization::result_count_text;
use localization::tr;
use lyrics::{CurrentLyrics, CurrentLyricsContent, LyricsSearchResult, release_japanese_reader};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use tracing::debug;

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

impl Shell {
    pub(crate) fn render_lyrics_panel(self: &Rc<Self>) {
        self.mark_lyrics_panes_dirty();
        self.render_lyrics_contents();
        self.update_lyrics_highlight();
    }

    pub(crate) fn render_lyrics_presentation(self: &Rc<Self>) {
        self.mark_lyrics_panes_dirty();
        self.render_lyrics_contents();
        self.refocus_current_lyrics_highlight();
    }

    pub(crate) fn sync_visible_lyrics_surfaces(self: &Rc<Self>) {
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
        let has_current_track = current_media.is_some();
        let (search_label, search_enabled) = if settings.private_mode {
            (tr("Private mode is on"), false)
        } else if has_current_track {
            (tr("Search lyrics"), true)
        } else {
            (tr("Nothing playing"), false)
        };
        let lyrics = self.visible_lyrics();
        let instrumental = self.visible_lyrics_are_instrumental();
        let pronunciation = self.visible_lyrics_pronunciation();
        let lyrics_origin = self.visible_lyrics_origin();
        let loading = self.current_lyrics_loading();
        let clear_auto_search_enabled = current_track_id.as_ref().is_some_and(|track_id| {
            settings
                .lyrics
                .can_suppress_auto_lyrics(settings.private_mode, track_id, lyrics_origin)
        });
        let lyrics_available = lyrics.is_some();
        let lyrics_editable = lyrics_available
            && current_media
                .as_ref()
                .is_some_and(|media_id| self.products.lyrics.current_writable(media_id));
        let show_furigana = settings.lyrics.show_furigana;
        let show_romanization = settings.lyrics.show_romanization;
        let word_by_word_highlighting = settings.lyrics.karaoke_mode;
        drop(settings);
        pane.set_save_action(&tr("Save Lyrics"), lyrics_available);
        pane.set_edit_action(lyrics_editable);
        pane.set_search_action(&search_label, search_enabled);
        pane.set_clear_auto_search_action(
            &tr("Clear fetched lyrics for this track"),
            clear_auto_search_enabled,
            !instrumental,
        );
        pane.set_offset_action(
            &tr("Lyrics offset (ms)"),
            &tr("Decrease"),
            &tr("Increase"),
            self.selected_lyrics()
                .map_or(0, |lyrics| lyrics.offset_millis.get()),
            lyrics_available,
        );
        let empty_status = self.lyrics_empty_status();
        let seek_shell = Rc::downgrade(self);
        let seek: Rc<dyn Fn(u64)> = Rc::new(move |position_millis| {
            if let Some(shell) = seek_shell.upgrade() {
                shell.seek_to_lyrics_position(position_millis);
            }
        });
        let content = if let Some(lyrics) = lyrics.as_deref() {
            LyricsPaneContent::Document {
                lyrics,
                pronunciation: pronunciation.as_deref(),
            }
        } else if instrumental {
            LyricsPaneContent::Instrumental
        } else if loading {
            LyricsPaneContent::Loading
        } else {
            LyricsPaneContent::Empty(empty_status)
        };
        pane.set_content(
            content,
            show_furigana,
            show_romanization,
            word_by_word_highlighting,
            seek,
        );
    }

    pub(crate) fn save_current_lyrics(self: &Rc<Self>) {
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
            self.products
                .lyrics
                .save_current_to_source(media_id, offset_millis);
            return;
        }
        let shell = Rc::clone(self);
        gtk::glib::spawn_future_local(async move {
            let dialog = gtk::FileDialog::builder()
                .title(tr("Save Lyrics"))
                .initial_name(lyrics_save_filename(&current.track.title))
                .build();
            let Ok(file) = dialog.save_future(Some(&shell.chrome.window)).await else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            shell
                .products
                .lyrics
                .save_current(media_id, offset_millis, path);
        });
    }

    fn should_save_lyrics_to_source(&self, media_id: &playback::CurrentMediaId) -> bool {
        self.settings.current.borrow().lyrics.save_lyrics_to_source
            && self.products.lyrics.current_writable(media_id)
    }

    pub(crate) fn present_lyrics_edit_dialog(self: &Rc<Self>) {
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

        let dialog = adw::Dialog::builder()
            .title(tr("Edit Lyrics"))
            .content_width(560)
            .content_height(600)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        body.set_margin_top(16);
        body.set_margin_bottom(16);
        body.set_margin_start(16);
        body.set_margin_end(16);
        let editor = gtk::TextView::new();
        editor.set_monospace(true);
        editor.set_wrap_mode(gtk::WrapMode::WordChar);
        let buffer = editor.buffer();
        buffer.create_tag(Some("line-time"), &[("foreground", &"#e8962c")]);
        buffer.create_tag(Some("word-time"), &[("foreground", &"#8ab4f8")]);
        buffer.set_text(&content);
        highlight_lrc_timestamps(&buffer);

        let offset = Rc::new(Cell::new(offset_millis));
        let offset_controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        offset_controls.set_halign(gtk::Align::Center);
        let offset_decrease_100 = gtk::Button::with_label("-100 ms");
        let offset_decrease_50 = gtk::Button::with_label("-50 ms");
        let offset_entry = gtk::Entry::new();
        offset_entry.set_text(&format!("{offset_millis} ms"));
        gtk::prelude::EditableExt::set_alignment(&offset_entry, 0.5);
        offset_entry.set_width_chars(6);
        offset_entry.set_max_width_chars(9);
        offset_entry.set_max_length(24);
        offset_entry.set_tooltip_text(Some(&tr("Lyrics offset (ms)")));
        let offset_increase_50 = gtk::Button::with_label("+50 ms");
        let offset_increase_100 = gtk::Button::with_label("+100 ms");
        offset_controls.append(&offset_decrease_100);
        offset_controls.append(&offset_decrease_50);
        offset_controls.append(&offset_entry);
        offset_controls.append(&offset_increase_50);
        offset_controls.append(&offset_increase_100);
        body.append(&offset_controls);

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

        let scroller = gtk::ScrolledWindow::builder()
            .child(&editor)
            .vexpand(true)
            .build();
        body.append(&scroller);

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label(&tr("Cancel"));
        let save = gtk::Button::with_label(&tr("Save"));
        save.add_css_class("suggested-action");
        actions.append(&cancel);
        actions.append(&save);
        body.append(&actions);
        toolbar.set_content(Some(&body));
        dialog.set_child(Some(&toolbar));

        let committed = Rc::new(Cell::new(false));
        let close = dialog.clone();
        cancel.connect_clicked(move |_| {
            close.close();
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
        let save_for_text = save.clone();
        buffer.connect_changed(move |buffer| {
            highlight_lrc_timestamps(buffer);
            let (start, end) = buffer.bounds();
            save_for_text.set_sensitive(!buffer.text(&start, &end, false).trim().is_empty());
        });
        let shell = Rc::clone(self);
        let close = dialog.clone();
        save.connect_clicked(move |_| {
            commit_offset();
            let (start, end) = buffer.bounds();
            let text = buffer.text(&start, &end, false).to_string();
            committed.set(true);
            shell
                .products
                .lyrics
                .update_lyrics_text(media_id.clone(), text);
            close.close();
        });
        present_light_dismiss_dialog(&dialog, &self.chrome.window);
    }
    pub(crate) fn present_lyrics_search_dialog(self: &Rc<Self>) {
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        if let Some(dialog) = lyrics.search_dialog.borrow().as_ref() {
            present_light_dismiss_dialog(&dialog.dialog, &self.chrome.window);
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

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);
        content.set_width_request(420);
        content.set_height_request(500);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.set_valign(gtk::Align::Center);
        let title = gtk::Label::new(Some(&tr("Search Lyrics")));
        title.add_css_class("title");
        title.set_xalign(0.0);
        title.set_hexpand(true);
        header.append(&title);
        let close_button = icon_button("rufin-window-close-symbolic", "Close");
        header.append(&close_button);
        content.append(&header);

        let helper = gtk::Label::new(Some(&tr(
            "Tap to the entry to load the lyrics and save in the app cache, click save only if you also want to save it in your music folder",
        )));
        helper.add_css_class("lyrics-search-helper");
        helper.set_xalign(0.0);
        helper.set_wrap(true);
        content.append(&helper);

        let artist_entry = gtk::Entry::new();
        artist_entry.set_placeholder_text(Some(&tr("Artist")));
        artist_entry.set_text(&current.track.artist);
        artist_entry.set_hexpand(true);
        content.append(&artist_entry);

        let title_entry = gtk::Entry::new();
        title_entry.set_placeholder_text(Some(&tr("Song")));
        title_entry.set_text(&current.track.title);
        title_entry.set_hexpand(true);
        content.append(&title_entry);

        let status = gtk::Label::new(Some(&tr("Ready")));
        status.add_css_class("muted");
        status.set_xalign(0.0);
        status.set_wrap(true);
        content.append(&status);

        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&list));
        content.append(&scroller);

        let dialog = adw::Dialog::builder()
            .content_width(520)
            .content_height(560)
            .child(&content)
            .build();
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

        present_light_dismiss_dialog(&dialog, &self.chrome.window);
        search_dialog.title_entry.grab_focus();
        self.schedule_lyrics_search();
    }
    pub(crate) fn apply_lyrics_search_results(
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
                        .products
                        .lyrics
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
                    shell
                        .products
                        .lyrics
                        .save_result_to_source(media_id, result);
                    return;
                }
                gtk::glib::spawn_future_local(async move {
                    let dialog = gtk::FileDialog::builder()
                        .title(tr("Save Lyrics"))
                        .initial_name(lyrics_save_filename(&result.track_name))
                        .build();
                    let Ok(file) = dialog.save_future(Some(&shell.chrome.window)).await else {
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
                    shell.products.lyrics.save_result(media_id, result, path);
                });
            });
            dialog.list.append(&row);
        }
    }
    pub(crate) fn apply_lyrics_search_failed(
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
    pub(crate) fn apply_lyrics_saved(&self, media_id: playback::CurrentMediaId, path: PathBuf) {
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
    fn hidden_lyrics_panes_keep_pending_content_without_rebuilding_on_every_open() {
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
