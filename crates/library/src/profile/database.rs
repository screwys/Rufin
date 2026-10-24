use super::encryption::protect_payload;
use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};

impl ProfileStore {
    pub async fn bind_profile(&self, profile: &str, publisher: &str) -> Result<()> {
        let mut connection = self.writer().await?;
        sqlx::query(
            "UPDATE connect_profile SET profile=?1,publisher=?2 WHERE singleton=1 AND (profile IS NOT ?1 OR publisher IS NOT ?2)",
        )
        .bind(profile)
        .bind(publisher)
        .execute(&mut *connection)
        .await?;
        Ok(())
    }

    pub async fn database_profile(path: &Path) -> Result<Option<String>> {
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new().filename(path).read_only(true),
        )
        .await?;
        let profile = async {
            let indexed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='connect_profile')")
                .fetch_one(&mut connection).await?;
            if !indexed { return Result::<Option<String>>::Ok(None); }
            sqlx::query_scalar::<_, Option<String>>("SELECT profile FROM connect_profile WHERE singleton=1")
                .fetch_optional(&mut connection).await.map(Option::flatten).map_err(Into::into)
        }.await;
        connection.close().await?;
        profile
    }

    /// Validate an owned input and protect its secrets with the installation key.
    pub async fn prepare_database(
        path: &Path,
        transport_key: &age::x25519::Identity,
        local_key: &age::x25519::Identity,
    ) -> Result<()> {
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(path)
                .journal_mode(SqliteJournalMode::Delete),
        )
        .await?;
        let prepared = async {
            let mut transaction = connection.begin().await?;
            validate_database(&mut transaction).await?;
            let delta: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='updates')",
            )
            .fetch_one(&mut *transaction)
            .await?;
            if !delta {
                validate_base_database(&mut transaction).await?;
                crate::schema::initialize_durable(&mut transaction).await?;
                crate::connect::initialize(&mut transaction).await?;
            }
            rewrap(&mut transaction, transport_key, local_key).await?;
            transaction.commit().await?;
            Result::<()>::Ok(())
        }
        .await;
        connection.close().await?;
        prepared
    }

    pub async fn validate_adoption_database(path: &Path) -> Result<()> {
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new().filename(path).read_only(true),
        )
        .await?;
        let validated = validate_base_database(&mut connection).await;
        connection.close().await?;
        validated
    }

    /// Install an owned input already normalized with this installation's key.
    pub async fn install_prepared_database(database: Database, path: &Path) -> Result<()> {
        let mut incoming =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path)).await?;
        let installed = async {
            let mut transaction = incoming.begin().await?;
            acknowledge_snapshot_on(&mut transaction, "").await?;
            transaction.commit().await?;
            let mut connection = database.writer().await?;
            let (_export, _reader) = database.acquire_export().await?;
            Database::connect_adopt_database_on(connection.as_mut().unwrap(), &mut incoming)
                .await?;
            Result::<()>::Ok(())
        }
        .await;
        incoming.close().await?;
        installed
    }

    /// Pin the committed application database before copying.
    pub async fn export_database(
        &self,
        path: &Path,
        transport_key: &age::x25519::Identity,
        shared: &[crate::StampedRecord],
        personal: &[ConnectRecord],
    ) -> Result<()> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Delete);
        let mut connection = SqliteConnection::connect_with(&options).await?;
        let result = async {
            {
                let (_export, mut reader) = self.database.acquire_export().await?;
                if self.closed.load(Ordering::Acquire) {
                    bail!("Connect profile is closed");
                }
                let mut snapshot = reader.begin().await?;
                let _: i64 = sqlx::query_scalar("SELECT revision FROM file_revision WHERE id=1")
                    .fetch_one(&mut *snapshot)
                    .await?;
                crate::db::copy_database(&mut snapshot, &mut connection).await?;
                snapshot.commit().await?;
            }
            let mut transaction = connection.begin().await?;
            Database::connect_prepare_export_on(&mut transaction).await?;
            sqlx::raw_sql("CREATE TABLE IF NOT EXISTS exported_settings(kind TEXT NOT NULL,object_key TEXT NOT NULL,stamp TEXT,personal INTEGER NOT NULL,payload TEXT,PRIMARY KEY(kind,object_key)) STRICT; DELETE FROM exported_settings;")
                .execute(&mut *transaction).await?;
            rewrap(&mut transaction, &self.identity, transport_key).await?;
            sqlx::raw_sql("DELETE FROM sync_cursors;")
                .execute(&mut *transaction)
                .await?;
            for (record, stamp, personal) in shared
                .iter()
                .map(|value| (&value.record, Some(value.stamp), false))
                .chain(
                    personal
                        .iter()
                        .filter(|record| {
                            !shared.iter().any(|shared| {
                                shared.record.kind == record.kind && shared.record.key == record.key
                            })
                        })
                        .map(|record| (record, None, true)),
                )
            {
                let payload = protect_payload(
                    &record.kind,
                    record
                        .value
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                    transport_key,
                )?;
                sqlx::query("INSERT INTO exported_settings VALUES(?1,?2,?3,?4,?5)")
                    .bind(&record.kind)
                    .bind(&record.key)
                    .bind(
                        stamp
                            .map(|value| serde_json::to_string(&value))
                            .transpose()?,
                    )
                    .bind(personal)
                    .bind(payload)
                    .execute(&mut *transaction)
                    .await?;
            }
            transaction.commit().await?;
            Result::<()>::Ok(())
        }
        .await;
        connection.close().await?;
        result?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)?
            .sync_all()?;
        Ok(())
    }

    /// Merge a same-profile snapshot without changing the input file.
    pub async fn merge_database(
        &self,
        path: &Path,
        transport_key: &age::x25519::Identity,
    ) -> Result<bool> {
        let mut incoming = SqliteConnection::connect_with(
            &SqliteConnectOptions::new().filename(path).read_only(true),
        )
        .await?;
        let result = async {
            let mut snapshot = incoming.begin().await?;
            validate_database(&mut snapshot).await?;
            let publisher = file_publisher(&mut snapshot).await?;
            let changed = self
                .merge_database_on(&mut snapshot, transport_key, publisher.as_deref())
                .await?;
            let mut connection = self.writer().await?;
            let mut transaction = connection.begin().await?;
            merge_receiver_metadata_on(&mut snapshot, &mut transaction).await?;
            if let Some(publisher) = publisher {
                history::register_on(&mut transaction, &[publisher]).await?;
            }
            transaction.commit().await?;
            snapshot.commit().await?;
            Result::<bool>::Ok(changed)
        }
        .await;
        incoming.close().await?;
        result
    }

    async fn merge_database_on(
        &self,
        incoming: &mut SqliteConnection,
        transport_key: &age::x25519::Identity,
        publisher: Option<&str>,
    ) -> Result<bool> {
        let delta: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='updates')",
        )
        .fetch_one(&mut *incoming)
        .await?;
        let table = if delta { "updates" } else { "connect_versions" };
        let mut kinds: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT DISTINCT kind FROM {table} WHERE kind IS NOT NULL"
        )))
        .fetch_all(&mut *incoming)
        .await?;
        kinds.sort_by_key(|kind| match kind.as_str() {
            "catalog:source" => 0,
            "catalog:file" => 1,
            _ => 2,
        });
        let mut changed = false;
        for kind in std::iter::once(None).chain(kinds.into_iter().map(Some)) {
            let (table, column) = if delta {
                ("updates", "payload")
            } else if kind.is_some() {
                ("connect_versions", "CAST(version AS BLOB)")
            } else {
                ("documents", "snapshot")
            };
            let predicate = if delta || kind.is_some() {
                "kind IS ?3"
            } else {
                "?3 IS NULL"
            };
            let mut cursor = String::new();
            loop {
                let rows: Vec<(String,Vec<u8>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT name,{column} FROM {table} WHERE name>?1 AND {predicate} AND name<=coalesce((SELECT min(name) FROM {table} WHERE name>?1 AND {predicate} AND name GLOB 'playlist_artwork:*'),char(1114111)) ORDER BY name LIMIT ?2")))
                    .bind(&cursor).bind(CONNECT_PAGE_SIZE as i64).bind(&kind).fetch_all(&mut *incoming).await?;
                let Some(last) = rows.last() else { break };
                cursor = last.0.clone();
                let updates = if delta {
                    rows.iter()
                        .filter_map(|(_, bytes)| decode_update(bytes).transpose())
                        .map(|update| {
                            let mut update = update?;
                            if update.current.is_none() {
                                update.bytes =
                                    expose(&update.document, &update.bytes, transport_key)?
                                        .into_owned();
                            }
                            Ok(update)
                        })
                        .collect::<Result<Vec<_>>>()?
                } else if kind.is_none() {
                    rows.iter()
                        .map(|(name, bytes)| {
                            Ok(Update {
                                version: FORMAT,
                                document: name.clone(),
                                bytes: expose(name, bytes, transport_key)?.into_owned(),
                                author: None,
                                current: None,
                            })
                        })
                        .collect::<Result<Vec<_>>>()?
                } else {
                    let names = rows.iter().map(|row| row.0.clone()).collect::<Vec<_>>();
                    Self::updates_on(incoming, transport_key, &self.versions(&names).await?).await?
                };
                let mut connection = self.writer().await?;
                let mut transaction = connection.begin().await?;
                let (applied, versions) = self
                    .import_page_on(&mut transaction, updates, false)
                    .await?;
                changed |= applied;
                if !delta
                    && kind.is_none()
                    && let Some(publisher) = publisher
                {
                    let acknowledgements = rows
                        .into_iter()
                        .zip(versions)
                        .map(|((name, _), version)| (publisher.to_owned(), name, version))
                        .collect::<Vec<_>>();
                    history::acknowledge_page_on(&mut transaction, &acknowledgements).await?;
                }
                transaction.commit().await?;
            }
        }
        Ok(changed)
    }

    /// Move released sync state before converting pending changes in the working database.
    pub async fn migrate_legacy_database(
        database: &Database,
        path: &Path,
        profile: &str,
    ) -> Result<bool> {
        if !path.try_exists()? {
            return Ok(false);
        }
        let mut writer = database.writer().await?;
        let connection = writer.as_mut().context("Library writer is closed")?;
        sqlx::query("ATTACH DATABASE ?1 AS legacy_profile")
            .bind(path.to_string_lossy().as_ref())
            .execute(&mut *connection)
            .await?;
        let migrated = async {
            let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM legacy_profile.sqlite_schema WHERE type='table'")
                .fetch_all(&mut *connection).await?;
            if !tables.iter().any(|table| table == "documents")
                || tables.iter().any(|table| table == "sources")
            {
                return Ok(false);
            }
            let mut transaction = connection.begin().await?;
            let bound: Option<String> = sqlx::query_scalar("SELECT profile FROM connect_profile WHERE singleton=1")
                .fetch_one(&mut *transaction).await?;
            if bound.is_some() {
                return Ok(false);
            }
            let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('documents','legacy_profile')")
                .fetch_all(&mut *transaction).await?;
            let version = if columns.iter().any(|column| column == "version") { "version" } else { "X''" };
            let revision = if columns.iter().any(|column| column == "revision") { "revision" } else { "0" };
            sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO documents(name,snapshot,version,revision) SELECT name,snapshot,{version},{revision} FROM legacy_profile.documents WHERE true ON CONFLICT(name) DO NOTHING")))
                .execute(&mut *transaction).await?;
            for (table, columns) in [
                ("history_peers", "peer"),
                ("history_acknowledgements", "peer,name,version"),
                ("history_pending", "name"),
                ("sync_cursors", "peer,setup,revision"),
                ("projection", "kind,object_key,payload"),
                ("local_values", "kind,object_key,payload"),
            ] {
                if !tables.iter().any(|source| source == table) { continue; }
                if matches!(table, "projection" | "local_values") {
                    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE TABLE IF NOT EXISTS {table}(kind TEXT NOT NULL,object_key TEXT NOT NULL,payload TEXT,PRIMARY KEY(kind,object_key)) STRICT")))
                        .execute(&mut *transaction).await?;
                }
                let filter = if table == "local_values" {
                    " WHERE kind IN ('source','integration','preference','scrobbling')"
                } else { "" };
                sqlx::query(sqlx::AssertSqlSafe(format!("INSERT OR IGNORE INTO {table}({columns}) SELECT {columns} FROM legacy_profile.{table}{filter}")))
                    .execute(&mut *transaction).await?;
            }
            if tables.iter().any(|table| table == "file_revision") {
                sqlx::query("UPDATE file_revision SET revision=max(revision,(SELECT revision FROM legacy_profile.file_revision WHERE id=1)) WHERE id=1")
                    .execute(&mut *transaction).await?;
            }
            sqlx::query("UPDATE connect_profile SET profile=?1 WHERE singleton=1")
                .bind(profile).execute(&mut *transaction).await?;
            transaction.commit().await?;
            Result::<bool>::Ok(true)
        }.await;
        sqlx::query("DETACH DATABASE legacy_profile")
            .execute(&mut *connection)
            .await?;
        migrated
    }
}

