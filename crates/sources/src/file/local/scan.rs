//! Reads Local observations in bounded batches and completes actual directory listings.

use futures_util::{StreamExt, TryStreamExt, stream};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use super::LocalSource;
use super::media::read_media;
use crate::file::check_cancelled;
use crate::file::cue::{CueSheet, cue_track, parse_cue_sheet};
use crate::file::media::{self as media, MediaRead, ScannedTrack};
use crate::source::{SourceReadProgress, SourceReadStage};
use crate::{SourceError, SourceResult};

const LOCAL_BATCH_SIZE: usize = 128;
const LOCAL_CUE_MAX_BYTES: u64 = 1024 * 1024;
pub(crate) const LOCAL_PARSER_VERSION: u32 = 18;

fn reusable_parser_version(version: Option<i64>) -> bool {
    // Accepted released metadata remains usable; newer readers retry rejected files.
    matches!(version, Some(14..=18))
}

pub(crate) async fn read_saved_file(
    catalog: &library::Catalog<'_>,
    media_uri: &str,
    path: &Path,
) -> SourceResult<(Option<library::LocalFileWrite>, library::TrackObservation)> {
    let (object_id, source_path, previous) = catalog
        .database
        .saved_file_observation(catalog.source, media_uri)
        .await?
        .ok_or(SourceError::NotFound)?;
    let path = normalize_observed_path(path);
    let (mut track, cover, portraits) = media::read_saved_media(&path, path.clone()).await?;
    let saved_path = path.clone();
    let (mut observation, revision, audio_revision) = tokio::task::spawn_blocking(move || {
        let observation = previous
            .map(|previous| {
                let roots = BTreeSet::from([Path::new(&previous.root)]);
                let (mut observation, _) = file_observation(
                    &roots,
                    &saved_path,
                    library::LocalFileKind::Media,
                    library::LocalFileState::Accepted,
                    &[],
                )?;
                observation.root = previous.root;
                observation.relative_path = previous.relative_path;
                Ok::<_, SourceError>(observation)
            })
            .transpose()?;
        Ok::<_, SourceError>((
            observation,
            super::artwork::revision(&saved_path).unwrap_or_default(),
            super::media::audio_revision(
                &saved_path.to_string_lossy(),
                &super::media::local_media_identity_path(&saved_path),
            ),
        ))
    })
    .await
    .map_err(|error| SourceError::Other(error.to_string()))??;
    if let Some(observation) = &mut observation {
        observation.picture_index = cover.map(i64::from);
        observation.artist_pictures = Some(portraits);
    }
    track.local_artwork = cover.map(|picture_index| crate::LocalImageRef::Embedded {
        source_id: crate::SourceId::new(catalog.source_id),
        path: path.to_string_lossy().into_owned(),
        picture_index,
        revision,
    });
    track.track.object_id = object_id;
    track.track.media_uri = Some(media_uri.to_owned());
    if source_path.is_some() {
        track.track.source_path = source_path;
    }
    track.audio_revision = audio_revision;
    Ok((observation, track.into_observation(media::accepted_at())))
}

pub(crate) async fn read_saved_document(
    catalog: &library::Catalog<'_>,
    media_uri: &str,
    document_uri: &str,
    temporary: &Path,
) -> SourceResult<(Option<library::LocalFileWrite>, library::TrackObservation)> {
    let (object_id, source_path, mut observation) = catalog
        .database
        .saved_file_observation(catalog.source, media_uri)
        .await?
        .ok_or(SourceError::NotFound)?;
    let stat_uri = document_uri.to_owned();
    let entry = tokio::task::spawn_blocking(move || crate::stat_document(&stat_uri))
        .await
        .map_err(|error| SourceError::Other(error.to_string()))??;
    let (mut track, cover, portraits) =
        media::read_saved_media(temporary, PathBuf::from(&entry.name)).await?;
    let path = source_path.unwrap_or_else(|| document_uri.to_owned());
    if let Some(observation) = &mut observation {
        observation.size_bytes = entry.size.and_then(|size| i64::try_from(size).ok());
        observation.revision = entry.revision.clone();
        observation.native_id = Some(entry.native_id);
        observation.picture_index = cover.map(i64::from);
        observation.artist_pictures = Some(portraits);
        observation.parse_version = Some(super::super::remote::scan::PARSER_VERSION);
        observation.state = library::LocalFileState::Accepted;
    }
    track.track.object_id = object_id;
    track.track.media_uri = Some(media_uri.to_owned());
    track.track.source_path = Some(path.clone());
    track.audio_revision = super::super::remote::scan::audio_revision(
        &path,
        entry.revision.as_deref(),
        entry.size.and_then(|size| i64::try_from(size).ok()),
    );
    track.local_artwork = cover.map(|picture_index| crate::LocalImageRef::Embedded {
        source_id: crate::SourceId::new(catalog.source_id),
        path,
        revision: entry.revision.unwrap_or_default(),
        picture_index,
    });
    Ok((observation, track.into_observation(media::accepted_at())))
}

