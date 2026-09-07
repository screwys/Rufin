use std::rc::Rc;

use crate::shell::Shell;
use ::library::PlaylistKey;

impl Shell {
    pub(crate) fn rename_playlist_dialog(
        self: &Rc<Self>,
        playlist_id: PlaylistKey,
        current_name: String,
    ) {
        let source = self.products.source.clone();
        let dialog = ui_shared::playlists::rename_playlist_dialog(current_name, move |name| {
            rufin_core::playlists::rename_playlist(&source, playlist_id, name);
        });
        self.present_selected_dialog(&dialog);
    }
}
