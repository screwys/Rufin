//! Shared track identity with device-owned files and encoding choices.
use super::*;
use std::{
    io::{Read, Write},
    path::{Component, Path},
};
use tokio_util::sync::CancellationToken;

impl Encoding {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Mp3 => "mp3",
        }
    }
}

#[derive(Serialize, Deserialize)]
struct OfferedMedia {
    hash: Option<String>,
    revision: String,
    encoding: Encoding,
    size: u64,
}

fn revision(reference: &serde_json::Value) -> String {
    reference["revision"].to_string()
}

fn file_uri(path: &Path) -> Result<String, String> {
    url::Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|()| "Media path must be absolute".into())
}

fn file_path(file: &library::ConnectMediaFile) -> Option<PathBuf> {
    library::file_media_path(&file.path).filter(|path| path.is_file())
}

/// A shared path is source-relative; it cannot select a file outside the root.
fn corresponding_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err("The shared track has an invalid source-relative path".into());
    }
    Ok(root.join(relative))
}

fn versioned_path(path: &Path, uri: &str, revision: &str, encoding: Encoding) -> PathBuf {
    let key = media_key(uri, revision, encoding);
    let extension = path.extension().unwrap_or_default().to_string_lossy();
    path.with_extension(format!("{key:.12}.{extension}"))
}

fn media_key(uri: &str, revision: &str, encoding: Encoding) -> String {
    blake3::hash(format!("{uri}\0{revision}\0{}", encoding.name()).as_bytes())
        .to_hex()
        .to_string()
}

fn serving_key(peer: &str, id: &str) -> String {
    format!("serve/{peer}/{id}")
}

fn mapped_root<'a>(
    folders: &'a BTreeMap<String, PathBuf>,
    reference: &serde_json::Value,
) -> Option<&'a PathBuf> {
    let source = reference["source_id"].as_str()?;
    reference["root_id"]
        .as_str()
        .and_then(|root| folders.get(&format!("{source}/{root}")))
        .or_else(|| {
            (reference["root_count"].as_u64().unwrap_or(1) <= 1)
                .then(|| folders.get(source))
                .flatten()
        })
}

impl ConnectOwner {
    fn selected_media_path(
        &self,
        uri: &str,
        encoding: Encoding,
        reference: &serde_json::Value,
    ) -> Result<Option<PathBuf>, String> {
        let config = self.status().settings;
        let Some(root) = mapped_root(&config.folders, reference) else {
            return Ok(None);
        };
        let Some(relative) = reference["relative_path"].as_str() else {
            return Ok(None);
        };
        let mut path = corresponding_path(root, relative)?;
        if encoding != Encoding::Original || library::cue_media_parts(uri).is_some() {
            let key = media_key(uri, &revision(reference), encoding);
            let extension = if encoding == Encoding::Mp3 {
                "mp3"
            } else {
                reference["source_format"].as_str().unwrap_or("audio")
            };
            path.set_extension(format!("{key:.12}.{extension}"));
        }
        Ok(Some(path))
    }

