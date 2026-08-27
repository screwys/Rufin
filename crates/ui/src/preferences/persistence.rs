use std::rc::Rc;

use crate::settings::HomeBlockKind;
use crate::{
    AccentPreference, LeftSidebarMode, LibraryListKey, LibraryListSettings, ThemePreference,
};
use adw::prelude::*;
use desktop_integration::{DisplayType, LinkType};
use localization::set_language_preference;
use playback::PlaybackSettings;
use secrets::SecretStorageMode;
use tracing::warn;

use crate::routes::playlist_picker::refresh_context_playlist_picker;
use crate::runtime::ScrobblingPreferences;
use crate::shell::Shell;
use crate::shell::layout::{ActiveLayoutProfile, ResolvedLeftSidebarMode, resolve_layout};

impl Shell {
    pub(super) fn retry_external_artwork(self: &Rc<Self>, warning_action: &'static str) {
        if let Err(error) = self.products.artwork.retry_external() {
            warn!(%error, action = warning_action, "failed to retry external artwork");
            return;
        }
        self.refresh_artwork_policy();
    }

    pub(crate) fn reconcile_lyrics_settings_ui(self: &Rc<Self>) {
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

    pub(super) fn set_external_metadata_enabled(self: &Rc<Self>, enabled: bool) {
        if self
            .update_app_settings("metadata setting", |settings| {
                if settings.external_metadata_enabled == enabled {
                    return false;
                }
                settings.external_metadata_enabled = enabled;
                true
            })
            .is_none()
        {
            return;
        }
        if enabled {
            self.retry_external_artwork("metadata setting");
        } else {
            self.refresh_artwork_policy();
        }
    }

    pub(super) fn set_external_site_links_enabled(self: &Rc<Self>, enabled: bool) {
        if self
            .update_app_settings("external site links setting", |settings| {
                if settings.external_site_links.enabled == enabled {
                    return false;
                }
                settings.external_site_links.enabled = enabled;
                true
            })
            .is_some()
        {
            self.reconcile_mounted_route();
        }
    }

    pub(super) fn set_lastfm_site_links_enabled(self: &Rc<Self>, enabled: bool) {
        if self
            .update_app_settings("Last.fm site links setting", |settings| {
                if settings.external_site_links.lastfm == enabled {
                    return false;
                }
                settings.external_site_links.lastfm = enabled;
                true
            })
            .is_some()
        {
            self.reconcile_mounted_route();
        }
    }

    pub(super) fn set_musicbrainz_site_links_enabled(self: &Rc<Self>, enabled: bool) {
        if self
            .update_app_settings("MusicBrainz site links setting", |settings| {
                if settings.external_site_links.musicbrainz == enabled {
                    return false;
                }
                settings.external_site_links.musicbrainz = enabled;
                true
            })
            .is_some()
        {
            self.reconcile_mounted_route();
        }
    }

    pub(super) fn set_server_site_links_enabled(self: &Rc<Self>, enabled: bool) {
        if self
            .update_app_settings("server site links setting", |settings| {
                if settings.external_site_links.server == enabled {
                    return false;
                }
                settings.external_site_links.server = enabled;
                true
            })
            .is_some()
        {
            self.reconcile_mounted_route();
        }
    }

    pub(super) fn set_prefer_server_playlist_covers(self: &Rc<Self>, enabled: bool) {
        if self
            .update_app_settings("playlist cover setting", |settings| {
                if settings.prefer_server_playlist_covers == enabled {
                    return false;
                }
                settings.prefer_server_playlist_covers = enabled;
                true
            })
            .is_none()
        {
            return;
        }
        self.reconcile_mounted_route();
        refresh_context_playlist_picker(self);
    }

    pub(crate) fn set_private_mode(self: &Rc<Self>, enabled: bool) {
        if self
            .update_app_settings("private mode setting", |settings| {
                if settings.private_mode == enabled {
                    return false;
                }
                settings.private_mode = enabled;
                true
            })
            .is_none()
        {
            return;
        }
        self.refresh_tray_private_mode();
        self.reconcile_mounted_route();
        self.refresh_artwork_policy();
        self.reconcile_lyrics_settings_ui();
    }

    pub(crate) fn set_cast_proxy_enabled(self: &Rc<Self>, enabled: bool) {
        self.update_app_settings("cast proxy setting", |settings| {
            if settings.cast_proxy_enabled == enabled {
                return false;
            }
            settings.cast_proxy_enabled = enabled;
            true
        });
    }

    pub(crate) fn set_cast_network_interface(self: &Rc<Self>, network_interface: Option<String>) {
        self.update_app_settings("casting network setting", |settings| {
            if settings.cast_network_interface == network_interface {
                return false;
            }
            settings.cast_network_interface = network_interface;
            true
        });
    }

    pub(super) fn set_notifications_enabled(self: &Rc<Self>, enabled: bool) {
        if self
            .update_app_settings("notification setting", |settings| {
                if settings.notifications_enabled == enabled {
                    return false;
                }
                settings.notifications_enabled = enabled;
                true
            })
            .is_some()
            && !enabled
        {
            self.withdraw_now_playing_notification();
        }
    }

    pub(super) fn set_control_notifications_enabled(self: &Rc<Self>, enabled: bool) {
        self.update_app_settings("control notification setting", |settings| {
            if settings.control_notifications_enabled == enabled {
                return false;
            }
            settings.control_notifications_enabled = enabled;
            true
        });
    }

    pub(super) fn set_release_notifications_enabled(self: &Rc<Self>, enabled: bool) {
        self.update_app_settings("release notification setting", |settings| {
            if settings.release_notifications_enabled == enabled {
                return false;
            }
            settings.release_notifications_enabled = enabled;
            true
        });
    }

    pub(super) fn set_automatic_updates_enabled(self: &Rc<Self>, enabled: bool) {
        self.update_app_settings("automatic update setting", |settings| {
            if settings.automatic_updates_enabled == enabled {
                return false;
            }
            settings.automatic_updates_enabled = enabled;
            true
        });
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

    pub(super) fn set_type_to_search_enabled(self: &Rc<Self>, enabled: bool) {
        self.update_app_settings("type to search setting", |settings| {
            if settings.type_to_search_enabled == enabled {
                return false;
            }
            settings.type_to_search_enabled = enabled;
            true
        });
    }

    pub(super) fn set_theme_preference(self: &Rc<Self>, preference: ThemePreference) {
        if let Some(settings) = self.update_app_settings("theme setting", |settings| {
            if settings.theme_preference == preference {
                return false;
            }
            settings.theme_preference = preference;
            true
        }) {
            self.appearance.apply(&settings);
        }
    }

    pub(super) fn set_accent_preference(self: &Rc<Self>, preference: AccentPreference) {
        if let Some(settings) = self.update_app_settings("accent setting", |settings| {
            if settings.accent_preference == preference {
                return false;
            }
            settings.accent_preference = preference;
            true
        }) {
            self.appearance.apply(&settings);
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

    pub(crate) fn save_left_sidebar_drag(self: &Rc<Self>, mode: LeftSidebarMode, width: i32) {
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

    pub(super) fn set_seekbar_waveform_enabled(self: &Rc<Self>, enabled: bool) {
        if self
            .update_app_settings("seekbar waveform setting", |settings| {
                if settings.seekbar_waveform_enabled == enabled {
                    return false;
                }
                settings.seekbar_waveform_enabled = enabled;
                true
            })
            .is_none()
        {
            return;
        }
        self.update_bottom_player();
    }

    pub(super) fn set_language_preference(self: &Rc<Self>, language: String) -> bool {
        let Some(settings) = self.update_app_settings("language setting", |settings| {
            if settings.language == language {
                return false;
            }
            settings.language = language;
            true
        }) else {
            return false;
        };

        set_language_preference(&settings.language);
        self.relocalize_visible_ui();
        true
    }

    fn refresh_artwork_policy(self: &Rc<Self>) {
        self.update_media_controls();
    }

    pub(super) fn set_discord_presence_enabled(self: &Rc<Self>, enabled: bool) {
        self.update_app_settings("Discord presence setting", |settings| {
            if settings.rich_presence.enabled == enabled {
                return false;
            }
            settings.rich_presence.enabled = enabled;
            true
        });
    }

    pub(super) fn set_discord_display_type(self: &Rc<Self>, display_type: DisplayType) {
        self.update_app_settings("Discord display setting", |settings| {
            if settings.rich_presence.display_type == display_type {
                return false;
            }
            settings.rich_presence.display_type = display_type;
            true
        });
    }

    pub(super) fn set_discord_link_type(self: &Rc<Self>, link_type: LinkType) {
        self.update_app_settings("Discord link setting", |settings| {
            if settings.rich_presence.link_type == link_type {
                return false;
            }
            settings.rich_presence.link_type = link_type;
            true
        });
    }

    pub(super) fn set_discord_show_paused(self: &Rc<Self>, enabled: bool) {
        self.update_app_settings("Discord paused setting", |settings| {
            if settings.rich_presence.show_paused == enabled {
                return false;
            }
            settings.rich_presence.show_paused = enabled;
            true
        });
    }

    pub(super) fn set_discord_show_as_listening(self: &Rc<Self>, enabled: bool) {
        self.update_app_settings("Discord activity type setting", |settings| {
            if settings.rich_presence.show_as_listening == enabled {
                return false;
            }
            settings.rich_presence.show_as_listening = enabled;
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
            .update_app_settings("home block settings", |settings| {
                if settings.home_blocks == blocks {
                    return false;
                }
                settings.home_blocks = blocks;
                true
            })
            .is_none()
        {
            return;
        }
        self.reconcile_mounted_route();
    }
}
