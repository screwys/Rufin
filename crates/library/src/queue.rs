//! Owns complete durable Queue order and bounded playback windows.
//! Playback resolution uses the bypass reader and never scans the Library catalog.

use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use sqlx::sqlite::SqliteRow;
use sqlx::{Connection, FromRow, Row, Sqlite, Transaction};

use crate::{Database, LibraryError, LibraryResult, ReadCancellation, SourceId, SourceKey};

const QUEUE_PAGE_LIMIT: usize = 100;
pub const QUEUE_CONTEXT_LIMIT: usize = 100;
const QUEUE_PRIMARY_ARTIST_SQL: &str = "COALESCE(
    (SELECT artist.media_uri FROM track_artists credit
     JOIN artists artist USING(artist_key)
     WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1),
    (SELECT artist.media_uri FROM album_artists credit
     JOIN artists artist USING(artist_key)
     WHERE credit.album_key=track.album_key ORDER BY credit.position LIMIT 1))";

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum QueueProvenance {
    Context {
        context_id: Arc<str>,
        source_rank: usize,
    },
    Manual,
    Random,
    Radio,
    AutoDj,
    Legacy,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum QueueRepeatMode {
    #[default]
    Off,
    One,
    All,
}

#[derive(
    Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct OccurrenceId(String);

impl OccurrenceId {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        assert!(!value.is_empty(), "OccurrenceId cannot be empty");
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for OccurrenceId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for OccurrenceId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for OccurrenceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
            musicbrainz_recording_id: None,
            musicbrainz_release_track_id: None,
            musicbrainz_album_id: None,
            musicbrainz_release_group_id: None,
            primary_artist_musicbrainz_id: None,
        }
    }
}

impl<'row> FromRow<'row, SqliteRow> for QueueItem {
    fn from_row(row: &'row SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            media_uri: row.try_get("media_uri")?,
            title: row.try_get("title")?,
            artist: row.try_get("artist")?,
            album: row.try_get("album")?,
            album_display_artist: row.try_get("album_display_artist")?,
            artwork_binding: row.try_get("artwork_binding")?,
            duration_millis: row.try_get("duration_millis")?,
            disc_number: row.try_get("disc_number")?,
            track_number: row.try_get("track_number")?,
            year: row.try_get("year")?,
            release_date: row.try_get("release_date")?,
            source_format: row.try_get("source_format")?,
            musicbrainz_recording_id: row.try_get("musicbrainz_recording_id")?,
            musicbrainz_release_track_id: row.try_get("musicbrainz_release_track_id")?,
            musicbrainz_album_id: row.try_get("musicbrainz_album_id")?,
            musicbrainz_release_group_id: row.try_get("musicbrainz_release_group_id")?,
            primary_artist_musicbrainz_id: row.try_get("primary_artist_musicbrainz_id")?,
        })
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
pub struct QueueOccurrence {
    pub occurrence: OccurrenceId,
    pub item: QueueItem,
    pub canonical_position: usize,
    pub provenance: QueueProvenance,
}

impl Deref for QueueOccurrence {
    type Target = QueueItem;

