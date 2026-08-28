use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use adw::prelude::*;
use gtk::glib;

use crate::layout::{
    configure_fill_width_clip, large_popup_content_height, large_popup_content_width,
    width_allocation_owner,
};
use crate::localization::{
    bind_drop_down_options_with, bind_widget_tooltip, bind_widget_tooltip_with, localized_label,
};
use crate::player::{select_next_audio_output, select_previous_audio_output};
use crate::preferences::dialogs::popup::present_light_dismiss_dialog;
use crate::shell::Shell;
use crate::shell::actions::{ADD_ICON, MORE_ICON, sort_order_icon, toggle_mute_shortcut};
use crate::{
    LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings, available_sort_fields,
};
use localization::{msgid, tr};

use super::collections::library_route_inset;
use super::library_fields::{
    field_set_for_layout, layout_button_content, layout_icon, layout_title, next_layout,
    populate_library_field_rows, populate_library_field_rows_for_set, supported_layouts,
    sync_layout_buttons,
};
use super::route_layout::ROUTE_TOP_MARGIN;

const LIBRARY_CONFIG_DIALOG_WIDTH: i32 = 620;
const LIBRARY_CONFIG_DIALOG_HEIGHT: i32 = 560;
const LIBRARY_ROUTE_BOTTOM_MARGIN: i32 = 8;
const LIBRARY_TOOLBAR_CONTROL_SPACING: i32 = 12;
const LIBRARY_TOOLBAR_ICON_BUTTON_WIDTH: i32 = 34;
const LIBRARY_TOOLBAR_SORT_MIN_WIDTH: i32 = 112;
const LIBRARY_TOOLBAR_SORT_CHAR_WIDTH: i32 = 8;
const LIBRARY_TOOLBAR_SORT_HORIZONTAL_PADDING: i32 = 44;
const LIBRARY_TOOLBAR_SORT_WIDTH_SHARE: i32 = 4;
const LIBRARY_TOOLBAR_COMPACT_COMMAND_WIDTH: i32 = 760;
pub(crate) struct LibraryPageShellOptions {
    pub(crate) key: LibraryListKey,
    pub(crate) empty: bool,
    pub(crate) empty_body: &'static str,
    pub(crate) search: gtk::SearchEntry,
    pub(crate) has_visible_results: Rc<dyn Fn() -> bool>,
    pub(crate) content: gtk::Widget,
}

#[derive(Clone)]
pub(crate) struct LibraryPageShell {
    widget: gtk::Box,
    contents: gtk::Stack,
    toolbar: LibraryToolbarProjection,
    search: gtk::SearchEntry,
    has_visible_results: Rc<dyn Fn() -> bool>,
    source_empty: Rc<Cell<bool>>,
}

impl LibraryPageShell {
    pub(crate) fn widget(&self) -> gtk::Widget {
        self.widget.clone().upcast()
    }
    pub(crate) fn insert_after_toolbar(&self, widget: &impl IsA<gtk::Widget>) {
        self.widget
            .insert_child_after(widget, Some(&self.toolbar.widget()));
    }
    pub(crate) fn apply_library_list_settings(
        &self,
        key: LibraryListKey,
        settings: &LibraryListSettings,
    ) {
        self.toolbar.apply(key, settings);
    }
    pub(crate) fn set_empty(&self, empty: bool) {
        self.source_empty.set(empty);
        self.contents.set_visible_child_name(library_page_child(
            empty,
            self.search.text().as_str(),
            (self.has_visible_results)(),
        ));
    }
}

#[derive(Clone)]
pub(crate) struct LibraryToolbarProjection {
    state: Rc<RefCell<LibraryToolbarState>>,
    widget: gtk::Widget,
    controls: gtk::Box,
    sort_dropdown: gtk::DropDown,
    direction: gtk::Button,
    layout: gtk::Button,
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
    pub(crate) fn widget(&self) -> gtk::Widget {
        self.widget.clone()
    }

    pub(crate) fn detach_controls(&self) -> gtk::Widget {
        if let Some(parent) = self
            .controls
            .parent()
            .and_then(|parent| parent.downcast::<gtk::Box>().ok())
        {
            parent.remove(&self.controls);
        }
        self.controls.clone().upcast()
    }

