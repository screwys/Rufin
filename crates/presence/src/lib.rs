//! Shared rich-presence settings and Discord activity construction.
//! Platform hosts own authentication and delivery.

use std::sync::Arc;

use playback::{CurrentMedia, PlaybackView, TransportStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const DEFAULT_CLIENT_ID: &str = "1505345384686419979";
pub const APP_ICON_ASSET: &str = "rufin";

const MAX_TEXT_LENGTH: usize = 127;
const MAX_URL_LENGTH: usize = 256;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum DisplayType {
    #[serde(rename = "artist")]
    Artist,
    #[serde(rename = "application", alias = "app")]
    #[default]
    Application,
    #[serde(rename = "song")]
    Song,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum LinkType {
    #[serde(rename = "last_fm")]
    LastFm,
    #[serde(rename = "musicbrainz")]
    #[default]
    MusicBrainz,
    #[serde(rename = "musicbrainz_last_fm")]
    MusicBrainzLastFm,
    #[serde(rename = "none")]
    None,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct Settings {
    #[serde(rename = "discord_presence_enabled")]
    pub enabled: bool,
    #[serde(rename = "discord_client_id")]
    pub client_id: String,
    #[serde(rename = "discord_display_type")]
    pub display_type: DisplayType,
    #[serde(rename = "discord_link_type")]
    pub link_type: LinkType,
    #[serde(rename = "discord_show_paused")]
    pub show_paused: bool,
    #[serde(rename = "discord_show_as_listening")]
    pub show_as_listening: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            client_id: DEFAULT_CLIENT_ID.to_string(),
            display_type: DisplayType::Application,
            link_type: LinkType::MusicBrainz,
            show_paused: false,
            show_as_listening: true,
        }
    }
}

impl Settings {
    pub fn sanitize(&mut self) {
        self.client_id = self.client_id.trim().to_string();
        if self.client_id.is_empty() {
            self.client_id = DEFAULT_CLIENT_ID.to_string();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackState {
    Playing,
    Paused,
}

#[derive(Clone)]
pub struct Activity {
    settings: Settings,
    media: Arc<CurrentMedia>,
    playback_state: PlaybackState,
    started_at_millis: Option<u64>,
    ended_at_millis: Option<u64>,
    pub large_image: String,
}

impl Activity {
    pub fn client_id(&self) -> &str {
        &self.settings.client_id
    }

    pub fn json(&self) -> Value {
        activity_json(self)
    }

    pub fn new(
        settings: &Settings,
        view: &PlaybackView,
        now_millis: u64,
        large_image: String,
    ) -> Option<Self> {
        let playback_state = visible_playback_state(settings, view.transport.state)?;
        let media = Arc::clone(view.transport.current.as_ref()?);
        media.id.run?;
        let duration_millis = duration_millis(&media);
        let started_at_millis = match playback_state {
            PlaybackState::Playing => {
                Some(now_millis.saturating_sub(view.transport.position_millis))
            }
            PlaybackState::Paused => None,
        };
        Some(Self {
            settings: settings.clone(),
            media,
            playback_state,
            started_at_millis,
            ended_at_millis: started_at_millis.and_then(|started| {
                (duration_millis > 0).then(|| started.saturating_add(duration_millis))
            }),
            large_image,
        })
    }

    pub fn matches(&self, view: &PlaybackView) -> bool {
        Some(self.playback_state) == visible_playback_state(&self.settings, view.transport.state)
            && view
                .transport
                .current
                .as_ref()
                .is_some_and(|media| media.as_ref() == self.media.as_ref())
    }
}

fn duration_millis(media: &CurrentMedia) -> u64 {
    u64::try_from(media.duration_millis.max(0)).unwrap_or(u64::MAX)
}

pub fn visible_playback_state(
    settings: &Settings,
    state: TransportStatus,
) -> Option<PlaybackState> {
    if !settings.enabled {
        return None;
    }
    Some(match state {
        TransportStatus::Playing | TransportStatus::Buffering => PlaybackState::Playing,
        TransportStatus::Paused if settings.show_paused => PlaybackState::Paused,
        TransportStatus::Stopped
        | TransportStatus::Resolving
        | TransportStatus::Paused
        | TransportStatus::Failed => return None,
    })
}

fn activity_json(activity: &Activity) -> Value {
    let track = &activity.media;
    let (small_image, small_text) = match activity.playback_state {
        PlaybackState::Playing => ("playing", "Playing"),
        PlaybackState::Paused => ("paused", "Paused"),
    };
    let mut value = json!({
        "details": discord_text(&track.title, "Idle"),
        "state": discord_text(&track.artist, "Unknown artist"),
        "assets": {
            "large_image": activity.large_image,
            "large_text": discord_text(&track.album, "Unknown album"),
            "small_image": small_image,
            "small_text": small_text,
        },
        "timestamps": {},
        "instance": false,
        "status_display_type": status_display_type(activity.settings.display_type),
        "type": if activity.settings.show_as_listening { 2 } else { 0 },
    });
    if let Some(start) = activity.started_at_millis {
        value["timestamps"]["start"] = json!(start / 1_000);
    }
    if let Some(end) = activity.ended_at_millis {
        value["timestamps"]["end"] = json!(end / 1_000);
    }
    let (details_url, state_url) = activity_urls(activity);
    if let Some(details_url) = details_url {
        value["details_url"] = json!(details_url);
    }
    if let Some(state_url) = state_url {
        value["state_url"] = json!(state_url);
    }
    value
}

const fn status_display_type(display_type: DisplayType) -> u8 {
    match display_type {
        DisplayType::Application => 0,
        DisplayType::Artist => 1,
        DisplayType::Song => 2,
    }
}

fn activity_urls(activity: &Activity) -> (Option<String>, Option<String>) {
    let track = &activity.media;
    let track_artist = track.artist.trim();
    let album_artist = activity
        .media
        .album_display_artist
        .as_deref()
        .map(str::trim)
        .filter(|artist| !artist.is_empty())
        .unwrap_or(track_artist);
    let mut details = None;
    let mut state = None;
    if matches!(
        activity.settings.link_type,
        LinkType::LastFm | LinkType::MusicBrainzLastFm
    ) {
        state = lastfm_artist_url(track_artist);
        details = lastfm_track_url(album_artist, &track.album, &track.title);
    }
    if matches!(
        activity.settings.link_type,
        LinkType::MusicBrainz | LinkType::MusicBrainzLastFm
    ) {
        if activity.settings.link_type == LinkType::MusicBrainz {
            state = track
                .primary_artist_musicbrainz_id
                .as_deref()
                .and_then(|id| musicbrainz_url("artist", id));
        }
        details = track
            .musicbrainz_release_track_id
            .as_deref()
            .and_then(|id| musicbrainz_url("track", id))
            .or_else(|| {
                track
                    .musicbrainz_recording_id
                    .as_deref()
                    .and_then(|id| musicbrainz_url("recording", id))
            })
            .or(details);
    }
    (details, state)
}

fn lastfm_artist_url(artist: &str) -> Option<String> {
    let artist = artist.trim();
    (!artist.is_empty()).then(|| format!("https://www.last.fm/music/{}", encode_segment(artist)))
}

fn lastfm_track_url(artist: &str, album: &str, title: &str) -> Option<String> {
    let artist = artist.trim();
    let title = title.trim();
    if artist.is_empty() || title.is_empty() {
        return None;
    }
    let album = if album.trim().is_empty() { "_" } else { album };
    let url = format!(
        "https://www.last.fm/music/{}/{}/{}",
        encode_segment(artist),
        encode_segment(album),
        encode_segment(title)
    );
    (url.len() <= MAX_URL_LENGTH).then_some(url)
}

fn musicbrainz_url(entity: &str, id: &str) -> Option<String> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    Some(format!(
        "https://musicbrainz.org/{entity}/{}",
        encode_segment(id)
    ))
}

fn discord_text(value: &str, fallback: &str) -> String {
    let text = if value.trim().is_empty() {
        fallback
    } else {
        value.trim()
    };
    let mut text = text.chars().take(MAX_TEXT_LENGTH).collect::<String>();
    if text.chars().count() < 2 {
        text.push(' ');
    }
    text
}

fn encode_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(*byte));
            }
            b' ' => encoded.push_str("%20"),
            byte => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}
