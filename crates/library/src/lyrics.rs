//! Owns one bounded lyrics cache row keyed by media URI, authority, role, language, and script,
//! and the search documents of each row. Fetching and provider policy remain outside Library.

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqliteConnection};

use crate::catalog::write::Write;
use crate::{Catalog, LibraryError, LibraryResult, ReadCancellation};

const LYRICS_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
/// A row has at most this many search documents; its FTS rowids are `lyrics_key << 2 | ordinal`.
const DOCUMENTS_PER_ROW: usize = 4;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, FromRow)]
pub struct LyricsCacheRow {
    pub media_uri: String,
    pub authority: String,
    pub role: String,
    pub language: String,
    pub script: String,
    pub lyrics: String,
    pub updated_at: i64,
    /// A person picked these lyrics; an automatic fetch does not replace them.
    pub picked: bool,
}

/// The display text of each version-4 lyric document that has any, by its position.
pub(crate) fn documents(payload: &str) -> Vec<(usize, String)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return Vec::new();
    };
    if value.get("version").and_then(serde_json::Value::as_i64) != Some(4) {
        return Vec::new();
    }
    let Some(documents) = value
        .pointer("/bundle/content/Documents")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    documents
        .iter()
        .enumerate()
        .filter_map(|(ordinal, document)| {
            let text = document
                .get("lines")
                .and_then(serde_json::Value::as_array)?
                .iter()
                .filter_map(|line| line.get("text")?.as_str())
                .map(|text| text.replace(['\u{1}', '\u{2}'], ""))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then_some((ordinal, text))
        })
        .take_while(|(ordinal, _)| *ordinal < DOCUMENTS_PER_ROW)
        .collect()
}

