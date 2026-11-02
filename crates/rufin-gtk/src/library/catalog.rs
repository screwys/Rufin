use crate::library::track_selection::{PlaylistEntrySelection, TrackSelection};
use crate::shell::shell::Shell;
use crate::widgets::mounted_route::{RouteCurrentTrack, RouteCurrentTrackSelection};
use crate::widgets::selection::{PlaylistEntrySelectionSnapshot, TrackSelectionSnapshot};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// What a page needs from the window it is shown in, for the route and source it was opened for.
pub struct CatalogUi {
    shell: Weak<Shell>,
    route: rufin_core::route::Route,
    source_id: Option<sources::SourceId>,
    pub library: std::sync::Arc<library::Catalog>,
    pub runtime: tokio::runtime::Handle,
    pub source: rufin_core::runtime::SourceHandle,
    pub playback: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
    pub settings: Rc<crate::widgets::settings::SettingsState>,
    pub artwork: Rc<crate::widgets::artwork::ArtworkState>,
    pub downloads: Rc<crate::downloads::DownloadsState>,
    pub local_source: bool,
    pub selected: Option<rufin_core::runtime::SelectedLibrary>,
    pub favorites: Option<Rc<crate::favorites::FavoriteSessionState>>,
    pub window: gtk::glib::WeakRef<gtk::ApplicationWindow>,
    pub global_search: gtk::SearchEntry,
    pub search_session: Rc<crate::library::SearchSession>,
    pub history_source_name: Option<String>,

    pub current: RefCell<Option<RouteCurrentTrack>>,
    pub current_track_selections: RefCell<Vec<RouteCurrentTrackSelection>>,
    pub media_playing_cells:
        RefCell<Vec<std::rc::Weak<crate::library::grid_cells::MediaPlayingBinding>>>,
    pub track_selections: RefCell<Vec<TrackSelection>>,
    pub playlist_entry_selection: RefCell<Option<PlaylistEntrySelection>>,
}

impl CatalogUi {
    pub(crate) fn new(
        shell: &Rc<Shell>,
        route: &rufin_core::route::Route,
        source_id: Option<&sources::SourceId>,
    ) -> Rc<Self> {
        let selected = shell.selected_library().as_deref().cloned();
        let configured = shell.source.configured.borrow();
        let local_source = configured.sources.iter().any(|source| {
            source.kind == "local"
                && selected
                    .as_ref()
                    .is_some_and(|selected| selected.source_id == source.id)
        });
        let history_source_name = configured
            .sources
            .iter()
            .find(|source| Some(&source.id) == configured.selected_source_id.as_ref())
            .map(crate::widgets::source_labels::configured_source_display_name);
        drop(configured);
        Rc::new(Self {
            shell: Rc::downgrade(shell),
            route: route.clone(),
            source_id: source_id.cloned(),
            library: shell.products.library.clone(),
            runtime: shell.products.runtime.clone(),
            source: shell.products.source.clone(),
            playback: shell.products.playback.clone(),
            settings: Rc::clone(&shell.settings),
            artwork: Rc::clone(&shell.artwork),
            downloads: Rc::clone(&shell.downloads),
            local_source,
            selected,
            favorites: shell
                .selected_ui
                .session()
                .map(|session| Rc::clone(&session.favorites)),
            window: shell.chrome.window.downgrade(),
            global_search: shell.chrome.topbar.search.clone(),
            search_session: Rc::clone(&shell.search_session),
            history_source_name,
            current: RefCell::new(None),
            current_track_selections: RefCell::new(Vec::new()),
            media_playing_cells: RefCell::new(Vec::new()),
            track_selections: RefCell::new(Vec::new()),
            playlist_entry_selection: RefCell::new(None),
        })
    }

    /// The window, while it is open.
    pub(crate) fn shell(&self) -> Option<Rc<Shell>> {
        self.shell.upgrade()
    }

    pub(crate) fn weak_shell(&self) -> Weak<Shell> {
        self.shell.clone()
    }

    /// Whether the window still shows the route and source this page was opened for.
    pub fn is_current(&self) -> bool {
        self.shell().is_some_and(|shell| {
            shell.navigation.routes.borrow().current() == &self.route
                && self.source_id.as_ref().is_none_or(|id| {
                    shell
                        .selected_library()
                        .as_deref()
                        .is_some_and(|selected| &selected.source_id == id)
                })
        })
    }

