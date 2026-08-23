use std::collections::HashSet;
use std::fs;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lofty::config::{GlobalOptions, ParseOptions, WriteOptions, apply_global_options};
use lofty::file::TaggedFileExt;
use lofty::probe::Probe;
use lofty::tag::{ItemKey, Tag, TagExt, TagType};

use library::{Track, TrackId};
use serde::Deserialize;
use tracing::{debug, warn};

use crate::{
    ExternalLyricsProvider, LyricsAgent, LyricsAgentRole, LyricsBundle as Lyrics, LyricsCue,
    LyricsCueLine, LyricsDocument, LyricsLine as LyricLine, LyricsOrigin, LyricsRole,
    LyricsSearchContent, LyricsSearchResult, normalize_language_tag,
};

const EXTERNAL_LYRICS_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const LRCLIB_RESPONSE_MAX_BYTES: usize = 2 * 1024 * 1024;
const LOCAL_LYRICS_MAX_BYTES: usize = 2 * 1024 * 1024;
const LOCAL_LYRICS_ALLOCATION_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LyricsPlan {
    native_search: sources::LyricsSearch,
    external_providers: Vec<ExternalLyricsProvider>,
    allow_external_fallback: bool,
    prefer_translations: bool,
    preferred_translation_language: String,
}

impl LyricsPlan {
    pub(crate) const fn native_search(&self) -> sources::LyricsSearch {
        self.native_search
    }

    pub(crate) fn external_providers(&self) -> &[ExternalLyricsProvider] {
        &self.external_providers
    }

    pub(crate) const fn allows_external_fallback(&self) -> bool {
        self.allow_external_fallback
    }

    pub(crate) const fn prefers_translations(&self) -> bool {
        self.prefer_translations
    }

    pub(crate) fn preferred_translation_language(&self) -> &str {
        &self.preferred_translation_language
    }
}

impl crate::Settings {
    pub(crate) fn automatic_lyrics_plan(
        &self,
        private_mode: bool,
        track_id: &TrackId,
    ) -> LyricsPlan {
        self.lyrics_plan(private_mode, track_id, false)
    }

    pub(crate) fn configured_lyrics_plan(
        &self,
        private_mode: bool,
        track_id: &TrackId,
    ) -> LyricsPlan {
        self.lyrics_plan(private_mode, track_id, true)
    }

    fn lyrics_plan(
        &self,
        private_mode: bool,
        track_id: &TrackId,
        configured_native_order: bool,
    ) -> LyricsPlan {
        let external_enabled =
            self.external_lyrics_enabled && !self.auto_lyrics_suppressed(track_id);
        let external_network = external_enabled && !private_mode;
        let native_search =
            if configured_native_order && external_network && self.prefer_server_lyrics {
                sources::LyricsSearch::ServerThenRemote
            } else if configured_native_order && external_network {
                sources::LyricsSearch::RemoteThenServer
            } else {
                sources::LyricsSearch::ServerOnly
            };
        LyricsPlan {
            native_search,
            external_providers: external_enabled
                .then(|| self.external_lyrics_providers.clone())
                .unwrap_or_default(),
            allow_external_fallback: external_network,
            prefer_translations: self.prefer_translations,
            preferred_translation_language: self.preferred_translation_language.clone(),
        }
    }

    pub fn auto_lyrics_suppressed(&self, track_id: &TrackId) -> bool {
        self.suppressed_auto_lyrics_track_ids
            .iter()
            .any(|stored| stored == track_id.as_str())
    }

    pub fn suppress_auto_lyrics(&mut self, track_id: &TrackId) -> bool {
        if self.auto_lyrics_suppressed(track_id) {
            false
        } else {
            self.suppressed_auto_lyrics_track_ids
                .push(track_id.as_str().to_string());
            true
        }
    }

    pub fn can_suppress_auto_lyrics(
        &self,
        private_mode: bool,
        track_id: &TrackId,
        origin: Option<LyricsOrigin>,
    ) -> bool {
        origin.is_some_and(|origin| {
            matches!(origin, LyricsOrigin::External(_))
                && self.external_lyrics_network_allowed(private_mode)
                && !self.auto_lyrics_suppressed(track_id)
        })
    }
}

pub(crate) fn lyrics_from_native(native: sources::NativeLyrics) -> Lyrics {
    Lyrics::from_documents(
        LyricsOrigin::Native,
        native
            .documents
            .into_iter()
            .map(|document| {
                let offset_millis = document.offset_millis;
                LyricsDocument {
                    role: match document.role {
                        sources::NativeLyricsRole::Original => LyricsRole::Original,
                        sources::NativeLyricsRole::Translation => LyricsRole::Translation,
                        sources::NativeLyricsRole::Pronunciation => LyricsRole::Pronunciation,
                    },
                    language: document
                        .language
                        .as_deref()
                        .and_then(normalize_language_tag),
                    offset_millis: 0,
                    lines: document
                        .lines
                        .into_iter()
                        .map(|line| LyricLine {
                            text: line.text,
                            start_millis: shifted_lyrics_time(line.start_millis, offset_millis),
                            end_millis: shifted_lyrics_time(line.end_millis, offset_millis),
                            cue_lines: line
                                .cue_lines
                                .into_iter()
                                .map(|cue_line| LyricsCueLine {
                                    text: cue_line.text,
                                    start_millis: shifted_lyrics_time(
                                        cue_line.start_millis,
                                        offset_millis,
                                    ),
                                    end_millis: shifted_lyrics_time(
                                        cue_line.end_millis,
                                        offset_millis,
                                    ),
                                    agent_id: cue_line.agent_id,
                                    cues: cue_line
                                        .cues
                                        .into_iter()
                                        .map(|cue| LyricsCue {
                                            text: cue.text,
                                            start_millis: shifted_lyrics_time(
                                                Some(cue.start_millis),
                                                offset_millis,
                                            )
                                            .unwrap_or_default(),
                                            end_millis: shifted_lyrics_time(
                                                cue.end_millis,
                                                offset_millis,
                                            ),
                                            byte_start: cue.byte_start,
                                            byte_end_exclusive: cue.byte_end_exclusive,
                                        })
                                        .collect(),
                                })
                                .collect(),
                        })
                        .collect(),
                    agents: document
                        .agents
                        .into_iter()
                        .map(|agent| LyricsAgent {
                            id: agent.id,
                            role: match agent.role {
                                sources::NativeLyricAgentRole::Main => LyricsAgentRole::Main,
                                sources::NativeLyricAgentRole::Voice => LyricsAgentRole::Voice,
                                sources::NativeLyricAgentRole::Background => {
                                    LyricsAgentRole::Background
                                }
                                sources::NativeLyricAgentRole::Group => LyricsAgentRole::Group,
                            },
                            name: agent.name,
                        })
                        .collect(),
                }
            })
            .collect(),
    )
}

fn shifted_lyrics_time(time: Option<u64>, offset_millis: i64) -> Option<u64> {
    time.map(|time| {
        if offset_millis >= 0 {
            time.saturating_sub(offset_millis.unsigned_abs())
        } else {
            time.saturating_add(offset_millis.unsigned_abs())
        }
    })
}

pub(crate) fn cached_lyrics_allowed(lyrics: &Lyrics, plan: &LyricsPlan, cue_track: bool) -> bool {
    let allowed = match lyrics.origin {
        LyricsOrigin::Local | LyricsOrigin::Native => true,
        LyricsOrigin::External(provider) => plan.external_providers.contains(&provider),
    };
    allowed && !(cue_track && lyrics.origin == LyricsOrigin::Local)
}

fn lyrics_from_text_content(provider: ExternalLyricsProvider, content: &str) -> Lyrics {
    if content_marks_instrumental(content, Some(provider)) {
        return Lyrics::instrumental(LyricsOrigin::External(provider));
    }
    Lyrics::from_documents(
        LyricsOrigin::External(provider),
        vec![LyricsDocument {
            role: LyricsRole::Original,
            language: None,
            offset_millis: 0,
            lines: content
                .lines()
                .filter_map(lyric_line_from_text)
                .filter(|line| provider_line_has_content(provider, line))
                .collect(),
            agents: Vec::new(),
        }],
    )
}

pub(crate) fn lyrics_with_displayable_content(mut lyrics: Lyrics) -> Option<Lyrics> {
    if lyrics.is_instrumental() {
        return Some(lyrics);
    }
    if let LyricsOrigin::External(provider) = lyrics.origin {
        for document in lyrics.documents_mut() {
            document
                .lines
                .retain(|line| provider_line_has_content(provider, line));
            if provider == ExternalLyricsProvider::Genius {
                let page_chrome_lines = genius_page_chrome_line_count(
                    document.lines.iter().map(|line| line.text.as_str()),
                );
                document.lines.drain(..page_chrome_lines);
            }
        }
    }
    lyrics
        .documents_mut()
        .retain(|document| !document.lines.is_empty());
    (!lyrics.documents().is_empty()).then_some(lyrics)
}

fn content_marks_instrumental(content: &str, provider: Option<ExternalLyricsProvider>) -> bool {
    const NETEASE_INSTRUMENTAL_TEXT: &str = "纯音乐，请欣赏";
    let has_marker = content.lines().any(|line| {
        let trimmed = line.trim().trim_start_matches('\u{feff}');
        lrc_tag_value(trimmed, "au").is_some_and(|value| value.eq_ignore_ascii_case("instrumental"))
            || provider == Some(ExternalLyricsProvider::Netease)
                && lyric_line_from_text(trimmed)
                    .is_some_and(|line| line.text.trim() == NETEASE_INSTRUMENTAL_TEXT)
    });
    has_marker
        && !content
            .lines()
            .filter_map(lyric_line_from_text)
            .any(|line| {
                provider.map_or_else(
                    || !line.text.trim().is_empty(),
                    |provider| provider_line_has_content(provider, &line),
                )
            })
}

fn lrc_tag_value<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    let (name, value) = inner.split_once(':')?;
    name.trim()
        .eq_ignore_ascii_case(tag)
        .then_some(value.trim())
}

fn provider_line_has_content(provider: ExternalLyricsProvider, line: &LyricLine) -> bool {
    const NETEASE_INSTRUMENTAL_TEXT: &str = "纯音乐，请欣赏";
    const NETEASE_NO_TEXT_LYRICS_MARKER: &str = "暂无文本歌词";
    const NETEASE_CREDIT_LABELS: &[&str] = &["作词", "作曲", "编曲", "制作人"];
    if provider != ExternalLyricsProvider::Netease {
        return true;
    }
    let text = line.text.trim();
    !text.is_empty()
        && text != NETEASE_INSTRUMENTAL_TEXT
        && !text.contains(NETEASE_NO_TEXT_LYRICS_MARKER)
        && !NETEASE_CREDIT_LABELS.iter().any(|label| {
            text.strip_prefix(label).is_some_and(|tail| {
                matches!(tail.trim_start().chars().next(), Some(':') | Some('：'))
            })
        })
}

