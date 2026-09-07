use std::rc::Rc;

use crate::{LeftSidebarMode, RightSidebarMode};
use adw::prelude::*;
use rufin_core::settings::HomeBlockKind;
use secrets::SecretStorageMode;
use tracing::warn;

use crate::shell::Shell;
use crate::shell::layout::{ActiveLayoutProfile, ResolvedLeftSidebarMode, resolve_layout};
use rufin_core::runtime::ScrobblingPreferences;

impl Shell {
    pub(super) fn retry_external_artwork(self: &Rc<Self>, warning_action: &'static str) {
        if let Err(error) = self.products.artwork.retry_external() {
            warn!(%error, action = warning_action, "failed to retry external artwork");
            return;
        }
        self.update_media_controls();
    }

    pub(crate) fn set_private_mode(self: &Rc<Self>, enabled: bool) {
        if self
            .settings
            .set_app_setting("private mode setting", enabled, |settings| {
                &mut settings.private_mode
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
        match self
            .products
            .source
            .change_secret_storage(mode)
            .recv()
            .await
        {
            Ok(Ok(())) => {
                self.settings.current.borrow_mut().secret_storage_mode = mode;
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

    pub(crate) fn toggle_active_left_sidebar_size(self: &Rc<Self>) {
        let next_mode = if self.left_sidebar_mode() == ResolvedLeftSidebarMode::Full {
            LeftSidebarMode::Compact
        } else {
            LeftSidebarMode::Full
        };
        self.set_active_left_sidebar_mode(next_mode);
    }

    pub(crate) fn set_active_left_sidebar_mode(self: &Rc<Self>, mode: LeftSidebarMode) {
        let active_profile =
            resolve_layout(&self.settings.current.borrow().layout, self.layout_width()).profile;
        if self
            .settings
            .update_app_settings("left sidebar setting", |settings| {
                let profile = match active_profile {
                    ActiveLayoutProfile::Default => &mut settings.layout.default_profile,
                    ActiveLayoutProfile::Narrow => &mut settings.layout.narrow_profile,
                };
                if profile.left_sidebar == mode {
                    return false;
                }
                profile.left_sidebar = mode;
                settings.layout.sanitize();
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
                    let width =
                        width.clamp(crate::MIN_LEFT_SIDEBAR_WIDTH, crate::MAX_LEFT_SIDEBAR_WIDTH);
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
                    crate::MIN_RIGHT_SIDEBAR_WIDTH,
                    crate::MAX_RIGHT_SIDEBAR_WIDTH,
                );
                if settings.layout.preferred_right_sidebar_width == width {
                    return false;
                }
                settings.layout.preferred_right_sidebar_width = width;
                true
            });
    }

    pub(super) fn update_scrobbling_settings(
        self: &Rc<Self>,
        warning_action: &'static str,
        update: impl FnOnce(&mut ScrobblingPreferences) -> bool,
    ) -> Option<ScrobblingPreferences> {
        let mut preferences = self.products.scrobbling.preferences();
        if !update(&mut preferences) {
            return None;
        }
        match self.products.scrobbling.save(&preferences) {
            Ok(committed) => {
                self.settings.current.borrow_mut().lastfm_api_key =
                    committed.lastfm.api_key.clone();
                Some(committed)
            }
            Err(error) => {
                warn!(%error, action = warning_action, "failed to save scrobbling settings");
                None
            }
        }
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
        self.reconcile_mounted_route();
        if added {
            self.refresh_mounted_home();
        }
    }
}
