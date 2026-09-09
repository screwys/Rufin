use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use ui_shared::library_field_editor::{
    populate_library_field_rows, populate_library_field_rows_for_set,
};

use adw::prelude::*;
#[cfg(test)]
use gtk::glib;
use gtk::subclass::prelude::*;

use crate::CatalogUi;
use crate::{
    LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings, available_sort_fields,
};
use localization::{msgid, tr};
use ui_shared::controls::{ADD_ICON, sort_order_icon};
use ui_shared::layout::{
    configure_fill_width_clip, large_popup_content_height, large_popup_content_width,
    width_allocation_owner,
};
use ui_shared::localization::{
    bind_drop_down_options_with, bind_widget_tooltip, bind_widget_tooltip_with, localized_label,
};
use ui_shared::mounted_route::{MountedRoute, MountedRouteCommand, MountedRouteResume};
use ui_shared::popup::present_light_dismiss_dialog;

use ui_shared::library_fields::{
    field_set_for_layout, layout_button_content, layout_icon, layout_title, next_layout,
    supported_layouts, sync_layout_buttons,
};
const LIBRARY_CONFIG_DIALOG_WIDTH: i32 = 620;
const LIBRARY_CONFIG_DIALOG_HEIGHT: i32 = 560;
const LIBRARY_TOOLBAR_ICON_BUTTON_WIDTH: i32 = 34;
const LIBRARY_TOOLBAR_SORT_MIN_WIDTH: i32 = 112;
const LIBRARY_TOOLBAR_SORT_CHAR_WIDTH: i32 = 8;
const LIBRARY_TOOLBAR_SORT_HORIZONTAL_PADDING: i32 = 44;
const LIBRARY_TOOLBAR_SORT_WIDTH_SHARE: i32 = 4;
const LIBRARY_TOOLBAR_COMPACT_COMMAND_WIDTH: i32 = 760;

ui_shared::composite_box!(
    pub LibraryPageView,
    library_page_view_imp,
    "RufinLibraryPageView",
    "/io/github/screwys/Rufin/ui/routes/library_page.ui",
    {
        toolbar_host: gtk::Box,
        category_host: gtk::Box,
        contents: gtk::Stack,
        empty_view: gtk::Box,
        search_empty_view: gtk::Box,
        empty_label: gtk::Label,
        search_empty_label: gtk::Label,
    }
);

ui_shared::composite_box!(
    pub LibraryToolbarView,
    library_toolbar_view_imp,
    "RufinLibraryToolbarView",
    "/io/github/screwys/Rufin/ui/routes/library_toolbar.ui",
    {
        search_host: gtk::Box,
        controls: gtk::Box,
        command_button: gtk::Button,
        import_button: gtk::Button,
        sort_dropdown: gtk::DropDown,
        direction: gtk::Button,
        layout: gtk::Button,
        configure: gtk::Button,
        reservation_host: gtk::Box,
    }
);

pub struct LibraryPageShellOptions {
    pub key: LibraryListKey,
    pub empty: bool,
    pub empty_body: &'static str,
    pub search: gtk::SearchEntry,
    pub has_visible_results: Rc<dyn Fn() -> bool>,
    pub content: gtk::Widget,
}

#[derive(Clone)]
pub struct LibraryPageShell {
    widget: LibraryPageView,
    toolbar: LibraryToolbarProjection,
    search: gtk::SearchEntry,
    has_visible_results: Rc<dyn Fn() -> bool>,
    source_empty: Rc<Cell<bool>>,
    tab_cycle: Option<MountedRouteCommand>,
}

impl LibraryPageShell {
    pub fn widget(&self) -> gtk::Widget {
        self.widget.clone().upcast()
    }
    pub fn set_category_switcher(&self, widget: &impl IsA<gtk::Widget>) {
        self.widget.imp().category_host.append(widget);
        self.widget.imp().category_host.set_visible(true);
    }
    pub fn mounted_route(&self, resume: MountedRouteResume) -> MountedRoute {
        let route = MountedRoute::new(self.widget(), resume)
            .with_search(self.search.clone())
            .with_layout_cycle(self.toolbar.layout_cycle());
        if let Some(cycle) = &self.tab_cycle {
            return route.with_tab_cycle(Rc::clone(cycle));
        }
        route
    }
    pub fn set_tab_cycle(&mut self, cycle: MountedRouteCommand) {
        self.tab_cycle = Some(cycle);
    }
    pub fn apply_library_list_settings(&self, key: LibraryListKey, settings: &LibraryListSettings) {
        self.toolbar.apply(key, settings);
    }
    pub fn set_empty(&self, empty: bool) {
        self.source_empty.set(empty);
        show_library_page_child(
            &self.widget,
            empty,
            self.search.text().as_str(),
            (self.has_visible_results)(),
        );
    }