fn lyric_line_from_text(line: &str) -> Option<LyricLine> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some((start_millis, text)) = parse_lrc_timestamp(trimmed) {
        let (display, cue_lines) = parse_inline_cues(text, start_millis);
        return Some(LyricLine {
            text: display,
            start_millis: Some(start_millis),
            end_millis: None,
            cue_lines,
        });
    }
    if let Some(cue_line) = parse_angle_bracket_line(trimmed) {
        return Some(cue_line);
    }
    if trimmed.starts_with('[') && trimmed.contains(']') {
        return None;
    }
    Some(LyricLine {
        text: trimmed.to_string(),
        start_millis: None,
        end_millis: None,
        cue_lines: Vec::new(),
    })
}

fn parse_lrc_timestamp(line: &str) -> Option<(u64, &str)> {
    let timestamp_end = line.find(']')?;
    let timestamp = line.get(1..timestamp_end)?;
    let (minutes, seconds) = timestamp.split_once(':')?;
    let minutes = minutes.parse::<u64>().ok()?;
    let (seconds, fraction) = seconds
        .split_once('.')
        .map(|(seconds, fraction)| (seconds, Some(fraction)))
        .unwrap_or((seconds, None));
    let seconds = seconds.parse::<u64>().ok()?;
    let fraction_millis = match fraction {
        Some(fraction) => fraction_to_millis(fraction)?,
        None => 0,
    };
    Some((
        (minutes * 60 + seconds) * 1_000 + fraction_millis,
        line.get(timestamp_end + 1..)?.trim(),
    ))
}

fn fraction_to_millis(fraction: &str) -> Option<u64> {
    let mut millis = 0_u64;
    for (index, character) in fraction.chars().take(3).enumerate() {
        let digit = u64::from(character.to_digit(10)?);
        millis += digit
            * match index {
                0 => 100,
                1 => 10,
                _ => 1,
            };
    }
    Some(millis)
}

#[allow(clippy::string_slice)]
fn parse_inline_cues(text: &str, line_start_millis: u64) -> (String, Vec<LyricsCueLine>) {
    #[derive(Clone, Copy)]
    struct Stamp {
        tag_start: usize,
        content_start: usize,
        millis: u64,
    }
    let mut timestamps: Vec<Stamp> = Vec::new();
    let text_bytes = text.as_bytes();
    let len = text.len();
    let mut i = 0;
    while i < len {
        if text_bytes[i] == b'<' {
            if let Some(close) = text[i + 1..].find('>') {
                let stamp_str = &text[i + 1..i + 1 + close];
                if let Some((millis, _)) = parse_lrc_inline_timestamp(stamp_str) {
                    timestamps.push(Stamp {
                        tag_start: i,
                        content_start: i + 1 + close + 1,
                        millis,
                    });
                }
                i += 1 + close + 1;
            } else {
                break;
            }
        } else {
            i += 1;
        }
    }

    if timestamps.is_empty() {
        return (text.to_string(), Vec::new());
    }

    let mut display = String::new();
    let mut cues = Vec::new();

    let prefix = &text[..timestamps[0].tag_start];
    if !prefix.is_empty() {
        let prefix_start = display.len();
        display.push_str(prefix);
        cues.push(LyricsCue {
            text: prefix.to_string(),
            start_millis: line_start_millis,
            end_millis: Some(timestamps[0].millis),
            byte_start: prefix_start,
            byte_end_exclusive: display.len(),
        });
    }

    for (idx, stamp) in timestamps.iter().enumerate() {
        let word_end = timestamps
            .get(idx + 1)
            .map_or(text.len(), |next| next.tag_start);
        if stamp.content_start >= word_end {
            continue;
        }
        let word = &text[stamp.content_start..word_end];
        if word.is_empty() {
            continue;
        }
        let display_start = display.len();
        display.push_str(word);
        let start_millis = stamp.millis;
        let end_millis = timestamps.get(idx + 1).map(|s| s.millis);
        cues.push(LyricsCue {
            text: word.to_string(),
            start_millis,
            end_millis,
            byte_start: display_start,
            byte_end_exclusive: display.len(),
        });
    }

    if cues.is_empty() {
        return (text.to_string(), Vec::new());
    }

    let cue_line = LyricsCueLine {
        text: display.clone(),
        start_millis: Some(line_start_millis),
        end_millis: None,
        agent_id: None,
        cues,
    };
    (display, vec![cue_line])
}

fn parse_lrc_inline_timestamp(stamp: &str) -> Option<(u64, &str)> {
    let (minutes, rest) = stamp.split_once(':')?;
    let minutes = minutes.parse::<u64>().ok()?;
    let (seconds, fraction) = rest
        .split_once('.')
        .map(|(s, f)| (s, Some(f)))
        .unwrap_or((rest, None));
    let seconds = seconds.parse::<u64>().ok()?;
    let fraction_millis = match fraction {
        Some(f) => fraction_to_millis(f)?,
        None => 0,
    };
    Some(((minutes * 60 + seconds) * 1_000 + fraction_millis, stamp))
}

fn parse_angle_bracket_line(line: &str) -> Option<LyricLine> {
    let (first_stamp, rest) = line.split_once('<')?;
    let (stamp, after_close) = rest.split_once('>')?;
    let line_millis = parse_lrc_inline_timestamp(stamp)?.0;
    if !first_stamp.trim().is_empty() {
        return None;
    }
    let mut display = String::new();
    let mut cues = Vec::new();
    let mut remaining = after_close;
    let mut prev_millis = line_millis;

    while !remaining.is_empty() {
        let (text_part, after_tag) = match remaining.split_once('<') {
            Some((t, a)) => (t, a),
            None => {
                let text_start = display.len();
                display.push_str(remaining);
                if !remaining.is_empty() && !cues.is_empty() {
                    cues.push(LyricsCue {
                        text: remaining.to_string(),
                        start_millis: prev_millis,
                        end_millis: None,
                        byte_start: text_start,
                        byte_end_exclusive: display.len(),
                    });
                }
                break;
            }
        };
        let text_start = display.len();
        display.push_str(text_part);
        if let Some((tag_stamp, tag_rest)) = after_tag.split_once('>') {
            if let Some((millis, _)) = parse_lrc_inline_timestamp(tag_stamp) {
                cues.push(LyricsCue {
                    text: text_part.to_string(),
                    start_millis: prev_millis,
                    end_millis: Some(millis),
                    byte_start: text_start,
                    byte_end_exclusive: display.len(),
                });
                prev_millis = millis;
                remaining = tag_rest;
            } else {
                display.push('<');
                display.push_str(tag_stamp);
                display.push('>');
                remaining = tag_rest;
            }
        } else {
            display.push('<');
            display.push_str(after_tag);
            break;
        }
    }

    if cues.is_empty() {
        return None;
    }

    let cue_line = LyricsCueLine {
        text: display.clone(),
        start_millis: Some(line_millis),
        end_millis: None,
        agent_id: None,
        cues,
    };

    Some(LyricLine {
        text: display,
        start_millis: Some(line_millis),
        end_millis: None,
        cue_lines: vec![cue_line],
    })
}

