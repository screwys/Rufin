//! Shared edits beside the working catalog. Ordered edits use Loro; completed
//! file results and other current values use the catalog's change journal.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::{CONNECT_PAGE_SIZE, ConnectRecord, Database};
use anyhow::{Context, Result, bail};
use flate2::bufread::MultiGzDecoder;
use loro::{ExportMode, LoroDoc, ToJson, VersionVector};
use serde::{Deserialize, Serialize};
use sqlx::{Connection, QueryBuilder, Row, SqliteConnection};
use tokio::sync::{MappedMutexGuard, MutexGuard};

mod database;
mod encryption;
mod history;
use encryption::{expose, expose_payload, protect, sensitive};
#[cfg(test)]
mod history_tests;

// Wire format, independent of Rufin releases and local database migrations.
const FORMAT: u32 = 1;
const CURRENT_FORMAT: u32 = 4;
pub const INCOMPATIBLE_PROFILE: &str =
    "Devices using Rufin Connect are no longer compatible. Please update Rufin.";
const PROJECTION_ORDER: &str = "CASE kind WHEN 'source' THEN 0 WHEN 'integration' THEN 0 WHEN 'playlist' THEN 1 WHEN 'native_playlist' THEN 1 WHEN 'album' THEN 2 WHEN 'artist' THEN 2 WHEN 'genre' THEN 2 WHEN 'mood' THEN 2 WHEN 'folder' THEN 2 WHEN 'file' THEN 2 WHEN 'track' THEN 3 WHEN 'reference' THEN 3 WHEN 'home' THEN 6 WHEN 'entry' THEN 5 WHEN 'native_entry' THEN 5 WHEN 'playlist_order' THEN 7 WHEN 'smart_order' THEN 7 WHEN 'album_artists' THEN 6 WHEN 'album_genres' THEN 6 WHEN 'track_artists' THEN 6 WHEN 'track_genres' THEN 6 WHEN 'track_moods' THEN 6 WHEN 'track_folders' THEN 6 ELSE 4 END,CASE WHEN kind IN ('entry','native_entry') THEN json_extract(payload,'$.playlist') END,CASE WHEN kind IN ('entry','native_entry','playlist_order','smart_order') THEN json_extract(payload,'$.position') END,object_key";

pub struct ProfileStore {
    database: Database,
    peer_id: u64,
    identity: Arc<age::x25519::Identity>,
    closed: AtomicBool,
}

#[derive(Serialize, Deserialize)]
struct Update {
    version: u32,
    document: String,
    bytes: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current: Option<ConnectRecord>,
}

/// A document's causal version and revision in this installation's change index.
/// Revisions are local cursors, never Loro versions or values to compare across peers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentVersion {
    pub name: String,
    pub version: Vec<u8>,
    pub revision: i64,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncStream {
    Authored = 2,
    NativeCache = 3,
}

impl ProfileStore {
    pub async fn open(
        database: Database,
        peer_id: u64,
        key: age::x25519::Identity,
    ) -> Result<Self> {
        let identity = Arc::new(key);
        let mut writer = database.writer().await?;
        let connection = writer.as_mut().context("Library writer is closed")?;
        let mut transaction = connection.begin().await?;
        let version: i64 =
            sqlx::query_scalar("SELECT schema_version FROM connect_profile WHERE singleton=1")
                .fetch_one(&mut *transaction)
                .await?;
        if version < 1 {
            sqlx::raw_sql(
                "DELETE FROM documents WHERE (name GLOB 'cache_artwork:*' OR name GLOB 'cache_lyrics:*');
                DELETE FROM history_pending WHERE (name GLOB 'cache_artwork:*' OR name GLOB 'cache_lyrics:*');
                DELETE FROM history_acknowledgements WHERE (peer,name) IN (
                    SELECT peer,name FROM history_acknowledgements WHERE (name GLOB 'cache_artwork:*' OR name GLOB 'cache_lyrics:*')
                );
                UPDATE connect_profile SET schema_version=1 WHERE singleton=1;",
            )
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        sqlx::query("UPDATE connect_profile SET actor=?1 WHERE singleton=1 AND actor IS NOT ?1")
            .bind(peer_id.to_string())
            .execute(&mut *connection)
            .await?;
        if version < 2 {
            index_documents(connection).await?;
            migrate_pending_on(connection, peer_id, &identity).await?;
            sqlx::query("UPDATE connect_profile SET schema_version=2 WHERE singleton=1")
                .execute(&mut *connection)
                .await?;
        }
        drop(writer);
        Ok(Self {
            database,
            peer_id,
            identity,
            closed: AtomicBool::new(false),
        })
    }

    pub async fn close(&self) -> Result<()> {
        self.closed.store(true, Ordering::Release);
        let _writer = self.database.writer().await?;
        let _export = self.database.acquire_export().await?;
        Ok(())
    }

    async fn writer(&self) -> Result<MappedMutexGuard<'_, SqliteConnection>> {
        let guard = self.database.writer().await?;
        if self.closed.load(Ordering::Acquire) {
            bail!("Connect profile is closed");
        }
        Ok(MutexGuard::map(guard, |connection| {
            connection.as_mut().unwrap()
        }))
    }

    pub async fn device_name(&self, peer: &str) -> Result<Option<String>> {
        let mut connection = self.writer().await?;
        let document = self
            .load_document(&mut connection, &format!("device:{peer}"))
            .await?;
        Ok(records(&document)?
            .remove(&("device".into(), peer.into()))
            .and_then(|value| value.as_str().map(str::to_owned)))
    }

    pub async fn revision(&self) -> Result<i64> {
        let mut connection = self.writer().await?;
        Ok(
            sqlx::query_scalar("SELECT revision FROM file_revision WHERE id=1")
                .fetch_one(&mut *connection)
                .await?,
        )
    }

    pub async fn sync_cursor_in(&self, peer: &str, stream: SyncStream) -> Result<i64> {
        let mut connection = self.writer().await?;
        Ok(
            sqlx::query_scalar("SELECT revision FROM sync_cursors WHERE peer=?1 AND setup=?2")
                .bind(peer)
                .bind(stream as i64)
                .fetch_optional(&mut *connection)
                .await?
                .unwrap_or(0),
        )
    }

    pub async fn acknowledge_sync_in(
        &self,
        peer: &str,
        stream: SyncStream,
        revision: i64,
    ) -> Result<()> {
        let mut connection = self.writer().await?;
        sqlx::query("INSERT INTO sync_cursors(peer,setup,revision) VALUES(?1,?2,?3) ON CONFLICT(peer,setup) DO UPDATE SET revision=excluded.revision")
            .bind(peer).bind(stream as i64).bind(revision).execute(&mut *connection).await?;
        Ok(())
    }

    /// Read only the current observations needed by a media transfer.
    pub async fn current_records(&self, keys: &[(String, String)]) -> Result<Vec<ConnectRecord>> {
        let mut connection = self.writer().await?;
        let mut records = Vec::new();
        for (kind, key) in keys {
            let record = current_record(&mut connection, kind, key).await?;
            if record.value.is_some() {
                records.push(record);
            }
        }
        Ok(records)
    }

    pub async fn control_records(&self, after: i64) -> Result<Vec<ConnectRecord>> {
        let mut connection = self.writer().await?;
        let mut output = Vec::new();
        for row in sqlx::query("SELECT name,snapshot FROM documents WHERE revision>?1 AND substr(name,1,instr(name,':')-1) IN ('device','connect_key','connect_network')").bind(after).fetch_all(&mut *connection).await? {
            let name:String=row.get(0);
            let document=LoroDoc::from_snapshot(&expose(&name,row.get(1),&self.identity)?)?;
            output.extend(records(&document)?.into_iter().map(|((kind,key),value)|ConnectRecord{kind,key,value:Some(value)}));
        }
        Ok(output)
    }

    /// Read one bounded sync stream after its last committed revision.
    pub async fn changes_in_stream(
        &self,
        after: i64,
        limit: usize,
        stream: SyncStream,
    ) -> Result<Vec<DocumentVersion>> {
        let mut connection = self.writer().await?;
        if stream == SyncStream::NativeCache {
            return sqlx::query("SELECT name,CAST(version AS BLOB),revision,1 FROM connect_versions WHERE kind NOT IN('state','smart','reference') AND revision>?1 ORDER BY revision,name LIMIT ?2")
                .bind(after).bind(i64::try_from(limit)?).fetch_all(&mut *connection).await?
                .into_iter().map(version_row).collect();
        }
        let predicate = "name NOT GLOB 'catalog:*' AND name NOT GLOB 'analysis:*'";
        Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT name,version,revision,current FROM (SELECT name,version,revision,0 current FROM documents WHERE {predicate} UNION ALL SELECT name,CAST(version AS BLOB),revision,1 FROM connect_versions WHERE kind IN('state','smart','reference')) WHERE revision>?1 AND revision<=coalesce((SELECT min(revision) FROM documents WHERE {predicate} AND revision>?1 AND (name GLOB 'playlist_artwork:*')),9223372036854775807) ORDER BY revision,name LIMIT ?2"
        )))
        .bind(after).bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&mut *connection).await?
        .into_iter().map(version_row).collect::<Result<Vec<_>>>()?)
    }

    /// Return versions in the requested order. Missing documents have an encoded
    /// empty version vector and revision zero, allowing their full history to sync.
    pub async fn versions(&self, names: &[String]) -> Result<Vec<DocumentVersion>> {
        let mut connection = self.writer().await?;
        let mut versions = Vec::with_capacity(names.len());
        for page in names.chunks(CONNECT_PAGE_SIZE) {
            let mut query = QueryBuilder::new("WITH requested(name) AS (");
            query.push_values(page, |mut row, name| {
                row.push_bind(name);
            });
            query.push(") SELECT d.name,d.version,d.revision,0 FROM requested r JOIN documents d USING(name) UNION ALL SELECT v.name,CAST(v.version AS BLOB),v.revision,1 FROM requested r JOIN connect_versions v USING(name)");
            let saved = query
                .build()
                .fetch_all(&mut *connection)
                .await?
                .into_iter()
                .map(version_row)
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .map(|version| (version.name.clone(), version))
                .collect::<BTreeMap<_, _>>();
            versions.extend(page.iter().map(|name| {
                saved.get(name).cloned().unwrap_or_else(|| DocumentVersion {
                    name: name.clone(),
                    version: if current_identity(name).is_some() {
                        crate::RecordStamp::default().encode()
                    } else {
                        VersionVector::default().encode()
                    },
                    revision: 0,
                })
            }));
        }
        Ok(versions)
    }

    /// Export data absent from the supplied remote version vectors.
    /// Unknown documents and versions the remote already contains produce no update.
    pub async fn updates(&self, versions: &[DocumentVersion]) -> Result<Vec<Vec<u8>>> {
        let mut connection = self.writer().await?;
        Self::updates_on(&mut connection, &self.identity, versions)
            .await?
            .iter()
            .map(serde_json::to_vec)
            .collect::<std::result::Result<_, _>>()
            .map_err(Into::into)
    }

    async fn updates_on(
        connection: &mut SqliteConnection,
        identity: &age::x25519::Identity,
        versions: &[DocumentVersion],
    ) -> Result<Vec<Update>> {
        let mut updates = Vec::new();
        for version in versions {
            if let Some((kind, key)) = current_identity(&version.name) {
                let local: Option<String> =
                    sqlx::query_scalar("SELECT version FROM connect_versions WHERE name=?1")
                        .bind(&version.name)
                        .fetch_optional(&mut *connection)
                        .await?;
                if let Some(local) = local {
                    let local = current_stamp(&local)?;
                    let remote = crate::RecordStamp::decode(&version.version).unwrap_or_default();
                    if remote == local || (kind != "listen" && remote > local) {
                        continue;
                    }
                    updates.push(Update {
                        version: CURRENT_FORMAT,
                        document: version.name.clone(),
                        bytes: local.encode(),
                        author: None,
                        current: Some(current_record(connection, kind, key).await?),
                    });
                }
                continue;
            }
            let local: Option<Vec<u8>> =
                sqlx::query_scalar("SELECT version FROM documents WHERE name=?1")
                    .bind(&version.name)
                    .fetch_optional(&mut *connection)
                    .await?;
            let Some(local) = local else { continue };
            let remote = VersionVector::decode(&version.version)?;
            if remote.includes_vv(&VersionVector::decode(&local)?) {
                continue;
            }
            let snapshot: Vec<u8> =
                sqlx::query_scalar("SELECT snapshot FROM documents WHERE name=?1")
                    .bind(&version.name)
                    .fetch_one(&mut *connection)
                    .await?;
            let bytes = expose(&version.name, &snapshot, identity)?;
            let bytes = if remote.is_empty() {
                bytes.into_owned()
            } else {
                LoroDoc::from_snapshot(&bytes)?.export(ExportMode::updates(&remote))?
            };
            updates.push(Update {
                version: FORMAT,
                document: version.name.clone(),
                bytes,
                author: None,
                current: None,
            });
        }
        Ok(updates)
    }

    /// Import a page atomically. Returns whether projected values changed, so
    /// repeated or history-only imports do not cause a Store or UI refresh.
    pub async fn import_updates(&self, updates: &[Vec<u8>]) -> Result<bool> {
        let mut connection = self.writer().await?;
        let mut transaction = connection.begin().await?;
        let mut changed = false;
        for page in updates.chunks(CONNECT_PAGE_SIZE) {
            let page = page
                .iter()
                .filter_map(|bytes| decode_update(bytes).transpose())
                .collect::<Result<Vec<_>>>()?;
            changed |= self.import_page_on(&mut transaction, page, false).await?.0;
        }
        transaction.commit().await?;
        Ok(changed)
    }

    /// Save profile control records.
    pub async fn write_records(&self, records: &[ConnectRecord]) -> Result<usize> {
        let mut connection = self.writer().await?;
        let mut transaction = connection.begin().await?;
        let changed = write_records_on(
            &mut transaction,
            self.peer_id,
            Some(&self.identity),
            records,
        )
        .await?;
        transaction.commit().await?;
        Ok(changed)
    }

    #[cfg(test)]
    async fn stored_records(&self) -> Result<Vec<ConnectRecord>> {
        let mut connection = self.writer().await?;
        let mut output = Vec::new();
        for row in sqlx::query("SELECT name,snapshot FROM documents")
            .fetch_all(&mut *connection)
            .await?
        {
            let name: String = row.get(0);
            let document = LoroDoc::from_snapshot(&expose(&name, row.get(1), &self.identity)?)?;
            output.extend(records(&document)?.into_iter().map(|((kind, key), value)| {
                ConnectRecord {
                    kind,
                    key,
                    value: Some(value),
                }
            }));
        }
        let keys: Vec<(String,String)> = sqlx::query_as("SELECT kind,object_key FROM connect_versions WHERE kind IN('state','smart','reference') AND deleted=0")
            .fetch_all(&mut *connection).await?;
        for (kind, key) in keys {
            output.push(current_record(&mut connection, &kind, &key).await?);
        }
        Ok(output)
    }

    /// Convert a legacy snapshot in one SQLite transaction. A corrupt or truncated
    /// input leaves the staged profile unchanged.
    pub async fn replace_snapshot<R: Read + Send + 'static>(
        &self,
        file: R,
        progress: impl Fn(usize) + Send,
    ) -> Result<bool> {
        progress(0);
        let mut input = tokio::task::spawn_blocking(move || snapshot_start(file)).await??;
        let mut peer = None;
        let mut connection = self.writer().await?;
        let mut transaction = connection.begin().await?;
        sqlx::raw_sql(
            "DELETE FROM documents; DELETE FROM connect_versions; DELETE FROM sync_cursors; DELETE FROM history_peers; DELETE FROM history_acknowledgements; DELETE FROM history_pending;",
        )
        .execute(&mut *transaction)
        .await?;
        let mut changed = false;
        let mut imported = 0;
        loop {
            // Only file and CPU work moves to the blocking pool. The caller
            // retains the transaction so cancellation still rolls back the import.
            let (reader, page, ended) =
                tokio::task::spawn_blocking(move || read_update_page(input)).await??;
            input = reader;
            let count = page.len();
            let mut page = page.into_iter().peekable();
            while page.peek().is_some() {
                let mut updates = Vec::new();
                let mut acknowledgements = Vec::new();
                // Device removal clears that peer's acknowledgements. Keep it
                // separate so batching retains the snapshot's operation order.
                while let Some(update) = page.next() {
                    if let Some(author) = &update.author {
                        peer = Some(author.clone());
                    }
                    let device = update.document.starts_with("device:");
                    acknowledgements.push((peer.clone(), update.document.clone()));
                    updates.push(update);
                    if device
                        || page
                            .peek()
                            .is_some_and(|next| next.document.starts_with("device:"))
                    {
                        break;
                    }
                }
                let (imported, versions) =
                    self.import_page_on(&mut transaction, updates, true).await?;
                changed |= imported;
                let acknowledgements = acknowledgements
                    .into_iter()
                    .zip(versions)
                    .filter_map(|((peer, name), version)| peer.map(|peer| (peer, name, version)))
                    .collect::<Vec<_>>();
                history::acknowledge_page_on(&mut transaction, &acknowledgements).await?;
            }
            imported += count;
            progress(imported);
            if ended {
                break;
            }
        }
        let has_author = peer.is_some();
        let mut members: Vec<String> = tokio::task::spawn_blocking(move || -> Result<_> {
            let members = if has_author {
                let mut line = String::new();
                input.read_line(&mut line)?;
                serde_json::from_str(&line).context("missing Connect snapshot membership")?
            } else {
                Vec::new()
            };
            // Finish gzip validation before committing any imported documents.
            std::io::copy(&mut input, &mut std::io::sink())?;
            Ok(members)
        })
        .await??;
        if let Some(peer) = &peer {
            members.push(peer.to_owned());
            history::register_on(&mut transaction, &members).await?;
        }
        transaction.commit().await?;
        Ok(changed)
    }

    #[cfg(test)]
    async fn clear(&self) -> Result<()> {
        Self::clear_database(&self.database).await
    }

    pub async fn clear_database(database: &Database) -> Result<()> {
        let mut writer = database.writer().await?;
        let mut transaction = writer
            .as_mut()
            .context("Library writer is closed")?
            .begin()
            .await?;
        let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type='table' AND name IN ('documents','connect_versions','connect_changes','projection','local_values','sync_cursors','history_peers','history_acknowledgements','history_pending','snapshot_revisions','connect_profile')")
            .fetch_all(&mut *transaction).await?;
        for table in tables
            .iter()
            .filter(|table| !matches!(table.as_str(), "snapshot_revisions" | "connect_profile"))
        {
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table}")))
                .execute(&mut *transaction)
                .await?;
        }
        if tables.iter().any(|table| table == "snapshot_revisions") {
            sqlx::query("UPDATE snapshot_revisions SET revision=0")
                .execute(&mut *transaction)
                .await?;
        }
        if tables.iter().any(|table| table == "connect_profile") {
            sqlx::query("UPDATE connect_profile SET profile=NULL,publisher=NULL WHERE singleton=1")
                .execute(&mut *transaction)
                .await?;
        }
        sqlx::raw_sql("UPDATE file_revision SET revision=0 WHERE id=1; UPDATE connect_profile SET enabled=0,receiving=0 WHERE singleton=1;")
            .execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
    }
}

