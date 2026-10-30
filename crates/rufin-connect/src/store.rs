//! Connect's own SQLite file. Connect is on exactly when it holds the profile row. Each record
//! is one last-writer-wins row; its value lives in the Store or settings, so a row holds one only
//! for kinds that have no other home.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use iroh::SecretKey;
use serde::{Deserialize, Serialize};
use sqlx::{
    Row as _, SqliteConnection,
    sqlite::{
        SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow,
        SqliteSynchronous,
    },
};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS connect_profile (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    -- 32 random bytes, hex.
    profile TEXT NOT NULL CHECK (length(profile) = 64),
    -- This device's iroh id.
    device TEXT NOT NULL,
    -- Last hybrid clock issued or seen: unix milliseconds << 16 | counter.
    clock INTEGER NOT NULL DEFAULT 0,
    -- Highest listen_key encoded; Listened changes re-read from 16 below it.
    listened INTEGER NOT NULL DEFAULT 0,
    -- The folder channel location as JSON ({\"path\"}, {\"document\"} or {\"connection\",\"path\"}), or NULL.
    folder TEXT,
    -- Last of this device's own sequences written to its folder directory.
    written INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE IF NOT EXISTS connect_records (
    -- '<kind>:<key>'.
    id TEXT PRIMARY KEY,
    -- Entries: the playlist id. Playlists and smart playlists: their folder id. Otherwise NULL.
    parent TEXT,
    clock INTEGER NOT NULL,
    -- The device that wrote this value; breaks clock ties.
    device TEXT NOT NULL,
    -- Order on this device. Pulls and folder segments read by it; an upsert takes a new one.
    sequence INTEGER NOT NULL UNIQUE,
    -- blake3 hash of the value as encoded ('' for a tombstone). A diff pass compares it with the
    -- value it encodes from the Store; a received row carries the sender's.
    digest BLOB NOT NULL,
    -- 1 for a tombstone.
    deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0,1)),
    -- The value, only for kinds that live nowhere else.
    body TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS connect_records_parent ON connect_records(parent) WHERE parent IS NOT NULL;

CREATE TABLE IF NOT EXISTS connect_peers (
    device TEXT PRIMARY KEY,
    -- Their sequence this device pulled up to over iroh.
    pulled INTEGER NOT NULL DEFAULT 0,
    -- Their own sequence this device read up to from their folder directory.
    read INTEGER NOT NULL DEFAULT 0,
    -- Last known iroh EndpointAddr as JSON.
    address TEXT
) STRICT;
";

macro_rules! rows {
    ($filter:literal) => {
        concat!(
            "SELECT id,parent,clock,device,sequence,lower(hex(digest)),deleted,body FROM connect_records",
            $filter
        )
    };
}

#[derive(Clone, Debug)]
pub struct Store {
    pool: SqlitePool,
    path: PathBuf,
    device: String,
}

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct ProfileRow {
    pub profile: String,
    pub device: String,
    pub clock: i64,
    pub listened: i64,
    pub folder: Option<String>,
    pub written: i64,
}

/// A record as a pull page or a folder segment carries it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub id: String,
    pub parent: Option<String>,
    pub clock: i64,
    pub device: String,
    pub sequence: i64,
    /// Lowercase hex; empty for a tombstone.
    pub digest: String,
    pub deleted: bool,
    pub value: Option<serde_json::Value>,
}

/// A value this device encoded that differs from its record. A `value` is kept as the row's body.
#[derive(Clone, Debug)]
pub struct Change {
    pub id: String,
    pub parent: Option<String>,
    pub digest: String,
    pub deleted: bool,
    pub value: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct Peer {
    pub device: String,
    pub pulled: i64,
    pub read: i64,
    pub address: Option<String>,
}

/// Where received rows came from, and how far that source has been applied.
#[derive(Clone, Copy, Debug)]
pub enum Cursor<'a> {
    Pulled { device: &'a str, sequence: i64 },
    Read { device: &'a str, sequence: i64 },
}

/// Which records to compare a diff pass with.
#[derive(Clone, Copy, Debug)]
pub enum Select<'a> {
    All,
    Ids(&'a [String]),
    /// Every record of a kind.
    Kind(&'a str),
    /// The records with this parent.
    Parent(&'a str),
}

/// The digest a diff pass compares: blake3 of the value as encoded.
pub fn digest(value: &impl Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}

pub fn new_profile() -> String {
    data_encoding::HEXLOWER.encode(&SecretKey::generate().to_bytes())
}

impl Store {
    /// Connect's records when it is on. A file without its profile row is an aborted turn-on or
    /// join and is deleted.
    pub async fn open(path: &Path) -> Result<Option<(Self, ProfileRow)>> {
        if !tokio::fs::try_exists(path).await? {
            return Ok(None);
        }
        let pool = connect(path, false).await?;
        let Some(profile) = read_profile(&pool).await? else {
            pool.close().await;
            remove(path).await?;
            return Ok(None);
        };
        Ok(Some((
            Self {
                pool,
                path: path.to_owned(),
                device: profile.device.clone(),
            },
            profile,
        )))
    }

    /// A new file for turning on or joining; Connect is on once [`Store::turn_on`] commits.
    pub async fn create(path: &Path, device: &str) -> Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let pool = connect(path, true).await?;
        sqlx::raw_sql(SCHEMA).execute(&pool).await?;
        Ok(Self {
            pool,
            path: path.to_owned(),
            device: device.to_owned(),
        })
    }

    pub async fn turn_on(&self, profile: &str) -> Result<()> {
        sqlx::query("INSERT INTO connect_profile(singleton,profile,device,clock) SELECT 1,?1,?2,COALESCE(MAX(clock),0) FROM connect_records")
            .bind(profile)
            .bind(&self.device)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn profile(&self) -> Result<Option<ProfileRow>> {
        read_profile(&self.pool).await
    }

    /// Closes the file for a storage reset.
    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// Turning off: the file and its journal go.
    pub async fn delete(self) -> Result<()> {
        self.pool.close().await;
        remove(&self.path).await
    }

    /// The last sequence this device issued.
    pub async fn sequence(&self) -> Result<i64> {
        Ok(
            sqlx::query_scalar("SELECT COALESCE(MAX(sequence),0) FROM connect_records")
                .fetch_one(&self.pool)
                .await?,
        )
    }

    /// Stored digests by id, tombstones included.
    pub async fn digests(&self, select: Select<'_>) -> Result<HashMap<String, String>> {
        let rows = match select {
            Select::All => {
                sqlx::query_as("SELECT id,lower(hex(digest)) FROM connect_records")
                    .fetch_all(&self.pool)
                    .await?
            }
            Select::Ids(ids) => sqlx::query_as("SELECT id,lower(hex(digest)) FROM connect_records WHERE id IN (SELECT value FROM json_each(?1))")
                .bind(serde_json::to_string(ids)?)
                .fetch_all(&self.pool)
                .await?,
            Select::Kind(kind) => {
                let (start, end) = kind_range(kind);
                sqlx::query_as(
                    "SELECT id,lower(hex(digest)) FROM connect_records WHERE id>=?1 AND id<?2",
                )
                .bind(start)
                .bind(end)
                .fetch_all(&self.pool)
                .await?
            }
            Select::Parent(parent) => {
                sqlx::query_as(
                    "SELECT id,lower(hex(digest)) FROM connect_records WHERE parent=?1",
                )
                .bind(parent)
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.into_iter().collect())
    }

    /// The live records of a kind that hold their value.
    pub async fn bodies(&self, kind: &str) -> Result<Vec<Row>> {
        let (start, end) = kind_range(kind);
        sqlx::query(rows!(
            " WHERE id>=?1 AND id<?2 AND deleted=0 AND body IS NOT NULL"
        ))
        .bind(start)
        .bind(end)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row)
        .collect()
    }

    /// Records in sequence order after `after`, only `device`'s own when given. Rows that hold
    /// their value carry it; the others are filled in by the encoder.
    pub async fn rows_after(
        &self,
        after: i64,
        limit: usize,
        device: Option<&str>,
    ) -> Result<Vec<Row>> {
        sqlx::query(rows!(
            " WHERE sequence>?1 AND (?3 IS NULL OR device=?3) ORDER BY sequence LIMIT ?2"
        ))
        .bind(after)
        .bind(limit as i64)
        .bind(device)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row)
        .collect()
    }

    pub async fn count_after(&self, after: i64) -> Result<u64> {
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM connect_records WHERE sequence>?1")
                .bind(after)
                .fetch_one(&self.pool)
                .await?;
        Ok(count as u64)
    }

    /// Records this device's changes with new clocks and sequences, in one transaction with the
    /// listen cursor. Returns the last sequence when a row was written.
    pub async fn write(&self, changes: Vec<Change>, listened: Option<i64>) -> Result<Option<i64>> {
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let mut clock: i64 =
            sqlx::query_scalar("SELECT COALESCE((SELECT clock FROM connect_profile),0)")
                .fetch_one(&mut *transaction)
                .await?;
        let rows = changes
            .into_iter()
            .map(|change| {
                clock = next_clock(clock);
                Row {
                    id: change.id,
                    parent: change.parent,
                    clock,
                    device: self.device.clone(),
                    sequence: 0,
                    digest: change.digest,
                    deleted: change.deleted,
                    value: change.value,
                }
            })
            .collect::<Vec<_>>();
        let sequence = insert(&mut transaction, &rows).await?;
        sqlx::query("UPDATE connect_profile SET clock=?1,listened=COALESCE(?2,listened)")
            .bind(clock)
            .bind(listened)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok((!rows.is_empty()).then_some(sequence))
    }

    /// The rows that win against the stored records: a greater `(clock, device)`, one per id.
    pub async fn newer(&self, rows: Vec<Row>) -> Result<Vec<Row>> {
        let ids = rows.iter().map(|row| &row.id).collect::<Vec<_>>();
        let stored: HashMap<String, (i64, String)> = sqlx::query_as::<_, (String, i64, String)>(
            "SELECT id,clock,device FROM connect_records WHERE id IN (SELECT value FROM json_each(?1))",
        )
        .bind(serde_json::to_string(&ids)?)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(id, clock, device)| (id, (clock, device)))
        .collect();
        let mut newest = HashMap::<String, Row>::new();
        for row in rows {
            let stamp = (row.clock, row.device.as_str());
            if stored
                .get(&row.id)
                .is_some_and(|(clock, device)| stamp <= (*clock, device.as_str()))
                || newest
                    .get(&row.id)
                    .is_some_and(|kept| stamp <= (kept.clock, kept.device.as_str()))
            {
                continue;
            }
            newest.insert(row.id.clone(), row);
        }
        let mut rows = newest.into_values().collect::<Vec<_>>();
        rows.sort_by_key(|row| row.sequence);
        Ok(rows)
    }

    /// Stores applied rows with new local sequences, raises the clock past them and advances the
    /// source's cursor, in one transaction. A row keeps its value only when given one.
    pub async fn receive(&self, rows: &[Row], cursor: Cursor<'_>) -> Result<()> {
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        insert(&mut transaction, rows).await?;
        sqlx::query("UPDATE connect_profile SET clock=MAX(clock,?1)")
            .bind(rows.iter().map(|row| row.clock).max().unwrap_or(0))
            .execute(&mut *transaction)
            .await?;
        let (statement, device, sequence) = match cursor {
            Cursor::Pulled { device, sequence } => (
                "INSERT INTO connect_peers(device,pulled) VALUES(?1,?2) ON CONFLICT(device) DO UPDATE SET pulled=excluded.pulled",
                device,
                sequence,
            ),
            Cursor::Read { device, sequence } => (
                "INSERT INTO connect_peers(device,read) VALUES(?1,?2) ON CONFLICT(device) DO UPDATE SET read=excluded.read",
                device,
                sequence,
            ),
        };
        sqlx::query(statement)
            .bind(device)
            .bind(sequence)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn peers(&self) -> Result<Vec<Peer>> {
        Ok(
            sqlx::query_as("SELECT device,pulled,read,address FROM connect_peers")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn set_address(&self, device: &str, address: &str) -> Result<()> {
        sqlx::query("INSERT INTO connect_peers(device,address) VALUES(?1,?2) ON CONFLICT(device) DO UPDATE SET address=excluded.address")
            .bind(device)
            .bind(address)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_folder(&self, folder: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE connect_profile SET folder=?1")
            .bind(folder)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_written(&self, written: i64) -> Result<()> {
        sqlx::query("UPDATE connect_profile SET written=?1")
            .bind(written)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Deletes Connect's file with its journal files.
pub async fn remove(path: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let mut file = path.as_os_str().to_os_string();
        file.push(suffix);
        match tokio::fs::remove_file(file).await {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
    }
    Ok(())
}

async fn connect(path: &Path, create: bool) -> Result<SqlitePool> {
    Ok(SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(create)
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal)
                .busy_timeout(Duration::from_secs(5)),
        )
        .await?)
}

async fn read_profile(pool: &SqlitePool) -> Result<Option<ProfileRow>> {
    Ok(
        sqlx::query_as("SELECT profile,device,clock,listened,folder,written FROM connect_profile")
            .fetch_optional(pool)
            .await?,
    )
}

/// Stores rows under new local sequences and returns the last one.
async fn insert(connection: &mut SqliteConnection, rows: &[Row]) -> Result<i64> {
    let sequence: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(sequence),0) FROM connect_records")
        .fetch_one(&mut *connection)
        .await?;
    let values = rows
        .iter()
        .zip(sequence + 1..)
        .map(|(row, sequence)| {
            Ok((
                &row.id,
                &row.parent,
                row.clock,
                &row.device,
                sequence,
                &row.digest,
                row.deleted,
                row.value.as_ref().map(serde_json::to_string).transpose()?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    sqlx::query("INSERT INTO connect_records(id,parent,clock,device,sequence,digest,deleted,body)
        SELECT value->>0,value->>1,value->>2,value->>3,value->>4,unhex(value->>5),value->>6,value->>7 FROM json_each(?1) WHERE true
        ON CONFLICT(id) DO UPDATE SET parent=excluded.parent,clock=excluded.clock,device=excluded.device,sequence=excluded.sequence,digest=excluded.digest,deleted=excluded.deleted,body=excluded.body")
        .bind(serde_json::to_string(&values)?)
        .execute(connection)
        .await?;
    Ok(sequence + rows.len() as i64)
}

fn row(row: &SqliteRow) -> Result<Row> {
    Ok(Row {
        id: row.try_get(0)?,
        parent: row.try_get(1)?,
        clock: row.try_get(2)?,
        device: row.try_get(3)?,
        sequence: row.try_get(4)?,
        digest: row.try_get(5)?,
        deleted: row.try_get(6)?,
        value: row
            .try_get::<Option<&str>, _>(7)?
            .map(serde_json::from_str)
            .transpose()?,
    })
}

/// Ids of a kind sort between `<kind>:` and `<kind>;`.
fn kind_range(kind: &str) -> (String, String) {
    (format!("{kind}:"), format!("{kind};"))
}

fn next_clock(last: i64) -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64);
    (now << 16).max(last + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_is_on_only_once_the_profile_row_commits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("connect").join("connect.sqlite");
        assert!(Store::open(&path).await.unwrap().is_none());
        assert!(!path.exists());

        // An interrupted join leaves records without a profile row.
        let store = Store::create(&path, "device").await.unwrap();
        let change = Change {
            id: "device:device".into(),
            parent: None,
            digest: digest(&"value").unwrap(),
            deleted: false,
            value: Some(serde_json::json!({"name": "Laptop", "trusted": true})),
        };
        assert_eq!(store.write(vec![change], None).await.unwrap(), Some(1));
        drop(store);
        assert!(Store::open(&path).await.unwrap().is_none());
        assert!(!path.exists());

        let store = Store::create(&path, "device").await.unwrap();
        let profile = new_profile();
        store.turn_on(&profile).await.unwrap();
        drop(store);
        let (store, row) = Store::open(&path).await.unwrap().unwrap();
        assert_eq!((row.profile, row.device.as_str()), (profile, "device"));
        store.delete().await.unwrap();
        assert!(!path.exists());
    }
}
