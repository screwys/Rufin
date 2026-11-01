use crate::sparse_model::{SparseRouteModel, SparseSource};
use adw::prelude::*;
use gtk::glib;
use playback::TransportStatus;
use rufin_core::view::{OpenOptions, Opened, Reread, View};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};
use tracing::warn;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteCurrentTrackContext {
    pub context_id: String,
    pub source_rank: usize,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteCurrentTrack {
    pub track_key: Option<library::TrackKey>,
    pub media_uri: String,
    pub context: Option<RouteCurrentTrackContext>,
    pub paused: bool,
}
pub fn route_current_track(player: Option<&playback::PlaybackView>) -> Option<RouteCurrentTrack> {
    let player = player?;
    let entry = player.transport.current.as_ref()?;
    let context = match &entry.row.provenance {
        playback::Provenance::Context {
            context_id,
            source_rank,
            ..
        } => Some(RouteCurrentTrackContext {
            context_id: context_id.to_string(),
            source_rank: *source_rank,
        }),
        playback::Provenance::Manual
        | playback::Provenance::Random
        | playback::Provenance::Radio
        | playback::Provenance::AutoDj
        | playback::Provenance::Legacy => None,
    };
    Some(RouteCurrentTrack {
        track_key: Some(entry.row.track.track_key),
        media_uri: entry.row.track.media_uri.clone(),
        context,
        paused: !matches!(
            player.transport.effective_state(),
            TransportStatus::Resolving | TransportStatus::Buffering | TransportStatus::Playing
        ),
    })
}
pub type RouteCurrentTrackSelection = Rc<dyn Fn(Option<&RouteCurrentTrack>) -> bool>;
pub type MountedRouteResume = Rc<dyn Fn()>;
pub type MountedDownloadChange = Rc<dyn Fn(&downloads::DownloadEvent)>;
pub type MountedRouteCommand = Rc<dyn Fn()>;
pub type MountedRouteSearchProvider = Rc<dyn Fn() -> Option<MountedRouteSearchTarget>>;
pub type MountedHomePageApply = Rc<dyn Fn(Vec<rufin_core::view::HomeSection>)>;
/// Shows committed favorites and ratings in a route's rows; true when it reads again instead.
pub type MountedStates = Rc<dyn Fn(&[library::StateChange]) -> bool>;
pub type MountedRouteItemNavigation = Rc<dyn Fn(gtk::DirectionType) -> glib::Propagation>;

#[derive(Clone)]
pub struct MountedRouteSearchTarget {
    pub search: gtk::SearchEntry,
    pub focus: Option<MountedRouteCommand>,
}

impl MountedRouteSearchTarget {
    pub fn focus(&self) {
        if let Some(focus) = &self.focus {
            focus();
        } else {
            self.search.grab_focus();
        }
    }
}

pub fn item_navigation_entry_position(
    current: u32,
    item_count: u32,
    direction: gtk::DirectionType,
) -> Option<u32> {
    if item_count == 0 {
        return None;
    }
    if current != gtk::INVALID_LIST_POSITION {
        return Some(current.min(item_count - 1));
    }
    match direction {
        gtk::DirectionType::Up | gtk::DirectionType::Left => Some(item_count - 1),
        gtk::DirectionType::Down | gtk::DirectionType::Right => Some(0),
        _ => None,
    }
}

/// The view a mounted route reads, which its models read their windows from, and the models
/// that show committed favorites and ratings in their rows.
pub struct RouteViewState {
    view: RefCell<Arc<View>>,
    rows: RefCell<Vec<Box<dyn Fn(&library::StateChange) -> bool>>>,
}

pub type RouteView = Rc<RouteViewState>;

impl RouteViewState {
    /// The view the route shows now.
    pub fn borrow(&self) -> std::cell::Ref<'_, Arc<View>> {
        self.view.borrow()
    }

    fn replace(&self, view: Arc<View>) -> Arc<View> {
        self.view.replace(view)
    }

    /// Keeps `model`'s rows showing the favorites and ratings committed while it lives.
    pub fn show_states<K, R>(&self, model: &Rc<SparseRouteModel<K, R>>)
    where
        K: Clone + Eq + Send + Sync + 'static,
        R: library::ShowsState + Clone + PartialEq + Send + Sync + 'static,
    {
        let model = Rc::downgrade(model);
        self.rows.borrow_mut().push(Box::new(move |change| {
            let Some(model) = model.upgrade() else {
                return false;
            };
            model.update_matching(
                |row| {
                    row.state(change)
                        .is_some_and(|state| !change.shown_in(state))
                },
                |row| {
                    if let Some(state) = row.state_mut(change) {
                        change.show_in(state);
                    }
                },
            );
            true
        }));
    }

    /// Shows committed favorites and ratings in the rows; true when the page reads again
    /// instead, because they order or filter its rows.
    pub fn show(&self, items: &[library::StateChange]) -> bool {
        if items.is_empty() {
            return false;
        }
        self.rows
            .borrow_mut()
            .retain(|rows| items.iter().all(|change| rows(change)));
        self.view.borrow().states_reorder()
    }
}

