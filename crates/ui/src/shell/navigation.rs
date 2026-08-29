use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::downloads::{OperationFeedback, OperationFeedbackKind};
use crate::interactions::{
    CONTEXT_MENU_HOVER_HELD_CLASS, CONTEXT_MENU_HOVER_OWNER_CLASS, install_context_menu_openers,
    keep_parent_grab_for_nested_native_menus, popdown_native_menu, replace_native_menu_checkmarks,
    show_native_menu_icons,
};
use crate::localization::{bind_widget_accessible_label, bind_widget_tooltip};
use crate::preferences::source::selector::source_submenu;
use crate::routes::cards::cover_play_only_hover_controls;
use crate::routes::collection_context::{
    present_album_context_menu, present_artist_context_menu, present_genre_context_menu,
    present_playlist_context_menu, present_smart_playlist_context_menu,
};
use crate::routes::collections::PlaybackTarget;
use crate::routes::library_fields::{playlist_artwork, smart_playlist_display_name};
use crate::routes::playlist_picker::{PlaylistTrackSource, playlist_drag_source};
use crate::routes::route::{CollectionCategory, Route};
use crate::routes::track_selection::TrackSelectionSnapshot;
use crate::{SidebarPin, SidebarRouteItem, format_duration_units};
use adw::prelude::*;
use app_identity::DISPLAY_NAME;
use artwork::ArtworkBinding;
use gtk::{gio, glib};
use library::{AlbumRow, ArtistRow, GenreRow, PlaylistRow, SmartPlaylistRow};
use playback::QueuePlacement;
use tracing::warn;

use super::{
    Shell,
    actions::{PLAY_ICON, PLAY_LATER_ICON, PLAY_NEXT_ICON, icon_button},
    chrome,
    layout::{COMPACT_RAIL_WIDTH, ResolvedLeftSidebarMode},
    route::{RouteStack, route_current_track},
};
use localization::{msgid, tr};

const NORMAL_NAV_ICON_SIZE: i32 = 16;
const COMPACT_NAV_ICON_SIZE: i32 = 24;
const MOUSE_BACK_BUTTON: u32 = 8;
const MOUSE_FORWARD_BUTTON: u32 = 9;
const COMPACT_RAIL_LABEL_WIDTH: i32 = COMPACT_RAIL_WIDTH - 16;
const COMPACT_RAIL_LABEL_WIDTH_CHARS: i32 = 8;
const NAV_SELECTED_CLASS: &str = "selected";
const SIDEBAR_PINS_HEADING_CLASS: &str = "sidebar-pins-heading";
const SIDEBAR_SPACER_CLASS: &str = "sidebar-spacer";
const NAV_ROUTE_HOME_CLASS: &str = "nav-route-home";
const NAV_ROUTE_SEARCH_CLASS: &str = "nav-route-search";
const LEFT_OPENING_PRIMARY_MENU_CLASS: &str = "left-opening-primary-menu";
const NAV_ROUTE_FAVORITES_CLASS: &str = "nav-route-favorites";
const NAV_ROUTE_HISTORY_CLASS: &str = "nav-route-history";
const NAV_ROUTE_ALBUMS_CLASS: &str = "nav-route-albums";
const NAV_ROUTE_TRACKS_CLASS: &str = "nav-route-tracks";
const NAV_ROUTE_ARTISTS_CLASS: &str = "nav-route-artists";
const NAV_ROUTE_ALBUM_ARTISTS_CLASS: &str = "nav-route-album-artists";
const NAV_ROUTE_GENRES_CLASS: &str = "nav-route-genres";
const NAV_ROUTE_MOODS_CLASS: &str = "nav-route-moods";
const NAV_ROUTE_FOLDERS_CLASS: &str = "nav-route-folders";
const NAV_ROUTE_PLAYLISTS_CLASS: &str = "nav-route-playlists";
const NAV_ROUTE_SMART_PLAYLISTS_CLASS: &str = "nav-route-smart-playlists";
const SIDEBAR_PIN_ROW_CLASS: &str = "sidebar-pin-row";
const SIDEBAR_PIN_PLAYING_CLASS: &str = "playing";
const SIDEBAR_PIN_COVER_SIZE: i32 = 40;
const COMPACT_SIDEBAR_PIN_COVER_SIZE: i32 = 56;
const PRIMARY_MENU_CLASS: &str = "rufin-primary-menu";
const NAV_ROUTE_ICONS: [(&str, &str, Option<&str>); 13] = [
    (NAV_ROUTE_HOME_CLASS, "rufin-home-symbolic", None),
    (NAV_ROUTE_SEARCH_CLASS, "rufin-search-symbolic", None),
    (
        NAV_ROUTE_FAVORITES_CLASS,
        "rufin-heart-outline-symbolic",
        Some("rufin-heart-filled-symbolic"),
    ),
    (NAV_ROUTE_HISTORY_CLASS, "rufin-history-symbolic", None),
    (NAV_ROUTE_ALBUMS_CLASS, "rufin-albums-symbolic", None),
    (
        NAV_ROUTE_TRACKS_CLASS,
        "rufin-tracks-symbolic",
        Some("rufin-tracks-selected-symbolic"),
    ),
    (NAV_ROUTE_ARTISTS_CLASS, "rufin-artists-symbolic", None),
    (
        NAV_ROUTE_ALBUM_ARTISTS_CLASS,
        "rufin-album-artists-symbolic",
        None,
    ),
    (NAV_ROUTE_GENRES_CLASS, "rufin-genres-symbolic", None),
    (NAV_ROUTE_MOODS_CLASS, "rufin-moods-symbolic", None),
    (
        NAV_ROUTE_FOLDERS_CLASS,
        "rufin-folders-symbolic",
        Some("rufin-folders-selected-symbolic"),
    ),
    (NAV_ROUTE_PLAYLISTS_CLASS, "rufin-playlists-symbolic", None),
    (
        NAV_ROUTE_SMART_PLAYLISTS_CLASS,
        "rufin-smart-playlists-symbolic",
        None,
    ),
];

pub(crate) struct NavigationState {
    pub(crate) routes: RefCell<RouteStack>,
    pub(crate) favorite_category: Cell<CollectionCategory>,
    pin_generation: Cell<u64>,
    pin_identity: RefCell<Option<SidebarPinIdentity>>,
    pin_items: RefCell<Vec<SidebarPinItem>>,
    pin_cancellation: RefCell<Option<library::ReadCancellation>>,
}

impl NavigationState {
    pub(crate) fn new() -> Self {
        Self {
            routes: RefCell::new(RouteStack::new(Route::Home)),
            favorite_category: Cell::new(CollectionCategory::Tracks),
            pin_generation: Cell::new(0),
            pin_identity: RefCell::new(None),
            pin_items: RefCell::new(Vec::new()),
            pin_cancellation: RefCell::new(None),
        }
    }
}

pub(super) struct PrimaryMenuWidgets {
    pub(super) button: gtk::Button,
    pub(super) popover: RefCell<Option<gtk::PopoverMenu>>,
}

pub(super) struct NormalPrimaryMenuWidgets {
    pub(super) button: gtk::MenuButton,
    pub(super) popover: RefCell<Option<gtk::PopoverMenu>>,
}

pub(crate) struct NavigationWidgets {
    pub(super) split_view: adw::OverlaySplitView,
    pub(super) left_resize_handle: gtk::Box,
    pub(super) normal_nav_panel: gtk::Box,
    pub(super) compact_nav_slot: gtk::ScrolledWindow,
    pub(super) tiny_nav_button: gtk::Button,
    pub(super) normal_nav_routes: adw::Sidebar,
    pub(super) normal_nav_pins: gtk::Box,
    pub(super) compact_nav: gtk::Box,
    pub(super) normal_main_menu: NormalPrimaryMenuWidgets,
    pub(super) compact_main_menu: PrimaryMenuWidgets,
}

pub(super) fn build_normal_navigation(shell: &Rc<Shell>) {
    let section = adw::SidebarSection::new();
    for item in nav_items(shell) {
        let sidebar_item = adw::SidebarItem::new(&tr(item.label));
        sidebar_item.set_icon_name(Some(item.icon_name));
        section.append(sidebar_item);
    }
    shell.navigation_view.normal_nav_routes.append(section);
    append_sidebar_pins(shell);
}

pub(super) fn build_compact_navigation(shell: &Rc<Shell>) {
    let top_reservation = shell.chrome.window_controls.compact_start_reservation();
    let top_handle = chrome::window_drag_handle_with_child("sidebar-drag-handle", &top_reservation);
    shell.navigation_view.compact_nav.append(&top_handle);
    shell
        .navigation_view
        .compact_nav
        .append(&primary_menu_button(
            &shell.navigation_view.compact_main_menu.button,
            &shell.navigation_view.compact_main_menu.popover,
            shell,
            true,
        ));
    for item in nav_items(shell) {
        shell.navigation_view.compact_nav.append(&rail_button(
            shell,
            item.icon_name,
            item.label,
            item.route.clone(),
        ));
    }
    append_compact_sidebar_pins(shell);
    shell.navigation_view.compact_nav.append(&sidebar_spacer());
}

pub(super) fn rebuild_navigation(shell: &Rc<Shell>) {
    if !request_sidebar_pins(shell) {
        clear_box(&shell.navigation_view.normal_nav_pins);
        let mut child = shell.navigation_view.compact_nav.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if widget.has_css_class(SIDEBAR_PIN_ROW_CLASS) {
                shell.navigation_view.compact_nav.remove(&widget);
            }
        }
        return;
    }
    shell.navigation_view.normal_nav_routes.remove_all();
    clear_box(&shell.navigation_view.normal_nav_pins);
    clear_box(&shell.navigation_view.compact_nav);
    build_normal_navigation(shell);
    build_compact_navigation(shell);
    update_navigation_selection(shell.as_ref());
    update_sidebar_pin_playback(shell.as_ref());
}

