use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use gtk::glib;

type Rows = Rc<RefCell<Vec<glib::WeakRef<adw::EntryRow>>>>;

pub(crate) fn editor(
    window: &gtk::ApplicationWindow,
    paths: &[String],
    local: bool,
    changed: Rc<dyn Fn(Vec<String>)>,
) -> (adw::PreferencesGroup, gtk::Button, gtk::Label) {
    let resource = crate::ui_resource::EXCLUDED_FOLDERS_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        group: adw::PreferencesGroup, add: gtk::Button, save: gtk::Button, status: gtk::Label,
    });
    let rows = Rc::new(RefCell::new(Vec::new()));
    for path in paths {
        add_row(&group, window, local, path, &rows, &changed);
    }
    let weak_group = group.downgrade();
    let window = window.downgrade();
    add.connect_clicked(move |_| {
        if let (Some(group), Some(window)) = (weak_group.upgrade(), window.upgrade()) {
            add_row(&group, &window, local, "", &rows, &changed).grab_focus();
        }
    });
    (group, save, status)
}

fn values(rows: &Rows) -> Vec<String> {
    rows.borrow()
        .iter()
        .filter_map(glib::WeakRef::upgrade)
        .map(|row| row.text().to_string())
        .filter(|path| !path.is_empty())
        .collect()
}

fn add_row(
    group: &adw::PreferencesGroup,
    window: &gtk::ApplicationWindow,
    local: bool,
    value: &str,
    rows: &Rows,
    changed: &Rc<dyn Fn(Vec<String>)>,
) -> adw::EntryRow {
    let resource = crate::ui_resource::EXCLUDED_FOLDERS_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        path: adw::EntryRow, browse: gtk::Button, remove: gtk::Button, chooser: gtk::FileDialog,
    });
    path.set_text(value);
    browse.set_visible(local);
    rows.borrow_mut().push(path.downgrade());
    let current_rows = Rc::clone(rows);
    let update = Rc::clone(changed);
    path.connect_text_notify(move |_| update(values(&current_rows)));
    let weak_group = group.downgrade();
    let weak_path = path.downgrade();
    let current_rows = Rc::clone(rows);
    let update = Rc::clone(changed);
    remove.connect_clicked(move |_| {
        if let (Some(group), Some(path)) = (weak_group.upgrade(), weak_path.upgrade()) {
            current_rows
                .borrow_mut()
                .retain(|row| row.upgrade().is_some_and(|row| row != path));
            group.remove(&path);
            update(values(&current_rows));
        }
    });
    let window = window.downgrade();
    let weak_path = path.downgrade();
    browse.connect_clicked(move |_| {
        let (Some(window), Some(path)) = (window.upgrade(), weak_path.upgrade()) else {
            return;
        };
        let text = path.text();
        if std::path::Path::new(text.as_str()).is_absolute() {
            chooser.set_initial_folder(Some(&gtk::gio::File::for_path(text.as_str())));
        }
        let chooser = chooser.clone();
        let path = path.downgrade();
        glib::spawn_future_local(async move {
            if let Ok(folder) = chooser.select_folder_future(Some(&window)).await
                && let Some(folder) = folder.path()
                && let Some(path) = path.upgrade()
            {
                path.set_text(&folder.to_string_lossy());
            }
        });
    });
    group.add(&path);
    path
}
