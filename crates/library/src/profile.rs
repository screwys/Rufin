//! Shared edits beside the working catalog. Ordered edits use Loro; completed
//! file results and other current values use the catalog's change journal.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::{CONNECT_PAGE_SIZE, ConnectRecord, Database};
use anyhow::{Context, Result, bail};
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
// Connect state saved by earlier versions is reset once instead of upgraded.
pub(crate) const SCHEMA_VERSION: i64 = 3;

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
        let mut writer = database.writer().await?;
        sqlx::query("UPDATE connect_profile SET actor=?1 WHERE singleton=1 AND actor IS NOT ?1")
            .bind(peer_id.to_string())
            .execute(writer.as_mut().context("Library writer is closed")?)
            .await?;
        drop(writer);
        Ok(Self {
            database,
            peer_id,
            identity: Arc::new(key),
            closed: AtomicBool::new(false),
        })
    }

    /// Whether this Store still holds Connect state from an earlier version.
    pub async fn reset_pending(database: &Database) -> Result<bool> {
        let mut writer = database.writer().await?;
        Ok(
            sqlx::query_scalar("SELECT schema_version<?1 FROM connect_profile WHERE singleton=1")
                .bind(SCHEMA_VERSION)
                .fetch_one(writer.as_mut().context("Library writer is closed")?)
                .await?,
        )
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

    #[cfg(test)]
    pub(crate) async fn current_records(
        &self,
        keys: &[(String, String)],
    ) -> Result<Vec<ConnectRecord>> {
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
                .map(|bytes| decode_update(bytes))
                .collect::<Result<Vec<_>>>()?;
            changed |= self.import_page_on(&mut transaction, page).await?.0;
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

    /// Return Connect's tables to the state of a new install and remove the
    /// Store records of files in the Connect directory. This also records that
    /// Connect state from earlier versions has been reset.
    pub async fn clear_database(database: &Database, directory: &Path) -> Result<()> {
        let files = url::Url::from_directory_path(directory)
            .ok()
            .context("The Connect directory is not absolute")?
            .to_string();
        let mut writer = database.writer().await?;
        let mut transaction = writer
            .as_mut()
            .context("Library writer is closed")?
            .begin()
            .await?;
        sqlx::raw_sql("DELETE FROM documents; DELETE FROM connect_versions; DELETE FROM sync_cursors; DELETE FROM history_peers; DELETE FROM history_acknowledgements; DELETE FROM history_pending; UPDATE file_revision SET revision=0 WHERE id=1;
            UPDATE connect_seed SET cursor=0,complete=0; UPDATE connect_playlist_seed SET playlist=0,position=-1; DELETE FROM queue_transfer_pages;")
            .execute(&mut *transaction).await?;
        sqlx::query("DELETE FROM connect_media_files WHERE substr(path,1,length(?1))=?1; DELETE FROM local_locators WHERE substr(access_uri,1,length(?1))=?1; DELETE FROM local_access_metadata WHERE substr(access_uri,1,length(?1))=?1;")
            .bind(files).execute(&mut *transaction).await?;
        sqlx::query("UPDATE connect_profile SET profile=NULL,publisher=NULL,actor='0',clock=0,enabled=0,receiving=0,schema_version=?1 WHERE singleton=1")
            .bind(SCHEMA_VERSION).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
    }
}

fn decode_update(bytes: &[u8]) -> Result<Update> {
    let update: Update = serde_json::from_slice(bytes).context("invalid Connect update")?;
    if !matches!(update.version, FORMAT | CURRENT_FORMAT)
        || (update.version == CURRENT_FORMAT) != update.current.is_some()
    {
        bail!(INCOMPATIBLE_PROFILE);
    }
    Ok(update)
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
    if record.kind == "entry" {
        let (source, playlist, _): (Option<String>, String, String) =
            serde_json::from_str(&record.key)?;
        return Ok(format!(
            "playlist:{}",
            serde_json::to_string(&(source, playlist))?
        ));
    }
    Ok(format!("{}:{}", record.kind, record.key))
}

impl ProfileStore {
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
    if matches!(record.kind.as_str(), "playlist" | "device") {
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
        "entry" | "playlist_order" | "smart_order"
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
        if matches!(kind.as_str(), "entry" | "playlist_order" | "smart_order") {
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
    ) -> Result<(bool, Vec<VersionVector>)> {
        let mut versions = vec![VersionVector::default(); updates.len()];
        let mut changed = false;
        let mut authored = Vec::new();
        let mut positions = Vec::new();
        let mut accepted = Vec::new();
        for (position, update) in updates.into_iter().enumerate() {
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
        for (position, result) in positions
            .into_iter()
            .zip(self.import_authored_page_on(connection, authored).await?)
        {
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
        let (results, changes, snapshots) = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut results = Vec::with_capacity(updates.len());
            let mut changes = BTreeMap::new();
            let mut snapshots = Vec::new();
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
                let metadata = LoroDoc::decode_import_blob_meta(&update.bytes, true)?;
                let reuse_snapshot =
                    previous.is_none() && metadata.mode == loro::EncodedBlobMode::Snapshot;
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
                let after = records(&document)?;
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
            ))
        })
        .await??;
        crate::connect::apply_records_on(connection, &changes).await?;
        save_snapshots_on(connection, Some(&self.identity), &snapshots).await?;
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

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
            .create_smart_playlist("First", &definition, None)
            .await
            .unwrap();
        let middle = a
            .database
            .create_smart_playlist("Middle", &definition, None)
            .await
            .unwrap();
        a.database
            .create_smart_playlist("Last", &definition, None)
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
            .create_smart_playlist("Appended", &definition, None)
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
        a.write_records(&[ConnectRecord {
            kind: "preference".into(),
            key: "theme".into(),
            value: Some(json!("dark")),
        }])
        .await
        .unwrap();
        let names = ["preference:theme".to_owned()];
        let updates = a.updates(&b.versions(&names).await.unwrap()).await.unwrap();
        ProfileStore::clear_database(&b.database, directory.path())
            .await
            .unwrap();
        b.import_updates(&updates).await.unwrap();
        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM documents ORDER BY name")
            .fetch_all(&mut *b.writer().await.unwrap())
            .await
            .unwrap();
        assert_eq!(names, ["preference:theme"]);
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
            .create_smart_playlist("Saved", &crate::SmartPlaylistDefinition::default(), None)
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
        assert!(peer.import_updates(&updates).await.unwrap());
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
        assert!(!peer.import_updates(&updates).await.unwrap());
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
        drop(profile);
        let profile = ProfileStore::open(
            crate::Database::open(&path).await.unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
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
        assert!(
            ProfileStore::prepare_database(&copied_path, &recipient_key, &recipient_key)
                .await
                .is_err()
        );
        ProfileStore::prepare_database(&copied_path, &transport, &recipient_key)
            .await
            .unwrap();
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
    async fn stale_profile_file_keeps_newer_edits() {
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
        let converted = directory.path().join("converted.sqlite");
        a.export_database(&converted, &KEY.parse().unwrap(), &[], &[])
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
