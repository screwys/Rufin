use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use library::{Database, TrackKey, TrackView};
use rufin_core::playback::{PlayRequest, QueuePlacement};

use rufin_core::settings::LibraryListSettings;

use gtk_widgets::library_fields::TrackPresentation;
use gtk_widgets::sparse_model::{SparseObjectModel, SparseRouteModel, SparseSource};

const TRACK_OVERSCAN: usize = 64;

struct TrackModelState {
    sparse: Rc<SparseRouteModel<TrackKey, TrackView>>,
    query: RefCell<library::CatalogQuery>,
    applied: Rc<RefCell<library::CatalogQuery>>,
}

#[derive(Clone)]
pub struct TrackCollectionModel(Rc<TrackModelState>);

impl TrackCollectionModel {
    pub fn new(
        database: Arc<Database>,
        runtime: tokio::runtime::Handle,
        page: library::CatalogPage,
    ) -> Self {
        let library::CatalogRows::Tracks(rows) = page.rows else {
            unreachable!("song page")
        };
        let query = page.query;
        let applied = Rc::new(RefCell::new(query.clone()));
        let load_request = applied.clone();
        let sparse = SparseRouteModel::new(
            SparseSource::Query { count: page.count },
            TRACK_OVERSCAN,
            runtime,
            Rc::new(move |_, range, cancellation| {
                let database = database.clone();
                let request = load_request.borrow().clone();
                Box::pin(async move {
                    request
                        .rows(&database, range.start, range.len(), &cancellation)
                        .await
                        .map(|rows| match rows {
                            library::CatalogRows::Tracks(rows) => rows,
                            _ => unreachable!("song projection"),
                        })
                        .map_err(|error| error.to_string())
                })
            }),
        );
        sparse.seed_matching_at(page.first_row_position, rows, |row| row.track_key);
        sparse.list_model().set_sections(page.disc_sections);
        Self(Rc::new(TrackModelState {
            sparse,
            applied,
            query: RefCell::new(query),
        }))
    }

    pub fn list_model(&self) -> SparseObjectModel {
        self.0.sparse.list_model()
    }

    pub fn sparse_model(&self) -> Rc<SparseRouteModel<TrackKey, TrackView>> {
        Rc::clone(&self.0.sparse)
    }

    pub fn ready_position(&self, media_uri: &str) -> Option<u32> {
        self.0
            .sparse
            .ready_position(|row| row.media_uri() == media_uri)
    }

    pub fn selection_input(&self, positions: &gtk::Bitset) -> library::TrackSelection {
        library::TrackSelection::Query {
            query: self.0.applied.borrow().clone(),
            ranges: Some(gtk_widgets::selection::selected_ranges(positions)),
        }
    }

    pub fn source_is_empty(&self) -> bool {
        self.0.sparse.len() == 0
    }

    pub fn visible_count(&self) -> usize {
        self.0.sparse.len()
    }

