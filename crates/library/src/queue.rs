//! Queue source reads, metadata, and saved playback state. Playback owns edits.

pub(crate) mod legacy;

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::sync::Arc;

use sqlx::{Connection, FromRow, Row, Sqlite, SqliteConnection};

use crate::{Database, LibraryError, LibraryResult, ReadCancellation, SourceId, SourceKey};

pub const QUEUE_CONTEXT_LIMIT: usize = 100;
const QUEUE_PRIMARY_ARTIST_SQL: &str = "COALESCE(
    (SELECT artist.media_uri FROM track_artists credit
     JOIN artists artist USING(artist_key)
     WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1),
    (SELECT artist.media_uri FROM album_artists credit
     JOIN artists artist USING(artist_key)
     WHERE credit.album_key=track.album_key ORDER BY credit.position LIMIT 1))";

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

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QueueItem {
    pub media_uri: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_display_artist: Option<String>,
    /// Effective catalog artwork carried by a runtime projection, never persisted by Queue.
    #[serde(skip)]
    pub artwork_binding: Option<Vec<u8>>,
    pub duration_millis: i64,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub source_format: Option<String>,
    #[serde(default)]
    pub audio_properties: crate::AudioProperties,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub musicbrainz_album_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub primary_artist_musicbrainz_id: Option<String>,
}

impl QueueItem {
    pub fn direct(
        media_uri: impl Into<String>,
        title: impl Into<String>,
        artist: impl Into<String>,
        album: impl Into<String>,
        duration_millis: i64,
    ) -> Self {
        Self {
            media_uri: media_uri.into(),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            album_display_artist: None,
            artwork_binding: None,
            duration_millis: duration_millis.max(0),
            disc_number: None,
            track_number: None,
            year: None,
            release_date: None,
            source_format: None,
            audio_properties: Default::default(),
            musicbrainz_recording_id: None,
            musicbrainz_release_track_id: None,
            musicbrainz_album_id: None,
            musicbrainz_release_group_id: None,
            primary_artist_musicbrainz_id: None,
        }
    }
}