fn inline_search_content(
    synced_lyrics: Option<String>,
    plain_lyrics: Option<String>,
) -> LyricsSearchContent {
    if synced_lyrics
        .as_deref()
        .is_some_and(|lyrics| !lyrics.trim().is_empty())
        || plain_lyrics
            .as_deref()
            .is_some_and(|lyrics| !lyrics.trim().is_empty())
    {
        LyricsSearchContent::Inline {
            synced_lyrics,
            plain_lyrics,
        }
    } else {
        LyricsSearchContent::Unavailable
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LrcLibLyricsDto {
    id: u64,
    #[serde(default)]
    track_name: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artist_name: String,
    #[serde(default)]
    album_name: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    synced_lyrics: Option<String>,
    #[serde(default)]
    plain_lyrics: Option<String>,
    #[serde(default)]
    instrumental: bool,
}
impl From<LrcLibLyricsDto> for LyricsSearchResult {
    fn from(value: LrcLibLyricsDto) -> Self {
        Self {
            provider: ExternalLyricsProvider::Lrclib,
            id: value.id.to_string(),
            track_name: if value.track_name.trim().is_empty() {
                value.name
            } else {
                value.track_name
            },
            artist_name: value.artist_name,
            album_name: value.album_name.unwrap_or_default(),
            duration_seconds: value.duration.unwrap_or_default().round() as u32,
            content: if value.instrumental {
                LyricsSearchContent::Instrumental
            } else {
                inline_search_content(value.synced_lyrics, value.plain_lyrics)
            },
        }
    }
}
#[derive(Debug, Deserialize)]
struct NeteaseSearchResponse {
    result: Option<NeteaseSearchResult>,
}
#[derive(Debug, Deserialize)]
struct NeteaseSearchResult {
    songs: Option<Vec<NeteaseSong>>,
}
#[derive(Debug, Deserialize)]
struct NeteaseSong {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artists: Vec<NeteaseArtist>,
    #[serde(default)]
    album: Option<NeteaseAlbum>,
    #[serde(default)]
    duration: Option<u64>,
}
#[derive(Debug, Deserialize)]
struct NeteaseArtist {
    #[serde(default)]
    name: String,
}
#[derive(Debug, Deserialize)]
struct NeteaseAlbum {
    #[serde(default)]
    name: String,
}
#[derive(Debug, Deserialize)]
struct NeteaseLyricsResponse {
    lrc: Option<NeteaseLyricsBody>,
    #[serde(default)]
    tlyric: Option<NeteaseLyricsBody>,
}
#[derive(Debug, Deserialize)]
struct NeteaseLyricsBody {
    lyric: Option<String>,
}
#[derive(Debug, Deserialize)]
struct GeniusSearchResponse {
    response: Option<GeniusResponseBody>,
}
#[derive(Debug, Deserialize)]
struct GeniusResponseBody {
    sections: Option<Vec<GeniusSection>>,
}
#[derive(Debug, Deserialize)]
struct GeniusSection {
    hits: Option<Vec<GeniusHit>>,
}
#[derive(Debug, Deserialize)]
struct GeniusHit {
    result: GeniusSong,
}
#[derive(Debug, Deserialize)]
struct GeniusSong {
    #[serde(default)]
    artist_names: String,
    #[serde(default)]
    full_title: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    instrumental: bool,
}
#[derive(Debug, Deserialize)]
struct SimpMusicSearchResponse {
    data: Option<Vec<SimpMusicLyric>>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SimpMusicLyric {
    #[serde(default)]
    artist_name: String,
    #[serde(default)]
    album_name: Option<String>,
    #[serde(default)]
    duration_seconds: Option<u32>,
    #[serde(default)]
    plain_lyric: Option<String>,
    #[serde(default)]
    song_title: String,
    #[serde(default)]
    synced_lyrics: Option<String>,
    #[serde(default)]
    video_id: String,
}
pub(crate) fn lrclib_search(
    artist_name: &str,
    track_name: &str,
) -> Result<Vec<LyricsSearchResult>, String> {
    lrclib_search_priority_urls(
        lrclib_search_urls(artist_name, track_name)?,
        artist_name,
        track_name,
    )
}
pub(crate) fn lrclib_automatic_search(
    artist_name: &str,
    track_name: &str,
) -> Result<Vec<LyricsSearchResult>, String> {
    lrclib_search_with_urls(
        lrclib_search_urls(artist_name, track_name)?,
        artist_name,
        track_name,
    )
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LyricsLookup {
    artist_names: Vec<String>,
    track_name: String,
    duration_seconds: u32,
}
impl LyricsLookup {
    pub(crate) fn from_search(artist_name: &str, track_name: &str, duration_seconds: u32) -> Self {
        let mut lookup = Self {
            artist_names: Vec::new(),
            track_name: track_name.trim().to_string(),
            duration_seconds,
        };
        lookup.push_artist_name(artist_name);
        lookup.push_primary_artist_variants();
        lookup
    }

    fn from_track(track: &Track) -> Self {
        let mut lookup = Self::from_search(&track.artist, &track.title, track.duration_seconds);
        if let Some(credit) = track.artist_credits().first() {
            lookup.push_artist_name(&credit.name);
        } else if let Some(credit) = track.album_artist_credits().first() {
            lookup.push_artist_name(&credit.name);
        }
        lookup.push_artist_name(&track.artist);
        lookup.push_primary_artist_variants();
        lookup
    }

    fn queries(&self) -> Vec<(String, String)> {
        let artists = if self.artist_names.is_empty() {
            vec![String::new()]
        } else {
            self.artist_names.clone()
        };
        let mut queries = Vec::new();
        let mut seen = HashSet::new();
        for artist_name in artists {
            if self.track_name.is_empty() && artist_name.is_empty() {
                continue;
            }
            let key = (
                normalize_search_text(&artist_name),
                normalize_search_text(&self.track_name),
            );
            if seen.insert(key) {
                queries.push((artist_name, self.track_name.clone()));
            }
        }
        queries
    }

    fn push_artist_name(&mut self, artist_name: &str) {
        let artist_name = artist_name.trim();
        let normalized = normalize_search_text(artist_name);
        if artist_name.is_empty()
            || normalized.is_empty()
            || self
                .artist_names
                .iter()
                .any(|existing| normalize_search_text(existing) == normalized)
        {
            return;
        }
        self.artist_names.push(artist_name.to_string());
    }

    fn push_primary_artist_variants(&mut self) {
        let artists = self.artist_names.clone();
        for artist_name in artists {
            if let Some(primary) = primary_artist_name(&artist_name) {
                self.push_artist_name(&primary);
            }
        }
    }
}
fn primary_artist_name(artist_name: &str) -> Option<String> {
    const SEPARATORS: &[&str] = &[
        " • ",
        "•",
        " · ",
        "·",
        " / ",
        " | ",
        "; ",
        ";",
        " feat. ",
        " feat ",
        " featuring ",
        " ft. ",
        " ft ",
        " with ",
        " x ",
        " vs. ",
    ];
    let artist_name = artist_name.trim();
    let lower = artist_name.to_ascii_lowercase();
    let index = SEPARATORS
        .iter()
        .filter_map(|separator| {
            lower
                .find(&separator.to_ascii_lowercase())
                .map(|index| (index, separator.len()))
        })
        .min_by_key(|(index, _)| *index)
        .map(|(index, _)| index)?;
    let primary = artist_name.get(..index)?.trim();
    if primary.is_empty() || normalize_search_text(primary) == normalize_search_text(artist_name) {
        None
    } else {
        Some(primary.to_string())
    }
}
pub fn search_lyrics(
    providers: &[ExternalLyricsProvider],
    artist_name: &str,
    track_name: &str,
) -> Result<Vec<LyricsSearchResult>, String> {
    let lookup = LyricsLookup::from_search(artist_name, track_name, 0);
    let mut results = Vec::new();
    let mut errors = Vec::new();
    let mut had_success = false;
    std::thread::scope(|scope| {
        let handles = providers
            .iter()
            .copied()
            .map(|provider| {
                let lookup = &lookup;
                scope.spawn(move || {
                    (
                        provider,
                        external_provider_search_for_lookup(provider, lookup, false, None),
                    )
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            let Ok((provider, result)) = handle.join() else {
                errors.push("lyric provider worker panicked".to_string());
                continue;
            };
            match result {
                Ok(mut batch) => {
                    had_success = true;
                    filter_external_results_for_lookup(&mut batch, &lookup);
                    order_external_provider_results(&mut batch, &lookup);
                    results.extend(batch);
                }
                Err(error) => errors.push(format!("{}: {error}", provider.title())),
            }
        }
    });
    if !had_success && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(results)
}
pub(crate) fn external_best_lyrics(
    track: &Track,
    providers: &[ExternalLyricsProvider],
    prefer_translations: bool,
    preferred_translation_language: &str,
    cancelled: &AtomicBool,
) -> Result<Option<Lyrics>, String> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    let lookup = LyricsLookup::from_track(track);
    let mut results = Vec::new();
    let mut errors = Vec::new();
    let mut had_success = false;
    std::thread::scope(|scope| {
        let handles = providers
            .iter()
            .copied()
            .map(|provider| {
                let lookup = &lookup;
                scope.spawn(move || {
                    (
                        provider,
                        external_best_lyrics_for_provider(lookup, provider, cancelled),
                    )
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            let Ok((provider, result)) = handle.join() else {
                errors.push("lyric provider worker panicked".to_string());
                continue;
            };
            match result {
                Ok(batch) => {
                    had_success = true;
                    if !batch.is_empty() {
                        results.extend(batch);
                    }
                }
                Err(error) => errors.push(format!("{}: {error}", provider.title())),
            }
        }
    });
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    dedupe_external_results(&mut results);
    if results.is_empty() {
        if !had_success && !errors.is_empty() {
            return Err(errors.join("; "));
        }
        return Ok(None);
    }
    filter_external_results_for_lookup(&mut results, &lookup);
    if results.is_empty() {
        return Ok(None);
    }
    order_external_results(&mut results, &lookup, providers);
    let selection = crate::Settings {
        prefer_translations,
        preferred_translation_language: preferred_translation_language.to_string(),
        ..crate::Settings::default()
    };
    let mut fallback = None;
    for result in results {
        if cancelled.load(Ordering::Acquire) {
            return Ok(None);
        }
        match lyrics_from_search_result(&result) {
            Ok(Some(lyrics))
                if lyrics.is_instrumental()
                    || !prefer_translations
                    || lyrics.has_preferred_translation(&selection) =>
            {
                return Ok(Some(lyrics));
            }
            Ok(Some(lyrics)) => {
                fallback.get_or_insert(lyrics);
            }
            Ok(None) => {}
            Err(error) => errors.push(format!("{}: {error}", result.provider.title())),
        }
    }
    if !had_success && !errors.is_empty() {
        Err(errors.join("; "))
    } else {
        Ok(fallback)
    }
}
fn external_best_lyrics_for_provider(
    lookup: &LyricsLookup,
    provider: ExternalLyricsProvider,
    cancelled: &AtomicBool,
) -> Result<Vec<LyricsSearchResult>, String> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(Vec::new());
    }
    let mut results = Vec::new();
    if provider == ExternalLyricsProvider::Lrclib
        && let Some(result) = lrclib_exact_result(lookup)?
    {
        results.push(result);
    }
    if cancelled.load(Ordering::Acquire) {
        return Ok(Vec::new());
    }
    results.extend(external_provider_search_for_lookup(
        provider,
        lookup,
        true,
        Some(cancelled),
    )?);
    dedupe_external_results(&mut results);
    Ok(results)
}
fn filter_external_results_for_lookup(
    results: &mut Vec<LyricsSearchResult>,
    lookup: &LyricsLookup,
) {
    results.retain(|result| external_result_matches_lookup(result, lookup));
}
fn external_result_matches_lookup(result: &LyricsSearchResult, lookup: &LyricsLookup) -> bool {
    if !lookup.track_name.is_empty()
        && text_match_score(&lookup.track_name, &result.track_name) > 70
    {
        return false;
    }
    if !lookup.artist_names.is_empty()
        && lookup
            .artist_names
            .iter()
            .map(|artist_name| text_match_score(artist_name, &result.artist_name))
            .min()
            .unwrap_or(0)
            > 80
    {
        return false;
    }
    true
}
fn lrclib_exact_result(lookup: &LyricsLookup) -> Result<Option<LyricsSearchResult>, String> {
    let Some((artist_name, track_name)) = lookup.queries().into_iter().next() else {
        return Ok(None);
    };
    let Some(url) = lrclib_get_url(&artist_name, &track_name, lookup.duration_seconds)? else {
        return Ok(None);
    };
    lrclib_fetch_get(external_lyrics_client()?, url)
}
fn external_provider_search_for_lookup(
    provider: ExternalLyricsProvider,
    lookup: &LyricsLookup,
    automatic: bool,
    cancelled: Option<&AtomicBool>,
) -> Result<Vec<LyricsSearchResult>, String> {
    if request_cancelled(cancelled) {
        return Ok(Vec::new());
    }
    let queries = lookup.queries();
    if queries.is_empty() {
        return Ok(Vec::new());
    }
    if queries.len() == 1 {
        let (artist_name, track_name) = &queries[0];
        if request_cancelled(cancelled) {
            return Ok(Vec::new());
        }
        return external_provider_search(provider, artist_name, track_name, automatic);
    }
    let mut results = Vec::new();
    let mut errors = Vec::new();
    let mut had_success = false;
    for (artist_name, track_name) in queries {
        if request_cancelled(cancelled) {
            return Ok(Vec::new());
        }
        match external_provider_search(provider, &artist_name, &track_name, automatic) {
            Ok(batch) => {
                had_success = true;
                results.extend(batch);
            }
            Err(error) => errors.push(error),
        }
    }
    if !had_success && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    dedupe_external_results(&mut results);
    Ok(results)
}

fn request_cancelled(cancelled: Option<&AtomicBool>) -> bool {
    cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire))
}
fn dedupe_external_results(results: &mut Vec<LyricsSearchResult>) {
    let mut seen = HashSet::new();
    results.retain(|result| seen.insert((result.provider, result.id.clone())));
}
fn external_provider_search(
    provider: ExternalLyricsProvider,
    artist_name: &str,
    track_name: &str,
    automatic: bool,
) -> Result<Vec<LyricsSearchResult>, String> {
    match provider {
        ExternalLyricsProvider::Lrclib => {
            if automatic {
                lrclib_automatic_search(artist_name, track_name)
            } else {
                lrclib_search(artist_name, track_name)
            }
        }
        ExternalLyricsProvider::Netease => netease_search(artist_name, track_name),
        ExternalLyricsProvider::Genius => genius_search(artist_name, track_name),
        ExternalLyricsProvider::SimpMusic => simpmusic_search(artist_name, track_name),
    }
}
fn external_lyrics_client() -> Result<&'static reqwest::blocking::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(EXTERNAL_LYRICS_REQUEST_TIMEOUT)
                .user_agent(format!("Rufin/{}", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}
fn netease_search(artist_name: &str, track_name: &str) -> Result<Vec<LyricsSearchResult>, String> {
    let query = [artist_name.trim(), track_name.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut url = reqwest::Url::parse("https://music.163.com/api/search/get")
        .map_err(|error| error.to_string())?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("s", &query);
        pairs.append_pair("type", "1");
        pairs.append_pair("limit", "5");
        pairs.append_pair("offset", "0");
    }
    let body = fetch_text(external_lyrics_client()?, url, "NetEase lyric search")?;
    parse_netease_search_body(&body)
}
pub(crate) fn parse_netease_search_body(body: &str) -> Result<Vec<LyricsSearchResult>, String> {
    let response = serde_json::from_str::<NeteaseSearchResponse>(body)
        .map_err(|error| format!("NetEase lyric search returned invalid data: {error}"))?;
    Ok(response
        .result
        .and_then(|result| result.songs)
        .unwrap_or_default()
        .into_iter()
        .filter(|song| !song.name.trim().is_empty() || !song.artists.is_empty())
        .map(|song| LyricsSearchResult {
            provider: ExternalLyricsProvider::Netease,
            id: song.id.to_string(),
            track_name: song.name,
            artist_name: song
                .artists
                .into_iter()
                .map(|artist| artist.name)
                .filter(|name| !name.trim().is_empty())
                .collect::<Vec<_>>()
                .join(", "),
            album_name: song.album.map(|album| album.name).unwrap_or_default(),
            duration_seconds: song.duration.unwrap_or_default().div_ceil(1000) as u32,
            content: LyricsSearchContent::Deferred,
        })
        .collect())
}
fn netease_fetch_lyrics(id: &str) -> Result<Option<String>, String> {
    Ok(netease_fetch_lyrics_response(id)?
        .lrc
        .and_then(|body| body.lyric)
        .filter(|lyrics| !lyrics.trim().is_empty()))
}
fn netease_fetch_lyrics_bundle(id: &str) -> Result<Option<Lyrics>, String> {
    Ok(lyrics_from_netease_response(netease_fetch_lyrics_response(
        id,
    )?))
}
fn lyrics_from_netease_response(response: NeteaseLyricsResponse) -> Option<Lyrics> {
    if response
        .lrc
        .as_ref()
        .and_then(|body| body.lyric.as_deref())
        .is_some_and(|content| {
            content_marks_instrumental(content, Some(ExternalLyricsProvider::Netease))
        })
    {
        return Some(Lyrics::instrumental(LyricsOrigin::External(
            ExternalLyricsProvider::Netease,
        )));
    }
    let mut documents = Vec::new();
    for (role, language, body) in [
        (LyricsRole::Original, None, response.lrc),
        (LyricsRole::Translation, Some("zh"), response.tlyric),
    ] {
        let Some(content) = body
            .and_then(|body| body.lyric)
            .filter(|lyrics| !lyrics.trim().is_empty())
        else {
            continue;
        };
        documents.push(LyricsDocument {
            role,
            language: language.map(str::to_string),
            offset_millis: 0,
            lines: content
                .lines()
                .filter_map(lyric_line_from_text)
                .filter(|line| provider_line_has_content(ExternalLyricsProvider::Netease, line))
                .collect(),
            agents: Vec::new(),
        });
    }
    lyrics_with_displayable_content(Lyrics::from_documents(
        LyricsOrigin::External(ExternalLyricsProvider::Netease),
        documents,
    ))
}
fn netease_fetch_lyrics_response(id: &str) -> Result<NeteaseLyricsResponse, String> {
    let mut url = reqwest::Url::parse("https://music.163.com/api/song/lyric")
        .map_err(|error| error.to_string())?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("id", id);
        pairs.append_pair("kv", "-1");
        pairs.append_pair("lv", "-1");
        pairs.append_pair("tv", "-1");
    }
    let body = fetch_text(external_lyrics_client()?, url, "NetEase lyric lookup")?;
    let response = serde_json::from_str::<NeteaseLyricsResponse>(&body)
        .map_err(|error| format!("NetEase lyric lookup returned invalid data: {error}"))?;
    Ok(response)
}
fn genius_search(artist_name: &str, track_name: &str) -> Result<Vec<LyricsSearchResult>, String> {
    let query = [artist_name.trim(), track_name.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut url = reqwest::Url::parse("https://genius.com/api/search/song")
        .map_err(|error| error.to_string())?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", &query);
        pairs.append_pair("per_page", "5");
    }
    let body = fetch_text(external_lyrics_client()?, url, "Genius lyric search")?;
    parse_genius_search_body(&body)
}
pub(crate) fn parse_genius_search_body(body: &str) -> Result<Vec<LyricsSearchResult>, String> {
    let response = serde_json::from_str::<GeniusSearchResponse>(body)
        .map_err(|error| format!("Genius lyric search returned invalid data: {error}"))?;
    let mut results = Vec::new();
    for section in response
        .response
        .and_then(|body| body.sections)
        .unwrap_or_default()
    {
        for hit in section.hits.unwrap_or_default() {
            let Some(url) = trusted_genius_lyrics_url(&hit.result.url) else {
                continue;
            };
            let track_name = if hit.result.full_title.trim().is_empty() {
                hit.result.title
            } else {
                hit.result.full_title
            };
            results.push(LyricsSearchResult {
                provider: ExternalLyricsProvider::Genius,
                id: url.to_string(),
                track_name,
                artist_name: hit.result.artist_names,
                album_name: String::new(),
                duration_seconds: 0,
                content: if hit.result.instrumental {
                    LyricsSearchContent::Instrumental
                } else {
                    LyricsSearchContent::Deferred
                },
            });
        }
    }
    Ok(results)
}
fn genius_fetch_lyrics(url: &str) -> Result<Option<String>, String> {
    let Some(url) = trusted_genius_lyrics_url(url) else {
        return Ok(None);
    };
    let body = fetch_text(external_lyrics_client()?, url, "Genius lyric lookup")?;
    Ok(extract_genius_lyrics(&body).filter(|lyrics| !lyrics.trim().is_empty()))
}
fn trusted_genius_lyrics_url(raw: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(raw).ok()?;
    (url.scheme() == "https"
        && url.host_str() == Some("genius.com")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}
fn simpmusic_search(
    artist_name: &str,
    track_name: &str,
) -> Result<Vec<LyricsSearchResult>, String> {
    let query = if !track_name.trim().is_empty() {
        track_name.trim().to_string()
    } else {
        artist_name.trim().to_string()
    };
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut url = reqwest::Url::parse("https://api-lyrics.simpmusic.org/v1/search")
        .map_err(|error| error.to_string())?;
    url.query_pairs_mut().append_pair("q", &query);
    let body = fetch_text(external_lyrics_client()?, url, "SimpMusic lyric search")?;
    parse_simpmusic_search_body(&body)
}
pub(crate) fn parse_simpmusic_search_body(body: &str) -> Result<Vec<LyricsSearchResult>, String> {
    let response = serde_json::from_str::<SimpMusicSearchResponse>(body)
        .map_err(|error| format!("SimpMusic lyric search returned invalid data: {error}"))?;
    Ok(response
        .data
        .unwrap_or_default()
        .into_iter()
        .filter(|song| !song.video_id.trim().is_empty())
        .map(|song| LyricsSearchResult {
            provider: ExternalLyricsProvider::SimpMusic,
            id: song.video_id,
            track_name: song.song_title,
            artist_name: song.artist_name,
            album_name: song.album_name.unwrap_or_default(),
            duration_seconds: song.duration_seconds.unwrap_or_default(),
            content: match inline_search_content(song.synced_lyrics, song.plain_lyric) {
                LyricsSearchContent::Unavailable => LyricsSearchContent::Deferred,
                content => content,
            },
        })
        .collect())
}
fn simpmusic_fetch_lyrics(id: &str) -> Result<Option<String>, String> {
    let url = reqwest::Url::parse(&format!("https://api-lyrics.simpmusic.org/v1/{id}"))
        .map_err(|error| error.to_string())?;
    let body = fetch_text(external_lyrics_client()?, url, "SimpMusic lyric lookup")?;
    parse_simpmusic_lyrics_body(&body)
}
fn parse_simpmusic_lyrics_body(body: &str) -> Result<Option<String>, String> {
    if let Ok(song) = serde_json::from_str::<SimpMusicLyric>(body) {
        return Ok(song
            .synced_lyrics
            .filter(|lyrics| !lyrics.trim().is_empty())
            .or_else(|| song.plain_lyric.filter(|lyrics| !lyrics.trim().is_empty())));
    }
    let response = serde_json::from_str::<SimpMusicSearchResponse>(body)
        .map_err(|error| format!("SimpMusic lyric lookup returned invalid data: {error}"))?;
    Ok(response.data.and_then(|mut songs| {
        songs.drain(..).find_map(|song| {
            song.synced_lyrics
                .filter(|lyrics| !lyrics.trim().is_empty())
                .or_else(|| song.plain_lyric.filter(|lyrics| !lyrics.trim().is_empty()))
        })
    }))
}
fn fetch_text(
    client: &reqwest::blocking::Client,
    url: reqwest::Url,
    context: &str,
) -> Result<String, String> {
    let response = send_get(client, url, context)
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| format!("{context} failed: {error}"))?;
    read_response_text_bounded(response, LRCLIB_RESPONSE_MAX_BYTES, context)
        .map_err(|error| format!("{context} failed: {error}"))
}

fn send_get(
    client: &reqwest::blocking::Client,
    url: reqwest::Url,
    context: &str,
) -> Result<reqwest::blocking::Response, reqwest::Error> {
    debug!(
        service = "lyrics",
        method = "GET",
        public_url = %url,
        %context,
        "sending remote request"
    );
    let started = Instant::now();
    let response = client.get(url).send()?;
    debug!(
        service = "lyrics",
        method = "GET",
        status = response.status().as_u16(),
        elapsed_ms = started.elapsed().as_millis(),
        %context,
        "received remote response"
    );
    Ok(response)
}
fn lrclib_search_with_urls(
    urls: Vec<reqwest::Url>,
    artist_name: &str,
    track_name: &str,
) -> Result<Vec<LyricsSearchResult>, String> {
    let client = external_lyrics_client()?;
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    let mut had_success = false;
    let mut errors = Vec::new();
    for url in urls {
        match lrclib_fetch_search(client, url) {
            Ok(batch) => {
                debug!(results = batch.len(), "received LRCLIB lyric search batch");
                had_success = true;
                for result in batch {
                    if seen.insert(result.id.clone()) {
                        results.push(result);
                    }
                }
            }
            Err(error) => errors.push(error),
        }
    }
    if !had_success && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    order_lrclib_results(&mut results, artist_name, track_name);
    Ok(results)
}
fn lrclib_search_priority_urls(
    urls: Vec<reqwest::Url>,
    artist_name: &str,
    track_name: &str,
) -> Result<Vec<LyricsSearchResult>, String> {
    if urls.is_empty() {
        return Ok(Vec::new());
    }
    let client = external_lyrics_client()?;
    let mut errors = Vec::new();
    let mut had_success = false;
    for url in urls {
        match lrclib_fetch_search(client, url) {
            Ok(mut results) => {
                debug!(
                    results = results.len(),
                    "received LRCLIB lyric search batch"
                );
                had_success = true;
                if !results.is_empty() {
                    order_lrclib_results(&mut results, artist_name, track_name);
                    return Ok(results);
                }
            }
            Err(error) => errors.push(error),
        }
    }
    if had_success {
        Ok(Vec::new())
    } else {
        Err(errors.join("; "))
    }
}
pub fn lyrics_from_search_result(result: &LyricsSearchResult) -> Result<Option<Lyrics>, String> {
    match &result.content {
        LyricsSearchContent::Instrumental => {
            return Ok(Some(Lyrics::instrumental(LyricsOrigin::External(
                result.provider,
            ))));
        }
        LyricsSearchContent::Unavailable => return Ok(None),
        LyricsSearchContent::Deferred if result.provider == ExternalLyricsProvider::Netease => {
            return netease_fetch_lyrics_bundle(&result.id);
        }
        LyricsSearchContent::Inline { .. } | LyricsSearchContent::Deferred => {}
    }
    let content = match lyrics_result_content(result) {
        Some(content) => Some(content.to_string()),
        None => external_fetch_lyrics(result)?,
    };
    let Some(content) = content.filter(|lyrics| !lyrics.trim().is_empty()) else {
        return Ok(None);
    };
    let lyrics = lyrics_from_text_content(result.provider, &content);
    Ok(lyrics_with_displayable_content(lyrics))
}
fn external_fetch_lyrics(result: &LyricsSearchResult) -> Result<Option<String>, String> {
    match result.provider {
        ExternalLyricsProvider::Lrclib => Ok(None),
        ExternalLyricsProvider::Netease => netease_fetch_lyrics(&result.id),
        ExternalLyricsProvider::Genius => genius_fetch_lyrics(&result.id),
        ExternalLyricsProvider::SimpMusic => simpmusic_fetch_lyrics(&result.id),
    }
}
pub(crate) fn lrclib_get_url(
    artist_name: &str,
    track_name: &str,
    duration_seconds: u32,
) -> Result<Option<reqwest::Url>, String> {
    let artist_name = artist_name.trim();
    let track_name = track_name.trim();
    if artist_name.is_empty() || track_name.is_empty() {
        return Ok(None);
    }
    let mut url =
        reqwest::Url::parse("https://lrclib.net/api/get").map_err(|error| error.to_string())?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("track_name", track_name);
        query.append_pair("artist_name", artist_name);
        if duration_seconds > 0 {
            query.append_pair("duration", &duration_seconds.to_string());
        }
    }
    Ok(Some(url))
}
pub(crate) fn lrclib_fetch_get(
    client: &reqwest::blocking::Client,
    url: reqwest::Url,
) -> Result<Option<LyricsSearchResult>, String> {
    let response = match send_get(client, url, "Lyric lookup") {
        Ok(response) => response,
        Err(error) => return Err(format!("Lyric lookup failed: {error}")),
    };
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = response
        .error_for_status()
        .map_err(|error| format!("Lyric lookup failed: {error}"))?;
    let body = read_response_text_bounded(response, LRCLIB_RESPONSE_MAX_BYTES, "Lyric lookup")
        .map_err(|error| format!("Lyric lookup failed: {error}"))?;
    parse_lrclib_get_body(&body).map(Some)
}
pub(crate) fn parse_lrclib_get_body(body: &str) -> Result<LyricsSearchResult, String> {
    serde_json::from_str::<LrcLibLyricsDto>(body)
        .map(LyricsSearchResult::from)
        .map_err(|error| format!("Lyric lookup returned invalid data: {error}"))
}
pub(crate) fn lrclib_search_urls(
    artist_name: &str,
    track_name: &str,
) -> Result<Vec<reqwest::Url>, String> {
    let artist_name = artist_name.trim();
    let track_name = track_name.trim();
    let combined_query = [track_name, artist_name]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let combined_query = normalize_search_text(&combined_query);
    let mut urls = Vec::new();
    if !artist_name.is_empty() && !track_name.is_empty() {
        let mut field_url = lrclib_search_base_url()?;
        {
            let mut query = field_url.query_pairs_mut();
            query.append_pair("track_name", track_name);
            query.append_pair("artist_name", artist_name);
        }
        push_unique_lrclib_search_url(&mut urls, field_url);
    }
    if !combined_query.is_empty() {
        let mut url = lrclib_search_base_url()?;
        url.query_pairs_mut().append_pair("q", &combined_query);
        push_unique_lrclib_search_url(&mut urls, url);
    }
    if !track_name.is_empty()
        && let Some(short_artist_query) = shortened_artist_query(artist_name)
    {
        let short_query = normalize_search_text(&format!("{track_name} {short_artist_query}"));
        if !short_query.is_empty() {
            let mut url = lrclib_search_base_url()?;
            url.query_pairs_mut().append_pair("q", &short_query);
            push_unique_lrclib_search_url(&mut urls, url);
        }
    }
    Ok(urls)
}
fn shortened_artist_query(artist_name: &str) -> Option<String> {
    let normalized = normalize_search_text(artist_name);
    let mut tokens = normalized.split_whitespace().collect::<Vec<_>>();
    if tokens.len() < 2 {
        return None;
    }
    tokens.pop();
    Some(tokens.join(" "))
}
fn push_unique_lrclib_search_url(urls: &mut Vec<reqwest::Url>, url: reqwest::Url) {
    if urls
        .iter()
        .all(|existing| existing.as_str() != url.as_str())
    {
        urls.push(url);
    }
}
pub(crate) fn lrclib_search_base_url() -> Result<reqwest::Url, String> {
    reqwest::Url::parse("https://lrclib.net/api/search").map_err(|error| error.to_string())
}
pub(crate) fn lrclib_fetch_search(
    client: &reqwest::blocking::Client,
    url: reqwest::Url,
) -> Result<Vec<LyricsSearchResult>, String> {
    let body = fetch_text(client, url, "Lyric search")?;
    parse_lrclib_search_body(&body)
}
pub(crate) fn parse_lrclib_search_body(body: &str) -> Result<Vec<LyricsSearchResult>, String> {
    let values = serde_json::from_str::<Vec<serde_json::Value>>(body)
        .map_err(|error| format!("Lyric search returned invalid data: {error}"))?;
    let mut results = Vec::new();
    for value in values {
        match serde_json::from_value::<LrcLibLyricsDto>(value) {
            Ok(dto) => {
                let result = LyricsSearchResult::from(dto);
                if !result.track_name.trim().is_empty() || !result.artist_name.trim().is_empty() {
                    results.push(result);
                }
            }
            Err(error) => {
                debug!(%error, "skipped invalid LRCLIB search result");
            }
        }
    }
    Ok(results)
}
pub(crate) fn order_lrclib_results(
    results: &mut [LyricsSearchResult],
    artist_name: &str,
    track_name: &str,
) {
    let lookup = LyricsLookup::from_search(artist_name, track_name, 0);
    results.sort_by(|a, b| {
        lyrics_match_score(a, &lookup)
            .cmp(&lyrics_match_score(b, &lookup))
            .then_with(|| lrclib_has_synced_lyrics(b).cmp(&lrclib_has_synced_lyrics(a)))
            .then_with(|| lrclib_has_plain_lyrics(b).cmp(&lrclib_has_plain_lyrics(a)))
            .then_with(|| a.track_name.cmp(&b.track_name))
            .then_with(|| a.artist_name.cmp(&b.artist_name))
    });
}
pub(crate) fn order_external_provider_results(
    results: &mut [LyricsSearchResult],
    lookup: &LyricsLookup,
) {
    results.sort_by(|a, b| {
        lyrics_match_score(a, lookup)
            .cmp(&lyrics_match_score(b, lookup))
            .then_with(|| result_has_synced_lyrics(b).cmp(&result_has_synced_lyrics(a)))
            .then_with(|| result_has_plain_lyrics(b).cmp(&result_has_plain_lyrics(a)))
            .then_with(|| a.track_name.cmp(&b.track_name))
            .then_with(|| a.artist_name.cmp(&b.artist_name))
    });
}
fn order_external_results(
    results: &mut [LyricsSearchResult],
    lookup: &LyricsLookup,
    providers: &[ExternalLyricsProvider],
) {
    results.sort_by(|a, b| {
        lyrics_match_score(a, lookup)
            .cmp(&lyrics_match_score(b, lookup))
            .then_with(|| {
                provider_rank(a.provider, providers).cmp(&provider_rank(b.provider, providers))
            })
            .then_with(|| result_has_synced_lyrics(b).cmp(&result_has_synced_lyrics(a)))
            .then_with(|| result_has_plain_lyrics(b).cmp(&result_has_plain_lyrics(a)))
            .then_with(|| a.track_name.cmp(&b.track_name))
            .then_with(|| a.artist_name.cmp(&b.artist_name))
    });
}
fn provider_rank(provider: ExternalLyricsProvider, providers: &[ExternalLyricsProvider]) -> usize {
    providers
        .iter()
        .position(|candidate| *candidate == provider)
        .unwrap_or(usize::MAX)
}
fn lyrics_match_score(result: &LyricsSearchResult, lookup: &LyricsLookup) -> u16 {
    text_match_score(&lookup.track_name, &result.track_name)
        .saturating_mul(2)
        .saturating_add(artist_match_score(lookup, &result.artist_name))
        .saturating_add(duration_match_penalty(
            lookup.duration_seconds,
            result.duration_seconds,
        ))
}
fn artist_match_score(lookup: &LyricsLookup, artist_name: &str) -> u16 {
    lookup
        .artist_names
        .iter()
        .map(|query| text_match_score(query, artist_name))
        .min()
        .unwrap_or(0)
}
fn duration_match_penalty(target_seconds: u32, candidate_seconds: u32) -> u16 {
    if target_seconds == 0 || candidate_seconds == 0 {
        return 0;
    }
    let diff = target_seconds.abs_diff(candidate_seconds);
    match diff {
        0..=2 => 0,
        3..=5 => 4,
        6..=10 => 12,
        11..=20 => 30,
        _ => 60 + diff.min(90) as u16,
    }
}
pub(crate) fn text_match_score(query: &str, candidate: &str) -> u16 {
    let query = normalize_search_text(query);
    if query.is_empty() {
        return 0;
    }
    let candidate = normalize_search_text(candidate);
    if candidate == query {
        return 0;
    }
    let query_tokens = query.split_whitespace().collect::<HashSet<_>>();
    if query_tokens.is_empty() {
        return 0;
    }
    let candidate_tokens = candidate.split_whitespace().collect::<HashSet<_>>();
    let matched = query_tokens.intersection(&candidate_tokens).count();
    let missing = query_tokens.len().saturating_sub(matched);
    let extra = candidate_tokens.len().saturating_sub(matched);
    if matched == 0 {
        if candidate.contains(&query) || query.contains(&candidate) {
            return 10;
        }
        return 100 + query_tokens.len() as u16 * 10;
    }
    (missing as u16 * 30) + (extra.min(6) as u16 * 4)
}
pub(crate) fn normalize_search_text(value: &str) -> String {
    let mut normalized = String::new();
    for character in value.chars() {
        if character.is_alphanumeric() {
            normalized.extend(character.to_lowercase());
        } else {
            normalized.push(' ');
        }
    }
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}
pub(crate) fn extract_genius_lyrics(body: &str) -> Option<String> {
    let mut sections = Vec::new();
    let mut remaining = body;
    while let Some(marker_start) = remaining.find("data-lyrics-container=\"true\"") {
        let after_marker = remaining.get(marker_start..)?;
        let tag_end = after_marker.find('>')? + marker_start;
        let after_tag = remaining.get(tag_end + 1..)?;
        let section_end = after_tag.find("</div>").unwrap_or(after_tag.len());
        let section = strip_html_tags(after_tag.get(..section_end).unwrap_or_default());
        if !section.trim().is_empty() {
            sections.push(section);
        }
        remaining = after_tag
            .get(section_end.min(after_tag.len())..)
            .unwrap_or_default();
    }
    if sections.is_empty()
        && let Some(lyrics_start) = body.find("class=\"lyrics\"")
    {
        let after_marker = body.get(lyrics_start..)?;
        let tag_end = after_marker.find('>')? + lyrics_start;
        let after_tag = body.get(tag_end + 1..)?;
        let section_end = after_tag.find("</div>").unwrap_or(after_tag.len());
        let section = strip_html_tags(after_tag.get(..section_end).unwrap_or_default());
        if !section.trim().is_empty() {
            sections.push(section);
        }
    }
    let lyrics = sections.join("\n");
    let page_chrome_lines = genius_page_chrome_line_count(lyrics.lines());
    let lyrics = lyrics
        .lines()
        .skip(page_chrome_lines)
        .collect::<Vec<_>>()
        .join("\n");
    (!lyrics.trim().is_empty()).then(|| lyrics.trim().to_string())
}
fn genius_page_chrome_line_count<'a>(lines: impl IntoIterator<Item = &'a str>) -> usize {
    let mut lines = lines.into_iter();
    let Some(contributors) = lines.next() else {
        return 0;
    };
    let Some(translations) = lines.next() else {
        return 0;
    };
    if !genius_contributors_heading(contributors)
        || !translations.trim().eq_ignore_ascii_case("translations")
    {
        return 0;
    }
    2 + usize::from(lines.next().is_some())
}
fn genius_contributors_heading(line: &str) -> bool {
    let mut words = line.split_whitespace();
    words
        .next()
        .is_some_and(|count| count.chars().all(|character| character.is_ascii_digit()))
        && words.next().is_some_and(|label| {
            label.eq_ignore_ascii_case("contributor") || label.eq_ignore_ascii_case("contributors")
        })
        && words.next().is_none()
}
fn strip_html_tags(value: &str) -> String {
    let mut stripped = String::new();
    let mut in_tag = false;
    let mut tag = String::new();
    for character in value.chars() {
        match character {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let tag_name = tag.trim().to_ascii_lowercase();
                if tag_name.starts_with("br") || tag_name.starts_with("/p") {
                    stripped.push('\n');
                }
            }
            _ if in_tag => tag.push(character),
            _ => stripped.push(character),
        }
    }
    decode_html_entities(&stripped)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
fn decode_html_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}
pub(crate) fn lrclib_has_synced_lyrics(result: &LyricsSearchResult) -> bool {
    result_has_synced_lyrics(result)
}
fn result_has_synced_lyrics(result: &LyricsSearchResult) -> bool {
    result
        .content
        .synced_lyrics()
        .is_some_and(|lyrics| !lyrics.trim().is_empty())
}
pub(crate) fn lrclib_has_plain_lyrics(result: &LyricsSearchResult) -> bool {
    result_has_plain_lyrics(result)
}
fn result_has_plain_lyrics(result: &LyricsSearchResult) -> bool {
    result
        .content
        .plain_lyrics()
        .is_some_and(|lyrics| !lyrics.trim().is_empty())
}
pub fn save_lyrics_search_result(
    result: &LyricsSearchResult,
    output_path: PathBuf,
) -> Result<Option<(PathBuf, Lyrics)>, String> {
    let content = match lyrics_result_content(result) {
        Some(content) => Some(content.to_string()),
        None => external_fetch_lyrics(result)?,
    }
    .filter(|lyrics| !lyrics.trim().is_empty());
    let Some(content) = content else {
        return Ok(None);
    };
    let lyrics = lyrics_from_text_content(result.provider, &content);
    let Some(lyrics) = lyrics_with_displayable_content(lyrics) else {
        return Ok(None);
    };
    let path = output_path;
    fs::write(&path, &content).map_err(|error| error.to_string())?;
    debug!(path = %path.display(), "saved lyric file");
    Ok(Some((path, lyrics)))
}
pub fn save_current_lyrics(
    lyrics: &LyricsDocument,
    offset_millis: i64,
    output_path: PathBuf,
) -> Result<PathBuf, String> {
    let content = lyrics
        .lines
        .iter()
        .map(|line| match line.start_millis {
            Some(start_millis) => {
                let start_millis = if offset_millis >= 0 {
                    start_millis.saturating_sub(offset_millis.unsigned_abs())
                } else {
                    start_millis.saturating_add(offset_millis.unsigned_abs())
                };
                let minutes = start_millis / 60_000;
                let seconds = start_millis % 60_000 / 1_000;
                let millis = start_millis % 1_000;
                format!("[{minutes:02}:{seconds:02}.{millis:03}]{}", line.text)
            }
            None => line.text.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&output_path, content).map_err(|error| error.to_string())?;
    debug!(path = %output_path.display(), "saved current lyric file");
    Ok(output_path)
}
pub(crate) fn lyrics_result_content(result: &LyricsSearchResult) -> Option<&str> {
    result
        .content
        .synced_lyrics()
        .filter(|lyrics| !lyrics.trim().is_empty())
        .or_else(|| {
            result
                .content
                .plain_lyrics()
                .filter(|lyrics| !lyrics.trim().is_empty())
        })
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalLyricsInput {
    pub audio_path: PathBuf,
    pub title: String,
    pub cue_track: bool,
}

pub(crate) fn local_sidecar_lyrics(input: &LocalLyricsInput) -> Option<Lyrics> {
    for path in local_sidecar_candidates(&input.audio_path, Some(&input.title), input.cue_track) {
        if let Some(lyrics) = lyrics_from_sidecar_file(&path) {
            return Some(lyrics);
        }
    }
    None
}

pub(crate) fn embedded_lyrics_from_audio(path: &Path) -> Option<Lyrics> {
    apply_global_options(
        GlobalOptions::new()
            .allocation_limit(LOCAL_LYRICS_ALLOCATION_LIMIT)
            .preserve_format_specific_items(false),
    );
    let options = ParseOptions::new().read_cover_art(false);
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            warn!(path = %path.display(), %e, "embedded lyrics: cannot open file");
            return None;
        }
    };
    let probe = match Probe::new(BufReader::new(file))
        .options(options)
        .guess_file_type()
    {
        Ok(p) => p,
        Err(e) => {
            warn!(path = %path.display(), %e, "embedded lyrics: cannot guess file type");
            return None;
        }
    };
    let Some(_file_type) = probe.file_type() else {
        warn!(path = %path.display(), "embedded lyrics: no file type detected");
        return None;
    };
    let tagged_file = match probe.read() {
        Ok(t) => t,
        Err(e) => {
            warn!(path = %path.display(), %e, "embedded lyrics: failed to read tags");
            return None;
        }
    };
    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());
    let Some(tag) = tag else {
        warn!(path = %path.display(), "embedded lyrics: no tag found");
        return None;
    };
    let raw = match extract_lyrics_from_tag(tag) {
        Some(r) => r,
        None => {
            debug!(path = %path.display(), "embedded lyrics: no lyrics tag found");
            return None;
        }
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > LOCAL_LYRICS_MAX_BYTES {
        debug!(path = %path.display(), len = trimmed.len(), "embedded lyrics: lyrics empty or too large");
        return None;
    }
    if content_marks_instrumental(trimmed, None) {
        return Some(Lyrics::instrumental(LyricsOrigin::Local));
    }
    let lines = trimmed
        .lines()
        .filter_map(lyric_line_from_text)
        .collect::<Vec<_>>();
    (!lines.is_empty()).then(|| {
        Lyrics::from_documents(
            LyricsOrigin::Local,
            vec![LyricsDocument {
                role: LyricsRole::Original,
                language: None,
                offset_millis: 0,
                lines,
                agents: Vec::new(),
            }],
        )
    })
}

fn extract_lyrics_from_tag(tag: &lofty::tag::Tag) -> Option<String> {
    for key in [ItemKey::UnsyncLyrics, ItemKey::Lyrics] {
        if let Some(value) = tag.get_string(key) {
            return Some(value.to_string());
        }
    }
    for item in tag.items() {
        let key_str = item
            .key()
            .map_key(tag.tag_type())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if key_str == "lyrics" {
            if let Some(value) = item.value().text() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn format_cue_timestamp(millis: u64, offset_millis: i64) -> String {
    let millis = if offset_millis >= 0 {
        millis.saturating_sub(offset_millis.unsigned_abs())
    } else {
        millis.saturating_add(offset_millis.unsigned_abs())
    };
    let minutes = millis / 60_000;
    let seconds = millis % 60_000 / 1_000;
    let fraction = millis % 1_000;
    format!("[{minutes:02}:{seconds:02}.{fraction:03}]")
}

fn format_inline_cues(line: &LyricLine, offset_millis: i64) -> Option<String> {
    let cue_line = line.cue_lines.first()?;
    if cue_line.cues.is_empty() {
        return None;
    }
    let mut result = String::new();
    for cue in &cue_line.cues {
        let millis = if offset_millis >= 0 {
            cue.start_millis
                .saturating_sub(offset_millis.unsigned_abs())
        } else {
            cue.start_millis
                .saturating_add(offset_millis.unsigned_abs())
        };
        let minutes = millis / 60_000;
        let seconds = millis % 60_000 / 1_000;
        let fraction = millis % 1_000;
        result.push_str(&format!(
            "<{minutes:02}:{seconds:02}.{fraction:03}>{}",
            cue.text
        ));
    }
    Some(result)
}

pub fn lyrics_to_lrc_text(document: &LyricsDocument, offset_millis: i64) -> String {
    document
        .lines
        .iter()
        .map(|line| {
            let line_ts = line
                .start_millis
                .map(|m| format_cue_timestamp(m, offset_millis));
            let inline = format_inline_cues(line, offset_millis);
            match (line_ts, inline) {
                (Some(ts), Some(cues)) => format!("{ts}{cues}"),
                (Some(ts), None) => format!("{ts}{}", line.text),
                (None, Some(cues)) => cues,
                (None, None) => line.text.clone(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn lyrics_from_edited_text(text: &str) -> Option<LyricsDocument> {
    let lines: Vec<LyricLine> = text.lines().filter_map(lyric_line_from_text).collect();
    if lines.is_empty() {
        return None;
    }
    Some(LyricsDocument {
        role: LyricsRole::Original,
        language: None,
        offset_millis: 0,
        lines,
        agents: Vec::new(),
    })
}

pub(crate) fn embed_lyrics_in_audio(path: &Path, lrc_text: &str) -> bool {
    apply_global_options(GlobalOptions::new().allocation_limit(LOCAL_LYRICS_ALLOCATION_LIMIT));
    let options = ParseOptions::new().read_cover_art(true);
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            warn!(path = %path.display(), %e, "embed lyrics: cannot open file");
            return false;
        }
    };
    let probe = match Probe::new(BufReader::new(file))
        .options(options)
        .guess_file_type()
    {
        Ok(p) => p,
        Err(e) => {
            warn!(path = %path.display(), %e, "embed lyrics: cannot guess file type");
            return false;
        }
    };
    let tagged_file = match probe.read() {
        Ok(t) => t,
        Err(e) => {
            warn!(path = %path.display(), %e, "embed lyrics: failed to read tags");
            return false;
        }
    };
    let mut tagged_file = tagged_file;
    let tag_type = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag())
        .map(|t| t.tag_type());
    let tag = match tagged_file.primary_tag_mut() {
        Some(tag) => Some(tag),
        None => tagged_file.first_tag_mut(),
    };
    let write_options = WriteOptions::new().remove_others(false);
    let Some(tag) = tag else {
        debug!(
            path = %path.display(),
            "embed lyrics: no writable tag found, creating ID3v2 tag"
        );
        let mut id3v2 = Tag::new(TagType::Id3v2);
        id3v2.insert_text(ItemKey::UnsyncLyrics, lrc_text.to_string());
        return id3v2.save_to_path(path, write_options).is_ok();
    };
    let inserted = tag.insert_text(ItemKey::UnsyncLyrics, lrc_text.to_string());
    if !inserted {
        debug!(
            path = %path.display(),
            tag_type = ?tag_type,
            "embed lyrics: primary tag does not support UnsyncLyrics, trying ID3v2 fallback"
        );
        let mut id3v2 = Tag::new(TagType::Id3v2);
        id3v2.insert_text(ItemKey::UnsyncLyrics, lrc_text.to_string());
        match id3v2.save_to_path(path, write_options) {
            Ok(()) => {
                debug!(path = %path.display(), "embed lyrics: saved via ID3v2 fallback");
                return true;
            }
            Err(e) => {
                warn!(path = %path.display(), %e, "embed lyrics: ID3v2 fallback also failed");
                return false;
            }
        }
    }
    match tag.save_to_path(path, write_options) {
        Ok(()) => {
            debug!(path = %path.display(), "embed lyrics: saved to audio file");
            true
        }
        Err(e) => {
            warn!(path = %path.display(), %e, "embed lyrics: failed to save");
            false
        }
    }
}
fn lyrics_from_sidecar_file(path: &Path) -> Option<Lyrics> {
    let content = read_text_file_bounded(path, LOCAL_LYRICS_MAX_BYTES).ok()?;
    if content_marks_instrumental(&content, None) {
        return Some(Lyrics::instrumental(LyricsOrigin::Local));
    }
    let lines = content
        .lines()
        .filter_map(lyric_line_from_text)
        .collect::<Vec<_>>();
    (!lines.is_empty()).then(|| {
        Lyrics::from_documents(
            LyricsOrigin::Local,
            vec![LyricsDocument {
                role: LyricsRole::Original,
                language: None,
                offset_millis: 0,
                lines,
                agents: Vec::new(),
            }],
        )
    })
}
fn local_sidecar_candidates(
    audio_path: &Path,
    title: Option<&str>,
    cue_track: bool,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if !cue_track {
        paths.push(audio_path.with_extension("lrc"));
    }
    if let Some(path) = title_matched_lrc(audio_path.parent(), title)
        && !paths.iter().any(|candidate| candidate == &path)
    {
        paths.push(path);
    }
    paths
}
fn title_matched_lrc(parent: Option<&Path>, title: Option<&str>) -> Option<PathBuf> {
    let parent = parent?;
    let title_key = normalized_lyrics_name(title?);
    if title_key.is_empty() {
        return None;
    }
    let mut matches = fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("lrc"))
                && path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| normalized_lyrics_name(stem) == title_key)
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches.into_iter().next()
}
fn normalized_lyrics_name(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}
fn read_response_text_bounded(
    response: reqwest::blocking::Response,
    limit: usize,
    context: &str,
) -> Result<String, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!(
            "{context} exceeded {} MiB limit",
            bytes_to_mib(limit)
        ));
    }
    let bytes = read_bytes_bounded(response, limit, context).map_err(|error| error.to_string())?;
    String::from_utf8(bytes).map_err(|error| error.to_string())
}
fn read_text_file_bounded(path: &Path, limit: usize) -> io::Result<String> {
    if fs::metadata(path)?.len() > limit as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("lyrics file exceeded {} MiB limit", bytes_to_mib(limit)),
        ));
    }
    let file = fs::File::open(path)?;
    let bytes = read_bytes_bounded(file, limit, "lyrics file")?;
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}
fn read_bytes_bounded<R: Read>(mut reader: R, limit: usize, context: &str) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(bytes);
        }
        if bytes
            .len()
            .checked_add(read)
            .is_none_or(|length| length > limit)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{context} exceeded {} MiB limit", bytes_to_mib(limit)),
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
}
fn bytes_to_mib(bytes: usize) -> usize {
    bytes / 1024 / 1024
}

