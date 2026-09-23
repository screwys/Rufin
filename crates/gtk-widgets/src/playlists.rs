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
    create: impl Fn(String, Option<library::SourceId>, Option<bool>) + 'static,
) -> adw::Dialog {
    let resource = crate::ui_resource::PLAYLIST_NAME_DIALOG_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        new_dialog: adw::Dialog, new_entry: gtk::Entry, new_owner: gtk::ToggleButton,
        new_public_options: gtk::Box, new_public: gtk::Switch,
        new_cancel: gtk::Button, new_create: gtk::Button,
    });
    new_entry.set_text(name);
    configure_ownership_toggle(&new_owner, selected.as_ref(), true);
    let supports_public = selected
        .as_ref()
        .is_some_and(|source| source.supports_playlist_public);
    new_public_options.set_visible(new_owner.is_active() && supports_public);
    let options = new_public_options.downgrade();
    new_owner.connect_toggled(move |owner| {
        if let Some(options) = options.upgrade() {
            options.set_visible(owner.is_active() && supports_public);
        }
    });
    let dialog = new_dialog.downgrade();
    new_cancel.connect_clicked(move |_| {
        if let Some(dialog) = dialog.upgrade() {
            dialog.close();
        }
    });
    let dialog = new_dialog.downgrade();
    new_create.connect_clicked(move |_| {
        let name = new_entry.text().trim().to_string();
        if name.is_empty() {
            return;
        }
        let current = new_owner.is_active();
        let source_id = selected
            .as_ref()
            .filter(|_| current)
            .map(|source| source.id.clone());
        let public = (current && supports_public).then_some(new_public.is_active());
        if let Some(dialog) = dialog.upgrade() {
            dialog.close();
        }
        create(name, source_id, public);
    });
    new_dialog
}