fn snapshot_reader(file: impl Read + Send + 'static) -> Result<Box<dyn BufRead + Send>> {
    let mut input = BufReader::new(file);
    if input.fill_buf()?.starts_with(&[0x1f, 0x8b]) {
        Ok(Box::new(BufReader::new(MultiGzDecoder::new(input))))
    } else {
        Ok(Box::new(input))
    }
}

fn snapshot_start(file: impl Read + Send + 'static) -> Result<Box<dyn BufRead + Send>> {
    let mut input = snapshot_reader(file)?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    validate_version(
        line.trim()
            .parse()
            .context("invalid Connect snapshot header")?,
    )?;
    Ok(input)
}

fn read_update_page(
    mut input: Box<dyn BufRead + Send>,
) -> Result<(Box<dyn BufRead + Send>, Vec<Update>, bool)> {
    let mut page = Vec::with_capacity(CONNECT_PAGE_SIZE);
    let mut line = String::new();
    for _ in 0..CONNECT_PAGE_SIZE {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            bail!("Connect snapshot is incomplete");
        }
        if line.trim_end() == "END" {
            return Ok((input, page, true));
        }
        let Some(update) = decode_update(line.as_bytes())? else {
            continue;
        };
        let image = update.document.starts_with("playlist_artwork:");
        page.push(update);
        if image {
            break;
        }
    }
    Ok((input, page, false))
}

fn decode_update(bytes: &[u8]) -> Result<Option<Update>> {
    let update: Update = serde_json::from_slice(bytes).context("invalid Connect update")?;
    if update.document.starts_with("cache_artwork:") || update.document.starts_with("cache_lyrics:")
    {
        return Ok(None);
    }
    validate_version(update.version)?;
    if (update.version == CURRENT_FORMAT) != update.current.is_some() {
        bail!(INCOMPATIBLE_PROFILE);
    }
    Ok(Some(update))
}

fn validate_version(version: u32) -> Result<()> {
    if !matches!(version, FORMAT | CURRENT_FORMAT) {
        bail!(INCOMPATIBLE_PROFILE);
    }
    Ok(())
}

fn current_identity(name: &str) -> Option<(&str, &str)> {
    let (kind, key) = name.split_once(':')?;
    let (kind, key) = if matches!(kind, "catalog" | "analysis") {
        let (kind, key) = key.split_once(':')?;
        (name.get(..name.find(':')? + 1 + kind.len())?, key)
    } else {
        (kind, key)
    };
    crate::connect_current_kind(kind).then_some((kind, key))
}

fn current_stamp(clock: &str) -> Result<crate::RecordStamp> {
    Ok(serde_json::from_str(clock)?)
}

fn scalar_snapshot(bytes: &[u8]) -> Result<Vec<crate::StampedRecord>> {
    let document = LoroDoc::new();
    let values = Arc::new(std::sync::Mutex::new(Vec::new()));
    let received = values.clone();
    let _subscription = document.inner().subscribe_root(Arc::new(move |event| {
        for event in event.events {
            if let Some(map) = event.diff.as_map() {
                for (key, value) in &map.updated {
                    let Ok((kind, key)) = serde_json::from_str::<(String, String)>(key) else {
                        continue;
                    };
                    let payload = value.value.as_ref().and_then(|value| {
                        value
                            .as_value()
                            .and_then(|value| value.as_string())
                            .map(|value| value.as_str().to_owned())
                    });
                    received.lock().unwrap().push((
                        kind,
                        key,
                        payload,
                        crate::RecordStamp {
                            lamport: value.idlp.lamport as u64,
                            actor: value.idlp.peer,
                        },
                    ));
                }
            }
        }
    }));
    document.import(bytes)?;
    std::mem::take(&mut *values.lock().unwrap())
        .into_iter()
        .map(|(kind, key, payload, stamp)| {
            Ok(crate::StampedRecord {
                record: ConnectRecord {
                    kind,
                    key,
                    value: payload
                        .map(|value| serde_json::from_str(&value))
                        .transpose()?,
                },
                stamp,
            })
        })
        .collect()
}

fn version_row(row: sqlx::sqlite::SqliteRow) -> Result<DocumentVersion> {
    let bytes: Vec<u8> = row.get(1);
    Ok(DocumentVersion {
        name: row.get(0),
        version: if row.get::<bool, _>(3) {
            current_stamp(std::str::from_utf8(&bytes)?)?.encode()
        } else {
            bytes
        },
        revision: row.get(2),
    })
}

async fn current_record(
    connection: &mut SqliteConnection,
    kind: &str,
    key: &str,
) -> Result<ConnectRecord> {
    let deleted: bool =
        sqlx::query_scalar("SELECT deleted FROM connect_versions WHERE kind=?1 AND object_key=?2")
            .bind(kind)
            .bind(key)
            .fetch_optional(&mut *connection)
            .await?
            .unwrap_or(false);
    if deleted {
        return Ok(ConnectRecord {
            kind: kind.into(),
            key: key.into(),
            value: None,
        });
    }
    Ok(Database::connect_record_on(connection, kind, key).await?)
}

async fn save_current_version(
    connection: &mut SqliteConnection,
    record: &ConnectRecord,
    stamp: crate::RecordStamp,
) -> Result<()> {
    let revision = next_revision(connection).await?;
    sqlx::query("UPDATE connect_profile SET clock=max(clock,?1) WHERE singleton=1")
        .bind(stamp.lamport as i64)
        .execute(&mut *connection)
        .await?;
    sqlx::query("INSERT INTO connect_versions(name,kind,object_key,version,revision,deleted) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(name) DO UPDATE SET version=excluded.version,revision=excluded.revision,deleted=excluded.deleted")
        .bind(document_name(record)?).bind(&record.kind).bind(&record.key).bind(serde_json::to_string(&stamp)?).bind(revision).bind(record.value.is_none()).execute(connection).await?;
    Ok(())
}

fn catalog_source(record: &ConnectRecord) -> Result<Option<String>> {
    if !record.kind.starts_with("catalog:") {
        return Ok(None);
    }
    let key: Vec<serde_json::Value> = serde_json::from_str(&record.key)?;
    Ok(Some(
        key.first()
            .and_then(serde_json::Value::as_str)
            .context("missing catalog source")?
            .to_owned(),
    ))
}

fn document_name(record: &ConnectRecord) -> Result<String> {
    if crate::connect_current_kind(&record.kind) {
        return Ok(format!("{}:{}", record.kind, record.key));
    }
    if matches!(record.kind.as_str(), "playlist_order" | "smart_order") {
        return Ok(record.kind.clone());
    }
    if matches!(record.kind.as_str(), "entry" | "native_entry") {
        let (source, playlist, _): (Option<String>, String, String) =
            serde_json::from_str(&record.key)?;
        return Ok(format!(
            "{}:{}",
            if record.kind == "entry" {
                "playlist"
            } else {
                "native_playlist"
            },
            serde_json::to_string(&(source, playlist))?
        ));
    }
    Ok(format!("{}:{}", record.kind, record.key))
}

impl ProfileStore {
    async fn remove_catalog_sources<'a>(
        &self,
        connection: &mut SqliteConnection,
        sources: impl Iterator<Item = &'a str>,
    ) -> Result<()> {
        for source in sources {
            sqlx::query("DELETE FROM connect_versions WHERE kind GLOB 'catalog:*' AND kind<>'catalog:source' AND json_extract(object_key,'$[0]')=?1")
                .bind(source)
                .execute(&mut *connection)
                .await?;
        }
        Ok(())
    }

    async fn load_document(
        &self,
        connection: &mut SqliteConnection,
        name: &str,
    ) -> Result<LoroDoc> {
        let document = LoroDoc::new();
        let snapshot: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT snapshot FROM documents WHERE name=?1")
                .bind(name)
                .fetch_optional(connection)
                .await?;
        if let Some(snapshot) = snapshot {
            document.import(&expose(name, &snapshot, &self.identity)?)?;
        }
        document.set_peer_id(self.peer_id)?;
        Ok(document)
    }
}

