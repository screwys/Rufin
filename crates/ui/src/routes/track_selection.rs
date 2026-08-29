//! Position-only multi-selection for sparse Track and playlist-entry routes.
//!
//! GTK's general multi-selection retains every selected model item. Rufin instead owns one
//! complete key order and clears selection when that order changes, so retaining a roaring bitset
//! of positions preserves native list selection without turning selected off-screen rows into
//! sparse hydration demand.

use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use library::{PlaylistEntryKey, PlaylistKey, SourceKey, TrackKey};
use playback::QueuePlacement;
use playback::SourceSessionEpoch;

use crate::shell::Shell;

use super::playlist_entry_model::PlaylistEntryModel;
use super::sparse_model::SparseObjectModel;
use super::track_model::TrackCollectionModel;

mod position_selection_imp {
    use super::*;

    #[derive(Default)]
    pub(crate) struct PositionSelectionModel {
        pub(super) model: RefCell<Option<SparseObjectModel>>,
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
            super::super::sparse_model::SparseObjectItem::static_type()
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
    pub(crate) struct PositionSelectionModel(ObjectSubclass<position_selection_imp::PositionSelectionModel>)
        @implements gio::ListModel, gtk::SelectionModel;
}

impl PositionSelectionModel {
    fn new(model: SparseObjectModel) -> Self {
        use glib::subclass::types::ObjectSubclassIsExt;

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

    fn clear_for_model_change(&self) {
        use glib::subclass::types::ObjectSubclassIsExt;

        self.imp().selected.replace(Some(gtk::Bitset::new_empty()));
    }
}

#[derive(Clone)]
pub(crate) struct TrackSelection {
    model: TrackCollectionModel,
    selection: PositionSelectionModel,
}

impl TrackSelection {
    pub(crate) fn new(model: TrackCollectionModel) -> Self {
        let selection = PositionSelectionModel::new(model.list_model());
        Self { model, selection }
    }

    pub(crate) fn selection_model(&self) -> gtk::SelectionModel {
        self.selection.clone().upcast()
    }

    pub(crate) fn connect_changed(&self, changed: impl Fn(&PositionSelectionModel) + 'static) {
        self.selection
            .connect_selection_changed(move |selection, _, _| changed(selection));
    }

    pub(crate) fn clear(&self) {
        self.selection.unselect_all();
    }

    pub(crate) fn uses_selection(&self, selection: &PositionSelectionModel) -> bool {
        self.selection == *selection
    }

    pub(crate) fn selected_tracks_for(&self, clicked: TrackKey) -> Option<TrackSelectionSnapshot> {
        self.selected_tracks()
            .filter(|selection| selection.tracks.len() > 1 && selection.tracks.contains(&clicked))
    }

    pub(crate) fn selected_tracks(&self) -> Option<TrackSelectionSnapshot> {
        let tracks = selected_keys(&self.model.order(), &self.selection.selection());
        (!tracks.is_empty()).then(|| TrackSelectionSnapshot {
            source_key: self.model.source_key(),
            source_session_epoch: self.model.source_session_epoch(),
            tracks: tracks.into(),
        })
    }

