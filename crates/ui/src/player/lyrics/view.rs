use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use localization::{msgid, tr};
use lyrics::{
    JapaneseReadingSegment, LyricsAgentRole, LyricsCue, LyricsDocument, LyricsLine,
    japanese_reading_for_language_options,
};

use crate::shell::layout::WINDOW_CHROME_MARGIN_END;

use super::wrapping_line::WrappingLine;

const DEFAULT_LYRICS_SCROLL_ANIMATION_MS: u64 = 300;
const MIN_LYRICS_SCROLL_ANIMATION_MS: u64 = 80;
const LYRICS_SCROLL_MS: u64 = 200;
const LYRICS_USER_SCROLL_PAUSE_MS: u64 = 3_000;
const LYRICS_SCROLL_READY_RETRY_MS: u64 = 32;
const LYRICS_SCROLL_READY_RETRIES: u8 = 12;

#[derive(Clone)]
pub(crate) struct LyricsPane {
    root: gtk::Overlay,
    scroller: gtk::ScrolledWindow,
    body: gtk::Box,
    save_button: gtk::Button,
    clear_auto_search_button: gtk::Button,
    search_button: gtk::Button,
    settings_button: gtk::Button,
    offset_decrease_button: gtk::Button,
    offset_entry: gtk::Entry,
    offset_increase_button: gtk::Button,
    rows: Rc<RefCell<Vec<LyricsRow>>>,
    active_index: Rc<Cell<Option<usize>>>,
    scroll_generation: Rc<Cell<u64>>,
    follow_pause_until: Rc<Cell<Option<Instant>>>,
}

#[derive(Clone)]
struct LyricsRow {
    track: LyricsRowTrack,
    line_index: usize,
    row: gtk::Widget,
    cues: Vec<LyricsCueHighlight>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum LyricsRowTrack {
    Primary,
    Pronunciation,
}

#[derive(Clone)]
struct LyricsCueHighlight {
    cue: LyricsCue,
    widget: gtk::Widget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LyricsFollowScrollPause {
    Inactive,
    Active,
    Expired,
}

pub(crate) enum LyricsPaneContent<'a> {
    Document {
        lyrics: &'a LyricsDocument,
        pronunciation: Option<&'a LyricsDocument>,
    },
    Instrumental,
    Loading,
    Empty(String),
}

impl LyricsPane {
    pub fn new() -> Self {
        let root = gtk::Overlay::new();
        root.add_css_class("lyrics-panel");
        root.set_vexpand(true);
        root.set_margin_start(8);
        root.set_margin_end(0);

        let clear_auto_search_button = gtk::Button::from_icon_name("rufin-process-stop-symbolic");
        clear_auto_search_button.add_css_class("icon-button");
        clear_auto_search_button.add_css_class("flat");
        clear_auto_search_button.add_css_class("circular");

        let search_button = gtk::Button::from_icon_name("rufin-system-search-symbolic");
        search_button.add_css_class("icon-button");
        search_button.add_css_class("flat");
        search_button.add_css_class("circular");
        let settings_button = gtk::Button::from_icon_name("rufin-applications-system-symbolic");
        settings_button.add_css_class("icon-button");
        settings_button.add_css_class("flat");
        settings_button.add_css_class("circular");
        settings_button.set_focus_on_click(false);
        let settings_label = tr("Lyrics settings");
        settings_button.set_tooltip_text(Some(&settings_label));
        settings_button.update_property(&[gtk::accessible::Property::Label(&settings_label)]);
        let top_controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        top_controls.add_css_class("lyrics-controls");
        top_controls.add_css_class("lyrics-top-controls");
        top_controls.set_halign(gtk::Align::Start);
        top_controls.set_valign(gtk::Align::Start);
        top_controls.append(&search_button);
        top_controls.append(&settings_button);

        let offset_decrease_button = lyrics_control_button("rufin-value-decrease-symbolic");
        let offset_entry = gtk::Entry::new();
        offset_entry.set_text("0 ms");
        gtk::prelude::EditableExt::set_alignment(&offset_entry, 0.5);
        offset_entry.set_width_chars(4);
        offset_entry.set_max_width_chars(8);
        offset_entry.set_max_length(24);
        offset_entry.add_css_class("flat");
        offset_entry.add_css_class("lyrics-offset-value");
        let offset_increase_button = lyrics_control_button("rufin-list-add-symbolic");

        let save_button = lyrics_control_button("rufin-document-save-disk-symbolic");

        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        controls.add_css_class("lyrics-controls");
        controls.add_css_class("lyrics-control-bar");
        controls.set_halign(gtk::Align::Center);
        controls.set_valign(gtk::Align::End);
        controls.set_margin_bottom(10);
        controls.append(&save_button);
        controls.append(&offset_decrease_button);
        controls.append(&offset_entry);
        controls.append(&offset_increase_button);
        controls.append(&clear_auto_search_button);

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_hexpand(true);
        scroller.set_vexpand(true);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
        body.set_vexpand(true);
        body.set_margin_end(WINDOW_CHROME_MARGIN_END);
        body.add_css_class("lyrics-lines");
        scroller.set_child(Some(&body));
        root.set_child(Some(&scroller));
        root.add_overlay(&top_controls);
        root.add_overlay(&controls);

        let pane = Self {
            root,
            scroller,
            body,
            save_button,
            clear_auto_search_button,
            search_button,
            settings_button,
            offset_decrease_button,
            offset_entry,
            offset_increase_button,
            rows: Rc::new(RefCell::new(Vec::new())),
            active_index: Rc::new(Cell::new(None)),
            scroll_generation: Rc::new(Cell::new(0)),
            follow_pause_until: Rc::new(Cell::new(None)),
        };
        pane.connect_user_scroll_pause();
        pane
    }

