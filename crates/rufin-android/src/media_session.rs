use playback::{PlaybackView, RepeatMode, TransportStatus};
use rufin_core::SettingsHandle;

use crate::host::{AndroidError, error};

#[derive(Clone, Copy, uniffi::Enum)]
pub enum AndroidRepeatMode {
    Off,
    One,
    All,
}

impl From<RepeatMode> for AndroidRepeatMode {
    fn from(value: RepeatMode) -> Self {
        match value {
            RepeatMode::Off => Self::Off,
            RepeatMode::One => Self::One,
            RepeatMode::All => Self::All,
        }
    }
}

impl From<AndroidRepeatMode> for RepeatMode {
    fn from(value: AndroidRepeatMode) -> Self {
        match value {
            AndroidRepeatMode::Off => Self::Off,
            AndroidRepeatMode::One => Self::One,
            AndroidRepeatMode::All => Self::All,
        }
    }
}

#[derive(Clone, Copy, uniffi::Enum)]
pub enum AndroidTransportState {
    Stopped,
    Buffering,
    Playing,
    Paused,
    Failed,
}

#[derive(uniffi::Record)]
pub struct AndroidPlaybackState {
    pub operation_failures: Vec<String>,
    pub media_uri: Option<String>,
    pub media_run: Option<u64>,
    pub occurrence_id: Option<String>,
    pub artwork_identity: Option<Vec<u8>>,
    pub artwork_revision: u64,
    pub favorite: bool,
    pub queue_revision: u64,
    pub queue_window_count: u64,
    pub queue_window_revision: u64,
    pub queue_total: u64,
    pub queue_index: Option<u64>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub context_id: Option<String>,
    pub context_title: Option<String>,
    pub context_category: Option<String>,
    pub context_kind: String,
    pub source_format: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub bit_depth: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub position_millis: u64,
    pub position_observed_at_millis: u64,
    pub position_advancing: bool,
    pub playback_rate: f64,
    pub duration_millis: u64,
    pub state: AndroidTransportState,
    pub desired_playing: bool,
    pub can_seek: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub shuffle: bool,
    pub auto_dj: bool,
    pub repeat: AndroidRepeatMode,
    pub volume: f64,
    pub local_output: bool,
    pub output_kind: crate::player::AndroidOutputKind,
    pub output_id: String,
    pub output_name: Option<String>,
    pub error: Option<String>,
}

impl AndroidPlaybackState {
    fn idle(settings: &rufin_core::settings::Settings) -> Self {
        Self {
            operation_failures: Vec::new(),
            media_uri: None,
            media_run: None,
            occurrence_id: None,
            artwork_identity: None,
            artwork_revision: 0,
            favorite: false,
            queue_revision: 0,
            queue_window_count: 0,
            queue_window_revision: 0,
            queue_total: 0,
            queue_index: None,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            context_id: None,
            context_title: None,
            context_category: None,
            context_kind: String::new(),
            source_format: None,
            bitrate_kbps: None,
            bit_depth: None,
            sample_rate_hz: None,
            position_millis: 0,
            position_observed_at_millis: 0,
            position_advancing: false,
            playback_rate: settings.playback.playback_rate,
            duration_millis: 0,
            state: AndroidTransportState::Stopped,
            desired_playing: false,
            can_seek: false,
            can_next: false,
            can_previous: false,
            shuffle: settings.shuffle_enabled,
            auto_dj: settings.auto_dj_enabled,
            repeat: settings.repeat_mode.into(),
            volume: settings.playback.volume,
            local_output: true,
            output_kind: crate::player::AndroidOutputKind::Local,
            output_id: String::new(),
            output_name: None,
            error: None,
        }
    }
}

