//! Persists one compact queue with separate canonical and traversal order.
//! Playback resolution uses the bypass reader and never scans the Library catalog.

use std::borrow::Borrow;
use std::collections::BTreeMap;

use sqlx::sqlite::SqliteRow;
use sqlx::{Connection, FromRow, QueryBuilder, Row, Sqlite};

use crate::{
    AlbumKey, ArtistKey, Database, LibraryError, LibraryResult, QueueOccurrenceKey,
    ReadCancellation, SourceKey, TrackArtistLink, TrackKey,
};

const QUEUE_PAGE_LIMIT: usize = 256;
const TRAVERSAL_WRITE_BATCH: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueueProvenance {
    Context {
        context_id: String,
        source_rank: i64,
    },
    Manual,
    Random,
    Radio,
    AutoDj,
    Legacy,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QueueRepeatMode {
    #[default]
    None,
    One,
    All,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueState {
    pub current: Option<QueueOccurrenceKey>,
    pub prepared_next: Option<QueueOccurrenceKey>,
    pub progress_millis: i64,
    pub repeat_mode: QueueRepeatMode,
    pub shuffled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueMedia {
    pub occurrence_key: QueueOccurrenceKey,
    pub source_id: String,
    pub track_key: Option<TrackKey>,
    pub track_object_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_display_artist: Option<String>,
    pub album_key: Option<AlbumKey>,
    pub album_object_id: Option<String>,
    pub primary_artist_key: Option<ArtistKey>,
    pub primary_artist_object_id: Option<String>,
    pub primary_artist_musicbrainz_id: Option<String>,
    pub download_media_uri: Option<String>,
    pub mapping_media_uri: Option<String>,
    pub source_media_uri: Option<String>,
    pub media_uri: Option<String>,
    pub artwork_binding: Option<Vec<u8>>,
    pub duration_millis: Option<i64>,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub favorite: Option<bool>,
    pub source_format: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub musicbrainz_album_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub cue_path: Option<String>,
    pub cue_start_millis: Option<i64>,
    pub cue_end_millis: Option<i64>,
    pub artist_links: Vec<TrackArtistLink>,
}

impl<'row> FromRow<'row, SqliteRow> for QueueMedia {
    fn from_row(row: &'row SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            occurrence_key: row.try_get("occurrence_key")?,
            source_id: String::new(),
            track_key: row.try_get("track_key")?,
            track_object_id: row.try_get("track_object_id")?,
            title: row.try_get("title")?,
            artist: row.try_get("artist")?,
            album: row.try_get("album")?,
            album_display_artist: row.try_get("album_display_artist")?,
            album_key: row.try_get("album_key")?,
            album_object_id: row.try_get("album_object_id")?,
            primary_artist_key: row.try_get("primary_artist_key")?,
            primary_artist_object_id: row.try_get("primary_artist_object_id")?,
            primary_artist_musicbrainz_id: row.try_get("primary_artist_musicbrainz_id")?,
            download_media_uri: row.try_get("download_media_uri")?,
            mapping_media_uri: row.try_get("mapping_media_uri")?,
            source_media_uri: row.try_get("source_media_uri")?,
            media_uri: row.try_get("media_uri")?,
            artwork_binding: row.try_get("artwork_binding")?,
            duration_millis: row.try_get("duration_millis")?,
            disc_number: row.try_get("disc_number")?,
            track_number: row.try_get("track_number")?,
            year: row.try_get("year")?,
            release_date: row.try_get("release_date")?,
            favorite: row.try_get("favorite")?,
            source_format: row.try_get("source_format")?,
            musicbrainz_recording_id: row.try_get("musicbrainz_recording_id")?,
            musicbrainz_release_track_id: row.try_get("musicbrainz_release_track_id")?,
            musicbrainz_album_id: row.try_get("musicbrainz_album_id")?,
            musicbrainz_release_group_id: row.try_get("musicbrainz_release_group_id")?,
            cue_path: row.try_get("cue_path")?,
            cue_start_millis: row.try_get("cue_start_millis")?,
            cue_end_millis: row.try_get("cue_end_millis")?,
            artist_links: Vec::new(),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueCurrentNext {
    pub current: Option<QueueMedia>,
    pub prepared_next: Option<QueueMedia>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueRestore {
    pub occurrences: Vec<QueueCompactOccurrence>,
    pub state: QueueState,
    pub current: Option<QueueMedia>,
    pub prepared_next: Option<QueueMedia>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueCompactOccurrence {
    pub occurrence_key: Option<QueueOccurrenceKey>,
    pub object_id: String,
    pub track_key: Option<TrackKey>,
    pub canonical_position: i64,
    pub traversal_position: i64,
    pub provenance: QueueProvenance,
}

#[derive(FromRow)]
struct QueueCompactScalar {
    occurrence_key: QueueOccurrenceKey,
    object_id: String,
    track_key: Option<TrackKey>,
    canonical_position: i64,
    traversal_position: i64,
    provenance_kind: String,
    provenance_context_id: Option<String>,
    provenance_source_rank: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueuePageRow {
    pub occurrence_key: QueueOccurrenceKey,
    pub object_id: String,
    pub position: i64,
    pub traversal_position: i64,
    pub provenance: QueueProvenance,
    pub track_key: Option<TrackKey>,
    pub track_object_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_display_artist: Option<String>,
    pub album_key: Option<AlbumKey>,
    pub album_object_id: Option<String>,
    pub primary_artist_key: Option<ArtistKey>,
    pub primary_artist_object_id: Option<String>,
    pub primary_artist_musicbrainz_id: Option<String>,
    pub media_uri: Option<String>,
    pub artwork_binding: Option<Vec<u8>>,
    pub duration_millis: Option<i64>,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub favorite: Option<bool>,
    pub rating: Option<i64>,
    pub is_downloaded: bool,
    pub source_format: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub musicbrainz_album_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub cue_path: Option<String>,
    pub cue_start_millis: Option<i64>,
    pub cue_end_millis: Option<i64>,
}

#[derive(FromRow)]
struct QueuePageScalar {
    occurrence_key: QueueOccurrenceKey,
    object_id: String,
    position: i64,
    traversal_position: i64,
    provenance_kind: String,
    provenance_context_id: Option<String>,
    provenance_source_rank: Option<i64>,
    track_key: Option<TrackKey>,
    track_object_id: String,
    title: String,
    artist: String,
    album: String,
    album_display_artist: Option<String>,
    album_object_id: Option<String>,
    primary_artist_object_id: Option<String>,
    primary_artist_musicbrainz_id: Option<String>,
    media_uri: Option<String>,
    artwork_binding: Option<Vec<u8>>,
    duration_millis: Option<i64>,
    disc_number: Option<i64>,
    track_number: Option<i64>,
    year: Option<i64>,
    release_date: Option<String>,
    favorite: Option<bool>,
    rating: Option<i64>,
    is_downloaded: bool,
    source_format: Option<String>,
    musicbrainz_recording_id: Option<String>,
    musicbrainz_release_track_id: Option<String>,
    musicbrainz_album_id: Option<String>,
    musicbrainz_release_group_id: Option<String>,
    cue_path: Option<String>,
    cue_start_millis: Option<i64>,
    cue_end_millis: Option<i64>,
}

#[derive(FromRow)]
struct QueuePageIdentity {
    track_key: TrackKey,
    album_key: Option<AlbumKey>,
    primary_artist_key: Option<ArtistKey>,
}

impl QueueRepeatMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::One => "one",
            Self::All => "all",
        }
    }

    fn parse(value: &str) -> LibraryResult<Self> {
        match value {
            "none" => Ok(Self::None),
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
            } => ("context", Some(context_id), Some(*source_rank)),
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
                context_id: context_id.ok_or_else(|| {
                    LibraryError::InvalidStore("queue Context has no context ID".to_string())
                })?,
                source_rank: source_rank.ok_or_else(|| {
                    LibraryError::InvalidStore("queue Context has no source rank".to_string())
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
    #[allow(clippy::too_many_arguments)]
    pub async fn persist_compact_queue<I>(
        &self,
        source: SourceKey,
        occurrences: I,
        current_object_id: Option<&str>,
        prepared_next_object_id: Option<&str>,
        progress_millis: i64,
        repeat_mode: QueueRepeatMode,
        shuffled: bool,
    ) -> LibraryResult<Vec<QueueOccurrenceKey>>
    where
        I: IntoIterator,
        I::Item: Borrow<QueueCompactOccurrence>,
        I::IntoIter: ExactSizeIterator,
    {
        let occurrences = occurrences.into_iter();
        let len = occurrences.len() as i64;
        if progress_millis < 0 {
            return Err(LibraryError::InvalidRequest(
                "invalid compact Queue order".to_string(),
            ));
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let offset = sqlx::query_scalar::<_, i64>(
            "SELECT count(*)+?2+1 FROM queue_occurrences WHERE source_key=?1",
        )
        .bind(source)
        .bind(len)
        .fetch_one(&mut *transaction)
        .await?;
        sqlx::query("UPDATE queue_occurrences SET position=position+?2,traversal_position=traversal_position+?2 WHERE source_key=?1").bind(source).bind(offset).execute(&mut *transaction).await?;
        for occurrence in occurrences {
            let occurrence = occurrence.borrow();
            if occurrence.object_id.is_empty()
                || occurrence.canonical_position < 0
                || occurrence.canonical_position >= len
                || occurrence.traversal_position < 0
                || occurrence.traversal_position >= len
            {
                transaction.rollback().await?;
                return Err(LibraryError::InvalidRequest(
                    "invalid compact Queue order".to_string(),
                ));
            }
            let (kind, context, rank) = occurrence.provenance.columns();
            let updated = if let Some(key) = occurrence.occurrence_key {
                sqlx::query("UPDATE queue_occurrences SET position=?4,traversal_position=?5,provenance_kind=?6,provenance_context_id=?7,provenance_source_rank=?8,track_key=COALESCE(?9,track_key) WHERE source_key=?1 AND queue_occurrence_key=?2 AND object_id=?3")
                    .bind(source).bind(key).bind(&occurrence.object_id).bind(occurrence.canonical_position).bind(occurrence.traversal_position).bind(kind).bind(context).bind(rank).bind(occurrence.track_key).execute(&mut *transaction).await?.rows_affected()==1
            } else {
                sqlx::query("UPDATE queue_occurrences SET position=?3,traversal_position=?4,provenance_kind=?5,provenance_context_id=?6,provenance_source_rank=?7,track_key=COALESCE(?8,track_key) WHERE source_key=?1 AND object_id=?2")
                    .bind(source).bind(&occurrence.object_id).bind(occurrence.canonical_position).bind(occurrence.traversal_position).bind(kind).bind(context).bind(rank).bind(occurrence.track_key).execute(&mut *transaction).await?.rows_affected()==1
            };
            if !updated {
                let Some(track_key) = occurrence.track_key else {
                    transaction.rollback().await?;
                    return Err(LibraryError::InvalidRequest(
                        "a new Queue occurrence requires a current Track".to_string(),
                    ));
                };
                let inserted=sqlx::query("INSERT INTO queue_occurrences(source_key,object_id,position,traversal_position,provenance_kind,provenance_context_id,provenance_source_rank,track_key,track_object_id,fallback_title,fallback_artist,fallback_album,fallback_album_display_artist,fallback_album_object_id,fallback_primary_artist_object_id,fallback_media_uri,fallback_artwork_binding,fallback_duration_millis,fallback_disc_number,fallback_track_number,fallback_year,fallback_release_date,fallback_favorite,fallback_source_format,fallback_musicbrainz_recording_id,fallback_cue_path,fallback_cue_start_millis,fallback_cue_end_millis) SELECT ?1,?2,?3,?4,?5,?6,?7,track.track_key,track.object_id,track.title,track.display_artist,track.display_album,album.display_artist,album.object_id,COALESCE((SELECT artist.object_id FROM track_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1),(SELECT artist.object_id FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=track.album_key ORDER BY credit.position LIMIT 1)),COALESCE((SELECT local.media_uri FROM local_access_files local WHERE local.source_key=track.source_key AND local.track_object_id=track.object_id ORDER BY CASE local.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,local.local_access_file_key LIMIT 1),track.media_uri),COALESCE(track.artwork_binding,album.artwork_binding),track.duration_millis,track.disc_number,track.track_number,track.year,track.release_date,COALESCE(track.user_favorite,track.source_favorite),track.source_format,track.musicbrainz_recording_id,track.cue_path,track.cue_start_millis,track.cue_end_millis FROM tracks track LEFT JOIN albums album USING(album_key) WHERE track.source_key=?1 AND track.track_key=?8")
                    .bind(source).bind(&occurrence.object_id).bind(occurrence.canonical_position).bind(occurrence.traversal_position).bind(kind).bind(context).bind(rank).bind(track_key).execute(&mut *transaction).await?.rows_affected();
                if inserted != 1 {
                    transaction.rollback().await?;
                    return Err(LibraryError::InvalidRequest(
                        "compact Queue Track is not current".to_string(),
                    ));
                }
                sqlx::query("UPDATE queue_occurrences SET
                    fallback_musicbrainz_release_track_id=(SELECT musicbrainz_release_track_id FROM tracks WHERE source_key=?1 AND track_key=?3),
                    fallback_musicbrainz_album_id=(SELECT album.musicbrainz_release_id FROM tracks track JOIN albums album USING(album_key) WHERE track.source_key=?1 AND track.track_key=?3),
                    fallback_musicbrainz_release_group_id=(SELECT album.musicbrainz_release_group_id FROM tracks track JOIN albums album USING(album_key) WHERE track.source_key=?1 AND track.track_key=?3),
                    fallback_primary_artist_musicbrainz_id=COALESCE(
                      (SELECT artist.musicbrainz_artist_id FROM track_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=?3 ORDER BY credit.position LIMIT 1),
                      (SELECT artist.musicbrainz_artist_id FROM tracks track JOIN album_artists credit USING(album_key) JOIN artists artist USING(artist_key) WHERE track.track_key=?3 ORDER BY credit.position LIMIT 1)
                    ) WHERE source_key=?1 AND object_id=?2")
                    .bind(source).bind(&occurrence.object_id).bind(track_key)
                    .execute(&mut *transaction).await?;
            }
        }
        sqlx::query("DELETE FROM queue_occurrences WHERE source_key=?1 AND position>=?2")
            .bind(source)
            .bind(len)
            .execute(&mut *transaction)
            .await?;
        let current = occurrence_key_by_object(&mut transaction, source, current_object_id).await?;
        let prepared =
            occurrence_key_by_object(&mut transaction, source, prepared_next_object_id).await?;
        sqlx::query("INSERT INTO queue_state(source_key,current_occurrence_key,prepared_next_occurrence_key,progress_millis,repeat_mode,shuffled) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(source_key) DO UPDATE SET current_occurrence_key=excluded.current_occurrence_key,prepared_next_occurrence_key=excluded.prepared_next_occurrence_key,progress_millis=excluded.progress_millis,repeat_mode=excluded.repeat_mode,shuffled=excluded.shuffled")
            .bind(source).bind(current).bind(prepared).bind(progress_millis).bind(repeat_mode.as_str()).bind(shuffled).execute(&mut *transaction).await?;
        let keys = sqlx::query_scalar::<_, QueueOccurrenceKey>("SELECT queue_occurrence_key FROM queue_occurrences WHERE source_key=?1 ORDER BY traversal_position")
            .bind(source).fetch_all(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(keys)
    }

    pub async fn remove_queue_occurrence(
        &self,
        source: SourceKey,
        occurrence: QueueOccurrenceKey,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        sqlx::query("UPDATE queue_state SET current_occurrence_key=CASE WHEN current_occurrence_key=?2 THEN NULL ELSE current_occurrence_key END,prepared_next_occurrence_key=CASE WHEN prepared_next_occurrence_key=?2 THEN NULL ELSE prepared_next_occurrence_key END WHERE source_key=?1")
            .bind(source).bind(occurrence).execute(&mut *transaction).await?;
        let removed = sqlx::query(
            "DELETE FROM queue_occurrences WHERE source_key=?1 AND queue_occurrence_key=?2",
        )
        .bind(source)
        .bind(occurrence)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            == 1;
        if removed {
            renumber_queue(&mut transaction, source).await?;
        }
        transaction.commit().await?;
        Ok(removed)
    }

    pub async fn move_queue_occurrence(
        &self,
        source: SourceKey,
        occurrence: QueueOccurrenceKey,
        canonical_position: usize,
        traversal_position: usize,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let Some((old_position, old_traversal)) = sqlx::query_as::<_, (i64, i64)>("SELECT position,traversal_position FROM queue_occurrences WHERE source_key=?1 AND queue_occurrence_key=?2")
            .bind(source).bind(occurrence).fetch_optional(&mut *transaction).await? else {
                transaction.rollback().await?;
                return Ok(false);
            };
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM queue_occurrences WHERE source_key=?1",
        )
        .bind(source)
        .fetch_one(&mut *transaction)
        .await?;
        let canonical_position = i64::try_from(canonical_position)
            .unwrap_or(i64::MAX)
            .min(count - 1);
        let traversal_position = i64::try_from(traversal_position)
            .unwrap_or(i64::MAX)
            .min(count - 1);
        let offset = count + 1;
        sqlx::query("UPDATE queue_occurrences SET position=position+?2,traversal_position=traversal_position+?2 WHERE source_key=?1")
            .bind(source).bind(offset).execute(&mut *transaction).await?;
        sqlx::query("UPDATE queue_occurrences SET position=CASE WHEN queue_occurrence_key=?2 THEN ?4 WHEN ?4<?3 AND position-?6>=?4 AND position-?6<?3 THEN position-?6+1 WHEN ?4>?3 AND position-?6>?3 AND position-?6<=?4 THEN position-?6-1 ELSE position-?6 END,traversal_position=CASE WHEN queue_occurrence_key=?2 THEN ?5 WHEN ?5<?7 AND traversal_position-?6>=?5 AND traversal_position-?6<?7 THEN traversal_position-?6+1 WHEN ?5>?7 AND traversal_position-?6>?7 AND traversal_position-?6<=?5 THEN traversal_position-?6-1 ELSE traversal_position-?6 END WHERE source_key=?1")
            .bind(source).bind(occurrence).bind(old_position).bind(canonical_position)
            .bind(traversal_position).bind(offset).bind(old_traversal)
            .execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn persist_queue_state(
        &self,
        source: SourceKey,
        current: Option<QueueOccurrenceKey>,
        prepared_next: Option<QueueOccurrenceKey>,
        progress_millis: i64,
        repeat_mode: QueueRepeatMode,
        shuffled: bool,
    ) -> LibraryResult<bool> {
        if progress_millis < 0 {
            return Err(LibraryError::InvalidRequest(
                "queue progress cannot be negative".to_string(),
            ));
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        validate_state_key(&mut transaction, source, current).await?;
        validate_state_key(&mut transaction, source, prepared_next).await?;
        let changed=sqlx::query("INSERT INTO queue_state(source_key,current_occurrence_key,prepared_next_occurrence_key,progress_millis,repeat_mode,shuffled) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(source_key) DO UPDATE SET current_occurrence_key=excluded.current_occurrence_key,prepared_next_occurrence_key=excluded.prepared_next_occurrence_key,progress_millis=excluded.progress_millis,repeat_mode=excluded.repeat_mode,shuffled=excluded.shuffled")
            .bind(source).bind(current).bind(prepared_next).bind(progress_millis)
            .bind(repeat_mode.as_str()).bind(shuffled).execute(&mut *transaction).await?.rows_affected()==1;
        transaction.commit().await?;
        Ok(changed)
    }

    pub async fn persist_queue_traversal(
        &self,
        source: SourceKey,
        occurrence_order: &[QueueOccurrenceKey],
    ) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM queue_occurrences WHERE source_key=?1",
        )
        .bind(source)
        .fetch_one(&mut *transaction)
        .await?;
        if usize::try_from(count).ok() != Some(occurrence_order.len()) {
            transaction.rollback().await?;
            return Err(LibraryError::InvalidRequest(
                "traversal order is not the complete Queue".to_string(),
            ));
        }
        let offset = count + 1;
        sqlx::query("UPDATE queue_occurrences SET traversal_position=traversal_position+?2 WHERE source_key=?1")
            .bind(source).bind(offset).execute(&mut *transaction).await?;
        for (batch_number, batch) in occurrence_order.chunks(TRAVERSAL_WRITE_BATCH).enumerate() {
            let start = batch_number * TRAVERSAL_WRITE_BATCH;
            let mut query = QueryBuilder::<Sqlite>::new(
                "WITH next(queue_occurrence_key,traversal_position) AS (",
            );
            query.push_values(batch.iter().enumerate(), |mut row, (offset, key)| {
                row.push_bind(*key).push_bind((start + offset) as i64);
            });
            query.push(") UPDATE queue_occurrences SET traversal_position=(SELECT traversal_position FROM next WHERE next.queue_occurrence_key=queue_occurrences.queue_occurrence_key) WHERE source_key=").push_bind(source).push(" AND queue_occurrence_key IN (SELECT queue_occurrence_key FROM next)");
            query
                .build()
                .persistent(false)
                .execute(&mut *transaction)
                .await?;
        }
        let accepted=sqlx::query_scalar::<_,i64>("SELECT count(*) FROM queue_occurrences WHERE source_key=?1 AND traversal_position>=0 AND traversal_position<?2")
            .bind(source).bind(count).fetch_one(&mut *transaction).await?;
        if accepted != count {
            transaction.rollback().await?;
            return Err(LibraryError::InvalidRequest(
                "traversal order is not the exact Queue occurrence set".to_string(),
            ));
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn persist_queue_traversal_objects(
        &self,
        source: SourceKey,
        occurrence_order: &[String],
    ) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM queue_occurrences WHERE source_key=?1",
        )
        .bind(source)
        .fetch_one(&mut *transaction)
        .await?;
        if usize::try_from(count).ok() != Some(occurrence_order.len()) {
            transaction.rollback().await?;
            return Err(LibraryError::InvalidRequest(
                "traversal order is not the complete Queue".to_string(),
            ));
        }
        let offset = count + 1;
        sqlx::query("UPDATE queue_occurrences SET traversal_position=traversal_position+?2 WHERE source_key=?1").bind(source).bind(offset).execute(&mut *transaction).await?;
        for (batch_number, batch) in occurrence_order.chunks(TRAVERSAL_WRITE_BATCH).enumerate() {
            let start = batch_number * TRAVERSAL_WRITE_BATCH;
            let mut query =
                QueryBuilder::<Sqlite>::new("WITH next(object_id,traversal_position) AS (");
            query.push_values(
                batch.iter().enumerate(),
                |mut row, (position, object_id)| {
                    row.push_bind(object_id)
                        .push_bind((start + position) as i64);
                },
            );
            query.push(") UPDATE queue_occurrences SET traversal_position=(SELECT traversal_position FROM next WHERE next.object_id=queue_occurrences.object_id) WHERE source_key=").push_bind(source).push(" AND object_id IN (SELECT object_id FROM next)");
            query
                .build()
                .persistent(false)
                .execute(&mut *transaction)
                .await?;
        }
        let accepted=sqlx::query_scalar::<_,i64>("SELECT count(*) FROM queue_occurrences WHERE source_key=?1 AND traversal_position>=0 AND traversal_position<?2").bind(source).bind(count).fetch_one(&mut *transaction).await?;
        if accepted != count {
            transaction.rollback().await?;
            return Err(LibraryError::InvalidRequest(
                "traversal order is not the exact Queue occurrence set".to_string(),
            ));
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn queue_current_next(&self, source: SourceKey) -> LibraryResult<QueueCurrentNext> {
        let mut connection = self.acquire_playback().await?;
        let mut transaction = connection.begin().await?;
        let keys = sqlx::query_as::<_, (Option<QueueOccurrenceKey>, Option<QueueOccurrenceKey>)>("SELECT current_occurrence_key,prepared_next_occurrence_key FROM queue_state WHERE source_key=?1")
            .bind(source).fetch_optional(&mut *transaction).await?.unwrap_or((None,None));
        let media = load_queue_media(&mut transaction, source, &[keys.0, keys.1]).await?;
        transaction.commit().await?;
        Ok(QueueCurrentNext {
            current: media.first().cloned().flatten(),
            prepared_next: media.get(1).cloned().flatten(),
        })
    }

    pub async fn queue_media_for_occurrence(
        &self,
        source: SourceKey,
        object_id: &str,
    ) -> LibraryResult<Option<QueueMedia>> {
        let mut connection = self.acquire_playback().await?;
        let mut transaction = connection.begin().await?;
        let key = sqlx::query_scalar::<_, QueueOccurrenceKey>(
            "SELECT queue_occurrence_key FROM queue_occurrences WHERE source_key=?1 AND object_id=?2",
        )
        .bind(source)
        .bind(object_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let media = load_queue_media(&mut transaction, source, &[key]).await?;
        transaction.commit().await?;
        Ok(media.into_iter().next().flatten())
    }

    pub async fn persist_queue_progress(
        &self,
        source: SourceKey,
        current_object_id: Option<&str>,
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
                 current_occurrence_key=(SELECT queue_occurrence_key FROM queue_occurrences WHERE source_key=?1 AND object_id=?2),
                 progress_millis=?3
             WHERE source_key=?1",
        )
        .bind(source)
        .bind(current_object_id)
        .bind(progress_millis)
        .execute(connection)
        .await?
        .rows_affected()
            == 1;
        Ok(changed)
    }

    pub async fn persist_queue_modes(
        &self,
        source: SourceKey,
        repeat_mode: QueueRepeatMode,
        shuffled: bool,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(
            sqlx::query("UPDATE queue_state SET repeat_mode=?2,shuffled=?3 WHERE source_key=?1")
                .bind(source)
                .bind(repeat_mode.as_str())
                .bind(shuffled)
                .execute(connection)
                .await?
                .rows_affected()
                == 1,
        )
    }

    pub async fn restore_queue(&self, source: SourceKey) -> LibraryResult<QueueRestore> {
        let mut connection = self.acquire_playback().await?;
        let mut transaction = connection.begin().await?;
        let scalars = sqlx::query_as::<_, QueueCompactScalar>("SELECT queue_occurrence_key occurrence_key,object_id,track_key,position canonical_position,traversal_position,provenance_kind,provenance_context_id,provenance_source_rank FROM queue_occurrences WHERE source_key=?1 ORDER BY traversal_position")
            .bind(source).fetch_all(&mut *transaction).await?;
        let occurrences = scalars
            .into_iter()
            .map(|row| {
                Ok(QueueCompactOccurrence {
                    occurrence_key: Some(row.occurrence_key),
                    object_id: row.object_id,
                    track_key: row.track_key,
                    canonical_position: row.canonical_position,
                    traversal_position: row.traversal_position,
                    provenance: QueueProvenance::parse(
                        &row.provenance_kind,
                        row.provenance_context_id,
                        row.provenance_source_rank,
                    )?,
                })
            })
            .collect::<LibraryResult<Vec<_>>>()?;
        let stored = sqlx::query_as::<_, (Option<QueueOccurrenceKey>,Option<QueueOccurrenceKey>,i64,String,bool)>("SELECT current_occurrence_key,prepared_next_occurrence_key,progress_millis,repeat_mode,shuffled FROM queue_state WHERE source_key=?1")
            .bind(source).fetch_optional(&mut *transaction).await?;
        let (current, prepared_next, progress_millis, repeat_mode, shuffled) = match stored {
            Some((current, prepared, progress, repeat_mode, shuffled)) => (
                current,
                prepared,
                progress,
                QueueRepeatMode::parse(&repeat_mode)?,
                shuffled,
            ),
            None => (None, None, 0, QueueRepeatMode::None, false),
        };
        let media = load_queue_media(&mut transaction, source, &[current, prepared_next]).await?;
        transaction.commit().await?;
        Ok(QueueRestore {
            occurrences,
            state: QueueState {
                current,
                prepared_next,
                progress_millis,
                repeat_mode,
                shuffled,
            },
            current: media.first().cloned().flatten(),
            prepared_next: media.get(1).cloned().flatten(),
        })
    }

    pub async fn queue_page(
        &self,
        source: SourceKey,
        after_position: Option<i64>,
        filter: &str,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<QueuePageRow>> {
        let limit = limit.clamp(1, QUEUE_PAGE_LIMIT) as i64;
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let rows = sqlx::query_as::<_, QueuePageScalar>("WITH page AS (SELECT queue_occurrence_key,position FROM queue_occurrences WHERE source_key=?1 AND position>?2 ORDER BY position LIMIT ?5) SELECT occurrence.queue_occurrence_key occurrence_key,occurrence.object_id,occurrence.position,occurrence.traversal_position,occurrence.provenance_kind,occurrence.provenance_context_id,occurrence.provenance_source_rank,occurrence.track_key,occurrence.track_object_id,COALESCE(track.title,occurrence.fallback_title,'') title,COALESCE(track.display_artist,occurrence.fallback_artist,'') artist,COALESCE(track.display_album,occurrence.fallback_album,'') album,COALESCE(album.display_artist,occurrence.fallback_album_display_artist) album_display_artist,COALESCE(album.object_id,occurrence.fallback_album_object_id) album_object_id,COALESCE((SELECT artist.object_id FROM track_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1),(SELECT artist.object_id FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=track.album_key ORDER BY credit.position LIMIT 1),occurrence.fallback_primary_artist_object_id) primary_artist_object_id,COALESCE((SELECT artist.musicbrainz_artist_id FROM track_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1),(SELECT artist.musicbrainz_artist_id FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=track.album_key ORDER BY credit.position LIMIT 1),occurrence.fallback_primary_artist_musicbrainz_id) primary_artist_musicbrainz_id,COALESCE((SELECT local.media_uri FROM local_access_files local WHERE local.source_key=occurrence.source_key AND local.track_object_id=occurrence.track_object_id ORDER BY CASE local.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,local.local_access_file_key LIMIT 1),track.media_uri,occurrence.fallback_media_uri) media_uri,COALESCE(track.artwork_binding,album.artwork_binding,occurrence.fallback_artwork_binding) artwork_binding,COALESCE(track.duration_millis,occurrence.fallback_duration_millis) duration_millis,COALESCE(track.disc_number,occurrence.fallback_disc_number) disc_number,COALESCE(track.track_number,occurrence.fallback_track_number) track_number,COALESCE(track.year,occurrence.fallback_year) year,COALESCE(track.release_date,occurrence.fallback_release_date) release_date,COALESCE(COALESCE(track.user_favorite,track.source_favorite),occurrence.fallback_favorite) favorite,COALESCE(track.user_rating,track.source_rating)/10 rating,EXISTS(SELECT 1 FROM local_access_files downloaded WHERE downloaded.source_key=occurrence.source_key AND downloaded.track_object_id=occurrence.track_object_id AND downloaded.origin='download') is_downloaded,COALESCE(track.source_format,occurrence.fallback_source_format) source_format,COALESCE(track.musicbrainz_recording_id,occurrence.fallback_musicbrainz_recording_id) musicbrainz_recording_id,COALESCE(track.musicbrainz_release_track_id,occurrence.fallback_musicbrainz_release_track_id) musicbrainz_release_track_id,COALESCE(album.musicbrainz_release_id,occurrence.fallback_musicbrainz_album_id) musicbrainz_album_id,COALESCE(album.musicbrainz_release_group_id,occurrence.fallback_musicbrainz_release_group_id) musicbrainz_release_group_id,COALESCE(track.cue_path,occurrence.fallback_cue_path) cue_path,COALESCE(track.cue_start_millis,occurrence.fallback_cue_start_millis) cue_start_millis,COALESCE(track.cue_end_millis,occurrence.fallback_cue_end_millis) cue_end_millis FROM page JOIN queue_occurrences occurrence USING(queue_occurrence_key) LEFT JOIN tracks track USING(track_key) LEFT JOIN albums album ON album.album_key=track.album_key WHERE (?3 OR instr(lower(COALESCE(track.title,occurrence.fallback_title,'')),?4)>0 OR instr(lower(COALESCE(track.display_artist,occurrence.fallback_artist,'')),?4)>0 OR instr(lower(COALESCE(track.display_album,occurrence.fallback_album,'')),?4)>0) ORDER BY occurrence.position LIMIT ?5")
            .bind(source).bind(after_position.unwrap_or(-1)).bind(filter.is_empty()).bind(filter).bind(limit)
            .fetch_all(&mut *connection).await;
        let rows = rows?;
        let track_keys = rows
            .iter()
            .filter_map(|row| row.track_key)
            .collect::<Vec<_>>();
        let identities = load_queue_page_identities(&mut connection, &track_keys).await?;
        Database::clear_progress(&mut connection).await?;
        rows.into_iter()
            .map(|row| {
                let identity = row.track_key.and_then(|key| identities.get(&key));
                let mut result = QueuePageRow::try_from(row)?;
                result.album_key = identity.and_then(|identity| identity.album_key);
                result.primary_artist_key =
                    identity.and_then(|identity| identity.primary_artist_key);
                Ok(result)
            })
            .collect()
    }
}

impl TryFrom<QueuePageScalar> for QueuePageRow {
    type Error = LibraryError;

    fn try_from(row: QueuePageScalar) -> Result<Self, Self::Error> {
        Ok(Self {
            occurrence_key: row.occurrence_key,
            object_id: row.object_id,
            position: row.position,
            traversal_position: row.traversal_position,
            provenance: QueueProvenance::parse(
                &row.provenance_kind,
                row.provenance_context_id,
                row.provenance_source_rank,
            )?,
            track_key: row.track_key,
            track_object_id: row.track_object_id,
            title: row.title,
            artist: row.artist,
            album: row.album,
            album_display_artist: row.album_display_artist,
            album_key: None,
            album_object_id: row.album_object_id,
            primary_artist_key: None,
            primary_artist_object_id: row.primary_artist_object_id,
            primary_artist_musicbrainz_id: row.primary_artist_musicbrainz_id,
            media_uri: row.media_uri,
            artwork_binding: row.artwork_binding,
            duration_millis: row.duration_millis,
            disc_number: row.disc_number,
            track_number: row.track_number,
            year: row.year,
            release_date: row.release_date,
            favorite: row.favorite,
            rating: row.rating,
            is_downloaded: row.is_downloaded,
            source_format: row.source_format,
            musicbrainz_recording_id: row.musicbrainz_recording_id,
            musicbrainz_release_track_id: row.musicbrainz_release_track_id,
            musicbrainz_album_id: row.musicbrainz_album_id,
            musicbrainz_release_group_id: row.musicbrainz_release_group_id,
            cue_path: row.cue_path,
            cue_start_millis: row.cue_start_millis,
            cue_end_millis: row.cue_end_millis,
        })
    }
}

async fn load_queue_page_identities(
    connection: &mut sqlx::SqliteConnection,
    keys: &[TrackKey],
) -> LibraryResult<BTreeMap<TrackKey, QueuePageIdentity>> {
    if keys.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(track_key,position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push(
        ") SELECT track.track_key,track.album_key,
           COALESCE(
             (SELECT relation.artist_key FROM track_artists relation
              WHERE relation.track_key=track.track_key ORDER BY relation.position LIMIT 1),
             (SELECT relation.artist_key FROM album_artists relation
              WHERE relation.album_key=track.album_key ORDER BY relation.position LIMIT 1)
           ) primary_artist_key
           FROM requested JOIN tracks track USING(track_key)
           ORDER BY requested.position",
    );
    let rows = query
        .build_query_as::<QueuePageIdentity>()
        .persistent(false)
        .fetch_all(connection)
        .await?;
    Ok(rows.into_iter().map(|row| (row.track_key, row)).collect())
}

async fn occurrence_key_by_object(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    source: SourceKey,
    object_id: Option<&str>,
) -> LibraryResult<Option<QueueOccurrenceKey>> {
    let Some(object_id) = object_id else {
        return Ok(None);
    };
    let key = sqlx::query_scalar(
        "SELECT queue_occurrence_key FROM queue_occurrences WHERE source_key=?1 AND object_id=?2",
    )
    .bind(source)
    .bind(object_id)
    .fetch_optional(&mut **transaction)
    .await?;
    key.map(Some).ok_or_else(|| {
        LibraryError::InvalidRequest("queue state references an unknown occurrence".to_string())
    })
}

async fn validate_state_key(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    source: SourceKey,
    key: Option<QueueOccurrenceKey>,
) -> LibraryResult<()> {
    if let Some(key) = key
        && sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM queue_occurrences WHERE source_key=?1 AND queue_occurrence_key=?2",
        )
        .bind(source)
        .bind(key)
        .fetch_optional(&mut **transaction)
        .await?
        .is_none()
    {
        return Err(LibraryError::InvalidRequest(
            "queue state references an unknown occurrence".to_string(),
        ));
    }
    Ok(())
}

async fn renumber_queue(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    source: SourceKey,
) -> LibraryResult<()> {
    let count =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM queue_occurrences WHERE source_key=?1")
            .bind(source)
            .fetch_one(&mut **transaction)
            .await?;
    let offset = count + 1;
    sqlx::query("UPDATE queue_occurrences SET position=position+?2,traversal_position=traversal_position+?2 WHERE source_key=?1")
        .bind(source).bind(offset).execute(&mut **transaction).await?;
    sqlx::query("WITH canonical AS MATERIALIZED (SELECT queue_occurrence_key,row_number() OVER(ORDER BY position)-1 next_position FROM queue_occurrences WHERE source_key=?1),traversal AS MATERIALIZED (SELECT queue_occurrence_key,row_number() OVER(ORDER BY traversal_position)-1 next_position FROM queue_occurrences WHERE source_key=?1) UPDATE queue_occurrences SET position=(SELECT next_position FROM canonical WHERE canonical.queue_occurrence_key=queue_occurrences.queue_occurrence_key),traversal_position=(SELECT next_position FROM traversal WHERE traversal.queue_occurrence_key=queue_occurrences.queue_occurrence_key) WHERE source_key=?1")
        .bind(source).execute(&mut **transaction).await?;
    Ok(())
}

async fn load_queue_media(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    source: SourceKey,
    keys: &[Option<QueueOccurrenceKey>],
) -> LibraryResult<Vec<Option<QueueMedia>>> {
    let present = keys.iter().flatten().copied().collect::<Vec<_>>();
    if present.is_empty() {
        return Ok(vec![None; keys.len()]);
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(occurrence_key,ordinal) AS (");
    query.push_values(present.iter().enumerate(), |mut row, (ordinal, key)| {
        row.push_bind(*key).push_bind(ordinal as i64);
    });
    query.push(") SELECT occurrence.queue_occurrence_key occurrence_key,occurrence.track_key,occurrence.track_object_id,COALESCE(track.title,occurrence.fallback_title,'') title,COALESCE(track.display_artist,occurrence.fallback_artist,'') artist,COALESCE(track.display_album,occurrence.fallback_album,'') album,COALESCE(album.display_artist,occurrence.fallback_album_display_artist) album_display_artist,track.album_key,COALESCE(album.object_id,occurrence.fallback_album_object_id) album_object_id,COALESCE((SELECT artist.artist_key FROM track_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1),(SELECT artist.artist_key FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=track.album_key ORDER BY credit.position LIMIT 1)) primary_artist_key,COALESCE((SELECT artist.object_id FROM track_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1),(SELECT artist.object_id FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=track.album_key ORDER BY credit.position LIMIT 1),occurrence.fallback_primary_artist_object_id) primary_artist_object_id,COALESCE((SELECT artist.musicbrainz_artist_id FROM track_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1),(SELECT artist.musicbrainz_artist_id FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=track.album_key ORDER BY credit.position LIMIT 1),occurrence.fallback_primary_artist_musicbrainz_id) primary_artist_musicbrainz_id,(SELECT local.media_uri FROM local_access_files local WHERE local.source_key=occurrence.source_key AND local.track_object_id=occurrence.track_object_id AND local.origin='download' ORDER BY local.local_access_file_key LIMIT 1) download_media_uri,(SELECT local.media_uri FROM local_access_files local WHERE local.source_key=occurrence.source_key AND local.track_object_id=occurrence.track_object_id AND local.origin='mapping' ORDER BY local.local_access_file_key LIMIT 1) mapping_media_uri,COALESCE(track.media_uri,occurrence.fallback_media_uri) source_media_uri,COALESCE((SELECT local.media_uri FROM local_access_files local WHERE local.source_key=occurrence.source_key AND local.track_object_id=occurrence.track_object_id ORDER BY CASE local.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,local.local_access_file_key LIMIT 1),track.media_uri,occurrence.fallback_media_uri) media_uri,COALESCE(track.artwork_binding,album.artwork_binding,occurrence.fallback_artwork_binding) artwork_binding,COALESCE(track.duration_millis,occurrence.fallback_duration_millis) duration_millis,COALESCE(track.disc_number,occurrence.fallback_disc_number) disc_number,COALESCE(track.track_number,occurrence.fallback_track_number) track_number,COALESCE(track.year,occurrence.fallback_year) year,COALESCE(track.release_date,occurrence.fallback_release_date) release_date,COALESCE(COALESCE(track.user_favorite,track.source_favorite),occurrence.fallback_favorite) favorite,COALESCE(track.source_format,occurrence.fallback_source_format) source_format,COALESCE(track.musicbrainz_recording_id,occurrence.fallback_musicbrainz_recording_id) musicbrainz_recording_id,COALESCE(track.musicbrainz_release_track_id,occurrence.fallback_musicbrainz_release_track_id) musicbrainz_release_track_id,COALESCE(album.musicbrainz_release_id,occurrence.fallback_musicbrainz_album_id) musicbrainz_album_id,COALESCE(album.musicbrainz_release_group_id,occurrence.fallback_musicbrainz_release_group_id) musicbrainz_release_group_id,COALESCE(track.cue_path,occurrence.fallback_cue_path) cue_path,COALESCE(track.cue_start_millis,occurrence.fallback_cue_start_millis) cue_start_millis,COALESCE(track.cue_end_millis,occurrence.fallback_cue_end_millis) cue_end_millis FROM requested JOIN queue_occurrences occurrence ON occurrence.queue_occurrence_key=requested.occurrence_key LEFT JOIN tracks track USING(track_key) LEFT JOIN albums album ON album.album_key=track.album_key WHERE occurrence.source_key=").push_bind(source).push(" ORDER BY requested.ordinal");
    let mut rows = query
        .build_query_as::<QueueMedia>()
        .persistent(false)
        .fetch_all(&mut **transaction)
        .await?;
    let source_id =
        sqlx::query_scalar::<_, String>("SELECT object_id FROM sources WHERE source_key=?1")
            .bind(source)
            .fetch_one(&mut **transaction)
            .await?;
    let track_keys = rows
        .iter()
        .filter_map(|row| row.track_key)
        .collect::<Vec<_>>();
    let track_rows = crate::tracks::load_track_rows(&mut **transaction, source, &track_keys)
        .await?
        .into_iter()
        .map(|track| (track.track_key, track.artists))
        .collect::<BTreeMap<_, _>>();
    for row in &mut rows {
        row.source_id.clone_from(&source_id);
        if let Some(track) = row.track_key {
            row.artist_links = track_rows.get(&track).cloned().unwrap_or_default();
        }
    }
    let mut rows = rows.into_iter();
    Ok(keys
        .iter()
        .map(|key| if key.is_some() { rows.next() } else { None })
        .collect())
}
