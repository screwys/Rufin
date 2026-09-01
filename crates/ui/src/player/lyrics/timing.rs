use super::view::next_lyrics_highlight_after;
use crate::shell::Shell;
use gtk::glib;
use playback::{PlaybackOutput, TransportStatus};
use std::rc::Rc;
use std::time::Duration;

impl Shell {
    pub(crate) fn cancel_scheduled_lyrics_highlight(&self) {
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        if let Some(source) = lyrics.timing_source.borrow_mut().take() {
            source.remove();
        }
    }
    pub(crate) fn schedule_next_lyrics_highlight(self: &Rc<Self>, position_millis: u64) {
        if !self.lyrics_surface_visible() {
            return;
        }
        let follows_local_clock = self.selected_playback().as_deref().is_some_and(|player| {
            lyrics_follow_local_clock(player.transport.state, &player.controls.playback_output)
        });
        if !follows_local_clock {
            return;
        }

        let Some(next_position_millis) = self.visible_lyrics().as_ref().and_then(|lyrics| {
            next_lyrics_highlight_after(&lyrics.lines, self.lyrics_position_millis(position_millis))
        }) else {
            return;
        };
        let lyrics_position_millis = self.lyrics_position_millis(position_millis);
        let Ok(delay_millis) =
            u64::try_from(i128::from(next_position_millis) - lyrics_position_millis)
        else {
            return;
        };
        let next_playback_position_millis = position_millis.saturating_add(delay_millis);
        let Some(lyrics) = self.selected_lyrics() else {
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
            shell.update_lyrics_highlight_at(next_playback_position_millis);
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

fn lyrics_follow_local_clock(state: TransportStatus, output: &PlaybackOutput) -> bool {
    state == TransportStatus::Playing && output.is_local()
}

#[cfg(test)]
mod tests {
    use playback::{PlaybackOutput, RemoteOutput, RemoteOutputProtocol, TransportStatus};

    use super::lyrics_follow_local_clock;

    #[test]
    fn remote_lyrics_wait_for_receiver_positions() {
        let remote = PlaybackOutput::Remote(RemoteOutput {
            id: "renderer".to_string(),
            name: "Renderer".to_string(),
            protocol: RemoteOutputProtocol::Upnp,
        });

        assert!(lyrics_follow_local_clock(
            TransportStatus::Playing,
            &PlaybackOutput::Local,
        ));
        assert!(!lyrics_follow_local_clock(
            TransportStatus::Playing,
            &remote,
        ));
    }
}