/// The view holder of a route opened with `read`.
pub fn route_view(read: &Opened) -> RouteView {
    Rc::new(RouteViewState {
        view: RefCell::new(Arc::clone(&read.view)),
        rows: RefCell::default(),
    })
}

/// A sparse model of the rows `view` lists, seeded with the window `read` took with it. The
/// seed and every later window keep the rows `map` takes, and they show committed favorites
/// and ratings.
pub fn view_model<K, R>(
    runtime: tokio::runtime::Handle,
    view: &RouteView,
    map: fn(library::Row) -> Option<R>,
    read: &mut Opened,
) -> Rc<SparseRouteModel<K, R>>
where
    K: Clone + Eq + Send + Sync + 'static,
    R: library::ShowsState + Clone + PartialEq + Send + Sync + 'static,
{
    let load_view = Rc::clone(view);
    let model = SparseRouteModel::new(
        SparseSource::Query {
            count: read.view.len(),
        },
        runtime,
        Rc::new(move |_, range, cancel| {
            let view = Arc::clone(&load_view.borrow());
            Box::pin(async move {
                view.rows(range, &cancel)
                    .await
                    .map(|rows| rows.into_iter().filter_map(map).collect())
                    .map_err(|error| error.to_string())
            })
        }),
    );
    model.seed_at(
        read.first,
        std::mem::take(&mut read.rows)
            .into_iter()
            .filter_map(map)
            .collect(),
    );
    view.show_states(&model);
    model
}

/// A mounted route's view and the reads that replace it. One read runs at a time and only the
/// latest request is read next. The route says what it wants read and how a new read applies
/// over the view it replaces; a read the route rejects is not kept.
pub struct PageReread {
    view: RouteView,
    inputs: Box<dyn Fn(&OpenOptions) -> OpenOptions>,
    apply: Box<dyn Fn(Opened, &View) -> bool>,
    runtime: tokio::runtime::Handle,
    context: &'static str,
    generation: Cell<u64>,
    running: RefCell<Option<(library::ReadCancellation, tokio::task::AbortHandle)>>,
    pending: RefCell<Option<(u64, PageRequest)>>,
}

struct PageRequest {
    view: Arc<View>,
    read: Reread,
    options: OpenOptions,
    window: library::RouteSeedWindow,
}

impl PageReread {
    pub fn new(
        runtime: tokio::runtime::Handle,
        context: &'static str,
        view: &RouteView,
        inputs: impl Fn(&OpenOptions) -> OpenOptions + 'static,
        apply: impl Fn(Opened, &View) -> bool + 'static,
    ) -> Rc<Self> {
        Rc::new(Self {
            view: Rc::clone(view),
            inputs: Box::new(inputs),
            apply: Box::new(apply),
            runtime,
            context,
            generation: Cell::new(0),
            running: RefCell::new(None),
            pending: RefCell::new(None),
        })
    }

    /// The route's view holder.
    pub fn route_view(&self) -> &RouteView {
        &self.view
    }

    /// The view last applied.
    pub fn view(&self) -> Arc<View> {
        Arc::clone(&self.view.borrow())
    }

    /// The options the route wants read now.
    pub fn wanted(&self) -> OpenOptions {
        (self.inputs)(self.view.borrow().options())
    }

    /// Reads from the top, for a new search or filter.
    pub fn reread(self: &Rc<Self>) {
        self.reread_at(library::RouteSeedWindow::top());
    }

    /// Reads where the list is, for a change to what it shows.
    pub fn reread_at(self: &Rc<Self>, window: library::RouteSeedWindow) {
        self.request(Reread::Page, window);
    }

    /// Reads again for a committed change: from `live`, where the list is, unless its search
    /// moved since the read it shows.
    pub fn refresh(self: &Rc<Self>, live: library::RouteSeedWindow) {
        let window = if self.wanted().filter == self.view.borrow().options().filter {
            live
        } else {
            library::RouteSeedWindow::top()
        };
        self.reread_at(window);
    }

    /// Reads an artist page's releases again.
    pub fn reread_releases(self: &Rc<Self>) {
        self.request(Reread::Releases, library::RouteSeedWindow::top());
    }

