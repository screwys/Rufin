use crate::library::track_selection::{PlaylistEntrySelection, TrackSelection};
use crate::shell::shell::Shell;
use crate::widgets::mounted_route::{RouteCurrentTrack, RouteCurrentTrackSelection};
use crate::widgets::selection::{PlaylistEntrySelectionSnapshot, TrackSelectionSnapshot};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// A page of the window: the route and library it was opened for, and what it shows of the
/// current track and its selections. Everything else is the window's, through `Deref`.
pub(crate) struct CatalogUi {
    shell: Rc<Shell>,
    route: rufin_core::route::Route,
    selected: Option<rufin_core::runtime::SelectedLibrary>,
    pub current: RefCell<Option<RouteCurrentTrack>>,
    pub current_track_selections: RefCell<Vec<RouteCurrentTrackSelection>>,
    pub media_playing_cells:
        RefCell<Vec<std::rc::Weak<crate::library::grid_cells::MediaPlayingBinding>>>,
    pub track_selections: RefCell<Vec<TrackSelection>>,
    pub playlist_entry_selection: RefCell<Option<PlaylistEntrySelection>>,
}

impl std::ops::Deref for CatalogUi {
    type Target = Rc<Shell>;

    fn deref(&self) -> &Rc<Shell> {
        &self.shell
    }
}

impl CatalogUi {
    pub(crate) fn new(
        shell: &Rc<Shell>,
        route: &rufin_core::route::Route,
        selected: Option<&rufin_core::runtime::SelectedLibrary>,
    ) -> Rc<Self> {
        Rc::new(Self {
            shell: Rc::clone(shell),
            route: route.clone(),
            selected: selected.cloned(),
            current: RefCell::new(None),
            current_track_selections: RefCell::new(Vec::new()),
            media_playing_cells: RefCell::new(Vec::new()),
            track_selections: RefCell::new(Vec::new()),
            playlist_entry_selection: RefCell::new(None),
        })
    }

    /// The window the page is shown in.
    pub(crate) fn shell(&self) -> &Rc<Shell> {
        &self.shell
    }

    /// Whether the window still shows the route this page was opened for, reading what it read
    /// in the library it was opened for.
    pub fn is_current(&self) -> bool {
        self.navigation.routes.borrow().current() == &self.route
            && self.selected.as_ref().is_none_or(|opened| {
                rufin_core::view::Scope::of(&self.route, self.search_session.mode())
                    .same(Some(opened), self.selected_library().as_deref())
            })
    }

    /// `is_current` for work that finishes after this call.
    pub fn current_check(self: &Rc<Self>) -> Rc<dyn Fn() -> bool> {
        let page = Rc::downgrade(self);
        Rc::new(move || page.upgrade().is_some_and(|page| page.is_current()))
    }

    /// Whether the selected source is a Local one, whose tracks are on this device already.
    pub fn local_source(&self) -> bool {
        let selected = self.selected_library();
        let configured = self.source.configured.borrow();
        selected.is_some_and(|selected| {
            configured.sources.iter().any(|source| {
                source.id == selected.source_id
                    && library::SourceKind::parse(&source.kind) == Some(library::SourceKind::Local)
            })
        })
    }

    pub fn route_width(&self) -> i32 {
        crate::shell::shell::layout::route_content_width(&self.shell)
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
        let configured = self.source.configured.borrow();
        crate::widgets::source_labels::source_display_label(
            configured
                .sources
                .iter()
                .find(|source| Some(source.id.as_str()) == source_id),
        )
    }

    pub fn history_source_id(&self) -> Option<sources::SourceId> {
        self.source.configured.borrow().selected_source_id.clone()
    }

    pub fn refresh_home_section(&self, kind: rufin_core::settings::HomeSectionKind) {
        if let Some(selected) = self.selected_library() {
            self.products
                .source
                .refresh_home(selected.source_id.clone(), kind);
        }
    }

    pub fn update_smart_playlist_pin(&self, summary: &library::SmartPlaylistRow) {
        crate::shell::shell::navigation::update_sidebar_smart_playlist_pin_metadata(
            &self.shell,
            summary,
        );
    }

    pub fn cycle_favorite_category(&self) {
        self.set_favorite_category(self.navigation.favorite_category.get().next());
    }

    pub fn bind_download_badge(&self, image: &gtk::Image, downloaded: bool) {
        self.downloads
            .bind_download_badge(image, downloaded && !self.local_source());
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
        let page = Rc::downgrade(self);
        selection.connect_changed(move |changed_selection| {
            if changed_selection.selection().is_empty() {
                return;
            }
            let Some(page) = page.upgrade() else {
                return;
            };
            for other in page.track_selections.borrow().iter() {
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

    /// Navigation for widgets that open routes from links.
    pub fn route_navigation(&self) -> Rc<dyn Fn(rufin_core::route::Route)> {
        let shell = Rc::downgrade(&self.shell);
        Rc::new(move |route| {
            if let Some(shell) = shell.upgrade() {
                shell.open_route(route);
            }
        })
    }

    pub fn update_library_list_settings(
        &self,
        key: rufin_core::settings::LibraryListKey,
        update: impl FnOnce(&mut rufin_core::settings::LibraryListSettings),
    ) {
        if self.settings.update_library_list_settings(key, update) {
            self.reconcile_mounted_route();
        }
    }

    pub fn library_settings_changed(&self) -> Rc<dyn Fn()> {
        let shell = Rc::downgrade(&self.shell);
        Rc::new(move || {
            if let Some(shell) = shell.upgrade() {
                shell.reconcile_mounted_route();
            }
        })
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