    pub fn widget(&self) -> &gtk::Overlay {
        &self.root
    }

    pub fn connect_search_clicked(&self, search: impl Fn() + 'static) {
        self.search_button.connect_clicked(move |_| search());
    }

    pub fn connect_settings_clicked(&self, open: impl Fn() + 'static) {
        self.settings_button.connect_clicked(move |_| open());
    }

    pub fn connect_save_clicked(&self, save: impl Fn() + 'static) {
        self.save_button.connect_clicked(move |_| save());
    }

    pub fn connect_clear_auto_search_clicked(&self, clear: impl Fn() + 'static) {
        self.clear_auto_search_button
            .connect_clicked(move |_| clear());
    }

    pub fn connect_offset_decrease_clicked(&self, decrease: impl Fn() + 'static) {
        self.offset_decrease_button
            .connect_clicked(move |_| decrease());
    }

    pub fn connect_offset_increase_clicked(&self, increase: impl Fn() + 'static) {
        self.offset_increase_button
            .connect_clicked(move |_| increase());
    }

    pub fn set_search_action(&self, label: &str, enabled: bool) {
        self.search_button.set_tooltip_text(Some(label));
        self.search_button
            .update_property(&[gtk::accessible::Property::Label(label)]);
        self.search_button.set_sensitive(enabled);
    }

    pub fn set_save_action(&self, label: &str, enabled: bool) {
        self.save_button.set_tooltip_text(Some(label));
        self.save_button
            .update_property(&[gtk::accessible::Property::Label(label)]);
        self.save_button.set_visible(enabled);
        self.save_button.set_sensitive(enabled);
    }

    pub fn set_clear_auto_search_action(&self, label: &str, enabled: bool, visible: bool) {
        self.clear_auto_search_button.set_tooltip_text(Some(label));
        self.clear_auto_search_button
            .update_property(&[gtk::accessible::Property::Label(label)]);
        self.clear_auto_search_button.set_visible(visible);
        self.clear_auto_search_button.set_sensitive(enabled);
    }

    pub fn connect_offset_committed(&self, commit: impl Fn(String) + 'static) {
        let commit: Rc<dyn Fn(String)> = Rc::new(commit);
        let activate_commit = Rc::clone(&commit);
        self.offset_entry.connect_activate(move |entry| {
            activate_commit(entry.text().to_string());
        });

        let entry = self.offset_entry.downgrade();
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(move |_| {
            if let Some(entry) = entry.upgrade() {
                commit(entry.text().to_string());
            }
        });
        self.offset_entry.add_controller(focus);
    }

    pub fn connect_offset_changed(&self, changed: impl Fn(String) + 'static) {
        self.offset_entry
            .connect_changed(move |entry| changed(entry.text().to_string()));
    }

    pub fn set_offset_action(
        &self,
        label: &str,
        decrease_label: &str,
        increase_label: &str,
        offset_millis: i64,
        enabled: bool,
    ) {
        self.offset_entry.set_text(&format!("{offset_millis} ms"));
        self.offset_entry.set_tooltip_text(Some(label));
        self.offset_entry
            .update_property(&[gtk::accessible::Property::Label(label)]);
        for (button, button_label) in [
            (&self.offset_decrease_button, decrease_label),
            (&self.offset_increase_button, increase_label),
        ] {
            button.set_visible(enabled);
            button.set_tooltip_text(Some(button_label));
            button.update_property(&[gtk::accessible::Property::Label(button_label)]);
        }
        self.offset_entry.set_visible(enabled);
    }

