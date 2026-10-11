use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use library::{Database, TrackKey, TrackRow};
use playback::{PlayRequest, QueuePlacement};

use rufin_core::settings::LibraryListSettings;

use gtk_widgets::library_fields::TrackPresentation;
use gtk_widgets::sparse_model::{SparseObjectModel, SparseRouteModel, SparseSource};

pub trait CollectionTrack: TrackPresentation {
    fn key(&self) -> TrackKey;
}
impl CollectionTrack for TrackRow {
    fn key(&self) -> TrackKey {
        self.track_key
    }
}
impl CollectionTrack for library::HistoryRow {
    fn key(&self) -> TrackKey {
        self.track_key
    }
}

const TRACK_OVERSCAN: usize = 64;

struct TrackModelState<T: CollectionTrack> {
    sparse: Rc<SparseRouteModel<TrackKey, T>>,
    query: RefCell<Option<library::SongQuery>>,
    applied: Rc<RefCell<Option<library::SongQuery>>>,
}

#[derive(Clone)]
pub struct TrackCollectionModel<T: CollectionTrack = TrackRow>(Rc<TrackModelState<T>>);

impl TrackCollectionModel {
    pub fn new(
        database: Arc<Database>,
        runtime: tokio::runtime::Handle,
        page: library::TrackRoutePage,
    ) -> Self {
        let model = Self::with_load(
            runtime,
            SparseSource::Query { count: page.count },
            Some(page.query.clone()),
            page.first_row_position,
            page.first_rows,
            Rc::new(move |_, range, request, cancellation| {
                let database = database.clone();
                Box::pin(async move {
                    database
                        .song_rows(
                            request.as_ref().expect("query-backed track route"),
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

impl<T: CollectionTrack> TrackCollectionModel<T> {
    pub fn with_load(
        runtime: tokio::runtime::Handle,
        order: SparseSource<TrackKey>,
        query: Option<library::SongQuery>,
        first_row_position: usize,
        first_rows: Vec<T>,
        load: Rc<
            dyn Fn(
                Vec<TrackKey>,
                std::ops::Range<usize>,
                Option<library::SongQuery>,
                library::ReadCancellation,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<Vec<T>, String>> + Send>,
            >,
        >,
    ) -> Self {
        let applied = Rc::new(RefCell::new(query.clone()));
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
        sparse.seed_matching_at(first_row_position, first_rows, |row| row.key());
        Self(Rc::new(TrackModelState {
            sparse,
            applied,
            query: RefCell::new(query),
        }))
    }

    pub fn list_model(&self) -> SparseObjectModel {
        self.0.sparse.list_model()
    }

    pub fn sparse_model(&self) -> Rc<SparseRouteModel<TrackKey, T>> {
        Rc::clone(&self.0.sparse)
    }

    pub fn ready_position(&self, media_uri: &str) -> Option<u32> {
        self.0
            .sparse
            .ready_position(|row| row.media_uri() == media_uri)
    }

    pub fn selection_input(&self, positions: &gtk::Bitset) -> library::SongSelection {
        if let Some(keys) = self.0.sparse.order().keys() {
            library::SongSelection::Tracks(
                gtk_widgets::selection::selected_values(keys, positions).into(),
            )
        } else {
            library::SongSelection::Query {
                query: self
                    .0
                    .applied
                    .borrow()
                    .as_ref()
                    .expect("query-backed track route")
                    .clone(),
                ranges: Some(gtk_widgets::selection::selected_ranges(positions)),
            }
        }
    }

    pub fn source_is_empty(&self) -> bool {
        self.0.sparse.len() == 0
    }

    pub fn visible_count(&self) -> usize {
        self.0.sparse.len()
    }

    pub fn query(&self) -> library::SongQuery {
        let mut query = self.0.query.borrow_mut();
        let query = query.as_mut().expect("query-backed track route");
        if let library::SongScope::Smart { now, .. } = &mut query.scope {
            *now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs() as i64);
        }
        query.clone()
    }

    pub fn set_query(&self, query: &str) -> bool {
        let query = query.trim();
        let mut request = self.0.query.borrow_mut();
        let Some(current) = request.as_mut() else {
            return false;
        };
        if current.filter == query {
            return false;
        }
        current.filter = query.to_string();
        true
    }

    pub fn set_scope(&self, scope: library::SongScope) {
        if let Some(query) = self.0.query.borrow_mut().as_mut() {
            query.scope = scope;
        }
    }

    pub fn apply_settings(&self, settings: &LibraryListSettings) {
        if let Some(query) = self.0.query.borrow_mut().as_mut() {
            query.sort = Some(settings.sort_key.track_sort());
            query.descending = settings.descending;
        }
    }

    pub fn replace_page(&self, page: library::TrackRoutePage<T>) -> bool {
        if self.0.query.borrow().as_ref() != Some(&page.query) {
            return false;
        }
        self.0.applied.replace(Some(page.query));
        self.0.sparse.replace_prepared_at(
            SparseSource::Query { count: page.count },
            page.first_row_position,
            page.first_rows,
            page.disc_sections,
            |row| row.key(),
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
        let selection = order.keys().map_or_else(
            || {
                self.0
                    .applied
                    .borrow()
                    .as_ref()
                    .expect("query-backed track route")
                    .clone()
                    .selection()
            },
            |keys| library::SongSelection::Tracks(Arc::clone(keys)),
        );
        let constructor = if collection_start {
            PlayRequest::ordered
        } else {
            PlayRequest::captured
        };
        let mut request = constructor(selection, anchor_index, placement, collection_start)
            .with_context(self.visible_context_id(context_base).into())
            .shuffled(shuffled);
        if !collection_start && let Some(row) = self.0.sparse.ready(anchor_index as u32) {
            request = request.with_anchor(row.key(), None);
        }
        queue.play(request);
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
        let query = self.0.applied.borrow();
        format!(
            "{context_base}|query={}|sort={:?}|descending={}|result={}",
            query.as_ref().map_or("", |query| query.filter.as_str()),
            query.as_ref().and_then(|query| query.sort),
            query.as_ref().is_some_and(|query| query.descending),
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
            track_key: TrackKey::from_raw(1),
            audio_properties: Default::default(),
            source_name: None,
            source_path: None,
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
        let settings = LibraryListSettings::for_key(rufin_core::settings::LibraryListKey::History);
        let query = library::SongQuery {
            sort: Some(settings.sort_key.track_sort()),
            descending: settings.descending,
            ..library::SongQuery::new(library::SongScope::History { source: None })
        };
        let model = TrackCollectionModel::with_load(
            runtime.handle().clone(),
            SparseSource::Query { count: 1 },
            Some(query),
            0,
            vec![row.clone()],
            Rc::new(|_, _, _, _| panic!("seeded rows must not request hydration")),
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
        updated.title = "After".into();
        let prepared = library::TrackRoutePage {
            disc_sections: Vec::new(),
            count: 1,
            first_row_position: 0,
            first_rows: vec![updated.clone()],
            query: pending,
        };
        assert!(model.replace_page(prepared.clone()));
        assert_eq!(model.query(), prepared.query);
        assert_eq!(model.sparse_model().ready(0).unwrap().title, "After");

        // A later catalog publication has the same query but new row data.
        updated.title = "Catalog update".into();
        assert!(model.replace_page(library::TrackRoutePage {
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
        model.apply_settings(&presentation);
        assert_eq!(model.visible_context_id("history"), visible_context);
        assert!(!model.replace_page(prepared.clone()));
        model.apply_settings(&settings);
        model.set_query("different search");
        assert_eq!(model.visible_context_id("history"), visible_context);
        assert!(!model.replace_page(prepared));
        assert_eq!(
            model.sparse_model().ready(0).unwrap().title,
            "Catalog update"
        );
    }
}
