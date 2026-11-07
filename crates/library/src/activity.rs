//! Owns accepted listens, on-demand Activity summaries, and per-service delivery targets.
//! Rufin Activity is recorded independently of external private-mode delivery policy.

use std::io::{BufRead, Write};

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqliteConnection};

use crate::{
    Catalog, LibraryError, LibraryResult, ListenKey, ListenOutboxKey, ReadCancellation, SourceId,
    TrackKey, source_entity_parts,
};

const HISTORY_LIMIT: i64 = 100;
const DELIVERY_LIMIT: usize = 100;
const ACTIVITY_EXPORT_SELECT: &str = "SELECT listen_key,external_id,
                    strftime('%Y-%m-%dT%H:%M:%SZ',started_at,'unixepoch') utc_time,
                    source_id,media_uri,NULL current_media_uri,
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
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
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
pub(crate) struct ActivityImportReport {
    pub(crate) accepted: u64,
    pub(crate) skipped: u64,
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

impl Catalog {
    pub async fn record_listen(
        &self,
        listen: &ListenWrite,
        current_track: Option<TrackKey>,
        deliveries: &[ListenDeliveryTarget],
    ) -> LibraryResult<ListenKey> {
        validate_listen(listen, deliveries)?;
        let mut write = self.write().await?;
        // The player names its track; its source comes from memory.
        let source_id = match source_entity_parts(&listen.media_uri) {
            Some((source, _, _)) => Some(source.as_str().to_owned()),
            None => match current_track {
                Some(key) => {
                    let snapshot = self.snapshot();
                    snapshot
                        .tracks
                        .get(key.raw())
                        .and_then(|record| snapshot.source_key(record.source))
                        .and_then(|source| snapshot.source(source))
                        .map(|source| source.object.clone())
                }
                None => sqlx::query_scalar("SELECT source.object_id FROM tracks track JOIN sources source USING(source_key) WHERE track.media_uri=?1")
                    .bind(&listen.media_uri).fetch_optional(write.connection().await?).await?,
            },
        };
        let (key, _) = write_listen(
            &mut write,
            listen,
            source_id.as_deref(),
            current_track,
            &listen.media_uri,
            None,
        )
        .await?;
        for delivery in deliveries {
            sqlx::query("INSERT OR IGNORE INTO listen_outbox(listen_key,service,account_id,next_attempt_at) VALUES (?1,?2,?3,?4)")
                .bind(key).bind(&delivery.service).bind(&delivery.account_id)
                .bind(delivery.next_attempt_at).execute(write.connection().await?).await?;
        }
        write.commit().await?;
        Ok(key)
    }

    /// Writes listens another installation accepted, in one Write and with no scrobble. Each
    /// takes a key of this Store; a listen already here keeps the longer listening.
    pub async fn import_listens(&self, listens: &[ActivityRecord]) -> LibraryResult<u64> {
        let mut write = self.write().await?;
        let mut changed = 0;
        for record in listens {
            let uri = record
                .current_media_uri
                .as_deref()
                .unwrap_or(&record.listen.media_uri);
            let source = record.source_id.as_deref();
            if write_listen(&mut write, &record.listen, source, None, uri, None)
                .await?
                .1
            {
                changed += 1;
            }
        }
        write.commit().await?;
        Ok(changed)
    }