async fn merge_receiver_metadata_on(
    source: &mut SqliteConnection,
    target: &mut SqliteConnection,
) -> Result<()> {
    let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type='table' AND name IN ('history_peers','history_acknowledgements')")
        .fetch_all(&mut *source).await?;
    if tables.iter().any(|table| table == "history_peers") {
        let mut cursor = String::new();
        loop {
            let peers: Vec<String> = sqlx::query_scalar(
                "SELECT peer FROM history_peers WHERE peer>?1 ORDER BY peer LIMIT ?2",
            )
            .bind(&cursor)
            .bind(CONNECT_PAGE_SIZE as i64)
            .fetch_all(&mut *source)
            .await?;
            let Some(last) = peers.last() else { break };
            cursor = last.clone();
            history::register_on(target, &peers).await?;
        }
    }
    if tables
        .iter()
        .any(|table| table == "history_acknowledgements")
    {
        let mut cursor = (String::new(), String::new());
        loop {
            let rows: Vec<(String, String, Vec<u8>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT peer,name,version FROM history_acknowledgements WHERE (peer,name)>(?1,?2) AND ({}) ORDER BY peer,name LIMIT ?3", history::predicate())))
                .bind(&cursor.0).bind(&cursor.1).bind(CONNECT_PAGE_SIZE as i64).fetch_all(&mut *source).await?;
            let Some(last) = rows.last() else { break };
            cursor = (last.0.clone(), last.1.clone());
            let versions = rows
                .into_iter()
                .map(|(peer, name, version)| Ok((peer, name, VersionVector::decode(&version)?)))
                .collect::<Result<Vec<_>>>()?;
            history::acknowledge_page_on(target, &versions).await?;
        }
    }
    Ok(())
}

async fn file_publisher(connection: &mut SqliteConnection) -> Result<Option<String>> {
    let present: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('connect_profile') WHERE name='publisher')",
    )
    .fetch_one(&mut *connection)
    .await?;
    if !present {
        return Ok(None);
    }
    Ok(sqlx::query_scalar::<_, Option<String>>(
        "SELECT publisher FROM connect_profile WHERE singleton=1",
    )
    .fetch_one(connection)
    .await?
    .filter(|publisher| !publisher.is_empty()))
}

