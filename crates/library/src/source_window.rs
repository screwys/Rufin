//! Shared ordering and bounded reads for source projections.
use crate::{FolderKey, LibraryResult, QueueCollection, QueueScope, QueueSource, SourceKey};
use sqlx::SqliteConnection;
pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) struct SourceQuery {
    pub from: String,
    pub predicate: String,
    pub order: Vec<String>,
    pub uri: String,
    pub entry_key: String,
}

impl SourceQuery {
    pub async fn count(&self, connection: &mut SqliteConnection) -> LibraryResult<i64> {
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM {} WHERE {}",
            self.from, self.predicate
        )))
        .fetch_one(connection)
        .await?)
    }

    pub fn select(&self, columns: &str) -> String {
        format!(
            "SELECT {columns} FROM {} WHERE {} ORDER BY {}",
            self.from,
            self.predicate,
            self.order.join(",")
        )
    }

    pub async fn members(
        mut self,
        connection: &mut SqliteConnection,
        anchor: Option<String>,
    ) -> LibraryResult<Vec<(String, Option<String>, bool)>> {
        let identity = if self.entry_key == "NULL" {
            "NULL".to_string()
        } else {
            self.predicate.push_str(" AND playlist.name IS NOT NULL");
            "json_array((SELECT object_id FROM sources WHERE source_key=playlist.source_key),playlist.object_id,entry.object_id)".to_string()
        };
        let columns = format!(
            "{},{identity},COALESCE(({}),0)",
            self.uri,
            anchor.unwrap_or_else(|| "0".into())
        );
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(self.select(&columns)))
            .fetch_all(connection)
            .await?)
    }
}

pub(crate) async fn selected_values_on<T>(
    connection: &mut SqliteConnection,
    query: &crate::source_window::SourceQuery,
    column: &str,
    ranges: &[std::ops::Range<usize>],
) -> LibraryResult<Vec<T>>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> + Send + Unpin,
{
    let Some(first) = ranges.first() else {
        return Ok(Vec::new());
    };
    if ranges.len() == 1 {
        let sql = format!("{} LIMIT ?1 OFFSET ?2", query.select(column));
        return Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(first.len() as i64)
            .bind(first.start as i64)
            .persistent(false)
            .fetch_all(connection)
            .await?);
    }
    let columns = format!(
        "{column} value, row_number() OVER (ORDER BY {})-1 position",
        query.order.join(",")
    );
    let sql = format!(
        "WITH ordered AS ({} LIMIT ?1)
         SELECT value FROM ordered WHERE EXISTS (
           SELECT 1 FROM json_each(?2) selected
           WHERE position>=json_extract(selected.value,'$.start')
             AND position<json_extract(selected.value,'$.end')) ORDER BY position",
        query.select(&columns),
    );
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(ranges.iter().map(|range| range.end).max().unwrap_or(0) as i64)
        .bind(serde_json::to_string(ranges)?)
        .persistent(false)
        .fetch_all(connection)
        .await?)
}

pub(crate) async fn source_members(
    connection: &mut SqliteConnection,
    source: &QueueSource,
) -> LibraryResult<Vec<(String, Option<String>, bool)>> {
    source_members_on(connection, source, None).await
}

pub(crate) async fn legacy_source_members(
    connection: &mut SqliteConnection,
    source: &QueueSource,
    seed: Option<u64>,
) -> LibraryResult<Vec<(String, Option<String>, bool)>> {
    let mut rows = source_members_on(connection, source, seed).await?;
    if seed.is_none()
        && let Some(anchor) = rows.iter().position(|row| row.2)
    {
        rows.drain(..anchor);
    }
    Ok(rows)
}

