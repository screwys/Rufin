//! Loro persistence for the shared Rufin profile. Each catalog row is a separate
//! document; a playlist's occurrences share a movable list. Playback is not stored here.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use flate2::{Compression, bufread::MultiGzDecoder, write::GzEncoder};
use library::{CONNECT_PAGE_SIZE, ConnectRecord, Database};
use loro::{ExportMode, LoroDoc, ToJson, VersionVector};
use serde::{Deserialize, Serialize};
use sqlx::{Connection, QueryBuilder, Row, SqliteConnection};
use tokio::sync::{MappedMutexGuard, Mutex, MutexGuard};

mod encryption;
mod history;
use encryption::{expose, expose_payload, protect, protect_payload, sensitive};
#[cfg(test)]
mod history_tests;

// Wire format, independent of Rufin releases and local database migrations.
const FORMAT: u32 = 1;
const SYNC_PRIORITY: &str = "CASE WHEN substr(name,1,instr(name||':',':')-1) IN ('cache_artwork','cache_lyrics') THEN 2 ELSE substr(name,1,instr(name||':',':')-1) IN ('source','integration','root','preference','scrobbling','device','connect_key','connect_network','connect_storage','playlist','playlist_order','smart','smart_order','state') END";
pub const INCOMPATIBLE_PROFILE: &str =
    "Devices using Rufin Connect are no longer compatible. Please update Rufin.";
const PROJECTION_ORDER: &str = "CASE kind WHEN 'source' THEN 0 WHEN 'integration' THEN 0 WHEN 'playlist' THEN 1 WHEN 'native_playlist' THEN 1 WHEN 'album' THEN 2 WHEN 'artist' THEN 2 WHEN 'genre' THEN 2 WHEN 'mood' THEN 2 WHEN 'folder' THEN 2 WHEN 'track' THEN 3 WHEN 'entry' THEN 5 WHEN 'native_entry' THEN 5 WHEN 'playlist_order' THEN 7 WHEN 'smart_order' THEN 7 WHEN 'album_artists' THEN 6 WHEN 'album_genres' THEN 6 WHEN 'track_artists' THEN 6 WHEN 'track_genres' THEN 6 WHEN 'track_moods' THEN 6 WHEN 'track_folders' THEN 6 ELSE 4 END,CASE WHEN kind IN ('entry','native_entry') THEN json_extract(payload,'$.playlist') END,CASE WHEN kind IN ('entry','native_entry','playlist_order','smart_order') THEN json_extract(payload,'$.position') END,object_key";

pub struct ProfileStore {
    connection: Mutex<Option<SqliteConnection>>,
    reader: Mutex<Option<SqliteConnection>>,
    peer_id: u64,
    identity: Arc<age::x25519::Identity>,
}

#[derive(Serialize, Deserialize)]
struct Update {
    version: u32,
    document: String,
    bytes: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    author: Option<String>,
}

/// A document's Loro history and its revision in this installation's change index.
/// Revisions are local cursors, never Loro versions or values to compare across peers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentVersion {
    pub name: String,
    pub version: Vec<u8>,
    pub revision: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SyncStream {
    Catalog = 0,
    Settings = 1,
    Cache = 2,
}

impl From<bool> for SyncStream {
    fn from(setup: bool) -> Self {
        if setup { Self::Settings } else { Self::Catalog }
    }
}

#[derive(Clone, Copy)]
enum SnapshotScope {
    Backup,
    Device,
    Setup,
    Cache,
}

impl SnapshotScope {
    fn predicate(self) -> &'static str {
        match self {
            Self::Backup => "1",
            Self::Device => {
                "substr(name,1,instr(name,':')-1) NOT IN ('cache_artwork','cache_lyrics')"
            }
            Self::Setup => {
                "substr(name,1,instr(name,':')-1) IN ('source','integration','root','preference','scrobbling','device','connect_key','connect_network','connect_storage')"
            }
            Self::Cache => "substr(name,1,instr(name,':')-1) IN ('cache_artwork','cache_lyrics')",
        }
    }

    fn revision_query(self) -> &'static str {
        match self {
            Self::Backup => "SELECT revision FROM file_revision WHERE id=1",
            Self::Device => {
                "SELECT coalesce(sum(revision),0) FROM snapshot_revisions WHERE stream IN (0,1)"
            }
            Self::Setup => "SELECT revision FROM snapshot_revisions WHERE stream=1",
            Self::Cache => "SELECT revision FROM snapshot_revisions WHERE stream=2",
        }
    }
}

