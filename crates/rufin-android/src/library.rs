use std::sync::Arc;

use rufin_core::runtime::{ProductHandles, SourceHandle, source::SourceOperation};

use crate::host::{AndroidError, error};

#[derive(uniffi::Object)]
pub struct AndroidLibrary {
    pub(crate) source: SourceHandle,
    pub(crate) queue: playback::QueueHandle,
    pub(crate) radio: playback::RadioHandle,
    pub(crate) settings: rufin_core::SettingsHandle,
    pub(crate) database: Arc<library::Database>,
    pub(crate) runtime: tokio::runtime::Handle,
    playback_updates: playback::PlaybackUpdates,
}

impl AndroidLibrary {
    pub(crate) fn new(products: &ProductHandles) -> Self {
        Self {
            source: products.source.clone(),
            queue: products.playback.queue.clone(),
            radio: products.playback.radio.clone(),
            settings: products.settings.clone(),
            database: products.library.clone(),
            runtime: products.runtime.clone(),
            playback_updates: products.playback.updates.clone(),
        }
    }
}

#[uniffi::export]
impl AndroidLibrary {
    pub async fn track_item(
        &self,
        media_uri: String,
    ) -> Result<Option<crate::browse::AndroidBrowseRow>, AndroidError> {
        let database = self.database.clone();
        let current = self.playback_updates.current().and_then(|view| {
            view.transport
                .current
                .as_ref()
                .filter(|current| current.media_uri == media_uri)
                .map(|current| current.item.clone())
        });
        self.runtime
            .spawn(async move {
                let cancellation = library::ReadCancellation::new();
                if let Some(track) = database
                    .track_row_by_uri(&media_uri, &cancellation)
                    .await
                    .map_err(error)?
                {
                    return Ok(Some(track.into()));
                }
                let queued = database
                    .queue_items_for_uris(std::slice::from_ref(&media_uri), &cancellation)
                    .await
                    .map_err(error)?
                    .into_iter()
                    .next();
                let Some(item) = current.or(queued) else {
                    return Ok(None);
                };
                let favorite = database
                    .favorite(&library::FavoriteTarget::Track(media_uri.clone()))
                    .await
                    .map_err(error)?;
                let source_id = library::source_entity_parts(&media_uri)
                    .map(|(source, _, _)| source.to_string());
                let downloaded = database
                    .downloaded_media_count(std::slice::from_ref(&media_uri), &cancellation)
                    .await
                    .map_err(error)?
                    > 0;
                Ok(Some(crate::browse::AndroidBrowseRow {
                    kind: "track".into(),
                    playback_context_id: None,
                    key: media_uri.clone(),
                    media_uri,
                    title: item.title,
                    subtitle: format!("{} · {}", item.artist, item.album),
                    artist: item.artist,
                    album: item.album,
                    fields: Vec::new(),
                    favorite,
                    duration_millis: item.duration_millis.max(0) as u64,
                    detail_route: None,
                    pin: None,
                    section: String::new(),
                    section_id: String::new(),
                    section_kind: String::new(),
                    section_refreshable: false,
                    year: item.year,
                    track_count: 1,
                    artwork_identity: item.artwork_binding,
                    downloaded,
                    source_id,
                    source_name: String::new(),
                    last_played: None,
                    writable: false,
                }))
            })
            .await
            .map_err(error)?
    }

    pub fn subscribe(&self) -> Arc<AndroidLibrarySubscription> {
        Arc::new(AndroidLibrarySubscription::new(self.source.clone()))
    }

    pub async fn refresh(&self) -> Result<(), AndroidError> {
        let selected = self
            .source
            .selected_library()
            .ok_or_else(|| error("No source selected"))?;
        self.source
            .refresh_source(selected.source_id)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn select_source(&self, source_id: String) -> Result<(), AndroidError> {
        self.source
            .select_source(sources::SourceId::new(source_id))
            .recv()
            .await
            .map_err(error)?
            .map_err(error)?;
        Ok(())
    }
}

#[derive(uniffi::Record)]
pub struct AndroidLibraryState {
    pub source_id: Option<String>,
    pub source_name: String,
    pub revision: u64,
    pub busy: bool,
    pub status: Option<String>,
    pub error: Option<String>,
}

struct Changes {
    configuration: tokio::sync::watch::Receiver<()>,
    catalog: tokio::sync::watch::Receiver<()>,
    operation: tokio::sync::watch::Receiver<SourceOperation>,
    initial: bool,
    revision: u64,
}

#[derive(uniffi::Object)]
pub struct AndroidLibrarySubscription {
    source: SourceHandle,
    changes: tokio::sync::Mutex<Changes>,
}

impl AndroidLibrarySubscription {
    fn new(source: SourceHandle) -> Self {
        let changes = Changes {
            configuration: source.configuration_changes(),
            catalog: source.catalog_changes(),
            operation: source.operation(),
            initial: true,
            revision: 0,
        };
        Self {
            source,
            changes: tokio::sync::Mutex::new(changes),
        }
    }
}

#[uniffi::export]
impl AndroidLibrarySubscription {
    pub async fn next(&self) -> Result<AndroidLibraryState, AndroidError> {
        let mut changes = self.changes.lock().await;
        let Changes {
            configuration,
            catalog,
            operation,
            initial,
            revision,
        } = &mut *changes;
        if *initial {
            *initial = false;
        } else {
            tokio::select! {
                result = configuration.changed() => { result.map_err(error)?; *revision += 1; },
                result = catalog.changed() => { result.map_err(error)?; *revision += 1; },
                result = operation.changed() => { result.map_err(error)?; },
            }
        }
        if configuration.has_changed().map_err(error)? || catalog.has_changed().map_err(error)? {
            *revision += 1;
        }
        configuration.borrow_and_update();
        catalog.borrow_and_update();
        let operation = operation.borrow_and_update().clone();
        let configured = self.source.list_sources();
        let selected = self.source.selected_library();
        let source_id = selected.map(|source| source.source_id.to_string());
        let name = configured
            .sources
            .iter()
            .find(|source| Some(source.id.as_str()) == source_id.as_deref())
            .map(|source| source.name.clone())
            .unwrap_or_default();
        let busy = !matches!(
            operation,
            SourceOperation::Idle | SourceOperation::Failed { .. }
        );
        let status = busy
            .then(|| rufin_core::runtime::source::source_operation_text(&operation))
            .flatten();
        let failure = match operation {
            SourceOperation::Failed { message, .. } => Some(message),
            _ => None,
        };
        Ok(AndroidLibraryState {
            source_id,
            source_name: name,
            revision: *revision,
            busy,
            status,
            error: failure,
        })
    }
}
