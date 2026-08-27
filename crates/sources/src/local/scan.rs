//! Streams Local files into Scan while retaining only one CUE component or one 128-path batch.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::UNIX_EPOCH;

use library::Scan;
use walkdir::WalkDir;

use super::cue::{CueSheet, CueTrack, parse_cue_sheet};
use super::media::{self, MediaRead, ScannedTrack};
use crate::source::{SourceReadProgress, SourceReadStage};
use crate::{SourceError, SourceResult};

const LOCAL_BATCH_SIZE: usize = 128;
const LOCAL_CUE_MAX_BYTES: u64 = 1024 * 1024;
const LOCAL_PARSER_VERSION: u32 = 9;

pub(super) async fn publish_metadata_paths(
    database: &library::Database,
    source_id: &str,
    paths: &[PathBuf],
    removed_album: Option<&str>,
    removed_artist: Option<&str>,
) -> SourceResult<library::ScanOutcome> {
    let mut scan = Scan::begin_items(database, source_id).await?;
    scan.begin_batch().await?;
    if let Some(album) = removed_album {
        scan.remove_album(album).await?;
    }
    if let Some(artist) = removed_artist {
        scan.remove_artist(artist).await?;
    }
    scan.finish_batch().await?;
    let mut worker = media::Worker::default();
    for page in paths.chunks(LOCAL_BATCH_SIZE) {
        scan.begin_batch().await?;
        let mut tracks = Vec::with_capacity(page.len());
        for path in page {
            match media::read_media(&mut worker, path.clone(), None) {
                MediaRead::Accepted(track) => tracks.push(*track),
                MediaRead::Rejected | MediaRead::Unreadable => {
                    return Err(SourceError::Other(format!(
                        "Could not reread metadata from {}",
                        path.display()
                    )));
                }
            }
        }
        stage_audio_tracks_batch(&mut scan, &tracks).await?;
        scan.finish_batch().await?;
    }
    Ok(scan.finish().await?)
}

