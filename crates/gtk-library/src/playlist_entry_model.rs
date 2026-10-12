use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use library::{PlaylistEntryKey, PlaylistEntryRow, PlaylistKey};

use rufin_core::settings::LibraryListSettings;

use gtk_widgets::sparse_model::{SparseObjectModel, SparseRouteModel, SparseSource};

const PLAYLIST_ENTRY_OVERSCAN: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistEntryProjectionRequest {
    pub query: library::SongQuery,
    pub settings: LibraryListSettings,
    pub first_row_position: usize,
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
        let request = PlaylistEntryProjectionRequest {
            query: rufin_core::browse::BrowseRequest::new(
                &rufin_core::route::Route::PlaylistDetail(playlist_key),
                rufin_core::route::CollectionCategory::Tracks,
                None,
                None,
                &settings,
                8,
            )
            .songs()
            .expect("playlist route"),
            settings: settings.clone(),
            first_row_position,
        };
        let applied = Rc::new(RefCell::new(request.clone()));
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
                        .song_playlist_rows(&request.query, range.start, range.len(), &cancellation)
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
                request: RefCell::new(request),
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

    pub fn selection_input(&self, positions: &gtk::Bitset) -> library::SongSelection {
        library::SongSelection::Query {
            query: self.inner.applied.borrow().query.clone(),
            ranges: Some(gtk_widgets::selection::selected_ranges(positions)),
        }
    }

    pub fn playlist_key(&self) -> PlaylistKey {
        self.inner.playlist_key
    }

    pub fn source_is_empty(&self) -> bool {
        self.inner.sparse.len() == 0
    }

    pub fn projection_request(&self) -> PlaylistEntryProjectionRequest {
        let mut request = self.inner.request.borrow().clone();
        let applied = self.inner.applied.borrow();
        request.first_row_position = if request.query == applied.query
            && request.settings.sort_key == applied.settings.sort_key
            && request.settings.descending == applied.settings.descending
        {
            self.inner.sparse.first_live_position().unwrap_or(0)
        } else {
            0
        };
        request
    }

    pub fn set_query(&self, query: &str) -> bool {
        let query = query.trim();
        let mut request = self.inner.request.borrow_mut();
        if request.query.filter == query {
            return false;
        }
        request.query.filter = query.to_string();
        true
    }

    pub fn apply_settings(&self, settings: LibraryListSettings) -> bool {
        let mut request = self.inner.request.borrow_mut();
        if request.settings == settings {
            return false;
        }
        request.query.sort = Some(settings.sort_key.track_sort());
        request.query.descending = settings.descending;
        request.settings = settings;
        true
    }

    pub fn replace_prepared(
        &self,
        count: usize,
        request: PlaylistEntryProjectionRequest,
        rows: Vec<PlaylistEntryRow>,
    ) {
        let first = request.first_row_position.min(count);
        self.inner.applied.replace(request);
        self.inner.sparse.replace_prepared_at(
            SparseSource::Query { count },
            first,
            rows,
            Vec::new(),
            |row| row.playlist_entry_key,
        );
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
            request = request.with_anchor(row.track_key, Some(row.playlist_entry_key));
        }
        queue.play_selection(request);
    }
}
