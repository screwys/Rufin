use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{StreamExt, stream};
use library::{LocalFileKind, LocalFileState, LocalFileWrite};

use super::FileSource;
use super::reader::FileReader;
use crate::file::remote::input::{FileInput, FileInputServer};
use crate::file::{check_cancelled, media};
use crate::{LocalImageRef, SourceError, SourceReadProgress, SourceReadStage, SourceResult};

pub(crate) const PARSER_VERSION: i64 = 9;

pub(crate) fn reusable_parser_version(version: Option<i64>) -> bool {
    // Earlier accepted metadata remains usable; version 9 reconsiders negative audio discovery.
    matches!(version, Some(7..=9))
}

pub(crate) fn audio_revision(
    path: &str,
    revision: Option<&str>,
    size: Option<i64>,
) -> blake3::Hasher {
    let mut hash = blake3::Hasher::new();
    hash.update(path.as_bytes());
    hash.update(revision.unwrap_or_default().as_bytes());
    hash.update(&size.unwrap_or_default().to_le_bytes());
    hash.update(&PARSER_VERSION.to_le_bytes());
    hash
}

impl FileSource {
    pub(crate) async fn stat(
        &self,
        input: &FileInputServer,
        relative: &str,
    ) -> SourceResult<LocalFileWrite> {
        match input.input() {
            FileInput::Peer { .. } => {
                return Err(SourceError::InvalidRequest(
                    "Peer access provides audio bytes",
                ));
            }
            FileInput::Documents(access) => {
                let access = Arc::clone(access);
                let path = relative.to_owned();
                let (path, entry) = tokio::task::spawn_blocking(move || access.resolve_path(&path))
                    .await
                    .map_err(|error| SourceError::Other(error.to_string()))??;
                self.document_observation(&path, entry)
            }
            FileInput::Smb(client) => {
                let entry = client.stat(relative).await?;
                self.observation(
                    relative,
                    entry.directory,
                    Some(entry.size),
                    Some(entry.revision),
                    entry.native_id,
                )
            }
            FileInput::WebDav(client) => {
                let url = url::Url::parse(&self.input_path(input.input(), relative)?)
                    .map_err(|_| SourceError::NotFound)?;
                let entry = client.stat(&url).await?;
                let revision = entry.revision();
                self.observation(
                    relative,
                    entry.directory,
                    entry.size,
                    revision,
                    entry.native_id,
                )
            }
        }
    }

    pub(crate) async fn read_saved_file(
        &self,
        media_uri: &str,
        relative: &str,
        input: &Arc<FileInputServer>,
        temporary: &std::path::Path,
    ) -> SourceResult<(LocalFileWrite, library::TrackObservation)> {
        let (_, _, object_id) =
            library::source_entity_parts(media_uri).ok_or(SourceError::NotFound)?;
        let mut observation = self.stat(input, relative).await?;
        let display_path = if self.document_access().is_some() {
            crate::file::documents::display_path(relative)
        } else {
            relative.to_owned()
        };
        let mut track = media::read_saved_media(temporary, display_path.into()).await?;
        track.observe_images(&mut observation)?;
        observation.state = LocalFileState::Accepted;
        track.local_artwork = track
            .picture_index
            .and_then(|index| u32::try_from(index).ok())
            .map(|picture_index| LocalImageRef::Embedded {
                source_id: self.source_id.clone(),
                path: observation.path.clone(),
                revision: observation.revision.clone().unwrap_or_default(),
                picture_index,
            });
        track.track.object_id = object_id;
        self.locate_track(&mut track, &observation);
        Ok((observation, track.into_observation(media::accepted_at())))
    }