    async fn store_media(
        &self,
        mut file: library::ConnectMediaFile,
        destination: &Path,
    ) -> Result<PathBuf, String> {
        let source = file_path(&file).ok_or("The completed media file is missing")?;
        let source_managed = file.managed;
        let encoding = if file.encoding == "mp3" {
            Encoding::Mp3
        } else {
            Encoding::Original
        };
        let reference = self
            .database
            .connect_track_reference(&file.media_uri)
            .await
            .map_err(error)?;
        let selected = reference
            .as_ref()
            .map(|reference| self.selected_media_path(&file.media_uri, encoding, reference))
            .transpose()?
            .flatten();
        let mut target = selected
            .clone()
            .unwrap_or_else(|| destination.to_path_buf());
        let previous = self
            .database
            .connect_media_file(&file.media_uri, &file.encoding)
            .await
            .map_err(error)?;
        let versioned = versioned_path(&target, &file.media_uri, &file.revision, encoding);
        if previous.as_ref().is_some_and(|old| {
            (!old.managed
                && old.revision != file.revision
                && file_path(old).as_ref() == Some(&target))
                || (old.managed && file_path(old).as_ref() == Some(&versioned))
        }) {
            target = versioned;
        }
        let owned_target = previous
            .as_ref()
            .is_some_and(|old| old.managed && file_path(old).as_ref() == Some(&target));
        let reused = encoding == Encoding::Original
            && selected.is_some()
            && target != source
            && target.is_file()
            && !owned_target;
        if reused {
            file.managed = false;
            file.hash = None;
        } else if target != source {
            // A revised original must not overwrite a file the user supplied.
            if target.exists() && !owned_target {
                target = versioned_path(&target, &file.media_uri, &file.revision, encoding);
            }
            let from = source.clone();
            let to = target.clone();
            tokio::task::spawn_blocking(move || -> Result<(), String> {
                let parent = to.parent().ok_or("Media path has no parent")?;
                std::fs::create_dir_all(parent).map_err(error)?;
                let staged = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
                std::fs::copy(&from, staged.path()).map_err(error)?;
                if owned_target {
                    staged.persist(&to).map_err(error)?;
                } else {
                    staged.persist_noclobber(&to).map_err(error)?;
                }
                Ok(())
            })
            .await
            .map_err(error)??;
            file.managed = true;
        }
        let remove_source = source != target && source_managed;
        file.path = file_uri(&target)?;
        self.database
            .connect_save_media_file(&file)
            .await
            .map_err(error)?;
        self.database
            .connect_set_local_file(&file.media_uri, &target, file.managed)
            .await
            .map_err(error)?;
        if remove_source {
            tokio::fs::remove_file(&source).await.map_err(error)?;
        }
        if let Some(previous) = previous.filter(|old| old.managed && old.path != file.path) {
            if let Some(path) = file_path(&previous) {
                tokio::fs::remove_file(path).await.map_err(error)?;
            }
            self.database
                .connect_forget_media_file(&previous)
                .await
                .map_err(error)?;
        }
        for old in self
            .database
            .connect_media_files(&file.media_uri)
            .await
            .map_err(error)?
        {
            if old.managed && old.encoding != file.encoding {
                self.remove_media_file(&old, old.path != file.path).await?;
            }
        }
        Ok(target)
    }

    async fn remove_media_file(
        &self,
        file: &library::ConnectMediaFile,
        remove_path: bool,
    ) -> Result<usize, String> {
        let mut removed = 0;
        if remove_path && let Some(path) = file_path(file) {
            tokio::fs::remove_file(path).await.map_err(error)?;
            removed = 1;
        }
        if let (Some(network), Some(hash)) = (self.network.lock().await.as_ref(), &file.hash) {
            let encoding = if file.encoding == "mp3" {
                Encoding::Mp3
            } else {
                Encoding::Original
            };
            network
                .media()
                .forget(hash, &media_key(&file.media_uri, &file.revision, encoding))
                .await
                .map_err(error)?;
        }
        self.database
            .connect_forget_media_file(file)
            .await
            .map_err(error)?;
        Ok(removed)
    }