#[cfg(test)]
mod tests {
    use library::{AlbumId, ArtistCredit, ArtistId, TrackData, TrackRelations};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn native_lyrics_apply_offset_and_preserve_language_roles_and_agents() {
        let lyrics = lyrics_from_native(sources::NativeLyrics {
            documents: vec![sources::NativeLyricsDocument {
                role: sources::NativeLyricsRole::Translation,
                language: Some("eng-US".to_string()),
                offset_millis: 250,
                lines: vec![sources::NativeLyricLine {
                    text: "Eyes".to_string(),
                    start_millis: Some(1_000),
                    end_millis: Some(2_000),
                    cue_lines: vec![sources::NativeLyricCueLine {
                        text: "Eyes".to_string(),
                        start_millis: Some(1_000),
                        end_millis: Some(2_000),
                        agent_id: Some("lead".to_string()),
                        cues: vec![sources::NativeLyricCue {
                            text: "Eyes".to_string(),
                            start_millis: 1_100,
                            end_millis: Some(1_400),
                            byte_start: 0,
                            byte_end_exclusive: 4,
                        }],
                    }],
                }],
                agents: vec![sources::NativeLyricAgent {
                    id: "lead".to_string(),
                    role: sources::NativeLyricAgentRole::Voice,
                    name: Some("Lead".to_string()),
                }],
            }],
        });

        assert_eq!(lyrics.origin, LyricsOrigin::Native);
        let document = &lyrics.documents()[0];
        assert_eq!(document.role, LyricsRole::Translation);
        assert_eq!(document.language.as_deref(), Some("en"));
        assert_eq!(document.lines[0].start_millis, Some(750));
        assert_eq!(document.lines[0].end_millis, Some(1_750));
        let cue_line = &document.lines[0].cue_lines[0];
        assert_eq!(cue_line.start_millis, Some(750));
        assert_eq!(cue_line.end_millis, Some(1_750));
        assert_eq!(cue_line.cues[0].start_millis, 850);
        assert_eq!(cue_line.cues[0].end_millis, Some(1_150));
        assert_eq!(cue_line.cues[0].byte_end_exclusive, 4);
        assert_eq!(document.agents[0].role, LyricsAgentRole::Voice);
        assert_eq!(document.agents[0].name.as_deref(), Some("Lead"));
    }