async fn save_snapshots_on(
    connection: &mut SqliteConnection,
    identity: Option<&age::x25519::Identity>,
    snapshots: &[(String, Vec<u8>, Vec<u8>, bool)],
) -> Result<()> {
    for page in snapshots.chunks(CONNECT_PAGE_SIZE) {
        let count = page.len() as i64;
        let end: i64 = sqlx::query_scalar(
            "UPDATE file_revision SET revision=revision+?1 WHERE id=1 RETURNING revision",
        )
        .bind(count)
        .fetch_one(&mut *connection)
        .await?;
        let encrypted = page
            .iter()
            .map(|(name, snapshot, version, _)| {
                Ok((
                    name,
                    match identity {
                        Some(key) => protect(name, snapshot, key)?.into_owned(),
                        None => snapshot.clone(),
                    },
                    version,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut insert =
            QueryBuilder::new("INSERT INTO documents(name,snapshot,revision,version) ");
        insert.push_values(
            encrypted.into_iter().enumerate(),
            |mut row, (offset, (name, snapshot, version))| {
                row.push_bind(name)
                    .push_bind(snapshot)
                    .push_bind(end - count + offset as i64 + 1)
                    .push_bind(version);
            },
        );
        insert.push(" ON CONFLICT(name) DO UPDATE SET snapshot=excluded.snapshot,revision=excluded.revision,version=excluded.version");
        insert.build().execute(&mut *connection).await?;
        let authored = page
            .iter()
            .filter(|(name, _, _, _)| history::retains_history(name))
            .collect::<Vec<_>>();
        if !authored.is_empty() {
            let mut pending = QueryBuilder::new("INSERT OR IGNORE INTO history_pending(name) ");
            pending.push_values(authored, |mut row, (name, _, _, _)| {
                row.push_bind(name);
            });
            pending.build().execute(&mut *connection).await?;
        }
        let removed = page
            .iter()
            .filter(|(_, _, _, removed)| *removed)
            .map(|(name, _, _, _)| name.strip_prefix("device:").unwrap())
            .collect::<Vec<_>>();
        history::remove_peers_on(connection, &removed).await?;
    }
    Ok(())
}
/// Called by the canonical writer before committing a local edit.
pub(crate) async fn write_local_on(
    connection: &mut SqliteConnection,
    records: &[ConnectRecord],
) -> crate::LibraryResult<()> {
    let actor: Option<String> = sqlx::query_scalar("SELECT actor FROM connect_profile WHERE singleton=1 AND profile IS NOT NULL AND enabled=1 AND receiving=0")
        .fetch_optional(&mut *connection).await?;
    if let Some(actor) = actor {
        write_records_on(connection, actor.parse().unwrap_or_default(), None, records)
            .await
            .map_err(|error| crate::LibraryError::InvalidStore(error.to_string()))?;
    }
    Ok(())
}

fn order_record(record: &ConnectRecord) -> ConnectRecord {
    ConnectRecord {
        kind: format!("{}_order", record.kind),
        key: record.key.clone(),
        value: record
            .value
            .as_ref()
            .map(|value| serde_json::json!({"position":value["position"]})),
    }
}

async fn write_records_on(
    connection: &mut SqliteConnection,
    actor: u64,
    identity: Option<&age::x25519::Identity>,
    changes: &[ConnectRecord],
) -> Result<usize> {
    let mut grouped: BTreeMap<String, Vec<ConnectRecord>> = BTreeMap::new();
    let mut current = 0;
    for record in changes {
        if crate::connect_current_kind(&record.kind) {
            crate::connect::mark_current_on(
                connection,
                &record.kind,
                &record.key,
                record.value.is_none(),
            )
            .await?;
            current += 1;
        } else {
            grouped
                .entry(document_name(record)?)
                .or_default()
                .push(record.clone());
        }
        if matches!(record.kind.as_str(), "playlist" | "smart") {
            let order = order_record(record);
            grouped
                .entry(document_name(&order)?)
                .or_default()
                .push(order);
        }
    }
    let mut snapshots = Vec::new();
    for (name, edits) in grouped {
        let snapshot: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT snapshot FROM documents WHERE name=?1")
                .bind(&name)
                .fetch_optional(&mut *connection)
                .await?;
        let document = LoroDoc::new();
        if let Some(snapshot) = snapshot {
            let snapshot = match identity {
                Some(key) => expose(&name, &snapshot, key)?.into_owned(),
                None => snapshot,
            };
            document.import(&snapshot)?;
        }
        document.set_peer_id(actor)?;
        let before = document.oplog_vv();
        for record in edits {
            edit(&document, &record)?;
        }
        document.commit();
        let version = document.oplog_vv();
        if before == version {
            continue;
        }
        let removed = name.starts_with("device:") && document.get_map("records").is_empty();
        snapshots.push((
            name,
            document.export(ExportMode::Snapshot)?,
            version.encode(),
            removed,
        ));
    }
    save_snapshots_on(connection, identity, &snapshots).await?;
    Ok(current + snapshots.len())
}

async fn next_revision(connection: &mut SqliteConnection) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "UPDATE file_revision SET revision=revision+1 WHERE id=1 RETURNING revision",
    )
    .fetch_one(connection)
    .await?)
}

async fn index_documents(connection: &mut SqliteConnection) -> Result<()> {
    sqlx::raw_sql("DROP TRIGGER IF EXISTS file_revision_insert; DROP TRIGGER IF EXISTS file_revision_update;
        DROP TRIGGER IF EXISTS documents_compressed_changed; DROP TRIGGER IF EXISTS snapshot_cache_insert;
        DROP TRIGGER IF EXISTS snapshot_cache_update; DROP TRIGGER IF EXISTS snapshot_cache_delete;
        DROP TRIGGER IF EXISTS snapshot_authored_insert; DROP TRIGGER IF EXISTS snapshot_authored_update; DROP TRIGGER IF EXISTS snapshot_authored_delete;
        DROP TRIGGER IF EXISTS snapshot_revision_insert; DROP TRIGGER IF EXISTS snapshot_revision_update; DROP TRIGGER IF EXISTS snapshot_revision_delete;
        DROP TABLE IF EXISTS outgoing; CREATE INDEX IF NOT EXISTS documents_revision ON documents(revision,name);")
        .execute(&mut *connection).await?;
    Ok(())
}

async fn migrate_pending_on(
    connection: &mut SqliteConnection,
    actor: u64,
    identity: &age::x25519::Identity,
) -> Result<()> {
    let mut transaction = connection.begin().await?;
    let old_capture: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='connect_capture')",
    )
    .fetch_one(&mut *transaction)
    .await?;
    if old_capture {
        sqlx::query("UPDATE connect_profile SET enabled=coalesce((SELECT enabled FROM connect_capture WHERE singleton=1),enabled)").execute(&mut *transaction).await?;
    }
    let native_kinds = serde_json::to_string(history::OBSERVATION_KINDS)?;
    let mut cursor = String::new();
    loop {
        let mut page: Vec<(String, Vec<u8>, i64, Vec<u8>)> = sqlx::query_as("SELECT name,snapshot,revision,version FROM documents WHERE name>?1 AND(revision=0 OR length(version)=0 OR substr(name,1,instr(name,':')-1) IN(SELECT value FROM json_each(?2))) ORDER BY name LIMIT ?3")
            .bind(&cursor).bind(&native_kinds).bind(CONNECT_PAGE_SIZE as i64).fetch_all(&mut *transaction).await?;
        let Some(last) = page.last() else { break };
        cursor = last.0.clone();
        for (name, bytes, revision, version) in &mut page {
            if *revision == 0 || version.is_empty() {
                if version.is_empty() {
                    *version =
                        LoroDoc::decode_import_blob_meta(&expose(name, bytes, identity)?, true)?
                            .partial_end_vv
                            .encode();
                }
                if *revision == 0 {
                    *revision = next_revision(&mut transaction).await?;
                }
                sqlx::query("UPDATE documents SET revision=?2,version=?3 WHERE name=?1")
                    .bind(name.as_str())
                    .bind(*revision)
                    .bind(&*version)
                    .execute(&mut *transaction)
                    .await?;
            }
            if history::retains_history(name) {
                continue;
            }
            if let Some((kind, key)) = current_identity(name) {
                let previous: Option<String> =
                    sqlx::query_scalar("SELECT version FROM connect_versions WHERE name=?1")
                        .bind(name.as_str())
                        .fetch_optional(&mut *transaction)
                        .await?;
                let scalar = matches!(kind, "state" | "smart" | "reference")
                    .then(|| scalar_snapshot(&expose(name, bytes, identity)?))
                    .transpose()?
                    .and_then(|values| {
                        values
                            .into_iter()
                            .find(|cell| cell.record.kind == kind && cell.record.key == key)
                    });
                let clock = VersionVector::decode(version)?;
                let baseline = crate::RecordStamp {
                    lamport: clock
                        .iter()
                        .map(|(_, count)| *count as u64)
                        .sum::<u64>()
                        .max(1),
                    actor: clock.iter().map(|(actor, _)| *actor).max().unwrap_or(actor),
                };
                let stamp = scalar.as_ref().map_or(baseline, |cell| cell.stamp);
                if previous
                    .map(|value| current_stamp(&value))
                    .transpose()?
                    .is_none_or(|previous| stamp > previous)
                {
                    let record = current_record(&mut transaction, kind, key).await?;
                    save_current_version(&mut transaction, &record, stamp).await?;
                }
            } else if let Some(uri) = name.strip_prefix("track:") {
                let needed: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT EXISTS(SELECT 1 FROM tracks track WHERE media_uri=?1 AND NOT EXISTS(SELECT 1 FROM sources source WHERE source.source_key=track.source_key AND source.kind IS NOT NULL) AND(track_key IN({}) OR EXISTS(SELECT 1 FROM user_media_state state WHERE state.media_uri=track.media_uri)))",crate::connect::NEEDED_TRACK_KEYS)))
                    .bind(uri).fetch_one(&mut *transaction).await?;
                if needed {
                    let record = current_record(&mut transaction, "reference", uri).await?;
                    let version = VersionVector::decode(version)?;
                    save_current_version(
                        &mut transaction,
                        &record,
                        crate::RecordStamp {
                            lamport: version
                                .iter()
                                .map(|(_, count)| *count as u64)
                                .sum::<u64>()
                                .max(1),
                            actor: version
                                .iter()
                                .map(|(actor, _)| *actor)
                                .max()
                                .unwrap_or(actor),
                        },
                    )
                    .await?;
                }
            }
        }
        page.retain(|(name, _, _, _)| !history::retains_history(name));
        if !page.is_empty() {
            for table in ["history_acknowledgements", "history_pending", "documents"] {
                let mut delete = QueryBuilder::new(format!("DELETE FROM {table} WHERE name IN("));
                let mut names = delete.separated(",");
                for (name, _, _, _) in &page {
                    names.push_bind(name);
                }
                names.push_unseparated(")");
                delete.build().execute(&mut *transaction).await?;
            }
        }
    }
    for table in ["projection", "legacy_connect_changes", "connect_changes"] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        )
        .bind(table)
        .fetch_one(&mut *transaction)
        .await?;
        if !exists {
            continue;
        }
        loop {
            let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT kind,object_key,payload FROM {table} ORDER BY {PROJECTION_ORDER} LIMIT ?1"
            )))
            .bind(CONNECT_PAGE_SIZE as i64)
            .fetch_all(&mut *transaction)
            .await?;
            if rows.is_empty() {
                break;
            }
            let mut edits = Vec::new();
            for row in rows {
                let mut kind: String = row.get(0);
                let key: String = row.get(1);
                let payload: Option<String> = row.get(2);
                let payload = if table == "projection" {
                    expose_payload(&kind, payload, identity)?
                } else {
                    payload
                };
                if kind == "track" {
                    kind = "reference".into();
                }
                let mut record = ConnectRecord {
                    kind,
                    key,
                    value: payload
                        .map(|value| serde_json::from_str(&value))
                        .transpose()?,
                };
                if table == "projection" {
                    if !crate::connect_current_kind(&record.kind) {
                        let name = document_name(&record)?;
                        let snapshot: Option<Vec<u8>> =
                            sqlx::query_scalar("SELECT snapshot FROM documents WHERE name=?1")
                                .bind(&name)
                                .fetch_optional(&mut *transaction)
                                .await?;
                        if let Some(snapshot) = snapshot {
                            let document =
                                LoroDoc::from_snapshot(&expose(&name, &snapshot, identity)?)?;
                            record.value = records(&document)?
                                .remove(&(record.kind.clone(), record.key.clone()));
                        }
                    }
                    crate::connect::apply_records_on(
                        &mut transaction,
                        std::slice::from_ref(&record),
                    )
                    .await?;
                    if crate::connect_current_kind(&record.kind) {
                        let version: Option<String> = sqlx::query_scalar(
                            "SELECT version FROM connect_versions WHERE name=?1",
                        )
                        .bind(document_name(&record)?)
                        .fetch_optional(&mut *transaction)
                        .await?;
                        let stamp = if let Some(version) = version {
                            current_stamp(&version)?
                        } else {
                            let clock: i64 = sqlx::query_scalar(
                                "SELECT clock FROM connect_profile WHERE singleton=1",
                            )
                            .fetch_one(&mut *transaction)
                            .await?;
                            crate::RecordStamp {
                                lamport: clock as u64 + 1,
                                actor,
                            }
                        };
                        save_current_version(&mut transaction, &record, stamp).await?;
                    }
                } else {
                    crate::connect::apply_records_on(
                        &mut transaction,
                        std::slice::from_ref(&record),
                    )
                    .await?;
                    if crate::connect_current_kind(&record.kind) {
                        let clock: i64 = sqlx::query_scalar(
                            "SELECT clock FROM connect_profile WHERE singleton=1",
                        )
                        .fetch_one(&mut *transaction)
                        .await?;
                        save_current_version(
                            &mut transaction,
                            &record,
                            crate::RecordStamp {
                                lamport: clock as u64 + 1,
                                actor,
                            },
                        )
                        .await?;
                    } else {
                        edits.push(record.clone());
                    }
                }
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "DELETE FROM {table} WHERE kind=?1 AND object_key=?2"
                )))
                .bind(&record.kind)
                .bind(&record.key)
                .execute(&mut *transaction)
                .await?;
                if record.kind == "reference" {
                    sqlx::query(sqlx::AssertSqlSafe(format!(
                        "DELETE FROM {table} WHERE kind='track' AND object_key=?1"
                    )))
                    .bind(&record.key)
                    .execute(&mut *transaction)
                    .await?;
                }
            }
            write_records_on(&mut transaction, actor, Some(identity), &edits).await?;
        }
    }
    sqlx::raw_sql("DROP TABLE IF EXISTS connect_changes; DROP TABLE IF EXISTS legacy_connect_changes; DROP TABLE IF EXISTS projection; DROP TABLE IF EXISTS connect_capture; DROP TABLE IF EXISTS snapshot_revisions;")
        .execute(&mut *transaction).await?;
    transaction.commit().await?;
    Ok(())
}

fn edit(document: &LoroDoc, record: &ConnectRecord) -> Result<()> {
    let map = document.get_map("records");
    let map_key = serde_json::to_string(&(&record.kind, &record.key))?;
    let mut record = record.clone();
    if let Some(value) = &mut record.value
        && let Some(previous) = map.get(&map_key)
        && let Some(previous) = previous.get_deep_value().as_string()
    {
        let mut previous = serde_json::from_str::<serde_json::Value>(previous)?;
        if record.kind == "entry" {
            let metadata = serde_json::to_value(crate::TrackMetadata::default())?;
            if let Some(previous) = previous.as_object_mut() {
                previous.retain(|field, _| metadata.get(field).is_none());
            }
        }
        preserve_unknown_fields(value, &previous);
    }
    if matches!(
        record.kind.as_str(),
        "playlist" | "native_playlist" | "device"
    ) {
        document
            .get_map("lifecycle")
            .insert("deleted", record.value.is_none())?;
        if record.value.is_none() {
            let keys = map
                .get_deep_value()
                .to_json_value()
                .as_object()
                .context("invalid Connect records")?
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            for key in keys {
                map.delete(&key)?;
            }
            let order = document.get_movable_list("occurrences");
            if !order.is_empty() {
                order.delete(0, order.len())?;
            }
            return Ok(());
        }
    }
    if matches!(
        record.kind.as_str(),
        "entry" | "native_entry" | "playlist_order" | "smart_order"
    ) {
        let list = document.get_movable_list("occurrences");
        let values = list.get_deep_value().to_json_value();
        let values = values.as_array().context("invalid playlist order")?;
        let previous = values.iter().position(|v| v.as_str() == Some(&record.key));
        match &record.value {
            Some(value) => {
                let mut value = value.clone();
                let position = value["position"].as_u64().unwrap_or(list.len() as u64) as usize;
                value
                    .as_object_mut()
                    .context("invalid playlist entry")?
                    .remove("position");
                map.insert(&map_key, serde_json::to_string(&value)?)?;
                if let Some(previous) = previous {
                    let position = position.min(list.len().saturating_sub(1));
                    if previous != position {
                        list.mov(previous, position)?;
                    }
                } else {
                    list.insert(position.min(list.len()), record.key.clone())?;
                }
            }
            None => {
                if let Some(previous) = previous {
                    list.delete(previous, 1)?;
                }
                map.delete(&map_key)?;
            }
        }
    } else if let Some(value) = &record.value {
        map.insert(&map_key, serde_json::to_string(value)?)?;
    } else {
        map.delete(&map_key)?;
    }
    Ok(())
}

// Omitted fields are outside this client's model. Known empty values are sent
// explicitly as null, empty strings or empty arrays; record deletions use None.
fn preserve_unknown_fields(current: &mut serde_json::Value, previous: &serde_json::Value) {
    if let (Some(current), Some(previous)) = (current.as_object_mut(), previous.as_object()) {
        for (key, value) in previous {
            match current.get_mut(key) {
                Some(current) => preserve_unknown_fields(current, value),
                None => {
                    current.insert(key.clone(), value.clone());
                }
            }
        }
    }
}

fn records(document: &LoroDoc) -> Result<BTreeMap<(String, String), serde_json::Value>> {
    if document
        .get_map("lifecycle")
        .get_deep_value()
        .to_json_value()["deleted"]
        == true
    {
        return Ok(BTreeMap::new());
    }
    let value = document.get_map("records").get_deep_value().to_json_value();
    let mut records = BTreeMap::new();
    let order = document
        .get_movable_list("occurrences")
        .get_deep_value()
        .to_json_value();
    let positions = order
        .as_array()
        .context("invalid playlist order")?
        .iter()
        .enumerate()
        .filter_map(|(index, value)| value.as_str().map(|value| (value, index)))
        .collect::<BTreeMap<_, _>>();
    for (key, value) in value.as_object().context("invalid Connect records")? {
        let (kind, key): (String, String) = serde_json::from_str(key)?;
        let mut value: serde_json::Value =
            serde_json::from_str(value.as_str().context("invalid Connect record")?)?;
        if matches!(
            kind.as_str(),
            "entry" | "native_entry" | "playlist_order" | "smart_order"
        ) {
            let Some(position) = positions.get(key.as_str()) else {
                continue;
            };
            value["position"] = serde_json::json!(position);
        }
        records.insert((kind, key), value);
    }
    Ok(records)
}