    pub(crate) fn set_layout_control_visible(&self, visible: bool) {
        self.layout.set_visible(visible);
    }

    pub(crate) fn cycle_layout(&self) {
        if self.layout.is_visible() && self.layout.is_sensitive() {
            self.layout.emit_clicked();
        }
    }

    pub(crate) fn apply(&self, key: LibraryListKey, settings: &LibraryListSettings) {
        let state = *self.state.borrow();
        if key != state.key {
            return;
        }
        self.syncing.set(true);
        self.sort_dropdown.set_selected(
            state
                .sort_fields
                .iter()
                .position(|field| *field == settings.sort_key)
                .unwrap_or(0) as u32,
        );
        self.direction
            .set_icon_name(sort_order_icon(settings.descending));
        let layout = toolbar_layout(settings.layout, state.include_detail);
        self.layout.set_icon_name(layout_icon(layout));
        self.layout_mode.set(layout);
        self.syncing.set(false);
    }
}

impl Shell {
    pub(crate) fn library_page_shell(
        self: &Rc<Self>,
        options: LibraryPageShellOptions,
    ) -> LibraryPageShell {
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, ROUTE_TOP_MARGIN);
        wrapper.add_css_class("route-content");
        wrapper.set_margin_top(ROUTE_TOP_MARGIN);
        wrapper.set_margin_bottom(LIBRARY_ROUTE_BOTTOM_MARGIN);
        wrapper.set_hexpand(true);
        wrapper.set_vexpand(true);
        let toolbar = self.library_toolbar_projection(options.key, options.search.clone());
        wrapper.append(&library_route_inset(toolbar.widget()));
        self.set_route_search(Some(options.search.clone()));
        let contents = gtk::Stack::new();
        contents.set_hexpand(true);
        contents.set_vexpand(true);
        contents.add_named(
            &library_route_inset(self.route_empty_view(options.empty_body)),
            Some("empty"),
        );
        contents.add_named(
            &library_route_inset(self.route_empty_view(msgid(r"No results ¯\_(°╭╮°)_/¯"))),
            Some("search-empty"),
        );
        contents.add_named(&options.content, Some("content"));
        contents.set_visible_child_name(library_page_child(
            options.empty,
            options.search.text().as_str(),
            (options.has_visible_results)(),
        ));
        wrapper.append(&contents);
        let source_empty = Rc::new(Cell::new(options.empty));
        let weak_contents = contents.downgrade();
        let search_empty = Rc::clone(&source_empty);
        let search_visible = Rc::clone(&options.has_visible_results);
        options.search.connect_search_changed(move |search| {
            let Some(contents) = weak_contents.upgrade() else {
                return;
            };
            contents.set_visible_child_name(library_page_child(
                search_empty.get(),
                search.text().as_str(),
                search_visible(),
            ));
        });
        LibraryPageShell {
            widget: wrapper,
            contents,
            toolbar,
            search: options.search,
            has_visible_results: options.has_visible_results,
            source_empty,
        }
    }

    pub(crate) fn set_route_search(&self, search: Option<gtk::SearchEntry>) {
        self.route_viewport.route_search.replace(search);
        self.route_viewport.route_search_focus.borrow_mut().take();
    }

    pub(crate) fn set_route_search_with_focus(
        &self,
        search: gtk::SearchEntry,
        focus: Rc<dyn Fn()>,
    ) {
        self.route_viewport.route_search.replace(Some(search));
        self.route_viewport.route_search_focus.replace(Some(focus));
    }

    pub(crate) fn set_current_route_layout_cycle(&self, cycle: Option<Rc<dyn Fn()>>) {
        self.route_viewport.route_layout_cycle.replace(cycle);
    }

    pub(crate) fn set_current_route_tab_cycle(&self, cycle: Option<Rc<dyn Fn()>>) {
        self.route_viewport.route_tab_cycle.replace(cycle);
    }

    pub(crate) fn focus_current_route_search(&self) {
        if !self.route_keyboard_available() {
            return;
        }
        let search = self.route_viewport.route_search.borrow().as_ref().cloned();
        if let Some(search) = search {
            let focus = self
                .route_viewport
                .route_search_focus
                .borrow()
                .as_ref()
                .cloned();
            if let Some(focus) = focus {
                focus();
            } else {
                search.grab_focus();
            }
        }
    }

    fn route_keyboard_available(&self) -> bool {
        !self.source.login_screen_active()
            && !self.fullscreen_player_visible()
            && !self.transient_route_input_active()
    }

    fn playback_keyboard_available(&self) -> bool {
        !self.source.login_screen_active() && !self.transient_route_input_active()
    }

    fn transient_route_input_active(&self) -> bool {
        self.preferences.active_dialog().is_some()
            || self.source.add_server.borrow().is_some()
            || self
                .selected_lyrics()
                .is_some_and(|lyrics| lyrics.search_dialog.borrow().is_some())
    }

    pub(crate) fn connect_route_keyboard(self: &Rc<Self>) {
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let shell = Rc::clone(self);
        key.connect_key_pressed(move |_, key, _, state| {
            let current_focus = GtkWindowExt::focus(&shell.chrome.window);
            if key_has_no_shortcut_modifiers(state) {
                if key == gtk::gdk::Key::space
                    && shell.playback_keyboard_available()
                    && !focus_blocks_playback_shortcut(current_focus.as_ref())
                {
                    shell.products.playback.transport.play_pause();
                    return glib::Propagation::Stop;
                }
                if key
                    .to_unicode()
                    .is_some_and(|character| character.eq_ignore_ascii_case(&'m'))
                    && shell.playback_keyboard_available()
                    && !focus_blocks_playback_shortcut(current_focus.as_ref())
                {
                    toggle_mute_shortcut(&shell);
                    return glib::Propagation::Stop;
                }
                if shell.route_keyboard_available()
                    && !focus_blocks_page_navigation(current_focus.as_ref())
                    && let Some(direction) = page_navigation_direction(key)
                {
                    return shell.navigate_current_route_items(direction);
                }
            }
            if key_has_audio_output_modifiers(state)
                && shell.playback_keyboard_available()
                && !focus_blocks_playback_shortcut(current_focus.as_ref())
            {
                match key {
                    gtk::gdk::Key::Up => {
                        select_previous_audio_output(&shell);
                        return glib::Propagation::Stop;
                    }
                    gtk::gdk::Key::Down => {
                        select_next_audio_output(&shell);
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            }
            let Some(search) = shell.route_viewport.route_search.borrow().as_ref().cloned() else {
                return glib::Propagation::Proceed;
            };
            let focus = shell
                .route_viewport
                .route_search_focus
                .borrow()
                .as_ref()
                .cloned();
            if !shell.settings.current.borrow().type_to_search_enabled
                || !shell.route_keyboard_available()
                || key_should_bypass_type_to_search(state)
                || focus_blocks_type_to_search(
                    GtkWindowExt::focus(&shell.chrome.window).as_ref(),
                    &search,
                )
            {
                return glib::Propagation::Proceed;
            }
            let Some(character) = key.to_unicode().filter(|character| !character.is_control())
            else {
                return glib::Propagation::Proceed;
            };
            if character.is_whitespace() && search.text().trim().is_empty() {
                return glib::Propagation::Proceed;
            }
            let mut position = search.position();
            if let Some((start, end)) = search.selection_bounds() {
                search.delete_text(start, end);
                position = start;
            }
            search.insert_text(&character.to_string(), &mut position);
            search.set_position(position);
            if let Some(focus) = focus {
                focus();
            } else {
                search.grab_focus();
            }
            glib::Propagation::Stop
        });
        self.chrome.window.add_controller(key);
    }
    pub(crate) fn library_toolbar_projection(
        self: &Rc<Self>,
        key: LibraryListKey,
        search: gtk::SearchEntry,
    ) -> LibraryToolbarProjection {
        self.library_toolbar_projection_with_detail(key, search, true)
    }

    pub(crate) fn library_toolbar_projection_without_detail(
        self: &Rc<Self>,
        key: LibraryListKey,
        search: gtk::SearchEntry,
        sort_fields: &'static [LibraryField],
    ) -> LibraryToolbarProjection {
        self.library_toolbar_projection_with_options(key, search, false, sort_fields)
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
        )
    }

    fn library_toolbar_projection_with_options(
        self: &Rc<Self>,
        key: LibraryListKey,
        search: gtk::SearchEntry,
        include_detail: bool,
        sort_fields: &'static [LibraryField],
    ) -> LibraryToolbarProjection {
        let toolbar = gtk::Box::new(
            gtk::Orientation::Horizontal,
            LIBRARY_TOOLBAR_CONTROL_SPACING,
        );
        toolbar.add_css_class("track-toolbar");
        toolbar.set_hexpand(true);
        toolbar.set_halign(gtk::Align::Fill);
        toolbar.set_width_request(1);
        search.set_hexpand(true);
        search.set_width_request(1);
        toolbar.append(&search);
        let controls = gtk::Box::new(
            gtk::Orientation::Horizontal,
            LIBRARY_TOOLBAR_CONTROL_SPACING,
        );
        let toolbar_state = Rc::new(RefCell::new(LibraryToolbarState {
            key,
            include_detail,
            sort_fields,
        }));
        let command_button = gtk::Button::new();
        set_library_command_button_content(&command_button, false, ADD_ICON, "New Playlist");
        bind_widget_tooltip(&command_button, "New Playlist");
        command_button.set_visible(matches!(
            key,
            LibraryListKey::Playlists | LibraryListKey::SmartPlaylists
        ));
        {
            let shell = Rc::clone(self);
            let state = Rc::clone(&toolbar_state);
            command_button.connect_clicked(move |_| match state.borrow().key {
                LibraryListKey::Playlists => shell.new_playlist_dialog(),
                LibraryListKey::SmartPlaylists => shell.new_smart_playlist_dialog(),
                _ => {}
            });
        }
        controls.append(&command_button);

        let settings = self.settings.current.borrow().library_list(key);
        let sort_options = gtk::StringList::new(&[]);
        let sort_dropdown = gtk::DropDown::new(Some(sort_options), None::<gtk::Expression>);
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
        controls.append(&sort_dropdown);

        let direction = gtk::Button::from_icon_name(sort_order_icon(settings.descending));
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
        controls.append(&direction);

        let current_layout = toolbar_layout(settings.layout, include_detail);
        let layout = gtk::Button::from_icon_name(layout_icon(current_layout));
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
        controls.append(&layout);

        let configure = gtk::Button::from_icon_name(MORE_ICON);
        configure_library_toolbar_icon_button(&configure, "Customize display");
        {
            let shell = Rc::clone(self);
            let state = Rc::clone(&toolbar_state);
            configure.connect_clicked(move |_| {
                let state = *state.borrow();
                shell.present_library_config_dialog_with_detail(state.key, state.include_detail);
            });
        }
        controls.append(&configure);
        toolbar.append(&self.window_controls_end_area(&controls, LIBRARY_TOOLBAR_CONTROL_SPACING));
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
        let widget = owner.upcast();
        let projection = LibraryToolbarProjection {
            state: toolbar_state,
            widget,
            controls,
            sort_dropdown,
            direction,
            layout,
            layout_mode,
            syncing,
        };
        let layout_projection = projection.clone();
        self.set_current_route_layout_cycle(Some(Rc::new(move || {
            layout_projection.cycle_layout()
        })));
        projection
    }
    pub(crate) fn window_controls_end_area(
        &self,
        controls: &impl IsA<gtk::Widget>,
        spacing: i32,
    ) -> gtk::Box {
        let reservation = self.chrome.window_controls.end_width_reservation();
        reservation.set_margin_start(spacing);
        let reservation_host = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        reservation_host.append(&reservation);
        self.right_panel
            .right_panel_slot
            .bind_property("visible", &reservation_host, "visible")
            .sync_create()
            .invert_boolean()
            .build();

        let area = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        area.append(controls);
        area.append(&reservation_host);
        area
    }
    fn present_library_config_dialog_with_detail(
        self: &Rc<Self>,
        key: LibraryListKey,
        include_detail: bool,
    ) {
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let title = adw::WindowTitle::new(&tr("Customize display"), &tr(key.title()));
        header.set_title_widget(Some(&title));
        toolbar.add_top_bar(&header);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.set_margin_top(18);
        content.set_margin_bottom(18);
        content.set_margin_start(18);
        content.set_margin_end(18);

        let layout_group = adw::PreferencesGroup::builder().title(tr("Layout")).build();
        let reset = gtk::Button::with_label(&tr("Reset"));
        reset.add_css_class("destructive-action");
        reset.set_valign(gtk::Align::End);
        reset.set_margin_end(16);
        bind_widget_tooltip(&reset, msgid("Reset to defaults"));
        layout_group.set_header_suffix(Some(&reset));
        let layout_row = adw::ActionRow::builder().title(tr("View")).build();
        let layout_buttons = Rc::new(RefCell::new(
            Vec::<(LibraryLayout, gtk::ToggleButton)>::new(),
        ));
        let layout_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        layout_box.add_css_class("linked");
        layout_box.add_css_class("preference-selection-buttons");
        layout_box.set_valign(gtk::Align::Center);
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
        layout_row.add_suffix(&layout_box);
        layout_group.add(&layout_row);
        content.append(&layout_group);

        let fields_group = adw::PreferencesGroup::builder().build();
        let rows = Rc::new(RefCell::new(Vec::<adw::ActionRow>::new()));
        content.append(&fields_group);

        for (layout, button) in layout_buttons.borrow().iter() {
            let shell = Rc::clone(self);
            let fields_group = fields_group.clone();
            let rows = Rc::clone(&rows);
            let layout_buttons = Rc::downgrade(&layout_buttons);
            let layout = *layout;
            button.connect_toggled(move |button| {
                if !button.is_active()
                    || shell.settings.current.borrow().library_list(key).layout == layout
                {
                    return;
                }
                shell.update_library_list_settings(key, |settings| {
                    settings.layout = layout;
                });
                if let Some(layout_buttons) = layout_buttons.upgrade() {
                    sync_layout_buttons(&layout_buttons, layout);
                }
                populate_library_field_rows(&shell, key, &fields_group, &rows);
            });
        }

        {
            let shell = Rc::clone(self);
            let fields_group = fields_group.clone();
            let rows = Rc::clone(&rows);
            let layout_buttons = Rc::clone(&layout_buttons);
            reset.connect_clicked(move |_| {
                let default_settings = LibraryListSettings::for_key(key);
                shell.update_library_list_settings(key, |settings| {
                    *settings = default_settings.clone();
                });
                sync_layout_buttons(&layout_buttons, default_settings.layout);
                populate_library_field_rows(&shell, key, &fields_group, &rows);
            });
        }

        if include_detail {
            populate_library_field_rows(self, key, &fields_group, &rows);
        } else {
            populate_library_field_rows_for_set(
                self,
                key,
                field_set_for_layout(current_layout),
                &fields_group,
                &rows,
            );
        }

        let scroller = gtk::ScrolledWindow::new();
        configure_fill_width_clip(&scroller, gtk::PolicyType::Automatic);
        scroller.set_child(Some(&content));
        toolbar.set_content(Some(&scroller));

        let dialog = adw::Dialog::builder()
            .content_width(large_popup_content_width(LIBRARY_CONFIG_DIALOG_WIDTH))
            .content_height(large_popup_content_height(
                self.chrome.window.height(),
                LIBRARY_CONFIG_DIALOG_HEIGHT,
            ))
            .child(&toolbar)
            .build();
        present_light_dismiss_dialog(&dialog, &self.chrome.window);
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
        field.title()
    }
}