impl Shell {
    pub(crate) fn rebuild_sidebar_navigation(self: &Rc<Self>) {
        rebuild_navigation(self);
        self.update_layout();
    }

    pub(crate) fn import_remote_playlist_pins_once(self: &Rc<Self>) {
        let Some(selected) = self.selected_library().as_deref().cloned() else {
            return;
        };
        let source_id = selected.artwork.source_id.clone();
        let remote = self
            .source
            .configured
            .borrow()
            .sources
            .iter()
            .any(|source| source.id == source_id && source.kind != "local");
        if !remote
            || self
                .settings
                .current
                .borrow()
                .sidebar
                .playlist_pin_imported_sources
                .contains(&source_id)
        {
            return;
        }
        let database = Arc::clone(&selected.database);
        let runtime = selected.runtime.clone();
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let cancellation = library::ReadCancellation::new();
        let task = runtime.spawn(async move {
            let order = database
                .playlist_order(
                    source,
                    folder,
                    library::PlaylistSort::Title,
                    false,
                    "",
                    &cancellation,
                )
                .await?;
            let mut object_ids = Vec::with_capacity(order.len());
            for keys in order.chunks(128) {
                object_ids.extend(
                    database
                        .playlist_rows(source, keys, folder, &cancellation)
                        .await?
                        .into_iter()
                        .map(|row| row.object_id),
                );
            }
            Ok::<_, library::LibraryError>(object_ids)
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else {
                return;
            };
            match task.await.ok().and_then(Result::ok) {
                Some(playlist_ids) => {
                    let changed = shell
                        .update_app_settings("remote playlist Pins import", |settings| {
                            settings
                                .sidebar
                                .import_playlist_pins_once(source_id.clone(), playlist_ids)
                        })
                        .is_some();
                    if changed {
                        shell.rebuild_sidebar_navigation();
                    }
                }
                None => warn!("failed to read remote playlists for Pins import"),
            }
        });
    }

    pub(crate) fn set_sidebar_pin(self: &Rc<Self>, pin: SidebarPin, pinned: bool) {
        if self
            .update_app_settings("Pins setting", |settings| {
                settings.sidebar.set_pinned(pin, pinned)
            })
            .is_some()
        {
            let _ = request_sidebar_pins(self);
        }
    }

    fn reorder_sidebar_pin(self: &Rc<Self>, moved: &SidebarPin, target: &SidebarPin) -> bool {
        let Some(after) = self
            .settings
            .current
            .borrow()
            .sidebar
            .pin_drop_after(moved, target)
        else {
            return false;
        };
        let changed = self
            .update_app_settings("Pins order", |settings| {
                settings.sidebar.reorder_pin(moved, target, after)
            })
            .is_some();
        if changed {
            reorder_current_sidebar_pin_projection(self, moved, target, after);
        }
        changed
    }
}

pub(super) fn update_navigation_selection(shell: &Shell) {
    let active_route = shell.navigation.routes.borrow().current().clone();
    let pin_is_selected =
        update_pin_selection(&shell.navigation_view.normal_nav_pins, &active_route);
    update_native_route_selection(shell, &active_route, pin_is_selected);
    update_button_navigation_selection(&shell.navigation_view.compact_nav, &active_route);
}

pub(crate) fn update_sidebar_pin_playback(shell: &Shell) {
    let selected_source = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.source_key);
    let current = route_current_track(shell.selected_playback().as_deref());
    let playback_context_id = current
        .as_ref()
        .zip(selected_source.as_ref())
        .filter(|(current, source)| &current.source_id == *source)
        .and_then(|(current, _)| current.context.as_ref())
        .map(|context| context.context_id.as_ref());

    let mut pin_rows = sidebar_pin_widgets(&shell.navigation_view.normal_nav_pins);
    pin_rows.extend(sidebar_pin_widgets(&shell.navigation_view.compact_nav));
    let playing_context_id = playback_context_id.and_then(|playback_context_id| {
        pin_rows
            .iter()
            .filter(|(_, pin_context_id)| {
                sidebar_pin_context_matches(pin_context_id, playback_context_id)
            })
            .max_by_key(|(_, pin_context_id)| {
                (
                    pin_context_id.as_str() == playback_context_id,
                    pin_context_id.len(),
                )
            })
            .map(|(_, pin_context_id)| pin_context_id.clone())
    });
    for (widget, pin_context_id) in pin_rows {
        if playing_context_id.as_ref() == Some(&pin_context_id) {
            widget.add_css_class(SIDEBAR_PIN_PLAYING_CLASS);
        } else {
            widget.remove_css_class(SIDEBAR_PIN_PLAYING_CLASS);
        }
    }
}

fn sidebar_pin_widgets(container: &gtk::Box) -> Vec<(gtk::Widget, String)> {
    let mut pin_rows = Vec::new();
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Some(pin_context_id) = widget
            .has_css_class(SIDEBAR_PIN_ROW_CLASS)
            .then(|| widget.widget_name())
            .and_then(|name| name.strip_prefix("sidebar-pin-").map(str::to_string))
        {
            pin_rows.push((widget, pin_context_id));
        }
    }
    pin_rows
}

fn sidebar_pin_context_matches(pin_context_id: &str, playback_context_id: &str) -> bool {
    playback_context_id == pin_context_id
        || if pin_context_id.starts_with("playlist:") {
            playback_context_id
                .strip_prefix(pin_context_id)
                .is_some_and(|suffix| suffix.starts_with(':'))
        } else {
            playback_context_id
                .strip_prefix(pin_context_id)
                .is_some_and(|suffix| suffix.starts_with('|'))
        }
}

pub(super) fn relocalize_primary_menu_button(
    button: &gtk::Button,
    popover_slot: &RefCell<Option<gtk::PopoverMenu>>,
    shell: &Rc<Shell>,
    compact: bool,
) {
    chrome::configure_primary_menu_button(button);
    button.set_child(Some(&sidebar_menu_content(compact)));
    let existing = popover_slot.borrow().clone();
    if let Some(popover) = existing.as_ref() {
        refresh_primary_menu(popover, shell);
    } else {
        update_primary_menu_popover(button, popover_slot, primary_menu_popover(shell), shell);
    }
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn sidebar_spacer() -> gtk::Box {
    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.add_css_class(SIDEBAR_SPACER_CLASS);
    spacer.set_vexpand(true);
    spacer
}

pub(super) fn normal_sidebar_header(
    shell: &Rc<Shell>,
    start_window_controls: &impl IsA<gtk::Widget>,
) -> adw::HeaderBar {
    let search = gtk::Button::from_icon_name("rufin-system-search-symbolic");
    bind_widget_tooltip(&search, msgid("Search"));
    let search_shell = Rc::clone(shell);
    search.connect_clicked(move |_| search_shell.navigate(Route::Search));

    let title = gtk::Label::new(Some(DISPLAY_NAME));
    title.add_css_class("heading");

    let menu = normal_primary_menu_button(
        &shell.navigation_view.normal_main_menu.button,
        &shell.navigation_view.normal_main_menu.popover,
        shell,
    );

    let header = adw::HeaderBar::new();
    header.add_css_class("flat");
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    header.pack_start(start_window_controls);
    header.pack_start(&search);
    header.set_title_widget(Some(&title));
    header.pack_end(&menu);
    header
}

fn update_pin_selection(container: &gtk::Box, active_route: &Route) -> bool {
    let active_pin_key = sidebar_pin_route_key(active_route);
    let mut selected = false;
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if !widget.has_css_class(SIDEBAR_PIN_ROW_CLASS) {
            continue;
        }
        let row_selected = active_pin_key
            .as_ref()
            .is_some_and(|key| widget.widget_name().as_str() == key);
        if row_selected {
            widget.add_css_class(NAV_SELECTED_CLASS);
            selected = true;
        } else {
            widget.remove_css_class(NAV_SELECTED_CLASS);
        }
    }
    selected
}

fn update_native_route_selection(shell: &Shell, active_route: &Route, pin_is_selected: bool) {
    let active_route_class = nav_route_class(active_route);
    let items = nav_items(shell);
    let selected_index = if pin_is_selected {
        None
    } else {
        items
            .iter()
            .position(|item| {
                active_route_class
                    .is_some_and(|active| nav_route_class(&item.route) == Some(active))
            })
            .map(|index| index as u32)
    };
    for (index, nav_item) in items.iter().enumerate() {
        if let Some(item) = shell.navigation_view.normal_nav_routes.item(index as u32) {
            let icon_name = nav_route_class(&nav_item.route)
                .and_then(|route_class| {
                    NAV_ROUTE_ICONS.iter().find_map(
                        |(class, normal_icon_name, selected_icon_name)| {
                            (*class == route_class).then_some(
                                if selected_index == Some(index as u32)
                                    && route_class != NAV_ROUTE_FAVORITES_CLASS
                                {
                                    selected_icon_name.unwrap_or(normal_icon_name)
                                } else {
                                    *normal_icon_name
                                },
                            )
                        },
                    )
                })
                .unwrap_or(nav_item.icon_name);
            item.set_icon_name(Some(icon_name));
        }
    }
    shell
        .navigation_view
        .normal_nav_routes
        .set_selected(selected_index.unwrap_or(gtk::INVALID_LIST_POSITION));
}

