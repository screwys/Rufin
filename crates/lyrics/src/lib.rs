use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use vibrato_rkyv::{Dictionary, LoadMode, Tokenizer};
use wana_kana::ConvertJapanese;

mod current;
mod events;
mod lyrics;

pub use current::{LyricsContext, LyricsHandle, LyricsService};
pub use events::{CurrentLyrics, CurrentLyricsContent, LyricsEvent};
pub use lyrics::{
    LocalLyricsInput, lyrics_from_search_result, lyrics_to_lrc_text, save_current_lyrics,
    save_lyrics_search_result, search_lyrics, shift_lrc_text_timestamps,
};

pub const LYRICS_PROVIDER_SETTINGS_VERSION: u8 = 1;

const fn msgid(message: &'static str) -> &'static str {
    message
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum ExternalLyricsProvider {
    #[serde(rename = "lrclib")]
    Lrclib,
    #[serde(rename = "netease")]
    Netease,
    #[serde(rename = "genius")]
    Genius,
    #[serde(rename = "simpmusic")]
    SimpMusic,
}

impl ExternalLyricsProvider {
    pub const fn all() -> [Self; 4] {
        [Self::Lrclib, Self::Netease, Self::Genius, Self::SimpMusic]
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Lrclib => msgid("LRCLIB"),
            Self::Netease => msgid("NetEase"),
            Self::Genius => msgid("Genius"),
            Self::SimpMusic => msgid("SimpMusic"),
        }
    }

    pub const fn key(self) -> &'static str {
        match self {
            Self::Lrclib => "lrclib",
            Self::Netease => "netease",
            Self::Genius => "genius",
            Self::SimpMusic => "simpmusic",
        }
    }

    pub fn from_key(value: &str) -> Option<Self> {
        Self::all()
            .into_iter()
            .find(|provider| provider.key() == value)
    }
}

pub fn default_external_lyrics_providers() -> Vec<ExternalLyricsProvider> {
    vec![
        ExternalLyricsProvider::Lrclib,
        ExternalLyricsProvider::Netease,
    ]
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Settings {
    pub external_lyrics_enabled: bool,
    #[serde(default = "default_external_lyrics_providers")]
    pub external_lyrics_providers: Vec<ExternalLyricsProvider>,
    #[serde(default = "default_true")]
    pub prefer_server_lyrics: bool,
    #[serde(default)]
    pub save_fetched_lyrics: bool,
    #[serde(default)]
    pub lyrics_provider_settings_version: u8,
    #[serde(default)]
    pub suppressed_auto_lyrics_track_ids: Vec<String>,
    #[serde(default)]
    pub prefer_translations: bool,
    #[serde(default = "default_translation_language")]
    pub preferred_translation_language: String,
    #[serde(default)]
    pub show_furigana: bool,
    #[serde(default)]
    pub show_romanization: bool,
    #[serde(default)]
    pub karaoke_mode: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics_font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics_font_size: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics_highlight_color: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            external_lyrics_enabled: true,
            external_lyrics_providers: default_external_lyrics_providers(),
            prefer_server_lyrics: true,
            save_fetched_lyrics: false,
            lyrics_provider_settings_version: LYRICS_PROVIDER_SETTINGS_VERSION,
            suppressed_auto_lyrics_track_ids: Vec::new(),
            prefer_translations: false,
            preferred_translation_language: default_translation_language(),
            show_furigana: false,
            show_romanization: false,
            karaoke_mode: false,
            lyrics_font_family: None,
            lyrics_font_size: None,
            lyrics_highlight_color: None,
        }
    }
}

