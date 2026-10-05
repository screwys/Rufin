//! Shared track identity with device-owned files and encoding choices.
use super::*;
use library::LocalMediaLocation;
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

fn location(uri: &str) -> Option<LocalMediaLocation> {
    if uri.starts_with("content://") {
        Some(LocalMediaLocation::Document(uri.into()))
    } else {
        library::file_media_path(uri).map(LocalMediaLocation::File)
    }
}

pub(super) fn raw_document_reference(uri: &str) -> bool {
    uri.starts_with("content://")
        || library::cue_media_parts(uri)
            .is_some_and(|(_, backing, _, _)| backing.starts_with("content://"))
}

fn location_uri(location: &LocalMediaLocation) -> Result<String, String> {
    match location {
        LocalMediaLocation::File(path) => file_uri(path),
        LocalMediaLocation::Document(uri) => Ok(uri.clone()),
    }
}

async fn available_location(
    location: LocalMediaLocation,
) -> Result<Option<LocalMediaLocation>, String> {
    match &location {
        LocalMediaLocation::File(path) => Ok(path.is_file().then_some(location)),
        LocalMediaLocation::Document(uri) => {
            let uri = uri.clone();
            let present = tokio::task::spawn_blocking(move || match sources::stat_document(&uri) {
                Ok(entry) => Ok(!entry.directory),
                Err(sources::SourceError::NotFound) => Ok(false),
                Err(error) => Err(error.to_string()),
            })
            .await
            .map_err(error)??;
            Ok(present.then_some(location))
        }
    }
}

fn reference_backing<'a>(uri: &'a str, reference: &'a serde_json::Value) -> &'a str {
    reference["backing_id"].as_str().unwrap_or(uri)
}

#[derive(Clone, PartialEq)]
enum MediaDestination {
    Native(PathBuf),
    Document { root: String, relative: Vec<String> },
}

impl MediaDestination {
    async fn entry(&self) -> Result<Option<sources::DocumentEntry>, String> {
        match self {
            Self::Native(_) => Ok(None),
            Self::Document { root, relative } => {
                let root = root.clone();
                let relative = relative.clone();
                tokio::task::spawn_blocking(move || sources::find_document(&root, &relative))
                    .await
                    .map_err(error)?
                    .map_err(error)
            }
        }
    }

    fn uri(&self, entry: Option<&sources::DocumentEntry>) -> Result<Option<String>, String> {
        match self {
            Self::Native(path) => file_uri(path).map(Some),
            Self::Document { .. } => Ok(entry.map(|entry| entry.uri.clone())),
        }
    }

    fn present(&self, entry: Option<&sources::DocumentEntry>) -> bool {
        match self {
            Self::Native(path) => path.is_file(),
            Self::Document { .. } => entry.is_some(),
        }
    }

    fn occupied(&self, entry: Option<&sources::DocumentEntry>) -> bool {
        match self {
            Self::Native(path) => path.exists(),
            Self::Document { .. } => entry.is_some(),
        }
    }

    fn version(&mut self, backing: &str, revision: &str, encoding: Encoding) {
        match self {
            Self::Native(path) => *path = versioned_path(path, backing, revision, encoding),
            Self::Document { relative, .. } => {
                let name = relative.last_mut().unwrap();
                *name = versioned_path(Path::new(name), backing, revision, encoding)
                    .to_string_lossy()
                    .into_owned();
            }
        }
    }
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
    let key = file_key(uri, revision, encoding);
    let extension = path.extension().unwrap_or_default().to_string_lossy();
    path.with_extension(format!("{key:.12}.{extension}"))
}

fn media_key(uri: &str, revision: &str, encoding: Encoding) -> String {
    blake3::hash(format!("{uri}\0{revision}\0{}", encoding.name()).as_bytes())
        .to_hex()
        .to_string()
}

fn file_key(uri: &str, revision: &str, encoding: Encoding) -> String {
    // CUE tracks keep their own identity, but transfer the same complete audio file.
    let backing = library::cue_media_parts(uri).map(|(_, backing, _, _)| backing);
    media_key(backing.as_deref().unwrap_or(uri), revision, encoding)
}

fn serving_key(peer: &str, id: &str) -> String {
    format!("serve/{peer}/{id}")
}