pub(super) fn install_normal_navigation_activation(shell: &Rc<Shell>) {
    let weak_shell = Rc::downgrade(shell);
    shell
        .navigation_view
        .normal_nav_routes
        .connect_activated(move |_, index| {
            let Some(shell) = weak_shell.upgrade() else {
                return;
            };
            if let Some(route) = sidebar_route_at_position(&shell, index as usize + 1) {
                if shell.navigation_view.split_view.is_collapsed() {
                    shell.navigation_view.split_view.set_show_sidebar(false);
                }
                shell.navigate(route);
            }
        });
}

fn update_button_navigation_selection(container: &gtk::Box, active_route: &Route) {
    let active_route_class = nav_route_class(active_route);
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if !widget.has_css_class("nav-button") {
            continue;
        }
        let selected =
            active_route_class.is_some_and(|route_class| widget.has_css_class(route_class));
        if selected {
            widget.add_css_class(NAV_SELECTED_CLASS);
        } else {
            widget.remove_css_class(NAV_SELECTED_CLASS);
        }
        if let (Some((normal_icon_name, selected_icon_name)), Some(icon)) =
            (nav_route_icon_names(&widget), nav_button_icon(&widget))
        {
            icon.set_icon_name(Some(if selected {
                selected_icon_name
            } else {
                normal_icon_name
            }));
        }
    }
}

fn nav_button_icon(widget: &gtk::Widget) -> Option<gtk::Image> {
    widget
        .first_child()
        .and_then(|child| child.downcast::<gtk::Box>().ok())
        .and_then(|content| content.first_child())
        .and_then(|child| child.downcast::<gtk::Image>().ok())
}

fn nav_route_icon_names(widget: &gtk::Widget) -> Option<(&'static str, &'static str)> {
    NAV_ROUTE_ICONS
        .into_iter()
        .find_map(|(route_class, normal_icon_name, selected_icon_name)| {
            widget.has_css_class(route_class).then_some((
                normal_icon_name,
                selected_icon_name.unwrap_or(normal_icon_name),
            ))
        })
}

fn primary_menu_button(
    button: &gtk::Button,
    popover_slot: &RefCell<Option<gtk::PopoverMenu>>,
    shell: &Rc<Shell>,
    compact: bool,
) -> gtk::Button {
    button.add_css_class("primary-menu-button");
    button.add_css_class("flat");
    if compact {
        button.add_css_class("nav-button");
        button.add_css_class("rail-button");
    }
    relocalize_primary_menu_button(button, popover_slot, shell, compact);
    button.clone()
}

fn update_primary_menu_popover(
    button: &gtk::Button,
    popover_slot: &RefCell<Option<gtk::PopoverMenu>>,
    popover: gtk::PopoverMenu,
    shell: &Rc<Shell>,
) {
    *popover_slot.borrow_mut() = Some(popover.clone());
    popover.set_parent(button);
    let row_popover = popover.clone();
    let row_shell = Rc::downgrade(shell);
    button.connect_clicked(move |_| {
        if let Some(shell) = row_shell.upgrade() {
            refresh_primary_menu(&row_popover, &shell);
            row_popover.popup();
        }
    });
    crate::interactions::popdown_on_anchor_unmap(button, &popover);
}

fn normal_primary_menu_button(
    button: &gtk::MenuButton,
    popover_slot: &RefCell<Option<gtk::PopoverMenu>>,
    shell: &Rc<Shell>,
) -> gtk::MenuButton {
    button.set_icon_name("rufin-open-menu-symbolic");
    bind_widget_tooltip(button, msgid("Menu"));
    bind_widget_accessible_label(button, msgid("Menu"));
    if popover_slot.borrow().is_none() {
        let popover = primary_menu_popover(shell);
        let row_shell = Rc::downgrade(shell);
        popover.connect_map(move |popover| {
            if let Some(shell) = row_shell.upgrade() {
                refresh_primary_menu(popover, &shell);
            }
        });
        button.set_popover(Some(&popover));
        popover_slot.replace(Some(popover));
    }
    button.clone()
}

pub(super) fn popup_primary_menu(shell: &Rc<Shell>) {
    match shell.left_sidebar_mode() {
        ResolvedLeftSidebarMode::Compact => popup_compact_primary_menu(shell),
        _ => popup_normal_primary_menu(shell),
    }
}

fn popup_compact_primary_menu(shell: &Rc<Shell>) {
    if let Some(popover) = shell
        .navigation_view
        .compact_main_menu
        .popover
        .borrow()
        .as_ref()
    {
        refresh_primary_menu(popover, shell);
        popover.popup();
    }
}

fn popup_normal_primary_menu(shell: &Rc<Shell>) {
    if shell
        .navigation_view
        .normal_main_menu
        .popover
        .borrow()
        .as_ref()
        .is_some()
    {
        shell.navigation_view.normal_main_menu.button.popup();
    }
}

pub(crate) fn popdown_primary_menu(shell: &Shell) {
    if shell
        .navigation_view
        .normal_main_menu
        .popover
        .borrow()
        .as_ref()
        .is_some_and(gtk::prelude::WidgetExt::is_visible)
    {
        shell.navigation_view.normal_main_menu.button.popdown();
    }
    if let Some(popover) = shell
        .navigation_view
        .compact_main_menu
        .popover
        .borrow()
        .as_ref()
        && popover.is_visible()
    {
        popdown_native_menu(popover);
    }
}

fn refresh_primary_menu(popover: &gtk::PopoverMenu, shell: &Rc<Shell>) {
    let menu = popover
        .menu_model()
        .and_then(|model| model.downcast::<gio::Menu>().ok())
        .expect("a primary menu popover keeps its mutable menu model");
    replace_primary_menu_model(&menu, shell);
    style_primary_menu(popover);
}

fn primary_menu_popover(shell: &Rc<Shell>) -> gtk::PopoverMenu {
    let menu = gio::Menu::new();
    replace_primary_menu_model(&menu, shell);
    let popover = gtk::PopoverMenu::from_model_full(&menu, gtk::PopoverMenuFlags::NESTED);
    popover.set_autohide(true);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_halign(gtk::Align::Start);
    style_primary_menu(&popover);
    popover
}

fn style_primary_menu(popover: &gtk::PopoverMenu) {
    popover.add_css_class(PRIMARY_MENU_CLASS);
    show_native_menu_icons(popover);
    replace_native_menu_checkmarks(popover);
    keep_parent_grab_for_nested_native_menus(popover);
    if popover.has_css_class(LEFT_OPENING_PRIMARY_MENU_CLASS) {
        mirror_primary_menu_cascade(popover);
    }
}

fn mirror_primary_menu_cascade(popover: &gtk::PopoverMenu) {
    popover.set_halign(gtk::Align::End);
    mirror_nested_primary_menus(popover.upcast_ref());
}

fn mirror_nested_primary_menus(widget: &gtk::Widget) {
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        if let Some(submenu) = current.downcast_ref::<gtk::PopoverMenu>() {
            submenu.add_css_class(LEFT_OPENING_PRIMARY_MENU_CLASS);
            submenu.set_position(gtk::PositionType::Left);
        }
        mirror_nested_primary_menus(&current);
    }
}

fn replace_primary_menu_model(menu: &gio::Menu, shell: &Rc<Shell>) {
    menu.remove_all();

    let source = gio::Menu::new();
    let (source_name, source_icon_name, source_menu) = source_submenu(shell);
    let source_item = gio::MenuItem::new_submenu(Some(&source_name), &source_menu);
    source_item.set_icon(&gio::ThemedIcon::new(source_icon_name));
    source.append_item(&source_item);
    menu.append_section(None, &source);

    let preferences = gio::Menu::new();
    append_menu_action(
        &preferences,
        &tr("Preferences"),
        "app.preferences",
        "rufin-preferences-system-symbolic",
    );
    append_menu_action(
        &preferences,
        &primary_menu_private_mode_label(shell.as_ref()),
        "win.toggle-private-mode",
        "rufin-system-lock-screen-symbolic",
    );
    menu.append_section(None, &preferences);

    let window = gio::Menu::new();
    append_menu_action(
        &window,
        &tr("Keyboard Shortcuts"),
        "app.show-shortcuts",
        "rufin-preferences-desktop-keyboard-shortcuts-symbolic",
    );
    append_menu_action(
        &window,
        &tr("Toggle Fullscreen"),
        "win.toggle-fullscreen",
        "rufin-view-fullscreen-symbolic",
    );
    append_menu_action(
        &window,
        &primary_menu_sidebar_toggle_label(shell.as_ref()),
        "win.toggle-left-sidebar",
        primary_menu_sidebar_toggle_icon(shell.as_ref()),
    );
    menu.append_section(None, &window);

    let information = gio::Menu::new();
    append_menu_action(
        &information,
        &tr("Version History"),
        "win.show-release-notes",
        "rufin-appointment-new-symbolic",
    );
    append_menu_action(
        &information,
        &tr("Troubleshooting"),
        "win.troubleshooting",
        "rufin-utilities-terminal-symbolic",
    );
    append_menu_action(
        &information,
        &tr("About Rufin"),
        "app.about",
        "rufin-help-about-symbolic",
    );
    menu.append_section(None, &information);
}

fn append_menu_action(menu: &gio::Menu, label: &str, action: &str, icon_name: &str) {
    let item = gio::MenuItem::new(Some(label), Some(action));
    item.set_icon(&gio::ThemedIcon::new(icon_name));
    menu.append_item(&item);
}

fn primary_menu_sidebar_toggle_label(shell: &Shell) -> String {
    if shell.left_sidebar_mode() == ResolvedLeftSidebarMode::Full {
        tr("Collapse sidebar")
    } else {
        tr("Expand sidebar")
    }
}

fn primary_menu_sidebar_toggle_icon(shell: &Shell) -> &'static str {
    if shell.left_sidebar_mode() == ResolvedLeftSidebarMode::Full {
        "rufin-sidebar-hide-symbolic"
    } else {
        "rufin-sidebar-show-symbolic"
    }
}

