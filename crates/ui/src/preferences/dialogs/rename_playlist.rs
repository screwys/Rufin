use std::rc::Rc;

use crate::shell::Shell;
use ::library::PlaylistKey;
use adw::prelude::*;
use localization::tr;

impl Shell {
    pub(crate) fn rename_playlist_dialog(
        self: &Rc<Self>,
        playlist_id: PlaylistKey,
        current_name: String,
    ) {
        let Some(source) = self.selected_source_operations() else {
            return;
        };
        self.rename_playlist_dialog_inner(current_name, move |name| {
            source.rename_playlist(playlist_id, name);
        });
    }

    fn rename_playlist_dialog_inner(
        self: &Rc<Self>,
        current_name: String,
        rename: impl Fn(String) + 'static,
    ) {
        let dialog = adw::AlertDialog::builder()
            .heading(tr("Rename Playlist"))
            .build();
        dialog.add_response("cancel", &tr("Cancel"));
        dialog.add_response("rename", &tr("Rename"));
        dialog.set_default_response(Some("rename"));
        dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
        let entry = gtk::Entry::new();
        entry.set_text(&current_name);
        entry.set_activates_default(true);
        dialog.set_extra_child(Some(&entry));
        dialog.connect_response(None, move |_, response| {
            if response == "rename" {
                let name = entry.text().trim().to_string();
                if !name.is_empty() {
                    rename(name);
                }
            }
        });
        self.present_selected_dialog(&dialog);
    }
}
