use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use library::{Database, TrackRow};
use playback::{PlayRequest, QueuePlacement};

use rufin_core::settings::LibraryListSettings;

use ui_shared::library_fields::TrackPresentation;
use ui_shared::sparse_model::{SparseObjectModel, SparseRouteModel};

const TRACK_OVERSCAN: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackProjectionRequest {
    pub query: String,
    pub settings: LibraryListSettings,
}

impl TrackProjectionRequest {
    pub fn same_query(&self, other: &Self) -> bool {
        self.query == other.query
            && self.settings.sort_key == other.settings.sort_key
            && self.settings.descending == other.settings.descending
    }
}

#[derive(Clone)]
pub struct PreparedTrackProjection<T = TrackRow> {
    pub order: Vec<String>,
    pub first_row_position: usize,
    pub first_rows: Vec<T>,
    pub request: TrackProjectionRequest,
}

struct TrackModelState<T: TrackPresentation> {
    sparse: Rc<SparseRouteModel<String, T>>,
    request: RefCell<TrackProjectionRequest>,
    applied: RefCell<TrackProjectionRequest>,
    queue_source: RefCell<Option<(library::QueueQuery, Option<library::FolderKey>)>>,
}

#[derive(Clone)]
pub struct TrackCollectionModel<T: TrackPresentation = TrackRow>(Rc<TrackModelState<T>>);

