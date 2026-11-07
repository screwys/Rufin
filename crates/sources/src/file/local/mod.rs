//! Local filesystem source.
//!
//! Owns native paths, traversal and changes, and writes observations to Library.

use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::file::walk::{self, Entry, Root, WalkScope};
use crate::source::SourceProgress;
use crate::{
    ConnectedSource, ImageBytes, SourceConfiguration, SourceEditResult, SourceError, SourceResult,
};

pub(crate) mod artwork;
pub(crate) mod media;
pub(crate) mod scan;
mod watch;

pub const LOCAL_SOURCE_ID: &str = "local";
pub const LOCAL_LIBRARY_SOURCE_ID: &str = "local:server:library";
const SOURCE_CONFIG_VERSION: u32 = 1;

pub fn read_local_image(reference: &crate::LocalImageRef) -> SourceResult<ImageBytes> {
    let reference = match reference {
        crate::LocalImageRef::File { path, .. } => {
            artwork::ArtworkReference::File(PathBuf::from(path))
        }
        crate::LocalImageRef::Embedded {
            path,
            picture_index,
            ..
        } => artwork::ArtworkReference::Embedded {
            path: PathBuf::from(path),
            picture_index: *picture_index,
        },
    };
    artwork::read_image(&reference)
}

#[derive(Deserialize)]
struct LocalSourcePayload {
    version: u32,
    #[serde(default)]
    roots: Vec<String>,
    #[serde(default)]
    excluded_folders: Vec<PathBuf>,
    #[serde(default)]
    document_roots: Vec<crate::DocumentRoot>,
    #[serde(default, alias = "base_url")]
    legacy_root: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalSourceConfig {
    pub roots: Vec<PathBuf>,
    pub excluded_folders: Vec<PathBuf>,
    pub document_roots: Vec<crate::DocumentRoot>,
}

impl LocalSourceConfig {
    pub fn from_configuration(stored: &SourceConfiguration) -> SourceResult<Self> {
        if stored.kind != LOCAL_SOURCE_ID {
            return Err(SourceError::InvalidConfig(format!(
                "expected {LOCAL_SOURCE_ID}, found {}",
                stored.kind
            )));
        }
        let payload: LocalSourcePayload = crate::config::decode_provider_payload(stored)?;
        crate::config::require_payload_version(payload.version, SOURCE_CONFIG_VERSION)?;
        let mut roots = payload
            .roots
            .into_iter()
            .filter(|root| !root.trim().is_empty())
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        if roots.is_empty()
            && let Some(root) = payload.legacy_root.filter(|root| !root.trim().is_empty())
        {
            roots.push(PathBuf::from(root));
        }
        Ok(Self {
            roots,
            excluded_folders: payload.excluded_folders,
            document_roots: payload.document_roots,
        })
    }

    pub(crate) fn into_payload(self) -> serde_json::Value {
        let roots = self
            .roots
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let excluded_folders = self
            .excluded_folders
            .iter()
            .map(|folder| folder.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        serde_json::json!({
            "version": SOURCE_CONFIG_VERSION,
            "roots": roots,
            "excluded_folders": excluded_folders,
            "document_roots": self.document_roots,
        })
    }
}

pub struct LocalSource {
    roots: Vec<PathBuf>,
    excluded_folders: Vec<PathBuf>,
    pub(crate) documents: Vec<crate::file::remote::FileSource>,
    pub(crate) artwork: walk::ArtworkState,
}

impl LocalSource {
    pub fn from_configuration(configuration: &SourceConfiguration) -> SourceResult<Self> {
        let config = LocalSourceConfig::from_configuration(configuration)?;
        let roots = configured_roots(config.roots)?;
        let excluded_folders = excluded_paths(&roots, &config.excluded_folders);
        Ok(Self {
            roots,
            excluded_folders,
            documents: config
                .document_roots
                .into_iter()
                .map(|root| crate::file::remote::FileSource::documents(configuration, root))
                .collect(),
            artwork: walk::ArtworkState::default(),
        })
    }