impl Settings {
    pub fn sanitize(&mut self) {
        let mut seen = Vec::new();
        self.external_lyrics_providers.retain(|provider| {
            if seen.contains(provider) {
                false
            } else {
                seen.push(*provider);
                true
            }
        });
        if self.lyrics_provider_settings_version < LYRICS_PROVIDER_SETTINGS_VERSION {
            self.suppressed_auto_lyrics_track_ids.clear();
            self.lyrics_provider_settings_version = LYRICS_PROVIDER_SETTINGS_VERSION;
        }
        self.preferred_translation_language =
            normalize_language_tag(&self.preferred_translation_language)
                .unwrap_or_else(default_translation_language);
        self.lyrics_font_family = self
            .lyrics_font_family
            .take()
            .map(|family| family.trim().chars().take(128).collect::<String>())
            .filter(|family| !family.is_empty());
        self.lyrics_font_size = self.lyrics_font_size.map(|size| size.clamp(12, 28));
        if self
            .lyrics_highlight_color
            .as_deref()
            .is_some_and(|color| !valid_lyrics_color(color))
        {
            self.lyrics_highlight_color = None;
        }
    }

    pub(crate) const fn external_lyrics_network_allowed(&self, private_mode: bool) -> bool {
        self.external_lyrics_enabled && !private_mode
    }

    pub fn move_external_lyrics_provider(
        &mut self,
        provider: ExternalLyricsProvider,
        direction: isize,
    ) -> bool {
        let Some(index) = self
            .external_lyrics_providers
            .iter()
            .position(|candidate| *candidate == provider)
        else {
            return false;
        };
        let Some(next) = index.checked_add_signed(direction) else {
            return false;
        };
        if next >= self.external_lyrics_providers.len() {
            return false;
        }
        self.external_lyrics_providers.swap(index, next);
        true
    }

