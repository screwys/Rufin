use library::ReadCancellation;
use sources::SourceId;

use super::SourceOwner;
use crate::runtime::{CatalogChange, CatalogPublication, SourceEvent};

impl SourceOwner {
    /// Pins every playlist of a remote source once, after its first successful refresh.
    pub(super) async fn import_playlist_pins_once(&self, source_id: &SourceId) {
        let stored = self.shared.settings.load();
        if stored
            .ui
            .sidebar
            .playlist_pin_imported_sources
            .contains(source_id)
            || self
                .configuration(source_id)
                .is_none_or(|configuration| configuration.is_local())
        {
            return;
        }
        let catalog = &self.shared.catalog;
        let ids = match catalog.source_identity_key(source_id).await {
            Ok(Some(source)) => catalog
                .source_playlist_object_ids(source, &ReadCancellation::new())
                .await
                .map_err(|error| error.to_string()),
            Ok(None) => return,
            Err(error) => Err(error.to_string()),
        };
        let imported = ids.and_then(|ids| {
            self.shared.settings.update(|stored| {
                Ok(stored
                    .ui
                    .sidebar
                    .import_playlist_pins_once(source_id.clone(), ids))
            })
        });
        match imported {
            Ok(true) => {
                self.shared
                    .send(SourceEvent::CatalogPublished(CatalogPublication {
                        source_key: None,
                        favorites: Vec::new(),
                        change: CatalogChange::Playlists(None),
                    }))
                    .await;
            }
            Ok(false) => {}
            Err(error) => self.shared.warn_nonfatal(&error),
        }
    }
}
