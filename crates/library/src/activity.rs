//! Owns accepted listens, on-demand Activity summaries, and per-service delivery targets.
//! Rufin Activity is recorded independently of external private-mode delivery policy.

use std::io::{BufRead, Write};

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{Connection, FromRow, SqliteConnection};

use crate::{
    Database, LibraryError, LibraryResult, ListenKey, ListenOutboxKey, ReadCancellation, SourceId,
    TrackKey, source_entity_parts,
};

const HISTORY_LIMIT: i64 = 100;
const DELIVERY_LIMIT: usize = 100;
const ACTIVITY_EXPORT_SELECT: &str = "SELECT listen_key,external_id,
                    strftime('%Y-%m-%dT%H:%M:%SZ',started_at,'unixepoch') utc_time,
                    source_id,COALESCE(captured_media_uri,media_uri) media_uri,
                    CASE WHEN captured_media_uri IS NOT NULL THEN media_uri END current_media_uri,
                    track_title title,artist_name artist,album_title album,
                    duration_millis,disc_number,track_number,year,release_date,source_format,
                    musicbrainz_recording_id,musicbrainz_release_track_id,started_at,
                    local_period,listened_millis,skipped
             FROM listens";
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_media_uri: Option<String>,
    #[serde(flatten)]
    pub listen: ListenWrite,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct ActivityImportReport {
    pub accepted: u64,
    pub skipped: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListenDeliveryTarget {
    pub service: String,
    pub account_id: String,
    pub next_attempt_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum CalendarActivityPeriod {
    Month { year: i32, month: u8 },
    Year(i32),
    Lifetime,
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
        current_track: Option<TrackKey>,
        deliveries: &[ListenDeliveryTarget],
    ) -> LibraryResult<ListenKey> {
        validate_listen(listen, deliveries)?;
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let current: Option<(String, Option<crate::SourceKey>, Option<String>)> = if let Some(key) =
            current_track
        {
            Some(sqlx::query_as("SELECT track.media_uri,source.source_key,source.object_id FROM tracks track LEFT JOIN sources source USING(source_key) WHERE track.track_key=?1")
                .bind(key).fetch_one(&mut *transaction).await?)
        } else {
            sqlx::query_as("SELECT track.media_uri,source.source_key,source.object_id FROM tracks track LEFT JOIN sources source USING(source_key) WHERE track.media_uri=?1")
                .bind(&listen.media_uri).fetch_optional(&mut *transaction).await?
        };
        let current_media_uri =
            current_track.and_then(|_| current.as_ref().map(|(uri, _, _)| uri.as_str()));
        let media_uri = current_media_uri.unwrap_or(&listen.media_uri);
        let source_id = source_entity_parts(media_uri)
            .map(|(source, _, _)| source.as_str().to_string())
            .or_else(|| current.as_ref().and_then(|(_, _, id)| id.clone()));
        let origin = current
            .as_ref()
            .and_then(|(_, key, id)| id.as_ref().zip(*key))
            .map(|(id, key)| (SourceId::new(id), key));
        let (key, inserted) = write_listen(
            &mut transaction,
            listen,
            source_id.as_deref(),
            current_media_uri,
            None,
        )
        .await?;
        for delivery in deliveries {
            sqlx::query("INSERT OR IGNORE INTO listen_outbox(listen_key,service,account_id,next_attempt_at) VALUES (?1,?2,?3,?4)")
                .bind(key).bind(&delivery.service).bind(&delivery.account_id)
                .bind(delivery.next_attempt_at).execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        drop(writer);
        if inserted {
            self.catalog_updated(
                origin,
                crate::CatalogUpdate {
                    activity: true,
                    ..Default::default()
                },
            )
            .await;
        }
        Ok(key)
    }

    /// Streams a consistent snapshot without collecting the Activity history in memory.
    pub async fn export_activity_jsonl(
        &self,
        output: impl Write,
        source_id: Option<&SourceId>,
    ) -> LibraryResult<u64> {
        let (_export, mut connection) = self.acquire_export().await?;
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
        let (_export, mut connection) = self.acquire_export().await?;
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

    pub async fn due_listen_deliveries(
        &self,
        now: i64,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PendingListenDelivery>> {
        let limit = limit.clamp(1, DELIVERY_LIMIT) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_as::<_, PendingListenDelivery>(
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
        .await?)
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

fn validate_listen(listen: &ListenWrite, deliveries: &[ListenDeliveryTarget]) -> LibraryResult<()> {
    if listen.external_id.as_deref().is_some_and(str::is_empty) || listen.media_uri.is_empty() {
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

pub(crate) async fn write_listen(
    connection: &mut SqliteConnection,
    listen: &ListenWrite,
    source_id: Option<&str>,
    current_media_uri: Option<&str>,
    imported_key: Option<i64>,
) -> LibraryResult<(ListenKey, bool)> {
    validate_listen(listen, &[])?;
    crate::catalog::remember_track(
        connection,
        &crate::TrackReference {
            media_uri: current_media_uri.unwrap_or(&listen.media_uri).to_owned(),
            metadata: crate::TrackMetadata {
                title: listen.title.clone(),
                artist: listen.artist.clone(),
                album: listen.album.clone(),
                duration_millis: listen.duration_millis,
                disc_number: listen.disc_number.unwrap_or_default(),
                track_number: listen.track_number.unwrap_or_default(),
                year: listen.year,
                release_date: listen.release_date.clone(),
                source_format: listen.source_format.clone(),
                musicbrainz_recording_id: listen.musicbrainz_recording_id.clone(),
                musicbrainz_release_track_id: listen.musicbrainz_release_track_id.clone(),
                ..Default::default()
            },
            artwork_binding: None,
        },
    )
    .await?;
    let inserted = sqlx::query_scalar::<_, ListenKey>(
        "INSERT INTO listens(
                 external_id,source_id,media_uri,listen_key,captured_media_uri,
                 track_title,artist_name,album_title,
                 disc_number,track_number,year,release_date,source_format,
                 musicbrainz_recording_id,musicbrainz_release_track_id,
                 started_at,local_period,duration_millis,listened_millis,skipped
             ) VALUES (
                 ?1,?2,?3,?19,?20,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
                 ?17,?18
             )
             ON CONFLICT DO NOTHING
             RETURNING listen_key",
    )
    .bind(&listen.external_id)
    .bind(source_id)
    .bind(current_media_uri.unwrap_or(&listen.media_uri))
    .bind(&listen.title)
    .bind(&listen.artist)
    .bind(&listen.album)
    .bind(listen.disc_number)
    .bind(listen.track_number)
    .bind(listen.year)
    .bind(&listen.release_date)
    .bind(&listen.source_format)
    .bind(
        listen
            .musicbrainz_recording_id
            .as_deref()
            .filter(|id| !id.is_empty()),
    )
    .bind(
        listen
            .musicbrainz_release_track_id
            .as_deref()
            .filter(|id| !id.is_empty()),
    )
    .bind(listen.started_at)
    .bind(&listen.local_period)
    .bind(listen.duration_millis)
    .bind(listen.listened_millis)
    .bind(listen.skipped)
    .bind(imported_key)
    .bind(
        current_media_uri
            .filter(|uri| *uri != listen.media_uri)
            .map(|_| &listen.media_uri),
    )
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
    Ok((key, inserted.is_some()))
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
    current_media_uri: Option<String>,
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
        write_listen(
            connection,
            &record.listen,
            record.source_id.as_deref(),
            record.current_media_uri.as_deref(),
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
                current_media_uri: row.current_media_uri,
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
    if record.item_kind == "track" {
        let known = sqlx::query_scalar::<_, String>(
            "SELECT track.media_uri FROM tracks track JOIN sources source USING(source_key)
             WHERE source.object_id=?1 AND track.object_id=?2",
        )
        .bind(&record.source_id)
        .bind(&record.track_object_id)
        .fetch_optional(&mut *connection)
        .await?;
        if known.is_none() {
            let uri =
                crate::normalize_direct_media_uri(&record.track_object_id).unwrap_or_else(|| {
                    crate::source_entity_uri(
                        &SourceId::new(&record.source_id),
                        "track",
                        &record.track_object_id,
                    )
                });
            crate::catalog::remember_track(
                connection,
                &crate::TrackReference {
                    media_uri: uri,
                    ..Default::default()
                },
            )
            .await?;
        }
    }
    sqlx::query("INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(source_id,period,item_kind,track_object_id) DO UPDATE SET play_count=excluded.play_count,skip_count=excluded.skip_count,last_played_at=excluded.last_played_at")
            .bind(&record.source_id).bind(&record.period).bind(&record.item_kind).bind(&record.track_object_id).bind(record.play_count).bind(record.skip_count).bind(record.last_played_at).execute(&mut *connection).await?;
    Ok(())
}

pub(crate) fn history_query(
    current: Option<&SourceId>,
    filter: &str,
    descending: bool,
) -> crate::query::OrderedQuery {
    let current = current.map(|source| crate::query::quote(source.as_str()));
    let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
    let filter = crate::query::quote(&filter);
    let sources = if let Some(source) = current.as_ref() {
        vec![
            format!("listen.source_id={source}"),
            format!(
                "listen.source_id IS NULL AND EXISTS (
            SELECT 1 FROM tracks member WHERE member.media_uri=listen.media_uri
            AND member.source_key=(SELECT source_key FROM sources WHERE object_id={source}))"
            ),
        ]
    } else {
        vec!["1".into()]
    };
    let recent_source = if let Some(source) = current.as_ref() {
        format!(
            "AND (recent.source_id={source} OR (recent.source_id IS NULL AND EXISTS (
            SELECT 1 FROM tracks member WHERE member.media_uri=recent.media_uri
            AND member.source_key=(SELECT source_key FROM sources WHERE object_id={source}))))"
        )
    } else {
        String::new()
    };
    let filter = format!("AND (instr(lower(listen.track_title),{filter})>0 OR instr(lower(listen.artist_name),{filter})>0
        OR instr(lower(listen.album_title),{filter})>0)");
    let recent_filter = filter.replace("listen.", "recent.");
    // Read at most 100 from each indexed source branch before the final merge.
    let selected = sources
        .into_iter()
        .map(|source| {
            format!(
                "SELECT * FROM (SELECT listen.* FROM listens listen WHERE {source} {filter}
        AND listen.listen_key=(SELECT recent.listen_key FROM listens recent
            WHERE recent.media_uri=listen.media_uri {recent_source} {recent_filter}
            ORDER BY recent.started_at DESC,recent.listen_key DESC LIMIT 1)
        ORDER BY listen.started_at DESC,listen.listen_key DESC LIMIT {HISTORY_LIMIT})"
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    let direction = if descending { "DESC" } else { "ASC" };
    crate::query::OrderedQuery {
        albumless:false,
        key:"listen.listen_key".into(),text:None,
        from: format!("(SELECT * FROM ({selected}) ORDER BY started_at DESC,listen_key DESC LIMIT {HISTORY_LIMIT}) listen
            JOIN tracks track USING(media_uri) LEFT JOIN albums album ON album.album_key=track.album_key"),
        predicate: "1".into(),
        order: vec![format!("listen.started_at {direction}"), format!("listen.listen_key {direction}")],
        uri: "track.media_uri".into(),
        entry_key: "NULL".into(),
        arguments: Default::default(),
    }
}

pub(crate) async fn load_history_page(
    connection: &mut SqliteConnection,
    query: &crate::query::OrderedQuery,
    offset: usize,
    limit: usize,
) -> LibraryResult<Vec<crate::TrackView>> {
    let selected: Vec<(TrackKey, ListenKey)> = crate::query::selected_rows_on(
        connection,
        query,
        "track.track_key,listen.listen_key",
        Some(std::slice::from_ref(
            &(offset..offset.saturating_add(limit.min(HISTORY_LIMIT as usize))),
        )),
        None,
    )
    .await?;
    let keys = selected.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let listens = selected.iter().map(|(_, key)| *key).collect::<Vec<_>>();
    let mut songs = crate::tracks::load_track_rows(connection, &keys).await?;
    overlay_history(connection, &mut songs, Some(&listens)).await?;
    Ok(songs)
}

pub(crate) async fn overlay_history(
    connection: &mut SqliteConnection,
    songs: &mut [crate::TrackView],
    selected: Option<&[ListenKey]>,
) -> LibraryResult<()> {
    use sqlx::Row;
    let uris = songs.iter().map(|song| &song.media_uri).collect::<Vec<_>>();
    let sql = format!(
        "SELECT listen.*,listen.track_title title,listen.artist_name artist,listen.album_title album,{} local_plays,{} local_skips FROM json_each(?1) requested JOIN tracks track ON track.media_uri=requested.value JOIN listens listen ON listen.listen_key=coalesce(json_extract(?2,'$['||requested.key||']'),(SELECT listen_key FROM listens WHERE media_uri=track.media_uri ORDER BY started_at DESC,listen_key DESC LIMIT 1))",
        crate::tracks::local_stat("play_count"),
        crate::tracks::local_stat("skip_count")
    );
    let mut facts = std::collections::HashMap::new();
    for row in sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(serde_json::to_string(&uris)?)
        .bind(selected.map(serde_json::to_string).transpose()?)
        .fetch_all(connection)
        .await?
    {
        let listen = ListenWrite::from_row(&row)?;
        let plays = row.try_get::<i64, _>("local_plays")?;
        let skips = row.try_get::<i64, _>("local_skips")?;
        facts.insert(listen.media_uri.clone(), (listen, plays, skips));
    }
    for song in songs {
        let Some((listen, plays, skips)) = facts.get(&song.media_uri) else {
            continue;
        };
        let metadata = &mut song.metadata;
        metadata.title.clone_from(&listen.title);
        metadata.artist.clone_from(&listen.artist);
        metadata.album.clone_from(&listen.album);
        metadata.duration_millis = listen.duration_millis;
        metadata.disc_number = listen.disc_number.unwrap_or_default();
        metadata.track_number = listen.track_number.unwrap_or_default();
        metadata.year = listen.year;
        metadata.release_date.clone_from(&listen.release_date);
        metadata.source_format.clone_from(&listen.source_format);
        metadata
            .musicbrainz_recording_id
            .clone_from(&listen.musicbrainz_recording_id);
        metadata
            .musicbrainz_release_track_id
            .clone_from(&listen.musicbrainz_release_track_id);
        song.state.last_played = Some(listen.started_at);
        song.state.play_count = *plays;
        song.state.skip_count = *skips;
    }
    Ok(())
}
