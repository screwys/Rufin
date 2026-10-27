//! Connect's own documents: device names, membership, keys and network settings, exchanged
//! between paired devices. Library rows are neither read nor written here.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::{CONNECT_PAGE_SIZE, Catalog, ConnectRecord};
use anyhow::{Context, Result, bail};
use loro::{ExportMode, LoroDoc, ToJson, VersionVector};
use serde::{Deserialize, Serialize};
use sqlx::{Connection, QueryBuilder, Row, SqliteConnection};

mod database;
mod encryption;
mod history;
use encryption::{expose, expose_payload, protect, sensitive};
#[cfg(test)]
mod history_tests;

// Wire format, independent of Rufin releases and local Store migrations.
const FORMAT: u32 = 1;
const CURRENT_FORMAT: u32 = 4;
pub const INCOMPATIBLE_PROFILE: &str =
    "Devices using Rufin Connect are no longer compatible. Please update Rufin.";
// Connect state saved by earlier versions is reset once instead of upgraded.
pub(crate) const SCHEMA_VERSION: i64 = 3;

pub struct ProfileStore {
    catalog: Catalog,
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
    pub async fn open(catalog: Catalog, peer_id: u64, key: age::x25519::Identity) -> Result<Self> {
        let mut writer = catalog.writer().await?;
        sqlx::query("UPDATE connect_profile SET actor=?1 WHERE singleton=1 AND actor IS NOT ?1")
            .bind(peer_id.to_string())
            .execute(&mut *writer)
            .await?;
        drop(writer);
        Ok(Self {
            catalog,
            peer_id,
            identity: Arc::new(key),
            closed: AtomicBool::new(false),
        })
    }

    /// Whether this Store still holds Connect state from an earlier version.
    pub async fn reset_pending(catalog: &Catalog) -> Result<bool> {
        let mut writer = catalog.writer().await?;
        Ok(
            sqlx::query_scalar("SELECT schema_version<?1 FROM connect_profile WHERE singleton=1")
                .bind(SCHEMA_VERSION)
                .fetch_one(&mut *writer)
                .await?,
        )
    }

    pub async fn close(&self) -> Result<()> {
        self.closed.store(true, Ordering::Release);
        let _writer = self.catalog.writer().await?;
        let _export = self.catalog.acquire_export().await?;
        Ok(())
    }

    async fn writer(&self) -> Result<crate::db::WriterGuard<'_>> {
        let guard = self.catalog.writer().await?;
        if self.closed.load(Ordering::Acquire) {
            bail!("Connect profile is closed");
        }
        Ok(guard)
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
        // Native caches are not shared until Connect sync comes back.
        if stream == SyncStream::NativeCache {
            return Ok(Vec::new());
        }
        let predicate = "name NOT GLOB 'catalog:*' AND name NOT GLOB 'analysis:*'";
        Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT name,version,revision,0 FROM documents WHERE {predicate} AND revision>?1 AND revision<=coalesce((SELECT min(revision) FROM documents WHERE {predicate} AND revision>?1 AND (name GLOB 'playlist_artwork:*')),9223372036854775807) ORDER BY revision,name LIMIT ?2"
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
            query.push(") SELECT d.name,d.version,d.revision,0 FROM requested r JOIN documents d USING(name)");
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
            // Current library values are not shared while Connect sync is off.
            if current_identity(&version.name).is_some() {
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

    /// Return Connect's tables to the state of a new install and remove the
    /// Store records of files in the Connect directory. This also records that
    /// Connect state from earlier versions has been reset.
    pub async fn clear_database(catalog: &Catalog, directory: &Path) -> Result<()> {
        let files = url::Url::from_directory_path(directory)
            .ok()
            .context("The Connect directory is not absolute")?
            .to_string();
        let mut write = catalog.write().await?;
        let connection = write.connection().await?;
        sqlx::raw_sql("DELETE FROM documents; DELETE FROM sync_cursors; DELETE FROM history_peers; DELETE FROM history_acknowledgements; DELETE FROM history_pending; UPDATE file_revision SET revision=0 WHERE id=1;
            DELETE FROM queue_transfer_pages;")
            .execute(&mut *connection).await?;
        let removed: Vec<String> = sqlx::query_scalar(
            "DELETE FROM downloads WHERE substr(path,1,length(?1))=?1 RETURNING media_uri",
        )
        .bind(&files)
        .fetch_all(&mut *connection)
        .await?;
        sqlx::query("DELETE FROM local_locators WHERE substr(access_uri,1,length(?1))=?1; DELETE FROM local_access_metadata WHERE substr(access_uri,1,length(?1))=?1;")
            .bind(&files).execute(&mut *connection).await?;
        sqlx::query("UPDATE connect_profile SET profile=NULL,publisher=NULL,actor='0',clock=0,enabled=0,schema_version=?1 WHERE singleton=1")
            .bind(SCHEMA_VERSION).execute(&mut *connection).await?;
        for uri in &removed {
            write.touch_download(uri);
        }
        write.keep();
        write.commit().await?;
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
    for record in changes {
        grouped
            .entry(document_name(record)?)
            .or_default()
            .push(record.clone());
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
    Ok(snapshots.len())
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
        for (position, update) in updates.into_iter().enumerate() {
            // Current library values stay with their sender while Connect sync is off.
            if update.current.is_some() {
                continue;
            }
            positions.push(position);
            authored.push(update);
        }
        for (position, result) in positions
            .into_iter()
            .zip(self.import_authored_page_on(connection, authored).await?)
        {
            changed |= result.0;
            versions[position] = result.1;
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
        let (results, snapshots) = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut results = Vec::with_capacity(updates.len());
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
            Ok((results, snapshots))
        })
        .await??;
        save_snapshots_on(connection, Some(&self.identity), &snapshots).await?;
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    const KEY: &str = "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33";

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
            crate::Catalog::open(&directory.path().join("a.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let path = directory.path().join("b.sqlite");
        let b = ProfileStore::open(
            crate::Catalog::open(&path).await.unwrap(),
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
            crate::Catalog::open(&path).await.unwrap(),
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
            crate::Catalog::open(&a_path).await.unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let b = ProfileStore::open(
            crate::Catalog::open(&directory.path().join("b.sqlite"))
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
            crate::Catalog::open(&a_path).await.unwrap(),
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
            crate::Catalog::open(&directory.path().join("a.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let b = ProfileStore::open(
            crate::Catalog::open(&directory.path().join("b.sqlite"))
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
    async fn incompatible_updates_leave_persisted_profile_ready_to_resume() {
        let directory = tempfile::tempdir().unwrap();
        let sender = ProfileStore::open(
            crate::Catalog::open(&directory.path().join("sender.sqlite"))
                .await
                .unwrap(),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let path = directory.path().join("receiver.sqlite");
        let receiver = ProfileStore::open(
            crate::Catalog::open(&path).await.unwrap(),
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
            crate::Catalog::open(&path).await.unwrap(),
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
}
