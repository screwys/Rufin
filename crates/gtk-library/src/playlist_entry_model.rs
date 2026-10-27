use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use library::{PlaylistEntryKey, PlaylistEntryView, PlaylistKey};

use rufin_core::collection_context::CollectionTarget;
use rufin_core::settings::LibraryListSettings;

use gtk_widgets::sparse_model::{SparseObjectModel, SparseRouteModel, SparseSource};

const PLAYLIST_ENTRY_OVERSCAN: usize = 64;

/// The read the shown entries came from, and the list settings it read with.
struct AppliedEntries {
    query: library::MemberRead,
    settings: LibraryListSettings,
}

#[derive(Clone)]
pub struct PlaylistEntryModel {
    inner: Rc<PlaylistEntryModelState>,
}

struct PlaylistEntryModelState {
    playlist_key: PlaylistKey,
    sparse: Rc<SparseRouteModel<PlaylistEntryKey, PlaylistEntryView>>,
    applied: Rc<RefCell<AppliedEntries>>,
}

impl PlaylistEntryModel {
    pub fn new(
        catalog: Arc<library::Catalog>,
        runtime: tokio::runtime::Handle,
        playlist_key: PlaylistKey,
        count: usize,
        first_row_position: usize,
        first_rows: Vec<PlaylistEntryView>,
        settings: LibraryListSettings,
        read: library::MemberRead,
    ) -> Self {
        let applied = Rc::new(RefCell::new(AppliedEntries {
            query: read,
            settings,
        }));
        let load_request = Rc::clone(&applied);
        let loader_catalog = Arc::clone(&catalog);
        let load = Rc::new(
            move |_: Vec<PlaylistEntryKey>,
                  range: std::ops::Range<usize>,
                  cancellation: library::ReadCancellation| {
                let catalog = Arc::clone(&loader_catalog);
                let query = load_request.borrow().query.clone();
                Box::pin(async move {
                    query
                        .rows(&catalog, range.start, range.len(), &cancellation)
                        .await
                        .map(|rows| {
                            rows.into_iter()
                                .filter_map(library::MemberRow::into_entry)
                                .collect()
                        })
                        .map_err(|error| error.to_string())
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        let sparse = SparseRouteModel::new(
            SparseSource::Query { count },
            PLAYLIST_ENTRY_OVERSCAN,
            runtime.clone(),
            load,
        );
        sparse.seed_matching_at(first_row_position, first_rows, |row| row.playlist_entry_key);
        Self {
            inner: Rc::new(PlaylistEntryModelState {
                playlist_key,
                sparse,
                applied,
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
            .ready_position(|row| row.playlist_entry_key == entry)
    }

    pub fn selection_input(&self, positions: &gtk::Bitset) -> library::MemberSelection {
        library::MemberSelection::Members {
            read: self.inner.applied.borrow().query.clone(),
            ranges: Some(gtk_widgets::selection::selected_ranges(positions)),
        }
    }

    pub fn playlist_key(&self) -> PlaylistKey {
        self.inner.playlist_key
    }

    pub fn source_is_empty(&self) -> bool {
        self.inner.sparse.len() == 0
    }

    /// Where the shown window starts, to read it again in place.
    pub fn first_live_position(&self) -> usize {
        self.inner.sparse.first_live_position().unwrap_or(0)
    }

    pub fn replace_prepared(
        &self,
        page: &rufin_core::browse::MemberPage,
        settings: LibraryListSettings,
    ) {
        let count = page.page.count;
        let first = page.page.first_row_position.min(count);
        self.inner.applied.replace(AppliedEntries {
            query: page.read.clone(),
            settings,
        });
        self.inner.sparse.replace_prepared_at(
            SparseSource::Query { count },
            first,
            page.page
                .rows
                .iter()
                .cloned()
                .filter_map(library::MemberRow::into_entry)
                .collect(),
            Vec::new(),
            |row| row.playlist_entry_key,
        );
    }

    pub fn ready(&self, position: u32) -> Option<Arc<PlaylistEntryView>> {
        self.inner.sparse.ready(position)
    }

    pub fn update_downloaded(&self, media_uri: &str, downloaded: bool) {
        self.inner.sparse.update_matching(
            |row| row.track.media_uri == media_uri && row.track.access.is_downloaded != downloaded,
            |row| row.track.access.is_downloaded = downloaded,
        );
    }

    pub fn visible_context_id(&self) -> String {
        let request = self.inner.applied.borrow();
        format!(
            "{}|query={}|sort={:?}|descending={}|result={}",
            CollectionTarget::Playlist(self.inner.playlist_key).context_id(),
            request.query.filter,
            request.settings.sort_key,
            request.settings.descending,
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
                .filter(|row| row.track.media_uri == media_uri)
                .map(|_| position);
        }
        if context_id != CollectionTarget::Playlist(self.inner.playlist_key).context_id() {
            return None;
        }
        self.inner.sparse.ready_position(|row| {
            row.track.media_uri == media_uri && row.position.max(0) as usize == source_rank
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
            self.inner.applied.borrow().query.clone().selection(),
            position,
            placement,
            collection_start,
        )
        .with_context(self.visible_context_id().into())
        .shuffled(shuffled);
        if !collection_start && let Some(row) = self.ready(position as u32) {
            request = request.with_anchor(row.track.track_key, Some(row.playlist_entry_key));
        }
        queue.play_selection(request);
    }
}