    pub fn configure_history_filter(
        &self,
        current_source_name: Option<String>,
        changed: impl Fn(bool) + 'static,
    ) {
        let dropdown = &self.toolbar.sort_dropdown;
        let labels = gtk::StringList::new(&[&tr("All")]);
        if let Some(name) = current_source_name.as_deref() {
            labels.append(name);
        }
        dropdown.set_model(Some(&labels));
        dropdown.set_selected(0);
        dropdown.set_sensitive(current_source_name.is_some());
        dropdown.connect_selected_notify(move |dropdown| changed(dropdown.selected() == 1));
    }
}

#[derive(Clone)]
pub struct LibraryToolbarProjection {
    state: Rc<RefCell<LibraryToolbarState>>,
    widget: gtk::Widget,
    sort_dropdown: gtk::DropDown,
    direction: gtk::Button,
    layout: gtk::Button,
    configure: gtk::Button,
    layout_mode: Rc<Cell<LibraryLayout>>,
    syncing: Rc<Cell<bool>>,
}

#[derive(Clone, Copy)]
struct LibraryToolbarState {
    key: LibraryListKey,
    include_detail: bool,
    sort_fields: &'static [LibraryField],
}

impl LibraryToolbarProjection {
    pub fn widget(&self) -> gtk::Widget {
        self.widget.clone()
    }

    pub fn set_layout_control_visible(&self, visible: bool) {
        self.layout.set_visible(visible);
    }

    pub fn set_configure_control_visible(&self, visible: bool) {
        self.configure.set_visible(visible);
    }

    pub fn cycle_layout(&self) {
        if self.layout.is_visible() && self.layout.is_sensitive() {
            self.layout.emit_clicked();
        }
    }

    pub fn layout_cycle(&self) -> MountedRouteCommand {
        let toolbar = self.clone();
        Rc::new(move || toolbar.cycle_layout())
    }

    pub fn apply(&self, key: LibraryListKey, settings: &LibraryListSettings) {
        let state = *self.state.borrow();
        if key != state.key {
            return;
        }
        self.syncing.set(true);
        if key != LibraryListKey::History {
            self.sort_dropdown.set_selected(
                state
                    .sort_fields
                    .iter()
                    .position(|field| *field == settings.sort_key)
                    .unwrap_or(0) as u32,
            );
        }
        self.direction
            .set_icon_name(sort_order_icon(settings.descending));
        let layout = toolbar_layout(settings.layout, state.include_detail);
        self.layout.set_icon_name(layout_icon(layout));
        self.layout_mode.set(layout);
        self.syncing.set(false);
    }
}

impl CatalogUi {
    pub fn library_page_shell(
        self: &Rc<Self>,
        options: LibraryPageShellOptions,
    ) -> LibraryPageShell {
        let wrapper = LibraryPageView::new();
        let toolbar = self.library_toolbar_projection(options.key, options.search.clone());
        wrapper.imp().toolbar_host.append(&toolbar.widget());
        wrapper.imp().empty_label.set_label(&tr(options.empty_body));
        wrapper
            .imp()
            .search_empty_label
            .set_label(&tr(msgid(r"No results ¯\_(°╭╮°)_/¯")));
        wrapper.imp().contents.add_child(&options.content);
        show_library_page_child(
            &wrapper,
            options.empty,
            options.search.text().as_str(),
            (options.has_visible_results)(),
        );
        let source_empty = Rc::new(Cell::new(options.empty));
        let weak_wrapper = wrapper.downgrade();
        let search_empty = Rc::clone(&source_empty);
        let search_visible = Rc::clone(&options.has_visible_results);
        options.search.connect_search_changed(move |search| {
            let Some(wrapper) = weak_wrapper.upgrade() else {
                return;
            };
            show_library_page_child(
                &wrapper,
                search_empty.get(),
                search.text().as_str(),
                search_visible(),
            );
        });
        LibraryPageShell {
            widget: wrapper,
            toolbar,
            search: options.search,
            has_visible_results: options.has_visible_results,
            source_empty,
            tab_cycle: None,
        }
    }

