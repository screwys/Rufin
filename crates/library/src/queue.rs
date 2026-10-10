//! Queue source reads, metadata, and saved playback state. Playback owns edits.

use std::ops::Deref;
use std::sync::Arc;

use sqlx::sqlite::SqliteRow;
use sqlx::{Connection, FromRow, Row, Sqlite};

use crate::schema::metadata_upsert;
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
pub enum QueueRepeatMode {
    #[default]
    Off,
    One,
    All,
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
pub struct QueueRow {
    pub position: usize,
    pub item: QueueItem,
    #[serde(default)]
    pub playlist_entry_id: Option<String>,
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
    Captured {
        entries: Arc<[QueueEntry]>,
        anchor_index: usize,
    },
    Choices(Arc<[Option<QueueChoice>]>),
    TrackSelection {
        query: QueueQuery,
        folder: Option<crate::FolderKey>,
        filter: String,
        sort: crate::TrackSort,
        descending: bool,
        ranges: Vec<std::ops::Range<usize>>,
    },
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
    PlaylistSelection {
        key: crate::PlaylistKey,
        filter: String,
        sort: crate::PlaylistEntrySort,
        descending: bool,
        ranges: Vec<std::ops::Range<usize>>,
        context_id: Arc<str>,
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
        #[serde(default)]
        downloaded_only: bool,
        recursive: bool,
    },
    Collection {
        collection: QueueCollection,
        favorites_only: bool,
        #[serde(default)]
        downloaded_only: bool,
    },
    Smart {
        key: crate::SmartPlaylistKey,
        source: Option<SourceKey>,
        now: i64,
    },
    SmartDisplay {
        key: crate::SmartPlaylistKey,
        source: Option<SourceKey>,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum QueueScope {
    Tracks {
        source: SourceId,
        folder: Option<String>,
        favorites_only: bool,
        #[serde(default)]
        downloaded_only: bool,
        recursive: bool,
    },
    Collection {
        reference: crate::CollectionSourceReference,
        favorites_only: bool,
        #[serde(default)]
        downloaded_only: bool,
    },
    Playlist {
        reference: crate::CollectionSourceReference,
        sort: crate::PlaylistEntrySort,
        anchor_entry: Option<String>,
    },
    Smart {
        reference: crate::SmartSourceReference,
        now: i64,
        #[serde(default)]
        display_sort: Option<crate::TrackSort>,
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
    pub media_uri: Arc<str>,
    pub playlist_entry_id: Option<Arc<str>>,
    pub provenance: QueueProvenance,
    #[serde(default)]
    pub received_from_connect: bool,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueRestore {
    pub entries: Arc<[QueueEntry]>,
    pub order: Arc<[u32]>,
    #[serde(skip)]
    pub rows: Vec<Arc<QueueRow>>,
    pub current_index: Option<usize>,
    pub progress_millis: i64,
    pub repeat_mode: QueueRepeatMode,
    pub shuffled: bool,
}

/// Compare playback order across devices without transferring queue contents.
pub fn queue_content_id(entries: &[QueueEntry], order: &[u32]) -> String {
    let mut hash = blake3::Hasher::new();
    for &index in order {
        let uri = entries[index as usize].media_uri.as_bytes();
        hash.update(&(uri.len() as u64).to_le_bytes());
        hash.update(uri);
    }
    hash.finalize().to_hex().to_string()
}

/// A page in play order, with each row's canonical member index.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct QueueTransferPage {
    pub offset: usize,
    pub rows: Vec<QueueRow>,
    pub order: Vec<u32>,
}

// Stored membership uses [URI, provenance, playlist identity, Connect origin] rows.
// Context provenance is [context index, source rank]; other provenance keeps its name.
// QueueRestore remains the public export format. Order and settings have their own tables.
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

struct SavedEntries<'a> {
    entries: &'a [QueueEntry],
    context_ids: &'a std::collections::HashMap<(&'a str, Option<&'a QueueContextTitle>), usize>,
}

impl serde::Serialize for SavedEntries<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.entries.iter().map(|entry| {
            let provenance = match &entry.provenance {
                QueueProvenance::Context {
                    context_id,
                    context_title,
                    source_rank,
                } => SavedProvenance::Context(
                    self.context_ids[&(context_id.as_ref(), context_title.as_deref())],
                    *source_rank,
                ),
                other => SavedProvenance::Other(other),
            };
            (
                &entry.media_uri,
                provenance,
                &entry.playlist_entry_id,
                entry.received_from_connect,
            )
        }))
    }
}