    pub fn set_content(
        &self,
        content: LyricsPaneContent<'_>,
        show_furigana: bool,
        show_romanization: bool,
        word_by_word_highlighting: bool,
        seek: Rc<dyn Fn(u64)>,
    ) {
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
        self.rows.borrow_mut().clear();
        self.active_index.set(None);
        self.cancel_scroll_animation();
        if !matches!(&content, LyricsPaneContent::Document { .. }) {
            self.body.add_css_class("lyrics-placeholder");
        } else {
            self.body.remove_css_class("lyrics-placeholder");
        }

        if let LyricsPaneContent::Document {
            lyrics: current_lyrics,
            pronunciation,
        } = content
        {
            self.append_document_rows(
                current_lyrics,
                LyricsRowTrack::Primary,
                show_furigana,
                show_romanization && pronunciation.is_none(),
                word_by_word_highlighting,
                Rc::clone(&seek),
            );
            if show_romanization && let Some(pronunciation) = pronunciation {
                let heading = gtk::Label::new(Some(&tr(msgid("Pronunciation"))));
                heading.add_css_class("lyrics-track-heading");
                self.body.append(&heading);
                self.append_document_rows(
                    pronunciation,
                    LyricsRowTrack::Pronunciation,
                    false,
                    false,
                    word_by_word_highlighting,
                    seek,
                );
            }
        } else if matches!(&content, LyricsPaneContent::Instrumental) {
            let indicator = gtk::Box::new(gtk::Orientation::Vertical, 20);
            indicator.set_halign(gtk::Align::Center);
            indicator.set_valign(gtk::Align::Center);
            indicator.set_vexpand(true);

            let icon = gtk::Image::from_icon_name("rufin-audio-x-generic-symbolic");
            icon.set_pixel_size(36);
            icon.add_css_class("dim-label");
            indicator.append(&icon);

            let label = gtk::Label::new(Some(&tr(msgid("Instrumental"))));
            label.add_css_class("dim-label");
            label.add_css_class("heading");
            indicator.append(&label);
            self.body.append(&indicator);
        } else if matches!(&content, LyricsPaneContent::Loading) {
            let placeholder = gtk::Box::new(gtk::Orientation::Vertical, 0);
            placeholder.set_halign(gtk::Align::Fill);
            placeholder.set_valign(gtk::Align::Fill);
            placeholder.set_hexpand(true);
            placeholder.set_vexpand(true);

            let spinner = gtk::Spinner::new();
            spinner.add_css_class("lyrics-loading-spinner");
            spinner.set_halign(gtk::Align::Center);
            spinner.set_valign(gtk::Align::Center);
            spinner.set_hexpand(true);
            spinner.set_vexpand(true);
            spinner.start();
            placeholder.append(&spinner);
            self.body.append(&placeholder);
        } else if let LyricsPaneContent::Empty(empty_status) = content {
            let status = gtk::Label::new(Some(&empty_status));
            status.add_css_class("muted");
            status.set_wrap(true);
            status.set_justify(gtk::Justification::Center);
            status.set_valign(gtk::Align::Center);
            status.set_vexpand(true);
            self.body.append(&status);
        }
    }

