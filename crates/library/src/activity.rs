//! Owns accepted listens, on-demand Activity summaries, and per-service delivery targets.
//! Rufin Activity is recorded independently of external private-mode delivery policy.

use std::io::{BufRead, Write};

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{Connection, FromRow, Row, SqliteConnection, sqlite::SqliteRow};

use crate::{
    AlbumKey, ArtistKey, Database, GenreKey, LibraryError, LibraryResult, ListenKey,
    ListenOutboxKey, ReadCancellation, SourceId, SourceKey, TrackKey, source_entity_parts,
};

const HISTORY_LIMIT: i64 = 100;
const ACTIVITY_RESULT_LIMIT: usize = 100;
const DELIVERY_LIMIT: usize = 100;
const ACTIVITY_EXPORT_SELECT: &str = "SELECT listen_key,external_id,
                    strftime('%Y-%m-%dT%H:%M:%SZ',started_at,'unixepoch') utc_time,
                    source_id,media_uri,track_title title,artist_name artist,album_title album,
                    duration_millis,disc_number,track_number,year,release_date,source_format,
                    musicbrainz_recording_id,musicbrainz_release_track_id,started_at,
                    local_period,listened_millis,skipped
             FROM listens";
const HISTORY_ROW_SELECT: &str = r#"
             listen.media_uri,
                    listen.track_title title,listen.artist_name artist,listen.album_title album,
                    album.display_artist album_display_artist,
                    listen.disc_number,listen.track_number,listen.year,listen.release_date,
                    listen.source_format,listen.musicbrainz_recording_id,
                    listen.musicbrainz_release_track_id,
                    listen.started_at last_played,
                    listen.duration_millis,track.artwork_binding,
                    track.date_added,track.bpm,
                    (SELECT COALESCE(group_concat(name, ', '),'') FROM (
                       SELECT genre.name FROM track_genres credit JOIN genres genre USING(genre_key)
                       WHERE credit.track_key=track.track_key ORDER BY credit.position
                    )) genre,
                    COALESCE((SELECT baseline.play_count FROM activity_baseline baseline
                              WHERE baseline.source_key=track.source_key
                                AND baseline.track_object_id=track.object_id
                                AND baseline.period='lifetime' AND baseline.item_kind='track'),0)
                      +(SELECT count(*) FROM listens accepted WHERE accepted.media_uri=listen.media_uri) play_count,
                    COALESCE((SELECT state.favorite FROM user_media_state state
                              WHERE state.media_uri=listen.media_uri),
                             (SELECT track.source_favorite FROM tracks track
                              WHERE track.media_uri=listen.media_uri),0) favorite,
                    COALESCE((SELECT state.rating FROM user_media_state state
                              WHERE state.media_uri=listen.media_uri),
                             (SELECT track.source_rating FROM tracks track
                              WHERE track.media_uri=listen.media_uri))/10 rating,
                    EXISTS(SELECT 1 FROM local_access_files access
                           WHERE access.media_uri=listen.media_uri AND access.origin='download') is_downloaded
             FROM selected listen
             LEFT JOIN tracks track USING(media_uri)
             LEFT JOIN albums album ON album.album_key=track.album_key
"#;

#[derive(Clone, Debug, FromRow, PartialEq, Deserialize, Serialize)]
pub struct ListenWrite {
    pub external_id: Option<String>,
    pub media_uri: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_millis: i64,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub source_format: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub started_at: i64,
    pub local_period: String,
    pub listened_millis: i64,
    pub skipped: bool,
}

