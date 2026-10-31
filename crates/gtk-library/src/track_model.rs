use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use library::{Catalog, Row, TrackKey, TrackRow};
use rufin_core::playback::{PlayRequest, QueuePlacement};

use gtk_widgets::library_fields::TrackPresentation;
use gtk_widgets::mounted_route::{RouteView, view_model};
use gtk_widgets::sparse_model::{SparseObjectModel, SparseRouteModel, SparseSource};
use rufin_core::view::Opened;

/// What a track list plays and selects: the read of the view the route shows now, or keys.
#[derive(Clone)]
enum TrackRead {
    Page(RouteView),
    /// An artist page's favorite tracks.
    Favorites(RouteView),
    Keys(Arc<[TrackKey]>),
}

impl TrackRead {
    fn members(&self) -> Option<library::MemberRead> {
        match self {
            Self::Page(view) => view.borrow().members().cloned(),
            Self::Favorites(view) => view.borrow().favorites().cloned(),
            Self::Keys(_) => None,
        }
    }

    fn selection(&self, ranges: Option<Vec<std::ops::Range<usize>>>) -> library::MemberSelection {
        match (self, self.members()) {
            (_, Some(read)) => library::MemberSelection::Members { read, ranges },
            (Self::Keys(keys), None) => library::MemberSelection::Keys {
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
            (Self::Page(_) | Self::Favorites(_), None) => library::MemberSelection::Keys {
                source: None,
                keys: Arc::new([]),
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
    read: TrackRead,
}

#[derive(Clone)]
pub struct TrackCollectionModel(Rc<TrackModelState>);

impl TrackCollectionModel {
    /// The tracks of `view`, seeded with the rows `read` took with it.
    pub fn new(runtime: tokio::runtime::Handle, view: &RouteView, mut read: Opened) -> Self {
        let members = read
            .view
            .members()
            .cloned()
            .expect("a track page reads members");
        let filter = RefCell::new(members.filter.clone());
        let sparse = view_model(runtime, view, track_row, &mut read);
        sparse
            .list_model()
            .set_sections(read.view.disc_sections().to_vec());
        Self(Rc::new(TrackModelState {
            sparse,
            read: TrackRead::Page(Rc::clone(view)),
            filter,
        }))
    }

    /// An artist page's favorite tracks, which `members` lists.
    pub fn favorites(runtime: tokio::runtime::Handle, view: &RouteView, rows: Vec<Row>) -> Self {
        let count = view.borrow().favorites_len().unwrap_or(0);
        let load_view = Rc::clone(view);
        let sparse = SparseRouteModel::new(
            SparseSource::Query { count },
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
        sparse.seed_at(0, rows.into_iter().filter_map(track_row).collect());
        view.show_states(&sparse);
        Self(Rc::new(TrackModelState {
            sparse,
            filter: RefCell::new(
                view.borrow()
                    .favorites()
                    .map(|read| read.filter.clone())
                    .unwrap_or_default(),
            ),
            read: TrackRead::Favorites(Rc::clone(view)),
        }))
    }

    pub fn replace_favorites(&self, count: usize, rows: Vec<Row>) {
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
            read: TrackRead::Keys(keys),
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

    /// Where a reread starts so the rows on screen stay.
    pub fn live_window(&self) -> library::RouteSeedWindow {
        self.0.sparse.live_window()
    }

    pub fn selection_input(&self, positions: &gtk::Bitset) -> library::MemberSelection {
        self.0
            .read
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
    pub fn replace_page(&self, read: Opened) -> bool {
        if read
            .view
            .members()
            .is_none_or(|members| members.filter != *self.0.filter.borrow())
        {
            return false;
        }
        self.0.sparse.replace_prepared_at(
            SparseSource::Query {
                count: read.view.len(),
            },
            read.first,
            read.rows.into_iter().filter_map(track_row).collect(),
            read.view.disc_sections().to_vec(),
            |row| row.track_key,
        )
    }

    pub fn resume_initial_demand(&self) {
        self.0.sparse.resume_initial_demand();
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
        let selection = self.0.read.selection(None);
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

    /// The context id of a play from these rows; `|result=` ties it to this order snapshot.
    /// A page's id is its view's; an artist's favorites and key lists start with
    /// `context_base`.
    pub fn visible_context_id(&self, context_base: &str) -> String {
        let order = self.0.sparse.order_id();
        match &self.0.read {
            TrackRead::Page(view) => format!("{}|result={order}", view.borrow().context_id()),
            TrackRead::Favorites(view) => match view.borrow().favorites() {
                Some(query) => format!(
                    "{context_base}|query={}|sort={:?}|descending={}|result={order}",
                    query.filter, query.sort, query.descending,
                ),
                None => format!("{context_base}|result={order}"),
            },
            TrackRead::Keys(_) => format!("{context_base}|result={order}"),
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
