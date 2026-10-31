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
                &mut settings.application.private_mode
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
        match self.products.source.change_secret_storage(mode).await {
            Ok(()) => {
                self.products.scrobbling.storage_reset();
                true
            }
            Err(error) => {
                warn!(%error, "failed to change secret storage mode");
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
        if let Err(error) = self.products.scrobbling.save(preferences) {
            warn!(%error, action = warning_action, "failed to save scrobbling settings");
        }
    }

    pub(super) fn save_scrobbling_credential(
        self: &Rc<Self>,
        warning_action: &'static str,
        edit: rufin_core::scrobbling::ScrobblingCredentialEdit,
    ) {
        let previous = self.products.scrobbling.preferences();
        let shell = Rc::clone(self);
        gtk::glib::spawn_future_local(async move {
            match shell.products.scrobbling.save_credential(edit).await {
                Ok(committed) => {
                    if previous.lastfm.api_key != committed.lastfm.api_key {
                        shell.retry_external_artwork(warning_action);
                    }
                }
                Err(error) => {
                    warn!(%error, action = warning_action, "failed to save scrobbling settings")
                }
            }
        });
    }

    /// Saves Home's blocks; Home reads again to show them in core's order.
    pub(super) fn set_home_blocks(self: &Rc<Self>, blocks: Vec<HomeBlockKind>) {
        match self.settings.persistence.set_home_blocks(blocks) {
            Ok(true) => {
                self.settings.sync();
            }
            Ok(false) => return,
            Err(error) => {
                warn!(%error, action = "home block settings", "failed to save settings");
                return;
            }
        }
        (self.effects)(crate::Effect::RefreshHome);
    }
}
