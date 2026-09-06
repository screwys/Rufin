use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use localization::tr;
use lyrics::{CurrentLyrics, CurrentLyricsContent, LyricsDocument, LyricsOrigin};

use crate::player::lyrics::LyricsPane;
use crate::player::lyrics::search::LyricsSearchDialog;
use crate::player::lyrics::timing::LyricsTiming;
use crate::player::state::{current_playback_media_id, current_playback_track_id};
use crate::shell::Shell;

pub(crate) struct LyricsState {
    pub(crate) panel_visible: Cell<bool>,
}

pub(crate) struct SelectedLyricsState {
    pub(crate) projection: RefCell<CurrentLyrics>,
    pub(crate) offset_millis: Cell<i64>,
    pub(crate) timing_source: RefCell<Option<glib::SourceId>>,
    pub(super) timing: RefCell<LyricsTiming>,
    pub(crate) right_pane_dirty: Cell<bool>,
    pub(crate) fullscreen_pane_dirty: Cell<bool>,
    pub(crate) search_dialog: RefCell<Option<LyricsSearchDialog>>,
    pub(crate) settings_dialog: gtk::glib::WeakRef<adw::PreferencesDialog>,
    pub(crate) right_pane: LyricsPane,
    pub(crate) fullscreen_pane: LyricsPane,
}

impl SelectedLyricsState {
    pub(crate) fn new() -> Self {
        let right_pane = LyricsPane::new();
        right_pane.use_right_panel_scrollbar();
        let fullscreen_pane = LyricsPane::new();
        fullscreen_pane
            .widget()
            .add_css_class("fullscreen-player-pane");
        Self {
            projection: RefCell::new(CurrentLyrics::Cleared),
            offset_millis: Cell::new(0),
            timing_source: RefCell::new(None),
            timing: RefCell::new(LyricsTiming::default()),
            right_pane_dirty: Cell::new(true),
            fullscreen_pane_dirty: Cell::new(true),
            search_dialog: RefCell::new(None),
            settings_dialog: gtk::glib::WeakRef::new(),
            right_pane,
            fullscreen_pane,
        }
    }
}

impl Drop for SelectedLyricsState {
    fn drop(&mut self) {
        if let Some(source) = self.timing_source.get_mut().take() {
            source.remove();
        }
        if let Some(dialog) = self.search_dialog.get_mut().take() {
            if let Some(source) = dialog.search_debounce_source.borrow_mut().take() {
                source.remove();
            }
            dialog.dialog.close();
        }
        if let Some(dialog) = self.settings_dialog.upgrade() {
            dialog.close();
        }
    }
}

impl Shell {
    pub(crate) fn right_lyrics_surface_visible(&self) -> bool {
        !self.fullscreen_player_visible()
            && self.right_sidebar_visible()
            && self.lyrics.panel_visible.get()
    }

    pub(crate) fn fullscreen_lyrics_surface_visible(&self) -> bool {
        self.fullscreen_player_visible()
            && self
                .player_view
                .fullscreen_player
                .stack
                .visible_child_name()
                .as_deref()
                == Some("lyrics")
    }

    pub(crate) fn lyrics_surface_visible(&self) -> bool {
        self.right_lyrics_surface_visible() || self.fullscreen_lyrics_surface_visible()
    }

    pub(crate) fn visible_lyrics(&self) -> Option<Arc<LyricsDocument>> {
        let current_media = current_playback_media_id(self.selected_playback().as_deref());
        let lyrics = self.selected_lyrics()?;
        match &*lyrics.projection.borrow() {
            CurrentLyrics::Ready {
                media_id,
                content: Some(CurrentLyricsContent::Document { document, .. }),
                ..
            } if current_media.as_ref() == Some(media_id) => Some(document.clone()),
            CurrentLyrics::Cleared
            | CurrentLyrics::Loading { .. }
            | CurrentLyrics::Ready { .. } => None,
        }
    }

