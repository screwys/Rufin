//! Shared ordering and bounded reads for source projections.
use crate::{
    FolderKey, LibraryResult, PlaylistEntryKey, PlaylistEntrySort, PlaylistKey, QueueCollection,
    QueueQuery, QueueScope, QueueSource, SourceId, SourceKey, TrackSort,
};
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
    pub fn select(&self, columns: &str) -> String {
        format!(
            "SELECT {columns} FROM {} WHERE {} ORDER BY {}",
            self.from,
            self.predicate,
            self.order.join(",")
        )
    }
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
        QueueScope::Smart { reference, now } => {
            let mut uris =
                crate::smart_playlists::smart_members_ref(connection, reference, *now).await?;
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
            crate::tracks::track_query(
                key,
                source.sort,
                source.descending,
                *favorites_only,
                folder,
                &source.filter,
                false,
            )
        }
        QueueScope::Collection {
            reference,
            favorites_only,
        } => {
            let Some((collection, folder)) =
                crate::collections::resolve_collection_reference(connection, reference).await?
            else {
                return Ok(Vec::new());
            };
            crate::collections::playback_query(
                connection,
                &collection,
                folder,
                &source.filter,
                source.sort,
                source.descending,
                *favorites_only,
            )
            .await?
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
            format!("abs({})", query.entry_key)
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
    let identity = if query.entry_key == "NULL" {
        "NULL".to_string()
    } else {
        let sources = if query.entry_key == "-entry.playlist_entry_key" {
            "catalog.sources"
        } else {
            "main.source_ids"
        };
        format!(
            "json_array((SELECT object_id FROM {sources} WHERE source_key=playlist.source_key),playlist.object_id,entry.object_id)"
        )
    };
    let columns = format!(
        "{},{identity},COALESCE(({}),0)",
        query.uri,
        anchor.unwrap_or_else(|| "0".into())
    );
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(query.select(&columns)))
        .fetch_all(connection)
        .await?)
}
pub(crate) async fn canonical_query(
    connection: &mut SqliteConnection,
    query: QueueQuery,
    folder: Option<FolderKey>,
    filter: String,
    sort: TrackSort,
    descending: bool,
    anchor_uri: Option<String>,
) -> LibraryResult<Option<QueueSource>> {
    let scope = match query {
        QueueQuery::Tracks {
            source,
            favorites_only,
            recursive,
        } => {
            let Some(source) = sqlx::query_scalar::<_, String>(
                "SELECT object_id FROM sources WHERE source_key=?1",
            )
            .bind(source)
            .fetch_optional(&mut *connection)
            .await?
            else {
                return Ok(None);
            };
            let folder = if let Some(key) = folder {
                let Some(id) = sqlx::query_scalar::<_, String>(
                    "SELECT object_id FROM folders WHERE folder_key=?1",
                )
                .bind(key)
                .fetch_optional(&mut *connection)
                .await?
                else {
                    return Ok(None);
                };
                Some(id)
            } else {
                None
            };
            QueueScope::Tracks {
                source: SourceId::new(source),
                folder,
                favorites_only,
                recursive,
            }
        }
        QueueQuery::Collection {
            collection,
            favorites_only,
        } => {
            let Some(reference) =
                crate::collections::canonical_collection_on(connection, &collection, folder)
                    .await?
            else {
                return Ok(None);
            };
            QueueScope::Collection {
                reference,
                favorites_only,
            }
        }
        QueueQuery::Smart { key, source, now } => {
            let Some(reference) =
                crate::smart_playlists::smart_source_reference(connection, key, source, folder)
                    .await?
            else {
                return Ok(None);
            };
            QueueScope::Smart { reference, now }
        }
    };
    Ok(Some(QueueSource {
        scope,
        filter,
        sort,
        descending,
        anchor_uri,
    }))
}

pub(crate) async fn canonical_playlist_query(
    connection: &mut SqliteConnection,
    key: PlaylistKey,
    folder: Option<FolderKey>,
    filter: String,
    sort: PlaylistEntrySort,
    descending: bool,
    anchor_entry: Option<PlaylistEntryKey>,
    anchor_uri: Option<String>,
) -> LibraryResult<Option<QueueSource>> {
    let Some(reference) = crate::collections::canonical_collection_on(
        connection,
        &QueueCollection::Playlist(key),
        folder,
    )
    .await?
    else {
        return Ok(None);
    };
    let anchor_entry = if let Some(entry) = anchor_entry {
        sqlx::query_scalar::<_,String>(if key.raw()<0 {"SELECT object_id FROM catalog.native_playlist_entries WHERE playlist_entry_key=-?1 AND playlist_key=-?2"} else {"SELECT object_id FROM main.playlist_entries WHERE playlist_entry_key=?1 AND playlist_key=?2"}).bind(entry).bind(key).fetch_optional(connection).await?
    } else {
        None
    };
    Ok(Some(QueueSource {
        scope: QueueScope::Playlist {
            reference,
            sort,
            anchor_entry,
        },
        filter,
        sort: TrackSort::Title,
        descending,
        anchor_uri,
    }))
}