    pub fn library_toolbar_projection(
        self: &Rc<Self>,
        key: LibraryListKey,
        search: gtk::SearchEntry,
    ) -> LibraryToolbarProjection {
        self.library_toolbar_projection_with_detail(key, search, true)
    }

    pub fn library_toolbar_projection_without_detail(
        self: &Rc<Self>,
        key: LibraryListKey,
        search: gtk::SearchEntry,
        sort_fields: &'static [LibraryField],
    ) -> LibraryToolbarProjection {
        self.library_toolbar_projection_with_options(key, search, false, sort_fields, 6)
    }

    fn library_toolbar_projection_with_detail(
        self: &Rc<Self>,
        key: LibraryListKey,
        search: gtk::SearchEntry,
        include_detail: bool,
    ) -> LibraryToolbarProjection {
        self.library_toolbar_projection_with_options(
            key,
            search,
            include_detail,
            available_sort_fields(key),
            12,
        )
    }

    fn library_toolbar_projection_with_options(
        self: &Rc<Self>,
        key: LibraryListKey,
        search: gtk::SearchEntry,
        include_detail: bool,
        sort_fields: &'static [LibraryField],
        outer_spacing: i32,
    ) -> LibraryToolbarProjection {
        let toolbar = LibraryToolbarView::new();
        toolbar.set_spacing(outer_spacing);
        search.set_hexpand(true);
        search.set_width_request(1);
        toolbar.imp().search_host.append(&search);
        let toolbar_state = Rc::new(RefCell::new(LibraryToolbarState {
            key,
            include_detail,
            sort_fields,
        }));
        toolbar
            .imp()
            .import_button
            .set_visible(key == LibraryListKey::Playlists);
        toolbar
            .imp()
            .import_button
            .set_action_name(Some("win.import-playlist"));
        let command_button = toolbar.imp().command_button.get();
        set_library_command_button_content(&command_button, false, ADD_ICON, "New Playlist");
        bind_widget_tooltip(&command_button, "New Playlist");
        command_button.set_visible(matches!(
            key,
            LibraryListKey::Playlists | LibraryListKey::SmartPlaylists
        ));
        {
            let state = Rc::clone(&toolbar_state);
            command_button.connect_clicked(move |button| {
                let action = match state.borrow().key {
                    LibraryListKey::Playlists => "win.new-playlist",
                    LibraryListKey::SmartPlaylists => "win.new-smart-playlist",
                    _ => return,
                };
                let _ = button.activate_action(action, None);
            });
        }
        let settings = self.settings.current.borrow().library_list(key);
        let sort_options = gtk::StringList::new(&[]);
        let sort_dropdown = toolbar.imp().sort_dropdown.get();
        sort_dropdown.set_model(Some(&sort_options));
        let preferred_sort_width = Rc::new(Cell::new(LIBRARY_TOOLBAR_SORT_MIN_WIDTH));
        let preferred_sort_width_for_locale = Rc::clone(&preferred_sort_width);
        let locale_toolbar_state = Rc::clone(&toolbar_state);
        bind_drop_down_options_with(
            &sort_dropdown,
            move || {
                let state = locale_toolbar_state.borrow();
                state
                    .sort_fields
                    .iter()
                    .map(|field| library_sort_title(state.key, *field))
                    .collect()
            },
            move |labels| {
                let width =
                    library_toolbar_sort_width_for_labels(labels.iter().map(String::as_str));
                preferred_sort_width_for_locale.set(width);
                width
            },
        );
        configure_sort_dropdown_factory(&sort_dropdown);
        sort_dropdown.set_sensitive(sort_fields.len() > 1);
        sort_dropdown.set_hexpand(false);
        sort_dropdown.set_halign(gtk::Align::End);
        let syncing = Rc::new(Cell::new(false));
        sort_dropdown.set_selected(
            sort_fields
                .iter()
                .position(|field| *field == settings.sort_key)
                .unwrap_or(0) as u32,
        );
        {
            let shell = Rc::clone(self);
            let syncing = Rc::clone(&syncing);
            let state = Rc::clone(&toolbar_state);
            sort_dropdown.connect_selected_notify(move |dropdown| {
                if syncing.get() {
                    return;
                }
                let state = *state.borrow();
                if state.key == LibraryListKey::History {
                    return;
                }
                let sort_key = state
                    .sort_fields
                    .get(dropdown.selected() as usize)
                    .copied()
                    .unwrap_or(LibraryField::Title);
                shell.update_library_list_settings(state.key, |settings| {
                    settings.sort_key = sort_key
                });
            });
        }
        let direction = toolbar.imp().direction.get();
        direction.set_icon_name(sort_order_icon(settings.descending));
        configure_library_toolbar_icon_button(&direction, "Change sort order");
        {
            let shell = Rc::clone(self);
            let syncing = Rc::clone(&syncing);
            let state = Rc::clone(&toolbar_state);
            direction.connect_clicked(move |direction| {
                if syncing.get() {
                    return;
                }
                let mut descending = false;
                shell.update_library_list_settings(state.borrow().key, |settings| {
                    settings.descending = !settings.descending;
                    descending = settings.descending;
                });
                direction.set_icon_name(sort_order_icon(descending));
            });
        }
        let current_layout = toolbar_layout(settings.layout, include_detail);
        let layout = toolbar.imp().layout.get();
        layout.set_icon_name(layout_icon(current_layout));
        configure_library_toolbar_icon_button(&layout, "Layout");
        let layout_mode = Rc::new(Cell::new(current_layout));
        let layout_mode_for_locale = Rc::clone(&layout_mode);
        bind_widget_tooltip_with(&layout, move || {
            format!(
                "{}: {}",
                tr("Layout"),
                tr(layout_title(layout_mode_for_locale.get()))
            )
        });
        {
            let shell = Rc::clone(self);
            let syncing = Rc::clone(&syncing);
            let layout_mode = Rc::clone(&layout_mode);
            let state = Rc::clone(&toolbar_state);
            layout.connect_clicked(move |_| {
                if syncing.get() {
                    return;
                }
                let state = *state.borrow();
                shell.update_library_list_settings(state.key, |settings| {
                    settings.layout = if state.include_detail {
                        next_layout(state.key, settings.layout)
                    } else {
                        next_row_grid_layout(settings.layout)
                    };
                    layout_mode.set(settings.layout);
                });
            });
        }
        let configure = toolbar.imp().configure.get();
        configure_library_toolbar_icon_button(&configure, "Customize display");
        {
            let shell = Rc::clone(self);
            let state = Rc::clone(&toolbar_state);
            configure.connect_clicked(move |_| {
                let state = *state.borrow();
                shell.present_library_config_dialog_with_detail(state.key, state.include_detail);
            });
        }
        (self.reserve_window_controls)(&toolbar.imp().reservation_host, outer_spacing);
        let command_compact = Cell::new(false);
        apply_library_command_button_layout(&command_button, &command_compact, 1);
        let applied_sort_width = Cell::new(sort_dropdown.width_request());
        let sort_dropdown_for_width = sort_dropdown.clone();
        let command_button_for_width = command_button.clone();
        let preferred_sort_width_for_allocation = Rc::clone(&preferred_sort_width);
        let owner = width_allocation_owner(&toolbar, move |width| {
            apply_library_command_button_layout(&command_button_for_width, &command_compact, width);
            let sort_width =
                responsive_toolbar_sort_width(width, preferred_sort_width_for_allocation.get());
            if applied_sort_width.replace(sort_width) != sort_width {
                sort_dropdown_for_width.set_width_request(sort_width);
            }
        });
        let widget = ui_shared::controls::window_drag_handle(&owner).upcast();
        let projection = LibraryToolbarProjection {
            state: toolbar_state,
            widget,
            sort_dropdown,
            direction,
            layout,
            configure,
            layout_mode,
            syncing,
        };
        projection
    }
    fn present_library_config_dialog_with_detail(
        self: &Rc<Self>,
        key: LibraryListKey,
        include_detail: bool,
    ) {
        let window = self.window.upgrade().expect("mounted catalog window");
        let resource = crate::ui_resource::LIBRARY_CONFIG_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        ui_shared::objects!(builder, resource, {
            dialog: adw::Dialog,
            title: adw::WindowTitle,
            scroller: gtk::ScrolledWindow,
            reset: gtk::Button,
            layout_box: gtk::Box,
            fields_group: adw::PreferencesGroup,
        });
        title.set_subtitle(&tr(ui_shared::settings::library_list_title(key)));
        configure_fill_width_clip(&scroller, gtk::PolicyType::Automatic);
        dialog.set_content_width(large_popup_content_width(LIBRARY_CONFIG_DIALOG_WIDTH));
        dialog.set_content_height(large_popup_content_height(
            window.height(),
            LIBRARY_CONFIG_DIALOG_HEIGHT,
        ));
        let layout_buttons = Rc::new(RefCell::new(
            Vec::<(LibraryLayout, gtk::ToggleButton)>::new(),
        ));
        let mut first_button: Option<gtk::ToggleButton> = None;
        let current_layout = toolbar_layout(
            self.settings.current.borrow().library_list(key).layout,
            include_detail,
        );
        for layout in supported_layouts(key)
            .into_iter()
            .filter(|layout| include_detail || *layout != LibraryLayout::Detail)
        {
            let button = gtk::ToggleButton::new();
            button.add_css_class("preference-selection-button");
            button.set_child(Some(&layout_button_content(layout)));
            button.set_tooltip_text(Some(&tr(layout_title(layout))));
            if let Some(first) = &first_button {
                button.set_group(Some(first));
            } else {
                first_button = Some(button.clone());
            }
            button.set_active(layout == current_layout);
            layout_box.append(&button);
            layout_buttons.borrow_mut().push((layout, button));
        }
        let rows = Rc::new(RefCell::new(Vec::<adw::ActionRow>::new()));

        for (layout, button) in layout_buttons.borrow().iter() {
            let settings = Rc::clone(&self.settings);
            let changed = self.library_settings_changed();
            let fields_group = fields_group.clone();
            let rows = Rc::clone(&rows);
            let layout_buttons = Rc::downgrade(&layout_buttons);
            let layout = *layout;
            button.connect_toggled(move |button| {
                if !button.is_active()
                    || settings.current.borrow().library_list(key).layout == layout
                {
                    return;
                }
                if settings.update_library_list_settings(key, |settings| {
                    settings.layout = layout;
                }) {
                    changed();
                }
                if let Some(layout_buttons) = layout_buttons.upgrade() {
                    sync_layout_buttons(&layout_buttons, layout);
                }
                populate_library_field_rows(
                    &settings,
                    Rc::clone(&changed),
                    key,
                    &fields_group,
                    &rows,
                );
            });
        }

        {
            let settings = Rc::clone(&self.settings);
            let changed = self.library_settings_changed();
            let fields_group = fields_group.clone();
            let rows = Rc::clone(&rows);
            let layout_buttons = Rc::clone(&layout_buttons);
            reset.connect_clicked(move |_| {
                let default_settings = LibraryListSettings::for_key(key);
                if settings.update_library_list_settings(key, |settings| {
                    *settings = default_settings.clone();
                }) {
                    changed();
                }
                sync_layout_buttons(&layout_buttons, default_settings.layout);
                populate_library_field_rows(
                    &settings,
                    Rc::clone(&changed),
                    key,
                    &fields_group,
                    &rows,
                );
            });
        }

        if include_detail {
            populate_library_field_rows(
                &self.settings,
                self.library_settings_changed(),
                key,
                &fields_group,
                &rows,
            );
        } else {
            populate_library_field_rows_for_set(
                &self.settings,
                self.library_settings_changed(),
                key,
                field_set_for_layout(current_layout),
                &fields_group,
                &rows,
            );
        }

        present_light_dismiss_dialog(&dialog, &window);
    }
}

