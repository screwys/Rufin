use crate::PlayerUi;
use gtk::glib;
use playback::TransportStatus;
use std::{
    rc::Rc,
    time::{Duration, Instant},
};
impl PlayerUi {
    pub fn cancel_scheduled_lyrics_highlight(&self) {
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        if let Some(source) = lyrics.timing_source.borrow_mut().take() {
            source.remove();
        }
    }
    pub fn schedule_next_lyrics_highlight(
        self: &Rc<Self>,
        position_millis: u64,
        observed_at: Instant,
    ) {
        if !self.lyrics_surface_visible() {
            return;
        }
        // Receiver positions anchor the same animation clock as local playback;
        // waiting for every remote poll makes karaoke advance in visible steps.
        if !self
            .selected_playback()
            .as_deref()
            .is_some_and(|player| player.transport.state == TransportStatus::Playing)
        {
            return;
        }

        if self.visible_lyrics().is_none() {
            return;
        }
        let current_position =
            position_millis.saturating_add(observed_at.elapsed().as_millis() as u64);
        let lyrics_position_millis = self.lyrics_position_millis(current_position);
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let karaoke = self.settings.current.borrow().lyrics.karaoke_mode;
        let Some(next_position_millis) = lyrics
            .timing
            .borrow()
            .next_after(lyrics_position_millis, karaoke)
        else {
            return;
        };
        let Ok(delay_millis) =
            u64::try_from(i128::from(next_position_millis) - lyrics_position_millis)
        else {
            return;
        };
        drop(lyrics);

        let shell = Rc::clone(self);
        let source = glib::timeout_add_local_once(Duration::from_millis(delay_millis), move || {
            let Some(lyrics) = shell.selected_lyrics() else {
                return;
            };
            let _source = lyrics.timing_source.borrow_mut().take();
            drop(lyrics);
            // Keep the original position/time anchor across ticks. Adding the
            // requested delay loses time whenever the GTK main loop runs late.
            shell.update_lyrics_highlight_at(position_millis, observed_at);
        });
        let Some(lyrics) = self.selected_lyrics() else {
            source.remove();
            return;
        };
        if let Some(previous_source) = lyrics.timing_source.borrow_mut().replace(source) {
            previous_source.remove();
        }
    }
}
