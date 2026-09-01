use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};

pub const DOMAIN: &str = "rufin";
const ENGLISH_LANGUAGE_PREFERENCE: &str = "en";
pub const SYSTEM_LANGUAGE_PREFERENCE: &str = "system";
pub const TRANSLATOR_CREDITS: &str =
    include_str!(concat!(env!("OUT_DIR"), "/translator_credits.txt"));

const LANGUAGE_NAMES: &[(&str, &str)] = &[
    ("af", "Afrikaans"),
    ("ar", "العربية"),
    ("az", "Azərbaycan"),
    ("be", "Беларуская"),
    ("bg", "Български"),
    ("bn", "বাংলা"),
    ("bs", "Bosanski"),
    ("ca", "Català"),
    ("cs", "Čeština"),
    ("cy", "Cymraeg"),
    ("da", "Dansk"),
    ("de", "Deutsch"),
    ("el", "Ελληνικά"),
    ("en", "English"),
    ("eo", "Esperanto"),
    ("es", "Español"),
    ("et", "Eesti"),
    ("eu", "Euskara"),
    ("fa", "فارسی"),
    ("fi", "Suomi"),
    ("fr", "Français"),
    ("ga", "Gaeilge"),
    ("gl", "Galego"),
    ("he", "עברית"),
    ("hi", "हिन्दी"),
    ("hr", "Hrvatski"),
    ("hu", "Magyar"),
    ("hy", "Հայերեն"),
    ("id", "Indonesia"),
    ("is", "Íslenska"),
    ("it", "Italiano"),
    ("ja", "日本語"),
    ("ka", "ქართული"),
    ("kk", "Қазақша"),
    ("ko", "한국어"),
    ("lt", "Lietuvių"),
    ("lv", "Latviešu"),
    ("mk", "Македонски"),
    ("ms", "Melayu"),
    ("nb", "Norsk bokmål"),
    ("nl", "Nederlands"),
    ("nn", "Norsk nynorsk"),
    ("pl", "Polski"),
    ("pt", "Português"),
    ("ro", "Română"),
    ("ru", "Русский"),
    ("sk", "Slovenčina"),
    ("sl", "Slovenščina"),
    ("sq", "Shqip"),
    ("sr", "Српски"),
    ("sv", "Svenska"),
    ("th", "ไทย"),
    ("tr", "Türkçe"),
    ("uk", "Українська"),
    ("vi", "Tiếng Việt"),
    ("zh", "中文"),
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageOption {
    pub id: String,
    pub title: String,
}

/// Binds the Rufin gettext domain before GTK initializes the process locale.
pub fn initialize() -> Result<(), String> {
    let localedir = locale_dir();
    gettextrs::bindtextdomain(DOMAIN, &localedir).map_err(|error| {
        format!(
            "could not bind {DOMAIN} translations at {}: {error}",
            localedir.display()
        )
    })?;
    gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8")
        .map_err(|error| format!("could not select UTF-8 for {DOMAIN}: {error}"))?;
    gettextrs::textdomain(DOMAIN)
        .map_err(|error| format!("could not select {DOMAIN} text domain: {error}"))?;
    Ok(())
}

pub fn default_language_preference() -> String {
    SYSTEM_LANGUAGE_PREFERENCE.to_string()
}

pub fn sanitize_language_preference(value: &str) -> String {
    let value = value.trim();
    if value.is_empty()
        || value.eq_ignore_ascii_case("default")
        || value.eq_ignore_ascii_case(SYSTEM_LANGUAGE_PREFERENCE)
    {
        return default_language_preference();
    }
    if value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'@'))
    {
        return default_language_preference();
    }
    value.to_string()
}

pub fn effective_language_preference(saved_language_preference: &str) -> String {
    selected_language_preference(
        saved_language_preference,
        env::var("RUFIN_LANGUAGE").ok().as_deref(),
    )
}

pub fn process_language(saved_language_preference: &str) -> Option<String> {
    let language = effective_language_preference(saved_language_preference);
    (language != SYSTEM_LANGUAGE_PREFERENCE).then_some(language)
}

fn selected_language_preference(
    saved_language_preference: &str,
    environment_override: Option<&str>,
) -> String {
    sanitize_language_preference(environment_override.unwrap_or(saved_language_preference))
}

pub fn language_options() -> Vec<LanguageOption> {
    let mut seen = BTreeSet::new();
    let mut options = vec![LanguageOption {
        id: default_language_preference(),
        title: tr("System default"),
    }];
    seen.insert(default_language_preference());
    options.push(LanguageOption {
        id: ENGLISH_LANGUAGE_PREFERENCE.to_string(),
        title: language_display_name(ENGLISH_LANGUAGE_PREFERENCE),
    });
    seen.insert(ENGLISH_LANGUAGE_PREFERENCE.to_string());

    for id in available_translation_language_ids() {
        let id = sanitize_language_preference(&id);
        if id == SYSTEM_LANGUAGE_PREFERENCE || is_english_language(&id) || !seen.insert(id.clone())
        {
            continue;
        }
        options.push(LanguageOption {
            title: language_display_name(&id),
            id,
        });
    }
    options
}

pub fn language_option_index(options: &[LanguageOption], language_preference: &str) -> u32 {
    let language_preference = sanitize_language_preference(language_preference);
    let language_preference = if is_english_language(&language_preference) {
        ENGLISH_LANGUAGE_PREFERENCE
    } else {
        &language_preference
    };
    options
        .iter()
        .position(|option| option.id == language_preference)
        .unwrap_or_default() as u32
}

pub fn tr(message: &str) -> String {
    gettextrs::dgettext(DOMAIN, message)
}