    /// Reads again what `settings` change.
    pub fn resume(self: &Rc<Self>, settings: &rufin_core::settings::app::Settings) {
        let read = self.view.borrow().settings_reread(settings);
        if let Some(read) = read {
            self.request(read, library::RouteSeedWindow::top());
        }
    }

    fn request(self: &Rc<Self>, read: Reread, window: library::RouteSeedWindow) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.pending.replace(Some((
            generation,
            PageRequest {
                view: self.view(),
                read,
                options: self.wanted(),
                window,
            },
        )));
        if let Some((cancellation, _)) = self.running.borrow().as_ref() {
            cancellation.cancel();
        }
        self.start();
    }

    fn start(self: &Rc<Self>) {
        if self.running.borrow().is_some() {
            return;
        }
        let Some((generation, request)) = self.pending.borrow_mut().take() else {
            return;
        };
        let cancel = library::ReadCancellation::new();
        let task = self.runtime.spawn({
            let cancel = cancel.clone();
            async move {
                let view = &request.view;
                match request.read {
                    Reread::Page => view.reread(request.options, request.window, &cancel).await,
                    Reread::Releases => {
                        view.reread_releases(request.options, request.window, &cancel)
                            .await
                    }
                }
                .map_err(|error| error.to_string())
            }
        });
        self.running.replace(Some((cancel, task.abort_handle())));
        let reread = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = task.await;
            let Some(reread) = reread.upgrade() else {
                return;
            };
            reread.running.borrow_mut().take();
            match result {
                Ok(_) if reread.generation.get() != generation => {}
                Ok(Ok(read)) => {
                    // Models read windows of the new view as soon as they take its rows.
                    let previous = reread.view.replace(Arc::clone(&read.view));
                    if !(reread.apply)(read, &previous) {
                        reread.view.replace(previous);
                    }
                }
                Ok(Err(error)) => {
                    warn!(%error, context = reread.context, "failed to read page again");
                }
                Err(_) => warn!(context = reread.context, "page reread task panicked"),
            }
            reread.start();
        });
    }
}

