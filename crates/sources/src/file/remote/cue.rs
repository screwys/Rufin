use std::sync::{Arc, Mutex};

use library::{LocalFileKind, LocalFileState, Scan};

use super::{FileSource, scan::PARSER_VERSION};
use crate::file::{
    cue::{cue_track, parse_cue_sheet},
    media,
};
use crate::{SourceError, SourceResult};

impl FileSource {
    pub(crate) async fn stage_cues(
        &self,
        scan: &mut Scan,
        progress: &(dyn Fn(usize) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
        rename: Option<&(String, String)>,
    ) -> SourceResult<usize> {
        let worker = Arc::new(Mutex::new(media::Worker::network()));
        let root_scope = self.location("")?;
        let mut after = None;
        let mut completed = 0;
        loop {
            let files = scan
                .local_inventory_file_page_in_root(
                    LocalFileKind::Cue,
                    after.as_deref(),
                    Some(&root_scope),
                )
                .await?;
            if files.is_empty() {
                break;
            }
            after = files.last().map(|file| file.path.clone());
            let previous = scan.local_file_reuse_candidates(&files).await?;
            for mut file in files {
                if cancelled() {
                    return Err(SourceError::Cancelled);
                }
                let renamed = if let Some((old, new)) = rename
                    && let Some(suffix) = file.path.strip_prefix(new)
                    && (suffix.is_empty() || suffix.starts_with('/'))
                {
                    let mut from = file.clone();
                    from.path = format!("{old}{suffix}");
                    scan.local_file_reuse_candidates(&[from]).await?
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
                        .cue_dependencies_unchanged(scan, &old.dependencies)
                        .await?
                {
                    scan.begin_batch().await?;
                    scan.retain_local_cue_paths(&[&file.path]).await?;
                    for page in old.dependencies.chunks(128) {
                        scan.write_local_dependency_paths(page).await?;
                    }
                    scan.finish_batch().await?;
                    continue;
                }
                file.parse_version = Some(PARSER_VERSION);
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
                        let referenced = super::referenced_path(
                            crate::file::documents::relative_parts(&relative).0,
                            &backing.path,
                        )?;
                        dependencies.push(if self.document_access().is_some() {
                            self.stat(&input, &referenced).await?.path
                        } else {
                            self.location(&referenced)?
                        });
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
                            format!(
                                "cue:{:016x}",
                                crate::policy::stable_hash(&super::file_identity(&file.path))
                            )
                        });
                    let mut failure = None;
                    for (entry, path) in sheet.files.iter().zip(&dependencies) {
                        if cancelled() {
                            return Err(SourceError::Cancelled);
                        }
                        let mut backing_file = match scan
                            .local_inventory_files(std::slice::from_ref(path))
                            .await?
                            .pop()
                        {
                            Some(file) => file,
                            None => match self.stat(&input, &self.relative(path)?).await {
                                Ok(file) => file,
                                Err(error) => {
                                    failure = Some(error);
                                    continue;
                                }
                            },
                        };
                        backing_file.parse_version = Some(PARSER_VERSION);
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
                                scan.retain_local_media_paths(std::slice::from_ref(path))
                                    .await?;
                                scan.write_local_files(&[(backing_file, vec![])]).await?;
                                continue;
                            }
                        };
                        backing_file.artist_pictures =
                            Some(serde_json::to_string(&backing.artist_pictures)?);
                        backing_file.picture_index = match &backing.local_artwork {
                            Some(crate::LocalImageRef::Embedded { picture_index, .. }) => {
                                Some(i64::from(*picture_index))
                            }
                            _ => None,
                        };
                        self.locate_track(&mut backing, &backing_file);
                        let duration = backing.track.duration_millis as u64;
                        for (chunk, cues) in entry.tracks.chunks(128).enumerate() {
                            let mut tracks = Vec::with_capacity(cues.len());
                            let parsed: SourceResult<bool> = (|| {
                                for (position, cue) in cues.iter().enumerate() {
                                    let end = entry
                                        .tracks
                                        .get(chunk * 128 + position + 1)
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
                                    track.track.object_id = format!("{prefix}:{}", cue.number);
                                    track.track.cue_path = Some(file.path.clone());
                                    tracks.push(track.into_inputs(scan.accepted_at())?);
                                }
                                Ok(true)
                            })();
                            scan.begin_batch().await?;
                            scan.write_tracks(
                                &tracks
                                    .iter()
                                    .map(|(album, track)| (track, Some(album)))
                                    .collect::<Vec<_>>(),
                            )
                            .await?;
                            scan.finish_batch().await?;
                            for _ in tracks {
                                completed += 1;
                                progress(completed);
                            }
                            if !parsed? {
                                return Ok(false);
                            }
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
                        scan.clear_freshness();
                        if let Some(old) = old.or_else(|| renamed.first()) {
                            scan.retain_local_cue_paths(&[&old.path]).await?;
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
        scan: &mut Scan,
        paths: &[String],
    ) -> SourceResult<bool> {
        for page in paths.chunks(128) {
            let files = scan.local_inventory_files(page).await?;
            if files.len() != page.len() {
                return Ok(false);
            }
            let previous = scan.local_file_reuse_candidates(&files).await?;
            if files.iter().any(|file| {
                file.revision.is_none()
                    || !previous.iter().any(|old| {
                        old.path == file.path
                            && old.revision == file.revision
                            && old.state == LocalFileState::Accepted
                            && old.parse_version == Some(PARSER_VERSION)
                    })
            }) {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
