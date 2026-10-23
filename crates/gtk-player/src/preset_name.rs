use adw::prelude::*;
use gtk::glib;

#[derive(Clone, Copy)]
pub(crate) enum NameAction {
    Save,
    Duplicate,
    Rename,
}

pub(crate) fn present(
    root: &impl IsA<gtk::Widget>,
    action: NameAction,
    initial_name: &str,
    accept: impl Fn(String) + 'static,
) {
    let resource = crate::ui_resource::EQUALIZER_DIALOGS_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        name_dialog: adw::Dialog, name: gtk::Entry, name_accept: gtk::Button,
        duplicate_title: gtk::Label, duplicate_label: gtk::Label,
        rename_title: gtk::Label, rename_label: gtk::Label,
    });
    match action {
        NameAction::Save => {}
        NameAction::Duplicate => {
            name_dialog.set_title(&duplicate_title.text());
            name_accept.set_label(&duplicate_label.text());
            name.set_text(initial_name);
        }
        NameAction::Rename => {
            name_dialog.set_title(&rename_title.text());
            name_accept.set_label(&rename_label.text());
            name.set_text(initial_name);
        }
    }
    name_dialog.set_focus(Some(&name));
    name_accept.set_sensitive(!name.text().trim().is_empty());
    let accept_weak = name_accept.downgrade();
    name.connect_changed(move |entry| {
        if let Some(accept) = accept_weak.upgrade() {
            accept.set_sensitive(!entry.text().trim().is_empty());
        }
    });
    let close = name_dialog.downgrade();
    name_accept.connect_clicked(move |_| {
        accept(name.text().trim().to_owned());
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });
    gtk_widgets::popup::present_light_dismiss_dialog(&name_dialog, root);
}

#[derive(Default)]
pub(crate) struct PresetIndicator {
    displayed: std::cell::RefCell<Option<(Option<gtk::glib::Object>, bool)>>,
}

impl PresetIndicator {
    // Refresh under the caller's syncing guard: expressions notify selection too.
    pub(crate) fn update(&self, row: &adw::ComboRow, modified: bool) {
        let selected = row.selected_item();
        let next = (selected.clone(), modified);
        if self.displayed.borrow().as_ref() == Some(&next) {
            return;
        }
        *self.displayed.borrow_mut() = Some(next);
        row.set_expression(Some(gtk::ClosureExpression::new::<String>(
            [] as [gtk::Expression; 0],
            gtk::glib::closure_local!(move |item: gtk::StringObject| -> String {
                let name = item.string();
                if modified && selected.as_ref() == Some(item.upcast_ref()) {
                    format!("{name} *")
                } else {
                    name.into()
                }
            }),
        )));
    }
}
