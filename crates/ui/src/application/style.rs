use crate::{AccentPreference, Settings, ThemePreference};

const LIGHT_SURFACE_TOKENS: &str = r#"  --window-bg-color: #fafafb;
  --window-fg-color: rgb(0 0 6 / 80%);
  --view-bg-color: #ffffff;
  --view-fg-color: rgb(0 0 6 / 80%);
  --headerbar-bg-color: #ffffff;
  --headerbar-fg-color: rgb(0 0 6 / 80%);
  --headerbar-border-color: rgb(0 0 6 / 80%);
  --headerbar-backdrop-color: #fafafb;
  --headerbar-shade-color: rgb(0 0 6 / 12%);
  --headerbar-darker-shade-color: rgb(0 0 6 / 12%);
  --sidebar-bg-color: #ebebed;
  --sidebar-fg-color: rgb(0 0 6 / 80%);
  --sidebar-backdrop-color: #f2f2f4;
  --sidebar-shade-color: rgb(0 0 6 / 7%);
  --sidebar-border-color: rgb(0 0 6 / 7%);
  --secondary-sidebar-bg-color: #f3f3f5;
  --secondary-sidebar-fg-color: rgb(0 0 6 / 80%);
  --secondary-sidebar-backdrop-color: #f6f6fa;
  --secondary-sidebar-shade-color: rgb(0 0 6 / 7%);
  --secondary-sidebar-border-color: rgb(0 0 6 / 7%);
  --card-bg-color: #ffffff;
  --card-fg-color: rgb(0 0 6 / 80%);
  --card-shade-color: rgb(0 0 6 / 7%);
  --dialog-bg-color: #fafafb;
  --dialog-fg-color: rgb(0 0 6 / 80%);
  --popover-bg-color: #ffffff;
  --popover-fg-color: rgb(0 0 6 / 80%);
  --popover-shade-color: rgb(0 0 6 / 7%);
  --thumbnail-bg-color: #ffffff;
  --thumbnail-fg-color: rgb(0 0 6 / 80%);
  --shade-color: rgb(0 0 6 / 7%);
  --scrollbar-outline-color: #ffffff;
  --active-toggle-bg-color: #ffffff;
  --active-toggle-fg-color: rgb(0 0 6 / 80%);
  --overview-bg-color: #f3f3f5;
  --overview-fg-color: rgb(0 0 6 / 80%);
  --standalone-color-oklab: min(l, 0.5) a b;
"#;

const DARK_SURFACE_TOKENS: &str = r#"  --window-bg-color: #222226;
  --window-fg-color: #ffffff;
  --view-bg-color: #1d1d20;
  --view-fg-color: #ffffff;
  --headerbar-bg-color: #2e2e32;
  --headerbar-fg-color: #ffffff;
  --headerbar-border-color: #ffffff;
  --headerbar-backdrop-color: #28282c;
  --headerbar-shade-color: rgb(0 0 6 / 36%);
  --headerbar-darker-shade-color: rgb(0 0 12 / 90%);
  --sidebar-bg-color: #2e2e32;
  --sidebar-fg-color: #ffffff;
  --sidebar-backdrop-color: #28282c;
  --sidebar-shade-color: rgb(0 0 6 / 25%);
  --sidebar-border-color: rgb(0 0 6 / 36%);
  --secondary-sidebar-bg-color: #28282c;
  --secondary-sidebar-fg-color: #ffffff;
  --secondary-sidebar-backdrop-color: #252529;
  --secondary-sidebar-shade-color: rgb(0 0 6 / 25%);
  --secondary-sidebar-border-color: rgb(0 0 6 / 36%);
  --card-bg-color: rgb(255 255 255 / 8%);
  --card-fg-color: #ffffff;
  --card-shade-color: rgb(0 0 6 / 36%);
  --dialog-bg-color: #36363a;
  --dialog-fg-color: #ffffff;
  --popover-bg-color: #36363a;
  --popover-fg-color: #ffffff;
  --popover-shade-color: rgb(0 0 6 / 25%);
  --thumbnail-bg-color: #39393d;
  --thumbnail-fg-color: #ffffff;
  --shade-color: rgb(0 0 6 / 25%);
  --scrollbar-outline-color: rgb(0 0 12 / 95%);
  --active-toggle-bg-color: rgb(255 255 255 / 20%);
  --active-toggle-fg-color: #ffffff;
  --overview-bg-color: #28282c;
  --overview-fg-color: #ffffff;
  --standalone-color-oklab: max(l, 0.85) a b;