    /// `is_current` for work that finishes after this call.
    pub fn current_check(self: &Rc<Self>) -> Rc<dyn Fn() -> bool> {
        let page = Rc::downgrade(self);
        Rc::new(move || page.upgrade().is_some_and(|page| page.is_current()))
    }

    pub fn route_width(&self) -> i32 {
        self.shell().map_or(1, |shell| {
            crate::shell::shell::layout::route_content_width(&shell)
        })
    }

    /// A table's width before it is allocated: the route's width less its inset and the
    /// fitted table's padding.
    pub fn table_initial_width(&self, content_inset: i32) -> i32 {
        const FITTED_TABLE_WIDTH_PADDING: i32 = 2;
        self.route_width()
            .saturating_sub(content_inset.max(0))
            .saturating_sub(FITTED_TABLE_WIDTH_PADDING)
            .max(1)
    }

    pub fn source_label(&self, source_id: Option<&str>) -> (&'static str, String) {
        let Some(shell) = self.shell() else {
            return crate::widgets::source_labels::source_display_label(None);
        };
        let configured = shell.source.configured.borrow();
        crate::widgets::source_labels::source_display_label(
            configured
                .sources
                .iter()
                .find(|source| Some(source.id.as_str()) == source_id),
        )
    }

    pub fn history_source_id(&self) -> Option<sources::SourceId> {
        self.shell()
            .and_then(|shell| shell.source.configured.borrow().selected_source_id.clone())
    }

    pub fn refresh_home_section(&self, kind: rufin_core::settings::HomeSectionKind) {
        if let Some(selected) = &self.selected {
            self.source.refresh_home(selected.source_id.clone(), kind);
        }
    }

    /// Rereads the shown page after its settings change.
    pub fn reconcile(&self) {
        if let Some(shell) = self.shell() {
            shell.reconcile_mounted_route();
        }
    }

    pub fn refresh(&self) {
        if let Some(shell) = self.shell() {
            shell.replace_current_route_when_ready();
        }
    }

    pub fn present_full_artwork(&self, artwork: Vec<artwork::ArtworkBinding>) {
        if let Some(shell) = self.shell() {
            shell.present_full_artwork_group(artwork);
        }
    }

    pub fn present_selected_dialog(&self, dialog: &impl IsA<adw::Dialog>) {
        if let Some(shell) = self.shell() {
            shell.present_selected_dialog(dialog);
        }
    }

    pub fn present_playlist_context_menu(
        &self,
        target: &gtk::Widget,
        playlist: library::PlaylistRow,
        play: Option<crate::menus::media_menus::CollectionPlay>,
        position: Option<(f64, f64)>,
    ) {
        if let Some(shell) = self.shell() {
            let playing = crate::player::state::current_playback_track(
                shell.player_ui.selected_playback().as_deref(),
            )
            .map(|track| track.media_uri);
            crate::menus::media_menus::present_playlist_context_menu(
                target, &shell, playlist, play, position, playing,
            );
        }
    }

    pub fn update_smart_playlist_pin(&self, summary: &library::SmartPlaylistRow) {
        if let Some(shell) = self.shell() {
            crate::shell::shell::navigation::update_sidebar_smart_playlist_pin_metadata(
                &shell, summary,
            );
        }
    }

    pub fn set_favorite_category(&self, category: rufin_core::route::CollectionCategory) {
        if let Some(shell) = self.shell() {
            shell.set_favorite_category(category);
        }
    }

    pub fn cycle_favorite_category(&self) {
        if let Some(shell) = self.shell() {
            shell.set_favorite_category(shell.navigation.favorite_category.get().next());
        }
    }

    pub fn bind_download_badge(&self, image: &gtk::Image, downloaded: bool) {
        self.downloads
            .bind_download_badge(image, downloaded && !self.local_source);
    }

    pub fn register_current_route_track_selection(&self, selection: RouteCurrentTrackSelection) {
        let current = self.current.borrow();
        if selection(current.as_ref()) {
            self.current_track_selections.borrow_mut().push(selection);
        }
    }