    pub(super) fn cancel_serving(&self, peer: &str, id: &str) {
        if let Some(cancel) = self
            .media_jobs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&serving_key(peer, id))
        {
            cancel.cancel();
        }
    }

    pub(super) async fn serve_media(
        &self,
        peer: &str,
        id: &str,
        uri: &str,
        encoding: Encoding,
        occurrence: Option<&library::OccurrenceId>,
    ) -> Result<serde_json::Value, String> {
        let reference = self
            .database
            .connect_track_reference(uri)
            .await
            .map_err(error)?;
        // Direct files can be queued without joining the collection. An actual
        // local queue occurrence authorizes their transfer, never a supplied path.
        let direct = if reference.is_none() {
            let current = occurrence
                .cloned()
                .or_else(|| {
                    self.transfers
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .get(peer)
                        .filter(|(started, _)| started.elapsed() < Duration::from_secs(300))
                        .map(|(_, snapshot)| snapshot.header.current.clone())
                })
                .ok_or("The track is not in this profile")?;
            let item = self
                .database
                .queue_item_for_occurrence(&current)
                .await
                .map_err(error)?
                .filter(|item| item.media_uri == uri)
                .ok_or("The requested track is not the captured playback")?;
            let received = self
                .database
                .connect_received_occurrence(&current)
                .await
                .map_err(error)?;
            if received
                && let Some(saved) = self
                    .database
                    .connect_media_file(uri, encoding.name())
                    .await
                    .map_err(error)?
                && let Some(path) = file_path(&saved)
                && let Some(hash) = saved.hash
            {
                return serde_json::to_value(OfferedMedia {
                    hash: Some(hash),
                    revision: saved.revision,
                    encoding,
                    size: tokio::fs::metadata(path).await.map_err(error)?.len(),
                })
                .map_err(error);
            }
            let path = (!received)
                .then(|| library::file_media_path(uri))
                .flatten()
                .or_else(|| {
                    (!received)
                        .then(|| {
                            library::cue_media_parts(uri)
                                .and_then(|(_, uri, _, _)| library::file_media_path(&uri))
                        })
                        .flatten()
                })
                .filter(|path| path.is_file())
                .or(self.database.connect_local_file(uri).await.map_err(error)?)
                .ok_or("The current item is not a local file")?;
            let metadata = tokio::fs::metadata(&path).await.map_err(error)?;
            Some((
                path,
                serde_json::json!({"source_format":item.source_format,"revision":[metadata.len(),metadata.modified().map_err(error)?.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos().to_string()]}),
            ))
        } else {
            None
        };
        let reference = reference
            .or_else(|| direct.as_ref().map(|(_, reference)| reference.clone()))
            .ok_or("The track is unavailable")?;
        let revision = revision(&reference);
        let saved = self
            .database
            .connect_media_file(uri, encoding.name())
            .await
            .map_err(error)?;
        if let Some(saved) = saved.as_ref().filter(|saved| saved.revision == revision)
            && let Some(path) = file_path(saved)
            && let Some(hash) = &saved.hash
        {
            return serde_json::to_value(OfferedMedia {
                hash: Some(hash.clone()),
                revision,
                encoding,
                size: tokio::fs::metadata(path).await.map_err(error)?.len(),
            })
            .map_err(error);
        }
        let original_receipt = self
            .database
            .connect_media_file(uri, Encoding::Original.name())
            .await
            .map_err(error)?;
        let original = if let Some((path, _)) = direct {
            path
        } else if let Some(path) = self
            .database
            .connect_original_file(uri)
            .await
            .map_err(error)?
        {
            path
        } else if let Some(path) = original_receipt
            .as_ref()
            .filter(|receipt| receipt.revision == revision)
            .and_then(file_path)
        {
            path
        } else {
            self.database
                .connect_local_file(uri)
                .await
                .map_err(error)?
                .ok_or("The original media is unavailable on this device")?
        };
        let original_managed = original_receipt.as_ref().is_some_and(|receipt| {
            receipt.managed && file_path(receipt).as_ref() == Some(&original)
        });
        // A smaller downloaded representation cannot stand in for an original.
        if let Some(smaller) = self
            .database
            .connect_media_file(uri, Encoding::Mp3.name())
            .await
            .map_err(error)?
            && file_path(&smaller).as_ref() == Some(&original)
        {
            return Err("This device has only the smaller representation. Request it from a device with the original.".into());
        }
        let key = media_key(uri, &revision, encoding);
        let job = serving_key(peer, id);
        let cancel = CancellationToken::new();
        self.media_jobs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(job.clone(), cancel.clone());
        let result = async {
            let path = match encoding {
                Encoding::Original => original,
                Encoding::Mp3 => {
                    let directory = self.directory.join("representations");
                    tokio::fs::create_dir_all(&directory).await.map_err(error)?;
                    let destination = directory.join(format!("{key}.mp3"));
                    if !destination.is_file() {
                        let output = destination.clone();
                        let cancel = cancel.clone();
                        self.status.send_modify(|status| {
                            status.media_status = Some(localization::tr("Preparing smaller media"))
                        });
                        tokio::task::spawn_blocking(move || transcode(&original, &output, &cancel))
                            .await
                            .map_err(error)??;
                    }
                    destination
                }
            };
            if cancel.is_cancelled() {
                return Err("Transfer cancelled".into());
            }
            let hash = match self.network.lock().await.clone() {
                Some(network) => Some(
                    network
                        .media()
                        .publish(&path, &format!("media/{key}"))
                        .await
                        .map_err(error)?,
                ),
                None => None,
            };
            self.database
                .connect_save_media_file(&library::ConnectMediaFile {
                    media_uri: uri.into(),
                    encoding: encoding.name().into(),
                    revision: revision.clone(),
                    path: file_uri(&path)?,
                    managed: encoding != Encoding::Original || original_managed,
                    hash: hash.clone(),
                })
                .await
                .map_err(error)?;
            serde_json::to_value(OfferedMedia {
                hash,
                revision,
                encoding,
                size: tokio::fs::metadata(path).await.map_err(error)?.len(),
            })
            .map_err(error)
        }
        .await;
        self.media_jobs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&job);
        result
    }

    async fn reusable_media(
        &self,
        uri: &str,
        encoding: Encoding,
        reference: &serde_json::Value,
    ) -> Result<Option<PathBuf>, String> {
        let revision = revision(reference);
        if encoding == Encoding::Original
            && let Some(path) = self
                .database
                .connect_original_file(uri)
                .await
                .map_err(error)?
        {
            return Ok(Some(path));
        }
        let receipt = self
            .database
            .connect_media_file(uri, encoding.name())
            .await
            .map_err(error)?;
        if let Some(receipt) = receipt
            .as_ref()
            .filter(|receipt| receipt.revision == revision)
            && let Some(path) = file_path(receipt)
        {
            if self
                .selected_media_path(uri, encoding, reference)?
                .is_some_and(|selected| {
                    selected != path && versioned_path(&selected, uri, &revision, encoding) != path
                })
            {
                return self.store_media(receipt.clone(), &path).await.map(Some);
            }
            if self
                .database
                .playback_access(uri)
                .await
                .map_err(error)?
                .is_none_or(|(access, _)| access != receipt.path)
            {
                self.database
                    .connect_set_local_file(uri, &path, receipt.managed)
                    .await
                    .map_err(error)?;
            }
            return Ok(Some(path));
        }
        if encoding != Encoding::Original {
            return Ok(None);
        }
        // Explicit source revisions supersede a previous receipt. Preserve a
        // user's reused original and put its updated copy in managed storage.
        if receipt.is_some() {
            return Ok(None);
        }
        let config = self.status().settings;
        let mapped = mapped_root(&config.folders, reference);
        if let (Some(root), Some(relative)) = (mapped, reference["relative_path"].as_str()) {
            let expected = corresponding_path(root, relative)?;
            if expected.is_file() {
                self.database
                    .connect_set_local_file(uri, &expected, false)
                    .await
                    .map_err(error)?;
                self.database
                    .connect_save_media_file(&library::ConnectMediaFile {
                        media_uri: uri.into(),
                        encoding: encoding.name().into(),
                        revision,
                        path: file_uri(&expected)?,
                        managed: false,
                        hash: None,
                    })
                    .await
                    .map_err(error)?;
                return Ok(Some(expected));
            }
        }
        let path = self.database.connect_local_file(uri).await.map_err(error)?;
        if let Some(smaller) = self
            .database
            .connect_media_file(uri, Encoding::Mp3.name())
            .await
            .map_err(error)?
            && file_path(&smaller) == path
        {
            return Ok(None);
        }
        Ok(path)
    }

    pub(super) async fn download(&self, peer: &str, uri: &str) -> Result<(), String> {
        let cancel = CancellationToken::new();
        if let Some(previous) = self
            .download_cancel
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .replace(cancel.clone())
        {
            previous.cancel();
        }
        let result = self
            .fetch_media(peer, uri, self.status().settings.encoding, cancel)
            .await;
        self.download_cancel
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        self.status.send_modify(|status| {
            status.media_status = Some(match &result {
                Ok(_) => localization::tr("Media available"),
                Err(message) => message.clone(),
            })
        });
        result.map(|_| ())
    }

    async fn fetch_media(
        &self,
        peer: &str,
        uri: &str,
        encoding: Encoding,
        cancel: CancellationToken,
    ) -> Result<PathBuf, String> {
        let reference = self
            .database
            .connect_track_reference(uri)
            .await
            .map_err(error)?
            .ok_or("The track is not in this profile")?;
        let mut reusable = self.reusable_media(uri, encoding, &reference).await?;
        if reusable.is_none()
            && encoding == Encoding::Mp3
            && self
                .database
                .connect_original_file(uri)
                .await
                .map_err(error)?
                .is_some()
        {
            let identity = self.active().await?.identity.clone();
            self.request_media(&identity, uri, encoding, &cancel, None)
                .await?;
            reusable = self.reusable_media(uri, encoding, &reference).await?;
        }
        if let Some(path) = reusable {
            return Ok(path);
        }
        let offer = self
            .request_media(peer, uri, encoding, &cancel, None)
            .await?;
        let key = media_key(uri, &offer.revision, encoding);
        let mut destination = self.directory.join("media").join(&key);
        let extension = match encoding {
            Encoding::Mp3 => "mp3",
            Encoding::Original => reference["source_format"].as_str().unwrap_or("audio"),
        };
        if !extension.is_empty() && extension.chars().all(|c| c.is_ascii_alphanumeric()) {
            destination.set_extension(extension);
        }
        if let Some(selected) = self.selected_media_path(uri, encoding, &reference)? {
            destination = selected;
        }
        let parent = destination.parent().ok_or("Media path has no parent")?;
        tokio::fs::create_dir_all(parent).await.map_err(error)?;
        let staged = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
        let receipt = self
            .receive_media(peer, uri, staged.path(), offer, cancel)
            .await?;
        self.store_media(receipt, &destination).await
    }

    /// A queued direct file remains queue metadata, not an enrolled collection.
    pub(super) async fn fetch_direct_continuation(
        &self,
        peer: &str,
        item: &library::QueueItem,
        occurrence: &library::OccurrenceId,
    ) -> Result<(), String> {
        let encoding = self.status().settings.encoding;
        let cancel = CancellationToken::new();
        let offer = self
            .request_media(peer, &item.media_uri, encoding, &cancel, Some(occurrence))
            .await?;
        let mut destination = self.directory.join("media").join(media_key(
            &item.media_uri,
            &offer.revision,
            encoding,
        ));
        let extension = if encoding == Encoding::Mp3 {
            Some("mp3")
        } else {
            item.source_format.as_deref()
        };
        if let Some(extension) = extension.filter(|extension| {
            !extension.is_empty() && extension.chars().all(|c| c.is_ascii_alphanumeric())
        }) {
            destination.set_extension(extension);
        }
        let receipt = self
            .receive_media(peer, &item.media_uri, &destination, offer, cancel)
            .await?;
        self.database
            .connect_save_media_file(&receipt)
            .await
            .map_err(error)?;
        self.database
            .connect_set_queue_file(item, &destination)
            .await
            .map_err(error)
    }

    async fn request_media(
        &self,
        peer: &str,
        uri: &str,
        encoding: Encoding,
        cancel: &CancellationToken,
        occurrence: Option<&library::OccurrenceId>,
    ) -> Result<OfferedMedia, String> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(error)?;
        let id = blake3::hash(&nonce).to_hex().to_string();
        let local = self.active().await?.identity == peer;
        self.status.send_modify(|status| {
            status.media_status = Some(localization::tr("Waiting for media provider"))
        });
        let offer = tokio::select! {
            offer = async {
                if local { self.serve_media(peer, &id, uri, encoding, occurrence).await }
                else { self.request(peer, Request::Media { id: id.clone(), uri: uri.into(), encoding, occurrence: occurrence.cloned() }).await }
            } => offer?,
            _ = cancel.cancelled() => {
                if local { self.cancel_serving(peer, &id); }
                else { let _ = self.request(peer, Request::CancelMedia { id }).await; }
                return Err("Transfer cancelled".into());
            }
        };
        let offer: OfferedMedia = serde_json::from_value(offer).map_err(error)?;
        if offer.encoding != encoding {
            return Err("The provider returned a different media representation".into());
        }
        Ok(offer)
    }

    async fn receive_media(
        &self,
        peer: &str,
        uri: &str,
        destination: &Path,
        offer: OfferedMedia,
        cancel: CancellationToken,
    ) -> Result<library::ConnectMediaFile, String> {
        let hash = offer
            .hash
            .ok_or("The device did not provide a media transfer")?;
        let network = self.connected_network().await?;
        let (progress, mut received) = tokio::sync::watch::channel(0u64);
        let statuses = self.status.clone();
        let report = tokio::spawn(async move {
            while received.changed().await.is_ok() {
                let bytes = *received.borrow_and_update();
                statuses.send_modify(|status| {
                    status.media_status = Some(localization::tr_with(
                        "Media transferring: {received} / {total} bytes",
                        &[
                            ("received", &bytes.to_string()),
                            ("total", &offer.size.to_string()),
                        ],
                    ))
                });
            }
        });
        let transferred = network
            .media()
            .fetch(peer, &hash, destination, cancel, progress)
            .await
            .map_err(error);
        report.abort();
        transferred?;
        Ok(library::ConnectMediaFile {
            media_uri: uri.into(),
            encoding: offer.encoding.name().into(),
            revision: offer.revision,
            path: file_uri(destination)?,
            managed: true,
            hash: Some(hash),
        })
    }

    /// Called by the shared player for current/upcoming media, including handoff.
    /// It consults locators rather than treating another device's path as local.
    pub(crate) async fn resolve_media(
        &self,
        occurrence: &library::QueueOccurrence,
    ) -> Result<Option<PathBuf>, String> {
        let uri = &occurrence.media_uri;
        if self.session.read().await.is_none() {
            return Ok(None);
        }
        let mut reference = self
            .database
            .connect_track_reference(uri)
            .await
            .map_err(error)?;
        if reference.is_none()
            && self
                .database
                .connect_received_occurrence(&occurrence.occurrence)
                .await
                .map_err(error)?
        {
            if let Some(path) = self
                .database
                .connect_media_file(uri, self.status().settings.encoding.name())
                .await
                .map_err(error)?
                .as_ref()
                .and_then(file_path)
            {
                self.database
                    .connect_set_queue_file(&occurrence.item, &path)
                    .await
                    .map_err(error)?;
                return Ok(Some(path));
            }
            let session = self.active().await?;
            let mut unavailable = "Waiting for a device with this media".to_string();
            for peer in self
                .connected_network()
                .await?
                .members()
                .await
                .map_err(error)?
            {
                if peer == session.identity {
                    continue;
                }
                match self
                    .request(
                        &peer,
                        Request::TrackReference {
                            uri: uri.to_owned(),
                        },
                    )
                    .await
                {
                    Ok(value) if !value.is_null() => {
                        self.database
                            .connect_apply(&[ConnectRecord {
                                kind: "track".into(),
                                key: uri.to_owned(),
                                value: Some(value.clone()),
                            }])
                            .await
                            .map_err(error)?;
                        reference = Some(value);
                        break;
                    }
                    _ if library::file_media_path(uri).is_some()
                        || library::cue_media_parts(uri).is_some() =>
                    {
                        match self
                            .fetch_direct_continuation(
                                &peer,
                                &occurrence.item,
                                &occurrence.occurrence,
                            )
                            .await
                        {
                            Ok(()) => {
                                return self.database.connect_local_file(uri).await.map_err(error);
                            }
                            Err(error) => unavailable = error,
                        }
                    }
                    _ => {}
                }
            }
            if reference.is_none()
                && (library::file_media_path(uri).is_some()
                    || library::cue_media_parts(uri).is_some())
            {
                return Err(unavailable);
            }
        }
        let Some(reference) = reference else {
            return Ok(None);
        };
        let is_local =
            library::file_media_path(uri).is_some() || library::cue_media_parts(uri).is_some();
        if !is_local {
            return Ok(None);
        }
        let encoding = self.status().settings.encoding;
        if let Some(path) = self.reusable_media(uri, encoding, &reference).await? {
            return Ok(Some(path));
        }
        let session = self.active().await?;
        if encoding == Encoding::Mp3
            && self
                .database
                .connect_original_file(uri)
                .await
                .map_err(error)?
                .is_some()
        {
            let identity = session.identity.clone();
            self.request_media(&identity, uri, encoding, &CancellationToken::new(), None)
                .await?;
            if let Some(path) = self.reusable_media(uri, encoding, &reference).await? {
                return Ok(Some(path));
            }
        }
        let mut last_error = "Waiting for a device with this media".to_owned();
        for peer in self
            .connected_network()
            .await?
            .members()
            .await
            .map_err(error)?
        {
            if peer == session.identity {
                continue;
            }
            match self
                .fetch_media(&peer, uri, encoding, CancellationToken::new())
                .await
            {
                Ok(path) => return Ok(Some(path)),
                Err(error) => last_error = error,
            }
        }
        self.status
            .send_modify(|status| status.media_status = Some(last_error.clone()));
        Err(last_error)
    }
}

