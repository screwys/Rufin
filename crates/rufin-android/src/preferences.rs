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

/// One of the standard accent colors.
#[derive(uniffi::Record)]
pub struct AndroidAccentChoice {
    pub accent: AccentPreference,
    /// Its name, untranslated.
    pub title: String,
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
    pub theme: ThemePreference,
    pub accent: AccentPreference,
    pub language: String,
    pub themes: Vec<AndroidThemeOption>,
    pub accents: Vec<AndroidAccentChoice>,
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

fn state(
    settings: &AppearancePreferences,
    themes: &[rufin_core::themes::Theme],
    errors: &[String],
    languages: &[String],
) -> AndroidPreferencesState {
    AndroidPreferencesState {
        theme: settings.theme_preference.clone(),
        accent: settings.accent_preference,
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
            .map(|accent| AndroidAccentChoice {
                accent,
                title: accent.title().into(),
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
        let mut changes = self.settings.changes();
        changes.mark_changed();
        Arc::new(AndroidPreferencesSubscription {
            changes: tokio::sync::Mutex::new((changes, None)),
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
    /// The settings feed and the appearance last returned.
    changes: tokio::sync::Mutex<(
        tokio::sync::watch::Receiver<Arc<rufin_core::settings::StoredSettings>>,
        Option<AppearancePreferences>,
    )>,
    runtime: tokio::runtime::Handle,
    directory: PathBuf,
    languages: Vec<String>,
    custom_changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<()>>,
    themes: Arc<tokio::sync::Mutex<Option<(Vec<rufin_core::themes::Theme>, Vec<String>)>>>,
}

#[uniffi::export]
impl AndroidPreferencesSubscription {
    pub async fn next(&self) -> Result<AndroidPreferencesState, AndroidError> {
        let mut guard = self.changes.lock().await;
        let (changes, shown) = &mut *guard;
        let mut custom = self.custom_changes.lock().await;
        let settings = loop {
            let themes = tokio::select! {
                changed = changes.changed() => { changed.map_err(error)?; false }
                changed = custom.changed() => { changed.map_err(error)?; true }
            };
            custom.borrow_and_update();
            let appearance = changes.borrow_and_update().ui.appearance.clone();
            if themes || shown.as_ref() != Some(&appearance) {
                *shown = Some(appearance.clone());
                break appearance;
            }
        };
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
