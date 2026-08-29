use std::rc::Rc;

use crate::SidebarPin;
use crate::shell::Shell;
use adw::prelude::*;
use library::TrackKey;
use localization::tr;

impl Shell {
    pub(crate) fn create_playlist_and_pin(self: &Rc<Self>, name: String, tracks: Vec<TrackKey>) {
        let Some(operations) = self.selected_source_operations() else {
            return;
        };
        let Some(source_id) = self
            .selected_library()
            .as_deref()
            .map(|selected| selected.artwork.source_id.clone())
        else {
            return;
        };
        let result = operations.create_playlist(name, tracks);
        let shell = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let Ok(Ok(Some(playlist_id))) = result.recv().await else {
                return;
            };
            if let Some(shell) = shell.upgrade() {
                shell.set_sidebar_pin(
                    SidebarPin::Playlist {
                        source_id,
                        playlist_id,
                    },
                    true,
                );
            }
        });
    }

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
        let shell = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if response == "create" {
                let name = entry.text().trim().to_string();
                if !name.is_empty()
                    && let Some(shell) = shell.upgrade()
                {
                    shell.create_playlist_and_pin(name, Vec::new());
                }
            }
        });
        self.present_selected_dialog(&dialog);
    }
}
