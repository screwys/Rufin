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
use library::{SourceKey, TrackKey};
use playback::{SourceSessionEpoch, TransportStatus};
use sources::SourceId;
use tracing::{debug, warn};

use super::Shell;
use super::navigation::update_navigation_selection;
use super::route_position::{
    RoutePositionKey, RoutePositionMemory, restore_route_position_before_snapshot,
};
use crate::routes::route::{CollectionCategory, Route};
use crate::routes::route_layout::{primary_route_scroll_adjustment, route_boundary};
use crate::routes::track_selection::{
    PlaylistEntrySelection, PlaylistEntrySelectionSnapshot, TrackSelection, TrackSelectionSnapshot,
};
use crate::routes::{
    AlbumCollectionOrder, NamedDetailId, load_artist_discography, load_artist_overview,
    load_artist_tracks, load_named_detail, load_playlist_detail, load_smart_playlist_detail,
};
use crate::{LibraryListKey, LibraryListSettings};

const SLOW_ROUTE_RENDER_MS: u64 = 250;

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
    pub(crate) source_id: SourceKey,
    pub(crate) track_id: TrackKey,
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
        source_id: player.transport.source_id,
        track_id: entry.track.track_key?,
        occurrence: entry.id.occurrence.clone(),
        context,
        paused: player.transport.effective_state() == TransportStatus::Paused,
    })
}

fn route_is_current(
    shell: &Shell,
    route: &Route,
    source: SourceKey,
    epoch: SourceSessionEpoch,
) -> bool {
    shell.navigation.routes.borrow().current() == route
        && shell.selected_library().as_deref().is_some_and(|selected| {
            selected.source_key == source && selected.source_session_epoch == epoch
        })
}

pub(crate) type RouteCurrentTrackSelection = Rc<dyn Fn(Option<&RouteCurrentTrack>) -> bool>;
pub(crate) type MountedRouteResume = Rc<dyn Fn()>;
pub(crate) type MountedRouteCommand = Rc<dyn Fn()>;
pub(crate) type MountedFavoriteSettlement = Rc<dyn Fn(crate::runtime::FavoriteSettlement)>;
pub(crate) type MountedHomePageApply = Rc<dyn Fn(library::HomePage)>;
pub(crate) type MountedRouteItemNavigation = Rc<dyn Fn(gtk::DirectionType) -> glib::Propagation>;

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
    mounted_route: RefCell<Option<MountedRouteEntry>>,
    position_memory: RefCell<RoutePositionMemory>,
    current_track_selections: RefCell<Vec<RouteCurrentTrackSelection>>,
    track_selections: RefCell<Vec<TrackSelection>>,
    playlist_entry_selection: RefCell<Option<PlaylistEntrySelection>>,
    pub(crate) route_search: RefCell<Option<gtk::SearchEntry>>,
    pub(crate) route_search_focus: RefCell<Option<Rc<dyn Fn()>>>,
    pub(crate) route_layout_cycle: RefCell<Option<MountedRouteCommand>>,
    pub(crate) route_tab_cycle: RefCell<Option<MountedRouteCommand>>,
    order_read_generation: Cell<u64>,
    order_cancellation: RefCell<Option<(u64, library::ReadCancellation)>>,
}

#[derive(Default)]
struct RouteActivationContext {
    current_track_selections: Vec<RouteCurrentTrackSelection>,
    track_selections: Vec<TrackSelection>,
    playlist_entry_selection: Option<PlaylistEntrySelection>,
    route_search: Option<gtk::SearchEntry>,
    route_search_focus: Option<Rc<dyn Fn()>>,
    route_layout_cycle: Option<MountedRouteCommand>,
    route_tab_cycle: Option<MountedRouteCommand>,
}

struct MountedRouteEntry {
    route: Route,
    view: MountedRoute,
    surface: gtk::Widget,
    position_key: Option<RoutePositionKey>,
    scroll_adjustment: Option<gtk::Adjustment>,
}