fn primary_menu_private_mode_label(shell: &Shell) -> String {
    if shell.settings.current.borrow().private_mode {
        tr("Turn off private mode")
    } else {
        tr("Turn on private mode")
    }
}

fn sidebar_menu_content(compact: bool) -> gtk::Box {
    let content = gtk::Box::new(
        if compact {
            gtk::Orientation::Vertical
        } else {
            gtk::Orientation::Horizontal
        },
        8,
    );
    content.set_halign(if compact {
        gtk::Align::Center
    } else {
        gtk::Align::Start
    });

    let icon_size = if compact {
        COMPACT_NAV_ICON_SIZE
    } else {
        NORMAL_NAV_ICON_SIZE
    };
    let icon = gtk::Image::from_icon_name("rufin-open-menu-symbolic");
    icon.add_css_class("nav-icon");
    icon.set_pixel_size(icon_size);
    icon.set_size_request(icon_size, icon_size);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    content.append(&icon);

    if compact {
        let text = gtk::Label::new(Some(&compact_sidebar_label_text("Menu")));
        configure_rail_label(&text);
        content.append(&text);
    }

    content
}

#[derive(Clone)]
struct NavItem {
    icon_name: &'static str,
    label: &'static str,
    route: Route,
}

#[derive(Clone)]
enum SidebarPinItem {
    Album(AlbumRow),
    Artist(ArtistRow, bool),
    Genre(GenreRow),
    Playlist(PlaylistRow),
    SmartPlaylist(SmartPlaylistRow),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SidebarPinIdentity {
    source: library::SourceKey,
    folder: Option<library::FolderKey>,
    pins: Vec<SidebarPin>,
}

impl SidebarPinItem {
    fn stored_pin(&self, source_id: &sources::SourceId) -> SidebarPin {
        match self {
            Self::Album(album) => SidebarPin::Album {
                source_id: source_id.clone(),
                album_id: album.object_id.clone(),
            },
            Self::Artist(artist, album_artist) => SidebarPin::Artist {
                source_id: source_id.clone(),
                artist_id: artist.object_id.clone(),
                album_artist: *album_artist,
            },
            Self::Genre(genre) => SidebarPin::Genre {
                source_id: source_id.clone(),
                genre_id: genre.object_id.clone(),
            },
            Self::Playlist(playlist) => SidebarPin::Playlist {
                source_id: source_id.clone(),
                playlist_id: playlist.object_id.clone(),
            },
            Self::SmartPlaylist(playlist) => SidebarPin::SmartPlaylist {
                source_id: source_id.clone(),
                playlist_id: playlist.object_id.clone(),
            },
        }
    }

    fn title(&self) -> String {
        match self {
            Self::Album(album) => album.title.clone(),
            Self::Artist(artist, _) => artist.name.clone(),
            Self::Genre(genre) => genre.name.clone(),
            Self::Playlist(playlist) => playlist.name.clone(),
            Self::SmartPlaylist(playlist) => smart_playlist_display_name(playlist),
        }
    }

    fn track_count(&self) -> u32 {
        match self {
            Self::Album(album) => album.track_count,
            Self::Artist(artist, _) => artist.track_count,
            Self::Genre(genre) => genre.track_count,
            Self::Playlist(playlist) => playlist.track_count,
            Self::SmartPlaylist(playlist) => playlist.track_count,
        }
        .max(0)
        .try_into()
        .unwrap_or(u32::MAX)
    }

    fn duration_seconds(&self) -> u32 {
        match self {
            Self::Album(album) => album.duration_millis,
            Self::Artist(artist, _) => artist.duration_millis,
            Self::Genre(genre) => genre.duration_millis,
            Self::Playlist(playlist) => playlist.duration_millis,
            Self::SmartPlaylist(playlist) => playlist.duration_millis,
        }
        .max(0)
        .saturating_div(1_000)
        .try_into()
        .unwrap_or(u32::MAX)
    }

    fn route(&self) -> Route {
        match self {
            Self::Album(album) => Route::AlbumDetail(album.album_key),
            Self::Artist(artist, album_artist) => {
                crate::routes::artist_detail_route(artist.artist_key, *album_artist)
            }
            Self::Genre(genre) => Route::GenreDetail(genre.genre_key),
            Self::Playlist(playlist) => Route::PlaylistDetail(playlist.playlist_key),
            Self::SmartPlaylist(playlist) => {
                Route::SmartPlaylistDetail(playlist.smart_playlist_key)
            }
        }
    }

    fn playback_target(&self) -> PlaybackTarget {
        match self {
            Self::Album(album) => PlaybackTarget::Album(album.album_key),
            Self::Artist(artist, album_artist) => {
                if *album_artist {
                    PlaybackTarget::AlbumArtist(artist.artist_key)
                } else {
                    PlaybackTarget::Artist(artist.artist_key)
                }
            }
            Self::Genre(genre) => PlaybackTarget::Genre(genre.genre_key),
            Self::Playlist(playlist) => PlaybackTarget::Playlist(playlist.playlist_key),
            Self::SmartPlaylist(playlist) => {
                PlaybackTarget::SmartPlaylist(playlist.smart_playlist_key)
            }
        }
    }

    fn artwork(&self, prefer_server_playlist_covers: bool) -> Vec<ArtworkBinding> {
        match self {
            Self::Album(album) => album
                .artwork_binding
                .iter()
                .map(|binding| ArtworkBinding::opaque(binding))
                .collect(),
            Self::Artist(artist, _) => artist
                .artwork_binding
                .iter()
                .map(|binding| ArtworkBinding::opaque(binding))
                .collect(),
            Self::Genre(genre) => genre
                .artwork_binding
                .as_ref()
                .or_else(|| genre.representative_artwork.first())
                .into_iter()
                .map(|binding| ArtworkBinding::opaque(binding))
                .collect(),
            Self::Playlist(playlist) => playlist_artwork(playlist, prefer_server_playlist_covers),
            Self::SmartPlaylist(playlist) => playlist
                .artwork_bindings
                .iter()
                .map(|binding| ArtworkBinding::opaque(binding))
                .collect(),
        }
    }

