use serde::Deserialize;
use std::{collections::BTreeMap, path::Path};

pub const CONTROLLER_COLORS: &[(&str, &str)] = &[
    ("--window-bg-color", "var(--window-bg-color)"),
    ("--window-fg-color", "var(--window-fg-color)"),
    ("--card-bg-color", "var(--card-bg-color)"),
    ("--accent-color", "var(--accent-color)"),
    ("--bg", "var(--view-bg-color)"),
    ("--sidebar", "var(--sidebar-bg-color)"),
    ("--surface", "var(--card-bg-color)"),
    ("--player", "var(--headerbar-bg-color)"),
    ("--text", "var(--view-fg-color)"),
    ("--accent", "var(--accent-color)"),
    ("--blue", "var(--accent-bg-color)"),
    ("--favorite", "var(--error-color)"),
    ("--accent-foreground", "var(--accent-fg-color)"),
    ("--popover", "var(--popover-bg-color)"),
    ("--right-sidebar", "var(--secondary-sidebar-bg-color)"),
    ("--line", "var(--sidebar-border-color)"),
    ("--border-color", "var(--border-color)"),
    ("--hover", "var(--shade-color)"),
    (
        "--muted",
        "color-mix(in srgb, var(--view-fg-color) 60%, transparent)",
    ),
];

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Light,
    Dark,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Accent {
    pub name: String,
    pub color: String,
    pub foreground: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Theme {
    #[serde(skip)]
    pub path: Option<std::path::PathBuf>,
    #[serde(skip)]
    pub id: String,
    pub name: String,
    pub mode: Mode,
    pub colors: BTreeMap<String, String>,
    #[serde(default)]
    pub accents: Vec<Accent>,
}

impl Theme {
    pub fn colors(&self, themes: &[Theme], accent: Option<&str>) -> BTreeMap<String, String> {
        let mut colors = themes[usize::from(self.mode == Mode::Dark)].colors.clone();
        colors.extend(self.colors.clone());
        if let Some(accent) = self.accent(accent) {
            colors.insert("accent-bg-color".into(), accent.color.clone());
            colors.insert("accent-fg-color".into(), accent.foreground.clone());
            colors.insert(
                "accent-color".into(),
                "oklab(from var(--accent-bg-color) var(--standalone-color-oklab))".into(),
            );
        }
        colors
    }

    pub fn accent(&self, name: Option<&str>) -> Option<&Accent> {
        self.accents
            .iter()
            .find(|accent| Some(accent.name.as_str()) == name)
            .or_else(|| self.accents.first())
    }
}

pub fn controller_colors(
    settings: &crate::settings::Settings,
    directory: &Path,
) -> BTreeMap<String, String> {
    let crate::settings::ThemePreference::Named(id) = &settings.theme_preference else {
        return BTreeMap::new();
    };
    let mut themes = builtins();
    if id.starts_with("custom:") {
        let (custom, errors) = custom(directory);
        for error in errors {
            tracing::warn!(%error, "could not load theme");
        }
        themes.extend(custom);
    }
    let Some(theme) = themes.iter().find(|theme| &theme.id == id) else {
        return BTreeMap::new();
    };
    let mut colors: BTreeMap<String, String> = theme
        .colors(&themes, settings.theme_accents.get(id).map(String::as_str))
        .into_iter()
        .map(|(key, value)| (format!("--{key}"), value))
        .collect();
    if theme.accents.is_empty()
        && let Some(color) = settings.accent_preference.color()
    {
        colors.insert("--accent-bg-color".into(), color.into());
        colors.insert("--accent-color".into(), color.into());
        colors.insert("--accent-fg-color".into(), "#ffffff".into());
    }
    colors
        .entry("--accent-bg-color".into())
        .or_insert_with(|| "#3584e4".into());
    colors
        .entry("--accent-fg-color".into())
        .or_insert_with(|| "#ffffff".into());
    colors.entry("--accent-color".into()).or_insert_with(|| {
        "oklab(from var(--accent-bg-color) var(--standalone-color-oklab))".into()
    });
    colors
        .entry("--error-color".into())
        .or_insert_with(|| "#e62d42".into());
    for &(key, value) in CONTROLLER_COLORS {
        if value != format!("var({key})") {
            colors.entry(key.into()).or_insert_with(|| value.into());
        }
    }
    colors.insert(
        "color-scheme".into(),
        match theme.mode {
            Mode::Light => "light",
            Mode::Dark => "dark",
        }
        .into(),
    );
    colors
}

pub fn builtins() -> Vec<Theme> {
    macro_rules! themes {
        ($($id:literal),+ $(,)?) => {
            vec![$({
                let mut theme: Theme = serde_json::from_str(include_str!(concat!("../../../resources/themes/", $id, ".json")))
                    .expect("bundled theme must be valid");
                theme.id = concat!("builtin:", $id).into();
                theme
            }),+]
        };
    }
    themes!(
        "light",
        "dark",
        "catppuccin-latte",
        "catppuccin-frappe",
        "catppuccin-macchiato",
        "catppuccin-mocha",
        "dracula",
        "gruvbox",
        "nord",
        "one-dark",
        "tokyo-night",
        "rose-pine",
        "solarized",
        "everforest"
    )
}

pub fn custom(directory: &Path) -> (Vec<Theme>, Vec<String>) {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Default::default(),
        Err(error) => {
            return (
                Vec::new(),
                vec![format!("{}: {error}", directory.display())],
            );
        }
    };
    let mut themes = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        let result = entry.map_err(|error| error.to_string()).and_then(|entry| {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|extension| !extension.eq_ignore_ascii_case("json"))
            {
                return Ok(None);
            }
            let read = || -> Result<Theme, Box<dyn std::error::Error>> {
                let mut theme: Theme = serde_json::from_slice(&std::fs::read(&path)?)?;
                theme.id = format!(
                    "custom:{}",
                    path.file_stem().unwrap_or_default().to_string_lossy()
                );
                theme.path = Some(path.clone());
                Ok(theme)
            };
            read()
                .map(Some)
                .map_err(|error| format!("{}: {error}", path.display()))
        });
        match result {
            Ok(Some(theme)) => themes.push(theme),
            Ok(None) => (),
            Err(error) => errors.push(error),
        }
    }
    themes.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
    (themes, errors)
}

pub fn create_folder(directory: &Path) -> std::io::Result<bool> {
    std::fs::create_dir_all(directory)?;
    if std::fs::read_dir(directory)?.next().is_none() {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join("custom.json"))?
            .write_all(include_bytes!("../../../resources/themes/custom.json"))?;
        return Ok(true);
    }
    Ok(false)
}