    fn deref(&self) -> &Self::Target {
        &self.item
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueRestore {
    pub removed_successors: Vec<(OccurrenceId, Option<OccurrenceId>)>,
    pub total: usize,
    pub window_start: usize,
    pub current_index: Option<usize>,
    pub wrap_previous: Option<QueueOccurrence>,
    pub wrap_next: Option<QueueOccurrence>,
    pub occurrences: Vec<QueueOccurrence>,
    pub current_occurrence: Option<OccurrenceId>,
    pub progress_millis: i64,
    pub repeat_mode: QueueRepeatMode,
    pub shuffled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum QueueCollection {
    Album(String),
    AlbumKey(crate::AlbumKey),
    Artist {
        media_uri: String,
        album_artist: bool,
    },
    ArtistKey {
        key: crate::ArtistKey,
        album_artist: bool,
    },
    Genre(crate::GenreKey),
    Mood(crate::MoodKey),
    Playlist(crate::PlaylistKey),
}

#[derive(Clone, Debug, PartialEq)]
pub enum QueueInput {
    Groups(Vec<QueueInput>),
    MediaUris {
        order: Arc<[String]>,
        provenance: QueueProvenance,
    },
    Items(Vec<(QueueItem, QueueProvenance)>),
    Uris {
        order: Arc<[String]>,
        context_id: Arc<str>,
        source_start: usize,
    },
    PlaylistEntries {
        order: Arc<[crate::PlaylistEntryKey]>,
        context_id: Arc<str>,
    },
    Smart {
        key: crate::SmartPlaylistKey,
        source: Option<SourceKey>,
        folder: Option<crate::FolderKey>,
        now: i64,
        context_id: Arc<str>,
    },
    Collection {
        collection: QueueCollection,
        folder: Option<crate::FolderKey>,
        context_id: Arc<str>,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueuePlacement {
    Replace { anchor_index: usize },
    AfterCurrent,
    End,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueueReorderTarget {
    Before(OccurrenceId),
    After(OccurrenceId),
    End,
}
#[derive(Clone, Debug, PartialEq)]
pub enum QueueEdit {
    Apply {
        input: QueueInput,
        placement: QueuePlacement,
        shuffle_seed: Option<u64>,
        random_start: bool,
        identity: Option<String>,
    },
    Insert {
        input: QueueInput,
        target: QueueReorderTarget,
    },
    Remove(Vec<OccurrenceId>),
    Reorder {
        occurrences: Vec<OccurrenceId>,
        target: QueueReorderTarget,
    },
    MoveAfterCurrent(OccurrenceId),
    Clear {
        include_current: bool,
    },
    Shuffle {
        enabled: bool,
        seed: u64,
    },
    TrimAutoDj {
        keep: usize,
    },
    Select(OccurrenceId),
    SelectOptional(Option<OccurrenceId>),
    SelectIndex(usize),
}

#[derive(FromRow)]
struct QueueOccurrenceRow {
    object_id: String,
    canonical_position: i64,
    provenance_kind: String,
    provenance_context_id: Option<String>,
    provenance_source_rank: Option<i64>,
    #[sqlx(flatten)]
    item: QueueItem,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueuePageRow {
    pub occurrence: OccurrenceId,
    pub position: i64,
    pub favorite: bool,
    pub primary_artist_media_uri: Option<String>,
    pub item: QueueItem,
}

impl Deref for QueuePageRow {
    type Target = QueueItem;
    fn deref(&self) -> &Self::Target {
        &self.item
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

impl QueueProvenance {
    fn columns(&self) -> (&'static str, Option<&str>, Option<i64>) {
        match self {
            Self::Context {
                context_id,
                source_rank,
            } => ("context", Some(context_id), Some(*source_rank as i64)),
            Self::Manual => ("manual", None, None),
            Self::Random => ("random", None, None),
            Self::Radio => ("radio", None, None),
            Self::AutoDj => ("auto-dj", None, None),
            Self::Legacy => ("legacy", None, None),
        }
    }

    fn parse(
        kind: &str,
        context_id: Option<String>,
        source_rank: Option<i64>,
    ) -> LibraryResult<Self> {
        match kind {
            "context" => Ok(Self::Context {
                context_id: context_id
                    .ok_or_else(|| {
                        LibraryError::InvalidStore("queue Context has no context ID".to_string())
                    })?
                    .into(),
                source_rank: usize::try_from(source_rank.ok_or_else(|| {
                    LibraryError::InvalidStore("queue Context has no source rank".to_string())
                })?)
                .map_err(|_| {
                    LibraryError::InvalidStore("queue Context has invalid source rank".to_string())
                })?,
            }),
            "manual" => Ok(Self::Manual),
            "random" => Ok(Self::Random),
            "radio" => Ok(Self::Radio),
            "auto-dj" => Ok(Self::AutoDj),
            "legacy" => Ok(Self::Legacy),
            _ => Err(LibraryError::InvalidStore(
                "invalid queue provenance".to_string(),
            )),
        }
    }
}

async fn persist_occurrence_page(
    transaction: &mut sqlx::SqliteConnection,
    occurrences: &[QueueOccurrence],
    traversal_offset: usize,
) -> LibraryResult<()> {
    for (traversal_position, occurrence) in occurrences.iter().enumerate() {
        let item = &occurrence.item;
        let (kind, context, rank) = occurrence.provenance.columns();
        sqlx::query(
            "INSERT INTO queue_occurrences(
                 object_id,media_uri,position,traversal_position,
                 provenance_kind,provenance_context_id,provenance_source_rank,
                 title,artist,album,album_display_artist,duration_millis,
                 disc_number,track_number,year,release_date,source_format,
                 musicbrainz_recording_id,musicbrainz_release_track_id,
                 musicbrainz_album_id,musicbrainz_release_group_id,
                 primary_artist_musicbrainz_id
             ) VALUES (
                 ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
                 ?17,?18,?19,?20,?21,?22
             ) ON CONFLICT(object_id) DO UPDATE SET
                 position=excluded.position,traversal_position=excluded.traversal_position,
                 provenance_kind=excluded.provenance_kind,
                 provenance_context_id=excluded.provenance_context_id,
                 provenance_source_rank=excluded.provenance_source_rank",
        )
        .bind(occurrence.occurrence.as_str())
        .bind(&item.media_uri)
        .bind(occurrence.canonical_position as i64)
        .bind((traversal_offset + traversal_position) as i64)
        .bind(kind)
        .bind(context)
        .bind(rank)
        .bind(&item.title)
        .bind(&item.artist)
        .bind(&item.album)
        .bind(&item.album_display_artist)
        .bind(item.duration_millis)
        .bind(item.disc_number)
        .bind(item.track_number)
        .bind(item.year)
        .bind(&item.release_date)
        .bind(&item.source_format)
        .bind(&item.musicbrainz_recording_id)
        .bind(&item.musicbrainz_release_track_id)
        .bind(&item.musicbrainz_album_id)
        .bind(&item.musicbrainz_release_group_id)
        .bind(&item.primary_artist_musicbrainz_id)
        .execute(&mut *transaction)
        .await?;
    }
    Ok(())
}

impl Database {
    /// Resolve only the active window of an already captured order. SQL-owned collection
    /// membership is prepared by the Queue writer once, so starting playback cannot alter it.
    pub async fn prepare_queue_window(
        &self,
        input: &QueueInput,
        anchor: usize,
        shuffle_seed: Option<u64>,
        random_start: bool,
        identity: &str,
    ) -> LibraryResult<Option<QueueRestore>> {
        let total = match input {
            QueueInput::Items(items) => items.len(),
            QueueInput::Uris { order, .. } | QueueInput::MediaUris { order, .. } => order.len(),
            QueueInput::PlaylistEntries { order, .. } => order.len(),
            _ => return Ok(None),
        };
        if total == 0 {
            return Ok(None);
        }
        let anchor = if random_start {
            shuffle_seed.unwrap_or(0) as usize % total
        } else {
            anchor.min(total - 1)
        };
        let positions = preview_positions(total, anchor, shuffle_seed);
        let cancellation = ReadCancellation::new();
        let rows = match input {
            QueueInput::Items(items) => positions
                .iter()
                .map(|position| items[*position].clone())
                .collect(),
            QueueInput::Uris {
                order,
                context_id,
                source_start,
            } => {
                let uris = positions
                    .iter()
                    .map(|position| order[*position].clone())
                    .collect::<Vec<_>>();
                self.queue_items_for_uris(&uris, &cancellation)
                    .await?
                    .into_iter()
                    .zip(&positions)
                    .map(|(item, position)| {
                        (
                            item,
                            QueueProvenance::Context {
                                context_id: context_id.clone(),
                                source_rank: source_start + position,
                            },
                        )
                    })
                    .collect()
            }
            QueueInput::MediaUris { order, provenance } => {
                let uris = positions
                    .iter()
                    .map(|position| order[*position].clone())
                    .collect::<Vec<_>>();
                self.queue_items_for_uris(&uris, &cancellation)
                    .await?
                    .into_iter()
                    .map(|item| (item, provenance.clone()))
                    .collect()
            }
            QueueInput::PlaylistEntries { order, context_id } => {
                let keys = positions
                    .iter()
                    .map(|position| order[*position])
                    .collect::<Vec<_>>();
                let entries = self.playlist_entry_rows(&keys, &cancellation).await?;
                if entries.len() != positions.len() {
                    return Ok(None);
                }
                entries
                    .into_iter()
                    .zip(&positions)
                    .map(|(item, position)| {
                        (
                            item.into(),
                            QueueProvenance::Context {
                                context_id: context_id.clone(),
                                source_rank: *position,
                            },
                        )
                    })
                    .collect()
            }
            _ => unreachable!(),
        };
        Ok(Some(preview_window(
            rows,
            total,
            anchor,
            shuffle_seed,
            identity,
        )))
    }

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
        if media_uris.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = tokio::select! {
            result = self.acquire_reader() => result?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        let requested = serde_json::to_string(media_uris)?;
        let sql = format!(
            "WITH requested AS (
               SELECT value media_uri,key ordinal FROM json_each(?1)
             ), {snapshots}
               SELECT requested.media_uri,COALESCE(track.title,entry.title,queued.title,listen.track_title,requested.media_uri) title,
                      COALESCE(track.display_artist,entry.artist,queued.artist,listen.artist_name,'') artist,
                      COALESCE(track.display_album,entry.album,queued.album,listen.album_title,'') album,
                      COALESCE(album.display_artist,entry.album_display_artist,queued.album_display_artist) album_display_artist,
                      track.artwork_binding,COALESCE(track.duration_millis,entry.duration_millis,queued.duration_millis,listen.duration_millis,0) duration_millis,
                      COALESCE(track.disc_number,entry.disc_number,queued.disc_number,listen.disc_number) disc_number,
                      COALESCE(track.track_number,entry.track_number,queued.track_number,listen.track_number) track_number,
                      COALESCE(track.year,entry.year,queued.year,listen.year) year,
                      COALESCE(track.release_date,entry.release_date,queued.release_date,listen.release_date) release_date,
                      COALESCE(track.source_format,entry.source_format,queued.source_format,listen.source_format) source_format,
                      COALESCE(track.musicbrainz_recording_id,entry.musicbrainz_recording_id,queued.musicbrainz_recording_id,listen.musicbrainz_recording_id) musicbrainz_recording_id,
                      COALESCE(track.musicbrainz_release_track_id,entry.musicbrainz_release_track_id,queued.musicbrainz_release_track_id,listen.musicbrainz_release_track_id) musicbrainz_release_track_id,
                      COALESCE(album.musicbrainz_release_id,queued.musicbrainz_album_id) musicbrainz_album_id,
                      COALESCE(album.musicbrainz_release_group_id,queued.musicbrainz_release_group_id) musicbrainz_release_group_id,
                      COALESCE(
                        (SELECT artist.musicbrainz_artist_id FROM track_artists credit
                         JOIN artists artist USING(artist_key)
                         WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1),
                        (SELECT artist.musicbrainz_artist_id FROM album_artists credit
                         JOIN artists artist USING(artist_key)
                         WHERE credit.album_key=track.album_key ORDER BY credit.position LIMIT 1),
                        queued.primary_artist_musicbrainz_id
                      ) primary_artist_musicbrainz_id
               FROM requested LEFT JOIN tracks track USING(media_uri)
               LEFT JOIN albums album USING(album_key)
               LEFT JOIN playlist_snapshots entry ON entry.media_uri=requested.media_uri
               LEFT JOIN queue_occurrences queued ON queued.queue_occurrence_key=(SELECT queue_occurrence_key FROM queue_occurrences WHERE media_uri=requested.media_uri ORDER BY snapshot_at DESC,queue_occurrence_key DESC LIMIT 1)
               LEFT JOIN listens listen ON listen.listen_key=(SELECT listen_key FROM listens WHERE media_uri=requested.media_uri ORDER BY started_at DESC,listen_key DESC LIMIT 1)
               ORDER BY requested.ordinal",
            snapshots = crate::playlists::PLAYLIST_URI_SNAPSHOTS,
        );
        let rows = sqlx::query_as::<_, QueueItem>(sqlx::AssertSqlSafe(sql.as_str()))
            .bind(requested)
            .fetch_all(&mut *connection)
            .await?;
        Ok(rows)
    }

    pub async fn persist_queue_progress(
        &self,
        current_object_id: Option<&OccurrenceId>,
        progress_millis: i64,
    ) -> LibraryResult<bool> {
        if progress_millis < 0 {
            return Err(LibraryError::InvalidRequest(
                "queue progress cannot be negative".to_string(),
            ));
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let changed = sqlx::query(
            "UPDATE queue_state SET
                 current_occurrence_id=?1,
                 progress_millis=?2
             WHERE singleton=1",
        )
        .bind(current_object_id.map(OccurrenceId::as_str))
        .bind(progress_millis)
        .execute(connection)
        .await?
        .rows_affected()
            == 1;
        Ok(changed)
    }

    pub async fn restore_queue(&self) -> LibraryResult<QueueRestore> {
        self.queue_window(None).await
    }

    pub async fn queue_context_occurrence(
        &self,
        context: &str,
        media_uri: Option<&str>,
        source_rank: usize,
    ) -> LibraryResult<Option<OccurrenceId>> {
        let mut connection = self.acquire_reader().await?;
        let id=match media_uri {
            Some(uri)=>sqlx::query_scalar::<_,String>("SELECT object_id FROM queue_occurrences WHERE provenance_context_id=?1 AND provenance_source_rank=?2 AND media_uri=?3 LIMIT 1").bind(context).bind(source_rank as i64).bind(uri).fetch_optional(&mut *connection).await?,
            None=>sqlx::query_scalar::<_,String>("SELECT object_id FROM queue_occurrences WHERE provenance_context_id=?1 AND provenance_source_rank=?2 LIMIT 1").bind(context).bind(source_rank as i64).fetch_optional(&mut *connection).await?,
        };
        Ok(id.map(OccurrenceId::new))
    }
    pub async fn queue_window(&self, anchor: Option<&OccurrenceId>) -> LibraryResult<QueueRestore> {
        let mut connection = self.acquire_reader().await?;
        let mut transaction = connection.begin().await?;
        let result = read_window(&mut transaction, anchor, None).await?;
        transaction.commit().await?;
        Ok(result)
    }
    pub async fn queue_window_at(&self, index: usize) -> LibraryResult<QueueRestore> {
        let mut connection = self.acquire_reader().await?;
        let mut transaction = connection.begin().await?;
        let result = read_window(&mut transaction, None, Some(index)).await?;
        transaction.commit().await?;
        Ok(result)
    }

    pub async fn queue_occurrences_for_source(
        &self,
        source: SourceKey,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        let Some(source_id) =
            sqlx::query_scalar::<_, String>("SELECT object_id FROM sources WHERE source_key=?1")
                .bind(source)
                .fetch_optional(&mut *connection)
                .await?
        else {
            return Ok(Vec::new());
        };
        let prefix = crate::keys::source_entity_prefix(&SourceId::new(source_id), "track");
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT occurrence.object_id FROM queue_occurrences occurrence
             WHERE substr(occurrence.media_uri,1,length(?2))=?2
                OR EXISTS(
                    SELECT 1 FROM tracks track
                    WHERE track.source_key=?1 AND track.media_uri=occurrence.media_uri
                )
             ORDER BY occurrence.position",
        )
        .bind(source)
        .bind(prefix)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn queue_page(
        &self,
        after_position: Option<i64>,
        filter: &str,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<QueuePageRow>> {
        self.queue_page_direction(after_position, filter, limit, false, cancellation)
            .await
    }

    pub async fn queue_page_direction(
        &self,
        position: Option<i64>,
        filter: &str,
        limit: usize,
        backwards: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<QueuePageRow>> {
        let limit = limit.clamp(1, QUEUE_PAGE_LIMIT) as i64;
        let filter = queue_search_pattern(filter);
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let comparison = if backwards { "<" } else { ">" };
        let direction = if backwards { "DESC" } else { "ASC" };
        let rows = sqlx::query_as::<_, QueuePageRow>(sqlx::AssertSqlSafe(format!(
            "WITH page AS (
               SELECT queue_occurrence_key,position FROM queue_occurrences
               WHERE position{comparison}?1
                 AND (?3='' OR title REGEXP ?3 OR artist REGEXP ?3 OR album REGEXP ?3)
               ORDER BY position {direction} LIMIT ?2
             )
             SELECT occurrence.object_id,occurrence.position,occurrence.media_uri,
               occurrence.title,occurrence.artist,occurrence.album,
               occurrence.album_display_artist,track.artwork_binding,
               occurrence.duration_millis,occurrence.disc_number,occurrence.track_number,
               occurrence.year,occurrence.release_date,occurrence.source_format,
               occurrence.musicbrainz_recording_id,occurrence.musicbrainz_release_track_id,
               occurrence.musicbrainz_album_id,occurrence.musicbrainz_release_group_id,
               occurrence.primary_artist_musicbrainz_id,
               {QUEUE_PRIMARY_ARTIST_SQL} primary_artist_media_uri,
               COALESCE((SELECT state.favorite FROM user_media_state state
                         WHERE state.media_uri=occurrence.media_uri),
                        (SELECT track.source_favorite FROM tracks track
                         WHERE track.media_uri=occurrence.media_uri),0) favorite
             FROM page JOIN queue_occurrences occurrence USING(queue_occurrence_key)
             LEFT JOIN tracks track USING(media_uri)
             ORDER BY occurrence.position"
        )))
        .bind(position.unwrap_or(if backwards { i64::MAX } else { -1 }))
        .bind(limit)
        .bind(filter)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(rows)
    }

    pub async fn prepared_queue_page(
        &self,
        window: &[Arc<QueueOccurrence>],
        filter: &str,
    ) -> LibraryResult<Vec<QueuePageRow>> {
        if window.len() > QUEUE_CONTEXT_LIMIT {
            return Err(LibraryError::InvalidStore(
                "Queue window exceeds its context".into(),
            ));
        }
        if window.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = sqlx::QueryBuilder::<Sqlite>::new(
            "WITH requested(ordinal,media_uri,title,artist,album) AS (",
        );
        query.push_values(
            window.iter().enumerate(),
            |mut row, (ordinal, occurrence)| {
                row.push_bind(ordinal as i64)
                    .push_bind(&occurrence.media_uri)
                    .push_bind(&occurrence.title)
                    .push_bind(&occurrence.artist)
                    .push_bind(&occurrence.album);
            },
        );
        query.push(
            ") SELECT ordinal,COALESCE(
                (SELECT state.favorite FROM user_media_state state WHERE state.media_uri=requested.media_uri),
                track.source_favorite,0),",
        );
        query
            .push(QUEUE_PRIMARY_ARTIST_SQL)
            .push(" FROM requested LEFT JOIN tracks track ON track.media_uri=requested.media_uri");
        let filter = queue_search_pattern(filter);
        if !filter.is_empty() {
            query
                .push(" WHERE requested.title REGEXP ")
                .push_bind(&filter)
                .push(" OR requested.artist REGEXP ")
                .push_bind(&filter)
                .push(" OR requested.album REGEXP ")
                .push_bind(&filter);
        }
        let mut connection = self.acquire_reader().await?;
        let facts = query
            .build_query_as::<(i64, bool, Option<String>)>()
            .fetch_all(&mut *connection)
            .await?;
        let mut rows = facts
            .into_iter()
            .map(|(ordinal, favorite, primary_artist_media_uri)| {
                let occurrence = &window[ordinal as usize];
                QueuePageRow {
                    occurrence: occurrence.occurrence.clone(),
                    position: occurrence.canonical_position as i64,
                    favorite,
                    primary_artist_media_uri,
                    item: occurrence.item.clone(),
                }
            })
            .collect::<Vec<_>>();
        rows.sort_unstable_by_key(|row| row.position);
        Ok(rows)
    }
}

fn queue_search_pattern(filter: &str) -> String {
    let filter: String = filter.trim().chars().take(256).collect();
    if filter.is_empty() {
        filter
    } else {
        format!("(?i){}", regex::escape(&filter))
    }
}

impl<'row> FromRow<'row, SqliteRow> for QueuePageRow {
    fn from_row(row: &'row SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            occurrence: OccurrenceId::new(row.try_get::<String, _>("object_id")?),
            position: row.try_get("position")?,
            favorite: row.try_get("favorite")?,
            primary_artist_media_uri: row.try_get("primary_artist_media_uri")?,
            item: QueueItem::from_row(row)?,
        })
    }
}

async fn require_occurrence(
    transaction: &mut sqlx::SqliteConnection,
    object_id: Option<&OccurrenceId>,
) -> LibraryResult<()> {
    let Some(object_id) = object_id else {
        return Ok(());
    };
    let present = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM queue_occurrences WHERE object_id=?1)",
    )
    .bind(object_id.as_str())
    .fetch_one(&mut *transaction)
    .await?;
    if present {
        Ok(())
    } else {
        Err(LibraryError::InvalidRequest(
            "queue state references an unknown occurrence".to_string(),
        ))
    }
}

async fn read_window(
    transaction: &mut Transaction<'_, Sqlite>,
    anchor: Option<&OccurrenceId>,
    index: Option<usize>,
) -> LibraryResult<QueueRestore> {
    let stored = sqlx::query_as::<_, (Option<String>,i64,String,bool)>("SELECT current_occurrence_id,progress_millis,repeat_mode,shuffled FROM queue_state WHERE singleton=1").fetch_optional(&mut **transaction).await?;
    let (current, progress_millis, repeat, shuffled) =
        stored.unwrap_or((None, 0, "none".into(), false));
    let total = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(max(traversal_position)+1,0) FROM queue_occurrences",
    )
    .fetch_one(&mut **transaction)
    .await? as usize;
    let current_index = sqlx::query_scalar::<_, i64>(
        "SELECT traversal_position FROM queue_occurrences WHERE object_id=?1",
    )
    .bind(current.as_deref())
    .fetch_optional(&mut **transaction)
    .await?
    .map(|v| v as usize);
    let selected = if let Some(anchor) = anchor {
        sqlx::query_scalar::<_, i64>(
            "SELECT traversal_position FROM queue_occurrences WHERE object_id=?1",
        )
        .bind(anchor.as_str())
        .fetch_optional(&mut **transaction)
        .await?
        .map(|v| v as usize)
    } else {
        index.or(current_index)
    }
    .unwrap_or(0)
    .min(total.saturating_sub(1));
    // Leave room for wrap neighbors and the backend's current/prepared snapshots.
    let limit = QUEUE_CONTEXT_LIMIT - 4;
    let window_start = selected.saturating_sub(50).min(total.saturating_sub(limit));
    let occurrences = read_occurrences(transaction, window_start, limit).await?;
    let wrap_previous = if total > limit && window_start == 0 {
        read_occurrences(transaction, total - 1, 1).await?.pop()
    } else {
        None
    };
    let wrap_next = if total > limit && window_start + occurrences.len() == total {
        read_occurrences(transaction, 0, 1).await?.pop()
    } else {
        None
    };
    Ok(QueueRestore {
        removed_successors: Vec::new(),
        total,
        window_start,
        current_index,
        wrap_previous,
        wrap_next,
        occurrences,
        current_occurrence: current
            .filter(|_| current_index.is_some())
            .map(OccurrenceId::new),
        progress_millis,
        repeat_mode: QueueRepeatMode::parse(&repeat)?,
        shuffled,
    })
}

async fn read_occurrences(
    transaction: &mut sqlx::SqliteConnection,
    start: usize,
    limit: usize,
) -> LibraryResult<Vec<QueueOccurrence>> {
    let rows = sqlx::query_as::<_,QueueOccurrenceRow>("SELECT occurrence.object_id,occurrence.media_uri,occurrence.position canonical_position,occurrence.provenance_kind,occurrence.provenance_context_id,occurrence.provenance_source_rank,occurrence.title,occurrence.artist,occurrence.album,occurrence.album_display_artist,track.artwork_binding,occurrence.duration_millis,occurrence.disc_number,occurrence.track_number,occurrence.year,occurrence.release_date,occurrence.source_format,occurrence.musicbrainz_recording_id,occurrence.musicbrainz_release_track_id,occurrence.musicbrainz_album_id,occurrence.musicbrainz_release_group_id,occurrence.primary_artist_musicbrainz_id FROM queue_occurrences occurrence LEFT JOIN tracks track USING(media_uri)  WHERE occurrence.traversal_position>=?1 ORDER BY occurrence.traversal_position LIMIT ?2")
        .bind(start as i64).bind(limit as i64).fetch_all(&mut *transaction).await?;
    rows.into_iter()
        .map(|row| {
            Ok(QueueOccurrence {
                occurrence: OccurrenceId::new(row.object_id),
                item: row.item,
                canonical_position: row.canonical_position as usize,
                provenance: QueueProvenance::parse(
                    &row.provenance_kind,
                    row.provenance_context_id,
                    row.provenance_source_rank,
                )?,
            })
        })
        .collect()
}

impl Database {
    pub async fn persist_queue_settings(
        &self,
        current: Option<&OccurrenceId>,
        progress: i64,
        repeat: QueueRepeatMode,
        shuffled: bool,
    ) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        save_state(&mut transaction, current, progress, repeat, shuffled).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn edit_queue_with_preview(
        &self,
        edit: QueueEdit,
        current: Option<&OccurrenceId>,
        repeat: QueueRepeatMode,
        mut shuffled: bool,
        mut progress: i64,
        preview: impl Fn(QueueRestore) + Send + Sync,
    ) -> LibraryResult<QueueRestore> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let mut current = current.cloned();
        let mut removed_successors = Vec::new();
        let before_removal = if matches!(&edit, QueueEdit::Remove(_) | QueueEdit::Clear { .. }) {
            capture_order_window(&mut transaction, current.as_ref()).await?
        } else {
            Vec::new()
        };
        let auto_dj_append = matches!(&edit,QueueEdit::Apply{input:QueueInput::Items(items),..} if items.iter().any(|(_,provenance)|*provenance==QueueProvenance::AutoDj));
        match edit {
            QueueEdit::Apply {
                input,
                placement,
                shuffle_seed,
                random_start,
                identity,
            } => {
                let replacing = matches!(placement, QueuePlacement::Replace { .. });
                let anchor = if let QueuePlacement::Replace { anchor_index } = placement {
                    anchor_index
                } else {
                    0
                };
                if replacing {
                    current = None;
                    progress = 0;
                    shuffled = shuffle_seed.is_some();
                }
                let target = match placement {
                    QueuePlacement::AfterCurrent => current
                        .clone()
                        .map(QueueReorderTarget::After)
                        .unwrap_or(QueueReorderTarget::End),
                    _ => QueueReorderTarget::End,
                };
                let preview = if replacing
                    && anchor == 0
                    && shuffle_seed.is_none()
                    && let Some(identity) = identity.as_deref()
                    && let QueueInput::Collection {
                        collection,
                        folder,
                        context_id,
                    } = &input
                {
                    let (total, members) = crate::collections::collection_queue_first_window(
                        &mut transaction,
                        collection,
                        *folder,
                    )
                    .await?;
                    if total > 0 {
                        let items = if matches!(collection, QueueCollection::Playlist(_)) {
                            let keys = members
                                .iter()
                                .filter_map(|(_, key)| *key)
                                .collect::<Vec<_>>();
                            self.playlist_entry_rows(&keys, &ReadCancellation::new())
                                .await?
                                .into_iter()
                                .map(QueueItem::from)
                                .collect()
                        } else {
                            let uris = members.into_iter().map(|(uri, _)| uri).collect::<Vec<_>>();
                            self.queue_items_for_uris(&uris, &ReadCancellation::new())
                                .await?
                        };
                        let positions = preview_positions(total, 0, None);
                        let rows = items
                            .into_iter()
                            .zip(positions)
                            .map(|(item, source_rank)| {
                                (
                                    item,
                                    QueueProvenance::Context {
                                        context_id: context_id.clone(),
                                        source_rank,
                                    },
                                )
                            })
                            .collect();
                        preview(preview_window(rows, total, 0, None, identity));
                    }
                    None
                } else {
                    Some(&preview as &(dyn Fn(QueueRestore) + Send + Sync))
                };
                let (start, count) = self
                    .insert_queue_input(
                        &mut transaction,
                        input,
                        target,
                        replacing,
                        identity.as_deref(),
                        anchor,
                        shuffle_seed,
                        random_start,
                        preview,
                    )
                    .await?;
                if replacing && count > 0 {
                    let offset = if random_start {
                        shuffle_seed.unwrap_or(0) as usize % count
                    } else {
                        anchor.min(count - 1)
                    };
                    current = occurrence_at(&mut transaction, start + offset).await?;
                }
                if let Some(seed) = shuffle_seed {
                    shuffle_order(&mut transaction, true, seed, current.as_ref()).await?;
                    shuffled = true;
                }
            }
            QueueEdit::Insert { input, target } => {
                self.insert_queue_input(
                    &mut transaction,
                    input,
                    target,
                    false,
                    None,
                    0,
                    None,
                    false,
                    Some(&preview),
                )
                .await?;
            }
            QueueEdit::Remove(ids) => {
                let old_index = sqlx::query_scalar::<_, i64>(
                    "SELECT traversal_position FROM queue_occurrences WHERE object_id=?1",
                )
                .bind(current.as_ref().map(OccurrenceId::as_str))
                .fetch_optional(&mut *transaction)
                .await?;
                let selected_removed = current.as_ref().is_some_and(|id| ids.contains(id));
                let mut first_canonical = i64::MAX;
                let mut first_traversal = i64::MAX;
                for id in ids {
                    if let Some((canonical,traversal))=sqlx::query_as::<_,(i64,i64)>("SELECT position,traversal_position FROM queue_occurrences WHERE object_id=?1").bind(id.as_str()).fetch_optional(&mut *transaction).await? {first_canonical=first_canonical.min(canonical);first_traversal=first_traversal.min(traversal);}
                    sqlx::query("DELETE FROM queue_occurrences WHERE object_id=?1")
                        .bind(id.as_str())
                        .execute(&mut *transaction)
                        .await?;
                }
                removed_successors =
                    removed_window_successors(&mut transaction, &before_removal, repeat).await?;
                if selected_removed {
                    current = sqlx::query_scalar::<_,String>("SELECT object_id FROM queue_occurrences WHERE traversal_position>?1 ORDER BY traversal_position LIMIT 1").bind(old_index.unwrap_or(-1)).fetch_optional(&mut *transaction).await?.map(OccurrenceId::new);
                    if current.is_none() && repeat == QueueRepeatMode::All {
                        current=sqlx::query_scalar::<_,String>("SELECT object_id FROM queue_occurrences ORDER BY traversal_position LIMIT 1").fetch_optional(&mut *transaction).await?.map(OccurrenceId::new);
                    }
                    progress = 0;
                }
                if first_canonical != i64::MAX {
                    remap_order(&mut transaction, "position", "position", first_canonical).await?;
                    remap_order(
                        &mut transaction,
                        "traversal_position",
                        "traversal_position",
                        first_traversal,
                    )
                    .await?;
                }
            }
            QueueEdit::Reorder {
                occurrences,
                target,
            } => {
                move_occurrences(&mut transaction, &occurrences, target, shuffled).await?;
            }
            QueueEdit::MoveAfterCurrent(occurrence) => {
                if let Some(id) = &current {
                    move_occurrences(
                        &mut transaction,
                        &[occurrence],
                        QueueReorderTarget::After(id.clone()),
                        false,
                    )
                    .await?;
                }
            }
            QueueEdit::Clear { include_current } => {
                sqlx::query("DELETE FROM queue_occurrences WHERE ?1 OR traversal_position>COALESCE((SELECT traversal_position FROM queue_occurrences WHERE object_id=?2),-1)").bind(include_current).bind(current.as_ref().map(OccurrenceId::as_str)).execute(&mut *transaction).await?;
                removed_successors =
                    removed_window_successors(&mut transaction, &before_removal, repeat).await?;
                compact_order(&mut transaction, "position").await?;
                compact_order(&mut transaction, "traversal_position").await?;
            }
            QueueEdit::Shuffle { enabled, seed } => {
                if enabled != shuffled {
                    shuffle_order(&mut transaction, enabled, seed, current.as_ref()).await?;
                    shuffled = enabled;
                }
            }
            QueueEdit::TrimAutoDj { keep } => {
                sqlx::query("DELETE FROM queue_occurrences WHERE object_id IN(SELECT object_id FROM queue_occurrences WHERE provenance_kind='auto-dj' AND traversal_position<COALESCE((SELECT traversal_position FROM queue_occurrences WHERE object_id=?1),0) ORDER BY traversal_position DESC LIMIT -1 OFFSET ?2)").bind(current.as_ref().map(OccurrenceId::as_str)).bind(keep as i64).execute(&mut *transaction).await?;
                compact_order(&mut transaction, "position").await?;
                compact_order(&mut transaction, "traversal_position").await?;
            }
            QueueEdit::SelectOptional(selected) => {
                if selected != current {
                    progress = 0;
                }
                current = selected;
            }
            QueueEdit::Select(id) => {
                if current.as_ref() != Some(&id) {
                    progress = 0;
                }
                current = Some(id);
            }
            QueueEdit::SelectIndex(index) => {
                let selected = occurrence_at(&mut transaction, index).await?;
                if selected != current {
                    progress = 0;
                }
                current = selected;
            }
        }
        if auto_dj_append {
            let removed=sqlx::query("DELETE FROM queue_occurrences WHERE object_id IN(SELECT object_id FROM queue_occurrences WHERE provenance_kind='auto-dj' AND traversal_position<COALESCE((SELECT traversal_position FROM queue_occurrences WHERE object_id=?1),0) ORDER BY traversal_position DESC LIMIT -1 OFFSET 10)").bind(current.as_ref().map(OccurrenceId::as_str)).execute(&mut *transaction).await?.rows_affected();
            if removed > 0 {
                compact_order(&mut transaction, "position").await?;
                compact_order(&mut transaction, "traversal_position").await?;
            }
        }
        if let Some(id) = &current {
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM queue_occurrences WHERE object_id=?1)",
            )
            .bind(id.as_str())
            .fetch_one(&mut *transaction)
            .await?;
            if !exists {
                current = None;
                progress = 0;
            }
        }
        save_state(
            &mut transaction,
            current.as_ref(),
            progress,
            repeat,
            shuffled,
        )
        .await?;
        let mut result = read_window(&mut transaction, current.as_ref(), None).await?;
        result.removed_successors = removed_successors;
        transaction.commit().await?;
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_queue_input(
        &self,
        transaction: &mut Transaction<'_, Sqlite>,
        input: QueueInput,
        target: QueueReorderTarget,
        replacing: bool,
        identity: Option<&str>,
        anchor: usize,
        shuffle_seed: Option<u64>,
        random_start: bool,
        preview: Option<&(dyn Fn(QueueRestore) + Send + Sync)>,
    ) -> LibraryResult<(usize, usize)> {
        if let QueueInput::Groups(inputs) = input {
            let mut target = target;
            let mut first = 0;
            let mut total = 0;
            for (index, input) in inputs.into_iter().enumerate() {
                let (start, count) = Box::pin(self.insert_queue_input(
                    transaction,
                    input,
                    target.clone(),
                    replacing && index == 0,
                    None,
                    0,
                    None,
                    false,
                    preview,
                ))
                .await?;
                if index == 0 {
                    first = start;
                }
                total += count;
                if count > 0
                    && let Some(last) = occurrence_at(transaction, start + count - 1).await?
                {
                    target = QueueReorderTarget::After(last);
                }
            }
            return Ok((first, total));
        }
        let total = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(max(position)+1,0) FROM queue_occurrences",
        )
        .fetch_one(&mut **transaction)
        .await?;
        let (canonical, traversal) = if replacing {
            (0, 0)
        } else {
            target_positions(transaction, &target, total).await?
        };
        if let QueueInput::Items(items) = input {
            if replacing {
                sqlx::query("DELETE FROM queue_occurrences")
                    .execute(&mut **transaction)
                    .await?;
            }
            let count = items.len();
            shift_order(transaction, "position", canonical, count as i64).await?;
            shift_order(transaction, "traversal_position", traversal, count as i64).await?;
            for (i, (item, provenance)) in items.into_iter().enumerate() {
                let row = QueueOccurrence {
                    occurrence: OccurrenceId::new(if let Some(identity) = identity {
                        format!("{identity}:{i}")
                    } else {
                        sqlx::query_scalar::<_, String>("SELECT lower(hex(randomblob(16)))")
                            .fetch_one(&mut **transaction)
                            .await?
                    }),
                    item,
                    provenance,
                    canonical_position: canonical as usize + i,
                };
                persist_occurrence_page(transaction, &[row], traversal as usize + i).await?;
            }
            return Ok((traversal as usize, count));
        }
        sqlx::query("CREATE TEMP TABLE IF NOT EXISTS queue_input(media_uri TEXT NOT NULL,source_rank INTEGER NOT NULL,entry_key INTEGER)").execute(&mut **transaction).await?;
        sqlx::query("DELETE FROM temp.queue_input")
            .execute(&mut **transaction)
            .await?;
        let playlist_entries = matches!(
            &input,
            QueueInput::PlaylistEntries { .. }
                | QueueInput::Collection {
                    collection: QueueCollection::Playlist(_),
                    ..
                }
        );
        let provenance = match input {
            QueueInput::MediaUris { order, provenance } => {
                for (offset, batch) in order.chunks(128).enumerate() {
                    sqlx::query("INSERT INTO temp.queue_input(media_uri,source_rank) SELECT value,key+?2 FROM json_each(?1)").bind(serde_json::to_string(batch)?).bind((offset*128) as i64).execute(&mut **transaction).await?;
                }
                provenance
            }
            QueueInput::Uris {
                order,
                context_id,
                source_start,
            } => {
                for (offset, batch) in order.chunks(128).enumerate() {
                    sqlx::query("INSERT INTO temp.queue_input(media_uri,source_rank) SELECT value,key+?2 FROM json_each(?1)").bind(serde_json::to_string(batch)?).bind((source_start+offset*128) as i64).execute(&mut **transaction).await?;
                }
                QueueProvenance::Context {
                    context_id,
                    source_rank: 0,
                }
            }
            QueueInput::PlaylistEntries { order, context_id } => {
                for (offset, batch) in order.chunks(128).enumerate() {
                    // The explicit entry keys retain duplicate snapshots and occurrence order.
                    sqlx::query(
                        "INSERT INTO temp.queue_input(media_uri,source_rank,entry_key)
                        SELECT entry.media_uri,requested.key+?2 AS source_rank,entry.playlist_entry_key
                        FROM json_each(?1) requested
                        JOIN main.playlist_entries entry ON entry.playlist_entry_key=requested.value
                        WHERE requested.value>=0
                        UNION ALL
                        SELECT entry.media_uri,requested.key+?2 AS source_rank,-entry.playlist_entry_key
                        FROM json_each(?1) requested
                        JOIN catalog.native_playlist_entries entry ON entry.playlist_entry_key=-requested.value
                        WHERE requested.value<0
                        ORDER BY source_rank",
                    )
                    .bind(serde_json::to_string(batch)?)
                    .bind((offset * 128) as i64)
                    .execute(&mut **transaction)
                    .await?;
                }
                QueueProvenance::Context {
                    context_id,
                    source_rank: 0,
                }
            }
            QueueInput::Smart {
                key,
                source,
                folder,
                now,
                context_id,
            } => {
                self.seed_smart_queue(transaction, key, source, folder, now)
                    .await?;
                QueueProvenance::Context {
                    context_id,
                    source_rank: 0,
                }
            }
            QueueInput::Collection {
                collection,
                folder,
                context_id,
            } => {
                crate::collections::seed_collection_queue(transaction, collection, folder).await?;
                QueueProvenance::Context {
                    context_id,
                    source_rank: 0,
                }
            }
            QueueInput::Items(_) => unreachable!(),
            QueueInput::Groups(_) => unreachable!(),
        };
        let count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM temp.queue_input")
            .fetch_one(&mut **transaction)
            .await?;
        if replacing
            && count > 0
            && let Some(identity) = identity
            && let Some(preview) = preview
        {
            let anchor = if random_start {
                shuffle_seed.unwrap_or(0) as usize % count as usize
            } else {
                anchor.min(count as usize - 1)
            };
            let positions = preview_positions(count as usize, anchor, shuffle_seed);
            let requested = serde_json::to_string(&positions)?;
            let inputs = sqlx::query_as::<_, (String, i64, Option<i64>)>("SELECT input.media_uri,input.source_rank,input.entry_key FROM json_each(?1) requested JOIN temp.queue_input input ON input.rowid=requested.value+1 ORDER BY requested.key")
                .bind(requested).fetch_all(&mut **transaction).await?;
            let rows = if playlist_entries {
                let keys = inputs
                    .iter()
                    .filter_map(|(_, _, key)| key.map(crate::PlaylistEntryKey::from_raw))
                    .collect::<Vec<_>>();
                self.playlist_entry_rows(&keys, &ReadCancellation::new())
                    .await?
                    .into_iter()
                    .map(QueueItem::from)
                    .collect()
            } else {
                let uris = inputs
                    .iter()
                    .map(|(uri, _, _)| uri.clone())
                    .collect::<Vec<_>>();
                self.queue_items_for_uris(&uris, &ReadCancellation::new())
                    .await?
            };
            let rows = rows
                .into_iter()
                .zip(inputs)
                .map(|(item, (_, rank, _))| {
                    let provenance = match &provenance {
                        QueueProvenance::Context { context_id, .. } => QueueProvenance::Context {
                            context_id: context_id.clone(),
                            source_rank: rank as usize,
                        },
                        provenance => provenance.clone(),
                    };
                    (item, provenance)
                })
                .collect();
            preview(preview_window(
                rows,
                count as usize,
                anchor,
                shuffle_seed,
                identity,
            ));
        }
        sqlx::query(
            "CREATE TEMP TABLE IF NOT EXISTS queue_next AS SELECT * FROM queue_occurrences WHERE 0",
        )
        .execute(&mut **transaction)
        .await?;
        sqlx::query("DELETE FROM temp.queue_next")
            .execute(&mut **transaction)
            .await?;
        let (kind, context, rank) = provenance.columns();
        sqlx::query("INSERT INTO temp.queue_next(object_id,media_uri,position,traversal_position,provenance_kind,provenance_context_id,provenance_source_rank,title,artist,album,album_display_artist,duration_millis,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,musicbrainz_album_id,musicbrainz_release_group_id,primary_artist_musicbrainz_id)
          SELECT CASE WHEN ?6 IS NULL THEN lower(hex(randomblob(16))) ELSE ?6||':'||(row_number() OVER(ORDER BY input.rowid)-1) END,input.media_uri,row_number() OVER(ORDER BY input.rowid)-1+?1,row_number() OVER(ORDER BY input.rowid)-1+?2,?3,?4,CASE WHEN ?5 THEN input.source_rank END,
          COALESCE(track.title,entry.title,queued.title,listen.track_title,input.media_uri),COALESCE(track.display_artist,entry.artist,queued.artist,listen.artist_name,''),COALESCE(track.display_album,entry.album,queued.album,listen.album_title,''),COALESCE(album.display_artist,entry.album_display_artist,queued.album_display_artist),COALESCE(track.duration_millis,entry.duration_millis,queued.duration_millis,listen.duration_millis,0),COALESCE(track.disc_number,entry.disc_number,queued.disc_number,listen.disc_number),COALESCE(track.track_number,entry.track_number,queued.track_number,listen.track_number),COALESCE(track.year,entry.year,queued.year,listen.year),COALESCE(track.release_date,entry.release_date,queued.release_date,listen.release_date),COALESCE(track.source_format,entry.source_format,queued.source_format,listen.source_format),COALESCE(track.musicbrainz_recording_id,entry.musicbrainz_recording_id,queued.musicbrainz_recording_id,listen.musicbrainz_recording_id),COALESCE(track.musicbrainz_release_track_id,entry.musicbrainz_release_track_id,queued.musicbrainz_release_track_id,listen.musicbrainz_release_track_id),COALESCE(album.musicbrainz_release_id,queued.musicbrainz_album_id),COALESCE(album.musicbrainz_release_group_id,queued.musicbrainz_release_group_id),
          (SELECT artist.musicbrainz_artist_id FROM track_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1)
          FROM temp.queue_input input LEFT JOIN tracks track USING(media_uri) LEFT JOIN albums album USING(album_key) LEFT JOIN playlist_entries entry ON entry.playlist_entry_key=COALESCE(input.entry_key,(SELECT playlist_entry_key FROM playlist_entries WHERE media_uri=input.media_uri ORDER BY title IS NULL,snapshot_at DESC,playlist_entry_key DESC LIMIT 1)) LEFT JOIN queue_occurrences queued ON queued.queue_occurrence_key=(SELECT queue_occurrence_key FROM queue_occurrences WHERE media_uri=input.media_uri ORDER BY snapshot_at DESC,queue_occurrence_key DESC LIMIT 1) LEFT JOIN listens listen ON listen.listen_key=(SELECT listen_key FROM listens WHERE media_uri=input.media_uri ORDER BY started_at DESC,listen_key DESC LIMIT 1) ORDER BY input.rowid")
          .bind(canonical).bind(traversal).bind(kind).bind(context).bind(rank.is_some()).bind(identity).execute(&mut **transaction).await?;
        if replacing {
            sqlx::query("DELETE FROM queue_occurrences")
                .execute(&mut **transaction)
                .await?;
        }
        shift_order(transaction, "position", canonical, count).await?;
        shift_order(transaction, "traversal_position", traversal, count).await?;
        sqlx::query("INSERT INTO queue_occurrences(object_id,media_uri,position,traversal_position,provenance_kind,provenance_context_id,provenance_source_rank,title,artist,album,album_display_artist,duration_millis,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,musicbrainz_album_id,musicbrainz_release_group_id,primary_artist_musicbrainz_id) SELECT object_id,media_uri,position,traversal_position,provenance_kind,provenance_context_id,provenance_source_rank,title,artist,album,album_display_artist,duration_millis,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,musicbrainz_album_id,musicbrainz_release_group_id,primary_artist_musicbrainz_id FROM temp.queue_next").execute(&mut **transaction).await?;
        sqlx::query("DELETE FROM temp.queue_next")
            .execute(&mut **transaction)
            .await?;
        sqlx::query("DELETE FROM temp.queue_input")
            .execute(&mut **transaction)
            .await?;
        Ok((traversal as usize, count as usize))
    }
}

async fn occurrence_at(
    transaction: &mut Transaction<'_, Sqlite>,
    index: usize,
) -> LibraryResult<Option<OccurrenceId>> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT object_id FROM queue_occurrences WHERE traversal_position=?1",
    )
    .bind(index as i64)
    .fetch_optional(&mut **transaction)
    .await?
    .map(OccurrenceId::new))
}

