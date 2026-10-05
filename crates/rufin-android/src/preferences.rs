use std::{collections::HashMap, path::PathBuf, sync::Arc};

use crate::host::{AndroidError, error};
use rufin_core::{
    SettingsHandle,
    runtime::ProductHandles,
    settings::{AccentPreference, AppearancePreferences, ThemePreference},
};

#[derive(uniffi::Enum)]
pub enum AndroidScrobblingEvent {
    OpenBrowser { url: String },
    Connected { username: String },
    Failed { reason: String },
    TimedOut,
}

#[derive(uniffi::Object)]
pub struct AndroidScrobblingSession {
    events: async_channel::Receiver<rufin_core::runtime::ScrobblingConnectionEvent>,
    opened: std::sync::Mutex<Option<async_channel::Sender<Result<(), String>>>>,
}

#[uniffi::export]
impl AndroidScrobblingSession {
    pub async fn next(&self) -> Result<AndroidScrobblingEvent, AndroidError> {
        use rufin_core::runtime::ScrobblingConnectionEvent as Event;
        Ok(match self.events.recv().await.map_err(error)? {
            Event::OpenUrl { url, opened } => {
                *self.opened.lock().map_err(error)? = Some(opened);
                AndroidScrobblingEvent::OpenBrowser { url }
            }
            Event::Connected { username } => AndroidScrobblingEvent::Connected { username },
            Event::Failed(reason) => AndroidScrobblingEvent::Failed { reason },
            Event::TimedOut => AndroidScrobblingEvent::TimedOut,
        })
    }
    pub async fn browser_opened(&self, failure: Option<String>) -> Result<(), AndroidError> {
        let opened = self.opened.lock().map_err(error)?.take();
        if let Some(opened) = opened {
            opened
                .send(failure.map_or(Ok(()), Err))
                .await
                .map_err(error)?;
        }
        Ok(())
    }
}

#[derive(uniffi::Record)]
pub struct AndroidAccentOption {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
}

#[derive(uniffi::Record)]
pub struct AndroidThemeOption {
    pub id: String,
    pub name: String,
    pub dark: bool,
    pub colors: HashMap<String, String>,
    pub accents: Vec<AndroidAccentOption>,
}

#[derive(uniffi::Record)]
pub struct AndroidLanguageOption {
    pub id: String,
    pub title: String,
}

#[derive(uniffi::Record)]
pub struct AndroidPreferencesState {
    pub theme_id: String,
    pub accent_id: String,
    pub theme_accent: Option<String>,
    pub language: String,
    pub themes: Vec<AndroidThemeOption>,
    pub accents: Vec<AndroidAccentOption>,
    pub languages: Vec<AndroidLanguageOption>,
    pub errors: Vec<String>,
}

#[derive(uniffi::Object)]
pub struct AndroidPreferences {
    settings: SettingsHandle,
    runtime: tokio::runtime::Handle,
    directory: PathBuf,
    custom_changes: tokio::sync::watch::Sender<()>,
    scrobbling: rufin_core::runtime::ScrobblingHandle,
    source: rufin_core::runtime::SourceHandle,
}

#[uniffi::export]
impl AndroidPreferences {
    pub fn set_half_stars(&self, source_id: String, enabled: bool) {
        self.source
            .set_half_stars(sources::SourceId::new(source_id), enabled);
    }