    /// The play ids of the listens that have one.
    pub async fn listen_ids(&self) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(
            "SELECT external_id FROM listens WHERE external_id IS NOT NULL ORDER BY external_id",
        )
        .fetch_all(&mut *connection)
        .await?)
    }

    /// The listens with these play ids, and those of these tracks.
    pub async fn listens(
        &self,
        ids: &[String],
        tracks: &[crate::TrackKey],
    ) -> LibraryResult<Vec<ActivityRecord>> {
        let mut connection = self.acquire_reader().await?;
        let rows: Vec<ActivityExportRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{ACTIVITY_EXPORT_SELECT} WHERE external_id IN(SELECT value FROM json_each(?1))
             OR media_uri IN(SELECT media_uri FROM tracks WHERE track_key IN(SELECT value FROM json_each(?2)))
             ORDER BY listen_key"
        )))
        .bind(serde_json::to_string(ids)?)
        .bind(serde_json::to_string(tracks)?)
        .fetch_all(&mut *connection)
        .await?;
        Ok(rows.into_iter().map(ActivityRecord::from).collect())
    }

    /// Deletes the listens with these play ids, in one Write.
    pub async fn delete_listens(&self, ids: &[String]) -> LibraryResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut write = self.write().await?;
        let uris: Vec<String> = sqlx::query_scalar(
            "DELETE FROM listens WHERE external_id IN(SELECT value FROM json_each(?1)) RETURNING media_uri",
        )
        .bind(serde_json::to_string(ids)?)
        .fetch_all(write.connection().await?)
        .await?;
        write
            .recount(&uris.iter().map(String::as_str).collect::<Vec<_>>())
            .await?;
        write.commit().await?;
        Ok(())
    }

    /// The historical aggregates released installations recorded, all of them or those of
    /// `keys`.
    pub async fn legacy_activity(
        &self,
        keys: Option<&[LegacyActivityKey]>,
    ) -> LibraryResult<Vec<LegacyActivityRecord>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as(
            "SELECT * FROM legacy_activity WHERE ?1 IS NULL OR (source_id,period,item_kind,track_object_id) IN
                 (SELECT value->>0,value->>1,value->>2,value->>3 FROM json_each(?1))
             ORDER BY source_id,period,item_kind,track_object_id",
        )
        .bind(keys.map(serde_json::to_string).transpose()?)
        .fetch_all(&mut *connection)
        .await?)
    }

    /// Deletes historical aggregates by key, in one Write.
    pub async fn delete_legacy_activity(&self, keys: &[LegacyActivityKey]) -> LibraryResult<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let mut write = self.write().await?;
        let removed: Vec<(String, String)> = sqlx::query_as(
            "DELETE FROM legacy_activity WHERE (source_id,period,item_kind,track_object_id) IN
                 (SELECT value->>0,value->>1,value->>2,value->>3 FROM json_each(?1))
             RETURNING source_id,track_object_id",
        )
        .bind(serde_json::to_string(keys)?)
        .fetch_all(write.connection().await?)
        .await?;
        let uris: Vec<String> = sqlx::query_scalar(
            "SELECT track.media_uri FROM json_each(?1) removed JOIN sources source ON source.object_id=removed.value->>0
             JOIN tracks track ON track.source_key=source.source_key AND track.object_id=removed.value->>1",
        )
        .bind(serde_json::to_string(&removed)?)
        .fetch_all(write.connection().await?)
        .await?;
        write
            .recount(&uris.iter().map(String::as_str).collect::<Vec<_>>())
            .await?;
        write.commit().await?;
        Ok(())
    }

    /// Writes historical aggregates in one Write, as a restore does.
    pub async fn put_legacy_activity(&self, records: &[LegacyActivityRecord]) -> LibraryResult<()> {
        let mut write = self.write().await?;
        for record in records.iter().filter(|record| record.valid()) {
            write_legacy_activity(&mut write, record).await?;
        }
        write.commit().await?;
        Ok(())
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

    pub async fn due_listen_deliveries(
        &self,
        now: i64,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PendingListenDelivery>> {
        let limit = limit.clamp(1, DELIVERY_LIMIT) as i64;
        let mut connection = self.acquire_cancellable(cancellation).await?;
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

    /// When the next waiting scrobble is due; None when none waits.
    /// When the next scrobble deferred past `now` is due, if any waits.
    pub async fn next_listen_delivery_at(&self, now: i64) -> LibraryResult<Option<i64>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(
            "SELECT min(next_attempt_at) FROM listen_outbox WHERE next_attempt_at>?1",
        )
        .bind(now)
        .fetch_one(&mut *connection)
        .await?)
    }

    pub async fn complete_listen_delivery(&self, outbox: ListenOutboxKey) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let removed = sqlx::query("DELETE FROM listen_outbox WHERE outbox_key=?1")
            .bind(outbox)
            .execute(write.connection().await?)
            .await?
            .rows_affected();
        write.commit().await?;
        Ok(removed == 1)
    }

    pub async fn defer_listen_delivery(
        &self,
        outbox: ListenOutboxKey,
        next_attempt_at: i64,
        last_error: Option<&str>,
    ) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let deferred = sqlx::query("UPDATE listen_outbox SET attempts=attempts+1,next_attempt_at=?2,last_error=?3 WHERE outbox_key=?1")
            .bind(outbox).bind(next_attempt_at).bind(last_error).execute(write.connection().await?).await?.rows_affected();
        write.commit().await?;
        Ok(deferred == 1)
    }

    pub async fn block_listen_deliveries(
        &self,
        service: &str,
        account_id: &str,
        last_error: &str,
    ) -> LibraryResult<u64> {
        let mut write = self.write().await?;
        let blocked = sqlx::query("UPDATE listen_outbox SET attempts=attempts+1,next_attempt_at=NULL,last_error=?3 WHERE service=?1 AND account_id=?2")
            .bind(service).bind(account_id).bind(last_error).execute(write.connection().await?).await?.rows_affected();
        write.commit().await?;
        Ok(blocked)
    }

    pub async fn remove_listen_deliveries(
        &self,
        service: &str,
        account_id: &str,
    ) -> LibraryResult<u64> {
        let mut write = self.write().await?;
        let removed = sqlx::query("DELETE FROM listen_outbox WHERE service=?1 AND account_id=?2")
            .bind(service)
            .bind(account_id)
            .execute(write.connection().await?)
            .await?
            .rows_affected();
        write.commit().await?;
        Ok(removed)
    }

    pub async fn wake_listen_deliveries(
        &self,
        service: &str,
        account_id: &str,
        now: i64,
    ) -> LibraryResult<u64> {
        let mut write = self.write().await?;
        let woken = sqlx::query("UPDATE listen_outbox SET next_attempt_at=?3,last_error=NULL WHERE service=?1 AND account_id=?2 AND next_attempt_at IS NULL")
            .bind(service).bind(account_id).bind(now).execute(write.connection().await?).await?.rows_affected();
        write.commit().await?;
        Ok(woken)
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

/// Writes one listen under the URI of `track` when one is named, else under `media_uri`, and
/// counts it into that track's plays in one statement.
pub(crate) async fn write_listen(
    write: &mut crate::catalog::write::Write<'_>,
    listen: &ListenWrite,
    source_id: Option<&str>,
    track: Option<TrackKey>,
    media_uri: &str,
    imported_key: Option<i64>,
) -> LibraryResult<(ListenKey, bool)> {
    validate_listen(listen, &[])?;
    let inserted = sqlx::query_as::<_, (ListenKey, Option<TrackKey>)>(
        "INSERT INTO listens(
                 external_id,source_id,media_uri,listen_key,
                 track_title,artist_name,album_title,
                 disc_number,track_number,year,release_date,source_format,
                 musicbrainz_recording_id,musicbrainz_release_track_id,
                 started_at,local_period,duration_millis,listened_millis,skipped
             ) VALUES (
                 ?1,?2,coalesce((SELECT media_uri FROM tracks WHERE track_key=?20),?3),?19,
                 ?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18
             )
             ON CONFLICT DO NOTHING
             RETURNING listen_key,(SELECT track_key FROM tracks WHERE tracks.media_uri=listens.media_uri)",
    )
    .bind(&listen.external_id)
    .bind(source_id)
    .bind(media_uri)
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
    .bind(track)
    .fetch_optional(write.connection().await?)
    .await?;
    let (key, changed) = match inserted {
        Some((key, track)) => {
            if let Some(track) = track {
                let stats = write.stats(track).heard(listen.skipped, listen.started_at);
                write.listened(track, stats);
            }
            (Some(key), true)
        }
        None => {
            // A repeated listen that was heard longer, or no longer counts as a skip.
            let updated: Option<(ListenKey, String, Option<TrackKey>)> = sqlx::query_as("UPDATE listens SET listened_millis=max(listened_millis,?2),skipped=min(skipped,?3) WHERE external_id=?1 AND(?2>listened_millis OR ?3<skipped) RETURNING listen_key,media_uri,(SELECT track_key FROM tracks WHERE tracks.media_uri=listens.media_uri)")
                .bind(&listen.external_id).bind(listen.listened_millis).bind(listen.skipped)
                .fetch_optional(write.connection().await?).await?;
            if let Some((_, uri, track)) = &updated {
                write.recount(&[uri]).await?;
                // The listen changed even when its track's counts did not.
                if let Some(track) = *track {
                    write.changed(crate::Scope::Listened { track });
                }
            }
            (updated.as_ref().map(|(key, ..)| *key), updated.is_some())
        }
    };
    // A repeated listen that changed nothing is read back for its key.
    let key = if let Some(key) = key {
        key
    } else {
        sqlx::query_scalar("SELECT listen_key FROM listens WHERE external_id=?1 OR (external_id IS NULL AND listen_key=?2)")
            .bind(&listen.external_id)
            .bind(imported_key)
            .fetch_optional(write.connection().await?)
            .await?
            .ok_or_else(|| {
                LibraryError::InvalidRequest(
                    "the accepted listen could not be read back".to_string(),
                )
            })?
    };
    Ok((key, changed))
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

impl From<ActivityExportRow> for ActivityRecord {
    fn from(row: ActivityExportRow) -> Self {
        Self {
            version: 1,
            listen_key: row.listen_key,
            source_id: row.source_id,
            current_media_uri: row.current_media_uri,
            listen: row.listen,
        }
    }
}

/// Shared accepted-Activity import inside an owner transaction.
pub(crate) async fn import_activity_jsonl_on(
    write: &mut crate::catalog::write::Write<'_>,
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
            write,
            &record.listen,
            record.source_id.as_deref(),
            None,
            record
                .current_media_uri
                .as_deref()
                .unwrap_or(&record.listen.media_uri),
            Some(record.listen_key),
        )
        .await?;
        report.accepted += 1;
    }
    Ok(report)
}

