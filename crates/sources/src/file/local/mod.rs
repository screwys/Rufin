//! Local filesystem source.
//!
//! Local owns native paths, traversal, and change notifications. It
//! produces canonical facts and inert change plans; it never retains another
//! queryable music library beside Library's selected collection.

use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::source::SourceReadProgress;
use crate::{
    ConnectedSource, ImageBytes, LocalFolderHostInput, SourceConfiguration, SourceEditResult,
    SourceError, SourceResult,
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

    pub(crate) async fn stage_catalog(
        &self,
        database: &library::Database,
        scan: &mut library::Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
        reuse_unchanged: bool,
    ) -> SourceResult<()> {
        scan::stage_catalog(database, self, scan, progress, cancelled, reuse_unchanged).await?;
        for source in &self.documents {
            source
                .stage_inventory(database, scan, progress, cancelled)
                .await?;
        }
        for source in &self.documents {
            source
                .stage_cues(
                    database,
                    scan,
                    &|completed| {
                        progress(SourceReadProgress {
                            stage: crate::SourceReadStage::Tracks,
                            completed,
                            total: None,
                        })
                    },
                    cancelled,
                    None,
                )
                .await?;
        }
        for source in &self.documents {
            source
                .stage_media_files(database, scan, progress, cancelled, None, 0)
                .await?;
        }
        crate::file::artwork::ArtworkFiles::Local
            .stage(database, scan, cancelled)
            .await?;
        scan.retain_connected_file_tracks().await?;
        Ok(())
    }

    pub(crate) async fn import_playlist_files(
        &self,
        database: &library::Database,
        source_id: &str,
        playlist: library::PlaylistKey,
    ) -> SourceResult<library::ScanOutcome> {
        let mut scan = library::Scan::begin_local_items(database, source_id).await?;
        scan::stage_imported_paths(database, &mut scan, Some(playlist)).await?;
        crate::file::artwork::ArtworkFiles::Local
            .stage(database, &mut scan, &|| false)
            .await?;
        Ok(scan.finish().await?)
    }

    pub(crate) async fn publish_metadata_paths(
        &self,
        database: &library::Database,
        source_id: &str,
        paths: &[PathBuf],
        removed_album: Option<&str>,
        removed_artist: Option<&str>,
    ) -> SourceResult<library::ScanOutcome> {
        scan::publish_metadata_paths(database, source_id, paths, removed_album, removed_artist)
            .await
    }

    pub(crate) async fn stage_metadata_paths(
        &self,
        scan: &mut library::Scan,
        paths: &[PathBuf],
    ) -> SourceResult<()> {
        scan::stage_metadata_paths(scan, paths).await
    }

    pub(crate) async fn publish_paths(
        &self,
        database: &library::Database,
        source: library::SourceKey,
        source_id: &str,
        paths: &[PathBuf],
        rename: Option<&(PathBuf, PathBuf)>,
    ) -> SourceResult<library::ScanOutcome> {
        scan::publish_paths(database, source, source_id, self, paths, rename).await
    }

    pub(crate) async fn catch_up(
        &self,
        database: &library::Database,
        source: library::SourceKey,
        source_id: &str,
        progress: &(dyn Fn(crate::SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<library::ScanOutcome> {
        if !self.documents.is_empty() {
            let mut scan =
                library::Scan::begin(database, source_id, "Local", "local", None).await?;
            self.stage_catalog(database, &mut scan, progress, cancelled, true)
                .await?;
            return scan.finish().await.map_err(Into::into);
        }
        scan::catch_up(database, source, source_id, self, progress, cancelled).await
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
    input: LocalFolderHostInput,
) -> SourceResult<ConnectedSource> {
    let source = LocalSource::from_roots(input.roots)?;
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