impl ProfileStore {
    pub async fn open(path: &Path, peer_id: u64, key: age::x25519::Identity) -> Result<Self> {
        let identity = Arc::new(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let mut connection = SqliteConnection::connect_with(&options).await?;
        sqlx::raw_sql("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS documents(name TEXT PRIMARY KEY,snapshot BLOB NOT NULL,revision INTEGER NOT NULL DEFAULT 0,version BLOB NOT NULL DEFAULT X'') STRICT;
            CREATE TABLE IF NOT EXISTS file_revision(id INTEGER PRIMARY KEY CHECK(id=1),revision INTEGER NOT NULL) STRICT;
            CREATE TABLE IF NOT EXISTS file_exchange(id INTEGER PRIMARY KEY CHECK(id=1),state TEXT NOT NULL) STRICT;
            CREATE TABLE IF NOT EXISTS cache_file_exchange(id INTEGER PRIMARY KEY CHECK(id=1),state TEXT NOT NULL) STRICT;
            INSERT OR IGNORE INTO file_revision VALUES(1,0);
            CREATE TRIGGER IF NOT EXISTS file_revision_delete AFTER DELETE ON documents BEGIN UPDATE file_revision SET revision=revision+1; END;
            CREATE INDEX IF NOT EXISTS documents_kind ON documents(substr(name,1,instr(name,':')-1),name);
            CREATE TABLE IF NOT EXISTS projection(kind TEXT NOT NULL,object_key TEXT NOT NULL,payload TEXT,PRIMARY KEY(kind,object_key)) STRICT;
            CREATE TABLE IF NOT EXISTS sync_cursors(peer TEXT NOT NULL,setup INTEGER NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(peer,setup)) STRICT;
            CREATE TABLE IF NOT EXISTS history_peers(peer TEXT PRIMARY KEY) STRICT;
            CREATE TABLE IF NOT EXISTS history_acknowledgements(peer TEXT NOT NULL,name TEXT NOT NULL,version BLOB NOT NULL,PRIMARY KEY(peer,name)) STRICT;
            CREATE TABLE IF NOT EXISTS history_pending(name TEXT PRIMARY KEY) STRICT;
            CREATE TABLE IF NOT EXISTS local_values(kind TEXT NOT NULL,object_key TEXT NOT NULL,payload TEXT,PRIMARY KEY(kind,object_key)) STRICT;")
            .execute(&mut connection).await?;
        index_documents(&mut connection, &identity).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP INDEX IF EXISTS projection_order; CREATE INDEX IF NOT EXISTS projection_order_native ON projection({PROJECTION_ORDER})"
        )))
        .execute(&mut connection)
        .await?;
        Ok(Self {
            connection: Mutex::new(Some(connection)),
            reader: Mutex::new(Some(
                SqliteConnection::connect_with(&options.read_only(true)).await?,
            )),
            peer_id,
            identity,
        })
    }

    pub async fn close(&self) -> Result<()> {
        let reader = self.reader.lock().await.take();
        let reader_closed = match reader {
            Some(reader) => reader.close().await,
            None => Ok(()),
        };
        let connection = self.connection.lock().await.take();
        let writer_closed = match connection {
            Some(connection) => connection.close().await,
            None => Ok(()),
        };
        reader_closed.and(writer_closed)?;
        Ok(())
    }

    pub async fn capture(&self, database: &Database) -> Result<usize> {
        let changes = database.connect_changes().await?;
        let records = changes
            .iter()
            .map(|change| change.record.clone())
            .collect::<Vec<_>>();
        self.write_records(&records).await?;
        database.connect_acknowledge(&changes).await?;
        Ok(changes.len())
    }

    pub async fn device_name(&self, peer: &str) -> Result<Option<String>> {
        let mut connection = database_connection(&self.connection).await?;
        Ok(sqlx::query_scalar("SELECT json_extract(payload,'$') FROM local_values WHERE kind='device' AND object_key=?1 AND json_type(payload)='text'")
            .bind(peer).fetch_optional(&mut *connection).await?)
    }

    pub async fn revision(&self) -> Result<i64> {
        let mut connection = database_connection(&self.connection).await?;
        Ok(
            sqlx::query_scalar("SELECT revision FROM file_revision WHERE id=1")
                .fetch_one(&mut *connection)
                .await?,
        )
    }

    pub async fn device_revision(&self) -> Result<i64> {
        let mut connection = database_connection(&self.connection).await?;
        Ok(sqlx::query_scalar(SnapshotScope::Device.revision_query())
            .fetch_one(&mut *connection)
            .await?)
    }

    pub async fn cache_revision(&self) -> Result<i64> {
        let mut connection = database_connection(&self.connection).await?;
        Ok(sqlx::query_scalar(SnapshotScope::Cache.revision_query())
            .fetch_one(&mut *connection)
            .await?)
    }

    pub async fn file_exchange(&self) -> Result<Option<String>> {
        let mut connection = database_connection(&self.connection).await?;
        Ok(
            sqlx::query_scalar("SELECT state FROM file_exchange WHERE id=1")
                .fetch_optional(&mut *connection)
                .await?,
        )
    }

    pub async fn save_file_exchange(&self, state: &str) -> Result<()> {
        let mut connection = database_connection(&self.connection).await?;
        sqlx::query("INSERT INTO file_exchange(id,state) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET state=excluded.state WHERE state<>excluded.state")
            .bind(state).execute(&mut *connection).await?;
        Ok(())
    }

    pub async fn cache_file_exchange(&self) -> Result<Option<String>> {
        let mut connection = database_connection(&self.connection).await?;
        Ok(
            sqlx::query_scalar("SELECT state FROM cache_file_exchange WHERE id=1")
                .fetch_optional(&mut *connection)
                .await?,
        )
    }

    pub async fn save_cache_file_exchange(&self, state: &str) -> Result<()> {
        let mut connection = database_connection(&self.connection).await?;
        sqlx::query("INSERT INTO cache_file_exchange(id,state) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET state=excluded.state WHERE state<>excluded.state")
            .bind(state).execute(&mut *connection).await?;
        Ok(())
    }

    /// Resume this peer's incoming stream after its last fully imported page.
    pub async fn sync_cursor(&self, peer: &str, setup: bool) -> Result<i64> {
        self.sync_cursor_in(peer, setup.into()).await
    }

    pub async fn sync_cursor_in(&self, peer: &str, stream: SyncStream) -> Result<i64> {
        let mut connection = database_connection(&self.connection).await?;
        Ok(
            sqlx::query_scalar("SELECT revision FROM sync_cursors WHERE peer=?1 AND setup=?2")
                .bind(peer)
                .bind(stream as i64)
                .fetch_optional(&mut *connection)
                .await?
                .unwrap_or(0),
        )
    }

    /// Persist only after every document in the page has been imported.
    pub async fn acknowledge_sync(&self, peer: &str, setup: bool, revision: i64) -> Result<()> {
        self.acknowledge_sync_in(peer, setup.into(), revision).await
    }

    pub async fn acknowledge_sync_in(
        &self,
        peer: &str,
        stream: SyncStream,
        revision: i64,
    ) -> Result<()> {
        let mut connection = database_connection(&self.connection).await?;
        sqlx::query("INSERT INTO sync_cursors(peer,setup,revision) VALUES(?1,?2,?3) ON CONFLICT(peer,setup) DO UPDATE SET revision=excluded.revision")
            .bind(peer).bind(stream as i64).bind(revision).execute(&mut *connection).await?;
        Ok(())
    }

    /// Read a bounded page of changed documents without loading their snapshots.
    /// Resume after the last returned revision. Repeated edits replace a document's
    /// index entry; their complete Loro history remains available through `updates`.
    pub async fn changes(&self, after: i64, limit: usize) -> Result<Vec<DocumentVersion>> {
        let mut connection = database_connection(&self.connection).await?;
        Ok(sqlx::query("SELECT name,version,revision FROM documents WHERE revision>?1 AND revision<=coalesce((SELECT min(revision) FROM documents WHERE revision>?1 AND (name GLOB 'playlist_artwork:*' OR name GLOB 'cache_artwork:*')),9223372036854775807) ORDER BY revision,name LIMIT ?2")
            .bind(after)
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .fetch_all(&mut *connection).await?
            .into_iter().map(|row| DocumentVersion {
                name: row.get(0), version: row.get(1), revision: row.get(2),
            }).collect())
    }

    /// Page settings and user edits separately from catalog ingestion. Each stream
    /// has its own local revision cursor so catalog catch-up cannot delay controls.
    pub async fn changes_in(
        &self,
        after: i64,
        limit: usize,
        setup: bool,
    ) -> Result<Vec<DocumentVersion>> {
        self.changes_in_stream(after, limit, setup.into()).await
    }

    pub async fn changes_in_stream(
        &self,
        after: i64,
        limit: usize,
        stream: SyncStream,
    ) -> Result<Vec<DocumentVersion>> {
        let mut connection = database_connection(&self.connection).await?;
        Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT name,version,revision FROM documents WHERE ({SYNC_PRIORITY})=?1 AND revision>?2 AND revision<=coalesce((SELECT min(revision) FROM documents WHERE ({SYNC_PRIORITY})=?1 AND revision>?2 AND (name GLOB 'playlist_artwork:*' OR name GLOB 'cache_artwork:*')),9223372036854775807) ORDER BY revision,name LIMIT ?3"
        )))
        .bind(stream as i64).bind(after).bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&mut *connection).await?
        .into_iter().map(|row| DocumentVersion {
            name: row.get(0), version: row.get(1), revision: row.get(2),
        }).collect())
    }

    /// Return versions in the requested order. Missing documents have an encoded
    /// empty version vector and revision zero, allowing their full history to sync.
    pub async fn versions(&self, names: &[String]) -> Result<Vec<DocumentVersion>> {
        let mut connection = database_connection(&self.connection).await?;
        let mut versions = Vec::with_capacity(names.len());
        for name in names {
            let row = sqlx::query("SELECT version,revision FROM documents WHERE name=?1")
                .bind(name)
                .fetch_optional(&mut *connection)
                .await?;
            let (version, revision) = match row {
                Some(row) => (row.get(0), row.get(1)),
                None => (VersionVector::default().encode(), 0),
            };
            versions.push(DocumentVersion {
                name: name.clone(),
                version,
                revision,
            });
        }
        Ok(versions)
    }

    /// Export only operations absent from the supplied remote version vectors.
    /// Unknown documents and histories the remote already contains produce no update.
    pub async fn updates(&self, versions: &[DocumentVersion]) -> Result<Vec<Vec<u8>>> {
        let mut connection = database_connection(&self.connection).await?;
        let mut updates = Vec::new();
        for version in versions {
            let remote = VersionVector::decode(&version.version)?;
            let local: Option<Vec<u8>> =
                sqlx::query_scalar("SELECT version FROM documents WHERE name=?1")
                    .bind(&version.name)
                    .fetch_optional(&mut *connection)
                    .await?;
            let Some(local) = local else { continue };
            if remote.includes_vv(&VersionVector::decode(&local)?) {
                continue;
            }
            let bytes = if remote.is_empty() {
                let bytes: Vec<u8> =
                    sqlx::query_scalar("SELECT snapshot FROM documents WHERE name=?1")
                        .bind(&version.name)
                        .fetch_one(&mut *connection)
                        .await?;
                if sensitive(&version.name) {
                    expose(&version.name, &bytes, &self.identity)?.into_owned()
                } else {
                    bytes
                }
            } else {
                let document = self.load_document(&mut connection, &version.name).await?;
                document.export(ExportMode::updates(&remote))?
            };
            updates.push(serde_json::to_vec(&Update {
                version: FORMAT,
                document: version.name.clone(),
                bytes,
                author: None,
            })?);
        }
        Ok(updates)
    }

    /// Import a page atomically. Returns whether projected values changed, so
    /// repeated or history-only imports do not cause a Store or UI refresh.
    pub async fn import_updates(&self, updates: &[Vec<u8>]) -> Result<bool> {
        let mut connection = database_connection(&self.connection).await?;
        let mut transaction = connection.begin().await?;
        let mut changed = false;
        for page in updates.chunks(CONNECT_PAGE_SIZE) {
            let page = page
                .iter()
                .map(|bytes| {
                    let update: Update =
                        serde_json::from_slice(bytes).context("invalid Connect update")?;
                    validate_version(update.version)?;
                    Ok(update)
                })
                .collect::<Result<Vec<_>>>()?;
            changed |= self
                .import_page_on(&mut transaction, page)
                .await?
                .iter()
                .any(|(changed, _)| *changed);
        }
        transaction.commit().await?;
        Ok(changed)
    }

    /// Call with a coherent source configuration and credential as one record.
    /// Equal values are no-ops even when a caller polls its settings owner.
    pub async fn write_records(&self, records: &[ConnectRecord]) -> Result<usize> {
        let mut ordered = records.to_vec();
        for record in records {
            if matches!(record.kind.as_str(), "playlist" | "smart") {
                ordered.push(ConnectRecord {
                    kind: format!("{}_order", record.kind),
                    key: record.key.clone(),
                    value: record
                        .value
                        .as_ref()
                        .map(|v| serde_json::json!({"position":v["position"]})),
                });
            }
        }
        let mut connection = database_connection(&self.connection).await?;
        let mut transaction = connection.begin().await?;
        let mut changed_documents = BTreeSet::new();
        for records in ordered.chunks(CONNECT_PAGE_SIZE) {
            let mut query = QueryBuilder::new("WITH requested(kind,object_key) AS (");
            query.push_values(records, |mut row, record| {
                row.push_bind(&record.kind).push_bind(&record.key);
            });
            query.push(") SELECT v.kind,v.object_key,v.payload FROM local_values v JOIN requested r USING(kind,object_key)");
            let mut previous_values = BTreeMap::new();
            for row in query.build().fetch_all(&mut *transaction).await? {
                let kind: String = row.get(0);
                let payload = expose_payload(&kind, row.get(2), &self.identity)?;
                previous_values.insert((kind, row.get::<String, _>(1)), payload);
            }
            let mut grouped: BTreeMap<String, Vec<&ConnectRecord>> = BTreeMap::new();
            let mut values = BTreeMap::new();
            for record in records {
                let key = (record.kind.clone(), record.key.clone());
                let old = previous_values.get(&key);
                let mut value = record.value.clone();
                if let (Some(value), Some(Some(old))) = (&mut value, old) {
                    preserve_unknown_fields(value, &serde_json::from_str(old)?);
                }
                let payload = value.as_ref().map(serde_json::to_string).transpose()?;
                if old == Some(&payload) {
                    continue;
                }
                grouped
                    .entry(document_name(record)?)
                    .or_default()
                    .push(record);
                previous_values.insert(key.clone(), payload.clone());
                let payload = protect_payload(&record.kind, payload, &self.identity)?;
                values.insert(key, payload);
            }
            if grouped.is_empty() {
                continue;
            }
            let mut insert =
                QueryBuilder::new("INSERT INTO local_values(kind,object_key,payload) ");
            insert.push_values(values, |mut row, ((kind, key), payload)| {
                row.push_bind(kind).push_bind(key).push_bind(payload);
            });
            insert.push(" ON CONFLICT(kind,object_key) DO UPDATE SET payload=excluded.payload");
            insert.build().execute(&mut *transaction).await?;
            let mut query =
                QueryBuilder::new("SELECT name,snapshot FROM documents WHERE name IN (");
            let mut names = query.separated(",");
            for name in grouped.keys() {
                names.push_bind(name);
            }
            names.push_unseparated(")");
            let mut saved: BTreeMap<String, Vec<u8>> = query
                .build()
                .fetch_all(&mut *transaction)
                .await?
                .into_iter()
                .map(|row| (row.get(0), row.get(1)))
                .collect();
            let mut snapshots = Vec::new();
            let mut projection = Vec::new();
            for (name, records) in grouped {
                changed_documents.insert(name.clone());
                let doc = LoroDoc::new();
                if let Some(snapshot) = saved.remove(&name) {
                    doc.import(&expose(&name, &snapshot, &self.identity)?)?;
                }
                doc.set_peer_id(self.peer_id)?;
                let before = doc.oplog_vv();
                let previous = self::records(&doc)?;
                for record in records {
                    edit(&doc, record)?;
                }
                doc.commit();
                if before == doc.oplog_vv() {
                    continue;
                }
                let current = self::records(&doc)?;
                // An incoming update may already be waiting for Store projection.
                // Refresh those queued values so capturing a newer local edit cannot
                // subsequently install the older incoming value over it.
                for key in previous
                    .keys()
                    .chain(current.keys())
                    .collect::<BTreeSet<_>>()
                {
                    if previous.get(key) == current.get(key) {
                        continue;
                    }
                    let payload = current.get(key).map(serde_json::to_string).transpose()?;
                    let payload = protect_payload(&key.0, payload, &self.identity)?;
                    projection.push((key.0.clone(), key.1.clone(), payload));
                }
                let removed_device = name.starts_with("device:") && current.is_empty();
                snapshots.push((
                    name,
                    doc.export(ExportMode::Snapshot)?,
                    doc.oplog_vv().encode(),
                    removed_device,
                ));
            }
            for page in projection.chunks(CONNECT_PAGE_SIZE) {
                let mut update = QueryBuilder::new("WITH updates(kind,object_key,payload) AS (");
                update.push_values(page, |mut row, (kind, key, payload)| {
                    row.push_bind(kind).push_bind(key).push_bind(payload);
                });
                update.push(") UPDATE projection SET payload=updates.payload FROM updates WHERE projection.kind=updates.kind AND projection.object_key=updates.object_key");
                update.build().execute(&mut *transaction).await?;
            }
            self.save_snapshots(&mut transaction, &snapshots).await?;
        }
        transaction.commit().await?;
        Ok(changed_documents.len())
    }

    /// Sources can be removed. Missing preference keys may belong to a newer device.
    pub async fn write_settings(&self, records: &[ConnectRecord]) -> Result<usize> {
        let keys = records
            .iter()
            .map(|r| (r.kind.clone(), r.key.clone()))
            .collect::<BTreeSet<_>>();
        let mut records = records.to_vec();
        {
            let mut connection = database_connection(&self.connection).await?;
            let rows=sqlx::query("SELECT kind,object_key FROM local_values WHERE kind IN ('source','integration') AND payload IS NOT NULL").fetch_all(&mut *connection).await?;
            for row in rows {
                let kind: String = row.get(0);
                let key: String = row.get(1);
                if !keys.contains(&(kind.clone(), key.clone())) {
                    records.push(ConnectRecord {
                        kind,
                        key,
                        value: None,
                    });
                }
            }
        }
        self.write_records(&records).await
    }

    pub async fn import(&self, bytes: &[u8]) -> Result<bool> {
        let update: Update = serde_json::from_slice(bytes).context("invalid Connect update")?;
        validate_version(update.version)?;
        let mut connection = database_connection(&self.connection).await?;
        let mut transaction = connection.begin().await?;
        let changed = self.import_page_on(&mut transaction, vec![update]).await?[0].0;
        transaction.commit().await?;
        Ok(changed)
    }

    /// Applies one bounded Store page, returning its records and whether the library
    /// changed. Acknowledge only after settings/device owners persist their changes too.
    pub async fn project(&self, database: &Database) -> Result<(Vec<ConnectRecord>, bool)> {
        let records = {
            let mut connection = database_connection(&self.connection).await?;
            let rows=sqlx::query(sqlx::AssertSqlSafe(format!("SELECT kind,object_key,payload FROM projection WHERE kind NOT IN ('cache_artwork','cache_lyrics') AND (kind!='playlist_artwork' OR NOT EXISTS(SELECT 1 FROM projection WHERE kind NOT IN ('playlist_artwork','cache_artwork','cache_lyrics'))) ORDER BY {PROJECTION_ORDER} LIMIT CASE WHEN EXISTS(SELECT 1 FROM projection WHERE kind NOT IN ('playlist_artwork','cache_artwork','cache_lyrics')) THEN ?1 ELSE 1 END"))).bind(CONNECT_PAGE_SIZE as i64).fetch_all(&mut *connection).await?;
            rows.into_iter()
                .map(|row| {
                    let kind: String = row.get(0);
                    Ok(ConnectRecord {
                        value: expose_payload(&kind, row.get(2), &self.identity)?
                            .map(|s| serde_json::from_str(&s))
                            .transpose()?,
                        kind,
                        key: row.get(1),
                    })
                })
                .collect::<Result<Vec<_>>>()?
        };
        match database.connect_apply(&records).await {
            Ok(changed) => Ok((records, changed)),
            Err(library::LibraryError::ConnectPending) => Ok((Vec::new(), false)),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn projection_pending(&self) -> Result<bool> {
        let mut connection = database_connection(&self.connection).await?;
        Ok(
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projection WHERE kind NOT IN ('cache_artwork','cache_lyrics'))")
                .fetch_one(&mut *connection)
                .await?,
        )
    }

    /// Cache owners apply their own bounded pages without delaying profile settings.
    pub async fn project_cache(&self, database: &Database) -> Result<(Vec<ConnectRecord>, bool)> {
        let records = {
            let mut connection = database_connection(&self.connection).await?;
            let mut rows = sqlx::query("SELECT kind,object_key,payload FROM projection WHERE kind='cache_lyrics' ORDER BY object_key LIMIT ?1")
                .bind(CONNECT_PAGE_SIZE as i64)
                .fetch_all(&mut *connection).await?;
            if rows.is_empty() {
                rows = sqlx::query("SELECT kind,object_key,payload FROM projection WHERE kind='cache_artwork' ORDER BY object_key LIMIT 1")
                    .fetch_all(&mut *connection).await?;
            }
            rows.into_iter()
                .map(|row| {
                    Ok(ConnectRecord {
                        kind: row.get(0),
                        key: row.get(1),
                        value: row
                            .get::<Option<String>, _>(2)
                            .map(|payload| serde_json::from_str(&payload))
                            .transpose()?,
                    })
                })
                .collect::<Result<Vec<_>>>()?
        };
        let changed = database.connect_apply(&records).await?;
        Ok((records, changed))
    }

    pub async fn cache_projection_pending(&self) -> Result<bool> {
        let mut connection = database_connection(&self.connection).await?;
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projection WHERE kind IN ('cache_artwork','cache_lyrics'))")
            .fetch_one(&mut *connection).await?)
    }

    /// Page current cache records by their document name, without reading histories.
    pub async fn cache_records(&self, after: &str, limit: usize) -> Result<Vec<ConnectRecord>> {
        let mut connection = database_connection(&self.connection).await?;
        let (kind, key) = after.split_once(':').unwrap_or(("", ""));
        let rows = sqlx::query("SELECT kind,object_key,payload FROM local_values WHERE kind IN ('cache_artwork','cache_lyrics') AND (kind,object_key)>(?1,?2) AND payload IS NOT NULL ORDER BY kind,object_key LIMIT ?3")
            .bind(kind).bind(key).bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .fetch_all(&mut *connection).await?;
        rows.into_iter()
            .map(|row| {
                Ok(ConnectRecord {
                    kind: row.get(0),
                    key: row.get(1),
                    value: Some(serde_json::from_str(row.get(2))?),
                })
            })
            .collect()
    }

    pub async fn cache_record(&self, kind: &str, key: &str) -> Result<Option<ConnectRecord>> {
        let mut connection = database_connection(&self.connection).await?;
        let row = sqlx::query("SELECT payload FROM projection WHERE kind=?1 AND object_key=?2 UNION ALL SELECT payload FROM local_values WHERE kind=?1 AND object_key=?2 LIMIT 1")
            .bind(kind)
            .bind(key)
            .fetch_optional(&mut *connection)
            .await?;
        row.map(|row| {
            Ok(ConnectRecord {
                kind: kind.into(),
                key: key.into(),
                value: row
                    .get::<Option<String>, _>(0)
                    .map(|payload| serde_json::from_str(&payload))
                    .transpose()?,
            })
        })
        .transpose()
    }

    /// Read artwork identities without loading image payloads during cleanup.
    pub async fn cache_keys(&self, after: &str, limit: usize) -> Result<Vec<String>> {
        let mut connection = database_connection(&self.connection).await?;
        Ok(sqlx::query_scalar("SELECT object_key FROM local_values v WHERE kind='cache_artwork' AND object_key>?1 AND payload IS NOT NULL AND NOT EXISTS(SELECT 1 FROM projection p WHERE p.kind=v.kind AND p.object_key=v.object_key) ORDER BY object_key LIMIT ?2")
            .bind(after).bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .fetch_all(&mut *connection).await?)
    }

    pub async fn acknowledge_projection(&self, records: &[ConnectRecord]) -> Result<()> {
        let mut connection = database_connection(&self.connection).await?;
        let mut transaction = connection.begin().await?;
        for page in records.chunks(CONNECT_PAGE_SIZE) {
            let protected = if page
                .iter()
                .any(|record| sensitive(&record.kind) && record.value.is_some())
            {
                let mut query = QueryBuilder::new("WITH requested(kind,object_key) AS (");
                query.push_values(
                    page.iter()
                        .filter(|record| sensitive(&record.kind) && record.value.is_some()),
                    |mut row, record| {
                        row.push_bind(&record.kind).push_bind(&record.key);
                    },
                );
                query.push(") SELECT p.kind,p.object_key,p.payload FROM projection p JOIN requested r USING(kind,object_key)");
                query
                    .build()
                    .fetch_all(&mut *transaction)
                    .await?
                    .into_iter()
                    .map(|row| {
                        (
                            (row.get::<String, _>(0), row.get::<String, _>(1)),
                            row.get::<Option<String>, _>(2),
                        )
                    })
                    .collect::<BTreeMap<_, _>>()
            } else {
                BTreeMap::new()
            };
            let mut acknowledged = Vec::new();
            let mut values = Vec::new();
            for record in page {
                let payload = record
                    .value
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?;
                let projected = if sensitive(&record.kind) && payload.is_some() {
                    protected
                        .get(&(record.kind.clone(), record.key.clone()))
                        .cloned()
                        .flatten()
                } else {
                    payload.clone()
                };
                if expose_payload(&record.kind, projected.clone(), &self.identity)? == payload {
                    acknowledged.push((&record.kind, &record.key, projected));
                }
                // Polling the local owners after an incoming edit must not republish it.
                let payload = protect_payload(&record.kind, payload, &self.identity)?;
                values.push((&record.kind, &record.key, payload));
            }
            if !acknowledged.is_empty() {
                let mut delete =
                    QueryBuilder::new("WITH acknowledged(kind,object_key,payload) AS (");
                delete.push_values(acknowledged, |mut row, (kind, key, payload)| {
                    row.push_bind(kind).push_bind(key).push_bind(payload);
                });
                delete.push(") DELETE FROM projection WHERE (kind,object_key) IN (SELECT a.kind,a.object_key FROM acknowledged a JOIN projection p ON p.kind=a.kind AND p.object_key=a.object_key AND p.payload IS a.payload)");
                delete.build().execute(&mut *transaction).await?;
            }
            let mut insert =
                QueryBuilder::new("INSERT INTO local_values(kind,object_key,payload) ");
            insert.push_values(values, |mut row, (kind, key, payload)| {
                row.push_bind(kind).push_bind(key).push_bind(payload);
            });
            insert.push(" ON CONFLICT(kind,object_key) DO UPDATE SET payload=excluded.payload");
            insert.build().execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    /// Stream current state and the history still needed by enrolled devices.
    pub async fn export_snapshot(&self, path: &Path) -> Result<()> {
        let (file, _) = self
            .export_documents(File::create(path)?, SnapshotScope::Backup, None)
            .await?;
        file.sync_all()?;
        Ok(())
    }

    /// The caller appends membership after exporting, before publishing the file.
    /// Returns the revision included in the snapshot.
    pub async fn export_device_snapshot<W: Write + Send + 'static>(
        &self,
        output: W,
        peer: &str,
    ) -> Result<(W, i64)> {
        self.export_documents(output, SnapshotScope::Device, Some(peer))
            .await
    }

    pub async fn export_setup_snapshot<W: Write + Send + 'static>(
        &self,
        output: W,
    ) -> Result<(W, i64)> {
        self.export_documents(output, SnapshotScope::Setup, None)
            .await
    }

    pub async fn export_cache_snapshot<W: Write + Send + 'static>(
        &self,
        output: W,
        peer: &str,
    ) -> Result<(W, i64)> {
        self.export_documents(output, SnapshotScope::Cache, Some(peer))
            .await
    }

    async fn export_documents<W: Write + Send + 'static>(
        &self,
        file: W,
        scope: SnapshotScope,
        peer: Option<&str>,
    ) -> Result<(W, i64)> {
        self.compress_documents(scope).await?;
        let mut output = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut output = BufWriter::new(file);
            let mut header = GzEncoder::new(&mut output, Compression::default());
            writeln!(header, "{FORMAT}")?;
            header.finish()?;
            Ok(output)
        })
        .await??;
        let mut author = peer.map(str::to_owned);
        // A WAL read transaction gives the file a consistent snapshot without
        // holding the writer connection throughout a full catalog export.
        let mut connection = database_connection(&self.reader).await?;
        let mut snapshot = connection.begin().await?;
        let revision = sqlx::query_scalar(scope.revision_query())
            .fetch_one(&mut *snapshot)
            .await?;
        let mut cursor = String::new();
        loop {
            let predicate = scope.predicate();
            let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT name,CASE WHEN compressed IS NULL THEN snapshot END,compressed FROM documents WHERE {predicate} AND name>?1 AND name<=coalesce((SELECT min(name) FROM documents WHERE {predicate} AND name>?1 AND (name GLOB 'playlist_artwork:*' OR name GLOB 'cache_artwork:*')),char(1114111)) ORDER BY name LIMIT ?2"
            )))
                .bind(&cursor)
                .bind(CONNECT_PAGE_SIZE as i64)
                .fetch_all(&mut *snapshot).await?;
            if rows.is_empty() {
                break;
            }
            cursor = rows.last().unwrap().get(0);
            // Reuse unchanged gzip members. Concatenating them preserves the
            // portable snapshot stream without serializing the catalog again.
            let mut author = author.take();
            let identity = Arc::clone(&self.identity);
            output = tokio::task::spawn_blocking(move || -> Result<_> {
                for row in rows {
                    let name: &str = row.get(0);
                    let cached = row
                        .get::<Option<&[u8]>, _>(2)
                        .map(|bytes| expose(name, bytes, &identity))
                        .transpose()?;
                    if author.is_none()
                        && let Some(cached) = &cached
                    {
                        output.write_all(cached)?;
                        continue;
                    }
                    let mut update = match cached {
                        Some(cached) => {
                            serde_json::from_reader(MultiGzDecoder::new(cached.as_ref()))?
                        }
                        None => Update {
                            version: FORMAT,
                            document: row.get(0),
                            bytes: expose(name, row.get(1), &identity)?.into_owned(),
                            author: None,
                        },
                    };
                    update.author = author.take();
                    let mut compressed = GzEncoder::new(Vec::new(), Compression::default());
                    serde_json::to_writer(&mut compressed, &update)?;
                    compressed.write_all(b"\n")?;
                    let compressed = compressed.finish()?;
                    output.write_all(&compressed)?;
                }
                Ok(output)
            })
            .await??;
        }
        let output = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut end = GzEncoder::new(&mut output, Compression::default());
            writeln!(end, "END")?;
            end.finish()?;
            output.flush()?;
            Ok(output.into_inner().map_err(|error| error.into_error())?)
        })
        .await??;
        snapshot.commit().await?;
        Ok((output, revision))
    }

    // Fill the cache before pinning the export's read snapshot. Writing compressed
    // copies during that snapshot prevents WAL checkpoints until the export ends.
    async fn compress_documents(&self, scope: SnapshotScope) -> Result<()> {
        let mut cursor = String::new();
        loop {
            let rows = {
                let mut connection = database_connection(&self.connection).await?;
                let predicate = scope.predicate();
                sqlx::query(sqlx::AssertSqlSafe(format!("SELECT name,snapshot FROM documents WHERE compressed IS NULL AND {predicate} AND name>?1 AND name<=coalesce((SELECT min(name) FROM documents WHERE compressed IS NULL AND {predicate} AND name>?1 AND (name GLOB 'playlist_artwork:*' OR name GLOB 'cache_artwork:*')),char(1114111)) ORDER BY name LIMIT ?2")))
                    .bind(&cursor).bind(CONNECT_PAGE_SIZE as i64)
                    .fetch_all(&mut *connection).await?
            };
            if rows.is_empty() {
                break;
            }
            cursor = rows.last().unwrap().get(0);
            let identity = Arc::clone(&self.identity);
            let encoded = tokio::task::spawn_blocking(move || -> Result<_> {
                rows.into_iter()
                    .map(|row| {
                        let name: String = row.get(0);
                        let update = Update {
                            version: FORMAT,
                            bytes: expose(&name, row.get(1), &identity)?.into_owned(),
                            document: name,
                            author: None,
                        };
                        let mut compressed = GzEncoder::new(Vec::new(), Compression::default());
                        serde_json::to_writer(&mut compressed, &update)?;
                        compressed.write_all(b"\n")?;
                        let compressed = compressed.finish()?;
                        let compressed =
                            protect(&update.document, &compressed, &identity)?.into_owned();
                        let stored_snapshot = if sensitive(&update.document) {
                            row.get(1)
                        } else {
                            update.bytes
                        };
                        Ok((update.document, stored_snapshot, compressed))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .await??;
            let mut connection = database_connection(&self.connection).await?;
            let mut transaction = connection.begin().await?;
            let mut update = QueryBuilder::new("WITH updates(name,snapshot,compressed) AS (");
            update.push_values(encoded, |mut row, (name, snapshot, compressed)| {
                row.push_bind(name)
                    .push_bind(snapshot)
                    .push_bind(compressed);
            });
            update.push(") UPDATE documents SET compressed=updates.compressed FROM updates WHERE documents.name=updates.name AND documents.snapshot=updates.snapshot");
            update.build().execute(&mut *transaction).await?;
            transaction.commit().await?;
        }
        Ok(())
    }

    /// Every document is validated inside one SQLite transaction before any pending
    /// projection becomes visible. A corrupt/truncated snapshot preserves usable state.
    pub async fn import_snapshot<R: Read + Send + 'static>(&self, input: R) -> Result<bool> {
        self.read_snapshot(input, false).await
    }

    /// Each cache page commits independently so a large file does not hold up settings.
    pub async fn import_cache_snapshot<R: Read + Send + 'static>(&self, file: R) -> Result<bool> {
        let mut input = tokio::task::spawn_blocking(move || snapshot_start(file)).await??;
        let mut peer = None;
        let mut changed = false;
        loop {
            let (reader, page, ended) =
                tokio::task::spawn_blocking(move || read_update_page(input)).await??;
            input = reader;
            if !page.is_empty() {
                let mut acknowledged = Vec::with_capacity(page.len());
                for update in &page {
                    if !update.document.starts_with("cache_artwork:")
                        && !update.document.starts_with("cache_lyrics:")
                    {
                        bail!("Connect cache snapshot contains another document kind");
                    }
                    if let Some(author) = &update.author {
                        peer = Some(author.clone());
                    }
                    acknowledged.push((peer.clone(), update.document.clone()));
                }
                let mut connection = database_connection(&self.connection).await?;
                let mut transaction = connection.begin().await?;
                let versions = self.import_page_on(&mut transaction, page).await?;
                let acknowledged = acknowledged
                    .into_iter()
                    .zip(versions)
                    .filter_map(|((peer, name), (imported, version))| {
                        changed |= imported;
                        peer.map(|peer| (peer, name, version))
                    })
                    .collect::<Vec<_>>();
                history::acknowledge_page_on(&mut transaction, &acknowledged).await?;
                transaction.commit().await?;
            }
            if ended {
                break;
            }
            tokio::task::yield_now().await;
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
            std::io::copy(&mut input, &mut std::io::sink())?;
            Ok(members)
        })
        .await??;
        if let Some(peer) = peer {
            members.push(peer);
            let mut connection = database_connection(&self.connection).await?;
            let mut transaction = connection.begin().await?;
            history::register_on(&mut transaction, &members).await?;
            transaction.commit().await?;
        }
        Ok(changed)
    }

    /// Check an imported snapshot for local edits it does not contain. Exports
    /// order documents by name, so both histories can be compared a page at a time.
    pub async fn snapshot_contains_current<R: Read + Send + 'static>(
        &self,
        file: R,
    ) -> Result<bool> {
        self.snapshot_contains_scope(file, SnapshotScope::Backup)
            .await
    }

    pub async fn device_snapshot_contains_current<R: Read + Send + 'static>(
        &self,
        file: R,
    ) -> Result<bool> {
        self.snapshot_contains_scope(file, SnapshotScope::Device)
            .await
    }

    pub async fn cache_snapshot_contains_current<R: Read + Send + 'static>(
        &self,
        file: R,
    ) -> Result<bool> {
        self.snapshot_contains_scope(file, SnapshotScope::Cache)
            .await
    }

    async fn snapshot_contains_scope<R: Read + Send + 'static>(
        &self,
        file: R,
        scope: SnapshotScope,
    ) -> Result<bool> {
        let (mut input, mut line) = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut input = snapshot_reader(file)?;
            let mut line = String::new();
            input.read_line(&mut line)?;
            Ok((input, line))
        })
        .await??;
        let mut remote: Option<Update> = None;
        let mut connection = database_connection(&self.reader).await?;
        let mut transaction = connection.begin().await?;
        let mut cursor = String::new();
        loop {
            let predicate = scope.predicate();
            let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT name,version,snapshot FROM documents WHERE {predicate} AND name>?1 AND name<=coalesce((SELECT min(name) FROM documents WHERE {predicate} AND name>?1 AND (name GLOB 'playlist_artwork:*' OR name GLOB 'cache_artwork:*')),char(1114111)) ORDER BY name LIMIT ?2"
            )))
            .bind(&cursor)
            .bind(CONNECT_PAGE_SIZE as i64)
            .fetch_all(&mut *transaction)
            .await?;
            if rows.is_empty() {
                return Ok(true);
            }
            cursor = rows.last().unwrap().get(0);
            let identity = Arc::clone(&self.identity);
            let compared = tokio::task::spawn_blocking(move || -> Result<_> {
                for row in rows {
                    let cursor: String = row.get(0);
                    while remote
                        .as_ref()
                        .is_none_or(|update| update.document < cursor)
                    {
                        line.clear();
                        if input.read_line(&mut line)? == 0 || line.trim_end() == "END" {
                            return Ok(None);
                        }
                        remote = Some(serde_json::from_str(&line)?);
                    }
                    let update = remote.as_ref().unwrap();
                    if update.document != cursor {
                        return Ok(None);
                    }
                    let saved = expose(&cursor, row.get(2), &identity)?;
                    if update.bytes == saved.as_ref() {
                        continue;
                    }
                    let incoming = LoroDoc::decode_import_blob_meta(&update.bytes, true)?;
                    let local = VersionVector::decode(&row.get::<Vec<u8>, _>(1))?;
                    if !incoming.partial_end_vv.includes_vv(&local) {
                        return Ok(None);
                    }
                    let saved = LoroDoc::decode_import_blob_meta(&saved, true)?;
                    if !incoming
                        .partial_start_vv
                        .includes_vv(&saved.partial_start_vv)
                    {
                        return Ok(None);
                    }
                }
                Ok(Some((input, line, remote)))
            })
            .await??;
            let Some(state) = compared else {
                return Ok(false);
            };
            (input, line, remote) = state;
        }
    }

    pub async fn replace_snapshot<R: Read + Send + 'static>(&self, input: R) -> Result<bool> {
        self.read_snapshot(input, true).await
    }

    async fn read_snapshot<R: Read + Send + 'static>(
        &self,
        file: R,
        replace: bool,
    ) -> Result<bool> {
        let mut input = tokio::task::spawn_blocking(move || snapshot_start(file)).await??;
        let mut peer = None;
        let mut connection = database_connection(&self.connection).await?;
        let mut transaction = connection.begin().await?;
        if replace {
            sqlx::raw_sql(
                "DELETE FROM documents; DELETE FROM projection; DELETE FROM local_values; DELETE FROM sync_cursors; DELETE FROM file_exchange; DELETE FROM cache_file_exchange; DELETE FROM history_peers; DELETE FROM history_acknowledgements; DELETE FROM history_pending;",
            )
            .execute(&mut *transaction)
            .await?;
        }
        let mut changed = false;
        loop {
            // Only file and CPU work moves to the blocking pool. The caller
            // retains the transaction so cancellation still rolls back the import.
            let (reader, page, ended) =
                tokio::task::spawn_blocking(move || read_update_page(input)).await??;
            input = reader;
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
                let versions = self.import_page_on(&mut transaction, updates).await?;
                let acknowledgements = acknowledgements
                    .into_iter()
                    .zip(versions)
                    .filter_map(|((peer, name), (imported, version))| {
                        changed |= imported;
                        peer.map(|peer| (peer, name, version))
                    })
                    .collect::<Vec<_>>();
                history::acknowledge_page_on(&mut transaction, &acknowledgements).await?;
            }
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

    /// Used only after the user confirms replacing this installation's old profile.
    pub async fn reproject_all(&self) -> Result<()> {
        let mut connection = database_connection(&self.connection).await?;
        let mut transaction = connection.begin().await?;
        let mut cursor = String::new();
        loop {
            let rows = sqlx::query(
                "SELECT name,snapshot FROM documents WHERE name>?1 AND name<=coalesce((SELECT min(name) FROM documents WHERE name>?1 AND (name GLOB 'playlist_artwork:*' OR name GLOB 'cache_artwork:*')),char(1114111)) ORDER BY name LIMIT ?2",
            )
            .bind(&cursor)
            .bind(CONNECT_PAGE_SIZE as i64)
            .fetch_all(&mut *transaction)
            .await?;
            if rows.is_empty() {
                break;
            }
            let mut projection = Vec::new();
            for row in rows {
                cursor = row.get(0);
                let document = LoroDoc::new();
                document.import(&expose(&cursor, row.get(1), &self.identity)?)?;
                for ((kind, key), value) in records(&document)? {
                    let payload = protect_payload(
                        &kind,
                        Some(serde_json::to_string(&value)?),
                        &self.identity,
                    )?;
                    projection.push((kind, key, payload));
                }
            }
            for page in projection.chunks(CONNECT_PAGE_SIZE) {
                let mut insert =
                    QueryBuilder::new("INSERT INTO projection(kind,object_key,payload) ");
                insert.push_values(page, |mut row, (kind, key, payload)| {
                    row.push_bind(kind).push_bind(key).push_bind(payload);
                });
                insert.push(" ON CONFLICT(kind,object_key) DO UPDATE SET payload=excluded.payload");
                insert.build().execute(&mut *transaction).await?;
            }
        }
        transaction.commit().await?;
        Ok(())
    }

    /// Used only after the user confirms replacing this installation's old profile.
    pub async fn clear(&self) -> Result<()> {
        let mut connection = database_connection(&self.connection).await?;
        sqlx::raw_sql("BEGIN; DELETE FROM documents; DELETE FROM projection; DELETE FROM local_values; DELETE FROM sync_cursors; DELETE FROM history_peers; DELETE FROM history_acknowledgements; DELETE FROM history_pending; COMMIT;").execute(&mut *connection).await?;
        Ok(())
    }
}