    pub fn reorder_external_lyrics_provider(
        &mut self,
        source: ExternalLyricsProvider,
        target: ExternalLyricsProvider,
        after: bool,
    ) -> bool {
        if source == target {
            return false;
        }
        let before = self.external_lyrics_providers.clone();
        let Some(source_index) = self
            .external_lyrics_providers
            .iter()
            .position(|provider| *provider == source)
        else {
            return false;
        };
        let provider = self.external_lyrics_providers.remove(source_index);
        let Some(mut target_index) = self
            .external_lyrics_providers
            .iter()
            .position(|provider| *provider == target)
        else {
            self.external_lyrics_providers.insert(
                source_index.min(self.external_lyrics_providers.len()),
                provider,
            );
            return false;
        };
        if after {
            target_index += 1;
        }
        self.external_lyrics_providers.insert(
            target_index.min(self.external_lyrics_providers.len()),
            provider,
        );
        self.external_lyrics_providers != before
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LyricsOrigin {
    Local,
    Native,
    External(ExternalLyricsProvider),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum LyricsRole {
    Original,
    Translation,
    Pronunciation,
}

impl LyricsRole {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Translation => "translation",
            Self::Pronunciation => "pronunciation",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LyricsLine {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_millis: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_millis: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cue_lines: Vec<LyricsCueLine>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LyricsCueLine {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_millis: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_millis: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cues: Vec<LyricsCue>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LyricsCue {
    pub text: String,
    pub start_millis: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_millis: Option<u64>,
    pub byte_start: usize,
    pub byte_end_exclusive: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LyricsAgentRole {
    Main,
    Voice,
    Background,
    Group,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LyricsAgent {
    pub id: String,
    pub role: LyricsAgentRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LyricsDocument {
    pub role: LyricsRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default)]
    pub offset_millis: i64,
    pub lines: Vec<LyricsLine>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<LyricsAgent>,
}

impl LyricsDocument {
    pub fn has_word_timing(&self) -> bool {
        self.lines
            .iter()
            .flat_map(|line| &line.cue_lines)
            .any(|line| !line.cues.is_empty())
    }

    pub fn is_japanese_for_readings(&self) -> bool {
        match self.language.as_deref().and_then(normalize_language_tag) {
            Some(language) => language == "ja",
            None => self.lines.iter().any(|line| {
                contains_japanese_kana(&line.text)
                    || line.cue_lines.iter().any(|cue_line| {
                        contains_japanese_kana(&cue_line.text)
                            || cue_line
                                .cues
                                .iter()
                                .any(|cue| contains_japanese_kana(&cue.text))
                    })
            }),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LyricsContent {
    Instrumental,
    Documents(Vec<LyricsDocument>),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LyricsBundle {
    pub origin: LyricsOrigin,
    pub content: LyricsContent,
}

impl LyricsBundle {
    pub fn instrumental(origin: LyricsOrigin) -> Self {
        Self {
            origin,
            content: LyricsContent::Instrumental,
        }
    }

    pub fn from_documents(origin: LyricsOrigin, documents: Vec<LyricsDocument>) -> Self {
        Self {
            origin,
            content: LyricsContent::Documents(documents),
        }
    }

    pub const fn is_instrumental(&self) -> bool {
        matches!(self.content, LyricsContent::Instrumental)
    }

    pub fn documents(&self) -> &[LyricsDocument] {
        match &self.content {
            LyricsContent::Instrumental => &[],
            LyricsContent::Documents(documents) => documents,
        }
    }

    pub fn documents_mut(&mut self) -> &mut Vec<LyricsDocument> {
        match &mut self.content {
            LyricsContent::Instrumental => panic!("instrumental lyrics do not contain documents"),
            LyricsContent::Documents(documents) => documents,
        }
    }

    pub fn selected_document(&self, settings: &Settings) -> Option<&LyricsDocument> {
        let documents = self.documents();
        if settings.prefer_translations {
            let target = normalize_language_tag(&settings.preferred_translation_language);
            if let Some(document) = documents.iter().find(|document| {
                document.role == LyricsRole::Translation
                    && language_matches(document.language.as_deref(), target.as_deref())
            }) {
                return Some(document);
            }
        }
        documents
            .iter()
            .find(|document| document.role == LyricsRole::Original)
            .or_else(|| documents.first())
    }

    pub fn pronunciation_for(&self, document: &LyricsDocument) -> Option<&LyricsDocument> {
        let documents = self.documents();
        let matching = |language: Option<&str>| {
            documents.iter().find(|candidate| {
                candidate.role == LyricsRole::Pronunciation
                    && match (
                        normalize_language_tag(candidate.language.as_deref().unwrap_or_default()),
                        normalize_language_tag(language.unwrap_or_default()),
                    ) {
                        (Some(candidate), Some(document)) => candidate == document,
                        _ => true,
                    }
            })
        };
        matching(document.language.as_deref()).or_else(|| {
            (document.role == LyricsRole::Translation)
                .then(|| {
                    documents
                        .iter()
                        .find(|candidate| candidate.role == LyricsRole::Original)
                        .and_then(|original| matching(original.language.as_deref()))
                })
                .flatten()
        })
    }

    pub fn has_preferred_translation(&self, settings: &Settings) -> bool {
        let target = normalize_language_tag(&settings.preferred_translation_language);
        self.documents().iter().any(|document| {
            document.role == LyricsRole::Translation
                && language_matches(document.language.as_deref(), target.as_deref())
        })
    }

    pub fn has_original(&self) -> bool {
        self.documents()
            .iter()
            .any(|document| document.role == LyricsRole::Original)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LyricsSearchContent {
    Instrumental,
    Inline {
        synced_lyrics: Option<String>,
        plain_lyrics: Option<String>,
    },
    Deferred,
    Unavailable,
}

impl LyricsSearchContent {
    pub fn synced_lyrics(&self) -> Option<&str> {
        match self {
            Self::Inline { synced_lyrics, .. } => synced_lyrics.as_deref(),
            Self::Instrumental | Self::Deferred | Self::Unavailable => None,
        }
    }

    pub fn plain_lyrics(&self) -> Option<&str> {
        match self {
            Self::Inline { plain_lyrics, .. } => plain_lyrics.as_deref(),
            Self::Instrumental | Self::Deferred | Self::Unavailable => None,
        }
    }

    pub const fn can_preview(&self) -> bool {
        !matches!(self, Self::Unavailable)
    }

    pub const fn can_save(&self) -> bool {
        matches!(self, Self::Inline { .. } | Self::Deferred)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LyricsSearchResult {
    pub provider: ExternalLyricsProvider,
    pub id: String,
    pub track_name: String,
    pub artist_name: String,
    pub album_name: String,
    pub duration_seconds: u32,
    pub content: LyricsSearchContent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LyricsQuery {
    pub artist_name: String,
    pub track_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JapaneseReadingSegment {
    pub surface: String,
    pub furigana: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JapaneseReading {
    pub segments: Vec<JapaneseReadingSegment>,
    pub romanization: String,
}

enum JapaneseReaderState {
    Ready(Tokenizer),
    Unavailable,
}

thread_local! {
    static JAPANESE_READER: RefCell<Option<JapaneseReaderState>> = const { RefCell::new(None) };
}

pub fn japanese_reading(text: &str) -> Option<JapaneseReading> {
    japanese_reading_for_language(text, None)
}

pub fn japanese_reading_for_language(
    text: &str,
    language: Option<&str>,
) -> Option<JapaneseReading> {
    japanese_reading_for_language_options(text, language, true, true)
}

pub fn japanese_reading_for_language_options(
    text: &str,
    language: Option<&str>,
    show_furigana: bool,
    show_romanization: bool,
) -> Option<JapaneseReading> {
    if !japanese_reader_needed(text, language, show_furigana, show_romanization) {
        return None;
    }
    JAPANESE_READER.with(|reader| {
        let mut reader = reader.borrow_mut();
        prepare_japanese_reader_state(&mut reader);
        let JapaneseReaderState::Ready(tokenizer) = reader.as_ref()? else {
            return None;
        };
        let mut worker = tokenizer.new_worker();
        worker.reset_sentence(text);
        worker.tokenize();
        let source_tokens = (0..worker.num_tokens())
            .map(|index| {
                let token = worker.token(index);
                let reading = match token.feature() {
                    "" | "*" => token.surface(),
                    reading => reading,
                };
                (token.surface().to_string(), reading.to_hiragana())
            })
            .collect::<Vec<_>>();
        let romanization = show_romanization
            .then(|| {
                source_tokens
                    .iter()
                    .fold(
                        (String::new(), false),
                        |(mut output, previous_was_word), (surface, reading)| {
                            let is_word =
                                surface.chars().any(|character| character.is_alphanumeric());
                            if previous_was_word && is_word {
                                output.push(' ');
                            }
                            if contains_japanese_script(surface) {
                                output.push_str(&reading.to_romaji());
                            } else {
                                output.push_str(surface);
                            }
                            (output, is_word)
                        },
                    )
                    .0
            })
            .unwrap_or_default();
        let segments = show_furigana
            .then(|| {
                source_tokens
                    .iter()
                    .flat_map(|(surface, reading)| {
                        japanese_reading_segments(surface.clone(), reading.clone())
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(JapaneseReading {
            segments,
            romanization,
        })
    })
}

pub fn japanese_reading_from_romanization(
    text: &str,
    romanization: &str,
) -> Option<JapaneseReading> {
    if !text.chars().any(is_kanji) {
        return None;
    }
    let reading = romanization
        .to_hiragana()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    if reading.is_empty() {
        return None;
    }
    let segments = japanese_reading_segments(text.to_string(), reading);
    segments
        .iter()
        .filter(|segment| segment.surface.chars().any(is_kanji))
        .all(|segment| segment.furigana.is_some())
        .then(|| JapaneseReading {
            segments,
            romanization: romanization.to_string(),
        })
}

fn japanese_reader_needed(
    text: &str,
    language: Option<&str>,
    show_furigana: bool,
    show_romanization: bool,
) -> bool {
    if !show_furigana && !show_romanization {
        return false;
    }
    let japanese = contains_japanese_script(text)
        && (contains_japanese_kana(text)
            || language.and_then(normalize_language_tag).as_deref() == Some("ja"));
    japanese && (show_romanization || show_furigana && text.chars().any(is_kanji))
}

pub fn release_japanese_reader() {
    JAPANESE_READER.with(|reader| {
        *reader.borrow_mut() = None;
    });
}

fn prepare_japanese_reader_state(reader: &mut Option<JapaneseReaderState>) {
    if reader.is_none() {
        *reader = Some(load_japanese_reader());
    }
}

fn load_japanese_reader() -> JapaneseReaderState {
    for path in japanese_dictionary_paths() {
        if !path.is_file() {
            continue;
        }
        match Dictionary::from_path(&path, LoadMode::Validate) {
            Ok(dictionary) => return JapaneseReaderState::Ready(Tokenizer::new(dictionary)),
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "could not load Japanese readings dictionary"
                );
                return JapaneseReaderState::Unavailable;
            }
        }
    }
    tracing::warn!("Japanese readings dictionary is unavailable");
    JapaneseReaderState::Unavailable
}

fn japanese_dictionary_paths() -> Vec<PathBuf> {
    japanese_dictionary_paths_for(std::env::current_exe().ok().as_deref())
}

fn japanese_dictionary_paths_for(executable: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(executable) = executable
        && let Some(directory) = executable.parent()
    {
        paths.push(
            directory
                .join("..")
                .join("Resources")
                .join("share")
                .join("rufin")
                .join("japanese-readings.dic"),
        );
        paths.push(
            directory
                .join("..")
                .join("share")
                .join("rufin")
                .join("japanese-readings.dic"),
        );
        paths.push(
            directory
                .join("share")
                .join("rufin")
                .join("japanese-readings.dic"),
        );
    }
    paths.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("data")
            .join("japanese-readings.dic"),
    );
    paths
}

pub fn contains_japanese_kana(text: &str) -> bool {
    text.chars().any(|character| {
        matches!(
            character as u32,
            0x3040..=0x30ff | 0x31f0..=0x31ff | 0xff66..=0xff9f
        )
    })
}

fn contains_japanese_script(text: &str) -> bool {
    contains_japanese_kana(text) || text.chars().any(is_kanji)
}

fn is_kanji(character: char) -> bool {
    matches!(
        character as u32,
        0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x2fa1f
    )
}

fn japanese_reading_segments(surface: String, reading: String) -> Vec<JapaneseReadingSegment> {
    let mut runs = Vec::<(String, bool)>::new();
    for character in surface.chars() {
        let is_kanji = is_kanji(character);
        if let Some((text, run_is_kanji)) = runs.last_mut()
            && *run_is_kanji == is_kanji
        {
            text.push(character);
        } else {
            runs.push((character.to_string(), is_kanji));
        }
    }
    if !runs.iter().any(|(_, is_kanji)| *is_kanji) {
        return vec![JapaneseReadingSegment {
            surface,
            furigana: None,
        }];
    }

    let normalized_runs = runs
        .iter()
        .map(|(text, is_kanji)| (!is_kanji).then(|| text.to_hiragana().chars().collect::<Vec<_>>()))
        .collect::<Vec<_>>();
    let reading = reading.chars().collect::<Vec<_>>();
    let Some(ranges) = align_japanese_reading_runs(&normalized_runs, &reading, 0, 0) else {
        let reading = (runs.len() == 1).then(|| reading.into_iter().collect());
        return runs
            .into_iter()
            .map(|(surface, is_kanji)| JapaneseReadingSegment {
                surface,
                furigana: is_kanji.then(|| reading.clone()).flatten(),
            })
            .collect();
    };
    runs.into_iter()
        .zip(ranges)
        .map(|((surface, is_kanji), range)| JapaneseReadingSegment {
            surface,
            furigana: is_kanji.then(|| reading[range].iter().collect()),
        })
        .collect()
}

fn align_japanese_reading_runs(
    normalized_runs: &[Option<Vec<char>>],
    reading: &[char],
    run_index: usize,
    reading_index: usize,
) -> Option<Vec<std::ops::Range<usize>>> {
    align_japanese_reading_runs_cached(
        normalized_runs,
        reading,
        run_index,
        reading_index,
        &mut HashMap::new(),
    )
}

fn align_japanese_reading_runs_cached(
    normalized_runs: &[Option<Vec<char>>],
    reading: &[char],
    run_index: usize,
    reading_index: usize,
    cache: &mut HashMap<(usize, usize), Option<Vec<std::ops::Range<usize>>>>,
) -> Option<Vec<std::ops::Range<usize>>> {
    if let Some(cached) = cache.get(&(run_index, reading_index)) {
        return cached.clone();
    }
    if run_index == normalized_runs.len() {
        return (reading_index == reading.len()).then(Vec::new);
    }
    let result = if let Some(expected) = normalized_runs[run_index].as_deref() {
        let end = reading_index.checked_add(expected.len())?;
        if reading.get(reading_index..end)? != expected {
            None
        } else {
            align_japanese_reading_runs_cached(normalized_runs, reading, run_index + 1, end, cache)
                .map(|mut remaining| {
                    remaining.insert(0, reading_index..end);
                    remaining
                })
        }
    } else {
        let mut aligned = None;
        for end in reading_index + 1..=reading.len() {
            if let Some(mut remaining) = align_japanese_reading_runs_cached(
                normalized_runs,
                reading,
                run_index + 1,
                end,
                cache,
            ) {
                remaining.insert(0, reading_index..end);
                aligned = Some(remaining);
                break;
            }
        }
        aligned
    };
    cache.insert((run_index, reading_index), result.clone());
    result
}

const fn default_true() -> bool {
    true
}

fn default_translation_language() -> String {
    "en".to_string()
}

fn valid_lyrics_color(color: &str) -> bool {
    color.strip_prefix('#').is_some_and(|digits| {
        matches!(digits.len(), 6 | 8)
            && digits
                .chars()
                .all(|character| character.is_ascii_hexdigit())
    })
}

pub fn normalize_language_tag(value: &str) -> Option<String> {
    let value = value.trim().replace('_', "-").to_ascii_lowercase();
    if value.is_empty() || matches!(value.as_str(), "und" | "xxx") {
        return None;
    }
    let primary = value.split('-').next().unwrap_or_default();
    let normalized = match primary {
        "eng" => "en",
        "jpn" => "ja",
        "zho" | "chi" => "zh",
        "kor" => "ko",
        "spa" => "es",
        "fra" | "fre" => "fr",
        "deu" | "ger" => "de",
        "ita" => "it",
        "por" => "pt",
        "rus" => "ru",
        other => other,
    };
    Some(normalized.to_string())
}

fn language_matches(candidate: Option<&str>, target: Option<&str>) -> bool {
    match (
        candidate.and_then(normalize_language_tag),
        target.and_then(normalize_language_tag),
    ) {
        (Some(candidate), Some(target)) => candidate == target,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn japanese_dictionary_paths_include_macos_bundle_resources() {
        let paths = japanese_dictionary_paths_for(Some(Path::new(
            "/Applications/Rufin.app/Contents/MacOS/rufin-bin",
        )));

        assert!(paths.contains(&PathBuf::from(
            "/Applications/Rufin.app/Contents/MacOS/../Resources/share/rufin/japanese-readings.dic",
        )));
    }

    #[test]
    fn sparse_settings_preserve_flat_defaults_and_provider_order() {
        let mut settings = serde_json::from_str::<Settings>(
            r#"{"external_lyrics_enabled":false,"external_lyrics_providers":["genius","genius","lrclib"],"word_by_word_highlighting":true}"#,
        )
        .expect("settings");
        settings.sanitize();

        assert!(!settings.external_lyrics_enabled);
        assert!(!settings.karaoke_mode);
        assert!(settings.prefer_server_lyrics);
        assert_eq!(
            settings.external_lyrics_providers,
            vec![
                ExternalLyricsProvider::Genius,
                ExternalLyricsProvider::Lrclib
            ]
        );
    }

    #[test]
    fn lyrics_appearance_settings_are_bounded_and_css_safe() {
        let mut settings = Settings {
            lyrics_font_family: Some(format!("  {}  ", "x".repeat(200))),
            lyrics_font_size: Some(100),
            lyrics_highlight_color: Some("red; } * { color: red".to_string()),
            ..Settings::default()
        };

        settings.sanitize();

        assert_eq!(
            settings.lyrics_font_family.as_deref().map(str::len),
            Some(128)
        );
        assert_eq!(settings.lyrics_font_size, Some(28));
        assert_eq!(settings.lyrics_highlight_color, None);
    }

    #[test]
    fn enabled_providers_can_be_reordered_by_drop_position() {
        let mut settings = Settings {
            external_lyrics_providers: vec![
                ExternalLyricsProvider::Lrclib,
                ExternalLyricsProvider::Netease,
                ExternalLyricsProvider::Genius,
            ],
            ..Settings::default()
        };

        assert!(settings.reorder_external_lyrics_provider(
            ExternalLyricsProvider::Genius,
            ExternalLyricsProvider::Lrclib,
            false,
        ));
        assert_eq!(
            settings.external_lyrics_providers,
            vec![
                ExternalLyricsProvider::Genius,
                ExternalLyricsProvider::Lrclib,
                ExternalLyricsProvider::Netease,
            ]
        );

        assert!(settings.reorder_external_lyrics_provider(
            ExternalLyricsProvider::Genius,
            ExternalLyricsProvider::Netease,
            true,
        ));
        assert_eq!(
            settings.external_lyrics_providers,
            vec![
                ExternalLyricsProvider::Lrclib,
                ExternalLyricsProvider::Netease,
                ExternalLyricsProvider::Genius,
            ]
        );
        assert!(!settings.reorder_external_lyrics_provider(
            ExternalLyricsProvider::SimpMusic,
            ExternalLyricsProvider::Lrclib,
            false,
        ));
    }

    #[test]
    fn translation_selection_matches_language_aliases_then_unknown_then_original() {
        let original = document(LyricsRole::Original, Some("ja"), "original");
        let french = document(LyricsRole::Translation, Some("fra"), "français");
        let english = document(LyricsRole::Translation, Some("eng"), "English");
        let bundle = LyricsBundle::from_documents(
            LyricsOrigin::Native,
            vec![original.clone(), french, english],
        );
        let settings = Settings {
            prefer_translations: true,
            preferred_translation_language: "en-US".to_string(),
            ..Settings::default()
        };
        assert_eq!(
            bundle
                .selected_document(&settings)
                .map(|document| document.lines[0].text.as_str()),
            Some("English")
        );

        let unknown = document(LyricsRole::Translation, None, "unknown language");
        let bundle =
            LyricsBundle::from_documents(LyricsOrigin::Native, vec![original.clone(), unknown]);
        assert_eq!(
            bundle
                .selected_document(&settings)
                .map(|document| document.lines[0].text.as_str()),
            Some("original")
        );

        let bundle = LyricsBundle::from_documents(
            LyricsOrigin::Native,
            vec![
                original,
                document(LyricsRole::Translation, Some("fr"), "français"),
            ],
        );
        assert_eq!(
            bundle
                .selected_document(&settings)
                .map(|document| document.lines[0].text.as_str()),
            Some("original")
        );
    }

    #[test]
    fn local_japanese_readings_annotate_kanji_and_render_romaji() {
        assert!(contains_japanese_kana("君の名は"));
        assert!(!contains_japanese_kana("中文歌词"));
        assert!(!contains_japanese_kana("한국어 가사"));

        let reading = japanese_reading("君との思い出").expect("Japanese reading");
        assert!(
            reading
                .segments
                .iter()
                .filter(|segment| !segment.surface.chars().any(is_kanji))
                .all(|segment| segment.furigana.is_none())
        );
        assert!(
            reading
                .segments
                .iter()
                .filter(|segment| segment.furigana.is_some())
                .all(|segment| segment.surface.chars().all(is_kanji))
        );
        assert!(reading.segments.windows(3).any(|segments| {
            segments[0].surface == "思"
                && segments[0].furigana.as_deref() == Some("おも")
                && segments[1].surface == "い"
                && segments[1].furigana.is_none()
                && segments[2].surface == "出"
                && segments[2].furigana.as_deref() == Some("で")
        }));
        assert_eq!(reading.romanization, "kimi to no omoide");

        assert!(japanese_reading("中文歌词").is_none());
        assert!(japanese_reading_for_language("愛", Some("jpn")).is_some());
        assert!(japanese_reading_for_language("愛", Some("zh")).is_none());

        JAPANESE_READER.with(|reader| assert!(reader.borrow().is_some()));
        release_japanese_reader();
        JAPANESE_READER.with(|reader| assert!(reader.borrow().is_none()));
    }

    #[test]
    fn romaji_preserves_non_japanese_parts_of_mixed_text() {
        let reading = japanese_reading("君との Moon will shine 42!").expect("Japanese reading");

        assert_eq!(reading.romanization, "kimi to no Moon will shine 42!");
    }

    #[test]
    fn supplied_romanization_owns_aligned_kanji_readings() {
        let reading =
            japanese_reading_from_romanization("あの娘たぶんいいひと", "ano ko tabun ii hito")
                .expect("aligned reading");

        assert!(reading.segments.iter().any(|segment| {
            segment.surface == "娘" && segment.furigana.as_deref() == Some("こ")
        }));
        assert!(
            japanese_reading_from_romanization("あの娘たぶんいいひと", "unrelated words").is_none()
        );
    }

    #[test]
    fn japanese_readings_are_inferred_once_for_the_complete_document() {
        let mut inferred = document(LyricsRole::Original, None, "君の名は");
        inferred.lines.push(LyricsLine {
            text: "東京".to_string(),
            start_millis: None,
            end_millis: None,
            cue_lines: Vec::new(),
        });
        assert!(inferred.is_japanese_for_readings());

        let tagged = document(LyricsRole::Original, Some("jpn"), "東京");
        assert!(tagged.is_japanese_for_readings());

        let mut inferred_from_cue = document(LyricsRole::Original, None, "東京");
        inferred_from_cue.lines[0].cue_lines.push(LyricsCueLine {
            text: "きみ".to_string(),
            start_millis: None,
            end_millis: None,
            agent_id: None,
            cues: Vec::new(),
        });
        assert!(inferred_from_cue.is_japanese_for_readings());

        let unknown_han = document(LyricsRole::Original, None, "東京");
        assert!(!unknown_han.is_japanese_for_readings());

        let chinese = document(LyricsRole::Original, Some("zh"), "君の名は");
        assert!(!chinese.is_japanese_for_readings());
    }

    #[test]
    fn japanese_documents_do_not_romanize_plain_latin_lines() {
        assert!(
            japanese_reading_for_language_options(
                "Already written in Latin",
                Some("ja"),
                false,
                true,
            )
            .is_none()
        );
    }

    #[test]
    fn non_japanese_lyrics_do_not_load_the_japanese_reader() {
        release_japanese_reader();

        assert!(
            japanese_reading_for_language_options("한국어 가사", Some("ko"), true, true).is_none()
        );
        assert!(
            japanese_reading_for_language_options("中文歌词", Some("zh"), true, true).is_none()
        );
        assert!(
            japanese_reading_for_language_options("君の名は", Some("ja"), false, false).is_none()
        );
        JAPANESE_READER.with(|reader| assert!(reader.borrow().is_none()));
    }

    fn document(role: LyricsRole, language: Option<&str>, text: &str) -> LyricsDocument {
        LyricsDocument {
            role,
            language: language.map(str::to_string),
            offset_millis: 0,
            lines: vec![LyricsLine {
                text: text.to_string(),
                start_millis: None,
                end_millis: None,
                cue_lines: Vec::new(),
            }],
            agents: Vec::new(),
        }
    }
}
