use crate::track_selection::{PlaylistEntrySelection, TrackSelection};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use ui_shared::mounted_route::{RouteCurrentTrack, RouteCurrentTrackSelection};
use ui_shared::selection::{PlaylistEntrySelectionSnapshot, TrackSelectionSnapshot};

pub struct CatalogUi {
    pub library: std::sync::Arc<library::Database>,
    pub runtime: tokio::runtime::Handle,
    pub source: rufin_core::runtime::SourceHandle,
    pub queue: playback::QueueHandle,
    pub radio: playback::RadioHandle,
    pub settings: Rc<ui_shared::settings::SettingsState>,
    pub artwork: Rc<ui_shared::artwork::ArtworkState>,
    pub downloads: Rc<ui_shared::downloads::DownloadsState>,
    pub media_menus: Rc<ui_shared::media_menus::MediaMenus>,
    pub selected: Option<rufin_core::runtime::SelectedLibrary>,
    pub favorites: Option<Rc<ui_shared::favorites::FavoriteSessionState>>,
    pub window: gtk::glib::WeakRef<gtk::ApplicationWindow>,
    pub route_width: Rc<dyn Fn() -> i32>,
    pub is_current: Rc<dyn Fn() -> bool>,
    pub can_back: bool,
    pub go_back: Rc<dyn Fn()>,
    pub reconcile: Rc<dyn Fn()>,
    pub refresh: Rc<dyn Fn()>,
    pub refresh_catalog: Rc<dyn Fn()>,
    pub reserve_window_controls: Rc<dyn Fn(&gtk::Box, i32)>,
    pub present_full_artwork: Rc<dyn Fn(artwork::ArtworkBinding)>,
    pub present_selected_dialog: Rc<dyn Fn(&adw::Dialog)>,
    pub playlist_context_menu: Rc<
        dyn Fn(
            &gtk::Widget,
            library::PlaylistRow,
            Option<ui_shared::media_menus::CollectionPlay>,
            Option<(f64, f64)>,
        ),
    >,
    pub has_current_track: bool,
    pub update_smart_playlist_pin: Rc<dyn Fn(&library::SmartPlaylistRow)>,
    pub add_current_to_playlist: Rc<dyn Fn(library::PlaylistKey, String)>,
    pub refresh_home_section: Rc<dyn Fn(rufin_core::settings::HomeSectionKind)>,
    pub set_favorite_category: Rc<dyn Fn(ui_shared::route::CollectionCategory)>,
    pub cycle_favorite_category: Rc<dyn Fn()>,
    pub history_source_name: Option<String>,
    pub history_source_id: Rc<dyn Fn() -> Option<sources::SourceId>>,

    pub current: RefCell<Option<RouteCurrentTrack>>,
    pub current_track_selections: RefCell<Vec<RouteCurrentTrackSelection>>,
    pub track_selections: RefCell<Vec<TrackSelection>>,
    pub playlist_entry_selection: RefCell<Option<PlaylistEntrySelection>>,
}

impl CatalogUi {
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
        let current = self.current.borrow();
        self.current_track_selections
            .borrow_mut()
            .retain(|selection| selection(current.as_ref()));
    }
}

impl CatalogUi {
    pub fn selected_library(&self) -> Option<&rufin_core::runtime::SelectedLibrary> {
        self.selected.as_ref()
    }
    pub fn route_navigation(&self) -> Rc<dyn Fn(ui_shared::route::Route)> {
        Rc::clone(&self.media_menus.navigate)
    }
    pub fn navigate(&self, route: ui_shared::route::Route) {
        (self.media_menus.navigate)(route);
    }
    pub fn update_library_list_settings(
        &self,
        key: rufin_core::settings::LibraryListKey,
        update: impl FnOnce(&mut rufin_core::settings::LibraryListSettings),
    ) {
        if self.settings.update_library_list_settings(key, update) {
            (self.reconcile)();
        }
    }
    pub fn library_settings_changed(&self) -> Rc<dyn Fn()> {
        Rc::clone(&self.reconcile)
    }
    pub fn register_favorite_button(
        &self,
        key: ui_shared::favorites::FavoriteControlKey,
        button: &gtk::Button,
    ) {
        if let Some(favorites) = &self.favorites {
            favorites.register_favorite_button(key, button);
        }
    }
    pub fn register_dynamic_favorite_button(
        &self,
        key: Rc<dyn Fn() -> Option<ui_shared::favorites::FavoriteControlKey>>,
        button: &gtk::Button,
    ) {
        if let Some(favorites) = &self.favorites {
            favorites.register_dynamic_favorite_button(key, button);
        }
    }
    pub fn projected_track_favorite(&self, uri: &str, fallback: bool) -> bool {
        (self.media_menus.projected_item_favorite)(
            &library::FavoriteTarget::Track(uri.to_string()),
            fallback,
        )
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
    pub fn set_favorite_with_feedback(
        &self,
        target: library::FavoriteTarget,
        favorite: bool,
        button: Option<&gtk::Button>,
    ) {
        if let Some(button) = button {
            ui_shared::favorites::set_favorite_button_active(button, favorite);
        }
        (self.media_menus.set_favorite)(target, favorite);
    }
    pub fn reapply_current_track(&self) {
        let current = self.current.borrow();
        self.current_track_selections
            .borrow_mut()
            .retain(|selection| selection(current.as_ref()));
    }
}