async fn database_connection(
    connection: &Mutex<Option<SqliteConnection>>,
) -> Result<MappedMutexGuard<'_, SqliteConnection>> {
    MutexGuard::try_map(connection.lock().await, Option::as_mut)
        .map_err(|_| anyhow::anyhow!("Connect profile is closed"))
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
        let update: Update = serde_json::from_str(&line)?;
        validate_version(update.version)?;
        let image = update.document.starts_with("playlist_artwork:")
            || update.document.starts_with("cache_artwork:");
        page.push(update);
        if image {
            break;
        }
    }
    Ok((input, page, false))
}

fn validate_version(version: u32) -> Result<()> {
    if version != FORMAT {
        bail!(INCOMPATIBLE_PROFILE);
    }
    Ok(())
}

fn document_name(record: &ConnectRecord) -> Result<String> {
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

    async fn save_snapshots(
        &self,
        connection: &mut SqliteConnection,
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
                        protect(name, snapshot, &self.identity)?.into_owned(),
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
            let mut pending = QueryBuilder::new("INSERT OR IGNORE INTO history_pending(name) ");
            pending.push_values(page, |mut row, (name, _, _, _)| {
                row.push_bind(name);
            });
            pending.build().execute(&mut *connection).await?;
            let removed = page
                .iter()
                .filter(|(_, _, _, removed)| *removed)
                .map(|(name, _, _, _)| name.strip_prefix("device:").unwrap())
                .collect::<Vec<_>>();
            if !removed.is_empty() {
                let mut delete =
                    QueryBuilder::new("DELETE FROM history_acknowledgements WHERE peer IN (");
                let mut peers = delete.separated(",");
                for peer in removed {
                    peers.push_bind(peer);
                }
                peers.push_unseparated(")");
                delete.build().execute(&mut *connection).await?;
                sqlx::query("INSERT OR IGNORE INTO history_pending SELECT name FROM documents")
                    .execute(&mut *connection)
                    .await?;
            }
        }
        Ok(())
    }
}