pub fn trn(singular: &str, plural: &str, count: u64) -> String {
    gettextrs::dngettext(
        DOMAIN,
        singular,
        plural,
        count.min(u64::from(u32::MAX)) as u32,
    )
}

pub fn tr_with(message: &str, args: &[(&str, &str)]) -> String {
    replace_placeholders(tr(message), args)
}

pub fn trn_with(singular: &str, plural: &str, count: u64, args: &[(&str, &str)]) -> String {
    replace_placeholders(trn(singular, plural, count), args)
}

pub fn album_count_text(count: u64) -> String {
    let label = count.to_string();
    trn_with(
        "{count} album",
        "{count} albums",
        count,
        &[("count", &label)],
    )
}

pub fn result_count_text(count: u64) -> String {
    let label = count.to_string();
    trn_with(
        "{count} result",
        "{count} results",
        count,
        &[("count", &label)],
    )
}

pub fn track_count_text(count: u64) -> String {
    let label = count.to_string();
    trn_with(
        "{count} track",
        "{count} tracks",
        count,
        &[("count", &label)],
    )
}

pub const fn msgid(message: &'static str) -> &'static str {
    message
}

fn replace_placeholders(mut text: String, args: &[(&str, &str)]) -> String {
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

fn is_english_language(language_preference: &str) -> bool {
    let language_preference = language_preference.replace('-', "_");
    matches!(language_preference.as_str(), "C" | "POSIX" | "en")
        || language_preference.starts_with("C.")
        || language_preference.starts_with("en_")
        || language_preference.starts_with("en.")
}

fn locale_dir() -> PathBuf {
    if let Some(path) = env::var_os("RUFIN_LOCALEDIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        return path;
    }
    locale_dir_candidates()
        .into_iter()
        .find(|candidate| candidate.exists())
        .unwrap_or_else(|| PathBuf::from("locales"))
}

fn locale_dir_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = option_env!("RUFIN_BUILD_LOCALEDIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
    {
        candidates.push(path);
    }
    if let Some(manifest_dir) = option_env!("CARGO_MANIFEST_DIR") {
        candidates.push(PathBuf::from(manifest_dir).join("../../locales"));
    }
    if let Ok(exe) = env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        candidates.push(exe_dir.join("share/locale"));
        candidates.push(exe_dir.join("../share/locale"));
        candidates.push(exe_dir.join("../Resources/share/locale"));
    }
    candidates.push(PathBuf::from("locales"));
    candidates
}

fn available_translation_language_ids() -> Vec<String> {
    let mut ids = BTreeSet::new();
    let localedir = locale_dir();
    collect_mo_language_ids(&localedir, &mut ids);
    collect_po_language_ids(&localedir, &mut ids);
    ids.into_iter().collect()
}

fn collect_mo_language_ids(localedir: &Path, ids: &mut BTreeSet<String>) {
    let Ok(entries) = fs::read_dir(localedir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.join("LC_MESSAGES").join("rufin.mo").is_file()
            && let Some(id) = entry.file_name().to_str().map(sanitize_language_preference)
            && id != SYSTEM_LANGUAGE_PREFERENCE
        {
            ids.insert(id);
        }
    }
}

fn collect_po_language_ids(localedir: &Path, ids: &mut BTreeSet<String>) {
    let Ok(entries) = fs::read_dir(localedir) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.extension().and_then(|extension| extension.to_str()) != Some("po") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if stem != DOMAIN {
            let id = sanitize_language_preference(stem);
            if id != SYSTEM_LANGUAGE_PREFERENCE {
                ids.insert(id);
            }
        }
    }
}

fn language_display_name(id: &str) -> String {
    let code = language_code(id);
    LANGUAGE_NAMES
        .iter()
        .find_map(|(language, name)| (*language == code).then_some((*name).to_string()))
        .unwrap_or_else(|| id.to_string())
}

fn language_code(id: &str) -> &str {
    id.split(['_', '-', '.', '@'])
        .next()
        .filter(|code| !code.is_empty())
        .unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_preferences_keep_system_and_explicit_values() {
        assert_eq!(sanitize_language_preference("default"), "system");
        assert_eq!(sanitize_language_preference("de-DE.UTF-8"), "de-DE.UTF-8");
        assert_eq!(sanitize_language_preference("invalid/value"), "system");
        assert_eq!(selected_language_preference("system", None), "system");
        assert_eq!(selected_language_preference("et", None), "et");
        assert_eq!(selected_language_preference("et", Some("de")), "de");
        assert_eq!(
            selected_language_preference("et", Some("default")),
            "system"
        );
    }

    #[test]
    fn language_options_select_english_aliases_and_saved_catalogs() {
        let options = vec![
            LanguageOption {
                id: "system".into(),
                title: "System default".into(),
            },
            LanguageOption {
                id: "en".into(),
                title: "English".into(),
            },
            LanguageOption {
                id: "de_DE".into(),
                title: "Deutsch".into(),
            },
        ];
        assert_eq!(language_option_index(&options, "C"), 1);
        assert_eq!(language_option_index(&options, "de_DE"), 2);
        assert_eq!(language_option_index(&options, "missing"), 0);
    }

    #[test]
    fn language_names_are_native_and_unknown_ids_survive() {
        assert_eq!(language_display_name("en_US"), "English");
        assert_eq!(language_display_name("et"), "Eesti");
        assert_eq!(language_display_name("tr"), "Türkçe");
        assert_eq!(language_display_name("ja"), "日本語");
        assert_eq!(language_display_name("zz_ZZ"), "zz_ZZ");
    }

    #[test]
    fn placeholder_replacement_keeps_named_values() {
        assert_eq!(
            replace_placeholders("Found {count} items".to_string(), &[("count", "3")]),
            "Found 3 items"
        );
    }
}
