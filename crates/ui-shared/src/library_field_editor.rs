use crate::library_fields::*;
use crate::settings::SettingsState;
use adw::prelude::*;
use localization::tr;
use rufin_core::settings::layout::{LibraryField, LibraryListKey};
use std::{cell::RefCell, rc::Rc};
pub fn populate_library_field_rows(
    settings: &Rc<SettingsState>,
    changed: Rc<dyn Fn()>,
    key: LibraryListKey,
    group: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<adw::ActionRow>>>,
) {
    let config = settings.current.borrow().library_list(key);
    populate_library_field_rows_for_set(
        settings,
        changed,
        key,
        field_set_for_layout(config.layout),
        group,
        rows,
    );
}
pub fn populate_library_field_rows_for_set(
    settings: &Rc<SettingsState>,
    changed: Rc<dyn Fn()>,
    key: LibraryListKey,
    field_set: LibraryFieldSet,
    group: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<adw::ActionRow>>>,
) {
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }

    let config = settings.current.borrow().library_list(key);
    group.set_title(&tr(field_group_title(field_set)));

    let active = active_fields_for_set(&config, field_set).to_vec();
    let mut order = active.clone();
    for field in available_fields_for_set(key, field_set) {
        if !order.contains(field) {
            order.push(*field);
        }
    }
    for field in order {
        let row = library_field_config_row(
            settings,
            Rc::clone(&changed),
            key,
            field_set,
            field,
            &active,
            group,
            rows,
        );
        group.add(&row);
        rows.borrow_mut().push(row);
    }
}
pub fn library_field_config_row(
    settings: &Rc<SettingsState>,
    changed: Rc<dyn Fn()>,
    key: LibraryListKey,
    field_set: LibraryFieldSet,
    field: LibraryField,
    active: &[LibraryField],
    group: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<adw::ActionRow>>>,
) -> adw::ActionRow {
    let enabled = active.contains(&field);
    let row = adw::ActionRow::builder()
        .title(tr(crate::settings::library_field_title(field)))
        .subtitle(if enabled { tr("Visible") } else { tr("Hidden") })
        .build();

    let drag = gtk::Image::from_icon_name("rufin-list-drag-handle-symbolic");
    drag.add_css_class("dim-label");
    drag.set_tooltip_text(Some(&tr("Drag to reorder")));
    row.add_prefix(&drag);

    let check = gtk::CheckButton::new();
    check.set_active(enabled);
    check.set_sensitive(can_toggle_field(active, field_set, field));
    check.set_valign(gtk::Align::Center);
    row.add_prefix(&check);
    row.set_activatable_widget(Some(&check));

    let up = gtk::Button::from_icon_name("rufin-go-up-symbolic");
    up.add_css_class("flat");
    up.set_tooltip_text(Some(&tr("Move up")));
    up.set_valign(gtk::Align::Center);
    up.set_sensitive(enabled);
    row.add_suffix(&up);

    let down = gtk::Button::from_icon_name("rufin-go-down-symbolic");
    down.add_css_class("flat");
    down.set_tooltip_text(Some(&tr("Move down")));
    down.set_valign(gtk::Align::Center);
    down.set_sensitive(enabled);
    row.add_suffix(&down);

    {
        let settings = Rc::clone(settings);
        let changed = Rc::clone(&changed);
        let group = group.downgrade();
        let rows = Rc::downgrade(rows);
        check.connect_toggled(move |check| {
            let (Some(group), Some(rows)) = (group.upgrade(), rows.upgrade()) else {
                return;
            };
            if settings.update_library_list_settings(key, |settings| {
                set_field_enabled(settings, field_set, field, check.is_active());
            }) {
                changed();
            }
            populate_library_field_rows_for_set(
                &settings,
                Rc::clone(&changed),
                key,
                field_set,
                &group,
                &rows,
            );
        });
    }
    {
        let settings = Rc::clone(settings);
        let changed = Rc::clone(&changed);
        let group = group.downgrade();
        let rows = Rc::downgrade(rows);
        up.connect_clicked(move |_| {
            let (Some(group), Some(rows)) = (group.upgrade(), rows.upgrade()) else {
                return;
            };
            if settings.update_library_list_settings(key, |settings| {
                move_visible_field(settings, field_set, field, -1);
            }) {
                changed();
            }
            populate_library_field_rows_for_set(
                &settings,
                Rc::clone(&changed),
                key,
                field_set,
                &group,
                &rows,
            );
        });
    }
    {
        let settings = Rc::clone(settings);
        let changed = Rc::clone(&changed);
        let group = group.downgrade();
        let rows = Rc::downgrade(rows);
        down.connect_clicked(move |_| {
            let (Some(group), Some(rows)) = (group.upgrade(), rows.upgrade()) else {
                return;
            };
            if settings.update_library_list_settings(key, |settings| {
                move_visible_field(settings, field_set, field, 1);
            }) {
                changed();
            }
            populate_library_field_rows_for_set(
                &settings,
                Rc::clone(&changed),
                key,
                field_set,
                &group,
                &rows,
            );
        });
    }

    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .build();
    let field_position = available_fields_for_set(key, field_set)
        .iter()
        .position(|candidate| *candidate == field)
        .expect("configured field is available") as u32;
    source.connect_prepare(move |_, _, _| {
        Some(gtk::gdk::ContentProvider::for_value(
            &field_position.to_value(),
        ))
    });
    drag.add_controller(source);

    let drop_target = gtk::DropTarget::new(u32::static_type(), gtk::gdk::DragAction::MOVE);
    let settings = Rc::clone(settings);
    let changed = Rc::clone(&changed);
    let group = group.downgrade();
    let rows = Rc::downgrade(rows);
    let row_for_drop = row.downgrade();
    drop_target.connect_drop(move |_, value, _, y| {
        let Ok(source_position) = value.get::<u32>() else {
            return false;
        };
        let Some(source_field) = available_fields_for_set(key, field_set)
            .get(source_position as usize)
            .copied()
        else {
            return false;
        };
        if source_field == field {
            return false;
        }
        let Some(row) = row_for_drop.upgrade() else {
            return false;
        };
        let (Some(group), Some(rows)) = (group.upgrade(), rows.upgrade()) else {
            return false;
        };
        let after = y > f64::from(row.height()) / 2.0;
        if settings.update_library_list_settings(key, |settings| {
            reorder_visible_field(settings, field_set, source_field, field, after);
        }) {
            changed();
        }
        populate_library_field_rows_for_set(
            &settings,
            Rc::clone(&changed),
            key,
            field_set,
            &group,
            &rows,
        );
        true
    });
    row.add_controller(drop_target);

    row
}
