//! Owns accepted listens, on-demand Activity summaries, and per-service delivery targets.
//! Rufin Activity is recorded independently of external private-mode delivery policy.

use sqlx::{Connection, FromRow};

use crate::{
    AlbumKey, ArtistKey, Database, GenreKey, LibraryError, LibraryResult, ListenKey,
    ListenOutboxKey, ReadCancellation, SourceKey, TrackKey, TrackRoutePage,
    tracks::load_track_rows,
};

const HISTORY_LIMIT: i64 = 100;
const ACTIVITY_RESULT_LIMIT: usize = 100;
const DELIVERY_LIMIT: usize = 100;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListenWrite {
    pub external_id: String,
    pub track_key: Option<TrackKey>,
    pub track_object_id: String,
    pub track_title: String,
    pub artist_name: String,
    pub album_title: String,
    pub started_at: i64,
    pub local_period: String,
    pub duration_millis: i64,
    pub listened_millis: i64,
    pub skipped: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListenDeliveryTarget {
    pub service: String,
    pub account_id: String,
    pub next_attempt_at: Option<i64>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct ActivityHistoryRow {
    pub listen_key: ListenKey,
    pub external_id: Option<String>,
    pub track_key: Option<TrackKey>,
    pub track_object_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub started_at: i64,
    pub duration_millis: i64,
    pub listened_millis: i64,
    pub skipped: bool,
    pub artwork_binding: Option<Vec<u8>>,
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
        source: SourceKey,
        listen: &ListenWrite,
        deliveries: &[ListenDeliveryTarget],
    ) -> LibraryResult<ListenKey> {
        validate_listen(listen, deliveries)?;
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let inserted = sqlx::query_scalar::<_, ListenKey>(
            "INSERT INTO listens(external_id,source_key,track_key,track_object_id,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped)
             SELECT ?2,?1,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12
             WHERE ?3 IS NULL OR EXISTS (
               SELECT 1 FROM tracks WHERE source_key=?1 AND track_key=?3
             )
             ON CONFLICT(external_id) DO NOTHING
             RETURNING listen_key",
        )
        .bind(source)
        .bind(&listen.external_id)
        .bind(listen.track_key)
        .bind(&listen.track_object_id)
        .bind(&listen.track_title)
        .bind(&listen.artist_name)
        .bind(&listen.album_title)
        .bind(listen.started_at)
        .bind(&listen.local_period)
        .bind(listen.duration_millis)
        .bind(listen.listened_millis)
        .bind(listen.skipped)
        .fetch_optional(&mut *transaction)
        .await?;
        let key = if let Some(key) = inserted {
            key
        } else {
            sqlx::query_scalar("SELECT listen_key FROM listens WHERE external_id=?1")
                .bind(&listen.external_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| {
                    LibraryError::InvalidRequest(
                        "the listen Track does not belong to the active source".to_string(),
                    )
                })?
        };
        for delivery in deliveries {
            sqlx::query("INSERT OR IGNORE INTO listen_outbox(listen_key,service,account_id,next_attempt_at) VALUES (?1,?2,?3,?4)")
                .bind(key).bind(&delivery.service).bind(&delivery.account_id)
                .bind(delivery.next_attempt_at).execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        Ok(key)
    }

    pub async fn activity_history(
        &self,
        source: SourceKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ActivityHistoryRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, ActivityHistoryRow>(
            "SELECT listen.listen_key,listen.external_id,listen.track_key,
                    listen.track_object_id,COALESCE(track.title,listen.track_title) title,
                    COALESCE(track.display_artist,listen.artist_name) artist,
                    COALESCE(track.display_album,listen.album_title) album,
                    listen.started_at,COALESCE(track.duration_millis,listen.duration_millis) duration_millis,
                    listen.listened_millis,listen.skipped,
                    COALESCE(album.artwork_binding,track.artwork_binding) artwork_binding
             FROM listens listen LEFT JOIN tracks track USING(track_key)
             LEFT JOIN albums album ON album.album_key=track.album_key
             WHERE listen.source_key=?1
             ORDER BY listen.started_at DESC,listen.listen_key DESC LIMIT ?2",
        )
        .bind(source)
        .bind(HISTORY_LIMIT)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn history_track_page(
        &self,
        source: SourceKey,
        folder: Option<crate::FolderKey>,
        query: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<TrackRoutePage> {
        let query: String = query.trim().to_lowercase().chars().take(256).collect();
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let order = sqlx::query_scalar::<_, TrackKey>(
            "SELECT track.track_key FROM listens listen
             JOIN tracks track USING(track_key)
             WHERE listen.source_key=?1
               AND (?2 IS NULL OR EXISTS (
                 SELECT 1 FROM track_folders scope
                 WHERE scope.track_key=track.track_key AND scope.folder_key=?2))
               AND (?3 OR instr(lower(track.title),?4)>0
                    OR instr(lower(track.display_artist),?4)>0
                    OR instr(lower(track.display_album),?4)>0)
             GROUP BY track.track_key
             ORDER BY max(listen.started_at) DESC,track.track_key DESC
             LIMIT ?5",
        )
        .bind(source)
        .bind(folder)
        .bind(query.is_empty())
        .bind(query)
        .bind(HISTORY_LIMIT)
        .fetch_all(&mut *transaction)
        .await?;
        let first_rows =
            load_track_rows(&mut transaction, source, &order[..order.len().min(64)]).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(TrackRoutePage { order, first_rows })
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
               SELECT track_key,count(*) play_count,COALESCE(sum(skipped),0) skip_count,
                      max(started_at) last_played,COALESCE(sum(listened_millis),0) listened_millis
               FROM listens WHERE source_key=?1 AND track_key IS NOT NULL
                 AND (?4 OR (?2 IS NOT NULL AND local_period=?2) OR (?3 IS NOT NULL AND substr(local_period,1,4)=?3))
               GROUP BY track_key
             )
             SELECT track.track_key,track.title,track.display_artist artist,track.display_album album,
                    COALESCE(window.play_count,0)+COALESCE(baseline.play_count,0) play_count,
                    COALESCE(window.skip_count,0)+COALESCE(baseline.skip_count,0) skip_count,
                    CASE WHEN baseline.last_played_at IS NULL THEN window.last_played
                         WHEN window.last_played IS NULL THEN baseline.last_played_at
                         ELSE max(window.last_played,baseline.last_played_at) END last_played,
                    COALESCE(window.listened_millis,0) listened_millis
             FROM tracks track LEFT JOIN window USING(track_key)
             LEFT JOIN period_baseline baseline ON baseline.track_object_id=track.object_id
             WHERE track.source_key=?1 AND (COALESCE(window.play_count,0)+COALESCE(baseline.play_count,0))>0
             ORDER BY play_count DESC,last_played DESC NULLS LAST,track.sort_text,track.track_key LIMIT ?5",
        )
        .bind(source).bind(&month).bind(&year).bind(lifetime).bind(limit)
        .fetch_all(&mut *transaction).await?;
        let albums = sqlx::query_as::<_, ActivityAlbumRow>(
            "WITH period_baseline AS (SELECT track_object_id,sum(play_count) play_count,sum(skip_count) skip_count,max(last_played_at) last_played_at FROM activity_baseline WHERE source_key=?1 AND item_kind='track' AND ((?4 AND period='lifetime') OR (NOT ?4 AND ((?2 IS NOT NULL AND period=?2) OR (?3 IS NOT NULL AND substr(period,1,4)=?3)))) GROUP BY track_object_id), window AS (
               SELECT track_key,count(*) play_count,COALESCE(sum(listened_millis),0) listened_millis
               FROM listens WHERE source_key=?1 AND track_key IS NOT NULL
                 AND (?4 OR (?2 IS NOT NULL AND local_period=?2) OR (?3 IS NOT NULL AND substr(local_period,1,4)=?3)) GROUP BY track_key
             ), facts AS (
               SELECT track.track_key,track.album_key,
                      COALESCE(window.play_count,0)+COALESCE(baseline.play_count,0) play_count,
                      COALESCE(window.listened_millis,0) listened_millis
               FROM tracks track LEFT JOIN window USING(track_key)
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
               SELECT track_key,count(*) play_count,COALESCE(sum(listened_millis),0) listened_millis
               FROM listens WHERE source_key=?1 AND track_key IS NOT NULL
                 AND (?4 OR (?2 IS NOT NULL AND local_period=?2) OR (?3 IS NOT NULL AND substr(local_period,1,4)=?3)) GROUP BY track_key
             ), facts AS (
               SELECT track.track_key,
                      COALESCE(window.play_count,0)+COALESCE(baseline.play_count,0) play_count,
                      COALESCE(window.listened_millis,0) listened_millis
               FROM tracks track LEFT JOIN window USING(track_key)
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
               SELECT track_key,count(*) play_count,COALESCE(sum(listened_millis),0) listened_millis
               FROM listens WHERE source_key=?1 AND track_key IS NOT NULL
                 AND (?4 OR (?2 IS NOT NULL AND local_period=?2) OR (?3 IS NOT NULL AND substr(local_period,1,4)=?3)) GROUP BY track_key
             ), facts AS (
               SELECT track.track_key,
                      COALESCE(window.play_count,0)+COALESCE(baseline.play_count,0) play_count,
                      COALESCE(window.listened_millis,0) listened_millis
               FROM tracks track LEFT JOIN window USING(track_key)
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
    if listen.external_id.is_empty() || listen.track_object_id.is_empty() {
        return Err(LibraryError::InvalidRequest(
            "listen identities cannot be empty".to_string(),
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