    pub fn from_roots(roots: Vec<PathBuf>) -> SourceResult<Self> {
        let roots = normalize_roots(roots)?;
        Ok(Self {
            roots,
            excluded_folders: Vec::new(),
            documents: Vec::new(),
            artwork: walk::ArtworkState::default(),
        })
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub(crate) fn document_source(&self, path: &str) -> Option<&crate::file::remote::FileSource> {
        self.documents
            .iter()
            .find(|source| source.relative(path).is_ok())
    }

    pub(crate) fn document_root_name(&self, locator: &str) -> Option<&str> {
        self.documents.iter().find_map(|source| {
            (source.location("").ok().as_deref() == Some(locator))
                .then(|| {
                    source
                        .document_access()
                        .map(|access| access.root.name.as_str())
                })
                .flatten()
        })
    }

    pub(crate) fn document_playlist(
        &self,
        path: &str,
    ) -> Option<(&crate::file::remote::FileSource, String)> {
        if let Some(source) = self.document_source(path) {
            return Some((source, source.relative(path).ok()?));
        }
        let (index, path) = path.strip_prefix("@document:")?.split_once('/')?;
        Some((
            self.documents.get(index.parse::<usize>().ok()?)?,
            path.to_owned(),
        ))
    }

    pub(crate) fn excludes(&self, path: &std::path::Path) -> bool {
        self.excluded_folders
            .iter()
            .any(|folder| path.starts_with(folder))
    }

    pub(crate) fn configured_file_roots(&self) -> SourceResult<Vec<String>> {
        let mut roots = self
            .roots
            .iter()
            .filter(|root| !self.excludes(root))
            .map(|root| root.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        roots.extend(
            self.documents
                .iter()
                .map(|source| source.location(""))
                .collect::<SourceResult<Vec<_>>>()?,
        );
        Ok(roots)
    }

    /// Walks the folders, then each document folder. One that could not be listed fails the
    /// walk once the others are walked.
    pub(crate) async fn walk(
        &self,
        observation: &mut library::Observation<'_>,
        scope: WalkScope<'_>,
        progress: &(dyn Fn(SourceProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let configured = self.configured_file_roots()?;
        let (paths, rename) = match scope {
            WalkScope::Paths { paths, rename } => (Some(paths), rename),
            WalkScope::All { .. } => (None, None),
        };
        let native = paths.map(|paths| {
            paths
                .iter()
                .filter(|path| self.document_source(path).is_none())
                .cloned()
                .collect::<Vec<_>>()
        });
        let mut failure = walk::walk(
            &LocalFiles::importing(self, &[]),
            observation,
            &configured,
            match &native {
                Some(paths) => WalkScope::Paths {
                    paths,
                    rename: rename.filter(|(old, _)| self.document_source(old).is_none()),
                },
                None => scope,
            },
            &self.artwork,
            progress,
            cancelled,
        )
        .await
        .err();
        for source in &self.documents {
            if matches!(failure, Some(SourceError::Cancelled)) {
                break;
            }
            let paths = paths.map(|paths| {
                paths
                    .iter()
                    .filter(|path| source.relative(path).is_ok())
                    .cloned()
                    .collect::<Vec<_>>()
            });
            if paths.as_ref().is_some_and(Vec::is_empty) {
                continue;
            }
            let walked = walk::walk(
                source,
                observation,
                &configured,
                match &paths {
                    Some(paths) => WalkScope::Paths {
                        paths,
                        rename: rename.filter(|(old, _)| source.relative(old).is_ok()),
                    },
                    None => scope,
                },
                &self.artwork,
                progress,
                cancelled,
            )
            .await;
            failure = failure.or(walked.err());
        }
        failure.map_or(Ok(()), Err)
    }

    /// A playlist's local entries: each file outside the folders is an import root and stays
    /// listed. A missing file stays a reference until a later reload finds it.
    pub(crate) async fn import_playlist_files(
        &self,
        observation: &mut library::Observation<'_>,
        playlist: library::PlaylistKey,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let mut after = String::new();
        let mut paths = Vec::new();
        loop {
            let page = observation
                .catalog
                .playlist_import_page(playlist, &after)
                .await?;
            let Some((last, _)) = page.last() else { break };
            after.clone_from(last);
            for (uri, path) in page {
                let path = path
                    .or_else(|| {
                        library::file_media_path(&uri)
                            .map(|path| path.to_string_lossy().into_owned())
                    })
                    .or_else(|| uri.starts_with("content:").then(|| uri.clone()));
                if let Some(path) = path.filter(|path| {
                    path.starts_with("content:") || std::path::Path::new(path).is_absolute()
                }) {
                    paths.push(path);
                }
            }
        }
        let configured = self.configured_file_roots()?;
        paths.retain(|path| {
            let path_ref = std::path::Path::new(path);
            !configured.iter().any(|root| path_ref.starts_with(root))
                && (path.starts_with("content:") || path_ref.exists())
        });
        if !paths.is_empty() {
            walk::walk(
                &LocalFiles::importing(self, &paths),
                observation,
                &configured,
                WalkScope::Paths {
                    paths: &paths,
                    rename: None,
                },
                &self.artwork,
                &|_| {},
                cancelled,
            )
            .await?;
        }
        observation
            .record_imported_files(playlist, &configured)
            .await?;
        Ok(())
    }

    pub(crate) fn image_bytes(&self, artwork: &crate::LocalImageRef) -> SourceResult<ImageBytes> {
        let reference = match artwork {
            crate::LocalImageRef::File { path, .. } => {
                self::artwork::ArtworkReference::File(PathBuf::from(path))
            }
            crate::LocalImageRef::Embedded {
                path,
                picture_index,
                ..
            } => self::artwork::ArtworkReference::Embedded {
                path: PathBuf::from(path),
                picture_index: *picture_index,
            },
        };
        if !self
            .roots
            .iter()
            .any(|root| reference.path().starts_with(root))
        {
            return Err(SourceError::NotFound);
        }
        artwork::read_image(&reference)
    }

    pub(crate) async fn image(
        &self,
        request: crate::SourceImageRequest,
    ) -> SourceResult<ImageBytes> {
        if let crate::SourceImageRequest::Local(reference) = &request {
            let (crate::LocalImageRef::File { path, .. }
            | crate::LocalImageRef::Embedded { path, .. }) = reference;
            if let Some(source) = self.document_source(path) {
                return source.image(request).await;
            }
        }
        match request {
            crate::SourceImageRequest::Local(reference) => self.image_bytes(&reference),
            crate::SourceImageRequest::Native { .. } => Err(SourceError::NotFound),
        }
    }

    pub(crate) fn watch(
        &self,
        on_ready: &mut dyn FnMut(bool) -> bool,
        on_change: &mut dyn FnMut(crate::LocalLiveChange) -> bool,
        should_stop: &dyn Fn() -> bool,
    ) -> SourceResult<()> {
        watch::LocalChangeFeed::new(self.roots.clone()).listen_forever(
            on_ready,
            on_change,
            should_stop,
        )
    }
}

pub(crate) fn connect(
    source_id: crate::SourceId,
    roots: Vec<PathBuf>,
) -> SourceResult<ConnectedSource> {
    let source = LocalSource::from_roots(roots)?;
    let configuration = crate::config::encode_provider_payload(
        source_id,
        LOCAL_SOURCE_ID,
        "Local",
        LocalSourceConfig {
            roots: source.roots().to_vec(),
            excluded_folders: Vec::new(),
            document_roots: Vec::new(),
        }
        .into_payload(),
    );
    Ok(ConnectedSource::local(configuration, source))
}

pub(crate) fn edit(
    current: SourceConfiguration,
    roots: Vec<PathBuf>,
    excluded_folders: Option<Vec<PathBuf>>,
) -> SourceResult<SourceEditResult> {
    crate::source::require_source_edit(&current, LOCAL_SOURCE_ID)?;
    let excluded_folders = match excluded_folders {
        Some(folders) => folders,
        None => LocalSourceConfig::from_configuration(&current)?.excluded_folders,
    };
    let mut source = LocalSource::from_roots(roots)?;
    let document_roots = LocalSourceConfig::from_configuration(&current)?.document_roots;
    source.documents = document_roots
        .iter()
        .cloned()
        .map(|root| crate::file::remote::FileSource::documents(&current, root))
        .collect();
    source.excluded_folders = excluded_paths(source.roots(), &excluded_folders);
    let configuration = crate::config::encode_provider_payload(
        current.source_id.clone(),
        LOCAL_SOURCE_ID,
        current.name.clone(),
        LocalSourceConfig {
            roots: source.roots().to_vec(),
            excluded_folders,
            document_roots,
        }
        .into_payload(),
    );
    if configuration == current {
        return Ok(SourceEditResult::Unchanged);
    }
    Ok(SourceEditResult::Connected(Box::new(
        ConnectedSource::local(configuration, source),
    )))
}

pub(crate) fn configured_roots(roots: Vec<PathBuf>) -> SourceResult<Vec<PathBuf>> {
    let mut configured = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for root in roots {
        if !root.is_absolute() {
            return Err(SourceError::InvalidConfig(format!(
                "Local music folder is not absolute: {}",
                root.display()
            )));
        }
        if seen.insert(root.clone()) {
            configured.push(root);
        }
    }
    Ok(configured)
}

fn excluded_paths(roots: &[PathBuf], folders: &[PathBuf]) -> Vec<PathBuf> {
    folders
        .iter()
        .filter(|folder| !folder.as_os_str().is_empty())
        .flat_map(|folder| {
            if folder.is_absolute() {
                vec![scan::normalize_observed_path(folder)]
            } else {
                roots
                    .iter()
                    .map(|root| scan::normalize_observed_path(&root.join(folder)))
                    .collect()
            }
        })
        .map(|path| {
            let mut normalized = PathBuf::new();
            for part in path.components() {
                match part {
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        normalized.pop();
                    }
                    _ => normalized.push(part.as_os_str()),
                }
            }
            normalized
        })
        .collect()
}

pub(crate) fn local_file_location(
    roots: &[PathBuf],
    path: &std::path::Path,
) -> Option<(String, PathBuf)> {
    let host = library::playlist_host_path(path);
    roots.iter().enumerate().find_map(|(index, root)| {
        let root_host = library::playlist_host_path(root);
        let relative = host.strip_prefix(&root_host).ok()?;
        Some((
            format!("@{index}/{}", relative.to_string_lossy()),
            root.join(relative),
        ))
    })
}

fn normalize_roots(roots: Vec<PathBuf>) -> SourceResult<Vec<PathBuf>> {
    let mut normalized = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for root in roots {
        let root = fs::canonicalize(&root).map_err(|error| {
            SourceError::Other(format!("Could not read {}: {error}", root.display()))
        })?;
        if root.is_dir() {
            fs::read_dir(&root).map_err(|error| {
                SourceError::Other(format!("Could not read {}: {error}", root.display()))
            })?;
        } else {
            fs::File::open(&root).map_err(|error| {
                SourceError::Other(format!("Could not read {}: {error}", root.display()))
            })?;
        }
        if seen.insert(root.clone()) {
            normalized.push(root);
        }
    }
    Ok(normalized)
}

/// The facts of a listed or opened file.
pub(crate) fn entry(name: String, metadata: &fs::Metadata) -> Entry {
    #[cfg(unix)]
    let (device_id, inode) = {
        use std::os::unix::fs::MetadataExt;
        (
            i64::try_from(metadata.dev()).ok(),
            i64::try_from(metadata.ino()).ok(),
        )
    };
    #[cfg(not(unix))]
    let (device_id, inode) = (None, None);
    Entry {
        native: name.clone(),
        name,
        directory: metadata.is_dir(),
        size: (!metadata.is_dir())
            .then(|| i64::try_from(metadata.len()).ok())
            .flatten(),
        mtime_ns: metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
            .unwrap_or_default(),
        device_id,
        inode,
        native_id: None,
        revision: None,
    }
}

fn local_path(root: &str, relative: &str) -> PathBuf {
    if relative.is_empty() {
        PathBuf::from(root)
    } else {
        std::path::Path::new(root).join(relative.replace('/', std::path::MAIN_SEPARATOR_STR))
    }
}

pub(crate) fn local_row(root: &str, relative: &str, entry: &Entry) -> library::LocalFileWrite {
    let path = local_path(root, relative);
    let kind = if entry.directory {
        library::LocalFileKind::Directory
    } else if scan::is_cue(&path) {
        library::LocalFileKind::Cue
    } else if crate::file::artwork::supported_image(&path) {
        library::LocalFileKind::Image
    } else {
        library::LocalFileKind::Media
    };
    library::LocalFileWrite {
        path: path.to_string_lossy().into_owned(),
        root: root.to_owned(),
        relative_path: relative.to_owned(),
        kind,
        size_bytes: entry.size,
        mtime_ns: entry.mtime_ns,
        device_id: entry.device_id,
        inode: entry.inode,
        native_id: entry.native_id.clone(),
        revision: entry.revision.clone(),
        picture_index: None,
        artist_pictures: None,
        parse_version: Some(scan::LOCAL_PARSER_VERSION),
        state: library::LocalFileState::Accepted,
    }
}

/// The Local folders as a walk lists them, plus the roots an import adds.
pub(crate) struct LocalFiles<'a> {
    local: &'a LocalSource,
    /// Dropped files and folders, and playlist entries, outside the folders.
    pub(crate) imports: Vec<String>,
}

impl<'a> LocalFiles<'a> {
    pub(crate) fn importing(local: &'a LocalSource, paths: &[String]) -> Self {
        let imports = paths
            .iter()
            .filter(|path| {
                !local
                    .roots
                    .iter()
                    .any(|root| std::path::Path::new(path).starts_with(root))
            })
            .cloned()
            .collect();
        Self { local, imports }
    }

    fn walked(&self) -> impl Iterator<Item = String> + '_ {
        self.local
            .roots
            .iter()
            .filter(|root| !self.local.excludes(root))
            .map(|root| root.to_string_lossy().into_owned())
            .chain(self.imports.iter().cloned())
    }
}

impl walk::FileSource for LocalFiles<'_> {
    fn roots(&self) -> SourceResult<Vec<Root>> {
        Ok(self
            .walked()
            .map(|root| Root {
                root,
                starts: vec![(String::new(), String::new())],
            })
            .collect())
    }

