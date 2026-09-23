use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use library::{Database, TrackRow};
use playback::{PlayRequest, QueuePlacement};

use rufin_core::settings::LibraryListSettings;

use gtk_widgets::library_fields::TrackPresentation;
use gtk_widgets::sparse_model::{SparseObjectModel, SparseRouteModel, SparseSource};

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
    pub order: SparseSource<String>,
    pub disc_sections: Vec<(u32, i64)>,
    pub first_row_position: usize,
    pub first_rows: Vec<T>,
    pub request: TrackProjectionRequest,
}

impl PreparedTrackProjection {
    pub fn from_page(page: library::TrackRoutePage, request: TrackProjectionRequest) -> Self {
        Self {
            order: SparseSource::Query { count: page.count },
            disc_sections: page.disc_sections,
            first_row_position: page.first_row_position,
            first_rows: page.first_rows,
            request,
        }
    }
}

struct TrackModelState<T: TrackPresentation> {
    sparse: Rc<SparseRouteModel<String, T>>,
    request: RefCell<TrackProjectionRequest>,
    applied: Rc<RefCell<TrackProjectionRequest>>,
    source: Option<(library::QueueQuery, Option<library::FolderKey>)>,
}

#[derive(Clone)]
pub struct TrackCollectionModel<T: TrackPresentation = TrackRow>(Rc<TrackModelState<T>>);

impl TrackCollectionModel {
    pub fn new(
        database: Arc<Database>,
        runtime: tokio::runtime::Handle,
        page: library::TrackRoutePage,
        settings: LibraryListSettings,
    ) -> Self {
        let query = page.query.clone();
        let model = Self::with_load(
            runtime,
            SparseSource::Query { count: page.count },
            Some((page.query.queue_query(), page.query.folder)),
            page.first_row_position,
            page.first_rows,
            settings,
            Rc::new(move |_, range, request, cancellation| {
                let database = database.clone();
                let query = query.clone();
                Box::pin(async move {
                    database
                        .query_tracks_page(
                            &query,
                            &request.query,
                            request.settings.sort_key.track_sort(),
                            request.settings.descending,
                            range.start,
                            range.len(),
                            &cancellation,
                        )
                        .await
                        .map_err(|error| error.to_string())
                })
            }),
        );
        model.list_model().set_sections(page.disc_sections);
        model
    }
}

impl<T: TrackPresentation> TrackCollectionModel<T> {
    pub fn with_load(
        runtime: tokio::runtime::Handle,
        order: SparseSource<String>,
        source: Option<(library::QueueQuery, Option<library::FolderKey>)>,
        first_row_position: usize,
        first_rows: Vec<T>,
        settings: LibraryListSettings,
        load: Rc<
            dyn Fn(
                Vec<String>,
                std::ops::Range<usize>,
                TrackProjectionRequest,
                library::ReadCancellation,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<Vec<T>, String>> + Send>,
            >,
        >,
    ) -> Self {
        let applied = Rc::new(RefCell::new(TrackProjectionRequest {
            query: String::new(),
            settings: settings.clone(),
        }));
        let load_request = applied.clone();
        let sparse = SparseRouteModel::new(
            order,
            TRACK_OVERSCAN,
            runtime,
            Rc::new(move |keys, range, cancellation| {
                let request = load_request.borrow().clone();
                load(keys, range, request, cancellation)
            }),
        );
        sparse.seed_matching_at(first_row_position, first_rows, |row| {
            row.media_uri().to_string()
        });
        Self(Rc::new(TrackModelState {
            sparse,
            applied,
            source,
            request: RefCell::new(TrackProjectionRequest {
                query: String::new(),
                settings,
            }),
        }))
    }

    pub fn list_model(&self) -> SparseObjectModel {
        self.0.sparse.list_model()
    }

    pub fn sparse_model(&self) -> Rc<SparseRouteModel<String, T>> {
        Rc::clone(&self.0.sparse)
    }

    pub fn ready_position(&self, media_uri: &str) -> Option<u32> {
        self.0
            .sparse
            .ready_position(|row| row.media_uri() == media_uri)
    }

    pub fn selection_input(&self, positions: &gtk::Bitset) -> library::QueueInput {
        if let Some((query, folder)) = &self.0.source {
            let request = self.0.applied.borrow();
            library::QueueInput::TrackSelection {
                query: query.clone(),
                folder: *folder,
                filter: request.query.clone(),
                sort: request.settings.sort_key.track_sort(),
                descending: request.settings.descending,
                ranges: gtk_widgets::selection::selected_ranges(positions),
            }
        } else {
            library::QueueInput::MediaUris {
                order: gtk_widgets::selection::selected_values(
                    self.0.sparse.order().keys().expect("supplied track keys"),
                    positions,
                )
                .into(),
                provenance: library::QueueProvenance::Manual,
            }
        }
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
            prepared.disc_sections,
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
        self.0
            .sparse
            .update_ready(position as usize, |row| row.set_favorite(favorite))
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
        shuffled: bool,
    ) {
        let order = self.0.sparse.order();
        if anchor_index >= order.len() {
            return;
        }
        let context_id = self.visible_context_id(context_base).into();
        let input = if let Some((query, folder)) = &self.0.source {
            let applied = self.0.applied.borrow();
            library::QueueInput::Query {
                query: query.clone(),
                folder: *folder,
                filter: applied.query.clone(),
                sort: applied.settings.sort_key.track_sort(),
                descending: applied.settings.descending,
                context_id,
                anchor_uri: (!collection_start)
                    .then(|| self.0.sparse.ready(anchor_index as u32))
                    .flatten()
                    .map(|row| row.media_uri().to_string()),
            }
        } else {
            library::QueueInput::Uris {
                order: order.keys().expect("supplied track keys").clone(),
                context_id,
                source_start: 0,
            }
        };
        let request = if collection_start {
            PlayRequest::ordered
        } else {
            PlayRequest::captured
        };
        queue.play(request(input, anchor_index, placement, collection_start).shuffled(shuffled));
    }

    pub fn play_source(
        &self,
        queue: playback::QueueHandle,
        placement: QueuePlacement,
        context_id: String,
        shuffled: bool,
    ) {
        self.play(queue, 0, placement, &context_id, true, shuffled);
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
                    .peek_ready(*position as usize)
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
        media_uri: &str,
        selected_position: u32,
    ) -> Option<u32> {
        self.position_for_current_uri(media_uri, Some(selected_position as usize))
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
            vec![row.media_uri.clone()].into(),
            None,
            0,
            vec![row.clone()],
            LibraryListSettings::for_key(rufin_core::settings::LibraryListKey::History),
            Rc::new(|_, _, _, _| panic!("seeded rows must not request hydration")),
        );
        let pending = model.projection_request();
        assert_eq!(
            model.position_for_current_uri("test:track", Some(0)),
            Some(0)
        );
        assert_eq!(
            model.position_for_current_uri("test:track", Some(1)),
            Some(0)
        );
        assert_eq!(
            model.position_for_current_uri("test:missing", Some(0)),
            None
        );
        let mut presentation = model.settings();
        presentation.layout = rufin_core::settings::LibraryLayout::Grid;
        presentation.row_fields.reverse();
        model.apply_settings(presentation.clone());
        let mut updated = row;
        updated.title = "After".into();
        let prepared = PreparedTrackProjection {
            disc_sections: Vec::new(),
            order: vec![updated.media_uri.clone()].into(),
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
            disc_sections: Vec::new(),
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