/// Historical aggregate facts from released installations; these are not invented listens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, FromRow)]
pub struct LegacyActivityRecord {
    pub source_id: String,
    pub period: String,
    pub item_kind: String,
    pub track_object_id: String,
    pub play_count: i64,
    pub skip_count: i64,
    pub last_played_at: Option<i64>,
    /// The display copy; backups made before it existed restore with the track's names.
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
}

/// A historical aggregate's key: source id, period, item kind and object id.
pub type LegacyActivityKey = (String, String, String, String);

impl LegacyActivityRecord {
    fn valid(&self) -> bool {
        !self.source_id.is_empty()
            && !self.track_object_id.is_empty()
            && self.play_count >= 0
            && self.skip_count >= 0
    }
}

pub(crate) async fn import_legacy_activity_jsonl_on(
    write: &mut crate::catalog::write::Write<'_>,
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
        if !record.valid() {
            report.skipped += 1;
            continue;
        }
        write_legacy_activity(write, &record).await?;
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
        serde_json::to_writer(&mut output, &ActivityRecord::from(row))?;
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
    write: &mut crate::catalog::write::Write<'_>,
    record: &LegacyActivityRecord,
) -> LibraryResult<()> {
    sqlx::query("INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at,title,artist,album)
        SELECT ?1,?2,?3,?4,?5,?6,?7,
            CASE WHEN ?8<>'' THEN ?8 ELSE coalesce(track.title,'') END,
            CASE WHEN ?8<>'' THEN ?9 ELSE coalesce(track.display_artist,'') END,
            CASE WHEN ?8<>'' THEN ?10 ELSE coalesce(track.display_album,'') END
        FROM (SELECT 1) LEFT JOIN tracks track ON ?3='track' AND track.source_key=(SELECT source_key FROM sources WHERE object_id=?1) AND track.object_id=?4
        WHERE true ON CONFLICT(source_id,period,item_kind,track_object_id) DO UPDATE SET play_count=excluded.play_count,skip_count=excluded.skip_count,last_played_at=excluded.last_played_at,title=excluded.title,artist=excluded.artist,album=excluded.album
        WHERE (legacy_activity.play_count,legacy_activity.skip_count,legacy_activity.last_played_at,legacy_activity.title,legacy_activity.artist,legacy_activity.album) IS NOT (excluded.play_count,excluded.skip_count,excluded.last_played_at,excluded.title,excluded.artist,excluded.album)")
        .bind(&record.source_id).bind(&record.period).bind(&record.item_kind).bind(&record.track_object_id)
        .bind(record.play_count).bind(record.skip_count).bind(record.last_played_at)
        .bind(&record.title).bind(&record.artist).bind(&record.album)
        .execute(write.connection().await?).await?;
    let uri: Option<String> = sqlx::query_scalar("SELECT media_uri FROM tracks WHERE source_key=(SELECT source_key FROM sources WHERE object_id=?1) AND object_id=?2")
        .bind(&record.source_id).bind(&record.track_object_id)
        .fetch_optional(write.connection().await?).await?;
    if let Some(uri) = uri {
        write.recount(&[&uri]).await?;
    }
    Ok(())
}

impl Catalog {
    /// The latest 100 listens, newest first, each with its track when one is left: filtered by
    /// the listen's own copy and scoped to a source. Listens are not resident; a History order
    /// reads them once, when it is built.
    pub(crate) async fn history(
        connection: &mut SqliteConnection,
        current: Option<&SourceId>,
        filter: &str,
        descending: bool,
    ) -> LibraryResult<Vec<crate::read::Listen>> {
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        let sources = if current.is_some() {
            vec![
                "listen.source_id=?1",
                "listen.source_id IS NULL AND EXISTS (SELECT 1 FROM tracks member WHERE member.media_uri=listen.media_uri AND member.source_key=(SELECT source_key FROM sources WHERE object_id=?1))",
            ]
        } else {
            vec!["1"]
        };
        // Read at most 100 from each source branch before the final merge.
        let selected = sources
            .into_iter()
            .map(|source| {
                format!(
                    "SELECT * FROM (SELECT listen.listen_key,listen.media_uri,listen.started_at FROM listens listen WHERE {source}
                    AND (instr(lower(listen.track_title),?2)>0 OR instr(lower(listen.artist_name),?2)>0 OR instr(lower(listen.album_title),?2)>0)
                    ORDER BY listen.started_at DESC,listen.listen_key DESC LIMIT {HISTORY_LIMIT})"
                )
            })
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        let direction = if descending { "DESC" } else { "ASC" };
        #[derive(FromRow)]
        struct Row {
            track_key: Option<TrackKey>,
            #[sqlx(flatten)]
            listen: ListenWrite,
        }
        let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT track.track_key,heard.*,heard.track_title title,heard.artist_name artist,heard.album_title album FROM (SELECT * FROM ({selected}) ORDER BY started_at DESC,listen_key DESC LIMIT {HISTORY_LIMIT}) listen
            JOIN listens heard ON heard.listen_key=listen.listen_key
            LEFT JOIN tracks track ON track.media_uri=listen.media_uri ORDER BY listen.started_at {direction},listen.listen_key {direction}"
        )))
        .bind(current.map(SourceId::as_str))
        .bind(filter)
        .fetch_all(connection)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| crate::read::Listen {
                track: row.track_key,
                copy: row.listen,
            })
            .collect())
    }
}