    fn present_context_menu(
        &self,
        target: &gtk::Widget,
        shell: &Rc<Shell>,
        position: Option<(f64, f64)>,
    ) {
        match self {
            Self::Album(album) => {
                present_album_context_menu(target, shell, album.clone(), None, None, position);
            }
            Self::Artist(artist, album_artist) => {
                present_artist_context_menu(
                    target,
                    shell,
                    artist.clone(),
                    *album_artist,
                    None,
                    position,
                );
            }
            Self::Genre(genre) => {
                present_genre_context_menu(target, shell, genre.clone(), None, position);
            }
            Self::Playlist(playlist) => {
                present_playlist_context_menu(target, shell, playlist.clone(), None, position);
            }
            Self::SmartPlaylist(playlist) => {
                present_smart_playlist_context_menu(
                    target,
                    shell,
                    playlist.clone(),
                    None,
                    position,
                );
            }
        }
    }
}

fn append_sidebar_pins(shell: &Rc<Shell>) {
    if !shell.settings.current.borrow().sidebar.pins_visible {
        return;
    }
    let pins = sidebar_pin_items(shell);
    if pins.is_empty() {
        return;
    }

    let heading = gtk::Label::new(Some(&tr("Pins")));
    heading.add_css_class(SIDEBAR_PINS_HEADING_CLASS);
    heading.set_xalign(0.0);
    shell.navigation_view.normal_nav_pins.append(&heading);

    let prefer_server_playlist_covers = shell
        .settings
        .current
        .borrow()
        .prefer_server_playlist_covers;
    for pin in pins {
        shell
            .navigation_view
            .normal_nav_pins
            .append(&sidebar_pin_row(shell, pin, prefer_server_playlist_covers));
    }
}

fn append_compact_sidebar_pins(shell: &Rc<Shell>) {
    let prefer_server_playlist_covers = shell
        .settings
        .current
        .borrow()
        .prefer_server_playlist_covers;
    for pin in sidebar_pin_items(shell) {
        shell
            .navigation_view
            .compact_nav
            .append(&compact_sidebar_pin(
                shell,
                pin,
                prefer_server_playlist_covers,
            ));
    }
}

pub(crate) fn request_sidebar_pins(shell: &Rc<Shell>) -> bool {
    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        shell.navigation.pin_items.borrow_mut().clear();
        shell.navigation.pin_identity.borrow_mut().take();
        return true;
    };
    let source_id = selected.artwork.source_id.clone();
    let pins = {
        let settings = shell.settings.current.borrow();
        if !settings.sidebar.pins_visible {
            Vec::new()
        } else {
            settings
                .sidebar
                .pins
                .iter()
                .filter(|pin| pin.source_id() == &source_id)
                .cloned()
                .collect()
        }
    };
    let identity = SidebarPinIdentity {
        source: selected.source_key,
        folder: selected.music_folder_key,
        pins: pins.clone(),
    };
    if shell.navigation.pin_identity.borrow().as_ref() == Some(&identity) {
        return shell.navigation.pin_cancellation.borrow().is_none();
    }
    let can_reconcile_current =
        shell
            .navigation
            .pin_identity
            .borrow()
            .as_ref()
            .is_some_and(|current| {
                current.source == identity.source && current.folder == identity.folder
            });
    if let Some(cancellation) = shell.navigation.pin_cancellation.borrow_mut().take() {
        cancellation.cancel();
    }
    let generation = shell.navigation.pin_generation.get().wrapping_add(1);
    shell.navigation.pin_generation.set(generation);
    shell
        .navigation
        .pin_identity
        .replace(Some(identity.clone()));
    if can_reconcile_current {
        let retained = {
            let mut items = shell.navigation.pin_items.borrow_mut();
            items.retain(|item| pins.contains(&item.stored_pin(&source_id)));
            items.clone()
        };
        reconcile_sidebar_pin_widgets(shell, &retained);
    }
    if pins.is_empty() {
        shell.navigation.pin_items.borrow_mut().clear();
        reconcile_sidebar_pin_widgets(shell, &[]);
        return true;
    }
    let cancellation = library::ReadCancellation::new();
    shell
        .navigation
        .pin_cancellation
        .replace(Some(cancellation.clone()));
    let database = Arc::clone(&selected.database);
    let source = selected.source_key;
    let folder = selected.music_folder_key;
    let runtime = selected.runtime.clone();
    let task = runtime.spawn(async move {
        let mut items = Vec::with_capacity(pins.len());
        for pin in pins {
            let item = match pin {
                SidebarPin::Album { album_id, .. } => {
                    let Some(key) = database
                        .album_key_by_object(source, &album_id, &cancellation)
                        .await?
                    else {
                        continue;
                    };
                    database
                        .album_rows(source, &[key], folder, &cancellation)
                        .await?
                        .pop()
                        .map(SidebarPinItem::Album)
                }
                SidebarPin::Artist {
                    artist_id,
                    album_artist,
                    ..
                } => {
                    let Some(key) = database
                        .artist_key_by_object(source, &artist_id, &cancellation)
                        .await?
                    else {
                        continue;
                    };
                    database
                        .artist_rows(source, &[key], album_artist, folder, &cancellation)
                        .await?
                        .pop()
                        .map(|artist| SidebarPinItem::Artist(artist, album_artist))
                }
                SidebarPin::Genre { genre_id, .. } => {
                    let Some(key) = database
                        .genre_key_by_object(source, &genre_id, &cancellation)
                        .await?
                    else {
                        continue;
                    };
                    database
                        .genre_rows(source, &[key], folder, &cancellation)
                        .await?
                        .pop()
                        .map(SidebarPinItem::Genre)
                }
                SidebarPin::Playlist { playlist_id, .. } => {
                    let Some(key) = database
                        .playlist_key_by_object(source, &playlist_id, &cancellation)
                        .await?
                    else {
                        continue;
                    };
                    database
                        .playlist_rows(source, &[key], folder, &cancellation)
                        .await?
                        .pop()
                        .map(SidebarPinItem::Playlist)
                }
                SidebarPin::SmartPlaylist { playlist_id, .. } => {
                    let Some(key) = database
                        .smart_playlist_key_by_object(source, &playlist_id, &cancellation)
                        .await?
                    else {
                        continue;
                    };
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_secs() as i64);
                    database
                        .smart_playlist_rows(source, &[key], folder, now, &cancellation)
                        .await?
                        .pop()
                        .map(SidebarPinItem::SmartPlaylist)
                }
            };
            if let Some(item) = item {
                items.push(item);
            }
        }
        Ok::<_, library::LibraryError>(items)
    });
    let shell = Rc::downgrade(shell);
    glib::spawn_future_local(async move {
        let Some(shell) = shell.upgrade() else {
            return;
        };
        let items = task.await.ok().and_then(Result::ok);
        if !sidebar_pin_publication_is_current(
            generation,
            shell.navigation.pin_generation.get(),
            &identity,
            shell.navigation.pin_identity.borrow().as_ref(),
        ) {
            return;
        }
        shell.navigation.pin_cancellation.borrow_mut().take();
        match items {
            Some(items) => {
                shell.navigation.pin_items.replace(items.clone());
                reconcile_sidebar_pin_widgets(&shell, &items);
            }
            None => {
                shell.navigation.pin_identity.borrow_mut().take();
                warn!("failed to load sidebar Pins");
            }
        }
    });
    false
}

fn sidebar_pin_publication_is_current(
    expected_generation: u64,
    current_generation: u64,
    expected_identity: &SidebarPinIdentity,
    current_identity: Option<&SidebarPinIdentity>,
) -> bool {
    expected_generation == current_generation && current_identity == Some(expected_identity)
}

pub(crate) fn refresh_sidebar_pins(shell: &Rc<Shell>) {
    shell.navigation.pin_identity.borrow_mut().take();
    let _ = request_sidebar_pins(shell);
}

fn sidebar_pin_items(shell: &Shell) -> Vec<SidebarPinItem> {
    shell.navigation.pin_items.borrow().clone()
}

fn reorder_current_sidebar_pin_projection(
    shell: &Rc<Shell>,
    moved: &SidebarPin,
    target: &SidebarPin,
    after: bool,
) {
    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        return;
    };
    let source_id = selected.artwork.source_id;
    let desired = shell
        .settings
        .current
        .borrow()
        .sidebar
        .pins
        .iter()
        .filter(|pin| pin.source_id() == &source_id)
        .cloned()
        .collect::<Vec<_>>();
    let (moved_key, target_key) = {
        let mut items = shell.navigation.pin_items.borrow_mut();
        let Some(moved_index) = items
            .iter()
            .position(|item| item.stored_pin(&source_id) == *moved)
        else {
            return;
        };
        let moved_item = items.remove(moved_index);
        let Some(target_index) = items
            .iter()
            .position(|item| item.stored_pin(&source_id) == *target)
        else {
            return;
        };
        let target_key = sidebar_pin_route_key(&items[target_index].route());
        let moved_key = sidebar_pin_route_key(&moved_item.route());
        items.insert(target_index + usize::from(after), moved_item);
        (moved_key, target_key)
    };
    if let Some(identity) = shell.navigation.pin_identity.borrow_mut().as_mut() {
        identity.pins = desired;
    }
    if let (Some(moved_key), Some(target_key)) = (moved_key, target_key) {
        reorder_sidebar_pin_widget(
            &shell.navigation_view.normal_nav_pins,
            &moved_key,
            &target_key,
            after,
        );
        reorder_sidebar_pin_widget(
            &shell.navigation_view.compact_nav,
            &moved_key,
            &target_key,
            after,
        );
    }
}

fn reorder_sidebar_pin_widget(
    container: &gtk::Box,
    moved_key: &str,
    target_key: &str,
    after: bool,
) {
    let widgets = keyed_sidebar_pin_widgets(container);
    let (Some(moved), Some(target)) = (widgets.get(moved_key), widgets.get(target_key)) else {
        return;
    };
    let previous = if after {
        Some(target.clone())
    } else {
        let mut previous = target.prev_sibling();
        if previous.as_ref() == Some(moved) {
            previous = moved.prev_sibling();
        }
        previous
    };
    container.reorder_child_after(moved, previous.as_ref());
}

fn reconcile_sidebar_pin_widgets(shell: &Rc<Shell>, items: &[SidebarPinItem]) {
    if items.is_empty() {
        clear_box(&shell.navigation_view.normal_nav_pins);
        remove_sidebar_pin_widgets(&shell.navigation_view.compact_nav);
        update_navigation_selection(shell.as_ref());
        update_sidebar_pin_playback(shell.as_ref());
        return;
    }
    let prefer_server_playlist_covers = shell
        .settings
        .current
        .borrow()
        .prefer_server_playlist_covers;
    let desired_keys = items
        .iter()
        .filter_map(|item| sidebar_pin_route_key(&item.route()))
        .collect::<Vec<_>>();

    let normal = &shell.navigation_view.normal_nav_pins;
    let heading = normal
        .first_child()
        .filter(|child| child.has_css_class(SIDEBAR_PINS_HEADING_CLASS))
        .unwrap_or_else(|| {
            let heading = gtk::Label::new(Some(&tr("Pins")));
            heading.add_css_class(SIDEBAR_PINS_HEADING_CLASS);
            heading.set_xalign(0.0);
            normal.prepend(&heading);
            heading.upcast()
        });
    let mut normal_widgets = keyed_sidebar_pin_widgets(normal);
    remove_obsolete_pin_widgets(normal, &normal_widgets, &desired_keys);
    let mut previous = Some(heading);
    for item in items {
        let Some(key) = sidebar_pin_route_key(&item.route()) else {
            continue;
        };
        let widget = normal_widgets.remove(&key).unwrap_or_else(|| {
            sidebar_pin_row(shell, item.clone(), prefer_server_playlist_covers).upcast()
        });
        update_sidebar_pin_widget_metadata(&widget, item);
        if widget.parent().is_some() {
            normal.reorder_child_after(&widget, previous.as_ref());
        } else {
            normal.insert_child_after(&widget, previous.as_ref());
        }
        previous = Some(widget);
    }

    let compact = &shell.navigation_view.compact_nav;
    let mut compact_widgets = keyed_sidebar_pin_widgets(compact);
    remove_obsolete_pin_widgets(compact, &compact_widgets, &desired_keys);
    let mut previous = compact_pin_anchor(compact);
    for item in items {
        let Some(key) = sidebar_pin_route_key(&item.route()) else {
            continue;
        };
        let widget = compact_widgets.remove(&key).unwrap_or_else(|| {
            compact_sidebar_pin(shell, item.clone(), prefer_server_playlist_covers).upcast()
        });
        if widget.parent().is_some() {
            compact.reorder_child_after(&widget, previous.as_ref());
        } else {
            compact.insert_child_after(&widget, previous.as_ref());
        }
        previous = Some(widget);
    }
    update_navigation_selection(shell.as_ref());
    update_sidebar_pin_playback(shell.as_ref());
}

fn keyed_sidebar_pin_widgets(container: &gtk::Box) -> HashMap<String, gtk::Widget> {
    let mut widgets = HashMap::new();
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.has_css_class(SIDEBAR_PIN_ROW_CLASS) {
            widgets.insert(widget.widget_name().to_string(), widget);
        }
    }
    widgets
}

