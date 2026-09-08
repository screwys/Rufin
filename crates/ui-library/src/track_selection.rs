//! Position-only multi-selection for sparse Track and playlist-entry routes.
//!
//! GTK's general multi-selection retains every selected model item. Rufin instead owns one
//! complete key order and clears selection when that order changes, so retaining a roaring bitset
//! of positions preserves native list selection without turning selected off-screen rows into
//! sparse hydration demand.

use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use library::PlaylistEntryKey;

use super::playlist_entry_model::PlaylistEntryModel;
use super::track_model::TrackCollectionModel;

#[derive(Clone)]
pub struct TrackSelection {
    order: Rc<dyn Fn() -> Arc<[String]>>,
    selection: PositionSelectionModel,
}

impl TrackSelection {
    pub fn new<T: ui_shared::library_fields::TrackPresentation>(
        model: TrackCollectionModel<T>,
    ) -> Self {
        let selection = PositionSelectionModel::new(model.list_model());
        Self {
            order: Rc::new(move || model.order()),
            selection,
        }
    }

    pub fn selection_model(&self) -> gtk::SelectionModel {
        self.selection.clone().upcast()
    }

    pub fn connect_changed(&self, changed: impl Fn(&PositionSelectionModel) + 'static) {
        self.selection
            .connect_selection_changed(move |selection, _, _| changed(selection));
    }

    pub fn clear(&self) {
        self.selection.unselect_all();
    }

    pub fn uses_selection(&self, selection: &PositionSelectionModel) -> bool {
        self.selection == *selection
    }

    pub fn selected_tracks_for(&self, clicked: &str) -> Option<TrackSelectionSnapshot> {
        self.selected_tracks().filter(|selection| {
            selection.media_uris.len() > 1 && selection.media_uris.iter().any(|uri| uri == clicked)
        })
    }

    pub fn selected_tracks(&self) -> Option<TrackSelectionSnapshot> {
        let media_uris = selected_values(&(self.order)(), &self.selection.selection());
        (!media_uris.is_empty()).then(|| TrackSelectionSnapshot {
            media_uris: media_uris.into(),
        })
    }

    pub fn dragged_tracks_for(&self, clicked: &str) -> TrackSelectionSnapshot {
        self.selected_tracks_for(clicked)
            .unwrap_or_else(|| TrackSelectionSnapshot {
                media_uris: Arc::from([clicked.to_string()]),
            })
    }
}

#[derive(Clone)]
pub struct PlaylistEntrySelection {
    model: PlaylistEntryModel,
    selection: PositionSelectionModel,
    playlist_name: Arc<str>,
    writable: bool,
}

impl PlaylistEntrySelection {
    pub fn new(model: PlaylistEntryModel, playlist_name: String, writable: bool) -> Self {
        let selection = PositionSelectionModel::new(model.list_model());
        Self {
            model,
            selection,
            playlist_name: playlist_name.into(),
            writable,
        }
    }

    pub fn selection_model(&self) -> gtk::SelectionModel {
        self.selection.clone().upcast()
    }

    pub fn selected_entries_for(
        &self,
        clicked: PlaylistEntryKey,
    ) -> Option<PlaylistEntrySelectionSnapshot> {
        let positions = self.selection.selection();
        let entries = selected_values(&self.model.order(), &positions);
        (entries.len() > 1 && entries.contains(&clicked)).then(|| self.snapshot(entries))
    }

    pub fn selected_entries(&self) -> Option<PlaylistEntrySelectionSnapshot> {
        let positions = self.selection.selection();
        let entries = selected_values(&self.model.order(), &positions);
        (!entries.is_empty()).then(|| self.snapshot(entries))
    }

    pub fn single_entry(&self, entry: PlaylistEntryKey) -> PlaylistEntrySelectionSnapshot {
        PlaylistEntrySelectionSnapshot {
            writable: self.writable,
            playlist: self.model.playlist_key(),
            playlist_name: Arc::clone(&self.playlist_name),
            entries: Arc::from([entry]),
        }
    }

    fn snapshot(&self, entries: Vec<PlaylistEntryKey>) -> PlaylistEntrySelectionSnapshot {
        PlaylistEntrySelectionSnapshot {
            writable: self.writable,
            playlist: self.model.playlist_key(),
            playlist_name: Arc::clone(&self.playlist_name),
            entries: entries.into(),
        }
    }
}

use ui_shared::selection::{
    PlaylistEntrySelectionSnapshot, PositionSelectionModel, TrackSelectionSnapshot, selected_values,
};

use artwork::ArtworkBinding;
use ui_shared::media_drag::{
    MediaDragPreviewBinding, MediaDragSource, media_drag_content_provider,
};
pub fn install_track_drag_source(
    target: &impl IsA<gtk::Widget>,
    artwork: &Rc<ui_shared::artwork::ArtworkState>,
    selection: TrackSelection,
    current_track: impl Fn() -> Option<(String, String, ArtworkBinding)> + 'static,
) {
    target.as_ref().set_valign(gtk::Align::Fill);
    let preview = MediaDragPreviewBinding::default();
    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::COPY)
        .build();
    source.set_propagation_phase(gtk::PropagationPhase::Capture);
    let prepare_preview = preview.clone();
    let preview_shell = Rc::downgrade(artwork);
    source.connect_prepare(move |_, _, _| {
        prepare_preview.clear();
        let artwork_state = preview_shell.upgrade()?;
        let (track, title, artwork) = current_track()?;
        prepare_preview.prepare_cache_only(&artwork_state, title, artwork);
        Some(media_drag_content_provider(MediaDragSource::selection(
            selection.dragged_tracks_for(&track),
        )))
    });
    preview.connect(&source);
    target.as_ref().add_controller(source);
}