/// Shows each track as its latest listen recorded it, with Rufin's own counts of its listens.
pub(crate) async fn overlay_history(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    rows: &mut [crate::TrackRow],
) -> LibraryResult<()> {
    let uris = rows.iter().map(|row| &row.media_uri).collect::<Vec<_>>();
    let listens: Vec<ListenWrite> = sqlx::query_as("SELECT listen.*,listen.track_title title,listen.artist_name artist,listen.album_title album FROM json_each(?1) requested JOIN listens listen ON listen.listen_key=(SELECT listen_key FROM listens WHERE media_uri=requested.value ORDER BY started_at DESC,listen_key DESC LIMIT 1)")
        .bind(serde_json::to_string(&uris)?)
        .fetch_all(connection)
        .await?;
    let facts = listens
        .into_iter()
        .map(|listen| (listen.media_uri.clone(), listen))
        .collect::<std::collections::HashMap<_, _>>();
    let snapshot = catalog.snapshot();
    let context = crate::catalog::views::Context::new(&snapshot);
    for row in rows {
        if let Some(listen) = facts.get(&row.media_uri) {
            let listened = snapshot
                .tracks
                .slot(row.track_key.raw())
                .map(|slot| context.listened(slot))
                .unwrap_or_default();
            crate::read::show_listen(row, listen, listened);
        }
    }
    Ok(())
}