    fn append_document_rows(
        &self,
        document: &LyricsDocument,
        track: LyricsRowTrack,
        show_furigana: bool,
        show_romanization: bool,
        word_by_word_highlighting: bool,
        seek: Rc<dyn Fn(u64)>,
    ) {
        let local_japanese_readings =
            track == LyricsRowTrack::Primary && document.is_japanese_for_readings();
        let show_furigana = show_furigana && local_japanese_readings;
        let show_romanization = show_romanization && local_japanese_readings;
        let reading_language = local_japanese_readings
            .then_some("ja")
            .or(document.language.as_deref());
        for (line_index, line) in document.lines.iter().enumerate() {
            if !lyric_line_has_text(line) {
                continue;
            }
            let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
            content.set_hexpand(true);
            content.set_halign(gtk::Align::Fill);
            let mut cue_highlights = Vec::new();
            let reading = (show_furigana || show_romanization)
                .then(|| {
                    japanese_reading_for_language_options(
                        &line.text,
                        reading_language,
                        show_furigana,
                        show_romanization,
                    )
                })
                .flatten();
            if word_by_word_highlighting && !line.cue_lines.is_empty() {
                for cue_line in &line.cue_lines {
                    let cue_part = gtk::Box::new(gtk::Orientation::Vertical, 0);
                    if let Some(agent) = cue_line
                        .agent_id
                        .as_deref()
                        .and_then(|id| document.agents.iter().find(|agent| agent.id == id))
                    {
                        cue_part.add_css_class(match agent.role {
                            LyricsAgentRole::Main => "lyrics-agent-main",
                            LyricsAgentRole::Voice => "lyrics-agent-voice",
                            LyricsAgentRole::Background => "lyrics-agent-background",
                            LyricsAgentRole::Group => "lyrics-agent-group",
                        });
                        if let Some(name) = agent.name.as_deref() {
                            let agent_label = gtk::Label::new(Some(name));
                            agent_label.add_css_class("lyrics-agent-name");
                            cue_part.append(&agent_label);
                        }
                    }
                    let cue_line_widget = WrappingLine::new();
                    cue_line_widget.add_css_class("lyrics-line");
                    if cue_line.cues.is_empty() {
                        cue_line_widget.append(&lyrics_reading_unit(
                            &cue_line.text,
                            show_furigana,
                            reading_language,
                        ));
                    } else {
                        let mut byte_cursor = 0;
                        for cue in &cue_line.cues {
                            if cue.byte_start >= byte_cursor
                                && let Some(gap) = cue_line.text.get(byte_cursor..cue.byte_start)
                                && !gap.is_empty()
                            {
                                cue_line_widget.append(&lyrics_reading_unit(
                                    gap,
                                    show_furigana,
                                    reading_language,
                                ));
                            }
                            let text = cue_line
                                .text
                                .get(cue.byte_start..cue.byte_end_exclusive)
                                .unwrap_or(&cue.text);
                            let widget = lyrics_reading_unit(text, show_furigana, reading_language);
                            widget.add_css_class("lyrics-cue");
                            cue_line_widget.append(&widget);
                            cue_highlights.push(LyricsCueHighlight {
                                cue: cue.clone(),
                                widget,
                            });
                            byte_cursor = byte_cursor.max(cue.byte_end_exclusive);
                        }
                        if let Some(tail) = cue_line.text.get(byte_cursor..)
                            && !tail.is_empty()
                        {
                            cue_line_widget.append(&lyrics_reading_unit(
                                tail,
                                show_furigana,
                                reading_language,
                            ));
                        }
                    }
                    cue_part.append(&cue_line_widget);
                    content.append(&cue_part);
                }
            } else if show_furigana && let Some(reading) = reading.as_ref() {
                content.append(&ruby_line(&reading.segments));
            } else {
                let label = lyrics_label(&line.text);
                label.add_css_class("lyrics-scroll-anchor");
                content.append(&label);
            }

            if show_romanization && let Some(reading) = reading {
                let label = lyrics_label(&reading.romanization);
                label.add_css_class("lyrics-romanization");
                content.append(&label);
            }

            let row: gtk::Widget = if let Some(start_millis) = line.start_millis {
                let row = gtk::Button::new();
                row.add_css_class("flat");
                row.set_hexpand(true);
                row.set_child(Some(&content));
                let seek = Rc::clone(&seek);
                row.connect_clicked(move |_| seek(start_millis));
                row.upcast()
            } else {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                row.set_hexpand(true);
                row.append(&content);
                row.upcast()
            };
            row.add_css_class("lyrics-row");
            self.body.append(&row);
            self.rows.borrow_mut().push(LyricsRow {
                track,
                line_index,
                row,
                cues: cue_highlights,
            });
        }
    }

    pub fn update_highlight(&self, lyrics: Option<&LyricsDocument>, position_millis: i128) {
        self.update_highlight_with_scroll_duration(lyrics, position_millis, None);
    }

    fn update_highlight_with_scroll_duration(
        &self,
        lyrics: Option<&LyricsDocument>,
        position_millis: i128,
        scroll_duration: Option<u64>,
    ) {
        let active_index = lyrics
            .and_then(|lyrics| active_lyrics_line_index(lyrics.lines.as_slice(), position_millis));
        let highlight_all_lines =
            lyrics.is_some_and(|lyrics| should_highlight_all_lyrics_lines(lyrics.lines.as_slice()));
        let previous_index = self.active_index.replace(active_index);
        let follow_pause = self.follow_scroll_pause();
        let scroll_target = {
            let rows = self.rows.borrow();
            for row in rows.iter() {
                let active = row.track == LyricsRowTrack::Primary
                    && (highlight_all_lines || Some(row.line_index) == active_index);
                if active {
                    row.row.add_css_class("lyrics-row-active");
                } else {
                    row.row.remove_css_class("lyrics-row-active");
                }
                let latest_started_cue = row
                    .cues
                    .iter()
                    .filter(|cue| position_millis >= i128::from(cue.cue.start_millis))
                    .map(|cue| cue.cue.start_millis)
                    .max();
                for cue in &row.cues {
                    if position_millis >= i128::from(cue.cue.start_millis) {
                        cue.widget.add_css_class("lyrics-cue-sung");
                    } else {
                        cue.widget.remove_css_class("lyrics-cue-sung");
                    }
                    let cue_active = match cue.cue.end_millis {
                        Some(end) => {
                            position_millis >= i128::from(cue.cue.start_millis)
                                && position_millis < i128::from(end)
                        }
                        None => latest_started_cue == Some(cue.cue.start_millis),
                    };
                    if cue_active {
                        cue.widget.add_css_class("lyrics-cue-active");
                    } else {
                        cue.widget.remove_css_class("lyrics-cue-active");
                    }
                }
            }

            lyrics_follow_scroll_target(active_index, previous_index, follow_pause).and_then(
                |index| {
                    let row = rows
                        .iter()
                        .find(|row| {
                            row.track == LyricsRowTrack::Primary && row.line_index == index
                        })?
                        .row
                        .clone();
                    let duration = scroll_duration.unwrap_or_else(|| {
                        lyrics
                            .map(|lyrics| {
                                lyrics_scroll_animation_millis(
                                    lyrics.lines.as_slice(),
                                    index,
                                    position_millis,
                                )
                            })
                            .unwrap_or(DEFAULT_LYRICS_SCROLL_ANIMATION_MS)
                    });
                    Some((row, duration))
                },
            )
        };

        if let Some((row, duration)) = scroll_target {
            self.scroll_row_into_view(row, duration);
        }
    }