fn library_page_child(source_empty: bool, query: &str, has_visible_results: bool) -> &'static str {
    if source_empty {
        "empty"
    } else if !query.trim().is_empty() && !has_visible_results {
        "search-empty"
    } else {
        "content"
    }
}

fn show_library_page_child(
    page: &LibraryPageView,
    source_empty: bool,
    query: &str,
    has_visible_results: bool,
) {
    let child: gtk::Widget = match library_page_child(source_empty, query, has_visible_results) {
        "empty" => page.imp().empty_view.get().upcast(),
        "search-empty" => page.imp().search_empty_view.get().upcast(),
        _ => page
            .imp()
            .contents
            .last_child()
            .expect("library page content"),
    };
    page.imp().contents.set_visible_child(&child);
}

fn toolbar_layout(layout: LibraryLayout, include_detail: bool) -> LibraryLayout {
    if !include_detail && layout == LibraryLayout::Detail {
        LibraryLayout::Grid
    } else {
        layout
    }
}

fn next_row_grid_layout(layout: LibraryLayout) -> LibraryLayout {
    match layout {
        LibraryLayout::Row => LibraryLayout::Grid,
        LibraryLayout::Grid | LibraryLayout::Detail => LibraryLayout::Row,
    }
}

#[cfg(test)]
mod search_empty_tests {
    use super::{library_page_child, next_layout};
    use crate::{LibraryLayout, LibraryListKey};

