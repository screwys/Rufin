use rufin_core::SettingsHandle;

use crate::host::{AndroidError, error};

#[derive(uniffi::Record)]
pub struct AndroidPlaybackUpdate {
    pub transport: playback::TransportView,
    pub controls: playback::ControlsView,
    pub queue: playback::QueueSummaryView,
    pub queue_loading: bool,
    pub notices: Vec<playback::PlaybackNotice>,
    pub favorite: bool,
    pub artwork_revision: u64,
    pub source_labels: Vec<String>,
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
    current_facts: Option<(String, bool)>,
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
    pub async fn next(&self) -> Result<AndroidPlaybackUpdate, AndroidError> {
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
        let mut favorite = false;
        if let Some(current) = view
            .as_ref()
            .and_then(|view| view.transport.current.as_ref())
        {
            let uri = &current.media_uri;
            if changes.current_facts.is_none() {
                let database = self.database.clone();
                let target = library::FavoriteTarget::Track(uri.clone());
                let favorite = self
                    .runtime
                    .spawn(async move { database.favorite(&target).await })
                    .await
                    .map_err(error)?
                    .map_err(error)?;
                changes.current_facts = Some((uri.clone(), favorite));
            }
            favorite = changes
                .current_facts
                .as_ref()
                .is_some_and(|(_, value)| *value);
            favorite = self
                .intents
                .projected_item_favorite(&library::FavoriteTarget::Track(uri.clone()), favorite);
        }
        let (transport, controls, queue, queue_loading) = view.as_ref().map_or_else(
            || {
                let settings = self.settings.load();
                (
                    playback::TransportView {
                        playback_rate: settings.playback.playback_rate,
                        ..Default::default()
                    },
                    playback::ControlsView {
                        repeat_mode: settings.repeat_mode,
                        shuffle_enabled: settings.shuffle_enabled,
                        auto_dj_enabled: settings.auto_dj_enabled,
                        volume: settings.playback.volume,
                        ..Default::default()
                    },
                    playback::QueueSummaryView::default(),
                    false,
                )
            },
            |view| {
                (
                    view.transport.clone(),
                    view.controls.clone(),
                    view.queue.clone(),
                    view.queue_loading,
                )
            },
        );
        let output_name = match &controls.playback_output {
            playback::PlaybackOutput::Remote(output) => Some(output.name.as_str()),
            _ => None,
        };
        let source_labels = rufin_core::settings::presentation::now_playing_labels(
            transport.current.as_ref(),
            output_name,
        );
        Ok(AndroidPlaybackUpdate {
            transport,
            controls,
            queue,
            queue_loading,
            notices,
            favorite,
            artwork_revision: self.artwork.cache_revision(),
            source_labels,
        })
    }
}