async fn acknowledge_snapshot_on(connection: &mut SqliteConnection, peer: &str) -> Result<()> {
    let publisher = file_publisher(connection)
        .await?
        .or_else(|| (!peer.is_empty()).then(|| peer.to_owned()));
    if let Some(publisher) = publisher {
        history::register_on(connection, std::slice::from_ref(&publisher)).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT OR REPLACE INTO history_acknowledgements(peer,name,version) SELECT ?1,name,version FROM documents WHERE {}", history::predicate()
        )))
            .bind(&publisher).execute(&mut *connection).await?;
        sqlx::query("INSERT INTO sync_cursors(peer,setup,revision) SELECT ?1,value,(SELECT revision FROM file_revision WHERE id=1) FROM json_each('[0,1,2,3]') WHERE true ON CONFLICT(peer,setup) DO UPDATE SET revision=max(revision,excluded.revision)")
            .bind(&publisher).execute(connection).await?;
    }
    Ok(())
}

async fn validate_database(connection: &mut SqliteConnection) -> Result<()> {
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await?;
    if !matches!(version, 48 | 49 | 50 | 51 | 53 | 54 | 55 | 56 | 57 | 58) {
        bail!(INCOMPATIBLE_PROFILE);
    }
    let profile: i64 =
        sqlx::query_scalar("SELECT schema_version FROM connect_profile WHERE singleton=1")
            .fetch_one(&mut *connection)
            .await?;
    if !matches!(profile, 1 | 2) {
        bail!(INCOMPATIBLE_PROFILE);
    }
    Ok(())
}

