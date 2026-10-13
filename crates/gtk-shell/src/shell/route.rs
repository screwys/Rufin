//! Navigation and the one mounted route.
//!
//! A route prepares one complete projection from the selected `Library`
//! away from GTK, then builds and mounts its GTK model. Home and playlist
//! detail have narrow point-update hooks because those operations are
//! intentionally visible without remounting the whole page.

use rufin_core::browse::{BrowseRequest, library_list_key};
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
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
use crate::{LibraryListKey, LibraryListSettings};
use gtk_library::route_layout::{primary_route_scroll_adjustment, route_boundary};
use gtk_library::{
    NamedDetailId, load_artist_discography, load_artist_overview, load_artist_tracks,
    load_named_detail, load_smart_playlist_detail,
};
use gtk_widgets::mounted_route::*;
use gtk_widgets::route::{CollectionCategory, Route};
use gtk_widgets::selection::{PlaylistEntrySelectionSnapshot, TrackSelectionSnapshot};

const SLOW_ROUTE_RENDER_MS: u64 = 250;

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
    order_cancellation: RefCell<Option<(u64, library::ReadCancellation)>>,
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
        B: FnOnce(Rc<gtk_library::CatalogUi>, T) -> MountedRoute + 'static,
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
                    shell.show_route_load_error(route, None, render_started);
                    return;
                }
                Err(error) => {
                    warn!(%error, ?route, context, "route preparation task failed");
                    shell.show_route_load_error(route, None, render_started);
                    return;
                }
            };
            shell.replace_mounted_route(route, None, render_started, move |catalog| {
                build(catalog, rows)
            });
        });
    }

    fn queue_prepared_route<T, E, F, Fut, B>(
        self: &Rc<Self>,
        selected: rufin_core::runtime::SelectedLibrary,
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
        B: FnOnce(
                Rc<gtk_library::CatalogUi>,
                T,
                rufin_core::runtime::SelectedLibrary,
            ) -> MountedRoute
            + 'static,
    {
        let source = selected.source_key;
        let source_id = selected.source_id.clone();
        let window = self.route_seed_window(&route);
        let (generation, cancellation) = self.begin_root_order_read();
        let task = self.products.runtime.spawn(load(window, cancellation));
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = task.await;
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(generation)
                || !route_is_current(&shell, &route, source)
            {
                return;
            }
            let prepared = match result {
                Ok(Ok(prepared)) => prepared,
                Ok(Err(error)) => {
                    warn!(%error, ?route, context, "failed to prepare route");
                    shell.show_route_load_error(route, Some(source_id), render_started);
                    return;
                }
                Err(error) => {
                    warn!(%error, ?route, context, "route preparation task failed");
                    shell.show_route_load_error(route, Some(source_id), render_started);
                    return;
                }
            };
            shell.replace_mounted_route(route, Some(source_id), render_started, move |catalog| {
                build(catalog, prepared, selected)
            });
        });
    }

    fn queue_home_route(
        self: &Rc<Self>,
        selected: rufin_core::runtime::SelectedLibrary,
        route: Route,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let (showcase_variation, explore_variation) = self.products.source.home_variations();
        let blocks = self.settings.current.borrow().home_blocks.clone();
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
                        &blocks,
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
        let database = Arc::clone(&self.products.library);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let (showcase_variation, explore_variation) = self.products.source.home_variations();
        let blocks = self.settings.current.borrow().home_blocks.clone();
        let (generation, cancellation) = self.begin_root_order_read();
        let task = self.products.runtime.spawn(async move {
            database
                .home_page(
                    source,
                    folder,
                    showcase_variation,
                    explore_variation,
                    &blocks,
                    &cancellation,
                )
                .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = task.await;
            let Some(shell) = shell.upgrade() else { return };
            if !shell.finish_root_order_read(generation)
                || !route_is_current(&shell, &Route::Home, source)
            {
                return;
            }
            let home = match result {
                Ok(Ok(home)) => home,
                Ok(Err(error)) => {
                    warn!(%error, "failed to refresh Home route");
                    shell.show_route_load_error(Route::Home, Some(selected.source_id), None);
                    return;
                }
                Err(error) => {
                    warn!(%error, "Home refresh task failed");
                    shell.show_route_load_error(Route::Home, Some(selected.source_id), None);
                    return;
                }
            };
            let view = shell
                .route_viewport
                .mounted_route
                .borrow()
                .as_ref()
                .filter(|entry| entry.route == Route::Home && !entry.load_failed)
                .map(|entry| entry.view.clone());
            if let Some(view) = view {
                view.apply_home_page(home);
            } else {
                let source_id = selected.source_id.clone();
                shell.replace_mounted_route(Route::Home, Some(source_id), None, move |catalog| {
                    catalog.home_route(selected, home)
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

    pub(crate) fn reset_navigation_to_home(self: &Rc<Self>) {
        self.close_activity();
        debug!("reset navigation to Home");
        self.player_ui.close_fullscreen_player();
        self.release_selected_navigation();
        self.render_current_route();
    }

    pub(crate) fn release_selected_navigation(&self) {
        self.route_viewport.position_memory.borrow_mut().clear();
        self.navigation.routes.borrow_mut().reset_to_home();
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
            .is_some_and(|entry| entry.route == route && !entry.load_failed);
        if !force && mounted_current {
            self.cancel_root_order_read();
            let current = self.route_viewport.mounted_route.borrow();
            self.route_viewport.route_loading.set_visible(
                current
                    .as_ref()
                    .is_some_and(|entry| !entry.surface.is_ready()),
            );
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
                let key = library_list_key(&route, CollectionCategory::Tracks);
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
                    .library_list(library_list_key(&route, CollectionCategory::Tracks));
                self.queue_playlist_detail_route(route, key, settings, render_started);
                return;
            }
            Route::SmartPlaylistDetail(key) => {
                self.queue_smart_playlist_detail_route(route, key, render_started);
                return;
            }
            Route::GenreDetail(genre_id) => {
                let id = NamedDetailId::Genre(genre_id);
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(library_list_key(&route, CollectionCategory::Tracks));
                self.queue_named_detail_route(route, id, settings, render_started);
                return;
            }
            Route::MoodDetail(mood_id) => {
                let id = NamedDetailId::Mood(mood_id);
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(library_list_key(&route, CollectionCategory::Tracks));
                self.queue_named_detail_route(route, id, settings, render_started);
                return;
            }
            Route::AlbumDetail(album_id) => {
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(library_list_key(&route, CollectionCategory::Tracks));
                self.queue_album_detail_route(route, album_id, settings, render_started);
                return;
            }
            Route::ArtistDetail(_) | Route::AlbumArtistDetail(_) => {
                let album_artist = matches!(&route, Route::AlbumArtistDetail(_));
                let track_settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(library_list_key(&route, CollectionCategory::Tracks));
                let album_settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(library_list_key(&route, CollectionCategory::Albums));
                self.queue_artist_overview_route(
                    route,
                    album_artist,
                    track_settings,
                    album_settings,
                    render_started,
                );
                return;
            }
            Route::ArtistDiscography(_) | Route::AlbumArtistDiscography(_) => {
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(library_list_key(&route, CollectionCategory::Albums));
                self.queue_artist_discography_route(route, settings, render_started);
                return;
            }
            Route::ArtistTracks(_) | Route::AlbumArtistTracks(_) => {
                let album_artist = matches!(&route, Route::AlbumArtistTracks(_));
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(library_list_key(&route, CollectionCategory::Tracks));
                self.queue_artist_tracks_route(
                    route,
                    album_artist,
                    settings,
                    false,
                    render_started,
                );
                return;
            }
            Route::ArtistFavoriteTracks(_) | Route::AlbumArtistFavoriteTracks(_) => {
                let album_artist = matches!(&route, Route::AlbumArtistFavoriteTracks(_));
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(library_list_key(&route, CollectionCategory::Tracks));
                self.queue_artist_tracks_route(route, album_artist, settings, true, render_started);
                return;
            }
            _ => {}
        }
        let Some(selected) = self.selected_library().as_deref().cloned() else {
            self.replace_mounted_route(route, None, render_started, |_| {
                MountedRoute::static_widget(gtk_library::route_layout::route_empty_view(
                    localization::msgid("Nothing here yet"),
                ))
            });
            return;
        };
        let source_id = Some(selected.source_id.clone());
        match route.clone() {
            Route::Home => {
                self.queue_home_route(selected, route, render_started);
            }
            Route::Search => {
                self.replace_mounted_route(route, source_id, render_started, |catalog| {
                    catalog.search_route()
                });
            }
            Route::SmartPlaylists => unreachable!("application-owned route"),
            Route::SmartPlaylistDetail(_) => unreachable!("application-owned route"),
            Route::Folders { path } => {
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(library_list_key(&route, CollectionCategory::Tracks));
                let source = self.products.source.clone();
                let database = self.products.library.clone();
                let load_selected = selected.clone();
                let load_path = path.clone();
                self.queue_prepared_route(
                    selected,
                    route,
                    render_started,
                    "Folder route",
                    move |window, cancellation| async move {
                        Ok::<_, std::convert::Infallible>(
                            gtk_library::folders::prepare_folder_route(
                                &source,
                                &database,
                                &load_selected,
                                &load_path,
                                &settings,
                                window,
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
                    .library_list(library_list_key(&route, CollectionCategory::Albums));
                self.queue_album_route(selected, route, settings, false, render_started);
            }
            Route::Tracks => {
                let settings = self
                    .settings
                    .current
                    .borrow()
                    .library_list(library_list_key(&route, CollectionCategory::Tracks));
                self.queue_root_track_route(selected, route, settings, render_started);
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
                        .library_list(library_list_key(&route, CollectionCategory::Tracks));
                    self.queue_root_track_route(selected, route, settings, render_started);
                }
                CollectionCategory::Albums => {
                    let settings = self
                        .settings
                        .current
                        .borrow()
                        .library_list(library_list_key(&route, CollectionCategory::Albums));
                    self.queue_album_route(selected, route, settings, true, render_started);
                }
                CollectionCategory::Artists => {
                    let settings = self
                        .settings
                        .current
                        .borrow()
                        .library_list(library_list_key(&route, CollectionCategory::Artists));
                    self.queue_artist_route(selected, route, false, settings, true, render_started);
                }
            },
            Route::History => unreachable!("History is application-owned"),
            Route::Genres | Route::Moods => {
                let genres = matches!(&route, Route::Genres);
                let key = library_list_key(&route, CollectionCategory::Tracks);
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
            Route::GenreDetail(_) | Route::MoodDetail(_) => unreachable!("application-owned route"),
            Route::PlaylistDetail(_) => unreachable!("application-owned route"),
        }
    }

    fn queue_root_track_route(
        self: &Rc<Self>,
        selected: rufin_core::runtime::SelectedLibrary,
        route: Route,
        settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        let source_key = selected.source_key;
        let folder = selected.music_folder_key;
        let build_route = route.clone();
        self.queue_prepared_route(
            selected,
            route.clone(),
            render_started,
            "Track route",
            move |window, cancellation| async move {
                database
                    .song_route_page(
                        &BrowseRequest::new(
                            &route,
                            CollectionCategory::Tracks,
                            Some(source_key),
                            folder,
                            &settings,
                            8,
                        )
                        .songs()
                        .expect("song route"),
                        window,
                        &cancellation,
                    )
                    .await
            },
            move |shell, page, selected| match build_route {
                Route::Tracks => shell.library_tracks_route(page, selected),
                Route::Favorites => shell.favorites_route(page, selected),
                _ => unreachable!("root Track route owner"),
            },
        );
    }

    fn queue_album_detail_route(
        self: &Rc<Self>,
        route: Route,
        album_uri: String,
        settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        self.queue_application_route(
            route,
            render_started,
            "Album detail route",
            move |window, cancellation| async move {
                gtk_library::load_album_detail(
                    &database,
                    &album_uri,
                    &settings,
                    window,
                    &cancellation,
                )
                .await
            },
            move |shell, detail| shell.album_detail_view(detail),
        );
    }

    fn queue_album_route(
        self: &Rc<Self>,
        selected: rufin_core::runtime::SelectedLibrary,
        route: Route,
        settings: LibraryListSettings,
        favorites_only: bool,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        let source_key = selected.source_key;
        let folder = selected.music_folder_key;
        self.queue_prepared_route(
            selected,
            route.clone(),
            render_started,
            "Album route",
            move |window, cancellation| async move {
                BrowseRequest {
                    favorites_only: favorites_only,
                    ..BrowseRequest::new(
                        &route,
                        CollectionCategory::Albums,
                        Some(source_key),
                        folder,
                        &settings,
                        8,
                    )
                }
                .collection()
                .expect("collection route")
                .page(&database, window, &cancellation)
                .await
                .map(|(count, start, rows)| match rows {
                    library::CollectionRows::Albums(rows) => (count, start, rows, Vec::new()),
                    library::CollectionRows::AlbumDetails(rows) => (count, start, Vec::new(), rows),
                    _ => unreachable!("album query"),
                })
            },
            move |shell, (order, first_row_position, first_rows, first_detail_rows), selected| {
                shell.library_albums_route(
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
        selected: rufin_core::runtime::SelectedLibrary,
        route: Route,
        album_artist: bool,
        settings: LibraryListSettings,
        favorites_only: bool,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        let source_key = selected.source_key;
        let folder = selected.music_folder_key;
        self.queue_prepared_route(
            selected,
            route.clone(),
            render_started,
            "Artist route",
            move |window, cancellation| async move {
                BrowseRequest {
                    favorites_only: favorites_only,
                    ..BrowseRequest::new(
                        &route,
                        CollectionCategory::Artists,
                        Some(source_key),
                        folder,
                        &settings,
                        8,
                    )
                }
                .collection()
                .expect("collection route")
                .page(&database, window, &cancellation)
                .await
                .map(|(count, start, rows)| (count, start, rows.artists()))
            },
            move |shell, (order, first_row_position, first_rows), selected| {
                shell.library_artist_list_route(
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
        let settings = self
            .settings
            .current
            .borrow()
            .library_list(LibraryListKey::SmartPlaylistTracks);
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
                load_smart_playlist_detail(
                    &database,
                    source,
                    folder,
                    key,
                    settings,
                    window,
                    &cancellation,
                )
                .await
            },
            move |shell, detail| shell.smart_playlist_detail_route(key, detail),
        );
    }

    fn queue_playlist_detail_route(
        self: &Rc<Self>,
        route: Route,
        key: library::PlaylistKey,
        settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        self.queue_application_route(
            route.clone(),
            render_started,
            "Playlist detail route",
            move |window, cancellation| async move {
                database
                    .playlist_detail_page(
                        &BrowseRequest::new(
                            &route,
                            CollectionCategory::Tracks,
                            None,
                            None,
                            &settings,
                            8,
                        )
                        .songs()
                        .expect("song route"),
                        window,
                        &cancellation,
                    )
                    .await
            },
            move |shell, detail| shell.playlist_detail_route(key, detail),
        );
    }

    fn queue_artist_overview_route(
        self: &Rc<Self>,
        route: Route,
        album_artist: bool,
        track_settings: LibraryListSettings,
        album_settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        self.queue_application_route(
            route.clone(),
            render_started,
            "Artist overview route",
            move |window, cancellation| async move {
                load_artist_overview(
                    &database,
                    &route,
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
                        gtk_library::route_layout::route_empty_view(localization::msgid(
                            "This isn't available",
                        )),
                    );
                };
                let artist = detail.summary.artist_key;
                shell.artist_detail_view(artist, album_artist, Some(detail))
            },
        );
    }

    fn queue_artist_discography_route(
        self: &Rc<Self>,
        route: Route,
        settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        self.queue_application_route(
            route.clone(),
            render_started,
            "Artist discography route",
            move |window, cancellation| async move {
                load_artist_discography(&database, &route, &settings, window, &cancellation).await
            },
            move |shell, detail| {
                let Some(detail) = detail else {
                    return MountedRoute::static_widget(
                        gtk_library::route_layout::route_empty_view(localization::msgid(
                            "This isn't available",
                        )),
                    );
                };
                shell.artist_discography_view(Some(detail))
            },
        );
    }

    fn queue_artist_tracks_route(
        self: &Rc<Self>,
        route: Route,
        album_artist: bool,
        settings: LibraryListSettings,
        favorites_only: bool,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        self.queue_application_route(
            route.clone(),
            render_started,
            "Artist tracks route",
            move |window, cancellation| async move {
                load_artist_tracks(&database, &route, &settings, window, &cancellation).await
            },
            move |shell, detail| {
                let Some(detail) = detail else {
                    return MountedRoute::static_widget(
                        gtk_library::route_layout::route_empty_view(localization::msgid(
                            "This isn't available",
                        )),
                    );
                };
                let artist = detail.summary.artist_key;
                if favorites_only {
                    shell.artist_favorite_tracks_view(artist, album_artist, Some(detail))
                } else {
                    shell.artist_tracks_view(artist, album_artist, Some(detail))
                }
            },
        );
    }

    fn queue_named_detail_route(
        self: &Rc<Self>,
        route: Route,
        id: NamedDetailId,
        settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        self.queue_application_route(
            route,
            render_started,
            "Named detail route",
            move |window, cancellation| async move {
                load_named_detail(&database, id, &settings, window, &cancellation).await
            },
            move |shell, detail| shell.named_detail_view(id, detail),
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
            route.clone(),
            render_started,
            "Playlist route",
            move |window, cancellation| async move {
                BrowseRequest::new(
                    &route,
                    CollectionCategory::Tracks,
                    source,
                    folder,
                    &settings,
                    8,
                )
                .collection()
                .expect("collection route")
                .page(&database, window, &cancellation)
                .await
                .map(|(count, start, rows)| (count, start, rows.playlists()))
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
            route.clone(),
            render_started,
            "Smart Playlist route",
            move |window, cancellation| async move {
                BrowseRequest {
                    now: now,
                    ..BrowseRequest::new(
                        &route,
                        CollectionCategory::Tracks,
                        source,
                        folder,
                        &settings,
                        8,
                    )
                }
                .collection()
                .expect("collection route")
                .page(&database, window, &cancellation)
                .await
                .map(|(count, start, rows)| (count, start, rows.smart_playlists()))
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
        let settings = self
            .settings
            .current
            .borrow()
            .library_list(LibraryListKey::History);
        self.queue_application_route(
            route.clone(),
            render_started,
            "History route",
            move |_, cancellation| async move {
                let query = BrowseRequest::new(
                    &route,
                    CollectionCategory::Tracks,
                    None,
                    None,
                    &settings,
                    8,
                )
                .songs()
                .expect("song route");
                let count = database.song_count(&query, &cancellation).await?.max(0) as usize;
                let rows = database
                    .song_history_rows(&query, 0, 64, &cancellation)
                    .await?;
                Ok::<_, library::LibraryError>((query, count, rows))
            },
            |shell, rows| shell.history_route(rows),
        );
    }

    fn queue_genre_route(
        self: &Rc<Self>,
        selected: rufin_core::runtime::SelectedLibrary,
        route: Route,
        settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        let source_key = selected.source_key;
        let folder = selected.music_folder_key;
        self.queue_prepared_route(
            selected,
            route.clone(),
            render_started,
            "Genre route",
            move |window, cancellation| async move {
                BrowseRequest::new(
                    &route,
                    CollectionCategory::Tracks,
                    Some(source_key),
                    folder,
                    &settings,
                    8,
                )
                .collection()
                .expect("collection route")
                .page(&database, window, &cancellation)
                .await
                .map(|(count, start, rows)| (count, start, rows.genres()))
            },
            |shell, (order, first_row_position, first_rows), selected| {
                shell.library_genres_route(order, first_row_position, first_rows, selected)
            },
        );
    }

    fn queue_mood_route(
        self: &Rc<Self>,
        selected: rufin_core::runtime::SelectedLibrary,
        route: Route,
        settings: LibraryListSettings,
        render_started: RouteTiming,
    ) {
        let database = Arc::clone(&self.products.library);
        let source_key = selected.source_key;
        let folder = selected.music_folder_key;
        self.queue_prepared_route(
            selected,
            route.clone(),
            render_started,
            "Mood route",
            move |window, cancellation| async move {
                BrowseRequest::new(
                    &route,
                    CollectionCategory::Tracks,
                    Some(source_key),
                    folder,
                    &settings,
                    8,
                )
                .collection()
                .expect("collection route")
                .page(&database, window, &cancellation)
                .await
                .map(|(count, start, rows)| (count, start, rows.moods()))
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
        let ready_shell = Rc::downgrade(self);
        let ready_view = view.clone();
        let surface = mount_route_surface(
            &boundary,
            scroll_adjustment.as_ref(),
            restore_position,
            view.item_scroll(),
            move || ready_view.resume_initial_demand(),
            move |surface| {
                if let Some(shell) = ready_shell.upgrade() {
                    shell.finish_route_loading(surface, render_started);
                }
            },
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
        self.prepare_startup_content_readiness();
        if let (Some(render_started), Some(teardown_ms), Some(model_ms), Some(mount_started)) =
            (render_started, teardown_ms, model_ms, mount_started)
        {
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
            .is_some_and(|entry| {
                entry.route == route && !entry.load_failed && entry.surface.is_ready()
            });
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

    fn finish_route_loading(self: &Rc<Self>, surface: &gtk::Widget, started: RouteTiming) {
        if self.route_viewport.order_cancellation.borrow().is_some() {
            return;
        }
        let ready = self
            .route_viewport
            .mounted_route
            .borrow()
            .as_ref()
            .is_some_and(|entry| {
                entry.surface.upcast_ref::<gtk::Widget>() == surface
                    && entry.route == *self.navigation.routes.borrow().current()
            });
        if ready {
            self.route_viewport.route_loading.set_visible(false);
            self.try_reveal_startup_route();
            debug!(
                total_ms = started.map(elapsed_ms),
                "visible route window ready"
            );
        }
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

    pub(super) fn mounted_route_is_ready(&self) -> bool {
        self.route_viewport
            .mounted_route
            .borrow()
            .as_ref()
            .is_some_and(|entry| entry.surface.is_ready())
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