    pub(crate) fn locate_track(&self, track: &mut media::ScannedTrack, file: &LocalFileWrite) {
        track.track.source_path = Some(file.path.clone());
        track.track.media_uri = None;
        track.audio_revision =
            audio_revision(&file.path, file.revision.as_deref(), file.size_bytes);
        if let Some(artwork) = &mut track.local_artwork {
            match artwork {
                LocalImageRef::File {
                    source_id,
                    path,
                    revision,
                    ..
                }
                | LocalImageRef::Embedded {
                    source_id,
                    path,
                    revision,
                    ..
                } => {
                    *source_id = self.source_id.clone();
                    *path = file.path.clone();
                    *revision = file.revision.clone().unwrap_or_default();
                }
            }
        }
    }
    pub(crate) async fn acquire_catalog(
        &self,
        catalog: &mut library::Catalog<'_>,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<usize> {
        let database = catalog.database;
        let source_id = catalog.source_id;
        let stamp = catalog.observed;
        let input = self.input().await?;
        let root_scope = self.location("")?;
        progress(SourceReadProgress {
            stage: SourceReadStage::Tracks,
            completed: 0,
            total: None,
        });
        for folder in self.folders() {
            if self.excludes(&folder) {
                continue;
            }
            let root = self.stat(&input, &folder).await?;
            if root.kind != LocalFileKind::Directory {
                return Err(SourceError::InvalidConfig(
                    "Selected file source path is not a directory".into(),
                ));
            }
            check_cancelled(cancelled)?;
            catalog
                .write_file_observations(&[(root, vec![])], Vec::new(), true)
                .await?;
        }
        let workers: [_; 4] =
            std::array::from_fn(|_| Arc::new(Mutex::new(media::Worker::default())));
        let mut completed = 0;
        let mut after = None;
        loop {
            check_cancelled(cancelled)?;
            let Some(directory) = database
                .source_next_directory(source_id, stamp, after.as_deref(), &root_scope)
                .await?
            else {
                break;
            };
            let report = |mut update: SourceReadProgress| {
                if update.stage == SourceReadStage::Tracks {
                    update.completed += completed;
                }
                progress(update);
            };
            completed += self
                .acquire_directory(
                    catalog,
                    &input,
                    &directory,
                    Arc::clone(&workers[0]),
                    &report,
                    cancelled,
                    None,
                )
                .await?;
            after = Some(directory);
        }
        completed = self
            .acquire_media_files(catalog, &workers, progress, cancelled, None, completed)
            .await?;
        if self.document_access().is_none() {
            crate::file::artwork::ArtworkFiles::Remote(self)
                .acquire(catalog, cancelled)
                .await?;
        }
        Ok(completed)
    }

    pub(super) async fn acquire_directory(
        &self,
        catalog: &mut library::Catalog<'_>,
        input: &Arc<FileInputServer>,
        directory: &str,
        worker: Arc<Mutex<media::Worker>>,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
        rename: Option<&(String, String)>,
    ) -> SourceResult<usize> {
        let database = catalog.database;
        let source_id = catalog.source_id;
        let unchanged = if matches!(input.input(), FileInput::WebDav(client) if client.recursive_etags())
        {
            let mut observation = database
                .source_files(source_id, &[directory.to_owned()])
                .await?
                .pop()
                .ok_or(SourceError::NotFound)?;
            observation.parse_version = Some(self.directory_version);
            catalog.observe_cached_file_tree(&observation).await?
        } else {
            false
        };
        if unchanged
            || self
                .retain_synced_directory(catalog, input, directory, cancelled)
                .await?
        {
            self.acquire_cues(
                catalog,
                Some(directory),
                worker,
                &|completed| {
                    progress(SourceReadProgress {
                        stage: SourceReadStage::Tracks,
                        completed,
                        total: None,
                    })
                },
                cancelled,
                rename,
            )
            .await
        } else {
            self.list_directory(
                catalog,
                input,
                &self.relative(directory)?,
                worker,
                progress,
                cancelled,
                rename,
            )
            .await
        }
    }

