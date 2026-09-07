use gtk::{gio, glib, prelude::*, subclass::prelude::*};
use library::{PlaylistEntryKey, PlaylistKey};
use playback::QueuePlacement;
use std::{cell::RefCell, sync::Arc};
mod position_selection_imp {
    use super::*;

    #[derive(Default)]
    pub struct PositionSelectionModel {
        pub(super) model: RefCell<Option<gio::ListModel>>,
        pub(super) model_handler: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) selected: RefCell<Option<gtk::Bitset>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PositionSelectionModel {
        const NAME: &'static str = "RufinPositionSelectionModel";
        type Type = super::PositionSelectionModel;
        type Interfaces = (gio::ListModel, gtk::SelectionModel);
    }

    impl ObjectImpl for PositionSelectionModel {
        fn dispose(&self) {
            if let (Some(model), Some(handler)) = (
                self.model.borrow().as_ref(),
                self.model_handler.borrow_mut().take(),
            ) {
                model.disconnect(handler);
            }
            self.model.borrow_mut().take();
            self.selected.borrow_mut().take();
        }
    }

    impl ListModelImpl for PositionSelectionModel {
        fn item_type(&self) -> glib::Type {
            self.model.borrow().as_ref().map_or_else(
                glib::Object::static_type,
                gio::prelude::ListModelExt::item_type,
            )
        }

        fn n_items(&self) -> u32 {
            self.model
                .borrow()
                .as_ref()
                .map_or(0, gio::prelude::ListModelExt::n_items)
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            self.model.borrow().as_ref()?.item(position)
        }
    }

    impl SelectionModelImpl for PositionSelectionModel {
        fn selection_in_range(&self, _: u32, _: u32) -> gtk::Bitset {
            self.selection_copy()
        }

        fn is_selected(&self, position: u32) -> bool {
            self.selected
                .borrow()
                .as_ref()
                .is_some_and(|selected| selected.contains(position))
        }

        fn select_all(&self) -> bool {
            self.replace_selection(gtk::Bitset::new_range(0, self.n_items()))
        }

        fn select_item(&self, position: u32, unselect_rest: bool) -> bool {
            if position >= self.n_items() {
                return true;
            }
            let selected = if unselect_rest {
                gtk::Bitset::new_empty()
            } else {
                self.selection_copy()
            };
            selected.add(position);
            self.replace_selection(selected)
        }

        fn select_range(&self, position: u32, n_items: u32, unselect_rest: bool) -> bool {
            let available = self.n_items().saturating_sub(position);
            let n_items = n_items.min(available);
            let selected = if unselect_rest {
                gtk::Bitset::new_empty()
            } else {
                self.selection_copy()
            };
            selected.add_range(position, n_items);
            self.replace_selection(selected)
        }

        fn set_selection(&self, selected: &gtk::Bitset, mask: &gtk::Bitset) -> bool {
            let next = self.selection_copy();
            let removed = mask.copy();
            removed.subtract(selected);
            next.subtract(&removed);
            let added = selected.copy();
            added.intersect(mask);
            next.union(&added);
            let n_items = self.n_items();
            if n_items == 0 {
                next.remove_all();
            } else if next.maximum() >= n_items {
                next.remove_range(n_items, u32::MAX - n_items + 1);
            }
            self.replace_selection(next)
        }

        fn unselect_all(&self) -> bool {
            self.replace_selection(gtk::Bitset::new_empty())
        }

        fn unselect_item(&self, position: u32) -> bool {
            let selected = self.selection_copy();
            selected.remove(position);
            self.replace_selection(selected)
        }

        fn unselect_range(&self, position: u32, n_items: u32) -> bool {
            let selected = self.selection_copy();
            selected.remove_range(position, n_items);
            self.replace_selection(selected)
        }
    }

    impl PositionSelectionModel {
        fn selection_copy(&self) -> gtk::Bitset {
            self.selected
                .borrow()
                .as_ref()
                .expect("position selection is initialized")
                .copy()
        }

