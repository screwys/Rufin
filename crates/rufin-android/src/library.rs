use std::sync::Arc;

use rufin_core::runtime::{ProductHandles, SourceHandle};

#[derive(uniffi::Object)]
pub struct AndroidLibrary {
    pub(crate) source: SourceHandle,
    pub(crate) playback: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
    pub(crate) settings: rufin_core::SettingsHandle,
    pub(crate) catalog: Arc<library::Catalog>,
    pub(crate) runtime: tokio::runtime::Handle,
    /// The committed changes the rows of pages without a View show.
    pub(crate) changes: rufin_core::shown::ShownChanges,
}

impl AndroidLibrary {
    pub(crate) fn new(products: &ProductHandles) -> Self {
        Self {
            source: products.source.clone(),
            playback: products.playback.clone(),
            settings: products.settings.clone(),
            catalog: products.library.clone(),
            runtime: products.runtime.clone(),
            changes: Default::default(),
        }
    }
}

#[uniffi::export]
impl AndroidLibrary {
    pub async fn media_details(
        &self,
        item: rufin_core::metadata::MediaDetailsItem,
    ) -> Result<rufin_core::metadata::MediaDetails, crate::host::AndroidError> {
        rufin_core::metadata::media_details(self.source.clone(), self.playback.clone(), item)
            .await
            .map_err(crate::host::error)
    }

    /// A queue track as a menu row. Menus show no columns.
    pub fn track_item(&self, track: library::TrackView) -> crate::browse::AndroidBrowseRow {
        library::Row::Track(track.into()).into()
    }
}
