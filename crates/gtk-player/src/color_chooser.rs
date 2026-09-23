use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

#[allow(deprecated)]
pub(crate) fn present_color_chooser(
    parent: &impl IsA<gtk::Window>,
    title: &str,
    initial: gtk::gdk::RGBA,
    default_color: impl Fn() -> gtk::gdk::RGBA + 'static,
    save: impl Fn(gtk::gdk::RGBA) + 'static,
) {
    let resource = crate::ui_resource::COLOR_CHOOSER_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        dialog: adw::Window,
        heading: adw::WindowTitle,
        cancel: gtk::Button,
        default: gtk::Button,
        select: gtk::Button,
        body: gtk::Box,
    });
    dialog.set_title(Some(title));
    heading.set_title(title);
    let chooser = Rc::new(RefCell::new(color_chooser(&initial)));
    body.append(&*chooser.borrow());
    dialog.set_transient_for(Some(parent));

    let close = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(close) = close.upgrade() {
            close.close();
        }
    });
    let default_chooser = Rc::clone(&chooser);
    default.connect_clicked(move |_| {
        let replacement = color_chooser(&default_color());
        body.remove(&*default_chooser.borrow());
        body.append(&replacement);
        default_chooser.replace(replacement);
    });
    let close = dialog.downgrade();
    select.connect_clicked(move |_| {
        save(chooser.borrow().rgba());
        if let Some(close) = close.upgrade() {
            close.close();
        }
    });
    dialog.present();
}

#[allow(deprecated)]
fn color_chooser(color: &gtk::gdk::RGBA) -> gtk::ColorChooserWidget {
    let chooser = gtk::ColorChooserWidget::new();
    chooser.set_show_editor(true);
    chooser.set_use_alpha(false);
    chooser.set_rgba(color);
    chooser.set_hexpand(true);
    chooser.set_vexpand(true);
    chooser
}
