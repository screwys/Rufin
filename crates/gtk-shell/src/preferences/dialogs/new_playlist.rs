use std::rc::Rc;

use crate::shell::Shell;

impl Shell {
    pub(crate) fn create_playlist_and_pin(
        self: &Rc<Self>,
        name: String,
        selection: library::MemberSelection,
        source_id: Option<library::SourceId>,
        public: Option<bool>,
        folder: Option<library::PlaylistFolderKey>,
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
                Ok(Some(created)) => {
                    let mut error = created.settings_error;
                    if let Some(folder) = folder {
                        let result = async {
                            if let Some(key) = created.playlist_key {
                                rufin_core::playlists::move_playlist_to_folder(
                                    &source,
                                    library::PlaylistFolderMember::Playlist { key },
                                    Some(folder),
                                )
                                .await
                                .map_err(|error| error.to_string())?;
                            }
                            Ok::<_, String>(())
                        }
                        .await;
                        if let Err(folder_error) = result {
                            error = Some(folder_error);
                        }
                    }
                    error
                }
                Ok(None) => None,
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
        let folder = match self.navigation.routes.borrow().current() {
            gtk_widgets::route::Route::PlaylistFolder { key, smart: false } => Some(*key),
            _ => None,
        };
        self.new_playlist_dialog_in_folder(name, selection, folder);
    }

    pub(crate) fn new_playlist_dialog_in_folder(
        self: &Rc<Self>,
        name: String,
        selection: library::MemberSelection,
        folder: Option<library::PlaylistFolderKey>,
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
                    shell.create_playlist_and_pin(
                        name,
                        selection.clone(),
                        source_id,
                        public,
                        folder,
                    );
                }
            },
        );
        self.present_selected_dialog(&dialog);
    }
}

impl Shell {
    pub(crate) fn new_playlist_folder_dialog(self: &Rc<Self>) {
        let smart = matches!(
            self.navigation.routes.borrow().current(),
            gtk_widgets::route::Route::SmartPlaylists
                | gtk_widgets::route::Route::PlaylistFolder { smart: true, .. }
        );
        let source = self.products.source.clone();
        let shell = Rc::downgrade(self);
        let dialog = gtk_media_menus::playlist_folders::folder_name_dialog(None, move |name| {
            let source = source.clone();
            let shell = shell.clone();
            gtk::glib::spawn_future_local(async move {
                if let Err(error) =
                    rufin_core::playlists::create_playlist_folder(&source, name, smart).await
                    && let Some(shell) = shell.upgrade()
                {
                    shell
                        .control_feedback
                        .show_feedback_toast(error.to_string());
                }
            });
        });
        self.present_selected_dialog(&dialog);
    }
}