pub(super) fn restore_single_click_activation_on_primary_press<T>(
    target: &T,
    restore: impl Fn(&T) + 'static,
) where
    T: IsA<gtk::Widget> + Clone + 'static,
{
    let pointer = gtk::GestureClick::new();
    pointer.set_button(gtk::gdk::BUTTON_PRIMARY);
    pointer.set_propagation_phase(gtk::PropagationPhase::Capture);
    let restore = weak_target_callback(target, move |target, ()| restore(target));
    pointer.connect_pressed(move |_, _, _, _| restore(()));
    target.add_controller(pointer);
}

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

fn key_should_bypass_type_to_search(state: gtk::gdk::ModifierType) -> bool {
    state.intersects(
        gtk::gdk::ModifierType::ALT_MASK
            | gtk::gdk::ModifierType::CONTROL_MASK
            | gtk::gdk::ModifierType::SUPER_MASK
            | gtk::gdk::ModifierType::HYPER_MASK
            | gtk::gdk::ModifierType::META_MASK,
    )
}

fn key_has_no_shortcut_modifiers(state: gtk::gdk::ModifierType) -> bool {
    !state.intersects(
        gtk::gdk::ModifierType::SHIFT_MASK
            | gtk::gdk::ModifierType::ALT_MASK
            | gtk::gdk::ModifierType::CONTROL_MASK
            | gtk::gdk::ModifierType::SUPER_MASK
            | gtk::gdk::ModifierType::HYPER_MASK
            | gtk::gdk::ModifierType::META_MASK,
    )
}

