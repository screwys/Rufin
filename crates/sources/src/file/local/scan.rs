//! Local file reads outside the walk: a saved file read again, and CUE sheets.

use std::fs;
use std::path::{Path, PathBuf};

use super::LocalSource;
use super::media::read_media;
use crate::file::cue::{CueSheet, cue_track, parse_cue_sheet};
use crate::file::media::{self as media, MediaRead, ScannedTrack};
use crate::file::walk;
use crate::source::SourceReadProgress;
use crate::{SourceError, SourceResult};

const LOCAL_CUE_MAX_BYTES: u64 = 1024 * 1024;
pub(crate) const LOCAL_PARSER_VERSION: i64 = 20;

pub(crate) async fn read_saved_file(
    observation: &library::Observation<'_>,
    media_uri: &str,
    path: &Path,
) -> SourceResult<(Option<library::LocalFileWrite>, library::TrackObservation)> {
    let (object_id, source_path, previous) = observation
        .catalog
        .saved_file_observation(observation.source, media_uri)
        .await?
        .ok_or(SourceError::NotFound)?;
    let path = normalize_observed_path(path);
    let track = media::read_saved_media(&path, path.clone()).await?;
    let saved_path = path.clone();
    let (mut track, mut file, revision, audio_revision) = tokio::task::spawn_blocking(move || {
        let mut track = track;
        crate::file::lyrics::read_local(&mut track);
        let file = match previous {
            Some(previous) => Some(super::local_row(
                &previous.root,
                &previous.relative_path,
                &super::entry(
                    String::new(),
                    &fs::metadata(&saved_path)
                        .map_err(|error| SourceError::Other(error.to_string()))?,
                ),
            )),
            None => None,
        };
        Ok::<_, SourceError>((
            track,
            file,
            super::artwork::revision(&saved_path).unwrap_or_default(),
            super::media::audio_revision(
                &saved_path.to_string_lossy(),
                &super::media::local_media_identity_path(&saved_path),
            ),
        ))
    })
    .await
    .map_err(|error| SourceError::Other(error.to_string()))??;
    if let Some(file) = &mut file {
        track.observe_images(file)?;
    }
    track.local_artwork = track
        .picture_index
        .and_then(|index| u32::try_from(index).ok())
        .map(|picture_index| crate::LocalImageRef::Embedded {
            source_id: crate::SourceId::new(observation.source_id),
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
    Ok((file, track.into_observation(media::accepted_at())))
}

pub(crate) async fn read_saved_document(
    observation: &library::Observation<'_>,
    media_uri: &str,
    document_uri: &str,
    temporary: &Path,
) -> SourceResult<(Option<library::LocalFileWrite>, library::TrackObservation)> {
    let (object_id, source_path, mut file) = observation
        .catalog
        .saved_file_observation(observation.source, media_uri)
        .await?
        .ok_or(SourceError::NotFound)?;
    let stat_uri = document_uri.to_owned();
    let entry = tokio::task::spawn_blocking(move || crate::stat_document(&stat_uri))
        .await
        .map_err(|error| SourceError::Other(error.to_string()))??;
    let mut track = media::read_saved_media(temporary, PathBuf::from(&entry.name)).await?;
    let path = source_path.unwrap_or_else(|| document_uri.to_owned());
    if let Some(file) = &mut file {
        file.size_bytes = entry.size.and_then(|size| i64::try_from(size).ok());
        file.revision = entry.revision.clone();
        file.native_id = Some(entry.native_id);
        track.observe_images(file)?;
        file.parse_version = Some(super::super::remote::scan::PARSER_VERSION);
        file.state = library::LocalFileState::Accepted;
    }
    track.track.object_id = object_id;
    track.track.media_uri = Some(media_uri.to_owned());
    track.track.source_path = Some(path.clone());
    track.audio_revision = super::super::remote::scan::audio_revision(
        &path,
        entry.revision.as_deref(),
        entry.size.and_then(|size| i64::try_from(size).ok()),
    );
    track.revision_hashes_audio = true;
    track.local_artwork = track
        .picture_index
        .and_then(|index| u32::try_from(index).ok())
        .map(|picture_index| crate::LocalImageRef::Embedded {
            source_id: crate::SourceId::new(observation.source_id),
            path,
            revision: entry.revision.unwrap_or_default(),
            picture_index,
        });
    Ok((file, track.into_observation(media::accepted_at())))
}

/// Dropped files and folders: each one outside the source's folders becomes an import root,
/// and a playlist holds what the walk listed under them.
pub(crate) async fn import_paths(
    observation: &mut library::Observation<'_>,
    local: &LocalSource,
    paths: Vec<PathBuf>,
    name: &str,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<library::PlaylistKey> {
    let paths = paths
        .iter()
        .map(|path| normalize_observed_path(path).to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let files = super::LocalFiles::importing(local, &paths);
    let configured = local.configured_file_roots()?;
    walk::walk(
        &files,
        observation,
        &configured,
        walk::WalkScope::Paths {
            paths: &paths,
            rename: None,
        },
        &local.artwork,
        progress,
        cancelled,
    )
    .await?;
    let playlist = observation.create_observed_playlist(&paths, name).await?;
    observation
        .record_imported_files(playlist, &configured)
        .await?;
    Ok(playlist)
}

pub(crate) fn normalize_observed_path(path: &Path) -> PathBuf {
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
            let mut track = cue_track(
                cue_path,
                album_title.as_deref(),
                album_performer.as_deref(),
                cue,
                end,
                &backing,
            );
            crate::file::lyrics::read_local(&mut track);
            tracks.push(track);
        }
    }
    Some(tracks)
}

/// A CUE sheet and the media it plays.
pub(crate) fn read_local_cue(worker: &mut media::Worker, path: &Path) -> walk::CueRead {
    let Ok(metadata) = fs::metadata(path) else {
        return walk::CueRead::Unreadable;
    };
    if metadata.len() > LOCAL_CUE_MAX_BYTES {
        return walk::CueRead::Rejected;
    }
    let Ok(text) = fs::read_to_string(path) else {
        return walk::CueRead::Unreadable;
    };
    let Some(sheet) = parse_cue_sheet(&text) else {
        return walk::CueRead::Rejected;
    };
    let inputs = sheet
        .files
        .iter()
        .map(|file| {
            normalize_observed_path(&resolve_cue_file_path(path, &file.path))
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    match read_cue_tracks(worker, path, sheet) {
        Some(tracks) => walk::CueRead::Accepted {
            inputs,
            tracks: tracks
                .into_iter()
                .map(|track| track.into_observation(media::accepted_at()))
                .collect(),
        },
        None => walk::CueRead::Unreadable,
    }
}

pub(crate) fn is_cue(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cue"))
}
