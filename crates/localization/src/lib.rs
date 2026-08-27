use std::{
    collections::{BTreeSet, HashMap},
    env, fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

const DOMAIN: &str = "rufin";
const ENGLISH_LANGUAGE_PREFERENCE: &str = "en";
const MO_MAGIC_LE: u32 = 0x9504_12de;
const MO_MAGIC_BE: u32 = 0xde12_0495;
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
static I18N_STATE: OnceLock<Mutex<I18nState>> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageOption {
    pub id: String,
    pub title: String,
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
    if let Ok(value) = env::var("RUFIN_LANGUAGE") {
        return sanitize_language_preference(&value);
    }
    sanitize_language_preference(saved_language_preference)
}

pub fn language_options() -> Vec<LanguageOption> {
    let mut seen = BTreeSet::new();
    let mut options = Vec::new();
    options.push(LanguageOption {
        id: default_language_preference(),
        title: tr("System default"),
    });
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
    state()
        .lock()
        .map(|state| state.catalog.translate(message))
        .unwrap_or_else(|_| message.to_string())
}

pub fn trn(singular: &str, plural: &str, count: u64) -> String {
    state()
        .lock()
        .map(|state| state.catalog.translate_plural(singular, plural, count))
        .unwrap_or_else(|_| english_plural(singular, plural, count).to_string())
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
        &[("count", label.as_str())],
    )
}

pub fn result_count_text(count: u64) -> String {
    let label = count.to_string();
    trn_with(
        "{count} result",
        "{count} results",
        count,
        &[("count", label.as_str())],
    )
}

pub fn track_count_text(count: u64) -> String {
    let label = count.to_string();
    trn_with(
        "{count} track",
        "{count} tracks",
        count,
        &[("count", label.as_str())],
    )
}

pub const fn msgid(message: &'static str) -> &'static str {
    message
}

pub fn set_language_preference(language_preference: &str) {
    if let Ok(mut state) = state().lock() {
        state.set_language_preference(language_preference);
    }
}

fn state() -> &'static Mutex<I18nState> {
    I18N_STATE.get_or_init(|| Mutex::new(I18nState::new(&default_language_preference())))
}

#[derive(Clone, Debug)]
struct I18nState {
    language_preference: String,
    catalog: Catalog,
}

impl I18nState {
    fn new(language_preference: &str) -> Self {
        let language_preference = sanitize_language_preference(language_preference);
        let catalog = Catalog::load(&language_preference);
        Self {
            language_preference,
            catalog,
        }
    }

    fn set_language_preference(&mut self, language_preference: &str) {
        let language_preference = sanitize_language_preference(language_preference);
        if self.language_preference == language_preference {
            return;
        }
        self.catalog = Catalog::load(&language_preference);
        self.language_preference = language_preference;
    }
}

#[derive(Clone, Debug, Default)]
struct Catalog {
    messages: HashMap<String, Vec<String>>,
    plural_rule: PluralRule,
}

impl Catalog {
    fn load(language_preference: &str) -> Self {
        let language_preference = catalog_language_preference(language_preference);
        if is_english_language(&language_preference) {
            return Self::default();
        }
        let localedir = locale_dir();
        for candidate in catalog_language_candidates(&language_preference) {
            let path = localedir
                .join(candidate)
                .join("LC_MESSAGES")
                .join(format!("{DOMAIN}.mo"));
            if let Some(catalog) = Self::from_mo_path(&path) {
                return catalog;
            }
        }
        Self::default()
    }

    fn from_mo_path(path: &Path) -> Option<Self> {
        let bytes = fs::read(path).ok()?;
        Self::from_mo_bytes(&bytes)
    }

    fn from_mo_bytes(bytes: &[u8]) -> Option<Self> {
        let magic = read_u32_le(bytes, 0)?;
        let endian = if magic == MO_MAGIC_LE {
            Endian::Little
        } else if magic == MO_MAGIC_BE {
            Endian::Big
        } else {
            return None;
        };
        let count = read_u32(bytes, 8, endian)? as usize;
        let originals = read_u32(bytes, 12, endian)? as usize;
        let translations = read_u32(bytes, 16, endian)? as usize;
        let mut catalog = Self::default();

        for index in 0..count {
            let original = mo_string(bytes, originals, index, endian)?;
            let translated = mo_string(bytes, translations, index, endian)?;
            let original = String::from_utf8_lossy(original).to_string();
            let translated = String::from_utf8_lossy(translated).to_string();
            if original.is_empty() {
                catalog.plural_rule = PluralRule::from_header(&translated);
                continue;
            }
            catalog.messages.insert(
                original,
                translated.split('\0').map(ToOwned::to_owned).collect(),
            );
        }

        Some(catalog)
    }

