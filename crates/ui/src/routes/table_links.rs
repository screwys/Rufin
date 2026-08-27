use std::{cell::RefCell, rc::Rc};

use ::library::TrackRow;
use adw::prelude::*;

use crate::localization::localized_column;
use crate::routes::collection_context::install_dynamic_track_context_menu;
use crate::shell::Shell;

use super::detail_links::{DetailLinkBinding, DetailLinks};
use super::factory_cells::FactoryCells;
use super::library_fields::item_at_from_item;
use super::sparse_model::connect_sparse_bind;

#[derive(Clone)]
pub(crate) struct TrackLinkCell {
    links: DetailLinkBinding,
    current_track: Rc<RefCell<Option<TrackRow>>>,
}

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
    let cells = FactoryCells::new();
    let value = Rc::new(value);
    let shell = Rc::clone(shell);

    let setup_shell = Rc::clone(&shell);
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, list_item| {
        let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let current_track = Rc::new(RefCell::new(None::<TrackRow>));

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.set_valign(gtk::Align::Center);
        root.set_halign(gtk::Align::Fill);
        root.set_hexpand(true);

        let label = gtk::Label::new(None);
        label.add_css_class("table-link-label");
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_halign(gtk::Align::Start);
        label.set_hexpand(false);
        label.set_width_chars(1);
        let links = DetailLinkBinding::new(&label, &setup_shell);
        root.append(&label);

        install_dynamic_track_context_menu(&root, &setup_shell, Rc::clone(&current_track));
        list_item.set_child(Some(&root));
        setup_cells.insert(
            list_item,
            TrackLinkCell {
                links,
                current_track,
            },
        );
    });

    let bind_cells = cells.clone();
    connect_sparse_bind(&factory, move |list_item| {
        let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = bind_cells.get(list_item) else {
            return;
        };
        let Some(track) = item_at_from_item::<TrackRow>(list_item) else {
            cell.links.clear();
            *cell.current_track.borrow_mut() = None;
            return;
        };
        let links = value(&track);
        *cell.current_track.borrow_mut() = Some(track);
        cell.links.bind(links);
    });

    let unbind_cells = cells.clone();
    factory.connect_unbind(move |_, list_item| {
        if let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = unbind_cells.get(list_item)
        {
            cell.links.clear();
            *cell.current_track.borrow_mut() = None;
        }
    });

    factory.connect_teardown(move |_, list_item| {
        if let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() {
            cells.remove(list_item);
        }
    });

    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}