impl From<&PlaybackView> for AndroidPlaybackState {
    fn from(view: &PlaybackView) -> Self {
        let transport = &view.transport;
        let (output_kind, output_id, output_name) = match &view.controls.playback_output {
            playback::PlaybackOutput::Local => {
                (crate::player::AndroidOutputKind::Local, String::new(), None)
            }
            playback::PlaybackOutput::Remote(output) => (
                output.protocol.into(),
                output.id.clone(),
                Some(output.name.clone()),
            ),
        };
        let current = transport.current.as_deref();
        Self {
            operation_failures: Vec::new(),
            media_uri: current.map(|media| media.media_uri.clone()),
            media_run: current.and_then(|media| media.id.run.map(playback::RunId::get)),
            occurrence_id: current.map(|media| media.occurrence.occurrence.to_string()),
            artwork_identity: current.and_then(|media| media.artwork_binding.clone()),
            artwork_revision: 0,
            favorite: false,
            queue_revision: view.queue.revision,
            queue_window_count: view.queue_window.len() as u64,
            queue_window_revision: 0,
            queue_total: view.queue.total as u64,
            queue_index: view.queue.current_index.map(|index| index as u64),
            title: current.map_or_else(String::new, |media| media.title.clone()),
            artist: current.map_or_else(String::new, |media| media.artist.clone()),
            album: current.map_or_else(String::new, |media| media.album.clone()),
            context_id: current.and_then(|media| match &media.provenance {
                library::QueueProvenance::Context { context_id, .. } => {
                    Some(context_id.to_string())
                }
                _ => None,
            }),
            context_title: current.and_then(|media| match &media.provenance {
                library::QueueProvenance::Context { context_title, .. } => context_title
                    .as_ref()
                    .map(|caption| caption.title.to_string()),
                _ => None,
            }),
            context_category: current.and_then(|media| match &media.provenance {
                library::QueueProvenance::Context { context_title, .. } => context_title
                    .as_ref()
                    .and_then(|caption| caption.kind.as_deref().map(str::to_owned)),
                _ => None,
            }),
            context_kind: current.map_or_else(String::new, |media| {
                match &media.provenance {
                    library::QueueProvenance::Context { .. } => "context",
                    library::QueueProvenance::Manual => "manual",
                    library::QueueProvenance::Random => "random",
                    library::QueueProvenance::Radio => "radio",
                    library::QueueProvenance::AutoDj => "auto_dj",
                    library::QueueProvenance::Legacy => "legacy",
                }
                .into()
            }),
            source_format: current.and_then(|media| {
                rufin_core::settings::presentation::audio_source_label(
                    media.source_format.as_deref(),
                    Some(&media.media_uri),
                )
            }),
            bitrate_kbps: None,
            bit_depth: None,
            sample_rate_hz: None,
            position_millis: transport.position_millis,
            position_observed_at_millis: transport.position_observed_at_millis,
            position_advancing: transport.state == TransportStatus::Playing,
            playback_rate: transport.playback_rate,
            duration_millis: transport.duration_millis,
            state: match transport.effective_state() {
                TransportStatus::Stopped => AndroidTransportState::Stopped,
                TransportStatus::Resolving | TransportStatus::Buffering => {
                    AndroidTransportState::Buffering
                }
                TransportStatus::Playing => AndroidTransportState::Playing,
                TransportStatus::Paused => AndroidTransportState::Paused,
                TransportStatus::Failed => AndroidTransportState::Failed,
            },
            desired_playing: transport.desired_playing,
            can_seek: transport.can_seek,
            can_next: view.queue.can_next,
            can_previous: current.is_some(),
            shuffle: view.controls.shuffle_enabled,
            auto_dj: view.controls.auto_dj_enabled,
            repeat: view.controls.repeat_mode.into(),
            volume: view.controls.volume,
            local_output: view.controls.playback_output.is_local(),
            output_kind,
            output_id,
            output_name,
            error: transport.error.clone(),
        }
    }
}

#[derive(uniffi::Object)]
pub struct AndroidPlaybackSubscription {
    subscription: tokio::sync::Mutex<PlaybackChanges>,
    settings: SettingsHandle,
    database: std::sync::Arc<library::Database>,
    runtime: tokio::runtime::Handle,
    updates: playback::PlaybackUpdates,
    intents: std::sync::Arc<rufin_core::favorites::FavoriteIntents>,
    artwork: artwork::Artwork,
}

struct PlaybackChanges {
    playback: playback::PlaybackSubscription,
    catalog: tokio::sync::watch::Receiver<()>,
    intents: tokio::sync::watch::Receiver<u64>,
    current_facts: Option<(String, bool, library::AudioProperties, Option<String>)>,
    queue_view: Option<std::sync::Arc<PlaybackView>>,
    queue_window_revision: u64,
}