impl downloads::ConnectDownload for ConnectOwner {
    fn enabled(&self) -> bool {
        let settings = self.status().settings;
        settings.profile.is_some()
            && !settings.setup_pending
            && !settings.adopting
            && !self.joining.load(std::sync::atomic::Ordering::Acquire)
    }

    fn extension(&self) -> Option<&'static str> {
        (self.status().settings.encoding == Encoding::Mp3).then_some("mp3")
    }

    fn reuse<'a>(
        &'a self,
        uri: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + Send + 'a>>
    {
        Box::pin(async move {
            if self
                .database
                .connect_original_file(uri)
                .await
                .map_err(error)?
                .is_some()
            {
                return Ok(true);
            }
            let Some(reference) = self
                .database
                .connect_track_reference(uri)
                .await
                .map_err(error)?
            else {
                return Ok(false);
            };
            Ok(self
                .reusable_media(uri, self.status().settings.encoding, &reference)
                .await?
                .is_some())
        })
    }

    fn destination<'a>(
        &'a self,
        uri: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<PathBuf>, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            let reference = self
                .database
                .connect_track_reference(uri)
                .await
                .map_err(error)?;
            reference
                .map(|reference| {
                    self.selected_media_path(uri, self.status().settings.encoding, &reference)
                })
                .transpose()
                .map(Option::flatten)
        })
    }

    fn finish<'a>(
        &'a self,
        file: library::ConnectMediaFile,
        destination: &'a Path,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move { self.store_media(file, destination).await.map(|_| ()) })
    }

    fn remove<'a>(
        &'a self,
        uri: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<usize, String>> + Send + 'a>>
    {
        Box::pin(async move {
            let files = self
                .database
                .connect_media_files(uri)
                .await
                .map_err(error)?;
            let mut removed = 0;
            for file in files.into_iter().filter(|file| file.managed) {
                removed += self.remove_media_file(&file, true).await?;
            }
            Ok(removed)
        })
    }

    fn transfer<'a>(
        &'a self,
        uri: &'a str,
        partial: &'a Path,
        extension: Option<&'a str>,
        cancel: CancellationToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = sources::SourceResult<library::ConnectMediaFile>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let key = format!("download/{uri}");
            self.media_jobs
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(key.clone(), cancel.clone());
            let encoding = if extension == Some("mp3") {
                Encoding::Mp3
            } else {
                Encoding::Original
            };
            let result = self
                .transfer_queued(uri, partial, encoding, cancel.clone())
                .await;
            self.media_jobs
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&key);
            if cancel.is_cancelled() {
                return Err(sources::SourceError::Cancelled);
            }
            result
        })
    }
}

