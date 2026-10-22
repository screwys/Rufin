use gtk::prelude::*;
use std::{cell::RefCell, rc::Rc};

pub fn copy_action(button: &gtk::Widget, icon: &gtk::Image) -> impl Fn(&str) + use<> {
    let reset = Rc::new(RefCell::new(None::<gtk::glib::SourceId>));
    let pending = reset.clone();
    let image = icon.downgrade();
    button.connect_unrealize(move |_| {
        if let Some(source) = pending.borrow_mut().take() {
            source.remove();
        }
        if let Some(image) = image.upgrade() {
            image.set_icon_name(Some("rufin-edit-copy-symbolic"));
        }
    });
    let button = button.downgrade();
    let icon = icon.downgrade();
    move |text| {
        let Some(button) = button.upgrade() else {
            return;
        };
        button.clipboard().set_text(text);
        if let Some(source) = reset.borrow_mut().take() {
            source.remove();
        }
        if let Some(icon) = icon.upgrade() {
            icon.set_icon_name(Some("rufin-object-select-symbolic"));
        }
        let icon = icon.clone();
        let pending = reset.clone();
        reset.replace(Some(gtk::glib::timeout_add_local_once(
            std::time::Duration::from_millis(1500),
            move || {
                pending.borrow_mut().take();
                if let Some(icon) = icon.upgrade() {
                    icon.set_icon_name(Some("rufin-edit-copy-symbolic"));
                }
            },
        )));
    }
}
