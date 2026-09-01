use std::rc::Rc;

use crate::shell::Shell;
use ::library::PlaylistKey;
use adw::prelude::*;

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
        let resource = crate::ui_resource::PLAYLIST_NAME_DIALOG_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        crate::ui_resource::objects!(builder, resource, {
            rename_dialog: adw::AlertDialog,
            rename_entry: gtk::Entry,
        });
        rename_entry.set_text(&current_name);
        rename_dialog.connect_response(None, move |_, response| {
            if response == "rename" {
                let name = rename_entry.text().trim().to_string();
                if !name.is_empty() {
                    rename(name);
                }
            }
        });
        self.present_selected_dialog(&rename_dialog);
    }
}
