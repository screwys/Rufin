use library::{LibraryResult, ReadCancellation, Row, SidebarPin};
use sources::SourceId;

use super::SourceOwner;
use super::feed::Notice;

impl SourceOwner {
    /// Rows of the pins the Catalog still holds, in pin order: smart playlists and their
    /// folders evaluated now, in the selected music folder.
    pub async fn pin_rows(
        &self,
        pins: &[SidebarPin],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(SidebarPin, Row)>> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        let folder = self
            .selected_library()
            .and_then(|selected| selected.music_folder_key);
        self.shared
            .catalog
            .pin_rows(pins, now, folder, cancellation)
            .await
    }

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
        let Some(source) = catalog.source_key(source_id) else {
            return;
        };
        let ids = catalog
            .source_playlist_object_ids(source, &ReadCancellation::new())
            .await
            .map_err(|error| error.to_string());
        let imported = ids.and_then(|ids| {
            self.shared.settings.update(|stored| {
                Ok(stored
                    .ui
                    .sidebar
                    .import_playlist_pins_once(source_id.clone(), ids))
            })
        });
        match imported {
            Ok(true) => self.notice(Notice::Pins),
            Ok(false) => {}
            Err(error) => self.shared.warn_nonfatal(&error),
        }
    }
}
