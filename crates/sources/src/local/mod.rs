//! Local filesystem source.
//!
//! Local owns walking, tags, CUE interpretation, and artwork locators. It
//! produces canonical facts and inert change plans; it never retains another
//! queryable music library beside Library's selected collection.

use std::fs;
use std::path::{Path, PathBuf};

use library::{HomeFacts, LocalComponentReplacement};
use serde::Deserialize;

use crate::source::{BatchEmitter, SourceReadProgress};
use crate::{
    ConnectedSource, ImageBytes, LocalFolderHostInput, SourceConfiguration, SourceEditResult,
    SourceError, SourceResult,
};

mod artwork;
mod cue;
mod discovery;
mod lofty_metadata;
mod media;
pub(crate) mod metadata;
mod scan;
mod watch;

#[cfg(test)]
mod tests;

pub const LOCAL_SOURCE_ID: &str = "local";
pub const LOCAL_LIBRARY_SOURCE_ID: &str = "local:server:library";
const SOURCE_CONFIG_VERSION: u32 = 1;

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
        if roots.is_empty() {
            return Err(SourceError::InvalidConfig(
                "a Local source must contain at least one folder".to_string(),
            ));
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

    pub(crate) fn metadata_entry_available(&self, item: &library::MetadataItem) -> bool {
        metadata::entry_editing_available(&self.roots, item)
    }

    pub(crate) fn read_metadata(
        &self,
        subject: &library::MetadataSubject,
    ) -> Result<library::MetadataDraft, library::MetadataError> {
        metadata::read_subject(&self.roots, subject)
    }

    pub(crate) fn write_metadata(
        &self,
        subject: &library::MetadataSubject,
        edit: &library::MetadataEdit,
    ) -> Result<std::collections::BTreeSet<PathBuf>, library::MetadataError> {
        metadata::write_subject(&self.roots, subject, edit)
    }

    pub(crate) fn write_rating(
        &self,
        track: &library::Track,
        rating: Option<u8>,
    ) -> Result<bool, library::MetadataError> {
        metadata::write_rating(&self.roots, track, rating)
    }

    pub(super) fn read_facts(
        &self,
        emitter: &BatchEmitter,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<(Option<library::ProviderFreshness>, HomeFacts)> {
        scan::acquire_complete(&self.roots, emitter, progress, cancelled)?;
        Ok((None, HomeFacts::RufinDefined))
    }

    pub(crate) fn prepare_change(
        &self,
        library: &library::Library,
        change: crate::ObservedSourceChange,
        observed_at: i64,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<Option<LocalComponentReplacement>, crate::SourceChangePreparationError> {
        let check = match change {
            crate::ObservedSourceChange::LocalPaths(paths) => {
                scan::check_exact(&self.roots, paths, cancelled)
            }
            crate::ObservedSourceChange::LocalRescan => {
                scan::check_automatic(&self.roots, cancelled)
            }
            crate::ObservedSourceChange::Full | crate::ObservedSourceChange::Jellyfin { .. } => {
                return Err(crate::SourceError::InvalidRequest(
                    "filesystem change preparation requires a Local change",
                )
                .into());
            }
        }?;
        let file_baseline = library.local_file_baseline(check.file_seeds())?;
        let Some(change) = scan::confirm_change(check, file_baseline, progress, cancelled)? else {
            return Ok(None);
        };
        let component_baseline = library.local_component_baseline(change.component_seeds())?;
        scan::complete_change(change, component_baseline, observed_at, cancelled)
            .map(Some)
            .map_err(Into::into)
    }

    pub(crate) fn image_bytes(
        &self,
        artwork: &library::LocalArtworkRef,
    ) -> SourceResult<ImageBytes> {
        let reference = match artwork {
            library::LocalArtworkRef::File { path, .. } => {
                self::artwork::ArtworkReference::File(PathBuf::from(path))
            }
            library::LocalArtworkRef::Embedded {
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

    pub(crate) fn inspect_accepted_artwork(
        &self,
        library: &library::Library,
        observed_at: i64,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<Option<LocalComponentReplacement>, crate::SourceChangePreparationError> {
        let seeds = self
            .roots
            .iter()
            .map(|root| {
                library::LocalComponentSeed::DirectoryTree(root.to_string_lossy().into_owned())
            })
            .collect::<Vec<_>>();
        let baseline = library.local_component_baseline(&seeds)?;
        scan::inspect_accepted_artwork(baseline, observed_at, cancelled).map_err(Into::into)
    }

    pub(crate) fn watch(
        &self,
        on_ready: &mut dyn FnMut(bool) -> bool,
        on_change: &mut dyn FnMut(crate::ObservedSourceChange) -> bool,
        should_stop: &dyn Fn() -> bool,
    ) -> SourceResult<()> {
        watch::LocalChangeFeed::new(self.roots.clone()).listen_forever(
            on_ready,
            on_change,
            should_stop,
        )
    }
}

pub(crate) fn mapped_metadata_editing_available(item: &library::MetadataItem) -> bool {
    metadata::mapped_editing_available(item)
}

pub(crate) fn read_mapped_metadata(
    subject: &library::MetadataSubject,
    targets: &[library::LocalAccessTarget],
) -> Result<library::MetadataDraft, library::MetadataError> {
    metadata::read_mapped_subject(subject, targets)
}

pub(crate) fn write_mapped_metadata(
    subject: &library::MetadataSubject,
    targets: &[library::LocalAccessTarget],
    edit: &library::MetadataEdit,
) -> Result<std::collections::BTreeSet<PathBuf>, library::MetadataError> {
    metadata::write_mapped_subject(subject, targets, edit)
}

pub(crate) fn connect(input: LocalFolderHostInput) -> SourceResult<ConnectedSource> {
    let source = LocalSource::from_roots(input.roots)?;
    let configuration = crate::config::encode_provider_payload(
        library::SourceId::new(LOCAL_LIBRARY_SOURCE_ID),
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

pub fn read_local_access(
    root: &Path,
    baseline: &[library::LocalAccessFile],
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<Vec<library::LocalAccessFile>> {
    scan::acquire_local_access(root, baseline, progress, cancelled)
}

pub fn verify_local_media_file(path: &Path) -> SourceResult<()> {
    let path = fs::canonicalize(path).map_err(|error| {
        SourceError::Other(format!("Could not read {}: {error}", path.display()))
    })?;
    let mut worker = media::Worker::default();
    let read = media::read_media(&mut worker, path.clone(), None);
    let media::MediaRead::Accepted(scanned) = read else {
        return Err(SourceError::Other(format!(
            "Could not read {}",
            path.display()
        )));
    };
    if let Some(reference) = &scanned.track.local_artwork {
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
    if configured.is_empty() {
        return Err(SourceError::InvalidConfig(
            "a Local source must contain at least one folder".to_string(),
        ));
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
    if normalized.is_empty() {
        return Err(SourceError::InvalidRequest(
            "at least one Local music folder is required",
        ));
    }
    Ok(normalized)
}

pub(super) fn path_is_in_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}
