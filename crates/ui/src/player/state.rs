use std::cell::{Cell, RefCell};
use std::time::Instant;

use gtk::glib;
use playback::{CurrentMediaId, PlaybackView, RemoteOutput};

#[derive(Default)]
pub(crate) struct PlaybackState {
    pub(crate) updating_controls: Cell<bool>,
    pub(crate) seek_generation: Cell<u64>,
    pub(crate) volume_persist_source: RefCell<Option<glib::SourceId>>,
    pub(crate) audio_output_options: RefCell<Vec<(Option<String>, String)>>,
    pub(crate) audio_output_refresh_running: Cell<bool>,
    pub(crate) audio_output_refresh_generation: Cell<u64>,
    pub(crate) audio_output_refreshed_at: Cell<Option<Instant>>,
    pub(crate) remote_output_options: RefCell<Vec<RemoteOutput>>,
}

impl PlaybackState {
    pub(crate) fn next_seek_generation(&self) -> u64 {
        let generation = self.seek_generation.get().saturating_add(1);
        self.seek_generation.set(generation);
        generation
    }

    pub(crate) fn seek_generation(&self) -> u64 {
        self.seek_generation.get()
    }
}

pub(crate) fn current_playback_track(
    player: Option<&PlaybackView>,
) -> Option<playback::PlaybackMedia> {
    player?
        .transport
        .current
        .as_ref()
        .map(|entry| entry.track.clone())
}

pub(crate) fn current_playback_track_id(player: Option<&PlaybackView>) -> Option<String> {
    current_playback_track(player).map(|track| track.track_object_id)
}

pub(crate) fn current_playback_media_id(player: Option<&PlaybackView>) -> Option<CurrentMediaId> {
    player?
        .transport
        .current
        .as_ref()
        .map(|current| current.id.clone())
}

#[cfg(test)]
mod tests {
    use super::PlaybackState;

    #[test]
    fn seek_commit_generations_do_not_repeat_across_source_sessions() {
        let playback = PlaybackState::default();
        let old_session_timer = playback.next_seek_generation();
        let new_session_timer = playback.next_seek_generation();

        assert_ne!(old_session_timer, new_session_timer);
        assert_eq!(playback.seek_generation(), new_session_timer);
    }
}