    fn track() -> Track {
        Track::new(TrackData {
            id: TrackId::new("jellyfin:track:lovesong"),
            album_id: Some(AlbumId::new("jellyfin:album:disintegration")),
            title: "Lovesong".to_string(),
            artist: "The Cure • Robert Smith".to_string(),
            album: "Disintegration".to_string(),
            album_artwork: None,
            year: 1989,
            release_date: None,
            date_added: None,
            last_played: None,
            play_count: None,
            user_rating: None,
            duration_seconds: 210,
            favorite: false,
            disc_number: 1,
            track_number: 7,
            image_ref: None,
            local_artwork: None,
            musicbrainz_recording_id: None,
            musicbrainz_release_track_id: None,
            source_path: None,
            cue: None,
            source_format: None,
            comment: None,
            skip_count: None,
            bpm: None,
            relations: TrackRelations {
                artists: vec![ArtistCredit {
                    id: ArtistId::new("jellyfin:artist:the-cure"),
                    name: "The Cure".to_string(),
                    musicbrainz_artist_id: None,
                }],
                ..TrackRelations::default()
            },
        })
    }

    #[test]
    fn plans_keep_cached_external_lyrics_in_private_mode() {
        let mut settings = crate::Settings::default();
        let track_id = track().id.clone();
        let external = Lyrics::from_documents(
            LyricsOrigin::External(ExternalLyricsProvider::Lrclib),
            vec![LyricsDocument {
                role: LyricsRole::Original,
                language: None,
                offset_millis: 0,
                lines: vec![test_line("cached", None)],
                agents: Vec::new(),
            }],
        );
        let plan = settings.configured_lyrics_plan(false, &track_id);
        assert_eq!(plan.native_search, sources::LyricsSearch::ServerThenRemote);
        assert!(plan.allow_external_fallback);
        assert!(cached_lyrics_allowed(&external, &plan, false));

        settings.prefer_server_lyrics = false;
        let plan = settings.configured_lyrics_plan(false, &track_id);
        assert_eq!(plan.native_search, sources::LyricsSearch::RemoteThenServer);

        let plan = settings.configured_lyrics_plan(true, &track_id);
        assert_eq!(plan.native_search, sources::LyricsSearch::ServerOnly);
        assert!(!plan.allow_external_fallback);
        assert!(cached_lyrics_allowed(&external, &plan, false));

        let plan = settings.automatic_lyrics_plan(false, &track_id);
        assert_eq!(plan.native_search, sources::LyricsSearch::ServerOnly);
        assert!(plan.allow_external_fallback);

        let mut disabled = crate::Settings::default();
        disabled.external_lyrics_enabled = false;
        let plan = disabled.automatic_lyrics_plan(false, &track_id);
        assert!(!cached_lyrics_allowed(&external, &plan, false));

        let mut provider_removed = crate::Settings::default();
        provider_removed
            .external_lyrics_providers
            .retain(|provider| *provider != ExternalLyricsProvider::Lrclib);
        let plan = provider_removed.automatic_lyrics_plan(false, &track_id);
        assert!(!cached_lyrics_allowed(&external, &plan, false));

        assert!(settings.suppress_auto_lyrics(&track_id));
        let plan = settings.automatic_lyrics_plan(false, &track_id);
        assert!(!plan.allow_external_fallback);
        assert!(!cached_lyrics_allowed(&external, &plan, false));
        assert!(!settings.suppress_auto_lyrics(&track_id));
    }