    pub(crate) fn visible_lyrics_are_instrumental(&self) -> bool {
        let current_media = current_playback_media_id(self.selected_playback().as_deref());
        let Some(lyrics) = self.selected_lyrics() else {
            return false;
        };
        matches!(
            &*lyrics.projection.borrow(),
            CurrentLyrics::Ready {
                media_id,
                content: Some(CurrentLyricsContent::Instrumental),
                ..
            } if current_media.as_ref() == Some(media_id)
        )
    }

    fn current_lyrics_resolved(&self) -> bool {
        let current_media = current_playback_media_id(self.selected_playback().as_deref());
        let Some(lyrics) = self.selected_lyrics() else {
            return false;
        };
        matches!(
            &*lyrics.projection.borrow(),
            CurrentLyrics::Ready {
                media_id,
                content: Some(_),
                ..
            } if current_media.as_ref() == Some(media_id)
        )
    }

    pub(crate) fn visible_lyrics_origin(&self) -> Option<LyricsOrigin> {
        let current_media = current_playback_media_id(self.selected_playback().as_deref());
        let lyrics = self.selected_lyrics()?;
        match &*lyrics.projection.borrow() {
            CurrentLyrics::Ready {
                media_id, origin, ..
            } if current_media.as_ref() == Some(media_id) => *origin,
            _ => None,
        }
    }

    pub(crate) fn visible_lyrics_pronunciation(&self) -> Option<Arc<LyricsDocument>> {
        let current_media = current_playback_media_id(self.selected_playback().as_deref());
        let lyrics = self.selected_lyrics()?;
        match &*lyrics.projection.borrow() {
            CurrentLyrics::Ready {
                media_id, content, ..
            } if current_media.as_ref() == Some(media_id) => match content {
                Some(CurrentLyricsContent::Document { pronunciation, .. }) => pronunciation.clone(),
                Some(CurrentLyricsContent::Instrumental) | None => None,
            },
            _ => None,
        }
    }

    pub(crate) fn current_lyrics_loading(&self) -> bool {
        let current_media = current_playback_media_id(self.selected_playback().as_deref());
        let Some(lyrics) = self.selected_lyrics() else {
            return false;
        };
        matches!(
            &*lyrics.projection.borrow(),
            CurrentLyrics::Loading { media_id }
                if current_media.as_ref() == Some(media_id)
        )
    }

    pub(crate) fn apply_current_lyrics(self: &Rc<Self>, projection: CurrentLyrics) {
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let document_changed = match (&*lyrics.projection.borrow(), &projection) {
            (
                CurrentLyrics::Ready {
                    content:
                        Some(CurrentLyricsContent::Document {
                            document: previous, ..
                        }),
                    ..
                },
                CurrentLyrics::Ready {
                    content: Some(CurrentLyricsContent::Document { document: next, .. }),
                    ..
                },
            ) => !Arc::ptr_eq(previous, next),
            (
                _,
                CurrentLyrics::Ready {
                    content: Some(CurrentLyricsContent::Document { .. }),
                    ..
                },
            ) => true,
            _ => false,
        };
        let (media_id, has_lyrics) = match &projection {
            CurrentLyrics::Ready {
                media_id, content, ..
            } => (Some(media_id.clone()), content.is_some()),
            CurrentLyrics::Loading { media_id } => (Some(media_id.clone()), false),
            CurrentLyrics::Cleared => (None, false),
        };
        if document_changed {
            self.restart_lyrics_follow_tracking();
            lyrics.offset_millis.set(0);
        }
        match &projection {
            CurrentLyrics::Ready {
                content: Some(CurrentLyricsContent::Document { document, .. }),
                ..
            } if document_changed => {
                *lyrics.timing.borrow_mut() = LyricsTiming::new(&document.lines);
            }
            CurrentLyrics::Ready {
                content: Some(CurrentLyricsContent::Document { .. }),
                ..
            } => {}
            _ => *lyrics.timing.borrow_mut() = LyricsTiming::default(),
        }
        *lyrics.projection.borrow_mut() = projection;
        self.render_lyrics_panel();
        if let Some(media_id) = media_id
            && let Some(dialog) = lyrics.search_dialog.borrow().as_ref()
            && dialog.media_id == media_id
            && dialog.status.text().as_str() == tr("Searching...")
            && !self.current_lyrics_loading()
        {
            dialog.status.set_text(&if has_lyrics {
                tr("Lyrics loaded")
            } else {
                tr("No lyrics")
            });
        }
        if document_changed {
            self.refocus_current_lyrics_highlight();
            let shell = Rc::clone(self);
            glib::idle_add_local_once(move || {
                shell.refocus_current_lyrics_highlight();
            });
        } else {
            let shell = Rc::clone(self);
            glib::idle_add_local_once(move || {
                shell.restart_lyrics_follow_tracking();
                shell.update_lyrics_highlight();
            });
        }
    }

