use crate::player::PlayerUi;
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
        lyrics_position_millis: i128,
    ) {
        if !self.lyrics_surface_visible() {
            return;
        }
        // Receiver positions anchor the same animation clock as local playback;
        // waiting for every remote poll makes karaoke advance in visible steps.
        let Some(rate) = self
            .selected_playback()
            .as_deref()
            .filter(|player| player.transport.state == TransportStatus::Playing)
            .map(|player| player.transport.playback_rate)
        else {
            return;
        };

        if self.visible_lyrics().is_none() {
            return;
        }
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let Some(shell) = self.shell() else {
            return;
        };
        let karaoke = shell
            .settings
            .current
            .borrow()
            .lyrics
            .preferences
            .karaoke_mode;
        let next_position = match &*lyrics.projection.borrow() {
            rufin_core::lyrics::CurrentLyrics::Ready {
                content: Some(rufin_core::lyrics::CurrentLyricsContent::Document { prepared, .. }),
                ..
            } => prepared.timing.next_after(lyrics_position_millis, karaoke),
            _ => None,
        };
        let Some(next_position_millis) = next_position else {
            return;
        };
        let Ok(delay_millis) =
            u64::try_from(i128::from(next_position_millis) - lyrics_position_millis)
        else {
            return;
        };
        drop(lyrics);

        let shell = Rc::clone(self);
        let delay = Duration::from_millis(delay_millis).div_f64(rate);
        let source = glib::timeout_add_local_once(delay, move || {
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
