use crate::{AccentPreference, Settings, ThemePreference};

use rufin_core::themes::{Mode, Theme};
use std::{cell::RefCell, path::PathBuf};

pub struct ApplicationAppearance {
    override_provider: gtk::CssProvider,
    pub themes: RefCell<Vec<Theme>>,
    pub system_style: adw::StyleManager,
}

impl ApplicationAppearance {
    pub fn install() -> Self {
        let appearance = Self {
            override_provider: gtk::CssProvider::new(),
            themes: RefCell::new(rufin_core::themes::builtins()),
            // A manager without a display observes the OS without forcing its scheme on the app.
            system_style: gtk::glib::Object::new(),
        };
        for error in appearance.reload() {
            tracing::warn!(%error, "could not load theme");
        }
        let override_provider = &appearance.override_provider;
        let Some(display) = gtk::gdk::Display::default() else {
            return appearance;
        };

        let base_provider = gtk::CssProvider::new();
        base_provider.load_from_resource(crate::ui_resource::BASE_CSS_RESOURCE);
        gtk::style_context_add_provider_for_display(
            &display,
            &base_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        gtk::style_context_add_provider_for_display(
            &display,
            override_provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
        );

        appearance
    }

    pub fn folder() -> PathBuf {
        rufin_core::paths::roots().config_dir().join("themes")
    }

    pub fn reload(&self) -> Vec<String> {
        let (custom, mut errors) = rufin_core::themes::custom(&Self::folder());
        let mut themes = rufin_core::themes::builtins();
        for theme in custom {
            let provider = gtk::CssProvider::new();
            let failures = std::rc::Rc::new(RefCell::new(Vec::new()));
            let output = failures.clone();
            provider.connect_parsing_error(move |_, _, error| {
                if error.is::<gtk::CssParserError>() {
                    output.borrow_mut().push(error.to_string());
                }
            });
            // Parse values as colors too: custom property declarations alone accept invalid colors.
            let colors = theme
                .colors
                .iter()
                .filter(|(key, _)| key.ends_with("-color"))
                .map(|(_, value)| value)
                .chain(
                    theme
                        .accents
                        .iter()
                        .flat_map(|accent| [&accent.color, &accent.foreground]),
                );
            let mut css = format!(
                ":root {{ {} }}",
                palette_css(&themes, &theme, None, AccentPreference::System)
            );
            for color in colors {
                css.push_str(&format!("* {{ color: {color}; }}"));
            }
            provider.load_from_string(&css);
            if failures.borrow().is_empty() {
                themes.push(theme);
            } else {
                errors.push(format!("{}: {}", theme.name, failures.borrow().join("\n")));
            }
        }
        *self.themes.borrow_mut() = themes;
        errors
    }

    pub fn apply(&self, settings: &Settings) {
        let themes = self.themes.borrow();
        adw::StyleManager::default()
            .set_color_scheme(color_scheme(&settings.theme_preference, &themes));
        self.override_provider
            .load_from_string(&appearance_override_css(settings, &themes));
    }

    pub fn preview_css(&self, theme: Option<&Theme>, settings: &Settings) -> String {
        let themes = self.themes.borrow();
        let theme = theme.unwrap_or_else(|| &themes[usize::from(self.system_style.is_dark())]);
        let mut css = String::new();
        if theme.accents.is_empty() && settings.accent_preference == AccentPreference::System {
            append_accent(
                &mut css,
                &self.system_style.accent_color_rgba().to_string(),
                "#ffffff",
            );
        }
        css.push_str(&palette_css(
            &themes,
            theme,
            settings.theme_accents.get(&theme.id).map(String::as_str),
            settings.accent_preference,
        ));
        css
    }
}

fn selected_theme<'a>(preference: &ThemePreference, themes: &'a [Theme]) -> Option<&'a Theme> {
    let id = match preference {
        ThemePreference::System => return None,
        ThemePreference::Light => "builtin:light",
        ThemePreference::Dark => "builtin:dark",
        ThemePreference::Named(id) => id,
    };
    themes.iter().find(|theme| theme.id == id)
}

fn color_scheme(preference: &ThemePreference, themes: &[Theme]) -> adw::ColorScheme {
    match selected_theme(preference, themes).map(|theme| theme.mode) {
        None => adw::ColorScheme::PreferLight,
        Some(Mode::Light) => adw::ColorScheme::ForceLight,
        Some(Mode::Dark) => adw::ColorScheme::ForceDark,
    }
}

fn palette_css(
    themes: &[Theme],
    theme: &Theme,
    accent: Option<&str>,
    standard_accent: AccentPreference,
) -> String {
    let colors = theme.colors(themes, accent);
    let mut css = String::new();
    for (key, value) in colors {
        css.push_str(&format!("  --{key}: {value};\n"));
    }
    if theme.accents.is_empty()
        && let Some(color) = standard_accent.color()
    {
        append_accent(&mut css, color, "#ffffff");
    }
    css
}