"#;

pub(crate) struct ApplicationAppearance {
    override_provider: gtk::CssProvider,
}

impl ApplicationAppearance {
    pub(crate) fn install() -> Self {
        let override_provider = gtk::CssProvider::new();
        let Some(display) = gtk::gdk::Display::default() else {
            return Self { override_provider };
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
            &override_provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
        );

        Self { override_provider }
    }

    pub(crate) fn apply(&self, settings: &Settings) {
        adw::StyleManager::default().set_color_scheme(color_scheme(settings.theme_preference));
        self.override_provider
            .load_from_string(&appearance_override_css(
                settings.theme_preference,
                settings.accent_preference,
                &settings.lyrics,
            ));
    }
}

fn color_scheme(preference: ThemePreference) -> adw::ColorScheme {
    match preference {
        ThemePreference::System => adw::ColorScheme::PreferLight,
        ThemePreference::Light => adw::ColorScheme::ForceLight,
        ThemePreference::Dark => adw::ColorScheme::ForceDark,
    }
}

fn appearance_override_css(
    theme: ThemePreference,
    accent: AccentPreference,
    lyrics: &lyrics::Settings,
) -> String {
    let surface_tokens = match theme {
        ThemePreference::System => "",
        ThemePreference::Light => LIGHT_SURFACE_TOKENS,
        ThemePreference::Dark => DARK_SURFACE_TOKENS,
    };
    let accent_color = accent_color(accent);
    let mut css = String::from(":root {\n");
    css.push_str(surface_tokens);
    if let Some(color) = accent_color {
        css.push_str("  --accent-bg-color: ");
        css.push_str(color);
        css.push_str(";\n  --accent-fg-color: #ffffff;\n");
        css.push_str(
            "  --accent-color: oklab(from var(--accent-bg-color) var(--standalone-color-oklab));\n",
        );
    }
    if let Some(color) = lyrics.lyrics_highlight_color.as_deref() {
        css.push_str("  --lyrics-highlight-color: ");
        css.push_str(color);
        css.push_str(";\n");
    }
    css.push_str("}\n");
    let selectors = ".lyrics-line, .lyrics-furigana, .lyrics-romanization, .lyrics-reading-surface, .lyrics-cue";
    if let Some(family) = lyrics.lyrics_font_family.as_deref() {
        css.push_str(selectors);
        css.push_str(" {\n");
        css.push_str("  font-family: '");
        css.push_str(&family.replace('\\', "\\\\").replace('\'', "\\'"));
        css.push_str("', sans-serif;\n");
        css.push_str("}\n");
    }
    if let Some(size) = lyrics.lyrics_font_size {
        css.push_str(&format!(".lyrics-line {{ font-size: {size}px; }}\n"));
    }
    css
}

fn accent_color(preference: AccentPreference) -> Option<&'static str> {
    match preference {
        AccentPreference::System => None,
        AccentPreference::Blue => Some("#3584e4"),
        AccentPreference::Teal => Some("#2190a4"),
        AccentPreference::Green => Some("#3a944a"),
        AccentPreference::Yellow => Some("#c88800"),
        AccentPreference::Orange => Some("#ed5b00"),
        AccentPreference::Red => Some("#e62d42"),
        AccentPreference::Pink => Some("#d56199"),
        AccentPreference::Purple => Some("#9141ac"),
        AccentPreference::Slate => Some("#6f8396"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn css(theme: ThemePreference, accent: AccentPreference) -> String {
        appearance_override_css(theme, accent, &lyrics::Settings::default())
    }

    #[test]
    fn theme_preferences_map_to_explicit_application_color_schemes() {
        assert_eq!(
            color_scheme(ThemePreference::System),
            adw::ColorScheme::PreferLight
        );
        assert_eq!(
            color_scheme(ThemePreference::Light),
            adw::ColorScheme::ForceLight
        );
        assert_eq!(
            color_scheme(ThemePreference::Dark),
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