pub(super) async fn catch_up(
    database: &library::Database,
    source: library::SourceKey,
    source_id: &str,
    roots: &[PathBuf],
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<library::ScanOutcome> {
    let mut scan = Scan::begin_items(database, source_id).await?;
    let mut after = None;
    let mut completed = 0_usize;
    let mut changed = false;
    loop {
        check_cancelled(cancelled)?;
        let page = database
            .local_file_page(
                source,
                after,
                LOCAL_BATCH_SIZE,
                &library::ReadCancellation::new(),
            )
            .await?;
        if page.is_empty() {
            break;
        }
        after = page.last().map(|file| file.local_file_key);
        let changed_paths = page
            .iter()
            .filter(|file| {
                matches!(
                    file.kind,
                    library::LocalFileKind::Media
                        | library::LocalFileKind::Cue
                        | library::LocalFileKind::Directory
                )
            })
            .filter(|file| cached_observation_changed(file))
            .map(|file| PathBuf::from(&file.path))
            .collect::<Vec<_>>();
        if !changed_paths.is_empty() {
            let seeds = changed_paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            stage_component_paths(&mut scan, source, &changed_paths, &seeds, cancelled).await?;
            changed = true;
        }
        completed = completed.saturating_add(page.len());
        progress(SourceReadProgress {
            stage: SourceReadStage::Files,
            completed,
            total: None,
        });
    }
    if changed {
        stage_component(database, source, roots, &mut scan, false, None, cancelled).await?;
    }
    Ok(scan.finish().await?)
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
    file.size_bytes != size
        || file.mtime_ns != mtime_ns
        || matches!(
            file.kind,
            library::LocalFileKind::Media | library::LocalFileKind::Cue
        ) && file.parse_version != Some(i64::from(LOCAL_PARSER_VERSION))
}

pub(super) async fn publish_paths(
    database: &library::Database,
    source: library::SourceKey,
    source_id: &str,
    roots: &[PathBuf],
    paths: &[PathBuf],
    rename: Option<&(PathBuf, PathBuf)>,
) -> SourceResult<library::ScanOutcome> {
    let paths = paths
        .iter()
        .map(|path| normalize_observed_path(path))
        .collect::<Vec<_>>();
    let rename =
        rename.map(|(old, new)| (normalize_observed_path(old), normalize_observed_path(new)));
    let seeds = paths
        .iter()
        .filter(|path| roots.iter().any(|root| path.starts_with(root)))
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if seeds.is_empty() {
        return Err(SourceError::Other(
            "Local watcher paths are outside the configured roots".to_string(),
        ));
    }
    let artwork_only = paths
        .iter()
        .all(|path| super::artwork::supported_image(path));
    let mut scan = Scan::begin_items(database, source_id).await?;
    stage_component_paths(&mut scan, source, &paths, &seeds, &|| false).await?;
    stage_component(
        database,
        source,
        roots,
        &mut scan,
        artwork_only,
        rename.as_ref(),
        &|| false,
    )
    .await?;
    Ok(scan.finish().await?)
}

fn normalize_observed_path(path: &Path) -> PathBuf {
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

async fn stage_component_paths(
    scan: &mut Scan,
    source: library::SourceKey,
    paths: &[PathBuf],
    seeds: &[String],
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()> {
    for page in seeds.chunks(LOCAL_BATCH_SIZE) {
        check_cancelled(cancelled)?;
        stage_component_path_page(scan, source, page).await?;
    }

    let mut walked = Vec::with_capacity(LOCAL_BATCH_SIZE);
    for directory in paths.iter().filter(|path| path.is_dir()) {
        for entry in WalkDir::new(directory)
            .follow_links(false)
            .sort_by_file_name()
        {
            check_cancelled(cancelled)?;
            let entry = entry.map_err(|error| {
                SourceError::Other(format!("Local component walk failed: {error}"))
            })?;
            walked.push(entry.path().to_string_lossy().into_owned());
            if walked.len() == LOCAL_BATCH_SIZE {
                stage_component_path_page(scan, source, &walked).await?;
                walked.clear();
            }
        }
    }
    if !walked.is_empty() {
        stage_component_path_page(scan, source, &walked).await?;
    }
    Ok(())
}

async fn stage_component_path_page(
    scan: &mut Scan,
    source: library::SourceKey,
    paths: &[String],
) -> SourceResult<()> {
    let mut directories = BTreeSet::new();
    let mut image_directories = BTreeSet::new();
    for path in paths.iter().map(PathBuf::from) {
        let target = if path.is_dir() {
            &mut directories
        } else if super::artwork::supported_image(&path) {
            &mut image_directories
        } else {
            continue;
        };
        if let Some(directory) = path
            .is_dir()
            .then_some(path.as_path())
            .or_else(|| path.parent())
        {
            let mut prefix = directory
                .to_string_lossy()
                .trim_end_matches(['/', '\\'])
                .to_string();
            prefix.push(std::path::MAIN_SEPARATOR);
            target.insert(prefix);
        }
    }
    scan.begin_batch().await?;
    scan.write_local_component_paths(paths).await?;
    scan.expand_local_artwork_prefixes(
        source,
        &directories.into_iter().collect::<Vec<_>>(),
        &image_directories.into_iter().collect::<Vec<_>>(),
    )
    .await?;
    scan.finish_batch().await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn stage_component(
    database: &library::Database,
    source: library::SourceKey,
    roots: &[PathBuf],
    scan: &mut Scan,
    artwork_only: bool,
    rename: Option<&(PathBuf, PathBuf)>,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()> {
    scan.begin_batch().await?;
    scan.expand_local_component(source).await?;
    scan.finish_batch().await?;

    scan.begin_batch().await?;
    if !artwork_only {
        scan.remove_local_component_tracks(source).await?;
    }
    scan.finish_batch().await?;

    let mut parsers = MediaPool::new();
    let mut after = None;
    loop {
        let page = scan
            .local_component_path_page(after.as_deref(), LOCAL_BATCH_SIZE)
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
            if !path.exists() {
                missing.push(path.to_string_lossy().into_owned());
            } else if path.is_dir() {
                observations.push(file_observation(
                    roots,
                    &path,
                    library::LocalFileKind::Directory,
                    library::LocalFileState::Observed,
                    &[],
                )?);
            } else if super::artwork::supported_image(&path) {
                observations.push(file_observation(
                    roots,
                    &path,
                    library::LocalFileKind::Image,
                    library::LocalFileState::Observed,
                    &[],
                )?);
            } else if !artwork_only && is_cue(&path) {
                cues.push(path);
            } else if !artwork_only && path.is_file() {
                audio.push(path);
            }
        }
        if !missing.is_empty() {
            scan.begin_batch().await?;
            scan.remove_local_file_paths(&missing).await?;
            scan.finish_batch().await?;
        }
        for path in cues {
            stage_exact_cue(database, roots, scan, &path, true).await?;
        }
        if !audio.is_empty() {
            stage_audio_batch(
                database,
                roots,
                scan,
                &audio,
                cancelled,
                true,
                rename,
                &mut parsers,
            )
            .await?;
        }
        if !observations.is_empty() {
            scan.begin_batch().await?;
            persist_observations(scan, observations).await?;
            scan.finish_batch().await?;
        }
    }

    if !artwork_only {
        let mut after = None;
        loop {
            let page = scan
                .local_affected_album_path_page(source, after.as_deref(), LOCAL_BATCH_SIZE)
                .await?;
            if page.is_empty() {
                break;
            }
            after = page.last().cloned();
            let mut media = Vec::new();
            for path in page.into_iter().map(PathBuf::from) {
                if is_cue(&path) {
                    if path.is_file() {
                        stage_exact_cue(database, roots, scan, &path, true).await?;
                    }
                } else if path.is_file() {
                    media.push(path);
                }
            }
            if !media.is_empty() {
                stage_audio_batch(
                    database,
                    roots,
                    scan,
                    &media,
                    cancelled,
                    false,
                    rename,
                    &mut parsers,
                )
                .await?;
            }
        }
    }
    Ok(())
}

async fn stage_exact_cue(
    database: &library::Database,
    roots: &[PathBuf],
    scan: &mut Scan,
    path: &Path,
    retain_unreadable: bool,
) -> SourceResult<usize> {
    let (state, dependencies, tracks, retain) = match read_cue(path) {
        CueRead::Accepted(sheet) => {
            let dependencies = sheet
                .files
                .iter()
                .map(|file| file.path.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let tracks = read_cue_tracks(&mut media::Worker::default(), path, sheet);
            let state = if tracks.is_some() {
                library::LocalFileState::Accepted
            } else {
                library::LocalFileState::Unreadable
            };
            let retain = retain_unreadable && tracks.is_none();
            (state, dependencies, tracks.unwrap_or_default(), retain)
        }
        CueRead::Rejected => (
            library::LocalFileState::Rejected,
            Vec::new(),
            Vec::new(),
            false,
        ),
        CueRead::Unreadable => (
            library::LocalFileState::Unreadable,
            if retain_unreadable {
                retained_cue_dependencies(database, scan, roots, path).await?
            } else {
                Vec::new()
            },
            Vec::new(),
            retain_unreadable,
        ),
    };
    let accepted = tracks.len();
    scan.begin_batch().await?;
    for page in dependencies.chunks(LOCAL_BATCH_SIZE) {
        scan.write_local_dependency_paths(page).await?;
    }
    stage_audio_tracks_batch(scan, &tracks).await?;
    if retain {
        scan.retain_local_cue_path(path.to_string_lossy().as_ref())
            .await?;
    }
    persist_observations(
        scan,
        vec![file_observation(
            roots,
            path,
            library::LocalFileKind::Cue,
            state,
            &dependencies,
        )?],
    )
    .await?;
    scan.finish_batch().await?;
    Ok(accepted)
}

pub(super) async fn stage_catalog(
    database: &library::Database,
    roots: &[PathBuf],
    scan: &mut Scan,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
    reuse_unchanged: bool,
) -> SourceResult<()> {
    let mut observations = Vec::with_capacity(LOCAL_BATCH_SIZE);
    let mut completed = 0_usize;
    for root in roots {
        for entry in WalkDir::new(root).follow_links(false).sort_by_file_name() {
            check_cancelled(cancelled)?;
            let entry =
                entry.map_err(|error| SourceError::Other(format!("Local walk failed: {error}")))?;
            let path = entry.path();
            let (kind, state, dependencies) = if entry.file_type().is_dir() {
                (
                    library::LocalFileKind::Directory,
                    library::LocalFileState::Observed,
                    Vec::new(),
                )
            } else if is_cue(path) {
                (
                    library::LocalFileKind::Cue,
                    library::LocalFileState::Observed,
                    Vec::new(),
                )
            } else if super::artwork::supported_image(path) {
                (
                    library::LocalFileKind::Image,
                    library::LocalFileState::Observed,
                    Vec::new(),
                )
            } else if entry.file_type().is_file() {
                (
                    library::LocalFileKind::Media,
                    library::LocalFileState::Observed,
                    Vec::new(),
                )
            } else {
                continue;
            };
            observations.push(file_observation(roots, path, kind, state, &dependencies)?);
            if observations.len() == LOCAL_BATCH_SIZE {
                completed = completed.saturating_add(observations.len());
                scan.begin_batch().await?;
                persist_observations(scan, std::mem::take(&mut observations)).await?;
                scan.finish_batch().await?;
                progress(SourceReadProgress {
                    stage: SourceReadStage::Files,
                    completed,
                    total: None,
                });
            }
        }
    }
    if !observations.is_empty() {
        completed = completed.saturating_add(observations.len());
        scan.begin_batch().await?;
        persist_observations(scan, observations).await?;
        scan.finish_batch().await?;
        progress(SourceReadProgress {
            stage: SourceReadStage::Files,
            completed,
            total: None,
        });
    }

    let mut parsed = 0_usize;
    let mut after = None;
    let mut parsers = MediaPool::new();
    loop {
        let paths = scan
            .local_inventory_path_page(
                library::LocalFileKind::Cue,
                after.as_deref(),
                false,
                LOCAL_BATCH_SIZE,
            )
            .await?;
        if paths.is_empty() {
            break;
        }
        after = paths.last().cloned();
        for path_text in paths {
            check_cancelled(cancelled)?;
            let path = PathBuf::from(&path_text);
            if reuse_unchanged
                && let Some(dependencies) = unchanged_cue(database, scan, roots, &path).await?
            {
                scan.begin_batch().await?;
                for page in dependencies.chunks(LOCAL_BATCH_SIZE) {
                    scan.write_local_dependency_paths(page).await?;
                }
                scan.retain_local_cue_path(&path_text).await?;
                scan.finish_batch().await?;
                continue;
            }
            parsed += stage_exact_cue(database, roots, scan, &path, reuse_unchanged).await?;
            progress(SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed: parsed,
                total: None,
            });
        }
    }

    let mut after = None;
    loop {
        let paths = scan
            .local_inventory_path_page(
                library::LocalFileKind::Media,
                after.as_deref(),
                true,
                LOCAL_BATCH_SIZE,
            )
            .await?;
        if paths.is_empty() {
            break;
        }
        after = paths.last().cloned();
        let paths = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
        parsed += stage_audio_batch(
            database,
            roots,
            scan,
            &paths,
            cancelled,
            reuse_unchanged,
            None,
            &mut parsers,
        )
        .await?;
        progress(SourceReadProgress {
            stage: SourceReadStage::Tracks,
            completed: parsed,
            total: None,
        });
    }
    progress(SourceReadProgress {
        stage: SourceReadStage::Finalizing,
        completed: parsed,
        total: Some(parsed),
    });
    Ok(())
}
async fn unchanged_cue(
    database: &library::Database,
    scan: &Scan,
    roots: &[PathBuf],
    path: &Path,
) -> SourceResult<Option<Vec<String>>> {
    let observation = file_observation(
        roots,
        path,
        library::LocalFileKind::Cue,
        library::LocalFileState::Observed,
        &[],
    )?
    .0;
    Ok(cached_cue(database, scan, &observation)
        .await?
        .and_then(|stored| {
            (stored.path == observation.path
                && stored.size_bytes == observation.size_bytes
                && stored.mtime_ns == observation.mtime_ns
                && stored.parse_version == Some(i64::from(LOCAL_PARSER_VERSION))
                && stored.state == library::LocalFileState::Accepted)
                .then_some(stored.dependencies)
        }))
}

async fn retained_cue_dependencies(
    database: &library::Database,
    scan: &Scan,
    roots: &[PathBuf],
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
    Ok(cached_cue(database, scan, &observation)
        .await?
        .map(|file| file.dependencies)
        .unwrap_or_default())
}

async fn cached_cue(
    database: &library::Database,
    scan: &Scan,
    observation: &library::LocalFileWrite,
) -> SourceResult<Option<library::LocalFileRow>> {
    let Some(source) = scan.existing_source() else {
        return Ok(None);
    };
    Ok(database
        .local_file_reuse_candidates(
            source,
            std::slice::from_ref(&observation),
            &library::ReadCancellation::new(),
        )
        .await?
        .into_iter()
        .find(|stored| stored.path == observation.path))
}

async fn stage_audio_batch(
    database: &library::Database,
    roots: &[PathBuf],
    scan: &mut Scan,
    paths: &[PathBuf],
    cancelled: &(dyn Fn() -> bool + Send + Sync),
    reuse_unchanged: bool,
    explicit_rename: Option<&(PathBuf, PathBuf)>,
    parsers: &mut MediaPool,
) -> SourceResult<usize> {
    let path_text = paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let dependencies = scan
        .local_dependency_paths(&path_text)
        .await?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let reuse = if reuse_unchanged {
        path_reuse(database, scan, roots, paths, explicit_rename).await?
    } else {
        PathReuse::default()
    };
    let unchanged = reuse.unchanged;
    let renamed_ids = reuse.renamed;
    let accepted_current = reuse.accepted;
    let mut accepted = Vec::with_capacity(paths.len());
    let mut retained = Vec::new();
    let mut observations = Vec::with_capacity(paths.len());
    let jobs = paths
        .iter()
        .filter(|path| !dependencies.contains(path.to_string_lossy().as_ref()))
        .filter(|path| !unchanged.contains_key(path.to_string_lossy().as_ref()))
        .cloned();
    let reads = parsers.read(jobs, cancelled)?;
    let mut reads = reads
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    for path in paths {
        if dependencies.contains(path.to_string_lossy().as_ref()) {
            continue;
        }
        if let Some(state) = unchanged.get(path.to_string_lossy().as_ref()) {
            observations.push(file_observation(
                roots,
                path,
                library::LocalFileKind::Media,
                *state,
                &[],
            )?);
            if *state == library::LocalFileState::Accepted {
                retained.push(path.to_string_lossy().into_owned());
            }
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
                if let Some(object_id) = renamed_ids.get(path.to_string_lossy().as_ref()) {
                    track.id = object_id.clone();
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
                if reuse_unchanged && accepted_current.contains(path.to_string_lossy().as_ref()) {
                    retained.push(path.to_string_lossy().into_owned())
                }
            }
        }
    }
    persist_observations(scan, observations).await?;
    let accepted_count = accepted.len();
    scan.begin_batch().await?;
    stage_audio_tracks_batch(scan, &accepted).await?;
    scan.retain_local_media_paths(&retained).await?;
    scan.finish_batch().await?;
    Ok(accepted_count)
}

async fn stage_audio_tracks_batch(scan: &mut Scan, tracks: &[ScannedTrack]) -> SourceResult<()> {
    let mut albums = BTreeSet::new();
    let mut artists = BTreeSet::new();
    let mut genres = BTreeSet::new();
    let mut moods = BTreeSet::new();
    for track in tracks {
        if albums.insert(track.album_id.as_str()) {
            stage_album(scan, track).await?;
        }
        for artist in &track.album_artists {
            if artists.insert(artist.id.as_str()) {
                stage_artist(scan, artist).await?;
            }
        }
        stage_track_row(scan, track).await?;
        for artist in &track.artists {
            if artists.insert(artist.id.as_str()) {
                stage_artist(scan, artist).await?;
            }
        }
        for genre in &track.genres {
            if genres.insert(genre.id.as_str()) {
                stage_genre(scan, genre).await?;
            }
        }
        for mood in &track.moods {
            if moods.insert(mood.id.as_str()) {
                stage_mood(scan, mood).await?;
            }
        }
        stage_loudness(scan, track).await?;
    }
    let album_artists = tracks
        .iter()
        .flat_map(|track| {
            track
                .album_artists
                .iter()
                .map(|artist| (track.album_id.as_str(), artist.id.as_str()))
        })
        .collect::<Vec<_>>();
    let album_genres = tracks
        .iter()
        .flat_map(|track| {
            track
                .genres
                .iter()
                .map(|genre| (track.album_id.as_str(), genre.id.as_str()))
        })
        .collect::<Vec<_>>();
    let album_release_types = tracks
        .iter()
        .flat_map(|track| {
            track
                .release_types
                .iter()
                .map(|kind| (track.album_id.as_str(), kind.as_str()))
        })
        .collect::<Vec<_>>();
    let track_artists = tracks
        .iter()
        .flat_map(|track| {
            track
                .artists
                .iter()
                .map(|artist| (track.id.as_str(), artist.id.as_str()))
        })
        .collect::<Vec<_>>();
    let track_genres = tracks
        .iter()
        .flat_map(|track| {
            track
                .genres
                .iter()
                .map(|genre| (track.id.as_str(), genre.id.as_str()))
        })
        .collect::<Vec<_>>();
    let track_moods = tracks
        .iter()
        .flat_map(|track| {
            track
                .moods
                .iter()
                .map(|mood| (track.id.as_str(), mood.id.as_str()))
        })
        .collect::<Vec<_>>();
    scan.write_album_relations(&album_artists, &album_genres, &album_release_types)
        .await?;
    scan.write_track_relations(&track_artists, &track_genres, &track_moods)
        .await?;
    Ok(())
}

fn local_worker_count(available_parallelism: usize) -> usize {
    available_parallelism.clamp(1, 4)
}

struct MediaPool {
    jobs: Option<mpsc::SyncSender<PathBuf>>,
    results: mpsc::Receiver<(PathBuf, MediaRead)>,
    workers: Vec<JoinHandle<()>>,
    worker_count: usize,
}

impl MediaPool {
    fn new() -> Self {
        let worker_count = local_worker_count(
            std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
        );
        let (job_send, job_receive) = mpsc::sync_channel::<PathBuf>(worker_count * 2);
        let (result_send, result_receive) =
            mpsc::sync_channel::<(PathBuf, MediaRead)>(worker_count * 2);
        let job_receive = Arc::new(Mutex::new(job_receive));
        let mut workers = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let job_receive = Arc::clone(&job_receive);
            let result_send = result_send.clone();
            workers.push(
                std::thread::Builder::new()
                    .name("rufin-local-parser".to_string())
                    .spawn(move || {
                        let mut worker = media::Worker::default();
                        loop {
                            let job = job_receive
                                .lock()
                                .expect("Local parser receiver is not poisoned")
                                .recv();
                            let Ok(job) = job else { break };
                            let result = (job.clone(), media::read_media(&mut worker, job, None));
                            if result_send.send(result).is_err() {
                                break;
                            }
                        }
                    })
                    .expect("start bounded Local parser"),
            );
        }
        drop(result_send);
        Self {
            jobs: Some(job_send),
            results: result_receive,
            workers,
            worker_count,
        }
    }

    fn read(
        &mut self,
        jobs: impl IntoIterator<Item = PathBuf>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<Vec<(PathBuf, MediaRead)>> {
        let mut jobs = jobs.into_iter();
        let mut in_flight = 0;
        let sender = self.jobs.as_ref().expect("Local parser pool is open");
        for _ in 0..self.worker_count {
            let Some(job) = jobs.next() else { break };
            sender
                .send(job)
                .map_err(|_| SourceError::Other("Local parser workers stopped".to_string()))?;
            in_flight += 1;
        }
        let mut results = Vec::new();
        while in_flight > 0 {
            let result = self
                .results
                .recv()
                .map_err(|_| SourceError::Other("Local parser workers stopped".to_string()))?;
            in_flight -= 1;
            check_cancelled(cancelled)?;
            results.push(result);
            if let Some(job) = jobs.next() {
                sender
                    .send(job)
                    .map_err(|_| SourceError::Other("Local parser workers stopped".to_string()))?;
                in_flight += 1;
            }
        }
        Ok(results)
    }
}

impl Drop for MediaPool {
    fn drop(&mut self) {
        self.jobs.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct PathReuse {
    unchanged: BTreeMap<String, library::LocalFileState>,
    accepted: BTreeSet<String>,
    renamed: BTreeMap<String, String>,
}

async fn path_reuse(
    database: &library::Database,
    scan: &Scan,
    roots: &[PathBuf],
    paths: &[PathBuf],
    explicit_rename: Option<&(PathBuf, PathBuf)>,
) -> SourceResult<PathReuse> {
    let Some(source) = scan.existing_source() else {
        return Ok(PathReuse::default());
    };
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
        .local_file_reuse_candidates(source, &facts, &library::ReadCancellation::new())
        .await?;
    let mut reuse = PathReuse::default();
    for observation in facts.into_iter().take(original_count) {
        if current.iter().any(|stored| {
            stored.path == observation.path
                && stored.size_bytes == observation.size_bytes
                && stored.mtime_ns == observation.mtime_ns
                && stored.parse_version == Some(i64::from(LOCAL_PARSER_VERSION))
        }) {
            if let Some(stored) = current
                .iter()
                .find(|stored| stored.path == observation.path)
            {
                reuse.unchanged.insert(observation.path, stored.state);
                if stored.track_object_id.is_some() {
                    reuse.accepted.insert(stored.path.clone());
                }
            }
        } else if let Some(stored) = current.iter().find(|stored| {
            stored.device_id == observation.device_id && stored.inode == observation.inode
        }) && let Some(object_id) = stored.track_object_id.as_ref()
        {
            reuse.renamed.insert(observation.path, object_id.clone());
        }
    }
    if let Some((old_path, new_path)) = explicit
        && let Some(object_id) = current
            .iter()
            .find(|stored| stored.path == old_path)
            .and_then(|stored| stored.track_object_id.clone())
    {
        reuse.renamed.insert(new_path, object_id);
    }
    Ok(reuse)
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
        let MediaRead::Accepted(backing) = media::read_media(worker, file.path.clone(), None)
        else {
            return None;
        };
        let duration_millis = u64::from(backing.duration_seconds) * 1_000;
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

fn cue_track(
    cue_path: &Path,
    album_title: Option<&str>,
    album_performer: Option<&str>,
    cue: &CueTrack,
    end_millis: u64,
    backing: &ScannedTrack,
) -> ScannedTrack {
    let mut track = backing.clone();
    let album_artist = album_performer
        .map(ToString::to_string)
        .unwrap_or_else(|| backing.album_artist.clone());
    track.id = media::cue_track_id(cue_path, cue.number);
    track.album = album_title
        .map(ToString::to_string)
        .unwrap_or_else(|| backing.album.clone());
    track.artist = cue
        .performer
        .clone()
        .unwrap_or_else(|| album_artist.clone());
    track.album_artists = media::split_names(&album_artist)
        .iter()
        .map(|name| media::artist_credit(name, None))
        .collect();
    track.artists = media::split_names(&track.artist)
        .iter()
        .map(|name| media::artist_credit(name, None))
        .collect();
    track.album_id = media::album_id(
        &track.album_artists,
        &track.album,
        track.musicbrainz_album_id.as_deref(),
        Some(cue_path),
    );
    track.title = cue
        .title
        .clone()
        .unwrap_or_else(|| format!("Track {}", cue.number));
    track.duration_seconds = end_millis
        .saturating_sub(cue.index_start_ms)
        .div_euclid(1_000)
        .max(1)
        .min(u64::from(u32::MAX)) as u32;
    track.disc_number = track.disc_number.max(1);
    track.track_number = cue.number;
    track.musicbrainz_recording_id = None;
    track.musicbrainz_release_track_id = None;
    track.comment = None;
    track.cue_path = Some(cue_path.to_string_lossy().into_owned());
    track.cue_start_millis = i64::try_from(cue.index_start_ms).ok();
    track.cue_end_millis = i64::try_from(end_millis).ok();
    track
}

async fn stage_album(scan: &mut Scan, track: &ScannedTrack) -> SourceResult<()> {
    let album_artwork = track
        .local_artwork
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()?;
    scan.write_album(
        &track.album_id,
        &track.album,
        &track.album.to_lowercase(),
        &track.album_artist,
        &track.album.to_lowercase(),
        Some(i64::from(track.year)).filter(|year| *year > 0),
        None,
        None,
        track.musicbrainz_album_id.as_deref(),
        track.musicbrainz_release_group_id.as_deref(),
        track.is_compilation,
        album_artwork.as_deref(),
        false,
        None,
        Some(scan.accepted_at()),
    )
    .await?;
    Ok(())
}

async fn stage_track_row(scan: &mut Scan, track: &ScannedTrack) -> SourceResult<()> {
    let artwork = track
        .local_artwork
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()?;
    let normalized_search = format!(
        "{} {} {} {}",
        track.title,
        track.album,
        track.artist,
        track.comment.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    scan.write_track(
        &track.id,
        Some(&track.album_id),
        &track.title,
        &normalized_search,
        &track.album,
        &track.artist,
        &track.title.to_lowercase(),
        i64::from(track.duration_seconds) * 1_000,
        i64::from(track.disc_number),
        i64::from(track.track_number),
        Some(i64::from(track.year)).filter(|year| *year > 0),
        None,
        None,
        Some(&format!("file://{}", track.source_path)),
        track.source_format.as_deref(),
        track.comment.as_deref(),
        track.bpm.map(i64::from),
        track.musicbrainz_recording_id.as_deref(),
        track.musicbrainz_release_track_id.as_deref(),
        track.cue_path.as_deref(),
        track.cue_start_millis,
        track.cue_end_millis,
        artwork.as_deref(),
        false,
        track.user_rating.map(i64::from),
        Some(scan.accepted_at()),
        None,
        None,
        None,
        Some(&track.source_path),
        audio_key(track),
    )
    .await?;
    Ok(())
}

async fn stage_loudness(scan: &mut Scan, track: &ScannedTrack) -> SourceResult<()> {
    if let Some(lufs) = track.track_r128_lufs {
        scan.write_track_source_loudness(&track.id, Some(lufs), None)
            .await?;
    }
    if let Some(lufs) = track.album_r128_lufs {
        scan.write_album_source_loudness(&track.album_id, Some(lufs), None)
            .await?;
    }
    Ok(())
}

async fn stage_genre(scan: &mut Scan, genre: &media::NamedCredit) -> SourceResult<()> {
    Ok(scan
        .write_genre(
            &genre.id,
            &genre.name,
            &genre.name.to_lowercase(),
            &genre.name.to_lowercase(),
            None,
        )
        .await?)
}

async fn stage_mood(scan: &mut Scan, mood: &media::NamedCredit) -> SourceResult<()> {
    Ok(scan
        .write_mood(
            &mood.id,
            &mood.name,
            &mood.name.to_lowercase(),
            &mood.name.to_lowercase(),
        )
        .await?)
}

async fn stage_artist(scan: &mut Scan, artist: &media::ArtistCredit) -> SourceResult<()> {
    Ok(scan
        .write_artist(
            &artist.id,
            &artist.name,
            &artist.name.to_lowercase(),
            &artist.name.to_lowercase(),
            artist.musicbrainz_artist_id.as_deref(),
            None,
            false,
            None,
        )
        .await?)
}

fn audio_key(track: &ScannedTrack) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"rufin-local-audio-v1\0");
    hash.update(track.source_path.as_bytes());
    if let Ok(metadata) = fs::metadata(&track.source_path) {
        hash.update(&metadata.len().to_le_bytes());
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|time| time.as_nanos())
            .unwrap_or_default();
        hash.update(&modified.to_le_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            hash.update(&metadata.dev().to_le_bytes());
            hash.update(&metadata.ino().to_le_bytes());
        }
    }
    hash.update(&LOCAL_PARSER_VERSION.to_le_bytes());
    hash.update(track.cue_path.as_deref().unwrap_or_default().as_bytes());
    hash.update(&track.cue_start_millis.unwrap_or(-1).to_le_bytes());
    hash.update(&track.cue_end_millis.unwrap_or(-1).to_le_bytes());
    *hash.finalize().as_bytes()
}

fn file_observation(
    roots: &[PathBuf],
    path: &Path,
    kind: library::LocalFileKind,
    state: library::LocalFileState,
    dependencies: &[String],
) -> SourceResult<(library::LocalFileWrite, Vec<String>)> {
    let metadata = fs::metadata(path).ok();
    let root = roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .ok_or(SourceError::NotFound)?;
    let relative_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
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
        parse_version: Some(i64::from(LOCAL_PARSER_VERSION)),
        state,
    };
    Ok((write, dependencies.to_vec()))
}

async fn persist_observations(
    scan: &mut Scan,
    observations: Vec<(library::LocalFileWrite, Vec<String>)>,
) -> SourceResult<()> {
    scan.write_local_files(&observations).await?;
    Ok(())
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
    parse_cue_sheet(path, &text)
        .map(CueRead::Accepted)
        .unwrap_or(CueRead::Rejected)
}

fn is_cue(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cue"))
}

fn check_cancelled(cancelled: &(dyn Fn() -> bool + Send + Sync)) -> SourceResult<()> {
    if cancelled() {
        Err(SourceError::Cancelled)
    } else {
        Ok(())
    }
}
