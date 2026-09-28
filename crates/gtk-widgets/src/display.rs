use adw::prelude::*;
use rufin_core::settings::layout::{DisplaySettings, DisplaySize, LibraryLayout};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[derive(Clone)]
pub struct DisplayEditor {
    pub group: adw::PreferencesGroup,
    size_row: adw::ActionRow,
    size: adw::ToggleGroup,
    spacing: adw::ToggleGroup,
    spacing_row: adw::ActionRow,
    switches: [adw::SwitchRow; 5],
    value: Rc<RefCell<DisplaySettings>>,
    syncing: Rc<Cell<bool>>,
}

impl DisplayEditor {
    pub fn prepend_rows(&self, rows: &[&adw::SwitchRow]) {
        self.group.remove(&self.size_row);
        self.group.remove(&self.spacing_row);
        for row in &self.switches {
            self.group.remove(row);
        }
        for row in rows {
            self.group.add(*row);
        }
        self.group.add(&self.size_row);
        self.group.add(&self.spacing_row);
        for row in &self.switches {
            self.group.add(row);
        }
    }

    pub fn new(value: &DisplaySettings, changed: impl Fn(&DisplaySettings) + 'static) -> Self {
        let resource = crate::ui_resource::DISPLAY_SETTINGS_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            display_group: adw::PreferencesGroup,
            size_row: adw::ActionRow,
            size: adw::ToggleGroup,
            spacing: adw::ToggleGroup,
            spacing_row: adw::ActionRow,
            show_header: adw::SwitchRow,
            hover_highlight: adw::SwitchRow,
            alternate_rows: adw::SwitchRow,
            row_borders: adw::SwitchRow,
            column_borders: adw::SwitchRow,
        });
        let editor = Self {
            group: display_group,
            size_row,
            size,
            spacing,
            spacing_row,
            switches: [
                show_header,
                hover_highlight,
                alternate_rows,
                row_borders,
                column_borders,
            ],
            value: Rc::new(RefCell::new(value.clone())),
            syncing: Rc::new(Cell::new(false)),
        };
        editor.sync(value);
        let changed = Rc::new(changed);
        for (index, toggle) in [&editor.size, &editor.spacing].into_iter().enumerate() {
            let value = editor.value.clone();
            let syncing = editor.syncing.clone();
            let changed = changed.clone();
            toggle.connect_active_notify(move |toggle| {
                if syncing.get() {
                    return;
                }
                let next = match toggle.active() {
                    0 => DisplaySize::Compact,
                    2 => DisplaySize::Large,
                    _ => DisplaySize::Default,
                };
                let mut settings = value.borrow().clone();
                if index == 0 {
                    settings.size = next;
                } else {
                    settings.grid_spacing = next;
                }
                value.replace(settings.clone());
                changed(&settings);
            });
        }
        for (index, row) in editor.switches.iter().enumerate() {
            let value = editor.value.clone();
            let syncing = editor.syncing.clone();
            let changed = changed.clone();
            row.connect_active_notify(move |row| {
                if syncing.get() {
                    return;
                }
                let mut settings = value.borrow().clone();
                let field = match index {
                    0 => &mut settings.show_header,
                    1 => &mut settings.hover_highlight,
                    2 => &mut settings.alternate_rows,
                    3 => &mut settings.row_borders,
                    _ => &mut settings.column_borders,
                };
                *field = row.is_active();
                value.replace(settings.clone());
                changed(&settings);
            });
        }
        editor
    }

    pub fn set_layout(&self, layout: Option<LibraryLayout>) {
        self.spacing_row
            .set_visible(layout == Some(LibraryLayout::Grid));
        for row in &self.switches {
            row.set_visible(matches!(
                layout,
                Some(LibraryLayout::Row | LibraryLayout::Detail)
            ));
        }
    }

    pub fn sync(&self, settings: &DisplaySettings) {
        self.syncing.set(true);
        let index = |size| match size {
            DisplaySize::Compact => 0,
            DisplaySize::Default => 1,
            DisplaySize::Large => 2,
        };
        self.size.set_active(index(settings.size));
        self.spacing.set_active(index(settings.grid_spacing));
        for (row, active) in self.switches.iter().zip([
            settings.show_header,
            settings.hover_highlight,
            settings.alternate_rows,
            settings.row_borders,
            settings.column_borders,
        ]) {
            row.set_active(active);
        }
        self.value.replace(settings.clone());
        self.syncing.set(false);
    }
}

pub fn apply(widget: &impl IsA<gtk::Widget>, settings: &DisplaySettings) {
    for (class, active) in [
        ("display-compact", settings.size == DisplaySize::Compact),
        ("display-large", settings.size == DisplaySize::Large),
        ("display-no-hover", !settings.hover_highlight),
        ("display-striped", settings.alternate_rows),
        ("display-row-borders", settings.row_borders),
        ("display-column-borders", settings.column_borders),
    ] {
        if active {
            widget.add_css_class(class);
        } else {
            widget.remove_css_class(class);
        }
    }
    if let Some(table) = widget.as_ref().downcast_ref::<gtk::ColumnView>() {
        table.set_show_row_separators(settings.row_borders);
        table.set_show_column_separators(settings.column_borders);
        // GtkColumnView exposes its header as its first child, without a visibility property.
        if let Some(header) = table.first_child() {
            header.set_visible(settings.show_header);
        }
    }
}
