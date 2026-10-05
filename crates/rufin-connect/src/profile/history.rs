use super::*;

impl ProfileStore {
    pub async fn register_members(&self, members: &[String]) -> Result<()> {
        let mut connection = database_connection(&self.connection).await?;
        let mut transaction = connection.begin().await?;
        register_on(&mut transaction, members).await?;
        transaction.commit().await?;
        Ok(())
    }

    /// These versions describe committed remote state, not data merely sent to it.
    pub async fn acknowledge_versions(
        &self,
        peer: &str,
        versions: &[DocumentVersion],
        members: &[String],
    ) -> Result<()> {
        let mut connection = database_connection(&self.connection).await?;
        let mut transaction = connection.begin().await?;
        register_on(&mut transaction, members).await?;
        register_on(&mut transaction, &[peer.to_owned()]).await?;
        for page in versions.chunks(CONNECT_PAGE_SIZE) {
            let page = page
                .iter()
                .map(|version| {
                    Ok((
                        peer.to_owned(),
                        version.name.clone(),
                        VersionVector::decode(&version.version)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            acknowledge_page_on(&mut transaction, &page).await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    /// Include membership observed after the snapshot, so a newly enrolled device
    /// cannot disappear from the acknowledgement used by another device to prune.
    pub async fn finish_device_snapshot<W: Write + Send + 'static>(
        &self,
        mut output: W,
        members: &[String],
    ) -> Result<W> {
        let mut connection = database_connection(&self.connection).await?;
        register_on(&mut connection, members).await?;
        let members = participants(&mut connection).await?;
        drop(connection);
        tokio::task::spawn_blocking(move || -> Result<W> {
            let mut membership = GzEncoder::new(&mut output, Compression::default());
            serde_json::to_writer(&mut membership, &members)?;
            membership.write_all(b"\n")?;
            membership.finish()?;
            output.flush()?;
            Ok(output)
        })
        .await?
    }

    /// Process a bounded page without projecting values or invalidating the UI.
    /// Only exact convergence is sufficient: intersecting versions could discard
    /// a peer's concurrent edits that this device has not received yet.
    pub async fn prune_history(
        &self,
        identity: &str,
        members: &[String],
        limit: usize,
    ) -> Result<usize> {
        let mut connection = database_connection(&self.connection).await?;
        let mut transaction = connection.begin().await?;
        register_on(&mut transaction, members).await?;
        register_on(&mut transaction, &[identity.to_owned()]).await?;
        let peers = participants(&mut transaction).await?;
        let names: Vec<String> =
            sqlx::query_scalar("SELECT name FROM history_pending ORDER BY name LIMIT ?1")
                .bind(i64::try_from(limit)?)
                .fetch_all(&mut *transaction)
                .await?;
        let mut changed = false;
        for name in &names {
            sqlx::query("DELETE FROM history_pending WHERE name=?1")
                .bind(name)
                .execute(&mut *transaction)
                .await?;
            let Some(row) = sqlx::query("SELECT snapshot,version FROM documents WHERE name=?1")
                .bind(name)
                .fetch_optional(&mut *transaction)
                .await?
            else {
                continue;
            };
            let version = VersionVector::decode(&row.get::<Vec<u8>, _>(1))?;
            let mut converged = true;
            for peer in peers.iter().filter(|peer| peer.as_str() != identity) {
                let acknowledged: Option<Vec<u8>> = sqlx::query_scalar(
                    "SELECT version FROM history_acknowledgements WHERE peer=?1 AND name=?2",
                )
                .bind(peer)
                .bind(name)
                .fetch_optional(&mut *transaction)
                .await?;
                if acknowledged
                    .as_deref()
                    .map(VersionVector::decode)
                    .transpose()?
                    .as_ref()
                    != Some(&version)
                {
                    converged = false;
                    break;
                }
            }
            if !converged {
                continue;
            }
            let snapshot: Vec<u8> = row.get(0);
            let identity = Arc::clone(&self.identity);
            let document_name = name.clone();
            let pruned = tokio::task::spawn_blocking(move || -> Result<_> {
                let document = LoroDoc::new();
                let plaintext = expose(&document_name, &snapshot, &identity)?;
                document.import(&plaintext)?;
                let pruned =
                    document.export(ExportMode::shallow_snapshot(&document.oplog_frontiers()))?;
                if pruned.len() < plaintext.len() {
                    Ok(Some(
                        protect(&document_name, &pruned, &identity)?.into_owned(),
                    ))
                } else {
                    Ok(None)
                }
            })
            .await??;
            let Some(pruned) = pruned else {
                continue;
            };
            sqlx::query("UPDATE documents SET snapshot=?2 WHERE name=?1")
                .bind(name)
                .bind(pruned)
                .execute(&mut *transaction)
                .await?;
            changed = true;
        }
        if changed {
            // Replication versions and projected values stay unchanged. Only the
            // file needs republishing with its smaller representation.
            next_revision(&mut transaction).await?;
        }
        transaction.commit().await?;
        Ok(names.len())
    }
}

pub(super) async fn register_on(
    connection: &mut SqliteConnection,
    members: &[String],
) -> Result<()> {
    let first: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM history_peers)")
        .fetch_one(&mut *connection)
        .await?;
    let mut added = false;
    for peer in members {
        added |= sqlx::query("INSERT OR IGNORE INTO history_peers VALUES(?1)")
            .bind(peer)
            .execute(&mut *connection)
            .await?
            .rows_affected()
            > 0;
    }
    if added && first {
        // Establish acknowledgements even if earlier sessions already advanced
        // their receive cursors. Version exchange skips data already present.
        sqlx::query("DELETE FROM sync_cursors")
            .execute(&mut *connection)
            .await?;
        sqlx::query("INSERT OR IGNORE INTO history_pending SELECT name FROM documents")
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

pub(super) async fn acknowledge_page_on(
    connection: &mut SqliteConnection,
    versions: &[(String, String, VersionVector)],
) -> Result<()> {
    for page in versions.chunks(CONNECT_PAGE_SIZE) {
        let mut query = QueryBuilder::<sqlx::Sqlite>::new("WITH requested(peer,name) AS (");
        query.push_values(page, |mut values, (peer, name, _)| {
            values.push_bind(peer).push_bind(name);
        });
        query.push(") SELECT DISTINCT h.peer,h.name,h.version FROM requested r JOIN history_acknowledgements h ON h.peer=r.peer AND h.name=r.name");
        let mut acknowledged = BTreeMap::new();
        for row in query.build().fetch_all(&mut *connection).await? {
            acknowledged.insert(
                (row.get::<String, _>(0), row.get::<String, _>(1)),
                VersionVector::decode(&row.get::<Vec<u8>, _>(2))?,
            );
        }
        let mut changed = BTreeSet::new();
        for (peer, name, version) in page {
            let key = (peer.clone(), name.clone());
            let previous = acknowledged.entry(key.clone()).or_default();
            if !previous.includes_vv(version) {
                previous.merge(version);
                changed.insert(key);
            }
        }
        if changed.is_empty() {
            continue;
        }
        let mut upsert = QueryBuilder::<sqlx::Sqlite>::new(
            "INSERT INTO history_acknowledgements(peer,name,version) ",
        );
        upsert.push_values(&changed, |mut values, key| {
            values
                .push_bind(&key.0)
                .push_bind(&key.1)
                .push_bind(acknowledged[key].encode());
        });
        upsert.push(" ON CONFLICT(peer,name) DO UPDATE SET version=excluded.version");
        upsert.build().execute(&mut *connection).await?;
        let mut pending =
            QueryBuilder::<sqlx::Sqlite>::new("INSERT OR IGNORE INTO history_pending(name) ");
        pending.push_values(&changed, |mut values, (_, name)| {
            values.push_bind(name);
        });
        pending.build().execute(&mut *connection).await?;
    }
    Ok(())
}

async fn participants(connection: &mut SqliteConnection) -> Result<Vec<String>> {
    let mut peers: BTreeSet<String> = sqlx::query_scalar("SELECT peer FROM history_peers")
        .fetch_all(&mut *connection)
        .await?
        .into_iter()
        .collect();
    // Read device documents directly: a removed device must stay removed even
    // when bootstrapping from a pruned snapshot with no projected old value.
    for row in sqlx::query(
        "SELECT name,snapshot FROM documents WHERE substr(name,1,instr(name,':')-1)='device'",
    )
    .fetch_all(&mut *connection)
    .await?
    {
        let name: String = row.get(0);
        let peer = name.strip_prefix("device:").unwrap();
        let document = LoroDoc::new();
        document.import(&row.get::<Vec<u8>, _>(1))?;
        if records(&document)?.contains_key(&("device".into(), peer.into())) {
            peers.insert(peer.into());
        } else {
            peers.remove(peer);
        }
    }
    Ok(peers.into_iter().collect())
}