    pub async fn application_settings(&self) -> Result<String, AndroidError> {
        let settings = self.settings.clone();
        self.runtime.spawn_blocking(move || {
            let ui = settings.load();
            serde_json::to_string(&serde_json::json!({
                "private_mode":ui.private_mode, "cast_proxy_enabled":ui.cast_proxy_enabled,
                "external_metadata_enabled":ui.external_metadata_enabled,
                "prefer_distinct_track_covers":ui.prefer_distinct_track_covers,
                "notifications_enabled":ui.notifications_enabled,
                "control_notifications_enabled":ui.control_notifications_enabled,
                "release_notifications_enabled":ui.release_notifications_enabled,
                "release_check_interval_hours":ui.release_check_interval_hours,
                "activity_overview":ui.activity_overview, "backup":ui.backup,
                "reduce_motion":ui.reduce_motion, "seekbar_waveform_enabled":ui.seekbar_waveform_enabled,
                "show_downloaded_badges":ui.show_downloaded_badges, "new_playlist_current":ui.new_playlist_current,
                "auto_dj_refill_threshold":ui.auto_dj_refill_threshold,
                "clear_queue_includes_current":ui.clear_queue_includes_current,
                "external_site_links":ui.external_site_links, "external_lyrics_enabled":ui.lyrics.external_lyrics_enabled,
                "playback":ui.playback, "fullscreen_dynamic_background":ui.fullscreen_dynamic_background,
                "fullscreen_background_image":ui.fullscreen_background_image,
                "fullscreen_lyrics_visible":ui.fullscreen_lyrics_visible,
                "fullscreen_current_lyrics_line_visible":ui.fullscreen_current_lyrics_line_visible,
                "fullscreen_visualizer_visible":ui.fullscreen_visualizer_visible,
                "right_panel_combined":ui.right_panel.combined,
                "home_blocks":ui.home_blocks, "context_menu":ui.context_menu, "secret_storage_mode":ui.secret_storage_mode,
            }))
        }).await.map_err(error)?.map_err(error)
    }

    pub async fn set_application_preference(
        &self,
        field: String,
        value: String,
    ) -> Result<(), AndroidError> {
        let value: serde_json::Value = serde_json::from_str(&value).map_err(error)?;
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || {
                settings.update_preferences(|ui| {
                    macro_rules! assign {
                        ($target:expr) => {
                            $target =
                                serde_json::from_value(value).map_err(|error| error.to_string())?
                        };
                    }
                    match field.as_str() {
                        "private_mode" => assign!(ui.private_mode),
                        "cast_proxy_enabled" => assign!(ui.cast_proxy_enabled),
                        "external_metadata_enabled" => assign!(ui.external_metadata_enabled),
                        "prefer_distinct_track_covers" => assign!(ui.prefer_distinct_track_covers),
                        "notifications_enabled" => assign!(ui.notifications_enabled),
                        "control_notifications_enabled" => {
                            assign!(ui.control_notifications_enabled)
                        }
                        "release_notifications_enabled" => {
                            assign!(ui.release_notifications_enabled)
                        }
                        "release_check_interval_hours" => assign!(ui.release_check_interval_hours),
                        "activity_overview.monthly_enabled" => {
                            assign!(ui.activity_overview.monthly_enabled)
                        }
                        "activity_overview.yearly_enabled" => {
                            assign!(ui.activity_overview.yearly_enabled)
                        }
                        "activity_overview.monthly_days_before_end" => {
                            assign!(ui.activity_overview.monthly_days_before_end)
                        }
                        "activity_overview.monthly_days_after_end" => {
                            assign!(ui.activity_overview.monthly_days_after_end)
                        }
                        "activity_overview.yearly_days_before_end" => {
                            assign!(ui.activity_overview.yearly_days_before_end)
                        }
                        "activity_overview.yearly_days_after_end" => {
                            assign!(ui.activity_overview.yearly_days_after_end)
                        }
                        "backup" => assign!(ui.backup),
                        "backup.enabled" => assign!(ui.backup.enabled),
                        "backup.encrypt" => assign!(ui.backup.encrypt),
                        "backup.retention_count" => assign!(ui.backup.retention_count),
                        "backup.destination_uri" => assign!(ui.backup.destination_uri),
                        "backup.schedule.frequency" => assign!(ui.backup.schedule.frequency),
                        "backup.schedule.weekday" => assign!(ui.backup.schedule.weekday),
                        "backup.schedule.hour" => assign!(ui.backup.schedule.hour),
                        "backup.contents" => assign!(ui.backup.contents),
                        "reduce_motion" => assign!(ui.reduce_motion),
                        "seekbar_waveform_enabled" => assign!(ui.seekbar_waveform_enabled),
                        "show_downloaded_badges" => assign!(ui.show_downloaded_badges),
                        "new_playlist_current" => assign!(ui.new_playlist_current),
                        "auto_dj_refill_threshold" => assign!(ui.auto_dj_refill_threshold),
                        "clear_queue_includes_current" => assign!(ui.clear_queue_includes_current),
                        "external_site_links.enabled" => assign!(ui.external_site_links.enabled),
                        "external_site_links.lastfm" => assign!(ui.external_site_links.lastfm),
                        "external_site_links.musicbrainz" => {
                            assign!(ui.external_site_links.musicbrainz)
                        }
                        "external_site_links.server" => assign!(ui.external_site_links.server),
                        "lyrics.external_lyrics_enabled" => {
                            assign!(ui.lyrics.external_lyrics_enabled)
                        }
                        "playback.transition_mode" => assign!(ui.playback.transition_mode),
                        "playback.crossfade_seconds" => assign!(ui.playback.crossfade_seconds),
                        "playback.skip_same_album_crossfade" => {
                            assign!(ui.playback.skip_same_album_crossfade)
                        }
                        "playback.audio_fade_on_status_change" => {
                            assign!(ui.playback.audio_fade_on_status_change)
                        }
                        "playback.loudness_normalization" => {
                            assign!(ui.playback.loudness_normalization)
                        }
                        "playback.loudness_normalization_scope" => {
                            assign!(ui.playback.loudness_normalization_scope)
                        }
                        "playback.ebu_r128_target_lufs" => {
                            assign!(ui.playback.ebu_r128_target_lufs)
                        }
                        "playback.write_ebu_r128_tags" => assign!(ui.playback.write_ebu_r128_tags),
                        "playback.stream_quality" => assign!(ui.playback.stream_quality),
                        "playback.playback_rate" => assign!(ui.playback.playback_rate),
                        "playback.preserve_pitch" => assign!(ui.playback.preserve_pitch),
                        "home_blocks" => assign!(ui.home_blocks),
                        "context_menu" => assign!(ui.context_menu),
                        "playback.volume_scale" => ui.playback.set_volume_scale_preserving_gain(
                            serde_json::from_value(value).map_err(|error| error.to_string())?,
                        ),
                        "fullscreen_dynamic_background" => {
                            assign!(ui.fullscreen_dynamic_background)
                        }
                        "fullscreen_background_image" => assign!(ui.fullscreen_background_image),
                        "fullscreen_lyrics_visible" => assign!(ui.fullscreen_lyrics_visible),
                        "fullscreen_current_lyrics_line_visible" => {
                            assign!(ui.fullscreen_current_lyrics_line_visible)
                        }
                        "fullscreen_visualizer_visible" => {
                            assign!(ui.fullscreen_visualizer_visible)
                        }
                        "right_panel_combined" => assign!(ui.right_panel.combined),
                        _ => return Err("Unknown preference".into()),
                    }
                    Ok(())
                })
            })
            .await
            .map_err(error)?
            .map_err(error)
    }
}

