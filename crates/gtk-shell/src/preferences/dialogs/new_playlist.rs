use std::rc::Rc;

use crate::shell::Shell;

impl Shell {
    pub(crate) fn create_playlist_and_pin(
        self: &Rc<Self>,
        name: String,
        selection: library::MemberSelection,
        source_id: Option<library::SourceId>,
        public: Option<bool>,
    ) {
        let source = self.products.source.clone();
        let shell = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let result = rufin_core::playlists::create_playlist(
                &source,
                source_id,
                name,
                selection.into(),
                public,
                None,
            )
            .await;
            let error = match result {
                Ok(created) => created.and_then(|created| created.settings_error),
                Err(error) => Some(error.to_string()),
            };
            if let Some(shell) = shell.upgrade()
                && let Some(error) = error
            {
                shell.control_feedback.show_feedback_toast(error);
            }
        });
    }

    pub(crate) fn new_playlist_dialog(self: &Rc<Self>) {
        self.new_playlist_dialog_with(
            String::new(),
            library::MemberSelection::Keys {
                source: None,
                keys: (Vec::new())
                    .into_iter()
                    .map(library::TrackKey::from_raw)
                    .collect::<Vec<_>>()
                    .into(),
            },
        );
    }

    pub(crate) fn new_playlist_dialog_with(
        self: &Rc<Self>,
        name: String,
        selection: library::MemberSelection,
    ) {
        let configured = self.source.configured.borrow();
        let selected = configured
            .selected_source_id
            .as_ref()
            .and_then(|id| configured.sources.iter().find(|source| &source.id == id))
            .cloned();
        drop(configured);
        let shell = Rc::downgrade(self);
        let dialog = gtk_widgets::playlists::new_playlist_dialog(
            &name,
            selected,
            move |name, source_id, public| {
                if let Some(shell) = shell.upgrade() {
                    shell.create_playlist_and_pin(name, selection.clone(), source_id, public);
                }
            },
        );
        self.present_selected_dialog(&dialog);
    }
}
