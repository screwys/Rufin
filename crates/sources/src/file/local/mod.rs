//! Local filesystem source.
//!
//! Local owns native paths, traversal, and change notifications. It
//! produces canonical facts and inert change plans; it never retains another
//! queryable music library beside Library's selected collection.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::source::SourceReadProgress;
use crate::{
    ConnectedSource, ImageBytes, LocalFolderHostInput, SourceConfiguration, SourceEditResult,
    SourceError, SourceResult,
};

use crate::file::media::{MediaRead, Worker};
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
    #[serde(default, alias = "base_url")]
    legacy_root: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalSourceConfig {
    pub roots: Vec<PathBuf>,
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
        Ok(Self { roots })
    }

    pub(crate) fn into_payload(self) -> serde_json::Value {
        let roots = self
            .roots
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        serde_json::json!({
            "version": SOURCE_CONFIG_VERSION,
            "roots": roots,
        })
    }
}

#[derive(Debug)]
pub struct LocalSource {
    roots: Vec<PathBuf>,
}

impl LocalSource {
    pub fn from_configuration(configuration: &SourceConfiguration) -> SourceResult<Self> {
        let config = LocalSourceConfig::from_configuration(configuration)?;
        let roots = configured_roots(config.roots)?;
        Ok(Self { roots })
    }

    pub fn from_roots(roots: Vec<PathBuf>) -> SourceResult<Self> {
        let roots = normalize_roots(roots)?;
        Ok(Self { roots })
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub(crate) async fn stage_catalog(
        &self,
        database: &library::Database,
        scan: &mut library::Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
        reuse_unchanged: bool,
    ) -> SourceResult<()> {
        scan::stage_catalog(
            database,
            &self.roots,
            scan,
            progress,
            cancelled,
            reuse_unchanged,
        )
        .await
    }

    pub(crate) async fn import_playlist_files(
        &self,
        database: &library::Database,
        source_id: &str,
        playlist: library::PlaylistKey,
    ) -> SourceResult<library::ScanOutcome> {
        database
            .import_local_playlist_paths(source_id, playlist)
            .await?;
        let mut scan = library::Scan::begin_items(database, source_id).await?;
        scan::stage_imported_paths(database, &mut scan).await?;
        scan::stage_artwork(database, &mut scan, &|| false).await?;
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

    pub(crate) async fn publish_paths(
        &self,
        database: &library::Database,
        source: library::SourceKey,
        source_id: &str,
        paths: &[PathBuf],
        rename: Option<&(PathBuf, PathBuf)>,
    ) -> SourceResult<library::ScanOutcome> {
        scan::publish_paths(database, source, source_id, &self.roots, paths, rename).await
    }

    pub(crate) async fn catch_up(
        &self,
        database: &library::Database,
        source: library::SourceKey,
        source_id: &str,
        progress: &(dyn Fn(crate::SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<library::ScanOutcome> {
        scan::catch_up(
            database,
            source,
            source_id,
            &self.roots,
            progress,
            cancelled,
        )
        .await
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

    pub(crate) fn image(&self, request: crate::SourceImageRequest) -> SourceResult<ImageBytes> {
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
        }
        .into_payload(),
    );
    Ok(ConnectedSource::local(configuration, source))
}

pub(crate) fn edit(
    current: SourceConfiguration,
    roots: Vec<PathBuf>,
) -> SourceResult<SourceEditResult> {
    crate::source::require_source_edit(&current, LOCAL_SOURCE_ID)?;
    let source = LocalSource::from_roots(roots)?;
    let configuration = crate::config::encode_provider_payload(
        current.source_id.clone(),
        LOCAL_SOURCE_ID,
        current.name.clone(),
        LocalSourceConfig {
            roots: source.roots().to_vec(),
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

pub fn verify_local_media_file(path: &Path) -> SourceResult<()> {
    let path = fs::canonicalize(path).map_err(|error| {
        SourceError::Other(format!("Could not read {}: {error}", path.display()))
    })?;
    let mut worker = Worker::default();
    let read = media::read_media(&mut worker, path.clone(), None);
    let MediaRead::Accepted(scanned) = read else {
        return Err(SourceError::Other(format!(
            "Could not read {}",
            path.display()
        )));
    };
    if let Some(reference) = &scanned.local_artwork {
        let root = path.parent().ok_or(SourceError::NotFound)?.to_path_buf();
        LocalSource { roots: vec![root] }.image_bytes(reference)?;
    }
    Ok(())
}

pub(crate) fn configured_roots(roots: Vec<PathBuf>) -> SourceResult<Vec<PathBuf>> {
    let mut configured = Vec::new();
    for root in roots {
        if !root.is_absolute() {
            return Err(SourceError::InvalidConfig(format!(
                "Local music folder is not absolute: {}",
                root.display()
            )));
        }
        if !configured.iter().any(|accepted| accepted == &root) {
            configured.push(root);
        }
    }
    Ok(configured)
}

fn normalize_roots(roots: Vec<PathBuf>) -> SourceResult<Vec<PathBuf>> {
    let mut normalized = Vec::new();
    for root in roots {
        let root = fs::canonicalize(&root).map_err(|error| {
            SourceError::Other(format!("Could not read {}: {error}", root.display()))
        })?;
        if !root.is_dir() {
            return Err(SourceError::Other(format!(
                "{} is not a music folder",
                root.display()
            )));
        }
        fs::read_dir(&root).map_err(|error| {
            SourceError::Other(format!("Could not read {}: {error}", root.display()))
        })?;
        if !normalized.iter().any(|accepted| accepted == &root) {
            normalized.push(root);
        }
    }
    Ok(normalized)
}