    #[test]
    fn local_sidecar_uses_title_for_cue_and_rejects_oversized_files() {
        let directory = tempdir().expect("tempdir");
        let audio = directory.path().join("album.flac");
        fs::write(&audio, []).expect("audio");
        fs::write(directory.path().join("album.lrc"), "[00:01.00]wrong").expect("same stem");
        fs::write(directory.path().join("Lovesong.lrc"), "[00:02.50]right").expect("title");
        let input = LocalLyricsInput {
            audio_path: audio.clone(),
            title: "Lovesong".to_string(),
            cue_track: true,
        };
        let lyrics = local_sidecar_lyrics(&input).expect("lyrics");
        assert_eq!(
            lyrics.documents()[0].lines,
            vec![test_line("right", Some(2_500))]
        );

        let oversized = directory.path().join("large.flac");
        fs::write(&oversized, []).expect("audio");
        let file = fs::File::create(oversized.with_extension("lrc")).expect("lyrics");
        file.set_len((LOCAL_LYRICS_MAX_BYTES + 1) as u64)
            .expect("oversized lyrics");
        assert_eq!(
            local_sidecar_lyrics(&LocalLyricsInput {
                audio_path: oversized,
                title: "missing".to_string(),
                cue_track: false,
            }),
            None
        );
    }