    fn translate(&self, message: &str) -> String {
        self.messages
            .get(message)
            .and_then(|forms| forms.first())
            .map(|translated| non_empty_translation(message, translated.clone()))
            .unwrap_or_else(|| message.to_string())
    }

    fn translate_plural(&self, singular: &str, plural: &str, count: u64) -> String {
        let key = format!("{singular}\0{plural}");
        if let Some(forms) = self.messages.get(&key)
            && !forms.is_empty()
        {
            let index = self.plural_rule.index(count).min(forms.len() - 1);
            return non_empty_translation(
                english_plural(singular, plural, count),
                forms[index].clone(),
            );
        }
        english_plural(singular, plural, count).to_string()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PluralRule {
    nplurals: usize,
    expression: PluralExpr,
}

impl Default for PluralRule {
    fn default() -> Self {
        Self {
            nplurals: 2,
            expression: PluralExpr::Binary(
                PluralBinaryOp::Ne,
                Box::new(PluralExpr::N),
                Box::new(PluralExpr::Number(1)),
            ),
        }
    }
}

impl PluralRule {
    fn from_header(header: &str) -> Self {
        let Some(forms) = header
            .lines()
            .find_map(|line| line.strip_prefix("Plural-Forms:"))
        else {
            return Self::default();
        };
        let nplurals = forms
            .split(';')
            .find_map(|part| part.trim().strip_prefix("nplurals="))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(2);
        let expression = forms
            .split(';')
            .find_map(|part| part.trim().strip_prefix("plural="))
            .and_then(|value| PluralParser::new(value.trim()).parse())
            .unwrap_or_else(|| Self::default().expression);
        Self {
            nplurals,
            expression,
        }
    }

    fn index(&self, count: u64) -> usize {
        let value = self.expression.eval(count);
        if value <= 0 {
            0
        } else {
            (value as usize).min(self.nplurals.saturating_sub(1))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PluralExpr {
    N,
    Number(i64),
    Not(Box<PluralExpr>),
    Neg(Box<PluralExpr>),
    Binary(PluralBinaryOp, Box<PluralExpr>, Box<PluralExpr>),
    Ternary(Box<PluralExpr>, Box<PluralExpr>, Box<PluralExpr>),
}

impl PluralExpr {
    fn eval(&self, count: u64) -> i64 {
        match self {
            Self::N => count.min(i64::MAX as u64) as i64,
            Self::Number(value) => *value,
            Self::Not(value) => i64::from(value.eval(count) == 0),
            Self::Neg(value) => -value.eval(count),
            Self::Binary(PluralBinaryOp::Or, left, right) => {
                i64::from(left.eval(count) != 0 || right.eval(count) != 0)
            }
            Self::Binary(PluralBinaryOp::And, left, right) => {
                i64::from(left.eval(count) != 0 && right.eval(count) != 0)
            }
            Self::Binary(op, left, right) => {
                let left = left.eval(count);
                let right = right.eval(count);
                match op {
                    PluralBinaryOp::Eq => i64::from(left == right),
                    PluralBinaryOp::Ne => i64::from(left != right),
                    PluralBinaryOp::Lt => i64::from(left < right),
                    PluralBinaryOp::Le => i64::from(left <= right),
                    PluralBinaryOp::Gt => i64::from(left > right),
                    PluralBinaryOp::Ge => i64::from(left >= right),
                    PluralBinaryOp::Add => left.saturating_add(right),
                    PluralBinaryOp::Sub => left.saturating_sub(right),
                    PluralBinaryOp::Mul => left.saturating_mul(right),
                    PluralBinaryOp::Div => {
                        if right == 0 {
                            0
                        } else {
                            left / right
                        }
                    }
                    PluralBinaryOp::Rem => {
                        if right == 0 {
                            0
                        } else {
                            left % right
                        }
                    }
                    PluralBinaryOp::Or => i64::from(left != 0 || right != 0),
                    PluralBinaryOp::And => i64::from(left != 0 && right != 0),
                }
            }
            Self::Ternary(condition, when_true, when_false) => {
                if condition.eval(count) != 0 {
                    when_true.eval(count)
                } else {
                    when_false.eval(count)
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PluralBinaryOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

struct PluralParser<'a> {
    input: &'a str,
    cursor: usize,
}

impl<'a> PluralParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, cursor: 0 }
    }

    fn parse(mut self) -> Option<PluralExpr> {
        let expression = self.parse_ternary()?;
        self.skip_ws();
        (self.cursor == self.input.len()).then_some(expression)
    }

    fn parse_ternary(&mut self) -> Option<PluralExpr> {
        let condition = self.parse_or()?;
        if !self.take("?") {
            return Some(condition);
        }
        let when_true = self.parse_ternary()?;
        if !self.take(":") {
            return None;
        }
        let when_false = self.parse_ternary()?;
        Some(PluralExpr::Ternary(
            Box::new(condition),
            Box::new(when_true),
            Box::new(when_false),
        ))
    }

    fn parse_or(&mut self) -> Option<PluralExpr> {
        let mut expression = self.parse_and()?;
        while self.take("||") {
            expression = PluralExpr::Binary(
                PluralBinaryOp::Or,
                Box::new(expression),
                Box::new(self.parse_and()?),
            );
        }
        Some(expression)
    }

    fn parse_and(&mut self) -> Option<PluralExpr> {
        let mut expression = self.parse_equality()?;
        while self.take("&&") {
            expression = PluralExpr::Binary(
                PluralBinaryOp::And,
                Box::new(expression),
                Box::new(self.parse_equality()?),
            );
        }
        Some(expression)
    }

    fn parse_equality(&mut self) -> Option<PluralExpr> {
        let mut expression = self.parse_relation()?;
        loop {
            let op = if self.take("==") {
                PluralBinaryOp::Eq
            } else if self.take("!=") {
                PluralBinaryOp::Ne
            } else {
                return Some(expression);
            };
            expression =
                PluralExpr::Binary(op, Box::new(expression), Box::new(self.parse_relation()?));
        }
    }

    fn parse_relation(&mut self) -> Option<PluralExpr> {
        let mut expression = self.parse_add()?;
        loop {
            let op = if self.take("<=") {
                PluralBinaryOp::Le
            } else if self.take(">=") {
                PluralBinaryOp::Ge
            } else if self.take("<") {
                PluralBinaryOp::Lt
            } else if self.take(">") {
                PluralBinaryOp::Gt
            } else {
                return Some(expression);
            };
            expression = PluralExpr::Binary(op, Box::new(expression), Box::new(self.parse_add()?));
        }
    }

    fn parse_add(&mut self) -> Option<PluralExpr> {
        let mut expression = self.parse_mul()?;
        loop {
            let op = if self.take("+") {
                PluralBinaryOp::Add
            } else if self.take("-") {
                PluralBinaryOp::Sub
            } else {
                return Some(expression);
            };
            expression = PluralExpr::Binary(op, Box::new(expression), Box::new(self.parse_mul()?));
        }
    }

    fn parse_mul(&mut self) -> Option<PluralExpr> {
        let mut expression = self.parse_unary()?;
        loop {
            let op = if self.take("*") {
                PluralBinaryOp::Mul
            } else if self.take("/") {
                PluralBinaryOp::Div
            } else if self.take("%") {
                PluralBinaryOp::Rem
            } else {
                return Some(expression);
            };
            expression =
                PluralExpr::Binary(op, Box::new(expression), Box::new(self.parse_unary()?));
        }
    }

    fn parse_unary(&mut self) -> Option<PluralExpr> {
        if self.take("!") {
            Some(PluralExpr::Not(Box::new(self.parse_unary()?)))
        } else if self.take("-") {
            Some(PluralExpr::Neg(Box::new(self.parse_unary()?)))
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Option<PluralExpr> {
        self.skip_ws();
        if self.take("n") {
            return Some(PluralExpr::N);
        }
        if self.take("(") {
            let expression = self.parse_ternary()?;
            return self.take(")").then_some(expression);
        }
        self.parse_number().map(PluralExpr::Number)
    }

    fn parse_number(&mut self) -> Option<i64> {
        self.skip_ws();
        let start = self.cursor;
        while self
            .input
            .as_bytes()
            .get(self.cursor)
            .is_some_and(u8::is_ascii_digit)
        {
            self.cursor += 1;
        }
        (self.cursor > start).then(|| self.input.get(start..self.cursor)?.parse::<i64>().ok())?
    }

    fn take(&mut self, token: &str) -> bool {
        self.skip_ws();
        if self
            .input
            .get(self.cursor..)
            .is_some_and(|input| input.starts_with(token))
        {
            self.cursor += token.len();
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) {
        while self
            .input
            .as_bytes()
            .get(self.cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.cursor += 1;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Endian {
    Little,
    Big,
}

fn catalog_language_preference(language_preference: &str) -> String {
    let language_preference = sanitize_language_preference(language_preference);
    if language_preference == SYSTEM_LANGUAGE_PREFERENCE {
        return system_language_preference()
            .unwrap_or_else(|| ENGLISH_LANGUAGE_PREFERENCE.to_string());
    }
    language_preference
}

fn system_language_preference() -> Option<String> {
    for key in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        let Ok(value) = env::var(key) else {
            continue;
        };
        for item in value.split(':') {
            let language = sanitize_language_preference(item);
            if !language.is_empty() {
                return Some(language);
            }
        }
    }
    None
}

fn catalog_language_candidates(language_preference: &str) -> Vec<String> {
    let normalized = sanitize_language_preference(language_preference).replace('-', "_");
    let mut candidates = Vec::new();
    push_candidate(&mut candidates, normalized.clone());
    if let Some(base) = normalized.split(['.', '@']).next() {
        push_candidate(&mut candidates, base.to_string());
    }
    push_candidate(&mut candidates, language_code(&normalized).to_string());
    for id in available_translation_language_ids() {
        if language_code(&id) == language_code(&normalized) {
            push_candidate(&mut candidates, id);
        }
    }
    candidates
}

fn push_candidate(candidates: &mut Vec<String>, candidate: impl Into<String>) {
    let candidate = candidate.into();
    if !candidate.is_empty() && !candidates.iter().any(|existing| existing == &candidate) {
        candidates.push(candidate);
    }
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize, endian: Endian) -> Option<u32> {
    let data = bytes.get(offset..offset + 4)?.try_into().ok()?;
    Some(match endian {
        Endian::Little => u32::from_le_bytes(data),
        Endian::Big => u32::from_be_bytes(data),
    })
}

fn mo_string(bytes: &[u8], table_offset: usize, index: usize, endian: Endian) -> Option<&[u8]> {
    let offset = table_offset.checked_add(index.checked_mul(8)?)?;
    let length = read_u32(bytes, offset, endian)? as usize;
    let string_offset = read_u32(bytes, offset + 4, endian)? as usize;
    bytes.get(string_offset..string_offset.checked_add(length)?)
}

fn replace_placeholders(mut text: String, args: &[(&str, &str)]) -> String {
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

fn english_plural<'a>(singular: &'a str, plural: &'a str, count: u64) -> &'a str {
    if count == 1 { singular } else { plural }
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

    for candidate in locale_dir_candidates() {
        if candidate.exists() {
            return candidate;
        }
    }

    PathBuf::from("locales")
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
        if !path
            .join("LC_MESSAGES")
            .join(format!("{DOMAIN}.mo"))
            .is_file()
        {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(sanitize_language_preference) else {
            continue;
        };
        if id != SYSTEM_LANGUAGE_PREFERENCE {
            ids.insert(id);
        }
    }
}

fn collect_po_language_ids(localedir: &Path, ids: &mut BTreeSet<String>) {
    let Ok(entries) = fs::read_dir(localedir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("po") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if stem == DOMAIN {
            continue;
        }
        let id = sanitize_language_preference(stem);
        if id != SYSTEM_LANGUAGE_PREFERENCE {
            ids.insert(id);
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

fn non_empty_translation(message: &str, translated: String) -> String {
    if translated.is_empty() && !message.is_empty() {
        message.to_string()
    } else {
        translated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i18n_language_system() {
        let options = vec![
            LanguageOption {
                id: SYSTEM_LANGUAGE_PREFERENCE.to_string(),
                title: "System default".to_string(),
            },
            LanguageOption {
                id: ENGLISH_LANGUAGE_PREFERENCE.to_string(),
                title: "English".to_string(),
            },
            LanguageOption {
                id: "de_DE".to_string(),
                title: "German".to_string(),
            },
        ];

        assert_eq!(language_option_index(&options, "C"), 1);
        assert_eq!(language_option_index(&options, "de_DE"), 2);
        assert_eq!(language_option_index(&options, "missing"), 0);
    }

    #[test]
    fn i18n_use_name() {
        assert_eq!(language_display_name("en_US"), "English");
        assert_eq!(language_display_name("et"), "Eesti");
        assert_eq!(language_display_name("tr"), "Türkçe");
        assert_eq!(language_display_name("ja"), "日本語");
        assert_eq!(language_display_name("zz_ZZ"), "zz_ZZ");
    }

    #[test]
    fn i18n_fall_empty() {
        assert_eq!(non_empty_translation("Previous", String::new()), "Previous");
        assert_eq!(non_empty_translation("", String::new()), "");
        assert_eq!(
            non_empty_translation("Play", "Translated Play".to_string()),
            "Translated Play"
        );
    }

    #[test]
    fn i18n_catalog_reads_mo() {
        let bytes = test_mo(&[
            (
                "",
                "Content-Type: text/plain; charset=UTF-8\nPlural-Forms: nplurals=2; plural=n != 1;\n",
            ),
            ("Play", "Mängi"),
            ("track\0tracks", "lugu\0lood"),
        ]);
        let catalog = Catalog::from_mo_bytes(&bytes).expect("catalog parses");

        assert_eq!(catalog.translate("Play"), "Mängi");
        assert_eq!(catalog.translate("Missing"), "Missing");
        assert_eq!(catalog.translate_plural("track", "tracks", 1), "lugu");
        assert_eq!(catalog.translate_plural("track", "tracks", 2), "lood");
    }

    #[test]
    fn i18n_plural_rules_cover_current_catalogs() {
        let et = PluralRule::from_header("Plural-Forms: nplurals=2; plural=n != 1;\n");
        assert_eq!(et.index(1), 0);
        assert_eq!(et.index(2), 1);

        let lv = PluralRule::from_header(
            "Plural-Forms: nplurals=3; plural=(n % 10 == 0 || n % 100 >= 11 && n % 100 <= 19) ? 0 : ((n % 10 == 1 && n % 100 != 11) ? 1 : 2);\n",
        );
        assert_eq!(lv.index(0), 0);
        assert_eq!(lv.index(1), 1);
        assert_eq!(lv.index(2), 2);

        let ru = PluralRule::from_header(
            "Plural-Forms: nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);\n",
        );
        assert_eq!(ru.index(1), 0);
        assert_eq!(ru.index(2), 1);
        assert_eq!(ru.index(5), 2);
    }

    #[test]
    fn i18n_replaces_named_placeholders() {
        assert_eq!(
            replace_placeholders("Found {count} items".to_string(), &[("count", "3")]),
            "Found 3 items"
        );
    }

    fn test_mo(entries: &[(&str, &str)]) -> Vec<u8> {
        let count = entries.len();
        let originals = 28usize;
        let translations = originals + count * 8;
        let mut bytes = vec![0; translations + count * 8];
        write_test_u32(&mut bytes, 0, MO_MAGIC_LE);
        write_test_u32(&mut bytes, 8, count as u32);
        write_test_u32(&mut bytes, 12, originals as u32);
        write_test_u32(&mut bytes, 16, translations as u32);

        for (index, (original, _)) in entries.iter().enumerate() {
            write_test_string(&mut bytes, originals + index * 8, original);
        }
        for (index, (_, translation)) in entries.iter().enumerate() {
            write_test_string(&mut bytes, translations + index * 8, translation);
        }

        bytes
    }

    fn write_test_string(bytes: &mut Vec<u8>, entry_offset: usize, value: &str) {
        let string_offset = bytes.len();
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(0);
        write_test_u32(bytes, entry_offset, value.len() as u32);
        write_test_u32(bytes, entry_offset + 4, string_offset as u32);
    }

    fn write_test_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}
