use crate::shell::Shell;
use gtk_editors::smart_playlist::RuleValueSuggestions;
use rufin_core::playlists::SmartPlaylistChange;
use std::rc::Rc;
impl Shell {
    pub(crate) fn publish_smart_playlist_change(
        self: &Rc<Self>,
        change: SmartPlaylistChange,
        settled: Option<Rc<dyn Fn(Result<(), String>)>>,
    ) {
        self.publish_smart_playlist_change_in_folder(change, settled, None);
    }

    fn publish_smart_playlist_change_in_folder(
        self: &Rc<Self>,
        change: SmartPlaylistChange,
        settled: Option<Rc<dyn Fn(Result<(), String>)>>,
        folder: Option<library::PlaylistFolderKey>,
    ) {
        let database = std::sync::Arc::clone(&self.products.library);
        let task = self.products.runtime.spawn(async move {
            let accepted = match change {
                SmartPlaylistChange::Create { name, definition } => {
                    match database.create_smart_playlist(&name, &definition).await {
                        Ok(key) => {
                            if let Some(folder) = folder {
                                database
                                    .move_playlist_to_folder(
                                        library::PlaylistFolderMember::SmartPlaylist { key },
                                        Some(folder),
                                    )
                                    .await
                                    .map(|_| true)
                            } else {
                                Ok(true)
                            }
                        }
                        Err(error) => Err(error),
                    }
                }
                SmartPlaylistChange::Update {
                    key,
                    name,
                    definition,
                    artwork,
                } => {
                    database
                        .update_smart_playlist(
                            key,
                            &name,
                            &definition,
                            artwork.as_ref().map(|change| match change {
                                sources::ArtworkChange::Replace(image) => {
                                    Some(image.bytes.as_slice())
                                }
                                sources::ArtworkChange::Remove => None,
                            }),
                        )
                        .await
                }
                SmartPlaylistChange::Delete(key) => database.delete_smart_playlist(key).await,
            };
            accepted
                .map_err(|error| error.to_string())
                .and_then(|accepted| {
                    accepted
                        .then_some(())
                        .ok_or_else(|| "Smart Playlist is no longer current".to_string())
                })
        });
        let shell = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let Some(shell) = shell.upgrade() else { return };
            if result.is_ok() {
                rufin_core::playlists::playlist_folders_changed(&shell.products.source).await;
            }
            if let Err(error) = result.as_ref() {
                shell
                    .control_feedback
                    .show_control_feedback_toast(error.clone());
            }
            if let Some(settled) = settled.as_ref() {
                settled(result);
            }
        });
    }
}
impl Shell {
    pub(crate) fn new_smart_playlist_dialog(self: &Rc<Self>) {
        self.load_smart_playlist_suggestions(None);
    }

    pub(crate) fn edit_smart_playlist_dialog(self: &Rc<Self>, playlist: library::SmartPlaylistRow) {
        self.load_smart_playlist_suggestions(Some(playlist));
    }

    fn load_smart_playlist_suggestions(
        self: &Rc<Self>,
        playlist: Option<library::SmartPlaylistRow>,
    ) {
        let Some(selected) = self.selected_library().as_deref().cloned() else {
            self.present_smart_playlist_dialog(playlist, RuleValueSuggestions::default());
            return;
        };
        let database = std::sync::Arc::clone(&self.products.library);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let task = self.products.runtime.spawn(async move {
            database
                .smart_playlist_value_suggestions(source, folder, &library::ReadCancellation::new())
                .await
        });
        let shell = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            let suggestions = task
                .await
                .ok()
                .and_then(Result::ok)
                .map(|values| RuleValueSuggestions {
                    genres: values.genres,
                    moods: values.moods,
                })
                .unwrap_or_default();
            shell.present_smart_playlist_dialog(playlist, suggestions);
        });
    }

    fn present_smart_playlist_dialog(
        self: &Rc<Self>,
        playlist: Option<library::SmartPlaylistRow>,
        suggestions: RuleValueSuggestions,
    ) {
        let folder = if playlist.is_none() {
            match self.navigation.routes.borrow().current() {
                gtk_widgets::route::Route::PlaylistFolder { key, smart: true } => Some(*key),
                _ => None,
            }
        } else {
            None
        };
        let shell = Rc::downgrade(self);
        let dialog = gtk_editors::smart_playlist::build_dialog(
            playlist,
            suggestions,
            self.source.configured.borrow().sources.clone().into(),
            Rc::new(move |change, settled| {
                if let Some(shell) = shell.upgrade() {
                    shell.publish_smart_playlist_change_in_folder(change, settled, folder);
                }
            }),
            &self.products.source,
            &self.artwork,
        );
        gtk_widgets::popup::present_light_dismiss_dialog(&dialog, &self.chrome.window);
    }
}
