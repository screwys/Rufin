//! Navigation and the one mounted route.
//!
//! A route prepares one complete projection from the selected `Library`
//! away from GTK, then builds and mounts its GTK model. Home and playlist
//! detail have narrow point-update hooks because those operations are
//! intentionally visible without remounting the whole page.
use rufin_core::browse::{OpenOptions, OpenedCollection};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use adw::prelude::*;
use gtk::glib;
use library::SourceKey;
use sources::SourceId;
use tracing::{debug, warn};

use super::Shell;
use super::navigation::update_navigation_selection;
use super::route_position::{
    RoutePosition, RoutePositionKey, RoutePositionMemory, RouteSurface, mount_route_surface,
};
use gtk_library::route_layout::{primary_route_scroll_adjustment, route_boundary};
use gtk_widgets::mounted_route::*;
use gtk_widgets::route::{CollectionCategory, Route};
use gtk_widgets::selection::{PlaylistEntrySelectionSnapshot, TrackSelectionSnapshot};

const SLOW_ROUTE_LOAD_MS: u64 = 250;

type RouteTiming = Option<Instant>;

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
    fn initial_route_has_no_navigation_history() {
        for item in rufin_core::settings::SidebarRouteItem::all() {
            let initial = rufin_core::route::sidebar_route_descriptor(item).root_route;
            let mut routes = RouteStack::new(initial.clone());
            assert_eq!(routes.current(), &initial);
            assert!(!routes.can_back());
            assert!(routes.forward().is_none());
        }
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
            path: vec![gtk_widgets::route::FolderPathItem {
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

fn route_is_current(shell: &Shell, route: &Route, source: SourceKey) -> bool {
    shell.navigation.routes.borrow().current() == route
        && shell
            .selected_library()
            .as_deref()
            .is_some_and(|selected| selected.source_key == source)
}

pub(crate) struct RouteViewport {
    pub(super) route_host: gtk::Stack,
    route_loading: gtk::Box,
    mounted_route: RefCell<Option<MountedRouteEntry>>,
    position_memory: RefCell<RoutePositionMemory>,
    catalog: RefCell<Option<Rc<gtk_library::CatalogUi>>>,
    order_read_generation: Cell<u64>,
    pub(super) order_cancellation: RefCell<Option<(u64, library::ReadCancellation)>>,
}

struct MountedRouteEntry {
    route: Route,
    load_failed: bool,
    view: MountedRoute,
    surface: RouteSurface,
    position_key: Option<RoutePositionKey>,
    scroll_adjustment: Option<gtk::Adjustment>,
}

impl RouteViewport {
    pub(super) fn new(route_host: gtk::Stack, route_loading: gtk::Box) -> Self {
        Self {
            route_host,
            route_loading,
            mounted_route: RefCell::new(None),
            position_memory: RefCell::new(RoutePositionMemory::default()),
            catalog: RefCell::new(None),
            order_read_generation: Cell::new(0),
            order_cancellation: RefCell::new(None),
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

    #[cfg(test)]
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
    pub(super) fn route_uses_selected_source(&self, route: &Route) -> bool {
        route.uses_selected_source()
            && (route != &Route::Search || self.search_session.mode() == library::SearchMode::Title)
    }

    fn route_seed_window(&self, route: &Route) -> library::RouteSeedWindow {
        let mounted = self.route_viewport.mounted_route.borrow();
        let current = mounted.as_ref().filter(|entry| entry.route == *route);
        let position = current
            .and_then(|entry| {
                entry.scroll_adjustment.as_ref().map(|adjustment| {
                    RoutePosition::capture(adjustment, entry.view.item_scroll().as_ref())
                })
            })
            .or_else(|| {
                self.route_viewport
                    .position_memory
                    .borrow_mut()
                    .restore(&RoutePositionKey::new(route.clone()))
            });
        position.map_or_else(library::RouteSeedWindow::top, RoutePosition::window)
    }

    fn queue_collection_route(
        self: &Rc<Self>,
        route: Route,
        selected: Option<rufin_core::runtime::SelectedLibrary>,
        options: OpenOptions,
        refresh_home: bool,
        render_started: RouteTiming,
    ) {
        let source = self.products.source.clone();
        let source_scoped = self.route_uses_selected_source(&route);
        let window = if refresh_home {
            library::RouteSeedWindow::top()
        } else {
            self.route_seed_window(&route)
        };
        let (generation, cancellation) = self.begin_root_order_read();
        let load_route = route.clone();
        let load_selected = selected.clone();
        let task = self.products.runtime.spawn(async move {
            source
                .open_collection(
                    &load_route,
                    load_selected.as_ref(),
                    &options,
                    window,
                    &cancellation,
                )
                .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = task.await;
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(generation)
                || shell.navigation.routes.borrow().current() != &route
                || source_scoped
                    && selected.as_ref().is_some_and(|selected| {
                        !route_is_current(&shell, &route, selected.source_key)
                    })
            {
                return;
            }
            let source_id = source_scoped
                .then(|| selected.as_ref().map(|selected| selected.source_id.clone()))
                .flatten();
            let opened = match result {
                Ok(Ok(opened)) => opened,
                Ok(Err(error)) => {
                    warn!(%error, ?route, "failed to open collection");
                    shell.show_route_load_error(route, source_id, render_started);
                    return;
                }
                Err(error) => {
                    warn!(%error, ?route, "collection task failed");
                    shell.show_route_load_error(route, source_id, render_started);
                    return;
                }
            };
            let apply_home = if refresh_home && shell.mounted_catalog_is_current() {
                shell
                    .route_viewport
                    .mounted_route
                    .borrow()
                    .as_ref()
                    .filter(|entry| !entry.load_failed)
                    .and_then(|entry| entry.view.home_page_apply.clone())
            } else {
                None
            };
            match (opened, apply_home) {
                (OpenedCollection::Home(home), Some(apply)) => apply(*home),
                (opened, _) => shell.replace_mounted_route(
                    route.clone(),
                    source_id,
                    render_started,
                    move |catalog| catalog.opened_collection_view(route, opened, selected),
                ),
            }
        });
    }

    pub(crate) fn refresh_mounted_home(self: &Rc<Self>) {
        if self.navigation.routes.borrow().current() != &Route::Home {
            return;
        }
        let Some(selected) = self.selected_library().as_deref().cloned() else {
            return;
        };
        self.queue_collection_route(
            Route::Home,
            Some(selected),
            OpenOptions::default(),
            true,
            None,
        );
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

    pub(crate) fn current_route_track_selection_snapshot(&self) -> Option<TrackSelectionSnapshot> {
        self.route_viewport
            .catalog
            .borrow()
            .as_ref()
            .and_then(|catalog| catalog.current_route_track_selection_snapshot())
    }

    pub(crate) fn current_playlist_entry_selection_snapshot(
        &self,
    ) -> Option<PlaylistEntrySelectionSnapshot> {
        self.route_viewport
            .catalog
            .borrow()
            .as_ref()
            .and_then(|catalog| catalog.current_playlist_entry_selection_snapshot())
    }

    pub(crate) fn refresh_current_route_now_playing_selections(&self) {
        self.refresh_activity_playback();
        let current = route_current_track(self.player_ui.selected_playback().as_deref());
        self.chrome.topbar.refresh_search_playback(current.as_ref());
        if let Some(catalog) = self.route_viewport.catalog.borrow().as_ref() {
            catalog.refresh_current_route_now_playing_selections(current);
        }
    }

    pub(crate) fn navigate(self: &Rc<Self>, route: Route) {
        self.close_activity();
        debug!(?route, "navigate");
        self.player_ui.close_fullscreen_player();
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

    pub(crate) fn go_back(self: &Rc<Self>) {
        if self.close_activity() {
            return;
        }
        let route = self.navigation.routes.borrow_mut().back().cloned();
        if let Some(route) = route {
            debug!(?route, "navigate back");
            gtk::prelude::RootExt::set_focus(&self.chrome.window, None::<&gtk::Widget>);
            self.render_current_route();
        }
    }

    pub(crate) fn go_forward(self: &Rc<Self>) {
        if self.close_activity() {
            return;
        }
        let route = self.navigation.routes.borrow_mut().forward().cloned();
        if let Some(route) = route {
            debug!(?route, "navigate forward");
            gtk::prelude::RootExt::set_focus(&self.chrome.window, None::<&gtk::Widget>);
            self.render_current_route();
        }
    }

    pub(crate) fn render_current_route(self: &Rc<Self>) {
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
            .is_some_and(|entry| entry.route == route && !entry.load_failed)
            && self.mounted_catalog_is_current();
        if !force && mounted_current {
            self.cancel_root_order_read();
            self.cancel_route_loading();
            return;
        }

        self.cancel_route_loading();
        self.show_route_loading();
        let render_started = tracing::enabled!(tracing::Level::WARN).then(Instant::now);
        let source_scoped = self.route_uses_selected_source(&route);
        let selected = self.selected_library().as_deref().cloned();
        if source_scoped && route.requires_selected_source() && selected.is_none() {
            self.cancel_root_order_read();
            self.replace_mounted_route(route, None, render_started, |_| {
                MountedRoute::static_widget(gtk_library::route_layout::route_empty_view(
                    localization::msgid("Nothing here yet"),
                ))
            });
            return;
        }
        let selected = if source_scoped { selected } else { None };
        if route == Route::Search {
            self.cancel_root_order_read();
            let source_id = selected.as_ref().map(|selected| selected.source_id.clone());
            self.replace_mounted_route(route, source_id, render_started, |catalog| {
                catalog.search_route()
            });
            return;
        }
        let category = if route == Route::Favorites {
            self.navigation.favorite_category.get()
        } else if matches!(
            route,
            Route::Albums
                | Route::ArtistDetail(_)
                | Route::AlbumArtistDetail(_)
                | Route::ArtistDiscography(_)
                | Route::AlbumArtistDiscography(_)
        ) {
            CollectionCategory::Albums
        } else {
            CollectionCategory::Tracks
        };
        self.queue_collection_route(
            route,
            selected,
            OpenOptions {
                category,
                ..Default::default()
            },
            false,
            render_started,
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
        build: impl FnOnce(Rc<gtk_library::CatalogUi>) -> MountedRoute,
    ) {
        let teardown_started = render_started.as_ref().map(|_| Instant::now());
        let previous = self.begin_mounted_route_replacement();
        let release_ms = teardown_started.map(elapsed_ms);
        let model_started = render_started.as_ref().map(|_| Instant::now());
        let catalog = self.build_catalog(&route, source_id.as_ref());
        catalog.refresh_current_route_now_playing_selections(route_current_track(
            self.player_ui.selected_playback().as_deref(),
        ));
        self.route_viewport
            .catalog
            .replace(Some(Rc::clone(&catalog)));
        let view = build(catalog);
        let model_ms = model_started.map(elapsed_ms);

        let removal_started = render_started.as_ref().map(|_| Instant::now());
        let widget = view.widget();
        if let Some(previous) = previous {
            if gtk::prelude::RootExt::focus(&self.chrome.window).is_some_and(|focus| {
                focus == *previous.surface.upcast_ref::<gtk::Widget>()
                    || focus.is_ancestor(&previous.surface)
            }) {
                // Clear focus before removal so GTK does not move it into the new page.
                gtk::prelude::RootExt::set_focus(&self.chrome.window, None::<&gtk::Widget>);
            }
            self.route_viewport.route_host.remove(&previous.surface);
            drop(previous);
        }
        let teardown_ms =
            removal_started.map(|started| elapsed_ms(started) + release_ms.unwrap_or(0));
        let mount_started = render_started.as_ref().map(|_| Instant::now());
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
        let ready_view = view.clone();
        let surface = mount_route_surface(
            &boundary,
            scroll_adjustment.as_ref(),
            restore_position,
            view.item_scroll(),
            move || ready_view.resume_initial_demand(),
        );
        self.route_viewport.route_host.add_child(&surface);
        let displaced = self
            .route_viewport
            .mounted_route
            .replace(Some(MountedRouteEntry {
                route: route.clone(),
                load_failed: false,
                view: view.clone(),
                surface: surface.clone(),
                position_key,
                scroll_adjustment,
            }));
        debug_assert!(displaced.is_none());
        self.route_viewport.route_host.set_visible_child(&surface);
        self.cancel_route_loading();
        self.finish_startup_route();
        if let (Some(render_started), Some(teardown_ms), Some(model_ms), Some(mount_started)) =
            (render_started, teardown_ms, model_ms, mount_started)
        {
            let mount_ms = elapsed_ms(mount_started);
            let total_ms = elapsed_ms(render_started);
            debug!(
                ?route,
                teardown_ms, model_ms, mount_ms, total_ms, "route render timing"
            );
            if total_ms >= SLOW_ROUTE_LOAD_MS {
                warn!(?route, total_ms, "slow route load");
            }
        }
    }

    fn show_route_load_error(
        self: &Rc<Self>,
        route: Route,
        source_id: Option<SourceId>,
        render_started: RouteTiming,
    ) {
        let resource = crate::ui_resource::ROUTE_ERROR_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            root: gtk::Box,
            message: gtk::Label,
            refresh: gtk::Button,
        });
        let keep_content = self
            .route_viewport
            .mounted_route
            .borrow()
            .as_ref()
            .is_some_and(|entry| entry.route == route && !entry.load_failed)
            && self.mounted_catalog_is_current();
        if keep_content {
            self.cancel_route_loading();
            self.control_feedback
                .show_feedback_toast(message.text().to_string());
            return;
        }
        let shell = Rc::downgrade(self);
        let reload: MountedRouteResume = Rc::new(move || {
            if let Some(shell) = shell.upgrade() {
                shell.replace_current_route_when_ready();
            }
        });
        let clicked = Rc::clone(&reload);
        refresh.connect_clicked(move |_| clicked());
        self.replace_mounted_route(route, source_id, render_started, move |_| {
            MountedRoute::static_widget(root.upcast()).with_catalog_refresh(reload)
        });
        if let Some(entry) = self.route_viewport.mounted_route.borrow_mut().as_mut() {
            entry.load_failed = true;
        }
    }

    fn show_route_loading(&self) {
        self.route_viewport.route_loading.set_visible(true);
    }

    fn cancel_route_loading(&self) {
        self.route_viewport.route_loading.set_visible(false);
    }

    pub(crate) fn apply_favorite_settlement_to_mounted_route(
        &self,
        settlement: rufin_core::runtime::FavoriteSettlement,
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

    fn mounted_catalog_is_current(&self) -> bool {
        let scope = |selected: &rufin_core::runtime::SelectedLibrary| {
            (selected.source_key, selected.music_folder_key)
        };
        let source_scoped =
            self.route_uses_selected_source(self.navigation.routes.borrow().current());
        let selected_scope = self.selected_library().as_deref().map(scope);
        self.route_viewport
            .catalog
            .borrow()
            .as_ref()
            .is_some_and(|catalog| {
                (catalog.is_current)()
                    && (!source_scoped || catalog.selected.as_ref().map(scope) == selected_scope)
            })
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
                RoutePosition::capture(adjustment, previous.view.item_scroll().as_ref()),
            );
        }
        self.clear_favorite_controls();
        self.clear_current_route_selections();
        previous
    }

    fn clear_current_route_selections(&self) {
        self.route_viewport.catalog.borrow_mut().take();
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}