impl RouteViewport {
    pub(super) fn new(route_host: gtk::Stack) -> Self {
        Self {
            route_host,
            mounted_route: RefCell::new(None),
            position_memory: RefCell::new(RoutePositionMemory::default()),
            current_track_selections: RefCell::new(Vec::new()),
            track_selections: RefCell::new(Vec::new()),
            playlist_entry_selection: RefCell::new(None),
            route_search: RefCell::new(None),
            route_search_focus: RefCell::new(None),
            route_layout_cycle: RefCell::new(None),
            route_tab_cycle: RefCell::new(None),
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
    favorite_settlement: Option<MountedFavoriteSettlement>,
    home_page_apply: Option<MountedHomePageApply>,
}

impl MountedRoute {
    pub(crate) fn new(widget: gtk::Widget, resume: MountedRouteResume) -> Self {
        Self {
            widget,
            catalog_refresh: Rc::clone(&resume),
            resume,
            item_navigation: None,
            favorite_settlement: None,
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

    pub(crate) fn with_favorite_settlement(mut self, apply: MountedFavoriteSettlement) -> Self {
        self.favorite_settlement = Some(apply);
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
    fn queue_home_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let epoch = selected.source_session_epoch;
        let source_id = selected.artwork.source_id.clone();
        let variation = self.home_variation.get();
        let (generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            database
                .home_page(source, folder, variation, &cancellation)
                .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(generation) {
                return;
            }
            let Ok(Ok(home)) = task.await else { return };
            if !route_is_current(&shell, &route, source, epoch) {
                return;
            }
            let build_shell = Rc::clone(&shell);
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                build_shell.home_route(selected, home)
            });
        });
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
        let epoch = selected.source_session_epoch;
        let variation = self.home_variation.get();
        let task = selected.runtime.spawn(async move {
            database
                .home_page(source, folder, variation, &library::ReadCancellation::new())
                .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            let Ok(Ok(home)) = task.await else { return };
            if !route_is_current(&shell, &Route::Home, source, epoch) {
                return;
            }
            if let Some(view) = shell
                .route_viewport
                .mounted_route
                .borrow()
                .as_ref()
                .map(|entry| entry.view.clone())
            {
                view.apply_home_page(home);
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

    pub(crate) fn cycle_current_route_layout(&self) {
        let cycle = self.route_viewport.route_layout_cycle.borrow().clone();
        if let Some(cycle) = cycle {
            cycle();
        }
    }

    pub(crate) fn cycle_current_route_tabs(&self) {
        let cycle = self.route_viewport.route_tab_cycle.borrow().clone();
        if let Some(cycle) = cycle {
            cycle();
        }
    }

    pub(crate) fn release_first_run_setup(&self) {
        let Some(view) = self.take_first_run_setup_view() else {
            return;
        };
        if view.parent().is_some() {
            self.chrome.login_host.remove(&view);
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
        clicked: TrackKey,
    ) -> Option<TrackSelectionSnapshot> {
        self.route_viewport
            .track_selections
            .borrow()
            .iter()
            .find_map(|selection| selection.selected_tracks_for(clicked))
    }

    pub(crate) fn current_route_track_selection_snapshot(&self) -> Option<TrackSelectionSnapshot> {
        self.route_viewport
            .playlist_entry_selection
            .borrow()
            .as_ref()
            .and_then(PlaylistEntrySelection::selected_tracks)
            .or_else(|| {
                self.route_viewport
                    .track_selections
                    .borrow()
                    .iter()
                    .find_map(TrackSelection::selected_tracks)
            })
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
        if !self.startup.route_revealed.get() && !self.source.login_screen_active() {
            self.render_startup_loading_view();
            return;
        }
        if self.source.login_screen_active() {
            self.clear_mounted_routes();
            let view = self.add_server_view();
            if self.chrome.login_host.first_child().as_ref() != Some(&view) {
                while let Some(child) = self.chrome.login_host.first_child() {
                    self.chrome.login_host.remove(&child);
                }
                self.chrome.login_host.append(&view);
            }
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
            return;
        }

        let render_started = Instant::now();
        let Some(selected) = self.selected_library().as_deref().cloned() else {
            self.replace_mounted_route(route, None, render_started, 0, || {
                MountedRoute::static_widget(
                    self.route_empty_view(localization::msgid("Nothing here yet")),
                )
            });
            return;
        };
        let source_id = Some(selected.artwork.source_id.clone());
        match route.clone() {
            Route::Home => {
                self.queue_home_route(selected, route, render_started);
            }
            Route::Search => {
                self.replace_mounted_route(route, source_id, render_started, 0, || {
                    self.search_route(&selected)
                });
            }
            Route::SmartPlaylists => {
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::SmartPlaylists);
                self.queue_smart_playlist_route(selected, route, settings, render_started);
            }
            Route::SmartPlaylistDetail(playlist_id) => {
                self.queue_smart_playlist_detail_route(
                    selected,
                    route,
                    playlist_id,
                    render_started,
                );
            }
            Route::Folders { path } => {
                self.replace_mounted_route(route, source_id, render_started, 0, || {
                    self.folders_route(path, &selected)
                });
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
            Route::History => {
                self.queue_history_route(selected, route, render_started);
            }
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
            Route::Playlists => {
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::Playlists);
                self.queue_playlist_route(selected, route, settings, render_started);
            }
            Route::AlbumDetail(album_id) => {
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::AlbumDetailTracks);
                self.queue_album_detail_route(selected, route, album_id, settings, render_started);
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
                    selected,
                    route,
                    artist_id,
                    album_artist,
                    track_settings,
                    album_settings,
                    render_started,
                );
            }
            Route::ArtistDiscography(artist_id) | Route::AlbumArtistDiscography(artist_id) => {
                let album_artist = matches!(&route, Route::AlbumArtistDiscography(_));
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistAlbums);
                self.queue_artist_discography_route(
                    selected,
                    route,
                    artist_id,
                    album_artist,
                    settings,
                    render_started,
                );
            }
            Route::ArtistTracks(artist_id) | Route::AlbumArtistTracks(artist_id) => {
                let album_artist = matches!(&route, Route::AlbumArtistTracks(_));
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistTracks);
                self.queue_artist_tracks_route(
                    selected,
                    route,
                    artist_id,
                    album_artist,
                    settings,
                    false,
                    render_started,
                );
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
                    selected,
                    route,
                    artist_id,
                    album_artist,
                    settings,
                    true,
                    render_started,
                );
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
            Route::PlaylistDetail(playlist_id) => {
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::PlaylistTracks);
                self.queue_playlist_detail_route(
                    selected,
                    route,
                    playlist_id,
                    settings,
                    render_started,
                );
            }
        }
    }

