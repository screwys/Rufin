use std::cell::Cell;

use adw::prelude::*;

use crate::layout::{AllocationOwner, width_allocation_owner};

pub(crate) fn style_compact_field_row(row: &impl IsA<gtk::Widget>) {
    row.add_css_class("compact-field-row");
}

pub(crate) fn compact_field_row_group(row: &impl IsA<gtk::Widget>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_hexpand(true);
    group.add(row);
    group
}

pub(crate) fn install_compact_field_row_responsiveness_at(
    fields: &gtk::Box,
    stack_width: i32,
) -> AllocationOwner {
    let resize_fields = fields.clone();
    let horizontal_spacing = fields.spacing();
    let stacked = Cell::new(false);
    width_allocation_owner(fields, move |width| {
        let stack = width < stack_width;
        if stacked.replace(stack) != stack {
            apply_compact_field_row_layout(&resize_fields, stack, horizontal_spacing);
        }
    })
}

fn apply_compact_field_row_layout(fields: &gtk::Box, stack: bool, horizontal_spacing: i32) {
    fields.set_orientation(if stack {
        gtk::Orientation::Vertical
    } else {
        gtk::Orientation::Horizontal
    });
    fields.set_homogeneous(!stack);
    fields.set_spacing(if stack { 8 } else { horizontal_spacing });
}
