use std::sync::Arc;

use rufin_core::runtime::{ProductHandles, SourceHandle};

use crate::host::{AndroidError, error};

#[derive(uniffi::Object)]
pub struct AndroidLibrary {
    pub(crate) source: SourceHandle,
    pub(crate) playback: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
    pub(crate) settings: rufin_core::SettingsHandle,
    pub(crate) database: Arc<library::Database>,
    pub(crate) runtime: tokio::runtime::Handle,
}

impl AndroidLibrary {
    pub(crate) fn new(products: &ProductHandles) -> Self {
        Self {
            source: products.source.clone(),
            playback: products.playback.clone(),
            settings: products.settings.clone(),
            database: products.library.clone(),
            runtime: products.runtime.clone(),
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
        let current = self.playback.updates.current().and_then(|view| {
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
                    context_source_id: source_id,
                    ..crate::browse::AndroidBrowseRow::new(
                        crate::browse::AndroidBrowseItem::Queued {
                            row: item,
                            favorite,
                            downloaded,
                        },
                    )
                }))
            })
            .await
            .map_err(error)?
    }
}