impl ProfileStore {
    async fn import_page_on(
        &self,
        connection: &mut SqliteConnection,
        updates: Vec<Update>,
        snapshot_input: bool,
    ) -> Result<(bool, Vec<VersionVector>)> {
        let mut versions = vec![VersionVector::default(); updates.len()];
        let mut changed = false;
        let mut authored = Vec::new();
        let mut positions = Vec::new();
        let mut accepted = Vec::new();
        for (position, mut update) in updates.into_iter().enumerate() {
            if update.current.is_none() && current_identity(&update.document).is_some() {
                let document = LoroDoc::from_snapshot(&update.bytes)?;
                let (kind, key) = current_identity(&update.document).unwrap();
                let scalar = matches!(kind, "state" | "smart" | "reference")
                    .then(|| scalar_snapshot(&update.bytes))
                    .transpose()?
                    .and_then(|values| {
                        values
                            .into_iter()
                            .find(|cell| cell.record.kind == kind && cell.record.key == key)
                    });
                update.current = Some(ConnectRecord {
                    kind: kind.into(),
                    key: key.into(),
                    value: records(&document)?.remove(&(kind.into(), key.into())),
                });
                let clock = document.oplog_vv();
                let baseline = crate::RecordStamp {
                    lamport: clock.iter().map(|(_, n)| *n as u64).sum(),
                    actor: clock
                        .iter()
                        .map(|(peer, _)| *peer)
                        .max()
                        .unwrap_or_default(),
                };
                if let Some(scalar) = scalar {
                    update.current = Some(scalar.record);
                    update.bytes = scalar.stamp.encode();
                } else {
                    update.bytes = baseline.encode();
                }
                update.version = CURRENT_FORMAT;
            }
            let Some(record) = update.current else {
                positions.push(position);
                authored.push(update);
                continue;
            };
            let name = document_name(&record)?;
            if record.kind != "catalog:source"
                && let Some(source) = catalog_source(&record)?
            {
                let removed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM connect_versions WHERE kind='catalog:source' AND json_extract(object_key,'$[0]')=?1 AND deleted=1)")
                    .bind(source).fetch_one(&mut *connection).await?;
                if removed {
                    continue;
                }
            }
            if name != update.document {
                bail!("Connect document contains another object's records");
            }
            let incoming = crate::RecordStamp::decode(&update.bytes)?;
            let previous: Option<String> =
                sqlx::query_scalar("SELECT version FROM connect_versions WHERE name=?1")
                    .bind(name)
                    .fetch_optional(&mut *connection)
                    .await?;
            let previous = previous
                .map(|value| current_stamp(&value))
                .transpose()?
                .unwrap_or_default();
            // A continued listen can gain time on a device with an older scalar clock.
            if record.kind == "listen"
                && let Some(value) = &record.value
            {
                let before = current_record(connection, &record.kind, &record.key).await?;
                if before.value.is_some() {
                    crate::connect::apply_records_on(connection, std::slice::from_ref(&record))
                        .await?;
                    let joined = current_record(connection, &record.kind, &record.key).await?;
                    let joined_changed = joined != before;
                    let merged = joined.value.as_ref().unwrap();
                    let same_facts = merged["listened_millis"] == value["listened_millis"]
                        && merged["skipped"].as_i64()
                            == value["skipped"]
                                .as_i64()
                                .or_else(|| value["skipped"].as_bool().map(i64::from));
                    let stamp = if (joined_changed && previous >= incoming)
                        || (incoming > previous && !same_facts)
                    {
                        previous.max(incoming).next(self.peer_id)
                    } else {
                        previous.max(incoming)
                    };
                    if stamp > previous {
                        save_current_version(connection, &joined, stamp).await?;
                    }
                    changed |= joined_changed;
                    continue;
                }
            }
            if previous >= incoming {
                continue;
            }
            save_current_version(connection, &record, incoming).await?;
            accepted.push(record);
        }
        let smart_changed = accepted.iter().any(|record| record.kind == "smart");
        changed |= crate::connect::apply_records_on(connection, &accepted).await?;
        for (position, result) in positions.into_iter().zip(
            self.import_authored_page_on(connection, authored, snapshot_input)
                .await?,
        ) {
            changed |= result.0;
            versions[position] = result.1;
        }
        if smart_changed {
            let document = self.load_document(connection, "smart_order").await?;
            let order = records(&document)?
                .into_iter()
                .map(|((kind, key), value)| ConnectRecord {
                    kind,
                    key,
                    value: Some(value),
                })
                .collect::<Vec<_>>();
            changed |= crate::connect::apply_records_on(connection, &order).await?;
        }
        Ok((changed, versions))
    }

    async fn import_authored_page_on(
        &self,
        connection: &mut SqliteConnection,
        updates: Vec<Update>,
        snapshot_input: bool,
    ) -> Result<Vec<(bool, VersionVector)>> {
        if updates.is_empty() {
            return Ok(Vec::new());
        }
        let mut query =
            QueryBuilder::new("SELECT name,version,snapshot FROM documents WHERE name IN (");
        let mut names = query.separated(",");
        for update in &updates {
            names.push_bind(&update.document);
        }
        names.push_unseparated(")");
        let mut saved: BTreeMap<String, (Vec<u8>, Vec<u8>)> = query
            .build()
            .fetch_all(&mut *connection)
            .await?
            .into_iter()
            .map(|row| (row.get(0), (row.get(1), row.get(2))))
            .collect();
        let identity = Arc::clone(&self.identity);
        let peer_id = self.peer_id;
        let (results, changes, snapshots, removed_sources) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let mut results = Vec::with_capacity(updates.len());
                let mut changes = BTreeMap::new();
                let mut snapshots = Vec::new();
                let mut deleted_sources = BTreeSet::new();
                for update in updates {
                    let name = update.document;
                    let previous = saved.get(&name);

                    if name.starts_with("catalog:") {
                        bail!("Connect catalog update has no current value");
                    }
                    if let Some((version, snapshot)) = previous
                        && !sensitive(&name)
                        && *snapshot == update.bytes
                    {
                        results.push((false, VersionVector::decode(version)?));
                        continue;
                    }
                    let (document, before, incoming, reuse_snapshot) =
                        if snapshot_input && previous.is_none() {
                            let document = LoroDoc::from_snapshot(&update.bytes)?;
                            let incoming = document.oplog_vv();
                            if incoming.is_empty() {
                                results.push((false, incoming));
                                continue;
                            }
                            document.set_peer_id(peer_id)?;
                            let reuse_snapshot = !document.is_shallow();
                            (document, BTreeMap::new(), incoming, reuse_snapshot)
                        } else {
                            let metadata = LoroDoc::decode_import_blob_meta(&update.bytes, true)?;
                            let reuse_snapshot = previous.is_none()
                                && metadata.mode == loro::EncodedBlobMode::Snapshot;
                            let incoming = metadata.partial_end_vv;
                            if let Some((version, _)) = previous
                                && VersionVector::decode(version)?.includes_vv(&incoming)
                            {
                                results.push((false, incoming));
                                continue;
                            }
                            let document = LoroDoc::new();
                            if let Some((_, snapshot)) = previous {
                                document.import(&expose(&name, snapshot, &identity)?)?;
                            }
                            document.set_peer_id(peer_id)?;
                            let before = records(&document)?;
                            let version = document.oplog_vv();
                            document.import(&update.bytes)?;
                            if version == document.oplog_vv() {
                                results.push((false, incoming));
                                continue;
                            }
                            (document, before, incoming, reuse_snapshot)
                        };
                    let after = records(&document)?;
                    if let Some(source) = name.strip_prefix("source:")
                        && !after.contains_key(&("source".into(), source.into()))
                    {
                        deleted_sources.insert(source.to_owned());
                    }
                    for key in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
                        if before.get(key) == after.get(key) {
                            continue;
                        }
                        let record = ConnectRecord {
                            kind: key.0.clone(),
                            key: key.1.clone(),
                            value: after.get(key).cloned(),
                        };
                        if document_name(&record)? != name {
                            bail!("Connect document contains another object's records");
                        }
                        changes.insert(key.clone(), record);
                    }
                    let snapshot = if reuse_snapshot {
                        update.bytes
                    } else {
                        document.export(ExportMode::Snapshot)?
                    };
                    let version = document.oplog_vv().encode();
                    saved.insert(
                        name.clone(),
                        (
                            version.clone(),
                            protect(&name, &snapshot, &identity)?.into_owned(),
                        ),
                    );
                    let removed_device = name.starts_with("device:") && after.is_empty();
                    snapshots.push((name, snapshot, version, removed_device));
                    results.push((before != after, incoming));
                }
                Ok((
                    results,
                    changes.into_values().collect::<Vec<_>>(),
                    snapshots,
                    deleted_sources,
                ))
            })
            .await??;
        crate::connect::apply_records_on(connection, &changes).await?;
        save_snapshots_on(connection, Some(&self.identity), &snapshots).await?;
        self.remove_catalog_sources(connection, removed_sources.iter().map(String::as_str))
            .await?;
        Ok(results)
    }
}

#[cfg(test)]
#[path = "../tests/support/catalog.rs"]
mod catalog_fixture;

#[cfg(test)]
mod tests {
    use super::*;

    use flate2::{Compression, write::GzEncoder};
    use serde_json::json;
    use std::io::Write;

    const KEY: &str = "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33";

    fn entry(id: &str, position: usize) -> ConnectRecord {
        ConnectRecord {
            kind: "entry".into(),
            key: json!([null, "mix", id]).to_string(),
            value: Some(
                json!({"playlist":"[null,\"mix\"]","object_id":id,"media_uri":"https://example.org/same.flac","position":position,"snapshot_at":1}),
            ),
        }
    }

    async fn missing_updates(from: &ProfileStore, to: &ProfileStore) -> Vec<Vec<u8>> {
        let mut updates = Vec::new();
        for stream in [SyncStream::Authored, SyncStream::NativeCache] {
            let mut cursor = 0;
            loop {
                let page = from
                    .changes_in_stream(cursor, CONNECT_PAGE_SIZE, stream)
                    .await
                    .unwrap();
                let Some(last) = page.last() else { break };
                cursor = last.revision;
                let names: Vec<_> = page.into_iter().map(|version| version.name).collect();
                updates.extend(
                    from.updates(&to.versions(&names).await.unwrap())
                        .await
                        .unwrap(),
                );
            }
        }
        updates
    }

    async fn deliver(from: &ProfileStore, to: &ProfileStore) {
        let updates = missing_updates(from, to).await;
        to.import_updates(&updates).await.unwrap();
        assert!(!to.import_updates(&updates).await.unwrap());
    }

