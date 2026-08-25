//! Streams Local files into Scan while retaining only one CUE component or one 128-path batch.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
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

pub(super) async fn stage_catalog(
    database: &library::Database,
    roots: &[PathBuf],
    scan: &mut Scan,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()> {
    validate_walk(roots, cancelled)?;
    let mut worker = media::Worker::default();
    let mut completed = 0_usize;

    // CUE dependencies are recorded in writer-affine TEMP storage before ordinary audio paths.
    for path in walk_paths(roots) {
        check_cancelled(cancelled)?;
        if !is_cue(&path) {
            continue;
        }
        if let Some(dependencies) = unchanged_cue(database, scan, roots, &path).await? {
            scan.begin_batch().await?;
            for dependency in &dependencies {
                scan.write_local_dependency_path(dependency).await?;
            }
            scan.retain_local_cue_path(path.to_string_lossy().as_ref())
                .await?;
            scan.finish_batch().await?;
            continue;
        }
        let Some(sheet) = read_cue(&path) else {
            let dependencies = retained_cue_dependencies(database, scan, roots, &path).await?;
            let observation = file_observation(
                roots,
                &path,
                library::LocalFileKind::Cue,
                library::LocalFileState::Unreadable,
                &dependencies,
            )?;
            persist_observations(database, scan, vec![observation]).await?;
            scan.begin_batch().await?;
            for dependency in &dependencies {
                scan.write_local_dependency_path(dependency).await?;
            }
            scan.retain_local_cue_path(path.to_string_lossy().as_ref())
                .await?;
            scan.finish_batch().await?;
            continue;
        };
        let dependencies = sheet
            .files
            .iter()
            .map(|file| file.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let observation = file_observation(
            roots,
            &path,
            library::LocalFileKind::Cue,
            library::LocalFileState::Accepted,
            &dependencies,
        )?;
        persist_observations(database, scan, vec![observation]).await?;
        let Some(tracks) = read_cue_tracks(&mut worker, &path, sheet) else {
            scan.begin_batch().await?;
            for dependency in &dependencies {
                scan.write_local_dependency_path(dependency).await?;
            }
            scan.retain_local_cue_path(path.to_string_lossy().as_ref())
                .await?;
            scan.finish_batch().await?;
            continue;
        };
        scan.begin_batch().await?;
        for dependency in &dependencies {
            scan.write_local_dependency_path(dependency).await?;
        }
        for track in &tracks {
            stage_track(scan, track).await?;
        }
        scan.finish_batch().await?;
        completed += 1;
        progress(SourceReadProgress {
            stage: SourceReadStage::Files,
            completed,
            total: None,
        });
    }

    let mut batch = Vec::with_capacity(LOCAL_BATCH_SIZE);
    for path in walk_paths(roots) {
        check_cancelled(cancelled)?;
        if is_cue(&path) || super::artwork::supported_image(&path) || !path.is_file() {
            continue;
        }
        batch.push(path);
        if batch.len() == LOCAL_BATCH_SIZE {
            stage_audio_batch(database, roots, &mut worker, scan, &batch).await?;
            completed += batch.len();
            batch.clear();
            progress(SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed,
                total: None,
            });
        }
    }
    if !batch.is_empty() {
        stage_audio_batch(database, roots, &mut worker, scan, &batch).await?;
        completed += batch.len();
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

pub(super) async fn persist_initial_observations(
    database: &library::Database,
    source: library::SourceKey,
    roots: &[PathBuf],
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()> {
    let mut batch = Vec::with_capacity(LOCAL_BATCH_SIZE);
    for path in walk_paths(roots) {
        check_cancelled(cancelled)?;
        batch.push(path);
        if batch.len() == LOCAL_BATCH_SIZE {
            persist_initial_batch(database, source, roots, &batch).await?;
            batch.clear();
        }
    }
    if !batch.is_empty() {
        persist_initial_batch(database, source, roots, &batch).await?;
    }
    Ok(())
}

async fn persist_initial_batch(
    database: &library::Database,
    source: library::SourceKey,
    roots: &[PathBuf],
    paths: &[PathBuf],
) -> SourceResult<()> {
    let path_text = paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let accepted = database
        .local_accepted_paths(source, &path_text, &library::ReadCancellation::new())
        .await?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut observations = Vec::with_capacity(paths.len());
    for path in paths {
        let path_value = path.to_string_lossy();
        let (kind, state, dependencies) = if is_cue(path) {
            match read_cue(path) {
                Some(sheet) => (
                    library::LocalFileKind::Cue,
                    library::LocalFileState::Accepted,
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
        } else {
            (
                library::LocalFileKind::Media,
                if accepted.contains(path_value.as_ref()) {
                    library::LocalFileState::Accepted
                } else {
                    library::LocalFileState::Rejected
                },
                Vec::new(),
            )
        };
        observations.push(file_observation(roots, path, kind, state, &dependencies)?);
    }
    database.upsert_local_files(source, &observations).await?;
    Ok(())
}

async fn stage_audio_batch(
    database: &library::Database,
    roots: &[PathBuf],
    worker: &mut media::Worker,
    scan: &mut Scan,
    paths: &[PathBuf],
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
    let unchanged = unchanged_accepted_paths(database, scan, roots, paths).await?;
    let mut accepted = Vec::with_capacity(paths.len());
    let mut retained = Vec::new();
    let mut observations = Vec::with_capacity(paths.len());
    for path in paths {
        if dependencies.contains(path.to_string_lossy().as_ref()) {
            continue;
        }
        if unchanged.contains(path.to_string_lossy().as_ref()) {
            observations.push(file_observation(
                roots,
                path,
                library::LocalFileKind::Media,
                library::LocalFileState::Accepted,
                &[],
            )?);
            retained.push(path);
            continue;
        }
        match media::read_media(worker, path.clone(), None) {
            MediaRead::Accepted(track) => {
                let observation = file_observation(
                    roots,
                    path,
                    library::LocalFileKind::Media,
                    library::LocalFileState::Accepted,
                    &[],
                )?;
                observations.push(observation);
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
                retained.push(path)
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
                retained.push(path)
            }
        }
    }
    persist_observations(database, scan, observations).await?;
    scan.begin_batch().await?;
    for track in &accepted {
        stage_track(scan, track).await?;
    }
    for path in retained {
        scan.retain_local_media_path(path.to_string_lossy().as_ref())
            .await?;
    }
    scan.finish_batch().await?;
    Ok(())
}

async fn unchanged_accepted_paths(
    database: &library::Database,
    scan: &Scan,
    roots: &[PathBuf],
    paths: &[PathBuf],
) -> SourceResult<BTreeSet<String>> {
    let Some(source) = scan.existing_source() else {
        return Ok(BTreeSet::new());
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
    let mut accepted = BTreeSet::new();
    for observation in facts {
        if current.iter().any(|stored| {
            stored.path == observation.path
                && stored.size_bytes == observation.size_bytes
                && stored.mtime_ns == observation.mtime_ns
                && stored.parse_version == Some(i64::from(LOCAL_PARSER_VERSION))
                && stored.state == library::LocalFileState::Accepted
        }) {
            accepted.insert(observation.path);
        }
    }
    Ok(accepted)
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
    for artist in &track.album_artists {
        stage_artist(scan, artist).await?;
        scan.write_local_album_artist(&track.album_id, &artist.id)
            .await?;
    }
    for release_type in &track.release_types {
        scan.write_local_album_release_type(&track.album_id, release_type)
            .await?;
    }
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
        audio_key(track),
    )
    .await?;
    for (position, artist) in track.artists.iter().enumerate() {
        stage_artist(scan, artist).await?;
        scan.write_track_artist(&track.id, &artist.id, position as i64)
            .await?;
    }
    for (position, genre) in track.genres.iter().enumerate() {
        scan.write_genre(
            &genre.id,
            &genre.name,
            &genre.name.to_lowercase(),
            &genre.name.to_lowercase(),
            None,
        )
        .await?;
        scan.write_track_genre(&track.id, &genre.id, position as i64)
            .await?;
        scan.write_local_album_genre(&track.album_id, &genre.id)
            .await?;
    }
    for (position, mood) in track.moods.iter().enumerate() {
        scan.write_mood(
            &mood.id,
            &mood.name,
            &mood.name.to_lowercase(),
            &mood.name.to_lowercase(),
        )
        .await?;
        scan.write_track_mood(&track.id, &mood.id, position as i64)
            .await?;
    }
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
    database: &library::Database,
    scan: &Scan,
    observations: Vec<(library::LocalFileWrite, Vec<String>)>,
) -> SourceResult<()> {
    if let Some(source) = scan.existing_source()
        && !observations.is_empty()
    {
        database.upsert_local_files(source, &observations).await?;
    }
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

fn walk_paths(roots: &[PathBuf]) -> impl Iterator<Item = PathBuf> + '_ {
    roots.iter().flat_map(|root| {
        WalkDir::new(root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
    })
}

fn validate_walk(
    roots: &[PathBuf],
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()> {
    for root in roots {
        fs::read_dir(root).map_err(|error| {
            SourceError::Other(format!(
                "Could not read Local root {}: {error}",
                root.display()
            ))
        })?;
        for entry in WalkDir::new(root).follow_links(false) {
            check_cancelled(cancelled)?;
            entry.map_err(|error| SourceError::Other(format!("Local walk failed: {error}")))?;
        }
    }
    Ok(())
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
