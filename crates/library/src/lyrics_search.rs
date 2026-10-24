//! The Store indexes saved lyric documents and returns bounded track results with excerpts.

use std::collections::BTreeMap;

use futures_util::TryStreamExt;
use sqlx::{Connection, SqliteConnection};

use crate::{
    Database, LibraryResult, LyricsSnippet, LyricsSnippetPart, ReadCancellation, SearchRequest,
    SearchResults, TrackKey,
};

const LYRICS_SEARCH_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS lyrics_documents (
    document_key INTEGER PRIMARY KEY,
    media_uri TEXT NOT NULL,
    authority TEXT NOT NULL,
    role TEXT NOT NULL,
    language TEXT NOT NULL,
    script TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    text TEXT NOT NULL,
    UNIQUE(media_uri, authority, role, language, script, ordinal),
    FOREIGN KEY(media_uri, authority, role, language, script)
        REFERENCES lyrics_cache(media_uri, authority, role, language, script)
        ON DELETE CASCADE ON UPDATE CASCADE
) STRICT;

-- Only the display text of each version-4 lyric document is searchable.
CREATE VIEW IF NOT EXISTS lyrics_search_input AS
SELECT cache.rowid cache_rowid, cache.media_uri, cache.authority, cache.role,
    cache.language, cache.script, CAST(document.key AS INTEGER) ordinal,
    (SELECT group_concat(text, char(10)) FROM (
        SELECT replace(replace(json_extract(line.value, '$.text'), char(1), ''), char(2), '') text
        FROM json_each(document.value, '$.lines') line
        ORDER BY CAST(line.key AS INTEGER)
    )) text
FROM lyrics_cache cache,
    json_each(CASE WHEN json_valid(cache.lyrics) THEN
        CASE WHEN json_extract(cache.lyrics, '$.version') = 4
            THEN json_extract(cache.lyrics, '$.bundle.content.Documents') END
        END) document;

CREATE VIRTUAL TABLE IF NOT EXISTS lyrics_fts USING fts5(
    text, content='lyrics_documents', content_rowid='document_key',
    tokenize='rufin_lyrics', prefix='3'
);

CREATE TRIGGER IF NOT EXISTS lyrics_documents_insert AFTER INSERT ON lyrics_documents BEGIN
    INSERT INTO lyrics_fts(rowid, text) VALUES(new.document_key, new.text);
END;
CREATE TRIGGER IF NOT EXISTS lyrics_documents_delete AFTER DELETE ON lyrics_documents BEGIN
    INSERT INTO lyrics_fts(lyrics_fts, rowid, text) VALUES('delete', old.document_key, old.text);
END;
CREATE TRIGGER IF NOT EXISTS lyrics_documents_update AFTER UPDATE OF text ON lyrics_documents BEGIN
    INSERT INTO lyrics_fts(lyrics_fts, rowid, text) VALUES('delete', old.document_key, old.text);
    INSERT INTO lyrics_fts(rowid, text) VALUES(new.document_key, new.text);
END;

CREATE TRIGGER IF NOT EXISTS lyrics_search_insert AFTER INSERT ON lyrics_cache BEGIN
    INSERT INTO lyrics_documents(media_uri,authority,role,language,script,ordinal,text)
        SELECT media_uri,authority,role,language,script,ordinal,text FROM lyrics_search_input
        WHERE cache_rowid=new.rowid AND length(trim(text))>0;
END;
CREATE TRIGGER IF NOT EXISTS lyrics_search_update AFTER UPDATE OF lyrics ON lyrics_cache
WHEN old.lyrics IS NOT new.lyrics BEGIN
    DELETE FROM lyrics_documents WHERE media_uri=new.media_uri AND authority=new.authority
        AND role=new.role AND language=new.language AND script=new.script;
    INSERT INTO lyrics_documents(media_uri,authority,role,language,script,ordinal,text)
        SELECT media_uri,authority,role,language,script,ordinal,text FROM lyrics_search_input
        WHERE cache_rowid=new.rowid AND length(trim(text))>0;
END;
"#;

pub(crate) async fn initialize(
    connection: &mut SqliteConnection,
    backfill: bool,
) -> LibraryResult<()> {
    sqlx::raw_sql(LYRICS_SEARCH_SCHEMA)
        .execute(&mut *connection)
        .await?;
    if backfill {
        let mut after = 0;
        loop {
            let page: Vec<i64> = sqlx::query_scalar(
                "SELECT rowid FROM lyrics_cache WHERE rowid>?1 ORDER BY rowid LIMIT 128",
            )
            .bind(after)
            .fetch_all(&mut *connection)
            .await?;
            let Some(last) = page.last().copied() else {
                break;
            };
            sqlx::query("INSERT INTO lyrics_documents(media_uri,authority,role,language,script,ordinal,text) SELECT media_uri,authority,role,language,script,ordinal,text FROM lyrics_search_input WHERE cache_rowid>?1 AND cache_rowid<=?2 AND length(trim(text))>0")
                .bind(after).bind(last).execute(&mut *connection).await?;
            after = last;
        }
    }
    Ok(())
}

impl Database {
    pub async fn search_tracks_by_lyrics(
        &self,
        request: &SearchRequest,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<SearchResults> {
        let expressions = expressions(request.query());
        if expressions.is_empty() {
            return Ok(SearchResults::default());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let mut keys = Vec::new();
        let mut lyrics = BTreeMap::new();
        for expression in expressions {
            // Read in index order, stopping after a page of distinct tracks.
            let mut rows = sqlx::query_as::<_, (i64, String)>(
                "SELECT track.track_key,snippet(lyrics_fts,0,char(1),char(2),'…',32)
                     FROM lyrics_fts
                     JOIN lyrics_documents document ON document.document_key=lyrics_fts.rowid
                     JOIN tracks track ON track.media_uri=document.media_uri
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
        let tracks = crate::tracks::load_track_requested(
            &mut transaction,
            crate::query::RequestedKeys::batch(&keys)?,
        )
        .await?;
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
