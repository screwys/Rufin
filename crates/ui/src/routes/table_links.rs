use std::rc::Rc;

use ::library::TrackRow;
use adw::prelude::*;

use crate::interactions::install_context_menu_openers;
use crate::localization::localized_column;
use crate::routes::collection_context::present_track_context_menu;
use crate::shell::Shell;

use super::detail_links::DetailLinks;
use super::library_fields::item_at_from_item;
use super::recycled_cells::{RecycledTextCell, list_cell};
use super::sparse_model::connect_sparse_bind;

pub(crate) fn track_link_column<F>(
    shell: &Rc<Shell>,
    title: &'static str,
    width: i32,
    value: F,
) -> gtk::ColumnViewColumn
where
    F: Fn(&TrackRow) -> DetailLinks + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let value = Rc::new(value);
    let shell = Rc::clone(shell);

    let setup_shell = Rc::clone(&shell);
    factory.connect_setup(move |_, list_item| {
        let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledTextCell::new();
        cell.set_valign(gtk::Align::Center);
        let label = cell.label();
        label.add_css_class("table-link-label");
        cell.enable_links(&setup_shell);
        let weak_item = list_item.downgrade();
        let weak_shell = Rc::downgrade(&setup_shell);
        install_context_menu_openers(
            &cell,
            Rc::new(move |target, position| {
                let (Some(item), Some(shell)) = (weak_item.upgrade(), weak_shell.upgrade()) else {
                    return;
                };
                if let Some(track) = item_at_from_item::<TrackRow>(&item) {
                    present_track_context_menu(target, &shell, track, position);
                }
            }),
        );
        list_item.set_child(Some(&cell));
    });

    connect_sparse_bind(&factory, move |list_item| {
        let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledTextCell>(list_item) else {
            return;
        };
        let Some(track) = item_at_from_item::<TrackRow>(list_item) else {
            cell.clear();
            return;
        };
        cell.bind_links(value(&track));
    });

    factory.connect_unbind(move |_, list_item| {
        if let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = list_cell::<RecycledTextCell>(list_item)
        {
            cell.clear();
        }
    });

    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}