fn encode_saved_queue(state: &QueueRestore) -> LibraryResult<String> {
    let mut contexts = Vec::new();
    let mut context_ids = std::collections::HashMap::new();
    for entry in state.entries.iter() {
        if let QueueProvenance::Context {
            context_id,
            context_title,
            ..
        } = &entry.provenance
        {
            context_ids
                .entry((context_id.as_ref(), context_title.as_deref()))
                .or_insert_with(|| {
                    contexts.push(match context_title {
                        Some(title) => SavedContext::Titled {
                            id: context_id.as_ref(),
                            title: title.clone(),
                        },
                        None => SavedContext::Legacy(context_id.as_ref()),
                    });
                    contexts.len() - 1
                });
        }
    }
    Ok(serde_json::to_string(&SavedQueue {
        entries: SavedEntries {
            entries: &state.entries,
            context_ids: &context_ids,
        },
        contexts,
    })?)
}

fn decode_saved_queue(value: serde_json::Value) -> LibraryResult<QueueRestore> {
    if value.get("contexts").is_none() {
        return Ok(serde_json::from_value(value)?);
    }
    type SavedEntry = (
        Arc<str>,
        SavedProvenance<QueueProvenance>,
        Option<Arc<str>>,
        bool,
    );
    let saved: SavedQueue<Vec<SavedEntry>, Vec<SavedContext<Arc<str>>>> =
        serde_json::from_value(value)?;
    let entries = saved
        .entries
        .into_iter()
        .map(
            |(media_uri, provenance, playlist_entry_id, received_from_connect)| {
                Ok(QueueEntry {
                    received_from_connect,
                    media_uri,
                    playlist_entry_id,
                    provenance: match provenance {
                        SavedProvenance::Context(context, source_rank) => {
                            let (id, title) =
                                match saved.contexts.get(context).ok_or_else(|| {
                                    LibraryError::InvalidStore(
                                        "saved Queue context is missing".into(),
                                    )
                                })? {
                                    SavedContext::Titled { id, title } => (id, Some(title.clone())),
                                    SavedContext::Legacy(id) => (id, None),
                                };
                            QueueProvenance::Context {
                                context_id: id.clone(),
                                context_title: title,
                                source_rank,
                            }
                        }
                        SavedProvenance::Other(provenance) => provenance,
                    },
                })
            },
        )
        .collect::<LibraryResult<Vec<_>>>()?;
    Ok(QueueRestore {
        entries: entries.into(),
        ..Default::default()
    })
}

