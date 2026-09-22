use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use library::{PlaylistEntryKey, PlaylistEntryRow, PlaylistKey};

use rufin_core::settings::LibraryListSettings;

use ui_shared::sparse_model::{SparseObjectModel, SparseRouteModel, SparseSource};

const PLAYLIST_ENTRY_OVERSCAN: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaylistEntryProjectionRequest {
    pub query: String,
    pub settings: LibraryListSettings,
}

#[derive(Clone)]
pub struct PlaylistEntryModel {
    inner: Rc<PlaylistEntryModelState>,
}

struct PlaylistEntryModelState {
    playlist_key: PlaylistKey,
    sparse: Rc<SparseRouteModel<PlaylistEntryKey, PlaylistEntryRow>>,
    request: RefCell<PlaylistEntryProjectionRequest>,
    applied: Rc<RefCell<PlaylistEntryProjectionRequest>>,
}

impl PlaylistEntryModel {
    pub fn new(
        database: Arc<library::Database>,
        runtime: tokio::runtime::Handle,
        playlist_key: PlaylistKey,
        count: usize,
        first_row_position: usize,
        first_rows: Vec<PlaylistEntryRow>,
        settings: LibraryListSettings,
    ) -> Self {
        let applied = Rc::new(RefCell::new(PlaylistEntryProjectionRequest {
            query: String::new(),
            settings: settings.clone(),
        }));
        let load_request = Rc::clone(&applied);
        let loader_database = Arc::clone(&database);
        let load = Rc::new(
            move |_: Vec<PlaylistEntryKey>,
                  range: std::ops::Range<usize>,
                  cancellation: library::ReadCancellation| {
                let database = Arc::clone(&loader_database);
                let request = load_request.borrow().clone();
                Box::pin(async move {
                    database
                        .playlist_entries_page(
                            playlist_key,
                            None,
                            request.settings.sort_key.playlist_entry_sort(),
                            request.settings.descending,
                            &request.query,
                            range.start,
                            range.len(),
                            &cancellation,
                        )
                        .await
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
                request: RefCell::new(PlaylistEntryProjectionRequest {
                    query: String::new(),
                    settings,
                }),
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

    pub fn selection_input(&self, positions: &gtk::Bitset) -> library::QueueInput {
        let applied = self.inner.applied.borrow();
        library::QueueInput::PlaylistSelection {
            key: self.inner.playlist_key,
            filter: applied.query.clone(),
            sort: applied.settings.sort_key.playlist_entry_sort(),
            descending: applied.settings.descending,
            ranges: ui_shared::selection::selected_ranges(positions),
            context_id: format!("playlist-selection:{}", self.inner.playlist_key).into(),
        }
    }

    pub fn playlist_key(&self) -> PlaylistKey {
        self.inner.playlist_key
    }

    pub fn source_is_empty(&self) -> bool {
        self.inner.sparse.len() == 0
    }

    pub fn projection_request(&self) -> PlaylistEntryProjectionRequest {
        self.inner.request.borrow().clone()
    }

    pub fn set_query(&self, query: &str) -> bool {
        let query = query.trim();
        let mut request = self.inner.request.borrow_mut();
        if request.query == query {
            return false;
        }
        request.query = query.to_string();
        true
    }

    pub fn apply_settings(&self, settings: LibraryListSettings) -> bool {
        let mut request = self.inner.request.borrow_mut();
        if request.settings == settings {
            return false;
        }
        request.settings = settings;
        true
    }

    pub fn replace_count(&self, count: usize, request: PlaylistEntryProjectionRequest) {
        self.inner.applied.replace(request);
        self.inner
            .sparse
            .replace_order(SparseSource::Query { count });
    }

    pub fn ready(&self, position: u32) -> Option<Arc<PlaylistEntryRow>> {
        self.inner.sparse.ready(position)
    }

    pub fn update_downloaded(&self, media_uri: &str, downloaded: bool) {
        self.inner.sparse.update_matching(
            |row| row.media_uri == media_uri && row.is_downloaded != downloaded,
            |row| row.is_downloaded = downloaded,
        );
    }

    pub fn visible_context_id(&self) -> String {
        let request = self.inner.applied.borrow();
        format!(
            "playlist:{}|query={}|sort={:?}|descending={}|result={}",
            self.inner.playlist_key,
            request.query,
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
                .filter(|row| row.media_uri == media_uri)
                .map(|_| position);
        }
        if context_id != format!("playlist:{}", self.inner.playlist_key) {
            return None;
        }
        self.inner.sparse.ready_position(|row| {
            row.media_uri == media_uri && row.position.max(0) as usize == source_rank
        })
    }

    pub fn activate(&self, position: u32, queue: playback::QueueHandle) {
        let Some(_row) = self.ready(position) else {
            return;
        };
        self.play(
            position as usize,
            queue,
            playback::QueuePlacement::Now,
            false,
            false,
        );
    }

    pub fn play(
        &self,
        position: usize,
        queue: playback::QueueHandle,
        placement: playback::QueuePlacement,
        collection_start: bool,
        shuffled: bool,
    ) {
        if position >= self.inner.sparse.len() {
            return;
        }
        let row = (!collection_start)
            .then(|| self.ready(position as u32))
            .flatten();
        let applied = self.inner.applied.borrow();
        let request = if collection_start {
            playback::PlayRequest::ordered
        } else {
            playback::PlayRequest::captured
        };
        queue.play(
            request(
                library::QueueInput::PlaylistQuery {
                    key: self.inner.playlist_key,
                    folder: None,
                    filter: applied.query.clone(),
                    sort: applied.settings.sort_key.playlist_entry_sort(),
                    descending: applied.settings.descending,
                    context_id: self.visible_context_id().into(),
                    anchor_entry: row.as_ref().map(|row| row.playlist_entry_key),
                    anchor_uri: row.as_ref().map(|row| row.media_uri.clone()),
                },
                position,
                placement,
                collection_start,
            )
            .shuffled(shuffled),
        );
    }
}
