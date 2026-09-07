//! Queue source reads, metadata, and saved playback state. Playback owns edits.

use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use sqlx::sqlite::SqliteRow;
use sqlx::{Connection, FromRow, Row, Sqlite};

use crate::{Database, LibraryError, LibraryResult, ReadCancellation, SourceId, SourceKey};

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
pub struct OccurrenceId(Arc<str>);

impl OccurrenceId {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        assert!(!value.is_empty(), "OccurrenceId cannot be empty");
        Self(value.into())
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
    #[serde(default)]
    pub source_index: Option<usize>,
    #[serde(default)]
    pub playlist_entry_id: Option<String>,
    pub provenance: QueueProvenance,
}

impl Deref for QueueOccurrence {
    type Target = QueueItem;

    fn deref(&self) -> &Self::Target {
        &self.item
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum QueueInput {
    Choices(Arc<[Option<QueueChoice>]>),
    Query {
        query: QueueQuery,
        folder: Option<crate::FolderKey>,
        filter: String,
        sort: crate::TrackSort,
        descending: bool,
        context_id: Arc<str>,
        anchor_uri: Option<String>,
    },
    PlaylistQuery {
        key: crate::PlaylistKey,
        folder: Option<crate::FolderKey>,
        filter: String,
        sort: crate::PlaylistEntrySort,
        descending: bool,
        context_id: Arc<str>,
        anchor_entry: Option<crate::PlaylistEntryKey>,
        anchor_uri: Option<String>,
    },
    Source {
        reference: QueueSource,
        context_id: Arc<str>,
    },
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

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueChoice {
    #[serde(default)]
    pub origin: Option<(usize, usize)>,
    pub media_uri: String,
    pub fallback: Option<QueueItem>,
    pub provenance: QueueProvenance,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum QueueQuery {
    Tracks {
        source: SourceKey,
        favorites_only: bool,
        recursive: bool,
    },
    Collection {
        collection: QueueCollection,
        favorites_only: bool,
    },
    Smart {
        key: crate::SmartPlaylistKey,
        source: Option<SourceKey>,
        now: i64,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum QueueScope {
    Tracks {
        source: SourceId,
        folder: Option<String>,
        favorites_only: bool,
        recursive: bool,
    },
    Collection {
        reference: crate::CollectionSourceReference,
        favorites_only: bool,
    },
    Playlist {
        reference: crate::CollectionSourceReference,
        sort: crate::PlaylistEntrySort,
        anchor_entry: Option<String>,
    },
    Smart {
        reference: crate::SmartSourceReference,
        now: i64,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueSource {
    pub scope: QueueScope,
    pub filter: String,
    pub sort: crate::TrackSort,
    pub descending: bool,
    pub anchor_uri: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct LegacyQueueInstruction {
    pub input: QueueInput,
    /// Displaced source entries belong only to this pass, unlike user additions.
    pub repeat: bool,
    #[serde(default)]
    pub seed: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct LegacyQueueCursor {
    pub source: usize,
    pub after: Option<String>,
    pub offset: usize,
    pub seed: Option<u64>,
    pub anchor: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueEntry {
    pub occurrence: OccurrenceId,
    pub media_uri: Arc<str>,
    pub playlist_entry_id: Option<Arc<str>>,
    pub provenance: QueueProvenance,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueRestore {
    pub entries: Arc<[QueueEntry]>,
    pub order: Arc<[u32]>,
    #[serde(skip)]
    pub occurrences: Vec<Arc<QueueOccurrence>>,
    pub current_index: Option<usize>,
    pub progress_millis: i64,
    pub repeat_mode: QueueRepeatMode,
    pub shuffled: bool,
    pub next_id: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum QueueReadRequest {
    Capture {
        input: Box<QueueInput>,
        anchor_index: usize,
        random_start: Option<u64>,
    },
    Hydrate {
        entries: Vec<QueueEntry>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueReadPage {
    pub entries: Vec<QueueEntry>,
    pub occurrences: Vec<Arc<QueueOccurrence>>,
    pub current_index: usize,
}

impl QueueRestore {
    pub fn current(&self) -> Option<&OccurrenceId> {
        self.current_index
            .and_then(|i| self.order.get(i))
            .and_then(|i| self.entries.get(*i as usize))
            .map(|item| &item.occurrence)
    }
}

impl QueueInput {
    pub fn clear_anchor(&mut self) {
        match self {
            Self::Source { reference, .. } => {
                reference.anchor_uri = None;
                if let QueueScope::Playlist { anchor_entry, .. } = &mut reference.scope {
                    *anchor_entry = None;
                }
            }
            Self::Query { anchor_uri, .. } => *anchor_uri = None,
            Self::PlaylistQuery {
                anchor_uri,
                anchor_entry,
                ..
            } => {
                *anchor_uri = None;
                *anchor_entry = None;
            }
            Self::Groups(inputs) => inputs.iter_mut().for_each(Self::clear_anchor),
            _ => {}
        }
    }
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
#[derive(FromRow)]
struct QueueOccurrenceRow {
    source_index: Option<i64>,
    playlist_entry_id: Option<String>,
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
    pub media_uri: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: Option<i64>,
    pub duration_millis: i64,
    pub artwork_binding: Option<Vec<u8>>,
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

    pub async fn prepared_queue_page(
        &self,
        window: &[Arc<QueueOccurrence>],
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
        query.push_values(
            window.iter().enumerate(),
            |mut row, (ordinal, occurrence)| {
                row.push_bind(ordinal as i64)
                    .push_bind(&occurrence.media_uri);
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
                    position: ordinal,
                    favorite,
                    primary_artist_media_uri,
                    media_uri: occurrence.media_uri.clone(),
                    title: occurrence.title.clone(),
                    artist: occurrence.artist.clone(),
                    album: occurrence.album.clone(),
                    year: occurrence.year,
                    duration_millis: occurrence.duration_millis,
                    artwork_binding: occurrence.artwork_binding.clone(),
                }
            })
            .collect::<Vec<_>>();
        rows.sort_unstable_by_key(|row| row.position);
        Ok(rows)
    }
}

impl Database {
    pub async fn read_queue(&self, request: QueueReadRequest) -> LibraryResult<QueueReadPage> {
        match request {
            QueueReadRequest::Capture {
                input,
                anchor_index,
                random_start,
            } => {
                let mut entries = Vec::new();
                let mut anchor = None;
                let mut reader = self.acquire_reader().await?;
                let mut transaction = reader.begin().await?;
                let namespace: String = sqlx::query_scalar("SELECT lower(hex(randomblob(16)))")
                    .fetch_one(&mut *transaction)
                    .await?;
                capture_input(
                    Some(self),
                    &mut transaction,
                    *input,
                    &namespace,
                    &mut entries,
                    &mut anchor,
                )
                .await?;
                transaction.commit().await?;
                drop(reader);
                let current_index = random_start
                    .filter(|_| !entries.is_empty())
                    .map(|seed| seed as usize % entries.len())
                    .unwrap_or_else(|| {
                        anchor
                            .unwrap_or(anchor_index)
                            .min(entries.len().saturating_sub(1))
                    });
                let occurrences = self
                    .hydrate_queue_entries(
                        &entries[current_index..entries.len().min(current_index + 1)],
                    )
                    .await?;
                Ok(QueueReadPage {
                    entries,
                    occurrences,
                    current_index,
                })
            }
            QueueReadRequest::Hydrate { entries } => {
                let occurrences = self.hydrate_queue_entries(&entries).await?;
                Ok(QueueReadPage {
                    entries,
                    occurrences,
                    current_index: 0,
                })
            }
        }
    }

    async fn hydrate_queue_entries(
        &self,
        entries: &[QueueEntry],
    ) -> LibraryResult<Vec<Arc<QueueOccurrence>>> {
        if entries.len() > QUEUE_CONTEXT_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "Queue metadata window exceeds 100".into(),
            ));
        }
        let mut reader = self.acquire_reader().await?;
        let ids = entries
            .iter()
            .map(|entry| entry.occurrence.as_str())
            .collect::<Vec<_>>();
        let sql = OCCURRENCE_SELECT.to_string()
            + " WHERE occurrence.object_id IN(SELECT value FROM json_each(?1))";
        let mut saved = sqlx::query_as::<_, QueueOccurrenceRow>(sqlx::AssertSqlSafe(sql))
            .bind(serde_json::to_string(&ids)?)
            .fetch_all(&mut *reader)
            .await?
            .into_iter()
            .map(|row| (row.object_id, row.item))
            .collect::<std::collections::HashMap<_, _>>();
        let mut transaction = reader.begin().await?;
        let identities = entries
            .iter()
            .map(|entry| entry.playlist_entry_id.as_deref())
            .collect::<Vec<_>>();
        let keys=sqlx::query_as::<_,(i64,crate::PlaylistEntryKey)>(
            "SELECT requested.key,entry.playlist_entry_key FROM json_each(?1) requested
             JOIN playlist_entries entry ON entry.object_id=json_extract(requested.value,'$[2]')
             JOIN playlists playlist ON playlist.playlist_key=entry.playlist_key AND playlist.object_id=json_extract(requested.value,'$[1]')
             LEFT JOIN source_ids source USING(source_key)
             WHERE source.object_id IS json_extract(requested.value,'$[0]') ORDER BY requested.key")
            .bind(serde_json::to_string(&identities)?).fetch_all(&mut *transaction).await?;
        let playlist_keys = keys.iter().map(|(_, key)| *key).collect::<Vec<_>>();
        let mut playlist_items = keys
            .iter()
            .zip(
                crate::playlists::load_playlist_entry_rows(&mut transaction, &playlist_keys)
                    .await?,
            )
            .map(|((index, _), row)| (*index as usize, QueueItem::from(row)))
            .collect::<std::collections::HashMap<_, _>>();
        transaction.commit().await?;
        drop(reader);
        let missing = entries
            .iter()
            .enumerate()
            .filter(|(index, entry)| {
                !saved.contains_key(entry.occurrence.as_str())
                    && !playlist_items.contains_key(index)
            })
            .map(|(index, entry)| (index, entry.media_uri.to_string()))
            .collect::<Vec<_>>();
        let uris = missing
            .iter()
            .map(|(_, uri)| uri.clone())
            .collect::<Vec<_>>();
        let mut hydrated = missing
            .into_iter()
            .zip(
                self.queue_items_for_uris(&uris, &ReadCancellation::new())
                    .await?,
            )
            .map(|((index, _), item)| (index, item))
            .collect::<std::collections::HashMap<_, _>>();
        let mut result = Vec::with_capacity(entries.len());
        let mut snapshots = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let existing = saved.remove(entry.occurrence.as_str());
            let already_saved = existing.is_some();
            let item = existing
                .or_else(|| playlist_items.remove(&index))
                .or_else(|| hydrated.remove(&index))
                .unwrap();
            let occurrence = QueueOccurrence {
                occurrence: entry.occurrence.clone(),
                item,
                canonical_position: index,
                source_index: None,
                playlist_entry_id: entry.playlist_entry_id.as_ref().map(ToString::to_string),
                provenance: entry.provenance.clone(),
            };
            if !already_saved {
                snapshots.push(occurrence.clone());
            }
            result.push(Arc::new(occurrence));
        }
        if !snapshots.is_empty() {
            let mut writer = self.writer().await?;
            let mut transaction = writer
                .as_mut()
                .ok_or(LibraryError::WriterUnavailable)?
                .begin()
                .await?;
            persist_occurrence_page(&mut transaction, &snapshots, 0).await?;
            transaction.commit().await?;
        }
        Ok(result)
    }
    pub async fn queue_item_for_occurrence(
        &self,
        occurrence: &OccurrenceId,
    ) -> LibraryResult<Option<QueueItem>> {
        let mut connection = self.acquire_reader().await?;
        Ok(read_occurrence(&mut connection, occurrence)
            .await?
            .map(|row| row.item))
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

    pub async fn save_queue_order(&self, state: &QueueRestore) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        sqlx::query("INSERT INTO queue_order(singleton,state) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET state=excluded.state")
            .bind(serde_json::to_string(&state.order)?).execute(&mut *transaction).await?;
        save_settings(
            &mut transaction,
            state.current(),
            state.progress_millis,
            state.repeat_mode,
            state.shuffled,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn discard_queue_capture(&self, namespace: &str) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query("DELETE FROM queue_occurrences WHERE substr(object_id,1,length(?1))=?1
                    AND object_id NOT IN(SELECT json_extract(value,'$.occurrence') FROM queue_saved,json_each(queue_saved.state,'$.entries'))")
            .bind(namespace).execute(connection).await?;
        Ok(())
    }
    pub async fn restore_queue(&self) -> LibraryResult<QueueRestore> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let json =
            sqlx::query_scalar::<_, String>("SELECT state FROM queue_saved WHERE singleton=1")
                .fetch_optional(&mut *transaction)
                .await?;
        let migrated = json.as_ref().is_none_or(|json| {
            serde_json::from_str::<serde_json::Value>(json)
                .ok()
                .is_none_or(|value| value.get("entries").is_none())
        });
        let mut state = match json {
            Some(json)
                if serde_json::from_str::<serde_json::Value>(&json)?
                    .get("entries")
                    .is_some() =>
            {
                serde_json::from_str(&json)?
            }
            Some(json) => migrate_saved(&mut transaction, serde_json::from_str(&json)?).await?,
            None => {
                let rows = read_all_occurrences(&mut transaction).await?;
                state_from_rows(rows)
            }
        };
        if let Some(order) =
            sqlx::query_scalar::<_, String>("SELECT state FROM queue_order WHERE singleton=1")
                .fetch_optional(&mut *transaction)
                .await?
        {
            state.order = serde_json::from_str(&order)?;
        }
        read_saved_settings(&mut transaction, &mut state).await?;
        if migrated {
            save_queue_on(&mut transaction, &state).await?;
        }
        transaction.commit().await?;
        drop(writer);
        let start = state.current_index.unwrap_or(0).saturating_sub(10);
        let entries = state
            .order
            .iter()
            .skip(start)
            .take(QUEUE_CONTEXT_LIMIT)
            .map(|index| state.entries[*index as usize].clone())
            .collect::<Vec<_>>();
        state.occurrences = self.hydrate_queue_entries(&entries).await?;
        Ok(state)
    }

    pub async fn persist_queue_settings(
        &self,
        current: Option<&OccurrenceId>,
        progress: i64,
        repeat: QueueRepeatMode,
        shuffled: bool,
    ) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        save_settings(
            writer.as_mut().ok_or(LibraryError::WriterUnavailable)?,
            current,
            progress,
            repeat,
            shuffled,
        )
        .await
    }

    pub async fn export_queue_jsonl(&self, output: impl std::io::Write) -> LibraryResult<()> {
        self.restore_queue().await?;
        let mut connection = self.acquire_reader().await?;
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
            "SELECT json_extract(entry.value,'$.occurrence') FROM queue_saved,json_each(queue_saved.state,'$.entries') entry
             WHERE substr(json_extract(entry.value,'$.media_uri'),1,length(?2))=?2
                OR EXISTS(SELECT 1 FROM tracks WHERE source_key=?1 AND media_uri=json_extract(entry.value,'$.media_uri'))")
            .bind(source).bind(prefix).fetch_all(&mut *connection).await?)
    }
}

async fn capture_input(
    database: Option<&Database>,
    connection: &mut sqlx::Transaction<'_, Sqlite>,
    input: QueueInput,
    namespace: &str,
    entries: &mut Vec<QueueEntry>,
    anchor: &mut Option<usize>,
) -> LibraryResult<()> {
    let input = match input {
        QueueInput::Groups(inputs) => {
            for input in inputs {
                Box::pin(capture_input(
                    database, connection, input, namespace, entries, anchor,
                ))
                .await?;
            }
            return Ok(());
        }
        QueueInput::Query {
            query,
            folder,
            filter,
            sort,
            descending,
            context_id,
            anchor_uri,
        } => {
            let Some(reference) = crate::source_window::canonical_query(
                connection, query, folder, filter, sort, descending, anchor_uri,
            )
            .await?
            else {
                return Ok(());
            };
            QueueInput::Source {
                reference,
                context_id,
            }
        }
        QueueInput::PlaylistQuery {
            key,
            folder,
            filter,
            sort,
            descending,
            context_id,
            anchor_entry,
            anchor_uri,
        } => {
            let Some(reference) = crate::source_window::canonical_playlist_query(
                connection,
                key,
                folder,
                filter,
                sort,
                descending,
                anchor_entry,
                anchor_uri,
            )
            .await?
            else {
                return Ok(());
            };
            QueueInput::Source {
                reference,
                context_id,
            }
        }
        QueueInput::Collection {
            collection,
            folder,
            context_id,
        } => {
            let input = match collection {
                QueueCollection::Playlist(key) => QueueInput::PlaylistQuery {
                    key,
                    folder,
                    filter: String::new(),
                    sort: crate::PlaylistEntrySort::Position,
                    descending: false,
                    context_id,
                    anchor_entry: None,
                    anchor_uri: None,
                },
                collection => {
                    let sort = if matches!(
                        collection,
                        QueueCollection::Album(_) | QueueCollection::AlbumKey(_)
                    ) {
                        crate::TrackSort::TrackNumber
                    } else {
                        crate::TrackSort::Title
                    };
                    QueueInput::Query {
                        query: QueueQuery::Collection {
                            collection,
                            favorites_only: false,
                        },
                        folder,
                        filter: String::new(),
                        sort,
                        descending: false,
                        context_id,
                        anchor_uri: None,
                    }
                }
            };
            return Box::pin(capture_input(
                database, connection, input, namespace, entries, anchor,
            ))
            .await;
        }
        QueueInput::Smart {
            key,
            source,
            folder,
            now,
            context_id,
        } => {
            return Box::pin(capture_input(
                database,
                connection,
                QueueInput::Query {
                    query: QueueQuery::Smart { key, source, now },
                    folder,
                    filter: String::new(),
                    sort: crate::TrackSort::Title,
                    descending: false,
                    context_id,
                    anchor_uri: None,
                },
                namespace,
                entries,
                anchor,
            ))
            .await;
        }
        input => input,
    };
    match input {
        QueueInput::Source {
            reference,
            context_id,
        } => {
            let rows = crate::source_window::source_members(connection, &reference).await?;
            for (rank, (uri, identity, selected)) in rows.into_iter().enumerate() {
                if selected {
                    *anchor = Some(entries.len());
                }
                push_entry(
                    entries,
                    namespace,
                    uri,
                    identity,
                    QueueProvenance::Context {
                        context_id: context_id.clone(),
                        source_rank: rank,
                    },
                );
            }
        }

        QueueInput::PlaylistEntries { order, context_id } => {
            for (chunk_index, keys) in order.chunks(100).enumerate() {
                let mut snapshots = Vec::new();
                for row in crate::playlists::load_playlist_entry_rows(connection, keys).await? {
                    let rank = chunk_index * 100
                        + keys
                            .iter()
                            .position(|key| *key == row.playlist_entry_key)
                            .unwrap_or(0);
                    let identity = playlist_identity(connection, row.playlist_entry_key).await?;
                    let entry = push_entry(
                        entries,
                        namespace,
                        row.media_uri.clone(),
                        identity,
                        QueueProvenance::Context {
                            context_id: context_id.clone(),
                            source_rank: rank,
                        },
                    );
                    snapshots.push(supplied_snapshot(entry, row.into()));
                }
                admit_snapshots(database, connection, &snapshots).await?;
            }
        }
        QueueInput::Items(items) => {
            for chunk in items.chunks(100) {
                let mut snapshots = Vec::with_capacity(chunk.len());
                for (item, provenance) in chunk {
                    let entry = push_entry(
                        entries,
                        namespace,
                        item.media_uri.clone(),
                        None,
                        provenance.clone(),
                    );
                    snapshots.push(supplied_snapshot(entry, item.clone()));
                }
                admit_snapshots(database, connection, &snapshots).await?;
            }
        }
        QueueInput::Choices(choices) => {
            for chunk in choices.chunks(100) {
                let mut snapshots = Vec::new();
                for choice in chunk.iter().flatten() {
                    let entry = push_entry(
                        entries,
                        namespace,
                        choice.media_uri.clone(),
                        None,
                        choice.provenance.clone(),
                    );
                    if let Some(item) = &choice.fallback {
                        snapshots.push(supplied_snapshot(entry, item.clone()));
                    }
                }
                admit_snapshots(database, connection, &snapshots).await?;
            }
        }
        QueueInput::MediaUris { order, provenance } => {
            for uri in order.iter() {
                push_entry(entries, namespace, uri.clone(), None, provenance.clone());
            }
        }
        QueueInput::Uris {
            order,
            context_id,
            source_start,
        } => {
            for (index, uri) in order.iter().enumerate() {
                push_entry(
                    entries,
                    namespace,
                    uri.clone(),
                    None,
                    QueueProvenance::Context {
                        context_id: context_id.clone(),
                        source_rank: source_start + index,
                    },
                );
            }
        }
        _ => unreachable!("source inputs normalized above"),
    }
    Ok(())
}

fn push_entry<'a>(
    entries: &'a mut Vec<QueueEntry>,
    namespace: &str,
    media_uri: String,
    playlist_entry_id: Option<String>,
    provenance: QueueProvenance,
) -> &'a QueueEntry {
    entries.push(QueueEntry {
        occurrence: format!("queue:{namespace}:{}", entries.len()).into(),
        media_uri: media_uri.into(),
        playlist_entry_id: playlist_entry_id.map(Into::into),
        provenance,
    });
    entries.last().unwrap()
}

fn supplied_snapshot(entry: &QueueEntry, item: QueueItem) -> QueueOccurrence {
    QueueOccurrence {
        occurrence: entry.occurrence.clone(),
        item,
        canonical_position: 0,
        source_index: None,
        playlist_entry_id: entry.playlist_entry_id.as_ref().map(ToString::to_string),
        provenance: entry.provenance.clone(),
    }
}
async fn admit_snapshots(
    database: Option<&Database>,
    connection: &mut sqlx::SqliteConnection,
    rows: &[QueueOccurrence],
) -> LibraryResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    if let Some(database) = database {
        let mut writer = database.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        persist_occurrence_page(&mut transaction, rows, 0).await?;
        transaction.commit().await?;
        Ok(())
    } else {
        persist_occurrence_page(connection, rows, 0).await
    }
}
async fn playlist_identity(
    connection: &mut sqlx::SqliteConnection,
    key: crate::PlaylistEntryKey,
) -> LibraryResult<Option<String>> {
    Ok(sqlx::query_scalar("SELECT json_array(source.object_id,playlist.object_id,entry.object_id) FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) LEFT JOIN source_ids source USING(source_key) WHERE entry.playlist_entry_key=?1")
        .bind(key).fetch_optional(connection).await?)
}
async fn save_settings(
    connection: &mut sqlx::SqliteConnection,
    current: Option<&OccurrenceId>,
    progress: i64,
    repeat: QueueRepeatMode,
    shuffled: bool,
) -> LibraryResult<()> {
    sqlx::query("INSERT INTO queue_state(singleton,current_occurrence_id,progress_millis,repeat_mode,shuffled) VALUES(1,?1,?2,?3,?4) ON CONFLICT(singleton) DO UPDATE SET current_occurrence_id=excluded.current_occurrence_id,progress_millis=excluded.progress_millis,repeat_mode=excluded.repeat_mode,shuffled=excluded.shuffled")
        .bind(current.map(OccurrenceId::as_str)).bind(progress.max(0)).bind(repeat.as_str()).bind(shuffled).execute(connection).await?;
    Ok(())
}
async fn save_queue_on(
    connection: &mut sqlx::SqliteConnection,
    state: &QueueRestore,
) -> LibraryResult<()> {
    let saved = serde_json::to_string(state)?;
    let occurrences = state
        .entries
        .iter()
        .map(|entry| entry.occurrence.as_str())
        .collect::<Vec<_>>();
    sqlx::query(
        "DELETE FROM queue_occurrences WHERE object_id NOT IN(SELECT value FROM json_each(?1))",
    )
    .bind(serde_json::to_string(&occurrences)?)
    .execute(&mut *connection)
    .await?;
    sqlx::query("INSERT INTO queue_saved(singleton,state) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET state=excluded.state").bind(saved).execute(&mut *connection).await?;
    sqlx::query("INSERT INTO queue_order(singleton,state) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET state=excluded.state")
        .bind(serde_json::to_string(&state.order)?).execute(&mut *connection).await?;
    save_settings(
        connection,
        state.current(),
        state.progress_millis,
        state.repeat_mode,
        state.shuffled,
    )
    .await
}
async fn read_saved_settings(
    connection: &mut sqlx::SqliteConnection,
    state: &mut QueueRestore,
) -> LibraryResult<()> {
    if let Some((current,progress,repeat,shuffled))=sqlx::query_as::<_,(Option<String>,i64,String,bool)>("SELECT current_occurrence_id,progress_millis,repeat_mode,shuffled FROM queue_state WHERE singleton=1").fetch_optional(connection).await? {
        state.current_index=current.and_then(|id| state.order.iter().position(|index|state.entries[*index as usize].occurrence.as_str()==id));
        state.progress_millis=progress;state.repeat_mode=QueueRepeatMode::parse(&repeat)?;state.shuffled=shuffled;
    }
    Ok(())
}
fn state_from_rows(rows: Vec<Arc<QueueOccurrence>>) -> QueueRestore {
    let entries = rows
        .iter()
        .map(|row| QueueEntry {
            occurrence: row.occurrence.clone(),
            media_uri: row.media_uri.clone().into(),
            playlist_entry_id: row.playlist_entry_id.clone().map(Into::into),
            provenance: row.provenance.clone(),
        })
        .collect::<Vec<_>>();
    QueueRestore {
        order: (0..entries.len() as u32).collect(),
        entries: entries.into(),
        ..Default::default()
    }
}

async fn migrate_saved(
    connection: &mut sqlx::Transaction<'_, Sqlite>,
    value: serde_json::Value,
) -> LibraryResult<QueueRestore> {
    let rows = read_all_occurrences(connection).await?;
    let sources: Vec<LegacyQueueInstruction> = serde_json::from_value(value["sources"].clone())?;
    let pending: Vec<LegacyQueueCursor> = serde_json::from_value(value["pending"].clone())?;
    let namespace: String = sqlx::query_scalar("SELECT lower(hex(randomblob(16)))")
        .fetch_one(&mut **connection)
        .await?;
    let mut entries = Vec::new();
    let mut members: Vec<Vec<Option<usize>>> = Vec::new();
    let mut anchors = Vec::new();
    for (source_index, source) in sources.iter().enumerate() {
        let mut captured = Vec::new();
        let mut anchor = None;
        capture_input(
            None,
            connection,
            source.input.clone(),
            &format!("{namespace}:{source_index}"),
            &mut captured,
            &mut anchor,
        )
        .await?;
        let mut mapping = Vec::new();
        if let QueueInput::Choices(choices) = &source.input {
            let mut captured = captured.into_iter();
            for choice in choices.iter() {
                let Some(choice) = choice else {
                    mapping.push(None);
                    continue;
                };
                let entry = captured.next().unwrap();
                let origin = choice.origin.and_then(|(source, position)| {
                    members.get(source)?.get(position).copied().flatten()
                });
                mapping.push(Some(origin.unwrap_or_else(|| {
                    let index = entries.len();
                    entries.push(entry);
                    index
                })));
            }
        } else {
            for entry in captured {
                mapping.push(Some(entries.len()));
                entries.push(entry);
            }
        }
        if let QueueInput::Source { reference, .. } = &source.input {
            let positions = mapping
                .iter()
                .flatten()
                .map(|index| {
                    (
                        (
                            entries[*index].media_uri.clone(),
                            entries[*index].playlist_entry_id.clone(),
                        ),
                        *index,
                    )
                })
                .collect::<std::collections::HashMap<_, _>>();
            mapping =
                crate::source_window::legacy_source_members(connection, reference, source.seed)
                    .await?
                    .into_iter()
                    .map(|(uri, identity, _)| {
                        positions
                            .get(&(uri.into(), identity.map(Into::into)))
                            .copied()
                    })
                    .collect();
            anchor = None;
        }
        members.push(mapping);
        anchors.push(anchor.unwrap_or(0));
    }
    let mut order = Vec::new();
    let mut used = std::collections::HashSet::new();
    for row in &rows {
        let candidate = row
            .source_index
            .and_then(|source| {
                members
                    .get(source)?
                    .get(row.canonical_position)
                    .copied()
                    .flatten()
            })
            .filter(|index| entries[*index].media_uri.as_ref() == row.media_uri);
        let index = candidate.unwrap_or_else(|| {
            let index = entries.len();
            entries.push(QueueEntry {
                occurrence: row.occurrence.clone(),
                media_uri: row.media_uri.clone().into(),
                playlist_entry_id: row.playlist_entry_id.clone().map(Into::into),
                provenance: row.provenance.clone(),
            });
            index
        });
        entries[index].occurrence = row.occurrence.clone();
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
                cursor.anchor.unwrap_or(anchors[cursor.source])
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
    let selected = value["current_index"]
        .as_u64()
        .and_then(|index| rows.get(index as usize))
        .map(|row| row.occurrence.clone());
    let mut state = QueueRestore {
        entries: entries.into(),
        order: order.into(),
        ..Default::default()
    };
    state.current_index = selected.and_then(|id| {
        state
            .order
            .iter()
            .position(|index| state.entries[*index as usize].occurrence == id)
    });
    state.progress_millis = value["progress_millis"].as_i64().unwrap_or(0);
    state.repeat_mode = serde_json::from_value(value["repeat_mode"].clone())?;
    state.shuffled = value["shuffled"].as_bool().unwrap_or(false);
    Ok(state)
}
pub(crate) async fn export_queue_jsonl_on(
    connection: &mut sqlx::SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<()> {
    let mut state: QueueRestore =
        sqlx::query_scalar::<_, String>("SELECT state FROM queue_saved WHERE singleton=1")
            .fetch_optional(&mut *connection)
            .await?
            .map(|json| serde_json::from_str(&json))
            .transpose()?
            .unwrap_or_default();
    if let Some(order) =
        sqlx::query_scalar::<_, String>("SELECT state FROM queue_order WHERE singleton=1")
            .fetch_optional(&mut *connection)
            .await?
    {
        state.order = serde_json::from_str(&order)?;
    }
    read_saved_settings(connection, &mut state).await?;
    let snapshots = read_all_occurrences(connection).await?;
    serde_json::to_writer(
        &mut output,
        &serde_json::json!({"version":3,"queue":state,"snapshots":snapshots}),
    )?;
    output.write_all(b"\n")?;
    Ok(())
}
pub(crate) async fn import_queue_jsonl_on(
    connection: &mut sqlx::SqliteConnection,
    mut input: impl std::io::BufRead,
) -> LibraryResult<()> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    let header: serde_json::Value = serde_json::from_str(&line)?;
    let (mut state, rows) = match header["version"].as_u64() {
        Some(3) => (
            serde_json::from_value::<QueueRestore>(header["queue"].clone())?,
            serde_json::from_value::<Vec<QueueOccurrence>>(header["snapshots"].clone())?,
        ),
        Some(2) => {
            let legacy = &header["queue"];
            let rows =
                serde_json::from_value::<Vec<QueueOccurrence>>(legacy["occurrences"].clone())?;
            let mut transaction = connection.begin().await?;
            for row in &rows {
                persist_occurrence_page(&mut transaction, std::slice::from_ref(row), 0).await?;
            }
            let state = migrate_saved(&mut transaction, legacy.clone()).await?;
            transaction.commit().await?;
            (state, rows)
        }
        Some(1) => {
            let mut rows = Vec::new();
            while {
                line.clear();
                input.read_line(&mut line)? != 0
            } {
                if !line.trim().is_empty() {
                    rows.push(serde_json::from_str::<QueueOccurrence>(&line)?);
                }
            }
            (
                state_from_rows(rows.iter().cloned().map(Arc::new).collect()),
                rows,
            )
        }
        _ => {
            return Err(LibraryError::InvalidRequest(
                "unsupported Queue export version".into(),
            ));
        }
    };
    if header["version"] == 1 {
        state.current_index = header["current_occurrence"].as_str().and_then(|id| {
            state
                .entries
                .iter()
                .position(|entry| entry.occurrence.as_str() == id)
        });
        state.progress_millis = header["progress_millis"].as_i64().unwrap_or(0);
        state.repeat_mode = serde_json::from_value(header["repeat_mode"].clone())?;
        state.shuffled = header["shuffled"].as_bool().unwrap_or(false);
    }
    persist_occurrence_page(connection, &rows, 0).await?;
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
                 primary_artist_musicbrainz_id,origin_source,origin_position,playlist_entry_id
             ) VALUES (
                 ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
                 ?17,?18,?19,?20,?21,?22,?23,?24,?25
             ) ON CONFLICT(object_id) DO UPDATE SET
                 position=excluded.position,traversal_position=excluded.traversal_position,
                 provenance_kind=excluded.provenance_kind,
                 provenance_context_id=excluded.provenance_context_id,
                 provenance_source_rank=excluded.provenance_source_rank",
        )
        .bind(occurrence.occurrence.as_str())
        .bind(&item.media_uri)
        .bind((traversal_offset + traversal_position) as i64)
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
        .bind(occurrence.source_index.map(|i| i as i64))
        .bind(occurrence.canonical_position as i64)
        .bind(&occurrence.playlist_entry_id)
        .execute(&mut *transaction)
        .await?;
    }
    Ok(())
}
async fn read_all_occurrences(
    connection: &mut sqlx::SqliteConnection,
) -> LibraryResult<Vec<Arc<QueueOccurrence>>> {
    sqlx::query_as::<_,QueueOccurrenceRow>("SELECT occurrence.object_id,occurrence.media_uri,origin_source source_index,playlist_entry_id,COALESCE(origin_position,position) canonical_position,provenance_kind,provenance_context_id,provenance_source_rank,occurrence.title,occurrence.artist,occurrence.album,occurrence.album_display_artist,track.artwork_binding,occurrence.duration_millis,occurrence.disc_number,occurrence.track_number,occurrence.year,occurrence.release_date,occurrence.source_format,occurrence.musicbrainz_recording_id,occurrence.musicbrainz_release_track_id,occurrence.musicbrainz_album_id,occurrence.musicbrainz_release_group_id,occurrence.primary_artist_musicbrainz_id FROM queue_occurrences occurrence LEFT JOIN tracks track USING(media_uri) ORDER BY traversal_position")
        .fetch_all(connection).await?.into_iter().map(|row|Ok(Arc::new(QueueOccurrence{occurrence:OccurrenceId::new(row.object_id),item:row.item,source_index:row.source_index.map(|i|i as usize),playlist_entry_id:row.playlist_entry_id,canonical_position:row.canonical_position as usize,provenance:QueueProvenance::parse(&row.provenance_kind,row.provenance_context_id,row.provenance_source_rank)?}))).collect()
}

async fn read_occurrence(
    connection: &mut sqlx::SqliteConnection,
    id: &OccurrenceId,
) -> LibraryResult<Option<QueueOccurrence>> {
    let row = sqlx::query_as::<_, QueueOccurrenceRow>(sqlx::AssertSqlSafe(
        OCCURRENCE_SELECT.to_string() + " WHERE occurrence.object_id=?1",
    ))
    .bind(id.as_str())
    .fetch_optional(connection)
    .await?;
    row.map(|row| {
        Ok(QueueOccurrence {
            occurrence: row.object_id.into(),
            item: row.item,
            source_index: row.source_index.map(|i| i as usize),
            playlist_entry_id: row.playlist_entry_id,
            canonical_position: row.canonical_position as usize,
            provenance: QueueProvenance::parse(
                &row.provenance_kind,
                row.provenance_context_id,
                row.provenance_source_rank,
            )?,
        })
    })
    .transpose()
}

const OCCURRENCE_SELECT: &str = "SELECT occurrence.object_id,occurrence.media_uri,origin_source source_index,playlist_entry_id,COALESCE(origin_position,position) canonical_position,provenance_kind,provenance_context_id,provenance_source_rank,occurrence.title,occurrence.artist,occurrence.album,occurrence.album_display_artist,track.artwork_binding,occurrence.duration_millis,occurrence.disc_number,occurrence.track_number,occurrence.year,occurrence.release_date,occurrence.source_format,occurrence.musicbrainz_recording_id,occurrence.musicbrainz_release_track_id,occurrence.musicbrainz_album_id,occurrence.musicbrainz_release_group_id,occurrence.primary_artist_musicbrainz_id FROM queue_occurrences occurrence LEFT JOIN tracks track USING(media_uri)";
