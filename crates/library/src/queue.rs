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
pub struct QueueInstruction {
    pub input: QueueInput,
    /// Displaced source entries belong only to this pass, unlike user additions.
    pub repeat: bool,
    #[serde(default)]
    pub seed: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueCursor {
    pub source: usize,
    pub after: Option<String>,
    pub offset: usize,
    pub seed: Option<u64>,
    pub anchor: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueRestore {
    pub occurrences: Vec<Arc<QueueOccurrence>>,
    pub current_index: Option<usize>,
    pub progress_millis: i64,
    pub repeat_mode: QueueRepeatMode,
    pub shuffled: bool,
    pub sources: Vec<QueueInstruction>,
    pub pending: std::collections::VecDeque<QueueCursor>,
    pub next_id: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueReadRequest {
    pub input: QueueInput,
    pub cursor: QueueCursor,
    pub limit: usize,
    pub history: bool,
    pub backwards: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueReadPage {
    pub input: QueueInput,
    pub items: Vec<(QueueItem, QueueProvenance, usize, Option<String>)>,
    pub cursor: QueueCursor,
    pub exhausted: bool,
    pub current_index: usize,
}

impl QueueRestore {
    pub fn current(&self) -> Option<&OccurrenceId> {
        self.current_index
            .and_then(|i| self.occurrences.get(i))
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
                    position: ordinal,
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

impl Database {
    pub async fn read_queue(&self, mut request: QueueReadRequest) -> LibraryResult<QueueReadPage> {
        let mut connection = self.acquire_reader().await?;
        request.input = self
            .normalize_queue_input(&mut connection, request.input)
            .await?;
        drop(connection);
        self.read_normalized(request).await
    }

    async fn normalize_queue_input(
        &self,
        connection: &mut sqlx::SqliteConnection,
        input: QueueInput,
    ) -> LibraryResult<QueueInput> {
        let input = match input {
            QueueInput::Groups(inputs) => {
                let mut result = Vec::new();
                for input in inputs {
                    match Box::pin(self.normalize_queue_input(connection, input)).await? {
                        QueueInput::Groups(inputs) => result.extend(inputs),
                        input => result.push(input),
                    }
                }
                if result
                    .iter()
                    .all(|input| matches!(input, QueueInput::Choices(_)))
                {
                    return Ok(QueueInput::Choices(
                        result
                            .into_iter()
                            .flat_map(|input| {
                                if let QueueInput::Choices(rows) = input {
                                    rows.to_vec()
                                } else {
                                    unreachable!()
                                }
                            })
                            .collect(),
                    ));
                }
                return Ok(QueueInput::Groups(result));
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
                return Ok(crate::source_window::canonical_query(
                    connection, query, folder, filter, sort, descending, anchor_uri,
                )
                .await?
                .map_or_else(
                    || QueueInput::Choices(Arc::from([])),
                    |reference| QueueInput::Source {
                        reference,
                        context_id,
                    },
                ));
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
                return Ok(crate::source_window::canonical_playlist_query(
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
                .map_or_else(
                    || QueueInput::Choices(Arc::from([])),
                    |reference| QueueInput::Source {
                        reference,
                        context_id,
                    },
                ));
            }
            QueueInput::Collection {
                collection,
                folder,
                context_id,
            } => {
                let input = if let QueueCollection::Playlist(key) = collection {
                    QueueInput::PlaylistQuery {
                        key,
                        folder,
                        filter: String::new(),
                        sort: crate::PlaylistEntrySort::Position,
                        descending: false,
                        context_id,
                        anchor_entry: None,
                        anchor_uri: None,
                    }
                } else {
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
                };
                return Box::pin(self.normalize_queue_input(connection, input)).await;
            }
            QueueInput::Smart {
                key,
                source,
                folder,
                now,
                context_id,
            } => {
                return Box::pin(self.normalize_queue_input(
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
                ))
                .await;
            }
            QueueInput::Items(items) => items
                .into_iter()
                .map(|(item, provenance)| QueueChoice {
                    origin: None,
                    media_uri: item.media_uri.clone(),
                    fallback: Some(item),
                    provenance,
                })
                .collect(),
            QueueInput::MediaUris { order, provenance } => order
                .iter()
                .map(|uri| QueueChoice {
                    origin: None,
                    media_uri: uri.clone(),
                    fallback: None,
                    provenance: provenance.clone(),
                })
                .collect(),
            QueueInput::Uris {
                order,
                context_id,
                source_start,
            } => order
                .iter()
                .enumerate()
                .map(|(i, uri)| QueueChoice {
                    origin: None,
                    media_uri: uri.clone(),
                    fallback: None,
                    provenance: QueueProvenance::Context {
                        context_id: context_id.clone(),
                        source_rank: source_start + i,
                    },
                })
                .collect(),
            QueueInput::PlaylistEntries { order, context_id } => {
                let mut result = Vec::new();
                let mut transaction = connection.begin().await?;
                for (start, keys) in order.chunks(100).enumerate() {
                    for row in
                        crate::playlists::load_playlist_entry_rows(&mut transaction, keys).await?
                    {
                        let rank = start * 100
                            + keys
                                .iter()
                                .position(|key| *key == row.playlist_entry_key)
                                .unwrap_or(0);
                        result.push(QueueChoice {
                            origin: None,
                            media_uri: row.media_uri.clone(),
                            fallback: Some(row.into()),
                            provenance: QueueProvenance::Context {
                                context_id: context_id.clone(),
                                source_rank: rank,
                            },
                        });
                    }
                }
                transaction.commit().await?;
                result
            }
            input => return Ok(input),
        };
        Ok(QueueInput::Choices(input.into_iter().map(Some).collect()))
    }

    async fn read_normalized(&self, request: QueueReadRequest) -> LibraryResult<QueueReadPage> {
        let mut cursor = request.cursor.clone();
        let mut items = Vec::new();
        let mut current_index = 0;
        let limit = request.limit.min(100);
        let exhausted = match &request.input {
            QueueInput::Groups(inputs) => {
                for (index, input) in inputs.iter().enumerate() {
                    let mut page = Box::pin(self.read_normalized(QueueReadRequest {
                        input: input.clone(),
                        ..request.clone()
                    }))
                    .await?;
                    if page.exhausted && page.items.is_empty() {
                        continue;
                    }
                    page.input = QueueInput::Groups(
                        std::iter::once(page.input)
                            .chain(inputs[index + 1..].iter().cloned())
                            .collect(),
                    );
                    return Ok(page);
                }
                true
            }
            QueueInput::Choices(choices) => {
                let mut positions = (0..choices.len()).collect::<Vec<_>>();
                if let Some(mut seed) = cursor.seed {
                    seed = seed.wrapping_add(0x9e3779b97f4a7c15);
                    for i in (1..positions.len()).rev() {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        positions.swap(i, seed as usize % (i + 1));
                    }
                    if let Some(index) = positions.iter().position(|i| Some(*i) == cursor.anchor) {
                        positions.swap(0, index);
                    }
                }
                if request.backwards {
                    positions.retain(|index| choices[*index].is_some());
                }
                let start = if request.backwards {
                    positions.len().saturating_sub(limit)
                } else if cursor.seed.is_some() {
                    cursor.offset
                } else {
                    cursor.anchor.unwrap_or(0) + cursor.offset
                };
                let history = if request.history && cursor.seed.is_none() {
                    start.min(10)
                } else {
                    0
                };
                current_index = history;
                let positions = positions
                    .into_iter()
                    .skip(start.saturating_sub(history))
                    .take(limit)
                    .collect::<Vec<_>>();
                let consumed = positions.len();
                let selected = positions
                    .iter()
                    .filter_map(|i| choices[*i].as_ref())
                    .collect::<Vec<_>>();
                let uris = selected
                    .iter()
                    .filter(|choice| choice.fallback.is_none())
                    .map(|choice| choice.media_uri.clone())
                    .collect::<Vec<_>>();
                let hydrated = self
                    .queue_items_for_uris(&uris, &ReadCancellation::new())
                    .await?
                    .into_iter()
                    .map(|item| (item.media_uri.clone(), item))
                    .collect::<std::collections::HashMap<_, _>>();
                for position in &positions {
                    if let Some(choice) = &choices[*position]
                        && let Some(item) = choice
                            .fallback
                            .as_ref()
                            .or_else(|| hydrated.get(&choice.media_uri))
                    {
                        items.push((item.clone(), choice.provenance.clone(), *position, None));
                    }
                }
                cursor.offset += consumed.saturating_sub(history);
                start.saturating_sub(history) + consumed >= choices.len()
            }
            QueueInput::Source {
                reference,
                context_id,
            } => {
                let mut connection = self.acquire_reader().await?;
                if cursor.after.is_none() && cursor.offset == 1 {
                    cursor.after = crate::source_window::read_source(
                        &mut connection,
                        reference,
                        None,
                        1,
                        cursor.seed,
                        false,
                    )
                    .await?
                    .pop()
                    .map(|(_, _, after, _)| after);
                }
                let mut members = Vec::new();
                if request.history && cursor.seed.is_none() && reference.anchor_uri.is_some() {
                    members = crate::source_window::read_source(
                        &mut connection,
                        reference,
                        None,
                        11,
                        None,
                        true,
                    )
                    .await?;
                    if !members.is_empty() {
                        members.remove(0);
                    }
                    members.reverse();
                    current_index = members.len();
                }
                let next = crate::source_window::read_source(
                    &mut connection,
                    reference,
                    cursor.after.as_deref(),
                    limit - members.len(),
                    cursor.seed,
                    request.backwards,
                )
                .await?;
                let exhausted = next.len() < limit - members.len();
                if let Some((_, _, after, _)) = next.last() {
                    cursor.after = Some(after.clone());
                }
                let start = cursor.anchor.unwrap_or(0) + cursor.offset;
                cursor.offset += next.len();
                members.extend(next);
                drop(connection);
                let resolved = if members.iter().any(|(_, entry, _, _)| entry.is_some()) {
                    self.playlist_entry_rows(
                        &members
                            .iter()
                            .filter_map(|(_, entry, _, _)| *entry)
                            .collect::<Vec<_>>(),
                        &ReadCancellation::new(),
                    )
                    .await?
                    .into_iter()
                    .map(QueueItem::from)
                    .collect()
                } else {
                    self.queue_items_for_uris(
                        &members
                            .iter()
                            .map(|(uri, _, _, _)| uri.clone())
                            .collect::<Vec<_>>(),
                        &ReadCancellation::new(),
                    )
                    .await?
                };
                items.extend(resolved.into_iter().enumerate().map(|(i, item)| {
                    (
                        item,
                        QueueProvenance::Context {
                            context_id: context_id.clone(),
                            source_rank: start.saturating_sub(current_index) + i,
                        },
                        start.saturating_sub(current_index) + i,
                        members[i].3.clone(),
                    )
                }));
                exhausted
            }
            _ => unreachable!("queue inputs are normalized before source reads"),
        };
        Ok(QueueReadPage {
            input: request.input,
            items,
            cursor,
            exhausted,
            current_index,
        })
    }

    pub async fn save_queue(&self, state: &QueueRestore) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        save_queue_on(&mut transaction, state).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn restore_queue(&self) -> LibraryResult<QueueRestore> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let mut state = if let Some(json) =
            sqlx::query_scalar::<_, String>("SELECT state FROM queue_saved WHERE singleton=1")
                .fetch_optional(&mut *transaction)
                .await?
        {
            {
                let mut state: QueueRestore = serde_json::from_str(&json)?;
                state.occurrences = read_saved_rows(&mut transaction).await?;
                state
            }
        } else {
            let state = migrate_queue_on(&mut transaction).await?;
            save_queue_on(&mut transaction, &state).await?;
            state
        };
        if let Some((current, progress, repeat, shuffled)) = sqlx::query_as::<_, (Option<String>,i64,String,bool)>(
            "SELECT current_occurrence_id,progress_millis,repeat_mode,shuffled FROM queue_state WHERE singleton=1")
            .fetch_optional(&mut *transaction).await? {
            state.current_index = current.and_then(|id| state.occurrences.iter().position(|row| row.occurrence.as_str()==id));
            state.progress_millis=progress;
            state.repeat_mode=QueueRepeatMode::parse(&repeat)?;
            state.shuffled=shuffled;
        }
        transaction.commit().await?;
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
    let mut saved = state.clone();
    saved.occurrences.clear();
    sqlx::query("INSERT INTO queue_saved(singleton,state) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET state=excluded.state")
        .bind(serde_json::to_string(&saved)?).execute(&mut *connection).await?;
    sqlx::query("DELETE FROM queue_occurrences")
        .execute(&mut *connection)
        .await?;
    persist_occurrence_page(
        connection,
        &state
            .occurrences
            .iter()
            .map(|row| row.as_ref().clone())
            .collect::<Vec<_>>(),
        0,
    )
    .await?;
    save_settings(
        connection,
        state.current(),
        state.progress_millis,
        state.repeat_mode,
        state.shuffled,
    )
    .await
}

pub(crate) async fn export_queue_jsonl_on(
    connection: &mut sqlx::SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<()> {
    let state = sqlx::query_scalar::<_, String>("SELECT state FROM queue_saved WHERE singleton=1")
        .fetch_optional(&mut *connection)
        .await?;
    let mut state: QueueRestore = if let Some(json) = state {
        let mut state: QueueRestore = serde_json::from_str(&json)?;
        state.occurrences = read_saved_rows(connection).await?;
        state
    } else {
        migrate_queue_on(connection).await?
    };
    read_saved_settings(connection, &mut state).await?;
    serde_json::to_writer(&mut output, &serde_json::json!({"version":2,"queue":state}))?;
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
    let state = match header["version"].as_u64() {
        Some(2) => serde_json::from_value(header["queue"].clone())?,
        Some(1) => {
            let current = header["current_occurrence"].as_str();
            let mut rows = Vec::new();
            loop {
                line.clear();
                if input.read_line(&mut line)? == 0 {
                    break;
                }
                if !line.trim().is_empty() {
                    rows.push(serde_json::from_str::<QueueOccurrence>(&line)?);
                }
            }
            let selected =
                current.and_then(|id| rows.iter().position(|row| row.occurrence.as_str() == id));
            let mut state = compact_legacy(connection, rows, selected).await?;
            state.progress_millis = header["progress_millis"].as_i64().unwrap_or(0);
            state.repeat_mode = serde_json::from_value(header["repeat_mode"].clone())?;
            state.shuffled = header["shuffled"].as_bool().unwrap_or(false);
            state
        }
        _ => {
            return Err(LibraryError::InvalidRequest(
                "unsupported Queue export version".into(),
            ));
        }
    };
    save_queue_on(connection, &state).await?;
    Ok(())
}

async fn migrate_queue_on(connection: &mut sqlx::SqliteConnection) -> LibraryResult<QueueRestore> {
    use futures_util::TryStreamExt;
    let selected = sqlx::query_scalar::<_,i64>("SELECT count(*) FROM queue_occurrences WHERE traversal_position < (SELECT traversal_position FROM queue_occurrences WHERE object_id=(SELECT current_occurrence_id FROM queue_state WHERE singleton=1)) HAVING EXISTS(SELECT 1 FROM queue_occurrences WHERE object_id=(SELECT current_occurrence_id FROM queue_state WHERE singleton=1))")
        .fetch_optional(&mut *connection).await?.map(|i|i as usize);
    let start = selected.unwrap_or(0).saturating_sub(10);
    let mut state = QueueRestore {
        current_index: selected.map(|i| i - start),
        ..Default::default()
    };
    let mut choices = Vec::new();
    {
        let mut rows = sqlx::query("SELECT occurrence.object_id,occurrence.media_uri,NULL source_index,NULL playlist_entry_id,occurrence.position canonical_position,provenance_kind,provenance_context_id,provenance_source_rank,occurrence.title,occurrence.artist,occurrence.album,occurrence.album_display_artist,track.artwork_binding,occurrence.duration_millis,occurrence.disc_number,occurrence.track_number,occurrence.year,occurrence.release_date,occurrence.source_format,occurrence.musicbrainz_recording_id,occurrence.musicbrainz_release_track_id,occurrence.musicbrainz_album_id,occurrence.musicbrainz_release_group_id,occurrence.primary_artist_musicbrainz_id,track.track_key IS NOT NULL known FROM queue_occurrences occurrence LEFT JOIN tracks track USING(media_uri) ORDER BY occurrence.traversal_position").fetch(&mut *connection);
        while let Some(raw) = rows.try_next().await? {
            let row = QueueOccurrenceRow::from_row(&raw)?;
            let occurrence = QueueOccurrence {
                occurrence: row.object_id.into(),
                item: row.item,
                source_index: Some(0),
                playlist_entry_id: None,
                canonical_position: choices.len(),
                provenance: QueueProvenance::parse(
                    &row.provenance_kind,
                    row.provenance_context_id,
                    row.provenance_source_rank,
                )?,
            };
            if (start..start + 100).contains(&choices.len()) {
                state.occurrences.push(Arc::new(occurrence.clone()));
            }
            choices.push(Some(QueueChoice {
                origin: None,
                media_uri: occurrence.media_uri.clone(),
                fallback: (!raw.try_get::<bool, _>("known")?).then_some(occurrence.item),
                provenance: occurrence.provenance,
            }));
        }
    }
    state.next_id = choices.len() as u64;
    if start + state.occurrences.len() < choices.len() {
        state.pending.push_back(QueueCursor {
            offset: start + state.occurrences.len(),
            ..Default::default()
        });
    }
    if !choices.is_empty() {
        state.sources.push(QueueInstruction {
            input: QueueInput::Choices(choices.into()),
            repeat: true,
            seed: None,
        });
    }
    read_saved_settings(connection, &mut state).await?;
    Ok(state)
}

async fn compact_legacy(
    connection: &mut sqlx::SqliteConnection,
    rows: Vec<QueueOccurrence>,
    selected: Option<usize>,
) -> LibraryResult<QueueRestore> {
    let mut state = QueueRestore::default();
    if rows.is_empty() {
        return Ok(state);
    }
    let start = selected.unwrap_or(0).saturating_sub(10);
    state.occurrences = rows
        .iter()
        .skip(start)
        .take(100)
        .cloned()
        .enumerate()
        .map(|(i, mut row)| {
            row.canonical_position = start + i;
            row.source_index = Some(0);
            Arc::new(row)
        })
        .collect();
    state.current_index = selected.map(|i| i - start);
    let total = rows.len();
    let mut choices = Vec::with_capacity(total);
    for row in rows {
        let known: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tracks WHERE media_uri=?1)")
                .bind(&row.media_uri)
                .fetch_one(&mut *connection)
                .await?;
        choices.push(QueueChoice {
            origin: None,
            media_uri: row.media_uri.clone(),
            fallback: (!known).then_some(row.item),
            provenance: row.provenance,
        });
    }
    state.sources.push(QueueInstruction {
        input: QueueInput::Choices(choices.into_iter().map(Some).collect()),
        repeat: true,
        seed: None,
    });
    if start + state.occurrences.len() < total {
        state.pending.push_back(QueueCursor {
            offset: start + state.occurrences.len(),
            ..QueueCursor::default()
        });
    }
    state.next_id = total as u64;
    Ok(state)
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

impl Database {
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
}

async fn read_saved_settings(
    connection: &mut sqlx::SqliteConnection,
    state: &mut QueueRestore,
) -> LibraryResult<()> {
    if let Some((current,progress,repeat,shuffled))=sqlx::query_as::<_,(Option<String>,i64,String,bool)>("SELECT current_occurrence_id,progress_millis,repeat_mode,shuffled FROM queue_state WHERE singleton=1").fetch_optional(connection).await? {
        state.current_index=current.and_then(|id|state.occurrences.iter().position(|row|row.occurrence.as_str()==id));state.progress_millis=progress;state.repeat_mode=QueueRepeatMode::parse(&repeat)?;state.shuffled=shuffled;
    }
    Ok(())
}

async fn read_saved_rows(
    connection: &mut sqlx::SqliteConnection,
) -> LibraryResult<Vec<Arc<QueueOccurrence>>> {
    sqlx::query_as::<_,QueueOccurrenceRow>("SELECT occurrence.object_id,occurrence.media_uri,origin_source source_index,playlist_entry_id,COALESCE(origin_position,position) canonical_position,provenance_kind,provenance_context_id,provenance_source_rank,occurrence.title,occurrence.artist,occurrence.album,occurrence.album_display_artist,track.artwork_binding,occurrence.duration_millis,occurrence.disc_number,occurrence.track_number,occurrence.year,occurrence.release_date,occurrence.source_format,occurrence.musicbrainz_recording_id,occurrence.musicbrainz_release_track_id,occurrence.musicbrainz_album_id,occurrence.musicbrainz_release_group_id,occurrence.primary_artist_musicbrainz_id FROM queue_occurrences occurrence LEFT JOIN tracks track USING(media_uri) ORDER BY position LIMIT 100")
        .fetch_all(connection).await?.into_iter().map(|row|Ok(Arc::new(QueueOccurrence{occurrence:OccurrenceId::new(row.object_id),item:row.item,source_index:row.source_index.map(|i|i as usize),playlist_entry_id:row.playlist_entry_id,canonical_position:row.canonical_position as usize,provenance:QueueProvenance::parse(&row.provenance_kind,row.provenance_context_id,row.provenance_source_rank)?}))).collect()
}