/// A semantic accepted listen. Delivery work is deliberately outside this record.
#[derive(Debug, Deserialize, Serialize)]
pub struct ActivityRecord {
    pub version: u32,
    pub listen_key: i64,
    pub source_id: Option<String>,
    #[serde(flatten)]
    pub listen: ListenWrite,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct ActivityImportReport {
    pub accepted: u64,
    pub skipped: u64,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct HistoryRow {
    pub media_uri: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_media_uri: Option<String>,
    #[sqlx(skip)]
    pub artists: Vec<crate::TrackArtistLink>,
    #[sqlx(skip)]
    pub album_artists: Vec<crate::TrackArtistLink>,
    pub album_display_artist: Option<String>,
    pub artwork_binding: Option<Vec<u8>>,
    pub duration_millis: i64,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub bpm: Option<i64>,
    pub genre: String,
    pub play_count: i64,
    pub source_format: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub last_played: Option<i64>,
    pub favorite: bool,
    pub rating: Option<i64>,
    pub is_downloaded: bool,
}

fn history_rows(rows: Vec<SqliteRow>) -> LibraryResult<Vec<HistoryRow>> {
    rows.iter()
        .map(|row| {
            let mut history = HistoryRow::from_row(row)?;
            history.artists = serde_json::from_str(row.try_get("artists")?)?;
            history.album_artists = serde_json::from_str(row.try_get("album_artists")?)?;
            Ok(history)
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListenDeliveryTarget {
    pub service: String,
    pub account_id: String,
    pub next_attempt_at: Option<i64>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct ActivityTrackRow {
    pub track_key: TrackKey,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub play_count: i64,
    pub skip_count: i64,
    pub last_played: Option<i64>,
    pub listened_millis: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalendarActivityPeriod {
    Month { year: i32, month: u8 },
    Year(i32),
    Lifetime,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct ActivityAlbumRow {
    pub album_key: AlbumKey,
    pub title: String,
    pub artist: String,
    pub play_count: i64,
    pub listened_millis: i64,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct ActivityArtistRow {
    pub artist_key: ArtistKey,
    pub name: String,
    pub play_count: i64,
    pub listened_millis: i64,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct ActivityGenreRow {
    pub genre_key: GenreKey,
    pub name: String,
    pub play_count: i64,
    pub listened_millis: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalendarActivitySummary {
    pub tracks: Vec<ActivityTrackRow>,
    pub albums: Vec<ActivityAlbumRow>,
    pub artists: Vec<ActivityArtistRow>,
    pub genres: Vec<ActivityGenreRow>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct PendingListenDelivery {
    pub outbox_key: ListenOutboxKey,
    pub listen_key: ListenKey,
    pub service: String,
    pub account_id: String,
    pub attempts: i64,
    pub next_attempt_at: i64,
    pub last_error: Option<String>,
    pub external_id: Option<String>,
    pub track_title: String,
    pub artist_name: String,
    pub album_title: String,
    pub started_at: i64,
    pub duration_millis: i64,
    pub listened_millis: i64,
    pub skipped: bool,
}

impl Database {
    pub async fn record_listen(
        &self,
        listen: &ListenWrite,
        deliveries: &[ListenDeliveryTarget],
    ) -> LibraryResult<ListenKey> {
        validate_listen(listen, deliveries)?;
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let source_id = match source_entity_parts(&listen.media_uri) {
            Some((source, _, _)) => Some(source.as_str().to_string()),
            None => sqlx::query_scalar("SELECT source.object_id FROM tracks track JOIN sources source USING(source_key) WHERE track.media_uri=?1 LIMIT 1")
                .bind(&listen.media_uri).fetch_optional(&mut *transaction).await?,
        };
        let key = write_listen(&mut transaction, listen, source_id.as_deref()).await?;
        for delivery in deliveries {
            sqlx::query("INSERT OR IGNORE INTO listen_outbox(listen_key,service,account_id,next_attempt_at) VALUES (?1,?2,?3,?4)")
                .bind(key).bind(&delivery.service).bind(&delivery.account_id)
                .bind(delivery.next_attempt_at).execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        Ok(key)
    }

    /// Streams a consistent snapshot without collecting the Activity history in memory.
    pub async fn export_activity_jsonl(
        &self,
        output: impl Write,
        source_id: Option<&SourceId>,
    ) -> LibraryResult<u64> {
        let mut connection = self.acquire_reader().await?;
        export_activity_jsonl_on(&mut connection, output, source_id).await
    }

    /// Service CSV formats exclude source identities and access locators.
    pub async fn export_activity_csv(
        &self,
        mut output: impl Write,
        format: ActivityCsvFormat,
        source_id: Option<&SourceId>,
    ) -> LibraryResult<u64> {
        if format == ActivityCsvFormat::ListenBrainz {
            output.write_all(b"artist,track,album,time\r\n")?;
        }
        let mut connection = self.acquire_reader().await?;
        let mut query = activity_export_query(source_id);
        let mut rows = query
            .build_query_as::<ActivityExportRow>()
            .fetch(&mut *connection);
        let mut count = 0;
        while let Some(row) = rows.try_next().await? {
            let listen = row.listen;
            let fields = match format {
                ActivityCsvFormat::LastFm => {
                    [listen.artist, listen.album, listen.title, row.utc_time]
                }
                ActivityCsvFormat::ListenBrainz => [
                    listen.artist,
                    listen.title,
                    listen.album,
                    listen.started_at.to_string(),
                ],
            };
            for (index, field) in fields.iter().enumerate() {
                if index != 0 {
                    output.write_all(b",")?;
                }
                output.write_all(b"\"")?;
                for part in field.split_inclusive('"') {
                    output.write_all(part.as_bytes())?;
                    if part.ends_with('"') {
                        output.write_all(b"\"")?;
                    }
                }
                output.write_all(b"\"")?;
            }
            output.write_all(b"\r\n")?;
            count += 1;
        }
        Ok(count)
    }

    /// Salvage/interchange imports keep useful records and never create delivery targets.
    /// Backup framing is checked by the archive owner before invoking this operation.
    pub async fn import_activity_jsonl(
        &self,
        input: impl BufRead,
    ) -> LibraryResult<ActivityImportReport> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let report = import_activity_jsonl_on(&mut transaction, input).await?;
        transaction.commit().await?;
        Ok(report)
    }

    pub async fn history_rows_by_uri(
        &self,
        media_uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<HistoryRow>> {
        if media_uris.len() > HISTORY_LIMIT as usize {
            return Err(LibraryError::InvalidRequest(
                "History row window exceeds 100".into(),
            ));
        }
        let mut connection = tokio::select! {
            result = self.acquire_reader() => result?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        let sql = format!(
            "WITH selected AS MATERIALIZED (
                SELECT listen.*,requested.key ordinal
                FROM json_each(?1) requested JOIN listens listen
                  ON listen.listen_key=(
                    SELECT recent.listen_key FROM listens recent WHERE recent.media_uri=requested.value
                    ORDER BY recent.started_at DESC,recent.listen_key DESC LIMIT 1
                  )
             ) SELECT {} {HISTORY_ROW_SELECT} ORDER BY listen.ordinal",
            crate::tracks::TRACK_LINK_COLUMNS,
        );
        history_rows(
            sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
                .bind(serde_json::to_string(media_uris)?)
                .fetch_all(&mut *connection)
                .await?,
        )
    }

    pub async fn activity_history(
        &self,
        current: Option<&SourceId>,
        query: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<HistoryRow>> {
        let query: String = query.trim().to_lowercase().chars().take(256).collect();
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let sources = if current.is_some() {
            vec![
                "listen.source_id=?1",
                "listen.source_id IS NULL AND EXISTS (
                   SELECT 1 FROM tracks member WHERE member.media_uri=listen.media_uri
                     AND member.source_key=(SELECT source_key FROM sources WHERE object_id=?1))",
            ]
        } else {
            vec!["1"]
        };
        let recent_source = if current.is_some() {
            "AND (recent.source_id=?1 OR (recent.source_id IS NULL AND EXISTS (
               SELECT 1 FROM tracks member WHERE member.media_uri=recent.media_uri
                 AND member.source_key=(SELECT source_key FROM sources WHERE object_id=?1))))"
        } else {
            ""
        };
        let filter = if query.is_empty() {
            ""
        } else {
            "AND (instr(lower(listen.track_title),?2)>0 OR instr(lower(listen.artist_name),?2)>0
                  OR instr(lower(listen.album_title),?2)>0)"
        };
        let recent_filter = filter.replace("listen.", "recent.");
        // Each source branch uses the history index and stops at 100. Merge at most 200
        // candidates; old unattributed Local listens need no backfill or full-history sort.
        let selected = sources
            .into_iter()
            .map(|source| {
                format!(
                    "SELECT * FROM (SELECT listen.* FROM listens listen
             WHERE {source} {filter} AND listen.listen_key=(
               SELECT recent.listen_key FROM listens recent
               WHERE recent.media_uri=listen.media_uri {recent_source} {recent_filter}
               ORDER BY recent.started_at DESC,recent.listen_key DESC LIMIT 1)
             ORDER BY listen.started_at DESC,listen.listen_key DESC LIMIT ?3)"
                )
            })
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        let sql = format!(
            "WITH selected AS MATERIALIZED (
               SELECT * FROM ({selected}) ORDER BY started_at DESC,listen_key DESC LIMIT ?3
             )
             SELECT {} {HISTORY_ROW_SELECT}
             ORDER BY listen.started_at DESC,listen.listen_key DESC",
            crate::tracks::TRACK_LINK_COLUMNS,
        );
        let result = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
            .bind(current.map(SourceId::as_str))
            .bind(query)
            .bind(HISTORY_LIMIT)
            .fetch_all(&mut *connection)
            .await;
        Database::clear_progress(&mut connection).await?;
        history_rows(result?)
    }

    pub async fn calendar_activity_summary(
        &self,
        source: SourceKey,
        period: CalendarActivityPeriod,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<CalendarActivitySummary> {
        let limit = limit.clamp(1, ACTIVITY_RESULT_LIMIT) as i64;
        let (month, year, lifetime) = calendar_filter(period)?;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let tracks = sqlx::query_as::<_, ActivityTrackRow>(
            "WITH period_baseline AS (SELECT track_object_id,sum(play_count) play_count,sum(skip_count) skip_count,max(last_played_at) last_played_at FROM activity_baseline WHERE source_key=?1 AND item_kind='track' AND ((?4 AND period='lifetime') OR (NOT ?4 AND ((?2 IS NOT NULL AND period=?2) OR (?3 IS NOT NULL AND substr(period,1,4)=?3)))) GROUP BY track_object_id), window AS (
               SELECT listens.media_uri,count(*) play_count,COALESCE(sum(skipped),0) skip_count,
                      max(started_at) last_played,COALESCE(sum(listened_millis),0) listened_millis
               FROM tracks member CROSS JOIN listens ON listens.media_uri=member.media_uri
               WHERE member.source_key=?1
                 AND (?4 OR (?2 IS NOT NULL AND local_period=?2) OR (?3 IS NOT NULL AND substr(local_period,1,4)=?3))
               GROUP BY listens.media_uri
             )
             SELECT track.track_key,track.title,track.display_artist artist,track.display_album album,
                    COALESCE(window.play_count,0)+COALESCE(baseline.play_count,0) play_count,
                    COALESCE(window.skip_count,0)+COALESCE(baseline.skip_count,0) skip_count,
                    CASE WHEN baseline.last_played_at IS NULL THEN window.last_played
                         WHEN window.last_played IS NULL THEN baseline.last_played_at
                         ELSE max(window.last_played,baseline.last_played_at) END last_played,
                    COALESCE(window.listened_millis,0) listened_millis
             FROM tracks track LEFT JOIN window USING(media_uri)
             LEFT JOIN period_baseline baseline ON baseline.track_object_id=track.object_id
             WHERE track.source_key=?1 AND (COALESCE(window.play_count,0)+COALESCE(baseline.play_count,0))>0
             ORDER BY play_count DESC,last_played DESC NULLS LAST,track.sort_text,track.track_key LIMIT ?5",
        )
        .bind(source).bind(&month).bind(&year).bind(lifetime).bind(limit)
        .fetch_all(&mut *transaction).await?;
        let albums = sqlx::query_as::<_, ActivityAlbumRow>(
            "WITH period_baseline AS (SELECT track_object_id,sum(play_count) play_count,sum(skip_count) skip_count,max(last_played_at) last_played_at FROM activity_baseline WHERE source_key=?1 AND item_kind='track' AND ((?4 AND period='lifetime') OR (NOT ?4 AND ((?2 IS NOT NULL AND period=?2) OR (?3 IS NOT NULL AND substr(period,1,4)=?3)))) GROUP BY track_object_id), window AS (
               SELECT listens.media_uri,count(*) play_count,COALESCE(sum(listened_millis),0) listened_millis
               FROM tracks member CROSS JOIN listens ON listens.media_uri=member.media_uri
               WHERE member.source_key=?1
                 AND (?4 OR (?2 IS NOT NULL AND local_period=?2) OR (?3 IS NOT NULL AND substr(local_period,1,4)=?3)) GROUP BY listens.media_uri
             ), facts AS (
               SELECT track.track_key,track.album_key,
                      COALESCE(window.play_count,0)+COALESCE(baseline.play_count,0) play_count,
                      COALESCE(window.listened_millis,0) listened_millis
               FROM tracks track LEFT JOIN window USING(media_uri)
               LEFT JOIN period_baseline baseline ON baseline.track_object_id=track.object_id
               WHERE track.source_key=?1
             )
             SELECT album.album_key,album.title,album.display_artist artist,
                    sum(facts.play_count) play_count,sum(facts.listened_millis) listened_millis
             FROM facts JOIN albums album USING(album_key) GROUP BY album.album_key
             HAVING play_count>0 ORDER BY play_count DESC,album.sort_text,album.album_key LIMIT ?5",
        )
        .bind(source).bind(&month).bind(&year).bind(lifetime).bind(limit)
        .fetch_all(&mut *transaction).await?;
        let artists = sqlx::query_as::<_, ActivityArtistRow>(
            "WITH period_baseline AS (SELECT track_object_id,sum(play_count) play_count,sum(skip_count) skip_count,max(last_played_at) last_played_at FROM activity_baseline WHERE source_key=?1 AND item_kind='track' AND ((?4 AND period='lifetime') OR (NOT ?4 AND ((?2 IS NOT NULL AND period=?2) OR (?3 IS NOT NULL AND substr(period,1,4)=?3)))) GROUP BY track_object_id), window AS (
               SELECT listens.media_uri,count(*) play_count,COALESCE(sum(listened_millis),0) listened_millis
               FROM tracks member CROSS JOIN listens ON listens.media_uri=member.media_uri
               WHERE member.source_key=?1
                 AND (?4 OR (?2 IS NOT NULL AND local_period=?2) OR (?3 IS NOT NULL AND substr(local_period,1,4)=?3)) GROUP BY listens.media_uri
             ), facts AS (
               SELECT track.track_key,
                      COALESCE(window.play_count,0)+COALESCE(baseline.play_count,0) play_count,
                      COALESCE(window.listened_millis,0) listened_millis
               FROM tracks track LEFT JOIN window USING(media_uri)
               LEFT JOIN period_baseline baseline ON baseline.track_object_id=track.object_id
               WHERE track.source_key=?1
             )
             SELECT artist.artist_key,artist.name,sum(facts.play_count) play_count,
                    sum(facts.listened_millis) listened_millis
             FROM facts JOIN track_artists credit USING(track_key) JOIN artists artist USING(artist_key)
             GROUP BY artist.artist_key HAVING play_count>0
             ORDER BY play_count DESC,artist.sort_text,artist.artist_key LIMIT ?5",
        )
        .bind(source).bind(&month).bind(&year).bind(lifetime).bind(limit)
        .fetch_all(&mut *transaction).await?;
        let genres = sqlx::query_as::<_, ActivityGenreRow>(
            "WITH period_baseline AS (SELECT track_object_id,sum(play_count) play_count,sum(skip_count) skip_count,max(last_played_at) last_played_at FROM activity_baseline WHERE source_key=?1 AND item_kind='track' AND ((?4 AND period='lifetime') OR (NOT ?4 AND ((?2 IS NOT NULL AND period=?2) OR (?3 IS NOT NULL AND substr(period,1,4)=?3)))) GROUP BY track_object_id), window AS (
               SELECT listens.media_uri,count(*) play_count,COALESCE(sum(listened_millis),0) listened_millis
               FROM tracks member CROSS JOIN listens ON listens.media_uri=member.media_uri
               WHERE member.source_key=?1
                 AND (?4 OR (?2 IS NOT NULL AND local_period=?2) OR (?3 IS NOT NULL AND substr(local_period,1,4)=?3)) GROUP BY listens.media_uri
             ), facts AS (
               SELECT track.track_key,
                      COALESCE(window.play_count,0)+COALESCE(baseline.play_count,0) play_count,
                      COALESCE(window.listened_millis,0) listened_millis
               FROM tracks track LEFT JOIN window USING(media_uri)
               LEFT JOIN period_baseline baseline ON baseline.track_object_id=track.object_id
               WHERE track.source_key=?1
             )
             SELECT genre.genre_key,genre.name,sum(facts.play_count) play_count,
                    sum(facts.listened_millis) listened_millis
             FROM facts JOIN track_genres credit USING(track_key) JOIN genres genre USING(genre_key)
             GROUP BY genre.genre_key HAVING play_count>0
             ORDER BY play_count DESC,genre.sort_text,genre.genre_key LIMIT ?5",
        )
        .bind(source).bind(&month).bind(&year).bind(lifetime).bind(limit)
        .fetch_all(&mut *transaction).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(CalendarActivitySummary {
            tracks,
            albums,
            artists,
            genres,
        })
    }

    pub async fn due_listen_deliveries(
        &self,
        now: i64,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PendingListenDelivery>> {
        let limit = limit.clamp(1, DELIVERY_LIMIT) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, PendingListenDelivery>(
            "SELECT outbox.outbox_key,outbox.listen_key,outbox.service,outbox.account_id,
                    outbox.attempts,outbox.next_attempt_at,outbox.last_error,
                    listen.external_id,listen.track_title,listen.artist_name,listen.album_title,
                    listen.started_at,listen.duration_millis,listen.listened_millis,listen.skipped
             FROM listen_outbox outbox JOIN listens listen USING(listen_key)
             WHERE outbox.next_attempt_at IS NOT NULL AND outbox.next_attempt_at<=?1
             ORDER BY outbox.next_attempt_at,outbox.outbox_key LIMIT ?2",
        )
        .bind(now)
        .bind(limit)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn complete_listen_delivery(&self, outbox: ListenOutboxKey) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query("DELETE FROM listen_outbox WHERE outbox_key=?1")
            .bind(outbox)
            .execute(connection)
            .await?
            .rows_affected()
            == 1)
    }

    pub async fn defer_listen_delivery(
        &self,
        outbox: ListenOutboxKey,
        next_attempt_at: i64,
        last_error: Option<&str>,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query("UPDATE listen_outbox SET attempts=attempts+1,next_attempt_at=?2,last_error=?3 WHERE outbox_key=?1")
            .bind(outbox).bind(next_attempt_at).bind(last_error).execute(connection).await?.rows_affected()==1)
    }

    pub async fn block_listen_deliveries(
        &self,
        service: &str,
        account_id: &str,
        last_error: &str,
    ) -> LibraryResult<u64> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query("UPDATE listen_outbox SET attempts=attempts+1,next_attempt_at=NULL,last_error=?3 WHERE service=?1 AND account_id=?2")
            .bind(service).bind(account_id).bind(last_error).execute(connection).await?.rows_affected())
    }

    pub async fn remove_listen_deliveries(
        &self,
        service: &str,
        account_id: &str,
    ) -> LibraryResult<u64> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(
            sqlx::query("DELETE FROM listen_outbox WHERE service=?1 AND account_id=?2")
                .bind(service)
                .bind(account_id)
                .execute(connection)
                .await?
                .rows_affected(),
        )
    }