fn key_has_audio_output_modifiers(state: gtk::gdk::ModifierType) -> bool {
    #[cfg(target_os = "macos")]
    let expected = gtk::gdk::ModifierType::ALT_MASK | gtk::gdk::ModifierType::META_MASK;
    #[cfg(not(target_os = "macos"))]
    let expected = gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK;
    let shortcut_modifiers = gtk::gdk::ModifierType::SHIFT_MASK
        | gtk::gdk::ModifierType::ALT_MASK
        | gtk::gdk::ModifierType::CONTROL_MASK
        | gtk::gdk::ModifierType::SUPER_MASK
        | gtk::gdk::ModifierType::HYPER_MASK
        | gtk::gdk::ModifierType::META_MASK;
    state & shortcut_modifiers == expected
}

fn page_navigation_direction(key: gtk::gdk::Key) -> Option<gtk::DirectionType> {
    match key {
        gtk::gdk::Key::Up => Some(gtk::DirectionType::Up),
        gtk::gdk::Key::Down => Some(gtk::DirectionType::Down),
        gtk::gdk::Key::Left => Some(gtk::DirectionType::Left),
        gtk::gdk::Key::Right => Some(gtk::DirectionType::Right),
        _ => None,
    }
}

fn focus_blocks_playback_shortcut(focus: Option<&gtk::Widget>) -> bool {
    focus.is_some_and(|focus| focus_is_text_input(focus) || focus_is_in_dialog(focus))
}

