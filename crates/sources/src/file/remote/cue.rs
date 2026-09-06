use std::sync::{Arc, Mutex};

use library::{Database, LocalFileKind, LocalFileState, ReadCancellation, Scan};

use super::{RemoteSource, scan::PARSER_VERSION};
use crate::file::{
    cue::{cue_track, parse_cue_sheet},
    media,
    scan::stage_audio_tracks_batch,
};
use crate::{SourceError, SourceResult};

impl RemoteSource {
    pub(crate) async fn stage_cues(
        &self,
        database: &Database,
        scan: &mut Scan,
        progress: &(dyn Fn(usize) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
        rename: Option<&(String, String)>,
    ) -> SourceResult<usize> {
        let worker = Arc::new(Mutex::new(media::Worker::network()));
        let mut after = None;
        let mut completed = 0;
        loop {
            let files = scan
                .local_inventory_file_page(LocalFileKind::Cue, after.as_deref())
                .await?;
            if files.is_empty() {
                break;
            }
            after = files.last().map(|file| file.path.clone());
            let previous = if let Some(source) = scan.existing_source() {
                database
                    .local_file_reuse_candidates(source, &files, &ReadCancellation::new())
                    .await?
            } else {
                vec![]
            };
            for mut file in files {
                if cancelled() {
                    return Err(SourceError::Cancelled);
                }
                let renamed = if let Some((old, new)) = rename
                    && let Some(suffix) = file.path.strip_prefix(new)
                    && (suffix.is_empty() || suffix.starts_with('/'))
                    && let Some(source) = scan.existing_source()
                {
                    let mut from = file.clone();
                    from.path = format!("{old}{suffix}");
                    database
                        .local_file_reuse_candidates(source, &[from], &ReadCancellation::new())
                        .await?
                } else {
                    vec![]
                };
                let old = previous.iter().find(|old| old.path == file.path);
                if let Some(old) = old
                    && file.revision.is_some()
                    && old.revision == file.revision
                    && old.parse_version == Some(PARSER_VERSION)
                    && old.state == LocalFileState::Accepted
                    && self
                        .cue_dependencies_unchanged(database, scan, &old.dependencies)
                        .await?
                {
                    file.state = LocalFileState::Accepted;
                    file.picture_index = old.picture_index;
                    scan.begin_batch().await?;
                    scan.retain_local_cue_path(&file.path).await?;
                    for page in old.dependencies.chunks(128) {
                        scan.write_local_dependency_paths(page).await?;
                    }
                    scan.write_local_files(&[(file, old.dependencies.clone())])
                        .await?;
                    scan.finish_batch().await?;
                    continue;
                }
                let relative = self.relative(&file.path)?;
                let mut dependencies = Vec::new();
                let parsed: SourceResult<bool> = async {
                    let input = self.input().await?;
                    let text = self.small_file(&input, &relative, 1024 * 1024).await?;
                    let Some(sheet) = std::str::from_utf8(&text).ok().and_then(parse_cue_sheet)
                    else {
                        return Ok(false);
                    };
                    for backing in &sheet.files {
                        dependencies.push(
                            self.location(&super::referenced_path(&relative, &backing.path)?)?,
                        );
                    }
                    for page in dependencies.chunks(128) {
                        scan.write_local_dependency_paths(page).await?;
                    }
                    let mut identity = old.or_else(|| renamed.first());
                    if identity.is_none()
                        && let Some(id) = &file.native_id
                    {
                        let mut matches = previous
                            .iter()
                            .filter(|old| old.native_id.as_ref() == Some(id));
                        let first = matches.next();
                        if matches.next().is_none()
                            && scan.local_inventory_native_paths(id).await? == [file.path.clone()]
                        {
                            identity = first;
                        }
                    }
                    let prefix = identity
                        .and_then(|old| old.track_object_id.as_deref())
                        .and_then(|id| id.rsplit_once(':').map(|(prefix, _)| prefix.to_string()))
                        .unwrap_or_else(|| {
                            format!("cue:{:016x}", crate::policy::stable_hash(&file.path))
                        });
                    let mut failure = None;
                    for (entry, path) in sheet.files.iter().zip(&dependencies) {
                        if cancelled() {
                            return Err(SourceError::Cancelled);
                        }
                        let Some(mut backing_file) = scan
                            .local_inventory_files(std::slice::from_ref(path))
                            .await?
                            .pop()
                        else {
                            failure = Some(SourceError::NotFound);
                            continue;
                        };
                        let mut backing = match self
                            .read_audio(&backing_file, Arc::clone(&worker))
                            .await
                        {
                            Ok(media::MediaRead::Accepted(backing)) => backing,
                            result => {
                                failure = Some(result.err().unwrap_or_else(|| {
                                    SourceError::Other("Could not read CUE backing audio".into())
                                }));
                                backing_file.state = LocalFileState::Unreadable;
                                scan.write_local_files(&[(backing_file, vec![])]).await?;
                                continue;
                            }
                        };
                        backing_file.picture_index = match &backing.local_artwork {
                            Some(crate::LocalImageRef::Embedded { picture_index, .. }) => {
                                Some(i64::from(*picture_index))
                            }
                            _ => None,
                        };
                        self.locate_track(&mut backing, &backing_file);
                        let duration = u64::from(backing.duration_seconds) * 1000;
                        for (position, cue) in entry.tracks.iter().enumerate() {
                            let end = entry
                                .tracks
                                .get(position + 1)
                                .map_or(duration, |next| next.index_start_ms);
                            if cue.index_start_ms >= end || end > duration {
                                return Ok(false);
                            }
                            // The relative CUE name supplies Local's album/tag fallbacks; URLs never become native paths.
                            let mut track = cue_track(
                                std::path::Path::new(&relative),
                                sheet.album_title.as_deref(),
                                sheet.album_performer.as_deref(),
                                cue,
                                end,
                                &backing,
                            );
                            track.id = format!("{prefix}:{}", cue.number);
                            track.cue_path = Some(file.path.clone());
                            scan.begin_batch().await?;
                            stage_audio_tracks_batch(scan, &[track]).await?;
                            scan.finish_batch().await?;
                            completed += 1;
                            progress(completed);
                        }
                        backing_file.state = LocalFileState::Accepted;
                        scan.write_local_files(&[(backing_file, vec![])]).await?;
                    }
                    failure.map_or(Ok(true), Err)
                }
                .await;
                file.state = match parsed {
                    Ok(true) => LocalFileState::Accepted,
                    Ok(false) => LocalFileState::Rejected,
                    Err(SourceError::Cancelled) => return Err(SourceError::Cancelled),
                    Err(error) => {
                        tracing::warn!(%error, "could not read remote CUE album");
                        scan.incomplete();
                        if let Some(old) = old {
                            scan.retain_local_cue_path(&old.path).await?;
                            for page in old.dependencies.chunks(128) {
                                scan.write_local_dependency_paths(page).await?;
                            }
                            if dependencies.is_empty() {
                                dependencies = old.dependencies.clone();
                            }
                        }
                        LocalFileState::Unreadable
                    }
                };
                scan.write_local_files(&[(file, dependencies)]).await?;
            }
        }
        Ok(completed)
    }

    async fn cue_dependencies_unchanged(
        &self,
        database: &Database,
        scan: &mut Scan,
        paths: &[String],
    ) -> SourceResult<bool> {
        let Some(source) = scan.existing_source() else {
            return Ok(false);
        };
        for page in paths.chunks(128) {
            let files = scan.local_inventory_files(page).await?;
            if files.len() != page.len() {
                return Ok(false);
            }
            let previous = database
                .local_file_reuse_candidates(source, &files, &ReadCancellation::new())
                .await?;
            if files.iter().any(|file| {
                file.revision.is_none()
                    || !previous.iter().any(|old| {
                        old.path == file.path
                            && old.revision == file.revision
                            && old.state == LocalFileState::Accepted
                    })
            }) {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