    #[test]
    fn library_page_only_shows_search_empty_for_an_unmatched_query() {
        assert_eq!(library_page_child(false, "missing", false), "search-empty");
        assert_eq!(library_page_child(false, "", false), "content");
        assert_eq!(library_page_child(false, "found", true), "content");
        assert_eq!(library_page_child(true, "missing", false), "empty");
    }

    #[test]
    fn layout_cycle_includes_detail_only_when_the_route_supports_it() {
        assert_eq!(
            next_layout(LibraryListKey::Albums, LibraryLayout::Row),
            LibraryLayout::Grid
        );
        assert_eq!(
            next_layout(LibraryListKey::Albums, LibraryLayout::Grid),
            LibraryLayout::Detail
        );
        assert_eq!(
            next_layout(LibraryListKey::Albums, LibraryLayout::Detail),
            LibraryLayout::Row
        );
        assert_eq!(
            next_layout(LibraryListKey::Artists, LibraryLayout::Grid),
            LibraryLayout::Row
        );
    }
}

fn library_sort_title(key: LibraryListKey, field: LibraryField) -> &'static str {
    if key == LibraryListKey::PlaylistTracks && field == LibraryField::RowIndex {
        msgid("Playlist order")
    } else {
        ui_shared::settings::library_field_title(field)
    }
}