        fn replace_selection(&self, selected: gtk::Bitset) -> bool {
            let changes = self.selection_copy();
            changes.difference(&selected);
            if changes.is_empty() {
                return true;
            }
            let first = changes.minimum();
            let last = changes.maximum();
            self.selected.replace(Some(selected));
            self.obj().selection_changed(first, last - first + 1);
            true
        }
    }
}

glib::wrapper! {
    pub struct PositionSelectionModel(ObjectSubclass<position_selection_imp::PositionSelectionModel>)
        @implements gio::ListModel, gtk::SelectionModel;
}

impl PositionSelectionModel {
    pub fn new(model: impl IsA<gio::ListModel> + Clone + 'static) -> Self {
        use glib::subclass::types::ObjectSubclassIsExt;

        let model = model.upcast::<gio::ListModel>();
        let selection: Self = glib::Object::new();
        selection
            .imp()
            .selected
            .replace(Some(gtk::Bitset::new_empty()));
        selection.imp().model.replace(Some(model.clone()));
        let weak = selection.downgrade();
        let handler = model.connect_items_changed(move |_, position, removed, added| {
            let Some(selection) = weak.upgrade() else {
                return;
            };
            selection.clear_for_model_change();
            selection.items_changed(position, removed, added);
        });
        selection.imp().model_handler.replace(Some(handler));
        selection
    }

    pub fn clear_for_model_change(&self) {
        use glib::subclass::types::ObjectSubclassIsExt;

        self.imp().selected.replace(Some(gtk::Bitset::new_empty()));
    }
}

#[derive(Clone)]
pub struct TrackSelectionSnapshot {
    pub media_uris: Arc<[String]>,
}
impl TrackSelectionSnapshot {
    pub fn play(&self, queue: &playback::QueueHandle, placement: QueuePlacement) {
        if self.media_uris.is_empty() {
            return;
        }
        queue.play(playback::PlayRequest::ordered(
            library::QueueInput::MediaUris {
                order: self.media_uris.clone(),
                provenance: library::QueueProvenance::Manual,
            },
            0,
            placement,
            false,
        ));
    }

    pub fn download_subject(&self) -> downloads::DownloadSubject {
        downloads::DownloadSubject::for_media_uris("track-selection", None, &self.media_uris)
    }
}
#[derive(Clone)]
pub struct PlaylistEntrySelectionSnapshot {
    pub playlist: PlaylistKey,
    pub playlist_name: Arc<str>,
    pub entries: Arc<[PlaylistEntryKey]>,
}
impl PlaylistEntrySelectionSnapshot {
    pub async fn media_uris(&self, database: &library::Database) -> Result<Vec<String>, String> {
        let cancellation = library::ReadCancellation::new();
        let mut media_uris = Vec::with_capacity(self.entries.len());
        for entries in self.entries.chunks(256) {
            media_uris.extend(
                database
                    .playlist_entry_media_uris(self.playlist, entries, &cancellation)
                    .await
                    .map_err(|error| error.to_string())?,
            );
        }
        Ok(media_uris)
    }

    pub fn play(&self, queue: &playback::QueueHandle, placement: QueuePlacement) {
        queue.play(playback::PlayRequest::ordered(
            library::QueueInput::PlaylistEntries {
                order: self.entries.clone(),
                context_id: format!("playlist-selection:{}", self.playlist).into(),
            },
            0,
            placement,
            false,
        ));
    }
}
pub fn selected_values<T: Clone>(order: &[T], positions: &gtk::Bitset) -> Vec<T> {
    keys_at_positions(order, selected_positions(positions))
}

fn selected_positions(positions: &gtk::Bitset) -> impl Iterator<Item = u32> + '_ {
    let first = gtk::BitsetIter::init_first(positions);
    first
        .into_iter()
        .flat_map(|(iter, first)| std::iter::once(first).chain(iter))
}

fn keys_at_positions<T: Clone>(order: &[T], positions: impl Iterator<Item = u32>) -> Vec<T> {
    positions
        .filter_map(|position| order.get(position as usize).cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_positions_follow_visible_track_order() {
        let order = ["one", "two", "three"];
        assert_eq!(
            keys_at_positions(&order, [0, 2].into_iter()),
            ["one", "three"]
        );
    }
}
