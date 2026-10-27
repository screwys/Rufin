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
            validate_base_database(&mut transaction).await?;
            crate::schema::initialize_durable(&mut transaction).await?;
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
    pub async fn install_prepared_database(catalog: Catalog, path: &Path) -> Result<()> {
        let mut incoming =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path)).await?;
        let installed = async {
            let mut transaction = incoming.begin().await?;
            acknowledge_snapshot_on(&mut transaction, "").await?;
            transaction.commit().await?;
            catalog.loaded().await;
            let kept = {
                let mut writer = catalog.writer().await?;
                let (_export, _reader) = catalog.acquire_export().await?;
                let kept = Catalog::downloaded_references_on(&mut writer).await?;
                // Memory is behind the Store until the reload below lands.
                catalog.require_reload();
                catalog.forget_image();
                Catalog::connect_adopt_database_on(&mut writer, &mut incoming).await?;
                let revision = catalog.reload_under(&mut writer).await?;
                catalog.publish(Arc::new(crate::Change {
                    revision,
                    scopes: vec![crate::Scope::Reload],
                }));
                kept
            };
            let mut write = catalog.write().await?;
            Catalog::keep_downloaded_references(&mut write, &kept).await?;
            write.commit().await?;
            Result::<()>::Ok(())
        }
        .await;
        incoming.close().await?;
        installed
    }

    /// Pin the committed application Store before copying.
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
                let (_export, mut reader) = self.catalog.acquire_export().await?;
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
            Catalog::connect_prepare_export_on(&mut transaction).await?;
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
        let mut changed = false;
        let mut cursor = String::new();
        loop {
            let rows: Vec<(String, Vec<u8>)> = sqlx::query_as("SELECT name,snapshot FROM documents WHERE name>?1 AND name<=coalesce((SELECT min(name) FROM documents WHERE name>?1 AND name GLOB 'playlist_artwork:*'),char(1114111)) ORDER BY name LIMIT ?2")
                .bind(&cursor).bind(CONNECT_PAGE_SIZE as i64).fetch_all(&mut *incoming).await?;
            let Some(last) = rows.last() else { break };
            cursor = last.0.clone();
            let updates = rows
                .iter()
                .map(|(name, bytes)| {
                    Ok(Update {
                        version: FORMAT,
                        document: name.clone(),
                        bytes: expose(name, bytes, transport_key)?.into_owned(),
                        current: None,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let mut writer = self.writer().await?;
            let mut write = writer.begin().await?;
            let (applied, versions) = self.import_page_on(&mut write, updates).await?;
            changed |= applied;
            if let Some(publisher) = publisher {
                let acknowledgements = rows
                    .into_iter()
                    .zip(versions)
                    .map(|((name, _), version)| (publisher.to_owned(), name, version))
                    .collect::<Vec<_>>();
                history::acknowledge_page_on(&mut write, &acknowledgements).await?;
            }
            write.commit().await?;
        }
        Ok(changed)
    }
}

async fn merge_receiver_metadata_on(
    source: &mut SqliteConnection,
    target: &mut SqliteConnection,
) -> Result<()> {
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
    Ok(())
}

async fn file_publisher(connection: &mut SqliteConnection) -> Result<Option<String>> {
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
    if version != crate::schema::STORE_VERSION {
        bail!(INCOMPATIBLE_PROFILE);
    }
    let profile: i64 =
        sqlx::query_scalar("SELECT schema_version FROM connect_profile WHERE singleton=1")
            .fetch_one(&mut *connection)
            .await?;
    if profile != SCHEMA_VERSION {
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
    let mut cursor = String::new();
    loop {
        let rows = sqlx::query("SELECT name,snapshot FROM documents WHERE substr(name,1,instr(name,':')-1) IN ('source','integration','scrobbling','connect_key') AND name>?1 ORDER BY name LIMIT ?2")
            .bind(&cursor).bind(CONNECT_PAGE_SIZE as i64).fetch_all(&mut *connection).await?;
        let Some(last) = rows.last() else { break };
        cursor = last.get(0);
        for row in rows {
            let name: String = row.get(0);
            let plaintext = expose(&name, row.get(1), from)?;
            if same_key {
                continue;
            }
            sqlx::query("UPDATE documents SET snapshot=?2 WHERE name=?1")
                .bind(&name)
                .bind(protect(&name, &plaintext, to)?.into_owned())
                .execute(&mut *connection)
                .await?;
        }
    }
    let mut cursor = (String::new(), String::new());
    loop {
        let rows = sqlx::query("SELECT kind,object_key,payload FROM exported_settings WHERE kind IN ('source','integration','scrobbling','connect_key') AND (kind,object_key)>(?1,?2) ORDER BY kind,object_key LIMIT ?3")
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
            sqlx::query("UPDATE exported_settings SET payload=?3 WHERE kind=?1 AND object_key=?2")
                .bind(kind)
                .bind(row.get::<String, _>(1))
                .bind(payload)
                .execute(&mut *connection)
                .await?;
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
        let mut shared = Vec::new();
        let mut personal = Vec::new();
        if exists {
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
        }
        connection.close().await?;
        Ok((shared, personal))
    }
}