impl AndroidPreferences {
    pub(crate) fn new(products: &ProductHandles, directory: PathBuf) -> Self {
        Self {
            settings: products.settings.clone(),
            runtime: products.runtime.clone(),
            directory,
            custom_changes: tokio::sync::watch::channel(()).0,
            scrobbling: products.scrobbling.clone(),
            source: products.source.clone(),
        }
    }
}

fn accent_id(accent: AccentPreference) -> &'static str {
    match accent {
        AccentPreference::System => "System",
        AccentPreference::Blue => "Blue",
        AccentPreference::Teal => "Teal",
        AccentPreference::Green => "Green",
        AccentPreference::Yellow => "Yellow",
        AccentPreference::Orange => "Orange",
        AccentPreference::Red => "Red",
        AccentPreference::Pink => "Pink",
        AccentPreference::Purple => "Purple",
        AccentPreference::Slate => "Slate",
    }
}

fn scrobbling_json(p: &rufin_core::runtime::ScrobblingPreferences) -> String {
    serde_json::json!({
        "lastfm": {"enabled":p.lastfm.enabled,"now_playing":p.lastfm.now_playing_enabled,"api_key":p.lastfm.api_key,"api_secret":p.lastfm.api_secret,"username":p.lastfm.username,"connected":p.lastfm.connected},
        "librefm":{"enabled":p.librefm.enabled,"now_playing":p.librefm.now_playing_enabled,"username":p.librefm.username,"connected":p.librefm.connected},
        "listenbrainz":{"enabled":p.listenbrainz.enabled,"now_playing":p.listenbrainz.now_playing_enabled,"token":p.listenbrainz.user_token,"username":p.listenbrainz.username,"connected":!p.listenbrainz.user_token.is_empty() && !p.listenbrainz.username.is_empty()}
    }).to_string()
}