async fn validate_base_database(connection: &mut SqliteConnection) -> Result<()> {
    let complete: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('sources','tracks','playlists','listens','local_files')")
        .fetch_one(connection).await?;
    if complete != 5 {
        bail!("Import the profile's base file first");
    }
    Ok(())
}

async fn rewrap(
    connection: &mut SqliteConnection,
    from: &age::x25519::Identity,
    to: &age::x25519::Identity,
) -> Result<()> {
    let same_key = from.to_public() == to.to_public();
    let frames: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='updates')",
    )
    .fetch_one(&mut *connection)
    .await?;
    let (table, column) = if frames {
        ("updates", "payload")
    } else {
        ("documents", "snapshot")
    };
    let mut cursor = String::new();
    loop {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT name,{column} FROM {table} WHERE substr(name,1,instr(name,':')-1) IN ('source','integration','scrobbling','connect_key') AND name>?1 ORDER BY name LIMIT ?2")))
            .bind(&cursor).bind(CONNECT_PAGE_SIZE as i64).fetch_all(&mut *connection).await?;
        let Some(last) = rows.last() else { break };
        cursor = last.get(0);
        for row in rows {
            let name: String = row.get(0);
            let bytes: Vec<u8> = row.get(1);
            let encrypted = if frames {
                let mut update: Update = serde_json::from_slice(&bytes)?;
                let plaintext = expose(&name, &update.bytes, from)?;
                if same_key {
                    continue;
                }
                update.bytes = protect(&name, &plaintext, to)?.into_owned();
                serde_json::to_vec(&update)?
            } else {
                let plaintext = expose(&name, &bytes, from)?;
                if same_key {
                    continue;
                }
                protect(&name, &plaintext, to)?.into_owned()
            };
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE {table} SET {column}=?2 WHERE name=?1"
            )))
            .bind(name)
            .bind(encrypted)
            .execute(&mut *connection)
            .await?;
        }
    }
    let tables:Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type='table' AND name IN('projection','local_values','exported_settings')")
        .fetch_all(&mut *connection).await?;
    for table in tables {
        let mut cursor = (String::new(), String::new());
        loop {
            let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT kind,object_key,payload FROM {table} WHERE kind IN ('source','integration','scrobbling','connect_key') AND (kind,object_key)>(?1,?2) ORDER BY kind,object_key LIMIT ?3")))
                .bind(&cursor.0).bind(&cursor.1).bind(CONNECT_PAGE_SIZE as i64).fetch_all(&mut *connection).await?;
            let Some(last) = rows.last() else { break };
            cursor = (last.get(0), last.get(1));
            for row in rows {
                let kind: String = row.get(0);
                let plaintext = expose_payload(&kind, row.get(2), from)?;
                if same_key {
                    continue;
                }
                let payload = protect_payload(&kind, plaintext, to)?;
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE {table} SET payload=?3 WHERE kind=?1 AND object_key=?2"
                )))
                .bind(kind)
                .bind(row.get::<String, _>(1))
                .bind(payload)
                .execute(&mut *connection)
                .await?;
            }
        }
    }
    // Secret rewrapping does not change edit versions.
    Ok(())
}

