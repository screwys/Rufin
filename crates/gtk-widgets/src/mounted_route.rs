use crate::sparse_model::{SparseRouteModel, SparseSource};
use adw::prelude::*;
use gtk::glib;
use playback::TransportStatus;
use rufin_core::view::{OpenOptions, Opened, Reread, View};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
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
pub type MountedFavoriteState = Rc<dyn Fn(&library::StateChange)>;
pub type MountedHomePageApply = Rc<dyn Fn(library::HomePage)>;
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

/// Runs at most one mounted-route read at a time and retains only the newest
/// request while the route still owns this value.
pub struct LatestMountedRouteRead<T: Send + 'static, R: Send + 'static = ()> {
    apply: Rc<dyn Fn(R, T)>,
    load: Arc<
        dyn Fn(R, library::ReadCancellation) -> Pin<Box<dyn Future<Output = T> + Send>>
            + Send
            + Sync,
    >,
    runtime: tokio::runtime::Handle,
    context: &'static str,
    generation: Cell<u64>,
    running: RefCell<Option<(library::ReadCancellation, tokio::task::AbortHandle)>>,
    pending: RefCell<Option<(u64, R)>>,
}

impl<T: Send + 'static, R: Clone + Send + 'static> LatestMountedRouteRead<T, R> {
    pub fn new_with_request(
        runtime: tokio::runtime::Handle,
        apply: Rc<dyn Fn(R, T)>,
        load: Arc<
            dyn Fn(R, library::ReadCancellation) -> Pin<Box<dyn Future<Output = T> + Send>>
                + Send
                + Sync,
        >,
        context: &'static str,
    ) -> Rc<Self> {
        Rc::new(Self {
            apply,
            load,
            runtime,
            context,
            generation: Cell::new(0),
            running: RefCell::new(None),
            pending: RefCell::new(None),
        })
    }

    pub fn request_with(self: &Rc<Self>, request: R) {
        self.queue(request);
        self.start();
    }

    fn queue(&self, request: R) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.pending.replace(Some((generation, request)));
        if let Some((cancellation, _)) = self.running.borrow().as_ref() {
            cancellation.cancel();
        }
    }

    fn start(self: &Rc<Self>) {
        if self.running.borrow().is_some() {
            return;
        }
        let Some((generation, request)) = self.pending.borrow_mut().take() else {
            return;
        };
        let cancellation = library::ReadCancellation::new();
        let task = self
            .runtime
            .spawn((self.load)(request.clone(), cancellation.clone()));
        self.running
            .replace(Some((cancellation, task.abort_handle())));
        let read = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = task.await;
            let Some(read) = read.upgrade() else {
                return;
            };
            read.running.borrow_mut().take();
            let value = match result {
                Ok(value) => value,
                Err(_) => {
                    warn!(context = read.context, "route projection task panicked");
                    read.start();
                    return;
                }
            };
            if read.generation.get() != generation {
                read.start();
                return;
            }
            (read.apply)(request, value);
        });
    }
}

impl<T: Send + 'static, R: Send + 'static> Drop for LatestMountedRouteRead<T, R> {
    fn drop(&mut self) {
        if let Some((cancellation, task)) = self.running.get_mut().take() {
            cancellation.cancel();
            task.abort();
        }
    }
}

/// The view a mounted route reads; its models read their windows from the view it holds now.
pub type RouteView = Rc<RefCell<Arc<View>>>;

/// The view holder of a route opened with `read`.
pub fn route_view(read: &Opened) -> RouteView {
    Rc::new(RefCell::new(Arc::clone(&read.view)))
}

/// A sparse model of the rows `view` lists, seeded with the window `read` took with it. The
/// seed and every later window keep the rows `map` takes.
pub fn view_model<K, R>(
    runtime: tokio::runtime::Handle,
    view: &RouteView,
    map: fn(library::Row) -> Option<R>,
    read: &mut Opened,
) -> Rc<SparseRouteModel<K, R>>
where
    K: Clone + Eq + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
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
    model
}

/// A mounted route's view and the reads that replace it; the latest request wins. The route
/// says what it wants read and how a new read applies; a read the route rejects is not kept.
pub struct PageReread {
    view: RouteView,
    inputs: Box<dyn Fn(&OpenOptions) -> OpenOptions>,
    read: Rc<LatestMountedRouteRead<Result<Opened, String>, PageRequest>>,
}