const SHUFFLE_MODULUS: u64 = 2_147_483_647;
const SHUFFLE_MULTIPLIER: u64 = 1_103_515_245;

// Sum floor((a*i+b)/m), i in 0..n, by Euclidean reduction. This lets a
// bounded preview select the original SQL shuffle ranks without sorting n IDs.
fn floor_sum(mut n: u128, mut m: u128, mut a: u128, mut b: u128) -> u128 {
    let mut sum = 0;
    loop {
        if a >= m {
            sum += n * (n - 1) / 2 * (a / m);
            a %= m;
        }
        if b >= m {
            sum += n * (b / m);
            b %= m;
        }
        let top = a * n + b;
        if top < m {
            return sum;
        }
        n = top / m;
        b = top % m;
        std::mem::swap(&mut a, &mut m);
    }
}

fn shuffle_count_below(total: usize, seed: u64, upper: u64) -> usize {
    let n = total as u128;
    let m = SHUFFLE_MODULUS as u128;
    let a = SHUFFLE_MULTIPLIER as u128;
    let b = a + (seed % SHUFFLE_MODULUS) as u128;
    (n - (floor_sum(n, m, a, b + m - upper as u128) - floor_sum(n, m, a, b))) as usize
}

fn shuffled_canonical_position(total: usize, anchor: usize, seed: u64, traversal: usize) -> usize {
    if traversal == 0 {
        return anchor;
    }
    let seed = seed % SHUFFLE_MODULUS;
    let anchor_hash = (((anchor as u128 + 1) * SHUFFLE_MULTIPLIER as u128 + seed as u128)
        % SHUFFLE_MODULUS as u128) as u64;
    let (mut lower, mut upper) = (0, SHUFFLE_MODULUS - 1);
    while lower < upper {
        let middle = (lower + upper) / 2;
        let count =
            shuffle_count_below(total, seed, middle + 1) - usize::from(anchor_hash <= middle);
        if count >= traversal {
            upper = middle;
        } else {
            lower = middle + 1;
        }
    }
    let before = shuffle_count_below(total, seed, lower) - usize::from(anchor_hash < lower);
    let inverse = 2_104_886_853_u128;
    let first = ((((lower + SHUFFLE_MODULUS - seed) % SHUFFLE_MODULUS) as u128 * inverse)
        % SHUFFLE_MODULUS as u128
        + SHUFFLE_MODULUS as u128
        - 1)
        % SHUFFLE_MODULUS as u128;
    let mut position = first as usize + (traversal - 1 - before) * SHUFFLE_MODULUS as usize;
    if anchor_hash == lower && position >= anchor {
        position += SHUFFLE_MODULUS as usize;
    }
    position
}

