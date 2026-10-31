use std::rc::Rc;
use std::sync::Arc;

use library::{PlaylistEntryKey, PlaylistKey, Row, TrackRow};

use rufin_core::collection_context::CollectionTarget;

use gtk_widgets::mounted_route::{RouteView, view_model};
use gtk_widgets::sparse_model::{SparseObjectModel, SparseRouteModel, SparseSource};
use rufin_core::view::Opened;

/// A playlist row: a track with its entry.
fn entry_row(row: Row) -> Option<TrackRow> {
    match row {
        Row::Track(track) if track.entry.is_some() => Some(track),
        _ => None,
    }
}

pub(crate) fn entry_key(row: &TrackRow) -> Option<PlaylistEntryKey> {
    row.entry.as_ref().map(|entry| entry.key)
}

#[derive(Clone)]
pub struct PlaylistEntryModel {
    inner: Rc<PlaylistEntryModelState>,
}

struct PlaylistEntryModelState {
    playlist_key: PlaylistKey,
    sparse: Rc<SparseRouteModel<(), TrackRow>>,
    /// The view the shown entries come from.
    view: RouteView,
}

impl PlaylistEntryModel {
    pub fn new(
        runtime: tokio::runtime::Handle,
        playlist_key: PlaylistKey,
        view: &RouteView,
        mut read: Opened,
    ) -> Self {
        let sparse = view_model(runtime, view, entry_row, &mut read);
        Self {
            inner: Rc::new(PlaylistEntryModelState {
                playlist_key,
                sparse,
                view: Rc::clone(view),
            }),
        }
    }

    pub fn list_model(&self) -> SparseObjectModel {
        self.inner.sparse.list_model()
    }

    pub fn resume_initial_demand(&self) {
        self.inner.sparse.resume_initial_demand();
    }

    pub fn ready_position(&self, entry: PlaylistEntryKey) -> Option<u32> {
        self.inner
            .sparse
            .ready_position(|row| entry_key(row) == Some(entry))
    }

    /// The entries read the route shows; a page whose playlist is gone has none.
    fn query(&self) -> library::MemberRead {
        self.inner
            .view
            .borrow()
            .members()
            .cloned()
            .expect("a shown playlist page reads its entries")
    }

    pub fn selection_input(&self, positions: &gtk::Bitset) -> library::MemberSelection {
        library::MemberSelection::Members {
            read: self.query(),
            ranges: Some(gtk_widgets::selection::selected_ranges(positions)),
        }
    }

    pub fn playlist_key(&self) -> PlaylistKey {
        self.inner.playlist_key
    }

    pub fn source_is_empty(&self) -> bool {
        self.inner.sparse.len() == 0
    }

    /// Where a reread starts so the rows on screen stay.
    pub fn live_window(&self) -> library::RouteSeedWindow {
        self.inner.sparse.live_window()
    }

    /// Shows a reread's entries; false when it has none because the playlist is gone.
    pub fn replace_prepared(&self, read: Opened) -> bool {
        if read.view.members().is_none() {
            return false;
        }
        let count = read.view.len();
        self.inner.sparse.replace_prepared_at(
            SparseSource::Query { count },
            read.first.min(count),
            read.rows.into_iter().filter_map(entry_row).collect(),
            Vec::new(),
            |_| (),
        );
        true
    }

    pub fn ready(&self, position: u32) -> Option<Arc<TrackRow>> {
        self.inner.sparse.ready(position)
    }

    pub fn update_downloaded(&self, media_uri: &str, downloaded: bool) {
        self.inner.sparse.update_matching(
            |row| row.media_uri == media_uri && row.is_downloaded != downloaded,
            |row| row.is_downloaded = downloaded,
        );
    }

    pub fn visible_context_id(&self) -> String {
        format!(
            "{}|result={}",
            self.inner.view.borrow().context_id(),
            self.inner.sparse.order_id()
        )
    }

    pub fn position_for_current(
        &self,
        media_uri: &str,
        context_id: &str,
        source_rank: usize,
    ) -> Option<u32> {
        if context_id == self.visible_context_id() {
            let position = u32::try_from(source_rank).ok()?;
            return self
                .ready(position)
                .filter(|row| row.media_uri == media_uri)
                .map(|_| position);
        }
        if context_id != CollectionTarget::Playlist(self.inner.playlist_key).context_id() {
            return None;
        }
        self.inner.sparse.ready_position(|row| {
            row.media_uri == media_uri
                && row
                    .entry
                    .as_ref()
                    .is_some_and(|entry| entry.position as usize == source_rank)
        })
    }

    pub fn activate(
        &self,
        position: u32,
        queue: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
    ) {
        let Some(_row) = self.ready(position) else {
            return;
        };
        self.play(
            position as usize,
            queue,
            rufin_core::playback::QueuePlacement::Now,
            false,
            false,
        );
    }

    pub fn play(
        &self,
        position: usize,
        queue: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
        placement: rufin_core::playback::QueuePlacement,
        collection_start: bool,
        shuffled: bool,
    ) {
        if position >= self.inner.sparse.len() {
            return;
        }
        let constructor = if collection_start {
            rufin_core::playback::PlayRequest::ordered
        } else {
            rufin_core::playback::PlayRequest::captured
        };
        let mut request = constructor(
            self.query().selection(),
            position,
            placement,
            collection_start,
        )
        .with_context(self.visible_context_id().into())
        .shuffled(shuffled);
        if !collection_start && let Some(row) = self.ready(position as u32) {
            request = request.with_anchor(row.track_key, entry_key(&row));
        }
        queue.play_selection(request);
    }
}
