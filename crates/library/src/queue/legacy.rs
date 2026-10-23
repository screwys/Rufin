use super::*;
use crate::query::{OrderedQuery, quote};
use crate::{AlbumKey, ArtistKey, Collection, FolderKey, GenreKey, MoodKey, SmartPlaylistKey};

#[derive(serde::Deserialize)]
struct SavedQueue {
    entries: Vec<SavedEntry>,
    #[serde(default)]
    contexts: Vec<SavedContext<Arc<str>>>,
    #[serde(default)]
    order: Arc<[u32]>,
    #[serde(default)]
    current_index: Option<usize>,
    #[serde(default)]
    progress_millis: i64,
    #[serde(default)]
    repeat_mode: QueueRepeatMode,
    #[serde(default)]
    shuffled: bool,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum SavedEntry {
    Compact((String, String, SavedProvenance, Option<String>)),
    Exported {
        occurrence: String,
        media_uri: String,
        provenance: QueueProvenance,
    },
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum SavedProvenance {
    Context(usize, usize),
    Other(QueueProvenance),
}

pub(crate) async fn stored_rows(
    connection: &mut SqliteConnection,
) -> LibraryResult<(Vec<LegacyQueueRow>, Vec<crate::TrackKey>)> {
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('queue_occurrences','old_store')")
            .fetch_all(&mut *connection)
            .await?;
    let column = |name: &str, absent: &str| {
        if columns.iter().any(|column| column == name) {
            format!("occurrence.{name}")
        } else {
            absent.to_owned()
        }
    };
    let source = column("origin_source", "NULL");
    let position = column("origin_position", "occurrence.position");
    let playlist = column("playlist_entry_id", "NULL");
    let title = column("provenance_context_title", "NULL");
    let records = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT occurrence.object_id,occurrence.media_uri,coalesce({position},occurrence.position) canonical_position,
            {source} source_index,{playlist} playlist_entry_id,{title} context_title,
            occurrence.provenance_kind,occurrence.provenance_context_id,occurrence.provenance_source_rank,track.track_key
        FROM old_store.queue_occurrences occurrence JOIN tracks track USING(media_uri) ORDER BY occurrence.traversal_position"
    )))
    .fetch_all(&mut *connection)
    .await?;
    let mut rows = Vec::with_capacity(records.len());
    let mut keys = Vec::with_capacity(records.len());
    for row in records {
        let provenance = match row.try_get::<String, _>("provenance_kind")?.as_str() {
            "context" => QueueProvenance::Context {
                context_id: row.try_get::<String, _>("provenance_context_id")?.into(),
                context_title: row
                    .try_get::<Option<String>, _>("context_title")?
                    .map(|title| serde_json::from_str(&title).map(Arc::new))
                    .transpose()?,
                source_rank: row.try_get::<i64, _>("provenance_source_rank")? as usize,
            },
            "manual" => QueueProvenance::Manual,
            "random" => QueueProvenance::Random,
            "radio" => QueueProvenance::Radio,
            "auto-dj" => QueueProvenance::AutoDj,
            _ => QueueProvenance::Legacy,
        };
        rows.push(LegacyQueueRow {
            occurrence: row.try_get("object_id")?,
            item: TrackReference {
                media_uri: row.try_get("media_uri")?,
                ..Default::default()
            },
            canonical_position: row.try_get::<i64, _>("canonical_position")? as usize,
            source_index: row
                .try_get::<Option<i64>, _>("source_index")?
                .map(|index| index as usize),
            playlist_entry_id: row.try_get("playlist_entry_id")?,
            provenance,
        });
        keys.push(row.try_get("track_key")?);
    }
    Ok((rows, keys))
}

