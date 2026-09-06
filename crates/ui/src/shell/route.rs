//! Navigation and the one mounted route.
//!
//! A route prepares one complete projection from the selected `Library`
//! away from GTK, then builds and mounts its GTK model. Home and playlist
//! detail have narrow point-update hooks because those operations are
//! intentionally visible without remounting the whole page.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use adw::prelude::*;
use gtk::glib;
use library::SourceKey;
use playback::TransportStatus;
use sources::SourceId;
use tracing::{debug, warn};

use super::Shell;
use super::navigation::update_navigation_selection;
use super::route_position::{
    RoutePositionKey, RoutePositionMemory, prepare_route_position_before_snapshot,
};
use crate::routes::route::{CollectionCategory, Route};
use crate::routes::route_layout::{primary_route_scroll_adjustment, route_boundary};
use crate::routes::track_selection::{
    PlaylistEntrySelection, PlaylistEntrySelectionSnapshot, TrackSelection, TrackSelectionSnapshot,
};
use crate::routes::{
    AlbumCollectionOrder, NamedDetailId, load_artist_discography, load_artist_overview,
    load_artist_tracks, load_named_detail, load_smart_playlist_detail,
};
use crate::{LibraryListKey, LibraryListSettings};

const SLOW_ROUTE_RENDER_MS: u64 = 250;

type RouteTiming = Option<Instant>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RouteCurrentTrackContext {
    pub(crate) context_id: String,
    pub(crate) source_rank: usize,
}

#[cfg(test)]
mod route_stack_tests {
    use super::*;

    #[test]
    fn repeated_route_navigation_is_ignored() {
        let mut routes = RouteStack::new(Route::Home);
        routes.navigate(Route::Home);
        assert!(!routes.can_back());
    }

    #[test]
    fn resetting_navigation_forgets_back_and_forward_routes() {
        let mut routes = RouteStack::new(Route::Home);
        routes.navigate(Route::Albums);
        routes.navigate(Route::Tracks);
        routes.back();
        routes.reset_to_home();
        assert_eq!(routes.current(), &Route::Home);
        assert!(!routes.can_back());
        assert!(routes.forward().is_none());
    }

    #[test]
    fn route_track_history_preserves_both_directions() {
        let mut routes = RouteStack::new(Route::Home);
        routes.navigate(Route::Albums);
        routes.navigate(Route::Tracks);
        assert_eq!(routes.back(), Some(&Route::Albums));
        assert_eq!(routes.back(), Some(&Route::Home));
        assert_eq!(routes.forward(), Some(&Route::Albums));
        assert_eq!(routes.forward(), Some(&Route::Tracks));
    }

