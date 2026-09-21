use std::rc::Rc;

use crate::SidebarPin;
use crate::shell::Shell;

impl Shell {
    pub(crate) fn create_playlist_and_pin(
        self: &Rc<Self>,
        name: String,
        media_uris: Vec<String>,
        source_id: Option<library::SourceId>,
        public: Option<bool>,
    ) {
        let result = rufin_core::playlists::create_playlist(
            &self.products.source,
            source_id.clone(),
            name,
            media_uris,
            public,
        );
        let shell = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let result = result.recv().await;
            if let Some(shell) = shell.upgrade() {
                let playlist_id = match result {
                    Ok(Ok(Some(id))) => id,
                    Ok(Err(error)) => {
                        shell.control_feedback.show_feedback_toast(error);
                        return;
                    }
                    _ => return,
                };
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
        self.new_playlist_dialog_with(String::new(), Vec::new());
    }

    pub(crate) fn new_playlist_dialog_with(self: &Rc<Self>, name: String, media_uris: Vec<String>) {
        let configured = self.source.configured.borrow();
        let selected = configured
            .selected_source_id
            .as_ref()
            .and_then(|id| configured.sources.iter().find(|source| &source.id == id))
            .cloned();
        drop(configured);
        let shell = Rc::downgrade(self);
        let dialog = ui_shared::playlists::new_playlist_dialog(
            &name,
            selected,
            move |name, source_id, public| {
                if let Some(shell) = shell.upgrade() {
                    shell.create_playlist_and_pin(name, media_uris.clone(), source_id, public);
                }
            },
        );
        self.present_selected_dialog(&dialog);
    }
}
