use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use library::{PlaylistEntryKey, PlaylistEntryRow, PlaylistKey};

use rufin_core::settings::LibraryListSettings;

use ui_shared::sparse_model::{SparseObjectModel, SparseRouteModel};

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
    applied: RefCell<PlaylistEntryProjectionRequest>,
}

impl PlaylistEntryModel {
    pub fn new(
        database: Arc<library::Database>,
        runtime: tokio::runtime::Handle,
        playlist_key: PlaylistKey,
        order: Vec<PlaylistEntryKey>,
        first_row_position: usize,
        first_rows: Vec<PlaylistEntryRow>,
        settings: LibraryListSettings,
    ) -> Self {
        let loader_database = Arc::clone(&database);
        let load = Arc::new(
            move |keys: Vec<PlaylistEntryKey>, cancellation: library::ReadCancellation| {
                let database = Arc::clone(&loader_database);
                Box::pin(async move {
                    database
                        .playlist_entry_rows(&keys, &cancellation)
                        .await
                        .map_err(|error| error.to_string())
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        let sparse = SparseRouteModel::new(order, PLAYLIST_ENTRY_OVERSCAN, runtime.clone(), load);
        sparse.seed_matching_at(first_row_position, first_rows, |row| row.playlist_entry_key);
        Self {
            inner: Rc::new(PlaylistEntryModelState {
                playlist_key,
                sparse,
                applied: RefCell::new(PlaylistEntryProjectionRequest {
                    query: String::new(),
                    settings: settings.clone(),
                }),
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

    pub fn order(&self) -> Arc<[PlaylistEntryKey]> {
        self.inner.sparse.order()
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

    pub fn replace_order(&self, order: Vec<PlaylistEntryKey>) {
        self.inner.applied.replace(self.projection_request());
        self.inner.sparse.replace_order(order);
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
        );
    }

    pub fn play(
        &self,
        position: usize,
        queue: playback::QueueHandle,
        placement: playback::QueuePlacement,
        collection_start: bool,
    ) {
        let order = self.order();
        let Some(anchor_entry) = order.get(position).copied() else {
            return;
        };
        let applied = self.inner.applied.borrow();
        let request = if collection_start {
            playback::PlayRequest::ordered
        } else {
            playback::PlayRequest::captured
        };
        queue.play(request(
            library::QueueInput::PlaylistQuery {
                key: self.inner.playlist_key,
                folder: None,
                filter: applied.query.clone(),
                sort: applied.settings.sort_key.playlist_entry_sort(),
                descending: applied.settings.descending,
                context_id: self.visible_context_id().into(),
                anchor_entry: Some(anchor_entry),
                anchor_uri: self.ready(position as u32).map(|row| row.media_uri.clone()),
            },
            position,
            placement,
            collection_start,
        ));
    }
}
