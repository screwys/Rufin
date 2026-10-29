use std::sync::Arc;

use rufin_core::runtime::{ProductHandles, SourceHandle};

#[derive(uniffi::Object)]
pub struct AndroidLibrary {
    pub(crate) source: SourceHandle,
    pub(crate) playback: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
    pub(crate) settings: rufin_core::SettingsHandle,
    pub(crate) catalog: Arc<library::Catalog>,
    pub(crate) runtime: tokio::runtime::Handle,
}

impl AndroidLibrary {
    pub(crate) fn new(products: &ProductHandles) -> Self {
        Self {
            source: products.source.clone(),
            playback: products.playback.clone(),
            settings: products.settings.clone(),
            catalog: products.library.clone(),
            runtime: products.runtime.clone(),
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

    /// A queue track as a menu row. Menus show no columns, so the row leaves out the date
    /// added.
    pub fn track_item(&self, track: library::TrackView) -> crate::browse::AndroidBrowseRow {
        library::Row::Track(library::TrackRow {
            track_key: track.track_key,
            entry: None,
            source_key: track.source_key,
            source_id: track.source_id,
            source_name: track.access.source_name,
            media_uri: track.media_uri,
            object_id: track.object_id,
            title: track.metadata.title,
            artist: track.metadata.artist,
            album: track.metadata.album,
            duration_millis: track.metadata.duration_millis,
            disc_number: track.metadata.disc_number,
            track_number: track.metadata.track_number,
            year: track.metadata.year,
            bpm: track.metadata.bpm,
            date_added: None,
            source_format: track.metadata.source_format,
            state: track.state,
            is_downloaded: track.access.is_downloaded,
            cue: track.access.cue_path.is_some(),
            links: track.links,
            release_date: track.metadata.release_date,
            source_path: track.access.source_path,
            audio: track.metadata.audio_properties,
            artwork_binding: track.artwork_binding,
        })
        .into()
    }
}