impl From<crate::PlaylistEntryRow> for QueueItem {
    fn from(entry: crate::PlaylistEntryRow) -> Self {
        Self {
            album_display_artist: entry.album_display_artist,
            artwork_binding: entry.artwork_binding,
            disc_number: entry.disc_number,
            track_number: entry.track_number,
            year: entry.year,
            release_date: entry.release_date,
            source_format: entry.source_format,
            audio_properties: entry.audio_properties,
            musicbrainz_recording_id: entry.musicbrainz_recording_id,
            musicbrainz_release_track_id: entry.musicbrainz_release_track_id,
            ..Self::direct(
                entry.media_uri,
                entry.title,
                entry.artist,
                entry.album,
                entry.duration_millis,
            )
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct QueueRow {
    #[serde(skip)]
    pub track_key: Option<crate::TrackKey>,
    #[serde(skip)]
    pub occurrence_id: Option<u64>,
    pub position: usize,
    pub item: QueueItem,
    pub provenance: QueueProvenance,
    #[serde(default)]
    pub received_from_connect: bool,
}

impl Deref for QueueRow {
    type Target = QueueItem;

    fn deref(&self) -> &Self::Target {
        &self.item
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueEntry {
    pub track_key: crate::TrackKey,
    pub provenance: QueueProvenance,
    #[serde(default)]
    pub received_from_connect: bool,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueRestore {
    #[serde(skip)]
    pub ids: Arc<[u64]>,
    pub entries: Arc<[QueueEntry]>,
    pub order: Arc<[u32]>,
    #[serde(skip)]
    pub rows: Vec<Arc<QueueRow>>,
    pub current_index: Option<usize>,
    pub progress_millis: i64,
    pub repeat_mode: QueueRepeatMode,
    pub shuffled: bool,
}

#[derive(Clone, Debug, Default)]
pub struct QueueOrderUpdate {
    pub rows: Vec<(usize, u64)>,
    pub total: usize,
}

impl QueueOrderUpdate {
    pub fn coalesce(&mut self, mut newer: Self) {
        if newer.total < self.total {
            self.rows.truncate(
                self.rows
                    .partition_point(|(position, _)| *position < newer.total),
            );
        }
        newer.rows.truncate(
            newer
                .rows
                .partition_point(|(position, _)| *position < newer.total),
        );
        if self.rows.is_empty() {
            *self = newer;
            return;
        }
        self.total = newer.total;
        if newer.rows.is_empty() || self.rows.last().unwrap().0 < newer.rows[0].0 {
            self.rows.extend(newer.rows);
            return;
        }
        let mut rows = Vec::with_capacity(self.rows.len() + newer.rows.len());
        let mut older = std::mem::take(&mut self.rows).into_iter().peekable();
        for row in newer.rows {
            while older.peek().is_some_and(|old| old.0 < row.0) {
                rows.push(older.next().unwrap());
            }
            if older.peek().is_some_and(|old| old.0 == row.0) {
                older.next();
            }
            rows.push(row);
        }
        rows.extend(older);
        self.rows = rows;
    }
}

#[derive(Clone, Debug, Default)]
pub struct QueueUpdate {
    pub replace: bool,
    pub entries: HashMap<u64, QueueEntry>,
    pub removed: HashSet<u64>,
    pub ordinary: Option<QueueOrderUpdate>,
    pub order: Option<QueueOrderUpdate>,
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
        for id in newer.removed.drain() {
            self.entries.remove(&id);
            self.removed.insert(id);
        }
        for (id, entry) in newer.entries.drain() {
            self.removed.remove(&id);
            self.entries.insert(id, entry);
        }
        for (old, next) in [
            (&mut self.ordinary, newer.ordinary),
            (&mut self.order, newer.order),
        ] {
            if let Some(next) = next {
                if let Some(old) = old {
                    old.coalesce(next);
                } else {
                    *old = Some(next);
                }
            }
        }
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
    pub rows: Vec<QueueRow>,
    pub order: Vec<u32>,
}

// The old JSON membership shares context titles through a separate array.
#[derive(serde::Serialize, serde::Deserialize)]
struct SavedQueue<E, C> {
    entries: E,
    contexts: C,
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

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum SavedProvenance<P> {
    Context(usize, usize),
    Other(P),
}

pub(crate) async fn decode_saved_queue(
    connection: &mut SqliteConnection,
    mut value: serde_json::Value,
) -> LibraryResult<QueueRestore> {
    if value.get("contexts").is_some() {
        type SavedEntry = (
            String,
            SavedProvenance<QueueProvenance>,
            Option<String>,
            bool,
        );
        let saved: SavedQueue<Vec<SavedEntry>, Vec<SavedContext<Arc<str>>>> =
            serde_json::from_value(value)?;
        let mut entries = Vec::with_capacity(saved.entries.len());
        for (uri, provenance, _, received_from_connect) in saved.entries {
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
            let track_key =
                crate::tracks::remember_song(connection, &uri, Default::default()).await?;
            entries.push(QueueEntry {
                track_key,
                provenance,
                received_from_connect,
            });
        }
        return Ok(QueueRestore {
            entries: entries.into(),
            ..Default::default()
        });
    }
    if let Some(entries) = value
        .get_mut("entries")
        .and_then(serde_json::Value::as_array_mut)
    {
        for entry in entries {
            if let Some(uri) = entry.get("media_uri").and_then(serde_json::Value::as_str) {
                let key = crate::tracks::remember_song(connection, uri, Default::default()).await?;
                entry["track_key"] = serde_json::to_value(key)?;
            }
        }
    }
    Ok(serde_json::from_value(value)?)
}

#[derive(Clone, Debug, PartialEq)]
pub enum QueueLoad {
    Songs(Vec<(crate::SongSelection, QueueProvenance)>),
    Items(Vec<(QueueItem, QueueProvenance)>),
    Uris(Vec<String>, QueueProvenance),
    Captured(Arc<[QueueEntry]>),
}

impl QueueLoad {
    pub fn songs(selection: crate::SongSelection) -> Self {
        Self::Songs(vec![(selection, QueueProvenance::Manual)])
    }
    pub fn items(items: Vec<(QueueItem, QueueProvenance)>) -> Self {
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
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QueuePageRow {
    pub entry: Arc<QueueRow>,
    pub source_path: Option<String>,
    pub source_name: Option<String>,
    pub favorite: bool,
    pub primary_artist_media_uri: Option<String>,
}

impl Deref for QueuePageRow {
    type Target = QueueRow;

    fn deref(&self) -> &Self::Target {
        &self.entry
    }
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

impl Database {
    pub async fn queue_artwork_for_uris(
        &self,
        media_uris: &[String],
    ) -> LibraryResult<Vec<(String, Option<Vec<u8>>)>> {
        if media_uris.len() > QUEUE_CONTEXT_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "Queue artwork window exceeds 100".into(),
            ));
        }
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT requested.value,track.artwork_binding FROM json_each(?1) requested LEFT JOIN tracks track ON track.media_uri=requested.value")
            .bind(serde_json::to_string(media_uris)?).fetch_all(&mut *connection).await?)
    }

    pub async fn queue_items_for_uris(
        &self,
        media_uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<QueueItem>> {
        if media_uris.len() > QUEUE_CONTEXT_LIMIT {
            return Err(LibraryError::InvalidRequest(format!(
                "Queue materialization is limited to {QUEUE_CONTEXT_LIMIT} media URIs"
            )));
        }
        let mut connection = tokio::select! {result=self.acquire_reader()=>result?,()=cancellation.cancelled()=>return Err(LibraryError::ReadCancelled)};
        queue_items_for_uris_on(&mut connection, media_uris).await
    }

    pub async fn prepared_queue_page(
        &self,
        window: &[Arc<QueueRow>],
    ) -> LibraryResult<Vec<QueuePageRow>> {
        if window.len() > QUEUE_CONTEXT_LIMIT {
            return Err(LibraryError::InvalidStore(
                "Queue window exceeds its context".into(),
            ));
        }
        if window.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = sqlx::QueryBuilder::<Sqlite>::new("WITH requested(ordinal,media_uri) AS (");
        query.push_values(window.iter().enumerate(), |mut row, (ordinal, entry)| {
            row.push_bind(ordinal as i64).push_bind(&entry.media_uri);
        });
        query.push(
            ") SELECT ordinal,COALESCE(
                (SELECT state.favorite FROM user_media_state state WHERE state.media_uri=requested.media_uri),
                track.source_favorite,0),",
        );
        query
            .push(QUEUE_PRIMARY_ARTIST_SQL)
            .push(",track.source_path,(SELECT display_name FROM sources WHERE source_key=track.source_key)")
            .push(" FROM requested LEFT JOIN tracks track ON track.media_uri=requested.media_uri ORDER BY ordinal");
        let mut connection = self.acquire_reader().await?;
        let facts = query
            .build_query_as::<(i64, bool, Option<String>, Option<String>, Option<String>)>()
            .fetch_all(&mut *connection)
            .await?;
        Ok(facts
            .into_iter()
            .map(
                |(ordinal, favorite, primary_artist_media_uri, source_path, source_name)| {
                    QueuePageRow {
                        entry: Arc::clone(&window[ordinal as usize]),
                        source_path,
                        source_name,
                        favorite,
                        primary_artist_media_uri,
                    }
                },
            )
            .collect())
    }
}

impl Database {
    pub async fn queue_search_page(
        &self,
        queue: &QueueRestore,
        filter: &str,
        offset: u64,
        limit: u32,
    ) -> LibraryResult<(Vec<usize>, u64)> {
        let mut reader = self.acquire_reader().await?;
        let mut transaction = reader.begin().await?;
        let filter = filter.trim().to_lowercase();
        let mut positions = Vec::new();
        let mut total = 0;
        for (page, order) in queue.order.chunks(4096).enumerate() {
            let keys = order
                .iter()
                .map(|i| queue.entries[*i as usize].track_key)
                .collect::<Vec<_>>();
            let matches=sqlx::query_scalar::<_,i64>("SELECT CAST(requested.key AS INTEGER) FROM json_each(?1) requested JOIN tracks ON track_key=requested.value WHERE instr(normalized_search,?2)>0 ORDER BY CAST(requested.key AS INTEGER)")
                .bind(serde_json::to_string(&keys)?).bind(&filter).fetch_all(&mut *transaction).await?;
            for position in matches {
                if total >= offset && positions.len() < limit as usize {
                    positions.push(page * 4096 + position as usize);
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
            .map(|row| (*row).clone())
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
            .execute(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?).await?;
        Ok(())
    }

    pub async fn discard_queue_transfer(&self, transfer: &str) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        sqlx::query("DELETE FROM queue_transfer_pages WHERE transfer=?1")
            .bind(transfer)
            .execute(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
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
        let mut writer = self.writer().await?;
        let reader = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut entries = vec![None; total];
        let mut order = Vec::new();
        let mut rows = Vec::new();
        for offset in (0..total).step_by(QUEUE_CONTEXT_LIMIT) {
            let payload: Option<String> = sqlx::query_scalar(
                "SELECT payload FROM queue_transfer_pages WHERE transfer=?1 AND page_offset=?2",
            )
            .bind(transfer)
            .bind(offset as i64)
            .fetch_optional(&mut *reader)
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
                    track_key: persist_metadata(&mut *reader, &row.item).await?,
                    provenance: row.provenance.clone(),
                });
                row.track_key = slot.as_ref().map(|entry| entry.track_key);
                if row.position == current {
                    let uris = track_uris_on(reader, &[slot.as_ref().unwrap().track_key]).await?;
                    row.item = queue_items_for_uris_on(reader, &uris)
                        .await?
                        .pop()
                        .expect("requested canonical queue song");
                    rows.push(Arc::new(row));
                }
            }
            order.extend(page.order);
        }
        if current >= total {
            return Err(LibraryError::InvalidRequest(
                "Invalid queue current position".into(),
            ));
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

    /// Commit staged metadata with the accepted queue in the same Store transaction.
    pub async fn commit_queue_transfer(
        &self,
        transfer: &str,
        queue: &QueueRestore,
    ) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        persist_transfer_on(&mut transaction, transfer, queue.entries.len()).await?;
        save_queue_on(&mut transaction, queue).await?;
        sqlx::query("DELETE FROM queue_transfer_pages WHERE transfer=?1")
            .bind(transfer)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
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
        self.import_queue_transfer_metadata(transfer, total).await?;
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

    pub async fn import_queue_transfer_metadata(
        &self,
        transfer: &str,
        total: usize,
    ) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        persist_transfer_on(&mut transaction, transfer, total).await?;
        sqlx::query("DELETE FROM queue_transfer_pages WHERE transfer=?1")
            .bind(transfer)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn remember_media_uris(
        &self,
        uris: &[String],
    ) -> LibraryResult<Vec<crate::TrackKey>> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let mut keys = Vec::with_capacity(uris.len());
        for uri in uris {
            keys.push(
                crate::tracks::remember_song(&mut transaction, uri, Default::default()).await?,
            );
        }
        transaction.commit().await?;
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
            QueueLoad::Songs(groups) => {
                let mut reader = self.acquire_reader().await?;
                let mut transaction = reader.begin().await?;
                for (selection, provenance) in groups {
                    let resolve_anchor = matches!(selection, crate::SongSelection::Query { .. });
                    let songs =
                        crate::selection::selected_songs_on(&mut transaction, &selection).await?;
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
                let mut writer = self.writer().await?;
                let mut transaction = writer
                    .as_mut()
                    .ok_or(LibraryError::WriterUnavailable)?
                    .begin()
                    .await?;
                match input {
                    QueueLoad::Items(items) => {
                        for (item, provenance) in items {
                            let track_key = persist_metadata(&mut transaction, &item).await?;
                            entries.push(QueueEntry {
                                track_key,
                                provenance,
                                received_from_connect: false,
                            });
                        }
                    }
                    QueueLoad::Uris(uris, provenance) => {
                        for (rank, uri) in uris.iter().enumerate() {
                            let track_key = crate::tracks::remember_song(
                                &mut transaction,
                                uri,
                                Default::default(),
                            )
                            .await?;
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
                transaction.commit().await?;
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
        let mut connection = tokio::select! {result=self.acquire_reader()=>result?,()=cancellation.cancelled()=>return Err(LibraryError::ReadCancelled)};
        let keys = entries
            .iter()
            .map(|(_, entry)| entry.track_key)
            .collect::<Vec<_>>();
        let uris = track_uris_on(&mut connection, &keys).await?;
        let items = queue_items_for_uris_on(&mut connection, &uris).await?;
        Ok(entries
            .iter()
            .zip(items)
            .map(|((position, entry), item)| {
                Arc::new(QueueRow {
                    occurrence_id: None,
                    track_key: Some(entry.track_key),
                    position: *position,
                    item,
                    provenance: entry.provenance.clone(),
                    received_from_connect: entry.received_from_connect,
                })
            })
            .collect())
    }

    pub async fn save_queue(&self, state: &QueueRestore) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        save_queue_on(&mut transaction, state).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn apply_queue_update(&self, update: &QueueUpdate) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        apply_queue_update_on(&mut transaction, update).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn restore_queue(&self) -> LibraryResult<QueueRestore> {
        let mut reader = self.acquire_reader().await?;
        let mut transaction = reader.begin().await?;
        let mut state = restore_queue_on(&mut transaction).await?;
        transaction.commit().await?;
        drop(reader);
        let start = state.current_index.unwrap_or(0).saturating_sub(10);
        let entries = state
            .order
            .iter()
            .enumerate()
            .skip(start)
            .take(QUEUE_CONTEXT_LIMIT)
            .map(|(position, index)| (position, state.entries[*index as usize].clone()))
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
        export_queue_jsonl_on(&mut connection, output).await
    }

    pub async fn import_queue_jsonl(&self, input: impl std::io::BufRead) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        import_queue_jsonl_on(&mut transaction, input).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn queue_positions_for_source(
        &self,
        queue: &QueueRestore,
        source: SourceKey,
    ) -> LibraryResult<Vec<usize>> {
        let mut reader = self.acquire_reader().await?;
        let mut positions = Vec::new();
        for (page, order) in queue.order.chunks(4096).enumerate() {
            let keys = order
                .iter()
                .map(|i| queue.entries[*i as usize].track_key)
                .collect::<Vec<_>>();
            let matching=sqlx::query_scalar::<_,i64>("SELECT CAST(requested.key AS INTEGER) FROM json_each(?1) requested JOIN tracks ON track_key=requested.value WHERE source_key=?2 ORDER BY CAST(requested.key AS INTEGER)")
                .bind(serde_json::to_string(&keys)?).bind(source).fetch_all(&mut *reader).await?;
            positions.extend(
                matching
                    .into_iter()
                    .map(|position| page * 4096 + position as usize),
            );
        }
        Ok(positions)
    }
}

async fn persist_transfer_on(
    connection: &mut sqlx::SqliteConnection,
    transfer: &str,
    total: usize,
) -> LibraryResult<()> {
    for offset in (0..total).step_by(QUEUE_CONTEXT_LIMIT) {
        let payload: String = sqlx::query_scalar(
            "SELECT payload FROM queue_transfer_pages WHERE transfer=?1 AND page_offset=?2",
        )
        .bind(transfer)
        .bind(offset as i64)
        .fetch_one(&mut *connection)
        .await?;
        let page: QueueTransferPage = serde_json::from_str(&payload)?;
        for row in &page.rows {
            persist_metadata(connection, &row.item).await?;
        }
    }
    Ok(())
}

async fn save_settings(
    connection: &mut sqlx::SqliteConnection,
    current: Option<usize>,
    progress: i64,
    repeat: QueueRepeatMode,
    shuffled: bool,
) -> LibraryResult<()> {
    sqlx::query("INSERT INTO queue_state(singleton,current_index,progress_millis,repeat_mode,shuffled) VALUES(1,?1,?2,?3,?4) ON CONFLICT(singleton) DO UPDATE SET current_index=excluded.current_index,progress_millis=excluded.progress_millis,repeat_mode=excluded.repeat_mode,shuffled=excluded.shuffled")
        .bind(current.map(|index| index as i64)).bind(progress.max(0)).bind(repeat.as_str()).bind(shuffled).execute(connection).await?;
    Ok(())
}
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
    let order = state
        .order
        .iter()
        .enumerate()
        .map(|(position, index)| {
            ids.get(*index as usize)
                .copied()
                .map(|id| (position, id))
                .ok_or_else(|| LibraryError::InvalidRequest("Invalid queue play order".into()))
        })
        .collect::<LibraryResult<Vec<_>>>()?;
    apply_queue_update_on(
        connection,
        &QueueUpdate {
            replace: true,
            entries: ids
                .iter()
                .copied()
                .zip(state.entries.iter().cloned())
                .collect(),
            ordinary: Some(QueueOrderUpdate {
                rows: ids.iter().copied().enumerate().collect(),
                total: ids.len(),
            }),
            order: Some(QueueOrderUpdate {
                rows: order,
                total: state.order.len(),
            }),
            current_index: state.current_index,
            progress_millis: state.progress_millis,
            repeat_mode: state.repeat_mode,
            shuffled: state.shuffled,
            ..Default::default()
        },
    )
    .await
}

async fn apply_queue_update_on(
    connection: &mut sqlx::SqliteConnection,
    update: &QueueUpdate,
) -> LibraryResult<()> {
    if update.replace {
        sqlx::raw_sql(
            "DELETE FROM queue_positions; DELETE FROM queue_entries; DELETE FROM queue_contexts;",
        )
        .execute(&mut *connection)
        .await?;
    } else {
        for removed in update.removed.iter().collect::<Vec<_>>().chunks(1000) {
            let contexts = sqlx::query_scalar::<_, Option<i64>>(
                "DELETE FROM queue_entries WHERE entry_id IN (SELECT value FROM json_each(?1)) RETURNING context_key",
            ).bind(serde_json::to_string(removed)?).fetch_all(&mut *connection).await?;
            sqlx::query("DELETE FROM queue_contexts WHERE context_key IN (SELECT value FROM json_each(?1)) AND NOT EXISTS(SELECT 1 FROM queue_entries WHERE context_key=queue_contexts.context_key)")
                .bind(serde_json::to_string(&contexts)?).execute(&mut *connection).await?;
        }
    }
    let mut contexts = HashMap::new();
    let mut entries = update.entries.iter().collect::<Vec<_>>();
    entries.sort_unstable_by_key(|(id, _)| *id);
    for entries in entries.chunks(1000) {
        let mut rows = Vec::with_capacity(entries.len());
        for (id, entry) in entries {
            let (context_key, source_rank, provenance) = match &entry.provenance {
                QueueProvenance::Context {
                    context_id,
                    context_title,
                    source_rank,
                } => {
                    let key = (context_id.as_ref(), context_title.as_deref());
                    let context_key = if let Some(key) = contexts.get(&key) {
                        *key
                    } else {
                        let value = serde_json::to_string(&match context_title {
                            Some(title) => SavedContext::Titled {
                                id: context_id.as_ref(),
                                title: title.clone(),
                            },
                            None => SavedContext::Legacy(context_id.as_ref()),
                        })?;
                        sqlx::query("INSERT INTO queue_contexts(context) VALUES(?1) ON CONFLICT(context) DO NOTHING")
                            .bind(&value).execute(&mut *connection).await?;
                        let stored: i64 = sqlx::query_scalar(
                            "SELECT context_key FROM queue_contexts WHERE context=?1",
                        )
                        .bind(value)
                        .fetch_one(&mut *connection)
                        .await?;
                        contexts.insert(key, stored);
                        stored
                    };
                    (Some(context_key), Some(*source_rank as i64), None)
                }
                provenance => (None, None, Some(serde_json::to_string(provenance)?)),
            };
            rows.push((
                **id as i64,
                entry.track_key.raw(),
                provenance,
                context_key,
                source_rank,
                entry.received_from_connect,
            ));
        }
        sqlx::query("INSERT INTO queue_entries(entry_id,track_key,provenance,context_key,source_rank,received_from_connect)
            SELECT json_extract(value,'$[0]'),json_extract(value,'$[1]'),json_extract(value,'$[2]'),json_extract(value,'$[3]'),json_extract(value,'$[4]'),json_extract(value,'$[5]') FROM json_each(?1) WHERE true ON CONFLICT(entry_id) DO UPDATE SET track_key=excluded.track_key")
            .bind(serde_json::to_string(&rows)?).execute(&mut *connection).await?;
    }
    for (kind, order) in [(0_i64, &update.ordinary), (1, &update.order)] {
        if let Some(order) = order {
            sqlx::query("DELETE FROM queue_positions WHERE order_kind=?1 AND position>=?2")
                .bind(kind)
                .bind(order.total as i64)
                .execute(&mut *connection)
                .await?;
            for rows in order.rows.chunks(1000) {
                sqlx::query("INSERT INTO queue_positions(order_kind,position,entry_id)
                    SELECT ?1,json_extract(value,'$[0]'),json_extract(value,'$[1]') FROM json_each(?2) WHERE true
                    ON CONFLICT(order_kind,position) DO UPDATE SET entry_id=excluded.entry_id")
                    .bind(kind).bind(serde_json::to_string(rows)?).execute(&mut *connection).await?;
            }
        }
    }
    save_settings(
        connection,
        update.current_index,
        update.progress_millis,
        update.repeat_mode,
        update.shuffled,
    )
    .await
}

async fn restore_queue_on(connection: &mut sqlx::SqliteConnection) -> LibraryResult<QueueRestore> {
    let contexts =
        sqlx::query_as::<_, (i64, String)>("SELECT context_key,context FROM queue_contexts")
            .fetch_all(&mut *connection)
            .await?
            .into_iter()
            .map(|(key, json)| Ok((key, serde_json::from_str::<SavedContext<Arc<str>>>(&json)?)))
            .collect::<LibraryResult<HashMap<_, _>>>()?;
    let rows = sqlx::query("SELECT entry.* FROM queue_positions position JOIN queue_entries entry USING(entry_id) WHERE order_kind=0 ORDER BY position")
        .fetch_all(&mut *connection).await?;
    let mut entries = Vec::with_capacity(rows.len());
    let mut ids = Vec::with_capacity(rows.len());
    for row in rows {
        ids.push(row.try_get::<i64, _>("entry_id")? as u64);
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
    let canonical = ids
        .iter()
        .enumerate()
        .map(|(position, id)| (*id, position as u32))
        .collect::<HashMap<_, _>>();
    let order = sqlx::query_scalar::<_, i64>(
        "SELECT entry_id FROM queue_positions WHERE order_kind=1 ORDER BY position",
    )
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|id| canonical[&(id as u64)])
    .collect();
    let mut state = QueueRestore {
        ids: ids.into(),
        entries: entries.into(),
        order,
        ..Default::default()
    };
    read_settings(connection, &mut state).await?;
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

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct LegacyQueueRow {
    #[serde(default)]
    pub playlist_entry_id: Option<String>,
    pub occurrence: String,
    pub item: QueueItem,
    pub canonical_position: usize,
    #[serde(default)]
    pub source_index: Option<usize>,
    pub provenance: QueueProvenance,
}

impl LegacyQueueRow {
    fn entry(&self, track_key: crate::TrackKey) -> QueueEntry {
        QueueEntry {
            track_key,
            provenance: self.provenance.clone(),
            received_from_connect: false,
        }
    }
}

pub(crate) async fn export_queue_jsonl_on(
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
        for item in queue_items_for_uris_on(connection, page).await? {
            if seen.insert(item.media_uri.clone()) {
                serde_json::to_writer(&mut output, &item)?;
                output.write_all(b"\n")?;
            }
        }
    }
    Ok(())
}
pub(crate) async fn import_queue_jsonl_on(
    connection: &mut sqlx::SqliteConnection,
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
                    persist_metadata(connection, &serde_json::from_str::<QueueItem>(&line)?)
                        .await?;
                }
            }
            decode_saved_queue(connection, header["queue"].clone()).await?
        }
        Some(version @ 1..=3) => {
            let mut rows: Vec<LegacyQueueRow> = if version == 3 {
                serde_json::from_value(header["snapshots"].clone())?
            } else if version == 2 {
                serde_json::from_value(header["queue"]["occurrences"].clone())?
            } else {
                Vec::new()
            };
            if version == 1 {
                while {
                    line.clear();
                    input.read_line(&mut line)? != 0
                } {
                    if !line.trim().is_empty() {
                        rows.push(serde_json::from_str(&line)?);
                    }
                }
            }
            let mut keys = Vec::with_capacity(rows.len());
            for row in &rows {
                keys.push(persist_metadata(connection, &row.item).await?);
            }
            match version {
                3 => decode_saved_queue(connection, header["queue"].clone()).await?,
                2 => {
                    let mut transaction = connection.begin().await?;
                    let state =
                        legacy::migrate_saved(&mut transaction, header["queue"].clone(), &rows)
                            .await?;
                    transaction.commit().await?;
                    state
                }
                _ => QueueRestore {
                    current_index: header["current_occurrence"]
                        .as_str()
                        .and_then(|id| rows.iter().position(|row| row.occurrence == id)),
                    progress_millis: header["progress_millis"].as_i64().unwrap_or(0),
                    repeat_mode: serde_json::from_value(header["repeat_mode"].clone())?,
                    shuffled: header["shuffled"].as_bool().unwrap_or(false),
                    order: (0..rows.len() as u32).collect(),
                    entries: rows
                        .iter()
                        .zip(keys)
                        .map(|(row, key)| row.entry(key))
                        .collect(),
                    ..Default::default()
                },
            }
        }
        _ => {
            return Err(LibraryError::InvalidRequest(
                "unsupported Queue export version".into(),
            ));
        }
    };
    save_queue_on(connection, &state).await
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

async fn persist_metadata(
    connection: &mut SqliteConnection,
    item: &QueueItem,
) -> LibraryResult<crate::TrackKey> {
    crate::tracks::remember_song(
        connection,
        &item.media_uri,
        crate::tracks::SongMetadata {
            title: Some(&item.title),
            artist: Some(&item.artist),
            album: Some(&item.album),
            album_display_artist: item.album_display_artist.as_deref(),
            duration_millis: Some(item.duration_millis),
            disc_number: item.disc_number,
            track_number: item.track_number,
            year: item.year,
            release_date: item.release_date.as_deref(),
            source_format: item.source_format.as_deref(),
            audio_properties: (item.audio_properties != crate::AudioProperties::default())
                .then_some(&item.audio_properties),
            musicbrainz_recording_id: item.musicbrainz_recording_id.as_deref(),
            musicbrainz_release_track_id: item.musicbrainz_release_track_id.as_deref(),
            musicbrainz_album_id: item.musicbrainz_album_id.as_deref(),
            musicbrainz_release_group_id: item.musicbrainz_release_group_id.as_deref(),
            primary_artist_musicbrainz_id: item.primary_artist_musicbrainz_id.as_deref(),
        },
    )
    .await
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

async fn queue_items_for_uris_on(
    connection: &mut SqliteConnection,
    uris: &[String],
) -> LibraryResult<Vec<QueueItem>> {
    Ok(sqlx::query_as("SELECT requested.value media_uri,COALESCE(track.title,requested.value) title,COALESCE(track.display_artist,'') artist,COALESCE(track.display_album,'') album,
        COALESCE(album.display_artist,track.album_display_artist) album_display_artist,track.artwork_binding,COALESCE(track.duration_millis,0) duration_millis,track.disc_number,track.track_number,
        track.year,track.release_date,track.source_format,COALESCE(track.audio_properties,'{}') audio_properties,track.musicbrainz_recording_id,track.musicbrainz_release_track_id,
        COALESCE(album.musicbrainz_release_id,track.musicbrainz_album_id) musicbrainz_album_id,COALESCE(album.musicbrainz_release_group_id,track.musicbrainz_release_group_id) musicbrainz_release_group_id,
        COALESCE((SELECT artist.musicbrainz_artist_id FROM track_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1),track.primary_artist_musicbrainz_id) primary_artist_musicbrainz_id
        FROM json_each(?1) requested LEFT JOIN tracks track ON track.media_uri=requested.value LEFT JOIN albums album USING(album_key) ORDER BY CAST(requested.key AS INTEGER)")
        .bind(serde_json::to_string(uris)?).fetch_all(connection).await?)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl QueueRow {
    pub fn same_row(&self, other: Arc<QueueRow>) -> bool {
        std::ptr::eq(self, other.as_ref()) || self == other.as_ref()
    }

    pub fn item(&self) -> QueueItem {
        self.item.clone()
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