impl ProfileStore {
    pub async fn read_export_settings(
        path: &Path,
        identity: &age::x25519::Identity,
    ) -> Result<(Vec<crate::StampedRecord>, Vec<ConnectRecord>)> {
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new().filename(path).read_only(true),
        )
        .await?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='exported_settings')").fetch_one(&mut connection).await?;
        let result = if exists {
            let mut shared = Vec::new();
            let mut personal = Vec::new();
            for row in
                sqlx::query("SELECT kind,object_key,stamp,personal,payload FROM exported_settings")
                    .fetch_all(&mut connection)
                    .await?
            {
                let kind: String = row.get(0);
                let record = ConnectRecord {
                    value: expose_payload(&kind, row.get(4), identity)?
                        .map(|value| serde_json::from_str(&value))
                        .transpose()?,
                    kind,
                    key: row.get(1),
                };
                if row.get::<bool, _>(3) {
                    personal.push(record)
                } else {
                    shared.push(crate::StampedRecord {
                        record,
                        stamp: serde_json::from_str(row.get::<&str, _>(2))?,
                    })
                }
            }
            (shared, personal)
        } else {
            (
                legacy_settings_on(&mut connection, identity).await?,
                Vec::new(),
            )
        };
        connection.close().await?;
        Ok(result)
    }

    pub async fn legacy_settings_records(
        &self,
    ) -> Result<(Vec<crate::StampedRecord>, Vec<ConnectRecord>)> {
        let mut connection = self.writer().await?;
        let records = legacy_settings_on(&mut connection, &self.identity).await?;
        let mut baseline = Vec::new();
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='local_values')",
        )
        .fetch_one(&mut *connection)
        .await?;
        if exists {
            for row in sqlx::query("SELECT kind,object_key,payload FROM local_values WHERE kind IN ('source','integration','preference','scrobbling')")
                .fetch_all(&mut *connection).await? {
                let kind: String = row.get(0);
                baseline.push(ConnectRecord {
                    value: expose_payload(&kind, row.get(2), &self.identity)?
                        .map(|value| serde_json::from_str(&value)).transpose()?,
                    kind,
                    key: row.get(1),
                });
            }
        }
        Ok((records, baseline))
    }

    pub async fn clear_legacy_settings(&self) -> Result<()> {
        let mut connection = self.writer().await?;
        sqlx::raw_sql("DELETE FROM documents WHERE substr(name,1,instr(name,':')-1) IN ('source','integration','preference','scrobbling'); DROP TABLE IF EXISTS local_values;")
            .execute(&mut *connection).await?;
        Ok(())
    }
}

async fn legacy_settings_on(
    connection: &mut SqliteConnection,
    identity: &age::x25519::Identity,
) -> Result<Vec<crate::StampedRecord>> {
    let documents: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='documents')",
    )
    .fetch_one(&mut *connection)
    .await?;
    if !documents {
        return Ok(Vec::new());
    }
    let mut output = Vec::new();
    for row in sqlx::query("SELECT name,snapshot FROM documents WHERE substr(name,1,instr(name,':')-1) IN ('source','integration','preference','scrobbling')").fetch_all(connection).await? {
        let name:String=row.get(0);
        output.extend(scalar_snapshot(&expose(&name,row.get(1),identity)?)?);
    }
    Ok(output)
}
