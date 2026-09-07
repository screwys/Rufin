use std::rc::Rc;

use adw::prelude::*;

use crate::interactions::install_context_menu_openers;
use crate::localization::localized_column;
use crate::routes::collection_context::present_track_context_menu;
use crate::shell::Shell;

use super::detail_links::DetailLinks;
use super::library_fields::object_item;
use super::recycled_cells::{RecycledTextCell, list_cell};
use super::sparse_model::connect_sparse_bind;

pub(crate) fn mapped_track_link_column<T, TrackValue, Value>(
    shell: &Rc<Shell>,
    title: &'static str,
    width: i32,
    track_value: TrackValue,
    value: Value,
) -> gtk::ColumnViewColumn
where
    T: Clone + 'static,
    TrackValue: Fn(&T) -> Option<String> + 'static,
    Value: Fn(&T) -> DetailLinks + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let value = Rc::new(value);
    let shell = Rc::clone(shell);
    let track_value: Rc<dyn Fn(&T) -> Option<String>> = Rc::new(track_value);

    let setup_shell = Rc::clone(&shell);
    let setup_track_value = Rc::clone(&track_value);
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
        let item_track_value = Rc::clone(&setup_track_value);
        install_context_menu_openers(
            &cell,
            Rc::new(move |target, position| {
                let (Some(item), Some(shell)) = (weak_item.upgrade(), weak_shell.upgrade()) else {
                    return;
                };
                if let Some(track) = item
                    .item()
                    .and_then(|object| object_item::<T, _>(object, |value| item_track_value(value)))
                    .flatten()
                {
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
        let Some(links) = list_item
            .item()
            .and_then(|object| object_item::<T, _>(object, |row| value(row)))
        else {
            cell.clear();
            return;
        };
        cell.bind_links(links);
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
