use super::*;

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
    let position = column("origin_position", "occurrence.position");
    let title = column("provenance_context_title", "NULL");
    let records = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT occurrence.object_id,occurrence.media_uri,coalesce({position},occurrence.position) canonical_position,
            {title} context_title,
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