    #[tokio::test]
    async fn synced_device_names_remain_available_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let a = ProfileStore::open(
            crate::Database::open(&directory.path().join("a.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let path = directory.path().join("b.sqlite");
        let b = ProfileStore::open(
            crate::Database::open(&path).await.unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        a.write_records(&[ConnectRecord {
            kind: "device".into(),
            key: "peer-identity".into(),
            value: Some(json!("Work Laptop")),
        }])
        .await
        .unwrap();
        deliver(&a, &b).await;
        drop(b);
        let b = ProfileStore::open(
            crate::Database::open(&path).await.unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            b.device_name("peer-identity").await.unwrap().as_deref(),
            Some("Work Laptop")
        );
    }

    #[tokio::test]
    async fn version_pages_catch_up_after_restart_and_merge_offline_edits() {
        let directory = tempfile::tempdir().unwrap();
        let a_path = directory.path().join("a.sqlite");
        let a = ProfileStore::open(
            crate::Database::open(&a_path).await.unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let b = ProfileStore::open(
            crate::Database::open(&directory.path().join("b.sqlite"))
                .await
                .unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let record = |key: &str, value: &str| ConnectRecord {
            kind: "preference".into(),
            key: key.into(),
            value: Some(json!(value)),
        };
        for key in ["a", "b", "c", "d", "e"] {
            a.write_records(&[record(key, "initial")]).await.unwrap();
        }
        let revision = a.revision().await.unwrap();
        a.write_records(&[record("a", "initial")]).await.unwrap();
        assert_eq!(a.revision().await.unwrap(), revision);
        drop(a);
        let a = ProfileStore::open(
            crate::Database::open(&a_path).await.unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(a.revision().await.unwrap(), revision);
        let mut cursor = 0;
        let mut names = Vec::new();
        loop {
            let page = a
                .changes_in_stream(cursor, 2, SyncStream::Authored)
                .await
                .unwrap();
            if page.is_empty() {
                break;
            }
            assert!(page.len() <= 2);
            assert!(page.iter().all(|version| version.revision > cursor));
            cursor = page.last().unwrap().revision;
            let page_names: Vec<_> = page.into_iter().map(|version| version.name).collect();
            let missing = b.versions(&page_names).await.unwrap();
            assert!(missing.iter().all(|version| version.revision == 0));
            let updates = a.updates(&missing).await.unwrap();
            assert!(b.import_updates(&updates).await.unwrap());
            let imported_revision = b.revision().await.unwrap();
            assert!(!b.import_updates(&updates).await.unwrap());
            assert_eq!(b.revision().await.unwrap(), imported_revision);
            assert!(
                a.updates(&b.versions(&page_names).await.unwrap())
                    .await
                    .unwrap()
                    .is_empty()
            );
            names.extend(page_names);
        }
        assert_eq!(names.len(), 5);
        assert_eq!(cursor, revision);
        a.write_records(&[record("a", "left")]).await.unwrap();
        b.write_records(&[record("a", "right")]).await.unwrap();
        let left = a.updates(&b.versions(&names).await.unwrap()).await.unwrap();
        let right = b.updates(&a.versions(&names).await.unwrap()).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(right.len(), 1);
        a.import_updates(&right).await.unwrap();
        b.import_updates(&left).await.unwrap();
        assert!(
            a.updates(&b.versions(&names).await.unwrap())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            b.updates(&a.versions(&names).await.unwrap())
                .await
                .unwrap()
                .is_empty()
        );
        let a_records = records(
            &a.load_document(&mut *a.writer().await.unwrap(), "preference:a")
                .await
                .unwrap(),
        )
        .unwrap();
        let b_records = records(
            &b.load_document(&mut *b.writer().await.unwrap(), "preference:a")
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(a_records, b_records);
        assert_eq!(
            a.changes_in_stream(cursor, 2, SyncStream::Authored)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn update_page_rolls_back_on_invalid_document() {
        let directory = tempfile::tempdir().unwrap();
        let a = ProfileStore::open(
            crate::Database::open(&directory.path().join("a.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let b = ProfileStore::open(
            crate::Database::open(&directory.path().join("b.sqlite"))
                .await
                .unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        a.write_records(&[ConnectRecord {
            kind: "preference".into(),
            key: "theme".into(),
            value: Some(json!("dark")),
        }])
        .await
        .unwrap();
        let versions = b.versions(&["preference:theme".into()]).await.unwrap();
        let mut updates = a.updates(&versions).await.unwrap();
        updates.push(b"invalid".to_vec());
        assert!(b.import_updates(&updates).await.is_err());
        assert_eq!(b.revision().await.unwrap(), 0);
        assert!(
            b.changes_in_stream(0, 10, SyncStream::Authored)
                .await
                .unwrap()
                .is_empty()
        );
        updates.pop();
        assert!(b.import_updates(&updates).await.unwrap());
    }

    #[tokio::test]
    async fn old_snapshots_gain_durable_revisions_without_rewriting_history() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("old.sqlite");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let mut old = SqliteConnection::connect_with(&options).await.unwrap();
        sqlx::raw_sql("CREATE TABLE documents(name TEXT PRIMARY KEY,snapshot BLOB NOT NULL) STRICT;
            CREATE TABLE file_revision(id INTEGER PRIMARY KEY CHECK(id=1),revision INTEGER NOT NULL) STRICT;
            INSERT INTO file_revision VALUES(1,7);
            CREATE TRIGGER file_revision_update AFTER UPDATE ON documents BEGIN UPDATE file_revision SET revision=revision+1; END;")
            .execute(&mut old).await.unwrap();
        let count = 2 * CONNECT_PAGE_SIZE + 1;
        let mut expected = BTreeMap::new();
        let mut transaction = old.begin().await.unwrap();
        for index in 0..count {
            let key = format!("theme-{index:04}");
            let document = LoroDoc::new();
            for (peer, value) in [(1, "dark"), (2, "light")] {
                document.set_peer_id(peer).unwrap();
                edit(
                    &document,
                    &ConnectRecord {
                        kind: "preference".into(),
                        key: key.clone(),
                        value: Some(json!(value)),
                    },
                )
                .unwrap();
                document.commit();
            }
            let name = format!("preference:{key}");
            let bytes = document.export(ExportMode::Snapshot).unwrap();
            sqlx::query("INSERT INTO documents VALUES(?1,?2)")
                .bind(&name)
                .bind(&bytes)
                .execute(&mut *transaction)
                .await
                .unwrap();
            expected.insert(name, (bytes, document.oplog_vv()));
        }
        sqlx::query("INSERT INTO documents VALUES('preference:zzz',X'00')")
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        old.close().await.unwrap();
        let rejected = crate::Database::open(directory.path().join("rejected.sqlite"))
            .await
            .unwrap();
        assert!(
            ProfileStore::migrate_legacy_database(&rejected, &path, "profile")
                .await
                .unwrap()
        );
        assert!(
            ProfileStore::open(rejected.clone(), 1, KEY.parse().unwrap())
                .await
                .is_err()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM documents WHERE revision<>0")
                .fetch_one(rejected.writer().await.unwrap().as_mut().unwrap())
                .await
                .unwrap(),
            0
        );
        let mut old = SqliteConnection::connect_with(&options).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM file_revision")
                .fetch_one(&mut old)
                .await
                .unwrap(),
            7
        );
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM pragma_table_info('documents') WHERE name IN ('revision','version')")
            .fetch_one(&mut old).await.unwrap(), 0);
        sqlx::query("DELETE FROM documents WHERE name='preference:zzz'")
            .execute(&mut old)
            .await
            .unwrap();
        let base: i64 = sqlx::query_scalar("SELECT revision FROM file_revision")
            .fetch_one(&mut old)
            .await
            .unwrap();
        old.close().await.unwrap();
        let canonical = directory.path().join("canonical.sqlite");
        let database = crate::Database::open(&canonical).await.unwrap();
        assert!(
            ProfileStore::migrate_legacy_database(&database, &path, "profile")
                .await
                .unwrap()
        );
        let profile = ProfileStore::open(database, 1, KEY.parse().unwrap())
            .await
            .unwrap();
        let versions = profile
            .changes_in_stream(0, count, SyncStream::Authored)
            .await
            .unwrap();
        assert_eq!(versions.len(), count);
        for (offset, version) in versions.iter().enumerate() {
            assert_eq!(version.revision, base + offset as i64 + 1);
            assert_eq!(
                VersionVector::decode(&version.version).unwrap(),
                expected[&version.name].1
            );
        }
        let saved = sqlx::query("SELECT name,snapshot FROM documents ORDER BY name")
            .fetch_all(&mut *profile.writer().await.unwrap())
            .await
            .unwrap();
        for row in saved {
            assert_eq!(
                row.get::<Vec<u8>, _>(1),
                expected[&row.get::<String, _>(0)].0
            );
        }
        profile
            .acknowledge_sync_in("peer", SyncStream::Authored, 99)
            .await
            .unwrap();
        drop(profile);
        let profile = ProfileStore::open(
            crate::Database::open(&canonical).await.unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            profile
                .changes_in_stream(0, count, SyncStream::Authored)
                .await
                .unwrap(),
            versions
        );
        assert_eq!(profile.revision().await.unwrap(), base + count as i64);
        assert_eq!(
            profile
                .sync_cursor_in("peer", SyncStream::Authored)
                .await
                .unwrap(),
            99
        );
    }

    #[tokio::test]
    async fn artwork_added_to_an_existing_profile_reaches_saved_tracks() {
        let directory = tempfile::tempdir().unwrap();
        let host = ProfileStore::open(
            Database::open(directory.path().join("host.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let guest = ProfileStore::open(
            Database::open(directory.path().join("guest.sqlite"))
                .await
                .unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        for profile in [&host, &guest] {
            profile
                .bind_profile(
                    "profile",
                    if profile.peer_id == 1 {
                        "host"
                    } else {
                        "guest"
                    },
                )
                .await
                .unwrap();
            profile
                .database
                .reconcile_source(&crate::SourceId::new("server"), "jellyfin", "Server")
                .await
                .unwrap();
            profile.database.connect_set_active(true).await.unwrap();
        }
        let source = &host.database;
        let uri = crate::source_entity_uri(&crate::SourceId::new("server"), "track", "track");
        let mut track = crate::TrackObservation {
            object_id: "track".into(),
            metadata: crate::TrackMetadata {
                title: "Track".into(),
                artist: "Artist".into(),
                duration_millis: 180_000,
                ..Default::default()
            },
            ..Default::default()
        };
        source
            .catalog("server", None)
            .await
            .unwrap()
            .apply_catalog(
                &crate::CatalogBatch {
                    tracks: vec![track.clone()],
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
        let mut reference = source.connect_track_reference(&uri).await.unwrap().unwrap();
        reference["future_field"] = json!("retained");
        host.import_updates(&[serde_json::to_vec(&Update {
            version: CURRENT_FORMAT,
            document: format!("reference:{uri}"),
            bytes: crate::RecordStamp {
                lamport: 1,
                actor: 9,
            }
            .encode(),
            author: None,
            current: Some(ConnectRecord {
                kind: "reference".into(),
                key: uri.clone(),
                value: Some(reference),
            }),
        })
        .unwrap()])
            .await
            .unwrap();
        source
            .create_playlist(
                None,
                "Saved",
                &crate::MemberSelection::Uris((vec![uri.clone()]).into()),
            )
            .await
            .unwrap();
        deliver(&host, &guest).await;
        let art = serde_json::to_vec(&[crate::ArtworkCandidate::new(
            crate::ArtworkImage::Native {
                source_id: crate::SourceId::new("server"),
                item_id: "track-cover".into(),
                tag: None,
            },
            None,
        )])
        .unwrap();
        track.artwork_binding = Some(art.clone());
        source
            .catalog("server", None)
            .await
            .unwrap()
            .apply_catalog(
                &crate::CatalogBatch {
                    tracks: vec![track],
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
        deliver(&host, &guest).await;
        let references = guest.stored_records().await.unwrap();
        assert_eq!(
            references
                .iter()
                .find(|record| record.kind == "reference" && record.key == uri)
                .unwrap()
                .value
                .as_ref()
                .unwrap()["future_field"],
            "retained"
        );
        let cancellation = crate::ReadCancellation::new();
        assert!(
            !guest
                .database
                .cached_source("server", &cancellation)
                .await
                .unwrap()
                .unwrap()
                .catalog_complete
        );
        assert_eq!(
            guest
                .database
                .track_artwork_bindings(&[uri], &cancellation)
                .await
                .unwrap(),
            vec![art]
        );
    }

    #[tokio::test]
    async fn older_models_preserve_new_fields_and_settings_without_echoing_them() {
        let directory = tempfile::tempdir().unwrap();
        let a = ProfileStore::open(
            crate::Database::open(&directory.path().join("newer.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let b = ProfileStore::open(
            crate::Database::open(&directory.path().join("older.sqlite"))
                .await
                .unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let source = |value| ConnectRecord {
            kind: "source".into(),
            key: "server".into(),
            value: Some(value),
        };
        let state = |favorite, extra| ConnectRecord {
            kind: "state".into(),
            key: "https://example.org/track.flac".into(),
            value: Some(if extra {
                json!({"media_uri":"https://example.org/track.flac","favorite":favorite,"rating":null,"new_column":"retained"})
            } else {
                json!({"media_uri":"https://example.org/track.flac","favorite":favorite,"rating":null})
            }),
        };
        let future = ConnectRecord {
            kind: "preference".into(),
            key: "new_feature".into(),
            value: Some(json!({"enabled":true})),
        };
        a.write_records(&[
            source(
                json!({"configuration":{"name":"Server","future_option":42},"credential":"secret"}),
            ),
            future.clone(),
        ])
        .await
        .unwrap();
        a.import_updates(&[serde_json::to_vec(&Update {
            version: CURRENT_FORMAT,
            document: format!("state:{}", state(false, true).key),
            bytes: crate::RecordStamp {
                lamport: 1,
                actor: 9,
            }
            .encode(),
            author: None,
            current: Some(state(false, true)),
        })
        .unwrap()])
            .await
            .unwrap();
        deliver(&a, &b).await;

        assert_eq!(
            b.write_records(&[source(
                json!({"configuration":{"name":"Server"},"credential":"secret"})
            )])
            .await
            .unwrap(),
            0
        );
        assert!(missing_updates(&b, &a).await.is_empty());
        b.write_records(&[source(
            json!({"configuration":{"name":"Renamed"},"credential":null}),
        )])
        .await
        .unwrap();
        b.bind_profile("profile", "b").await.unwrap();
        b.database.connect_set_active(true).await.unwrap();
        b.database
            .set_favorite(&crate::FavoriteTarget::Track(state(true, false).key), true)
            .await
            .unwrap();
        deliver(&b, &a).await;
        let mut connection = a.writer().await.unwrap();
        let source_values = records(
            &a.load_document(&mut connection, "source:server")
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            source_values[&("source".into(), "server".into())],
            json!({"configuration":{"name":"Renamed","future_option":42},"credential":null})
        );
        let preferences = records(
            &a.load_document(&mut connection, "preference:new_feature")
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            preferences[&("preference".into(), "new_feature".into())],
            future.value.unwrap()
        );
        let track = current_record(&mut connection, "state", "https://example.org/track.flac")
            .await
            .unwrap()
            .value
            .unwrap();
        assert_eq!(track["new_column"], "retained");
        assert_eq!(track["favorite"], true);
    }

    #[tokio::test]
    async fn incompatible_updates_leave_persisted_profile_ready_to_resume() {
        let directory = tempfile::tempdir().unwrap();
        let sender = ProfileStore::open(
            crate::Database::open(&directory.path().join("sender.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let path = directory.path().join("receiver.sqlite");
        let receiver = ProfileStore::open(
            crate::Database::open(&path).await.unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let record = |key, value| ConnectRecord {
            kind: "preference".into(),
            key: String::from(key),
            value: Some(json!(value)),
        };
        sender
            .write_records(&[record("theme", "dark")])
            .await
            .unwrap();
        deliver(&sender, &receiver).await;
        sender
            .write_records(&[record("language", "en")])
            .await
            .unwrap();
        let pending = missing_updates(&sender, &receiver).await;
        let mut incompatible: Update = serde_json::from_slice(&pending[0]).unwrap();
        incompatible.version = FORMAT + 1;
        assert_eq!(
            receiver
                .import_updates(&[serde_json::to_vec(&incompatible).unwrap()])
                .await
                .unwrap_err()
                .to_string(),
            INCOMPATIBLE_PROFILE
        );
        let snapshot = directory.path().join("incompatible.jsonl");
        std::fs::write(
            &snapshot,
            format!(
                "{FORMAT}\n{}\nEND\n",
                serde_json::to_string(&incompatible).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(
            receiver
                .replace_snapshot(std::fs::File::open(&snapshot).unwrap(), |_| {})
                .await
                .unwrap_err()
                .to_string(),
            INCOMPATIBLE_PROFILE
        );
        drop(receiver);
        let receiver = ProfileStore::open(
            crate::Database::open(&path).await.unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        deliver(&sender, &receiver).await;
        let mut connection = receiver.writer().await.unwrap();
        for (key, value) in [("theme", "dark"), ("language", "en")] {
            let values = records(
                &receiver
                    .load_document(&mut connection, &format!("preference:{key}"))
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(values[&("preference".into(), key.into())], value);
        }
    }

    #[tokio::test]
    async fn concurrent_playlist_occurrences_and_credential_edits_converge_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let a = ProfileStore::open(
            crate::Database::open(&directory.path().join("a.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let b = ProfileStore::open(
            crate::Database::open(&directory.path().join("b.sqlite"))
                .await
                .unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        a.write_records(&[entry("first", 0), entry("duplicate", 1)])
            .await
            .unwrap();
        deliver(&a, &b).await;
        a.write_records(&[
            entry("third", 1),
            ConnectRecord {
                kind: "source".into(),
                key: "server".into(),
                value: Some(json!({"configuration":{"name":"Server"},"credential":"changed"})),
            },
        ])
        .await
        .unwrap();
        b.write_records(&[entry("duplicate", 0), entry("fourth", 2)])
            .await
            .unwrap();
        deliver(&a, &b).await;
        deliver(&b, &a).await;
        let expected = {
            let mut connection = a.writer().await.unwrap();
            records(
                &a.load_document(&mut connection, "playlist:[null,\"mix\"]")
                    .await
                    .unwrap(),
            )
            .unwrap()
        };
        drop(b);
        let b = ProfileStore::open(
            crate::Database::open(&directory.path().join("b.sqlite"))
                .await
                .unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let actual = {
            let mut connection = b.writer().await.unwrap();
            records(
                &b.load_document(&mut connection, "playlist:[null,\"mix\"]")
                    .await
                    .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 4);
        let mut connection = b.writer().await.unwrap();
        let source = records(
            &b.load_document(&mut connection, "source:server")
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            source[&("source".into(), "server".into())]["credential"],
            "changed"
        );
    }

    #[tokio::test]
    async fn server_playlist_pins_resolve_after_profile_sync_and_source_refresh() {
        let directory = tempfile::tempdir().unwrap();
        let a = ProfileStore::open(
            crate::Database::open(&directory.path().join("a.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let mut b = ProfileStore::open(
            crate::Database::open(&directory.path().join("b.sqlite"))
                .await
                .unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        a.bind_profile("profile", "source").await.unwrap();
        b.bind_profile("profile", "target").await.unwrap();
        let source = a.database.clone();
        let target = b.database.clone();
        let id = crate::SourceId::new("server");
        let cancel = crate::ReadCancellation::new();
        let art = serde_json::to_vec(&[crate::ArtworkCandidate::new(
            crate::ArtworkImage::Native {
                source_id: id.clone(),
                item_id: "playlist-cover".into(),
                tag: None,
            },
            None,
        )])
        .unwrap();
        let sidebar = json!({"pins":[{"Playlist":{"source_id":"server","playlist_id":"mix"}}]});
        for revision in 0..3 {
            for (receiving, database) in [(false, &source), (true, &target)] {
                if receiving {
                    if revision == 0 {
                        source.connect_initialize_profile().await.unwrap();
                        a.write_records(&[ConnectRecord {
                            kind: "preference".into(),
                            key: "sidebar".into(),
                            value: Some(sidebar.clone()),
                        }])
                        .await
                        .unwrap();
                    }
                    while source.connect_seed_page().await.unwrap() {}

                    if revision == 0 {
                        let base = directory.path().join("base.sqlite");
                        a.export_database(&base, &KEY.parse().unwrap(), &[], &[])
                            .await
                            .unwrap();
                        b.close().await.unwrap();
                        ProfileStore::prepare_database(
                            &base,
                            &KEY.parse().unwrap(),
                            &KEY.parse().unwrap(),
                        )
                        .await
                        .unwrap();
                        ProfileStore::install_prepared_database(target.clone(), &base)
                            .await
                            .unwrap();
                        b = ProfileStore::open(target.clone(), 2, KEY.parse().unwrap())
                            .await
                            .unwrap();
                        target.connect_set_active(true).await.unwrap();
                    } else {
                        deliver(&a, &b).await;
                    }

                    let mut connection = b.writer().await.unwrap();
                    let preferences = records(
                        &b.load_document(&mut connection, "preference:sidebar")
                            .await
                            .unwrap(),
                    )
                    .unwrap();
                    assert_eq!(
                        preferences[&("preference".into(), "sidebar".into())],
                        sidebar
                    );
                    if revision == 0 {
                        let key = target
                            .playlist_key_by_identity(Some(&id), "mix", &cancel)
                            .await
                            .unwrap()
                            .expect("shared source cache resolves the pin before refresh");
                        let metadata = target.playlists(&[key], &cancel).await.unwrap();
                        assert_eq!(metadata[0].name, "Server mix");
                        assert_eq!(metadata[0].track_count, 140);
                        let entries = crate::MemberRead::new(crate::Collection::Playlist(key))
                            .rows(&target, 0, 140, &cancel)
                            .await
                            .unwrap();
                        let entries = entries
                            .into_iter()
                            .map(|row| row.into_entry().unwrap())
                            .collect::<Vec<_>>();
                        assert_eq!(entries.len(), 140);
                        assert_eq!(
                            entries[0].track.media_uri,
                            crate::source_entity_uri(&id, "track", "track-0")
                        );
                    }
                }
                database
                    .reconcile_source(&crate::SourceId::new("server"), "server", "Server")
                    .await
                    .unwrap();
                let mut catalog = database.catalog("server", None).await.unwrap();
                let stamp = catalog.start_observation().await.unwrap();
                if revision < 2 {
                    catalog
                        .apply_catalog(
                            &crate::CatalogBatch {
                                playlists: vec![crate::PlaylistObservation {
                                    object_id: "mix".into(),
                                    name: "Server mix".into(),
                                    sort_text: "server mix".into(),
                                    artwork_binding: Some(art.to_vec()),
                                    writable: Some(false),
                                    metadata_writable: Some(false),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            },
                            true,
                        )
                        .await
                        .unwrap();

                    for offset in (0..140).step_by(64) {
                        let entries = (offset..(offset + 64).min(140))
                            .map(|position| {
                                let entry = if revision == 0 {
                                    position
                                } else {
                                    139 - position
                                };
                                crate::PlaylistEntryObservation {
                                    object_id: format!("entry-{entry}"),
                                    track_id: format!("track-{entry}"),
                                }
                            })
                            .collect::<Vec<_>>();
                        catalog
                            .write_source_playlist_page(
                                "mix",
                                offset,
                                &entries,
                                offset + entries.len() == 140,
                            )
                            .await
                            .unwrap();
                    }
                }
                catalog
                    .complete_source_observation(stamp, &["playlist"], None)
                    .await
                    .unwrap();
                database
                    .publish_catalog_update("server", catalog.into_changes())
                    .await;
            }
            let key = target
                .playlist_key_by_identity(Some(&id), "mix", &cancel)
                .await
                .unwrap();
            if revision == 2 {
                assert!(
                    crate::ListRead {
                        playlist_scope: Default::default(),
                        kind: crate::ListKind::Playlists,
                        source: (None).into(),
                        sort: crate::CatalogSort::Position,
                        folder: None,
                        filter: ("").to_string(),
                        descending: false,
                        favorites_only: false,
                        downloaded_only: false,
                        activity: false
                    }
                    .playlists(&target, 0, 140, &cancel)
                    .await
                    .unwrap()
                    .is_empty()
                );
                continue;
            }
            let key = key.expect("shared pin resolves from the receiving source cache");
            let row = target
                .playlists(&[key], &cancel)
                .await
                .unwrap()
                .pop()
                .unwrap();
            assert_eq!(row.name, "Server mix");
            assert_eq!(row.track_count, 140);
            assert!(!row.writable);
            let candidates: Vec<crate::ArtworkCandidate> =
                serde_json::from_slice(row.artwork_binding.as_deref().unwrap()).unwrap();
            let native: Vec<crate::ArtworkCandidate> = serde_json::from_slice(&art).unwrap();
            assert_eq!(candidates.first(), native.first());
            let entries = (&crate::MemberRead::new(crate::Collection::Playlist(key)))
                .rows(&target, 0, 140, &cancel)
                .await
                .map(|rows| {
                    rows.into_iter()
                        .filter_map(crate::MemberRow::into_entry)
                        .collect::<Vec<_>>()
                })
                .unwrap();
            let first = if revision == 0 {
                "track-0"
            } else {
                "track-139"
            };
            assert_eq!(
                entries[0].track.media_uri,
                crate::source_entity_uri(&id, "track", first)
            );
            assert_eq!(entries.len(), 140);
            deliver(&a, &b).await;
        }
    }

    #[tokio::test]
    async fn playlist_order_and_setup_snapshot_preserve_profile_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let a = ProfileStore::open(
            crate::Database::open(&directory.path().join("a.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let mut b = ProfileStore::open(
            crate::Database::open(&directory.path().join("b.sqlite"))
                .await
                .unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let playlist = |id: &str, position| ConnectRecord {
            kind: "playlist".into(),
            key: json!([null, id]).to_string(),
            value: Some(
                json!({"object_id":id,"name":id,"normalized_name":id,"sort_text":id,"position":position}),
            ),
        };
        a.write_records(&[playlist("a", 0), playlist("b", 1), playlist("c", 2)])
            .await
            .unwrap();
        deliver(&a, &b).await;
        a.write_records(&[playlist("c", 0), playlist("a", 1), playlist("b", 2)])
            .await
            .unwrap();
        deliver(&a, &b).await;
        let db = b.database.clone();
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT object_id FROM playlists WHERE owner='rufin' ORDER BY position",
        )
        .fetch_all(&mut *b.writer().await.unwrap())
        .await
        .unwrap();
        assert_eq!(names, ["c", "a", "b"]);
        let definition = crate::SmartPlaylistDefinition::default();
        a.database
            .create_smart_playlist("First", &definition)
            .await
            .unwrap();
        let middle = a
            .database
            .create_smart_playlist("Middle", &definition)
            .await
            .unwrap();
        a.database
            .create_smart_playlist("Last", &definition)
            .await
            .unwrap();
        let mut expected: Vec<String> =
            sqlx::query_scalar("SELECT name FROM smart_playlists ORDER BY position")
                .fetch_all(&mut *a.writer().await.unwrap())
                .await
                .unwrap();
        a.bind_profile("profile", "sender").await.unwrap();
        a.database.connect_initialize_profile().await.unwrap();
        assert!(a.database.connect_seed_page().await.unwrap());
        let order = records(
            &a.load_document(&mut *a.writer().await.unwrap(), "smart_order")
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(order.len(), expected.len());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM connect_versions WHERE kind IN ('smart','reference')",
            )
            .fetch_one(&mut *a.writer().await.unwrap())
            .await
            .unwrap(),
            0
        );
        while a.database.connect_seed_page().await.unwrap() {}
        let snapshot = directory.path().join("setup.sqlite");
        a.export_database(&snapshot, &KEY.parse().unwrap(), &[], &[])
            .await
            .unwrap();
        b.close().await.unwrap();
        ProfileStore::prepare_database(&snapshot, &KEY.parse().unwrap(), &KEY.parse().unwrap())
            .await
            .unwrap();
        ProfileStore::install_prepared_database(db.clone(), &snapshot)
            .await
            .unwrap();
        b = ProfileStore::open(db.clone(), 2, KEY.parse().unwrap())
            .await
            .unwrap();
        b.bind_profile("profile", "receiver").await.unwrap();
        db.connect_set_active(true).await.unwrap();
        a.database
            .update_smart_playlist(middle, "Renamed", &definition, None)
            .await
            .unwrap();
        *expected
            .iter_mut()
            .find(|name| name.as_str() == "Middle")
            .unwrap() = "Renamed".into();
        deliver(&a, &b).await;
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT name FROM smart_playlists ORDER BY position")
                .fetch_all(&mut *b.writer().await.unwrap())
                .await
                .unwrap(),
            expected
        );
        a.database
            .create_smart_playlist("Appended", &definition)
            .await
            .unwrap();
        expected.push("Appended".into());
        deliver(&a, &b).await;
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT name FROM smart_playlists ORDER BY position")
                .fetch_all(&mut *b.writer().await.unwrap())
                .await
                .unwrap(),
            expected
        );
        a.write_records(&[
            ConnectRecord {
                kind: "preference".into(),
                key: "theme".into(),
                value: Some(json!("dark")),
            },
            ConnectRecord {
                kind: "root".into(),
                key: "music".into(),
                value: Some(json!({"source_id":"local","root_id":"music","root_label":"Music"})),
            },
        ])
        .await
        .unwrap();
        let documents = 2;
        let names = ["preference:theme".to_owned(), "root:music".to_owned()];
        let updates = a.updates(&b.versions(&names).await.unwrap()).await.unwrap();
        b.clear().await.unwrap();
        b.import_updates(&updates).await.unwrap();
        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM documents ORDER BY name")
            .fetch_all(&mut *b.writer().await.unwrap())
            .await
            .unwrap();
        assert_eq!(names, ["preference:theme", "root:music"]);
        assert_eq!(names.len(), documents);
        let roots = db.connect_roots("local").await.unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].id, "music");
        assert_eq!(roots[0].label, "Music");
    }

    #[tokio::test]
    async fn authored_and_current_changes_keep_separate_cursors_and_converge() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(
            Database::open(directory.path().join("profile.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let peer = ProfileStore::open(
            Database::open(directory.path().join("peer.sqlite"))
                .await
                .unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let source = crate::SourceId::new("server");
        for profile in [&store, &peer] {
            profile
                .bind_profile(
                    "profile",
                    if profile.peer_id == 1 {
                        "sender"
                    } else {
                        "peer"
                    },
                )
                .await
                .unwrap();
            profile
                .database
                .reconcile_source(&source, "webdav", "Files")
                .await
                .unwrap();
            profile.database.connect_set_active(true).await.unwrap();
        }
        let uri = crate::source_entity_uri(&source, "track", "one");
        let state = |favorite| ConnectRecord {
            kind: "playlist".into(),
            key: json!([null, "mix"]).to_string(),
            value: Some(
                json!({"object_id":"mix","name":if favorite {"Favorites"} else {"Mix"},"normalized_name":"mix","sort_text":"mix","position":0}),
            ),
        };
        store.write_records(&[state(false)]).await.unwrap();
        store.write_records(&[state(true)]).await.unwrap();
        let authored = store
            .changes_in_stream(0, 10, SyncStream::Authored)
            .await
            .unwrap();
        assert_eq!(authored.len(), 2);
        let revision = authored.last().unwrap().revision;
        assert!(
            authored
                .iter()
                .any(|version| version.name == "playlist_order")
        );
        assert!(
            authored
                .iter()
                .any(|version| version.name == "playlist:[null,\"mix\"]")
        );
        assert!(
            store
                .changes_in_stream(revision, 10, SyncStream::Authored)
                .await
                .unwrap()
                .is_empty()
        );
        store
            .acknowledge_sync_in("peer", SyncStream::Authored, revision)
            .await
            .unwrap();
        assert_eq!(
            store
                .sync_cursor_in("peer", SyncStream::Authored)
                .await
                .unwrap(),
            revision
        );
        assert_eq!(
            store
                .sync_cursor_in("peer", SyncStream::NativeCache)
                .await
                .unwrap(),
            0
        );

        let media_path = "https://files.example.test/music/one.flac";
        let key = json!(["server", media_path]).to_string();
        let file = crate::LocalFileWrite {
            path: media_path.into(),
            root: "https://files.example.test/music".into(),
            relative_path: "one.flac".into(),
            kind: crate::LocalFileKind::Media,
            size_bytes: Some(12),
            mtime_ns: 0,
            device_id: None,
            inode: None,
            native_id: None,
            picture_index: None,
            artist_pictures: None,
            revision: Some("one-v1".into()),
            parse_version: Some(1),
            state: crate::LocalFileState::Accepted,
        };
        store
            .database
            .catalog("server", None)
            .await
            .unwrap()
            .write_file_observations(
                &[(file.clone(), Vec::new())],
                vec![crate::TrackObservation {
                    object_id: "one".into(),
                    media_uri: Some(uri.clone()),
                    source_path: Some(media_path.into()),
                    metadata: crate::TrackMetadata {
                        title: "First".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                true,
            )
            .await
            .unwrap();
        let keys = vec![("catalog:file".into(), key.clone())];
        let native = store.current_records(&keys).await.unwrap().remove(0);
        let revision = store.revision().await.unwrap();
        let mut retry = store.database.catalog("server", None).await.unwrap();
        retry
            .write_file_observations(
                &[(file.clone(), Vec::new())],
                serde_json::from_value(native.value.as_ref().unwrap()["tracks"].clone()).unwrap(),
                true,
            )
            .await
            .unwrap();
        assert!(!retry.changes().changed());
        assert_eq!(store.revision().await.unwrap(), revision);
        let native_page = store
            .changes_in_stream(0, 10, SyncStream::NativeCache)
            .await
            .unwrap();
        assert_eq!(native_page.len(), 1);
        assert!(
            !store
                .changes_in_stream(0, 10, SyncStream::Authored)
                .await
                .unwrap()
                .iter()
                .any(|version| version.name.starts_with("catalog:"))
        );
        let names = vec![native_page[0].name.clone()];
        let old = store
            .updates(&peer.versions(&names).await.unwrap())
            .await
            .unwrap();
        assert!(peer.import_updates(&old).await.unwrap());
        assert_eq!(
            peer.current_records(&keys).await.unwrap(),
            vec![native.clone()]
        );
        for (profile, title) in [(&store, "Left"), (&peer, "Right")] {
            let mut tracks: Vec<crate::TrackObservation> =
                serde_json::from_value(native.value.as_ref().unwrap()["tracks"].clone()).unwrap();
            tracks[0].metadata.title = title.into();
            profile
                .database
                .catalog("server", None)
                .await
                .unwrap()
                .write_file_observations(&[(file.clone(), Vec::new())], tracks, true)
                .await
                .unwrap();
        }
        deliver(&store, &peer).await;
        deliver(&peer, &store).await;
        assert_eq!(
            store.versions(&names).await.unwrap()[0].version,
            peer.versions(&names).await.unwrap()[0].version
        );
        assert_eq!(
            store.current_records(&keys).await.unwrap(),
            peer.current_records(&keys).await.unwrap()
        );
        assert!(!store.import_updates(&old).await.unwrap());

        let mut cursor = store.revision().await.unwrap();
        let native_before = store
            .changes_in_stream(0, 10, SyncStream::NativeCache)
            .await
            .unwrap();
        store
            .database
            .set_favorite(&crate::FavoriteTarget::Track(uri.clone()), true)
            .await
            .unwrap();
        let smart = store
            .database
            .create_smart_playlist("Saved", &crate::SmartPlaylistDefinition::default())
            .await
            .unwrap();
        let (_, playlist) = store
            .database
            .create_playlist(
                None,
                "Saved tracks",
                &crate::MemberSelection::Uris((vec![uri.clone()]).into()),
            )
            .await
            .unwrap()
            .unwrap();
        let smart: String =
            sqlx::query_scalar("SELECT object_id FROM smart_playlists WHERE smart_playlist_key=?1")
                .bind(smart)
                .fetch_one(&mut *store.writer().await.unwrap())
                .await
                .unwrap();
        let scalar_keys: Vec<(String, String)> = vec![
            ("state".into(), uri.clone()),
            ("smart".into(), smart),
            ("reference".into(), uri.clone()),
        ];
        let reference_name = format!("reference:{uri}");
        let playlist_name = format!("playlist:{}", json!([null, playlist]));
        let mut authored_names = Vec::new();
        assert_eq!(
            store
                .changes_in_stream(0, 10, SyncStream::NativeCache)
                .await
                .unwrap(),
            native_before
        );
        for deleted in [false, true] {
            if deleted {
                let smart = store
                    .database
                    .smart_playlist_key_by_object(
                        &scalar_keys[1].1,
                        &crate::ReadCancellation::new(),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                store.database.delete_smart_playlist(smart).await.unwrap();
                let reset = format!(
                    "{{\"version\":1}}\n{}\n",
                    json!({"media_uri":uri,"favorite":null,"rating":null})
                );
                store
                    .database
                    .import_user_media_state_jsonl(std::io::Cursor::new(reset))
                    .await
                    .unwrap();
                let clock: i64 =
                    sqlx::query_scalar("SELECT clock+1 FROM connect_profile WHERE singleton=1")
                        .fetch_one(&mut *store.writer().await.unwrap())
                        .await
                        .unwrap();
                store
                    .import_updates(&[serde_json::to_vec(&Update {
                        version: CURRENT_FORMAT,
                        document: format!("reference:{uri}"),
                        bytes: crate::RecordStamp {
                            lamport: clock as u64,
                            actor: 9,
                        }
                        .encode(),
                        author: None,
                        current: Some(ConnectRecord {
                            kind: "reference".into(),
                            key: uri.clone(),
                            value: None,
                        }),
                    })
                    .unwrap()])
                    .await
                    .unwrap();
            }
            loop {
                let page = store
                    .changes_in_stream(cursor, 1, SyncStream::Authored)
                    .await
                    .unwrap();
                let Some(version) = page.last() else { break };
                cursor = version.revision;
                if !deleted {
                    authored_names.push(version.name.clone());
                }
                let names = vec![version.name.clone()];
                let updates = store
                    .updates(&peer.versions(&names).await.unwrap())
                    .await
                    .unwrap();
                peer.import_updates(&updates).await.unwrap();
                store
                    .acknowledge_sync_in("peer", SyncStream::Authored, cursor)
                    .await
                    .unwrap();
            }
            if !deleted {
                assert!(
                    authored_names
                        .iter()
                        .position(|name| name == &reference_name)
                        .unwrap()
                        < authored_names
                            .iter()
                            .position(|name| name == &playlist_name)
                            .unwrap()
                );
            }
            assert_eq!(
                peer.current_records(&scalar_keys).await.unwrap().len(),
                if deleted { 0 } else { 3 }
            );
            assert_eq!(
                store
                    .sync_cursor_in("peer", SyncStream::Authored)
                    .await
                    .unwrap(),
                cursor
            );
        }
        assert_eq!(
            store
                .changes_in_stream(0, 10, SyncStream::NativeCache)
                .await
                .unwrap(),
            native_before
        );
        for (index, lower_first) in [true, false].into_iter().enumerate() {
            let identity = format!("continued-{index}");
            let mut listen = crate::ListenWrite {
                external_id: Some(identity.clone()),
                media_uri: uri.clone(),
                title: "Continued".into(),
                artist: "Artist".into(),
                album: "Album".into(),
                duration_millis: 180_000,
                disc_number: None,
                track_number: None,
                year: None,
                release_date: None,
                source_format: None,
                musicbrainz_recording_id: None,
                musicbrainz_release_track_id: None,
                started_at: 1_700_000_000,
                local_period: "2023-11".into(),
                listened_millis: 100_000,
                skipped: false,
            };
            peer.database
                .record_listen(&listen, None, &[])
                .await
                .unwrap();
            store
                .database
                .set_favorite(&crate::FavoriteTarget::Track(uri.clone()), lower_first)
                .await
                .unwrap();
            listen.listened_millis = 40_000;
            listen.skipped = true;
            store
                .database
                .record_listen(&listen, None, &[])
                .await
                .unwrap();
            let names = vec![format!("listen:{identity}")];
            let high = store.versions(&names).await.unwrap();
            let low = peer.versions(&names).await.unwrap();
            assert!(
                crate::RecordStamp::decode(&high[0].version).unwrap()
                    > crate::RecordStamp::decode(&low[0].version).unwrap()
            );
            let partial = store.updates(&low).await.unwrap();
            let complete = peer.updates(&high).await.unwrap();
            assert_eq!(partial.len(), 1);
            assert_eq!(complete.len(), 1);
            if lower_first {
                assert!(store.import_updates(&complete).await.unwrap());
                assert!(
                    !peer
                        .import_updates(
                            &store
                                .updates(&peer.versions(&names).await.unwrap())
                                .await
                                .unwrap()
                        )
                        .await
                        .unwrap()
                );
            } else {
                assert!(!peer.import_updates(&partial).await.unwrap());
                assert!(
                    store
                        .import_updates(
                            &peer
                                .updates(&store.versions(&names).await.unwrap())
                                .await
                                .unwrap()
                        )
                        .await
                        .unwrap()
                );
            }
            let keys = vec![("listen".into(), identity)];
            let expected = store.current_records(&keys).await.unwrap();
            assert_eq!(
                expected[0].value.as_ref().unwrap()["listened_millis"],
                100_000
            );
            assert_eq!(expected[0].value.as_ref().unwrap()["skipped"], 0);
            assert_eq!(peer.current_records(&keys).await.unwrap(), expected);
            assert_eq!(
                store.versions(&names).await.unwrap()[0].version,
                peer.versions(&names).await.unwrap()[0].version
            );
            let revisions = (
                store.revision().await.unwrap(),
                peer.revision().await.unwrap(),
            );
            assert!(!store.import_updates(&complete).await.unwrap());
            assert!(!peer.import_updates(&partial).await.unwrap());
            assert!(
                store
                    .updates(&peer.versions(&names).await.unwrap())
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                peer.updates(&store.versions(&names).await.unwrap())
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                revisions,
                (
                    store.revision().await.unwrap(),
                    peer.revision().await.unwrap()
                )
            );
        }
        for profile in [&store, &peer] {
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listen_outbox")
                    .fetch_one(&mut *profile.writer().await.unwrap())
                    .await
                    .unwrap(),
                0
            );
        }
        let watermark = store.revision().await.unwrap();
        let mut tracks: Vec<crate::TrackObservation> = serde_json::from_value(
            store.current_records(&keys).await.unwrap()[0]
                .value
                .as_ref()
                .unwrap()["tracks"]
                .clone(),
        )
        .unwrap();
        tracks[0].metadata.title = "Delta".into();
        store
            .database
            .catalog("server", None)
            .await
            .unwrap()
            .write_file_observations(&[(file.clone(), Vec::new())], tracks, true)
            .await
            .unwrap();
        let analyzed_uri = crate::source_entity_uri(&source, "track", "two");
        let analysis_key = [7; 32];
        let analyzed_path = "https://files.example.test/music/two.flac";
        let mut analyzed_file = file;
        analyzed_file.path = analyzed_path.into();
        analyzed_file.relative_path = "two.flac".into();
        analyzed_file.revision = Some("two-v1".into());
        store
            .database
            .catalog("server", None)
            .await
            .unwrap()
            .write_file_observations(
                &[(analyzed_file, Vec::new())],
                vec![crate::TrackObservation {
                    object_id: "two".into(),
                    media_uri: Some(analyzed_uri.clone()),
                    source_path: Some(analyzed_path.into()),
                    metadata: crate::TrackMetadata {
                        title: "Analyzed".into(),
                        ..Default::default()
                    },
                    loudness_analysis_key: analysis_key,
                    ..Default::default()
                }],
                true,
            )
            .await
            .unwrap();
        let (source_key, track_key): (crate::SourceKey, crate::TrackKey) =
            sqlx::query_as("SELECT source_key,track_key FROM tracks WHERE media_uri=?1")
                .bind(&analyzed_uri)
                .fetch_one(&mut *store.database.acquire_reader().await.unwrap())
                .await
                .unwrap();
        let loudness = crate::LoudnessMeasurement {
            analysis_key,
            integrated_lufs: Some(-14.0),
            true_peak: Some(0.8),
            replay_gain_db: None,
            replay_gain_peak: None,
        };
        assert!(
            store
                .database
                .write_track_analyzed_loudness(source_key, track_key, &loudness)
                .await
                .unwrap()
        );
        let waveform = crate::WaveformCacheRow {
            media_uri: analyzed_uri.clone(),
            analysis_key,
            peaks: vec![(0.25, 0.5), (0.75, 1.0)],
        };
        assert!(
            store
                .database
                .write_waveform_cache(&waveform)
                .await
                .unwrap()
        );
        assert!(
            peer.database
                .audio_analysis_key(&analyzed_uri)
                .await
                .unwrap()
                .is_none()
        );
        let path = directory.path().join("delta.sqlite");
        let names = store
            .changes_in_stream(watermark, CONNECT_PAGE_SIZE, SyncStream::NativeCache)
            .await
            .unwrap()
            .into_iter()
            .map(|version| version.name)
            .collect::<Vec<_>>();
        let updates = store
            .updates(&peer.versions(&names).await.unwrap())
            .await
            .unwrap();
        let mut carrier = SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        sqlx::raw_sql("PRAGMA user_version=50;
            CREATE TABLE updates(name TEXT PRIMARY KEY,kind TEXT,payload BLOB NOT NULL) STRICT;
            CREATE TABLE connect_profile(singleton INTEGER PRIMARY KEY,profile TEXT,publisher TEXT,schema_version INTEGER NOT NULL) STRICT;
            INSERT INTO connect_profile VALUES(1,NULL,NULL,2);")
            .execute(&mut carrier).await.unwrap();
        for bytes in updates {
            let update: Update = serde_json::from_slice(&bytes).unwrap();
            sqlx::query("INSERT INTO updates VALUES(?1,?2,?3)")
                .bind(update.document)
                .bind(update.current.map(|record| record.kind))
                .bind(bytes)
                .execute(&mut carrier)
                .await
                .unwrap();
        }
        carrier.close().await.unwrap();
        ProfileStore::prepare_database(&path, &KEY.parse().unwrap(), &KEY.parse().unwrap())
            .await
            .unwrap();
        let mut reader = SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .read_only(true),
        )
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM updates")
                .fetch_one(&mut reader)
                .await
                .unwrap(),
            4
        );
        assert!(!sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name IN('sources','local_values','projection','current_values','documents','connect_versions'))").fetch_one(&mut reader).await.unwrap());
        reader.close().await.unwrap();
        assert!(
            peer.merge_database(&path, &KEY.parse().unwrap())
                .await
                .unwrap()
        );
        assert_eq!(
            peer.current_records(&keys).await.unwrap()[0]
                .value
                .as_ref()
                .unwrap()["tracks"][0]["metadata"]["title"],
            "Delta"
        );
        assert_eq!(
            peer.database
                .playback_loudness(&analyzed_uri, &crate::ReadCancellation::new())
                .await
                .unwrap()
                .0,
            Some(loudness)
        );
        let received_waveform = peer
            .database
            .waveform_cache(&analyzed_uri)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received_waveform.analysis_key, waveform.analysis_key);
        assert_eq!(received_waveform.peaks, waveform.peaks);
        let revision = peer.revision().await.unwrap();
        assert!(
            !peer
                .merge_database(&path, &KEY.parse().unwrap())
                .await
                .unwrap()
        );
        assert_eq!(peer.revision().await.unwrap(), revision);
        store
            .database
            .catalog("server", None)
            .await
            .unwrap()
            .remove_source_files(&[media_path.to_owned()], i64::MAX)
            .await
            .unwrap();
        assert!(store.current_records(&keys).await.unwrap().is_empty());
        assert!(!store.import_updates(&old).await.unwrap());
        assert!(store.current_records(&keys).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn newer_local_edits_supersede_received_values() {
        let directory = tempfile::tempdir().unwrap();
        let a = ProfileStore::open(
            crate::Database::open(&directory.path().join("a.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let b = ProfileStore::open(
            crate::Database::open(&directory.path().join("b.sqlite"))
                .await
                .unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let source = |credential: &str| ConnectRecord {
            kind: "source".into(),
            key: "server".into(),
            value: Some(json!({"configuration":{"name":"Server"},"credential":credential})),
        };
        a.write_records(&[source("remote")]).await.unwrap();
        deliver(&a, &b).await;
        b.write_records(&[source("new-local")]).await.unwrap();
        let projected = b.stored_records().await.unwrap();
        assert_eq!(projected, vec![source("new-local")]);

        a.bind_profile("profile", "a").await.unwrap();
        b.bind_profile("profile", "b").await.unwrap();
        let source_id = crate::SourceId::new("server");
        for database in [&a.database, &b.database] {
            database
                .reconcile_source(&source_id, "jellyfin", "Server")
                .await
                .unwrap();
            database.connect_set_active(true).await.unwrap();
        }
        {
            let mut catalog = a.database.catalog("server", None).await.unwrap();
            catalog.start_observation().await.unwrap();
            catalog
                .apply_catalog(
                    &crate::CatalogBatch {
                        playlists: vec![crate::PlaylistObservation {
                            object_id: "native".into(),
                            name: "Remote".into(),
                            sort_text: "remote".into(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    true,
                )
                .await
                .unwrap();
        }
        let snapshot = directory.path().join("ready.sqlite");
        a.export_database(&snapshot, &KEY.parse().unwrap(), &[], &[])
            .await
            .unwrap();
        let database = b.database.clone();
        b.close().await.unwrap();
        ProfileStore::prepare_database(&snapshot, &KEY.parse().unwrap(), &KEY.parse().unwrap())
            .await
            .unwrap();
        ProfileStore::install_prepared_database(database.clone(), &snapshot)
            .await
            .unwrap();
        let b = ProfileStore::open(database, 2, KEY.parse().unwrap())
            .await
            .unwrap();
        let initial: String = sqlx::query_scalar(
            "SELECT name FROM playlists WHERE owner='source' AND object_id='native'",
        )
        .fetch_one(&mut *b.writer().await.unwrap())
        .await
        .unwrap();
        assert_eq!(initial, "Remote");
        {
            let mut catalog = b.database.catalog("server", None).await.unwrap();
            catalog.start_observation().await.unwrap();
            catalog
                .apply_catalog(
                    &crate::CatalogBatch {
                        playlists: vec![crate::PlaylistObservation {
                            object_id: "native".into(),
                            name: "Local scan".into(),
                            sort_text: "local scan".into(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    true,
                )
                .await
                .unwrap();
        }

        assert!(
            !b.merge_database(&snapshot, &KEY.parse().unwrap())
                .await
                .unwrap()
        );
        assert!(
            !b.changes_in_stream(0, 100, SyncStream::NativeCache)
                .await
                .unwrap()
                .iter()
                .any(|version| version.name.starts_with("catalog:native_"))
        );
        let saved: String = sqlx::query_scalar(
            "SELECT name FROM playlists WHERE owner='source' AND object_id='native'",
        )
        .fetch_one(&mut *b.writer().await.unwrap())
        .await
        .unwrap();
        assert_eq!(saved, "Local scan");
    }

    #[tokio::test]
    async fn file_edits_are_delivered_to_peers_after_offline_import() {
        let directory = tempfile::tempdir().unwrap();
        let original = ProfileStore::open(
            crate::Database::open(&directory.path().join("original.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let offline = ProfileStore::open(
            crate::Database::open(&directory.path().join("offline.sqlite"))
                .await
                .unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let joining = ProfileStore::open(
            crate::Database::open(&directory.path().join("joining.sqlite"))
                .await
                .unwrap(),
            3,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let record = |value| ConnectRecord {
            kind: "preference".into(),
            key: "private_mode".into(),
            value: Some(json!(value)),
        };
        original
            .write_records(&[
                record(true),
                ConnectRecord {
                    kind: "preference".into(),
                    key: "theme".into(),
                    value: Some(json!("dark")),
                },
            ])
            .await
            .unwrap();
        let file = directory.path().join("profile");
        let peer_file = directory.path().join("peer-profile");
        original
            .export_database(&peer_file, &KEY.parse().unwrap(), &[], &[])
            .await
            .unwrap();
        joining
            .merge_database(&peer_file, &KEY.parse().unwrap())
            .await
            .unwrap();
        assert!(missing_updates(&joining, &original).await.is_empty());
        original
            .export_database(&file, &KEY.parse().unwrap(), &[], &[])
            .await
            .unwrap();
        offline
            .merge_database(&file, &KEY.parse().unwrap())
            .await
            .unwrap();
        offline.write_records(&[record(false)]).await.unwrap();
        offline
            .export_database(&file, &KEY.parse().unwrap(), &[], &[])
            .await
            .unwrap();
        joining
            .merge_database(&file, &KEY.parse().unwrap())
            .await
            .unwrap();
        assert!(!missing_updates(&joining, &original).await.is_empty());
        joining
            .merge_database(&peer_file, &KEY.parse().unwrap())
            .await
            .unwrap();
        assert_eq!(missing_updates(&joining, &original).await.len(), 1);
        deliver(&joining, &original).await;
        assert!(missing_updates(&joining, &original).await.is_empty());
        assert!(
            !joining
                .merge_database(&file, &KEY.parse().unwrap())
                .await
                .unwrap()
        );
        assert!(missing_updates(&joining, &original).await.is_empty());
        assert!(
            original
                .stored_records()
                .await
                .unwrap()
                .contains(&record(false))
        );
        joining.bind_profile("profile", "joining").await.unwrap();
        joining.database.connect_set_active(true).await.unwrap();
        joining
            .database
            .set_favorite(&crate::FavoriteTarget::Track("track".into()), true)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn snapshot_replacement_and_database_adoption_preserve_causal_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profile.sqlite");
        let snapshot = directory.path().join("profile.jsonl");
        let profile = ProfileStore::open(
            crate::Database::open(&path).await.unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        profile
            .write_records(&["device", "playlist_artwork"].map(|kind| ConnectRecord {
                kind: kind.into(),
                key: if kind == "playlist_artwork" {
                    json!([null, "saved"]).to_string()
                } else {
                    "saved".into()
                },
                value: Some(if kind == "playlist_artwork" {
                    json!({"artwork_binding":null})
                } else {
                    json!("retained")
                }),
            }))
            .await
            .unwrap();
        profile
            .export_database(&snapshot, &KEY.parse().unwrap(), &[], &[])
            .await
            .unwrap();
        assert_eq!(
            profile
                .sync_cursor_in("peer", SyncStream::Authored)
                .await
                .unwrap(),
            0
        );
        profile
            .acknowledge_sync_in("peer", SyncStream::Authored, 12)
            .await
            .unwrap();
        profile
            .acknowledge_sync_in("peer", SyncStream::NativeCache, 24)
            .await
            .unwrap();
        let retained: Vec<(String, Vec<u8>)> = {
            let mut connection = profile.writer().await.unwrap();
            let retained = sqlx::query_as("SELECT name,snapshot FROM documents ORDER BY name")
                .fetch_all(&mut *connection)
                .await
                .unwrap();
            sqlx::raw_sql(
                "CREATE TABLE projection(kind TEXT NOT NULL,object_key TEXT NOT NULL,payload TEXT,PRIMARY KEY(kind,object_key)) STRICT;
                CREATE TABLE local_values(kind TEXT NOT NULL,object_key TEXT NOT NULL,payload TEXT,PRIMARY KEY(kind,object_key)) STRICT;
                INSERT OR IGNORE INTO history_pending SELECT name FROM documents;
                INSERT INTO history_acknowledgements SELECT 'peer',name,version FROM documents;
                INSERT INTO documents(name,snapshot,revision,version) VALUES('cache_artwork:old',X'00',1,X'');
                INSERT INTO projection VALUES('cache_artwork','old','null');
                INSERT INTO local_values VALUES('cache_artwork','old','null');
                INSERT INTO history_pending VALUES('cache_artwork:old');
                INSERT INTO history_acknowledgements VALUES('peer','cache_artwork:old',X'');
                INSERT INTO documents(name,snapshot,revision,version) VALUES('cache_lyrics:old',X'00',1,X'');
                INSERT INTO projection VALUES('cache_lyrics','old','null');
                INSERT INTO local_values VALUES('cache_lyrics','old','null');
                INSERT INTO history_pending VALUES('cache_lyrics:old');
                INSERT INTO history_acknowledgements VALUES('peer','cache_lyrics:old',X'');
                UPDATE connect_profile SET schema_version=0 WHERE singleton=1;",
            )
            .execute(&mut *connection)
            .await
            .unwrap();
            retained
        };
        drop(profile);
        let profile = ProfileStore::open(
            crate::Database::open(&path).await.unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        profile.clear_legacy_settings().await.unwrap();
        {
            let mut connection = profile.writer().await.unwrap();
            let saved: Vec<(String, Vec<u8>)> =
                sqlx::query_as("SELECT name,snapshot FROM documents ORDER BY name")
                    .fetch_all(&mut *connection)
                    .await
                    .unwrap();
            assert_eq!(saved, retained);
            assert!(!sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name IN('projection','local_values'))").fetch_one(&mut *connection).await.unwrap());
            for table in ["history_pending", "history_acknowledgements"] {
                let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT count(*) FROM {table}"
                )))
                .fetch_one(&mut *connection)
                .await
                .unwrap();
                assert_eq!(count, 2, "{table}");
            }
        }
        assert_eq!(
            profile
                .sync_cursor_in("peer", SyncStream::Authored)
                .await
                .unwrap(),
            12
        );
        assert_eq!(
            profile
                .sync_cursor_in("peer", SyncStream::NativeCache)
                .await
                .unwrap(),
            24
        );
        assert!(
            !profile
                .merge_database(&snapshot, &KEY.parse().unwrap())
                .await
                .unwrap()
        );
        assert_eq!(
            profile
                .sync_cursor_in("peer", SyncStream::Authored)
                .await
                .unwrap(),
            12
        );
        assert_eq!(
            profile
                .sync_cursor_in("peer", SyncStream::NativeCache)
                .await
                .unwrap(),
            24
        );
        let bytes = std::fs::read(&snapshot).unwrap();
        std::fs::write(&snapshot, b"invalid SQLite file").unwrap();
        assert!(
            profile
                .merge_database(&snapshot, &KEY.parse().unwrap())
                .await
                .is_err()
        );
        assert_eq!(
            profile
                .sync_cursor_in("peer", SyncStream::NativeCache)
                .await
                .unwrap(),
            24
        );
        std::fs::write(&snapshot, bytes).unwrap();
        let database = profile.database.clone();
        profile.close().await.unwrap();
        ProfileStore::prepare_database(&snapshot, &KEY.parse().unwrap(), &KEY.parse().unwrap())
            .await
            .unwrap();
        ProfileStore::install_prepared_database(database.clone(), &snapshot)
            .await
            .unwrap();
        let profile = ProfileStore::open(database, 1, KEY.parse().unwrap())
            .await
            .unwrap();
        assert_eq!(
            profile
                .sync_cursor_in("peer", SyncStream::Authored)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            profile
                .sync_cursor_in("peer", SyncStream::NativeCache)
                .await
                .unwrap(),
            0
        );
        profile
            .acknowledge_sync_in("peer", SyncStream::NativeCache, 30)
            .await
            .unwrap();
        let database = profile.database.clone();
        profile.close().await.unwrap();
        ProfileStore::prepare_database(&snapshot, &KEY.parse().unwrap(), &KEY.parse().unwrap())
            .await
            .unwrap();
        ProfileStore::install_prepared_database(database.clone(), &snapshot)
            .await
            .unwrap();
        let profile = ProfileStore::open(database, 1, KEY.parse().unwrap())
            .await
            .unwrap();
        assert_eq!(
            profile
                .sync_cursor_in("peer", SyncStream::NativeCache)
                .await
                .unwrap(),
            0
        );

        profile.bind_profile("shared", "donor").await.unwrap();
        let secret = ConnectRecord {
            kind: "source".into(),
            key: "server".into(),
            value: Some(json!({"name":"Server","password":"private-connect-token"})),
        };
        profile
            .write_records(std::slice::from_ref(&secret))
            .await
            .unwrap();
        profile
            .database
            .reconcile_source(&crate::SourceId::new("server"), "webdav", "Files")
            .await
            .unwrap();
        profile.database.connect_set_active(true).await.unwrap();
        let path = "https://files.example.test/music/one.flac";
        profile
            .database
            .catalog("server", None)
            .await
            .unwrap()
            .write_file_observations(
                &[(
                    crate::LocalFileWrite {
                        path: path.into(),
                        root: "https://files.example.test/music".into(),
                        relative_path: "one.flac".into(),
                        kind: crate::LocalFileKind::Media,
                        size_bytes: Some(12),
                        mtime_ns: 0,
                        device_id: None,
                        inode: None,
                        native_id: None,
                        picture_index: None,
                        artist_pictures: None,
                        revision: Some("one-v1".into()),
                        parse_version: Some(1),
                        state: crate::LocalFileState::Accepted,
                    },
                    Vec::new(),
                )],
                vec![crate::TrackObservation {
                    object_id: "one".into(),
                    source_path: Some(path.into()),
                    metadata: crate::TrackMetadata {
                        title: "Original".into(),
                        artist: "Artist".into(),
                        duration_millis: 180_000,
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                true,
            )
            .await
            .unwrap();
        let native = Database::connect_record_on(
            &mut *profile.writer().await.unwrap(),
            "catalog:file",
            &json!(["server", path]).to_string(),
        )
        .await
        .unwrap();
        profile
            .acknowledge_sync_in("peer", SyncStream::NativeCache, 30)
            .await
            .unwrap();
        let names: Vec<String> = {
            let mut connection = profile.writer().await.unwrap();
            sqlx::query_scalar("SELECT name FROM documents ORDER BY name")
                .fetch_all(&mut *connection)
                .await
                .unwrap()
        };
        let versions = profile.versions(&names).await.unwrap();
        let transport = age::x25519::Identity::generate();
        let copied_path = directory.path().join("copied.sqlite");
        profile
            .export_database(&copied_path, &transport, &[], &[])
            .await
            .unwrap();
        let bytes = std::fs::read(&copied_path).unwrap();
        assert!(
            !bytes
                .windows(21)
                .any(|bytes| bytes == b"private-connect-token")
        );
        let copied = ProfileStore::open(
            crate::Database::open(&copied_path).await.unwrap(),
            99,
            transport.clone(),
        )
        .await
        .unwrap();
        assert_eq!(copied.versions(&names).await.unwrap(), versions);
        assert_eq!(
            copied.revision().await.unwrap(),
            profile.revision().await.unwrap()
        );
        {
            let mut connection = copied.writer().await.unwrap();
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT revision FROM file_revision WHERE id=1")
                    .fetch_one(&mut *connection)
                    .await
                    .unwrap(),
                profile.revision().await.unwrap()
            );
            let snapshot: Vec<u8> =
                sqlx::query_scalar("SELECT snapshot FROM documents WHERE name='source:server'")
                    .fetch_one(&mut *connection)
                    .await
                    .unwrap();
            assert!(expose("source:server", &snapshot, &KEY.parse().unwrap()).is_err());
            assert!(expose("source:server", &snapshot, &transport).is_ok());
        }
        assert!(copied.stored_records().await.unwrap().contains(&secret));
        assert_eq!(
            copied
                .sync_cursor_in("peer", SyncStream::NativeCache)
                .await
                .unwrap(),
            0
        );
        assert!(missing_updates(&profile, &copied).await.is_empty());
        copied.close().await.unwrap();
        assert!(
            copied
                .write_records(std::slice::from_ref(&secret))
                .await
                .is_err()
        );
        copied.database.clone().close().await.unwrap();

        let recipient_path = directory.path().join("recipient.sqlite");
        let recipient_key = age::x25519::Identity::generate();
        let recipient = ProfileStore::open(
            crate::Database::open(&recipient_path).await.unwrap(),
            2,
            recipient_key.clone(),
        )
        .await
        .unwrap();
        recipient
            .write_records(&[ConnectRecord {
                kind: "preference".into(),
                key: "old".into(),
                value: Some(json!(true)),
            }])
            .await
            .unwrap();
        let floor = recipient.revision().await.unwrap();
        {
            let legacy_path = directory.path().join("legacy.sqlite");
            let mut incoming = SqliteConnection::connect_with(
                &sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&legacy_path)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
            catalog_fixture::copy_before_index(&mut incoming, &copied_path).await;
            sqlx::raw_sql(
                "PRAGMA user_version=49;
                UPDATE connect_profile SET schema_version=1;
                UPDATE connect_versions SET version=json_object(CAST(json_extract(version,'$.actor') AS TEXT),json_extract(version,'$.lamport'));
                INSERT INTO connect_versions(name,kind,object_key,version,revision,deleted)
                SELECT 'catalog:track:[\"server\",\"one\"]','catalog:track','[\"server\",\"one\"]','{\"1\":1}',revision,0 FROM file_revision WHERE id=1;",
            )
            .execute(&mut incoming)
            .await
            .unwrap();
            incoming.close().await.unwrap();
            std::fs::rename(legacy_path, &copied_path).unwrap();
        }
        assert!(
            ProfileStore::prepare_database(&copied_path, &recipient_key, &recipient_key)
                .await
                .is_err()
        );
        ProfileStore::prepare_database(&copied_path, &transport, &recipient_key)
            .await
            .unwrap();
        {
            let mut incoming = SqliteConnection::connect_with(
                &sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&copied_path)
                    .read_only(true),
            )
            .await
            .unwrap();
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM connect_versions WHERE kind='catalog:track' OR json_type(version,'$.lamport') IS NULL").fetch_one(&mut incoming).await.unwrap(), 0);
            incoming.close().await.unwrap();
        }
        recipient
            .merge_database(&copied_path, &recipient_key)
            .await
            .unwrap();
        assert!(recipient.revision().await.unwrap() > floor);
        let imported = recipient.versions(&names).await.unwrap();
        for original in &versions {
            let adopted = imported
                .iter()
                .find(|version| version.name == original.name)
                .unwrap();
            assert_eq!(original.version, adopted.version);
        }
        assert!(
            recipient
                .stored_records()
                .await
                .unwrap()
                .contains(&ConnectRecord {
                    kind: "preference".into(),
                    key: "old".into(),
                    value: Some(json!(true)),
                })
        );
        {
            let mut connection = recipient.writer().await.unwrap();
            let acknowledged: Vec<(String, Vec<u8>)> = sqlx::query_as(
                "SELECT name,version FROM history_acknowledgements WHERE peer='donor'",
            )
            .fetch_all(&mut *connection)
            .await
            .unwrap();
            let acknowledged = acknowledged.into_iter().collect::<BTreeMap<_, _>>();
            let expected = versions
                .iter()
                .filter(|version| history::retains_history(&version.name))
                .map(|version| (version.name.clone(), version.version.clone()))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(acknowledged, expected);
        }
        assert!(recipient.stored_records().await.unwrap().contains(&secret));
        let mut changed = native.clone();
        changed.value.as_mut().unwrap()["tracks"][0]["metadata"]["title"] = json!("Receiver edit");
        let mut edited_secret = secret.clone();
        edited_secret.value =
            Some(json!({"name":"Receiver server","password":"private-connect-token"}));
        recipient.bind_profile("shared", "receiver").await.unwrap();
        recipient.database.connect_set_active(true).await.unwrap();
        let file_name = format!("{}:{}", native.kind, native.key);
        let previous_file = recipient
            .versions(std::slice::from_ref(&file_name))
            .await
            .unwrap()
            .remove(0);
        recipient
            .database
            .catalog("server", None)
            .await
            .unwrap()
            .write_file_observations(
                &[(
                    serde_json::from_value(changed.value.as_ref().unwrap()["file"].clone())
                        .unwrap(),
                    Vec::new(),
                )],
                serde_json::from_value(changed.value.as_ref().unwrap()["tracks"].clone()).unwrap(),
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            recipient
                .current_records(&[(native.kind.clone(), native.key.clone())])
                .await
                .unwrap(),
            vec![changed.clone()]
        );
        let updated_file = recipient.versions(&[file_name]).await.unwrap().remove(0);
        assert!(
            crate::RecordStamp::decode(&updated_file.version).unwrap()
                > crate::RecordStamp::decode(&previous_file.version).unwrap()
        );
        recipient
            .write_records(std::slice::from_ref(&edited_secret))
            .await
            .unwrap();
        deliver(&recipient, &profile).await;
        assert!(
            profile
                .stored_records()
                .await
                .unwrap()
                .contains(&edited_secret)
        );
        assert_eq!(
            profile
                .current_records(&[(native.kind.clone(), native.key.clone())])
                .await
                .unwrap(),
            vec![changed.clone()]
        );
        recipient.close().await.unwrap();
        let recipient = ProfileStore::open(
            crate::Database::open(&recipient_path).await.unwrap(),
            2,
            recipient_key,
        )
        .await
        .unwrap();
        assert!(
            recipient
                .stored_records()
                .await
                .unwrap()
                .contains(&edited_secret)
        );
        assert_eq!(
            recipient
                .current_records(&[(native.kind, native.key)])
                .await
                .unwrap(),
            vec![changed]
        );
    }

    #[tokio::test]
    async fn invalid_snapshot_rolls_back_every_document_and_stale_snapshot_keeps_newer_edits() {
        let directory = tempfile::tempdir().unwrap();
        let a = ProfileStore::open(
            crate::Database::open(&directory.path().join("a.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let b = ProfileStore::open(
            crate::Database::open(&directory.path().join("b.sqlite"))
                .await
                .unwrap(),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let staged = ProfileStore::open(
            crate::Database::open(&directory.path().join("staged.sqlite"))
                .await
                .unwrap(),
            3,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let record = |value| ConnectRecord {
            kind: "preference".into(),
            key: "theme".into(),
            value: Some(json!(value)),
        };
        a.write_records(&[record("dark")]).await.unwrap();
        a.write_records(
            &(0..CONNECT_PAGE_SIZE)
                .map(|index| ConnectRecord {
                    kind: "preference".into(),
                    key: format!("setting-{index:04}"),
                    value: Some(json!(index)),
                })
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();
        let snapshot = directory.path().join("profile.jsonl");
        let names = a
            .changes_in_stream(0, CONNECT_PAGE_SIZE + 1, SyncStream::Authored)
            .await
            .unwrap()
            .into_iter()
            .map(|version| version.name)
            .collect::<Vec<_>>();
        let updates = a.updates(&b.versions(&names).await.unwrap()).await.unwrap();
        let mut legacy = GzEncoder::new(Vec::new(), Compression::default());
        writeln!(legacy, "{FORMAT}").unwrap();
        for update in updates {
            legacy.write_all(&update).unwrap();
            legacy.write_all(b"\n").unwrap();
        }
        writeln!(legacy, "END").unwrap();
        let complete = legacy.finish().unwrap();
        std::fs::write(&snapshot, &complete).unwrap();
        let mut decoded = String::new();
        MultiGzDecoder::new(complete.as_slice())
            .read_to_string(&mut decoded)
            .unwrap();
        let decoded = decoded.replace(
            "\"document\":\"preference:theme\"",
            "\"document\":\"preference:another\"",
        );
        let mut wrong_document = GzEncoder::new(Vec::new(), Compression::default());
        wrong_document.write_all(decoded.as_bytes()).unwrap();
        for invalid in [
            complete[..complete.len() - 4].to_vec(),
            wrong_document.finish().unwrap(),
        ] {
            assert!(
                staged
                    .replace_snapshot(std::io::Cursor::new(invalid), |_| {})
                    .await
                    .is_err()
            );
            assert!(
                b.changes_in_stream(0, CONNECT_PAGE_SIZE, SyncStream::Authored)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM documents")
                    .fetch_one(&mut *staged.writer().await.unwrap())
                    .await
                    .unwrap(),
                0
            );
        }
        staged
            .replace_snapshot(std::io::Cursor::new(complete), |_| {})
            .await
            .unwrap();
        let converted = directory.path().join("converted.sqlite");
        staged
            .export_database(&converted, &KEY.parse().unwrap(), &[], &[])
            .await
            .unwrap();
        ProfileStore::prepare_database(&converted, &KEY.parse().unwrap(), &KEY.parse().unwrap())
            .await
            .unwrap();
        b.merge_database(&converted, &KEY.parse().unwrap())
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM documents")
                .fetch_one(&mut *b.writer().await.unwrap())
                .await
                .unwrap(),
            (CONNECT_PAGE_SIZE + 1) as i64
        );
        b.write_records(&[record("light")]).await.unwrap();
        assert!(
            !b.merge_database(&converted, &KEY.parse().unwrap())
                .await
                .unwrap()
        );
        let mut connection = b.writer().await.unwrap();
        let current = records(
            &b.load_document(&mut connection, "preference:theme")
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(current[&("preference".into(), "theme".into())], "light");
    }
}