    pub fn refocus_highlight(&self, lyrics: Option<&LyricsDocument>, position_millis: i128) {
        self.active_index.set(None);
        self.follow_pause_until.set(None);
        self.cancel_scroll_animation();
        self.update_highlight_with_scroll_duration(lyrics, position_millis, Some(0));
    }

    pub fn pause_follow_scroll(&self) {
        self.follow_pause_until.set(Some(
            Instant::now() + Duration::from_millis(LYRICS_USER_SCROLL_PAUSE_MS),
        ));
    }

    pub fn clear_follow_scroll_pause(&self) {
        self.follow_pause_until.set(None);
    }

    pub fn restart_follow_tracking(&self) {
        self.active_index.set(None);
        self.follow_pause_until.set(None);
        self.cancel_scroll_animation();
    }

    fn connect_user_scroll_pause(&self) {
        let controller = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let pane = self.clone();
        controller.connect_scroll(move |_, _, _| {
            pane.pause_follow_scroll();
            glib::Propagation::Proceed
        });
        self.scroller.add_controller(controller);
    }

    fn follow_scroll_pause(&self) -> LyricsFollowScrollPause {
        let pause = lyrics_follow_scroll_pause_state(self.follow_pause_until.get(), Instant::now());
        if pause == LyricsFollowScrollPause::Expired {
            self.follow_pause_until.set(None);
        }
        pause
    }

    fn cancel_scroll_animation(&self) {
        self.scroll_generation
            .set(self.scroll_generation.get().saturating_add(1));
    }

    fn scroll_row_into_view(&self, row: gtk::Widget, duration_millis: u64) {
        let scroller = self.scroller.clone();
        let generation = self.scroll_generation.get().saturating_add(1);
        self.scroll_generation.set(generation);
        let scroll_generation = Rc::clone(&self.scroll_generation);
        scroll_row_into_view_when_ready(
            scroller,
            row,
            duration_millis,
            scroll_generation,
            generation,
            LYRICS_SCROLL_READY_RETRIES,
        );
    }
}

fn lyrics_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_xalign(0.5);
    label.set_justify(gtk::Justification::Center);
    label.set_hexpand(true);
    label.add_css_class("lyrics-line");
    label
}

fn ruby_line(segments: &[JapaneseReadingSegment]) -> WrappingLine {
    let line = WrappingLine::new();
    line.add_css_class("lyrics-line");
    for segment in segments {
        line.append(&ruby_segment(segment));
    }
    line
}

fn lyrics_reading_unit(text: &str, show_furigana: bool, language: Option<&str>) -> gtk::Widget {
    if show_furigana
        && let Some(reading) = japanese_reading_for_language_options(text, language, true, false)
    {
        let phrase = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        phrase.set_valign(gtk::Align::Baseline);
        for segment in &reading.segments {
            phrase.append(&ruby_segment(segment));
        }
        return phrase.upcast();
    }
    reading_surface_label(text).upcast()
}

fn ruby_segment(segment: &JapaneseReadingSegment) -> gtk::Box {
    let segment_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    segment_box.set_halign(gtk::Align::Center);
    segment_box.set_valign(gtk::Align::Baseline);
    segment_box.set_baseline_child(1);
    if segment.furigana.is_some() {
        segment_box.add_css_class("lyrics-ruby-annotated");
    }

    let furigana = gtk::Label::new(Some(segment.furigana.as_deref().unwrap_or(" ")));
    furigana.add_css_class("lyrics-furigana");
    furigana.set_halign(gtk::Align::Center);
    segment_box.append(&furigana);
    segment_box.append(&reading_surface_label(&segment.surface));
    segment_box
}