fn state(
    settings: &AppearancePreferences,
    directory: &std::path::Path,
    languages: &[String],
) -> AndroidPreferencesState {
    let mut themes = rufin_core::themes::builtins();
    let (custom, errors) = rufin_core::themes::custom(directory);
    themes.extend(custom);
    let theme_id = match &settings.theme_preference {
        ThemePreference::System => "system".into(),
        ThemePreference::Light => "light".into(),
        ThemePreference::Dark => "dark".into(),
        ThemePreference::Named(id) => id.clone(),
    };
    AndroidPreferencesState {
        theme_accent: settings.theme_accents.get(&theme_id).cloned(),
        theme_id,
        accent_id: accent_id(settings.accent_preference).into(),
        language: settings.language.clone(),
        themes: themes
            .iter()
            .map(|theme| {
                let colors = rufin_core::themes::theme_colors(
                    theme,
                    &themes,
                    settings.accent_preference,
                    &settings.theme_accents,
                )
                .into_iter()
                .map(|(key, value)| (key.trim_start_matches("--").to_string(), value))
                .collect();
                AndroidThemeOption {
                    id: theme.id.clone(),
                    name: theme.name.clone(),
                    dark: theme.mode == rufin_core::themes::Mode::Dark,
                    colors,
                    accents: theme
                        .accents
                        .iter()
                        .map(|accent| AndroidAccentOption {
                            id: accent.name.clone(),
                            name: accent.name.clone(),
                            color: Some(accent.color.clone()),
                        })
                        .collect(),
                }
            })
            .collect(),
        accents: AccentPreference::ALL
            .into_iter()
            .map(|accent| AndroidAccentOption {
                id: accent_id(accent).into(),
                name: localization::tr(accent_id(accent)),
                color: accent.color().map(str::to_string),
            })
            .collect(),
        languages: localization::language_options_for(languages.iter().cloned())
            .into_iter()
            .map(|option| AndroidLanguageOption {
                id: option.id,
                title: option.title,
            })
            .collect(),
        errors,
    }
}

#[uniffi::export]
impl AndroidPreferences {
    pub async fn scrobbling_preferences(&self) -> Result<String, AndroidError> {
        let p = self
            .scrobbling
            .load_preferences()
            .recv()
            .await
            .map_err(error)?;
        Ok(scrobbling_json(&p))
    }

    pub async fn set_scrobbling(
        &self,
        service: String,
        enabled: bool,
        now_playing: bool,
    ) -> Result<String, AndroidError> {
        let mut p = self.scrobbling.preferences();
        match service.as_str() {
            "lastfm" => {
                p.lastfm.enabled = enabled;
                p.lastfm.now_playing_enabled = now_playing;
            }
            "librefm" => {
                p.librefm.enabled = enabled;
                p.librefm.now_playing_enabled = now_playing;
            }
            "listenbrainz" => {
                p.listenbrainz.enabled = enabled;
                p.listenbrainz.now_playing_enabled = now_playing;
            }
            _ => return Err(error("Unknown scrobbling service")),
        }
        let owner = self.scrobbling.clone();
        let saved = self
            .runtime
            .spawn_blocking(move || owner.save(&p))
            .await
            .map_err(error)?
            .map_err(error)?;
        Ok(scrobbling_json(&saved))
    }

    pub async fn set_scrobbling_credential(
        &self,
        service: String,
        value: String,
        secret: String,
    ) -> Result<String, AndroidError> {
        if !matches!(service.as_str(), "lastfm" | "listenbrainz") {
            return Err(error("Unknown scrobbling service"));
        }
        let saved = self
            .scrobbling
            .save_credential(move |p| match service.as_str() {
                "lastfm" => {
                    p.lastfm.api_key = value;
                    p.lastfm.api_secret = secret;
                }
                _ => p.listenbrainz.user_token = value,
            })
            .recv()
            .await
            .map_err(error)?
            .map_err(error)?;
        Ok(scrobbling_json(&saved))
    }

