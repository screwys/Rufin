use crate::format_duration;
use adw::prelude::*;
use gtk::glib;
use localization::tr;
use lyrics::{LyricsSearchContent, LyricsSearchResult};
use std::{cell::Cell, rc::Rc};

pub struct LyricsEditor {
    pub content: gtk::Box,
    pub view: gtk::TextView,
    offset: Rc<Cell<i64>>,
    offset_entry: gtk::Entry,
    offset_controls: adw::WrapBox,
    commit_offset: Rc<dyn Fn()>,
}

impl LyricsEditor {
    pub fn new(text: &str, offset_millis: i64, on_offset: Rc<dyn Fn(i64)>) -> Self {
        let resource = crate::ui_resource::LYRICS_EDITOR_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            content: gtk::Box, editor: gtk::TextView, offset_entry: gtk::Entry,
            offset_controls: adw::WrapBox,
            offset_decrease_100: gtk::Button, offset_decrease_50: gtk::Button,
            offset_increase_50: gtk::Button, offset_increase_100: gtk::Button,
        });
        let buffer = editor.buffer();
        buffer.create_tag(Some("line-time"), &[("foreground", &"#e8962c")]);
        buffer.create_tag(Some("word-time"), &[("foreground", &"#8ab4f8")]);
        buffer.set_text(text);
        highlight_lrc_timestamps(&buffer);
        buffer.connect_changed(highlight_lrc_timestamps);
        let offset = Rc::new(Cell::new(offset_millis));
        offset_entry.set_text(&format!("{offset_millis} ms"));
        let apply_offset: Rc<dyn Fn(i64)> = Rc::new({
            let offset = offset.clone();
            let entry = offset_entry.downgrade();
            let buffer = buffer.clone();
            move |value| {
                let previous = offset.replace(value);
                let delta = value.saturating_sub(previous);
                if delta != 0 {
                    let (start, end) = buffer.bounds();
                    buffer.set_text(&lyrics::shift_lrc_text_timestamps(
                        &buffer.text(&start, &end, false),
                        delta,
                    ));
                    on_offset(value);
                }
                if let Some(entry) = entry.upgrade() {
                    entry.set_text(&format!("{value} ms"));
                }
            }
        });
        let commit_offset: Rc<dyn Fn()> = Rc::new({
            let offset = offset.clone();
            let entry = offset_entry.downgrade();
            let apply = apply_offset.clone();
            move || {
                if let Some(entry) = entry.upgrade() {
                    match parse_lyrics_offset_millis(&entry.text()) {
                        Some(value) => apply(value),
                        None => entry.set_text(&format!("{} ms", offset.get())),
                    }
                }
            }
        });
        for (button, delta) in [
            (&offset_decrease_100, -100),
            (&offset_decrease_50, -50),
            (&offset_increase_50, 50),
            (&offset_increase_100, 100),
        ] {
            let offset = offset.clone();
            let apply = apply_offset.clone();
            let commit = commit_offset.clone();
            crate::interactions::connect_repeating_button(button, move || {
                commit();
                apply(offset.get().saturating_add(delta));
            });
        }
        let commit = commit_offset.clone();
        offset_entry.connect_activate(move |_| commit());
        let focus = gtk::EventControllerFocus::new();
        let commit = commit_offset.clone();
        focus.connect_leave(move |_| commit());
        offset_entry.add_controller(focus);
        Self {
            content,
            view: editor,
            offset,
            offset_entry,
            offset_controls,
            commit_offset,
        }
    }

    pub fn set_content(&self, text: &str, offset: i64) {
        self.offset.set(offset);
        self.offset_entry.set_text(&format!("{offset} ms"));
        self.view.buffer().set_text(text);
    }

    pub fn text(&self) -> String {
        (self.commit_offset)();
        let buffer = self.view.buffer();
        let (start, end) = buffer.bounds();
        buffer.text(&start, &end, false).to_string()
    }

    pub fn set_editable(&self, editable: bool) {
        self.view.set_editable(editable);
        self.offset_controls.set_sensitive(editable);
    }
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

pub fn parse_lyrics_offset_millis(value: &str) -> Option<i64> {
    let value = value.trim();
    let number = ["ms", "MS", "Ms", "mS"]
        .into_iter()
        .find_map(|suffix| value.strip_suffix(suffix))
        .unwrap_or(value)
        .trim();
    number.parse().ok()
}

pub fn lyrics_search_result_has_content(result: &LyricsSearchResult) -> bool {
    result.content.can_preview()
}

pub fn lyrics_search_result_can_save(result: &LyricsSearchResult) -> bool {
    result.content.can_save()
}

fn lyrics_result_title(result: &LyricsSearchResult) -> String {
    format!("{} - {}", result.artist_name, result.track_name)
}

pub fn lyrics_result_title_markup(result: &LyricsSearchResult) -> glib::GString {
    glib::markup_escape_text(&lyrics_result_title(result))
}

pub fn lyrics_result_subtitle(result: &LyricsSearchResult) -> String {
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
    if matches!(result.content, LyricsSearchContent::Instrumental) {
        subtitle.push_str(&tr("Instrumental"));
    } else if result
        .content
        .synced_lyrics()
        .is_some_and(|lyrics| !lyrics.trim().is_empty())
    {
        subtitle.push_str(&tr("Synced lyrics"));
    } else if result
        .content
        .plain_lyrics()
        .is_some_and(|lyrics| !lyrics.trim().is_empty())
    {
        subtitle.push_str(&tr("Plain lyrics"));
    } else if matches!(result.content, LyricsSearchContent::Deferred) {
        subtitle.push_str(&tr("Remote lyrics"));
    } else {
        subtitle.push_str(&tr("No lyrics"));
    }
    subtitle
}

pub fn lyrics_result_subtitle_markup(result: &LyricsSearchResult) -> glib::GString {
    glib::markup_escape_text(&lyrics_result_subtitle(result))
}
