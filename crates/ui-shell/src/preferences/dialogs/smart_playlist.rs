use crate::shell::Shell;
use std::rc::Rc;
use ui_shared::smart_playlist::{RuleValueSuggestions, SmartPlaylistChange};
impl Shell {
    pub(crate) fn publish_smart_playlist_change(
        self: &Rc<Self>,
        change: SmartPlaylistChange,
        settled: Option<Rc<dyn Fn(Result<(), String>)>>,
    ) {
        let database = std::sync::Arc::clone(&self.products.library);
        let task = self.products.runtime.spawn(async move {
            let accepted = match change {
                SmartPlaylistChange::Create { name, definition } => database
                    .create_smart_playlist(&name, &definition)
                    .await
                    .map(|_| true),
                SmartPlaylistChange::Update {
                    key,
                    name,
                    definition,
                } => {
                    database
                        .update_smart_playlist(key, &name, &definition)
                        .await
                }
                SmartPlaylistChange::Delete(key) => database.delete_smart_playlist(key).await,
                SmartPlaylistChange::Move { dragged, target } => {
                    database.move_smart_playlist(dragged, target).await
                }
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
                shell.refresh_mounted_catalog();
                crate::shell::playlist_picker::refresh_context_playlist_picker(&shell);
                crate::shell::navigation::refresh_sidebar_pins(&shell);
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
        let database = std::sync::Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let task = selected.runtime.spawn(async move {
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
        let configured = self.source.configured.borrow();
        let source = configured
            .selected_source_id
            .as_ref()
            .and_then(|id| configured.sources.iter().find(|source| &source.id == id))
            .cloned();
        drop(configured);
        let shell = Rc::downgrade(self);
        let dialog = ui_shared::smart_playlist::build_dialog(
            playlist,
            suggestions,
            source,
            Rc::new(move |change, settled| {
                if let Some(shell) = shell.upgrade() {
                    shell.publish_smart_playlist_change(change, settled);
                }
            }),
        );
        self.present_selected_dialog(&dialog);
    }
}