fn focus_blocks_page_navigation(focus: Option<&gtk::Widget>) -> bool {
    focus.is_some_and(|focus| {
        focus_is_in_dialog(focus)
            || focus_is_text_input(focus)
            || focus.is::<gtk::Range>()
            || focus.ancestor(gtk::Range::static_type()).is_some()
            || focus.is::<gtk::DropDown>()
            || focus.ancestor(gtk::DropDown::static_type()).is_some()
    })
}

fn focus_is_in_dialog(focus: &gtk::Widget) -> bool {
    focus.is::<adw::Dialog>() || focus.ancestor(adw::Dialog::static_type()).is_some()
}

fn focus_is_text_input(focus: &gtk::Widget) -> bool {
    focus.is::<gtk::Editable>()
        || focus.is::<gtk::TextView>()
        || focus.ancestor(gtk::Editable::static_type()).is_some()
        || focus.ancestor(gtk::TextView::static_type()).is_some()
}

fn focus_blocks_type_to_search(focus: Option<&gtk::Widget>, search: &gtk::SearchEntry) -> bool {
    let Some(focus) = focus else {
        return false;
    };
    focus.is_ancestor(search) || focus_is_text_input(focus) || focus_is_in_dialog(focus)
}

fn library_toolbar_compact_for_width(width: i32) -> bool {
    width < LIBRARY_TOOLBAR_COMPACT_COMMAND_WIDTH
}
pub(crate) fn toolbar_sort_width_for_labels<'a>(labels: impl IntoIterator<Item = &'a str>) -> i32 {
    labels
        .into_iter()
        .map(toolbar_sort_label_width)
        .max()
        .unwrap_or(LIBRARY_TOOLBAR_SORT_MIN_WIDTH)
}

pub(crate) fn library_toolbar_sort_width_for_labels<'a>(
    labels: impl IntoIterator<Item = &'a str>,
) -> i32 {
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

pub(crate) fn responsive_toolbar_sort_width(toolbar_width: i32, preferred_width: i32) -> i32 {
    (toolbar_width / LIBRARY_TOOLBAR_SORT_WIDTH_SHARE)
        .max(LIBRARY_TOOLBAR_SORT_MIN_WIDTH)
        .min(preferred_width.max(LIBRARY_TOOLBAR_SORT_MIN_WIDTH))
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