    pub(super) async fn acquire_media_files(
        &self,
        catalog: &mut library::Catalog<'_>,
        workers: &[Arc<Mutex<media::Worker>>; 4],
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
        rename: Option<&(String, String)>,
        mut completed: usize,
    ) -> SourceResult<usize> {
        let database = catalog.database;
        let source_id = catalog.source_id;
        let stamp = catalog.observed;
        let root = self.location("")?;
        let report = |completed| {
            progress(SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed,
                total: None,
            })
        };
        report(completed);
        let mut after = None;
        loop {
            check_cancelled(cancelled)?;
            let observations = database
                .source_file_page(
                    source_id,
                    LocalFileKind::Media,
                    stamp,
                    after.as_deref(),
                    None,
                    Some(&root),
                )
                .await?;
            if observations.is_empty() {
                break;
            }
            after = observations.last().map(|file| file.path.clone());
            let candidates = database
                .source_file_reuse_candidates(source_id, &observations)
                .await?;
            let paths = observations
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>();
            let dependencies = database
                .source_dependency_paths(source_id, &paths, stamp, true)
                .await?;
            let files = observations
                .into_iter()
                .map(|file| {
                    let reusable = candidates.iter().find(|old| {
                        old.path == file.path
                            && file.revision.is_some()
                            && old.revision == file.revision
                            && reusable_parser_version(old.parse_version)
                            && (old.state == LocalFileState::Rejected
                                && old.parse_version == Some(PARSER_VERSION)
                                || old.state == LocalFileState::Accepted && old.current_output)
                    });
                    (file, reusable.map(|old| (old.state, old.parse_version)))
                })
                .collect::<Vec<_>>();
            let mut files = stream::iter(files.into_iter().enumerate().map(
                |(index, (file, reusable))| {
                    let dependency = dependencies.contains(&file.path);
                    let worker = Arc::clone(&workers[index % workers.len()]);
                    async move {
                        let parsed = if dependency || reusable.is_some() || cancelled() {
                            None
                        } else {
                            Some(self.read_audio(&file, worker).await)
                        };
                        (file, reusable, parsed)
                    }
                },
            ))
            .buffered(workers.len());
            let mut accepted = Vec::new();
            let mut observations = Vec::new();
            let mut reported = Instant::now();
            let result: SourceResult<()> = async {
                while let Some((mut file, reusable, parsed)) = files.next().await {
                    check_cancelled(cancelled)?;
                    if dependencies.contains(&file.path) {
                        continue;
                    }
                    if let Some((state, version)) = reusable {
                        file.state = state;
                        file.parse_version = version;
                        observations.push((file, vec![]));
                        if state == LocalFileState::Accepted {
                            completed += 1;
                            if reported.elapsed() >= Duration::from_millis(100) {
                                report(completed);
                                reported = Instant::now();
                            }
                        }
                        continue;
                    }
                    file.parse_version = Some(PARSER_VERSION);
                    let exact = candidates
                        .iter()
                        .find(|old| old.path == file.path && old.track_object_id.is_some());
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
                    let mut prior = exact.or_else(|| renamed.first());
                    if prior.is_none()
                        && let Some(id) = &file.native_id
                    {
                        let mut matches = candidates.iter().filter(|old| {
                            old.native_id.as_ref() == Some(id) && old.track_object_id.is_some()
                        });
                        let first = matches.next();
                        if matches.next().is_none()
                            && database
                                .source_file_native_paths(source_id, id, stamp)
                                .await?
                                == [file.path.clone()]
                        {
                            prior = first;
                        }
                    }
                    let parsed = match parsed.expect("uncached media is read before staging") {
                        Ok(parsed) => parsed,
                        Err(SourceError::Cancelled) => return Err(SourceError::Cancelled),
                        Err(error) => {
                            tracing::warn!(error = %error, "could not read remote media file");
                            media::MediaRead::Unreadable
                        }
                    };
                    match parsed {
                        media::MediaRead::Accepted(mut track) => {
                            if let Some(id) = prior.and_then(|old| old.track_object_id.as_ref()) {
                                track.track.object_id = id.clone();
                            } else {
                                track.track.object_id = format!(
                                    "file:{:016x}",
                                    crate::policy::stable_hash(&super::file_identity(&file.path))
                                );
                            }
                            track.observe_images(&mut file)?;
                            self.locate_track(&mut track, &file);
                            file.state = LocalFileState::Accepted;
                            accepted.push(track.into_observation(media::accepted_at()));
                            observations.push((file, vec![]));
                            completed += 1;
                            if reported.elapsed() >= Duration::from_millis(100) {
                                report(completed);
                                reported = Instant::now();
                            }
                        }
                        media::MediaRead::Rejected => {
                            file.state = LocalFileState::Rejected;
                            observations.push((file, vec![]));
                        }
                        media::MediaRead::Unreadable => {
                            file.state = LocalFileState::Unreadable;

                            observations.push((file, vec![]));
                        }
                    }
                }
                Ok(())
            }
            .await;
            if !observations.is_empty() {
                check_cancelled(cancelled)?;
                catalog
                    .write_file_observations(&observations, accepted, true)
                    .await?;
                report(completed);
            }
            result?;
        }
        Ok(completed)
    }

    async fn retain_synced_directory(
        &self,
        catalog: &mut library::Catalog<'_>,
        input: &FileInputServer,
        path: &str,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<bool> {
        let database = catalog.database;
        let source_id = catalog.source_id;
        let FileInput::WebDav(client) = input.input() else {
            return Ok(false);
        };
        let Some(mut file) = database
            .source_files(source_id, &[path.into()])
            .await?
            .pop()
        else {
            return Ok(false);
        };
        let previous = database
            .source_file_reuse_candidates(source_id, std::slice::from_ref(&file))
            .await?;
        let Some(token) = previous
            .iter()
            .find(|old| {
                old.path == path
                    && old.state == LocalFileState::Accepted
                    && old.parse_version == Some(self.directory_version)
            })
            .and_then(|old| old.revision.as_deref())
            .and_then(super::webdav::dav::sync_token)
        else {
            return Ok(false);
        };
        let url = url::Url::parse(&self.input_path(input.input(), &self.relative(path)?)?)
            .map_err(|_| SourceError::NotFound)?;
        let mut changed = false;
        let result = client
            .sync(&url, &token, |entry| {
                let href = client.resolve_href(&url, &entry.href);
                if entry
                    .status
                    .is_some_and(|status| !(200..300).contains(&status))
                    || href.as_ref().is_ok_and(|href| {
                        href.path().trim_end_matches('/') != url.path().trim_end_matches('/')
                    })
                {
                    changed = true;
                }
                std::future::ready(href.map(|_| ()))
            })
            .await;
        match result {
            Ok(Some(_)) if !changed => {
                catalog.observe_file_children(path).await?;
                file.state = LocalFileState::Accepted;
                file.parse_version = Some(self.directory_version);
                check_cancelled(cancelled)?;
                catalog
                    .write_file_observations(&[(file, vec![])], Vec::new(), true)
                    .await?;
                Ok(true)
            }
            Ok(_)
            | Err(SourceError::Server {
                status: 403 | 405 | 409 | 410 | 501,
                ..
            }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn read_audio(
        &self,
        file: &LocalFileWrite,
        worker: Arc<Mutex<media::Worker>>,
    ) -> SourceResult<media::MediaRead> {
        let relative = self.relative(&file.path)?;
        if media::excluded_from_audio_scan(std::path::Path::new(&file.relative_path)) {
            return Ok(media::MediaRead::Rejected);
        }
        let input = self.input().await?;
        let path = self.input_path(input.input(), &relative)?;
        let mut reader = FileReader::open(
            &input,
            &path,
            file.revision.as_deref().unwrap_or_default(),
            &file.path,
        )
        .await?;
        let uri = reader.uri().to_owned();
        let source_id = self.source_id.clone();
        let artwork_path = file.path.clone();
        let revision = file.revision.clone().unwrap_or_default();
        let display_path = file.relative_path.clone();
        tokio::task::spawn_blocking(move || {
            let mut worker = worker.lock().unwrap_or_else(|p| p.into_inner());
            let mut parsed =
                media::read_media_input(&mut worker, display_path.into(), &mut reader, &uri, None);
            if let media::MediaRead::Accepted(track) = &mut parsed {
                track.local_artwork = track
                    .picture_index
                    .and_then(|index| u32::try_from(index).ok())
                    .map(|picture_index| LocalImageRef::Embedded {
                        source_id,
                        path: artwork_path,
                        picture_index,
                        revision,
                    });
            }
            if reader.failed() {
                media::MediaRead::Unreadable
            } else {
                parsed
            }
        })
        .await
        .map_err(|e| SourceError::Other(e.to_string()))
    }

    pub(crate) async fn list_directory(
        &self,
        catalog: &mut library::Catalog<'_>,
        input: &Arc<FileInputServer>,
        relative: &str,
        worker: Arc<Mutex<media::Worker>>,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
        rename: Option<&(String, String)>,
    ) -> SourceResult<usize> {
        let database = catalog.database;
        let source_id = catalog.source_id;
        let (send, receive) = async_channel::bounded(128);
        let listing = async {
            match input.input() {
                FileInput::Peer { .. } => {
                    return Err(SourceError::InvalidRequest(
                        "Peer access provides audio bytes",
                    ));
                }
                FileInput::Documents(access) => {
                    let access = Arc::clone(access);
                    let path = relative.to_owned();
                    let parent = tokio::task::spawn_blocking(move || access.resolve(&path))
                        .await
                        .map_err(|error| SourceError::Other(error.to_string()))??;
                    let mut offset = 0;
                    loop {
                        let uri = parent.uri.clone();
                        let page = tokio::task::spawn_blocking(move || {
                            crate::list_documents(&uri, offset)
                        })
                        .await
                        .map_err(|error| SourceError::Other(error.to_string()))??;
                        for entry in page.entries {
                            let parent = crate::file::documents::relative_parts(relative)
                                .0
                                .trim_end_matches('/');
                            let path = if parent.is_empty() {
                                entry.name.clone()
                            } else {
                                format!("{parent}/{}", entry.name)
                            };
                            send.send(self.document_observation(&path, entry)?)
                                .await
                                .map_err(|_| SourceError::Cancelled)?;
                        }
                        match page.next {
                            Some(next) => offset = next,
                            None => break,
                        }
                    }
                    Ok(())
                }
                FileInput::Smb(client) => {
                    client
                        .list(relative, move |entry: crate::file::remote::smb::Entry| {
                            let send = send.clone();
                            async move {
                                if self.excludes(&entry.path)
                                    || super::metadata::is_write_temporary(&entry.path)
                                {
                                    return Ok(());
                                }
                                send.send(self.observation(
                                    &entry.path,
                                    entry.directory,
                                    Some(entry.size),
                                    Some(entry.revision),
                                    entry.native_id,
                                )?)
                                .await
                                .map_err(|_| SourceError::Cancelled)
                            }
                        })
                        .await
                }
                FileInput::WebDav(client) => {
                    let mut collection =
                        url::Url::parse(&self.input_path(input.input(), relative)?)
                            .map_err(|_| SourceError::NotFound)?;
                    if !collection.path().ends_with('/') {
                        collection
                            .path_segments_mut()
                            .map_err(|_| SourceError::NotFound)?
                            .push("");
                    }
                    let collection_path = collection.path().trim_end_matches('/').to_string();
                    client
                        .list(
                            &collection.clone(),
                            1,
                            move |entry: crate::file::remote::webdav::dav::Entry| {
                                let send = send.clone();
                                let collection = collection.clone();
                                let collection_path = collection_path.clone();
                                async move {
                                    let url = client.resolve_href(&collection, &entry.href)?;
                                    if url.path().trim_end_matches('/') == collection_path {
                                        return Ok(());
                                    }
                                    let encoded = url
                                        .path()
                                        .strip_prefix(client.root().path())
                                        .ok_or(SourceError::NotFound)?;
                                    let relative = percent_encoding::percent_decode_str(encoded)
                                        .decode_utf8()
                                        .map_err(|_| SourceError::NotFound)?;
                                    if self.excludes(relative.trim_end_matches('/'))
                                        || super::metadata::is_write_temporary(&relative)
                                    {
                                        return Ok(());
                                    }
                                    let revision = entry.revision();
                                    send.send(self.observation(
                                        relative.trim_end_matches('/'),
                                        entry.directory,
                                        entry.size,
                                        revision,
                                        entry.native_id,
                                    )?)
                                    .await
                                    .map_err(|_| SourceError::Cancelled)
                                }
                            },
                        )
                        .await
                }
            }
        };
        let staging = async {
            let mut batch = Vec::with_capacity(128);
            let mut completed = 0;
            while let Ok(file) = receive.recv().await {
                check_cancelled(cancelled)?;
                if file.kind == LocalFileKind::Cue {
                    let previous = database
                        .source_file_reuse_candidates(source_id, std::slice::from_ref(&file))
                        .await?;
                    catalog
                        .write_file_observations(&[(file.clone(), vec![])], Vec::new(), true)
                        .await?;
                    completed += self
                        .acquire_cue_file(
                            catalog,
                            file,
                            &previous,
                            Arc::clone(&worker),
                            &|count| {
                                progress(SourceReadProgress {
                                    stage: SourceReadStage::Tracks,
                                    completed: completed + count,
                                    total: None,
                                })
                            },
                            cancelled,
                            rename,
                            true,
                        )
                        .await?;
                    continue;
                }
                batch.push((file, vec![]));
                if batch.len() == 128 {
                    check_cancelled(cancelled)?;
                    catalog
                        .write_file_observations(&batch, Vec::new(), true)
                        .await?;
                    batch.clear();
                    progress(SourceReadProgress {
                        stage: SourceReadStage::Tracks,
                        completed,
                        total: None,
                    });
                }
            }
            if !batch.is_empty() {
                check_cancelled(cancelled)?;
                catalog
                    .write_file_observations(&batch, Vec::new(), true)
                    .await?;
            }
            progress(SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed,
                total: None,
            });
            Ok::<_, SourceError>(completed)
        };
        let (_, completed) = tokio::try_join!(listing, staging)?;
        if let Some(mut directory) = database
            .source_files(source_id, &[self.location(relative)?])
            .await?
            .pop()
        {
            directory.state = LocalFileState::Accepted;
            directory.parse_version = Some(self.directory_version);
            check_cancelled(cancelled)?;
            catalog
                .write_file_observations(&[(directory, vec![])], Vec::new(), true)
                .await?;
        }
        Ok(completed)
    }

    pub(crate) fn document_observation(
        &self,
        relative: &str,
        entry: crate::DocumentEntry,
    ) -> SourceResult<LocalFileWrite> {
        let relative = if entry.directory
            && !relative.is_empty()
            && !relative
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .contains('\u{1f}')
        {
            format!(
                "{relative}\u{1f}{}",
                percent_encoding::utf8_percent_encode(
                    &entry.uri,
                    percent_encoding::NON_ALPHANUMERIC
                )
            )
        } else {
            relative.to_owned()
        };
        let relative = if entry.directory {
            relative
        } else {
            crate::file::documents::document_relative(&relative, &entry.native_id, &entry.uri)
        };
        let mut file = self.observation(
            &relative,
            entry.directory,
            entry.size,
            entry.revision,
            Some(entry.native_id),
        )?;
        file.relative_path = crate::file::documents::display_path(&relative);
        Ok(file)
    }

    pub(crate) fn observation(
        &self,
        relative: &str,
        directory: bool,
        size: Option<u64>,
        revision: Option<String>,
        native_id: Option<String>,
    ) -> SourceResult<LocalFileWrite> {
        let display_path = crate::file::documents::relative_parts(relative).0;
        let extension = display_path
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let kind = if directory {
            LocalFileKind::Directory
        } else if crate::file::artwork::supported_image(std::path::Path::new(display_path)) {
            LocalFileKind::Image
        } else {
            match extension.as_str() {
                "cue" => LocalFileKind::Cue,
                _ => LocalFileKind::Media,
            }
        };
        Ok(LocalFileWrite {
            path: self.location(relative)?,
            root: self.location("")?,
            relative_path: relative.into(),
            kind,
            size_bytes: size.and_then(|s| s.try_into().ok()),
            mtime_ns: 0,
            device_id: None,
            inode: None,
            native_id,
            revision,
            picture_index: None,
            artist_pictures: None,
            parse_version: Some(if directory {
                self.directory_version
            } else {
                PARSER_VERSION
            }),
            state: LocalFileState::Observed,
        })
    }
}