#[cfg(test)]
fn weak_target_callback<T, A>(
    target: &T,
    callback: impl Fn(&T, A) + 'static,
) -> impl Fn(A) + 'static
where
    T: glib::object::ObjectType,
{
    let weak_target = target.downgrade();
    move |argument| {
        if let Some(target) = weak_target.upgrade() {
            callback(&target, argument);
        }
    }
}

fn library_toolbar_compact_for_width(width: i32) -> bool {
    width < LIBRARY_TOOLBAR_COMPACT_COMMAND_WIDTH
}
pub fn toolbar_sort_width_for_labels<'a>(labels: impl IntoIterator<Item = &'a str>) -> i32 {
    labels
        .into_iter()
        .map(toolbar_sort_label_width)
        .max()
        .unwrap_or(LIBRARY_TOOLBAR_SORT_MIN_WIDTH)
}

pub fn library_toolbar_sort_width_for_labels<'a>(labels: impl IntoIterator<Item = &'a str>) -> i32 {
    let route_width = toolbar_sort_width_for_labels(labels);
    let shared_library_width = LibraryListKey::all()
        .into_iter()
        .flat_map(|key| {
            available_sort_fields(key)
                .iter()
                .map(move |field| library_sort_title(key, *field))
        })
        .map(|title| toolbar_sort_label_width(&tr(title)))
        .max()
        .unwrap_or(LIBRARY_TOOLBAR_SORT_MIN_WIDTH);
    route_width.max(shared_library_width)
}

fn toolbar_sort_label_width(label: &str) -> i32 {
    (label.chars().count() as i32 * LIBRARY_TOOLBAR_SORT_CHAR_WIDTH
        + LIBRARY_TOOLBAR_SORT_HORIZONTAL_PADDING)
        .max(LIBRARY_TOOLBAR_SORT_MIN_WIDTH)
}