fn remove_obsolete_pin_widgets(
    container: &gtk::Box,
    widgets: &HashMap<String, gtk::Widget>,
    desired_keys: &[String],
) {
    for (key, widget) in widgets {
        if !desired_keys.contains(key) {
            container.remove(widget);
        }
    }
}

fn remove_sidebar_pin_widgets(container: &gtk::Box) {
    for widget in keyed_sidebar_pin_widgets(container).into_values() {
        container.remove(&widget);
    }
}

fn update_sidebar_pin_widget_metadata(widget: &gtk::Widget, item: &SidebarPinItem) {
    for (class, text) in [
        ("sidebar-pin-title", item.title()),
        ("sidebar-pin-track-count", item.track_count().to_string()),
        (
            "sidebar-pin-duration",
            format_duration_units(item.duration_seconds()),
        ),
    ] {
        if let Some(label) = descendant_with_class(widget, class)
            .and_then(|widget| widget.downcast::<gtk::Label>().ok())
        {
            label.set_text(&text);
        }
    }
}

fn descendant_with_class(widget: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
    let mut child = widget.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.has_css_class(class) {
            return Some(widget);
        }
        if let Some(descendant) = descendant_with_class(&widget, class) {
            return Some(descendant);
        }
    }
    None
}

fn compact_pin_anchor(container: &gtk::Box) -> Option<gtk::Widget> {
    let mut anchor = None;
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.has_css_class(SIDEBAR_PIN_ROW_CLASS) || widget.has_css_class(SIDEBAR_SPACER_CLASS)
        {
            break;
        }
        anchor = Some(widget);
    }
    anchor
}

fn sidebar_pin_row(
    shell: &Rc<Shell>,
    pin: SidebarPinItem,
    prefer_server_playlist_covers: bool,
) -> gtk::Overlay {
    let route = pin.route();
    let title = pin.title();
    let track_count = pin.track_count();
    let duration_seconds = pin.duration_seconds();
    let artwork = pin.artwork(prefer_server_playlist_covers);
    let row = gtk::Overlay::new();
    row.add_css_class("nav-button");
    row.add_css_class(SIDEBAR_PIN_ROW_CLASS);
    row.add_css_class(CONTEXT_MENU_HOVER_OWNER_CLASS);
    row.set_overflow(gtk::Overflow::Hidden);
    row.set_widget_name(
        sidebar_pin_route_key(&route)
            .as_deref()
            .expect("a sidebar pin always has a detail route"),
    );
    row.set_hexpand(true);
    row.set_width_request(1);

    let activate = gtk::Button::new();
    activate.add_css_class("flat");
    activate.add_css_class("sidebar-pin-activate");
    activate.set_hexpand(true);
    activate.set_width_request(1);
    activate.update_property(&[gtk::accessible::Property::Label(&title)]);

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.set_hexpand(true);
    content.set_width_request(1);
    let cover = shell
        .cover_group_projection_for_artwork(
            &artwork,
            SIDEBAR_PIN_COVER_SIZE,
            SIDEBAR_PIN_COVER_SIZE,
        )
        .widget();
    cover.set_can_target(false);
    content.append(&cover);

    let identity = gtk::Box::new(gtk::Orientation::Vertical, 1);
    identity.set_hexpand(true);
    identity.set_width_request(1);
    identity.set_valign(gtk::Align::Center);
    let title_label = gtk::Label::new(Some(&title));
    title_label.add_css_class("sidebar-pin-title");
    title_label.set_xalign(0.0);
    title_label.set_hexpand(true);
    title_label.set_width_chars(1);
    title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    identity.append(&title_label);
    identity.append(&sidebar_pin_metadata(track_count, duration_seconds));
    content.append(&identity);
    activate.set_child(Some(&content));

    let navigation_shell = Rc::clone(shell);
    activate.connect_clicked(move |_| navigation_shell.navigate(route.clone()));
    install_sidebar_pin_double_click(&activate, shell, pin.playback_target());
    row.set_child(Some(&activate));
    install_sidebar_pin_reorder(&row, shell, &pin);
    install_playlist_pin_drop(&row, shell, &pin);

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 1);
    controls.add_css_class("sidebar-pin-controls");
    controls.set_halign(gtk::Align::End);
    controls.set_valign(gtk::Align::Center);
    controls.set_margin_end(2);
    controls.set_visible(false);
    let playback_target = pin.playback_target();
    for (icon_name, label, placement) in [
        (PLAY_ICON, "Play", QueuePlacement::Now),
        (PLAY_NEXT_ICON, "Play Next", QueuePlacement::Next),
        (PLAY_LATER_ICON, "Play Later", QueuePlacement::Last),
    ] {
        controls.append(&sidebar_pin_transport_button(
            shell,
            &playback_target,
            icon_name,
            label,
            placement,
        ));
    }
    row.add_overlay(&controls);
    row.set_measure_overlay(&controls, false);

    let motion = gtk::EventControllerMotion::new();
    let controls_for_enter = controls.clone();
    motion.connect_enter(move |_, _, _| controls_for_enter.set_visible(true));
    let controls_for_leave = controls.clone();
    let row_for_leave = row.clone();
    motion.connect_leave(move |_| {
        if !row_for_leave.has_css_class(CONTEXT_MENU_HOVER_HELD_CLASS) {
            controls_for_leave.set_visible(false);
        }
    });
    let motion_for_hold = motion.clone();
    let controls_for_hold = controls.clone();
    row.connect_css_classes_notify(move |row| {
        if !row.has_css_class(CONTEXT_MENU_HOVER_HELD_CLASS) {
            controls_for_hold.set_visible(motion_for_hold.contains_pointer());
        }
    });
    row.add_controller(motion);

    let context_shell = Rc::clone(shell);
    let context_pin = pin.clone();
    install_context_menu_openers(
        &row,
        Rc::new(move |target, position| {
            context_pin.present_context_menu(target, &context_shell, position);
        }),
    );
    row
}

fn compact_sidebar_pin(
    shell: &Rc<Shell>,
    pin: SidebarPinItem,
    prefer_server_playlist_covers: bool,
) -> gtk::Overlay {
    let route = pin.route();
    let title = pin.title();
    let artwork = pin.artwork(prefer_server_playlist_covers);
    let route_key =
        sidebar_pin_route_key(&route).expect("a compact sidebar pin always has a detail route");
    let row = gtk::Overlay::new();
    row.add_css_class("nav-button");
    row.add_css_class(SIDEBAR_PIN_ROW_CLASS);
    row.add_css_class("compact-sidebar-pin");
    row.set_widget_name(&route_key);
    row.set_halign(gtk::Align::Center);
    row.set_overflow(gtk::Overflow::Hidden);
    row.set_tooltip_text(Some(&title));

    let activate = gtk::Button::new();
    activate.add_css_class("flat");
    activate.add_css_class("compact-sidebar-pin-activate");
    activate.set_tooltip_text(Some(&title));
    activate.update_property(&[gtk::accessible::Property::Label(&title)]);
    let cover = shell
        .cover_group_projection_for_artwork(
            &artwork,
            COMPACT_SIDEBAR_PIN_COVER_SIZE,
            COMPACT_SIDEBAR_PIN_COVER_SIZE,
        )
        .widget();
    cover.set_can_target(false);
    activate.set_child(Some(&cover));
    let navigation_shell = Rc::clone(shell);
    activate.connect_clicked(move |_| navigation_shell.navigate(route.clone()));
    install_sidebar_pin_double_click(&activate, shell, pin.playback_target());
    row.set_child(Some(&activate));
    install_sidebar_pin_reorder(&row, shell, &pin);
    install_playlist_pin_drop(&row, shell, &pin);

    let (controls, play) = cover_play_only_hover_controls(COMPACT_SIDEBAR_PIN_COVER_SIZE, "Play");
    play.add_css_class("compact-sidebar-pin-play");
    play.set_size_request(32, 32);
    play.set_tooltip_text(Some(&title));
    let playback_shell = Rc::clone(shell);
    let playback_target = pin.playback_target();
    play.connect_clicked(move |_| {
        playback_target.play(&playback_shell, QueuePlacement::Now, true);
    });
    controls.add_to_overlay(&row);
    controls.connect_hover(&row);

    let context_shell = Rc::clone(shell);
    install_context_menu_openers(
        &row,
        Rc::new(move |target, position| {
            pin.present_context_menu(target, &context_shell, position);
        }),
    );
    row
}

fn install_sidebar_pin_reorder(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    pin: &SidebarPinItem,
) {
    let Some(source_id) = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.artwork.source_id.clone())
    else {
        return;
    };
    let pin = pin.stored_pin(&source_id);
    let payload = sidebar_pin_drag_variant(&pin);
    let drag = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .build();
    drag.connect_prepare(move |_, _, _| {
        Some(gtk::gdk::ContentProvider::for_value(&payload.to_value()))
    });
    let drag_target = target.as_ref().downgrade();
    drag.connect_drag_begin(move |source, _| {
        let Some(target) = drag_target.upgrade() else {
            return;
        };
        source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&target))), 0, 0);
    });
    target.add_controller(drag);

    let drop = gtk::DropTarget::new(glib::Variant::static_type(), gtk::gdk::DragAction::MOVE);
    let weak_target = target.as_ref().downgrade();
    let enter_target = weak_target.clone();
    drop.connect_enter(move |_, _, _| {
        if let Some(target) = enter_target.upgrade() {
            target.add_css_class("sidebar-pin-reorder-target");
        }
        gtk::gdk::DragAction::MOVE
    });
    let leave_target = weak_target.clone();
    drop.connect_leave(move |_| {
        if let Some(target) = leave_target.upgrade() {
            target.remove_css_class("sidebar-pin-reorder-target");
        }
    });
    let shell = Rc::downgrade(shell);
    drop.connect_drop(move |_, value, _, _| {
        let Some(target) = weak_target.upgrade() else {
            return false;
        };
        target.remove_css_class("sidebar-pin-reorder-target");
        let Some(moved) = sidebar_pin_from_drag_value(value) else {
            return false;
        };
        let Some(shell) = shell.upgrade() else {
            return false;
        };
        shell.reorder_sidebar_pin(&moved, &pin)
    });
    target.add_controller(drop);
}

