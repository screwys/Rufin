use std::rc::Rc;

use crate::SidebarPin;
use crate::preferences::source::configure_ownership_toggle;
use crate::shell::Shell;
use adw::prelude::*;
use library::PlaylistKey;

impl Shell {
    pub(crate) fn remove_media_from_playlist(
        self: &Rc<Self>,
        playlist: PlaylistKey,
        entries: Vec<library::PlaylistEntryKey>,
    ) {
        self.products
            .source
            .remove_playlist_entries(playlist, entries);
    }

    pub(crate) fn move_media_in_playlist(
        self: &Rc<Self>,
        playlist: PlaylistKey,
        entry: library::PlaylistEntryKey,
        position: usize,
    ) {
        self.products
            .source
            .move_playlist_entry(playlist, entry, position);
    }

    pub(crate) fn add_media_to_playlist(
        self: &Rc<Self>,
        playlist: PlaylistKey,
        media_uris: Vec<String>,
        skip_existing: bool,
        settled: Rc<dyn Fn(usize)>,
    ) {
        let result = self
            .products
            .source
            .add_playlist_tracks(playlist, media_uris, skip_existing);
        gtk::glib::spawn_future_local(async move {
            let accepted = result.recv().await.ok().and_then(Result::ok).unwrap_or(0);
            settled(accepted);
        });
    }

    pub(crate) fn create_playlist_and_pin(
        self: &Rc<Self>,
        name: String,
        media_uris: Vec<String>,
        source_id: Option<library::SourceId>,
    ) {
        let result = self
            .products
            .source
            .create_playlist(source_id.clone(), name, media_uris);
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
        self.new_playlist_dialog_with(String::new(), Vec::new());
    }

    pub(crate) fn new_playlist_dialog_with(self: &Rc<Self>, name: String, media_uris: Vec<String>) {
        let resource = crate::ui_resource::PLAYLIST_NAME_DIALOG_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        crate::ui_resource::objects!(builder, resource, {
            new_dialog: adw::AlertDialog,
            new_entry: gtk::Entry,
            new_owner: gtk::ToggleButton,
        });
        let configured = self.source.configured.borrow();
        let selected = configured
            .selected_source_id
            .as_ref()
            .and_then(|selected| {
                configured
                    .sources
                    .iter()
                    .find(|source| &source.id == selected)
            })
            .cloned();
        drop(configured);
        new_entry.set_text(&name);
        configure_ownership_toggle(
            &new_owner,
            selected.as_ref(),
            self.settings.current.borrow().new_playlist_current,
        );
        let shell = Rc::downgrade(self);
        new_dialog.connect_response(None, move |_, response| {
            if response == "create" {
                let name = new_entry.text().trim().to_string();
                if !name.is_empty()
                    && let Some(shell) = shell.upgrade()
                {
                    let current = new_owner.is_active();
                    shell.set_app_setting("playlist destination", current, |settings| {
                        &mut settings.new_playlist_current
                    });
                    let source_id = selected
                        .as_ref()
                        .filter(|_| current)
                        .map(|source| source.id.clone());
                    shell.create_playlist_and_pin(name, media_uris.clone(), source_id);
                }
            }
        });
        self.present_selected_dialog(&new_dialog);
    }
}