pub(crate) async fn catch_up(
    catalog: &mut library::Catalog<'_>,
    local: &LocalSource,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()> {
    let database = catalog.database;
    let source_id = catalog.source_id;
    require_roots(database, source_id, local, local.roots()).await?;
    let stamp = Some(catalog.start_observation().await?);
    let mut after = None;
    let mut completed = 0;
    loop {
        check_cancelled(cancelled)?;
        let page = database
            .local_file_page(
                catalog.source,
                after,
                LOCAL_BATCH_SIZE,
                &library::ReadCancellation::new(),
            )
            .await?;
        if page.is_empty() {
            break;
        }
        after = page.last().map(|file| file.local_file_key);
        completed += page.len();
        let paths = page
            .iter()
            .filter(|file| {
                local
                    .roots()
                    .iter()
                    .any(|root| Path::new(&file.path).starts_with(root))
            })
            .filter(|file| cached_observation_changed(file))
            .map(|file| PathBuf::from(&file.path))
            .collect::<Vec<_>>();
        if !paths.is_empty() {
            progress(SourceReadProgress {
                stage: SourceReadStage::Files,
                completed,
                total: None,
            });
            acquire_paths(catalog, local, &paths, None, progress, cancelled).await?;
        }
    }
    acquire_imported_paths(catalog, local, None, cancelled).await?;
    if catalog.changes().metadata {
        catalog.record_imported_files(None).await?;
    }
    check_cancelled(cancelled)?;
    catalog
        .purge_source_file_absences(stamp.expect("file observation"))
        .await?;
    Ok(())
}

fn cached_observation_changed(file: &library::LocalFileRow) -> bool {
    let Ok(metadata) = fs::metadata(&file.path) else {
        return true;
    };
    let size = i64::try_from(metadata.len()).ok();
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or_default();
    file.state == library::LocalFileState::Rejected
        && file.parse_version != Some(i64::from(LOCAL_PARSER_VERSION))
        || file.state == library::LocalFileState::Unreadable
        || file.state == library::LocalFileState::Observed
        || file.size_bytes != size
        || file.mtime_ns != mtime_ns
        || file.kind == library::LocalFileKind::Directory
            && file.state != library::LocalFileState::Accepted
        || matches!(
            file.kind,
            library::LocalFileKind::Media | library::LocalFileKind::Cue
        ) && !reusable_parser_version(file.parse_version)
}

pub(crate) async fn acquire_paths(
    catalog: &mut library::Catalog<'_>,
    local: &LocalSource,
    paths: &[PathBuf],
    rename: Option<&(PathBuf, PathBuf)>,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()> {
    let database = catalog.database;
    let source_id = catalog.source_id;
    require_roots(database, source_id, local, paths).await?;
    let roots = local
        .roots()
        .iter()
        .map(PathBuf::as_path)
        .collect::<BTreeSet<_>>();
    let paths = paths
        .iter()
        .map(|path| normalize_observed_path(path))
        .filter(|path| !local.excludes(path))
        .collect::<Vec<_>>();
    let seeds = paths
        .iter()
        .filter(|path| roots.iter().any(|root| path.starts_with(root)))
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if seeds.is_empty() {
        return Ok(());
    }
    let rename =
        rename.map(|(old, new)| (normalize_observed_path(old), normalize_observed_path(new)));
    let directories = paths
        .iter()
        .filter(|path| path.is_dir())
        .cloned()
        .collect::<Vec<_>>();
    let parsers = local_workers();
    acquire_directories(
        catalog,
        local,
        &directories,
        progress,
        cancelled,
        true,
        true,
        &parsers,
    )
    .await?;
    let prefixes = paths
        .iter()
        .filter_map(|path| {
            let directory = if crate::file::artwork::supported_image(path) {
                path.parent()
            } else {
                Some(path.as_path())
            }?;
            Some(format!(
                "{}{}",
                directory
                    .to_string_lossy()
                    .trim_end_matches(std::path::is_separator),
                std::path::MAIN_SEPARATOR
            ))
        })
        .collect::<Vec<_>>();
    acquire_component(
        catalog,
        &roots,
        local,
        &seeds,
        &prefixes,
        &directories,
        rename.as_ref(),
        progress,
        cancelled,
        &parsers,
    )
    .await?;
    progress(SourceReadProgress {
        stage: SourceReadStage::Artwork,
        completed: 0,
        total: None,
    });
    crate::file::artwork::ArtworkFiles::Local
        .acquire(catalog, cancelled)
        .await?;
    progress(SourceReadProgress {
        stage: SourceReadStage::Finalizing,
        completed: 0,
        total: None,
    });
    Ok(())
}

async fn require_roots(
    database: &library::Database,
    source_id: &str,
    local: &LocalSource,
    paths: &[PathBuf],
) -> SourceResult<()> {
    for root in local
        .roots()
        .iter()
        .filter(|root| paths.iter().any(|path| path.starts_with(root)))
    {
        if !root
            .try_exists()
            .map_err(|error| SourceError::Other(error.to_string()))?
        {
            let previous = database
                .source_files(source_id, &[root.to_string_lossy().into_owned()])
                .await?;
            if !previous.first().is_some_and(|file| {
                matches!(
                    file.kind,
                    library::LocalFileKind::Media | library::LocalFileKind::Cue
                )
            }) {
                return Err(SourceError::Other(format!(
                    "Local root is unavailable: {}",
                    root.display()
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn normalize_observed_path(path: &Path) -> PathBuf {
    if let Ok(path) = fs::canonicalize(path) {
        return path;
    }
    let mut current = path;
    let mut missing = Vec::new();
    while let Some(name) = current.file_name() {
        missing.push(name.to_os_string());
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent;
        if let Ok(mut canonical) = fs::canonicalize(current) {
            for component in missing.iter().rev() {
                canonical.push(component);
            }
            return canonical;
        }
    }
    path.to_path_buf()
}

#[allow(clippy::too_many_arguments)]
async fn acquire_component(
    catalog: &mut library::Catalog<'_>,
    roots: &BTreeSet<&Path>,
    local: &LocalSource,
    seeds: &[String],
    prefixes: &[String],
    listed: &[PathBuf],
    rename: Option<&(PathBuf, PathBuf)>,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
    parsers: &[Arc<Mutex<media::Worker>>],
) -> SourceResult<()> {
    let database = catalog.database;
    let source_id = catalog.source_id;
    let stamp = catalog.observed;
    let mut parsed = 0;
    progress(SourceReadProgress {
        stage: SourceReadStage::Tracks,
        completed: parsed,
        total: None,
    });

    let mut after = None;
    loop {
        let page = database
            .source_component_page(source_id, seeds, prefixes, after.as_deref())
            .await?;
        if page.is_empty() {
            break;
        }
        after = page.last().cloned();
        let mut missing = Vec::new();
        let mut cues = Vec::new();
        let mut audio = Vec::new();
        let mut observations = Vec::new();
        for path in page.into_iter().map(PathBuf::from) {
            if local.excludes(&path) || !roots.iter().any(|root| path.starts_with(root)) {
                continue;
            }
            if listed.iter().any(|directory| path.starts_with(directory)) {
                continue;
            }
            if !path.exists() {
                missing.push(path.to_string_lossy().into_owned());
            } else if path.is_dir() {
                continue;
            } else if crate::file::artwork::supported_image(&path) {
                observations.push(file_observation(
                    roots,
                    &path,
                    library::LocalFileKind::Image,
                    library::LocalFileState::Observed,
                    &[],
                )?);
            } else if is_cue(&path) {
                cues.push(path);
            } else if path.is_file() {
                audio.push(path);
            }
        }
        if !missing.is_empty() {
            check_cancelled(cancelled)?;
            catalog
                .remove_source_files(&missing, stamp.expect("file observation"))
                .await?;
        }
        for path in cues {
            parsed += acquire_exact_cue(catalog, roots, &path, true, true, cancelled).await?;
            progress(SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed: parsed,
                total: None,
            });
        }
        let dependencies = database
            .source_dependency_paths(
                source_id,
                &audio
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
                stamp,
                true,
            )
            .await?;
        audio.retain(|path| {
            !dependencies
                .iter()
                .any(|dependency| dependency == path.to_string_lossy().as_ref())
        });
        if !audio.is_empty() {
            parsed += acquire_audio_batch(
                catalog, roots, &audio, cancelled, true, rename, parsers, true,
            )
            .await?;
            progress(SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed: parsed,
                total: None,
            });
        }
        if !observations.is_empty() {
            check_cancelled(cancelled)?;
            catalog
                .write_file_observations(&observations, Vec::new(), true)
                .await?;
        }
    }

    {
        progress(SourceReadProgress {
            stage: SourceReadStage::Finalizing,
            completed: parsed,
            total: None,
        });
    }
    Ok(())
}

async fn acquire_exact_cue(
    catalog: &mut library::Catalog<'_>,
    roots: &BTreeSet<&Path>,
    path: &Path,
    retain_unreadable: bool,
    admit: bool,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<usize> {
    let database = catalog.database;
    let source_id = catalog.source_id;
    let (state, mut dependencies, tracks, retain, dependencies_observed) = match read_cue(path) {
        CueRead::Accepted(sheet) => {
            let dependencies = sheet
                .files
                .iter()
                .map(|file| {
                    normalize_observed_path(&resolve_cue_file_path(path, &file.path))
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<Vec<_>>();
            let tracks = read_cue_tracks(&mut media::Worker::default(), path, sheet);
            let dependencies_observed = tracks.is_some();
            let state = if tracks.is_some() {
                library::LocalFileState::Accepted
            } else {
                library::LocalFileState::Unreadable
            };
            let retain = retain_unreadable && tracks.is_none();
            (
                state,
                dependencies,
                tracks.unwrap_or_default(),
                retain,
                dependencies_observed,
            )
        }
        CueRead::Rejected => (
            library::LocalFileState::Rejected,
            Vec::new(),
            Vec::new(),
            false,
            false,
        ),
        CueRead::Unreadable => (
            library::LocalFileState::Unreadable,
            Vec::new(),
            Vec::new(),
            retain_unreadable,
            false,
        ),
    };
    if retain {
        for path in retained_cue_dependencies(database, source_id, roots, path).await? {
            if !dependencies.contains(&path) {
                dependencies.push(path);
            }
        }
    }
    let accepted = tracks.len();
    let observed_tracks = tracks
        .into_iter()
        .map(|track| track.into_observation(media::accepted_at()))
        .collect::<Vec<_>>();
    let mut observations = vec![file_observation(
        roots,
        path,
        library::LocalFileKind::Cue,
        state,
        &dependencies,
    )?];
    if dependencies_observed {
        for path in dependencies
            .iter()
            .map(PathBuf::from)
            .filter(|path| path.is_file() && roots.iter().any(|root| path.starts_with(root)))
        {
            observations.push(file_observation(
                roots,
                &path,
                library::LocalFileKind::Media,
                library::LocalFileState::Accepted,
                &[],
            )?);
        }
    }
    check_cancelled(cancelled)?;
    catalog
        .write_file_observations(&observations, observed_tracks, admit)
        .await?;
    Ok(accepted)
}

pub(crate) async fn acquire_catalog(
    catalog: &mut library::Catalog<'_>,
    local: &LocalSource,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
    reuse_unchanged: bool,
) -> SourceResult<()> {
    acquire_directories(
        catalog,
        local,
        local.roots(),
        progress,
        cancelled,
        reuse_unchanged,
        true,
        &local_workers(),
    )
    .await?;
    acquire_imported_paths(catalog, local, None, cancelled).await?;
    Ok(())
}

pub(crate) async fn import_paths(
    catalog: &mut library::Catalog<'_>,
    configured: &[PathBuf],
    paths: Vec<PathBuf>,
    name: &str,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<library::PlaylistKey> {
    let mut local = LocalSource::from_roots(paths)?;
    let starts = local.roots().to_vec();
    local.roots.retain(|root| {
        !configured
            .iter()
            .any(|configured| root.starts_with(configured))
    });
    local.roots.extend_from_slice(configured);
    let stamp = Some(catalog.start_observation().await?);
    acquire_directories(
        catalog,
        &local,
        &starts,
        progress,
        cancelled,
        true,
        true,
        &local_workers(),
    )
    .await?;
    check_cancelled(cancelled)?;
    catalog
        .purge_source_file_absences(stamp.expect("file observation"))
        .await?;
    crate::file::artwork::ArtworkFiles::Local
        .acquire(catalog, cancelled)
        .await?;
    let playlist = catalog
        .create_observed_playlist(stamp.unwrap(), name)
        .await?;
    catalog.record_imported_files(Some(playlist)).await?;
    Ok(playlist)
}

async fn acquire_directories(
    catalog: &mut library::Catalog<'_>,
    local: &LocalSource,
    starts: &[PathBuf],
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
    reuse_unchanged: bool,
    admit: bool,
    parsers: &[Arc<Mutex<media::Worker>>],
) -> SourceResult<()> {
    let database = catalog.database;
    let source_id = catalog.source_id;
    let stamp = catalog.observed;
    if starts.is_empty() {
        return Ok(());
    }
    let roots = local
        .roots()
        .iter()
        .map(PathBuf::as_path)
        .collect::<BTreeSet<_>>();
    let mut completed = 0;
    for path in starts.iter().filter(|path| !local.excludes(path)) {
        check_cancelled(cancelled)?;
        let metadata = fs::metadata(path).map_err(|error| SourceError::Other(error.to_string()))?;
        if metadata.is_dir() {
            catalog
                .write_file_observations(
                    &[file_observation(
                        &roots,
                        path,
                        library::LocalFileKind::Directory,
                        library::LocalFileState::Observed,
                        &[],
                    )?],
                    Vec::new(),
                    admit,
                )
                .await?;
        } else if is_cue(path) {
            if !reuse_unchanged
                || unchanged_cue(catalog, &roots, path, admit, cancelled)
                    .await?
                    .is_none()
            {
                completed +=
                    acquire_exact_cue(catalog, &roots, path, true, admit, cancelled).await?;
            }
        } else {
            let kind = if crate::file::artwork::supported_image(path) {
                library::LocalFileKind::Image
            } else {
                library::LocalFileKind::Media
            };
            catalog
                .write_file_observations(
                    &[file_observation(
                        &roots,
                        path,
                        kind,
                        library::LocalFileState::Observed,
                        &[],
                    )?],
                    Vec::new(),
                    admit,
                )
                .await?;
        }
    }
    let mut listed = 0;
    for root in local.roots() {
        let mut after = None;
        while let Some(directory) = database
            .source_next_directory(source_id, stamp, after.as_deref(), &root.to_string_lossy())
            .await?
        {
            after = Some(directory.clone());
            let directory = PathBuf::from(directory);
            check_cancelled(cancelled)?;
            let mut observations = Vec::with_capacity(LOCAL_BATCH_SIZE);
            for entry in
                fs::read_dir(&directory).map_err(|error| SourceError::Other(error.to_string()))?
            {
                check_cancelled(cancelled)?;
                let entry = entry.map_err(|error| SourceError::Other(error.to_string()))?;
                let path = entry.path();
                if local.excludes(&path) {
                    continue;
                }
                let kind = entry
                    .file_type()
                    .map_err(|error| SourceError::Other(error.to_string()))?;
                listed += 1;
                if kind.is_file() && is_cue(&path) {
                    if !reuse_unchanged
                        || unchanged_cue(catalog, &roots, &path, admit, cancelled)
                            .await?
                            .is_none()
                    {
                        completed +=
                            acquire_exact_cue(catalog, &roots, &path, true, admit, cancelled)
                                .await?;
                    }
                    progress(SourceReadProgress {
                        stage: SourceReadStage::Tracks,
                        completed,
                        total: None,
                    });
                    continue;
                }
                let kind = if kind.is_dir() {
                    library::LocalFileKind::Directory
                } else if kind.is_file() && crate::file::artwork::supported_image(&path) {
                    library::LocalFileKind::Image
                } else if kind.is_file() {
                    library::LocalFileKind::Media
                } else {
                    continue;
                };
                observations.push(file_observation(
                    &roots,
                    &path,
                    kind,
                    library::LocalFileState::Observed,
                    &[],
                )?);
                if observations.len() == LOCAL_BATCH_SIZE {
                    check_cancelled(cancelled)?;
                    catalog
                        .write_file_observations(&observations, Vec::new(), admit)
                        .await?;
                    observations.clear();
                    progress(SourceReadProgress {
                        stage: SourceReadStage::Files,
                        completed: listed,
                        total: None,
                    });
                }
            }
            observations.push(file_observation(
                &roots,
                &directory,
                library::LocalFileKind::Directory,
                library::LocalFileState::Accepted,
                &[],
            )?);
            check_cancelled(cancelled)?;
            catalog
                .write_file_observations(&observations, Vec::new(), admit)
                .await?;
            progress(SourceReadProgress {
                stage: SourceReadStage::Files,
                completed: listed,
                total: None,
            });
        }
    }
    for root in local.roots() {
        let mut after = None;
        loop {
            let page = database
                .source_file_page(
                    source_id,
                    library::LocalFileKind::Media,
                    stamp,
                    after.as_deref(),
                    None,
                    Some(&root.to_string_lossy()),
                )
                .await?;
            if page.is_empty() {
                break;
            }
            after = page.last().map(|file| file.path.clone());
            let dependencies = database
                .source_dependency_paths(
                    source_id,
                    &page
                        .iter()
                        .map(|file| file.path.clone())
                        .collect::<Vec<_>>(),
                    stamp,
                    admit,
                )
                .await?;
            let paths = page
                .into_iter()
                .filter(|file| !dependencies.contains(&file.path))
                .map(|file| PathBuf::from(file.path))
                .collect::<Vec<_>>();
            completed += acquire_audio_batch(
                catalog,
                &roots,
                &paths,
                cancelled,
                reuse_unchanged,
                None,
                parsers,
                admit,
            )
            .await?;
            progress(SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed,
                total: None,
            });
        }
    }
    Ok(())
}

async fn unchanged_cue(
    catalog: &mut library::Catalog<'_>,
    roots: &BTreeSet<&Path>,
    path: &Path,
    admit: bool,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<Option<Vec<String>>> {
    let database = catalog.database;
    let source_id = catalog.source_id;
    let observation = file_observation(
        roots,
        path,
        library::LocalFileKind::Cue,
        library::LocalFileState::Observed,
        &[],
    )?
    .0;
    let Some(stored) = cached_cue(database, source_id, &observation).await? else {
        return Ok(None);
    };
    if !stored.current_output
        || stored
            .dependencies
            .iter()
            .any(|path| normalize_observed_path(Path::new(path)) != Path::new(path))
        || stored.size_bytes != observation.size_bytes
        || stored.mtime_ns != observation.mtime_ns
        || !reusable_parser_version(stored.parse_version)
        || stored.state != library::LocalFileState::Accepted
    {
        return Ok(None);
    }
    let backing = stored
        .dependencies
        .iter()
        .map(PathBuf::from)
        .filter(|path| roots.iter().any(|root| path.starts_with(root)))
        .map(|path| {
            file_observation(
                roots,
                &path,
                library::LocalFileKind::Media,
                library::LocalFileState::Observed,
                &[],
            )
        })
        .collect::<SourceResult<Vec<_>>>()?;
    check_cancelled(cancelled)?;
    catalog
        .write_file_observations(&backing, Vec::new(), admit)
        .await?;
    if !database
        .source_cue_dependencies_unchanged(source_id, &observation.path)
        .await?
    {
        return Ok(None);
    }
    let mut observation = observation;
    observation.state = library::LocalFileState::Accepted;
    check_cancelled(cancelled)?;
    catalog
        .write_file_observations(
            &[(observation, stored.dependencies.clone())],
            Vec::new(),
            admit,
        )
        .await?;
    Ok(Some(stored.dependencies))
}

async fn retained_cue_dependencies(
    database: &library::Database,
    source_id: &str,
    roots: &BTreeSet<&Path>,
    path: &Path,
) -> SourceResult<Vec<String>> {
    let observation = file_observation(
        roots,
        path,
        library::LocalFileKind::Cue,
        library::LocalFileState::Observed,
        &[],
    )?
    .0;
    Ok(cached_cue(database, source_id, &observation)
        .await?
        .map(|file| file.dependencies)
        .unwrap_or_default())
}

async fn cached_cue(
    database: &library::Database,
    source_id: &str,
    observation: &library::LocalFileWrite,
) -> SourceResult<Option<library::LocalFileRow>> {
    Ok(database
        .source_file_reuse_candidates(source_id, std::slice::from_ref(observation))
        .await?
        .into_iter()
        .find(|stored| stored.path == observation.path))
}

async fn acquire_audio_batch(
    catalog: &mut library::Catalog<'_>,
    roots: &BTreeSet<&Path>,
    paths: &[PathBuf],
    cancelled: &(dyn Fn() -> bool + Send + Sync),
    reuse_unchanged: bool,
    explicit_rename: Option<&(PathBuf, PathBuf)>,
    parsers: &[Arc<Mutex<media::Worker>>],
    admit: bool,
) -> SourceResult<usize> {
    if paths.is_empty() {
        return Ok(0);
    }
    let database = catalog.database;
    let source_id = catalog.source_id;
    let reuse = if reuse_unchanged {
        path_reuse(database, source_id, roots, paths, explicit_rename).await?
    } else {
        PathReuse::default()
    };
    let unchanged = reuse.unchanged;
    let renamed_ids = reuse.renamed;
    let mut accepted: Vec<ScannedTrack> = Vec::with_capacity(paths.len());
    let mut aliases = Vec::new();
    let mut observations = Vec::with_capacity(paths.len());
    let jobs = paths
        .iter()
        .filter(|path| !unchanged.contains_key(path.to_string_lossy().as_ref()))
        .cloned()
        .enumerate()
        .map(|(index, path)| (path, Arc::clone(&parsers[index % parsers.len()])))
        .collect::<Vec<_>>();
    let mut reads = stream::iter(jobs.into_iter().map(|(path, worker)| async move {
        check_cancelled(cancelled)?;
        let read = tokio::task::spawn_blocking(move || {
            let mut worker = worker.lock().unwrap_or_else(|error| error.into_inner());
            let read = read_media(&mut worker, path.clone(), None);
            (path, read)
        })
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?;
        check_cancelled(cancelled)?;
        Ok::<_, SourceError>(read)
    }))
    .buffered(parsers.len())
    .try_collect::<BTreeMap<_, _>>()
    .await?;
    for path in paths {
        if let Some(state) = unchanged.get(path.to_string_lossy().as_ref()) {
            observations.push(file_observation(
                roots,
                path,
                library::LocalFileKind::Media,
                *state,
                &[],
            )?);
            continue;
        }
        match reads.remove(path).unwrap_or(MediaRead::Unreadable) {
            MediaRead::Accepted(mut track) => {
                let observation = file_observation(
                    roots,
                    path,
                    library::LocalFileKind::Media,
                    library::LocalFileState::Accepted,
                    &[],
                )?;
                observations.push(observation);
                if let Some(uri) = track.track.media_uri.as_deref()
                    && let Some((object_id, _)) = database.source_track_file(uri).await?
                {
                    track.track.object_id = object_id;
                }
                let previous = if let Some((object_id, previous)) =
                    renamed_ids.get(path.to_string_lossy().as_ref())
                {
                    track.track.object_id = object_id.clone();
                    Path::new(previous)
                } else {
                    path.as_path()
                };
                if let Some(canonical) = track.track.media_uri.as_ref()
                    && let Ok(previous) = url::Url::from_file_path(previous)
                    && previous.as_str() != canonical
                {
                    if let Some(existing) = accepted
                        .iter()
                        .find(|existing| existing.track.media_uri.as_ref() == Some(canonical))
                    {
                        track.track.object_id = existing.track.object_id.clone();
                    } else if let Some((object_id, _)) =
                        database.source_track_file(canonical).await?
                    {
                        track.track.object_id = object_id;
                    }
                    aliases.push((
                        previous.into(),
                        canonical.clone(),
                        track.track.object_id.clone(),
                    ));
                }
                accepted.push(*track)
            }
            MediaRead::Rejected => {
                let observation = file_observation(
                    roots,
                    path,
                    library::LocalFileKind::Media,
                    library::LocalFileState::Rejected,
                    &[],
                )?;
                observations.push(observation);
            }
            MediaRead::Unreadable => {
                let observation = file_observation(
                    roots,
                    path,
                    library::LocalFileKind::Media,
                    library::LocalFileState::Unreadable,
                    &[],
                )?;
                observations.push(observation);
            }
        }
    }
    let accepted_count = accepted.len();
    let facts = accepted
        .into_iter()
        .map(|track| track.into_observation(media::accepted_at()))
        .collect::<Vec<_>>();
    if !aliases.is_empty() {
        check_cancelled(cancelled)?;
        catalog.merge_local_file_uris(&aliases).await?;
    }
    check_cancelled(cancelled)?;
    catalog
        .write_file_observations(&observations, facts, admit)
        .await?;

    Ok(accepted_count)
}

fn local_workers() -> Vec<Arc<Mutex<media::Worker>>> {
    let count = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .clamp(1, 4);
    (0..count)
        .map(|_| Arc::new(Mutex::new(media::Worker::default())))
        .collect()
}

#[derive(Default)]
struct PathReuse {
    unchanged: BTreeMap<String, library::LocalFileState>,
    renamed: BTreeMap<String, (String, String)>,
}

async fn path_reuse(
    database: &library::Database,
    source_id: &str,
    roots: &BTreeSet<&Path>,
    paths: &[PathBuf],
    explicit_rename: Option<&(PathBuf, PathBuf)>,
) -> SourceResult<PathReuse> {
    let mut facts = Vec::new();
    for path in paths {
        let observation = file_observation(
            roots,
            path,
            library::LocalFileKind::Media,
            library::LocalFileState::Observed,
            &[],
        )?
        .0;
        facts.push(observation);
    }
    let original_count = facts.len();
    let explicit = explicit_rename
        .filter(|(_, new_path)| paths.iter().any(|path| path == new_path))
        .map(|(old_path, new_path)| {
            (
                old_path.to_string_lossy().into_owned(),
                new_path.to_string_lossy().into_owned(),
            )
        });
    if let Some((old_path, _)) = explicit.as_ref()
        && let Some(mut old) = facts.first().cloned()
    {
        old.path = old_path.clone();
        old.device_id = None;
        old.inode = None;
        facts.push(old);
    }
    let current = database
        .source_file_reuse_candidates(source_id, &facts)
        .await?;
    let mut reuse = PathReuse::default();
    for observation in facts.into_iter().take(original_count) {
        if current.iter().any(|stored| {
            stored.path == observation.path
                && stored.size_bytes == observation.size_bytes
                && stored.mtime_ns == observation.mtime_ns
                && reusable_parser_version(stored.parse_version)
                && (stored.state != library::LocalFileState::Rejected
                    || stored.parse_version == Some(i64::from(LOCAL_PARSER_VERSION)))
                && (stored.state == library::LocalFileState::Rejected || stored.current_output)
                && matches!(
                    stored.state,
                    library::LocalFileState::Accepted | library::LocalFileState::Rejected
                )
        }) {
            if let Some(stored) = current
                .iter()
                .find(|stored| stored.path == observation.path)
            {
                reuse.unchanged.insert(observation.path, stored.state);
            }
        } else if let Some(stored) = current.iter().find(|stored| {
            stored.track_object_id.is_some()
                && (stored.path == observation.path
                    || (observation.device_id.is_some()
                        && observation.inode.is_some()
                        && stored.device_id == observation.device_id
                        && stored.inode == observation.inode
                        && Path::new(&stored.path)
                            .try_exists()
                            .is_ok_and(|exists| !exists)))
        }) && let Some(object_id) = stored.track_object_id.as_ref()
        {
            reuse
                .renamed
                .insert(observation.path, (object_id.clone(), stored.path.clone()));
        }
    }
    if let Some((old_path, new_path)) = explicit {
        let identity = if let Some(id) = current
            .iter()
            .find(|stored| stored.path == old_path)
            .and_then(|stored| stored.track_object_id.clone())
        {
            Some(id)
        } else {
            database
                .source_file_track_identity(source_id, &old_path)
                .await?
        };
        if let Some(id) = identity {
            reuse.renamed.insert(new_path, (id, old_path));
        }
    }
    Ok(reuse)
}

fn resolve_cue_file_path(cue_path: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        cue_path
            .parent()
            .map(|parent| parent.join(&path))
            .unwrap_or(path)
    }
}

fn read_cue_tracks(
    worker: &mut media::Worker,
    cue_path: &Path,
    sheet: CueSheet,
) -> Option<Vec<ScannedTrack>> {
    let mut tracks = Vec::new();
    let album_title = sheet.album_title;
    let album_performer = sheet.album_performer;
    for file in sheet.files {
        let MediaRead::Accepted(mut backing) =
            read_media(worker, resolve_cue_file_path(cue_path, &file.path), None)
        else {
            return None;
        };
        // Existing CUE URIs include the backing path as written in the sheet.
        backing.track.media_uri = Some(
            url::Url::from_file_path(backing.track.source_path.as_ref()?)
                .ok()?
                .into(),
        );
        backing.track.source_path = backing.track.source_path.as_deref().map(|path| {
            normalize_observed_path(Path::new(path))
                .to_string_lossy()
                .into_owned()
        });
        let duration_millis = backing.track.metadata.duration_millis as u64;
        for (position, cue) in file.tracks.iter().enumerate() {
            let end = file
                .tracks
                .get(position + 1)
                .map(|next| next.index_start_ms)
                .unwrap_or(duration_millis);
            if cue.index_start_ms >= end || end > duration_millis {
                return None;
            }
            let track = cue_track(
                cue_path,
                album_title.as_deref(),
                album_performer.as_deref(),
                cue,
                end,
                &backing,
            );
            tracks.push(track);
        }
    }
    Some(tracks)
}

fn file_observation(
    roots: &BTreeSet<&Path>,
    path: &Path,
    kind: library::LocalFileKind,
    state: library::LocalFileState,
    dependencies: &[String],
) -> SourceResult<(library::LocalFileWrite, Vec<String>)> {
    let metadata = fs::metadata(path).ok();
    let root = path
        .ancestors()
        .find(|ancestor| roots.contains(ancestor))
        .ok_or(SourceError::NotFound)?;
    let relative_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    #[cfg(unix)]
    let (device_id, inode) = metadata
        .as_ref()
        .map(|metadata| {
            use std::os::unix::fs::MetadataExt;
            (
                i64::try_from(metadata.dev()).ok(),
                i64::try_from(metadata.ino()).ok(),
            )
        })
        .unwrap_or((None, None));
    #[cfg(not(unix))]
    let (device_id, inode) = (None, None);
    let mtime_ns = metadata
        .as_ref()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or_default();
    let write = library::LocalFileWrite {
        path: path.to_string_lossy().into_owned(),
        root: root.to_string_lossy().into_owned(),
        relative_path,
        kind,
        size_bytes: metadata
            .as_ref()
            .and_then(|metadata| i64::try_from(metadata.len()).ok()),
        mtime_ns,
        device_id,
        inode,
        native_id: None,
        picture_index: None,
        artist_pictures: None,
        revision: None,
        parse_version: Some(i64::from(LOCAL_PARSER_VERSION)),
        state,
    };
    Ok((write, dependencies.to_vec()))
}

enum CueRead {
    Accepted(CueSheet),
    Rejected,
    Unreadable,
}

fn read_cue(path: &Path) -> CueRead {
    let Ok(metadata) = fs::metadata(path) else {
        return CueRead::Unreadable;
    };
    if metadata.len() > LOCAL_CUE_MAX_BYTES {
        return CueRead::Rejected;
    }
    let Ok(text) = fs::read_to_string(path) else {
        return CueRead::Unreadable;
    };
    parse_cue_sheet(&text)
        .map(CueRead::Accepted)
        .unwrap_or(CueRead::Rejected)
}

fn is_cue(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cue"))
}

pub(crate) async fn acquire_imported_paths(
    catalog: &mut library::Catalog<'_>,
    local: &LocalSource,
    playlist: Option<library::PlaylistKey>,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()> {
    let database = catalog.database;
    let source_id = catalog.source_id;
    let stamp = catalog.observed;
    let mut after = String::new();
    let parsers = local_workers();
    loop {
        let page = database.imported_local_uri_page(playlist, &after).await?;
        if page.is_empty() {
            break;
        }
        after = page.last().unwrap().0.clone();
        let mut staged = database
            .source_observed_media_uris(
                source_id,
                stamp.unwrap_or(0),
                &page.iter().map(|(uri, _)| uri.as_str()).collect::<Vec<_>>(),
            )
            .await?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut uris = Vec::with_capacity(page.len());
        let mut aliases = Vec::new();
        let mut documents = Vec::new();
        for (mut uri, cue) in page.into_iter().filter(|(uri, _)| !staged.contains(uri)) {
            if library::document_media_id(&uri).is_some() || uri.starts_with("content:") {
                let access = if uri.starts_with("content:") {
                    Some(uri.clone())
                } else {
                    database
                        .playback_access(&uri)
                        .await?
                        .map(|(access, _)| access)
                };
                if let Some(access) = access.filter(|access| access.starts_with("content:")) {
                    documents.push((uri, access));
                }
                continue;
            }
            if cue
                .as_ref()
                .is_some_and(|path| path.starts_with("rufin-document:"))
            {
                continue;
            }
            let access = if let Some(path) = library::file_media_path(&uri) {
                if path.is_file() {
                    Some(path)
                } else if let Some((_, access)) = super::local_file_location(local.roots(), &path)
                    .filter(|(_, access)| access.is_file())
                {
                    Some(access)
                } else {
                    database
                        .playback_access(&uri)
                        .await?
                        .and_then(|(uri, _)| library::file_media_path(&uri))
                        .or(Some(path))
                }
            } else {
                None
            };
            if let Some(path) = access.as_deref() {
                if fs::metadata(path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                {
                    check_cancelled(cancelled)?;
                    catalog
                        .remove_source_files(
                            &[path.to_string_lossy().into_owned()],
                            stamp.expect("file observation"),
                        )
                        .await?;
                    continue;
                }
                let identity = super::media::local_media_identity_path(path);
                if let Ok(canonical) = url::Url::from_file_path(&identity)
                    && canonical.as_str() != uri
                {
                    let canonical = canonical.to_string();
                    let object_id = database
                        .source_track_file(&canonical)
                        .await?
                        .map_or_else(|| media::track_id(&identity), |(id, _)| id);
                    aliases.push((
                        std::mem::replace(&mut uri, canonical.clone()),
                        canonical,
                        object_id,
                    ));
                }
            }
            uris.push((uri, cue, access));
        }
        acquire_imported_documents(catalog, documents, cancelled).await?;
        if !aliases.is_empty() {
            check_cancelled(cancelled)?;
            catalog.merge_local_file_uris(&aliases).await?;
            uris.sort_unstable_by(|a, b| {
                a.0.cmp(&b.0)
                    .then_with(|| a.2.is_none().cmp(&b.2.is_none()))
            });
            uris.dedup_by(|a, b| a.0 == b.0);
            staged.extend(
                database
                    .source_observed_media_uris(
                        source_id,
                        stamp.unwrap_or(0),
                        &uris
                            .iter()
                            .map(|(uri, _, _)| uri.as_str())
                            .collect::<Vec<_>>(),
                    )
                    .await?,
            );
        }
        let cues = uris
            .iter()
            .filter(|(uri, _, _)| !staged.contains(uri))
            .filter_map(|(_, cue, _)| cue.as_ref().map(PathBuf::from))
            .collect::<BTreeSet<_>>();
        let paths = uris
            .into_iter()
            .filter(|(uri, _, _)| !staged.contains(uri))
            .filter_map(|(_, cue, access)| cue.is_none().then_some(access).flatten())
            .collect::<Vec<_>>();
        let roots = cues
            .iter()
            .chain(&paths)
            .filter_map(|path| path.ancestors().last())
            .chain(local.roots().iter().map(PathBuf::as_path))
            .collect::<BTreeSet<_>>();
        for path in &cues {
            if unchanged_cue(catalog, &roots, path, true, cancelled)
                .await?
                .is_none()
            {
                acquire_exact_cue(catalog, &roots, path, true, true, cancelled).await?;
            }
        }
        acquire_audio_batch(
            catalog, &roots, &paths, cancelled, true, None, &parsers, true,
        )
        .await?;
    }
    Ok(())
}

async fn acquire_imported_documents(
    catalog: &mut library::Catalog<'_>,
    documents: Vec<(String, String)>,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()> {
    let mut files = Vec::with_capacity(documents.len());
    for (uri, access) in documents {
        check_cancelled(cancelled)?;
        let path = access.clone();
        let entry = tokio::task::spawn_blocking(move || crate::stat_document(&path))
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?;
        check_cancelled(cancelled)?;
        let entry = match entry {
            Ok(entry) => entry,
            Err(SourceError::NotFound) => {
                let path = catalog
                    .database
                    .source_track_file(&uri)
                    .await?
                    .map_or(access, |(_, path)| path);
                catalog
                    .remove_source_files(&[path], catalog.observed.expect("file observation"))
                    .await?;
                continue;
            }
            Err(_) => continue,
        };
        files.push((
            uri,
            library::LocalFileWrite {
                path: entry.uri.clone(),
                root: entry.uri,
                relative_path: entry.name,
                kind: library::LocalFileKind::Media,
                size_bytes: entry.size.and_then(|size| i64::try_from(size).ok()),
                mtime_ns: 0,
                device_id: None,
                inode: None,
                native_id: Some(entry.native_id),
                revision: entry.revision,
                picture_index: None,
                artist_pictures: None,
                parse_version: Some(super::super::remote::scan::PARSER_VERSION),
                state: library::LocalFileState::Observed,
            },
        ));
    }
    if files.is_empty() {
        return Ok(());
    }
    let candidates = catalog
        .database
        .source_file_reuse_candidates(
            catalog.source_id,
            &files
                .iter()
                .map(|(_, file)| file.clone())
                .collect::<Vec<_>>(),
        )
        .await?;
    let mut observations = Vec::with_capacity(files.len());
    let mut tracks = Vec::new();
    for (uri, mut file) in files {
        check_cancelled(cancelled)?;
        let access = file.path.clone();
        let previous = candidates
            .iter()
            .filter(|previous| {
                previous.path == file.path
                    || (file.native_id.is_some() && previous.native_id == file.native_id)
            })
            .max_by_key(|previous| (previous.current_output, previous.path == file.path));
        if let Some(previous) = previous {
            file.path.clone_from(&previous.path);
            file.root.clone_from(&previous.root);
            file.relative_path.clone_from(&previous.relative_path);
        }
        if let Some(previous) = previous.filter(|previous| {
            file.revision.is_some()
                && previous.revision == file.revision
                && super::super::remote::scan::reusable_parser_version(previous.parse_version)
                && (previous.state == library::LocalFileState::Rejected
                    && previous.parse_version == file.parse_version
                    || previous.state == library::LocalFileState::Accepted
                        && previous.current_output)
        }) {
            file.state = previous.state;
            file.parse_version = previous.parse_version;
        } else {
            let read =
                crate::file::documents::read_document_media(&access, catalog.source_id).await;
            check_cancelled(cancelled)?;
            match read {
                Ok((_, MediaRead::Accepted(mut track))) => {
                    track.track.object_id =
                        catalog.database.source_track_file(&uri).await?.map_or_else(
                            || {
                                previous
                                    .and_then(|previous| previous.track_object_id.clone())
                                    .unwrap_or_else(|| {
                                        media::track_id(Path::new(
                                            file.native_id.as_deref().unwrap_or(&file.path),
                                        ))
                                    })
                            },
                            |(id, _)| id,
                        );
                    track.track.media_uri = Some(uri);
                    track.track.source_path = Some(file.path.clone());
                    track.audio_revision = super::super::remote::scan::audio_revision(
                        &file.path,
                        file.revision.as_deref(),
                        file.size_bytes,
                    );
                    file.picture_index = Some(match &track.local_artwork {
                        Some(crate::LocalImageRef::Embedded { picture_index, .. }) => {
                            i64::from(*picture_index)
                        }
                        _ => -1,
                    });
                    file.artist_pictures = Some(serde_json::to_string(&track.artist_pictures)?);
                    file.state = library::LocalFileState::Accepted;
                    tracks.push(track.into_observation(media::accepted_at()));
                }
                Ok((_, MediaRead::Rejected)) => file.state = library::LocalFileState::Rejected,
                Ok((_, MediaRead::Unreadable)) | Err(_) => {
                    file.state = library::LocalFileState::Unreadable
                }
            }
        }
        observations.push((file, Vec::new()));
    }
    catalog
        .write_file_observations(&observations, tracks, true)
        .await?;
    Ok(())
}