impl ConnectOwner {
    async fn transfer_queued(
        &self,
        uri: &str,
        partial: &Path,
        encoding: Encoding,
        cancel: CancellationToken,
    ) -> sources::SourceResult<library::ConnectMediaFile> {
        use sources::SourceError;
        let reference = self
            .database
            .connect_track_reference(uri)
            .await?
            .ok_or(SourceError::NotFound)?;
        let mut reusable = self
            .reusable_media(uri, encoding, &reference)
            .await
            .map_err(SourceError::Other)?;
        if reusable.is_none()
            && encoding == Encoding::Mp3
            && self.database.connect_original_file(uri).await?.is_some()
        {
            let identity = self
                .active()
                .await
                .map_err(SourceError::Network)?
                .identity
                .clone();
            self.request_media(&identity, uri, encoding, &cancel, None)
                .await
                .map_err(SourceError::Other)?;
            reusable = self
                .reusable_media(uri, encoding, &reference)
                .await
                .map_err(SourceError::Other)?;
        }
        if let Some(path) = reusable {
            tokio::select! {
                copy = tokio::fs::copy(path, partial) => { copy.map_err(|error|SourceError::Other(error.to_string()))?; },
                _ = cancel.cancelled() => return Err(SourceError::Cancelled),
            }
            return Ok(library::ConnectMediaFile {
                media_uri: uri.into(),
                encoding: encoding.name().into(),
                revision: revision(&reference),
                path: file_uri(partial).map_err(SourceError::Other)?,
                managed: true,
                hash: None,
            });
        }
        let session = self.active().await.map_err(SourceError::Network)?;
        let mut unavailable = "Waiting for a device with this media".to_string();
        for peer in self
            .connected_network()
            .await
            .map_err(SourceError::Network)?
            .members()
            .await
            .map_err(|error| SourceError::Network(error.to_string()))?
        {
            if peer == session.identity {
                continue;
            }
            let offer = match self
                .request_media(&peer, uri, encoding, &cancel, None)
                .await
            {
                Ok(offer) => offer,
                Err(error) if !cancel.is_cancelled() => {
                    unavailable = error;
                    continue;
                }
                Err(_) => return Err(SourceError::Cancelled),
            };
            return self
                .receive_media(&peer, uri, partial, offer, cancel)
                .await
                .map_err(SourceError::Network);
        }
        Err(SourceError::Network(unavailable))
    }
}

fn transcode(input: &Path, destination: &Path, cancel: &CancellationToken) -> Result<(), String> {
    let stream = playback::ResolvedStream::new(file_uri(input)?);
    let mut reader = audio_processing::TranscodedAudioReader::mp3(&stream)?;
    let mut output = tempfile::NamedTempFile::new_in(
        destination
            .parent()
            .ok_or("Media destination has no parent")?,
    )
    .map_err(error)?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err("Transcoding cancelled".into());
        }
        let read = reader.read(&mut buffer).map_err(error)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read]).map_err(error)?;
    }
    output.as_file().sync_all().map_err(error)?;
    output.persist(destination).map_err(error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corresponding_folder_keeps_source_layout_and_cannot_escape_root() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            corresponding_path(directory.path(), "Artist/Album/track.flac").unwrap(),
            directory
                .path()
                .join("Artist")
                .join("Album")
                .join("track.flac")
        );
        assert!(corresponding_path(directory.path(), "../track.flac").is_err());
        assert!(corresponding_path(directory.path(), "Artist/../../track.flac").is_err());
        assert!(corresponding_path(directory.path(), "").is_err());
        assert!(corresponding_path(directory.path(), directory.path().to_str().unwrap()).is_err());
    }
}
