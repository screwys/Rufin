use std::sync::Arc;

use library::{AlbumKey, ArtistKey, QueueMedia, SourceKey, TrackArtistLink, TrackKey, TrackRow};

use crate::sequence::{OccurrenceId, RepeatMode, Sequence};
use crate::{PlaybackSession, Provenance, RunId, SourceSessionEpoch, TransportStatus};

/// The bounded current or prepared-next media facts Playback consumes.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackMedia {
    pub source_id: String,
    pub track_key: Option<TrackKey>,
    pub track_object_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_display_artist: Option<String>,
    pub album_key: Option<AlbumKey>,
    pub primary_artist_key: Option<ArtistKey>,
    pub media_uri: Option<String>,
    pub artwork_binding: Option<Vec<u8>>,
    pub duration_millis: i64,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub favorite: Option<bool>,
    pub rating: Option<i64>,
    pub is_downloaded: bool,
    pub source_format: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub musicbrainz_album_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub primary_artist_musicbrainz_id: Option<String>,
    pub cue_path: Option<String>,
    pub cue_start_millis: Option<i64>,
    pub cue_end_millis: Option<i64>,
    pub artist_links: Vec<TrackArtistLink>,
}

impl From<QueueMedia> for PlaybackMedia {
    fn from(media: QueueMedia) -> Self {
        Self {
            source_id: media.source_id,
            track_key: media.track_key,
            track_object_id: media.track_object_id,
            title: media.title,
            artist: media.artist,
            album: media.album,
            album_display_artist: media.album_display_artist,
            album_key: media.album_key,
            primary_artist_key: media.primary_artist_key,
            media_uri: media.media_uri,
            artwork_binding: media.artwork_binding,
            duration_millis: media.duration_millis.unwrap_or_default(),
            disc_number: media.disc_number,
            track_number: media.track_number,
            year: media.year,
            release_date: media.release_date,
            favorite: media.favorite,
            rating: None,
            is_downloaded: false,
            source_format: media.source_format,
            musicbrainz_recording_id: media.musicbrainz_recording_id,
            musicbrainz_release_track_id: media.musicbrainz_release_track_id,
            musicbrainz_album_id: media.musicbrainz_album_id,
            musicbrainz_release_group_id: media.musicbrainz_release_group_id,
            primary_artist_musicbrainz_id: media.primary_artist_musicbrainz_id,
            cue_path: media.cue_path,
            cue_start_millis: media.cue_start_millis,
            cue_end_millis: media.cue_end_millis,
            artist_links: media.artist_links,
        }
    }
}

