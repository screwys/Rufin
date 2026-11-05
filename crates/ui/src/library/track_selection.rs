//! Position-only multi-selection for sparse Track and playlist-entry routes.
//!
//! Selected positions are resolved when an action needs them. Selecting off-screen rows does not
//! hydrate their widgets or prepare a complete collection order.

use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use library::PlaylistEntryKey;

use crate::widgets::view_model::Rows;
use library::{PlaylistKey, Row};

/// Where the track `media_uri` names is read in `rows`.
fn track_position(rows: &Rows<Row>, media_uri: &str) -> Option<u32> {
    rows.ready_position(|row| match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => track.media_uri == media_uri,
        _ => false,
    })
}

#[derive(Clone)]
pub struct TrackSelection {
    input: Rc<dyn Fn(&gtk::Bitset) -> library::MemberSelection>,
    position: Rc<dyn Fn(&str) -> Option<u32>>,
    selection: PositionSelectionModel,
}

impl TrackSelection {
    pub fn new(rows: &Rows<Row>) -> Self {
        let selection = PositionSelectionModel::new(rows.list_model());
        let input = rows.clone();
        let locate = rows.clone();
        Self {
            input: Rc::new(move |positions| input.selection(positions)),
            position: Rc::new(move |uri| track_position(&locate, uri)),
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
        self.selected_tracks_for(clicked).unwrap_or_else(|| {
            let positions = gtk::Bitset::new_empty();
            if let Some(position) = (self.position)(clicked) {
                positions.add(position);
            }
            TrackSelectionSnapshot {
                input: (self.input)(&positions),
                count: positions.size() as usize,
            }
        })
    }
}

#[derive(Clone)]
pub struct PlaylistEntrySelection {
    rows: Rows<Row>,
    playlist: PlaylistKey,
    selection: PositionSelectionModel,
    playlist_name: Arc<str>,
    writable: bool,
}

impl PlaylistEntrySelection {
    pub fn new(
        rows: &Rows<Row>,
        playlist: PlaylistKey,
        playlist_name: String,
        writable: bool,
    ) -> Self {
        let selection = PositionSelectionModel::new(rows.list_model());
        Self {
            rows: rows.clone(),
            playlist,
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
        let position = self.rows.ready_position(|row| {
            matches!(row, Row::Track(track) if track.entry.as_ref().is_some_and(|entry| entry.key == clicked))
        })?;
        (positions.size() > 1 && positions.contains(position)).then(|| self.snapshot(&positions))
    }

    pub fn selected_entries(&self) -> Option<PlaylistEntrySelectionSnapshot> {
        let positions = self.selection.selection();
        (positions.size() > 0).then(|| self.snapshot(&positions))
    }

    pub fn single_entry(&self, entry: library::PlaylistEntry) -> PlaylistEntrySelectionSnapshot {
        PlaylistEntrySelectionSnapshot {
            writable: self.writable,
            playlist: self.playlist,
            playlist_name: Arc::clone(&self.playlist_name),
            count: 1,
            input: library::MemberSelection::Entries(vec![entry.key].into()),
            entry: Some(entry),
        }
    }

    fn snapshot(&self, positions: &gtk::Bitset) -> PlaylistEntrySelectionSnapshot {
        PlaylistEntrySelectionSnapshot {
            writable: self.writable,
            playlist: self.playlist,
            playlist_name: Arc::clone(&self.playlist_name),
            input: self.rows.selection(positions),
            count: positions.size() as usize,
            entry: None,
        }
    }
}

use crate::widgets::selection::{
    PlaylistEntrySelectionSnapshot, PositionSelectionModel, TrackSelectionSnapshot,
};

use crate::widgets::media_drag::{
    MediaDragPreviewBinding, MediaDragSource, media_drag_content_provider,
};
use artwork::ArtworkBinding;
pub fn install_track_drag_source(
    target: &impl IsA<gtk::Widget>,
    artwork: &Rc<crate::widgets::artwork::ArtworkState>,
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