    pub fn register_current_route_track_selection_owner(
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
            for other in shell.track_selections.borrow().iter() {
                if !other.uses_selection(changed_selection) {
                    other.clear();
                }
            }
        });
        self.track_selections.borrow_mut().push(selection);
    }

    pub fn current_route_track_selection(&self, clicked: &str) -> Option<TrackSelectionSnapshot> {
        self.track_selections
            .borrow()
            .iter()
            .find_map(|selection| selection.selected_tracks_for(clicked))
    }

    pub fn current_route_track_selection_snapshot(&self) -> Option<TrackSelectionSnapshot> {
        self.track_selections
            .borrow()
            .iter()
            .find_map(TrackSelection::selected_tracks)
    }

    pub fn set_current_playlist_entry_selection(&self, selection: PlaylistEntrySelection) {
        self.playlist_entry_selection.replace(Some(selection));
    }

    pub fn current_playlist_entry_selection(
        &self,
        clicked: library::PlaylistEntryKey,
    ) -> Option<PlaylistEntrySelectionSnapshot> {
        self.playlist_entry_selection
            .borrow()
            .as_ref()
            .and_then(|selection| selection.selected_entries_for(clicked))
    }

    pub fn current_playlist_entry_selection_snapshot(
        &self,
    ) -> Option<PlaylistEntrySelectionSnapshot> {
        self.playlist_entry_selection
            .borrow()
            .as_ref()
            .and_then(PlaylistEntrySelection::selected_entries)
    }

    pub fn current_playlist_entry_selection_owner(&self) -> Option<PlaylistEntrySelection> {
        self.playlist_entry_selection.borrow().clone()
    }

    pub fn refresh_current_route_now_playing_selections(&self, current: Option<RouteCurrentTrack>) {
        self.current.replace(current);
        self.reapply_current_track();
    }
}

impl CatalogUi {
    pub fn selected_library(&self) -> Option<&rufin_core::runtime::SelectedLibrary> {
        self.selected.as_ref()
    }
    /// Navigation for widgets that open routes from links.
    pub fn route_navigation(&self) -> Rc<dyn Fn(rufin_core::route::Route)> {
        let shell = self.shell.clone();
        Rc::new(move |route| {
            if let Some(shell) = shell.upgrade() {
                shell.open_route(route);
            }
        })
    }
    pub fn navigate(&self, route: rufin_core::route::Route) {
        if let Some(shell) = self.shell() {
            shell.open_route(route);
        }
    }
    pub fn update_library_list_settings(
        &self,
        key: rufin_core::settings::LibraryListKey,
        update: impl FnOnce(&mut rufin_core::settings::LibraryListSettings),
    ) {
        if self.settings.update_library_list_settings(key, update) {
            self.reconcile();
        }
    }
    pub fn library_settings_changed(&self) -> Rc<dyn Fn()> {
        let shell = self.shell.clone();
        Rc::new(move || {
            if let Some(shell) = shell.upgrade() {
                shell.reconcile_mounted_route();
            }
        })
    }
    pub fn register_favorite_button(
        &self,
        key: crate::favorites::FavoriteControlKey,
        button: &gtk::Button,
    ) {
        if let Some(favorites) = &self.favorites {
            favorites.register_favorite_button(key, button);
        }
    }
    pub fn register_dynamic_favorite_button(
        &self,
        key: Rc<dyn Fn() -> Option<crate::favorites::FavoriteControlKey>>,
        button: &gtk::Button,
    ) {
        if let Some(favorites) = &self.favorites {
            favorites.register_dynamic_favorite_button(key, button);
        }
    }
    pub fn update_visible_favorite_buttons(
        &self,
        target: &library::FavoriteTarget,
        favorite: bool,
    ) {
        if let Some(favorites) = &self.favorites {
            favorites.update_visible_favorite_buttons(target, favorite);
        }
    }
    /// Hearts change when the favorite is committed, from the publication that follows.
    pub fn set_favorite(&self, target: library::FavoriteTarget, favorite: bool) {
        if let Some(shell) = self.shell() {
            shell.set_favorite(target, favorite);
        }
    }
    pub fn reapply_current_track(&self) {
        let current = self.current.borrow();
        self.media_playing_cells.borrow_mut().retain(|cell| {
            let Some(cell) = cell.upgrade() else {
                return false;
            };
            cell.refresh(current.as_ref());
            true
        });
        self.current_track_selections
            .borrow_mut()
            .retain(|selection| selection(current.as_ref()));
    }
}