    pub async fn wake_listen_deliveries(
        &self,
        service: &str,
        account_id: &str,
        now: i64,
    ) -> LibraryResult<u64> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query("UPDATE listen_outbox SET next_attempt_at=?3,last_error=NULL WHERE service=?1 AND account_id=?2 AND next_attempt_at IS NULL")
            .bind(service).bind(account_id).bind(now).execute(connection).await?.rows_affected())
    }
}

fn calendar_filter(
    period: CalendarActivityPeriod,
) -> LibraryResult<(Option<String>, Option<String>, bool)> {
    match period {
        CalendarActivityPeriod::Lifetime => Ok((None, None, true)),
        CalendarActivityPeriod::Year(year) if (1970..=9999).contains(&year) => {
            Ok((None, Some(format!("{year:04}")), false))
        }
        CalendarActivityPeriod::Year(_) => Err(LibraryError::InvalidRequest(
            "Activity year is out of range".to_string(),
        )),
        CalendarActivityPeriod::Month { year, month } => {
            if !(1..=12).contains(&month) {
                return Err(LibraryError::InvalidRequest(
                    "Activity month must be between 1 and 12".to_string(),
                ));
            }
            if !(1970..=9999).contains(&year) {
                return Err(LibraryError::InvalidRequest(
                    "Activity year is out of range".to_string(),
                ));
            }
            Ok((Some(format!("{year:04}-{month:02}")), None, false))
        }
    }
}

