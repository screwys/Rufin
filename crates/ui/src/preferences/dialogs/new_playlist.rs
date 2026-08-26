use std::rc::Rc;

use crate::shell::Shell;
use adw::prelude::*;
use localization::tr;

impl Shell {
    pub(crate) fn new_playlist_dialog(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::builder()
            .heading(tr("New Playlist"))
            .build();
        dialog.add_response("cancel", &tr("Cancel"));
        dialog.add_response("create", &tr("Create"));
        dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some(&tr("Playlist name")));
        dialog.set_extra_child(Some(&entry));
        let source = self.selected_source_operations();
        dialog.connect_response(None, move |_, response| {
            if response == "create" {
                let name = entry.text().trim().to_string();
                if !name.is_empty()
                    && let Some(source) = source.as_ref()
                {
                    source.create_playlist(name, Vec::new());
                }
            }
        });
        self.present_selected_dialog(&dialog);
    }
}
