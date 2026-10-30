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
        for expression in expressions {
            // Read in index order, stopping after a page of distinct tracks.
            let mut rows = sqlx::query_as::<_, (i64, String)>(
                "SELECT track.track_key,''
                     FROM lyrics_fts
                     JOIN lyrics_cache lyrics ON lyrics.lyrics_key=lyrics_fts.rowid>>2
                     JOIN tracks track ON track.media_uri=lyrics.media_uri
                     WHERE lyrics_fts MATCH ?1
                       AND track.track_key NOT IN(SELECT value FROM json_each(?2))
                     ORDER BY lyrics_fts.rowid",
            )
            .bind(&expression)
            .bind(serde_json::to_string(&keys)?)
            .fetch(&mut *transaction);
            while let Some((key, excerpt)) = rows.try_next().await? {
                if keys.contains(&key) {
                    continue;
                }
                keys.push(key);
                lyrics.insert(TrackKey::from_raw(key), snippet(&excerpt));
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

fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn expressions(query: &str) -> Vec<String> {
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
    let phrase = quoted(
        &terms
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    );
    let exact = terms
        .iter()
        .map(|(text, _)| quoted(text))
        .collect::<Vec<_>>()
        .join(" AND ");
    let mut result = vec![phrase];
    if !result.contains(&exact) {
        result.push(exact);
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
        let mut prefix = terms
            .iter()
            .map(|(text, _)| quoted(text))
            .collect::<Vec<_>>();
        prefix.last_mut().expect("nonempty terms").push('*');
        result.push(prefix.join(" AND "));
    }
    result
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