fn reading_surface_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("lyrics-reading-surface");
    label.add_css_class("lyrics-scroll-anchor");
    label.set_halign(gtk::Align::Center);
    label
}

fn first_lyrics_scroll_surface(widget: &gtk::Widget) -> Option<gtk::Widget> {
    if widget.has_css_class("lyrics-scroll-anchor") {
        return Some(widget.clone());
    }
    std::iter::successors(widget.first_child(), gtk::Widget::next_sibling)
        .find_map(|child| first_lyrics_scroll_surface(&child))
}

fn last_lyrics_scroll_surface(widget: &gtk::Widget) -> Option<gtk::Widget> {
    if widget.has_css_class("lyrics-scroll-anchor") {
        return Some(widget.clone());
    }
    std::iter::successors(widget.last_child(), gtk::Widget::prev_sibling)
        .find_map(|child| last_lyrics_scroll_surface(&child))
}

fn scroll_row_into_view_when_ready(
    scroller: gtk::ScrolledWindow,
    row: gtk::Widget,
    duration_millis: u64,
    scroll_generation: Rc<Cell<u64>>,
    generation: u64,
    retries_left: u8,
) {
    glib::idle_add_local_once(move || {
        if scroll_generation.get() != generation {
            return;
        }

        let first_surface = first_lyrics_scroll_surface(&row).unwrap_or_else(|| row.clone());
        let last_surface = last_lyrics_scroll_surface(&row).unwrap_or_else(|| row.clone());
        let first_bounds = first_surface.compute_bounds(&scroller);
        let last_bounds = last_surface.compute_bounds(&scroller);
        let adjustment = scroller.vadjustment();
        let ready = first_bounds.is_some()
            && last_bounds.is_some()
            && scroller.height() > 1
            && adjustment.page_size() > 1.0;
        if !ready && retries_left > 0 {
            glib::timeout_add_local_once(
                Duration::from_millis(LYRICS_SCROLL_READY_RETRY_MS),
                move || {
                    scroll_row_into_view_when_ready(
                        scroller,
                        row,
                        duration_millis,
                        scroll_generation,
                        generation,
                        retries_left - 1,
                    );
                },
            );
            return;
        }

        let (Some(first_bounds), Some(last_bounds)) = (first_bounds, last_bounds) else {
            return;
        };
        let viewport_height = f64::from(scroller.height().max(1));
        let surface_top = f64::from(first_bounds.y().min(last_bounds.y()));
        let surface_bottom = f64::from(
            (first_bounds.y() + first_bounds.height()).max(last_bounds.y() + last_bounds.height()),
        );
        let upper = adjustment.upper() - adjustment.page_size();
        let target = centered_scroll_target(
            surface_top,
            surface_bottom,
            adjustment.value(),
            viewport_height,
        )
        .clamp(adjustment.lower(), upper.max(adjustment.lower()));
        let start = adjustment.value();
        let delta = target - start;
        if duration_millis == 0 || delta.abs() < 1.0 {
            adjustment.set_value(target);
            return;
        }
        let started_at = Instant::now();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            if scroll_generation.get() != generation {
                return glib::ControlFlow::Break;
            }
            let elapsed = started_at.elapsed().as_millis() as f64;
            let progress = (elapsed / duration_millis as f64).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - progress).powi(3);
            adjustment.set_value(start + delta * eased);
            if progress >= 1.0 {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    });
}

fn centered_scroll_target(
    surface_top: f64,
    surface_bottom: f64,
    current_scroll: f64,
    viewport_height: f64,
) -> f64 {
    let surface_center = current_scroll + (surface_top + surface_bottom) / 2.0;
    surface_center - viewport_height / 2.0
}

pub(crate) fn active_lyrics_line_index(
    lines: &[LyricsLine],
    position_millis: i128,
) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let start = line.start_millis?;
            (i128::from(start) <= position_millis).then_some((
                lyric_line_has_text(line).then_some(index),
                start,
                index,
            ))
        })
        .max_by_key(|(_, start, index)| (*start, *index))
        .and_then(|(index, _, _)| index)
}

pub(crate) fn should_highlight_all_lyrics_lines(lines: &[LyricsLine]) -> bool {
    !lines.is_empty() && lines.iter().all(|line| line.start_millis.is_none())
}

