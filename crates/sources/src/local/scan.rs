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
        for path in page {
            match media::read_media(&mut worker, path.clone(), None) {
                MediaRead::Accepted(track) => stage_track(&mut scan, &track).await?,
                MediaRead::Rejected | MediaRead::Unreadable => {
                    return Err(SourceError::Other(format!(
                        "Could not reread metadata from {}",
                        path.display()
                    )));
                }
            }
        }
        scan.finish_batch().await?;
    }
    Ok(scan.finish().await?)
}

pub(super) async fn publish_paths(
    database: &library::Database,
    source: library::SourceKey,
    source_id: &str,
    roots: &[PathBuf],
    paths: &[PathBuf],
) -> SourceResult<library::ScanOutcome> {
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
    for page in seeds.chunks(LOCAL_BATCH_SIZE) {
        scan.begin_batch().await?;
        scan.write_local_component_paths(page).await?;
        scan.finish_batch().await?;
    }

    let mut walked = Vec::with_capacity(LOCAL_BATCH_SIZE);
    for directory in paths.iter().filter(|path| path.is_dir()) {
        for entry in WalkDir::new(directory)
            .follow_links(false)
            .sort_by_file_name()
        {
            let entry = entry.map_err(|error| {
                SourceError::Other(format!("Local component walk failed: {error}"))
            })?;
            walked.push(entry.path().to_string_lossy().into_owned());
            if walked.len() == LOCAL_BATCH_SIZE {
                scan.begin_batch().await?;
                scan.write_local_component_paths(&walked).await?;
                scan.finish_batch().await?;
                walked.clear();
            }
        }
    }
    if !walked.is_empty() {
        scan.begin_batch().await?;
        scan.write_local_component_paths(&walked).await?;
        scan.finish_batch().await?;
    }
    scan.begin_batch().await?;
    scan.expand_local_component(source).await?;
    scan.finish_batch().await?;

    let mut after = None;
    loop {
        let page = scan
            .local_component_path_page(after.as_deref(), LOCAL_BATCH_SIZE)
            .await?;
        if page.is_empty() {
            break;
        }
        after = page.last().cloned();
        let mut prefixes = BTreeSet::new();
        for path in page.iter().map(PathBuf::from) {
            if super::artwork::supported_image(&path)
                && let Some(directory) = path.parent()
            {
                let prefix = format!("{}/", directory.to_string_lossy().trim_end_matches('/'));
                if database
                    .local_directory_album_count(source, &prefix, &library::ReadCancellation::new())
                    .await?
                    == 1
                {
                    prefixes.insert(prefix);
                }
            }
            if path.is_dir() {
                prefixes.insert(format!("{}/", path.to_string_lossy().trim_end_matches('/')));
            }
        }
        if !prefixes.is_empty() {
            let prefixes = prefixes.into_iter().collect::<Vec<_>>();
            scan.begin_batch().await?;
            scan.write_local_component_prefixes(source, &prefixes)
                .await?;
            for prefix in &prefixes {
                scan.invalidate_local_album_artwork_directory(prefix)
                    .await?;
            }
            scan.finish_batch().await?;
        }
    }
    scan.begin_batch().await?;
    scan.expand_local_component(source).await?;
    if !artwork_only {
        scan.remove_local_component_tracks(source).await?;
    }
    scan.finish_batch().await?;

    let mut after = None;
    loop {
        let page = scan
            .local_component_path_page(after.as_deref(), LOCAL_BATCH_SIZE)
            .await?;
        if page.is_empty() {
            break;
        }
        after = page.last().cloned();
        scan.begin_batch().await?;
        for path in &page {
            if !Path::new(path).exists() {
                scan.remove_local_file_path(path).await?;
            }
        }
        scan.finish_batch().await?;
    }

    if !artwork_only {
        let mut after = None;
        loop {
            let page = scan
                .local_component_path_page(after.as_deref(), LOCAL_BATCH_SIZE)
                .await?;
            if page.is_empty() {
                break;
            }
            after = page.last().cloned();
            for path in page.iter().map(PathBuf::from).filter(|path| is_cue(path)) {
                if !path.is_file() {
                    continue;
                }
                let (state, dependencies, tracks) = match read_cue(&path) {
                    Some(sheet) => {
                        let dependencies = sheet
                            .files
                            .iter()
                            .map(|file| file.path.to_string_lossy().into_owned())
                            .collect::<Vec<_>>();
                        let tracks = read_cue_tracks(&mut media::Worker::default(), &path, sheet);
                        let state = if tracks.is_some() {
                            library::LocalFileState::Accepted
                        } else {
                            library::LocalFileState::Unreadable
                        };
                        (state, dependencies, tracks.unwrap_or_default())
                    }
                    None => (library::LocalFileState::Unreadable, Vec::new(), Vec::new()),
                };
                scan.begin_batch().await?;
                for dependency in &dependencies {
                    scan.write_local_dependency_path(dependency).await?;
                }
                for track in &tracks {
                    stage_track(&mut scan, track).await?;
                }
                persist_observations(
                    &mut scan,
                    vec![file_observation(
                        roots,
                        &path,
                        library::LocalFileKind::Cue,
                        state,
                        &dependencies,
                    )?],
                )
                .await?;
                scan.finish_batch().await?;
            }
        }

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
            let audio = page
                .into_iter()
                .map(PathBuf::from)
                .filter(|path| {
                    path.is_file() && !is_cue(path) && !super::artwork::supported_image(path)
                })
                .collect::<Vec<_>>();
            if !audio.is_empty() {
                stage_audio_batch(
                    database,
                    roots,
                    &mut scan,
                    &audio,
                    &|| false,
                    true,
                    &mut parsers,
                )
                .await?;
            }
        }
    }

    let mut after = None;
    loop {
        let page = scan
            .local_component_path_page(after.as_deref(), LOCAL_BATCH_SIZE)
            .await?;
        if page.is_empty() {
            break;
        }
        after = page.last().cloned();
        let observations = page
            .into_iter()
            .map(PathBuf::from)
            .filter_map(|path| {
                let kind = if path.is_dir() {
                    Some(library::LocalFileKind::Directory)
                } else if path.is_file() && super::artwork::supported_image(&path) {
                    Some(library::LocalFileKind::Image)
                } else {
                    None
                }?;
                Some(file_observation(
                    roots,
                    &path,
                    kind,
                    library::LocalFileState::Observed,
                    &[],
                ))
            })
            .collect::<SourceResult<Vec<_>>>()?;
        if !observations.is_empty() {
            scan.begin_batch().await?;
            persist_observations(&mut scan, observations).await?;
            scan.finish_batch().await?;
        }
    }
    Ok(scan.finish().await?)
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
                match read_cue(path) {
                    Some(sheet) => (
                        library::LocalFileKind::Cue,
                        library::LocalFileState::Observed,
                        sheet
                            .files
                            .into_iter()
                            .map(|file| file.path.to_string_lossy().into_owned())
                            .collect(),
                    ),
                    None => (
                        library::LocalFileKind::Cue,
                        library::LocalFileState::Unreadable,
                        Vec::new(),
                    ),
                }
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
                scan.begin_batch().await?;
                persist_observations(scan, std::mem::take(&mut observations)).await?;
                scan.finish_batch().await?;
            }
        }
    }
    if !observations.is_empty() {
        scan.begin_batch().await?;
        persist_observations(scan, observations).await?;
        scan.finish_batch().await?;
    }

    let mut completed = 0_usize;
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
                for dependency in &dependencies {
                    scan.write_local_dependency_path(dependency).await?;
                }
                scan.retain_local_cue_path(&path_text).await?;
                scan.finish_batch().await?;
                continue;
            }
            let sheet = read_cue(&path);
            let dependencies = match sheet.as_ref() {
                Some(sheet) => sheet
                    .files
                    .iter()
                    .map(|file| file.path.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
                None if reuse_unchanged => {
                    retained_cue_dependencies(database, scan, roots, &path).await?
                }
                None => Vec::new(),
            };
            let tracks = sheet
                .and_then(|sheet| read_cue_tracks(&mut media::Worker::default(), &path, sheet));
            let state = if tracks.is_some() {
                library::LocalFileState::Accepted
            } else {
                library::LocalFileState::Unreadable
            };
            scan.begin_batch().await?;
            for dependency in &dependencies {
                scan.write_local_dependency_path(dependency).await?;
            }
            if let Some(tracks) = tracks {
                for track in &tracks {
                    stage_track(scan, track).await?;
                }
            } else if reuse_unchanged {
                scan.retain_local_cue_path(&path_text).await?;
            }
            persist_observations(
                scan,
                vec![file_observation(
                    roots,
                    &path,
                    library::LocalFileKind::Cue,
                    state,
                    &dependencies,
                )?],
            )
            .await?;
            scan.finish_batch().await?;
            completed += 1;
            progress(SourceReadProgress {
                stage: SourceReadStage::Files,
                completed,
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
        stage_audio_batch(
            database,
            roots,
            scan,
            &paths,
            cancelled,
            reuse_unchanged,
            &mut parsers,
        )
        .await?;
        completed += paths.len();
        progress(SourceReadProgress {
            stage: SourceReadStage::Tracks,
            completed,
            total: None,
        });
    }
    progress(SourceReadProgress {
        stage: SourceReadStage::Finalizing,
        completed,
        total: Some(completed),
    });
    Ok(())
}
async fn unchanged_cue(
    database: &library::Database,
    scan: &Scan,
    roots: &[PathBuf],
    path: &Path,
) -> SourceResult<Option<Vec<String>>> {
    let Some(source) = scan.existing_source() else {
        return Ok(None);
    };
    let observation = file_observation(
        roots,
        path,
        library::LocalFileKind::Cue,
        library::LocalFileState::Observed,
        &[],
    )?
    .0;
    let (Some(device), Some(inode)) = (observation.device_id, observation.inode) else {
        return Ok(None);
    };
    let current = database
        .local_file_identities(
            source,
            &[(device, inode)],
            &library::ReadCancellation::new(),
        )
        .await?;
    Ok(current.into_iter().find_map(|stored| {
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
    let Some(source) = scan.existing_source() else {
        return Ok(Vec::new());
    };
    let observation = file_observation(
        roots,
        path,
        library::LocalFileKind::Cue,
        library::LocalFileState::Observed,
        &[],
    )?
    .0;
    let (Some(device), Some(inode)) = (observation.device_id, observation.inode) else {
        return Ok(Vec::new());
    };
    Ok(database
        .local_file_identities(
            source,
            &[(device, inode)],
            &library::ReadCancellation::new(),
        )
        .await?
        .into_iter()
        .next()
        .map(|file| file.dependencies)
        .unwrap_or_default())
}

async fn stage_audio_batch(
    database: &library::Database,
    roots: &[PathBuf],
    scan: &mut Scan,
    paths: &[PathBuf],
    cancelled: &(dyn Fn() -> bool + Send + Sync),
    reuse_unchanged: bool,
    parsers: &mut MediaPool,
) -> SourceResult<()> {
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
        path_reuse(database, scan, roots, paths).await?
    } else {
        PathReuse::default()
    };
    let unchanged = reuse.unchanged;
    let mut renamed_ids = BTreeMap::new();
    for (new_path, old_path) in reuse.renamed {
        if let Some(object_id) = database
            .local_track_objects_for_paths(
                scan.existing_source()
                    .expect("reused Local path has a source"),
                std::slice::from_ref(&old_path),
                &library::ReadCancellation::new(),
            )
            .await?
            .into_iter()
            .next()
        {
            renamed_ids.insert(new_path, object_id);
        }
    }
    let accepted_current = if reuse_unchanged {
        match scan.existing_source() {
            Some(source) => database
                .local_accepted_paths(source, &path_text, &library::ReadCancellation::new())
                .await?
                .into_iter()
                .collect::<BTreeSet<_>>(),
            None => BTreeSet::new(),
        }
    } else {
        BTreeSet::new()
    };
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
    scan.begin_batch().await?;
    stage_audio_tracks_batch(scan, &accepted).await?;
    scan.retain_local_media_paths(&retained).await?;
    scan.finish_batch().await?;
    Ok(())
}

async fn stage_audio_tracks_batch(scan: &mut Scan, tracks: &[ScannedTrack]) -> SourceResult<()> {
    let mut albums = BTreeSet::new();
    let mut artists = BTreeSet::new();
    let mut genres = BTreeSet::new();
    let mut moods = BTreeSet::new();
    let mut album_artists = Vec::new();
    let mut album_genres = Vec::new();
    let mut album_release_types = Vec::new();
    let mut track_artists = Vec::new();
    let mut track_genres = Vec::new();
    let mut track_moods = Vec::new();

    for track in tracks {
        if albums.insert(track.album_id.as_str()) {
            stage_album(scan, track).await?;
        }
        for artist in &track.album_artists {
            if artists.insert(artist.id.as_str()) {
                stage_artist(scan, artist).await?;
            }
            album_artists.push(library::ScanLink {
                owner_id: &track.album_id,
                related_id: &artist.id,
                position: 0,
            });
        }
        for release_type in &track.release_types {
            album_release_types.push(library::ScanLink {
                owner_id: &track.album_id,
                related_id: release_type,
                position: 0,
            });
        }
        stage_track_row(scan, track).await?;
        for (position, artist) in track.artists.iter().enumerate() {
            if artists.insert(artist.id.as_str()) {
                stage_artist(scan, artist).await?;
            }
            track_artists.push(library::ScanLink {
                owner_id: &track.id,
                related_id: &artist.id,
                position: position as i64,
            });
        }
        for (position, genre) in track.genres.iter().enumerate() {
            if genres.insert(genre.id.as_str()) {
                stage_genre(scan, genre).await?;
            }
            track_genres.push(library::ScanLink {
                owner_id: &track.id,
                related_id: &genre.id,
                position: position as i64,
            });
            album_genres.push(library::ScanLink {
                owner_id: &track.album_id,
                related_id: &genre.id,
                position: 0,
            });
        }
        for (position, mood) in track.moods.iter().enumerate() {
            if moods.insert(mood.id.as_str()) {
                stage_mood(scan, mood).await?;
            }
            track_moods.push(library::ScanLink {
                owner_id: &track.id,
                related_id: &mood.id,
                position: position as i64,
            });
        }
        stage_loudness(scan, track).await?;
    }
    scan.write_local_album_artists(&album_artists).await?;
    scan.write_local_album_genres(&album_genres).await?;
    scan.write_local_album_release_types(&album_release_types)
        .await?;
    scan.write_track_artists(&track_artists).await?;
    scan.write_track_genres(&track_genres).await?;
    scan.write_track_moods(&track_moods).await?;
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
    renamed: BTreeMap<String, String>,
}

async fn path_reuse(
    database: &library::Database,
    scan: &Scan,
    roots: &[PathBuf],
    paths: &[PathBuf],
) -> SourceResult<PathReuse> {
    let Some(source) = scan.existing_source() else {
        return Ok(PathReuse::default());
    };
    let mut facts = Vec::new();
    let mut identities = Vec::new();
    for path in paths {
        let observation = file_observation(
            roots,
            path,
            library::LocalFileKind::Media,
            library::LocalFileState::Observed,
            &[],
        )?
        .0;
        if let (Some(device), Some(inode)) = (observation.device_id, observation.inode) {
            identities.push((device, inode));
            facts.push(observation);
        }
    }
    let current = database
        .local_file_identities(source, &identities, &library::ReadCancellation::new())
        .await?;
    let mut reuse = PathReuse::default();
    for observation in facts {
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
            }
        } else if let Some(stored) = current.iter().find(|stored| {
            stored.device_id == observation.device_id && stored.inode == observation.inode
        }) {
            reuse.renamed.insert(observation.path, stored.path.clone());
        }
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

async fn stage_track(scan: &mut Scan, track: &ScannedTrack) -> SourceResult<()> {
    stage_album(scan, track).await?;
    for artist in &track.album_artists {
        stage_artist(scan, artist).await?;
        scan.write_local_album_artist(&track.album_id, &artist.id)
            .await?;
    }
    for release_type in &track.release_types {
        scan.write_local_album_release_type(&track.album_id, release_type)
            .await?;
    }
    stage_track_row(scan, track).await?;
    for (position, artist) in track.artists.iter().enumerate() {
        stage_artist(scan, artist).await?;
        scan.write_track_artist(&track.id, &artist.id, position as i64)
            .await?;
    }
    for (position, genre) in track.genres.iter().enumerate() {
        stage_genre(scan, genre).await?;
        scan.write_track_genre(&track.id, &genre.id, position as i64)
            .await?;
        scan.write_local_album_genre(&track.album_id, &genre.id)
            .await?;
    }
    for (position, mood) in track.moods.iter().enumerate() {
        stage_mood(scan, mood).await?;
        scan.write_track_mood(&track.id, &mood.id, position as i64)
            .await?;
    }
    stage_loudness(scan, track).await
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

fn read_cue(path: &Path) -> Option<CueSheet> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.len() > LOCAL_CUE_MAX_BYTES {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    parse_cue_sheet(path, &text)
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
