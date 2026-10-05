//! Cache exchange follows profile setup and uses the same shared documents.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

impl ConnectOwner {
    pub(super) async fn export_cache_session(
        &self,
        session: &Session,
    ) -> Result<(tempfile::NamedTempFile, i64), String> {
        let key = self
            .secret(file_key(&self.identity_reference()?, &session.profile))
            .await?
            .ok_or("The Connect file key is unavailable")?;
        let output = tempfile::NamedTempFile::new_in(&self.directory).map_err(error)?;
        let writer = portable::encrypt(output.reopen().map_err(error)?, &session.profile, &key)?;
        let (writer, revision) = session
            .documents
            .export_cache_snapshot(writer, &session.identity)
            .await
            .map_err(error)?;
        let members = match self.network.lock().await.as_ref() {
            Some(network) => network.members().await.map_err(error)?,
            None => vec![session.identity.clone()],
        };
        let writer = session
            .documents
            .finish_device_snapshot(writer, &members)
            .await
            .map_err(error)?;
        tokio::task::spawn_blocking(move || writer.finish().map_err(error))
            .await
            .map_err(error)??;
        Ok((output, revision))
    }

    pub(super) fn start_cache_sync(self: &Arc<Self>, session: Arc<Session>) {
        let weak = Arc::downgrade(self);
        let mut changes = self.source.shared.artwork.subscribe_cache_changes();
        let mut lyrics = self.playback.lyrics.handle().current();
        let mut status = self.subscribe();
        self.runtime.spawn(async move {
            session.stop.run_until_cancelled(async {
                let mut entries = None;
                let mut seeded = false;
                let mut initial_seed = true;
                let mut removals: Option<String> = None;
                loop {
                    let Some(owner) = weak.upgrade() else { return };
                    let settings = owner.status().settings;
                    let mut busy = false;
                    if settings.enabled && !settings.setup_pending && !settings.adopting
                        && !owner.joining.load(std::sync::atomic::Ordering::Acquire)
                    {
                        let result = async {
                            let pending = owner.apply_cache(&session).await?;
                            let seeded_lyrics = owner.database.connect_seed_cache_page().await.map_err(error)?;
                            if seeded_lyrics {
                                owner.synchronize().await?;
                            }
                            if !seeded {
                                if entries.is_none() {
                                    let artwork = owner.source.shared.artwork.clone();
                                    entries = Some(tokio::task::spawn_blocking(move || artwork.cache_entries())
                                        .await.map_err(error)?.map_err(error)?);
                                }
                                let mut iterator = entries.take().unwrap();
                                let (next, iterator) = tokio::task::spawn_blocking(move || (iterator.next(), iterator))
                                    .await.map_err(error)?;
                                entries = Some(iterator);
                                if let Some(entry) = next {
                                    let entry = entry.map_err(error)?;
                                    if !initial_seed || session.documents.cache_record("cache_artwork", &entry.key).await.map_err(error)?.is_none() {
                                        owner.publish_cache_file(&session, &entry.key).await?;
                                    }
                                    return Ok::<_, String>(true);
                                }
                                entries = None;
                                seeded = true;
                                initial_seed = false;
                                owner.exchange_session(&session, true).await?;
                            }
                            if let Some(cursor) = &removals {
                                let keys = session.documents.cache_keys(cursor, library::CONNECT_PAGE_SIZE).await.map_err(error)?;
                                if keys.is_empty() { removals = None; } else {
                                    removals = keys.last().cloned();
                                    for key in keys {
                                        let artwork = owner.source.shared.artwork.clone();
                                        let inspect = key.clone();
                                        if tokio::task::spawn_blocking(move || artwork.cache_file(&inspect)).await.map_err(error)?.map_err(error)?.is_none() {
                                            owner.publish_cache_file(&session, &key).await?;
                                        }
                                    }
                                    return Ok(true);
                                }
                            }
                            Ok(seeded_lyrics || pending)
                        }.await;
                        match result {
                            Ok(value) => busy = value,
                            Err(error) => tracing::warn!(%error, "Could not exchange shared cache"),
                        }
                    }
                    drop(owner);
                    tokio::select! {
                        change = changes.recv() => {
                            let Some(owner) = weak.upgrade() else { return };
                            let settings = owner.status().settings;
                            if !settings.enabled || settings.setup_pending || settings.adopting {
                                seeded = false;
                                entries = None;
                                if !initial_seed { removals = Some(String::new()); }
                                continue;
                            }
                            let mut change = change;
                            for index in 0..library::CONNECT_PAGE_SIZE {
                            match change {
                                Ok(artwork::ArtworkCacheChange::Written(key) | artwork::ArtworkCacheChange::Removed(key)) => {
                                    if let Err(error) = owner.publish_cache_file(&session, &key).await {
                                        tracing::warn!(%error, "Could not publish shared artwork cache");
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                    seeded = false;
                                    entries = None;
                                    initial_seed = false;
                                    removals = Some(String::new());
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                            }
                            if index + 1 == library::CONNECT_PAGE_SIZE { break; }
                            match changes.try_recv() {
                                Ok(next) => change = Ok(next),
                                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => change = Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)),
                                Err(_) => break,
                            }
                            }
                            if let Err(error) = owner.exchange_session(&session, true).await {
                                tracing::warn!(%error, "Could not exchange shared cache files");
                            }
                        }
                        _ = lyrics.changed() => {
                            if let Some(owner) = weak.upgrade() {
                                let settings = owner.status().settings;
                                if settings.enabled && !settings.setup_pending && !settings.adopting {
                                    if let Err(error) = owner.synchronize().await {
                                        tracing::warn!(%error, "Could not publish shared lyrics cache");
                                    } else if let Err(error) = owner.exchange_session(&session, true).await {
                                        tracing::warn!(%error, "Could not exchange shared lyrics cache");
                                    }
                                }
                            }
                        }
                        _ = session.cache_changed.notified() => {},
                        _ = status.changed() => {},
                        _ = tokio::task::yield_now(), if busy => {},
                        _ = tokio::time::sleep(Duration::from_secs(300)) => {
                            if let Some(owner) = weak.upgrade() {
                                let settings = owner.status().settings;
                                if settings.enabled && !settings.setup_pending && !settings.adopting
                                    && let Err(error) = owner.exchange_session(&session, true).await
                                { tracing::warn!(%error, "Could not exchange shared cache files"); }
                            }
                        },
                    }
                }
            }).await;
        });
    }