    fn restart_lyrics_follow_tracking(&self) {
        if let Some(lyrics) = self.selected_lyrics() {
            lyrics.right_pane.restart_follow_tracking();
            lyrics.fullscreen_pane.restart_follow_tracking();
        }
    }

    pub(crate) fn refocus_current_lyrics_highlight(&self) {
        let lyrics = self.visible_lyrics();
        let position_millis = self.lyrics_position_millis(self.current_position_millis());
        let Some(selected_lyrics) = self.selected_lyrics() else {
            return;
        };
        if self.right_lyrics_surface_visible() {
            selected_lyrics
                .right_pane
                .refocus_highlight(lyrics.as_deref(), position_millis);
        }
        if self.fullscreen_lyrics_surface_visible() {
            selected_lyrics
                .fullscreen_pane
                .refocus_highlight(lyrics.as_deref(), position_millis);
        }
    }

    pub(crate) fn request_initial_lyrics_if_needed(&self) {
        self.request_auto_lyrics_if_needed();
    }

    pub(crate) fn request_auto_lyrics_if_needed(&self) {
        let Some(media_id) = current_playback_media_id(self.selected_playback().as_deref()) else {
            return;
        };
        if self.current_lyrics_resolved() || self.current_lyrics_loading() {
            return;
        }
        if !self.lyrics_surface_visible() {
            return;
        }
        self.products.lyrics.load(media_id);
    }

    pub(crate) fn suppress_auto_lyrics_for_current(self: &Rc<Self>) {
        let Some(media_id) = current_playback_media_id(self.selected_playback().as_deref()) else {
            return;
        };
        let Some(track_id) = current_playback_track_id(self.selected_playback().as_deref()) else {
            return;
        };
        self.products.lyrics.clear_fetched(media_id);
        self.update_app_settings("lyrics auto-search setting", |settings| {
            settings.lyrics.suppress_auto_lyrics(&track_id)
        });
        self.render_lyrics_panel();
    }

    pub(crate) fn lyrics_empty_status(&self) -> String {
        let settings = self.settings.current.borrow();
        if settings.private_mode {
            tr("Server lyrics are off in private mode")
        } else if !settings.lyrics.external_lyrics_enabled {
            tr("External lyric lookup is off")
        } else {
            tr("No lyrics")
        }
    }

    pub(crate) fn update_lyrics_highlight(self: &Rc<Self>) {
        self.cancel_scheduled_lyrics_highlight();
        self.update_lyrics_highlight_at(self.current_position_millis());
    }

    pub(crate) fn update_lyrics_highlight_at(self: &Rc<Self>, position_millis: u64) {
        if !self.lyrics_surface_visible() {
            return;
        }
        let lyrics = self.visible_lyrics();
        let lyrics_position_millis = self.lyrics_position_millis(position_millis);
        let Some(selected_lyrics) = self.selected_lyrics() else {
            return;
        };
        if self.right_lyrics_surface_visible() {
            selected_lyrics
                .right_pane
                .update_highlight(lyrics.as_deref(), lyrics_position_millis);
        }
        if self.fullscreen_lyrics_surface_visible() {
            selected_lyrics
                .fullscreen_pane
                .update_highlight(lyrics.as_deref(), lyrics_position_millis);
        }
        self.schedule_next_lyrics_highlight(position_millis);
    }