    pub fn authorize_scrobbling(
        &self,
        service: String,
    ) -> Result<Arc<AndroidScrobblingSession>, AndroidError> {
        use rufin_core::runtime::ScrobblingConnection;
        let p = self.scrobbling.preferences();
        let request = match service.as_str() {
            "lastfm" => ScrobblingConnection::LastFm {
                api_key: p.lastfm.api_key,
                api_secret: p.lastfm.api_secret,
            },
            "librefm" => ScrobblingConnection::LibreFm,
            _ => return Err(error("Unknown scrobbling service")),
        };
        Ok(Arc::new(AndroidScrobblingSession {
            events: self.scrobbling.connect(request),
            opened: std::sync::Mutex::new(None),
        }))
    }

    pub async fn connect_listenbrainz(&self, user_token: String) -> Result<String, AndroidError> {
        let saved = self
            .scrobbling
            .connect_listenbrainz(user_token)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)?;
        Ok(scrobbling_json(&saved))
    }

    pub async fn set_secret_storage(&self, mode: String) -> Result<(), AndroidError> {
        let mode = match mode.as_str() {
            "system-keyring" => secrets::SecretStorageMode::SystemKeyring,
            "config-file" => secrets::SecretStorageMode::ConfigFile,
            _ => return Err(error("Unknown secret storage")),
        };
        self.source
            .change_secret_storage(mode)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }
    pub fn subscribe(
        &self,
        packaged_languages: Vec<String>,
    ) -> Arc<AndroidPreferencesSubscription> {
        let mut changes = self.settings.appearance_changes();
        changes.mark_changed();
        Arc::new(AndroidPreferencesSubscription {
            changes: tokio::sync::Mutex::new(changes),
            runtime: self.runtime.clone(),
            directory: self.directory.clone(),
            languages: packaged_languages,
            custom_changes: tokio::sync::Mutex::new(self.custom_changes.subscribe()),
        })
    }

    pub async fn import_theme(&self, name: String, bytes: Vec<u8>) -> Result<(), AndroidError> {
        let settings = self.settings.clone();
        let directory = self.directory.clone();
        self.runtime
            .spawn_blocking(move || {
                let id = rufin_core::themes::import(&directory, &name, &bytes)?;
                settings.set_theme(ThemePreference::Named(id))
            })
            .await
            .map_err(error)?
            .map_err(error)?;
        self.custom_changes.send_replace(());
        Ok(())
    }

    pub async fn set_theme(&self, id: String) -> Result<(), AndroidError> {
        let theme = match id.as_str() {
            "system" => ThemePreference::System,
            "light" => ThemePreference::Light,
            "dark" => ThemePreference::Dark,
            _ => ThemePreference::Named(id),
        };
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.set_theme(theme))
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn set_accent(&self, id: String) -> Result<(), AndroidError> {
        let accent = AccentPreference::ALL
            .into_iter()
            .find(|accent| accent_id(*accent) == id)
            .ok_or_else(|| error("Unknown accent"))?;
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.set_accent(accent))
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn set_theme_accent(
        &self,
        theme: String,
        accent: String,
    ) -> Result<(), AndroidError> {
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.set_theme_accent(theme, accent))
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn set_language(&self, language: String) -> Result<(), AndroidError> {
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.set_language(language))
            .await
            .map_err(error)?
            .map_err(error)
    }
}

#[derive(uniffi::Object)]
pub struct AndroidPreferencesSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<AppearancePreferences>>,
    runtime: tokio::runtime::Handle,
    directory: PathBuf,
    languages: Vec<String>,
    custom_changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<()>>,
}

#[uniffi::export]
impl AndroidPreferencesSubscription {
    pub async fn next(&self) -> Result<AndroidPreferencesState, AndroidError> {
        let mut changes = self.changes.lock().await;
        let mut custom = self.custom_changes.lock().await;
        tokio::select! {
            changed = changes.changed() => changed.map_err(error)?,
            changed = custom.changed() => changed.map_err(error)?,
        }
        custom.borrow_and_update();
        let settings = changes.borrow_and_update().clone();
        let directory = self.directory.clone();
        let languages = self.languages.clone();
        self.runtime
            .spawn_blocking(move || state(&settings, &directory, &languages))
            .await
            .map_err(error)
    }
}
