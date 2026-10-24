use std::sync::{Arc, Mutex};

use library::{LocalFileKind, LocalFileState};

use super::{
    FileSource,
    scan::{PARSER_VERSION, reusable_parser_version},
};
use crate::file::{
    check_cancelled,
    cue::{cue_track, parse_cue_sheet},
    media,
};
use crate::{SourceError, SourceResult};

impl FileSource {
    pub(super) async fn acquire_cues(
        &self,
        catalog: &mut library::Catalog<'_>,
        parent: Option<&str>,
        worker: Arc<Mutex<media::Worker>>,
        progress: &(dyn Fn(usize) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
        rename: Option<&(String, String)>,
    ) -> SourceResult<usize> {
        let database = catalog.database;
        let source_id = catalog.source_id;
        let stamp = catalog.observed;
        let root = self.location("")?;
        let mut after = None;
        let mut completed = 0;
        loop {
            let files = database
                .source_file_page(
                    source_id,
                    LocalFileKind::Cue,
                    stamp,
                    after.as_deref(),
                    parent,
                    Some(&root),
                )
                .await?;
            if files.is_empty() {
                break;
            }
            after = files.last().map(|file| file.path.clone());
            let previous = database
                .source_file_reuse_candidates(source_id, &files)
                .await?;
            for file in files {
                completed += self
                    .acquire_cue_file(
                        catalog,
                        file,
                        &previous,
                        Arc::clone(&worker),
                        &|count| progress(completed + count),
                        cancelled,
                        rename,
                        true,
                    )
                    .await?;
            }
        }
        Ok(completed)
    }

    pub(super) async fn acquire_cue_file(
        &self,
        catalog: &mut library::Catalog<'_>,
        mut file: library::LocalFileWrite,
        previous: &[library::LocalFileRow],
        worker: Arc<Mutex<media::Worker>>,
        progress: &(dyn Fn(usize) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
        rename: Option<&(String, String)>,
        reuse_unchanged: bool,
    ) -> SourceResult<usize> {
        let database = catalog.database;
        let source_id = catalog.source_id;
        let stamp = catalog.observed;
        let mut completed = 0;
        check_cancelled(cancelled)?;
        let renamed = if let Some((old, new)) = rename
            && let Some(suffix) = file.path.strip_prefix(new)
            && (suffix.is_empty() || suffix.starts_with('/'))
        {
            let mut from = file.clone();
            from.path = format!("{old}{suffix}");
            database
                .source_file_reuse_candidates(source_id, &[from])
                .await?
        } else {
            vec![]
        };
        let old = previous
            .iter()
            .find(|old| old.path == file.path && old.track_object_id.is_some());
        if reuse_unchanged
            && let Some(old) = old
            && file.revision.is_some()
            && old.revision == file.revision
            && reusable_parser_version(old.parse_version)
            && old.state == LocalFileState::Accepted
            && old.current_output
        {
            let input = self.input().await?;
            self.observe_lyrics_dependencies(catalog, &old.dependencies)
                .await?;
            for paths in old.dependencies.chunks(128) {
                let mut current = Vec::with_capacity(paths.len());
                for path in paths {
                    if crate::file::lyrics::is_sidecar(path) {
                        continue;
                    }
                    match self.stat(&input, &self.relative(path)?).await {
                        Ok(backing) => current.push((backing, vec![])),
                        Err(SourceError::Cancelled) => return Err(SourceError::Cancelled),
                        Err(error) => {
                            tracing::warn!(%error, "could not read CUE backing facts");
                            file.state = LocalFileState::Unreadable;
                            check_cancelled(cancelled)?;
                            catalog
                                .write_file_observations(
                                    &[(file, old.dependencies.clone())],
                                    Vec::new(),
                                    true,
                                )
                                .await?;
                            return Ok(0);
                        }
                    }
                }
                check_cancelled(cancelled)?;
                catalog
                    .write_file_observations(&current, Vec::new(), true)
                    .await?;
            }
            if database
                .source_cue_dependencies_unchanged(source_id, &file.path)
                .await?
            {
                file.state = LocalFileState::Accepted;
                file.parse_version = old.parse_version;
                check_cancelled(cancelled)?;
                catalog
                    .write_file_observations(&[(file, old.dependencies.clone())], Vec::new(), true)
                    .await?;
                let completed = database
                    .source_file_track_count(source_id, &old.path)
                    .await?;
                progress(completed);
                return Ok(completed);
            }
        }
        file.parse_version = Some(PARSER_VERSION);
        let relative = self.relative(&file.path)?;
        let mut dependencies = Vec::new();
        let mut lyrics_read_failed = false;
        let parsed: SourceResult<bool> = async {
            let input = self.input().await?;
            let text = self.small_file(&input, &relative, 1024 * 1024).await?;
            let Some(sheet) = std::str::from_utf8(&text).ok().and_then(parse_cue_sheet) else {
                return Ok(false);
            };
            let mut identity = old.or_else(|| renamed.first());
            if identity.is_none()
                && let Some(id) = &file.native_id
            {
                let mut matches = previous.iter().filter(|old| {
                    old.native_id.as_ref() == Some(id) && old.track_object_id.is_some()
                });
                let first = matches.next();
                if matches.next().is_none()
                    && database
                        .source_file_native_paths(source_id, id, stamp)
                        .await?
                        == [file.path.clone()]
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
            for entry in &sheet.files {
                check_cancelled(cancelled)?;
                let referenced = super::referenced_path(
                    crate::file::documents::relative_parts(&relative).0,
                    &entry.path,
                )?;
                let mut backing_file = match self.stat(&input, &referenced).await {
                    Ok(file) => file,
                    Err(error) => {
                        failure = Some(error);
                        continue;
                    }
                };
                dependencies.push(backing_file.path.clone());
                backing_file.parse_version = Some(PARSER_VERSION);
                let mut backing = match self
                    .read_audio(database, &backing_file, Arc::clone(&worker))
                    .await
                {
                    Ok(media::MediaRead::Accepted(backing)) => backing,
                    result => {
                        failure = Some(result.err().unwrap_or_else(|| {
                            SourceError::Other("Could not read CUE backing audio".into())
                        }));
                        backing_file.state = LocalFileState::Unreadable;

                        check_cancelled(cancelled)?;
                        catalog
                            .write_file_observations(&[(backing_file, vec![])], Vec::new(), true)
                            .await?;
                        continue;
                    }
                };
                backing.observe_images(&mut backing_file)?;
                self.locate_track(&mut backing, &backing_file);
                let duration = backing.track.metadata.duration_millis as u64;
                for (chunk, cues) in entry.tracks.chunks(128).enumerate() {
                    let mut tracks = Vec::with_capacity(cues.len());
                    let parsed: SourceResult<bool> = async {
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
                            if let Err(error) = self
                                .read_track_lyrics(database, &referenced, &mut track)
                                .await
                            {
                                tracing::warn!(%error, "Could not read CUE track lyrics");
                                track.lyrics = None;
                                lyrics_read_failed = true;
                            }
                            for path in &track.lyrics_paths {
                                if !dependencies.contains(path) {
                                    dependencies.push(path.clone());
                                }
                            }
                            tracks.push(track.into_observation(media::accepted_at()));
                        }
                        Ok(true)
                    }
                    .await;
                    self.observe_lyrics_dependencies(catalog, &dependencies)
                        .await?;
                    check_cancelled(cancelled)?;
                    let count = tracks.len();
                    catalog.write_file_observations(&[], tracks, true).await?;

                    completed += count;
                    progress(completed);
                    if !parsed? {
                        return Ok(false);
                    }
                }
                backing_file.state = LocalFileState::Accepted;
                check_cancelled(cancelled)?;
                catalog
                    .write_file_observations(&[(backing_file, vec![])], Vec::new(), true)
                    .await?;
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

                if let Some(old) = old.or_else(|| renamed.first()) {
                    for path in &old.dependencies {
                        if !dependencies.contains(path) {
                            dependencies.push(path.clone());
                        }
                    }
                }
                LocalFileState::Unreadable
            }
        };
        if lyrics_read_failed {
            file.parse_version = None;
        }
        check_cancelled(cancelled)?;
        catalog
            .write_file_observations(&[(file, dependencies)], Vec::new(), true)
            .await?;
        Ok(completed)
    }
}