async fn source_members_on(
    connection: &mut SqliteConnection,
    source: &QueueSource,
    legacy_seed: Option<u64>,
) -> LibraryResult<Vec<(String, Option<String>, bool)>> {
    let mut query = match &source.scope {
        QueueScope::Smart {
            reference,
            now,
            display_sort,
        } => {
            let mut uris = crate::smart_playlists::smart_members_ref(
                connection,
                reference,
                *now,
                &source.filter,
                *display_sort,
                source.descending,
            )
            .await?;
            if let Some(seed) = legacy_seed {
                let anchor = source.anchor_uri.as_deref();
                let known = if let Some(uri) = anchor {
                    sqlx::query_scalar::<_, i64>("SELECT track_key FROM tracks WHERE media_uri=?1")
                        .bind(uri)
                        .fetch_optional(&mut *connection)
                        .await?
                } else {
                    None
                };
                let catalog_first = anchor.is_none() || known.is_some();
                let pivot = known
                    .map(|key| (key * 1103515245) % 2147483647)
                    .unwrap_or((seed % 2147483647) as i64);
                uris=sqlx::query_scalar(
                    "SELECT requested.value FROM json_each(?1) requested LEFT JOIN tracks track ON track.media_uri=requested.value
                     ORDER BY CASE WHEN track.track_key IS NOT NULL THEN ?2+(((track.track_key*1103515245)%2147483647)<?3)
                                   ELSE (2-?2)+(requested.value<COALESCE(?4,'')) END,
                              CASE WHEN track.track_key IS NOT NULL THEN (track.track_key*1103515245)%2147483647 END, requested.value")
                    .bind(serde_json::to_string(&uris)?).bind(if catalog_first {0}else{2}).bind(pivot)
                    .bind(if catalog_first {None}else{anchor}).fetch_all(&mut *connection).await?;
            }
            return Ok(uris
                .into_iter()
                .map(|uri| {
                    let selected = source.anchor_uri.as_ref() == Some(&uri);
                    (uri, None, selected)
                })
                .collect());
        }

        QueueScope::Tracks {
            source: id,
            folder,
            favorites_only,
            downloaded_only,
            ..
        } => {
            let Some(key) = sqlx::query_scalar::<_, SourceKey>(
                "SELECT source_key FROM sources WHERE object_id=?1",
            )
            .bind(id.as_str())
            .fetch_optional(&mut *connection)
            .await?
            else {
                return Ok(Vec::new());
            };
            let folder = if let Some(id) = folder {
                let Some(key) = sqlx::query_scalar::<_, FolderKey>(
                    "SELECT folder_key FROM folders WHERE source_key=?1 AND object_id=?2",
                )
                .bind(key)
                .bind(id)
                .fetch_optional(&mut *connection)
                .await?
                else {
                    return Ok(Vec::new());
                };
                Some(key)
            } else {
                None
            };
            let mut query = crate::tracks::track_query(
                key,
                source.sort,
                source.descending,
                *favorites_only,
                folder,
                &source.filter,
            );
            if *downloaded_only {
                crate::tracks::downloaded_filter(&mut query);
            }
            query
        }
        QueueScope::Collection {
            reference,
            favorites_only,
            downloaded_only,
        } => {
            let Some((collection, folder)) =
                crate::collections::resolve_collection_reference(connection, reference).await?
            else {
                return Ok(Vec::new());
            };
            let mut query = crate::collections::playback_query(
                connection,
                &collection,
                folder,
                &source.filter,
                source.sort,
                source.descending,
                *favorites_only,
            )
            .await?;
            if *downloaded_only {
                crate::tracks::downloaded_filter(&mut query);
            }
            query
        }
        QueueScope::Playlist {
            reference, sort, ..
        } => {
            let Some((QueueCollection::Playlist(key), folder)) =
                crate::collections::resolve_collection_reference(connection, reference).await?
            else {
                return Ok(Vec::new());
            };
            crate::playlists::playlist_query(key, folder, *sort, source.descending, &source.filter)
        }
    };
    let anchor = if let QueueScope::Playlist {
        anchor_entry: Some(entry),
        ..
    } = &source.scope
    {
        Some(format!("entry.object_id={}", quote(entry)))
    } else {
        source
            .anchor_uri
            .as_ref()
            .map(|uri| format!("{}={}", query.uri, quote(uri)))
    };

    if let Some(seed) = legacy_seed {
        let key = if query.entry_key == "NULL" {
            "track.track_key".to_string()
        } else {
            query.entry_key.clone()
        };
        let hash = format!("({key}*1103515245)%2147483647");
        let mut pivot = (seed % 2147483647) as i64;
        if let Some(anchor) = &anchor
            && let Some(value) = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
                "SELECT {hash} FROM {} WHERE {} AND ({anchor}) LIMIT 1",
                query.from, query.predicate
            )))
            .fetch_optional(&mut *connection)
            .await?
        {
            pivot = value;
        }
        query.order = vec![
            format!("(({hash})<{pivot}) ASC"),
            format!("{hash} ASC"),
            format!("{key} ASC"),
        ];
    }
    query.members(connection, anchor).await
}