fn mapped_root<'a>(
    folders: &'a BTreeMap<String, downloads::DownloadDirectory>,
    reference: &serde_json::Value,
) -> Option<&'a downloads::DownloadDirectory> {
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
    fn local_file_path(&self, path: PathBuf) -> PathBuf {
        self.source
            .shared
            .settings
            .local_configuration()
            .and_then(|local| local.local_file_location(&path))
            .map_or(path, |(_, access)| access)
    }

    async fn original_media_file(&self, uri: &str) -> Result<Option<LocalMediaLocation>, String> {
        let location = self
            .database
            .connect_original_file(uri, |path| self.local_file_path(path))
            .await
            .map_err(error)?;
        match location {
            Some(location) => available_location(location).await,
            None => Ok(None),
        }
    }

    pub(crate) async fn is_local_media(&self, uri: &str) -> Result<bool, String> {
        let cue = library::cue_media_parts(uri);
        let backing = cue.as_ref().map_or(uri, |(_, backing, _, _)| backing);
        if location(backing).is_some() || library::document_media_id(backing).is_some() {
            return Ok(true);
        }
        if library::source_entity_parts(uri).is_some_and(|(source, kind, _)| {
            kind == "track"
                && self
                    .source
                    .configuration(&source)
                    .is_some_and(|config| config.is_local())
        }) {
            return Ok(true);
        }
        Ok(self
            .database
            .connect_track_reference(uri)
            .await
            .map_err(error)?
            .is_some_and(|reference| reference["local_media"].as_bool() == Some(true)))
    }

    fn representation_path(&self, backing: &str, revision: &str, encoding: Encoding) -> PathBuf {
        self.directory.join("representations").join(format!(
            "{}.{}",
            file_key(backing, revision, encoding),
            encoding.name()
        ))
    }

    fn document_representation_directory(
        &self,
        backing: &str,
        revision: &str,
        encoding: Encoding,
    ) -> PathBuf {
        self.directory.join("representations").join(format!(
            "{}.documents",
            file_key(backing, revision, encoding)
        ))
    }

    fn document_representation_path(
        &self,
        backing: &str,
        revision: &str,
        encoding: Encoding,
        entry: &sources::DocumentEntry,
    ) -> PathBuf {
        let metadata = format!(
            "{}\0{:?}\0{:?}",
            entry.native_id, entry.revision, entry.size
        );
        self.document_representation_directory(backing, revision, encoding)
            .join(format!(
                "{}.{}",
                blake3::hash(metadata.as_bytes()).to_hex(),
                encoding.name()
            ))
    }

    async fn materialize_document(
        &self,
        uri: &str,
        backing: &str,
        revision: &str,
        encoding: Encoding,
        cancel: &CancellationToken,
    ) -> Result<PathBuf, String> {
        let uri = uri.to_string();
        let inspect = uri.clone();
        let entry = tokio::task::spawn_blocking(move || sources::stat_document(&inspect))
            .await
            .map_err(error)?
            .map_err(error)?;
        let destination = self.document_representation_path(backing, revision, encoding, &entry);
        if entry.revision.is_some() && destination.is_file() {
            return Ok(destination);
        }
        let output = destination.clone();
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            let parent = output.parent().unwrap();
            std::fs::create_dir_all(parent).map_err(error)?;
            let mut staged = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
            let mut input = sources::open_document_input(&uri).map_err(error)?;
            let mut buffer = [0_u8; 65536];
            loop {
                if cancel.is_cancelled() {
                    return Err("Transfer cancelled".into());
                }
                let read = input.read(&mut buffer).map_err(error)?;
                if read == 0 {
                    break;
                }
                staged.write_all(&buffer[..read]).map_err(error)?;
            }
            staged.as_file().sync_all().map_err(error)?;
            staged.persist(output).map_err(error)?;
            Ok::<_, String>(())
        })
        .await
        .map_err(error)??;
        Ok(destination)
    }

    async fn materialize_receipt(
        &self,
        file: &library::ConnectMediaFile,
        cancel: &CancellationToken,
    ) -> Result<PathBuf, String> {
        match location(&file.path).ok_or("The media location is unavailable")? {
            LocalMediaLocation::File(path) => Ok(path),
            LocalMediaLocation::Document(uri) => {
                let backing = self
                    .database
                    .connect_backing_id(&file.media_uri)
                    .await
                    .map_err(error)?;
                let encoding = if file.encoding == "mp3" {
                    Encoding::Mp3
                } else {
                    Encoding::Original
                };
                self.materialize_document(&uri, &backing, &file.revision, encoding, cancel)
                    .await
            }
        }
    }

    fn media_path(
        &self,
        uri: &str,
        revision: &str,
        encoding: Encoding,
        source_format: Option<&str>,
    ) -> PathBuf {
        let mut path = self
            .directory
            .join("media")
            .join(file_key(uri, revision, encoding));
        let extension = match encoding {
            Encoding::Mp3 => "mp3",
            Encoding::Original => source_format.unwrap_or("audio"),
        };
        if !extension.is_empty() && extension.chars().all(|c| c.is_ascii_alphanumeric()) {
            path.set_extension(extension);
        }
        path
    }

    fn selected_media_destination(
        &self,
        uri: &str,
        encoding: Encoding,
        reference: &serde_json::Value,
    ) -> Result<Option<MediaDestination>, String> {
        let config = self.status().settings;
        let Some(root) = mapped_root(&config.folders, reference) else {
            return Ok(None);
        };
        let Some(relative) = reference["relative_path"].as_str() else {
            return Ok(None);
        };
        let backing = reference_backing(uri, reference);
        Ok(Some(match root {
            downloads::DownloadDirectory::Native(root) => {
                let mut path = self.local_file_path(corresponding_path(root, relative)?);
                if encoding == Encoding::Mp3 {
                    let key = file_key(backing, &revision(reference), encoding);
                    path.set_extension(format!("{key:.12}.mp3"));
                }
                MediaDestination::Native(path)
            }
            downloads::DownloadDirectory::Document { uri: root } => {
                let mut relative = relative
                    .split('/')
                    .filter(|part| *part != ".")
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if relative.is_empty()
                    || relative.iter().any(|part| part.is_empty() || part == "..")
                {
                    return Err("The shared track has an invalid source-relative path".into());
                }
                if encoding == Encoding::Mp3 {
                    let key = file_key(backing, &revision(reference), encoding);
                    let name = relative.last_mut().unwrap();
                    *name = Path::new(name)
                        .with_extension(format!("{key:.12}.mp3"))
                        .to_string_lossy()
                        .into_owned();
                }
                MediaDestination::Document {
                    root: root.clone(),
                    relative,
                }
            }
        }))
    }

    // Callers hold media_files while changing files and their receipts.
    async fn store_media(
        &self,
        mut file: library::ConnectMediaFile,
        destination: &Path,
    ) -> Result<LocalMediaLocation, String> {
        let source_file = file.clone();
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
            .map(|reference| self.selected_media_destination(&file.media_uri, encoding, reference))
            .transpose()?
            .flatten();
        let backing_id = self
            .database
            .connect_backing_id(&file.media_uri)
            .await
            .map_err(error)?;
        let mut target = selected
            .clone()
            .unwrap_or_else(|| MediaDestination::Native(destination.into()));
        let source = if let MediaDestination::Native(path) = &mut target {
            let source = self
                .materialize_receipt(&file, &CancellationToken::new())
                .await?;
            let backing = self
                .database
                .connect_backing_media_file(&file.media_uri, &file.encoding, &file.revision)
                .await
                .map_err(error)?;
            if let Some(backing) = backing.as_ref().and_then(file_path).filter(|backing| {
                selected.as_ref().is_none_or(|selected| match selected {
                    MediaDestination::Native(selected) => {
                        backing == selected
                            || *backing
                                == versioned_path(selected, &backing_id, &file.revision, encoding)
                    }
                    MediaDestination::Document { .. } => false,
                })
            }) {
                *path = backing;
            }
            Some(source)
        } else {
            None
        };
        let previous = self
            .database
            .connect_media_file(&file.media_uri, &file.encoding)
            .await
            .map_err(error)?;
        let file_encoding = &file.encoding;
        let file_revision = &file.revision;
        let inspect = |target: MediaDestination| async move {
            let entry = target.entry().await?;
            let shared = match target.uri(entry.as_ref())? {
                Some(uri) => self
                    .database
                    .connect_media_file_at_path(&uri, file_encoding, file_revision)
                    .await
                    .map_err(error)?,
                None => None,
            };
            Ok::<_, String>((entry, shared))
        };
        let (mut entry, mut shared) = inspect(target.clone()).await?;
        if shared
            .as_ref()
            .is_some_and(|old| old.revision != file.revision)
        {
            target.version(&backing_id, &file.revision, encoding);
            (entry, shared) = inspect(target.clone()).await?;
        }
        let shared = shared.filter(|old| old.revision == file.revision);
        let target_uri = target.uri(entry.as_ref())?;
        let owned_target = previous.as_ref().is_some_and(|old| {
            old.managed
                && match &target {
                    MediaDestination::Native(path) => file_path(old).as_ref() == Some(path),
                    MediaDestination::Document { .. } => Some(&old.path) == target_uri.as_ref(),
                }
        });
        let same = match &target {
            MediaDestination::Native(path) => source.as_ref() == Some(path),
            MediaDestination::Document { .. } => target_uri.as_ref() == Some(&file.path),
        };
        let mapped_original =
            encoding == Encoding::Original && selected.as_ref() == Some(&target) && !owned_target;
        let reused = target.present(entry.as_ref())
            && (shared.is_some() || mapped_original || same)
            && (!same || matches!(target, MediaDestination::Document { .. }));
        if reused {
            file.managed = shared
                .as_ref()
                .map_or(file.managed && same, |old| old.managed);
            file.hash = shared.and_then(|old| old.hash);
        } else if !same {
            // A revised original must not overwrite a file the user supplied.
            if target.occupied(entry.as_ref()) && !owned_target {
                target.version(&backing_id, &file.revision, encoding);
                entry = target.entry().await?;
                if entry.is_some() {
                    return Err("A file already occupies the versioned media destination".into());
                }
            }
            match &target {
                MediaDestination::Native(path) => {
                    let from = source.as_ref().unwrap().clone();
                    let to = path.clone();
                    tokio::task::spawn_blocking(move || -> Result<(), String> {
                        let parent = to.parent().ok_or("Media path has no parent")?;
                        std::fs::create_dir_all(parent).map_err(error)?;
                        let staged = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
                        std::fs::copy(from, staged.path()).map_err(error)?;
                        if owned_target {
                            staged.persist(to).map_err(error)?;
                        } else {
                            staged.persist_noclobber(to).map_err(error)?;
                        }
                        Ok(())
                    })
                    .await
                    .map_err(error)??;
                }
                MediaDestination::Document { root, relative } => {
                    let path = self
                        .materialize_receipt(&file, &CancellationToken::new())
                        .await?;
                    let root = root.clone();
                    let relative = relative.clone();
                    entry = Some(
                        tokio::task::spawn_blocking(move || {
                            if let Some(entry) = entry {
                                sources::save_document(
                                    &entry.uri,
                                    &path,
                                    entry.revision.as_deref(),
                                )
                                .map_err(error)?;
                                return sources::stat_document(&entry.uri).map_err(error);
                            }
                            let parent = sources::create_document_directories(
                                &root,
                                &relative[..relative.len() - 1],
                            )
                            .map_err(error)?;
                            sources::create_document(
                                &parent.uri,
                                relative.last().unwrap(),
                                "application/octet-stream",
                                &path,
                            )
                            .map_err(error)
                        })
                        .await
                        .map_err(error)??,
                    );
                }
            }
            file.managed = true;
        }
        file.path = target.uri(entry.as_ref())?.unwrap();
        if let MediaDestination::Native(path) = &target
            && !reused
            && let Some(network) = self.network.lock().await.as_ref()
        {
            network
                .media()
                .relocate(source.as_ref().unwrap(), path)
                .await
                .map_err(error)?;
        }
        self.database
            .connect_save_media_file(&file)
            .await
            .map_err(error)?;
        match &target {
            MediaDestination::Native(path) => {
                if reference.is_some() {
                    self.database
                        .connect_set_local_file(&file.media_uri, path, file.managed)
                        .await
                        .map_err(error)?;
                }
            }
            MediaDestination::Document { root, .. } => {
                self.database
                    .connect_set_document_file(
                        &file.media_uri,
                        &file.path,
                        root,
                        file.managed,
                        entry.as_ref().unwrap().size,
                    )
                    .await
                    .map_err(error)?;
            }
        }
        let source = match &target {
            MediaDestination::Native(path) => source.filter(|source| source != path),
            MediaDestination::Document { .. } => {
                file_path(&source_file).filter(|_| source_file.path != file.path)
            }
        };
        let mut retain_source = false;
        // A blob publisher keeps reading its registered file after this method returns.
        if matches!(target, MediaDestination::Document { .. })
            && !reused
            && let Some(path) = &source
        {
            let entry = entry.as_ref().unwrap();
            let destination =
                self.document_representation_path(&backing_id, &file.revision, encoding, entry);
            retain_source = *path == destination;
            if !retain_source {
                if entry.revision.is_none() || !destination.is_file() {
                    let from = path.clone();
                    let to = destination.clone();
                    tokio::task::spawn_blocking(move || {
                        std::fs::create_dir_all(to.parent().unwrap()).map_err(error)?;
                        let staged =
                            tempfile::NamedTempFile::new_in(to.parent().unwrap()).map_err(error)?;
                        std::fs::copy(from, staged.path()).map_err(error)?;
                        staged.as_file().sync_all().map_err(error)?;
                        staged.persist(to).map_err(error)?;
                        Ok::<_, String>(())
                    })
                    .await
                    .map_err(error)??;
                }
                if let Some(network) = self.network.lock().await.as_ref() {
                    network
                        .media()
                        .relocate(path, &destination)
                        .await
                        .map_err(error)?;
                }
            }
        }
        if source_file.managed
            && !retain_source
            && let Some(path) = source
            && !self
                .database
                .connect_media_file_users(&source_file)
                .await
                .map_err(error)?
                .0
        {
            tokio::fs::remove_file(path).await.map_err(error)?;
        }
        if let Some(previous) =
            previous.filter(|old| old.path != file.path || old.revision != file.revision)
        {
            self.remove_media_file(&previous, previous.managed && previous.path != file.path)
                .await?;
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
        match target {
            MediaDestination::Native(path) => Ok(LocalMediaLocation::File(path)),
            MediaDestination::Document { .. } => Ok(LocalMediaLocation::Document(file.path)),
        }
    }

    async fn remove_media_file(
        &self,
        file: &library::ConnectMediaFile,
        remove_path: bool,
    ) -> Result<usize, String> {
        let (path_used, blob_used) = self
            .database
            .connect_media_file_users(file)
            .await
            .map_err(error)?;
        let mut removed = 0;
        if remove_path && !path_used {
            if file.path.starts_with("content://") {
                let uri = file.path.clone();
                removed = usize::from(
                    tokio::task::spawn_blocking(move || match sources::delete_document(&uri) {
                        Ok(()) => Ok(true),
                        Err(sources::SourceError::NotFound) => Ok(false),
                        Err(error) => Err(error.to_string()),
                    })
                    .await
                    .map_err(error)??,
                );
            } else if let Some(path) = file_path(file) {
                tokio::fs::remove_file(path).await.map_err(error)?;
                removed = 1;
            }
        }
        if let (Some(network), Some(hash)) = (self.network.lock().await.as_ref(), &file.hash) {
            network
                .media()
                .forget(hash, blob_used)
                .await
                .map_err(error)?;
        }
        self.database
            .connect_forget_media_file(file)
            .await
            .map_err(error)?;
        if file.path.starts_with("content://")
            && !self
                .database
                .connect_document_backing_used(&file.media_uri, &file.encoding, &file.revision)
                .await
                .map_err(error)?
        {
            let backing = self
                .database
                .connect_backing_id(&file.media_uri)
                .await
                .map_err(error)?;
            let encoding = if file.encoding == "mp3" {
                Encoding::Mp3
            } else {
                Encoding::Original
            };
            match tokio::fs::remove_dir_all(self.document_representation_directory(
                &backing,
                &file.revision,
                encoding,
            ))
            .await
            {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(removed)
    }

    pub(super) fn cancel_serving(&self, peer: &str, id: &str) {
        if let Some(cancel) = self
            .media_jobs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&serving_key(peer, id))
        {
            cancel.cancel();
        }
    }

    async fn offer_saved_media(
        &self,
        uri: &str,
        encoding: Encoding,
        revision: Option<&str>,
    ) -> Result<Option<serde_json::Value>, String> {
        let _files = self.media_files.lock().await;
        let Some(mut saved) = self
            .database
            .connect_media_file(uri, encoding.name())
            .await
            .map_err(error)?
            .filter(|saved| revision.is_none_or(|revision| saved.revision == revision))
        else {
            return Ok(None);
        };
        if !saved.path.starts_with("content://") && file_path(&saved).is_none() {
            return Ok(None);
        }
        let path = self
            .materialize_receipt(&saved, &CancellationToken::new())
            .await?;
        let hash = self
            .connected_network()
            .await?
            .media()
            .publish(&path)
            .await
            .map_err(error)?;
        if saved.hash.as_ref() != Some(&hash) {
            saved.hash = Some(hash.clone());
            self.database
                .connect_save_media_file(&saved)
                .await
                .map_err(error)?;
        }
        serde_json::to_value(OfferedMedia {
            hash: Some(hash),
            revision: saved.revision,
            encoding,
            size: tokio::fs::metadata(path).await.map_err(error)?.len(),
        })
        .map(Some)
        .map_err(error)
    }

    pub(super) async fn serve_media(
        &self,
        peer: &str,
        id: &str,
        uri: &str,
        encoding: Encoding,
        occurrence: Option<&library::OccurrenceId>,
    ) -> Result<serde_json::Value, String> {
        let session = self.active().await?;
        let mut reference = self
            .database
            .connect_track_reference(uri)
            .await
            .map_err(error)?;
        if raw_document_reference(uri) && occurrence.is_some() {
            reference = None;
        }
        // A direct item is authorized by its captured queue occurrence or a private locator.
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
            if received && raw_document_reference(uri) {
                return Err(
                    "The source device did not provide a portable document identity".into(),
                );
            }
            if received && let Some(offer) = self.offer_saved_media(uri, encoding, None).await? {
                return Ok(offer);
            }
            let cue = library::cue_media_parts(uri);
            let backing = cue.as_ref().map_or(uri, |(_, backing, _, _)| backing);
            let original = (!received)
                .then(|| location(backing))
                .flatten()
                .map(|location| match location {
                    LocalMediaLocation::File(path) => {
                        LocalMediaLocation::File(self.local_file_path(path))
                    }
                    document => document,
                })
                .filter(|location| match location {
                    LocalMediaLocation::File(path) => path.is_file(),
                    LocalMediaLocation::Document(_) => true,
                })
                .or(self.database.connect_local_file(uri).await.map_err(error)?)
                .ok_or("The current item is not a local file")?;
            let revision = match &original {
                LocalMediaLocation::File(path) => {
                    let metadata = tokio::fs::metadata(path).await.map_err(error)?;
                    serde_json::json!([
                        metadata.len(),
                        metadata
                            .modified()
                            .map_err(error)?
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos()
                            .to_string()
                    ])
                }
                LocalMediaLocation::Document(uri) => {
                    let uri = uri.clone();
                    let entry = tokio::task::spawn_blocking(move || sources::stat_document(&uri))
                        .await
                        .map_err(error)?
                        .map_err(error)?;
                    serde_json::json!([entry.size, entry.revision])
                }
            };
            Some((
                original,
                serde_json::json!({"source_format":item.source_format,"revision":revision,"backing_id":backing}),
            ))
        } else {
            None
        };
        let reference = reference
            .or_else(|| direct.as_ref().map(|(_, reference)| reference.clone()))
            .ok_or("The track is unavailable")?;
        let revision = revision(&reference);
        if let Some(offer) = self
            .offer_saved_media(uri, encoding, Some(&revision))
            .await?
        {
            return Ok(offer);
        }
        let original_receipt = self
            .database
            .connect_media_file(uri, Encoding::Original.name())
            .await
            .map_err(error)?;
        let original = if let Some((location, _)) = direct {
            location
        } else if let Some(location) = self.original_media_file(uri).await? {
            location
        } else if let Some(location) = original_receipt
            .as_ref()
            .filter(|receipt| receipt.revision == revision)
            .and_then(|receipt| location(&receipt.path))
        {
            location
        } else if let Some(location) = self.database.connect_local_file(uri).await.map_err(error)? {
            location
        } else {
            return Ok(serde_json::Value::Null);
        };
        let original_uri = location_uri(&original)?;
        let original_managed = original_receipt
            .as_ref()
            .is_some_and(|receipt| receipt.managed && receipt.path == original_uri);
        if let Some(smaller) = self
            .database
            .connect_media_file(uri, Encoding::Mp3.name())
            .await
            .map_err(error)?
            && smaller.path == original_uri
        {
            return Err("This device has only the smaller representation. Request it from a device with the original.".into());
        }
        let backing = reference_backing(uri, &reference);
        let job = serving_key(peer, id);
        let cancel = session.stop.child_token();
        self.media_jobs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(job.clone(), cancel.clone());
        let result = async {
            let document_original = matches!(&original, LocalMediaLocation::Document(_));
            let original = match original {
                LocalMediaLocation::File(path) => path,
                LocalMediaLocation::Document(uri) => {
                    self.materialize_document(&uri, backing, &revision, Encoding::Original, &cancel)
                        .await?
                }
            };
            let path = match encoding {
                Encoding::Original => original,
                Encoding::Mp3 => {
                    let directory = self.directory.join("representations");
                    tokio::fs::create_dir_all(&directory).await.map_err(error)?;
                    let destination = self.representation_path(backing, &revision, encoding);
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
            let _files = self.media_files.lock().await;
            let hash = match self.network.lock().await.clone() {
                Some(network) => Some(network.media().publish(&path).await.map_err(error)?),
                None => None,
            };
            self.database
                .connect_save_media_file(&library::ConnectMediaFile {
                    media_uri: uri.into(),
                    encoding: encoding.name().into(),
                    revision: revision.clone(),
                    path: if document_original && encoding == Encoding::Original {
                        original_uri
                    } else {
                        file_uri(&path)?
                    },
                    managed: encoding != Encoding::Original || original_managed,
                    hash: hash.clone(),
                })
                .await
                .map_err(error)?;
            if encoding == Encoding::Original
                && let Some(previous) = original_receipt
                    .as_ref()
                    .filter(|old| old.path.starts_with("content://") && old.revision != revision)
            {
                self.remove_media_file(previous, false).await?;
            }
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

    async fn register_media_access(
        &self,
        file: &library::ConnectMediaFile,
        reference: &serde_json::Value,
    ) -> Result<(), String> {
        match location(&file.path).ok_or("The media location is unavailable")? {
            LocalMediaLocation::File(path) => self
                .database
                .connect_set_local_file(&file.media_uri, &path, file.managed)
                .await
                .map_err(error),
            LocalMediaLocation::Document(uri) => {
                let document = uri.clone();
                let entry = tokio::task::spawn_blocking(move || sources::stat_document(&document))
                    .await
                    .map_err(error)?
                    .map_err(error)?;
                let settings = self.status().settings;
                let root = match mapped_root(&settings.folders, reference) {
                    Some(downloads::DownloadDirectory::Document { uri }) => uri.as_str(),
                    _ => uri.as_str(),
                };
                self.database
                    .connect_set_document_file(
                        &file.media_uri,
                        &uri,
                        root,
                        file.managed,
                        entry.size,
                    )
                    .await
                    .map_err(error)
            }
        }
    }

    async fn reusable_media(
        &self,
        uri: &str,
        encoding: Encoding,
        reference: &serde_json::Value,
    ) -> Result<Option<LocalMediaLocation>, String> {
        let revision = revision(reference);
        if encoding == Encoding::Original
            && let Some(original) = self.original_media_file(uri).await?
        {
            return Ok(Some(original));
        }
        let _files = self.media_files.lock().await;
        let receipt = self
            .database
            .connect_media_file(uri, encoding.name())
            .await
            .map_err(error)?;
        let available =
            if let Some(receipt) = receipt.as_ref().filter(|file| file.revision == revision) {
                match location(&receipt.path) {
                    Some(location) => available_location(location).await?,
                    None => None,
                }
            } else {
                None
            };
        if available.is_none()
            && let Some(mut shared) = self
                .database
                .connect_backing_media_file(uri, encoding.name(), &revision)
                .await
                .map_err(error)?
        {
            if let Some(location) = location(&shared.path)
                && available_location(location).await?.is_some()
            {
                shared.media_uri = uri.into();
                let destination = self.media_path(
                    reference_backing(uri, reference),
                    &revision,
                    encoding,
                    reference["source_format"].as_str(),
                );
                return self.store_media(shared, &destination).await.map(Some);
            }
        }
        if let Some(available) = available {
            let receipt = receipt.as_ref().unwrap();
            let relocate = match self.selected_media_destination(uri, encoding, reference)? {
                Some(MediaDestination::Native(selected)) => match &available {
                    LocalMediaLocation::File(path) => {
                        selected != *path
                            && versioned_path(
                                &selected,
                                reference_backing(uri, reference),
                                &revision,
                                encoding,
                            ) != *path
                    }
                    LocalMediaLocation::Document(_) => true,
                },
                Some(MediaDestination::Document { root, .. }) => match &available {
                    LocalMediaLocation::Document(document) => !self
                        .database
                        .connect_document_mapping_matches(
                            uri,
                            document,
                            &root,
                            reference["relative_path"].as_str().unwrap_or_default(),
                        )
                        .await
                        .map_err(error)?,
                    LocalMediaLocation::File(_) => true,
                },
                None => false,
            };
            if relocate {
                let destination = self.media_path(
                    reference_backing(uri, reference),
                    &revision,
                    encoding,
                    reference["source_format"].as_str(),
                );
                return self
                    .store_media(receipt.clone(), &destination)
                    .await
                    .map(Some);
            }
            if self
                .database
                .playback_access(uri)
                .await
                .map_err(error)?
                .is_none_or(|(access, _)| access != receipt.path)
            {
                self.register_media_access(receipt, reference).await?;
            }
            return Ok(Some(available));
        }
        if encoding != Encoding::Original
            || receipt.is_some_and(|receipt| receipt.revision != revision)
        {
            return Ok(None);
        }
        let candidate = match self.selected_media_destination(uri, encoding, reference)? {
            Some(MediaDestination::Native(path)) => {
                path.is_file().then_some(LocalMediaLocation::File(path))
            }
            Some(MediaDestination::Document { root, relative }) => {
                tokio::task::spawn_blocking(move || sources::find_document(&root, &relative))
                    .await
                    .map_err(error)?
                    .map_err(error)?
                    .filter(|entry| !entry.directory)
                    .map(|entry| LocalMediaLocation::Document(entry.uri))
            }
            None => None,
        };
        if let Some(candidate) = candidate {
            let file = library::ConnectMediaFile {
                media_uri: uri.into(),
                encoding: encoding.name().into(),
                revision,
                path: location_uri(&candidate)?,
                managed: false,
                hash: None,
            };
            self.register_media_access(&file, reference).await?;
            self.database
                .connect_save_media_file(&file)
                .await
                .map_err(error)?;
            return Ok(Some(candidate));
        }
        let candidate = match self.database.connect_local_file(uri).await.map_err(error)? {
            Some(location) => available_location(location).await?,
            None => None,
        };
        if let Some(candidate) = &candidate
            && let Some(smaller) = self
                .database
                .connect_media_file(uri, Encoding::Mp3.name())
                .await
                .map_err(error)?
        {
            if smaller.path == location_uri(candidate)? {
                return Ok(None);
            }
        }
        Ok(candidate)
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
    ) -> Result<LocalMediaLocation, String> {
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
                .original_media_file(uri)
                .await
                .map_err(error)?
                .is_some()
        {
            let identity = self.active().await?.identity.clone();
            self.request_media(&identity, uri, encoding, &cancel, None)
                .await
                .map_err(error)?;
            reusable = self.reusable_media(uri, encoding, &reference).await?;
        }
        if let Some(path) = reusable {
            return Ok(path);
        }
        let offer = self
            .request_media(peer, uri, encoding, &cancel, None)
            .await
            .map_err(error)?;
        let mut destination = self.media_path(
            reference_backing(uri, &reference),
            &offer.revision,
            encoding,
            reference["source_format"].as_str(),
        );
        // Documents are published after receipt; their incoming bytes use managed staging.
        if let Some(MediaDestination::Native(selected)) =
            self.selected_media_destination(uri, encoding, &reference)?
        {
            destination = selected;
        }
        let parent = destination.parent().ok_or("Media path has no parent")?;
        tokio::fs::create_dir_all(parent).await.map_err(error)?;
        let staged = tempfile::NamedTempFile::new_in(parent)
            .map_err(error)?
            .into_temp_path();
        let receipt = self
            .receive_media(peer, uri, &staged, offer, cancel)
            .await?;
        let _files = self.media_files.lock().await;
        self.store_media(receipt, &destination).await
    }

    /// A queued direct file remains queue metadata, not an enrolled collection.
    pub(super) async fn fetch_direct_continuation(
        &self,
        peer: &str,
        item: &library::QueueItem,
        occurrence: &library::OccurrenceId,
    ) -> Result<(), String> {
        if raw_document_reference(&item.media_uri) {
            return Err("The source device did not provide a portable document identity".into());
        }
        let encoding = self.status().settings.encoding;
        let cancel = CancellationToken::new();
        let offer = self
            .request_media(peer, &item.media_uri, encoding, &cancel, Some(occurrence))
            .await
            .map_err(error)?;
        let destination = self.media_path(
            &item.media_uri,
            &offer.revision,
            encoding,
            item.source_format.as_deref(),
        );
        let parent = destination.parent().ok_or("Media path has no parent")?;
        tokio::fs::create_dir_all(parent).await.map_err(error)?;
        let staged = tempfile::NamedTempFile::new_in(parent)
            .map_err(error)?
            .into_temp_path();
        let receipt = self
            .receive_media(peer, &item.media_uri, &staged, offer, cancel)
            .await?;
        let _files = self.media_files.lock().await;
        let destination = self.store_media(receipt, &destination).await?;
        self.register_queue_access(item, &destination).await
    }

    async fn register_queue_access(
        &self,
        item: &library::QueueItem,
        location: &LocalMediaLocation,
    ) -> Result<(), String> {
        match location {
            LocalMediaLocation::File(path) => self
                .database
                .connect_set_queue_file(item, path)
                .await
                .map_err(error),
            LocalMediaLocation::Document(uri) => {
                let document = uri.clone();
                let entry = tokio::task::spawn_blocking(move || sources::stat_document(&document))
                    .await
                    .map_err(error)?
                    .map_err(error)?;
                self.database
                    .connect_set_queue_document(item, uri, entry.size)
                    .await
                    .map_err(error)
            }
        }
    }

    async fn request_media(
        &self,
        peer: &str,
        uri: &str,
        encoding: Encoding,
        cancel: &CancellationToken,
        occurrence: Option<&library::OccurrenceId>,
    ) -> sources::SourceResult<OfferedMedia> {
        use sources::SourceError;
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|error| SourceError::Other(error.to_string()))?;
        let id = blake3::hash(&nonce).to_hex().to_string();
        let local = self.active().await.map_err(SourceError::Network)?.identity == peer;
        self.publish_media_received();
        let offer = tokio::select! {
            offer = async {
                if local { self.serve_media(peer, &id, uri, encoding, occurrence).await.map_err(SourceError::Other) }
                else { self.request(peer, Request::Media { id: id.clone(), uri: uri.into(), encoding, occurrence: occurrence.cloned() }).await.map_err(SourceError::Network) }
            } => offer?,
            _ = cancel.cancelled() => {
                if local { self.cancel_serving(peer, &id); }
                else { let _ = self.request(peer, Request::CancelMedia { id }).await; }
                return Err(SourceError::Cancelled);
            }
        };
        let offer: Option<OfferedMedia> = serde_json::from_value(offer)?;
        let offer = offer.ok_or(SourceError::NotFound)?;
        if offer.encoding != encoding {
            return Err(SourceError::Other(
                "The provider returned a different media representation".into(),
            ));
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
        let (progress, _received) = tokio::sync::watch::channel(0u64);
        network
            .media()
            .fetch(peer, &hash, destination, cancel, progress)
            .await
            .map_err(error)?;
        self.media_received
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.publish_media_received();
        Ok(library::ConnectMediaFile {
            media_uri: uri.into(),
            encoding: offer.encoding.name().into(),
            revision: offer.revision,
            path: file_uri(destination)?,
            managed: true,
            hash: Some(hash),
        })
    }

    fn publish_media_received(&self) {
        self.status.send_if_modified(|status| {
            let count = self
                .media_received
                .load(std::sync::atomic::Ordering::Relaxed)
                .to_string();
            let message = localization::tr_with("Files received: {count}", &[("count", &count)]);
            if status.media_status.as_ref() == Some(&message) {
                return false;
            }
            status.media_status = Some(message);
            true
        });
    }

    /// Fetch a missing file or resolve a file reference received from another device.
    pub(crate) async fn fetch_playback_file(
        &self,
        occurrence: &library::QueueOccurrence,
    ) -> Result<Option<LocalMediaLocation>, String> {
        let uri = &occurrence.media_uri;
        let received = self
            .database
            .connect_received_occurrence(&occurrence.occurrence)
            .await
            .map_err(error)?;
        if received && raw_document_reference(uri) {
            return Err("The source device did not provide a portable document identity".into());
        }
        let mut reference = self
            .database
            .connect_track_reference(uri)
            .await
            .map_err(error)?;
        if received
            && reference.is_none()
            && let Some(receipt) = self
                .database
                .connect_media_file(uri, self.status().settings.encoding.name())
                .await
                .map_err(error)?
        {
            if let Some(location) = location(&receipt.path)
                && let Some(location) = available_location(location).await?
            {
                self.register_queue_access(&occurrence.item, &location)
                    .await?;
                return Ok(Some(location));
            }
        }
        if self.session.read().await.is_none() {
            return Ok(None);
        }
        if reference.is_none() && received {
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
                    _ => {
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
                }
            }
            if reference.is_none() {
                return Err(unavailable);
            }
        }
        let Some(reference) = reference else {
            return Ok(None);
        };
        let encoding = self.status().settings.encoding;
        if let Some(path) = self.reusable_media(uri, encoding, &reference).await? {
            return Ok(Some(path));
        }
        let session = self.active().await?;
        if encoding == Encoding::Mp3
            && self
                .original_media_file(uri)
                .await
                .map_err(error)?
                .is_some()
        {
            let identity = session.identity.clone();
            self.request_media(&identity, uri, encoding, &CancellationToken::new(), None)
                .await
                .map_err(error)?;
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
                .original_media_file(uri)
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
            let Some(reference) = reference else {
                return Ok(None);
            };
            let encoding = self.status().settings.encoding;
            Ok(
                match self.selected_media_destination(uri, encoding, &reference)? {
                    Some(MediaDestination::Native(path)) => Some(path),
                    Some(MediaDestination::Document { .. }) => Some(self.media_path(
                        reference_backing(uri, &reference),
                        &revision(&reference),
                        encoding,
                        reference["source_format"].as_str(),
                    )),
                    None => None,
                },
            )
        })
    }

    fn finish<'a>(
        &'a self,
        file: library::ConnectMediaFile,
        destination: &'a Path,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            let _files = self.media_files.lock().await;
            self.store_media(file, destination).await.map(|_| ())
        })
    }

    fn remove<'a>(
        &'a self,
        uri: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<usize, String>> + Send + 'a>>
    {
        Box::pin(async move {
            let _files = self.media_files.lock().await;
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
            && self
                .original_media_file(uri)
                .await
                .map_err(SourceError::Other)?
                .is_some()
        {
            let identity = self
                .active()
                .await
                .map_err(SourceError::Network)?
                .identity
                .clone();
            self.request_media(&identity, uri, encoding, &cancel, None)
                .await?;
            reusable = self
                .reusable_media(uri, encoding, &reference)
                .await
                .map_err(SourceError::Other)?;
        }
        if let Some(path) = reusable {
            let path = match path {
                LocalMediaLocation::File(path) => path,
                LocalMediaLocation::Document(document) => self
                    .materialize_document(
                        &document,
                        reference_backing(uri, &reference),
                        &revision(&reference),
                        encoding,
                        &cancel,
                    )
                    .await
                    .map_err(SourceError::Other)?,
            };
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
        let mut unavailable = None;
        let mut missing = false;
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
                Err(SourceError::NotFound) => {
                    missing = true;
                    continue;
                }
                Err(error) if !cancel.is_cancelled() => {
                    unavailable = Some(error);
                    continue;
                }
                Err(_) => return Err(SourceError::Cancelled),
            };
            return self
                .receive_media(&peer, uri, partial, offer, cancel)
                .await
                .map_err(SourceError::Network);
        }
        Err(unavailable.unwrap_or_else(|| {
            if missing {
                SourceError::NotFound
            } else {
                SourceError::Network("Waiting for a device with this media".into())
            }
        }))
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