impl AndroidPlaybackSubscription {
    pub(crate) fn new(
        products: &rufin_core::runtime::ProductHandles,
        intents: std::sync::Arc<rufin_core::favorites::FavoriteIntents>,
    ) -> Self {
        Self {
            subscription: tokio::sync::Mutex::new(PlaybackChanges {
                playback: products.playback.updates.subscribe(),
                catalog: products.source.catalog_changes(),
                intents: intents.subscribe(),
                current_facts: None,
                queue_view: None,
                queue_window_revision: 0,
            }),
            settings: products.settings.clone(),
            database: products.library.clone(),
            runtime: products.runtime.clone(),
            updates: products.playback.updates.clone(),
            artwork: products.artwork.clone(),
            intents,
        }
    }
}

#[uniffi::export]
impl AndroidPlaybackSubscription {
    pub async fn next(&self) -> Result<AndroidPlaybackState, AndroidError> {
        let mut changes = self.subscription.lock().await;
        let PlaybackChanges {
            playback,
            catalog,
            intents,
            ..
        } = &mut *changes;
        let (view, catalog_received, notices) = tokio::select! {
            projection = playback.recv() => {
                let projection = projection.map_err(error)?;
                let (view, notices) = projection.map_or((None, Vec::new()), |p| (Some(std::sync::Arc::new(p.view)), p.notices));
                (view, false, notices)
            },
            changed = catalog.changed() => { changed.map_err(error)?; (self.updates.current(), true, Vec::new()) },
            changed = intents.changed() => { changed.map_err(error)?; (self.updates.current(), false, Vec::new()) },
        };
        let catalog_changed = catalog_received || changes.catalog.has_changed().map_err(error)?;
        let cached_uri = changes.current_facts.as_ref().map(|(uri, ..)| uri.as_str());
        let current_uri = view
            .as_ref()
            .and_then(|v| v.transport.current.as_ref())
            .map(|m| m.media_uri.as_str());
        if catalog_changed || cached_uri != current_uri {
            changes.current_facts = None;
        }
        changes.catalog.borrow_and_update();
        if catalog_changed
            || changes
                .queue_view
                .as_ref()
                .map(|previous| &previous.queue_window)
                != view.as_ref().map(|current| &current.queue_window)
        {
            changes.queue_window_revision += 1;
        }
        changes.queue_view = view.clone();
        let mut state = view.as_ref().map_or_else(
            || AndroidPlaybackState::idle(&self.settings.load()),
            |view| AndroidPlaybackState::from(view.as_ref()),
        );
        state.queue_window_revision = changes.queue_window_revision;
        state.artwork_revision = self.artwork.cache_revision();
        state.operation_failures = notices
            .into_iter()
            .filter_map(|notice| match notice {
                playback::PlaybackNotice::OperationFailed(message) => Some(message),
                _ => None,
            })
            .collect();
        if let Some(current) = view
            .as_ref()
            .and_then(|view| view.transport.current.as_ref())
        {
            let uri = &current.media_uri;
            if changes.current_facts.is_none() {
                let database = self.database.clone();
                let occurrence = std::sync::Arc::clone(&current.occurrence);
                let row = self
                    .runtime
                    .spawn(async move { database.prepared_queue_page(&[occurrence]).await })
                    .await
                    .map_err(error)?
                    .map_err(error)?
                    .pop()
                    .expect("Current queue row");
                tracing::info!(
                    favorite = row.favorite,
                    run = current.id.run.map(playback::RunId::get),
                    catalog_changed,
                    "Android current track favorite loaded"
                );
                let format = rufin_core::settings::presentation::audio_source_label(
                    current.source_format.as_deref(),
                    row.source_path.as_deref().or(Some(uri)),
                );
                changes.current_facts =
                    Some((uri.clone(), row.favorite, row.audio_properties, format));
            }
            state.favorite = changes
                .current_facts
                .as_ref()
                .is_some_and(|(_, value, _, _)| *value);
            state.bitrate_kbps = changes
                .current_facts
                .as_ref()
                .and_then(|(_, _, value, _)| value.bitrate_kbps);
            state.bit_depth = changes
                .current_facts
                .as_ref()
                .and_then(|(_, _, value, _)| value.bit_depth);
            state.sample_rate_hz = changes
                .current_facts
                .as_ref()
                .and_then(|(_, _, value, _)| value.sample_rate_hz);
            state.source_format = changes
                .current_facts
                .as_ref()
                .and_then(|(_, _, _, value)| value.clone());
            state.favorite = self.intents.projected_item_favorite(
                &library::FavoriteTarget::Track(uri.clone()),
                state.favorite,
            );
        }
        Ok(state)
    }
}
