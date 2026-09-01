use std::rc::Rc;

use crate::SidebarPin;
use crate::shell::Shell;
use adw::prelude::*;
use library::TrackKey;

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
        let resource = crate::ui_resource::PLAYLIST_NAME_DIALOG_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        crate::ui_resource::objects!(builder, resource, {
            new_dialog: adw::AlertDialog,
            new_entry: gtk::Entry,
        });
        let shell = Rc::downgrade(self);
        new_dialog.connect_response(None, move |_, response| {
            if response == "create" {
                let name = new_entry.text().trim().to_string();
                if !name.is_empty()
                    && let Some(shell) = shell.upgrade()
                {
                    shell.create_playlist_and_pin(name, Vec::new());
                }
            }
        });
        self.present_selected_dialog(&new_dialog);
    }
}