async fn next_revision(connection: &mut SqliteConnection) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "UPDATE file_revision SET revision=revision+1 WHERE id=1 RETURNING revision",
    )
    .fetch_one(connection)
    .await?)
}

/// Upgrade existing snapshots once, in bounded pages, without rewriting them.
async fn index_documents(
    connection: &mut SqliteConnection,
    identity: &Arc<age::x25519::Identity>,
) -> Result<()> {
    let mut transaction = connection.begin().await?;
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('documents')")
            .fetch_all(&mut *transaction)
            .await?;
    if !columns.iter().any(|name| name == "revision") {
        sqlx::query("ALTER TABLE documents ADD COLUMN revision INTEGER NOT NULL DEFAULT 0")
            .execute(&mut *transaction)
            .await?;
    }
    if !columns.iter().any(|name| name == "version") {
        sqlx::query("ALTER TABLE documents ADD COLUMN version BLOB NOT NULL DEFAULT X''")
            .execute(&mut *transaction)
            .await?;
    }
    if !columns.iter().any(|name| name == "compressed") {
        sqlx::query("ALTER TABLE documents ADD COLUMN compressed BLOB")
            .execute(&mut *transaction)
            .await?;
    }
    sqlx::raw_sql(
        "DROP TRIGGER IF EXISTS file_revision_insert;
        DROP TRIGGER IF EXISTS file_revision_update;
        DROP TABLE IF EXISTS outgoing;
        DROP INDEX IF EXISTS documents_sync_priority;
        CREATE TRIGGER IF NOT EXISTS documents_compressed_changed AFTER UPDATE OF snapshot ON documents BEGIN UPDATE documents SET compressed=NULL WHERE name=NEW.name; END;
        CREATE INDEX IF NOT EXISTS documents_uncompressed ON documents(name) WHERE compressed IS NULL;
        CREATE INDEX IF NOT EXISTS documents_revision ON documents(revision,name);",
    )
    .execute(&mut *transaction)
    .await?;
    let index = format!(
        "CREATE INDEX documents_setup_revision ON documents(({SYNC_PRIORITY}),revision,name)"
    );
    let previous: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type='index' AND name='documents_setup_revision'",
    )
    .fetch_optional(&mut *transaction)
    .await?;
    if previous.as_deref() != Some(&index) {
        sqlx::query("DROP INDEX IF EXISTS documents_setup_revision")
            .execute(&mut *transaction)
            .await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(index))
            .execute(&mut *transaction)
            .await?;
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE INDEX IF NOT EXISTS documents_image_revision ON documents(({SYNC_PRIORITY}),revision,name) WHERE name GLOB 'playlist_artwork:*' OR name GLOB 'cache_artwork:*';
        CREATE INDEX IF NOT EXISTS documents_image_name ON documents(name) WHERE name GLOB 'playlist_artwork:*' OR name GLOB 'cache_artwork:*';"
    )))
    .execute(&mut *transaction).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE TABLE IF NOT EXISTS snapshot_revisions(stream INTEGER PRIMARY KEY,revision INTEGER NOT NULL) STRICT;
        INSERT OR IGNORE INTO snapshot_revisions SELECT stream,coalesce((SELECT max(revision) FROM documents WHERE ({SYNC_PRIORITY})=stream),0) FROM (SELECT 0 AS stream UNION ALL SELECT 1 UNION ALL SELECT 2);"
    )))
    .execute(&mut *transaction).await?;
    let new_stream = SYNC_PRIORITY.replace("name", "NEW.name");
    let old_stream = SYNC_PRIORITY.replace("name", "OLD.name");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE TRIGGER IF NOT EXISTS snapshot_revision_insert AFTER INSERT ON documents BEGIN UPDATE snapshot_revisions SET revision=revision+1 WHERE stream=({new_stream}); END;
        CREATE TRIGGER IF NOT EXISTS snapshot_revision_update AFTER UPDATE OF snapshot ON documents WHEN NEW.snapshot IS NOT OLD.snapshot BEGIN UPDATE snapshot_revisions SET revision=revision+1 WHERE stream=({new_stream}); END;
        CREATE TRIGGER IF NOT EXISTS snapshot_revision_delete AFTER DELETE ON documents BEGIN UPDATE snapshot_revisions SET revision=revision+1 WHERE stream=({old_stream}); END;"
    )))
    .execute(&mut *transaction).await?;
    loop {
        let rows = sqlx::query(
            "SELECT name,snapshot FROM documents WHERE revision=0 AND name<=coalesce((SELECT min(name) FROM documents WHERE revision=0 AND (name GLOB 'playlist_artwork:*' OR name GLOB 'cache_artwork:*')),char(1114111)) ORDER BY name LIMIT ?1",
        )
        .bind(CONNECT_PAGE_SIZE as i64)
        .fetch_all(&mut *transaction)
        .await?;
        if rows.is_empty() {
            break;
        }
        let identity = Arc::clone(identity);
        let versions = tokio::task::spawn_blocking(move || {
            rows.into_iter()
                .map(|row| {
                    let name: String = row.get(0);
                    let snapshot = expose(&name, row.get(1), &identity)?;
                    let metadata = LoroDoc::decode_import_blob_meta(&snapshot, true)?;
                    Ok((name, metadata.partial_end_vv.encode()))
                })
                .collect::<Result<Vec<_>>>()
        })
        .await??;
        let count = versions.len() as i64;
        let end: i64 = sqlx::query_scalar(
            "UPDATE file_revision SET revision=revision+?1 WHERE id=1 RETURNING revision",
        )
        .bind(count)
        .fetch_one(&mut *transaction)
        .await?;
        let mut update = QueryBuilder::new("WITH updates(name,revision,version) AS (");
        update.push_values(
            versions.into_iter().enumerate(),
            |mut row, (offset, (name, version))| {
                row.push_bind(name)
                    .push_bind(end - count + offset as i64 + 1)
                    .push_bind(version);
            },
        );
        update.push(") UPDATE documents SET revision=updates.revision,version=updates.version FROM updates WHERE documents.name=updates.name");
        update.build().execute(&mut *transaction).await?;
    }
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
        preserve_unknown_fields(value, &serde_json::from_str(previous)?);
    }
    if matches!(record.kind.as_str(), "playlist" | "native_playlist") {
        document
            .get_map("lifecycle")
            .insert("deleted", record.value.is_none())?;
        if record.value.is_none() {
            let keys = map
                .get_deep_value()
                .to_json_value()
                .as_object()
                .context("invalid playlist records")?
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
        let (results, projection, snapshots) = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut results = Vec::with_capacity(updates.len());
            let mut projection = Vec::new();
            let mut snapshots = Vec::new();
            for update in updates {
                let name = update.document;
                let previous = saved.get(&name);
                if let Some((version, snapshot)) = previous
                    && !sensitive(&name)
                    && *snapshot == update.bytes
                {
                    results.push((false, VersionVector::decode(version)?));
                    continue;
                }
                let incoming =
                    LoroDoc::decode_import_blob_meta(&update.bytes, true)?.partial_end_vv;
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
                    let payload = record
                        .value
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?;
                    let payload = protect_payload(&record.kind, payload, &identity)?;
                    projection.push((record.kind, record.key, payload));
                }
                let snapshot = document.export(ExportMode::Snapshot)?;
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
            Ok((results, projection, snapshots))
        })
        .await??;
        for page in projection.chunks(CONNECT_PAGE_SIZE) {
            let mut insert = QueryBuilder::new("INSERT INTO projection(kind,object_key,payload) ");
            insert.push_values(page, |mut row, (kind, key, payload)| {
                row.push_bind(kind).push_bind(key).push_bind(payload);
            });
            insert.push(" ON CONFLICT(kind,object_key) DO UPDATE SET payload=excluded.payload");
            insert.build().execute(&mut *connection).await?;
        }
        self.save_snapshots(connection, &snapshots).await?;
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
        let mut cursor = 0;
        let mut updates = Vec::new();
        loop {
            let page = from.changes(cursor, CONNECT_PAGE_SIZE).await.unwrap();
            let Some(last) = page.last() else { break };
            cursor = last.revision;
            let names: Vec<_> = page.into_iter().map(|version| version.name).collect();
            updates.extend(
                from.updates(&to.versions(&names).await.unwrap())
                    .await
                    .unwrap(),
            );
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
        let a = ProfileStore::open(&directory.path().join("a.sqlite"), 1, KEY.parse().unwrap())
            .await
            .unwrap();
        let path = directory.path().join("b.sqlite");
        let b = ProfileStore::open(&path, 2, KEY.parse().unwrap())
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
        let database = Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let (records, _) = b.project(&database).await.unwrap();
        b.acknowledge_projection(&records).await.unwrap();
        drop(b);
        let b = ProfileStore::open(&path, 2, KEY.parse().unwrap())
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
        let a = ProfileStore::open(&a_path, 1, KEY.parse().unwrap())
            .await
            .unwrap();
        let b = ProfileStore::open(&directory.path().join("b.sqlite"), 2, KEY.parse().unwrap())
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
        let a = ProfileStore::open(&a_path, 1, KEY.parse().unwrap())
            .await
            .unwrap();
        assert_eq!(a.revision().await.unwrap(), revision);
        let mut cursor = 0;
        let mut names = Vec::new();
        loop {
            let page = a.changes(cursor, 2).await.unwrap();
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
            &a.load_document(
                &mut *database_connection(&a.connection).await.unwrap(),
                "preference:a",
            )
            .await
            .unwrap(),
        )
        .unwrap();
        let b_records = records(
            &b.load_document(
                &mut *database_connection(&b.connection).await.unwrap(),
                "preference:a",
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(a_records, b_records);
        assert_eq!(a.changes(cursor, 2).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn update_page_rolls_back_on_invalid_document() {
        let directory = tempfile::tempdir().unwrap();
        let a = ProfileStore::open(&directory.path().join("a.sqlite"), 1, KEY.parse().unwrap())
            .await
            .unwrap();
        let b = ProfileStore::open(&directory.path().join("b.sqlite"), 2, KEY.parse().unwrap())
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
        assert!(b.changes(0, 10).await.unwrap().is_empty());
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
        assert!(
            ProfileStore::open(&path, 1, age::x25519::Identity::generate())
                .await
                .is_err()
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
        // open() adds the deletion trigger outside the migration transaction.
        let base: i64 = sqlx::query_scalar("SELECT revision FROM file_revision")
            .fetch_one(&mut old)
            .await
            .unwrap();
        old.close().await.unwrap();
        let profile = ProfileStore::open(&path, 1, KEY.parse().unwrap())
            .await
            .unwrap();
        let versions = profile.changes(0, count).await.unwrap();
        assert_eq!(versions.len(), count);
        for (offset, version) in versions.iter().enumerate() {
            assert_eq!(version.revision, base + offset as i64 + 1);
            assert_eq!(
                VersionVector::decode(&version.version).unwrap(),
                expected[&version.name].1
            );
        }
        let saved = sqlx::query("SELECT name,snapshot FROM documents ORDER BY name")
            .fetch_all(&mut *database_connection(&profile.connection).await.unwrap())
            .await
            .unwrap();
        for row in saved {
            assert_eq!(
                row.get::<Vec<u8>, _>(1),
                expected[&row.get::<String, _>(0)].0
            );
        }
        profile.acknowledge_sync("peer", true, 99).await.unwrap();
        drop(profile);
        let profile = ProfileStore::open(&path, 1, KEY.parse().unwrap())
            .await
            .unwrap();
        assert_eq!(profile.changes(0, count).await.unwrap(), versions);
        assert_eq!(profile.revision().await.unwrap(), base + count as i64);
        assert_eq!(profile.sync_cursor("peer", true).await.unwrap(), 99);
    }

    #[tokio::test]
    async fn artwork_added_to_an_existing_profile_reaches_the_catalog() {
        let directory = tempfile::tempdir().unwrap();
        let host = ProfileStore::open(
            &directory.path().join("host.sqlite"),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let guest = ProfileStore::open(
            &directory.path().join("guest.sqlite"),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let source = Database::open(directory.path().join("source.sqlite"))
            .await
            .unwrap();
        let destination = Database::open(directory.path().join("destination.sqlite"))
            .await
            .unwrap();
        let art = b"provider artwork binding";
        let mut scan = library::Scan::begin(&source, "server", "Server", "server", None)
            .await
            .unwrap();
        scan.write_artist(
            "artist",
            "Artist",
            "artist",
            Some("artist"),
            None,
            Some(art),
            None,
            None,
        )
        .await
        .unwrap();
        scan.finish().await.unwrap();
        source.connect_initialize_profile().await.unwrap();
        while source.connect_seed_page().await.unwrap() {}
        let mut old = source
            .connect_changes()
            .await
            .unwrap()
            .into_iter()
            .find(|change| change.record.kind == "artist")
            .unwrap()
            .record;
        let value = old.value.as_mut().unwrap().as_object_mut().unwrap();
        value.remove("artwork_binding");
        value.insert("future_field".into(), json!("retained"));
        host.write_records(&[old]).await.unwrap();
        deliver(&host, &guest).await;
        let (records, changed) = guest.project(&destination).await.unwrap();
        assert!(changed);
        guest.acknowledge_projection(&records).await.unwrap();
        let cached = destination
            .cached_source("server", &library::ReadCancellation::new())
            .await
            .unwrap()
            .unwrap();
        // Bounded library backfill adds the newly captured field to the existing document.
        assert_eq!(host.capture(&source).await.unwrap(), 1);
        deliver(&host, &guest).await;
        let (records, changed) = guest.project(&destination).await.unwrap();
        assert!(changed);
        assert_eq!(
            records[0].value.as_ref().unwrap()["future_field"],
            "retained"
        );
        assert!(!guest.project(&destination).await.unwrap().1);
        guest.acknowledge_projection(&records).await.unwrap();
        assert!(!guest.projection_pending().await.unwrap());
        let bindings = destination
            .artwork_preparation_page(cached.source, None, 20, &library::ReadCancellation::new())
            .await
            .unwrap();
        assert_eq!(bindings.len(), 1);
        let artist: library::ArtistArtworkBinding = serde_json::from_slice(&bindings[0]).unwrap();
        assert_eq!(artist.artist_name, "Artist");
        assert_eq!(artist.source.as_deref(), Some(art.as_slice()));
    }

    #[tokio::test]
    async fn older_models_preserve_new_fields_and_settings_without_echoing_them() {
        let directory = tempfile::tempdir().unwrap();
        let a = ProfileStore::open(
            &directory.path().join("newer.sqlite"),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let b = ProfileStore::open(
            &directory.path().join("older.sqlite"),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let database = Database::open(directory.path().join("library.sqlite"))
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
            state(false, true),
        ])
        .await
        .unwrap();
        deliver(&a, &b).await;
        assert!(b.projection_pending().await.unwrap());
        let (projected, changed) = b.project(&database).await.unwrap();
        assert!(changed);
        assert!(!b.project(&database).await.unwrap().1);
        b.acknowledge_projection(&projected).await.unwrap();
        assert!(!b.projection_pending().await.unwrap());
        assert_eq!(
            b.write_settings(&[source(
                json!({"configuration":{"name":"Server"},"credential":"secret"})
            )])
            .await
            .unwrap(),
            0
        );
        assert!(missing_updates(&b, &a).await.is_empty());
        b.write_settings(&[source(
            json!({"configuration":{"name":"Renamed"},"credential":null}),
        )])
        .await
        .unwrap();
        b.write_records(&[state(true, false)]).await.unwrap();
        deliver(&b, &a).await;
        let mut connection = database_connection(&a.connection).await.unwrap();
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
        let track = records(
            &a.load_document(&mut connection, "state:https://example.org/track.flac")
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            track[&("state".into(), "https://example.org/track.flac".into())]["new_column"],
            "retained"
        );
        assert_eq!(
            track[&("state".into(), "https://example.org/track.flac".into())]["favorite"],
            true
        );
    }

    #[tokio::test]
    async fn incompatible_updates_leave_persisted_profile_ready_to_resume() {
        let directory = tempfile::tempdir().unwrap();
        let sender = ProfileStore::open(
            &directory.path().join("sender.sqlite"),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let path = directory.path().join("receiver.sqlite");
        let receiver = ProfileStore::open(&path, 2, KEY.parse().unwrap())
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
                .import(&serde_json::to_vec(&incompatible).unwrap())
                .await
                .unwrap_err()
                .to_string(),
            INCOMPATIBLE_PROFILE
        );
        let snapshot = directory.path().join("incompatible.jsonl");
        sender.export_snapshot(&snapshot).await.unwrap();
        let snapshot_contents =
            std::io::read_to_string(snapshot_reader(File::open(&snapshot).unwrap()).unwrap())
                .unwrap();
        std::fs::write(
            &snapshot,
            snapshot_contents.replacen("\"version\":1", "\"version\":2", 1),
        )
        .unwrap();
        assert_eq!(
            receiver
                .replace_snapshot(std::fs::File::open(&snapshot).unwrap())
                .await
                .unwrap_err()
                .to_string(),
            INCOMPATIBLE_PROFILE
        );
        drop(receiver);
        let receiver = ProfileStore::open(&path, 2, KEY.parse().unwrap())
            .await
            .unwrap();
        deliver(&sender, &receiver).await;
        let mut connection = database_connection(&receiver.connection).await.unwrap();
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
        let a = ProfileStore::open(&directory.path().join("a.sqlite"), 1, KEY.parse().unwrap())
            .await
            .unwrap();
        let b = ProfileStore::open(&directory.path().join("b.sqlite"), 2, KEY.parse().unwrap())
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
            let mut connection = database_connection(&a.connection).await.unwrap();
            records(
                &a.load_document(&mut connection, "playlist:[null,\"mix\"]")
                    .await
                    .unwrap(),
            )
            .unwrap()
        };
        drop(b);
        let b = ProfileStore::open(&directory.path().join("b.sqlite"), 2, KEY.parse().unwrap())
            .await
            .unwrap();
        let actual = {
            let mut connection = database_connection(&b.connection).await.unwrap();
            records(
                &b.load_document(&mut connection, "playlist:[null,\"mix\"]")
                    .await
                    .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 4);
        let mut connection = database_connection(&b.connection).await.unwrap();
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
    async fn server_playlist_pins_resolve_after_catalog_sync_and_refresh() {
        let directory = tempfile::tempdir().unwrap();
        let source = Database::open(directory.path().join("source.sqlite"))
            .await
            .unwrap();
        let target = Database::open(directory.path().join("target.sqlite"))
            .await
            .unwrap();
        let a = ProfileStore::open(&directory.path().join("a.sqlite"), 1, KEY.parse().unwrap())
            .await
            .unwrap();
        let b = ProfileStore::open(&directory.path().join("b.sqlite"), 2, KEY.parse().unwrap())
            .await
            .unwrap();
        let id = library::SourceId::new("server");
        let cancel = library::ReadCancellation::new();
        let art = br#"{"source_id":"server","image":"playlist-cover"}"#;
        for revision in 0..3 {
            let mut scan = library::Scan::begin(&source, "server", "Server", "server", None)
                .await
                .unwrap();
            if revision < 2 {
                scan.write_playlist("mix", "Server mix", "server mix", "server mix", Some(art))
                    .await
                    .unwrap();
                scan.write_playlist_permissions("mix", false, false)
                    .await
                    .unwrap();
                for position in 0..140 {
                    let entry = if revision == 0 {
                        position
                    } else {
                        139 - position
                    };
                    scan.write_playlist_entry(
                        "mix",
                        &format!("entry-{entry}"),
                        &format!("track-{entry}"),
                        position,
                    )
                    .await
                    .unwrap();
                }
            }
            scan.finish().await.unwrap();
            if revision == 0 {
                source.connect_initialize_profile().await.unwrap();
            }
            while source.connect_seed_page().await.unwrap() {}
            while a.capture(&source).await.unwrap() > 0 {}
            deliver(&a, &b).await;
            loop {
                let (page, _) = b.project(&target).await.unwrap();
                if page.is_empty() {
                    break;
                }
                b.acknowledge_projection(&page).await.unwrap();
            }
            assert!(!b.projection_pending().await.unwrap());
            let key = target
                .playlist_key_by_identity(Some(&id), "mix", &cancel)
                .await
                .unwrap();
            if revision == 2 {
                assert!(key.is_none());
                continue;
            }
            let key = key.expect("shared pin resolves without contacting its server");
            let row = target
                .playlist_rows(&[key], &cancel)
                .await
                .unwrap()
                .pop()
                .unwrap();
            assert_eq!(row.name, "Server mix");
            assert_eq!(row.track_count, 140);
            assert!(!row.writable);
            assert_eq!(row.artwork_binding.as_deref(), Some(art.as_slice()));
            let entries = target
                .playlist_entries_page(
                    key,
                    None,
                    library::PlaylistEntrySort::Position,
                    false,
                    "",
                    0,
                    140,
                    &cancel,
                )
                .await
                .unwrap();
            let first = if revision == 0 {
                "track-0"
            } else {
                "track-139"
            };
            assert_eq!(
                entries[0].media_uri,
                library::source_entity_uri(&id, "track", first)
            );
            assert_eq!(entries.len(), 140);
            deliver(&a, &b).await;
            assert!(!b.projection_pending().await.unwrap());
        }
    }

    #[tokio::test]
    async fn playlist_order_and_setup_snapshot_preserve_profile_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let a = ProfileStore::open(&directory.path().join("a.sqlite"), 1, KEY.parse().unwrap())
            .await
            .unwrap();
        let b = ProfileStore::open(&directory.path().join("b.sqlite"), 2, KEY.parse().unwrap())
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
        let db = Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        loop {
            let (page, _) = b.project(&db).await.unwrap();
            if page.is_empty() {
                break;
            }
            b.acknowledge_projection(&page).await.unwrap();
        }
        db.connect_initialize_profile().await.unwrap();
        while db.connect_seed_page().await.unwrap() {}
        let names = db
            .connect_changes()
            .await
            .unwrap()
            .into_iter()
            .filter(|c| c.record.kind == "playlist")
            .map(|c| {
                c.record.value.unwrap()["object_id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["c", "a", "b"]);
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
        let setup = directory.path().join("setup.jsonl");
        a.export_setup_snapshot(std::fs::File::create(&setup).unwrap())
            .await
            .unwrap();
        b.replace_snapshot(std::fs::File::open(&setup).unwrap())
            .await
            .unwrap();
        let mut connection = database_connection(&b.connection).await.unwrap();
        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM documents ORDER BY name")
            .fetch_all(&mut *connection)
            .await
            .unwrap();
        assert_eq!(names, ["preference:theme", "root:music"]);
        drop(connection);
        let (page, _) = b.project(&db).await.unwrap();
        b.acknowledge_projection(&page).await.unwrap();
        let roots = db.connect_roots("local").await.unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].id, "music");
        assert_eq!(roots[0].label, "Music");
    }

    #[tokio::test]
    async fn settings_and_catalog_have_separate_indexed_cursors() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(
            &directory.path().join("profile.sqlite"),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let record = |kind: &str, key: &str, value| ConnectRecord {
            kind: kind.into(),
            key: key.into(),
            value: Some(json!(value)),
        };
        store
            .write_records(&[record("future-catalog", "track", 1)])
            .await
            .unwrap();
        store
            .write_records(&[record("preference", "theme", 1)])
            .await
            .unwrap();
        store
            .write_records(&[record("preference", "theme", 2)])
            .await
            .unwrap();
        let updates = store.changes_in(0, 3, true).await.unwrap();
        let documents = updates
            .iter()
            .map(|update| update.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(documents, ["preference:theme"]);
        assert_eq!(updates[0].revision, 3);
        let catalog = store.changes_in(0, 1, false).await.unwrap();
        assert_eq!(catalog[0].name, "future-catalog:track");
        assert_eq!(catalog[0].revision, 1);
        assert!(store.changes_in(3, 1, true).await.unwrap().is_empty());
        store
            .write_records(&[ConnectRecord {
                kind: "playlist_order".into(),
                key: "mix".into(),
                value: Some(json!({"position":0})),
            }])
            .await
            .unwrap();
        let next = store.changes_in(3, 1, true).await.unwrap();
        assert_eq!(next[0].name, "playlist_order");
        let mut connection = database_connection(&store.connection).await.unwrap();
        let plan: Vec<String> = sqlx::query(sqlx::AssertSqlSafe(format!(
            "EXPLAIN QUERY PLAN SELECT name,version,revision FROM documents WHERE ({SYNC_PRIORITY})=1 AND revision>0 ORDER BY revision,name LIMIT 1"
        )))
        .fetch_all(&mut *connection)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get("detail"))
        .collect();
        assert!(
            plan.iter()
                .any(|step| step.contains("documents_setup_revision")),
            "{plan:?}"
        );
    }

    #[tokio::test]
    async fn newer_local_credentials_replace_queued_incoming_projection() {
        let directory = tempfile::tempdir().unwrap();
        let a = ProfileStore::open(&directory.path().join("a.sqlite"), 1, KEY.parse().unwrap())
            .await
            .unwrap();
        let b = ProfileStore::open(&directory.path().join("b.sqlite"), 2, KEY.parse().unwrap())
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
        let db = Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let (projected, _) = b.project(&db).await.unwrap();
        assert_eq!(projected, vec![source("new-local")]);
        b.acknowledge_projection(&projected).await.unwrap();
        assert!(b.project(&db).await.unwrap().0.is_empty());
    }

    #[tokio::test]
    async fn file_edits_are_delivered_to_peers_after_offline_import() {
        let directory = tempfile::tempdir().unwrap();
        let original = ProfileStore::open(
            &directory.path().join("original.sqlite"),
            1,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let offline = ProfileStore::open(
            &directory.path().join("offline.sqlite"),
            2,
            KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        let joining = ProfileStore::open(
            &directory.path().join("joining.sqlite"),
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
        original.export_snapshot(&peer_file).await.unwrap();
        joining
            .replace_snapshot(std::fs::File::open(&peer_file).unwrap())
            .await
            .unwrap();
        assert!(
            joining
                .snapshot_contains_current(std::fs::File::open(&peer_file).unwrap())
                .await
                .unwrap()
        );
        assert!(missing_updates(&joining, &original).await.is_empty());
        original.export_snapshot(&file).await.unwrap();
        offline
            .replace_snapshot(std::fs::File::open(&file).unwrap())
            .await
            .unwrap();
        offline.write_records(&[record(false)]).await.unwrap();
        offline.export_snapshot(&file).await.unwrap();
        joining
            .replace_snapshot(std::fs::File::open(&file).unwrap())
            .await
            .unwrap();
        assert!(
            !joining
                .snapshot_contains_current(std::fs::File::open(&peer_file).unwrap())
                .await
                .unwrap()
        );
        assert!(
            joining
                .snapshot_contains_current(std::fs::File::open(&file).unwrap())
                .await
                .unwrap()
        );
        assert!(!missing_updates(&joining, &original).await.is_empty());
        joining
            .import_snapshot(std::fs::File::open(&peer_file).unwrap())
            .await
            .unwrap();
        assert_eq!(missing_updates(&joining, &original).await.len(), 1);
        deliver(&joining, &original).await;
        assert!(
            original
                .snapshot_contains_current(std::fs::File::open(&file).unwrap())
                .await
                .unwrap()
        );
        assert!(missing_updates(&joining, &original).await.is_empty());
        assert!(
            !joining
                .import_snapshot(std::fs::File::open(&file).unwrap())
                .await
                .unwrap()
        );
        assert!(missing_updates(&joining, &original).await.is_empty());
        let db = Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        assert_eq!(original.project(&db).await.unwrap().0, vec![record(false)]);
        joining
            .write_records(&[ConnectRecord {
                kind: "state".into(),
                key: "track".into(),
                value: Some(json!({"favorite": true})),
            }])
            .await
            .unwrap();
        assert!(
            !joining
                .snapshot_contains_current(std::fs::File::open(&file).unwrap())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn snapshot_replacement_resets_cursors_atomically_but_restart_and_merge_preserve_them() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profile.sqlite");
        let snapshot = directory.path().join("profile.jsonl");
        let profile = ProfileStore::open(&path, 1, KEY.parse().unwrap())
            .await
            .unwrap();
        profile
            .write_records(&[ConnectRecord {
                kind: "preference".into(),
                key: "theme".into(),
                value: Some(json!("dark")),
            }])
            .await
            .unwrap();
        profile.export_snapshot(&snapshot).await.unwrap();
        assert_eq!(profile.sync_cursor("peer", true).await.unwrap(), 0);
        profile.acknowledge_sync("peer", true, 12).await.unwrap();
        profile.acknowledge_sync("peer", false, 24).await.unwrap();
        drop(profile);
        let profile = ProfileStore::open(&path, 1, KEY.parse().unwrap())
            .await
            .unwrap();
        assert_eq!(profile.sync_cursor("peer", true).await.unwrap(), 12);
        assert_eq!(profile.sync_cursor("peer", false).await.unwrap(), 24);
        profile
            .import_snapshot(std::fs::File::open(&snapshot).unwrap())
            .await
            .unwrap();
        profile
            .import_snapshot(std::fs::File::open(&snapshot).unwrap())
            .await
            .unwrap();
        assert_eq!(profile.sync_cursor("peer", true).await.unwrap(), 12);
        assert_eq!(profile.sync_cursor("peer", false).await.unwrap(), 24);
        let bytes = std::fs::read(&snapshot).unwrap();
        std::fs::write(&snapshot, &bytes[..bytes.len() - 4]).unwrap();
        assert!(
            profile
                .replace_snapshot(std::fs::File::open(&snapshot).unwrap())
                .await
                .is_err()
        );
        assert_eq!(profile.sync_cursor("peer", true).await.unwrap(), 12);
        assert_eq!(profile.sync_cursor("peer", false).await.unwrap(), 24);
        std::fs::write(&snapshot, bytes).unwrap();
        profile
            .replace_snapshot(std::fs::File::open(&snapshot).unwrap())
            .await
            .unwrap();
        assert_eq!(profile.sync_cursor("peer", true).await.unwrap(), 0);
        assert_eq!(profile.sync_cursor("peer", false).await.unwrap(), 0);
        profile.acknowledge_sync("peer", false, 30).await.unwrap();
        profile
            .replace_snapshot(std::fs::File::open(&snapshot).unwrap())
            .await
            .unwrap();
        assert_eq!(profile.sync_cursor("peer", false).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn invalid_snapshot_rolls_back_every_document_and_stale_snapshot_keeps_newer_edits() {
        let directory = tempfile::tempdir().unwrap();
        let a = ProfileStore::open(&directory.path().join("a.sqlite"), 1, KEY.parse().unwrap())
            .await
            .unwrap();
        let b = ProfileStore::open(&directory.path().join("b.sqlite"), 2, KEY.parse().unwrap())
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
        a.export_snapshot(&snapshot).await.unwrap();
        let complete = std::fs::read(&snapshot).unwrap();
        std::fs::write(&snapshot, &complete[..complete.len() - 4]).unwrap();
        assert!(
            b.import_snapshot(std::fs::File::open(&snapshot).unwrap())
                .await
                .is_err()
        );
        let mut connection = database_connection(&b.connection).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM documents")
                .fetch_one(&mut *connection)
                .await
                .unwrap(),
            0
        );
        drop(connection);
        std::fs::write(&snapshot, &complete).unwrap();
        b.import_snapshot(std::fs::File::open(&snapshot).unwrap())
            .await
            .unwrap();
        b.write_records(&[record("light")]).await.unwrap();
        assert!(
            !b.import_snapshot(std::fs::File::open(&snapshot).unwrap())
                .await
                .unwrap()
        );
        let mut connection = database_connection(&b.connection).await.unwrap();
        let current = records(
            &b.load_document(&mut connection, "preference:theme")
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(current[&("preference".into(), "theme".into())], "light");
    }
}