pub(crate) fn next_lyrics_line_start_after(
    lines: &[LyricsLine],
    position_millis: i128,
) -> Option<u64> {
    lines
        .iter()
        .flat_map(|line| {
            std::iter::once(line.start_millis).chain(line.cue_lines.iter().flat_map(|cue_line| {
                cue_line
                    .cues
                    .iter()
                    .flat_map(|cue| [Some(cue.start_millis), cue.end_millis].into_iter())
            }))
        })
        .flatten()
        .filter(|start| i128::from(*start) > position_millis)
        .min()
}

pub(crate) fn lyrics_follow_scroll_pause_state(
    paused_until: Option<Instant>,
    now: Instant,
) -> LyricsFollowScrollPause {
    match paused_until {
        Some(paused_until) if now < paused_until => LyricsFollowScrollPause::Active,
        Some(_) => LyricsFollowScrollPause::Expired,
        None => LyricsFollowScrollPause::Inactive,
    }
}

pub(crate) fn lyrics_follow_scroll_target(
    active_index: Option<usize>,
    previous_index: Option<usize>,
    follow_pause: LyricsFollowScrollPause,
) -> Option<usize> {
    if follow_pause == LyricsFollowScrollPause::Active {
        return None;
    }
    active_index.filter(|index| {
        follow_pause == LyricsFollowScrollPause::Expired || Some(*index) != previous_index
    })
}

pub(crate) fn lyrics_scroll_animation_millis(
    lines: &[LyricsLine],
    active_index: usize,
    position_millis: i128,
) -> u64 {
    let budget = lines
        .iter()
        .skip(active_index + 1)
        .filter(|line| lyric_line_has_text(line))
        .filter_map(|line| line.start_millis)
        .find(|start| i128::from(*start) > position_millis)
        .and_then(|next_start| {
            u64::try_from(i128::from(next_start) - position_millis)
                .ok()
                .and_then(|gap| gap.checked_sub(LYRICS_SCROLL_MS))
        });
    budget
        .map(|budget| {
            budget.clamp(
                MIN_LYRICS_SCROLL_ANIMATION_MS,
                DEFAULT_LYRICS_SCROLL_ANIMATION_MS,
            )
        })
        .unwrap_or(DEFAULT_LYRICS_SCROLL_ANIMATION_MS)
}

fn lyric_line_has_text(line: &LyricsLine) -> bool {
    !line.text.trim().is_empty()
}

fn lyrics_control_button(icon_name: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon_name);
    button.add_css_class("icon-button");
    button.add_css_class("flat");
    button.add_css_class("circular");
    button
}

#[cfg(test)]
mod tests {
    use super::{
        LyricsFollowScrollPause, active_lyrics_line_index, centered_scroll_target,
        lyrics_follow_scroll_pause_state, lyrics_follow_scroll_target,
        lyrics_scroll_animation_millis, next_lyrics_line_start_after,
        should_highlight_all_lyrics_lines,
    };
    use lyrics::{LyricsCue, LyricsCueLine, LyricsLine as LyricLine};
    use std::time::{Duration, Instant};

    #[test]
    fn sync_lyrics_started() {
        let lines = vec![
            line("intro", Some(1_000)),
            line("verse", Some(5_500)),
            line("unsynced", None),
            line("chorus", Some(9_000)),
        ];

        assert_eq!(active_lyrics_line_index(&lines, 999), None);
        assert_eq!(active_lyrics_line_index(&lines, 1_000), Some(0));
        assert_eq!(active_lyrics_line_index(&lines, 5_499), Some(0));
        assert_eq!(active_lyrics_line_index(&lines, 5_500), Some(1));
        assert_eq!(active_lyrics_line_index(&lines, 8_999), Some(1));
        assert_eq!(active_lyrics_line_index(&lines, 9_000), Some(3));
    }

    #[test]
    fn lyrics_blank_line_clears_highlight() {
        let lines = vec![
            line("current", Some(1_000)),
            line("", Some(5_000)),
            line("next", Some(9_000)),
        ];

        assert_eq!(active_lyrics_line_index(&lines, 4_999), Some(0));
        assert_eq!(active_lyrics_line_index(&lines, 5_000), None);
        assert_eq!(active_lyrics_line_index(&lines, 8_999), None);
        assert_eq!(active_lyrics_line_index(&lines, 9_000), Some(2));
    }

    #[test]
    fn lyrics_keep_active() {
        let lines = vec![line("last", Some(1_000)), line(" ", Some(5_000))];

        assert_eq!(active_lyrics_line_index(&lines, 5_000), None);
        assert_eq!(active_lyrics_line_index(&lines, 50_000), None);
    }

    #[test]
    fn unsynchronized_lyrics_timed() {
        let lines = vec![line("plain", None)];

        assert_eq!(active_lyrics_line_index(&lines, 0), None);
    }