fn append_accent(css: &mut String, color: &str, foreground: &str) {
    css.push_str(&format!("  --accent-bg-color: {color};\n  --accent-fg-color: {foreground};\n  --accent-color: oklab(from var(--accent-bg-color) var(--standalone-color-oklab));\n"));
}

fn appearance_override_css(settings: &Settings, themes: &[Theme]) -> String {
    let mut css = String::from(":root {\n");
    let theme = selected_theme(&settings.theme_preference, themes);
    if let Some(theme) = theme {
        // Light and Dark keep inheriting the system accent when no override is selected.
        if matches!(
            settings.theme_preference,
            ThemePreference::Light | ThemePreference::Dark
        ) {
            for (key, value) in &theme.colors {
                css.push_str(&format!("  --{key}: {value};\n"));
            }
        } else {
            css.push_str(&palette_css(
                themes,
                theme,
                settings.theme_accents.get(&theme.id).map(String::as_str),
                settings.accent_preference,
            ));
        }
    }
    if (theme.is_none() || !matches!(settings.theme_preference, ThemePreference::Named(_)))
        && let Some(color) = settings.accent_preference.color()
    {
        append_accent(&mut css, color, "#ffffff");
    }
    let lyrics = &settings.lyrics;
    if let Some(color) = lyrics.lyrics_highlight_color.as_deref() {
        css.push_str("  --lyrics-highlight-color: ");
        css.push_str(color);
        css.push_str(";\n");
    }
    css.push_str("}\n");
    let selectors = ".lyrics-line, .lyrics-instrumental, .lyrics-furigana, .lyrics-romanization, .lyrics-reading-surface, .lyrics-cue";
    if let Some(family) = lyrics.lyrics_font_family.as_deref() {
        css.push_str(selectors);
        css.push_str(" {\n");
        css.push_str("  font-family: '");
        css.push_str(&family.replace('\\', "\\\\").replace('\'', "\\'"));
        css.push_str("', sans-serif;\n");
        css.push_str("}\n");
    }
    if let Some(size) = lyrics.lyrics_font_size {
        css.push_str(&format!(
            ".lyrics-line, .lyrics-instrumental {{ font-size: {size}px; }}\n"
        ));
    }
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    fn css(theme: ThemePreference, accent: AccentPreference) -> String {
        let settings = Settings {
            theme_preference: theme,
            accent_preference: accent,
            ..Settings::default()
        };
        appearance_override_css(&settings, &rufin_core::themes::builtins())
    }

    #[test]
    fn theme_preferences_map_to_explicit_application_color_schemes() {
        assert_eq!(
            color_scheme(&ThemePreference::System, &rufin_core::themes::builtins()),
            adw::ColorScheme::PreferLight
        );
        assert_eq!(
            color_scheme(&ThemePreference::Light, &rufin_core::themes::builtins()),
            adw::ColorScheme::ForceLight
        );
        assert_eq!(
            color_scheme(&ThemePreference::Dark, &rufin_core::themes::builtins()),
            adw::ColorScheme::ForceDark
        );
    }

    #[test]
    fn system_appearance_only_overrides_lyrics_tokens() {
        let css = css(ThemePreference::System, AccentPreference::System);
        assert!(!css.contains("--lyrics-highlight-color"));
        assert!(!css.contains("--window-bg-color"));
        assert!(!css.contains("--accent-bg-color"));
    }

    #[test]
    fn explicit_color_schemes_override_surface_tokens_only() {
        let light = css(ThemePreference::Light, AccentPreference::System);
        assert!(light.contains("--window-bg-color: #fafafb"));
        assert!(light.contains("--view-bg-color: #ffffff"));
        assert!(!light.contains("--accent-bg-color"));

        let dark = css(ThemePreference::Dark, AccentPreference::System);
        assert!(dark.contains("--window-bg-color: #222226"));
        assert!(dark.contains("--view-bg-color: #1d1d20"));
        assert!(!dark.contains("--accent-bg-color"));
    }

    #[test]
    fn every_explicit_accent_overrides_the_accent_tokens() {
        let expected = [
            (AccentPreference::Blue, "#3584e4"),
            (AccentPreference::Teal, "#2190a4"),
            (AccentPreference::Green, "#3a944a"),
            (AccentPreference::Yellow, "#c88800"),
            (AccentPreference::Orange, "#ed5b00"),
            (AccentPreference::Red, "#e62d42"),
            (AccentPreference::Pink, "#d56199"),
            (AccentPreference::Purple, "#9141ac"),
            (AccentPreference::Slate, "#6f8396"),
        ];
        for (preference, color) in expected {
            let css = css(ThemePreference::System, preference);
            assert!(css.contains(&format!("--accent-bg-color: {color}")));
            assert!(css.contains("--accent-fg-color: #ffffff"));
            assert!(css.contains("--accent-color: oklab("));
            assert!(!css.contains("--window-bg-color"));
        }
    }
}
