//! Position-only multi-selection for sparse Track and playlist-entry routes.
//!
//! Selected positions are resolved when an action needs them. Selecting off-screen rows does not
//! hydrate their widgets or prepare a complete collection order.

use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use library::PlaylistEntryKey;

use super::playlist_entry_model::PlaylistEntryModel;
use super::track_model::TrackCollectionModel;

#[derive(Clone)]
pub struct TrackSelection {
    input: Rc<dyn Fn(&gtk::Bitset) -> library::QueueInput>,
    position: Rc<dyn Fn(&str) -> Option<u32>>,
    selection: PositionSelectionModel,
}

impl TrackSelection {
    pub fn new<T: gtk_widgets::library_fields::TrackPresentation>(
        model: TrackCollectionModel<T>,
    ) -> Self {
        let selection = PositionSelectionModel::new(model.list_model());
        let locate = model.clone();
        Self {
            input: Rc::new(move |positions| model.selection_input(positions)),
            position: Rc::new(move |uri| locate.ready_position(uri)),
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
        let position = (self.position)(clicked)?;
        let positions = self.selection.selection();
        (positions.size() > 1 && positions.contains(position)).then(|| TrackSelectionSnapshot {
            input: (self.input)(&positions),
            count: positions.size() as usize,
        })
    }

    pub fn selected_tracks(&self) -> Option<TrackSelectionSnapshot> {
        let positions = self.selection.selection();
        (positions.size() > 0).then(|| TrackSelectionSnapshot {
            input: (self.input)(&positions),
            count: positions.size() as usize,
        })
    }

    pub fn dragged_tracks_for(&self, clicked: &str) -> TrackSelectionSnapshot {
        self.selected_tracks_for(clicked)
            .unwrap_or_else(|| TrackSelectionSnapshot {
                input: library::QueueInput::MediaUris {
                    order: Arc::from([clicked.to_string()]),
                    provenance: library::QueueProvenance::Manual,
                },
                count: 1,
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
        let position = self.model.ready_position(clicked)?;
        (positions.size() > 1 && positions.contains(position)).then(|| self.snapshot(&positions))
    }

    pub fn selected_entries(&self) -> Option<PlaylistEntrySelectionSnapshot> {
        let positions = self.selection.selection();
        (positions.size() > 0).then(|| self.snapshot(&positions))
    }

    pub fn single_entry(&self, entry: PlaylistEntryKey) -> PlaylistEntrySelectionSnapshot {
        PlaylistEntrySelectionSnapshot {
            writable: self.writable,
            playlist: self.model.playlist_key(),
            playlist_name: Arc::clone(&self.playlist_name),
            count: 1,
            input: library::QueueInput::PlaylistEntries {
                order: Arc::from([entry]),
                context_id: format!("playlist-selection:{}", self.model.playlist_key()).into(),
            },
        }
    }

    fn snapshot(&self, positions: &gtk::Bitset) -> PlaylistEntrySelectionSnapshot {
        PlaylistEntrySelectionSnapshot {
            writable: self.writable,
            playlist: self.model.playlist_key(),
            playlist_name: Arc::clone(&self.playlist_name),
            input: self.model.selection_input(positions),
            count: positions.size() as usize,
        }
    }
}

use gtk_widgets::selection::{
    PlaylistEntrySelectionSnapshot, PositionSelectionModel, TrackSelectionSnapshot,
};

use artwork::ArtworkBinding;
use gtk_widgets::media_drag::{
    MediaDragPreviewBinding, MediaDragSource, media_drag_content_provider,
};
pub fn install_track_drag_source(
    target: &impl IsA<gtk::Widget>,
    artwork: &Rc<gtk_widgets::artwork::ArtworkState>,
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