impl TrackCollectionModel {
    pub fn new(
        database: Arc<Database>,
        runtime: tokio::runtime::Handle,
        order: Vec<String>,
        first_row_position: usize,
        first_rows: Vec<TrackRow>,
        settings: LibraryListSettings,
    ) -> Self {
        let rows_database = Arc::clone(&database);
        let load = Arc::new(
            move |keys: Vec<String>, cancellation: library::ReadCancellation| {
                let database = Arc::clone(&rows_database);
                Box::pin(async move {
                    database
                        .track_rows_by_uri(&keys, &cancellation)
                        .await
                        .map_err(|error| error.to_string())
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        Self::with_load(
            runtime,
            order,
            first_row_position,
            first_rows,
            settings,
            load,
        )
    }
}

impl<T: TrackPresentation> TrackCollectionModel<T> {
    pub fn with_load(
        runtime: tokio::runtime::Handle,
        order: Vec<String>,
        first_row_position: usize,
        first_rows: Vec<T>,
        settings: LibraryListSettings,
        load: ui_shared::sparse_model::SparseLoad<String, T>,
    ) -> Self {
        let sparse = SparseRouteModel::new(order, TRACK_OVERSCAN, runtime.clone(), load.clone());
        sparse.seed_matching_at(first_row_position, first_rows, |row| {
            row.media_uri().to_string()
        });
        Self(Rc::new(TrackModelState {
            sparse,
            queue_source: RefCell::new(None),
            applied: RefCell::new(TrackProjectionRequest {
                query: String::new(),
                settings: settings.clone(),
            }),
            request: RefCell::new(TrackProjectionRequest {
                query: String::new(),
                settings,
            }),
        }))
    }

    pub fn list_model(&self) -> SparseObjectModel {
        self.0.sparse.list_model()
    }

    pub fn set_queue_source(&self, query: library::QueueQuery, folder: Option<library::FolderKey>) {
        self.0.queue_source.replace(Some((query, folder)));
    }

    pub fn sparse_model(&self) -> Rc<SparseRouteModel<String, T>> {
        Rc::clone(&self.0.sparse)
    }

    pub fn order(&self) -> Arc<[String]> {
        self.0.sparse.order()
    }

    pub fn source_is_empty(&self) -> bool {
        self.0.sparse.len() == 0
    }

    pub fn visible_count(&self) -> usize {
        self.0.sparse.len()
    }

    pub fn settings(&self) -> LibraryListSettings {
        self.0.request.borrow().settings.clone()
    }

    pub fn projection_request(&self) -> TrackProjectionRequest {
        self.0.request.borrow().clone()
    }

    pub fn set_query(&self, query: &str) -> bool {
        let query = query.trim();
        let mut request = self.0.request.borrow_mut();
        if request.query == query {
            return false;
        }
        request.query = query.to_string();
        true
    }

    pub fn apply_settings(&self, settings: LibraryListSettings) -> bool {
        let mut request = self.0.request.borrow_mut();
        if request.settings == settings {
            return false;
        }
        request.settings = settings;
        true
    }

    pub fn replace_prepared(&self, prepared: PreparedTrackProjection<T>) -> bool {
        if !self.0.request.borrow().same_query(&prepared.request) {
            return false;
        }
        self.0.applied.replace(prepared.request.clone());
        self.0.sparse.replace_prepared_at(
            prepared.order,
            prepared.first_row_position,
            prepared.first_rows,
            |row| row.media_uri().to_string(),
        )
    }

    pub fn resume_initial_demand(&self) {
        self.0.sparse.resume_initial_demand();
    }

    pub fn update_favorite(&self, media_uri: &str, favorite: bool) -> bool {
        let Some(position) = self
            .0
            .sparse
            .ready_position(|row| row.media_uri() == media_uri)
        else {
            return false;
        };
        let Some(track) = self.0.sparse.order().get(position as usize).cloned() else {
            return false;
        };
        self.0
            .sparse
            .update_ready(&track, |row| row.set_favorite(favorite))
    }

    pub fn update_downloaded(&self, media_uri: &str, downloaded: bool) {
        self.0.sparse.update_matching(
            |row| row.media_uri() == media_uri && row.downloaded() != downloaded,
            |row| row.set_downloaded(downloaded),
        );
    }

    pub fn play(
        &self,
        queue: playback::QueueHandle,
        anchor_index: usize,
        placement: QueuePlacement,
        context_base: &str,
        collection_start: bool,
    ) {
        let order = self.0.sparse.order();
        let Some(anchor_uri) = order.get(anchor_index).cloned() else {
            return;
        };
        let input = if let Some((query, folder)) = self.0.queue_source.borrow().as_ref() {
            let applied = self.0.applied.borrow();
            library::QueueInput::Query {
                query: query.clone(),
                folder: *folder,
                filter: applied.query.clone(),
                sort: applied.settings.sort_key.track_sort(),
                descending: applied.settings.descending,
                context_id: self.visible_context_id(context_base).into(),
                anchor_uri: Some(anchor_uri),
            }
        } else {
            library::QueueInput::Uris {
                order,
                context_id: self.visible_context_id(context_base).into(),
                source_start: 0,
            }
        };
        let request = if collection_start {
            PlayRequest::ordered
        } else {
            PlayRequest::captured
        };
        queue.play(request(input, anchor_index, placement, collection_start));
    }

    pub fn play_source(
        &self,
        queue: playback::QueueHandle,
        placement: QueuePlacement,
        context_id: String,
    ) {
        self.play(queue, 0, placement, &context_id, true);
    }

    pub fn visible_context_id(&self, context_base: &str) -> String {
        let request = self.0.applied.borrow();
        format!(
            "{context_base}|query={}|sort={:?}|descending={}|result={}",
            request.query,
            request.settings.sort_key,
            request.settings.descending,
            self.0.sparse.order_id()
        )
    }

    pub fn position_for_current_uri(
        &self,
        media_uri: &str,
        source_rank: Option<usize>,
    ) -> Option<u32> {
        source_rank
            .and_then(|position| u32::try_from(position).ok())
            .filter(|position| {
                self.0
                    .sparse
                    .ready(*position)
                    .is_some_and(|row| row.media_uri() == media_uri)
            })
            .or_else(|| {
                self.0
                    .sparse
                    .ready_position(|row| row.media_uri() == media_uri)
            })
    }

    pub fn selection_position_after_point_change(
        &self,
        _: &str,
        selected_position: u32,
    ) -> Option<u32> {
        Some(selected_position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_projection_survives_presentation_changes_and_refreshes_metadata() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let row = library::HistoryRow {
            media_uri: "test:track".into(),
            title: "Before".into(),
            artist: String::new(),
            album: String::new(),
            album_media_uri: None,
            artists: Vec::new(),
            album_artists: Vec::new(),
            album_display_artist: None,
            artwork_binding: None,
            duration_millis: 1000,
            disc_number: None,
            track_number: None,
            year: None,
            release_date: None,
            date_added: None,
            bpm: None,
            genre: String::new(),
            play_count: 1,
            source_format: None,
            musicbrainz_recording_id: None,
            musicbrainz_release_track_id: None,
            last_played: None,
            favorite: false,
            rating: None,
            is_downloaded: false,
        };
        let model = TrackCollectionModel::with_load(
            runtime.handle().clone(),
            vec![row.media_uri.clone()],
            0,
            vec![row.clone()],
            LibraryListSettings::for_key(rufin_core::settings::LibraryListKey::History),
            Arc::new(|_, _| panic!("seeded rows must not request hydration")),
        );
        let pending = model.projection_request();
        let mut presentation = model.settings();
        presentation.layout = rufin_core::settings::LibraryLayout::Grid;
        presentation.row_fields.reverse();
        model.apply_settings(presentation.clone());
        let mut updated = row;
        updated.title = "After".into();
        let prepared = PreparedTrackProjection {
            order: vec![updated.media_uri.clone()],
            first_row_position: 0,
            first_rows: vec![updated.clone()],
            request: pending,
        };
        assert!(model.replace_prepared(prepared.clone()));
        assert_eq!(model.settings(), presentation);
        assert_eq!(model.sparse_model().ready(0).unwrap().title, "After");

        // A later catalog publication has the same query but new row data.
        updated.title = "Catalog update".into();
        assert!(model.replace_prepared(PreparedTrackProjection {
            first_rows: vec![updated],
            ..prepared.clone()
        }));
        assert_eq!(
            model.sparse_model().ready(0).unwrap().title,
            "Catalog update"
        );

        let visible_context = model.visible_context_id("history");
        presentation.descending = !presentation.descending;
        model.apply_settings(presentation);
        assert_eq!(model.visible_context_id("history"), visible_context);
        assert!(!model.replace_prepared(prepared.clone()));
        model.apply_settings(prepared.request.settings.clone());
        model.set_query("different search");
        assert_eq!(model.visible_context_id("history"), visible_context);
        assert!(!model.replace_prepared(prepared));
        assert_eq!(
            model.sparse_model().ready(0).unwrap().title,
            "Catalog update"
        );
    }
}