fn sidebar_pin_drag_variant(pin: &SidebarPin) -> glib::Variant {
    let (kind, source_id, object_id, album_artist) = match pin {
        SidebarPin::Album {
            source_id,
            album_id,
        } => (0_u8, source_id, album_id, false),
        SidebarPin::Artist {
            source_id,
            artist_id,
            album_artist,
        } => (1, source_id, artist_id, *album_artist),
        SidebarPin::Genre {
            source_id,
            genre_id,
        } => (2, source_id, genre_id, false),
        SidebarPin::Playlist {
            source_id,
            playlist_id,
        } => (3, source_id, playlist_id, false),
        SidebarPin::SmartPlaylist {
            source_id,
            playlist_id,
        } => (4, source_id, playlist_id, false),
    };
    (kind, source_id.to_string(), object_id.clone(), album_artist).to_variant()
}

fn sidebar_pin_from_drag_value(value: &glib::Value) -> Option<SidebarPin> {
    let variant = value.get::<glib::Variant>().ok()?;
    let (kind, source_id, object_id, album_artist) = variant.get::<(u8, String, String, bool)>()?;
    let source_id = sources::SourceId::new(source_id);
    match kind {
        0 => Some(SidebarPin::Album {
            source_id,
            album_id: object_id,
        }),
        1 => Some(SidebarPin::Artist {
            source_id,
            artist_id: object_id,
            album_artist,
        }),
        2 => Some(SidebarPin::Genre {
            source_id,
            genre_id: object_id,
        }),
        3 => Some(SidebarPin::Playlist {
            source_id,
            playlist_id: object_id,
        }),
        4 => Some(SidebarPin::SmartPlaylist {
            source_id,
            playlist_id: object_id,
        }),
        _ => None,
    }
}

fn install_playlist_pin_drop(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    pin: &SidebarPinItem,
) {
    let SidebarPinItem::Playlist(playlist) = pin else {
        return;
    };
    let playlist = playlist.clone();
    let weak_target = target.as_ref().downgrade();
    let drop_target = gtk::DropTarget::new(
        glib::BoxedAnyObject::static_type(),
        gtk::gdk::DragAction::COPY,
    );
    let enter_target = weak_target.clone();
    drop_target.connect_enter(move |_, _, _| {
        if let Some(target) = enter_target.upgrade() {
            target.add_css_class("playlist-pin-drop-target");
        }
        gtk::gdk::DragAction::COPY
    });
    let leave_target = weak_target.clone();
    drop_target.connect_leave(move |_| {
        if let Some(target) = leave_target.upgrade() {
            target.remove_css_class("playlist-pin-drop-target");
        }
    });
    let shell = Rc::downgrade(shell);
    drop_target.connect_drop(move |_, value, _, _| {
        if let Some(target) = weak_target.upgrade() {
            target.remove_css_class("playlist-pin-drop-target");
        }
        let Some(shell) = shell.upgrade() else {
            return false;
        };
        let Some(source) = playlist_drag_source(value) else {
            return false;
        };
        add_source_to_playlist_pin(&shell, &playlist, source)
    });
    target.add_controller(drop_target);
}

fn add_source_to_playlist_pin(
    shell: &Rc<Shell>,
    playlist: &PlaylistRow,
    source: PlaylistTrackSource,
) -> bool {
    if let Some(selection) = source.ready_selection() {
        return add_selection_to_playlist_pin(shell, playlist, selection);
    }
    if !source.is_current(shell) {
        return false;
    }
    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        return false;
    };
    let runtime = selected.runtime.clone();
    let resolve_source = source.clone();
    let task = runtime.spawn(async move { resolve_source.resolve(&selected).await });
    let shell = Rc::downgrade(shell);
    let playlist = playlist.clone();
    gtk::glib::spawn_future_local(async move {
        let Some(shell) = shell.upgrade() else {
            return;
        };
        let Some((tracks, subject)) = task.await.ok().and_then(Result::ok) else {
            return;
        };
        if source.is_current(&shell) {
            add_tracks_to_playlist_pin(&shell, &playlist, tracks, subject);
        }
    });
    true
}

fn add_selection_to_playlist_pin(
    shell: &Rc<Shell>,
    playlist: &PlaylistRow,
    selection: TrackSelectionSnapshot,
) -> bool {
    if !selection.is_current(shell) {
        return false;
    }
    add_tracks_to_playlist_pin(
        shell,
        playlist,
        selection.tracks.to_vec(),
        selection.download_subject(),
    )
}

fn add_tracks_to_playlist_pin(
    shell: &Rc<Shell>,
    playlist: &PlaylistRow,
    tracks: Vec<library::TrackKey>,
    subject: ::downloads::DownloadSubject,
) -> bool {
    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        return false;
    };
    let skip_duplicates = !selected.playlist_tracks_can_repeat;
    let Some(operations) = shell.selected_source_operations() else {
        return false;
    };
    let scheduled = operations.add_playlist_tracks(playlist.playlist_key, tracks, skip_duplicates);
    if scheduled == 0 {
        return false;
    }
    shell.show_operation_feedback(&OperationFeedback {
        subject,
        item_count: scheduled,
        kind: OperationFeedbackKind::PlaylistAdded {
            destination: playlist.name.clone(),
        },
    });
    true
}

fn install_sidebar_pin_double_click(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    playback: PlaybackTarget,
) {
    let click = gtk::GestureClick::new();
    click.set_button(1);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let shell = Rc::clone(shell);
    click.connect_pressed(move |gesture, presses, _, _| {
        if presses >= 2 && presses % 2 == 0 {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            playback.play(&shell, QueuePlacement::Now, true);
        }
    });
    target.add_controller(click);
}

fn sidebar_pin_metadata(track_count: u32, duration_seconds: u32) -> gtk::Box {
    let metadata = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    metadata.add_css_class("sidebar-pin-metadata");
    metadata.set_hexpand(true);
    metadata.set_width_request(1);
    metadata.set_valign(gtk::Align::Center);
    metadata.set_overflow(gtk::Overflow::Hidden);

    let track_metadata = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    track_metadata.add_css_class("sidebar-pin-metadata-item");
    track_metadata.set_valign(gtk::Align::Center);

    let tracks_icon = gtk::Image::from_icon_name("rufin-tracks-symbolic");
    tracks_icon.add_css_class("sidebar-pin-metadata-icon");
    tracks_icon.set_pixel_size(12);
    tracks_icon.set_size_request(12, 12);
    tracks_icon.set_valign(gtk::Align::Center);
    track_metadata.append(&tracks_icon);
    let tracks = gtk::Label::new(Some(&track_count.to_string()));
    tracks.add_css_class("sidebar-pin-metadata-label");
    tracks.add_css_class("sidebar-pin-track-count");
    tracks.set_valign(gtk::Align::Center);
    tracks.set_yalign(0.5);
    track_metadata.append(&tracks);
    metadata.append(&track_metadata);

    let duration_metadata = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    duration_metadata.add_css_class("sidebar-pin-metadata-item");
    duration_metadata.set_hexpand(true);
    duration_metadata.set_width_request(1);
    duration_metadata.set_valign(gtk::Align::Center);
    duration_metadata.set_overflow(gtk::Overflow::Hidden);

    let duration_icon = gtk::Image::from_icon_name("rufin-preferences-system-time-symbolic");
    duration_icon.add_css_class("sidebar-pin-metadata-icon");
    duration_icon.set_pixel_size(12);
    duration_icon.set_size_request(12, 12);
    duration_icon.set_valign(gtk::Align::Center);
    duration_metadata.append(&duration_icon);
    let duration = gtk::Label::new(Some(&format_duration_units(duration_seconds)));
    duration.add_css_class("sidebar-pin-metadata-label");
    duration.add_css_class("sidebar-pin-duration");
    duration.set_hexpand(true);
    duration.set_width_request(1);
    duration.set_xalign(0.0);
    duration.set_valign(gtk::Align::Center);
    duration.set_yalign(0.5);
    duration.set_ellipsize(gtk::pango::EllipsizeMode::End);
    duration_metadata.append(&duration);
    metadata.append(&duration_metadata);
    metadata
}

fn sidebar_pin_transport_button(
    shell: &Rc<Shell>,
    target: &PlaybackTarget,
    icon_name: &str,
    label: &str,
    placement: QueuePlacement,
) -> gtk::Button {
    let button = icon_button(icon_name, label);
    button.add_css_class("sidebar-pin-control");
    let shell = Rc::clone(shell);
    let target = target.clone();
    button.connect_clicked(move |_| {
        target.play(&shell, placement, true);
    });
    button
}

fn nav_items(shell: &Shell) -> Vec<NavItem> {
    shell
        .settings
        .current
        .borrow()
        .sidebar
        .route_items
        .iter()
        .filter(|entry| entry.visible)
        .map(|entry| nav_item(entry.item))
        .collect()
}