    pub(crate) fn dragged_tracks_for(&self, clicked: TrackKey) -> TrackSelectionSnapshot {
        self.selected_tracks_for(clicked)
            .unwrap_or_else(|| TrackSelectionSnapshot {
                source_key: self.model.source_key(),
                source_session_epoch: self.model.source_session_epoch(),
                tracks: Arc::from([clicked]),
            })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TrackSelectionSnapshot {
    pub(crate) source_key: SourceKey,
    pub(crate) source_session_epoch: SourceSessionEpoch,
    pub(crate) tracks: Arc<[TrackKey]>,
}

#[derive(Clone)]
pub(crate) struct PlaylistEntrySelection {
    model: PlaylistEntryModel,
    selection: PositionSelectionModel,
    playlist_name: Rc<str>,
}

impl PlaylistEntrySelection {
    pub(crate) fn new(model: PlaylistEntryModel, playlist_name: String) -> Self {
        let selection = PositionSelectionModel::new(model.list_model());
        Self {
            model,
            selection,
            playlist_name: playlist_name.into(),
        }
    }

    pub(crate) fn selection_model(&self) -> gtk::SelectionModel {
        self.selection.clone().upcast()
    }

    pub(crate) fn selected_entries_for(
        &self,
        clicked: PlaylistEntryKey,
    ) -> Option<PlaylistEntrySelectionSnapshot> {
        let positions = self.selection.selection();
        let entries = selected_values(&self.model.order(), &positions);
        (entries.len() > 1 && entries.contains(&clicked))
            .then(|| self.snapshot(entries, &positions))
    }

    pub(crate) fn selected_tracks(&self) -> Option<TrackSelectionSnapshot> {
        self.selected_entries().map(|selection| selection.tracks)
    }

    pub(crate) fn selected_entries(&self) -> Option<PlaylistEntrySelectionSnapshot> {
        let positions = self.selection.selection();
        let entries = selected_values(&self.model.order(), &positions);
        (!entries.is_empty()).then(|| self.snapshot(entries, &positions))
    }

    pub(crate) fn single_entry(
        &self,
        entry: PlaylistEntryKey,
        track: Option<TrackKey>,
    ) -> PlaylistEntrySelectionSnapshot {
        PlaylistEntrySelectionSnapshot {
            playlist: self.model.playlist_key(),
            playlist_name: Rc::clone(&self.playlist_name),
            entries: Arc::from([entry]),
            tracks: TrackSelectionSnapshot {
                source_key: self.model.source_key(),
                source_session_epoch: self.model.source_session_epoch(),
                tracks: track.into_iter().collect::<Vec<_>>().into(),
            },
        }
    }

    fn snapshot(
        &self,
        entries: Vec<PlaylistEntryKey>,
        positions: &gtk::Bitset,
    ) -> PlaylistEntrySelectionSnapshot {
        let tracks = selected_positions(positions)
            .filter_map(|position| self.model.track_key_at_position(position as usize))
            .collect::<Vec<_>>();
        PlaylistEntrySelectionSnapshot {
            playlist: self.model.playlist_key(),
            playlist_name: Rc::clone(&self.playlist_name),
            entries: entries.into(),
            tracks: TrackSelectionSnapshot {
                source_key: self.model.source_key(),
                source_session_epoch: self.model.source_session_epoch(),
                tracks: tracks.into(),
            },
        }
    }
}

#[derive(Clone)]
pub(crate) struct PlaylistEntrySelectionSnapshot {
    pub(crate) playlist: PlaylistKey,
    pub(crate) playlist_name: Rc<str>,
    pub(crate) entries: Arc<[PlaylistEntryKey]>,
    pub(crate) tracks: TrackSelectionSnapshot,
}

impl TrackSelectionSnapshot {
    pub(crate) fn is_current(&self, shell: &Shell) -> bool {
        shell.selected_library().as_deref().is_some_and(|selected| {
            selected.source_key == self.source_key
                && selected.source_session_epoch == self.source_session_epoch
        })
    }

    pub(crate) fn play(&self, shell: &Shell, placement: QueuePlacement) {
        let Some(selected) = shell
            .selected_library()
            .as_deref()
            .filter(|selected| {
                selected.source_key == self.source_key
                    && selected.source_session_epoch == self.source_session_epoch
            })
            .cloned()
        else {
            return;
        };
        let Some(anchor_key) = self.tracks.first().copied() else {
            return;
        };
        let order = Arc::clone(&self.tracks);
        let database = Arc::clone(&selected.database);
        let queue = shell.products.playback.queue.clone();
        selected.runtime.spawn(async move {
            let cancellation = library::ReadCancellation::new();
            let Some(anchor) = database
                .track_rows(selected.source_key, &[anchor_key], &cancellation)
                .await
                .ok()
                .and_then(|mut rows| rows.pop())
                .map(playback::PlaybackMedia::from)
            else {
                return;
            };
            if let Some(request) = playback::LoadedPlayRequest::manual(
                selected.source_key,
                selected.source_session_epoch,
                order,
                anchor,
                placement,
            ) {
                queue.play_loaded(request);
            }
        });
    }

    pub(crate) fn download_subject(&self) -> downloads::DownloadSubject {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.source_key.hash(&mut hasher);
        self.tracks.hash(&mut hasher);
        downloads::DownloadSubject::Prepared {
            context_id: format!("track-selection:{:016x}", hasher.finish()),
            title: None,
        }
    }
}

fn selected_keys(order: &[TrackKey], positions: &gtk::Bitset) -> Vec<TrackKey> {
    selected_values(order, positions)
}

fn selected_values<T: Copy>(order: &[T], positions: &gtk::Bitset) -> Vec<T> {
    keys_at_positions(order, selected_positions(positions))
}

fn selected_positions(positions: &gtk::Bitset) -> impl Iterator<Item = u32> + '_ {
    let first = gtk::BitsetIter::init_first(positions);
    first
        .into_iter()
        .flat_map(|(iter, first)| std::iter::once(first).chain(iter))
}

fn keys_at_positions<T: Copy>(order: &[T], positions: impl Iterator<Item = u32>) -> Vec<T> {
    positions
        .filter_map(|position| order.get(position as usize).copied())
        .collect()
}

pub(crate) fn install_track_drag_source(
    target: &impl IsA<gtk::Widget>,
    selection: TrackSelection,
    current_track: impl Fn() -> Option<TrackKey> + 'static,
) {
    let target = target.as_ref();
    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::COPY)
        .build();
    source.connect_prepare(move |_, _, _| {
        let track = current_track()?;
        let payload = glib::BoxedAnyObject::new(selection.dragged_tracks_for(track));
        Some(gtk::gdk::ContentProvider::for_value(&payload.to_value()))
    });
    let weak_target = target.downgrade();
    source.connect_drag_begin(move |source, _| {
        let Some(target) = weak_target.upgrade() else {
            return;
        };
        let paintable = gtk::WidgetPaintable::new(Some(&target));
        source.set_icon(Some(&paintable), 0, 0);
    });
    target.add_controller(source);
}

pub(crate) fn track_drag_payload(value: &glib::Value) -> Option<TrackSelectionSnapshot> {
    let payload = value.get::<glib::BoxedAnyObject>().ok()?;
    Some(payload.try_borrow::<TrackSelectionSnapshot>().ok()?.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_positions_follow_visible_track_order() {
        let order = [
            TrackKey::from_raw(11),
            TrackKey::from_raw(22),
            TrackKey::from_raw(33),
        ];
        assert_eq!(
            keys_at_positions(&order, [0, 2].into_iter()),
            [TrackKey::from_raw(11), TrackKey::from_raw(33)]
        );
    }
}