    pub(crate) fn lyrics_position_millis(&self, position_millis: u64) -> i128 {
        let offset_millis = self
            .selected_lyrics()
            .map_or(0, |lyrics| lyrics.offset_millis.get());
        i128::from(position_millis) + i128::from(offset_millis)
    }

    pub(crate) fn adjust_lyrics_offset(self: &Rc<Self>, delta_millis: i64) {
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let offset_millis = lyrics.offset_millis.get().saturating_add(delta_millis);
        drop(lyrics);
        self.set_lyrics_offset(offset_millis);
    }

    pub(crate) fn set_lyrics_offset_from_text(self: &Rc<Self>, value: &str) {
        let Some(offset_millis) = parse_lyrics_offset_millis(value) else {
            self.update_lyrics_offset_controls();
            return;
        };
        self.set_lyrics_offset(offset_millis);
    }

    pub(crate) fn apply_lyrics_offset_from_text(self: &Rc<Self>, value: &str) {
        let Some(offset_millis) = parse_lyrics_offset_millis(value) else {
            return;
        };
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let changed = lyrics.offset_millis.replace(offset_millis) != offset_millis;
        drop(lyrics);
        if changed {
            self.update_lyrics_highlight();
        }
    }

    fn set_lyrics_offset(self: &Rc<Self>, offset_millis: i64) {
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let changed = lyrics.offset_millis.replace(offset_millis) != offset_millis;
        drop(lyrics);
        self.update_lyrics_offset_controls();
        if changed {
            self.update_lyrics_highlight();
        }
    }

    fn update_lyrics_offset_controls(&self) {
        let label = tr("Lyrics offset (ms)");
        let decrease_label = tr("Decrease");
        let increase_label = tr("Increase");
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let offset_millis = lyrics.offset_millis.get();
        for pane in [&lyrics.right_pane, &lyrics.fullscreen_pane] {
            pane.set_offset_action(
                &label,
                &decrease_label,
                &increase_label,
                offset_millis,
                true,
            );
        }
    }

    pub(crate) fn current_position_millis(&self) -> u64 {
        self.selected_playback()
            .as_deref()
            .map_or(0, |player| player.transport.position_millis)
    }

    pub(crate) fn seek_to_lyrics_position(self: &Rc<Self>, position_millis: u64) {
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        lyrics.right_pane.clear_follow_scroll_pause();
        lyrics.fullscreen_pane.clear_follow_scroll_pause();
        let offset_millis = lyrics.offset_millis.get();
        drop(lyrics);
        let position_millis = playback_position_for_lyrics_position(position_millis, offset_millis);
        self.products
            .playback
            .transport
            .seek_millis(position_millis);
        self.update_lyrics_highlight_at(position_millis);
    }
}

fn playback_position_for_lyrics_position(position_millis: u64, offset_millis: i64) -> u64 {
    if offset_millis >= 0 {
        position_millis.saturating_sub(offset_millis.unsigned_abs())
    } else {
        position_millis.saturating_add(offset_millis.unsigned_abs())
    }
}

pub(crate) fn parse_lyrics_offset_millis(value: &str) -> Option<i64> {
    let value = value.trim();
    let number = ["ms", "MS", "Ms", "mS"]
        .into_iter()
        .find_map(|suffix| value.strip_suffix(suffix))
        .unwrap_or(value)
        .trim();
    number.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{parse_lyrics_offset_millis, playback_position_for_lyrics_position};

    #[test]
    fn lyrics_offset_input_accepts_milliseconds() {
        assert_eq!(parse_lyrics_offset_millis("100ms"), Some(100));
        assert_eq!(parse_lyrics_offset_millis(" -250 ms "), Some(-250));
        assert_eq!(parse_lyrics_offset_millis("50"), Some(50));
        assert_eq!(parse_lyrics_offset_millis("later"), None);
    }

    #[test]
    fn lyrics_row_seek_applies_the_inverse_offset() {
        assert_eq!(playback_position_for_lyrics_position(5_000, 250), 4_750);
        assert_eq!(playback_position_for_lyrics_position(5_000, -250), 5_250);
        assert_eq!(playback_position_for_lyrics_position(100, 250), 0);
    }
}