    #[test]
    fn local_sidecar_recognizes_the_instrumental_lrc_tag() {
        let directory = tempdir().expect("tempdir");
        let audio = directory.path().join("instrumental.flac");
        fs::write(&audio, []).expect("audio");
        fs::write(audio.with_extension("lrc"), "[au: instrumental]\n").expect("lyrics");

        assert!(
            local_sidecar_lyrics(&LocalLyricsInput {
                audio_path: audio,
                title: "Instrumental".to_string(),
                cue_track: false,
            })
            .is_some_and(|lyrics| lyrics.is_instrumental())
        );
    }

    #[test]
    fn instrumental_lrc_tag_does_not_hide_real_lyrics() {
        let lyrics = lyrics_from_text_content(
            ExternalLyricsProvider::Lrclib,
            "[au: instrumental]\n[00:01.00]A real line",
        );

        assert!(!lyrics.is_instrumental());
        assert_eq!(lyrics.documents()[0].lines[0].text, "A real line");
    }

    #[test]
    fn parser_keeps_timing_and_filters_netease_placeholders() {
        let lyrics = lyrics_from_text_content(
            ExternalLyricsProvider::Netease,
            "[00:00.00] 作曲 : Composer\n[00:05.00]纯音乐，请欣赏\n[00:10.005]actual line",
        );
        assert_eq!(
            lyrics.documents()[0].lines,
            vec![test_line("actual line", Some(10_005))]
        );
        assert_eq!(
            parse_lrc_timestamp("[12:34.5]line"),
            Some((754_500, "line"))
        );
    }

