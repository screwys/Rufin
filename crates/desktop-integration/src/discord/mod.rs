mod cover;
mod ipc;

use std::sync::{
    Arc, Mutex, Weak,
    mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use playback::{PlaybackView, TransportStatus};
use tracing::debug;

pub use ipc::{DEFAULT_CLIENT_ID, DisplayType, LinkType, Settings};

pub(crate) struct LatestSender<T> {
    value: Arc<Mutex<Option<T>>>,
    wake: SyncSender<()>,
}

impl<T> LatestSender<T> {
    fn publish(&self, value: T) {
        let Ok(mut latest) = self.value.lock() else {
            return;
        };
        *latest = Some(value);
        drop(latest);
        if let Err(TrySendError::Disconnected(())) = self.wake.try_send(()) {
            self.clear();
        }
    }

    fn clear(&self) {
        if let Ok(mut latest) = self.value.lock() {
            *latest = None;
        }
    }
}

pub(crate) struct LatestReceiver<T> {
    value: Arc<Mutex<Option<T>>>,
    wake: Receiver<()>,
}

impl<T> LatestReceiver<T> {
    fn recv(&self) -> Option<T> {
        loop {
            self.wake.recv().ok()?;
            if let Some(value) = self.take() {
                return Some(value);
            }
        }
    }

    fn recv_timeout(&self, delay: Duration) -> Result<T, RecvTimeoutError> {
        loop {
            self.wake.recv_timeout(delay)?;
            if let Some(value) = self.take() {
                return Ok(value);
            }
        }
    }

    fn take(&self) -> Option<T> {
        self.value.lock().ok()?.take()
    }
}

fn latest_slot<T>() -> (LatestSender<T>, LatestReceiver<T>) {
    let value = Arc::new(Mutex::new(None));
    let (wake, receiver) = sync_channel(1);
    (
        LatestSender {
            value: Arc::clone(&value),
            wake,
        },
        LatestReceiver {
            value,
            wake: receiver,
        },
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ArtworkKey {
    album: metadata_lookup::AlbumCover,
    policy: metadata_lookup::AlbumCoverPolicy,
}

pub(crate) struct ArtworkRequest {
    revision: u64,
    key: ArtworkKey,
    queued_at: Instant,
    owner: Weak<Inner>,
}

impl ArtworkRequest {
    pub fn queued_for(&self) -> Duration {
        self.queued_at.elapsed()
    }

    pub fn complete(self, result: Result<Option<String>, String>) {
        if let Some(owner) = self.owner.upgrade() {
            owner.complete_artwork(self.revision, &self.key, result);
        }
    }
}

pub(crate) struct ArtworkRequests {
    receiver: LatestReceiver<ArtworkRequest>,
}

impl ArtworkRequests {
    pub fn recv(&self) -> Option<ArtworkRequest> {
        self.receiver.recv()
    }
}

#[derive(Clone)]
struct Presence {
    inner: Arc<Inner>,
}

#[derive(Clone)]
pub struct Discord {
    presence: Presence,
}

struct Inner {
    state: Mutex<State>,
    artwork: LatestSender<ArtworkRequest>,
}

#[derive(Default)]
struct State {
    settings: Settings,
    lastfm_api_key: String,
    activity: Option<Arc<ipc::Activity>>,
    artwork: ArtworkState,
    next_artwork_revision: u64,
    worker: Option<ipc::Worker>,
}

#[derive(Default)]
enum ArtworkState {
    #[default]
    Empty,
    Pending {
        revision: u64,
        key: ArtworkKey,
    },
    Ready {
        key: ArtworkKey,
        url: Option<String>,
    },
}

impl Presence {
    fn new() -> (Self, ArtworkRequests) {
        let (artwork, receiver) = latest_slot();
        let inner = Arc::new(Inner {
            state: Mutex::new(State::default()),
            artwork,
        });
        (Self { inner }, ArtworkRequests { receiver })
    }

    pub fn update(
        &self,
        mut settings: Settings,
        delivery_enabled: bool,
        lastfm_api_key: &str,
        view: Option<&PlaybackView>,
    ) {
        settings.enabled &= delivery_enabled && ipc::SUPPORTED;
        let Ok(mut state) = self.inner.state.lock() else {
            return;
        };
        state.settings = settings;
        state.lastfm_api_key = lastfm_api_key.to_string();
        self.refresh(&mut state, view, unix_now_millis());
    }

    pub fn observe(&self, view: Option<&PlaybackView>, position_discontinuity: bool) {
        let Ok(mut state) = self.inner.state.lock() else {
            return;
        };
        if !position_discontinuity && state.matches(view) {
            return;
        }
        self.refresh(&mut state, view, unix_now_millis());
    }

    fn refresh(&self, state: &mut State, view: Option<&PlaybackView>, now_millis: u64) {
        let Some(view) = view else {
            self.clear(state);
            return;
        };
        if state.settings.enabled
            && matches!(
                view.transport.state,
                TransportStatus::Resolving | TransportStatus::Buffering
            )
        {
            if view.transport.state == TransportStatus::Resolving
                && matches!(
                    &state.artwork,
                    ArtworkState::Pending { key, .. }
                        if Some(key) != ArtworkKey::from_view(
                            view,
                            &state.lastfm_api_key,
                            state.settings.link_type,
                        ).as_ref()
                )
            {
                state.artwork = ArtworkState::Empty;
                self.inner.artwork.clear();
            }
            return;
        }
        let Some(mut activity) = ipc::Activity::new(
            &state.settings,
            view,
            now_millis,
            ipc::APP_ICON_URL.to_string(),
        ) else {
            self.clear(state);
            return;
        };
        activity.large_image = self.artwork_image(state, view);
        let activity = Arc::new(activity);
        state.activity = Some(Arc::clone(&activity));
        if !matches!(state.artwork, ArtworkState::Pending { .. }) {
            state.publish(Some(activity));
        }
    }

    fn clear(&self, state: &mut State) {
        if matches!(state.artwork, ArtworkState::Pending { .. }) {
            state.artwork = ArtworkState::Empty;
        }
        self.inner.artwork.clear();
        if state.activity.take().is_some() {
            state.publish(None);
        }
    }

    fn artwork_image(&self, state: &mut State, view: &PlaybackView) -> String {
        let Some(key) =
            ArtworkKey::from_view(view, &state.lastfm_api_key, state.settings.link_type)
        else {
            state.artwork = ArtworkState::Empty;
            self.inner.artwork.clear();
            return ipc::APP_ICON_URL.to_string();
        };
        match &state.artwork {
            ArtworkState::Pending { key: pending, .. } if pending == &key => {
                return state
                    .activity
                    .as_ref()
                    .map(|activity| activity.large_image.clone())
                    .unwrap_or_else(|| ipc::APP_ICON_URL.to_string());
            }
            ArtworkState::Ready { key: ready, url } if ready == &key => {
                return url.clone().unwrap_or_else(|| ipc::APP_ICON_URL.to_string());
            }
            ArtworkState::Empty | ArtworkState::Pending { .. } | ArtworkState::Ready { .. } => {}
        }

        state.next_artwork_revision = state.next_artwork_revision.wrapping_add(1);
        let revision = state.next_artwork_revision;
        state.artwork = ArtworkState::Pending {
            revision,
            key: key.clone(),
        };
        self.inner.artwork.publish(ArtworkRequest {
            revision,
            key,
            queued_at: Instant::now(),
            owner: Arc::downgrade(&self.inner),
        });
        ipc::APP_ICON_URL.to_string()
    }
}

impl Discord {
    pub fn new() -> Self {
        let (presence, requests) = Presence::new();
        cover::start(requests);
        Self { presence }
    }

    pub fn update(
        &self,
        settings: Settings,
        delivery_enabled: bool,
        lastfm_api_key: &str,
        view: Option<&PlaybackView>,
    ) {
        self.presence
            .update(settings, delivery_enabled, lastfm_api_key, view);
    }

    pub fn observe(&self, view: Option<&PlaybackView>, position_discontinuity: bool) {
        self.presence.observe(view, position_discontinuity);
    }
}

impl State {
    fn matches(&self, view: Option<&PlaybackView>) -> bool {
        match (&self.activity, view) {
            (None, None) => true,
            (Some(_), None) => false,
            (None, Some(view)) => {
                ipc::visible_playback_state(&self.settings, view.transport.state).is_none()
                    || view
                        .transport
                        .current
                        .as_ref()
                        .and_then(|media| media.id.run)
                        .is_none()
                    || view.transport.current.is_none()
            }
            (Some(activity), Some(view)) => activity.matches(view),
        }
    }

    fn publish(&mut self, activity: Option<Arc<ipc::Activity>>) {
        match activity {
            Some(activity) => self
                .worker
                .get_or_insert_with(ipc::Worker::new)
                .publish(Some(activity)),
            None => {
                if let Some(worker) = &self.worker {
                    worker.publish(None);
                }
            }
        }
    }

    fn complete_artwork(
        &mut self,
        revision: u64,
        key: &ArtworkKey,
        result: Result<Option<String>, String>,
    ) -> Option<Arc<ipc::Activity>> {
        if !matches!(
            &self.artwork,
            ArtworkState::Pending { revision: pending, key: pending_key }
                if *pending == revision && pending_key == key
        ) {
            return None;
        }
        let url = match result {
            Ok(url) => url,
            Err(error) => {
                debug!(%error, "rich-presence artwork lookup failed");
                None
            }
        };
        self.artwork = ArtworkState::Ready {
            key: key.clone(),
            url: url.clone(),
        };
        let image = url.unwrap_or_else(|| ipc::APP_ICON_URL.to_string());
        let activity = self.activity.as_mut()?;
        Arc::make_mut(activity).large_image = image;
        Some(Arc::clone(activity))
    }
}

impl ArtworkKey {
    fn from_view(view: &PlaybackView, lastfm_api_key: &str, link_type: LinkType) -> Option<Self> {
        if link_type == LinkType::None {
            return None;
        }
        let track = &view.transport.current.as_ref()?.track;
        let musicbrainz_album_id = track.musicbrainz_album_id.clone();
        let musicbrainz_release_group_id = track.musicbrainz_release_group_id.clone();
        let lastfm_api_key = if matches!(link_type, LinkType::LastFm | LinkType::MusicBrainzLastFm)
        {
            lastfm_api_key
        } else {
            ""
        };
        let allow_musicbrainz = matches!(
            link_type,
            LinkType::MusicBrainz | LinkType::MusicBrainzLastFm
        );
        let album_artist = track
            .album_display_artist
            .as_deref()
            .filter(|artist| !artist.trim().is_empty())
            .unwrap_or(&track.artist);
        Some(Self {
            album: metadata_lookup::AlbumCover::new(
                album_artist,
                &track.album,
                musicbrainz_release_group_id.as_deref(),
                musicbrainz_album_id.as_deref(),
            )?,
            policy: metadata_lookup::AlbumCoverPolicy::new(lastfm_api_key, allow_musicbrainz),
        })
    }
}

impl Inner {
    fn complete_artwork(
        &self,
        revision: u64,
        key: &ArtworkKey,
        result: Result<Option<String>, String>,
    ) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if let Some(activity) = state.complete_artwork(revision, key, result) {
            state.publish(Some(activity));
        }
    }
}

fn unix_now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use library::SourceKey;
    use playback::{
        ControlsView, CurrentMedia, CurrentMediaId, OccurrenceId, PlaybackMedia, PlaybackView,
        Provenance, QueueSummaryView, RepeatMode, RunId, SourceSessionEpoch, TransportStatus,
        TransportView,
    };

    pub(crate) fn test_view(
        run: u64,
        album: &str,
        state: TransportStatus,
        position_millis: u64,
    ) -> PlaybackView {
        let occurrence = OccurrenceId::new(format!("presence:{run}"));
        let source = SourceKey::from_raw(1);
        let current = Arc::new(CurrentMedia {
            id: CurrentMediaId {
                source_key: source,
                source_session_epoch: SourceSessionEpoch::new(1),
                run: Some(RunId::new(run)),
                occurrence: occurrence.clone(),
            },
            track: PlaybackMedia {
                source_id: "source".to_string(),
                track_key: Some(library::TrackKey::from_raw(1)),
                track_object_id: "track".to_string(),
                title: "Track".to_string(),
                artist: "Artist".to_string(),
                album: album.to_string(),
                album_display_artist: Some("Album Artist".to_string()),
                album_key: None,
                primary_artist_key: None,
                media_uri: None,
                artwork_binding: None,
                duration_millis: 42_500,
                disc_number: Some(1),
                track_number: Some(1),
                year: Some(2026),
                release_date: None,
                favorite: Some(false),
                rating: None,
                is_downloaded: false,
                source_format: None,
                musicbrainz_recording_id: Some("recording-id".to_string()),
                musicbrainz_release_track_id: Some("track-id".to_string()),
                musicbrainz_album_id: None,
                musicbrainz_release_group_id: None,
                primary_artist_musicbrainz_id: Some("artist-id".to_string()),
                cue_path: None,
                cue_start_millis: None,
                cue_end_millis: None,
                artist_links: Vec::new(),
            },
            provenance: Provenance::Manual,
        });
        PlaybackView {
            queue: QueueSummaryView {
                revision: run,
                total: 1,
                current_occurrence: Some(occurrence),
                current_index: Some(0),
                current_position: Some(0),
                next_occurrence: None,
            },
            transport: TransportView {
                source_id: source,
                current: Some(current),
                state,
                desired_playing: matches!(
                    state,
                    TransportStatus::Resolving
                        | TransportStatus::Buffering
                        | TransportStatus::Playing
                ),
                position_millis,
                duration_millis: 42_500,
                can_seek: true,
                buffering_percent: None,
                error: None,
            },
            controls: ControlsView {
                repeat_mode: RepeatMode::Off,
                shuffle_enabled: false,
                auto_dj_enabled: false,
                volume: 1.0,
                muted: false,
                audio_output: None,
                playback_output: playback::PlaybackOutput::Local,
            },
        }
    }
}
