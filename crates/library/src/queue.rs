//! Queue source reads, metadata, and saved playback state. Playback owns edits.

pub(crate) mod legacy;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sqlx::{Connection, Row, SqliteConnection};

use crate::catalog::write::Scope;
use crate::{Catalog, LibraryError, LibraryResult, ReadCancellation, TrackReference};

pub const QUEUE_CONTEXT_LIMIT: usize = 100;

#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct QueueContextTitle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Arc<str>>,
    pub title: Arc<str>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum QueueProvenance {
    Context {
        context_id: Arc<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_title: Option<Arc<QueueContextTitle>>,
        source_rank: usize,
    },
    Manual,
    Random,
    Radio,
    AutoDj,
    Legacy,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum QueueRepeatMode {
    #[default]
    Off,
    One,
    All,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct QueueRow {
    #[serde(skip)]
    pub occurrence_id: Option<u64>,
    pub position: usize,
    pub track: crate::TrackView,
    pub provenance: QueueProvenance,
    #[serde(default)]
    pub received_from_connect: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueEntry {
    pub track_key: crate::TrackKey,
    pub provenance: QueueProvenance,
    #[serde(default)]
    pub received_from_connect: bool,
}

/// Where an entry sits: its rank in the queue's order and, while shuffle is on, its rank in the
/// play order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueueRank {
    pub rank: Vec<u8>,
    pub shuffle: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueRestore {
    #[serde(skip)]
    pub ids: Arc<[u64]>,
    pub entries: Arc<[QueueEntry]>,
    /// The saved ranks of the entries; empty for a queue the Store did not give.
    #[serde(skip)]
    pub ranks: Arc<[QueueRank]>,
    pub order: Arc<[u32]>,
    #[serde(skip)]
    pub rows: Vec<Arc<QueueRow>>,
    pub current_index: Option<usize>,
    pub progress_millis: i64,
    pub repeat_mode: QueueRepeatMode,
    pub shuffled: bool,
}

#[derive(Clone, Debug, Default)]
pub struct QueueUpdate {
    pub replace: bool,
    /// New entries and entries whose track or place changed, with their ranks.
    pub entries: HashMap<u64, (QueueEntry, QueueRank)>,
    pub removed: HashSet<u64>,
    /// Shuffle went off, so no entry keeps a shuffle rank.
    pub unshuffled: bool,
    /// The current index, position, repeat and shuffle below are saved too.
    pub state: bool,
    pub current_index: Option<usize>,
    pub progress_millis: i64,
    pub repeat_mode: QueueRepeatMode,
    pub shuffled: bool,
}

impl QueueUpdate {
    pub fn coalesce(&mut self, mut newer: Self) {
        if newer.replace {
            *self = newer;
            return;
        }
        if newer.unshuffled {
            self.unshuffled = true;
            for (_, rank) in self.entries.values_mut() {
                rank.shuffle = None;
            }
        }
        for id in newer.removed.drain() {
            self.entries.remove(&id);
            self.removed.insert(id);
        }
        for (id, entry) in newer.entries.drain() {
            self.removed.remove(&id);
            self.entries.insert(id, entry);
        }
        self.state |= newer.state;
        self.current_index = newer.current_index;
        self.progress_millis = newer.progress_millis;
        self.repeat_mode = newer.repeat_mode;
        self.shuffled = newer.shuffled;
    }
}

/// A page in play order, with each row's canonical member index.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct QueueTransferPage {
    pub offset: usize,
    pub rows: Vec<QueueTransferRow>,
    pub order: Vec<u32>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct QueueTransferRow {
    pub position: usize,
    pub item: TrackReference,
    pub provenance: QueueProvenance,
    #[serde(default)]
    pub received_from_connect: bool,
}

impl From<&QueueRow> for QueueTransferRow {
    fn from(row: &QueueRow) -> Self {
        Self {
            position: row.position,
            item: row.track.clone().into(),
            provenance: row.provenance.clone(),
            received_from_connect: row.received_from_connect,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum SavedContext<S> {
    Titled {
        id: S,
        title: Arc<QueueContextTitle>,
    },
    Legacy(S),
}

async fn decode_queue_export(
    write: &mut crate::catalog::write::Write<'_>,
    mut value: serde_json::Value,
) -> LibraryResult<QueueRestore> {
    if let Some(entries) = value
        .get_mut("entries")
        .and_then(serde_json::Value::as_array_mut)
    {
        for entry in entries {
            if let Some(uri) = entry.get("media_uri").and_then(serde_json::Value::as_str) {
                let key = write
                    .put_reference(&TrackReference {
                        media_uri: uri.to_owned(),
                        ..Default::default()
                    })
                    .await?;
                entry["track_key"] = serde_json::to_value(key)?;
            }
        }
    }
    Ok(serde_json::from_value(value)?)
}

#[derive(Clone, Debug, PartialEq)]
pub enum QueueLoad {
    Tracks(Vec<(crate::MemberSelection, QueueProvenance)>),
    Items(Vec<(TrackReference, QueueProvenance)>),
    Uris(Vec<String>, QueueProvenance),
    Captured(Arc<[QueueEntry]>),
}

impl QueueLoad {
    pub fn tracks(selection: crate::MemberSelection) -> Self {
        Self::Tracks(vec![(selection, QueueProvenance::Manual)])
    }
    pub fn track_keys(keys: Vec<crate::TrackKey>, provenance: QueueProvenance) -> Self {
        Self::Captured(
            keys.into_iter()
                .map(|track_key| QueueEntry {
                    track_key,
                    provenance: provenance.clone(),
                    received_from_connect: false,
                })
                .collect(),
        )
    }
    pub fn items(items: Vec<(TrackReference, QueueProvenance)>) -> Self {
        Self::Items(items)
    }
    pub fn media_uris(uris: impl IntoIterator<Item = String>, provenance: QueueProvenance) -> Self {
        Self::Uris(uris.into_iter().collect(), provenance)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueEntries {
    pub entries: Vec<QueueEntry>,
    pub anchor_index: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum QueuePlacement {
    Replace { anchor_index: usize },
    AfterCurrent,
    End,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum QueueReorderTarget {
    Before(usize),
    After(usize),
    BeforeOccurrence(u64),
    AfterOccurrence(u64),
    End,
}
impl QueueRepeatMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "none",
            Self::One => "one",
            Self::All => "all",
        }
    }

    fn parse(value: &str) -> LibraryResult<Self> {
        match value {
            "none" => Ok(Self::Off),
            "one" => Ok(Self::One),
            "all" => Ok(Self::All),
            _ => Err(LibraryError::InvalidStore(
                "invalid queue repeat mode".to_string(),
            )),
        }
    }
}

impl Catalog {
    pub async fn track_references_for_uris(
        &self,
        media_uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackReference>> {
        if media_uris.len() > QUEUE_CONTEXT_LIMIT {
            return Err(LibraryError::InvalidRequest(format!(
                "Queue materialization is limited to {QUEUE_CONTEXT_LIMIT} media URIs"
            )));
        }
        let mut connection = tokio::select! {result=self.acquire_reader()=>result?,()=cancellation.cancelled()=>return Err(LibraryError::ReadCancelled)};
        track_references_for_uris_on(self, &mut connection, media_uris).await
    }
}

impl Catalog {
    pub async fn queue_search_page(
        &self,
        queue: &QueueRestore,
        filter: &str,
        offset: u64,
        limit: u32,
    ) -> LibraryResult<(Vec<usize>, u64)> {
        let filter = filter.trim().to_lowercase();
        self.loaded().await;
        let snapshot = self.snapshot();
        let context = crate::catalog::views::Context::new(&snapshot);
        let mut positions = Vec::new();
        let mut total = 0;
        for (position, index) in queue.order.iter().enumerate() {
            let key = queue.entries[*index as usize].track_key;
            let matched = snapshot
                .tracks
                .get(key.raw())
                .is_some_and(|record| context.track_search(record).contains(&filter));
            if matched {
                if total >= offset && positions.len() < limit as usize {
                    positions.push(position);
                }
                total += 1;
            }
        }
        Ok((positions, total))
    }

    pub async fn queue_transfer_page(
        &self,
        queue: &QueueRestore,
        offset: usize,
    ) -> LibraryResult<QueueTransferPage> {
        let end = offset
            .saturating_add(QUEUE_CONTEXT_LIMIT)
            .min(queue.entries.len());
        let order = queue.order.get(offset..end).ok_or_else(|| {
            LibraryError::InvalidRequest("Queue transfer offset is out of range".into())
        })?;
        let requested = order
            .iter()
            .enumerate()
            .map(|(index, canonical)| (offset + index, queue.entries[*canonical as usize].clone()))
            .collect::<Vec<_>>();
        let rows = self
            .queue_rows_for_entries(&requested, &ReadCancellation::new())
            .await?
            .into_iter()
            .map(|row| QueueTransferRow::from(row.as_ref()))
            .collect();
        Ok(QueueTransferPage {
            offset,
            rows,
            order: queue.order[offset..end].to_vec(),
        })
    }

    /// Staging does not replace this device's queue or disturb its metadata.
    pub async fn stage_queue_transfer_page(
        &self,
        transfer: &str,
        page: &QueueTransferPage,
    ) -> LibraryResult<()> {
        if page.rows.len() > QUEUE_CONTEXT_LIMIT
            || page.rows.len() != page.order.len()
            || !page.offset.is_multiple_of(QUEUE_CONTEXT_LIMIT)
        {
            return Err(LibraryError::InvalidRequest(
                "Invalid queue transfer page".into(),
            ));
        }
        let payload = serde_json::to_string(page)?;
        let mut writer = self.writer().await?;
        sqlx::query("INSERT INTO queue_transfer_pages(transfer,page_offset,payload) VALUES(?1,?2,?3) ON CONFLICT(transfer,page_offset) DO UPDATE SET payload=excluded.payload")
            .bind(transfer).bind(page.offset as i64).bind(payload)
            .execute(&mut *writer).await?;
        Ok(())
    }

    pub async fn discard_queue_transfer(&self, transfer: &str) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        sqlx::query("DELETE FROM queue_transfer_pages WHERE transfer=?1")
            .bind(transfer)
            .execute(&mut *writer)
            .await?;
        Ok(())
    }

    /// Read one page at a time; only compact membership and order stay in memory.
    pub async fn prepare_queue_transfer(
        &self,
        transfer: &str,
        total: usize,
        current: usize,
    ) -> LibraryResult<QueueRestore> {
        let mut write = self.write().await?;
        let mut entries = vec![None; total];
        let mut order = Vec::new();
        let mut playing = None;
        for offset in (0..total).step_by(QUEUE_CONTEXT_LIMIT) {
            let payload: Option<String> = sqlx::query_scalar(
                "SELECT payload FROM queue_transfer_pages WHERE transfer=?1 AND page_offset=?2",
            )
            .bind(transfer)
            .bind(offset as i64)
            .fetch_optional(write.connection().await?)
            .await?;
            let mut page: QueueTransferPage = serde_json::from_str(&payload.ok_or_else(|| {
                LibraryError::InvalidRequest("Queue transfer is incomplete".into())
            })?)?;
            if page.rows.len() != QUEUE_CONTEXT_LIMIT.min(total - offset) {
                return Err(LibraryError::InvalidRequest(
                    "Queue transfer is incomplete".into(),
                ));
            }
            for (index, mut row) in page.rows.drain(..).enumerate() {
                let canonical = page.order[index] as usize;
                let slot = entries
                    .get_mut(canonical)
                    .filter(|slot| slot.is_none())
                    .ok_or_else(|| {
                        LibraryError::InvalidRequest("Invalid queue play order".into())
                    })?;
                row.position = offset + index;
                row.received_from_connect = true;
                *slot = Some(QueueEntry {
                    received_from_connect: true,
                    track_key: write.put_reference(&row.item).await?,
                    provenance: row.provenance.clone(),
                });
                if row.position == current {
                    playing = Some((slot.as_ref().unwrap().track_key, row.provenance));
                }
            }
            order.extend(page.order);
        }
        if current >= total {
            return Err(LibraryError::InvalidRequest(
                "Invalid queue current position".into(),
            ));
        }
        write.commit().await?;
        // The staged tracks are resident once the transfer commits.
        let mut rows = Vec::new();
        if let Some((key, provenance)) = playing {
            let song = self
                .tracks(&[key], &ReadCancellation::new())
                .await?
                .pop()
                .expect("requested canonical queue song");
            rows.push(Arc::new(QueueRow {
                position: current,
                track: song,
                provenance,
                received_from_connect: true,
                occurrence_id: None,
            }));
        }
        let entries = entries.into_iter().map(Option::unwrap).collect::<Vec<_>>();
        let current_index = Some(current);
        Ok(QueueRestore {
            entries: entries.into(),
            order: order.into(),
            rows,
            current_index,
            ..Default::default()
        })
    }

    /// Save the accepted queue and delete its staged pages in one Store transaction.
    pub async fn commit_queue_transfer(
        &self,
        transfer: &str,
        queue: &QueueRestore,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        save_queue_on(write.connection().await?, queue).await?;
        sqlx::query("DELETE FROM queue_transfer_pages WHERE transfer=?1")
            .bind(transfer)
            .execute(write.connection().await?)
            .await?;
        write.changed(Scope::Queue);
        write.commit().await?;
        Ok(())
    }

    /// Import a captured input without replacing the currently playing queue.
    pub async fn queue_from_transfer(
        &self,
        transfer: &str,
        total: usize,
        current: usize,
    ) -> LibraryResult<(QueueLoad, usize)> {
        let queue = self
            .prepare_queue_transfer(transfer, total, current)
            .await?;
        self.discard_queue_transfer(transfer).await?;
        Ok((
            QueueLoad::Captured(
                queue
                    .order
                    .iter()
                    .map(|index| queue.entries[*index as usize].clone())
                    .collect(),
            ),
            queue.current_index.unwrap_or_default(),
        ))
    }

    pub async fn remember_media_uris(
        &self,
        uris: &[String],
    ) -> LibraryResult<Vec<crate::TrackKey>> {
        let mut write = self.write().await?;
        let mut keys = Vec::with_capacity(uris.len());
        for uri in uris {
            keys.push(
                write
                    .put_reference(&TrackReference {
                        media_uri: uri.clone(),
                        ..Default::default()
                    })
                    .await?,
            );
        }
        write.commit().await?;
        Ok(keys)
    }

    pub async fn queue_entries(
        &self,
        input: QueueLoad,
        selected: Option<(crate::TrackKey, Option<crate::PlaylistEntryKey>)>,
    ) -> LibraryResult<QueueEntries> {
        let mut entries = Vec::new();
        let mut anchor = None;
        match input {
            QueueLoad::Tracks(groups) => {
                let mut reader = self.acquire_reader().await?;
                let mut transaction = reader.begin().await?;
                for (selection, provenance) in groups {
                    let resolve_anchor = !matches!(selection, crate::MemberSelection::Concat(_));
                    let songs =
                        crate::selection::selected_tracks_on(self, &mut transaction, &selection)
                            .await?;
                    for (rank, (track_key, entry_key)) in songs.into_iter().enumerate() {
                        if resolve_anchor
                            && selected.is_some_and(|(key, entry)| {
                                entry.map_or(key == track_key, |entry| Some(entry) == entry_key)
                            })
                        {
                            anchor.get_or_insert(entries.len());
                        }
                        let mut provenance = provenance.clone();
                        if let QueueProvenance::Context { source_rank, .. } = &mut provenance {
                            *source_rank += rank;
                        }
                        entries.push(QueueEntry {
                            track_key,
                            provenance,
                            received_from_connect: false,
                        });
                    }
                }
                transaction.commit().await?;
            }
            QueueLoad::Items(_) | QueueLoad::Uris(_, _) => {
                let mut write = self.write().await?;
                match input {
                    QueueLoad::Items(items) => {
                        let references = items
                            .iter()
                            .map(|(item, _)| item.clone())
                            .collect::<Vec<_>>();
                        let keys = write.put_references(&references).await?;
                        for ((_, provenance), track_key) in items.into_iter().zip(keys) {
                            entries.push(QueueEntry {
                                track_key,
                                provenance,
                                received_from_connect: false,
                            });
                        }
                    }
                    QueueLoad::Uris(uris, provenance) => {
                        let references = uris
                            .iter()
                            .map(|uri| TrackReference {
                                media_uri: uri.clone(),
                                ..Default::default()
                            })
                            .collect::<Vec<_>>();
                        let keys = write.put_references(&references).await?;
                        for (rank, track_key) in keys.into_iter().enumerate() {
                            let mut provenance = provenance.clone();
                            if let QueueProvenance::Context { source_rank, .. } = &mut provenance {
                                *source_rank += rank;
                            }
                            entries.push(QueueEntry {
                                track_key,
                                provenance,
                                received_from_connect: false,
                            });
                        }
                    }
                    _ => unreachable!(),
                }
                write.commit().await?;
            }
            QueueLoad::Captured(captured) => entries.extend(captured.iter().cloned()),
        }
        Ok(QueueEntries {
            entries,
            anchor_index: anchor,
        })
    }

    pub async fn queue_rows_for_entries(
        &self,
        entries: &[(usize, QueueEntry)],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Arc<QueueRow>>> {
        if entries.len() > QUEUE_CONTEXT_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "Queue window exceeds its context".into(),
            ));
        }
        let keys = entries
            .iter()
            .map(|(_, entry)| entry.track_key)
            .collect::<Vec<_>>();
        let songs = self
            .tracks(&keys, cancellation)
            .await?
            .into_iter()
            .map(|song| (song.track_key, song))
            .collect::<HashMap<_, _>>();
        // An entry whose track the Store no longer holds has no row; the others keep their own.
        Ok(entries
            .iter()
            .filter_map(|(position, entry)| {
                Some(Arc::new(QueueRow {
                    occurrence_id: None,
                    position: *position,
                    track: songs.get(&entry.track_key)?.clone(),
                    provenance: entry.provenance.clone(),
                    received_from_connect: entry.received_from_connect,
                }))
            })
            .collect())
    }

    pub async fn save_queue(&self, state: &QueueRestore) -> LibraryResult<()> {
        let mut write = self.write().await?;
        save_queue_on(write.connection().await?, state).await?;
        write.changed(Scope::Queue);
        write.commit().await?;
        Ok(())
    }

    /// Saves playback's queue edits. Clients hear of the queue only when rows changed.
    pub async fn apply_queue_update(&self, update: &QueueUpdate) -> LibraryResult<()> {
        let mut write = self.write().await?;
        if apply_queue_update_on(write.connection().await?, update).await? > 0 {
            write.changed(Scope::Queue);
        }
        write.commit().await?;
        Ok(())
    }

    pub async fn restore_queue(&self) -> LibraryResult<QueueRestore> {
        let mut reader = self.acquire_reader().await?;
        let mut transaction = reader.begin().await?;
        let mut state = restore_queue_on(&mut transaction).await?;
        transaction.commit().await?;
        drop(reader);
        // Now playing reads its one track; playback reads the rest of the shown window.
        let entries = state
            .current_index
            .and_then(|position| Some((position, *state.order.get(position)?)))
            .map(|(position, index)| (position, state.entries[index as usize].clone()))
            .into_iter()
            .collect::<Vec<_>>();
        state.rows = self
            .queue_rows_for_entries(&entries, &ReadCancellation::new())
            .await?;
        for row in &mut state.rows {
            Arc::make_mut(row).occurrence_id = Some(state.ids[state.order[row.position] as usize]);
        }
        Ok(state)
    }

    pub async fn export_queue_jsonl(&self, output: impl std::io::Write) -> LibraryResult<()> {
        let (_export, mut connection) = self.acquire_export().await?;
        export_queue_jsonl_on(self, &mut connection, output).await
    }

    pub async fn import_queue_jsonl(&self, input: impl std::io::BufRead) -> LibraryResult<()> {
        let mut write = self.write().await?;
        import_queue_jsonl_on(&mut write, input).await?;
        write.commit().await?;
        Ok(())
    }
}

async fn save_settings(
    connection: &mut sqlx::SqliteConnection,
    current: Option<usize>,
    progress: i64,
    repeat: QueueRepeatMode,
    shuffled: bool,
) -> LibraryResult<u64> {
    Ok(sqlx::query("INSERT INTO queue_state(singleton,current_index,progress_millis,repeat_mode,shuffled) VALUES(1,?1,?2,?3,?4) ON CONFLICT(singleton) DO UPDATE SET current_index=excluded.current_index,progress_millis=excluded.progress_millis,repeat_mode=excluded.repeat_mode,shuffled=excluded.shuffled WHERE (current_index,progress_millis,repeat_mode,shuffled) IS NOT (excluded.current_index,excluded.progress_millis,excluded.repeat_mode,excluded.shuffled)")
        .bind(current.map(|index| index as i64)).bind(progress.max(0)).bind(repeat.as_str()).bind(shuffled).execute(connection).await?.rows_affected())
}

/// Replaces the saved queue. A queue without saved ranks gets the ranks of a list built by
/// appends. Without shuffle the queue plays in its own order, so the current index moves to the
/// current entry's place in it.
pub(crate) async fn save_queue_on(
    connection: &mut sqlx::SqliteConnection,
    state: &QueueRestore,
) -> LibraryResult<()> {
    let ids = if state.ids.is_empty() {
        (1..=state.entries.len() as u64).collect::<Vec<_>>()
    } else if state.ids.len() == state.entries.len() {
        state.ids.to_vec()
    } else {
        return Err(LibraryError::InvalidRequest(
            "Queue IDs do not match its entries".into(),
        ));
    };
    if state.order.len() != state.entries.len()
        || state
            .order
            .iter()
            .any(|index| *index as usize >= state.entries.len())
    {
        return Err(LibraryError::InvalidRequest(
            "Invalid queue play order".into(),
        ));
    }
    let ranks = if state.ranks.len() == state.entries.len() {
        state.ranks.to_vec()
    } else {
        let mut ranks = (0..state.entries.len())
            .map(|index| QueueRank {
                rank: crate::rank::nth(index as u64),
                shuffle: None,
            })
            .collect::<Vec<_>>();
        if state.shuffled {
            for (position, index) in state.order.iter().enumerate() {
                ranks[*index as usize].shuffle = Some(crate::rank::nth(position as u64));
            }
        }
        ranks
    };
    let current_index = if state.shuffled {
        state.current_index
    } else {
        state
            .current_index
            .and_then(|index| state.order.get(index))
            .map(|index| *index as usize)
    };
    apply_queue_update_on(
        connection,
        &QueueUpdate {
            replace: true,
            entries: ids
                .iter()
                .copied()
                .zip(state.entries.iter().cloned().zip(ranks))
                .collect(),
            current_index,
            state: true,
            progress_millis: state.progress_millis,
            repeat_mode: state.repeat_mode,
            shuffled: state.shuffled,
            ..Default::default()
        },
    )
    .await
    .map(drop)
}

/// Writes a queue update. The rows it changed.
/// Writes a queue update. The rows it changed.
async fn apply_queue_update_on(
    connection: &mut sqlx::SqliteConnection,
    update: &QueueUpdate,
) -> LibraryResult<u64> {
    let mut changed = 0;
    if update.replace {
        changed += sqlx::raw_sql("DELETE FROM queue_entries; DELETE FROM queue_contexts;")
            .execute(&mut *connection)
            .await?
            .rows_affected();
    } else {
        for removed in update.removed.iter().collect::<Vec<_>>().chunks(1000) {
            let contexts = sqlx::query_scalar::<_, Option<i64>>(
                "DELETE FROM queue_entries WHERE entry_id IN (SELECT value FROM json_each(?1)) RETURNING context_key",
            ).bind(serde_json::to_string(removed)?).fetch_all(&mut *connection).await?;
            changed += contexts.len() as u64;
            sqlx::query("DELETE FROM queue_contexts WHERE context_key IN (SELECT value FROM json_each(?1)) AND NOT EXISTS(SELECT 1 FROM queue_entries WHERE context_key=queue_contexts.context_key)")
                .bind(serde_json::to_string(&contexts)?).execute(&mut *connection).await?;
        }
        if update.unshuffled {
            changed += sqlx::query(
                "UPDATE queue_entries SET shuffle_rank=NULL WHERE shuffle_rank IS NOT NULL",
            )
            .execute(&mut *connection)
            .await?
            .rows_affected();
        }
    }
    let contexts = stored_contexts(connection, update).await?;
    let mut entries = update.entries.iter().collect::<Vec<_>>();
    entries.sort_unstable_by_key(|(id, _)| *id);
    for entries in entries.chunks(1000) {
        let mut rows = Vec::with_capacity(entries.len());
        for (id, (entry, rank)) in entries {
            let (context_key, source_rank, provenance) = match &entry.provenance {
                QueueProvenance::Context {
                    context_id,
                    context_title,
                    source_rank,
                } => (
                    contexts
                        .get(&(context_id.as_ref(), context_title.as_deref()))
                        .copied(),
                    Some(*source_rank as i64),
                    None,
                ),
                provenance => (None, None, Some(serde_json::to_string(provenance)?)),
            };
            rows.push((
                **id as i64,
                entry.track_key.raw(),
                provenance,
                context_key,
                source_rank,
                entry.received_from_connect,
                crate::rank::hex(&rank.rank),
                rank.shuffle.as_deref().map(crate::rank::hex),
            ));
        }
        // A stored entry keeps its provenance, so only its track and places can change.
        changed += sqlx::query("INSERT INTO queue_entries(entry_id,track_key,provenance,context_key,source_rank,received_from_connect,rank,shuffle_rank)
            SELECT value->>0,value->>1,value->>2,value->>3,value->>4,value->>5,unhex(value->>6),unhex(value->>7) FROM json_each(?1) WHERE true
            ON CONFLICT(entry_id) DO UPDATE SET track_key=excluded.track_key,rank=excluded.rank,shuffle_rank=excluded.shuffle_rank
            WHERE (track_key,rank,shuffle_rank) IS NOT (excluded.track_key,excluded.rank,excluded.shuffle_rank)")
            .bind(serde_json::to_string(&rows)?).execute(&mut *connection).await?.rows_affected();
    }
    if update.state {
        changed += save_settings(
            connection,
            update.current_index,
            update.progress_millis,
            update.repeat_mode,
            update.shuffled,
        )
        .await?;
    }
    Ok(changed)
}

/// The stored keys of the contexts an update's entries name; contexts not stored yet are
/// stored now.
async fn stored_contexts<'a>(
    connection: &mut sqlx::SqliteConnection,
    update: &'a QueueUpdate,
) -> LibraryResult<HashMap<(&'a str, Option<&'a QueueContextTitle>), i64>> {
    let mut named = HashMap::new();
    for (entry, _) in update.entries.values() {
        if let QueueProvenance::Context {
            context_id,
            context_title,
            ..
        } = &entry.provenance
        {
            if let std::collections::hash_map::Entry::Vacant(vacant) =
                named.entry((context_id.as_ref(), context_title.as_deref()))
            {
                vacant.insert(serde_json::to_string(&match context_title {
                    Some(title) => SavedContext::Titled {
                        id: context_id.as_ref(),
                        title: title.clone(),
                    },
                    None => SavedContext::Legacy(context_id.as_ref()),
                })?);
            }
        }
    }
    if named.is_empty() {
        return Ok(HashMap::new());
    }
    let values = named.values().collect::<Vec<_>>();
    let mut stored: HashMap<String, i64> = sqlx::query_as::<_, (i64, String)>(
        "SELECT context_key,context FROM queue_contexts WHERE context IN (SELECT value FROM json_each(?1))",
    )
    .bind(serde_json::to_string(&values)?)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|(key, context)| (context, key))
    .collect();
    let mut keys = HashMap::with_capacity(named.len());
    for (name, value) in named {
        let key = match stored.get(&value) {
            Some(key) => *key,
            None => {
                let key: i64 = sqlx::query_scalar(
                    "INSERT INTO queue_contexts(context) VALUES(?1) RETURNING context_key",
                )
                .bind(&value)
                .fetch_one(&mut *connection)
                .await?;
                stored.insert(value, key);
                key
            }
        };
        keys.insert(name, key);
    }
    Ok(keys)
}

async fn restore_queue_on(connection: &mut sqlx::SqliteConnection) -> LibraryResult<QueueRestore> {
    let mut state = QueueRestore::default();
    read_settings(connection, &mut state).await?;
    let contexts =
        sqlx::query_as::<_, (i64, String)>("SELECT context_key,context FROM queue_contexts")
            .fetch_all(&mut *connection)
            .await?
            .into_iter()
            .map(|(key, json)| Ok((key, serde_json::from_str::<SavedContext<Arc<str>>>(&json)?)))
            .collect::<LibraryResult<HashMap<_, _>>>()?;
    let rows = sqlx::query("SELECT entry_id,track_key,rank,shuffle_rank,context_key,source_rank,provenance,received_from_connect FROM queue_entries ORDER BY rank,entry_id")
        .fetch_all(&mut *connection)
        .await?;
    let mut entries = Vec::with_capacity(rows.len());
    let mut ids = Vec::with_capacity(rows.len());
    let mut ranks = Vec::with_capacity(rows.len());
    for row in rows {
        ids.push(row.try_get::<i64, _>("entry_id")? as u64);
        ranks.push(QueueRank {
            rank: row.try_get("rank")?,
            shuffle: row
                .try_get::<Option<Vec<u8>>, _>("shuffle_rank")?
                .filter(|_| state.shuffled),
        });
        let provenance = match row.try_get::<Option<i64>, _>("context_key")? {
            Some(key) => {
                let (context_id, context_title) = match &contexts[&key] {
                    SavedContext::Titled { id, title } => (id.clone(), Some(title.clone())),
                    SavedContext::Legacy(id) => (id.clone(), None),
                };
                QueueProvenance::Context {
                    context_id,
                    context_title,
                    source_rank: row.try_get::<i64, _>("source_rank")? as usize,
                }
            }
            None => serde_json::from_str(&row.try_get::<String, _>("provenance")?)?,
        };
        entries.push(QueueEntry {
            track_key: row.try_get("track_key")?,
            provenance,
            received_from_connect: row.try_get("received_from_connect")?,
        });
    }
    let mut order = (0..entries.len() as u32).collect::<Vec<_>>();
    if state.shuffled {
        order.sort_by(|a, b| {
            let key = |index: &u32| {
                let rank = &ranks[*index as usize];
                (
                    rank.shuffle.as_ref().unwrap_or(&rank.rank),
                    ids[*index as usize],
                )
            };
            key(a).cmp(&key(b))
        });
    }
    state.ids = ids.into();
    state.entries = entries.into();
    state.ranks = ranks.into();
    state.order = order.into();
    Ok(state)
}

async fn read_settings(
    connection: &mut sqlx::SqliteConnection,
    state: &mut QueueRestore,
) -> LibraryResult<()> {
    if let Some((current, progress, repeat, shuffled)) = sqlx::query_as::<_, (Option<i64>, i64, String, bool)>("SELECT current_index,progress_millis,repeat_mode,shuffled FROM queue_state WHERE singleton=1").fetch_optional(connection).await? {
        state.current_index = current.map(|index| index as usize);
        state.progress_millis = progress;
        state.repeat_mode = QueueRepeatMode::parse(&repeat)?;
        state.shuffled = shuffled;
    }
    Ok(())
}

#[derive(serde::Deserialize)]
pub(crate) struct LegacyQueueRow {
    pub occurrence: String,
    pub item: TrackReference,
    pub canonical_position: usize,
    pub provenance: QueueProvenance,
}

impl LegacyQueueRow {
    pub(crate) fn entry(&self, track_key: crate::TrackKey) -> QueueEntry {
        QueueEntry {
            track_key,
            provenance: self.provenance.clone(),
            received_from_connect: false,
        }
    }
}

pub(crate) async fn export_queue_jsonl_on(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<()> {
    let state = restore_queue_on(connection).await?;
    let keys = state
        .entries
        .iter()
        .map(|entry| entry.track_key)
        .collect::<Vec<_>>();
    let uris = track_uris_on(connection, &keys).await?;
    let mut saved = serde_json::to_value(&state)?;
    for (entry, uri) in saved["entries"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .zip(&uris)
    {
        entry.as_object_mut().unwrap().remove("track_key");
        entry["media_uri"] = uri.clone().into();
    }
    serde_json::to_writer(&mut output, &serde_json::json!({"version":4,"queue":saved}))?;
    output.write_all(b"\n")?;
    let mut seen = HashSet::new();
    for page in uris.chunks(QUEUE_CONTEXT_LIMIT) {
        for item in track_references_for_uris_on(catalog, connection, page).await? {
            if seen.insert(item.media_uri.clone()) {
                serde_json::to_writer(&mut output, &item)?;
                output.write_all(b"\n")?;
            }
        }
    }
    Ok(())
}
pub(crate) async fn import_queue_jsonl_on(
    write: &mut crate::catalog::write::Write<'_>,
    mut input: impl std::io::BufRead,
) -> LibraryResult<()> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    let header: serde_json::Value = serde_json::from_str(&line)?;
    let state = match header["version"].as_u64() {
        Some(4) => {
            while {
                line.clear();
                input.read_line(&mut line)? != 0
            } {
                if !line.trim().is_empty() {
                    write
                        .put_reference(&serde_json::from_str::<TrackReference>(&line)?)
                        .await?;
                }
            }
            decode_queue_export(write, header["queue"].clone()).await?
        }
        Some(3) => {
            let rows: Vec<LegacyQueueRow> = serde_json::from_value(header["snapshots"].clone())?;
            for row in &rows {
                write.put_reference(&row.item).await?;
            }
            let saved = legacy::decode_saved(header["queue"].clone())?;
            let mut keys = Vec::with_capacity(saved.uris.len());
            for uri in &saved.uris {
                keys.push(
                    write
                        .put_reference(&TrackReference {
                            media_uri: uri.clone(),
                            ..Default::default()
                        })
                        .await?,
                );
            }
            saved.restore(keys).0
        }
        _ => {
            return Err(LibraryError::InvalidRequest(
                "unsupported Queue export version".into(),
            ));
        }
    };
    save_queue_on(write.connection().await?, &state).await
}
#[allow(non_upper_case_globals)]
impl QueuePlacement {
    pub const Now: Self = Self::Replace { anchor_index: 0 };
    pub const Next: Self = Self::AfterCurrent;
    pub const Last: Self = Self::End;
    pub const fn with_anchor(self, anchor_index: usize) -> Self {
        match self {
            Self::Replace { .. } => Self::Replace { anchor_index },
            other => other,
        }
    }
}

/// Fisher-Yates shuffle with SplitMix64, shared by queue capture and playback order.
pub fn shuffle_order<T>(values: &mut [T], mut seed: u64) {
    for i in (1..values.len()).rev() {
        seed = seed.wrapping_add(0x9e3779b97f4a7c15);
        let mut n = seed;
        n = (n ^ (n >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        n = (n ^ (n >> 27)).wrapping_mul(0x94d049bb133111eb);
        n ^= n >> 31;
        values.swap(i, ((u128::from(n) * (i as u128 + 1)) >> 64) as usize);
    }
}

async fn track_uris_on(
    connection: &mut SqliteConnection,
    keys: &[crate::TrackKey],
) -> LibraryResult<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT media_uri FROM json_each(?1) requested JOIN tracks ON track_key=requested.value ORDER BY CAST(requested.key AS INTEGER)")
        .bind(serde_json::to_string(keys)?).fetch_all(connection).await?)
}

async fn track_references_for_uris_on(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    uris: &[String],
) -> LibraryResult<Vec<TrackReference>> {
    let keys = sqlx::query_scalar::<_, crate::TrackKey>("SELECT track_key FROM json_each(?1) requested JOIN tracks ON media_uri=requested.value ORDER BY CAST(requested.key AS INTEGER)")
        .bind(serde_json::to_string(uris)?).fetch_all(&mut *connection).await?;
    let songs = catalog.track_views_on(connection, &keys).await?;
    let songs = songs
        .into_iter()
        .map(|song| (song.media_uri.clone(), TrackReference::from(song)))
        .collect::<HashMap<_, _>>();
    Ok(uris
        .iter()
        .map(|uri| {
            songs
                .get(uri)
                .cloned()
                .unwrap_or_else(|| TrackReference::direct(uri.clone(), uri.clone(), "", "", 0))
        })
        .collect())
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl QueueRow {
    pub fn same_row(&self, other: Arc<QueueRow>) -> bool {
        std::ptr::eq(self, other.as_ref()) || self == other.as_ref()
    }

    pub fn track(&self) -> crate::TrackView {
        self.track.clone()
    }
    pub fn position(&self) -> usize {
        self.position
    }
    pub fn context_id(&self) -> Option<String> {
        match &self.provenance {
            QueueProvenance::Context { context_id, .. } => Some(context_id.to_string()),
            _ => None,
        }
    }
    pub fn source_rank(&self) -> Option<usize> {
        match &self.provenance {
            QueueProvenance::Context { source_rank, .. } => Some(*source_rank),
            _ => None,
        }
    }
    pub fn context_title(&self) -> Option<String> {
        match &self.provenance {
            QueueProvenance::Context { context_title, .. } => context_title
                .as_ref()
                .map(|caption| caption.title.to_string()),
            _ => None,
        }
    }
    pub fn context_category(&self) -> Option<String> {
        match &self.provenance {
            QueueProvenance::Context { context_title, .. } => context_title
                .as_ref()
                .and_then(|caption| caption.kind.as_deref().map(str::to_owned)),
            _ => None,
        }
    }
    pub fn context_kind(&self) -> String {
        match self.provenance {
            QueueProvenance::Context { .. } => "context",
            QueueProvenance::Manual => "manual",
            QueueProvenance::Random => "random",
            QueueProvenance::Radio => "radio",
            QueueProvenance::AutoDj => "auto_dj",
            QueueProvenance::Legacy => "legacy",
        }
        .into()
    }
}