pub(crate) async fn decode_saved(
    connection: &mut SqliteConnection,
    value: serde_json::Value,
) -> LibraryResult<(QueueRestore, Vec<Option<String>>)> {
    let saved: SavedQueue = serde_json::from_value(value)?;
    let mut entries = Vec::with_capacity(saved.entries.len());
    let mut occurrences = Vec::with_capacity(saved.entries.len());
    for entry in saved.entries {
        let (occurrence, media_uri, provenance) = match entry {
            SavedEntry::Compact((occurrence, uri, provenance, _)) => {
                let provenance = match provenance {
                    SavedProvenance::Context(context, source_rank) => {
                        let (id, title) = match saved.contexts.get(context).ok_or_else(|| {
                            LibraryError::InvalidStore("saved Queue context is missing".into())
                        })? {
                            SavedContext::Titled { id, title } => (id.clone(), Some(title.clone())),
                            SavedContext::Legacy(id) => (id.clone(), None),
                        };
                        QueueProvenance::Context {
                            context_id: id,
                            context_title: title,
                            source_rank,
                        }
                    }
                    SavedProvenance::Other(value) => value,
                };
                (occurrence, uri, provenance)
            }
            SavedEntry::Exported {
                occurrence,
                media_uri,
                provenance,
            } => (occurrence, media_uri, provenance),
        };
        let track_key = crate::catalog::remember_track(
            connection,
            &TrackReference {
                media_uri,
                ..Default::default()
            },
        )
        .await?;
        entries.push(QueueEntry {
            track_key,
            provenance,
            received_from_connect: false,
        });
        occurrences.push(Some(occurrence));
    }
    let ids = (1..=entries.len() as u64).collect();
    Ok((
        QueueRestore {
            ids,
            entries: entries.into(),
            order: saved.order,
            current_index: saved.current_index,
            progress_millis: saved.progress_millis,
            repeat_mode: saved.repeat_mode,
            shuffled: saved.shuffled,
            ..Default::default()
        },
        occurrences,
    ))
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
enum QueueInput {
    Choices(Arc<[Option<QueueChoice>]>),
    Source {
        reference: QueueSource,
        context_id: Arc<str>,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
struct QueueChoice {
    #[serde(default)]
    origin: Option<(usize, usize)>,
    media_uri: String,
    fallback: Option<TrackReference>,
    provenance: QueueProvenance,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
enum QueueScope {
    Tracks {
        source: SourceId,
        folder: Option<String>,
        favorites_only: bool,
    },
    Collection {
        reference: CollectionSourceReference,
        favorites_only: bool,
    },
    Playlist {
        reference: CollectionSourceReference,
        sort: crate::CatalogSort,
        anchor_entry: Option<String>,
    },
    Smart {
        reference: SmartSourceReference,
        now: i64,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
struct QueueSource {
    pub scope: QueueScope,
    pub filter: String,
    pub sort: crate::CatalogSort,
    pub descending: bool,
    pub anchor_uri: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
struct LegacyQueueInstruction {
    pub input: QueueInput,
    /// Displaced source entries belong only to this pass, unlike user additions.
    pub repeat: bool,
    #[serde(default)]
    pub seed: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
struct LegacyQueueCursor {
    pub source: usize,
    pub offset: usize,
    pub seed: Option<u64>,
    pub anchor: Option<usize>,
}

pub(crate) async fn migrate_saved(
    connection: &mut SqliteConnection,
    value: serde_json::Value,
    rows: &[LegacyQueueRow],
) -> LibraryResult<(QueueRestore, Vec<Option<String>>)> {
    let sources: Vec<LegacyQueueInstruction> = serde_json::from_value(value["sources"].clone())?;
    let pending: Vec<LegacyQueueCursor> = serde_json::from_value(value["pending"].clone())?;
    let mut row_keys = HashMap::new();
    for row in rows {
        row_keys.insert(
            row.item.media_uri.clone(),
            persist_metadata(connection, &row.item).await?,
        );
    }
    let mut entries = Vec::new();
    let mut occurrences = Vec::new();
    let mut members: Vec<Vec<Option<usize>>> = Vec::new();
    for source in &sources {
        let mut mapping = Vec::new();
        match &source.input {
            QueueInput::Choices(choices) => {
                for choice in choices.iter() {
                    let Some(choice) = choice else {
                        mapping.push(None);
                        continue;
                    };
                    let key = if let Some(item) = &choice.fallback {
                        persist_metadata(connection, item).await?
                    } else {
                        crate::catalog::remember_track(
                            connection,
                            &crate::TrackReference {
                                media_uri: choice.media_uri.clone(),
                                ..Default::default()
                            },
                        )
                        .await?
                    };
                    let origin = choice.origin.and_then(|(source, position)| {
                        members.get(source)?.get(position).copied().flatten()
                    });
                    let index = origin.unwrap_or_else(|| {
                        let index = entries.len();
                        entries.push(QueueEntry {
                            track_key: key,
                            provenance: choice.provenance.clone(),
                            received_from_connect: false,
                        });
                        occurrences.push(None);
                        index
                    });
                    mapping.push(Some(index));
                }
            }
            QueueInput::Source {
                reference,
                context_id,
            } => {
                let rows = source_members_on(connection, reference, None).await?;
                let start = rows.iter().position(|row| row.2).unwrap_or(0);
                let mut positions = HashMap::new();
                for (rank, (uri, identity, _)) in rows.into_iter().enumerate() {
                    let index = entries.len();
                    let key = crate::catalog::remember_track(
                        connection,
                        &crate::TrackReference {
                            media_uri: uri.clone(),
                            ..Default::default()
                        },
                    )
                    .await?;
                    entries.push(QueueEntry {
                        track_key: key,
                        provenance: QueueProvenance::Context {
                            context_id: context_id.clone(),
                            context_title: None,
                            source_rank: rank,
                        },
                        received_from_connect: false,
                    });
                    occurrences.push(None);
                    positions.insert((uri, identity), index);
                    mapping.push(Some(index));
                }
                if source.seed.is_some() {
                    mapping = source_members_on(connection, reference, source.seed)
                        .await?
                        .into_iter()
                        .map(|(uri, identity, _)| positions.get(&(uri, identity)).copied())
                        .collect();
                } else {
                    mapping.drain(..start);
                }
            }
        }
        members.push(mapping);
    }
    let mut order = Vec::new();
    let mut used = std::collections::HashSet::new();
    let mut selected = None;
    for (position, row) in rows.iter().enumerate() {
        let candidate = row
            .source_index
            .and_then(|source| {
                members
                    .get(source)?
                    .get(row.canonical_position)
                    .copied()
                    .flatten()
            })
            .filter(|index| entries[*index].track_key == row_keys[&row.item.media_uri]);
        let index = candidate.unwrap_or_else(|| {
            let index = entries.len();
            entries.push(row.entry(row_keys[&row.item.media_uri]));
            occurrences.push(None);
            index
        });
        occurrences[index] = Some(row.occurrence.clone());
        if value["current_index"].as_u64() == Some(position as u64) {
            selected = Some(index as u32);
        }
        if used.insert(index) {
            order.push(index as u32);
        }
    }
    for cursor in pending {
        let Some(mapping) = members.get(cursor.source) else {
            continue;
        };
        let mut positions = (0..mapping.len()).collect::<Vec<_>>();
        let source_program = matches!(sources[cursor.source].input, QueueInput::Source { .. });
        if !source_program && let Some(mut seed) = cursor.seed {
            seed = seed.wrapping_add(0x9e3779b97f4a7c15);
            for index in (1..positions.len()).rev() {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                positions.swap(index, seed as usize % (index + 1));
            }
            if let Some(index) = positions
                .iter()
                .position(|index| Some(*index) == cursor.anchor)
            {
                positions.swap(0, index);
            }
        }
        let start = cursor.offset
            + if !source_program && cursor.seed.is_none() {
                cursor.anchor.unwrap_or(0)
            } else {
                0
            };
        for position in positions.into_iter().skip(start) {
            if let Some(index) = mapping[position]
                && used.insert(index)
            {
                order.push(index as u32);
            }
        }
    }
    // Recovered members outside the retained window belong to earlier history.
    let history = (0..entries.len())
        .filter(|index| !used.contains(index))
        .map(|index| index as u32)
        .collect::<Vec<_>>();
    order.splice(..0, history);
    let current_index =
        selected.and_then(|selected| order.iter().position(|index| *index == selected));
    let mut state = QueueRestore {
        ids: (1..=entries.len() as u64).collect(),
        entries: entries.into(),
        order: order.into(),
        current_index,
        ..Default::default()
    };
    state.progress_millis = value["progress_millis"].as_i64().unwrap_or(0);
    state.repeat_mode = serde_json::from_value(value["repeat_mode"].clone())?;
    state.shuffled = value["shuffled"].as_bool().unwrap_or(false);
    Ok((state, occurrences))
}

impl OrderedQuery {
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
            "track.media_uri,{identity},COALESCE(({}),0)",
            anchor.unwrap_or_else(|| "0".into())
        );
        Ok(sqlx::query_as_with(
            sqlx::AssertSqlSafe(self.select(&columns)),
            self.arguments.clone(),
        )
        .fetch_all(connection)
        .await?)
    }
}

async fn source_members_on(
    connection: &mut SqliteConnection,
    source: &QueueSource,
    legacy_seed: Option<u64>,
) -> LibraryResult<Vec<(String, Option<String>, bool)>> {
    let sort = if legacy_seed.is_some() {
        crate::CatalogSort::Title
    } else {
        source.sort
    };
    let mut query = match &source.scope {
        QueueScope::Smart { reference, now } => {
            let mut uris = smart_members_ref(
                connection,
                reference,
                *now,
                &source.filter,
                None,
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
            crate::query::member_query_on(
                connection,
                &crate::MemberRead {
                    collection: crate::Collection::Source(key),
                    sort: Some(sort),
                    folder,
                    filter: source.filter.clone(),
                    descending: source.descending,
                    favorites_only: *favorites_only,
                    ..crate::MemberRead::new(crate::Collection::Source(key))
                },
            )
            .await?
        }
        QueueScope::Collection {
            reference,
            favorites_only,
        } => {
            let Some((collection, folder)) =
                resolve_collection_reference(connection, reference).await?
            else {
                return Ok(Vec::new());
            };
            crate::query::member_query_on(
                connection,
                &crate::MemberRead {
                    collection: collection,
                    sort: Some(sort),
                    folder,
                    filter: source.filter.clone(),
                    descending: source.descending,
                    favorites_only: *favorites_only,
                    downloaded_only: false,
                    activity: false,
                },
            )
            .await?
        }
        QueueScope::Playlist {
            reference, sort, ..
        } => {
            let Some((Collection::Playlist(key), folder)) =
                resolve_collection_reference(connection, reference).await?
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
            .map(|uri| format!("track.media_uri={}", quote(uri)))
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
            && let Some(value) = sqlx::query_scalar_with::<_, i64, _>(
                sqlx::AssertSqlSafe(format!(
                    "SELECT {hash} FROM {} WHERE {} AND ({anchor}) LIMIT 1",
                    query.from, query.predicate
                )),
                query.arguments.clone(),
            )
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

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
pub(crate) enum CollectionIdentity {
    Album(String),
    Artist {
        media_uri: String,
        album_artist: bool,
    },
    Genre {
        source_id: crate::SourceId,
        object_id: String,
    },
    Mood {
        source_id: crate::SourceId,
        object_id: String,
    },
    Playlist {
        source_id: Option<crate::SourceId>,
        object_id: String,
        native: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
pub(crate) struct CollectionSourceReference {
    pub collection: CollectionIdentity,
    pub folder: Option<(crate::SourceId, String)>,
}

async fn resolve_collection_reference(
    connection: &mut SqliteConnection,
    reference: &CollectionSourceReference,
) -> LibraryResult<Option<(crate::Collection, Option<FolderKey>)>> {
    use crate::Collection as C;
    let collection = match &reference.collection {
        CollectionIdentity::Album(uri) => {
            let Some(key) = sqlx::query_scalar::<_, AlbumKey>(
                "SELECT album_key FROM albums WHERE media_uri=?1",
            )
            .bind(uri)
            .fetch_optional(&mut *connection)
            .await?
            else {
                return Ok(None);
            };
            C::Album(key)
        }
        CollectionIdentity::Artist {
            media_uri,
            album_artist,
        } => {
            let Some(key) = sqlx::query_scalar::<_, ArtistKey>(
                "SELECT artist_key FROM artists WHERE media_uri=?1",
            )
            .bind(media_uri)
            .fetch_optional(&mut *connection)
            .await?
            else {
                return Ok(None);
            };
            C::Artist {
                key,
                album_artist: *album_artist,
            }
        }
        CollectionIdentity::Genre {
            source_id,
            object_id,
        } => {
            let Some(key)=sqlx::query_scalar::<_,GenreKey>("SELECT genre_key FROM genres JOIN sources USING(source_key) WHERE sources.object_id=?1 AND genres.object_id=?2").bind(source_id.as_str()).bind(object_id).fetch_optional(&mut *connection).await? else{return Ok(None);};
            C::Genre(key)
        }
        CollectionIdentity::Mood {
            source_id,
            object_id,
        } => {
            let Some(key)=sqlx::query_scalar::<_,MoodKey>("SELECT mood_key FROM moods JOIN sources USING(source_key) WHERE sources.object_id=?1 AND moods.object_id=?2").bind(source_id.as_str()).bind(object_id).fetch_optional(&mut *connection).await? else{return Ok(None);};
            C::Mood(key)
        }
        CollectionIdentity::Playlist {
            source_id,
            object_id,
            native,
        } => {
            let Some(key) = sqlx::query_scalar::<_, crate::PlaylistKey>(
                "SELECT playlist.playlist_key FROM playlists playlist LEFT JOIN sources source USING(source_key)
                 WHERE source.object_id IS ?1 AND playlist.object_id=?2 AND playlist.owner=?3"
            )
                .bind(source_id.as_ref().map(crate::SourceId::as_str))
                .bind(object_id)
                .bind(if *native { "source" } else { "rufin" })
                .fetch_optional(&mut *connection)
                .await?
            else {
                return Ok(None);
            };
            C::Playlist(key)
        }
    };
    let folder = if let Some((source, id)) = &reference.folder {
        let Some(key)=sqlx::query_scalar::<_,FolderKey>("SELECT folder_key FROM folders JOIN sources USING(source_key) WHERE sources.object_id=?1 AND folders.object_id=?2").bind(source.as_str()).bind(id).fetch_optional(connection).await? else{return Ok(None);};
        Some(key)
    } else {
        None
    };
    Ok(Some((collection, folder)))
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
pub(crate) struct SmartSourceReference {
    pub object_id: String,
}

async fn smart_members_ref(
    connection: &mut SqliteConnection,
    reference: &SmartSourceReference,
    now: i64,
    filter: &str,
    display_sort: Option<crate::CatalogSort>,
    descending: bool,
) -> LibraryResult<Vec<String>> {
    let Some(key) = resolve_smart_reference(connection, reference).await? else {
        return Ok(Vec::new());
    };
    let query =
        crate::smart_playlists::smart_query(connection, key, filter, display_sort, descending, now)
            .await?;
    Ok(sqlx::query_scalar_with(
        sqlx::AssertSqlSafe(query.select("track.media_uri")),
        query.arguments.clone(),
    )
    .persistent(false)
    .fetch_all(connection)
    .await?)
}

async fn resolve_smart_reference(
    connection: &mut SqliteConnection,
    reference: &SmartSourceReference,
) -> LibraryResult<Option<SmartPlaylistKey>> {
    Ok(
        sqlx::query_scalar("SELECT smart_playlist_key FROM smart_playlists WHERE object_id=?1")
            .bind(&reference.object_id)
            .fetch_optional(connection)
            .await?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stable_source_references_survive_catalog_and_definition_key_changes() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("store.sqlite"))
            .await
            .unwrap();
        let mut writer = database.writer().await.unwrap();
        let connection = writer.as_mut().unwrap();
        sqlx::raw_sql("DELETE FROM smart_playlists; INSERT INTO sources(source_key,object_id,display_name,normalized_name,artwork_digest) VALUES (1,'source','Source','source',zeroblob(32)); INSERT INTO genres(genre_key,source_key,object_id,media_uri,metadata) VALUES (1,1,'genre','genre:genre',jsonb(json_object('name','Genre','sort_text','genre'))); INSERT INTO folders(folder_key,source_key,object_id,media_uri,metadata) VALUES (1,1,'folder','folder:folder',jsonb(json_object('name','Folder','sort_text','folder')));").execute(&mut *connection).await.unwrap();
        let definition = crate::SmartPlaylistDefinition {
            source_id: Some(crate::SourceId::new("source")),
            ..crate::SmartPlaylistDefinition::default()
        };
        sqlx::query("INSERT INTO smart_playlists(object_id,name,normalized_name,definition_json,position) VALUES('smart','Smart','smart',?1,0)")
            .bind(serde_json::to_string(&definition).unwrap())
            .execute(&mut *connection)
            .await
            .unwrap();
        let smart = SmartSourceReference {
            object_id: "smart".into(),
        };
        let collection = CollectionSourceReference {
            collection: CollectionIdentity::Genre {
                source_id: crate::SourceId::new("source"),
                object_id: "genre".into(),
            },
            folder: Some((crate::SourceId::new("source"), "folder".into())),
        };
        sqlx::raw_sql("DELETE FROM sources; UPDATE smart_playlists SET smart_playlist_key=500; INSERT INTO sources(source_key,object_id,display_name,normalized_name,artwork_digest) VALUES (99,'source','Source','source',zeroblob(32)); INSERT INTO genres(genre_key,source_key,object_id,media_uri,metadata) VALUES (88,99,'genre','genre:genre',jsonb(json_object('name','Genre','sort_text','genre'))); INSERT INTO folders(folder_key,source_key,object_id,media_uri,metadata) VALUES (77,99,'folder','folder:folder',jsonb(json_object('name','Folder','sort_text','folder')));").execute(&mut *connection).await.unwrap();
        let key = resolve_smart_reference(connection, &smart)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(key.raw(), 500);
        let (collection, folder) = resolve_collection_reference(connection, &collection)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(collection,crate::Collection::Genre(key) if key.raw()==88));
        assert_eq!(folder.unwrap().raw(), 77);
        sqlx::query("DELETE FROM folders")
            .execute(&mut *connection)
            .await
            .unwrap();
        assert!(
            resolve_smart_reference(connection, &smart)
                .await
                .unwrap()
                .is_some(),
            "browsing folders do not own smart playlists"
        );
    }
}