    fn excluded(&self, root: &str, relative: &str) -> bool {
        !relative.is_empty() && self.local.excludes(&local_path(root, relative))
    }

    fn artwork(&self) -> crate::file::artwork::ArtworkFiles<'_> {
        crate::file::artwork::ArtworkFiles::Local
    }

    fn row(
        &self,
        root: &str,
        relative: &str,
        _native: &str,
        entry: &Entry,
    ) -> SourceResult<library::LocalFileWrite> {
        Ok(local_row(root, relative, entry))
    }

    fn place(&self, path: &str) -> Option<(String, String, String)> {
        if path.starts_with("content:") {
            return self
                .imports
                .contains(&path.to_owned())
                .then(|| (path.to_owned(), String::new(), String::new()));
        }
        let path = std::path::Path::new(path);
        let root = self
            .walked()
            .filter(|root| path.starts_with(root))
            .max_by_key(String::len)?;
        let relative = path
            .strip_prefix(&root)
            .ok()?
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        Some((root, relative.clone(), relative))
    }

    async fn list(&self, root: &str, native: &str) -> SourceResult<Vec<Entry>> {
        let directory = local_path(root, native);
        tokio::task::spawn_blocking(move || {
            let mut entries = Vec::new();
            for entry in
                fs::read_dir(&directory).map_err(|error| SourceError::Other(error.to_string()))?
            {
                let entry = entry.map_err(|error| SourceError::Other(error.to_string()))?;
                // A name that is not UTF-8 cannot be stored as a path the source opens.
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                let kind = entry
                    .file_type()
                    .map_err(|error| SourceError::Other(error.to_string()))?;
                // Symbolic links are not followed.
                if !kind.is_file() && !kind.is_dir() {
                    continue;
                }
                // An entry removed since the listing is gone; one that cannot be read fails it.
                let metadata = match entry.metadata() {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(SourceError::Other(error.to_string())),
                };
                entries.push(self::entry(name, &metadata));
            }
            Ok(entries)
        })
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
    }

