use crate::source_labels::configure_ownership_toggle;
use adw::prelude::*;
use library::PlaylistKey;
use rufin_core::runtime::source::SourceSummary;
use std::rc::Rc;

pub fn delete_playlist_dialog(name: &str, delete: impl Fn() + 'static) -> adw::AlertDialog {
    let resource = crate::ui_resource::PLAYLIST_DELETE_DIALOG_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, { dialog: adw::AlertDialog });
    dialog.set_body(&dialog.body().replace("{name}", name));
    dialog.connect_response(Some("delete"), move |_, _| delete());
    dialog
}

pub fn add_media_to_playlist(
    source: &rufin_core::source::SourceOwner,
    playlist: PlaylistKey,
    media_uris: Vec<String>,
    skip_existing: bool,
    settled: Rc<dyn Fn(usize)>,
) {
    let result =
        rufin_core::playlists::add_playlist_tracks(source, playlist, media_uris, skip_existing);
    gtk::glib::spawn_future_local(async move {
        let accepted = result.recv().await.ok().and_then(Result::ok).unwrap_or(0);
        settled(accepted);
    });
}
pub fn new_playlist_dialog(
    name: &str,
    selected: Option<SourceSummary>,
    current: bool,
    create: impl Fn(String, bool, Option<library::SourceId>) + 'static,
) -> adw::AlertDialog {
    let resource = crate::ui_resource::PLAYLIST_NAME_DIALOG_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, { new_dialog: adw::AlertDialog, new_entry: gtk::Entry, new_owner: gtk::ToggleButton });
    new_entry.set_text(name);
    configure_ownership_toggle(&new_owner, selected.as_ref(), current);
    new_dialog.connect_response(None, move |_, response| {
        if response == "create" {
            let name = new_entry.text().trim().to_string();
            if !name.is_empty() {
                let current = new_owner.is_active();
                let source_id = selected
                    .as_ref()
                    .filter(|_| current)
                    .map(|source| source.id.clone());
                create(name, current, source_id);
            }
        }
    });
    new_dialog
}

pub fn rename_playlist_dialog(
    current_name: String,
    rename: impl Fn(String) + 'static,
) -> adw::AlertDialog {
    let resource = crate::ui_resource::PLAYLIST_NAME_DIALOG_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        rename_dialog: adw::AlertDialog,
        rename_entry: gtk::Entry,
    });
    rename_entry.set_text(&current_name);
    rename_entry.select_region(0, -1);
    rename_dialog.connect_response(None, move |_, response| {
        if response == "rename" {
            let name = rename_entry.text().trim().to_string();
            if !name.is_empty() {
                rename(name);
            }
        }
    });
    rename_dialog
}
