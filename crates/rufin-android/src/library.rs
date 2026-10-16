use std::sync::Arc;

use rufin_core::runtime::{ProductHandles, SourceHandle};

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
    pub fn track_item(&self, song: library::TrackView) -> crate::browse::AndroidBrowseRow {
        song.into()
    }
}