fn sidebar_pin_route_key(route: &Route) -> Option<String> {
    match route {
        Route::AlbumDetail(id) => Some(format!("sidebar-pin-album:{id}")),
        Route::ArtistDetail(id) => Some(format!("sidebar-pin-artist:{id}")),
        Route::AlbumArtistDetail(id) => Some(format!("sidebar-pin-album-artist:{id}")),
        Route::GenreDetail(id) => Some(format!("sidebar-pin-genre:{id}")),
        Route::PlaylistDetail(id) => Some(format!("sidebar-pin-playlist:{id}")),
        Route::SmartPlaylistDetail(id) => Some(format!("sidebar-pin-smart-playlist:{id}")),
        _ => None,
    }
}

pub(crate) fn sidebar_route_at_position(shell: &Shell, position: usize) -> Option<Route> {
    sidebar_route_at_position_in(
        &shell.settings.current.borrow().sidebar.route_items,
        position,
    )
}

fn sidebar_route_at_position_in(
    route_items: &[crate::SidebarRouteItemSettings],
    position: usize,
) -> Option<Route> {
    let item = route_items
        .iter()
        .filter(|entry| entry.visible)
        .nth(position.checked_sub(1)?)?;
    Some(nav_item(item.item).route)
}

fn nav_item(item: SidebarRouteItem) -> NavItem {
    match item {
        SidebarRouteItem::Home => NavItem {
            icon_name: "rufin-home-symbolic",
            label: msgid("Home"),
            route: Route::Home,
        },
        SidebarRouteItem::Search => NavItem {
            icon_name: "rufin-search-symbolic",
            label: msgid("Search"),
            route: Route::Search,
        },
        SidebarRouteItem::Favorites => NavItem {
            icon_name: "rufin-heart-outline-symbolic",
            label: msgid("Favorites"),
            route: Route::Favorites,
        },
        SidebarRouteItem::History => NavItem {
            icon_name: "rufin-history-symbolic",
            label: msgid("History"),
            route: Route::History,
        },
        SidebarRouteItem::Albums => NavItem {
            icon_name: "rufin-albums-symbolic",
            label: msgid("Albums"),
            route: Route::Albums,
        },
        SidebarRouteItem::Tracks => NavItem {
            icon_name: "rufin-tracks-symbolic",
            label: msgid("Tracks"),
            route: Route::Tracks,
        },
        SidebarRouteItem::Artists => NavItem {
            icon_name: "rufin-artists-symbolic",
            label: msgid("Artists"),
            route: Route::Artists,
        },
        SidebarRouteItem::AlbumArtists => NavItem {
            icon_name: "rufin-album-artists-symbolic",
            label: msgid("Album Artists"),
            route: Route::AlbumArtists,
        },
        SidebarRouteItem::Genres => NavItem {
            icon_name: "rufin-genres-symbolic",
            label: msgid("Genres"),
            route: Route::Genres,
        },
        SidebarRouteItem::Moods => NavItem {
            icon_name: "rufin-moods-symbolic",
            label: msgid("Moods"),
            route: Route::Moods,
        },
        SidebarRouteItem::Folders => NavItem {
            icon_name: "rufin-folders-symbolic",
            label: msgid("Folders"),
            route: Route::Folders { path: Vec::new() },
        },
        SidebarRouteItem::Playlists => NavItem {
            icon_name: "rufin-playlists-symbolic",
            label: msgid("Playlists"),
            route: Route::Playlists,
        },
        SidebarRouteItem::SmartPlaylists => NavItem {
            icon_name: "rufin-smart-playlists-symbolic",
            label: msgid("Smart Playlists"),
            route: Route::SmartPlaylists,
        },
    }
}

fn rail_button(shell: &Rc<Shell>, icon_name: &str, label: &str, route: Route) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("nav-button");
    button.add_css_class("flat");
    if let Some(route_class) = nav_route_class(&route) {
        button.add_css_class(route_class);
    }
    button.add_css_class("rail-button");
    let accessible_label = tr(label);
    button.update_property(&[gtk::accessible::Property::Label(&accessible_label)]);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_halign(gtk::Align::Center);
    let icon_size = COMPACT_NAV_ICON_SIZE;
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.add_css_class("nav-icon");
    icon.set_pixel_size(icon_size);
    icon.set_size_request(icon_size, icon_size);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    content.append(&icon);
    let text = gtk::Label::new(Some(&compact_sidebar_label_text(label)));
    configure_rail_label(&text);
    content.append(&text);
    button.set_child(Some(&content));

    let shell = Rc::clone(shell);
    button.connect_clicked(move |_| {
        if shell.navigation_view.split_view.is_collapsed() {
            shell.navigation_view.split_view.set_show_sidebar(false);
        }
        shell.navigate(route.clone());
    });
    button
}

fn compact_sidebar_label_text(label: &str) -> String {
    let translated = tr(label);
    let compact = {
        let words = translated.split_whitespace().collect::<Vec<_>>();
        if words.len() == 2 {
            Some(format!("{}\n{}", words[0], words[1]))
        } else {
            None
        }
    };
    compact.unwrap_or(translated)
}

fn configure_rail_label(label: &gtk::Label) {
    configure_sidebar_entry_label(label);
    label.set_xalign(0.5);
    label.set_justify(gtk::Justification::Center);
    label.set_lines(2);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_width_request(COMPACT_RAIL_LABEL_WIDTH);
    label.set_size_request(COMPACT_RAIL_LABEL_WIDTH, -1);
    label.set_width_chars(1);
    label.set_max_width_chars(COMPACT_RAIL_LABEL_WIDTH_CHARS);
}

fn configure_sidebar_entry_label(label: &gtk::Label) {
    label.add_css_class("sidebar-entry-label");
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
}

fn nav_route_class(route: &Route) -> Option<&'static str> {
    match route {
        Route::Home => Some(NAV_ROUTE_HOME_CLASS),
        Route::Search => Some(NAV_ROUTE_SEARCH_CLASS),
        Route::Favorites => Some(NAV_ROUTE_FAVORITES_CLASS),
        Route::History => Some(NAV_ROUTE_HISTORY_CLASS),
        Route::Albums | Route::AlbumDetail(_) => Some(NAV_ROUTE_ALBUMS_CLASS),
        Route::Tracks => Some(NAV_ROUTE_TRACKS_CLASS),
        Route::Artists
        | Route::ArtistDetail(_)
        | Route::ArtistDiscography(_)
        | Route::ArtistTracks(_)
        | Route::ArtistFavoriteTracks(_) => Some(NAV_ROUTE_ARTISTS_CLASS),
        Route::AlbumArtists
        | Route::AlbumArtistDetail(_)
        | Route::AlbumArtistDiscography(_)
        | Route::AlbumArtistTracks(_)
        | Route::AlbumArtistFavoriteTracks(_) => Some(NAV_ROUTE_ALBUM_ARTISTS_CLASS),
        Route::Genres | Route::GenreDetail(_) => Some(NAV_ROUTE_GENRES_CLASS),
        Route::Moods | Route::MoodDetail(_) => Some(NAV_ROUTE_MOODS_CLASS),
        Route::Folders { .. } => Some(NAV_ROUTE_FOLDERS_CLASS),
        Route::Playlists | Route::PlaylistDetail(_) => Some(NAV_ROUTE_PLAYLISTS_CLASS),
        Route::SmartPlaylists | Route::SmartPlaylistDetail(_) => {
            Some(NAV_ROUTE_SMART_PLAYLISTS_CLASS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_position_uses_visible_configured_route_order() {
        let mut items = crate::settings::SidebarRouteItem::all()
            .into_iter()
            .map(|item| crate::SidebarRouteItemSettings {
                item,
                visible: true,
            })
            .collect::<Vec<_>>();
        items[1].visible = false;
        assert_eq!(sidebar_route_at_position_in(&items, 1), Some(Route::Home));
        assert_eq!(
            sidebar_route_at_position_in(&items, 2),
            Some(Route::Favorites)
        );
    }

    #[test]
    fn navigation_playlist_classes_keep_smart_playlists_separate() {
        assert_eq!(
            nav_route_class(&Route::PlaylistDetail(library::PlaylistKey::from_raw(1))),
            Some(NAV_ROUTE_PLAYLISTS_CLASS)
        );
        assert_eq!(
            nav_route_class(&Route::SmartPlaylistDetail(
                library::SmartPlaylistKey::from_raw(1)
            )),
            Some(NAV_ROUTE_SMART_PLAYLISTS_CLASS)
        );
    }

    #[test]
    fn sidebar_pin_playback_only_matches_its_page_context() {
        assert!(sidebar_pin_context_matches("album:1", "album:1|query=rock"));
        assert!(sidebar_pin_context_matches(
            "playlist:1",
            "playlist:1:Title:false:"
        ));
        assert!(!sidebar_pin_context_matches("album:1", "album:10"));
    }

    #[test]
    fn stale_sidebar_pin_result_cannot_rebuild_navigation() {
        let identity = SidebarPinIdentity {
            source: library::SourceKey::from_raw(1),
            folder: None,
            pins: Vec::new(),
        };
        assert!(sidebar_pin_publication_is_current(
            2,
            2,
            &identity,
            Some(&identity)
        ));
        assert!(!sidebar_pin_publication_is_current(
            1,
            2,
            &identity,
            Some(&identity)
        ));
    }
}

pub(super) fn install_mouse_history_buttons(shell: &Rc<Shell>) {
    let click = gtk::GestureClick::new();
    click.set_button(0);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);

    let history_shell = Rc::clone(shell);
    click.connect_pressed(move |click, _, _, _| match click.current_button() {
        MOUSE_BACK_BUTTON => {
            click.set_state(gtk::EventSequenceState::Claimed);
            history_shell.go_back();
        }
        MOUSE_FORWARD_BUTTON => {
            click.set_state(gtk::EventSequenceState::Claimed);
            history_shell.go_forward();
        }
        _ => {}
    });

    shell.chrome.window.add_controller(click);
}
