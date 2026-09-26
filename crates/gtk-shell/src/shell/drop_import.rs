use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};
use gtk_widgets::route::Route;
use localization::tr_with;

use super::Shell;

pub(super) fn install(shell: &Rc<Shell>) {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    target.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(shell);
    target.connect_drop(move |_, value, _, _| {
        let Ok(files) = value.get::<gdk::FileList>() else {
            return false;
        };
        let Some(shell) = weak.upgrade() else {
            return false;
        };
        let paths = files
            .files()
            .iter()
            .filter_map(|file| file.path())
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return false;
        }
        glib::spawn_future_local(import(shell, paths));
        true
    });
    shell.chrome.window.add_controller(target);
}

async fn import(shell: Rc<Shell>, paths: Vec<PathBuf>) {
    let resource = crate::ui_resource::DROP_IMPORT_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        dialog: adw::AlertDialog, playlist: gtk::ToggleButton, library: gtk::ToggleButton,
        name: adw::EntryRow, name_group: gtk::ListBox,
        playlist_description: gtk::Label, library_description: gtk::Label,
    });
    dialog.set_body(&tr_with(
        "Selected items: {count}",
        &[("count", &paths.len().to_string())],
    ));
    name.set_text(
        &paths[0]
            .file_name()
            .unwrap_or(paths[0].as_os_str())
            .to_string_lossy(),
    );
    dialog.set_response_enabled("add", !name.text().trim().is_empty());
    playlist
        .bind_property("active", &name_group, "visible")
        .sync_create()
        .build();
    playlist
        .bind_property("active", &playlist_description, "visible")
        .sync_create()
        .build();
    library
        .bind_property("active", &library_description, "visible")
        .sync_create()
        .build();
    let weak_dialog = dialog.downgrade();
    let weak_name = name.downgrade();
    playlist.connect_toggled(move |choice| {
        if let (Some(dialog), Some(name)) = (weak_dialog.upgrade(), weak_name.upgrade()) {
            dialog
                .set_response_enabled("add", !choice.is_active() || !name.text().trim().is_empty());
        }
    });
    let weak_dialog = dialog.downgrade();
    let weak_playlist = playlist.downgrade();
    name.connect_changed(move |name| {
        if let (Some(dialog), Some(playlist)) = (weak_dialog.upgrade(), weak_playlist.upgrade()) {
            dialog.set_response_enabled(
                "add",
                !playlist.is_active() || !name.text().trim().is_empty(),
            );
        }
    });
    gtk_widgets::popup::install_light_dismiss(&dialog);
    if dialog.choose_future(Some(&shell.chrome.window)).await != "add" {
        return;
    }
    let result = if playlist.is_active() {
        rufin_core::playlists::import_local_paths(&shell.products.source, paths, name.text().into())
            .recv()
            .await
            .map(|result| result.map(Some))
    } else {
        shell
            .products
            .source
            .add_local_paths(paths)
            .recv()
            .await
            .map(|result| result.map(|()| None))
    };
    match result {
        Ok(Ok(Some(playlist))) => shell.navigate(Route::PlaylistDetail(playlist)),
        Ok(Err(error)) => shell.control_feedback.show_feedback_toast(error),
        Ok(Ok(None)) | Err(_) => {}
    }
}
