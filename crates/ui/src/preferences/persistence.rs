use std::rc::Rc;

use crate::settings::HomeBlockKind;
use crate::{LeftSidebarMode, LibraryListKey, LibraryListSettings, RightSidebarMode};
use adw::prelude::*;
use playback::PlaybackSettings;
use secrets::SecretStorageMode;
use tracing::warn;

use crate::runtime::ScrobblingPreferences;
use crate::shell::Shell;
use crate::shell::layout::{ActiveLayoutProfile, ResolvedLeftSidebarMode, resolve_layout};

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
        let search_dialog = self.selected_lyrics().and_then(|lyrics| {
            lyrics
                .search_dialog
                .borrow()
                .as_ref()
                .map(|dialog| dialog.dialog.clone())
        });
        if let Some(dialog) = search_dialog {
            dialog.close();
        }
        self.render_lyrics_panel();
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
        self.update_app_settings("left sidebar drag", |settings| {
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
        self.update_app_settings("right sidebar width", |settings| {
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

    pub(crate) fn update_library_list_settings(
        &self,
        key: LibraryListKey,
        update: impl FnOnce(&mut LibraryListSettings),
    ) {
        let committed = self.update_app_settings("library list settings", |settings| {
            if !settings.library_lists.iter().any(|entry| entry.key == key) {
                settings
                    .library_lists
                    .push(crate::LibraryListSettingsEntry {
                        key,
                        settings: LibraryListSettings::for_key(key),
                    });
            }
            if let Some(entry) = settings
                .library_lists
                .iter_mut()
                .find(|entry| entry.key == key)
            {
                let previous = entry.settings.clone();
                update(&mut entry.settings);
                entry.settings.sanitize(key);
                return entry.settings != previous;
            }
            false
        });
        if committed.is_some() {
            self.reconcile_mounted_route();
        }
    }

    pub(crate) fn update_playback_settings(
        self: &Rc<Self>,
        update: impl FnOnce(&mut PlaybackSettings),
    ) {
        if let Some(settings) = self.update_app_settings("playback settings", |settings| {
            let previous = settings.playback.clone();
            update(&mut settings.playback);
            settings.playback.sanitize();
            settings.playback != previous
        }) {
            self.sync_fullscreen_equalizer_controls(&settings.playback.equalizer);
            self.update_bottom_player();
        }
    }

    pub(super) fn set_home_blocks(self: &Rc<Self>, blocks: Vec<HomeBlockKind>) {
        if self
            .set_app_setting("home block settings", blocks, |settings| {
                &mut settings.home_blocks
            })
            .is_none()
        {
            return;
        }
        self.reconcile_mounted_route();
    }
}