#[derive(Clone, Debug, PartialEq)]
pub enum QueueReadRequest {
    Capture {
        input: Box<QueueInput>,
        context_title: Option<Arc<QueueContextTitle>>,
        anchor_index: usize,
        random_start: Option<u64>,
        shuffled: Option<u64>,
    },
    Hydrate {
        entries: Vec<(usize, QueueEntry)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueReadPage {
    pub entries: Vec<QueueEntry>,
    pub rows: Vec<Arc<QueueRow>>,
    pub current_index: usize,
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum QueuePlacement {
    Replace { anchor_index: usize },
    AfterCurrent,
    End,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum QueueReorderTarget {
    Before(usize),
    After(usize),
    End,
}
#[derive(Clone, Debug, PartialEq)]
pub struct QueuePageRow {
    pub entry: Arc<QueueRow>,
    pub audio_properties: crate::AudioProperties,
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
               LEFT JOIN queue_occurrences queued ON queued.media_uri=requested.media_uri
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
        current_index: Option<usize>,
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
                 current_index=?1,
                 progress_millis=?2
             WHERE singleton=1",
        )
        .bind(current_index.map(|index| index as i64))
        .bind(progress_millis)
        .execute(connection)
        .await?
        .rows_affected()
            == 1;
        Ok(changed)
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
            .push(",COALESCE(track.audio_properties,'{}'),track.source_path,(SELECT display_name FROM sources WHERE source_key=track.source_key)")
            .push(" FROM requested LEFT JOIN tracks track ON track.media_uri=requested.media_uri ORDER BY ordinal");
        let mut connection = self.acquire_reader().await?;
        let facts = query
            .build_query_as::<(
                i64,
                bool,
                Option<String>,
                crate::AudioProperties,
                Option<String>,
                Option<String>,
            )>()
            .fetch_all(&mut *connection)
            .await?;
        Ok(facts
            .into_iter()
            .map(
                |(
                    ordinal,
                    favorite,
                    primary_artist_media_uri,
                    audio_properties,
                    source_path,
                    source_name,
                )| QueuePageRow {
                    entry: Arc::clone(&window[ordinal as usize]),
                    audio_properties,
                    source_path,
                    source_name,
                    favorite,
                    primary_artist_media_uri,
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
        let sql = format!(
            "WITH requested AS (SELECT CAST(key AS INTEGER) position,value media_uri FROM json_each(?1)), {snapshots}
             SELECT requested.position FROM requested
             LEFT JOIN tracks track USING(media_uri)
             LEFT JOIN playlist_snapshots entry ON entry.media_uri=requested.media_uri
             LEFT JOIN queue_occurrences metadata ON metadata.media_uri=requested.media_uri
             LEFT JOIN listens listen ON listen.listen_key=(SELECT listen_key FROM listens WHERE media_uri=requested.media_uri ORDER BY started_at DESC,listen_key DESC LIMIT 1)
             WHERE instr(track.normalized_search,?2)>0
                OR instr(lower(COALESCE(track.title,entry.title,metadata.title,listen.track_title,requested.media_uri) || ' ' || COALESCE(track.display_artist,entry.artist,metadata.artist,listen.artist_name,'') || ' ' || COALESCE(track.display_album,entry.album,metadata.album,listen.album_title,'')),?2)>0
             ORDER BY requested.position",
            snapshots = crate::playlists::PLAYLIST_URI_SNAPSHOTS,
        );
        let mut positions = Vec::new();
        let mut total = 0;
        for (page, order) in queue.order.chunks(4096).enumerate() {
            let uris = order
                .iter()
                .map(|index| queue.entries[*index as usize].media_uri.as_ref())
                .collect::<Vec<_>>();
            let matches = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql.as_str()))
                .bind(serde_json::to_string(&uris)?)
                .bind(&filter)
                .fetch_all(&mut *transaction)
                .await?;
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
            .queue_rows(&requested)
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
        let mut reader = self.acquire_reader().await?;
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
                    media_uri: row.item.media_uri.clone().into(),
                    playlist_entry_id: row.playlist_entry_id.clone().map(Into::into),
                    provenance: row.provenance.clone(),
                });
                if row.position == current {
                    row.item.artwork_binding = sqlx::query_scalar::<_, Option<Vec<u8>>>(
                        "SELECT artwork_binding FROM tracks WHERE media_uri=?1",
                    )
                    .bind(&row.item.media_uri)
                    .fetch_optional(&mut *reader)
                    .await?
                    .flatten();
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
    pub async fn queue_input_from_transfer(
        &self,
        transfer: &str,
        total: usize,
        current: usize,
    ) -> LibraryResult<QueueInput> {
        let queue = self
            .prepare_queue_transfer(transfer, total, current)
            .await?;
        self.import_queue_transfer_metadata(transfer, total).await?;
        Ok(QueueInput::Captured {
            entries: queue
                .order
                .iter()
                .map(|index| queue.entries[*index as usize].clone())
                .collect(),
            anchor_index: queue.current_index.unwrap_or_default(),
        })
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

    pub async fn read_queue(&self, request: QueueReadRequest) -> LibraryResult<QueueReadPage> {
        match request {
            QueueReadRequest::Capture {
                input,
                context_title,
                anchor_index,
                random_start,
                shuffled,
            } => {
                let mut entries = Vec::new();
                let mut anchor = None;
                let mut writer = self.writer().await?;
                let mut transaction = writer
                    .as_mut()
                    .ok_or(LibraryError::WriterUnavailable)?
                    .begin()
                    .await?;
                capture_input(&mut transaction, *input, &mut entries, &mut anchor).await?;
                if let Some(title) = context_title {
                    for entry in &mut entries {
                        if let QueueProvenance::Context { context_title, .. } =
                            &mut entry.provenance
                        {
                            *context_title = Some(title.clone());
                        }
                    }
                }
                transaction.commit().await?;
                drop(writer);
                if let Some(seed) = shuffled {
                    shuffle_order(&mut entries, seed);
                    anchor = Some(0);
                }
                let current_index = random_start
                    .filter(|_| !entries.is_empty())
                    .map(|seed| seed as usize % entries.len())
                    .unwrap_or_else(|| {
                        anchor
                            .unwrap_or(anchor_index)
                            .min(entries.len().saturating_sub(1))
                    });
                let requested = entries
                    .get(current_index)
                    .cloned()
                    .map(|entry| (current_index, entry))
                    .into_iter()
                    .collect::<Vec<_>>();
                let rows = self.queue_rows(&requested).await?;
                Ok(QueueReadPage {
                    entries,
                    rows,
                    current_index,
                })
            }
            QueueReadRequest::Hydrate { entries } => {
                let rows = self.queue_rows(&entries).await?;
                Ok(QueueReadPage {
                    entries: entries.into_iter().map(|(_, entry)| entry).collect(),
                    rows,
                    current_index: 0,
                })
            }
        }
    }

    async fn queue_rows(
        &self,
        entries: &[(usize, QueueEntry)],
    ) -> LibraryResult<Vec<Arc<QueueRow>>> {
        let uris = entries
            .iter()
            .map(|(_, entry)| entry.media_uri.to_string())
            .collect::<Vec<_>>();
        let items = self
            .queue_items_for_uris(&uris, &ReadCancellation::new())
            .await?;
        Ok(entries
            .iter()
            .zip(items)
            .map(|((position, entry), item)| {
                Arc::new(QueueRow {
                    position: *position,
                    item,
                    playlist_entry_id: entry.playlist_entry_id.as_ref().map(ToString::to_string),
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
            state.current_index,
            state.progress_millis,
            state.repeat_mode,
            state.shuffled,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn restore_queue(&self) -> LibraryResult<QueueRestore> {
        let mut reader = self.acquire_reader().await?;
        let mut transaction = reader.begin().await?;
        let mut state =
            sqlx::query_scalar::<_, String>("SELECT state FROM queue_saved WHERE singleton=1")
                .fetch_optional(&mut *transaction)
                .await?
                .map(|json| decode_saved_queue(serde_json::from_str(&json)?))
                .transpose()?
                .unwrap_or_default();
        if let Some(order) =
            sqlx::query_scalar::<_, String>("SELECT state FROM queue_order WHERE singleton=1")
                .fetch_optional(&mut *transaction)
                .await?
        {
            state.order = serde_json::from_str(&order)?;
        }
        read_settings(&mut transaction, &mut state).await?;
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
        state.rows = self.queue_rows(&entries).await?;
        Ok(state)
    }

    pub async fn persist_queue_settings(
        &self,
        current: Option<usize>,
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
        let mut positions = Vec::new();
        for (page, order) in queue.order.chunks(4096).enumerate() {
            let uris = order
                .iter()
                .map(|index| queue.entries[*index as usize].media_uri.as_ref())
                .collect::<Vec<_>>();
            let matches = sqlx::query_scalar::<_, i64>(
                "SELECT CAST(requested.key AS INTEGER) FROM json_each(?1) requested
                 WHERE substr(requested.value,1,length(?2))=?2
                    OR EXISTS(SELECT 1 FROM tracks WHERE source_key=?3 AND media_uri=requested.value)")
                .bind(serde_json::to_string(&uris)?).bind(&prefix).bind(source).fetch_all(&mut *connection).await?;
            positions.extend(
                matches
                    .into_iter()
                    .map(|position| page * 4096 + position as usize),
            );
        }
        Ok(positions)
    }
}

async fn playlist_queue_items(
    connection: &mut sqlx::Transaction<'_, Sqlite>,
    keys: &[crate::PlaylistEntryKey],
) -> LibraryResult<Vec<(usize, QueueItem)>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = crate::playlists::playlist_entries_query(keys);
    query.push("entry.ordinal,entry.media_uri,
        COALESCE(track.title,entry.title,'') title,
        COALESCE(track.display_artist,entry.artist,'') artist,
        COALESCE(track.display_album,entry.album,'') album,
        COALESCE(album.display_artist,entry.album_display_artist) album_display_artist,
        track.artwork_binding,COALESCE(track.duration_millis,entry.duration_millis,0) duration_millis,
        COALESCE(track.disc_number,entry.disc_number) disc_number,
        COALESCE(track.track_number,entry.track_number) track_number,
        COALESCE(track.year,entry.year) year,
        COALESCE(track.release_date,entry.release_date) release_date,
        COALESCE(track.source_format,entry.source_format) source_format,
        COALESCE(track.musicbrainz_recording_id,entry.musicbrainz_recording_id) musicbrainz_recording_id,
        COALESCE(track.musicbrainz_release_track_id,entry.musicbrainz_release_track_id) musicbrainz_release_track_id,
        NULL musicbrainz_album_id,NULL musicbrainz_release_group_id,NULL primary_artist_musicbrainz_id
        FROM entries entry LEFT JOIN tracks track USING(media_uri)
        LEFT JOIN albums album USING(album_key) ORDER BY entry.ordinal");
    query
        .build()
        .persistent(false)
        .fetch_all(&mut **connection)
        .await?
        .iter()
        .map(|row| {
            Ok((
                row.try_get::<i64, _>("ordinal")? as usize,
                QueueItem::from_row(row)?,
            ))
        })
        .collect()
}

async fn capture_input(
    connection: &mut sqlx::Transaction<'_, Sqlite>,
    input: QueueInput,
    entries: &mut Vec<QueueEntry>,
    anchor: &mut Option<usize>,
) -> LibraryResult<()> {
    let input = match input {
        selection @ QueueInput::PlaylistSelection { .. } => {
            let order =
                crate::playlists::selected_playlist_entries_on(connection, &selection).await?;
            let QueueInput::PlaylistSelection { context_id, .. } = selection else {
                unreachable!()
            };
            QueueInput::PlaylistEntries {
                order: order.into(),
                context_id,
            }
        }
        QueueInput::TrackSelection {
            query,
            folder,
            filter,
            sort,
            descending,
            ranges,
        } => {
            for uri in crate::tracks::query_selected_track_uris_on(
                connection, &query, folder, &filter, sort, descending, &ranges,
            )
            .await?
            {
                entries.push(QueueEntry {
                    media_uri: uri.into(),
                    playlist_entry_id: None,
                    provenance: QueueProvenance::Manual,
                    received_from_connect: false,
                });
            }
            return Ok(());
        }
        QueueInput::Groups(inputs) => {
            for input in inputs {
                Box::pin(capture_input(connection, input, entries, anchor)).await?;
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
                            downloaded_only: false,
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
            return Box::pin(capture_input(connection, input, entries, anchor)).await;
        }
        QueueInput::Smart {
            key,
            source,
            folder,
            now,
            context_id,
        } => {
            return Box::pin(capture_input(
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
                entries,
                anchor,
            ))
            .await;
        }
        input => input,
    };
    match input {
        QueueInput::Captured {
            entries: captured,
            anchor_index,
        } => {
            *anchor = Some(entries.len() + anchor_index);
            entries.extend(captured.iter().cloned());
        }
        QueueInput::Source {
            reference,
            context_id,
        } => {
            let rows = crate::source_window::source_members(connection, &reference).await?;
            for (rank, (uri, identity, selected)) in rows.into_iter().enumerate() {
                if selected {
                    *anchor = Some(entries.len());
                }
                entries.push(QueueEntry {
                    media_uri: uri.into(),
                    playlist_entry_id: identity.map(Into::into),
                    provenance: QueueProvenance::Context {
                        context_id: context_id.clone(),
                        context_title: None,
                        source_rank: rank,
                    },
                    received_from_connect: false,
                });
            }
        }
        QueueInput::PlaylistEntries { order, context_id } => {
            for (chunk_index, keys) in order.chunks(QUEUE_CONTEXT_LIMIT).enumerate() {
                for (ordinal, item) in playlist_queue_items(connection, keys).await? {
                    let identity = playlist_identity(connection, keys[ordinal]).await?;
                    persist_metadata(connection, &item).await?;
                    entries.push(QueueEntry {
                        media_uri: item.media_uri.into(),
                        playlist_entry_id: identity.map(Into::into),
                        provenance: QueueProvenance::Context {
                            context_id: context_id.clone(),
                            context_title: None,
                            source_rank: chunk_index * QUEUE_CONTEXT_LIMIT + ordinal,
                        },
                        received_from_connect: false,
                    });
                }
            }
        }
        QueueInput::Items(items) => {
            for (item, provenance) in items {
                persist_metadata(connection, &item).await?;
                entries.push(QueueEntry {
                    media_uri: item.media_uri.into(),
                    playlist_entry_id: None,
                    provenance,
                    received_from_connect: false,
                });
            }
        }
        QueueInput::Choices(choices) => {
            for choice in choices.iter().flatten() {
                if let Some(item) = &choice.fallback {
                    persist_metadata(connection, item).await?;
                }
                entries.push(QueueEntry {
                    media_uri: choice.media_uri.clone().into(),
                    playlist_entry_id: None,
                    provenance: choice.provenance.clone(),
                    received_from_connect: false,
                });
            }
        }
        QueueInput::MediaUris { order, provenance } => {
            entries.extend(order.iter().map(|uri| QueueEntry {
                media_uri: uri.clone().into(),
                playlist_entry_id: None,
                provenance: provenance.clone(),
                received_from_connect: false,
            }));
        }
        QueueInput::Uris {
            order,
            context_id,
            source_start,
        } => {
            entries.extend(order.iter().enumerate().map(|(index, uri)| QueueEntry {
                media_uri: uri.clone().into(),
                playlist_entry_id: None,
                provenance: QueueProvenance::Context {
                    context_id: context_id.clone(),
                    context_title: None,
                    source_rank: source_start + index,
                },
                received_from_connect: false,
            }));
        }
        _ => unreachable!("source inputs normalized above"),
    }
    Ok(())
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
async fn playlist_identity(
    connection: &mut sqlx::SqliteConnection,
    key: crate::PlaylistEntryKey,
) -> LibraryResult<Option<String>> {
    let sql = if key.raw() < 0 {
        "SELECT json_array(source.object_id,playlist.object_id,entry.object_id) FROM catalog.native_playlist_entries entry JOIN catalog.native_playlists playlist USING(playlist_key) JOIN catalog.sources source USING(source_key) WHERE entry.playlist_entry_key=?1"
    } else {
        "SELECT json_array(source.object_id,playlist.object_id,entry.object_id) FROM main.playlist_entries entry JOIN main.playlists playlist USING(playlist_key) LEFT JOIN main.source_ids source USING(source_key) WHERE entry.playlist_entry_key=?1"
    };
    Ok(sqlx::query_scalar(sql)
        .bind(key.raw().abs())
        .fetch_optional(connection)
        .await?)
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
async fn save_queue_on(
    connection: &mut sqlx::SqliteConnection,
    state: &QueueRestore,
) -> LibraryResult<()> {
    sqlx::query("INSERT INTO queue_saved(singleton,state) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET state=excluded.state")
        .bind(encode_saved_queue(state)?).execute(&mut *connection).await?;
    sqlx::query("INSERT INTO queue_order(singleton,state) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET state=excluded.state")
        .bind(serde_json::to_string(&state.order)?).execute(&mut *connection).await?;
    save_settings(
        connection,
        state.current_index,
        state.progress_millis,
        state.repeat_mode,
        state.shuffled,
    )
    .await
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
    pub occurrence: String,
    pub item: QueueItem,
    pub canonical_position: usize,
    #[serde(default)]
    pub source_index: Option<usize>,
    #[serde(default)]
    pub playlist_entry_id: Option<String>,
    pub provenance: QueueProvenance,
}

impl LegacyQueueRow {
    fn entry(&self) -> QueueEntry {
        QueueEntry {
            media_uri: self.item.media_uri.clone().into(),
            playlist_entry_id: self.playlist_entry_id.clone().map(Into::into),
            provenance: self.provenance.clone(),
            received_from_connect: false,
        }
    }
}

async fn migrate_saved(
    connection: &mut sqlx::Transaction<'_, Sqlite>,
    value: serde_json::Value,
    rows: &[LegacyQueueRow],
) -> LibraryResult<QueueRestore> {
    let sources: Vec<LegacyQueueInstruction> = serde_json::from_value(value["sources"].clone())?;
    let pending: Vec<LegacyQueueCursor> = serde_json::from_value(value["pending"].clone())?;
    let mut entries = Vec::new();
    let mut members: Vec<Vec<Option<usize>>> = Vec::new();
    let mut anchors = Vec::new();
    for source in &sources {
        let mut captured = Vec::new();
        let mut anchor = None;
        capture_input(connection, source.input.clone(), &mut captured, &mut anchor).await?;
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
            .filter(|index| entries[*index].media_uri.as_ref() == row.item.media_uri);
        let index = candidate.unwrap_or_else(|| {
            let index = entries.len();
            entries.push(row.entry());
            index
        });
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
    let current_index =
        selected.and_then(|selected| order.iter().position(|index| *index == selected));
    let mut state = QueueRestore {
        entries: entries.into(),
        order: order.into(),
        current_index,
        ..Default::default()
    };
    state.progress_millis = value["progress_millis"].as_i64().unwrap_or(0);
    state.repeat_mode = serde_json::from_value(value["repeat_mode"].clone())?;
    state.shuffled = value["shuffled"].as_bool().unwrap_or(false);
    Ok(state)
}
pub(crate) async fn export_queue_jsonl_on(
    connection: &mut sqlx::SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<()> {
    let mut state =
        sqlx::query_scalar::<_, String>("SELECT state FROM queue_saved WHERE singleton=1")
            .fetch_optional(&mut *connection)
            .await?
            .map(|json| decode_saved_queue(serde_json::from_str(&json)?))
            .transpose()?
            .unwrap_or_default();
    if let Some(order) =
        sqlx::query_scalar::<_, String>("SELECT state FROM queue_order WHERE singleton=1")
            .fetch_optional(&mut *connection)
            .await?
    {
        state.order = serde_json::from_str(&order)?;
    }
    read_settings(connection, &mut state).await?;
    serde_json::to_writer(&mut output, &serde_json::json!({"version":4,"queue":state}))?;
    output.write_all(b"\n")?;
    use futures_util::TryStreamExt;
    let mut records = sqlx::query_as::<_, QueueItem>("SELECT metadata.*,NULL artwork_binding FROM queue_occurrences metadata ORDER BY queue_occurrence_key").fetch(connection);
    while let Some(item) = records.try_next().await? {
        serde_json::to_writer(&mut output, &item)?;
        output.write_all(b"\n")?;
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
            let state = serde_json::from_value(header["queue"].clone())?;
            while {
                line.clear();
                input.read_line(&mut line)? != 0
            } {
                if !line.trim().is_empty() {
                    let item: QueueItem = serde_json::from_str(&line)?;
                    persist_metadata(connection, &item).await?;
                }
            }
            state
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
            for row in &rows {
                persist_metadata(connection, &row.item).await?;
            }
            match version {
                3 => serde_json::from_value(header["queue"].clone())?,
                2 => {
                    let mut transaction = connection.begin().await?;
                    let state =
                        migrate_saved(&mut transaction, header["queue"].clone(), &rows).await?;
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
                    entries: rows.iter().map(LegacyQueueRow::entry).collect(),
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
    connection: &mut sqlx::SqliteConnection,
    item: &QueueItem,
) -> LibraryResult<()> {
    sqlx::query(metadata_upsert!(
        "INSERT INTO queue_occurrences(
            media_uri,title,artist,album,album_display_artist,duration_millis,
            disc_number,track_number,year,release_date,source_format,
            musicbrainz_recording_id,musicbrainz_release_track_id,musicbrainz_album_id,
            musicbrainz_release_group_id,primary_artist_musicbrainz_id)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)"
    ))
    .bind(&item.media_uri)
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
    .execute(connection)
    .await?;
    Ok(())
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
