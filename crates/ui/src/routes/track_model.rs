use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use library::{Database, SourceKey, TrackKey, TrackRow};
use playback::{LoadedPlayRequest, PlaybackMedia, QueuePlacement, SourceSessionEpoch};

use crate::LibraryListSettings;

use super::sparse_model::{SparseObjectModel, SparseRouteModel};

const TRACK_OVERSCAN: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TrackProjectionRequest {
    pub(crate) query: String,
    pub(crate) settings: LibraryListSettings,
}

#[derive(Clone)]
pub(crate) struct PreparedTrackProjection {
    pub(crate) order: Vec<TrackKey>,
    pub(crate) first_rows: Vec<TrackRow>,
    pub(crate) request: TrackProjectionRequest,
}

struct TrackModelState {
    source_key: SourceKey,
    source_session_epoch: SourceSessionEpoch,
    sparse: Rc<SparseRouteModel<TrackKey, TrackRow>>,
    database: Arc<Database>,
    runtime: tokio::runtime::Handle,
    request: RefCell<TrackProjectionRequest>,
}

#[derive(Clone)]
pub(crate) struct TrackCollectionModel(Rc<TrackModelState>);

impl TrackCollectionModel {
    pub(crate) fn new(
        source_key: SourceKey,
        source_session_epoch: SourceSessionEpoch,
        database: Arc<Database>,
        runtime: tokio::runtime::Handle,
        order: Vec<TrackKey>,
        first_rows: Vec<TrackRow>,
        settings: LibraryListSettings,
    ) -> Self {
        let rows_database = Arc::clone(&database);
        let load = Arc::new(
            move |keys: Vec<TrackKey>, cancellation: library::ReadCancellation| {
                let database = Arc::clone(&rows_database);
                Box::pin(async move {
                    database
                        .track_rows(source_key, &keys, &cancellation)
                        .await
                        .map_err(|error| error.to_string())
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        let sparse = SparseRouteModel::new(order, TRACK_OVERSCAN, runtime.clone(), load);
        sparse.seed_matching(first_rows, |row| row.track_key);
        Self(Rc::new(TrackModelState {
            source_key,
            source_session_epoch,
            sparse,
            database,
            runtime: runtime.clone(),
            request: RefCell::new(TrackProjectionRequest {
                query: String::new(),
                settings,
            }),
        }))
    }

    pub(crate) fn list_model(&self) -> SparseObjectModel {
        self.0.sparse.list_model()
    }

    pub(crate) fn sparse_model(&self) -> Rc<SparseRouteModel<TrackKey, TrackRow>> {
        Rc::clone(&self.0.sparse)
    }

    pub(crate) fn order(&self) -> Arc<[TrackKey]> {
        self.0.sparse.order()
    }

    pub(crate) fn source_key(&self) -> SourceKey {
        self.0.source_key
    }

    pub(crate) fn source_session_epoch(&self) -> SourceSessionEpoch {
        self.0.source_session_epoch
    }

    pub(crate) fn source_is_empty(&self) -> bool {
        self.0.sparse.len() == 0
    }

    pub(crate) fn visible_count(&self) -> usize {
        self.0.sparse.len()
    }

    pub(crate) fn settings(&self) -> LibraryListSettings {
        self.0.request.borrow().settings.clone()
    }

    pub(crate) fn projection_request(&self) -> TrackProjectionRequest {
        self.0.request.borrow().clone()
    }

    pub(crate) fn set_query(&self, query: &str) -> bool {
        let query = query.trim();
        let mut request = self.0.request.borrow_mut();
        if request.query == query {
            return false;
        }
        request.query = query.to_string();
        true
    }

    pub(crate) fn apply_settings(&self, settings: LibraryListSettings) -> bool {
        let mut request = self.0.request.borrow_mut();
        if request.settings == settings {
            return false;
        }
        request.settings = settings;
        true
    }

    pub(crate) fn replace_prepared(&self, prepared: PreparedTrackProjection) -> bool {
        if *self.0.request.borrow() != prepared.request {
            return false;
        }
        self.0
            .sparse
            .replace_prepared(prepared.order, prepared.first_rows, |row| row.track_key)
    }

    pub(crate) fn ready(&self, position: u32) -> Option<Arc<TrackRow>> {
        self.0.sparse.ready(position)
    }

    pub(crate) fn update_favorite(&self, track: TrackKey, favorite: bool) -> bool {
        self.0
            .sparse
            .update_ready(&track, |row| row.favorite = favorite)
    }

    pub(crate) fn play_request(
        &self,
        anchor_index: usize,
        placement: QueuePlacement,
        context_base: &str,
        shuffled_start: bool,
    ) -> Option<LoadedPlayRequest> {
        let anchor = self.ready(u32::try_from(anchor_index).ok()?)?;
        LoadedPlayRequest::context(
            self.0.source_key,
            self.0.source_session_epoch,
            self.0.sparse.order(),
            PlaybackMedia::from((*anchor).clone()),
            anchor_index,
            placement,
            self.visible_context_id(context_base),
            shuffled_start,
        )
    }

    pub(crate) fn play_source(
        &self,
        queue: playback::QueueHandle,
        placement: QueuePlacement,
        context_id: String,
        shuffled_start: bool,
    ) {
        let order = self.0.sparse.order();
        let Some(anchor_key) = order.first().copied() else {
            return;
        };
        let database = Arc::clone(&self.0.database);
        let source = self.0.source_key;
        let epoch = self.0.source_session_epoch;
        let runtime = self.0.runtime.clone();
        runtime.spawn(async move {
            let cancellation = library::ReadCancellation::new();
            let anchor = database
                .track_rows(source, &[anchor_key], &cancellation)
                .await
                .ok()
                .and_then(|mut rows| rows.pop())
                .map(PlaybackMedia::from);
            let Some(anchor) = anchor else {
                return;
            };
            let Some(request) = LoadedPlayRequest::context(
                source,
                epoch,
                order,
                anchor,
                0,
                placement,
                context_id,
                shuffled_start,
            ) else {
                return;
            };
            queue.play_loaded(request);
        });
    }

    pub(crate) fn visible_context_id(&self, context_base: &str) -> String {
        let request = self.0.request.borrow();
        format!(
            "{context_base}|query={}|sort={:?}|descending={}",
            request.query, request.settings.sort_key, request.settings.descending
        )
    }

    pub(crate) fn position_for_current(
        &self,
        track_key: &TrackKey,
        source_rank: Option<usize>,
    ) -> Option<u32> {
        let order = self.0.sparse.order();
        source_rank
            .filter(|position| order.get(*position) == Some(track_key))
            .and_then(|position| u32::try_from(position).ok())
            .or_else(|| {
                self.0
                    .sparse
                    .ready_position(|row| row.track_key == *track_key)
            })
    }

    pub(crate) fn selection_position_after_point_change(
        &self,
        _: &TrackKey,
        selected_position: u32,
    ) -> Option<u32> {
        Some(selected_position)
    }
}

pub(crate) fn history_played_at_text(played_at: i64) -> String {
    gtk::glib::DateTime::from_unix_local(played_at)
        .and_then(|date| date.format("%Y-%m-%d %H:%M"))
        .map(String::from)
        .unwrap_or_else(|_| played_at.to_string())
}