fn preview_positions(total: usize, anchor: usize, shuffled: Option<u64>) -> Vec<usize> {
    let selected = if shuffled.is_some() { 0 } else { anchor };
    let start = selected.saturating_sub(48).min(total.saturating_sub(96));
    let end = (start + 96).min(total);
    let mut positions = (start..end).collect::<Vec<_>>();
    if total > 96 {
        if start == 0 {
            positions.push(total - 1);
        }
        if end == total {
            positions.push(0);
        }
    }
    if let Some(seed) = shuffled {
        for position in &mut positions {
            *position = shuffled_canonical_position(total, anchor, seed, *position);
        }
    }
    positions
}

fn preview_window(
    rows: Vec<(QueueItem, QueueProvenance)>,
    total: usize,
    anchor: usize,
    shuffled: Option<u64>,
    identity: &str,
) -> QueueRestore {
    let selected = if shuffled.is_some() { 0 } else { anchor };
    let window_start = selected.saturating_sub(48).min(total.saturating_sub(96));
    let mut occurrences = rows
        .into_iter()
        .zip(preview_positions(total, anchor, shuffled))
        .map(|((item, provenance), canonical_position)| QueueOccurrence {
            occurrence: OccurrenceId::new(format!("{identity}:{canonical_position}")),
            item,
            provenance,
            canonical_position,
        })
        .collect::<Vec<_>>();
    let wrap_next = (total > 96 && window_start + 96 >= total)
        .then(|| occurrences.pop())
        .flatten();
    let wrap_previous = (total > 96 && window_start == 0)
        .then(|| occurrences.pop())
        .flatten();
    QueueRestore {
        removed_successors: Vec::new(),
        total,
        window_start,
        current_index: Some(selected),
        wrap_previous,
        wrap_next,
        occurrences,
        current_occurrence: Some(OccurrenceId::new(format!("{identity}:{anchor}"))),
        progress_millis: 0,
        repeat_mode: QueueRepeatMode::Off,
        shuffled: shuffled.is_some(),
    }
}
async fn save_state(
    transaction: &mut sqlx::SqliteConnection,
    current: Option<&OccurrenceId>,
    progress: i64,
    repeat: QueueRepeatMode,
    shuffled: bool,
) -> LibraryResult<()> {
    sqlx::query("INSERT INTO queue_state(singleton,current_occurrence_id,progress_millis,repeat_mode,shuffled) VALUES(1,?1,?2,?3,?4) ON CONFLICT(singleton) DO UPDATE SET current_occurrence_id=excluded.current_occurrence_id,progress_millis=excluded.progress_millis,repeat_mode=excluded.repeat_mode,shuffled=excluded.shuffled").bind(current.map(OccurrenceId::as_str)).bind(progress.max(0)).bind(repeat.as_str()).bind(shuffled).execute(&mut *transaction).await?;
    Ok(())
}
async fn target_positions(
    transaction: &mut Transaction<'_, Sqlite>,
    target: &QueueReorderTarget,
    total: i64,
) -> LibraryResult<(i64, i64)> {
    let (id, offset) = match target {
        QueueReorderTarget::Before(id) => (id, 0),
        QueueReorderTarget::After(id) => (id, 1),
        QueueReorderTarget::End => return Ok((total, total)),
    };
    Ok(sqlx::query_as::<_, (i64, i64)>(
        "SELECT position+?2,traversal_position+?2 FROM queue_occurrences WHERE object_id=?1",
    )
    .bind(id.as_str())
    .bind(offset)
    .fetch_optional(&mut **transaction)
    .await?
    .unwrap_or((total, total)))
}
// Lift affected ranks above the live range before assigning their final values, avoiding
// uniqueness collisions regardless of SQLite's UPDATE row visitation order.
// SQL fragments below are private column names and numeric ranks only; no user text is interpolated.
async fn shift_order(
    transaction: &mut Transaction<'_, Sqlite>,
    column: &str,
    start: i64,
    delta: i64,
) -> LibraryResult<()> {
    if delta == 0 {
        return Ok(());
    }
    let high = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
        "SELECT COALESCE(max({column})+1,0)+?1 FROM queue_occurrences"
    )))
    .bind(delta.abs())
    .fetch_one(&mut **transaction)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE queue_occurrences SET {column}={column}+?1 WHERE {column}>=?2"
    )))
    .bind(high)
    .bind(start)
    .execute(&mut **transaction)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE queue_occurrences SET {column}={column}-?1+?2 WHERE {column}>=?1"
    )))
    .bind(high)
    .bind(delta)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}