impl From<TrackRow> for PlaybackMedia {
    fn from(track: TrackRow) -> Self {
        let primary_artist_key = track
            .artists
            .first()
            .or_else(|| track.album_artists.first())
            .map(|artist| artist.artist_key);
        let album_display_artist = track
            .album_artists
            .first()
            .map(|artist| artist.name.clone());
        Self {
            source_id: track.source_id.clone(),
            track_key: Some(track.track_key),
            track_object_id: track.object_id,
            title: track.title,
            artist: track.display_artist,
            album: track.display_album,
            album_display_artist,
            album_key: track.album_key,
            primary_artist_key,
            media_uri: track.media_uri,
            artwork_binding: track.artwork_binding,
            duration_millis: track.duration_millis,
            disc_number: Some(track.disc_number),
            track_number: Some(track.track_number),
            year: track.year,
            release_date: track.release_date,
            favorite: Some(track.favorite),
            rating: track.rating,
            is_downloaded: track.is_downloaded,
            source_format: track.source_format,
            musicbrainz_recording_id: track.musicbrainz_recording_id,
            musicbrainz_release_track_id: track.musicbrainz_release_track_id,
            musicbrainz_album_id: None,
            musicbrainz_release_group_id: None,
            primary_artist_musicbrainz_id: None,
            cue_path: track.cue_path,
            cue_start_millis: track.cue_start_millis,
            cue_end_millis: track.cue_end_millis,
            artist_links: if track.artists.is_empty() {
                track.album_artists
            } else {
                track.artists
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueSummaryView {
    pub revision: u64,
    pub total: usize,
    pub current_occurrence: Option<OccurrenceId>,
    pub current_index: Option<usize>,
    pub current_position: Option<usize>,
    pub next_occurrence: Option<OccurrenceId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurrentMedia {
    pub id: CurrentMediaId,
    pub track: PlaybackMedia,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CurrentMediaId {
    pub source_key: SourceKey,
    pub source_session_epoch: SourceSessionEpoch,
    pub run: Option<RunId>,
    pub occurrence: OccurrenceId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransportView {
    pub source_id: SourceKey,
    pub current: Option<Arc<CurrentMedia>>,
    pub state: TransportStatus,
    pub desired_playing: bool,
    pub position_millis: u64,
    pub duration_millis: u64,
    pub can_seek: bool,
    pub buffering_percent: Option<u8>,
    pub error: Option<String>,
}

impl TransportView {
    pub fn effective_state(&self) -> TransportStatus {
        effective_transport_state(self.state, self.desired_playing)
    }
}

fn effective_transport_state(state: TransportStatus, desired_playing: bool) -> TransportStatus {
    match state {
        TransportStatus::Stopped | TransportStatus::Failed => state,
        _ if !desired_playing => TransportStatus::Paused,
        TransportStatus::Paused => TransportStatus::Buffering,
        state => state,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ControlsView {
    pub repeat_mode: RepeatMode,
    pub shuffle_enabled: bool,
    pub auto_dj_enabled: bool,
    pub volume: f64,
    pub muted: bool,
    pub audio_output: Option<String>,
    pub playback_output: PlaybackOutput,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RemoteOutputProtocol {
    Upnp,
    GoogleCast,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RemoteOutput {
    pub id: String,
    pub name: String,
    pub protocol: RemoteOutputProtocol,
}

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub enum PlaybackOutput {
    #[default]
    Local,
    Remote(RemoteOutput),
}

impl PlaybackOutput {
    pub const fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackView {
    pub queue: QueueSummaryView,
    pub transport: TransportView,
    pub controls: ControlsView,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlaybackNotice {
    RunStarted(RunId),
    PositionDiscontinuity(crate::PositionDiscontinuity),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackProjection {
    pub view: PlaybackView,
    pub notices: Vec<PlaybackNotice>,
}

impl Sequence {
    pub fn summary(&self) -> QueueSummaryView {
        let next_index = self.next_index_eos();
        QueueSummaryView {
            revision: self.revision(),
            total: self.entries().len(),
            current_occurrence: self.selected().map(|entry| entry.occurrence.clone()),
            current_index: self.selected_index(),
            current_position: self.selected().map(|entry| entry.canonical_position),
            next_occurrence: next_index
                .and_then(|index| self.entries().get(index))
                .map(|entry| entry.occurrence.clone()),
        }
    }
}

impl PlaybackSession {
    pub fn view(&self) -> PlaybackView {
        let sequence = self.sequence();
        let settings = self.settings();
        PlaybackView {
            queue: sequence.summary(),
            transport: TransportView {
                source_id: sequence.source_key(),
                current: sequence.selected().and_then(|entry| {
                    let (occurrence, media) = self.current_media_fact()?;
                    if occurrence != &entry.occurrence {
                        return None;
                    }
                    Arc::new(CurrentMedia {
                        id: CurrentMediaId {
                            source_key: sequence.source_key(),
                            source_session_epoch: self.source_session_epoch(),
                            run: self.current_run(),
                            occurrence: entry.occurrence.clone(),
                        },
                        track: media.clone(),
                        provenance: entry.provenance.clone(),
                    })
                    .into()
                }),
                state: self.status(),
                desired_playing: self.desired_playing(),
                position_millis: self.position_millis(),
                duration_millis: self.duration_millis(),
                can_seek: self.can_seek(),
                buffering_percent: self.buffering_percent(),
                error: self.last_error().map(str::to_string),
            },
            controls: ControlsView {
                repeat_mode: sequence.repeat_mode(),
                shuffle_enabled: sequence.shuffle_enabled(),
                auto_dj_enabled: self.auto_dj_enabled(),
                volume: if self.output_muted() {
                    0.0
                } else {
                    self.output_volume()
                },
                muted: self.output_muted(),
                audio_output: settings.audio_output.clone(),
                playback_output: self.playback_output().clone(),
            },
        }
    }
}