#[derive(Clone)]
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
        apply: impl Fn(Opened) -> bool + 'static,
    ) -> Rc<Self> {
        let current = Rc::downgrade(view);
        let apply = Rc::new(move |_, result: Result<Opened, String>| {
            let read = match result {
                Ok(read) => read,
                Err(error) => {
                    warn!(%error, context, "failed to read page again");
                    return;
                }
            };
            let Some(current) = current.upgrade() else {
                return;
            };
            // Models read windows of the new view as soon as they take its rows.
            let previous = current.replace(Arc::clone(&read.view));
            if !apply(read) {
                current.replace(previous);
            }
        });
        let load = Arc::new(|request: PageRequest, cancel: library::ReadCancellation| {
            Box::pin(async move {
                let view = &request.view;
                match request.read {
                    Reread::Page => view.reread(request.options, request.window, &cancel).await,
                    Reread::Releases => {
                        view.reread_releases(request.options, request.window, &cancel)
                            .await
                    }
                }
                .map_err(|error| error.to_string())
            }) as Pin<Box<dyn Future<Output = _> + Send>>
        });
        Rc::new(Self {
            view: Rc::clone(view),
            inputs: Box::new(inputs),
            read: LatestMountedRouteRead::new_with_request(runtime, apply, load, context),
        })
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
    pub fn reread(&self) {
        self.reread_at(library::RouteSeedWindow::top());
    }

    /// Reads where the list is, for a change to what it shows.
    pub fn reread_at(&self, window: library::RouteSeedWindow) {
        self.request(Reread::Page, window);
    }

    /// Reads again for a committed change: from `live`, where the list is, unless its search
    /// moved since the read it shows.
    pub fn refresh(&self, live: library::RouteSeedWindow) {
        let window = if self.wanted().filter == self.view.borrow().options().filter {
            live
        } else {
            library::RouteSeedWindow::top()
        };
        self.reread_at(window);
    }

    /// Reads an artist page's releases again.
    pub fn reread_releases(&self) {
        self.request(Reread::Releases, library::RouteSeedWindow::top());
    }

    fn request(&self, read: Reread, window: library::RouteSeedWindow) {
        self.read.request_with(PageRequest {
            view: self.view(),
            read,
            options: self.wanted(),
            window,
        });
    }

    /// Reads again what `settings` change.
    pub fn resume(&self, settings: &rufin_core::settings::app::Settings) {
        let read = self.view.borrow().settings_reread(settings);
        if let Some(read) = read {
            self.request(read, library::RouteSeedWindow::top());
        }
    }

    /// Reads again where the list is when a favorite change can move or drop its rows.
    pub fn favorite_changed(&self, live: library::RouteSeedWindow) {
        if self.view.borrow().favorites_reorder() {
            self.refresh(live);
        }
    }
}

#[cfg(test)]
mod latest_mounted_route_read_tests {
    use super::*;

    #[test]
    fn latest_route_read_applies_only_the_newest_request() {
        let runtime = tokio::runtime::Runtime::new().expect("route test runtime");
        let context = gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let (started_sender, started_receiver) = async_channel::bounded(2);
                    let (release_sender, release_receiver) = async_channel::bounded(2);
                    let (applied_sender, applied_receiver) = async_channel::bounded(2);
                    let load = Arc::new(move |request: usize, _| {
                        let started = started_sender.clone();
                        let release = release_receiver.clone();
                        Box::pin(async move {
                            started.send(request).await.expect("publish started read");
                            release.recv().await.expect("release route read");
                            request
                        }) as Pin<Box<dyn Future<Output = usize> + Send>>
                    });
                    let apply = Rc::new(move |request: usize, value: usize| {
                        assert_eq!(request, value);
                        applied_sender
                            .try_send(value)
                            .expect("publish applied route read");
                    });
                    let read = LatestMountedRouteRead::new_with_request(
                        runtime.handle().clone(),
                        apply,
                        load,
                        "test route",
                    );

                    read.request_with(0);
                    assert_eq!(started_receiver.recv().await.unwrap(), 0);
                    read.request_with(1);
                    read.request_with(2);
                    release_sender.send(()).await.unwrap();
                    assert_eq!(started_receiver.recv().await.unwrap(), 2);
                    assert!(applied_receiver.is_empty());
                    release_sender.send(()).await.unwrap();
                    assert_eq!(applied_receiver.recv().await.unwrap(), 2);
                    assert!(applied_receiver.is_empty());
                })
            })
            .expect("install route test MainContext");
    }

    #[test]
    fn detached_route_read_does_not_publish() {
        struct DropNotice(async_channel::Sender<()>);
        impl Drop for DropNotice {
            fn drop(&mut self) {
                let _ = self.0.try_send(());
            }
        }

        let runtime = tokio::runtime::Runtime::new().expect("route test runtime");
        let context = gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let (started_sender, started_receiver) = async_channel::bounded(1);
                    let (dropped_sender, dropped_receiver) = async_channel::bounded(1);
                    let applied = Rc::new(Cell::new(false));
                    let load = Arc::new(move |(): (), _| {
                        let started = started_sender.clone();
                        let dropped = dropped_sender.clone();
                        Box::pin(async move {
                            let notice = DropNotice(dropped);
                            started.send(()).await.expect("publish started read");
                            std::future::pending::<()>().await;
                            notice
                        })
                            as Pin<Box<dyn Future<Output = DropNotice> + Send>>
                    });
                    let apply_flag = Rc::clone(&applied);
                    let read = LatestMountedRouteRead::new_with_request(
                        runtime.handle().clone(),
                        Rc::new(move |(), _| apply_flag.set(true)),
                        load,
                        "detached test route",
                    );

                    read.request_with(());
                    started_receiver.recv().await.unwrap();
                    drop(read);
                    dropped_receiver.recv().await.unwrap();
                    assert!(!applied.get());
                })
            })
            .expect("install route test MainContext");
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
    pub favorite_state: Option<MountedFavoriteState>,
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
            favorite_state: None,
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

    pub fn with_favorite_state(mut self, apply: MountedFavoriteState) -> Self {
        self.favorite_state = Some(apply);
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

    pub fn apply_favorite_state(&self, state: &library::StateChange) {
        if let Some(apply) = &self.favorite_state {
            apply(state);
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