async fn remap_order(
    transaction: &mut Transaction<'_, Sqlite>,
    column: &str,
    ordering: &str,
    start: i64,
) -> LibraryResult<()> {
    sqlx::query("CREATE TEMP TABLE IF NOT EXISTS queue_ranks(object_id TEXT PRIMARY KEY,rank INTEGER NOT NULL)").execute(&mut **transaction).await?;
    sqlx::query("DELETE FROM temp.queue_ranks")
        .execute(&mut **transaction)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO temp.queue_ranks SELECT object_id,row_number() OVER(ORDER BY {ordering})-1+?1 FROM queue_occurrences WHERE {column}>=?1"))).bind(start).execute(&mut **transaction).await?;
    let high = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
        "SELECT COALESCE(max({column})+1,0) FROM queue_occurrences"
    )))
    .fetch_one(&mut **transaction)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE queue_occurrences SET {column}={column}+?1 WHERE {column}>=?2"
    )))
    .bind(high)
    .bind(start)
    .execute(&mut **transaction)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE queue_occurrences SET {column}=(SELECT rank FROM temp.queue_ranks WHERE object_id=queue_occurrences.object_id) WHERE {column}>=?1"))).bind(high).execute(&mut **transaction).await?;
    sqlx::query("DELETE FROM temp.queue_ranks")
        .execute(&mut **transaction)
        .await?;
    Ok(())
}
async fn compact_order(
    transaction: &mut Transaction<'_, Sqlite>,
    column: &str,
) -> LibraryResult<()> {
    remap_order(transaction, column, column, 0).await
}
async fn shuffle_order(
    transaction: &mut Transaction<'_, Sqlite>,
    enabled: bool,
    seed: u64,
    current: Option<&OccurrenceId>,
) -> LibraryResult<()> {
    if !enabled {
        return remap_order(transaction, "traversal_position", "position", 0).await;
    }
    let current_key = sqlx::query_scalar::<_, i64>(
        "SELECT queue_occurrence_key FROM queue_occurrences WHERE object_id=?1",
    )
    .bind(current.map(OccurrenceId::as_str))
    .fetch_optional(&mut **transaction)
    .await?
    .unwrap_or(-1);
    remap_order(transaction, "traversal_position",
        &format!("queue_occurrence_key!={current_key}, ((queue_occurrence_key*{SHUFFLE_MULTIPLIER}+{})%{SHUFFLE_MODULUS}),position", seed % SHUFFLE_MODULUS), 0).await
}
async fn move_occurrences(
    transaction: &mut Transaction<'_, Sqlite>,
    ids: &[OccurrenceId],
    target: QueueReorderTarget,
    shuffled: bool,
) -> LibraryResult<()> {
    if ids.is_empty()
        || matches!(&target,QueueReorderTarget::Before(other)|QueueReorderTarget::After(other) if ids.contains(other))
    {
        return Ok(());
    }
    sqlx::query("CREATE TEMP TABLE IF NOT EXISTS queue_moves(object_id TEXT PRIMARY KEY,ordinal INTEGER NOT NULL)").execute(&mut **transaction).await?;
    sqlx::query("DELETE FROM temp.queue_moves")
        .execute(&mut **transaction)
        .await?;
    for (batch, ids) in ids.chunks(128).enumerate() {
        sqlx::query(
            "INSERT OR IGNORE INTO temp.queue_moves SELECT value,key+?2 FROM json_each(?1)",
        )
        .bind(serde_json::to_string(ids)?)
        .bind((batch * 128) as i64)
        .execute(&mut **transaction)
        .await?;
    }
    sqlx::query("CREATE TEMP TABLE IF NOT EXISTS queue_ranks(object_id TEXT PRIMARY KEY,rank INTEGER NOT NULL)").execute(&mut **transaction).await?;
    let total =
        sqlx::query_scalar::<_, i64>("SELECT COALESCE(max(position)+1,0) FROM queue_occurrences")
            .fetch_one(&mut **transaction)
            .await?;
    let destinations = target_positions(transaction, &target, total).await?;
    for (column, destination) in [
        ("position", destinations.0),
        ("traversal_position", destinations.1),
    ] {
        if shuffled && column == "traversal_position" {
            continue;
        }
        let bounds = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(sqlx::AssertSqlSafe(format!(
            "SELECT min({column}),max({column}) FROM temp.queue_moves JOIN queue_occurrences USING(object_id)"
        )))
        .fetch_one(&mut **transaction)
        .await?;
        let (Some(first), Some(last)) = bounds else {
            continue;
        };
        let low = first.min(destination);
        let high = last.max(destination - 1);
        // Remap only the crossed interval once. Selected occurrences form one ordered block,
        // even when the selection came from a filtered page with gaps between its rows.
        sqlx::query("DELETE FROM temp.queue_ranks")
            .execute(&mut **transaction)
            .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO temp.queue_ranks
             SELECT occurrence.object_id,row_number() OVER(
               ORDER BY CASE WHEN moved.object_id IS NOT NULL THEN ?1 ELSE occurrence.{column} END,
                        moved.object_id IS NULL,COALESCE(moved.ordinal,occurrence.{column}))-1+?2
             FROM queue_occurrences occurrence LEFT JOIN temp.queue_moves moved USING(object_id)
             WHERE occurrence.{column} BETWEEN ?2 AND ?3"
        )))
        .bind(destination)
        .bind(low)
        .bind(high)
        .execute(&mut **transaction)
        .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE queue_occurrences SET {column}={column}+?1 WHERE {column} BETWEEN ?2 AND ?3"
        )))
        .bind(total + 1)
        .bind(low)
        .bind(high)
        .execute(&mut **transaction)
        .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE queue_occurrences SET {column}=(SELECT rank FROM temp.queue_ranks WHERE object_id=queue_occurrences.object_id) WHERE {column} BETWEEN ?1 AND ?2"
        )))
        .bind(low + total + 1)
        .bind(high + total + 1)
        .execute(&mut **transaction)
        .await?;
    }
    sqlx::query("DELETE FROM temp.queue_ranks")
        .execute(&mut **transaction)
        .await?;
    sqlx::query("DELETE FROM temp.queue_moves")
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct QueueArchiveState {
    version: u32,
    current_occurrence: Option<OccurrenceId>,
    progress_millis: i64,
    repeat_mode: QueueRepeatMode,
    shuffled: bool,
}