    fn queue_root_track_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        settings: LibraryListSettings,
        favorites_only: bool,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let source_id = selected.artwork.source_id.clone();
        let epoch = selected.source_session_epoch;
        let folder = selected.music_folder_key;
        let (read_generation, cancellation) = self.begin_root_order_read();
        let runtime = selected.runtime.clone();
        let task = runtime.spawn(async move {
            database
                .track_route_page(
                    source_key,
                    folder,
                    favorites_only,
                    "",
                    settings.sort_key.track_sort(),
                    settings.descending,
                    &cancellation,
                )
                .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else {
                return;
            };
            if !shell.finish_root_order_read(read_generation) {
                return;
            }
            let page = match task.await {
                Ok(Ok(page)) => page,
                Ok(Err(error)) => {
                    warn!(%error, ?route, "failed to read Track route order");
                    return;
                }
                Err(error) => {
                    warn!(%error, ?route, "Track route order task failed");
                    return;
                }
            };
            if shell.navigation.routes.borrow().current() != &route
                || !shell.selected_library().as_deref().is_some_and(|current| {
                    current.source_key == source_key && current.source_session_epoch == epoch
                })
            {
                return;
            }
            let build_selected = selected;
            let build_shell = Rc::clone(&shell);
            let library::TrackRoutePage { order, first_rows } = page;
            shell.replace_mounted_route(
                route.clone(),
                Some(source_id),
                render_started,
                0,
                move || match route {
                    Route::Tracks => {
                        build_shell.library_tracks_route(order, first_rows, build_selected)
                    }
                    Route::Favorites => {
                        build_shell.favorites_route(order, first_rows, build_selected)
                    }
                    _ => unreachable!("root Track route owner"),
                },
            );
        });
    }

    fn queue_album_detail_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        album: library::AlbumKey,
        _: LibraryListSettings,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let source_id = selected.artwork.source_id.clone();
        let epoch = selected.source_session_epoch;
        let folder = selected.music_folder_key;
        let (read_generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            database
                .album_detail(source_key, album, folder, &cancellation)
                .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else {
                return;
            };
            if !shell.finish_root_order_read(read_generation) {
                return;
            }
            let detail = match task.await {
                Ok(Ok(detail)) => detail,
                Ok(Err(error)) => {
                    warn!(%error, "failed to read Album detail");
                    return;
                }
                Err(error) => {
                    warn!(%error, "Album detail task failed");
                    return;
                }
            };
            if shell.navigation.routes.borrow().current() != &route
                || !shell.selected_library().as_deref().is_some_and(|current| {
                    current.source_key == source_key && current.source_session_epoch == epoch
                })
            {
                return;
            }
            let build_shell = Rc::clone(&shell);
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                build_shell.album_detail_view(album, detail, selected)
            });
        });
    }

    fn queue_album_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        settings: LibraryListSettings,
        favorites_only: bool,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let source_id = selected.artwork.source_id.clone();
        let epoch = selected.source_session_epoch;
        let folder = selected.music_folder_key;
        let layout = settings.layout;
        let (read_generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            if layout == crate::LibraryLayout::Detail {
                database
                    .album_detail_route_order(
                        source_key,
                        folder,
                        favorites_only,
                        "",
                        settings.sort_key.album_sort(),
                        settings.descending,
                        &cancellation,
                    )
                    .await
                    .map(|order| (AlbumCollectionOrder::Detail(order), Vec::new()))
            } else {
                let (order, first_rows) = database
                    .album_route_page(
                        source_key,
                        folder,
                        favorites_only,
                        "",
                        settings.sort_key.album_sort(),
                        settings.descending,
                        &cancellation,
                    )
                    .await?;
                Ok((AlbumCollectionOrder::Rows(order), first_rows))
            }
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else {
                return;
            };
            if !shell.finish_root_order_read(read_generation) {
                return;
            }
            let (order, first_rows) = match task.await {
                Ok(Ok(prepared)) => prepared,
                Ok(Err(error)) => {
                    warn!(%error, "failed to read Album route order");
                    return;
                }
                Err(error) => {
                    warn!(%error, "Album route task failed");
                    return;
                }
            };
            if shell.navigation.routes.borrow().current() != &route
                || !shell.selected_library().as_deref().is_some_and(|current| {
                    current.source_key == source_key && current.source_session_epoch == epoch
                })
            {
                return;
            }
            let build_shell = Rc::clone(&shell);
            let build_route = route.clone();
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                build_shell.library_albums_route(
                    build_route,
                    favorites_only,
                    order,
                    first_rows,
                    selected,
                )
            });
        });
    }

    fn queue_artist_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        album_artist: bool,
        settings: LibraryListSettings,
        favorites_only: bool,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let source_id = selected.artwork.source_id.clone();
        let epoch = selected.source_session_epoch;
        let folder = selected.music_folder_key;
        let (read_generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            database
                .artist_route_page(
                    source_key,
                    folder,
                    album_artist,
                    favorites_only,
                    "",
                    settings.sort_key.artist_sort(),
                    settings.descending,
                    &cancellation,
                )
                .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(read_generation) {
                return;
            }
            let (order, first_rows) = match task.await {
                Ok(Ok(prepared)) => prepared,
                Ok(Err(error)) => {
                    warn!(%error, "failed to read Artist route order");
                    return;
                }
                Err(error) => {
                    warn!(%error, "Artist route task failed");
                    return;
                }
            };
            if shell.navigation.routes.borrow().current() != &route
                || !shell.selected_library().as_deref().is_some_and(|current| {
                    current.source_key == source_key && current.source_session_epoch == epoch
                })
            {
                return;
            }
            let build_shell = Rc::clone(&shell);
            let build_route = route.clone();
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                build_shell.library_artist_list_route(
                    build_route,
                    album_artist,
                    favorites_only,
                    order,
                    first_rows,
                    selected,
                )
            });
        });
    }

    fn queue_smart_playlist_detail_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        key: library::SmartPlaylistKey,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let epoch = selected.source_session_epoch;
        let source_id = selected.artwork.source_id.clone();
        let (generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            load_smart_playlist_detail(&database, source, folder, key, &cancellation).await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(generation) {
                return;
            }
            let Ok(Ok(detail)) = task.await else { return };
            if !route_is_current(&shell, &route, source, epoch) {
                return;
            }
            let build_shell = Rc::clone(&shell);
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                build_shell.smart_playlist_detail_route(key, detail, selected)
            });
        });
    }

    fn queue_playlist_detail_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        key: library::PlaylistKey,
        settings: LibraryListSettings,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let epoch = selected.source_session_epoch;
        let source_id = selected.artwork.source_id.clone();
        let (generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            load_playlist_detail(&database, source, folder, key, &settings, &cancellation).await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(generation) {
                return;
            }
            let Ok(Ok(detail)) = task.await else { return };
            if !route_is_current(&shell, &route, source, epoch) {
                return;
            }
            let build_shell = Rc::clone(&shell);
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                build_shell.playlist_detail_route(key, detail, selected)
            });
        });
    }

    fn queue_artist_overview_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        artist: library::ArtistKey,
        album_artist: bool,
        track_settings: LibraryListSettings,
        album_settings: LibraryListSettings,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let epoch = selected.source_session_epoch;
        let source_id = selected.artwork.source_id.clone();
        let (generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            load_artist_overview(
                &database,
                source,
                folder,
                artist,
                album_artist,
                &track_settings,
                &album_settings,
                &cancellation,
            )
            .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(generation) {
                return;
            }
            let Ok(Ok(detail)) = task.await else { return };
            if !route_is_current(&shell, &route, source, epoch) {
                return;
            }
            let build_shell = Rc::clone(&shell);
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                build_shell.artist_detail_view(artist, album_artist, detail, selected)
            });
        });
    }

    fn queue_artist_discography_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        artist: library::ArtistKey,
        album_artist: bool,
        settings: LibraryListSettings,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let epoch = selected.source_session_epoch;
        let source_id = selected.artwork.source_id.clone();
        let (generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            load_artist_discography(
                &database,
                source,
                folder,
                artist,
                album_artist,
                &settings,
                &cancellation,
            )
            .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(generation) {
                return;
            }
            let Ok(Ok(detail)) = task.await else { return };
            if !route_is_current(&shell, &route, source, epoch) {
                return;
            }
            let build_shell = Rc::clone(&shell);
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                build_shell.artist_discography_view(artist, album_artist, detail, selected)
            });
        });
    }

    fn queue_artist_tracks_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        artist: library::ArtistKey,
        album_artist: bool,
        settings: LibraryListSettings,
        favorites_only: bool,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let epoch = selected.source_session_epoch;
        let source_id = selected.artwork.source_id.clone();
        let (generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            load_artist_tracks(
                &database,
                source,
                folder,
                artist,
                album_artist,
                &settings,
                favorites_only,
                &cancellation,
            )
            .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(generation) {
                return;
            }
            let Ok(Ok(detail)) = task.await else { return };
            if !route_is_current(&shell, &route, source, epoch) {
                return;
            }
            let build_shell = Rc::clone(&shell);
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                if favorites_only {
                    build_shell.artist_favorite_tracks_view(artist, album_artist, detail, selected)
                } else {
                    build_shell.artist_tracks_view(artist, album_artist, detail, selected)
                }
            });
        });
    }

    fn queue_named_detail_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        id: NamedDetailId,
        settings: LibraryListSettings,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let source_id = selected.artwork.source_id.clone();
        let epoch = selected.source_session_epoch;
        let folder = selected.music_folder_key;
        let (read_generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            load_named_detail(&database, source, folder, id, &settings, &cancellation).await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(read_generation) {
                return;
            }
            let (summary, page) = match task.await {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    warn!(%error, "failed to read named detail route");
                    return;
                }
                Err(error) => {
                    warn!(%error, "named detail route task failed");
                    return;
                }
            };
            if shell.navigation.routes.borrow().current() != &route
                || !shell.selected_library().as_deref().is_some_and(|current| {
                    current.source_key == source && current.source_session_epoch == epoch
                })
            {
                return;
            }
            let build_shell = Rc::clone(&shell);
            let library::TrackRoutePage { order, first_rows } = page;
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                build_shell.named_detail_view(id, summary, order, first_rows, selected)
            });
        });
    }

    fn queue_playlist_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        settings: LibraryListSettings,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let source_id = selected.artwork.source_id.clone();
        let epoch = selected.source_session_epoch;
        let folder = selected.music_folder_key;
        let (read_generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            database
                .playlist_route_page(
                    source_key,
                    folder,
                    settings.sort_key.playlist_sort(),
                    settings.descending,
                    "",
                    &cancellation,
                )
                .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(read_generation) {
                return;
            }
            let (order, first_rows) = match task.await {
                Ok(Ok(prepared)) => prepared,
                Ok(Err(error)) => {
                    warn!(%error, "failed to read Playlist route order");
                    return;
                }
                Err(error) => {
                    warn!(%error, "Playlist route task failed");
                    return;
                }
            };
            if shell.navigation.routes.borrow().current() != &route
                || !shell.selected_library().as_deref().is_some_and(|current| {
                    current.source_key == source_key && current.source_session_epoch == epoch
                })
            {
                return;
            }
            let build_shell = Rc::clone(&shell);
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                build_shell.library_playlists_route(order, first_rows, selected)
            });
        });
    }

    fn queue_smart_playlist_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        settings: LibraryListSettings,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let source_id = selected.artwork.source_id.clone();
        let epoch = selected.source_session_epoch;
        let folder = selected.music_folder_key;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64);
        let (read_generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            database
                .smart_playlist_route_page(
                    source_key,
                    folder,
                    settings.sort_key.smart_playlist_sort(),
                    settings.descending,
                    now,
                    &cancellation,
                )
                .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(read_generation) {
                return;
            }
            let (order, first_rows) = match task.await {
                Ok(Ok(prepared)) => prepared,
                Ok(Err(error)) => {
                    warn!(%error, "failed to read Smart Playlist order");
                    return;
                }
                Err(error) => {
                    warn!(%error, "Smart Playlist task failed");
                    return;
                }
            };
            if shell.navigation.routes.borrow().current() != &route
                || !shell.selected_library().as_deref().is_some_and(|current| {
                    current.source_key == source_key && current.source_session_epoch == epoch
                })
            {
                return;
            }
            let build_shell = Rc::clone(&shell);
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                build_shell.library_smart_playlists_route(order, first_rows, selected)
            });
        });
    }

    fn queue_history_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let source_id = selected.artwork.source_id.clone();
        let epoch = selected.source_session_epoch;
        let folder = selected.music_folder_key;
        let (read_generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            database
                .history_track_page(source_key, folder, "", &cancellation)
                .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(read_generation) {
                return;
            }
            let page = match task.await {
                Ok(Ok(page)) => page,
                Ok(Err(error)) => {
                    warn!(%error, "failed to read History");
                    return;
                }
                Err(error) => {
                    warn!(%error, "History task failed");
                    return;
                }
            };
            if shell.navigation.routes.borrow().current() != &route
                || !shell.selected_library().as_deref().is_some_and(|current| {
                    current.source_key == source_key && current.source_session_epoch == epoch
                })
            {
                return;
            }
            let build_shell = Rc::clone(&shell);
            let library::TrackRoutePage {
                order: rows,
                first_rows,
            } = page;
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                build_shell.history_route(rows, first_rows, selected)
            });
        });
    }

    fn queue_genre_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        settings: LibraryListSettings,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let source_id = selected.artwork.source_id.clone();
        let epoch = selected.source_session_epoch;
        let folder = selected.music_folder_key;
        let (read_generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            database
                .genre_route_page(
                    source_key,
                    folder,
                    "",
                    settings.sort_key.genre_sort(),
                    settings.descending,
                    &cancellation,
                )
                .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(read_generation) {
                return;
            }
            let (order, first_rows) = match task.await {
                Ok(Ok(prepared)) => prepared,
                Ok(Err(error)) => {
                    warn!(%error, "failed to read Genre order");
                    return;
                }
                Err(error) => {
                    warn!(%error, "Genre order task failed");
                    return;
                }
            };
            if shell.navigation.routes.borrow().current() != &route
                || !shell.selected_library().as_deref().is_some_and(|current| {
                    current.source_key == source_key && current.source_session_epoch == epoch
                })
            {
                return;
            }
            let build_shell = Rc::clone(&shell);
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                build_shell.library_genres_route(order, first_rows, selected)
            });
        });
    }

    fn queue_mood_route(
        self: &Rc<Self>,
        selected: crate::runtime::SelectedLibrary,
        route: Route,
        settings: LibraryListSettings,
        render_started: Instant,
    ) {
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let source_id = selected.artwork.source_id.clone();
        let epoch = selected.source_session_epoch;
        let folder = selected.music_folder_key;
        let (read_generation, cancellation) = self.begin_root_order_read();
        let task = selected.runtime.spawn(async move {
            database
                .mood_route_page(
                    source_key,
                    folder,
                    "",
                    settings.sort_key.mood_sort(),
                    settings.descending,
                    &cancellation,
                )
                .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(read_generation) {
                return;
            }
            let (order, first_rows) = match task.await {
                Ok(Ok(prepared)) => prepared,
                Ok(Err(error)) => {
                    warn!(%error, "failed to read Mood order");
                    return;
                }
                Err(error) => {
                    warn!(%error, "Mood order task failed");
                    return;
                }
            };
            if shell.navigation.routes.borrow().current() != &route
                || !shell.selected_library().as_deref().is_some_and(|current| {
                    current.source_key == source_key && current.source_session_epoch == epoch
                })
            {
                return;
            }
            let build_shell = Rc::clone(&shell);
            shell.replace_mounted_route(route, Some(source_id), render_started, 0, move || {
                build_shell.library_moods_route(order, first_rows, selected)
            });
        });
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
        render_started: Instant,
        projection_ms: u64,
        build: impl FnOnce() -> MountedRoute,
    ) {
        let replacement_started = Instant::now();
        let previous = self.begin_mounted_route_replacement();
        let model_started = Instant::now();
        let view = build();
        let model_ms = elapsed_ms(model_started);

        let mount_started = Instant::now();
        let widget = view.widget();
        if let Some(previous) = previous {
            self.route_viewport.route_host.remove(&previous.surface);
            drop(previous);
        }
        let teardown_ms = elapsed_ms(replacement_started).saturating_sub(model_ms);
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
        let surface = match (scroll_adjustment.as_ref(), restore_position) {
            (Some(adjustment), Some(position)) => {
                restore_route_position_before_snapshot(&boundary, adjustment, position)
            }
            _ => boundary,
        };
        let context = self.take_current_route_context();
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
        self.install_current_route_context(context);
        self.route_viewport.route_host.set_visible_child(&surface);
        let mount_ms = elapsed_ms(mount_started);
        let total_ms = elapsed_ms(render_started);
        debug!(
            ?route,
            projection_ms, teardown_ms, model_ms, mount_ms, total_ms, "route render timing"
        );
        if total_ms >= SLOW_ROUTE_RENDER_MS {
            warn!(
                ?route,
                projection_ms, teardown_ms, model_ms, mount_ms, total_ms, "slow route render"
            );
        }
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
            self.route_viewport
                .position_memory
                .borrow_mut()
                .record(key.clone(), adjustment.value());
        }
        self.clear_favorite_controls();
        self.clear_current_route_context();
        previous
    }

    fn take_current_route_context(&self) -> RouteActivationContext {
        RouteActivationContext {
            current_track_selections: std::mem::take(
                &mut *self.route_viewport.current_track_selections.borrow_mut(),
            ),
            track_selections: std::mem::take(
                &mut *self.route_viewport.track_selections.borrow_mut(),
            ),
            playlist_entry_selection: self
                .route_viewport
                .playlist_entry_selection
                .borrow_mut()
                .take(),
            route_search: self.route_viewport.route_search.borrow_mut().take(),
            route_search_focus: self.route_viewport.route_search_focus.borrow_mut().take(),
            route_layout_cycle: self.route_viewport.route_layout_cycle.borrow_mut().take(),
            route_tab_cycle: self.route_viewport.route_tab_cycle.borrow_mut().take(),
        }
    }

    fn install_current_route_context(&self, context: RouteActivationContext) {
        self.route_viewport
            .current_track_selections
            .replace(context.current_track_selections);
        self.route_viewport
            .track_selections
            .replace(context.track_selections);
        self.route_viewport
            .playlist_entry_selection
            .replace(context.playlist_entry_selection);
        self.route_viewport
            .route_search
            .replace(context.route_search);
        self.route_viewport
            .route_search_focus
            .replace(context.route_search_focus);
        self.route_viewport
            .route_layout_cycle
            .replace(context.route_layout_cycle);
        self.route_viewport
            .route_tab_cycle
            .replace(context.route_tab_cycle);
    }

    fn clear_current_route_context(&self) {
        self.route_viewport.route_search.borrow_mut().take();
        self.route_viewport.route_search_focus.borrow_mut().take();
        self.route_viewport.route_layout_cycle.borrow_mut().take();
        self.route_viewport.route_tab_cycle.borrow_mut().take();
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
