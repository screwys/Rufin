//! Local filesystem source.
//!
//! Owns native paths, traversal and changes, and writes observations to Library.

use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::file::check_cancelled;
use crate::source::SourceReadProgress;
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
        })
    }

    pub fn from_roots(roots: Vec<PathBuf>) -> SourceResult<Self> {
        let roots = normalize_roots(roots)?;
        Ok(Self {
            roots,
            excluded_folders: Vec::new(),
            documents: Vec::new(),
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

    pub(crate) async fn acquire_catalog(
        &self,
        catalog: &mut library::Catalog<'_>,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
        reuse_unchanged: bool,
    ) -> SourceResult<()> {
        scan::acquire_catalog(catalog, self, progress, cancelled, reuse_unchanged).await?;
        for source in &self.documents {
            source.acquire_catalog(catalog, progress, cancelled).await?;
        }
        if catalog.changes().metadata {
            catalog.record_imported_files(None).await?;
        }
        check_cancelled(cancelled)?;
        catalog
            .update_source_file_membership(&self.configured_file_roots()?, &self.excluded_folders)
            .await?;
        crate::file::artwork::ArtworkFiles::Local
            .acquire(catalog, progress, cancelled)
            .await?;
        progress(SourceReadProgress {
            stage: crate::SourceReadStage::Finalizing,
            completed: 0,
            total: None,
        });
        Ok(())
    }

    pub(crate) async fn import_playlist_files(
        &self,
        catalog: &mut library::Catalog<'_>,
        playlist: library::PlaylistKey,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        catalog.start_observation().await?;
        scan::acquire_imported_paths(catalog, self, Some(playlist), cancelled).await?;
        catalog.record_imported_files(Some(playlist)).await?;
        crate::file::artwork::ArtworkFiles::Local
            .acquire(catalog, &|_| {}, cancelled)
            .await
    }

    pub(crate) async fn acquire_paths(
        &self,
        catalog: &mut library::Catalog<'_>,
        paths: &[PathBuf],
        rename: Option<&(PathBuf, PathBuf)>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        for documents in &self.documents {
            let relative = paths
                .iter()
                .filter_map(|path| documents.relative(&path.to_string_lossy()).ok())
                .collect::<Vec<_>>();
            if !relative.is_empty() {
                let renamed = rename.and_then(|(from, to)| {
                    Some((
                        documents.relative(&from.to_string_lossy()).ok()?,
                        documents.relative(&to.to_string_lossy()).ok()?,
                    ))
                });
                documents
                    .acquire_paths(catalog, &relative, renamed.as_ref(), cancelled)
                    .await?;
            }
        }
        let native = paths
            .iter()
            .filter(|path| self.document_source(&path.to_string_lossy()).is_none())
            .cloned()
            .collect::<Vec<_>>();
        scan::acquire_paths(catalog, self, &native, rename, &|_| {}, cancelled).await?;
        Ok(())
    }

    pub(crate) async fn catch_up(
        &self,
        catalog: &mut library::Catalog<'_>,
        progress: &(dyn Fn(crate::SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let database = catalog.database;
        let expected = self
            .configured_file_roots()?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let stored = database
            .local_file_roots(catalog.source)
            .await?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if !expected.is_subset(&stored) || !self.documents.is_empty() {
            catalog.start_observation().await?;
            let result = self
                .acquire_catalog(catalog, progress, cancelled, true)
                .await;
            return result;
        }
        scan::catch_up(catalog, self, progress, cancelled).await?;
        check_cancelled(cancelled)?;
        catalog
            .update_source_file_membership(&self.configured_file_roots()?, &self.excluded_folders)
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
