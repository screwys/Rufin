use std::{collections::HashMap, path::PathBuf, sync::Arc};

use crate::host::{AndroidError, error};
use rufin_core::{
    SettingsHandle,
    runtime::ProductHandles,
    settings::{AccentPreference, AppearancePreferences, ThemePreference},
};

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
    pub accent_id: Option<String>,
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
    themes: Arc<tokio::sync::Mutex<Option<(Vec<rufin_core::themes::Theme>, Vec<String>)>>>,
}

impl AndroidPreferences {
    pub(crate) fn new(products: &ProductHandles, directory: PathBuf) -> Self {
        Self {
            settings: products.settings.clone(),
            runtime: products.runtime.clone(),
            directory,
            custom_changes: tokio::sync::watch::channel(()).0,
            themes: Arc::new(tokio::sync::Mutex::new(None)),
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

fn state(
    settings: &AppearancePreferences,
    themes: &[rufin_core::themes::Theme],
    errors: &[String],
    languages: &[String],
) -> AndroidPreferencesState {
    let theme_id = match &settings.theme_preference {
        ThemePreference::System => "system".into(),
        ThemePreference::Light => "light".into(),
        ThemePreference::Dark => "dark".into(),
        ThemePreference::Named(id) => id.clone(),
    };
    AndroidPreferencesState {
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
                    accent_id: theme
                        .accent(settings.theme_accents.get(&theme.id).map(String::as_str))
                        .map(|accent| accent.name.clone()),
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
        errors: errors.to_vec(),
    }
}

#[uniffi::export]
impl AndroidPreferences {
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
            themes: self.themes.clone(),
        })
    }

    pub async fn import_theme(&self, name: String, bytes: Vec<u8>) -> Result<(), AndroidError> {
        let settings = self.settings.clone();
        let directory = self.directory.clone();
        self.runtime
            .spawn_blocking(move || {
                let id = rufin_core::themes::import(&directory, &name, &bytes)
                    .map_err(rufin_core::settings::SettingsError::from)?;
                settings.set_theme(ThemePreference::Named(id))
            })
            .await
            .map_err(error)?
            .map_err(error)?;
        *self.themes.lock().await = None;
        self.custom_changes.send_replace(());
        Ok(())
    }
}

#[derive(uniffi::Object)]
pub struct AndroidPreferencesSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<AppearancePreferences>>,
    runtime: tokio::runtime::Handle,
    directory: PathBuf,
    languages: Vec<String>,
    custom_changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<()>>,
    themes: Arc<tokio::sync::Mutex<Option<(Vec<rufin_core::themes::Theme>, Vec<String>)>>>,
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
        let mut loaded = self.themes.lock().await;
        if loaded.is_none() {
            let directory = self.directory.clone();
            *loaded = Some(
                self.runtime
                    .spawn_blocking(move || {
                        let mut themes = rufin_core::themes::builtins();
                        let (custom, errors) = rufin_core::themes::custom(&directory);
                        themes.extend(custom);
                        (themes, errors)
                    })
                    .await
                    .map_err(error)?,
            );
        }
        let (themes, errors) = loaded.as_ref().unwrap();
        Ok(state(&settings, themes, errors, &self.languages))
    }
}