    #[test]
    fn folder_routes_keep_history() {
        let root = Route::Folders { path: Vec::new() };
        let child = Route::Folders {
            path: vec![crate::routes::route::FolderPathItem {
                id: "folder".to_string(),
                name: "Folder".to_string(),
            }],
        };
        let mut routes = RouteStack::new(root.clone());
        routes.navigate(child.clone());
        assert_eq!(routes.back(), Some(&root));
        assert_eq!(routes.forward(), Some(&child));
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RouteCurrentTrack {
    pub(crate) media_uri: String,
    pub(crate) occurrence: playback::OccurrenceId,
    pub(crate) context: Option<RouteCurrentTrackContext>,
    pub(crate) paused: bool,
}

pub(crate) fn route_current_track(
    player: Option<&playback::PlaybackView>,
) -> Option<RouteCurrentTrack> {
    let player = player?;
    let entry = player.transport.current.as_ref()?;
    let context = match &entry.provenance {
        playback::Provenance::Context {
            context_id,
            source_rank,
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
        media_uri: entry.media_uri.clone(),
        occurrence: entry.id.occurrence.clone(),
        context,
        paused: player.transport.effective_state() == TransportStatus::Paused,
    })
}

fn route_is_current(shell: &Shell, route: &Route, source: SourceKey) -> bool {
    shell.navigation.routes.borrow().current() == route
        && shell
            .selected_library()
            .as_deref()
            .is_some_and(|selected| selected.source_key == source)
}

pub(crate) type RouteCurrentTrackSelection = Rc<dyn Fn(Option<&RouteCurrentTrack>) -> bool>;
pub(crate) type MountedRouteResume = Rc<dyn Fn()>;
pub(crate) type MountedDownloadChange = Rc<dyn Fn(&downloads::DownloadEvent)>;
pub(crate) type MountedRouteCommand = Rc<dyn Fn()>;
pub(crate) type MountedRouteSearchProvider = Rc<dyn Fn() -> Option<MountedRouteSearchTarget>>;
pub(crate) type MountedFavoriteSettlement = Rc<dyn Fn(crate::runtime::FavoriteSettlement)>;
pub(crate) type MountedHomePageApply = Rc<dyn Fn(library::HomePage)>;
pub(crate) type MountedRouteItemNavigation = Rc<dyn Fn(gtk::DirectionType) -> glib::Propagation>;

#[derive(Clone)]
pub(crate) struct MountedRouteSearchTarget {
    pub(crate) search: gtk::SearchEntry,
    pub(crate) focus: Option<MountedRouteCommand>,
}

impl MountedRouteSearchTarget {
    pub(crate) fn focus(&self) {
        if let Some(focus) = &self.focus {
            focus();
        } else {
            self.search.grab_focus();
        }
    }
}

pub(crate) fn item_navigation_entry_position(
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
pub(crate) struct LatestMountedRouteRead<T: Send + 'static, R: Send + 'static = ()> {
    apply: Rc<dyn Fn(R, T)>,
    load: Arc<dyn Fn(R) -> Pin<Box<dyn Future<Output = T> + Send>> + Send + Sync>,
    runtime: tokio::runtime::Handle,
    context: &'static str,
    generation: Cell<u64>,
    running: Cell<Option<u64>>,
    pending: RefCell<Option<(u64, R)>>,
}

impl<T: Send + 'static, R: Clone + Send + 'static> LatestMountedRouteRead<T, R> {
    pub(crate) fn new_with_request(
        runtime: tokio::runtime::Handle,
        apply: Rc<dyn Fn(R, T)>,
        load: Arc<dyn Fn(R) -> Pin<Box<dyn Future<Output = T> + Send>> + Send + Sync>,
        context: &'static str,
    ) -> Rc<Self> {
        Rc::new(Self {
            apply,
            load,
            runtime,
            context,
            generation: Cell::new(0),
            running: Cell::new(None),
            pending: RefCell::new(None),
        })
    }

    pub(crate) fn request_with(self: &Rc<Self>, request: R) {
        self.queue(request);
        self.start();
    }

    fn queue(&self, request: R) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.pending.replace(Some((generation, request)));
    }

    fn start(self: &Rc<Self>) {
        if self.running.get().is_some() {
            return;
        }
        let Some((generation, request)) = self.pending.borrow_mut().take() else {
            return;
        };
        self.running.set(Some(generation));
        let load = Arc::clone(&self.load);
        let runtime = self.runtime.clone();
        let read = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let load_request = request.clone();
            let result = runtime.spawn(async move { load(load_request).await }).await;
            let Some(read) = read.upgrade() else {
                return;
            };
            read.running.set(None);
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
                    let load = Arc::new(move |request: usize| {
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
                    let (release_sender, release_receiver) = async_channel::bounded(1);
                    let (dropped_sender, dropped_receiver) = async_channel::bounded(1);
                    let applied = Rc::new(Cell::new(false));
                    let load = Arc::new(move |(): ()| {
                        let started = started_sender.clone();
                        let release = release_receiver.clone();
                        let dropped = dropped_sender.clone();
                        Box::pin(async move {
                            started.send(()).await.expect("publish started read");
                            release.recv().await.expect("release detached read");
                            DropNotice(dropped)
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
                    release_sender.send(()).await.unwrap();
                    dropped_receiver.recv().await.unwrap();
                    assert!(!applied.get());
                })
            })
            .expect("install route test MainContext");
    }
}

pub(crate) struct RouteViewport {
    pub(super) route_host: gtk::Stack,
    route_loading: gtk::Box,
    route_cover_generation: Cell<u64>,
    mounted_route: RefCell<Option<MountedRouteEntry>>,
    position_memory: RefCell<RoutePositionMemory>,
    current_track_selections: RefCell<Vec<RouteCurrentTrackSelection>>,
    track_selections: RefCell<Vec<TrackSelection>>,
    playlist_entry_selection: RefCell<Option<PlaylistEntrySelection>>,
    order_read_generation: Cell<u64>,
    order_cancellation: RefCell<Option<(u64, library::ReadCancellation)>>,
}

struct MountedRouteEntry {
    route: Route,
    view: MountedRoute,
    surface: gtk::Widget,
    position_key: Option<RoutePositionKey>,
    scroll_adjustment: Option<gtk::Adjustment>,
}

impl RouteViewport {
    pub(super) fn new(route_host: gtk::Stack, route_loading: gtk::Box) -> Self {
        Self {
            route_host,
            route_loading,
            route_cover_generation: Cell::new(0),
            mounted_route: RefCell::new(None),
            position_memory: RefCell::new(RoutePositionMemory::default()),
            current_track_selections: RefCell::new(Vec::new()),
            track_selections: RefCell::new(Vec::new()),
            playlist_entry_selection: RefCell::new(None),
            order_read_generation: Cell::new(0),
            order_cancellation: RefCell::new(None),
        }
    }
}

#[derive(Clone)]
pub(crate) struct MountedRoute {
    widget: gtk::Widget,
    resume: MountedRouteResume,
    catalog_refresh: MountedRouteResume,
    item_navigation: Option<MountedRouteItemNavigation>,
    search: Option<MountedRouteSearchProvider>,
    layout_cycle: Option<MountedRouteCommand>,
    tab_cycle: Option<MountedRouteCommand>,
    initial_demand: MountedRouteResume,
    favorite_settlement: Option<MountedFavoriteSettlement>,
    download_change: Option<MountedDownloadChange>,
    home_page_apply: Option<MountedHomePageApply>,
}

impl MountedRoute {
    pub(crate) fn new(widget: gtk::Widget, resume: MountedRouteResume) -> Self {
        Self {
            widget,
            catalog_refresh: Rc::clone(&resume),
            resume,
            item_navigation: None,
            search: None,
            layout_cycle: None,
            tab_cycle: None,
            initial_demand: Rc::new(|| {}),
            favorite_settlement: None,
            download_change: None,
            home_page_apply: None,
        }
    }

    pub(crate) fn static_widget(widget: gtk::Widget) -> Self {
        Self::new(widget, Rc::new(|| {})).with_catalog_refresh(Rc::new(|| {}))
    }

    pub(crate) fn with_catalog_refresh(mut self, refresh: MountedRouteResume) -> Self {
        self.catalog_refresh = refresh;
        self
    }

    pub(crate) fn with_item_navigation(
        mut self,
        item_navigation: MountedRouteItemNavigation,
    ) -> Self {
        self.item_navigation = Some(item_navigation);
        self
    }

    pub(crate) fn with_search(mut self, search: gtk::SearchEntry) -> Self {
        self.search = Some(Rc::new(move || {
            Some(MountedRouteSearchTarget {
                search: search.clone(),
                focus: None,
            })
        }));
        self
    }

    pub(crate) fn with_search_provider(mut self, search: MountedRouteSearchProvider) -> Self {
        self.search = Some(search);
        self
    }

    pub(crate) fn with_layout_cycle(mut self, cycle: MountedRouteCommand) -> Self {
        self.layout_cycle = Some(cycle);
        self
    }

    pub(crate) fn with_tab_cycle(mut self, cycle: MountedRouteCommand) -> Self {
        self.tab_cycle = Some(cycle);
        self
    }

    pub(crate) fn with_initial_demand(mut self, resume: MountedRouteResume) -> Self {
        self.initial_demand = resume;
        self
    }

    pub(crate) fn with_favorite_settlement(mut self, apply: MountedFavoriteSettlement) -> Self {
        self.favorite_settlement = Some(apply);
        self
    }

    pub(crate) fn with_download_change(mut self, apply: MountedDownloadChange) -> Self {
        self.download_change = Some(match self.download_change.take() {
            Some(previous) => Rc::new(move |event| {
                previous(event);
                apply(event);
            }),
            None => apply,
        });
        self
    }

    pub(crate) fn with_home_page_apply(mut self, apply: MountedHomePageApply) -> Self {
        self.home_page_apply = Some(apply);
        self
    }

    fn apply_favorite_settlement(&self, settlement: crate::runtime::FavoriteSettlement) {
        if let Some(apply) = &self.favorite_settlement {
            apply(settlement);
        }
    }

    fn apply_home_page(&self, home: library::HomePage) {
        if let Some(apply) = &self.home_page_apply {
            apply(home);
        }
    }

    pub(crate) fn widget(&self) -> gtk::Widget {
        self.widget.clone()
    }

    pub(crate) fn resume(&self) {
        (self.resume)();
    }

    fn navigate_items(&self, direction: gtk::DirectionType) -> glib::Propagation {
        if let Some(navigate) = &self.item_navigation {
            navigate(direction)
        } else {
            glib::Propagation::Stop
        }
    }

    fn search(&self) -> Option<MountedRouteSearchTarget> {
        self.search.as_ref().and_then(|search| search())
    }

    fn cycle_layout(&self) {
        if let Some(cycle) = &self.layout_cycle {
            cycle();
        }
    }

    fn cycle_tabs(&self) {
        if let Some(cycle) = &self.tab_cycle {
            cycle();
        }
    }

    fn resume_initial_demand(&self) {
        (self.initial_demand)();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RouteStack {
    back: Vec<Route>,
    current: Route,
    forward: Vec<Route>,
}

impl RouteStack {
    pub(crate) fn new(initial: Route) -> Self {
        Self {
            back: Vec::new(),
            current: initial,
            forward: Vec::new(),
        }
    }

    pub(crate) fn current(&self) -> &Route {
        &self.current
    }

    pub(crate) fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub(crate) fn navigate(&mut self, route: Route) {
        if self.current == route {
            return;
        }
        let previous = std::mem::replace(&mut self.current, route);
        self.back.push(previous);
        self.forward.clear();
    }

    pub(crate) fn reset_to_home(&mut self) {
        self.back.clear();
        self.current = Route::Home;
        self.forward.clear();
    }

    pub(crate) fn back(&mut self) -> Option<&Route> {
        let previous = self.back.pop()?;
        let current = std::mem::replace(&mut self.current, previous);
        self.forward.push(current);
        Some(&self.current)
    }

    pub(crate) fn forward(&mut self) -> Option<&Route> {
        let next = self.forward.pop()?;
        let current = std::mem::replace(&mut self.current, next);
        self.back.push(current);
        Some(&self.current)
    }
}

impl Shell {
    fn route_seed_window(&self, route: &Route) -> library::RouteSeedWindow {
        let mounted_relative = self
            .route_viewport
            .mounted_route
            .borrow()
            .as_ref()
            .filter(|entry| entry.route == *route)
            .and_then(|entry| entry.scroll_adjustment.as_ref())
            .map(|adjustment| {
                let scrollable = (adjustment.upper() - adjustment.page_size()).max(0.0);
                if scrollable <= f64::EPSILON {
                    0.0
                } else {
                    (adjustment.value() / scrollable).clamp(0.0, 1.0)
                }
            });
        let relative = mounted_relative.or_else(|| {
            self.route_viewport
                .position_memory
                .borrow_mut()
                .restore(&RoutePositionKey::new(route.clone()))
                .map(|position| position.relative())
        });
        relative.map_or_else(library::RouteSeedWindow::top, |relative| {
            library::RouteSeedWindow::relative(relative)
        })
    }

    fn queue_application_route<T, E, F, Fut, B>(
        self: &Rc<Self>,
        route: Route,
        render_started: RouteTiming,
        context: &'static str,
        load: F,
        build: B,
    ) where
        T: Send + 'static,
        E: std::fmt::Display + Send + 'static,
        F: FnOnce(library::RouteSeedWindow, library::ReadCancellation) -> Fut + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
        B: FnOnce(Rc<Shell>, T) -> MountedRoute + 'static,
    {
        let window = self.route_seed_window(&route);
        let (generation, cancellation) = self.begin_root_order_read();
        let task = self.products.runtime.spawn(load(window, cancellation));
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = task.await;
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(generation)
                || shell.navigation.routes.borrow().current() != &route
            {
                return;
            }
            let rows = match result {
                Ok(Ok(rows)) => rows,
                Ok(Err(error)) => {
                    warn!(%error, ?route, context, "failed to prepare route");
                    return;
                }
                Err(error) => {
                    warn!(%error, ?route, context, "route preparation task failed");
                    return;
                }
            };
            let build_shell = Rc::clone(&shell);
            shell.replace_mounted_route(route, None, render_started, move || {
                build(build_shell, rows)
            });
        });
    }

    fn queue_prepared_route<T, E, F, Fut, B>(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        render_started: RouteTiming,
        context: &'static str,
        load: F,
        build: B,
    ) where
        T: Send + 'static,
        E: std::fmt::Display + Send + 'static,
        F: FnOnce(library::RouteSeedWindow, library::ReadCancellation) -> Fut + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
        B: FnOnce(Rc<Shell>, T, crate::runtime::SelectedLibrary) -> MountedRoute + 'static,
    {
        let source = selected.source_key;
        let source_id = selected.source_id.clone();
        let window = self.route_seed_window(&route);
        let (generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(load(window, cancellation));
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = task.await;
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(generation) {
                return;
            }
            let prepared = match result {
                Ok(Ok(prepared)) => Some(prepared),
                Ok(Err(error)) => {
                    warn!(%error, ?route, context, "failed to prepare route");
                    None
                }
                Err(error) => {
                    warn!(%error, ?route, context, "route preparation task failed");
                    None
                }
            };
            if !route_is_current(&shell, &route, source) {
                return;
            }
            let Some(prepared) = prepared else {
                shell.replace_mounted_route(route, Some(source_id), render_started, || {
                    MountedRoute::static_widget(
                        shell.route_empty_view(localization::msgid("Nothing here yet")),
                    )
                });
                return;
            };
            let build_shell = Rc::clone(&shell);
            shell.replace_mounted_route(route, Some(source_id), render_started, move || {
                build(build_shell, prepared, selected)
            });
        });
    }

    fn queue_home_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let showcase_variation = self.home_showcase_variation.get();
        let explore_variation = self.home_explore_variation.get();
        self.queue_prepared_route(
            selected,
            route,
            render_started,
            "Home route",
            move |_window, cancellation| async move {
                database
                    .home_page(
                        source,
                        folder,
                        showcase_variation,
                        explore_variation,
                        &cancellation,
                    )
                    .await
            },
            |shell, home, selected| shell.home_route(selected, home),
        );
    }

    pub(crate) fn refresh_mounted_home(self: &Rc<Self>) {
        if self.navigation.routes.borrow().current() != &Route::Home {
            return;
        }
        let Some(selected) = self.selected_library().as_deref().cloned() else {
            return;
        };
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let showcase_variation = self.home_showcase_variation.get();
        let explore_variation = self.home_explore_variation.get();
        let (generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            database
                .home_page(
                    source,
                    folder,
                    showcase_variation,
                    explore_variation,
                    &cancellation,
                )
                .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(Ok(home)) = task.await else { return };
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(generation) {
                return;
            }
            if !route_is_current(&shell, &Route::Home, source) {
                return;
            }
            let view = shell
                .route_viewport
                .mounted_route
                .borrow()
                .as_ref()
                .filter(|entry| entry.route == Route::Home)
                .map(|entry| entry.view.clone());
            if let Some(view) = view {
                view.apply_home_page(home);
            } else {
                let source_id = selected.source_id.clone();
                let build_shell = Rc::clone(&shell);
                shell.replace_mounted_route(Route::Home, Some(source_id), None, move || {
                    build_shell.home_route(selected, home)
                });
            }
        });
    }

    pub(crate) fn refresh_mounted_catalog(&self) {
        let refresh = self
            .route_viewport
            .mounted_route
            .borrow()
            .as_ref()
            .map(|entry| Rc::clone(&entry.view.catalog_refresh));
        if let Some(refresh) = refresh {
            refresh();
        }
    }

    pub(crate) fn navigate_current_route_items(
        &self,
        direction: gtk::DirectionType,
    ) -> glib::Propagation {
        if let Some(entry) = self.route_viewport.mounted_route.borrow().as_ref() {
            entry.view.navigate_items(direction)
        } else {
            glib::Propagation::Stop
        }
    }

    pub(crate) fn current_route_search(&self) -> Option<MountedRouteSearchTarget> {
        self.route_viewport
            .mounted_route
            .borrow()
            .as_ref()
            .and_then(|entry| entry.view.search())
    }

    pub(crate) fn cycle_current_route_layout(&self) {
        if let Some(entry) = self.route_viewport.mounted_route.borrow().as_ref() {
            entry.view.cycle_layout();
        }
    }

    pub(crate) fn cycle_current_route_tabs(&self) {
        if let Some(entry) = self.route_viewport.mounted_route.borrow().as_ref() {
            entry.view.cycle_tabs();
        }
    }

    pub(crate) fn register_current_route_track_selection(
        &self,
        selection: RouteCurrentTrackSelection,
    ) {
        let current = route_current_track(self.selected_playback().as_deref());
        if selection(current.as_ref()) {
            self.route_viewport
                .current_track_selections
                .borrow_mut()
                .push(selection);
        }
    }

    pub(crate) fn register_current_route_track_selection_owner(
        self: &Rc<Self>,
        selection: TrackSelection,
    ) {
        let shell = Rc::downgrade(self);
        selection.connect_changed(move |changed_selection| {
            if changed_selection.selection().is_empty() {
                return;
            }
            let Some(shell) = shell.upgrade() else {
                return;
            };
            for other in shell.route_viewport.track_selections.borrow().iter() {
                if !other.uses_selection(changed_selection) {
                    other.clear();
                }
            }
        });
        self.route_viewport
            .track_selections
            .borrow_mut()
            .push(selection);
    }

    pub(crate) fn current_route_track_selection(
        &self,
        clicked: &str,
    ) -> Option<TrackSelectionSnapshot> {
        self.route_viewport
            .track_selections
            .borrow()
            .iter()
            .find_map(|selection| selection.selected_tracks_for(clicked))
    }

    pub(crate) fn current_route_track_selection_snapshot(&self) -> Option<TrackSelectionSnapshot> {
        self.route_viewport
            .track_selections
            .borrow()
            .iter()
            .find_map(TrackSelection::selected_tracks)
    }

    pub(crate) fn set_current_playlist_entry_selection(&self, selection: PlaylistEntrySelection) {
        self.route_viewport
            .playlist_entry_selection
            .replace(Some(selection));
    }

    pub(crate) fn current_playlist_entry_selection(
        &self,
        clicked: library::PlaylistEntryKey,
    ) -> Option<PlaylistEntrySelectionSnapshot> {
        self.route_viewport
            .playlist_entry_selection
            .borrow()
            .as_ref()
            .and_then(|selection| selection.selected_entries_for(clicked))
    }

    pub(crate) fn current_playlist_entry_selection_snapshot(
        &self,
    ) -> Option<PlaylistEntrySelectionSnapshot> {
        self.route_viewport
            .playlist_entry_selection
            .borrow()
            .as_ref()
            .and_then(PlaylistEntrySelection::selected_entries)
    }

    pub(crate) fn current_playlist_entry_selection_owner(&self) -> Option<PlaylistEntrySelection> {
        self.route_viewport
            .playlist_entry_selection
            .borrow()
            .clone()
    }

    pub(crate) fn refresh_current_route_now_playing_selections(&self) {
        let current = route_current_track(self.selected_playback().as_deref());
        self.route_viewport
            .current_track_selections
            .borrow_mut()
            .retain(|selection| selection(current.as_ref()));
    }

    pub(crate) fn navigate(self: &Rc<Self>, route: Route) {
        debug!(?route, "navigate");
        self.close_fullscreen_player();
        self.navigation.routes.borrow_mut().navigate(route);
        self.render_current_route();
    }

    pub(crate) fn set_favorite_category(self: &Rc<Self>, category: CollectionCategory) {
        if self.navigation.favorite_category.replace(category) == category
            || self.navigation.routes.borrow().current() != &Route::Favorites
        {
            return;
        }
        self.replace_current_route_when_ready();
    }

    pub(crate) fn reset_navigation_to_home(self: &Rc<Self>) {
        debug!("reset navigation to Home");
        self.close_fullscreen_player();
        self.release_selected_navigation();
        self.render_current_route();
    }

    pub(crate) fn release_selected_navigation(&self) {
        self.route_viewport.position_memory.borrow_mut().clear();
        self.navigation.routes.borrow_mut().reset_to_home();
    }

    pub(crate) fn go_back(self: &Rc<Self>) {
        let route = self.navigation.routes.borrow_mut().back().cloned();
        if let Some(route) = route {
            debug!(?route, "navigate back");
            gtk::prelude::RootExt::set_focus(&self.chrome.window, None::<&gtk::Widget>);
            self.render_current_route();
        }
    }

    pub(crate) fn go_forward(self: &Rc<Self>) {
        let route = self.navigation.routes.borrow_mut().forward().cloned();
        if let Some(route) = route {
            debug!(?route, "navigate forward");
            gtk::prelude::RootExt::set_focus(&self.chrome.window, None::<&gtk::Widget>);
            self.render_current_route();
        }
    }

    pub(crate) fn render_current_route(self: &Rc<Self>) {
        if !self.startup.route_revealed.get() {
            self.render_startup_loading_view();
            return;
        }
        self.render_current_route_content();
    }

    pub(crate) fn render_current_route_content(self: &Rc<Self>) {
        self.render_current_route_content_inner(false);
    }

    pub(crate) fn replace_current_route_when_ready(self: &Rc<Self>) {
        self.render_current_route_content_inner(true);
    }

    fn render_current_route_content_inner(self: &Rc<Self>, force: bool) {
        let route = self.navigation.routes.borrow().current().clone();
        update_navigation_selection(self);
        let mounted_current = self
            .route_viewport
            .mounted_route
            .borrow()
            .as_ref()
            .is_some_and(|entry| entry.route == route);
        if !force && mounted_current {
            self.cancel_root_order_read();
            self.cancel_route_loading();
            return;
        }

        self.cancel_route_loading();
        self.show_route_loading();
        let render_started = tracing::enabled!(tracing::Level::DEBUG).then(Instant::now);
        match route.clone() {
            Route::History => {
                self.queue_history_route(route, render_started);
                return;
            }
            Route::Playlists | Route::SmartPlaylists => {
                let key = if route == Route::Playlists {
                    LibraryListKey::Playlists
                } else {
                    LibraryListKey::SmartPlaylists
                };
                let settings = self.settings.current.borrow().library_list(key);
                if route == Route::Playlists {
                    self.queue_playlist_route(route, settings, render_started);
                } else {
                    self.queue_smart_playlist_route(route, settings, render_started);
                }
                return;
            }
            Route::PlaylistDetail(key) => {
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::PlaylistTracks);
                self.queue_playlist_detail_route(route, key, settings, render_started);
                return;
            }
            Route::SmartPlaylistDetail(key) => {
                self.queue_smart_playlist_detail_route(route, key, render_started);
                return;
            }
            Route::AlbumDetail(album_id) => {
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::AlbumDetailTracks);
                self.queue_album_detail_route(route, album_id, settings, render_started);
                return;
            }
            Route::ArtistDetail(artist_id) | Route::AlbumArtistDetail(artist_id) => {
                let album_artist = matches!(&route, Route::AlbumArtistDetail(_));
                let track_settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistTracks);
                let album_settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistAlbums);
                self.queue_artist_overview_route(
                    route,
                    artist_id,
                    album_artist,
                    track_settings,
                    album_settings,
                    render_started,
                );
                return;
            }
            Route::ArtistDiscography(artist_id) | Route::AlbumArtistDiscography(artist_id) => {
                let album_artist = matches!(&route, Route::AlbumArtistDiscography(_));
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistAlbums);
                self.queue_artist_discography_route(
                    route,
                    artist_id,
                    album_artist,
                    settings,
                    render_started,
                );
                return;
            }
            Route::ArtistTracks(artist_id) | Route::AlbumArtistTracks(artist_id) => {
                let album_artist = matches!(&route, Route::AlbumArtistTracks(_));
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistTracks);
                self.queue_artist_tracks_route(
                    route,
                    artist_id,
                    album_artist,
                    settings,
                    false,
                    render_started,
                );
                return;
            }
            Route::ArtistFavoriteTracks(artist_id)
            | Route::AlbumArtistFavoriteTracks(artist_id) => {
                let album_artist = matches!(&route, Route::AlbumArtistFavoriteTracks(_));
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistTracks);
                self.queue_artist_tracks_route(
                    route,
                    artist_id,
                    album_artist,
                    settings,
                    true,
                    render_started,
                );
                return;
            }
            _ => {}
        }
        let Some(selected) = self.selected_library().as_deref().cloned() else {
            self.replace_mounted_route(route, None, render_started, || {
                MountedRoute::static_widget(
                    self.route_empty_view(localization::msgid("Nothing here yet")),
                )
            });
            return;
        };
        let source_id = Some(selected.source_id.clone());
        match route.clone() {
            Route::Home => {
                self.queue_home_route(selected, route, render_started);
            }
            Route::Search => {
                self.replace_mounted_route(route, source_id, render_started, || {
                    self.search_route(&selected)
                });
            }
            Route::SmartPlaylists => unreachable!("application-owned route"),
            Route::SmartPlaylistDetail(_) => unreachable!("application-owned route"),
            Route::Folders { path } => {
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::Tracks);
                let load_selected = selected.clone();
                let load_path = path.clone();
                self.queue_prepared_route(
                    selected,
                    route,
                    render_started,
                    "Folder route",
                    move |_window, cancellation| async move {
                        Ok::<_, std::convert::Infallible>(
                            crate::routes::folders::prepare_folder_route(
                                &load_selected,
                                &load_path,
                                &settings,
                                &cancellation,
                            )
                            .await,
                        )
                    },
                    move |shell, prepared, selected| shell.folders_route(path, &selected, prepared),
                );
            }
            Route::Albums => {
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::Albums);
                self.queue_album_route(selected, route, settings, false, render_started);
            }
            Route::Tracks => {
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::Tracks);
                self.queue_root_track_route(selected, route, settings, false, render_started);
            }
            Route::Artists | Route::AlbumArtists => {
                let album_artists = matches!(&route, Route::AlbumArtists);
                let key = if album_artists {
                    LibraryListKey::AlbumArtists
                } else {
                    LibraryListKey::Artists
                };
                let settings = self.settings.current.borrow().library_list(key);
                self.queue_artist_route(
                    selected,
                    route,
                    album_artists,
                    settings,
                    false,
                    render_started,
                );
            }
            Route::Favorites => match self.navigation.favorite_category.get() {
                CollectionCategory::Tracks => {
                    let settings = self
                        .settings
                        .current
                        .borrow()
                        .library_list(LibraryListKey::FavoriteTracks);
                    self.queue_root_track_route(selected, route, settings, true, render_started);
                }
                CollectionCategory::Albums => {
                    let settings = self
                        .settings
                        .current
                        .borrow()
                        .library_list(LibraryListKey::Albums);
                    self.queue_album_route(selected, route, settings, true, render_started);
                }
                CollectionCategory::Artists => {
                    let settings = self
                        .settings
                        .current
                        .borrow()
                        .library_list(LibraryListKey::Artists);
                    self.queue_artist_route(selected, route, false, settings, true, render_started);
                }
            },
            Route::History => unreachable!("History is application-owned"),
            Route::Genres | Route::Moods => {
                let genres = matches!(&route, Route::Genres);
                let key = if genres {
                    LibraryListKey::Genres
                } else {
                    LibraryListKey::Moods
                };
                let settings = self.settings.current.borrow().library_list(key);
                if genres {
                    self.queue_genre_route(selected, route, settings, render_started);
                } else {
                    self.queue_mood_route(selected, route, settings, render_started);
                }
            }
            Route::Playlists => unreachable!("application-owned route"),
            Route::AlbumDetail(_)
            | Route::ArtistDetail(_)
            | Route::AlbumArtistDetail(_)
            | Route::ArtistDiscography(_)
            | Route::AlbumArtistDiscography(_)
            | Route::ArtistTracks(_)
            | Route::AlbumArtistTracks(_)
            | Route::ArtistFavoriteTracks(_)
            | Route::AlbumArtistFavoriteTracks(_) => {
                unreachable!("media routes are application-owned")
            }
            Route::GenreDetail(genre_id) => {
                let id = NamedDetailId::Genre(genre_id);
                let settings = self.settings.current.borrow().library_list(id.key());
                self.queue_named_detail_route(selected, route, id, settings, render_started);
            }
            Route::MoodDetail(mood_id) => {
                let id = NamedDetailId::Mood(mood_id);
                let settings = self.settings.current.borrow().library_list(id.key());
                self.queue_named_detail_route(selected, route, id, settings, render_started);
            }
            Route::PlaylistDetail(_) => unreachable!("application-owned route"),
        }
    }

    fn queue_root_track_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        settings: LibraryListSettings,
        favorites_only: bool,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let folder = selected.music_folder_key;
        let build_route = route.clone();
        self.queue_prepared_route(
            selected,
            route,
            render_started,
            "Track route",
            move |window, cancellation| async move {
                database
                    .track_route_page(
                        source_key,
                        folder,
                        favorites_only,
                        "",
                        settings.sort_key.track_sort(),
                        settings.descending,
                        window,
                        &cancellation,
                    )
                    .await
            },
            move |shell, page, selected| {
                let library::TrackRoutePage {
                    order,
                    first_row_position,
                    first_rows,
                } = page;
                match build_route {
                    Route::Tracks => {
                        shell.library_tracks_route(order, first_row_position, first_rows, selected)
                    }
                    Route::Favorites => {
                        shell.favorites_route(order, first_row_position, first_rows, selected)
                    }
                    _ => unreachable!("root Track route owner"),
                }
            },
        );
    }

    fn queue_album_detail_route(
        self: &Rc<Self>,
        route: Route,
        album_uri: String,
        _: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        self.queue_application_route(
            route,
            render_started,
            "Album detail route",
            move |window, cancellation| async move {
                let detail = database.album_detail(&album_uri, &cancellation).await?;
                let Some(detail) = detail else {
                    return Ok::<_, library::LibraryError>((None, 0, Vec::new()));
                };
                let seed = window.range(detail.track_order.len());
                let first_row_position = seed.start;
                let first_rows = database
                    .track_rows_by_uri(&detail.track_order[seed], &cancellation)
                    .await?;
                Ok((Some(detail), first_row_position, first_rows))
            },
            move |shell, (detail, first_row_position, first_rows)| {
                shell.album_detail_view(detail, first_row_position, first_rows)
            },
        );
    }

    fn queue_album_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        settings: LibraryListSettings,
        favorites_only: bool,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let folder = selected.music_folder_key;
        let layout = settings.layout;
        let build_route = route.clone();
        self.queue_prepared_route(
            selected,
            route,
            render_started,
            "Album route",
            move |window, cancellation| async move {
                if layout == crate::LibraryLayout::Detail {
                    let (media_uris, album_uris, albums_with_genres) = database
                        .album_detail_route_order(
                            source_key,
                            folder,
                            favorites_only,
                            "",
                            settings.sort_key.album_sort(),
                            settings.descending,
                            &cancellation,
                        )
                        .await?;
                    let seed = window.range(media_uris.len());
                    let first_row_position = seed.start;
                    let items = media_uris[seed].to_vec();
                    let (albums, tracks) = database
                        .album_detail_route_rows(source_key, &items, folder, &cancellation)
                        .await?;
                    let first_detail_rows =
                        crate::routes::album_detail_rows_for_items(items, albums, tracks)
                            .map_err(library::LibraryError::InvalidRequest)?;
                    Ok::<_, library::LibraryError>((
                        AlbumCollectionOrder::Detail {
                            media_uris,
                            albums: album_uris,
                            albums_with_genres,
                        },
                        first_row_position,
                        Vec::new(),
                        first_detail_rows,
                    ))
                } else {
                    let (order, first_row_position, first_rows) = database
                        .album_route_page(
                            source_key,
                            folder,
                            favorites_only,
                            "",
                            settings.sort_key.album_sort(),
                            settings.descending,
                            window,
                            &cancellation,
                        )
                        .await?;
                    Ok((
                        AlbumCollectionOrder::Rows(order),
                        first_row_position,
                        first_rows,
                        Vec::new(),
                    ))
                }
            },
            move |shell, (order, first_row_position, first_rows, first_detail_rows), selected| {
                shell.library_albums_route(
                    build_route,
                    favorites_only,
                    order,
                    first_row_position,
                    first_rows,
                    first_detail_rows,
                    selected,
                )
            },
        );
    }

    fn queue_artist_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        album_artist: bool,
        settings: LibraryListSettings,
        favorites_only: bool,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let folder = selected.music_folder_key;
        let build_route = route.clone();
        self.queue_prepared_route(
            selected,
            route,
            render_started,
            "Artist route",
            move |window, cancellation| async move {
                database
                    .artist_route_page(
                        source_key,
                        folder,
                        album_artist,
                        favorites_only,
                        "",
                        settings.sort_key.artist_sort(),
                        settings.descending,
                        window,
                        &cancellation,
                    )
                    .await
            },
            move |shell, (order, first_row_position, first_rows), selected| {
                shell.library_artist_list_route(
                    build_route,
                    album_artist,
                    favorites_only,
                    order,
                    first_row_position,
                    first_rows,
                    selected,
                )
            },
        );
    }

    fn queue_smart_playlist_detail_route(
        self: &Rc<Self>,
        route: Route,
        key: library::SmartPlaylistKey,
        render_started: RouteTiming,
    ) {
        let selected = self.selected_library();
        let source = selected.as_deref().map(|selected| selected.source_key);
        let folder = selected
            .as_deref()
            .and_then(|selected| selected.music_folder_key);
        drop(selected);
        let database = Arc::clone(&self.products.library);
        self.queue_application_route(
            route,
            render_started,
            "Smart Playlist detail route",
            move |window, cancellation| async move {
                load_smart_playlist_detail(&database, source, folder, key, window, &cancellation)
                    .await
            },
            move |shell, detail| shell.smart_playlist_detail_route(key, detail, source, folder),
        );
    }

    fn queue_playlist_detail_route(
        self: &Rc<Self>,
        route: Route,
        key: library::PlaylistKey,
        settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let selected = self.selected_library();
        let source = selected.as_deref().map(|selected| selected.source_key);
        let folder = selected
            .as_deref()
            .and_then(|selected| selected.music_folder_key);
        drop(selected);
        let database = Arc::clone(&self.products.library);
        self.queue_application_route(
            route,
            render_started,
            "Playlist detail route",
            move |window, cancellation| async move {
                database
                    .playlist_detail_page(
                        key,
                        folder,
                        settings.sort_key.playlist_entry_sort(),
                        settings.descending,
                        window,
                        &cancellation,
                    )
                    .await
            },
            move |shell, detail| shell.playlist_detail_route(key, detail, source, folder),
        );
    }

    fn queue_artist_overview_route(
        self: &Rc<Self>,
        route: Route,
        artist_uri: String,
        album_artist: bool,
        track_settings: LibraryListSettings,
        album_settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        self.queue_application_route(
            route,
            render_started,
            "Artist overview route",
            move |window, cancellation| async move {
                let Some(row) = database
                    .artist_row_by_media_uri(&artist_uri, &cancellation)
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    return Ok(None);
                };
                let source = row.source_key;
                let artist = row.artist_key;
                let folder = None;
                load_artist_overview(
                    &database,
                    source,
                    folder,
                    artist,
                    album_artist,
                    &track_settings,
                    &album_settings,
                    window,
                    &cancellation,
                )
                .await
            },
            move |shell, detail| {
                let Some(detail) = detail else {
                    return MountedRoute::static_widget(
                        shell.route_empty_view(localization::msgid("This isn't available")),
                    );
                };
                let source = detail.summary.source_key;
                let artist = detail.summary.artist_key;
                shell.artist_detail_view(artist, album_artist, Some(detail), source)
            },
        );
    }

    fn queue_artist_discography_route(
        self: &Rc<Self>,
        route: Route,
        artist_uri: String,
        album_artist: bool,
        settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        self.queue_application_route(
            route,
            render_started,
            "Artist discography route",
            move |window, cancellation| async move {
                let Some(row) = database
                    .artist_row_by_media_uri(&artist_uri, &cancellation)
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    return Ok(None);
                };
                let source = row.source_key;
                let artist = row.artist_key;
                let folder = None;
                load_artist_discography(
                    &database,
                    source,
                    folder,
                    artist,
                    album_artist,
                    &settings,
                    window,
                    &cancellation,
                )
                .await
            },
            move |shell, detail| {
                let Some(detail) = detail else {
                    return MountedRoute::static_widget(
                        shell.route_empty_view(localization::msgid("This isn't available")),
                    );
                };
                let source = detail.summary.source_key;
                let artist = detail.summary.artist_key;
                shell.artist_discography_view(artist, album_artist, Some(detail), source)
            },
        );
    }

    fn queue_artist_tracks_route(
        self: &Rc<Self>,
        route: Route,
        artist_uri: String,
        album_artist: bool,
        settings: LibraryListSettings,
        favorites_only: bool,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        self.queue_application_route(
            route,
            render_started,
            "Artist tracks route",
            move |window, cancellation| async move {
                let Some(row) = database
                    .artist_row_by_media_uri(&artist_uri, &cancellation)
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    return Ok(None);
                };
                let source = row.source_key;
                let artist = row.artist_key;
                let folder = None;
                load_artist_tracks(
                    &database,
                    source,
                    folder,
                    artist,
                    album_artist,
                    &settings,
                    favorites_only,
                    window,
                    &cancellation,
                )
                .await
            },
            move |shell, detail| {
                let Some(detail) = detail else {
                    return MountedRoute::static_widget(
                        shell.route_empty_view(localization::msgid("This isn't available")),
                    );
                };
                let source = detail.summary.source_key;
                let artist = detail.summary.artist_key;
                if favorites_only {
                    shell.artist_favorite_tracks_view(artist, album_artist, Some(detail), source)
                } else {
                    shell.artist_tracks_view(artist, album_artist, Some(detail), source)
                }
            },
        );
    }

    fn queue_named_detail_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        id: NamedDetailId,
        settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        self.queue_prepared_route(
            selected,
            route,
            render_started,
            "Named detail route",
            move |window, cancellation| async move {
                load_named_detail(
                    &database,
                    source,
                    folder,
                    id,
                    &settings,
                    window,
                    &cancellation,
                )
                .await
            },
            move |shell, (summary, page), selected| {
                let library::TrackRoutePage {
                    order,
                    first_row_position,
                    first_rows,
                } = page;
                shell.named_detail_view(
                    id,
                    summary,
                    order,
                    first_row_position,
                    first_rows,
                    selected,
                )
            },
        );
    }

    fn queue_playlist_route(
        self: &Rc<Self>,
        route: Route,
        settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let selected = self.selected_library();
        let source = selected.as_deref().map(|selected| selected.source_key);
        let folder = selected
            .as_deref()
            .and_then(|selected| selected.music_folder_key);
        drop(selected);
        let database = Arc::clone(&self.products.library);
        self.queue_application_route(
            route,
            render_started,
            "Playlist route",
            move |window, cancellation| async move {
                database
                    .playlist_route_page(
                        source,
                        folder,
                        settings.sort_key.playlist_sort(),
                        settings.descending,
                        "",
                        window,
                        &cancellation,
                    )
                    .await
            },
            move |shell, (order, first_row_position, first_rows)| {
                shell.library_playlists_route(order, first_row_position, first_rows, source, folder)
            },
        );
    }

    fn queue_smart_playlist_route(
        self: &Rc<Self>,
        route: Route,
        settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let selected = self.selected_library();
        let source = selected.as_deref().map(|selected| selected.source_key);
        let folder = selected
            .as_deref()
            .and_then(|selected| selected.music_folder_key);
        drop(selected);
        let database = Arc::clone(&self.products.library);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64);
        self.queue_application_route(
            route,
            render_started,
            "Smart Playlist route",
            move |window, cancellation| async move {
                database
                    .smart_playlist_route_page(
                        source,
                        folder,
                        settings.sort_key.smart_playlist_sort(),
                        settings.descending,
                        now,
                        window,
                        &cancellation,
                    )
                    .await
            },
            move |shell, (order, first_row_position, first_rows)| {
                shell.library_smart_playlists_route(
                    order,
                    first_row_position,
                    first_rows,
                    source,
                    folder,
                )
            },
        );
    }

    fn queue_history_route(self: &Rc<Self>, route: Route, render_started: RouteTiming) {
        let database = Arc::clone(&self.products.library);
        self.queue_application_route(
            route,
            render_started,
            "History route",
            move |_, cancellation| async move {
                database.activity_history(None, "", &cancellation).await
            },
            |shell, rows| shell.history_route(rows),
        );
    }

    fn queue_genre_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let folder = selected.music_folder_key;
        self.queue_prepared_route(
            selected,
            route,
            render_started,
            "Genre route",
            move |window, cancellation| async move {
                database
                    .genre_route_page(
                        source_key,
                        folder,
                        "",
                        settings.sort_key.genre_sort(),
                        settings.descending,
                        window,
                        &cancellation,
                    )
                    .await
            },
            |shell, (order, first_row_position, first_rows), selected| {
                shell.library_genres_route(order, first_row_position, first_rows, selected)
            },
        );
    }

    fn queue_mood_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let folder = selected.music_folder_key;
        self.queue_prepared_route(
            selected,
            route,
            render_started,
            "Mood route",
            move |window, cancellation| async move {
                database
                    .mood_route_page(
                        source_key,
                        folder,
                        "",
                        settings.sort_key.mood_sort(),
                        settings.descending,
                        window,
                        &cancellation,
                    )
                    .await
            },
            |shell, (order, first_row_position, first_rows), selected| {
                shell.library_moods_route(order, first_row_position, first_rows, selected)
            },
        );
    }

    fn begin_root_order_read(&self) -> (u64, library::ReadCancellation) {
        self.cancel_root_order_read();
        let generation = self
            .route_viewport
            .order_read_generation
            .get()
            .wrapping_add(1);
        self.route_viewport.order_read_generation.set(generation);
        let cancellation = library::ReadCancellation::new();
        self.route_viewport
            .order_cancellation
            .replace(Some((generation, cancellation.clone())));
        (generation, cancellation)
    }

    fn finish_root_order_read(&self, generation: u64) -> bool {
        if self.route_viewport.order_read_generation.get() != generation
            || !self
                .route_viewport
                .order_cancellation
                .borrow()
                .as_ref()
                .is_some_and(|(owner, _)| *owner == generation)
        {
            return false;
        }
        self.route_viewport.order_cancellation.borrow_mut().take();
        true
    }

    fn cancel_root_order_read(&self) {
        self.route_viewport.order_read_generation.set(
            self.route_viewport
                .order_read_generation
                .get()
                .wrapping_add(1),
        );
        if let Some((_, cancellation)) = self.route_viewport.order_cancellation.borrow_mut().take()
        {
            cancellation.cancel();
        }
    }

    fn replace_mounted_route(
        self: &Rc<Self>,
        route: Route,
        source_id: Option<SourceId>,
        render_started: RouteTiming,
        build: impl FnOnce() -> MountedRoute,
    ) {
        let replacement_started = render_started.as_ref().map(|_| Instant::now());
        let previous = self.begin_mounted_route_replacement();
        let route_loading_generation = self.begin_route_loading_gate();
        let model_started = render_started.as_ref().map(|_| Instant::now());
        let view = build();
        let model_ms = model_started.map(elapsed_ms);

        let mount_started = render_started.as_ref().map(|_| Instant::now());
        let widget = view.widget();
        if let Some(previous) = previous {
            self.route_viewport.route_host.remove(&previous.surface);
            drop(previous);
        }
        let scroll_adjustment = primary_route_scroll_adjustment(&widget);
        let position_key = source_id
            .filter(|_| scroll_adjustment.is_some())
            .map(|_| RoutePositionKey::new(route.clone()));
        let restore_position = position_key.as_ref().and_then(|key| {
            self.route_viewport
                .position_memory
                .borrow_mut()
                .restore(key)
        });
        let boundary = route_boundary(widget);
        let ready_shell = Rc::downgrade(self);
        let ready_view = view.clone();
        let surface = prepare_route_position_before_snapshot(
            &boundary,
            scroll_adjustment.as_ref(),
            restore_position
                .filter(|position| position.value > 0.0)
                .map(|position| position.value),
            move || {
                ready_view.resume_initial_demand();
                let Some(cover_generation) = route_loading_generation else {
                    return;
                };
                if let Some(shell) = ready_shell.upgrade()
                    && shell.route_viewport.route_cover_generation.get() == cover_generation
                {
                    shell.close_route_cover_registration(
                        cover_generation,
                        shell.route_viewport.route_host.upcast_ref(),
                    );
                    shell.try_finish_route_loading(cover_generation);
                }
            },
        );
        self.route_viewport.route_host.add_child(&surface);
        let displaced = self
            .route_viewport
            .mounted_route
            .replace(Some(MountedRouteEntry {
                route: route.clone(),
                view: view.clone(),
                surface: surface.clone(),
                position_key,
                scroll_adjustment,
            }));
        debug_assert!(displaced.is_none());
        self.route_viewport.route_host.set_visible_child(&surface);
        if let (
            Some(render_started),
            Some(replacement_started),
            Some(model_ms),
            Some(mount_started),
        ) = (render_started, replacement_started, model_ms, mount_started)
        {
            let teardown_ms = elapsed_ms(replacement_started).saturating_sub(model_ms);
            let mount_ms = elapsed_ms(mount_started);
            let total_ms = elapsed_ms(render_started);
            debug!(
                ?route,
                teardown_ms, model_ms, mount_ms, total_ms, "route render timing"
            );
            if total_ms >= SLOW_ROUTE_RENDER_MS {
                warn!(
                    ?route,
                    teardown_ms, model_ms, mount_ms, total_ms, "slow route render"
                );
            }
        }
    }

    fn show_route_loading(&self) {
        if self.startup.route_revealed.get() {
            self.route_viewport.route_loading.set_visible(true);
        }
    }

    fn begin_route_loading_gate(self: &Rc<Self>) -> Option<u64> {
        if !self.startup.route_revealed.get() {
            return None;
        }
        self.route_viewport.route_loading.set_visible(true);
        let generation = self.begin_route_cover_prime();
        self.route_viewport.route_cover_generation.set(generation);
        Some(generation)
    }

    pub(in crate::shell) fn try_finish_route_loading(&self, cover_generation: u64) {
        if self.route_viewport.route_cover_generation.get() == cover_generation
            && self.route_cover_prime_ready(cover_generation)
        {
            self.finish_route_loading(cover_generation);
        }
    }

    fn finish_route_loading(&self, cover_generation: u64) {
        if self.route_viewport.route_cover_generation.get() != cover_generation {
            return;
        }
        self.route_viewport.route_loading.set_visible(false);
        self.route_viewport.route_cover_generation.set(0);
        self.finish_route_cover_prime_gate();
    }

    fn cancel_route_loading(&self) {
        self.route_viewport.route_cover_generation.set(0);
        self.route_viewport.route_loading.set_visible(false);
        self.finish_route_cover_prime_gate();
    }

    pub(crate) fn apply_favorite_settlement_to_mounted_route(
        &self,
        settlement: crate::runtime::FavoriteSettlement,
    ) {
        if let Some(view) = self
            .route_viewport
            .mounted_route
            .borrow()
            .as_ref()
            .map(|entry| entry.view.clone())
        {
            view.apply_favorite_settlement(settlement);
        }
    }

    pub(crate) fn apply_download_change_to_mounted_route(&self, event: &downloads::DownloadEvent) {
        let apply = self
            .route_viewport
            .mounted_route
            .borrow()
            .as_ref()
            .and_then(|entry| entry.view.download_change.clone());
        if let Some(apply) = apply {
            apply(event);
        }
    }

    pub(crate) fn reconcile_mounted_route(&self) {
        if let Some(view) = self
            .route_viewport
            .mounted_route
            .borrow()
            .as_ref()
            .map(|entry| entry.view.clone())
        {
            view.resume();
        }
    }

    pub(crate) fn has_active_mounted_route(&self) -> bool {
        self.route_viewport.mounted_route.borrow().is_some()
    }

    fn begin_mounted_route_replacement(&self) -> Option<MountedRouteEntry> {
        let previous = self.route_viewport.mounted_route.borrow_mut().take();
        if let Some(previous) = previous.as_ref()
            && let (Some(key), Some(adjustment)) = (
                previous.position_key.as_ref(),
                previous.scroll_adjustment.as_ref(),
            )
        {
            self.route_viewport.position_memory.borrow_mut().record(
                key.clone(),
                adjustment.value(),
                adjustment.upper(),
                adjustment.page_size(),
            );
        }
        self.clear_favorite_controls();
        self.clear_current_route_selections();
        previous
    }

    fn clear_current_route_selections(&self) {
        self.route_viewport
            .current_track_selections
            .borrow_mut()
            .clear();
        self.route_viewport.track_selections.borrow_mut().clear();
        self.route_viewport
            .playlist_entry_selection
            .borrow_mut()
            .take();
    }

    pub(crate) fn clear_mounted_routes(&self) {
        self.cancel_route_loading();
        let mounted_route = self.begin_mounted_route_replacement();
        if mounted_route.is_none() && self.route_viewport.route_host.first_child().is_none() {
            return;
        }
        let mut child = self.route_viewport.route_host.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            self.route_viewport.route_host.remove(&current);
        }
        drop(mounted_route);
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}