    #[test]
    fn netease_translation_is_an_independent_selectable_document() {
        let response = serde_json::from_str::<NeteaseLyricsResponse>(
            r#"{
                "lrc":{"lyric":"[00:01.00]Original"},
                "tlyric":{"lyric":"[00:01.00]中文翻译"}
            }"#,
        )
        .expect("NetEase response");
        let lyrics = lyrics_from_netease_response(response).expect("lyrics bundle");

        assert_eq!(lyrics.documents().len(), 2);
        assert_eq!(lyrics.documents()[0].role, LyricsRole::Original);
        assert_eq!(lyrics.documents()[1].role, LyricsRole::Translation);
        assert_eq!(lyrics.documents()[1].language.as_deref(), Some("zh"));
        let english = crate::Settings {
            prefer_translations: true,
            preferred_translation_language: "en".to_string(),
            ..crate::Settings::default()
        };
        assert_eq!(
            lyrics
                .selected_document(&english)
                .map(|document| document.lines[0].text.as_str()),
            Some("Original")
        );
        let chinese = crate::Settings {
            preferred_translation_language: "zh".to_string(),
            ..english
        };
        assert_eq!(
            lyrics
                .selected_document(&chinese)
                .map(|document| document.lines[0].text.as_str()),
            Some("中文翻译")
        );
    }

    #[test]
    fn netease_instrumental_message_is_a_positive_result() {
        let response = serde_json::from_str::<NeteaseLyricsResponse>(
            r#"{"lrc":{"lyric":"[00:00.00]纯音乐，请欣赏"}}"#,
        )
        .expect("NetEase response");

        assert!(
            lyrics_from_netease_response(response).is_some_and(|lyrics| lyrics.is_instrumental())
        );
    }

    #[test]
    fn provider_payloads_and_genius_url_boundary_are_preserved() {
        let lrclib = parse_lrclib_search_body(
            r#"[{"id":7,"trackName":"Song","artistName":"Artist","duration":185,"syncedLyrics":"[00:01.00]line"}]"#,
        )
        .expect("LRCLIB");
        assert_eq!(lrclib[0].provider, ExternalLyricsProvider::Lrclib);
        assert_eq!(lrclib[0].duration_seconds, 185);

        let netease = parse_netease_search_body(
            r#"{"result":{"songs":[{"id":42,"name":"Song","artists":[{"name":"Artist"}],"album":{"name":"Album"},"duration":95000}]}}"#,
        )
        .expect("NetEase");
        assert_eq!(netease[0].provider, ExternalLyricsProvider::Netease);
        assert_eq!(netease[0].duration_seconds, 95);

        let genius = parse_genius_search_body(
            r#"{"response":{"sections":[{"hits":[{"result":{"artist_names":"Artist","title":"Song","url":"https://example.test/song"}},{"result":{"artist_names":"Artist","title":"Song","url":"https://genius.com/artist-song-lyrics","instrumental":true}}]}]}}"#,
        )
        .expect("Genius");
        assert_eq!(genius.len(), 1);
        assert!(genius[0].id.starts_with("https://genius.com/"));
        assert_eq!(genius[0].content, LyricsSearchContent::Instrumental);
    }

    #[test]
    fn lrclib_instrumental_flag_is_a_positive_result() {
        let result = parse_lrclib_search_body(
            r#"[{"id":7,"trackName":"Song","artistName":"Artist","instrumental":true,"syncedLyrics":null,"plainLyrics":null}]"#,
        )
        .expect("LRCLIB")
        .pop()
        .expect("result");

        assert!(
            lyrics_from_search_result(&result)
                .expect("lyrics lookup")
                .is_some_and(|lyrics| lyrics.is_instrumental())
        );
    }

    #[test]
    fn credited_artist_and_duration_drive_filtering_and_ranking() {
        let lookup = LyricsLookup::from_track(&track());
        let mut results = vec![
            LyricsSearchResult {
                provider: ExternalLyricsProvider::Lrclib,
                id: "wrong-title".to_string(),
                track_name: "Shout".to_string(),
                artist_name: "The Cure".to_string(),
                album_name: String::new(),
                duration_seconds: 210,
                content: LyricsSearchContent::Inline {
                    synced_lyrics: None,
                    plain_lyrics: Some("line".to_string()),
                },
            },
            LyricsSearchResult {
                provider: ExternalLyricsProvider::Lrclib,
                id: "wrong-duration".to_string(),
                track_name: "Lovesong".to_string(),
                artist_name: "The Cure".to_string(),
                album_name: String::new(),
                duration_seconds: 310,
                content: LyricsSearchContent::Inline {
                    synced_lyrics: Some("[00:01.00]line".to_string()),
                    plain_lyrics: Some("line".to_string()),
                },
            },
            LyricsSearchResult {
                provider: ExternalLyricsProvider::Lrclib,
                id: "right".to_string(),
                track_name: "Lovesong".to_string(),
                artist_name: "The Cure".to_string(),
                album_name: String::new(),
                duration_seconds: 211,
                content: LyricsSearchContent::Inline {
                    synced_lyrics: None,
                    plain_lyrics: Some("line".to_string()),
                },
            },
        ];
        filter_external_results_for_lookup(&mut results, &lookup);
        order_external_provider_results(&mut results, &lookup);
        assert_eq!(
            results
                .iter()
                .map(|result| result.id.as_str())
                .collect::<Vec<_>>(),
            vec!["right", "wrong-duration"]
        );
    }

    #[test]
    fn save_uses_the_selected_path_and_rejects_empty_provider_content() {
        let directory = tempdir().expect("tempdir");
        let output = directory.path().join("chosen.lrc");
        let result = LyricsSearchResult {
            provider: ExternalLyricsProvider::Lrclib,
            id: "one".to_string(),
            track_name: "Lovesong".to_string(),
            artist_name: "The Cure".to_string(),
            album_name: "Disintegration".to_string(),
            duration_seconds: 210,
            content: LyricsSearchContent::Inline {
                synced_lyrics: Some("[00:01.00]line".to_string()),
                plain_lyrics: None,
            },
        };
        let (path, lyrics) = save_lyrics_search_result(&result, output.clone())
            .expect("save")
            .expect("lyrics");
        assert_eq!(path, output);
        assert_eq!(fs::read_to_string(path).expect("saved"), "[00:01.00]line");
        assert_eq!(lyrics.documents()[0].lines[0].start_millis, Some(1_000));

        let placeholder = LyricsSearchResult {
            provider: ExternalLyricsProvider::Netease,
            content: LyricsSearchContent::Inline {
                synced_lyrics: Some("[00:01.00]暂无文本歌词".to_string()),
                plain_lyrics: None,
            },
            ..result
        };
        assert_eq!(
            save_lyrics_search_result(&placeholder, directory.path().join("placeholder.lrc"))
                .expect("placeholder"),
            None
        );
    }

    #[test]
    fn current_lyrics_save_the_visible_timing() {
        let directory = tempdir().expect("tempdir");
        let output = directory.path().join("current.lrc");
        let lyrics = LyricsDocument {
            role: LyricsRole::Original,
            language: None,
            offset_millis: 0,
            lines: vec![test_line("first", Some(1_000)), test_line("note", None)],
            agents: Vec::new(),
        };

        let path = save_current_lyrics(&lyrics, 250, output.clone()).expect("save current");

        assert_eq!(path, output);
        assert_eq!(
            fs::read_to_string(path).expect("saved current"),
            "[00:00.750]first\nnote"
        );
    }

    fn test_line(text: &str, start_millis: Option<u64>) -> LyricLine {
        LyricLine {
            text: text.to_string(),
            start_millis,
            end_millis: None,
            cue_lines: Vec::new(),
        }
    }
}
