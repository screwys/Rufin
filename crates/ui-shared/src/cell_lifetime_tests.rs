use crate::sparse_model::*;
use gtk::{gio, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
#[test]
#[ignore = "requires a GTK display"]
fn repeated_list_teardown_releases_every_recycled_cell() {
    gtk::init().expect("GTK display");
    crate::register_resources().expect("compiled resources");

    for _ in 0..16 {
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        for value in 0..128_u32 {
            model.append(&glib::BoxedAnyObject::new(value));
        }
        let selection = gtk::NoSelection::new(Some(model));
        let factory = gtk::SignalListItemFactory::new();
        let live = Rc::new(RefCell::new(Vec::new()));
        let setup_count = Rc::new(Cell::new(0_usize));
        let setup_live = Rc::clone(&live);
        let setup_total = Rc::clone(&setup_count);
        factory.connect_setup(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let cell = crate::recycled_cells::RecycledArtworkCell::new(48);
            setup_live
                .borrow_mut()
                .push((cell.downgrade(), cell.artwork().downgrade()));
            setup_total.set(setup_total.get() + 1);
            item.set_child(Some(&cell));
        });
        connect_sparse_bind(&factory, |_| {});
        let teardown_count = Rc::new(Cell::new(0_usize));
        let teardown_total = Rc::clone(&teardown_count);
        factory.connect_teardown(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            assert!(item.child().is_none());
            teardown_total.set(teardown_total.get() + 1);
        });

        let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
        let window = gtk::Window::new();
        window.set_default_size(320, 480);
        window.set_child(Some(&list));
        window.present();
        while glib::MainContext::default().iteration(false) {}
        window.set_child(None::<&gtk::Widget>);
        window.close();
        drop(list);
        drop(selection);
        drop(window);
        while glib::MainContext::default().iteration(false) {}

        assert!(setup_count.get() > 0);
        assert_eq!(teardown_count.get(), setup_count.get());
        assert!(
            live.borrow()
                .iter()
                .all(|(cell, artwork)| cell.upgrade().is_none() && artwork.upgrade().is_none())
        );
    }
}
