//! The Store indexes saved lyric documents and returns bounded track results with excerpts.

use std::collections::BTreeMap;

use futures_util::TryStreamExt;
use sqlx::{Connection, SqliteConnection};

use crate::{
    Catalog, LibraryResult, LyricsSnippet, LyricsSnippetPart, ReadCancellation, SearchRequest,
    SearchResults, TrackKey,
};

/// One row per display document of a lyrics row, written by the lyrics Write. It needs the
/// `rufin_lyrics` tokenizer, so it stays outside the Store schema.
const LYRICS_SEARCH_SCHEMA: &str = "CREATE VIRTUAL TABLE lyrics_fts USING fts5(
    text, content='', contentless_delete=1, tokenize='rufin_lyrics', prefix='3'
)";

pub(crate) async fn initialize(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(LYRICS_SEARCH_SCHEMA)
        .execute(&mut *connection)
        .await?;
    Ok(())
}

impl Catalog {
    pub async fn search_tracks_by_lyrics(
        &self,
        request: &SearchRequest,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<SearchResults> {
        let expressions = expressions(request.query());
        if expressions.is_empty() {
            return Ok(SearchResults::default());
        }
        let mut connection = self.acquire_cancellable(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let mut keys = Vec::new();
        let mut lyrics = BTreeMap::new();
        for phrases in expressions {
            // Read in index order, stopping after a page of distinct tracks.
            let mut rows = sqlx::query_as::<_, (i64, String, i64)>(
                "SELECT track.track_key,lyrics.lyrics,lyrics_fts.rowid&3
                     FROM lyrics_fts
                     JOIN lyrics_cache lyrics ON lyrics.lyrics_key=lyrics_fts.rowid>>2
                     JOIN tracks track ON track.media_uri=lyrics.media_uri
                     WHERE lyrics_fts MATCH ?1
                       AND track.track_key NOT IN(SELECT value FROM json_each(?2))
                     ORDER BY lyrics_fts.rowid",
            )
            .bind(expression(&phrases))
            .bind(serde_json::to_string(&keys)?)
            .fetch(&mut *transaction);
            while let Some((key, payload, ordinal)) = rows.try_next().await? {
                if keys.contains(&key) {
                    continue;
                }
                keys.push(key);
                let text = crate::lyrics::documents(&payload)
                    .into_iter()
                    .find(|(document, _)| *document as i64 == ordinal)
                    .map(|(_, text)| text)
                    .unwrap_or_default();
                lyrics.insert(TrackKey::from_raw(key), snippet(&excerpt(&text, &phrases)));
                if keys.len() == request.limit() {
                    break;
                }
            }
            if keys.len() == request.limit() {
                break;
            }
        }
        let tracks = self
            .search_rows(&mut transaction, &[keys, Vec::new(), Vec::new()], false)
            .await?
            .tracks;
        transaction.commit().await?;
        Ok(SearchResults {
            tracks,
            lyrics,
            ..SearchResults::default()
        })
    }
}

/// One quoted phrase of a match expression; a prefix phrase matches its last token as a prefix.
#[derive(Clone, PartialEq)]
struct Phrase {
    text: String,
    prefix: bool,
}

fn expression(phrases: &[Phrase]) -> String {
    phrases
        .iter()
        .map(|phrase| {
            let quoted = format!("\"{}\"", phrase.text.replace('"', "\"\""));
            if phrase.prefix { quoted + "*" } else { quoted }
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// The whole query as one phrase, then every term, then every term with the last one a prefix.
fn expressions(query: &str) -> Vec<Vec<Phrase>> {
    let mut terms = Vec::new();
    let mut text = String::new();
    let mut in_phrase = false;
    for character in query.chars() {
        if character == '"' || character.is_whitespace() && !in_phrase {
            if text.chars().any(char::is_alphanumeric) {
                terms.push((std::mem::take(&mut text), in_phrase));
            } else {
                text.clear();
            }
            if character == '"' {
                in_phrase = !in_phrase;
            }
        } else {
            text.push(character);
        }
    }
    if text.chars().any(char::is_alphanumeric) {
        terms.push((text, in_phrase));
    }
    if terms.is_empty() {
        return Vec::new();
    }
    let phrase = |text: &str, prefix| Phrase {
        text: text.to_owned(),
        prefix,
    };
    let whole = phrase(
        &terms
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        false,
    );
    let exact = terms
        .iter()
        .map(|(text, _)| phrase(text, false))
        .collect::<Vec<_>>();
    let mut result = vec![vec![whole]];
    if !result.contains(&exact) {
        result.push(exact.clone());
    }
    let (last, is_phrase) = terms.last().expect("nonempty terms");
    if !is_phrase
        && last
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric())
            .count()
            >= 3
        && !last.chars().any(crate::lyrics_tokenizer::character_token)
    {
        let mut prefix = exact;
        prefix.last_mut().expect("nonempty terms").prefix = true;
        result.push(prefix);
    }
    result
}

/// Tokens per excerpt, as `snippet()` was asked for.
const EXCERPT_TOKENS: i64 = 32;

/// A combining mark that `unicode61` folds away inside a word.
fn diacritic(character: char) -> bool {
    let code = character as u32;
    match code {
        768..=799 => 0x0802_9FDF_u32 >> (code - 768) & 1 == 1,
        800..=817 => 0x0003_61F8_u32 >> (code - 800) & 1 == 1,
        _ => false,
    }
}

/// Folds a word character as `unicode61 remove_diacritics=2` does: lower case, and a Latin
/// letter with diacritics becomes its base letter.
fn fold(character: char, folded: &mut String) {
    for lower in character.to_lowercase() {
        let mut base = None;
        let mut marks = true;
        unicode_normalization::char::decompose_canonical(lower, |part| match base {
            None => base = Some(part),
            Some(_) => marks &= unicode_normalization::char::is_combining_mark(part),
        });
        match base {
            _ if diacritic(lower) => {}
            Some(base) if u32::from(lower) < 0x2000 && base.is_ascii_alphabetic() && marks => {
                folded.push(base);
            }
            _ => folded.push(lower),
        }
    }
}

/// The tokens `rufin_lyrics` splits a text into: byte range and folded text.
fn tokens(text: &str) -> Vec<(usize, usize, String)> {
    let mut tokens: Vec<(usize, usize, String)> = Vec::new();
    let mut open = false;
    for (offset, character) in text.char_indices() {
        let end = offset + character.len_utf8();
        if crate::lyrics_tokenizer::character_token(character) {
            tokens.push((offset, end, character.to_string()));
            open = false;
        } else if character.is_alphanumeric()
            && !unicode_normalization::char::is_combining_mark(character)
            || open && diacritic(character)
        {
            if !open {
                tokens.push((offset, end, String::new()));
                open = true;
            }
            let token = tokens.last_mut().expect("an open token");
            token.1 = end;
            fold(character, &mut token.2);
        } else {
            open = false;
        }
    }
    tokens
}

/// The excerpt FTS5's `snippet(lyrics_fts,0,char(1),char(2),'…',32)` would build from the
/// document, which the contentless table does not keep: the window with the most distinct
/// phrases, preferring a sentence start, matches marked with `\u{1}` and `\u{2}`.
#[expect(
    clippy::string_slice,
    reason = "Offsets are token boundaries from char_indices on the same text"
)]
fn excerpt(text: &str, phrases: &[Phrase]) -> String {
    let tokens = tokens(text);
    let phrases = phrases
        .iter()
        .map(|phrase| {
            let tokens = self::tokens(&phrase.text);
            (
                tokens.into_iter().map(|token| token.2).collect::<Vec<_>>(),
                phrase.prefix,
            )
        })
        .collect::<Vec<_>>();
    // Phrase instances in position order: (position, phrase, length).
    let mut instances = Vec::new();
    for position in 0..tokens.len() {
        for (index, (phrase, prefix)) in phrases.iter().enumerate() {
            let matched = !phrase.is_empty()
                && phrase.iter().enumerate().all(|(offset, wanted)| {
                    tokens.get(position + offset).is_some_and(|token| {
                        if *prefix && offset + 1 == phrase.len() {
                            token.2.starts_with(wanted.as_str())
                        } else {
                            token.2 == *wanted
                        }
                    })
                });
            if matched {
                instances.push((position as i64, index, phrase.len() as i64));
            }
        }
    }
    let size = tokens.len() as i64;
    let score = |start: i64| {
        let mut seen = vec![false; phrases.len()];
        let (mut score, mut first, mut last) = (0, -1, 0);
        for &(position, phrase, length) in &instances {
            if (start..start + EXCERPT_TOKENS).contains(&position) {
                score += if seen[phrase] { 1 } else { 1000 };
                seen[phrase] = true;
                if first < 0 {
                    first = position;
                }
                last = position + length;
            }
        }
        let centered = first - (EXCERPT_TOKENS - (last - first)) / 2;
        (score, centered.min(size - EXCERPT_TOKENS).max(0))
    };
    let bytes = text.as_bytes();
    let sentences = tokens
        .iter()
        .enumerate()
        .filter(|(position, (start, ..))| {
            *position == 0
                || bytes[..*start]
                    .iter()
                    .rposition(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
                    .is_some_and(|kept| kept + 1 < *start && matches!(bytes[kept], b'.' | b':'))
        })
        .map(|(position, _)| position as i64)
        .collect::<Vec<_>>();
    let (mut best, mut start) = (0, 0);
    for &(position, ..) in &instances {
        let (value, centered) = score(position);
        if value > best {
            (best, start) = (value, centered);
        }
        let sentence = sentences
            .iter()
            .rev()
            .find(|sentence| **sentence <= position);
        if let Some(&sentence) =
            sentence.filter(|sentence| size > EXCERPT_TOKENS && **sentence < position)
        {
            let value = score(sentence).0 + if sentence == 0 { 120 } else { 100 };
            if value > best {
                (best, start) = (value, sentence);
            }
        }
    }
    // Overlapping instances coalesce; a span that starts before the window is not shown, and
    // spans that touch are marked as one.
    let mut spans = Vec::<(i64, i64)>::new();
    for &(position, _, length) in &instances {
        match spans.last_mut() {
            Some(span) if position <= span.1 => span.1 = span.1.max(position + length - 1),
            _ => spans.push((position, position + length - 1)),
        }
    }
    let end = start + EXCERPT_TOKENS - 1;
    let mut marks = Vec::<(usize, usize)>::new();
    for (first, last) in spans
        .into_iter()
        .filter(|span| (start..=end).contains(&span.0))
    {
        let (from, to) = (tokens[first as usize].0, tokens[last.min(end) as usize].1);
        match marks.last_mut() {
            Some(mark) if mark.1 == from => mark.1 = to,
            _ => marks.push((from, to)),
        }
    }
    let mut excerpt = String::new();
    let mut copied = 0;
    if start > 0 {
        excerpt.push('…');
        copied = tokens[start as usize].0;
    }
    for (from, to) in marks {
        excerpt.push_str(&text[copied..from]);
        excerpt.push('\u{1}');
        excerpt.push_str(&text[from..to]);
        excerpt.push('\u{2}');
        copied = to;
    }
    if end >= size - 1 {
        excerpt.push_str(&text[copied..]);
    } else {
        excerpt.push_str(&text[copied..tokens[end as usize].1]);
        excerpt.push('…');
    }
    excerpt
}

#[expect(
    clippy::string_slice,
    reason = "Offsets come from char_indices on the same text"
)]
fn snippet(value: &str) -> LyricsSnippet {
    let mut parts = Vec::new();
    let mut highlighted = false;
    let mut start = 0;
    for (offset, character) in value.char_indices() {
        if !matches!(character, '\u{1}' | '\u{2}') {
            continue;
        }
        if start < offset {
            parts.push(LyricsSnippetPart {
                text: value[start..offset].to_owned(),
                highlighted,
            });
        }
        highlighted = character == '\u{1}';
        start = offset + character.len_utf8();
    }
    if start < value.len() {
        parts.push(LyricsSnippetPart {
            text: value[start..].to_owned(),
            highlighted,
        });
    }
    // Keep the first match visible in the two-line result, even after a stanza break.
    if let Some(context) = parts.first_mut().filter(|part| !part.highlighted) {
        let line = context.text.rsplit('\n').next().unwrap_or_default();
        let offset = line
            .char_indices()
            .rev()
            .nth(11)
            .map_or(0, |(offset, _)| offset);
        let retained = &line[offset..];
        if retained.len() < context.text.len() {
            context.text = format!("…{}", retained.trim_start());
        }
    }
    for part in &mut parts {
        part.text = part
            .text
            .chars()
            .map(|character| {
                if character.is_whitespace() {
                    ' '
                } else {
                    character
                }
            })
            .collect();
    }
    LyricsSnippet { parts }
}