    #[test]
    fn unsynchronized_lyrics_highlight() {
        let lines = vec![line("first", None), line("second", None)];

        assert!(should_highlight_all_lyrics_lines(&lines));
    }

    #[test]
    fn sync_lyrics_every() {
        let lines = vec![line("first", Some(1_000)), line("untimed note", None)];

        assert!(!should_highlight_all_lyrics_lines(&lines));
        assert!(!should_highlight_all_lyrics_lines(&[]));
    }

    #[test]
    fn lyrics_schedule_line() {
        let lines = vec![
            line("intro", Some(1_000)),
            line("verse", Some(5_500)),
            line("unsynced", None),
            line("chorus", Some(9_000)),
        ];

        assert_eq!(next_lyrics_line_start_after(&lines, 999), Some(1_000));
        assert_eq!(next_lyrics_line_start_after(&lines, 1_000), Some(5_500));
        assert_eq!(next_lyrics_line_start_after(&lines, 5_499), Some(5_500));
        assert_eq!(next_lyrics_line_start_after(&lines, 5_500), Some(9_000));
        assert_eq!(next_lyrics_line_start_after(&lines, 9_000), None);
    }

    #[test]
    fn lyrics_schedule_boundary() {
        let lines = vec![
            line("current", Some(1_000)),
            line("", Some(5_000)),
            line("next", Some(9_000)),
        ];

        assert_eq!(next_lyrics_line_start_after(&lines, 4_999), Some(5_000));
        assert_eq!(next_lyrics_line_start_after(&lines, 5_000), Some(9_000));
    }

    #[test]
    fn karaoke_cue_boundaries_drive_highlight_updates() {
        let mut karaoke = line("word by word", Some(1_000));
        karaoke.cue_lines = vec![LyricsCueLine {
            text: karaoke.text.clone(),
            start_millis: Some(1_000),
            end_millis: Some(2_000),
            agent_id: None,
            cues: vec![
                LyricsCue {
                    text: "word".to_string(),
                    start_millis: 1_000,
                    end_millis: Some(1_400),
                    byte_start: 0,
                    byte_end_exclusive: 4,
                },
                LyricsCue {
                    text: "by word".to_string(),
                    start_millis: 1_400,
                    end_millis: Some(2_000),
                    byte_start: 5,
                    byte_end_exclusive: 12,
                },
            ],
        }];

        assert_eq!(next_lyrics_line_start_after(&[karaoke], 1_000), Some(1_400));
    }

    #[test]
    fn lyrics_finish_line() {
        let lines = vec![line("current", Some(5_500)), line("next", Some(6_000))];

        let duration = lyrics_scroll_animation_millis(&lines, 0, 5_500);

        assert!(duration <= 300);
        assert!(duration >= 80);
        assert_eq!(
            lyrics_scroll_animation_millis(&lines, 0, 5_501),
            duration - 1
        );
    }

    #[test]
    fn lyrics_follow_scroll_pause_expires() {
        let now = Instant::now();

        assert_eq!(
            lyrics_follow_scroll_pause_state(None, now),
            LyricsFollowScrollPause::Inactive
        );
        assert_eq!(
            lyrics_follow_scroll_pause_state(Some(now + Duration::from_millis(1)), now),
            LyricsFollowScrollPause::Active
        );
        assert_eq!(
            lyrics_follow_scroll_pause_state(Some(now), now),
            LyricsFollowScrollPause::Expired
        );
    }

    #[test]
    fn lyrics_ignore_line() {
        assert_eq!(
            lyrics_follow_scroll_target(Some(3), Some(3), LyricsFollowScrollPause::Inactive),
            None
        );
        assert_eq!(
            lyrics_follow_scroll_target(Some(4), Some(3), LyricsFollowScrollPause::Inactive),
            Some(4)
        );
        assert_eq!(
            lyrics_follow_scroll_target(Some(3), Some(3), LyricsFollowScrollPause::Expired),
            Some(3)
        );
        assert_eq!(
            lyrics_follow_scroll_target(Some(4), Some(3), LyricsFollowScrollPause::Active),
            None
        );
    }

    #[test]
    fn lyrics_scroll_centers_the_surface_instead_of_its_annotations() {
        let target = centered_scroll_target(38.0, 58.0, 240.0, 100.0);

        assert_eq!(target, 238.0);
        assert_ne!(target, centered_scroll_target(20.0, 58.0, 240.0, 100.0));
    }

    fn line(text: &str, start_millis: Option<u64>) -> LyricLine {
        LyricLine {
            text: text.to_string(),
            start_millis,
            end_millis: None,
            cue_lines: Vec::new(),
        }
    }
}