    pub fn query(&self) -> library::CatalogQuery {
        let mut query = self.0.query.borrow_mut();
        if let library::CatalogSubject::Tracks {
            scope: library::TrackScope::Smart { now, .. },
            ..
        } = &mut query.subject
        {
            *now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs() as i64);
        }
        query.clone()
    }

    pub fn set_query(&self, query: &str) -> bool {
        let query = query.trim();
        let mut current = self.0.query.borrow_mut();
        if current.filter == query {
            return false;
        }
        current.filter = query.to_string();
        true
    }

    pub fn set_scope(&self, scope: library::TrackScope) {
        if let library::CatalogSubject::Tracks { scope: current, .. } =
            &mut self.0.query.borrow_mut().subject
        {
            *current = scope;
        }
    }

    pub fn apply_settings(&self, settings: &LibraryListSettings) {
        let mut query = self.0.query.borrow_mut();
        if let library::CatalogSubject::Tracks { sort, .. } = &mut query.subject {
            *sort = Some(settings.sort_key.catalog_sort());
        }
        query.descending = settings.descending;
    }

    pub fn replace_page(&self, page: library::CatalogPage) -> bool {
        if *self.0.query.borrow() != page.query {
            return false;
        }
        let library::CatalogRows::Tracks(rows) = page.rows else {
            unreachable!("song page")
        };
        self.0.applied.replace(page.query);
        self.0.sparse.replace_prepared_at(
            SparseSource::Query { count: page.count },
            page.first_row_position,
            rows,
            page.disc_sections,
            |row| row.track_key,
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
        queue: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
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
        let selection = self.0.applied.borrow().clone().selection();
        let constructor = if collection_start {
            PlayRequest::ordered
        } else {
            PlayRequest::captured
        };
        let mut request = constructor(selection, anchor_index, placement, collection_start)
            .with_context(self.visible_context_id(context_base).into())
            .shuffled(shuffled);
        if !collection_start && let Some(row) = self.0.sparse.ready(anchor_index as u32) {
            request = request.with_anchor(row.track_key, None);
        }
        queue.play_selection(request);
    }

    pub fn play_source(
        &self,
        queue: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
        placement: QueuePlacement,
        context_id: String,
        shuffled: bool,
    ) {
        self.play(queue, 0, placement, &context_id, true, shuffled);
    }

    pub fn visible_context_id(&self, context_base: &str) -> String {
        let query = self.0.applied.borrow();
        format!(
            "{context_base}|query={}|sort={:?}|descending={}|result={}",
            query.filter,
            query.track_sort_option(),
            query.descending,
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
        let row = library::TrackView {
            track_key: TrackKey::from_raw(1),
            source_key: None,
            source_id: None,
            object_id: None,
            media_uri: "test:track".into(),
            artwork_binding: None,
            metadata: library::TrackMetadata {
                title: "Before".into(),
                duration_millis: 1000,
                ..Default::default()
            },
            state: library::TrackState {
                play_count: 1,
                ..Default::default()
            },
            access: Default::default(),
            links: Default::default(),
        };
        let settings = LibraryListSettings::for_key(rufin_core::settings::LibraryListKey::History);
        let query = library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::History { source: None },
                sort: Some(settings.sort_key.catalog_sort()),
            },
            descending: settings.descending,
            ..library::CatalogQuery::tracks(library::TrackScope::History { source: None })
        };
        let directory = tempfile::tempdir().unwrap();
        let database = runtime
            .block_on(Database::open(directory.path().join("catalog.sqlite3")))
            .unwrap();
        let model = TrackCollectionModel::new(
            Arc::new(database),
            runtime.handle().clone(),
            library::CatalogPage {
                query,
                count: 1,
                first_row_position: 0,
                rows: library::CatalogRows::Tracks(vec![row.clone()]),
                disc_sections: Vec::new(),
            },
        );
        let pending = model.query();
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
        let mut presentation = settings.clone();
        presentation.layout = rufin_core::settings::LibraryLayout::Grid;
        presentation.row_fields.reverse();
        model.apply_settings(&presentation);
        let mut updated = row;
        updated.metadata.title = "After".into();
        let prepared = library::CatalogPage {
            disc_sections: Vec::new(),
            count: 1,
            first_row_position: 0,
            rows: library::CatalogRows::Tracks(vec![updated.clone()]),
            query: pending,
        };
        assert!(model.replace_page(prepared.clone()));
        assert_eq!(model.query(), prepared.query);
        assert_eq!(
            model.sparse_model().ready(0).unwrap().metadata.title,
            "After"
        );

        // A later catalog publication has the same query but new row data.
        updated.metadata.title = "Catalog update".into();
        assert!(model.replace_page(library::CatalogPage {
            disc_sections: Vec::new(),
            rows: library::CatalogRows::Tracks(vec![updated]),
            ..prepared.clone()
        }));
        assert_eq!(
            model.sparse_model().ready(0).unwrap().metadata.title,
            "Catalog update"
        );

        let visible_context = model.visible_context_id("history");
        presentation.descending = !presentation.descending;
        model.apply_settings(&presentation);
        assert_eq!(model.visible_context_id("history"), visible_context);
        assert!(!model.replace_page(prepared.clone()));
        model.apply_settings(&settings);
        model.set_query("different search");
        assert_eq!(model.visible_context_id("history"), visible_context);
        assert!(!model.replace_page(prepared));
        assert_eq!(
            model.sparse_model().ready(0).unwrap().metadata.title,
            "Catalog update"
        );
    }
}
