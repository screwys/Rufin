use crate::editors::smart_playlist::RuleValueSuggestions;
use crate::shell::shell::Shell;
use rufin_core::playlists::SmartPlaylistChange;
use std::rc::Rc;
impl Shell {
    pub(crate) fn publish_smart_playlist_change(
        self: &Rc<Self>,
        change: SmartPlaylistChange,
        folder: Option<library::PlaylistFolderKey>,
        settled: Option<Rc<dyn Fn(Result<(), String>)>>,
    ) {
        let owner = self.products.source.clone();
        let shell = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let result = rufin_core::playlists::apply_smart_playlist_change(&owner, change, folder)
                .await
                .map_err(|error| error.to_string())
                .and_then(|applied| {
                    applied
                        .map(|_| ())
                        .ok_or_else(|| "Smart Playlist is no longer current".to_string())
                });
            let Some(shell) = shell.upgrade() else { return };
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
        let catalog = std::sync::Arc::clone(&self.products.library);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let task = self.products.runtime.spawn(async move {
            catalog
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
                rufin_core::route::Route::PlaylistFolder { key, smart: true } => Some(*key),
                _ => None,
            }
        } else {
            None
        };
        let shell = Rc::downgrade(self);
        let dialog = crate::editors::smart_playlist::build_dialog(
            playlist,
            suggestions,
            self.source.configured.borrow().sources.clone().into(),
            Rc::new(move |change, settled| {
                if let Some(shell) = shell.upgrade() {
                    shell.publish_smart_playlist_change(change, folder, settled);
                }
            }),
            &self.products.source,
            &self.artwork,
        );
        crate::widgets::popup::present_light_dismiss_dialog(&dialog, &self.chrome.window);
    }
}