fn validate_listen(listen: &ListenWrite, deliveries: &[ListenDeliveryTarget]) -> LibraryResult<()> {
    if listen.external_id.as_deref().is_some_and(str::is_empty) || listen.media_uri.is_empty() {
        return Err(LibraryError::InvalidRequest(
            "listen identities cannot be empty".to_string(),
        ));
    }
    if [
        listen.musicbrainz_recording_id.as_deref(),
        listen.musicbrainz_release_track_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(str::is_empty)
    {
        return Err(LibraryError::InvalidRequest(
            "listen recording identities cannot be empty".into(),
        ));
    }
    if listen.started_at < 0 || listen.duration_millis < 0 || listen.listened_millis < 0 {
        return Err(LibraryError::InvalidRequest(
            "listen time values cannot be negative".to_string(),
        ));
    }
    if !valid_local_period(&listen.local_period) {
        return Err(LibraryError::InvalidRequest(
            "listen local period must be YYYY-MM".to_string(),
        ));
    }
    if deliveries
        .iter()
        .any(|target| target.service.is_empty() || target.account_id.is_empty())
    {
        return Err(LibraryError::InvalidRequest(
            "listen delivery identity cannot be empty".to_string(),
        ));
    }
    Ok(())
}

fn valid_local_period(period: &str) -> bool {
    let bytes = period.as_bytes();
    bytes.len() == 7
        && bytes[4] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && matches!(
            &bytes[5..],
            b"01"
                | b"02"
                | b"03"
                | b"04"
                | b"05"
                | b"06"
                | b"07"
                | b"08"
                | b"09"
                | b"10"
                | b"11"
                | b"12"
        )
}

pub(crate) async fn write_listen(
    connection: &mut SqliteConnection,
    listen: &ListenWrite,
    source_id: Option<&str>,
) -> LibraryResult<ListenKey> {
    write_imported_listen(connection, listen, source_id, None).await
}

pub(crate) async fn write_imported_listen(
    connection: &mut SqliteConnection,
    listen: &ListenWrite,
    source_id: Option<&str>,
    imported_key: Option<i64>,
) -> LibraryResult<ListenKey> {
    validate_listen(listen, &[])?;
    let inserted = sqlx::query_scalar::<_, ListenKey>(
        "INSERT INTO listens(
                 external_id,source_id,media_uri,listen_key,
                 track_title,artist_name,album_title,
                 disc_number,track_number,year,release_date,source_format,
                 musicbrainz_recording_id,musicbrainz_release_track_id,
                 started_at,local_period,duration_millis,listened_millis,skipped
             ) VALUES (
                 ?1,?2,?3,?19,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
                 ?17,?18
             )
             ON CONFLICT DO NOTHING
             RETURNING listen_key",
    )
    .bind(&listen.external_id)
    .bind(source_id)
    .bind(&listen.media_uri)
    .bind(&listen.title)
    .bind(&listen.artist)
    .bind(&listen.album)
    .bind(listen.disc_number)
    .bind(listen.track_number)
    .bind(listen.year)
    .bind(&listen.release_date)
    .bind(&listen.source_format)
    .bind(&listen.musicbrainz_recording_id)
    .bind(&listen.musicbrainz_release_track_id)
    .bind(listen.started_at)
    .bind(&listen.local_period)
    .bind(listen.duration_millis)
    .bind(listen.listened_millis)
    .bind(listen.skipped)
    .bind(imported_key)
    .fetch_optional(&mut *connection)
    .await?;
    let key = if let Some(key) = inserted {
        key
    } else {
        sqlx::query_scalar("SELECT listen_key FROM listens WHERE external_id=?1 OR (external_id IS NULL AND listen_key=?2)")
            .bind(&listen.external_id)
            .bind(imported_key)
            .fetch_optional(&mut *connection)
            .await?
            .ok_or_else(|| {
                LibraryError::InvalidRequest(
                    "the accepted listen could not be read back".to_string(),
                )
            })?
    };
    Ok(key)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityCsvFormat {
    LastFm,
    ListenBrainz,
}

#[derive(FromRow)]
struct ActivityExportRow {
    listen_key: i64,
    utc_time: String,
    source_id: Option<String>,
    #[sqlx(flatten)]
    listen: ListenWrite,
}

/// Shared accepted-Activity import inside an owner transaction.
pub(crate) async fn import_activity_jsonl_on(
    connection: &mut SqliteConnection,
    input: impl BufRead,
) -> LibraryResult<ActivityImportReport> {
    let mut report = ActivityImportReport::default();
    for line in input.lines() {
        let line = line?;
        let record = match serde_json::from_str::<ActivityRecord>(&line) {
            Ok(record) if record.version == 1 && validate_listen(&record.listen, &[]).is_ok() => {
                record
            }
            _ => {
                report.skipped += 1;
                continue;
            }
        };
        write_imported_listen(
            connection,
            &record.listen,
            record.source_id.as_deref(),
            Some(record.listen_key),
        )
        .await?;
        report.accepted += 1;
    }
    Ok(report)
}

/// Historical aggregate facts from released installations; these are not invented listens.
#[derive(Serialize, Deserialize, FromRow)]
pub(crate) struct LegacyActivityRecord {
    pub(crate) source_id: String,
    pub(crate) period: String,
    pub(crate) item_kind: String,
    pub(crate) track_object_id: String,
    pub(crate) play_count: i64,
    pub(crate) skip_count: i64,
    pub(crate) last_played_at: Option<i64>,
}

pub(crate) async fn import_legacy_activity_jsonl_on(
    connection: &mut SqliteConnection,
    input: impl BufRead,
) -> LibraryResult<ActivityImportReport> {
    let mut lines = input.lines();
    let header = lines.next().transpose()?.ok_or_else(|| {
        LibraryError::InvalidRequest("missing historical Activity version".into())
    })?;
    if serde_json::from_str::<serde_json::Value>(&header)?["version"] != 1 {
        return Err(LibraryError::InvalidRequest(
            "unsupported historical Activity version".into(),
        ));
    }
    let mut report = ActivityImportReport::default();
    for line in lines {
        let line = line?;
        let record: LegacyActivityRecord = match serde_json::from_str(&line) {
            Ok(record) => record,
            Err(_) => {
                report.skipped += 1;
                continue;
            }
        };
        if record.source_id.is_empty()
            || record.track_object_id.is_empty()
            || record.play_count < 0
            || record.skip_count < 0
        {
            report.skipped += 1;
            continue;
        }
        write_legacy_activity(connection, &record).await?;
        report.accepted += 1;
    }
    Ok(report)
}

fn activity_export_query(source_id: Option<&SourceId>) -> sqlx::QueryBuilder<sqlx::Sqlite> {
    let mut query = sqlx::QueryBuilder::new(ACTIVITY_EXPORT_SELECT);
    // Use the source index rather than an optional predicate over every listen.
    if let Some(source_id) = source_id {
        query
            .push(" WHERE source_id = ")
            .push_bind(source_id.as_str());
    }
    query.push(" ORDER BY listen_key");
    query
}

pub(crate) async fn export_activity_jsonl_on(
    connection: &mut SqliteConnection,
    mut output: impl Write,
    source_id: Option<&SourceId>,
) -> LibraryResult<u64> {
    let mut query = activity_export_query(source_id);
    let mut rows = query
        .build_query_as::<ActivityExportRow>()
        .fetch(&mut *connection);
    let mut count = 0;
    while let Some(row) = rows.try_next().await? {
        serde_json::to_writer(
            &mut output,
            &ActivityRecord {
                version: 1,
                listen_key: row.listen_key,
                source_id: row.source_id,
                listen: row.listen,
            },
        )?;
        output.write_all(b"\n")?;
        count += 1;
    }
    Ok(count)
}

pub(crate) async fn export_legacy_activity_jsonl_on(
    connection: &mut SqliteConnection,
    mut output: impl Write,
) -> LibraryResult<u64> {
    output.write_all(b"{\"version\":1}\n")?;
    let mut rows = sqlx::query_as::<_, LegacyActivityRecord>(
        "SELECT * FROM legacy_activity ORDER BY source_id,period,item_kind,track_object_id",
    )
    .fetch(&mut *connection);
    let mut count = 0;
    while let Some(row) = rows.try_next().await? {
        serde_json::to_writer(&mut output, &row)?;
        output.write_all(b"\n")?;
        count += 1;
    }
    Ok(count)
}

pub(crate) async fn write_legacy_activity(
    connection: &mut SqliteConnection,
    record: &LegacyActivityRecord,
) -> LibraryResult<()> {
    sqlx::query("INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(source_id,period,item_kind,track_object_id) DO UPDATE SET play_count=excluded.play_count,skip_count=excluded.skip_count,last_played_at=excluded.last_played_at")
            .bind(&record.source_id).bind(&record.period).bind(&record.item_kind).bind(&record.track_object_id).bind(record.play_count).bind(record.skip_count).bind(record.last_played_at).execute(&mut *connection).await?;
    Ok(())
}