    async fn stat(&self, root: &str, native: &str) -> SourceResult<Entry> {
        if root.starts_with("content:") {
            let uri = root.to_owned();
            let document = tokio::task::spawn_blocking(move || crate::stat_document(&uri))
                .await
                .map_err(|error| SourceError::Other(error.to_string()))??;
            return Ok(Entry {
                name: document.name.clone(),
                native: document.name,
                directory: document.directory,
                size: document.size.and_then(|size| i64::try_from(size).ok()),
                native_id: Some(document.native_id),
                revision: document.revision,
                ..Entry::default()
            });
        }
        let path = local_path(root, native);
        tokio::task::spawn_blocking(move || match fs::metadata(&path) {
            Ok(metadata) => Ok(entry(
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                &metadata,
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(SourceError::NotFound)
            }
            Err(error) => Err(SourceError::Other(error.to_string())),
        })
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
    }

    async fn read_media(
        &self,
        file: &mut library::LocalFileWrite,
        _native: &str,
        worker: std::sync::Arc<std::sync::Mutex<crate::file::media::Worker>>,
        _siblings: Option<&std::collections::BTreeSet<String>>,
    ) -> SourceResult<crate::file::media::MediaRead> {
        if file.path.starts_with("content:") {
            // A document a playlist named, outside the document folders.
            let (_, mut read) =
                crate::file::documents::read_document_media(&file.path, LOCAL_SOURCE_ID).await?;
            if let crate::file::media::MediaRead::Accepted(track) = &mut read {
                let identity = crate::document_identity(&file.path)?;
                track.track.object_id = crate::file::media::track_id(std::path::Path::new(
                    file.native_id.as_deref().unwrap_or(&file.path),
                ));
                track.track.media_uri = Some(library::document_media_uri(&identity));
                track.track.source_path = Some(file.path.clone());
                track.audio_revision = crate::file::remote::scan::audio_revision(
                    &file.path,
                    file.revision.as_deref(),
                    file.size_bytes,
                );
                track.revision_hashes_audio = true;
            }
            file.parse_version = Some(crate::file::remote::scan::PARSER_VERSION);
            return Ok(read);
        }
        let path = PathBuf::from(&file.path);
        tokio::task::spawn_blocking(move || {
            let mut worker = worker.lock().unwrap_or_else(|error| error.into_inner());
            media::read_media(&mut worker, path, None)
        })
        .await
        .map_err(|error| SourceError::Other(error.to_string()))
    }

    async fn read_cue(
        &self,
        file: &mut library::LocalFileWrite,
        _native: &str,
        worker: std::sync::Arc<std::sync::Mutex<crate::file::media::Worker>>,
        _siblings: Option<&std::collections::BTreeSet<String>>,
    ) -> SourceResult<walk::CueRead> {
        let path = PathBuf::from(&file.path);
        tokio::task::spawn_blocking(move || {
            let mut worker = worker.lock().unwrap_or_else(|error| error.into_inner());
            scan::read_local_cue(&mut worker, &path)
        })
        .await
        .map_err(|error| SourceError::Other(error.to_string()))
    }
}
