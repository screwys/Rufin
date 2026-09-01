use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::Instant;

use artwork::ArtworkBinding;
use gtk::glib;
use playback::{CurrentMediaId, PlaybackView, RemoteOutput};

use crate::routes::detail_links::{DetailLinks, playback_artist_links};
use crate::routes::route::Route;
use localization::tr;

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

pub(crate) struct NowPlayingPresentation {
    pub(crate) current: bool,
    pub(crate) source_id: Option<sources::SourceId>,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    pub(crate) artist_links: DetailLinks,
    pub(crate) album_links: DetailLinks,
    pub(crate) artwork: ArtworkBinding,
    pub(crate) rating: Option<u8>,
    pub(crate) has_track_key: bool,
    pub(crate) meta: Vec<String>,
}

impl NowPlayingPresentation {
    pub(crate) fn new(player: Option<&PlaybackView>) -> Self {
        let current = player.and_then(|player| player.transport.current.as_deref());
        let title = current
            .map(|entry| entry.track.title.clone())
            .unwrap_or_else(|| tr("Nothing playing"));
        let artist = current
            .map(|entry| entry.track.artist.clone())
            .unwrap_or_else(|| tr("Queue a track to begin"));
        let album = current
            .map(|entry| entry.track.album.clone())
            .unwrap_or_default();
        Self {
            current: current.is_some(),
            source_id: current.map(|entry| sources::SourceId::new(entry.track.source_id.clone())),
            artist_links: current.map_or_else(
                || DetailLinks::text(&artist),
                |entry| playback_artist_links(&artist, &entry.track.artist_links),
            ),
            album_links: DetailLinks::route(
                &album,
                current
                    .and_then(|entry| entry.track.album_key)
                    .map(Route::AlbumDetail),
            ),
            artwork: current
                .and_then(|entry| entry.track.artwork_binding.as_deref())
                .map(ArtworkBinding::opaque)
                .unwrap_or_default(),
            rating: current.and_then(|entry| u8::try_from(entry.track.rating?).ok()),
            has_track_key: current.is_some_and(|entry| entry.track.track_key.is_some()),
            meta: now_playing_meta_parts(current),
            title,
            artist,
            album,
        }
    }
}

fn now_playing_meta_parts(entry: Option<&playback::CurrentMedia>) -> Vec<String> {
    let Some(entry) = entry else {
        return Vec::new();
    };
    let mut parts = Vec::new();
    if let Some(source) = queue_entry_source_label(entry) {
        parts.push(source);
    }
    if let Some(year) = entry.track.year.filter(|year| *year > 0) {
        parts.push(year.to_string());
    }
    parts
}

fn queue_entry_source_label(entry: &playback::CurrentMedia) -> Option<String> {
    entry
        .track
        .source_format
        .as_deref()
        .and_then(audio_source_label_from_format)
        .or_else(|| {
            entry
                .track
                .media_uri
                .as_deref()
                .and_then(audio_source_label_from_path)
        })
}

fn audio_source_label_from_path(path: &str) -> Option<String> {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let extension = Path::new(path).extension()?.to_str()?.trim();
    audio_source_label_from_format(extension)
}

fn audio_source_label_from_format(value: &str) -> Option<String> {
    let value = value
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .trim()
        .trim_start_matches('.');
    if value.is_empty() {
        return None;
    }
    Some(match value.to_ascii_lowercase().as_str() {
        "mpeg" | "mpga" => "MP3".to_string(),
        other => other.to_ascii_uppercase(),
    })
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
