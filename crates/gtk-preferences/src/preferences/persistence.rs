use crate::Preferences;
use adw::prelude::*;
use rufin_core::runtime::ScrobblingPreferences;
use rufin_core::settings::HomeBlockKind;
use secrets::SecretStorageMode;
use std::rc::Rc;
use tracing::warn;

impl Preferences {
    pub(super) fn retry_external_artwork(self: &Rc<Self>, warning_action: &'static str) {
        if let Err(error) = self.products.artwork.retry_external() {
            warn!(%error, action = warning_action, "failed to retry external artwork");
            return;
        }
        (self.effects)(crate::Effect::MediaControlsChanged);
    }

    pub fn set_private_mode(self: &Rc<Self>, enabled: bool) {
        if self
            .settings
            .set_app_setting("private mode setting", enabled, |settings| {
                &mut settings.private_mode
            })
            .is_none()
        {
            return;
        }
        (self.effects)(crate::Effect::PrivateModeChanged);
        (self.effects)(crate::Effect::CatalogChanged);
        (self.effects)(crate::Effect::MediaControlsChanged);
        let search_dialog = self.player_ui.selected_lyrics().and_then(|lyrics| {
            lyrics
                .search_dialog
                .borrow()
                .as_ref()
                .map(|dialog| dialog.dialog.clone())
        });
        if let Some(dialog) = search_dialog {
            dialog.close();
        }
        self.player_ui.render_lyrics_panel();
    }

    pub(super) async fn set_secret_storage_mode(self: &Rc<Self>, mode: SecretStorageMode) -> bool {
        match self
            .products
            .source
            .change_secret_storage(mode)
            .recv()
            .await
        {
            Ok(Ok(())) => {
                *self.settings.current.borrow_mut() = self.settings.persistence.load();
                self.products.scrobbling.storage_reset();
                true
            }
            Ok(Err(error)) => {
                warn!(%error, "failed to change secret storage mode");
                false
            }
            Err(_) => {
                warn!("secret storage operation ended before completion");
                false
            }
        }
    }

    pub(super) fn update_scrobbling_settings(
        self: &Rc<Self>,
        warning_action: &'static str,
        update: impl FnOnce(&mut ScrobblingPreferences) -> bool,
    ) {
        let mut preferences = self.products.scrobbling.preferences();
        if !update(&mut preferences) {
            return;
        }
        if let Err(error) = self.products.scrobbling.save(&preferences) {
            warn!(%error, action = warning_action, "failed to save scrobbling settings");
        }
    }

    pub(super) fn save_scrobbling_credential(
        self: &Rc<Self>,
        warning_action: &'static str,
        update: impl FnOnce(&mut ScrobblingPreferences) + Send + 'static,
    ) {
        let previous = self.products.scrobbling.preferences();
        let saved = self.products.scrobbling.save_credential(update);
        let shell = Rc::clone(self);
        gtk::glib::spawn_future_local(async move {
            match saved.recv().await {
                Ok(Ok(committed)) => {
                    shell.settings.current.borrow_mut().lastfm_api_key =
                        committed.lastfm.api_key.clone();
                    if previous.lastfm.api_key != committed.lastfm.api_key {
                        shell.retry_external_artwork(warning_action);
                    }
                }
                Ok(Err(error)) => {
                    warn!(%error, action = warning_action, "failed to save scrobbling settings")
                }
                Err(_) => {}
            }
        });
    }

    pub(super) fn set_home_blocks(self: &Rc<Self>, blocks: Vec<HomeBlockKind>) {
        let previous = self.settings.current.borrow().home_blocks.clone();
        let added = blocks.iter().any(|block| !previous.contains(block));
        if self
            .settings
            .set_app_setting("home block settings", blocks, |settings| {
                &mut settings.home_blocks
            })
            .is_none()
        {
            return;
        }
        (self.effects)(crate::Effect::CatalogChanged);
        if added {
            (self.effects)(crate::Effect::RefreshHome);
        }
    }
}
