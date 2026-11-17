use library::{LibraryResult, ReadCancellation, Row, SidebarPin};
use sources::SourceId;

use super::SourceOwner;
use super::feed::Notice;

/// What a pin shows: a collection's row, or a radio station.
#[derive(Clone, Debug, PartialEq)]
pub enum Pinned {
    Row(Box<Row>),
    Station(Box<crate::stations::Station>),
}

impl SourceOwner {
    /// What the pins the Store still holds show, in pin order. Collections are counted as their
    /// pages read them in the selection: smart playlists and their folders evaluated now, and
    /// the selected music folder narrowing only its own source's pins.
    pub async fn pin_rows(
        &self,
        pins: &[SidebarPin],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(SidebarPin, Pinned)>> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        let folder = self
            .selected_library()
            .and_then(|selected| selected.music_folder_key);
        let catalog = &self.shared.catalog;
        let mut rows = catalog
            .pin_rows(pins, now, folder, cancellation)
            .await?
            .into_iter()
            .peekable();
        let mut stations = catalog.pinned_stations(pins).await?.into_iter().peekable();
        Ok(pins
            .iter()
            .filter_map(|pin| {
                if let Some((pin, row)) = rows.next_if(|(shown, _)| shown == pin) {
                    return Some((pin, Pinned::Row(Box::new(row))));
                }
                let (pin, row) = stations.next_if(|(shown, _)| shown == pin)?;
                let station = crate::stations::pinned(self, &pin, &row);
                Some((pin, Pinned::Station(Box::new(station))))
            })
            .collect())
    }

    /// The pin of the collection `route` shows, as Add to Saved pins its row; None for a page
    /// no pin keeps.
    pub async fn route_pin(
        &self,
        route: &crate::route::Route,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<SidebarPin>> {
        let catalog = &self.shared.catalog;
        let Some(item) = route.pinned_item(catalog) else {
            return Ok(None);
        };
        let rows = catalog.rows_of(&[item], false, cancellation).await?;
        Ok(rows.first().and_then(Row::pin))
    }

    /// Whether the pinned `row` takes dropped tracks: a writable playlist of the selected library,
    /// as Add to Playlist offers it.
    pub fn takes_tracks(&self, row: &Row) -> bool {
        let Row::Playlist(library::PlaylistListRow::Playlist(playlist)) = row else {
            return false;
        };
        playlist.writable
            && playlist.source_key.is_none_or(|source| {
                self.selected_library()
                    .is_some_and(|selected| selected.source_key == source)
            })
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
