use crate::shell::shell::Shell;
use crate::shell::shell::layout::{ActiveLayoutProfile, ResolvedLeftSidebarMode, resolve_layout};
use adw::prelude::*;
use rufin_core::runtime::ScrobblingPreferences;
use rufin_core::settings::HomeBlockKind;
use rufin_core::settings::SecretStorageMode;
use rufin_core::settings::{LeftSidebarMode, RightSidebarMode};
use std::rc::Rc;
use tracing::warn;

impl Shell {
    pub(super) fn retry_external_artwork(self: &Rc<Self>, warning_action: &'static str) {
        if let Err(error) = self.products.artwork.retry_external() {
            warn!(%error, action = warning_action, "failed to retry external artwork");
            return;
        }
        self.update_media_controls();
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
        self.refresh_tray_private_mode();
        self.reconcile_mounted_route();
        self.update_media_controls();
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
            Ok(()) => true,
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
        self.refresh_mounted_home();
    }
}

impl Shell {
    pub(crate) fn toggle_active_left_sidebar_size(self: &Rc<Self>) {
        let next_mode = if self.left_sidebar_mode() == ResolvedLeftSidebarMode::Full {
            LeftSidebarMode::Compact
        } else {
            LeftSidebarMode::Full
        };
        self.set_active_left_sidebar_mode(next_mode);
    }

    pub(crate) fn set_active_left_sidebar_mode(self: &Rc<Self>, mode: LeftSidebarMode) {
        let window_width = self.layout_width();
        let active_profile =
            resolve_layout(&self.settings.current.borrow().layout, window_width).profile;
        if self
            .settings
            .update_app_settings("left sidebar setting", |settings| {
                let mut requested = settings.layout.clone();
                let profile = match active_profile {
                    ActiveLayoutProfile::Default => &mut requested.default_profile,
                    ActiveLayoutProfile::Narrow => &mut requested.narrow_profile,
                };
                profile.left_sidebar = mode;
                if mode == LeftSidebarMode::Full
                    && resolve_layout(&requested, window_width).left_sidebar
                        != ResolvedLeftSidebarMode::Full
                {
                    let profile = match active_profile {
                        ActiveLayoutProfile::Default => &mut requested.default_profile,
                        ActiveLayoutProfile::Narrow => &mut requested.narrow_profile,
                    };
                    profile.right_sidebar = RightSidebarMode::Hidden;
                }
                requested.sanitize();
                if settings.layout == requested {
                    return false;
                }
                settings.layout = requested;
                true
            })
            .is_none()
        {
            return;
        }
        self.update_layout();
        self.chrome.window.queue_resize();
    }

    pub(crate) fn save_left_sidebar_drag(
        self: &Rc<Self>,
        mode: LeftSidebarMode,
        width: i32,
        hide_right: bool,
    ) {
        let active_profile =
            resolve_layout(&self.settings.current.borrow().layout, self.layout_width()).profile;
        self.settings
            .update_app_settings("left sidebar drag", |settings| {
                let profile = match active_profile {
                    ActiveLayoutProfile::Default => &mut settings.layout.default_profile,
                    ActiveLayoutProfile::Narrow => &mut settings.layout.narrow_profile,
                };
                let mut changed = false;
                if profile.left_sidebar != mode {
                    profile.left_sidebar = mode;
                    changed = true;
                }
                if hide_right && profile.right_sidebar != RightSidebarMode::Hidden {
                    profile.right_sidebar = RightSidebarMode::Hidden;
                    changed = true;
                }
                if mode == LeftSidebarMode::Full {
                    let width = width.clamp(
                        rufin_core::settings::MIN_LEFT_SIDEBAR_WIDTH,
                        rufin_core::settings::MAX_LEFT_SIDEBAR_WIDTH,
                    );
                    if settings.layout.preferred_left_sidebar_width != width {
                        settings.layout.preferred_left_sidebar_width = width;
                        changed = true;
                    }
                }
                changed
            });
        self.update_layout();
    }

    pub(crate) fn save_preferred_right_sidebar_width(&self, width: i32) {
        self.settings
            .update_app_settings("right sidebar width", |settings| {
                let width = width.clamp(
                    rufin_core::settings::MIN_RIGHT_SIDEBAR_WIDTH,
                    rufin_core::settings::MAX_RIGHT_SIDEBAR_WIDTH,
                );
                if settings.layout.preferred_right_sidebar_width == width {
                    return false;
                }
                settings.layout.preferred_right_sidebar_width = width;
                true
            });
    }
}
