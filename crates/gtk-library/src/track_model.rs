use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use library::{Database, TrackKey, TrackView};
use rufin_core::playback::{PlayRequest, QueuePlacement};

use rufin_core::settings::LibraryListSettings;

use gtk_widgets::library_fields::TrackPresentation;
use gtk_widgets::sparse_model::{SparseObjectModel, SparseRouteModel, SparseSource};

const TRACK_OVERSCAN: usize = 64;

#[derive(Clone, PartialEq)]
enum TrackRead {
    Members(library::MemberRead),
    Keys(Arc<[TrackKey]>),
}

impl TrackRead {
    fn selection(&self, ranges: Option<Vec<std::ops::Range<usize>>>) -> library::MemberSelection {
        match self {
            Self::Members(read) => library::MemberSelection::Members {
                read: read.clone(),
                ranges,
            },
            Self::Keys(keys) => library::MemberSelection::Keys {
                source: None,
                keys: ranges.map_or_else(
                    || keys.clone(),
                    |ranges| {
                        ranges
                            .into_iter()
                            .flat_map(|range| keys[range].iter().copied())
                            .collect()
                    },
                ),
            },
        }
    }
}

struct TrackModelState {
    sparse: Rc<SparseRouteModel<TrackKey, TrackView>>,
    query: RefCell<TrackRead>,
    applied: Rc<RefCell<TrackRead>>,
}

#[derive(Clone)]
pub struct TrackCollectionModel(Rc<TrackModelState>);

impl TrackCollectionModel {
    pub fn new(
        database: Arc<Database>,
        runtime: tokio::runtime::Handle,
        page: rufin_core::browse::MemberPage,
    ) -> Self {
        let rows = page
            .page
            .rows
            .into_iter()
            .map(library::MemberRow::into_track)
            .collect();
        let query = TrackRead::Members(page.read);
        let applied = Rc::new(RefCell::new(query.clone()));
        let load_request = applied.clone();
        let sparse = SparseRouteModel::new(
            SparseSource::Query {
                count: page.page.count,
            },
            TRACK_OVERSCAN,
            runtime,
            Rc::new(move |_, range, cancellation| {
                let database = database.clone();
                let request = load_request.borrow().clone();
                Box::pin(async move {
                    let TrackRead::Members(request) = request else {
                        unreachable!()
                    };
                    request
                        .rows(&database, range.start, range.len(), &cancellation)
                        .await
                        .map(|rows| {
                            rows.into_iter()
                                .map(library::MemberRow::into_track)
                                .collect()
                        })
                        .map_err(|error| error.to_string())
                })
            }),
        );
        sparse.seed_matching_at(page.page.first_row_position, rows, |row| row.track_key);
        sparse.list_model().set_sections(page.disc_sections);
        Self(Rc::new(TrackModelState {
            sparse,
            applied,
            query: RefCell::new(query),
        }))
    }

    pub fn related(
        database: Arc<Database>,
        runtime: tokio::runtime::Handle,
        rows: Vec<TrackView>,
    ) -> Self {
        let keys: Arc<[TrackKey]> = rows.iter().map(|row| row.track_key).collect();
        let sparse = SparseRouteModel::new(
            SparseSource::Keys(keys.clone()),
            TRACK_OVERSCAN,
            runtime,
            Rc::new(move |keys, _, cancel| {
                let database = database.clone();
                Box::pin(async move {
                    database
                        .tracks(&keys, &cancel)
                        .await
                        .map_err(|error| error.to_string())
                })
            }),
        );
        sparse.seed_matching_at(0, rows, |row| row.track_key);
        let query = TrackRead::Keys(keys);
        Self(Rc::new(TrackModelState {
            sparse,
            applied: Rc::new(RefCell::new(query.clone())),
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

    pub fn selection_input(&self, positions: &gtk::Bitset) -> library::MemberSelection {
        self.0
            .applied
            .borrow()
            .selection(Some(gtk_widgets::selection::selected_ranges(positions)))
    }

    pub fn source_is_empty(&self) -> bool {
        self.0.sparse.len() == 0
    }

    pub fn visible_count(&self) -> usize {
        self.0.sparse.len()
    }

    pub fn query(&self) -> library::MemberRead {
        let mut current = self.0.query.borrow_mut();
        let TrackRead::Members(query) = &mut *current else {
            unreachable!("Related rows have no collection read")
        };
        if let library::Collection::Smart { now, .. } = &mut query.collection {
            *now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs() as i64);
        }
        query.clone()
    }

    pub fn set_query(&self, query: &str) -> bool {
        let query = query.trim();
        let mut read = self.0.query.borrow_mut();
        let TrackRead::Members(current) = &mut *read else {
            return false;
        };
        if current.filter == query {
            return false;
        }
        current.filter = query.to_string();
        true
    }

    pub fn set_scope(&self, scope: library::Collection) {
        if let TrackRead::Members(read) = &mut *self.0.query.borrow_mut() {
            read.collection = scope;
        }
    }

    pub fn apply_settings(&self, settings: &LibraryListSettings) {
        let mut current = self.0.query.borrow_mut();
        let TrackRead::Members(query) = &mut *current else {
            return;
        };
        query.sort = Some(settings.sort_key.catalog_sort());
        query.descending = settings.descending;
    }

    pub fn replace_page(&self, page: rufin_core::browse::MemberPage) -> bool {
        if *self.0.query.borrow() != TrackRead::Members(page.read.clone()) {
            return false;
        }
        let rows = page
            .page
            .rows
            .into_iter()
            .map(library::MemberRow::into_track)
            .collect();
        self.0.applied.replace(TrackRead::Members(page.read));
        self.0.sparse.replace_prepared_at(
            SparseSource::Query {
                count: page.page.count,
            },
            page.page.first_row_position,
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
        let selection = self.0.applied.borrow().selection(None);
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
        match &*self.0.applied.borrow() {
            TrackRead::Members(query) => format!(
                "{context_base}|query={}|sort={:?}|descending={}|result={}",
                query.filter,
                query.sort,
                query.descending,
                self.0.sparse.order_id()
            ),
            TrackRead::Keys(_) => format!("{context_base}|result={}", self.0.sparse.order_id()),
        }
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
        let query = library::MemberRead {
            sort: Some(settings.sort_key.catalog_sort()),
            descending: settings.descending,
            ..library::MemberRead::new(library::Collection::History { source: None })
        };
        let directory = tempfile::tempdir().unwrap();
        let database = runtime
            .block_on(Database::open(directory.path().join("catalog.sqlite3")))
            .unwrap();
        let model = TrackCollectionModel::new(
            Arc::new(database),
            runtime.handle().clone(),
            rufin_core::browse::MemberPage {
                read: query,
                page: library::Page {
                    count: 1,
                    first_row_position: 0,
                    rows: vec![library::MemberRow::Track(row.clone())],
                },
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
        let prepared = rufin_core::browse::MemberPage {
            disc_sections: Vec::new(),
            page: library::Page {
                count: 1,
                first_row_position: 0,
                rows: vec![library::MemberRow::Track(updated.clone())],
            },
            read: pending,
        };
        assert!(model.replace_page(prepared.clone()));
        assert_eq!(model.query(), prepared.read);
        assert_eq!(
            model.sparse_model().ready(0).unwrap().metadata.title,
            "After"
        );

        // A later catalog publication has the same query but new row data.
        updated.metadata.title = "Catalog update".into();
        assert!(model.replace_page(rufin_core::browse::MemberPage {
            disc_sections: Vec::new(),
            page: library::Page {
                rows: vec![library::MemberRow::Track(updated)],
                ..prepared.page.clone()
            },
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
