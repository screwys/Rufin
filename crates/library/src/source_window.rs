//! Shared ordering and bounded reads for source projections.
use crate::{
    FolderKey, LibraryResult, PlaylistEntryKey, PlaylistEntrySort, PlaylistKey, QueueCollection,
    QueueQuery, QueueScope, QueueSource, SourceId, SourceKey, TrackSort,
};
use sqlx::{AssertSqlSafe, SqliteConnection};
pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) struct SourceQuery {
    pub from: String,
    pub predicate: String,
    pub order: Vec<(String, bool)>,
    pub uri: String,
    pub key: String,
    pub entry_key: String,
}

impl SourceQuery {
    pub fn select(&self, columns: &str) -> String {
        format!(
            "SELECT {columns} FROM {} WHERE {} ORDER BY {}",
            self.from,
            self.predicate,
            self.order_sql(false)
        )
    }

    fn order_sql(&self, backwards: bool) -> String {
        self.order
            .iter()
            .map(|(field, desc)| {
                format!("{field} {}", if *desc ^ backwards { "DESC" } else { "ASC" })
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    pub async fn window(
        mut self,
        connection: &mut SqliteConnection,
        after: Option<&str>,
        limit: usize,
        seed: Option<u64>,
        anchor: Option<String>,
        backwards: bool,
    ) -> LibraryResult<Vec<(String, Option<PlaylistEntryKey>, String, Option<String>)>> {
        if seed.is_some() {
            self.order = vec![
                (format!("({}*1103515245)%2147483647", self.key), false),
                (self.key.clone(), false),
            ];
        }
        let columns = self
            .order
            .iter()
            .map(|(field, _)| field.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let (mut phase, mut cursor, mut pivot): (u8, Option<String>, u64) =
            after.map(serde_json::from_str).transpose()?.unwrap_or((
                u8::from(backwards && seed.is_some()),
                None,
                seed.unwrap_or(0) % 2147483647,
            ));
        let mut inclusive = false;
        if after.is_none()
            && let Some(anchor) = anchor
        {
            cursor = sqlx::query_scalar::<_, String>(AssertSqlSafe(format!(
                "SELECT json_array({columns}) FROM {} WHERE {} AND ({anchor}) LIMIT 1",
                self.from, self.predicate
            )))
            .fetch_optional(&mut *connection)
            .await?;
            inclusive = cursor.is_some();
            if seed.is_some()
                && let Some(cursor) = &cursor
            {
                pivot = serde_json::from_str::<serde_json::Value>(cursor)?[0]
                    .as_u64()
                    .unwrap_or(pivot);
                phase = 0;
            }
        }
        let mut rows = Vec::new();
        while rows.len() < limit.min(100) {
            let mut predicate = self.predicate.clone();
            if seed.is_some() {
                predicate.push_str(&format!(
                    " AND {} {} {pivot}",
                    self.order[0].0,
                    if phase == 0 { ">=" } else { "<" }
                ));
            }
            if let Some(cursor) = &cursor {
                let value = quote(cursor);
                let seek = if self.order.iter().all(|(_, desc)| *desc == self.order[0].1) {
                    let values = (0..self.order.len())
                        .map(|i| format!("json_extract({value},'$[{i}]')"))
                        .collect::<Vec<_>>()
                        .join(",");
                    format!(
                        "({columns}){}{}({values})",
                        if self.order[0].1 ^ backwards {
                            "<"
                        } else {
                            ">"
                        },
                        if inclusive { "=" } else { "" }
                    )
                } else {
                    let mut alternatives = Vec::new();
                    for (i, (field, desc)) in self.order.iter().enumerate() {
                        let mut terms = (0..i)
                            .map(|j| {
                                format!("({}) IS json_extract({value},'$[{j}]')", self.order[j].0)
                            })
                            .collect::<Vec<_>>();
                        terms.push(format!(
                            "({field}) {} json_extract({value},'$[{i}]')",
                            if *desc ^ backwards { "<" } else { ">" }
                        ));
                        alternatives.push(format!("({})", terms.join(" AND ")));
                    }
                    if inclusive {
                        alternatives.push(format!("json_array({columns})={value}"));
                    }
                    alternatives.join(" OR ")
                };
                predicate.push_str(&format!(" AND ({seek})"));
            }
            let available = limit.min(100) - rows.len();
            let entry_id = if self.entry_key == "NULL" {
                "NULL"
            } else {
                "entry.object_id"
            };
            let page=sqlx::query_as::<_,(String,Option<PlaylistEntryKey>,String,Option<String>)>(AssertSqlSafe(format!("SELECT {},{},json_array({columns}),{entry_id} FROM {} WHERE {predicate} ORDER BY {} LIMIT {available}",self.uri,self.entry_key,self.from,self.order_sql(backwards))))
                .fetch_all(&mut *connection).await?;
            let complete = page.len() < available;
            for (uri, entry, position, entry_id) in page {
                rows.push((
                    uri,
                    entry,
                    serde_json::to_string(&(phase, Some(position), pivot))?,
                    entry_id,
                ));
            }
            if !complete
                || seed.is_none()
                || (backwards && phase == 0)
                || (!backwards && phase == 1)
            {
                break;
            }
            phase = if backwards { 0 } else { 1 };
            cursor = None;
            inclusive = false;
        }
        Ok(rows)
    }
}

pub(crate) async fn read_source(
    connection: &mut SqliteConnection,
    source: &QueueSource,
    after: Option<&str>,
    limit: usize,
    seed: Option<u64>,
    backwards: bool,
) -> LibraryResult<Vec<(String, Option<PlaylistEntryKey>, String, Option<String>)>> {
    let query = match &source.scope {
        QueueScope::Smart { reference, now } => {
            let rows = if backwards {
                if let Some(anchor) = &source.anchor_uri {
                    crate::smart_playlists::smart_source_history_ref(
                        connection, reference, *now, anchor, limit,
                    )
                    .await?
                } else {
                    crate::smart_playlists::smart_source_last_ref(connection, reference, *now, seed)
                        .await?
                        .into_iter()
                        .collect()
                }
            } else {
                crate::smart_playlists::smart_source_window_ref(
                    connection,
                    reference,
                    *now,
                    after,
                    limit,
                    seed,
                    source.anchor_uri.as_deref(),
                )
                .await?
            };
            return Ok(rows
                .into_iter()
                .map(|(uri, cursor)| (uri, None, cursor, None))
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
    query
        .window(connection, after, limit, seed, anchor, backwards)
        .await
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

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "large isolated source and persistence verification"]
    async fn million_track_queue_reads_and_saves_stay_bounded() {
        use std::sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        };
        let dir = tempfile::tempdir().unwrap();
        let database = crate::Database::open(dir.path().join("queue.sqlite3"))
            .await
            .unwrap();
        let mut previous = 0;
        for size in [300_000, 1_000_000] {
            let source;
            {
                let mut writer = database.writer().await.unwrap();
                let connection = writer.as_mut().unwrap();
                sqlx::query("INSERT OR IGNORE INTO sources(source_key,object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES(1,'source','Source','source',zeroblob(32),zeroblob(32))").execute(&mut *connection).await.unwrap();
                source = SourceKey::from_raw(1);
                sqlx::query("WITH RECURSIVE n(i) AS(VALUES(?1) UNION ALL SELECT i+1 FROM n WHERE i<?2) INSERT INTO tracks(track_key,source_key,object_id,media_uri,title,normalized_search,display_album,display_artist,sort_text,duration_millis) SELECT i,1,'track-'||i,'https://example.test/'||i,'Track '||i,'track','Album','Artist',printf('%07d',i),1000 FROM n").bind(previous+1).bind(size).execute(&mut *connection).await.unwrap();
                for seed in [None, Some(192837)] {
                    let count = Arc::new(AtomicU64::new(0));
                    let ticks = count.clone();
                    connection
                        .lock_handle()
                        .await
                        .unwrap()
                        .set_progress_handler(100, move || {
                            ticks.fetch_add(1, Ordering::Relaxed);
                            true
                        });
                    let started = std::time::Instant::now();
                    let first = crate::tracks::track_query(
                        source,
                        TrackSort::Title,
                        false,
                        false,
                        None,
                        "",
                        false,
                    )
                    .window(connection, None, 100, seed, None, false)
                    .await
                    .unwrap();
                    let next = crate::tracks::track_query(
                        source,
                        TrackSort::Title,
                        false,
                        false,
                        None,
                        "",
                        false,
                    )
                    .window(
                        connection,
                        first.last().map(|row| row.2.as_str()),
                        100,
                        seed,
                        None,
                        false,
                    )
                    .await
                    .unwrap();
                    connection
                        .lock_handle()
                        .await
                        .unwrap()
                        .remove_progress_handler();
                    let steps = count.load(Ordering::Relaxed) * 100;
                    tracing::info!(
                        "tracks={size}, shuffle={seed:?}, two 100-member reads: {:?}, ~{steps} VM instructions",
                        started.elapsed()
                    );
                    assert_eq!(first.len(), 100);
                    assert_eq!(next.len(), 100);
                    assert!(steps < 50_000, "source read scanned unrelated tracks");
                }
            }
            for seed in [None, Some(192837)] {
                let started = std::time::Instant::now();
                let page = database
                    .read_queue(crate::QueueReadRequest {
                        input: crate::QueueInput::Query {
                            query: QueueQuery::Tracks {
                                source,
                                favorites_only: false,
                                recursive: false,
                            },
                            folder: None,
                            filter: String::new(),
                            sort: TrackSort::Title,
                            descending: false,
                            context_id: "tracks".into(),
                            anchor_uri: Some(format!("https://example.test/{}", size - 150)),
                        },
                        cursor: crate::QueueCursor {
                            seed,
                            anchor: Some((size - 151) as usize),
                            ..Default::default()
                        },
                        limit: 100,
                        history: true,
                        backwards: false,
                    })
                    .await
                    .unwrap();
                let read = started.elapsed();
                let state = crate::QueueRestore {
                    current_index: Some(page.current_index),
                    sources: vec![crate::QueueInstruction {
                        input: page.input,
                        repeat: true,
                        seed: page.cursor.seed,
                    }],
                    pending: [page.cursor].into(),
                    next_id: 100,
                    occurrences: page
                        .items
                        .into_iter()
                        .enumerate()
                        .map(
                            |(i, (item, provenance, canonical_position, playlist_entry_id))| {
                                Arc::new(crate::QueueOccurrence {
                                    occurrence: format!("queue:{i}").into(),
                                    item,
                                    provenance,
                                    canonical_position,
                                    playlist_entry_id,
                                    source_index: Some(0),
                                })
                            },
                        )
                        .collect(),
                    ..Default::default()
                };
                let save = std::time::Instant::now();
                database.save_queue(&state).await.unwrap();
                tracing::info!(
                    "tracks={size}, shuffle={seed:?}, clicked playback read+metadata: {read:?}, save: {:?}",
                    save.elapsed()
                );
                assert_eq!(database.restore_queue().await.unwrap(), state);
                let mut writer = database.writer().await.unwrap();
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM queue_occurrences")
                        .fetch_one(writer.as_mut().unwrap())
                        .await
                        .unwrap(),
                    100
                );
            }
            previous = size;
        }
    }
}
