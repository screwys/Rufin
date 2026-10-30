use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use library::{Catalog, Row, TrackKey, TrackRow};
use rufin_core::playback::{PlayRequest, QueuePlacement};

use gtk_widgets::library_fields::TrackPresentation;
use gtk_widgets::mounted_route::{RouteRead, RouteView, view_rows};
use gtk_widgets::sparse_model::{SparseObjectModel, SparseRouteModel, SparseSource};

const TRACK_OVERSCAN: usize = 64;

#[derive(Clone)]
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

/// A track list row of any track page.
pub(crate) fn track_row(row: Row) -> Option<TrackRow> {
    match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => Some(track),
        _ => None,
    }
}

struct TrackModelState {
    sparse: Rc<SparseRouteModel<TrackKey, TrackRow>>,
    /// The search the page wants; a page read for another search is stale.
    filter: RefCell<String>,
    applied: Rc<RefCell<TrackRead>>,
}

#[derive(Clone)]
pub struct TrackCollectionModel(Rc<TrackModelState>);

impl TrackCollectionModel {
    /// The tracks of `view`, seeded with the rows `read` took with it.
    pub fn new(runtime: tokio::runtime::Handle, view: &RouteView, read: RouteRead) -> Self {
        let members = read
            .view
            .members()
            .cloned()
            .expect("a track page reads members");
        let filter = RefCell::new(members.filter.clone());
        let sparse = SparseRouteModel::new(
            SparseSource::Query {
                count: read.view.len(),
            },
            TRACK_OVERSCAN,
            runtime,
            view_rows(view, track_row),
        );
        sparse.seed_matching_at(
            read.first,
            read.rows.into_iter().filter_map(track_row).collect(),
            |row| row.track_key,
        );
        sparse.list_model().set_sections(read.discs);
        Self(Rc::new(TrackModelState {
            sparse,
            applied: Rc::new(RefCell::new(TrackRead::Members(members))),
            filter,
        }))
    }

    /// An artist page's favorite tracks, which `members` lists.
    pub fn favorites(
        runtime: tokio::runtime::Handle,
        view: &RouteView,
        members: library::MemberRead,
        rows: Vec<Row>,
    ) -> Self {
        let count = view.borrow().favorites_len().unwrap_or(0);
        let load_view = Rc::clone(view);
        let sparse = SparseRouteModel::new(
            SparseSource::Query { count },
            TRACK_OVERSCAN,
            runtime,
            Rc::new(move |_, range, cancel| {
                let view = Arc::clone(&load_view.borrow());
                Box::pin(async move {
                    view.favorite_rows(range, &cancel)
                        .await
                        .map(|rows| rows.into_iter().filter_map(track_row).collect())
                        .map_err(|error| error.to_string())
                })
            }),
        );
        sparse.seed_matching_at(0, rows.into_iter().filter_map(track_row).collect(), |row| {
            row.track_key
        });
        Self(Rc::new(TrackModelState {
            sparse,
            filter: RefCell::new(members.filter.clone()),
            applied: Rc::new(RefCell::new(TrackRead::Members(members))),
        }))
    }

    pub fn replace_favorites(&self, members: library::MemberRead, count: usize, rows: Vec<Row>) {
        self.0.applied.replace(TrackRead::Members(members));
        self.0.sparse.replace_prepared_at(
            SparseSource::Query { count },
            0,
            rows.into_iter().filter_map(track_row).collect(),
            Vec::new(),
            |row| row.track_key,
        );
    }

    pub fn related(
        catalog: Arc<Catalog>,
        runtime: tokio::runtime::Handle,
        rows: Vec<TrackRow>,
    ) -> Self {
        let keys: Arc<[TrackKey]> = rows.iter().map(|row| row.track_key).collect();
        let sparse = SparseRouteModel::new(
            SparseSource::Keys(keys.clone()),
            TRACK_OVERSCAN,
            runtime,
            Rc::new(move |keys: Vec<TrackKey>, _, cancel| {
                let catalog = catalog.clone();
                Box::pin(async move {
                    let items = keys
                        .into_iter()
                        .map(|key| library::Item::Track { key, entry: None })
                        .collect::<Vec<_>>();
                    catalog
                        .rows_of(&items, true, &cancel)
                        .await
                        .map(|rows| rows.into_iter().filter_map(track_row).collect())
                        .map_err(|error| error.to_string())
                })
            }),
        );
        sparse.seed_matching_at(0, rows, |row| row.track_key);
        Self(Rc::new(TrackModelState {
            sparse,
            applied: Rc::new(RefCell::new(TrackRead::Keys(keys))),
            filter: RefCell::default(),
        }))
    }

    pub fn list_model(&self) -> SparseObjectModel {
        self.0.sparse.list_model()
    }

    pub fn sparse_model(&self) -> Rc<SparseRouteModel<TrackKey, TrackRow>> {
        Rc::clone(&self.0.sparse)
    }

    pub fn ready_position(&self, media_uri: &str) -> Option<u32> {
        self.0
            .sparse
            .ready_position(|row| row.media_uri() == media_uri)
    }

    /// Where a reread starts so the rows on screen stay: the first live row.
    pub fn live_window(&self) -> library::RouteSeedWindow {
        self.0.sparse.first_live_position().map_or_else(
            library::RouteSeedWindow::top,
            library::RouteSeedWindow::position,
        )
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

    pub fn set_query(&self, query: &str) {
        self.0.filter.replace(query.trim().to_string());
    }

    /// Takes a reread of the page; a read for another search is stale.
    pub fn replace_page(&self, read: RouteRead) -> bool {
        let Some(members) = read.view.members().cloned() else {
            return false;
        };
        if members.filter != *self.0.filter.borrow() {
            return false;
        }
        self.0.applied.replace(TrackRead::Members(members));
        self.0.sparse.replace_prepared_at(
            SparseSource::Query {
                count: read.view.len(),
            },
            read.first,
            read.rows.into_iter().filter_map(track_row).collect(),
            read.discs,
            |row| row.track_key,
        )
    }

    pub fn resume_initial_demand(&self) {
        self.0.sparse.resume_initial_demand();
    }

    pub fn update_favorite(&self, media_uri: &str, favorite: bool) {
        self.0.sparse.update_matching(
            |row| row.media_uri() == media_uri,
            |row| row.set_favorite(favorite),
        );
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
