use std::cell::{Cell, RefCell};
use std::time::Instant;

use artwork::ArtworkBinding;
use gtk::glib;
use playback::{CurrentMediaId, PlaybackView, RemoteOutput};

use gtk_widgets::detail_links::{DetailLinks, track_artist_links};
use gtk_widgets::route::Route;
use localization::tr;
use rufin_core::settings::presentation::now_playing_labels;

#[derive(Default)]
pub struct PlaybackState {
    pub updating_controls: Cell<bool>,
    pub seek_pointer_active: Cell<bool>,
    pub volume_persist_source: RefCell<Option<glib::SourceId>>,
    pub audio_output_options: RefCell<Vec<(Option<String>, String)>>,
    pub audio_output_refresh_running: Cell<bool>,
    pub audio_output_refresh_generation: Cell<u64>,
    pub audio_output_refreshed_at: Cell<Option<Instant>>,
    pub remote_output_options: RefCell<Vec<RemoteOutput>>,
}

pub struct NowPlayingPresentation {
    pub current: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub artist_links: DetailLinks,
    pub album_links: DetailLinks,
    pub artwork: ArtworkBinding,
    pub meta: Vec<String>,
}

impl NowPlayingPresentation {
    pub fn new(player: Option<&PlaybackView>) -> Self {
        let current = player.and_then(|player| player.transport.current.as_ref());
        let title = current
            .map(|entry| entry.row.track.metadata.title.clone())
            .unwrap_or_else(|| tr("Nothing playing"));
        let artist = current
            .map(|entry| entry.row.track.metadata.artist.clone())
            .unwrap_or_else(|| tr("Queue a track to begin"));
        let album = current
            .map(|entry| entry.row.track.metadata.album.clone())
            .unwrap_or_default();
        Self {
            current: current.is_some(),
            artist_links: current
                .map(|current| track_artist_links(&current.row.track))
                .unwrap_or_else(|| DetailLinks::text(&artist)),
            album_links: DetailLinks::route(
                &album,
                current.and_then(|current| {
                    current
                        .row
                        .track
                        .links
                        .album_media_uri
                        .clone()
                        .map(Route::AlbumDetail)
                }),
            ),
            artwork: current
                .and_then(|entry| entry.row.track.artwork_binding.as_deref())
                .map(ArtworkBinding::opaque)
                .unwrap_or_default(),
            meta: now_playing_labels(current, None),
            title,
            artist,
            album,
        }
    }
}

pub fn current_playback_track(player: Option<&PlaybackView>) -> Option<library::TrackView> {
    player?
        .transport
        .current
        .as_ref()
        .map(|entry| entry.row.track.clone())
}

pub fn current_playback_track_id(player: Option<&PlaybackView>) -> Option<String> {
    player?
        .transport
        .current
        .as_ref()
        .map(|entry| entry.row.track.media_uri.clone())
}

pub fn current_playback_media_id(player: Option<&PlaybackView>) -> Option<CurrentMediaId> {
    player?
        .transport
        .current
        .as_ref()
        .map(|current| current.id.clone())
}

pub fn playback_window_title(title: Option<&str>, artist: Option<&str>) -> String {
    title
        .into_iter()
        .chain(artist)
        .filter(|part| !part.trim().is_empty())
        .chain(std::iter::once(app_identity::DISPLAY_NAME))
        .collect::<Vec<_>>()
        .join(" · ")
}