pub fn responsive_toolbar_sort_width(toolbar_width: i32, preferred_width: i32) -> i32 {
    (toolbar_width / LIBRARY_TOOLBAR_SORT_WIDTH_SHARE)
        .max(1)
        .min(preferred_width.max(1))
}

fn configure_sort_dropdown_factory(dropdown: &gtk::DropDown) {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_lines(2);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        item.set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(label) = item
            .child()
            .and_then(|child| child.downcast::<gtk::Label>().ok())
        else {
            return;
        };
        let Some(value) = item
            .item()
            .and_then(|value| value.downcast::<gtk::StringObject>().ok())
        else {
            return;
        };
        label.set_text(&value.string());
    });
    dropdown.set_factory(Some(&factory));
}

fn apply_library_command_button_layout(
    command_button: &gtk::Button,
    command_compact: &Cell<bool>,
    width: i32,
) {
    let width = width.max(1);
    let compact = library_toolbar_compact_for_width(width);
    if command_compact.replace(compact) != compact {
        set_library_command_button_content(command_button, compact, ADD_ICON, "New Playlist");
    }
}

fn configure_library_toolbar_icon_button(button: &gtk::Button, tooltip: &str) {
    button.add_css_class("flat");
    button.add_css_class("icon-button");
    button.add_css_class("circular");
    button.add_css_class("library-toolbar-icon-button");
    button.set_width_request(LIBRARY_TOOLBAR_ICON_BUTTON_WIDTH);
    bind_widget_tooltip(button, tooltip);
}

fn set_library_command_button_content(
    button: &gtk::Button,
    compact: bool,
    icon_name: &str,
    label: &str,
) {
    button.add_css_class("flat");
    if compact {
        button.remove_css_class("pill-button");
        button.remove_css_class("pill");
        button.add_css_class("icon-button");
        button.add_css_class("circular");
        button.set_child(Some(&gtk::Image::from_icon_name(icon_name)));
        return;
    }

    button.remove_css_class("icon-button");
    button.remove_css_class("circular");
    button.add_css_class("pill-button");
    button.add_css_class("pill");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name(icon_name));
    content.append(&localized_label(label));
    button.set_child(Some(&content));
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use adw::prelude::*;

    use super::{next_row_grid_layout, toolbar_layout, weak_target_callback};
    use crate::LibraryLayout;

    #[test]
    fn narrow_toolbar_can_shrink_the_sort_control_below_its_normal_floor() {
        assert_eq!(super::responsive_toolbar_sort_width(414, 200), 103);
        assert_eq!(super::responsive_toolbar_sort_width(448, 200), 112);
        assert_eq!(super::responsive_toolbar_sort_width(800, 160), 160);
    }

    #[test]
    fn signal_callback_does_not_retain_its_source_or_target() {
        let source = gtk::gio::SimpleAction::new("source", None);
        let weak_source = source.downgrade();
        let target = gtk::gio::SimpleAction::new("target", None);
        let weak_target = target.downgrade();
        let calls = Rc::new(Cell::new(0));
        let callback_calls = Rc::clone(&calls);
        let callback = weak_target_callback(&target, move |_, ()| {
            callback_calls.set(callback_calls.get() + 1);
        });
        source.connect_activate(move |_, _| callback(()));

        source.activate(None);
        assert_eq!(calls.get(), 1);
        drop(target);
        source.activate(None);

        assert!(
            weak_target.upgrade().is_none(),
            "the callback must not retain its target"
        );
        assert_eq!(calls.get(), 1);
        drop(source);
        assert!(
            weak_source.upgrade().is_none(),
            "the callback must not retain its signal source"
        );
    }

    #[test]
    fn search_toolbar_cycles_only_presentable_row_and_grid_layouts() {
        assert_eq!(
            toolbar_layout(LibraryLayout::Detail, false),
            LibraryLayout::Grid
        );
        assert_eq!(
            next_row_grid_layout(LibraryLayout::Detail),
            LibraryLayout::Row
        );
        assert_eq!(
            next_row_grid_layout(LibraryLayout::Row),
            LibraryLayout::Grid
        );
        assert_eq!(
            next_row_grid_layout(LibraryLayout::Grid),
            LibraryLayout::Row
        );
    }
}
