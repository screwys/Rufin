//! Creating a playlist, with its name, owner and visibility, and creating a folder.
use adw::prelude::*;
use rufin_core::runtime::source::SourceSummary;
use std::rc::Rc;

use crate::shell::shell::Shell;
use crate::widgets::source_labels::configure_ownership_toggle;

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
            rufin_core::route::Route::PlaylistFolder { key, smart: false } => Some(*key),
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
        let dialog = new_playlist_dialog(&name, selected, move |name, source_id, public| {
            if let Some(shell) = shell.upgrade() {
                shell.create_playlist_and_pin(name, selection.clone(), source_id, public, folder);
            }
        });
        self.present_selected_dialog(&dialog);
    }
}

impl Shell {
    pub(crate) fn new_playlist_folder_dialog(self: &Rc<Self>) {
        let smart = matches!(
            self.navigation.routes.borrow().current(),
            rufin_core::route::Route::SmartPlaylists
                | rufin_core::route::Route::PlaylistFolder { smart: true, .. }
        );
        crate::menus::playlist_folders::present_new_folder_dialog(
            self,
            smart,
            Vec::new(),
            Rc::new(|| {}),
        );
    }
}

fn new_playlist_dialog(
    name: &str,
    selected: Option<SourceSummary>,
    create: impl Fn(String, Option<library::SourceId>, Option<bool>) + 'static,
) -> adw::Dialog {
    let resource = "preferences/dialogs/playlist_name.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        new_dialog: adw::Dialog, new_entry: gtk::Entry, new_owner: gtk::ToggleButton,
        new_public_options: gtk::Box, new_public: gtk::Switch,
        new_cancel: gtk::Button, new_create: gtk::Button,
    });
    new_entry.set_text(name);
    // A playlist made for a source that cannot own one is a Rufin playlist, with no choice.
    let selected = selected.filter(|source| source.owns_playlists);
    if let Some(source) = selected.as_ref() {
        configure_ownership_toggle(&new_owner, source, true);
    } else {
        new_owner.set_visible(false);
        new_entry.remove_css_class("playlist-name-owner-entry");
    }
    let supports_public = selected
        .as_ref()
        .is_some_and(|source| source.supports_playlist_public);
    new_public_options.set_visible(new_owner.is_active() && supports_public);
    let options = new_public_options.downgrade();
    new_owner.connect_toggled(move |owner| {
        if let Some(options) = options.upgrade() {
            options.set_visible(owner.is_active() && supports_public);
        }
    });
    let dialog = new_dialog.downgrade();
    new_cancel.connect_clicked(move |_| {
        if let Some(dialog) = dialog.upgrade() {
            dialog.close();
        }
    });
    let dialog = new_dialog.downgrade();
    new_create.connect_clicked(move |_| {
        let name = new_entry.text().trim().to_string();
        if name.is_empty() {
            return;
        }
        let current = new_owner.is_active();
        let source_id = selected
            .as_ref()
            .filter(|_| current)
            .map(|source| source.id.clone());
        let public = (current && supports_public).then_some(new_public.is_active());
        if let Some(dialog) = dialog.upgrade() {
            dialog.close();
        }
        create(name, source_id, public);
    });
    new_dialog
}