impl Drop for PageReread {
    fn drop(&mut self) {
        if let Some((cancellation, task)) = self.running.get_mut().take() {
            cancellation.cancel();
            task.abort();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemScrollPosition {
    pub position: u32,
    pub offset: f64,
    pub query_position: Option<usize>,
}

#[derive(Clone)]
pub struct NativeListScroll {
    pub list: gtk::ListView,
    items: Rc<dyn Fn() -> Vec<(u32, gtk::Widget)>>,
    query_positions: bool,
    native_position: Option<Rc<dyn Fn(u32) -> u32>>,
}

impl NativeListScroll {
    pub fn new(
        list: gtk::ListView,
        items: Rc<dyn Fn() -> Vec<(u32, gtk::Widget)>>,
        query_positions: bool,
    ) -> Self {
        Self {
            list,
            items,
            query_positions,
            native_position: None,
        }
    }

    pub fn with_native_position(mut self, map: Rc<dyn Fn(u32) -> u32>) -> Self {
        self.native_position = Some(map);
        self
    }

    pub fn capture(&self) -> Option<ItemScrollPosition> {
        (self.items)()
            .into_iter()
            .filter_map(|(position, widget)| {
                let bounds = widget.compute_bounds(&self.list)?;
                (widget.is_mapped()
                    && bounds.y() < self.list.height() as f32
                    && bounds.y() + bounds.height() > 0.0)
                    .then_some(ItemScrollPosition {
                        position,
                        offset: f64::from(bounds.y()),
                        query_position: self.query_positions.then_some(position as usize),
                    })
            })
            .min_by(|left, right| left.offset.total_cmp(&right.offset))
    }

    pub fn scroll_to(&self, position: u32) {
        let position = self
            .native_position
            .as_ref()
            .map_or(position, |map| map(position));
        let count = self.list.model().map_or(0, |model| model.n_items());
        if count > 0 {
            self.list
                .scroll_to(position.min(count - 1), gtk::ListScrollFlags::NONE, None);
        }
    }

    pub fn restore_offset(&self, saved: ItemScrollPosition) -> bool {
        let Some((_, widget)) = (self.items)()
            .into_iter()
            .find(|(position, _)| *position == saved.position)
        else {
            return false;
        };
        let (Some(bounds), Some(adjustment)) =
            (widget.compute_bounds(&self.list), self.list.vadjustment())
        else {
            return false;
        };
        let value = adjustment.value() + f64::from(bounds.y()) - saved.offset;
        adjustment.set_value(value.clamp(
            adjustment.lower(),
            (adjustment.upper() - adjustment.page_size()).max(adjustment.lower()),
        ));
        true
    }
}

#[derive(Clone)]
pub struct MountedRoute {
    pub widget: gtk::Widget,
    pub resume: MountedRouteResume,
    pub catalog_refresh: MountedRouteResume,
    pub item_navigation: Option<MountedRouteItemNavigation>,
    pub item_scroll: Option<Rc<dyn Fn() -> Option<NativeListScroll>>>,
    pub search: Option<MountedRouteSearchProvider>,
    pub layout_cycle: Option<MountedRouteCommand>,
    pub tab_cycle: Option<MountedRouteCommand>,
    pub initial_demand: MountedRouteResume,
    /// Shows committed favorites and ratings in the route's rows.
    pub states: Option<MountedStates>,
    pub download_change: Option<MountedDownloadChange>,
    pub home_page_apply: Option<MountedHomePageApply>,
}

impl MountedRoute {
    pub fn new(widget: gtk::Widget, resume: MountedRouteResume) -> Self {
        Self {
            widget,
            catalog_refresh: Rc::clone(&resume),
            resume,
            item_navigation: None,
            item_scroll: None,
            search: None,
            layout_cycle: None,
            tab_cycle: None,
            initial_demand: Rc::new(|| {}),
            states: None,
            download_change: None,
            home_page_apply: None,
        }
    }

    pub fn static_widget(widget: gtk::Widget) -> Self {
        Self::new(widget, Rc::new(|| {})).with_catalog_refresh(Rc::new(|| {}))
    }

    pub fn with_catalog_refresh(mut self, refresh: MountedRouteResume) -> Self {
        self.catalog_refresh = refresh;
        self
    }

    pub fn with_item_navigation(mut self, item_navigation: MountedRouteItemNavigation) -> Self {
        self.item_navigation = Some(item_navigation);
        self
    }

    pub fn with_item_scroll(mut self, scroll: Rc<dyn Fn() -> Option<NativeListScroll>>) -> Self {
        self.item_scroll = Some(scroll);
        self
    }

    pub fn item_scroll(&self) -> Option<NativeListScroll> {
        self.item_scroll.as_ref().and_then(|scroll| scroll())
    }

    pub fn with_search(mut self, search: gtk::SearchEntry) -> Self {
        self.search = Some(Rc::new(move || {
            Some(MountedRouteSearchTarget {
                search: search.clone(),
                focus: None,
            })
        }));
        self
    }

    pub fn with_search_provider(mut self, search: MountedRouteSearchProvider) -> Self {
        self.search = Some(search);
        self
    }

    pub fn with_layout_cycle(mut self, cycle: MountedRouteCommand) -> Self {
        self.layout_cycle = Some(cycle);
        self
    }

    pub fn with_tab_cycle(mut self, cycle: MountedRouteCommand) -> Self {
        self.tab_cycle = Some(cycle);
        self
    }

    pub fn with_initial_demand(mut self, resume: MountedRouteResume) -> Self {
        self.initial_demand = resume;
        self
    }

    pub fn with_states(self, view: &RouteView) -> Self {
        let view = Rc::clone(view);
        self.with_shown_states(Rc::new(move |items| view.show(items)))
    }

    pub fn with_shown_states(mut self, states: MountedStates) -> Self {
        self.states = Some(states);
        self
    }

    pub fn with_download_change(mut self, apply: MountedDownloadChange) -> Self {
        self.download_change = Some(match self.download_change.take() {
            Some(previous) => Rc::new(move |event| {
                previous(event);
                apply(event);
            }),
            None => apply,
        });
        self
    }

    pub fn with_home_page_apply(mut self, apply: MountedHomePageApply) -> Self {
        self.home_page_apply = Some(apply);
        self
    }

    /// Shows committed favorites and ratings in the route's rows, or reads it again where they
    /// order or filter them.
    pub fn show_states(&self, items: &[library::StateChange]) {
        if !items.is_empty() && self.states.as_ref().is_some_and(|show| show(items)) {
            (self.catalog_refresh)();
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.widget.clone()
    }

    pub fn resume(&self) {
        (self.resume)();
    }

    pub fn navigate_items(&self, direction: gtk::DirectionType) -> glib::Propagation {
        if let Some(navigate) = &self.item_navigation {
            navigate(direction)
        } else {
            glib::Propagation::Stop
        }
    }

    pub fn search(&self) -> Option<MountedRouteSearchTarget> {
        self.search.as_ref().and_then(|search| search())
    }

    pub fn cycle_layout(&self) {
        if let Some(cycle) = &self.layout_cycle {
            cycle();
        }
    }

    pub fn cycle_tabs(&self) {
        if let Some(cycle) = &self.tab_cycle {
            cycle();
        }
    }

    pub fn resume_initial_demand(&self) {
        (self.initial_demand)();
    }
}