/// Indexes the documents of a row whose search rows were removed or never written.
async fn index_lyrics(
    connection: &mut SqliteConnection,
    key: i64,
    payload: &str,
) -> LibraryResult<()> {
    for (ordinal, text) in documents(payload) {
        sqlx::query("INSERT INTO lyrics_fts(rowid,text) VALUES(?1,?2)")
            .bind(key << 2 | ordinal as i64)
            .bind(text)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

/// Removes the search rows of these lyrics rows. A contentless table keeps a second row for a
/// rowid inserted twice, so every rewrite of a row removes its search rows first.
pub(crate) async fn unindex_lyrics(
    connection: &mut SqliteConnection,
    keys: &[i64],
) -> LibraryResult<()> {
    if keys.is_empty() {
        return Ok(());
    }
    sqlx::query("DELETE FROM lyrics_fts WHERE rowid IN(SELECT (row.value<<2)|ordinal.value FROM json_each(?1) row,json_each('[0,1,2,3]') ordinal)")
        .bind(serde_json::to_string(keys)?)
        .execute(connection)
        .await?;
    Ok(())
}

/// Indexes every stored row, for a Store whose search table was just created.
pub(crate) async fn index_all_lyrics(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let mut after = 0_i64;
    loop {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT lyrics_key,lyrics FROM lyrics_cache WHERE lyrics_key>?1 ORDER BY lyrics_key LIMIT 512",
        )
        .bind(after)
        .fetch_all(&mut *connection)
        .await?;
        let Some((last, _)) = rows.last() else {
            return Ok(());
        };
        after = *last;
        for (key, payload) in rows {
            index_lyrics(connection, key, &payload).await?;
        }
    }
}

/// Writes rows whose payload differs, with their search documents. Rows already stored are
/// replaced only when `replace` holds for them.
async fn put_lyrics_on(
    write: &mut Write<'_>,
    rows: &[LyricsCacheRow],
    replace: &str,
) -> LibraryResult<()> {
    for row in rows {
        if row.authority.is_empty()
            || row.role.is_empty()
            || row.updated_at < 0
            || row.lyrics.len() > LYRICS_PAYLOAD_BYTES
        {
            return Err(LibraryError::InvalidRequest(
                "invalid lyrics cache row".to_string(),
            ));
        }
        let connection = write.connection().await?;
        let key: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "INSERT INTO lyrics_cache(media_uri,authority,role,language,script,lyrics,updated_at,picked) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(media_uri,authority,role,language,script) DO UPDATE SET lyrics=excluded.lyrics,updated_at=excluded.updated_at,picked=excluded.picked
             WHERE (lyrics_cache.lyrics IS NOT excluded.lyrics OR lyrics_cache.picked<>excluded.picked)
               AND (excluded.picked OR NOT lyrics_cache.picked) AND {replace} RETURNING lyrics_key"
        )))
        .bind(&row.media_uri)
        .bind(&row.authority)
        .bind(&row.role)
        .bind(&row.language)
        .bind(&row.script)
        .bind(&row.lyrics)
        .bind(row.updated_at)
        .bind(row.picked)
        .fetch_optional(&mut *connection)
        .await?;
        if let Some(key) = key {
            unindex_lyrics(connection, &[key]).await?;
            index_lyrics(connection, key, &row.lyrics).await?;
            // A row written without the mark never replaced a picked one, and no client shows
            // the cache as it changes.
            if row.picked {
                write.changed(crate::Scope::Lyrics {
                    uri: row.media_uri.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Deletes the rows a condition names, with their search documents.
async fn remove_lyrics_on(
    write: &mut Write<'_>,
    condition: &str,
    binds: &[&str],
) -> LibraryResult<u64> {
    let connection = write.connection().await?;
    let mut query = sqlx::query_as::<_, (i64, String, bool)>(sqlx::AssertSqlSafe(format!(
        "DELETE FROM lyrics_cache WHERE {condition} RETURNING lyrics_key,media_uri,picked"
    )));
    for bind in binds {
        query = query.bind(*bind);
    }
    let removed = query.fetch_all(&mut *connection).await?;
    unindex_lyrics(
        connection,
        &removed.iter().map(|(key, ..)| *key).collect::<Vec<_>>(),
    )
    .await?;
    let mut uris = removed
        .iter()
        .filter(|(.., picked)| *picked)
        .map(|(_, uri, _)| uri)
        .collect::<Vec<_>>();
    uris.sort_unstable();
    uris.dedup();
    for uri in uris {
        write.changed(crate::Scope::Lyrics { uri: uri.clone() });
    }
    Ok(removed.len() as u64)
}

/// Source lyrics replace the stored source lyrics of the same track.
pub(crate) async fn put_source_lyrics_on(
    write: &mut Write<'_>,
    rows: &[(&str, &str)],
) -> LibraryResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let now = chrono::Utc::now().timestamp();
    for (uri, payload) in rows {
        let empty = serde_json::from_str::<serde_json::Value>(payload)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/bundle/content/Documents")?
                    .as_array()
                    .map(Vec::is_empty)
            });
        let stale = if empty == Some(true) {
            "media_uri=?1 AND authority='source'"
        } else {
            "media_uri=?1 AND authority='source' AND(role<>'original' OR language<>'' OR script<>'')"
        };
        remove_lyrics_on(write, stale, &[uri]).await?;
        if empty != Some(true) {
            put_lyrics_on(
                write,
                &[LyricsCacheRow {
                    media_uri: (*uri).to_owned(),
                    authority: "source".into(),
                    role: "original".into(),
                    language: String::new(),
                    script: String::new(),
                    lyrics: (*payload).to_owned(),
                    updated_at: now,
                    picked: false,
                }],
                "true",
            )
            .await?;
        }
    }
    Ok(())
}

/// A track that moved to another URI takes its lyrics along, unless the new URI has its own.
pub(crate) async fn move_lyrics_on(
    write: &mut Write<'_>,
    from: &str,
    to: &str,
) -> LibraryResult<()> {
    let connection = write.connection().await?;
    let copied: Vec<(i64, String, bool)> = sqlx::query_as("INSERT INTO lyrics_cache(media_uri,authority,role,language,script,lyrics,updated_at,picked) SELECT ?2,authority,role,language,script,lyrics,updated_at,picked FROM lyrics_cache WHERE media_uri=?1 ORDER BY lyrics_key ON CONFLICT DO NOTHING RETURNING lyrics_key,lyrics,picked")
        .bind(from)
        .bind(to)
        .fetch_all(&mut *connection)
        .await?;
    for (key, payload, _) in &copied {
        index_lyrics(connection, *key, payload).await?;
    }
    if copied.iter().any(|(.., picked)| *picked) {
        write.changed(crate::Scope::Lyrics { uri: to.to_owned() });
    }
    remove_lyrics_on(write, "media_uri=?1", &[from]).await?;
    Ok(())
}

impl Catalog {
    /// Tracks of a source without source lyrics.
    pub async fn uncached_source_lyrics_page(
        &self,
        source: crate::SourceKey,
        after: Option<crate::TrackKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(crate::TrackKey, String)>> {
        let mut connection = self.acquire_cancellable(cancellation).await?;
        Ok(sqlx::query_as(
            "SELECT track_key,media_uri FROM tracks track
            WHERE source_key=?1 AND indexed=1 AND track_key>?2
              AND NOT EXISTS(SELECT 1 FROM lyrics_cache lyrics
                WHERE lyrics.media_uri=track.media_uri AND lyrics.authority='source')
            ORDER BY track_key LIMIT 128",
        )
        .bind(source)
        .bind(after.map_or(0, crate::TrackKey::raw))
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn clear_lyrics_cache(&self, active_media_uris: &[String]) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let active = serde_json::to_string(active_media_uris)?;
        remove_lyrics_on(
            &mut write,
            "NOT picked AND NOT EXISTS(SELECT 1 FROM downloads download WHERE download.media_uri=lyrics_cache.media_uri) AND media_uri NOT IN (SELECT value FROM json_each(?1))",
            &[&active],
        )
        .await?;
        write.commit().await?;
        Ok(())
    }

    pub async fn lyrics_cache_for_role(
        &self,
        media_uri: &str,
        role: &str,
        language: &str,
        script: &str,
        authority: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<LyricsCacheRow>> {
        let mut connection = self.acquire_cancellable(cancellation).await?;
        Ok(sqlx::query_as::<_, LyricsCacheRow>(
            "SELECT media_uri,authority,role,language,script,lyrics,updated_at,picked
             FROM lyrics_cache WHERE media_uri=?1
               AND (?5='source' OR (role=?2 AND language=?3 AND script=?4))
               AND authority=?5 ORDER BY updated_at DESC,role='original' DESC LIMIT 1",
        )
        .bind(media_uri)
        .bind(role)
        .bind(language)
        .bind(script)
        .bind(authority)
        .fetch_optional(&mut *connection)
        .await?)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn write_lyrics_cache(
        &self,
        media_uri: &str,
        authority: &str,
        role: &str,
        language: &str,
        script: &str,
        lyrics: &str,
        updated_at: i64,
        picked: bool,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        put_lyrics_on(
            &mut write,
            &[LyricsCacheRow {
                media_uri: media_uri.to_owned(),
                authority: authority.to_owned(),
                role: role.to_owned(),
                language: language.to_owned(),
                script: script.to_owned(),
                lyrics: lyrics.to_owned(),
                updated_at,
                picked,
            }],
            "true",
        )
        .await?;
        write.commit().await?;
        Ok(())
    }

    /// The lyrics people picked, all of them or those of `uris`, in URI order.
    pub async fn picked_lyrics(
        &self,
        uris: Option<&[String]>,
    ) -> LibraryResult<Vec<LyricsCacheRow>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as(
            "SELECT media_uri,authority,role,language,script,lyrics,updated_at,picked FROM lyrics_cache
             WHERE picked AND (?1 IS NULL OR media_uri IN(SELECT value FROM json_each(?1)))
             ORDER BY media_uri,authority,role,language,script",
        )
        .bind(uris.map(serde_json::to_string).transpose()?)
        .fetch_all(&mut *connection)
        .await?)
    }

    /// Makes the picked lyrics of each URI the given rows, in one Write.
    pub async fn replace_picked_lyrics(
        &self,
        tracks: &[(String, Vec<LyricsCacheRow>)],
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        for (uri, rows) in tracks {
            let kept = rows
                .iter()
                .map(|row| (&row.authority, &row.role, &row.language, &row.script))
                .collect::<Vec<_>>();
            let kept = serde_json::to_string(&kept)?;
            remove_lyrics_on(
                &mut write,
                "media_uri=?1 AND picked AND (authority,role,language,script) NOT IN(SELECT value->>0,value->>1,value->>2,value->>3 FROM json_each(?2))",
                &[uri, &kept],
            )
            .await?;
            put_lyrics_on(&mut write, rows, "true").await?;
        }
        write.commit().await?;
        Ok(())
    }

    pub async fn remove_lyrics_cache(
        &self,
        media_uri: &str,
        authority: &str,
        role: &str,
        language: &str,
        script: &str,
    ) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let removed = remove_lyrics_on(
            &mut write,
            "media_uri=?1 AND authority=?2 AND role=?3 AND language=?4 AND script=?5",
            &[media_uri, authority, role, language, script],
        )
        .await?;
        write.commit().await?;
        Ok(removed == 1)
    }

    pub async fn remove_track_lyrics_by_authority(
        &self,
        media_uri: &str,
        authority: &str,
    ) -> LibraryResult<u64> {
        let mut write = self.write().await?;
        let removed = remove_lyrics_on(
            &mut write,
            "media_uri=?1 AND authority=?2",
            &[media_uri, authority],
        )
        .await?;
        write.commit().await?;
        Ok(removed)
    }
}