    async fn publish_cache_file(&self, session: &Session, key: &str) -> Result<(), String> {
        let artwork = self.source.shared.artwork.clone();
        let key = key.to_owned();
        let record = tokio::task::spawn_blocking(move || {
            let value = artwork
                .cache_file(&key)
                .map_err(error)?
                .map(std::fs::read)
                .transpose()
                .map_err(error)?
                .map(|bytes| serde_json::json!({"bytes":STANDARD.encode(bytes)}));
            Ok::<_, String>(ConnectRecord {
                kind: "cache_artwork".into(),
                key,
                value,
            })
        })
        .await
        .map_err(error)??;
        if session
            .documents
            .write_records(&[record])
            .await
            .map_err(error)?
            > 0
        {
            if let Some(network) = self.network.lock().await.as_ref() {
                network.refresh();
            }
        }
        Ok(())
    }

    async fn apply_cache(&self, session: &Session) -> Result<bool, String> {
        let mut artwork_changed = false;
        for _ in 0..16 {
            let (records, _) = session
                .documents
                .project_cache(&self.database)
                .await
                .map_err(error)?;
            if records.is_empty() {
                break;
            }
            self.playback.lyrics.handle().cache_changed(&records);
            for record in records
                .iter()
                .filter(|record| record.kind == "cache_artwork")
            {
                let artwork = self.source.shared.artwork.clone();
                let record = record.clone();
                artwork_changed |= tokio::task::spawn_blocking(move || match record.value {
                    Some(value) => {
                        let bytes = STANDARD
                            .decode(
                                value["bytes"]
                                    .as_str()
                                    .ok_or("Shared artwork is missing its image")?,
                            )
                            .map_err(error)?;
                        artwork
                            .import_cache_file(&record.key, &bytes)
                            .map_err(error)
                    }
                    None => artwork.remove_cache_file(&record.key).map_err(error),
                })
                .await
                .map_err(error)??;
            }
            session
                .documents
                .acknowledge_projection(&records)
                .await
                .map_err(error)?;
            tokio::task::yield_now().await;
        }
        if artwork_changed {
            self.source.shared.artwork.invalidate_decoded_cache();
            self.source.connect_artwork_changed().await;
        }
        session
            .documents
            .cache_projection_pending()
            .await
            .map_err(error)
    }
}