impl Database {
    pub async fn export_queue_jsonl(&self, output: impl std::io::Write) -> LibraryResult<()> {
        let mut connection = self.acquire_reader().await?;
        let mut transaction = connection.begin().await?;
        export_queue_jsonl_on(&mut transaction, output).await?;
        transaction.commit().await?;
        Ok(())
    }
    pub async fn import_queue_jsonl(&self, input: impl std::io::BufRead) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        import_queue_jsonl_on(&mut transaction, input).await?;
        transaction.commit().await?;
        Ok(())
    }
}
pub(crate) async fn export_queue_jsonl_on(
    connection: &mut sqlx::SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<()> {
    let state=sqlx::query_as::<_,(Option<String>,i64,String,bool)>("SELECT current_occurrence_id,progress_millis,repeat_mode,shuffled FROM queue_state WHERE singleton=1").fetch_optional(&mut *connection).await?.unwrap_or((None,0,"none".into(),false));
    serde_json::to_writer(
        &mut output,
        &QueueArchiveState {
            version: 1,
            current_occurrence: state.0.map(OccurrenceId::new),
            progress_millis: state.1,
            repeat_mode: QueueRepeatMode::parse(&state.2)?,
            shuffled: state.3,
        },
    )?;
    output
        .write_all(b"\n")
        .map_err(|error| LibraryError::InvalidRequest(error.to_string()))?;
    let mut start = 0;
    loop {
        let page = read_occurrences(connection, start, QUEUE_CONTEXT_LIMIT).await?;
        if page.is_empty() {
            break;
        }
        start += page.len();
        for occurrence in page {
            serde_json::to_writer(&mut output, &occurrence)?;
            output
                .write_all(b"\n")
                .map_err(|error| LibraryError::InvalidRequest(error.to_string()))?;
        }
    }
    Ok(())
}
pub(crate) async fn import_queue_jsonl_on(
    connection: &mut sqlx::SqliteConnection,
    mut input: impl std::io::BufRead,
) -> LibraryResult<()> {
    let mut line = String::new();
    input
        .read_line(&mut line)
        .map_err(|error| LibraryError::InvalidRequest(error.to_string()))?;
    let state: QueueArchiveState = serde_json::from_str(&line)?;
    if state.version != 1 {
        return Err(LibraryError::InvalidRequest(
            "unsupported Queue export version".into(),
        ));
    }
    sqlx::query("DELETE FROM queue_occurrences")
        .execute(&mut *connection)
        .await?;
    let mut position = 0;
    loop {
        line.clear();
        if input
            .read_line(&mut line)
            .map_err(|error| LibraryError::InvalidRequest(error.to_string()))?
            == 0
        {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let occurrence: QueueOccurrence = serde_json::from_str(&line)?;
        persist_occurrence_page(connection, &[occurrence], position).await?;
        position += 1;
    }
    let valid = sqlx::query_scalar::<_, bool>(
        "SELECT count(*)=?1 AND COALESCE(max(position),-1)=?1-1 FROM queue_occurrences",
    )
    .bind(position as i64)
    .fetch_one(&mut *connection)
    .await?;
    if !valid {
        return Err(LibraryError::InvalidRequest(
            "invalid Queue occurrence order".into(),
        ));
    }
    require_occurrence(connection, state.current_occurrence.as_ref()).await?;
    save_state(
        connection,
        state.current_occurrence.as_ref(),
        state.progress_millis,
        state.repeat_mode,
        state.shuffled,
    )
    .await?;
    Ok(())
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

async fn capture_order_window(
    transaction: &mut Transaction<'_, Sqlite>,
    current: Option<&OccurrenceId>,
) -> LibraryResult<Vec<(String, i64)>> {
    let total = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(max(traversal_position)+1,0) FROM queue_occurrences",
    )
    .fetch_one(&mut **transaction)
    .await?;
    let anchor = sqlx::query_scalar::<_, i64>(
        "SELECT traversal_position FROM queue_occurrences WHERE object_id=?1",
    )
    .bind(current.map(OccurrenceId::as_str))
    .fetch_optional(&mut **transaction)
    .await?
    .unwrap_or(0);
    let start = (anchor - 50).max(0).min((total - 96).max(0));
    let mut rows=sqlx::query_as::<_,(String,i64)>("SELECT object_id,traversal_position FROM queue_occurrences WHERE traversal_position>=?1 ORDER BY traversal_position LIMIT 96").bind(start).fetch_all(&mut **transaction).await?;
    let wrap = if total > 96 && start == 0 {
        Some(total - 1)
    } else if total > 96 && start + 96 >= total {
        Some(0)
    } else {
        None
    };
    if let Some(rank) = wrap {
        if let Some(row)=sqlx::query_as::<_,(String,i64)>("SELECT object_id,traversal_position FROM queue_occurrences WHERE traversal_position=?1").bind(rank).fetch_optional(&mut **transaction).await? {rows.push(row);}
    }
    Ok(rows)
}
async fn removed_window_successors(
    transaction: &mut Transaction<'_, Sqlite>,
    before: &[(String, i64)],
    repeat: QueueRepeatMode,
) -> LibraryResult<Vec<(OccurrenceId, Option<OccurrenceId>)>> {
    let mut removed = Vec::new();
    for (id, rank) in before {
        if sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM queue_occurrences WHERE object_id=?1)",
        )
        .bind(id)
        .fetch_one(&mut **transaction)
        .await?
        {
            continue;
        }
        let mut successor=sqlx::query_scalar::<_,String>("SELECT object_id FROM queue_occurrences WHERE traversal_position>?1 ORDER BY traversal_position LIMIT 1").bind(rank).fetch_optional(&mut **transaction).await?;
        if successor.is_none() && repeat == QueueRepeatMode::All {
            successor = sqlx::query_scalar::<_, String>(
                "SELECT object_id FROM queue_occurrences ORDER BY traversal_position LIMIT 1",
            )
            .fetch_optional(&mut **transaction)
            .await?;
        }
        removed.push((
            OccurrenceId::new(id.clone()),
            successor.map(OccurrenceId::new),
        ));
    }
    Ok(removed)
}
