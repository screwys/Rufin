//! Ordinary cache files are shared on demand and published independently.
use super::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum CacheRequest {
    Artwork {
        family: String,
        size: Option<u32>,
        fallback: bool,
    },
    Lyrics {
        query: LyricsCacheQuery,
    },
    MediaAssets {
        uri: String,
    },
}

#[derive(Serialize, Deserialize)]
struct ImageOffer {
    key: String,
    hash: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct LyricsCacheQuery {
    pub media_uri: String,
    pub authority: String,
    pub role: String,
    pub language: String,
    pub script: String,
    pub input: [u8; 32],
}

impl LyricsCacheQuery {
    fn identity(&self) -> String {
        if self.authority == "source" {
            format!(
                "lyrics:source:{}:{}",
                self.media_uri,
                blake3::Hash::from_bytes(self.input)
            )
        } else {
            serde_json::to_string(&(
                &self.media_uri,
                &self.authority,
                &self.role,
                &self.language,
                &self.script,
            ))
            .expect("lyrics cache key")
        }
    }

    fn accepts(&self, row: &library::LyricsCacheRow) -> bool {
        row.media_uri == self.media_uri
            && row.authority == self.authority
            && if self.authority == "source" {
                row.cache_input_digest == self.input
            } else {
                row.role == self.role && row.language == self.language && row.script == self.script
            }
    }
}

impl From<&library::LyricsCacheRow> for LyricsCacheQuery {
    fn from(row: &library::LyricsCacheRow) -> Self {
        Self {
            media_uri: row.media_uri.clone(),
            authority: row.authority.clone(),
            role: row.role.clone(),
            language: row.language.clone(),
            script: row.script.clone(),
            input: row.cache_input_digest,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Publication {
    destination: portable::Destination,
    artwork: u64,
    lyrics: i64,
}

fn cache_filename(profile: &str, identity: &str) -> String {
    if let Some(image) = identity.strip_prefix("image:") {
        let (_, image) = image.split_once('/').expect("ordinary image cache key");
        format!(
            "cache/{}/images/{image}.rufin-connect",
            blake3::hash(profile.as_bytes()).to_hex()
        )
    } else {
        format!(
            "cache/{}.rufin-connect",
            blake3::hash(format!("{profile}:{identity}").as_bytes()).to_hex()
        )
    }
}

impl ConnectOwner {
    pub(super) async fn answer_cache(
        &self,
        request: CacheRequest,
    ) -> Result<serde_json::Value, String> {
        match request {
            CacheRequest::Artwork {
                family,
                size,
                fallback,
            } => {
                let query = artwork::CacheImageRequest {
                    family,
                    size: size.map_or(artwork::ImageSize::Original, artwork::ImageSize::Thumbnail),
                    fallback,
                };
                let Some(entry) = self
                    .source
                    .shared
                    .artwork
                    .cached_family_file(&query)
                    .map_err(error)?
                else {
                    return Ok(serde_json::Value::Null);
                };
                let hash = self
                    .connected_network()
                    .await?
                    .media()
                    .publish(&entry.path)
                    .await
                    .map_err(error)?;
                serde_json::to_value(ImageOffer {
                    key: entry.key,
                    hash,
                })
                .map_err(error)
            }
            CacheRequest::Lyrics { query } => {
                let row = self
                    .database
                    .lyrics_cache_for_role(
                        &query.media_uri,
                        &query.role,
                        &query.language,
                        &query.script,
                        query.input,
                        &query.authority,
                        &library::ReadCancellation::new(),
                    )
                    .await
                    .map_err(error)?;
                serde_json::to_value(row).map_err(error)
            }
            CacheRequest::MediaAssets { uri } => {
                serde_json::to_value(self.media_assets(&uri).await?).map_err(error)
            }
        }
    }

    async fn shared_cache_file(
        &self,
        session: &Session,
        filename: &str,
    ) -> Result<Option<tempfile::TempPath>, String> {
        let Some(destination) = self.status().settings.destination else {
            return Ok(None);
        };
        let folder = portable::ExchangeFolder::new(self, destination)?;
        let incoming = tempfile::NamedTempFile::new_in(&self.directory).map_err(error)?;
        let Some(input) = folder.read_named(filename, incoming.path()).await? else {
            return Ok(None);
        };
        let keys = self.exchange_keys(session).await?;
        let snapshot = self.read_exchange_file(session, input, &keys).await?;
        let output = tempfile::NamedTempFile::new_in(&self.directory).map_err(error)?;
        tokio::task::spawn_blocking(move || {
            std::io::copy(
                &mut snapshot.reader()?,
                &mut output.reopen().map_err(error)?,
            )
            .map_err(error)?;
            Ok(Some(output.into_temp_path()))
        })
        .await
        .map_err(error)?
    }

    pub(super) async fn fetch_cached_artwork(
        &self,
        query: artwork::CacheImageRequest,
    ) -> Option<(String, tempfile::TempPath)> {
        let settings = self.status().settings;
        if !settings.enabled || settings.setup_pending || settings.adopting {
            return None;
        }
        let session = self.active().await.ok()?;
        session
            .stop
            .run_until_cancelled(async {
                for peer in self
                    .status()
                    .devices
                    .into_iter()
                    .filter(|peer| peer.reachable && peer.enrolled)
                {
                    let result = async {
                        let offer: Option<ImageOffer> = serde_json::from_value(
                            self.request(
                                &peer.id,
                                Request::CacheFile {
                                    asset: CacheRequest::Artwork {
                                        family: query.family.clone(),
                                        size: match query.size {
                                            artwork::ImageSize::Original => None,
                                            artwork::ImageSize::Thumbnail(size) => Some(size),
                                        },
                                        fallback: query.fallback,
                                    },
                                },
                            )
                            .await?,
                        )
                        .map_err(error)?;
                        let Some(offer) =
                            offer.filter(|offer| query.select_key([offer.key.as_str()]).is_some())
                        else {
                            return Ok(None);
                        };
                        let output = tempfile::NamedTempFile::new_in(&self.directory)
                            .map_err(error)?
                            .into_temp_path();
                        self.connected_network()
                            .await?
                            .media()
                            .fetch(
                                &peer.id,
                                &offer.hash,
                                &output,
                                session.stop.clone(),
                                tokio::sync::watch::channel(0).0,
                            )
                            .await
                            .map_err(error)?;
                        Ok::<_, String>(Some((offer.key, output)))
                    }
                    .await;
                    match result {
                        Ok(Some(image)) => return Some(image),
                        Ok(None) => {}
                        Err(error) => {
                            tracing::debug!(%error, "Could not read a peer's cached artwork")
                        }
                    }
                }
                let result = async {
                    let Some(destination) = self.status().settings.destination else {
                        return Ok(None);
                    };
                    let folder = portable::ExchangeFolder::new(self, destination)?;
                    let directory = format!(
                        "cache/{}/images/{}",
                        blake3::hash(session.profile.as_bytes()).to_hex(),
                        query.family
                    );
                    let mut keys = Vec::new();
                    folder
                        .visit(&directory, |file| {
                            if let Some(name) = file.name().strip_suffix(".rufin-connect") {
                                let kind = if name.starts_with("original.") {
                                    "originals"
                                } else {
                                    "ready"
                                };
                                keys.push(format!("{kind}/{}/{name}", query.family));
                            }
                            std::future::ready(Ok(()))
                        })
                        .await?;
                    let Some(key) = query.select_key(keys.iter().map(String::as_str)) else {
                        return Ok(None);
                    };
                    Ok::<_, String>(
                        self.shared_cache_file(
                            &session,
                            &cache_filename(&session.profile, &format!("image:{key}")),
                        )
                        .await?
                        .map(|file| (key.to_owned(), file)),
                    )
                }
                .await;
                match result {
                    Ok(image) => image,
                    Err(error) => {
                        tracing::debug!(%error, "Could not read stored artwork");
                        None
                    }
                }
            })
            .await
            .flatten()
    }

    pub(crate) async fn fetch_cached_lyrics(
        &self,
        query: LyricsCacheQuery,
    ) -> Option<library::LyricsCacheRow> {
        let settings = self.status().settings;
        if !settings.enabled || settings.setup_pending || settings.adopting {
            return None;
        }
        let session = self.active().await.ok()?;
        session
            .stop
            .run_until_cancelled(async {
                let mut found = None;
                for peer in self
                    .status()
                    .devices
                    .into_iter()
                    .filter(|peer| peer.reachable && peer.enrolled)
                {
                    let result = self
                        .request(
                            &peer.id,
                            Request::CacheFile {
                                asset: CacheRequest::Lyrics {
                                    query: query.clone(),
                                },
                            },
                        )
                        .await
                        .and_then(|value| {
                            serde_json::from_value::<Option<library::LyricsCacheRow>>(value)
                                .map_err(error)
                        });
                    match result {
                        Ok(Some(row)) if query.accepts(&row) => {
                            found = Some(row);
                            break;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            tracing::debug!(%error, "Could not read a peer's cached lyrics")
                        }
                    }
                }
                if found.is_none() {
                    if let Ok(Some(file)) = self
                        .shared_cache_file(
                            &session,
                            &cache_filename(&session.profile, &query.identity()),
                        )
                        .await
                    {
                        found = tokio::task::spawn_blocking(move || {
                            serde_json::from_reader::<_, library::LyricsCacheRow>(
                                std::fs::File::open(file).map_err(error)?,
                            )
                            .map_err(error)
                        })
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .filter(|row| query.accepts(row));
                    }
                }
                let row = found?;
                if let Err(error) = self
                    .database
                    .import_lyrics_cache(vec![row.clone()], true)
                    .await
                {
                    tracing::warn!(%error, "Could not save shared lyrics");
                }
                Some(row)
            })
            .await
            .flatten()
    }

    async fn publish_cache_file(
        &self,
        session: &Session,
        folder: &portable::ExchangeFolder,
        identity: &str,
        fingerprint: &str,
        input: &std::path::Path,
        key: &str,
    ) -> Result<(), String> {
        if session
            .documents
            .cache_fingerprint(identity)
            .await
            .map_err(error)?
            .as_deref()
            == Some(fingerprint)
        {
            return Ok(());
        }
        let output = tempfile::NamedTempFile::new_in(&self.directory).map_err(error)?;
        let mut writer = portable::encrypt(output.reopen().map_err(error)?, &session.profile, key)?;
        let mut input = match std::fs::File::open(input) {
            Ok(input) => input,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(failure) => return Err(error(failure)),
        };
        let output = tokio::task::spawn_blocking(move || {
            std::io::copy(&mut input, &mut writer).map_err(error)?;
            writer.finish().map_err(error)?;
            Ok::<_, String>(output)
        })
        .await
        .map_err(error)??;
        let filename = cache_filename(&session.profile, identity);
        folder
            .ensure(filename.rsplit_once('/').expect("cache folder").0)
            .await?;
        folder.write(&filename, output).await?;
        session
            .documents
            .save_cache_fingerprint(identity, fingerprint)
            .await
            .map_err(error)
    }

    async fn publish_artwork(
        &self,
        session: &Session,
        folder: &portable::ExchangeFolder,
        image: &str,
        key: &str,
    ) -> Result<(), String> {
        let Some(path) = self
            .source
            .shared
            .artwork
            .cache_file(image)
            .map_err(error)?
        else {
            return Ok(());
        };
        let metadata = match tokio::fs::metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(failure) => return Err(error(failure)),
        };
        let fingerprint = format!(
            "{:?}:{}",
            metadata.modified().map_err(error)?,
            metadata.len()
        );
        self.publish_cache_file(
            session,
            folder,
            &format!("image:{image}"),
            &fingerprint,
            &path,
            key,
        )
        .await
    }

    pub(super) async fn publish_cache(
        self: &Arc<Self>,
        session: &Arc<Session>,
        changed: Option<(&[String], u64)>,
    ) -> Result<(), String> {
        let Some(destination) = self.status().settings.destination else {
            return Ok(());
        };
        let _exchange = session.cache_file_exchange.lock().await;
        let previous = session
            .documents
            .cache_file_exchange()
            .await
            .map_err(error)?
            .and_then(|state| serde_json::from_str::<Publication>(&state).ok())
            .filter(|state| state.destination == destination);
        if previous.is_none() {
            session
                .documents
                .reset_cache_assets()
                .await
                .map_err(error)?;
        }
        let artwork = changed.filter(|_| previous.is_some()).map_or_else(
            || self.source.shared.artwork.cache_write_revision(),
            |(_, revision)| revision.max(previous.as_ref().map_or(0, |state| state.artwork)),
        );
        let lyrics = self.database.lyrics_cache_revision().await.map_err(error)?;
        if previous
            .as_ref()
            .is_some_and(|state| state.artwork == artwork && state.lyrics == lyrics)
        {
            return Ok(());
        }
        let folder = portable::ExchangeFolder::new(self, destination.clone())?;
        folder.ensure("cache").await?;
        let key = self
            .secret(file_key(&self.identity_reference()?, &session.profile))
            .await?
            .ok_or("The Connect file key is unavailable")?;
        if previous
            .as_ref()
            .is_none_or(|state| state.artwork != artwork)
        {
            if let Some((keys, _)) = changed.filter(|_| previous.is_some()) {
                for image in keys {
                    self.publish_artwork(session, &folder, image, &key).await?;
                }
            } else {
                for entry in self.source.shared.artwork.cache_entries().map_err(error)? {
                    self.publish_artwork(session, &folder, &entry.map_err(error)?.key, &key)
                        .await?;
                }
            }
        }
        if previous.as_ref().is_none_or(|state| state.lyrics != lyrics) {
            let mut cursor = previous
                .as_ref()
                .map_or((i64::MIN, 0), |state| (state.lyrics, i64::MAX));
            loop {
                let page = self
                    .database
                    .lyrics_cache_changes(cursor, lyrics)
                    .await
                    .map_err(error)?;
                if page.is_empty() {
                    break;
                }
                for (next, row) in page {
                    let identity = LyricsCacheQuery::from(&row).identity();
                    let bytes = serde_json::to_vec(&row).map_err(error)?;
                    let fingerprint = blake3::hash(&bytes).to_hex().to_string();
                    if session
                        .documents
                        .cache_fingerprint(&identity)
                        .await
                        .map_err(error)?
                        .as_deref()
                        != Some(&fingerprint)
                    {
                        let file =
                            tempfile::NamedTempFile::new_in(&self.directory).map_err(error)?;
                        std::fs::write(file.path(), bytes).map_err(error)?;
                        self.publish_cache_file(
                            session,
                            &folder,
                            &identity,
                            &fingerprint,
                            file.path(),
                            &key,
                        )
                        .await?;
                    }
                    cursor = next;
                }
            }
        }
        session
            .documents
            .save_cache_file_exchange(
                &serde_json::to_string(&Publication {
                    destination,
                    artwork,
                    lyrics,
                })
                .map_err(error)?,
            )
            .await
            .map_err(error)
    }

    pub(super) fn start_cache_sync(self: &Arc<Self>, session: Arc<Session>) {
        let weak = Arc::downgrade(self);
        let mut writes = self.source.shared.artwork.subscribe_cache_writes();
        let mut lyrics = self.database.subscribe_lyrics_cache_writes();
        let mut status = self.subscribe();
        self.runtime.spawn(async move {
            session.stop.run_until_cancelled(async {
                let mut changed = None;
                let mut needs_scan = true;
                let mut failed = false;
                loop {
                    let Some(owner) = weak.upgrade() else { return };
                    let settings = owner.status().settings;
                    if settings.enabled && settings.destination.is_some() && !settings.setup_pending && !settings.adopting
                        && !owner.joining.load(std::sync::atomic::Ordering::Acquire)
                    {
                        let batch = if needs_scan { None } else { changed.as_ref().map(|(keys, revision): &(Vec<String>, u64)| (keys.as_slice(), *revision)) };
                        match owner.publish_cache(&session, batch).await {
                            Ok(()) => { needs_scan = false; failed = false; },
                            Err(error) => { needs_scan = true; failed = true; tracing::warn!(%error, "Could not publish shared cache"); },
                        }
                    }
                    drop(owner);
                    changed = None;
                    tokio::select! {
                        result = writes.recv() => {
                            let mut keys = BTreeSet::new();
                            let mut complete = true;
                            let mut revision = 0;
                            let mut result = result;
                            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
                            loop {
                                match result {
                                    Ok((key, written_revision)) if complete => {
                                        revision = revision.max(written_revision); keys.insert(key);
                                        if keys.len() > 256 { keys.clear(); complete = false; }
                                    },
                                    Ok(_) => {},
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => { keys.clear(); complete = false; },
                                }
                                if tokio::time::Instant::now() >= deadline { break; }
                                match tokio::time::timeout_at(deadline, writes.recv()).await { Ok(next) => result = next, Err(_) => break }
                            }
                            changed = complete.then(|| (keys.into_iter().collect::<Vec<_>>(), revision));
                        },
                        _ = lyrics.changed() => {},
                        _ = session.cache_changed.notified() => {},
                        _ = status.changed() => {},
                        _ = tokio::time::sleep(Duration::from_secs(300)), if failed => {},
                    }
                }
            }).await;
        });
    }
}
