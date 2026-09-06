use std::sync::{Arc, Mutex};

use futures_util::{StreamExt, stream};
use library::{Database, LocalFileKind, LocalFileState, LocalFileWrite, ReadCancellation, Scan};

use super::RemoteSource;
use super::reader::FileReader;
use crate::file::remote::input::{FileInput, FileInputServer};
use crate::file::{media, scan::stage_audio_tracks_batch};
use crate::{LocalImageRef, SourceError, SourceReadProgress, SourceReadStage, SourceResult};

pub(crate) const PARSER_VERSION: i64 = 1;

impl RemoteSource {
    pub(crate) async fn stat(
        &self,
        input: &FileInputServer,
        relative: &str,
    ) -> SourceResult<LocalFileWrite> {
        match input.input() {
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

    pub(crate) async fn publish_saved_file(
        &self,
        database: &Database,
        media_uri: &str,
        copy: &super::metadata::WorkingFile,
    ) -> SourceResult<library::ScanOutcome> {
        let mut scan = Scan::begin_items(database, self.source_id.as_str()).await?;
        self.stage_saved_file(&mut scan, media_uri, copy).await?;
        self.stage_artwork(database, &mut scan, &|| false).await?;
        scan.finish().await.map_err(Into::into)
    }

    pub(crate) async fn stage_saved_file(
        &self,
        scan: &mut Scan,
        media_uri: &str,
        copy: &super::metadata::WorkingFile,
    ) -> SourceResult<()> {
        let (_, _, object_id) =
            library::source_entity_parts(media_uri).ok_or(SourceError::NotFound)?;
        let mut observation = self.stat(&copy.input, &copy.relative).await?;
        let mut file =
            std::fs::File::open(&copy.file).map_err(|e| SourceError::Other(e.to_string()))?;
        let uri = url::Url::from_file_path(&copy.file).map_err(|_| SourceError::NotFound)?;
        let relative = copy.relative.clone();
        let parsed = tokio::task::spawn_blocking(move || {
            let parsed = media::read_media_input(
                &mut media::Worker::network(),
                relative.into(),
                &mut file,
                uri.as_str(),
                None,
            );
            let picture = crate::file::artwork::inspect_embedded_input(
                &mut crate::file::discovery::Reader::network(),
                &mut file,
                uri.as_str(),
            );
            (parsed, picture)
        })
        .await
        .map_err(|e| SourceError::Other(e.to_string()))?;
        let (media::MediaRead::Accepted(mut track), picture) = parsed else {
            return Err(SourceError::Other("Saved media could not be read".into()));
        };
        observation.picture_index = picture.map(i64::from);
        track.local_artwork = picture.map(|picture_index| LocalImageRef::Embedded {
            source_id: self.source_id.clone(),
            path: observation.path.clone(),
            revision: observation.revision.clone().unwrap_or_default(),
            picture_index,
        });
        track.id = object_id;
        self.locate_track(&mut track, &observation);
        observation.state = LocalFileState::Accepted;
        scan.begin_batch().await?;
        stage_audio_tracks_batch(scan, &[*track]).await?;
        scan.write_local_files(&[(observation, vec![])]).await?;
        scan.finish_batch().await?;
        Ok(())
    }

    pub(crate) fn locate_track(&self, track: &mut media::ScannedTrack, file: &LocalFileWrite) {
        track.source_path = file.path.clone();
        track.local_uri = None;
        let mut revision = blake3::Hasher::new();
        revision.update(file.path.as_bytes());
        revision.update(file.revision.as_deref().unwrap_or_default().as_bytes());
        revision.update(&file.size_bytes.unwrap_or_default().to_le_bytes());
        revision.update(&PARSER_VERSION.to_le_bytes());
        track.audio_revision = revision;
        if let Some(artwork) = &mut track.local_artwork {
            match artwork {
                LocalImageRef::File {
                    source_id,
                    path,
                    revision,
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
    pub(crate) async fn stage_catalog(
        &self,
        database: &Database,
        scan: &mut Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let input = self.input().await?;
        progress(SourceReadProgress {
            stage: SourceReadStage::Files,
            completed: 0,
            total: None,
        });
        for folder in if self.settings.folders.is_empty() {
            vec![String::new()]
        } else {
            self.settings.folders.clone()
        } {
            let root = self.stat(&input, &folder).await?;
            if root.kind != LocalFileKind::Directory {
                return Err(SourceError::InvalidConfig(
                    "Selected file source path is not a directory".into(),
                ));
            }
            scan.write_local_files(&[(root, vec![])]).await?;
        }
        let mut after = None;
        loop {
            if cancelled() {
                return Err(SourceError::Cancelled);
            }
            let page = scan
                .local_inventory_path_page(LocalFileKind::Directory, after.as_deref(), false, 1)
                .await?;
            let Some(directory) = page.into_iter().next() else {
                break;
            };
            let unchanged = if matches!(input.input(), FileInput::WebDav(client) if client.recursive_etags())
            {
                let observation = scan
                    .local_inventory_files(std::slice::from_ref(&directory))
                    .await?
                    .pop()
                    .ok_or(SourceError::NotFound)?;
                scan.retain_file_tree(&observation).await?
            } else {
                false
            };
            if !unchanged {
                if !self
                    .retain_synced_directory(database, &input, scan, &directory)
                    .await?
                {
                    self.list_directory(&input, scan, &self.relative(&directory)?, cancelled)
                        .await?;
                }
            }
            after = Some(directory);
        }
        self.stage_files(database, scan, progress, cancelled, None)
            .await?;
        progress(SourceReadProgress {
            stage: SourceReadStage::Finalizing,
            completed: 0,
            total: None,
        });
        self.stage_artwork(database, scan, cancelled).await
    }

    pub(crate) async fn stage_files(
        &self,
        database: &Database,
        scan: &mut Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
        rename: Option<&(String, String)>,
    ) -> SourceResult<()> {
        let report = |completed| {
            progress(SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed,
                total: None,
            })
        };
        report(0);
        let mut completed = self
            .stage_cues(database, scan, &report, cancelled, rename)
            .await?;
        let workers: [_; 4] =
            std::array::from_fn(|_| Arc::new(Mutex::new(media::Worker::network())));
        let mut after = None;
        loop {
            if cancelled() {
                return Err(SourceError::Cancelled);
            }
            let observations = scan
                .local_inventory_file_page(LocalFileKind::Media, after.as_deref())
                .await?;
            if observations.is_empty() {
                break;
            }
            after = observations.last().map(|file| file.path.clone());
            let candidates = if let Some(source) = scan.existing_source() {
                database
                    .local_file_reuse_candidates(source, &observations, &ReadCancellation::new())
                    .await?
            } else {
                vec![]
            };
            let dependencies = scan
                .local_dependency_paths(
                    &observations
                        .iter()
                        .map(|file| file.path.clone())
                        .collect::<Vec<_>>(),
                )
                .await?;
            let files = observations
                .into_iter()
                .map(|file| {
                    let reusable = candidates.iter().find(|old| {
                        old.path == file.path
                            && file.revision.is_some()
                            && old.revision == file.revision
                            && old.parse_version == Some(PARSER_VERSION)
                            && (old.state == LocalFileState::Rejected
                                || old.state == LocalFileState::Accepted
                                    && old.track_object_id.is_some())
                    });
                    (file, reusable.cloned())
                })
                .collect::<Vec<_>>();
            let retained = files
                .iter()
                .filter(|(file, old)| {
                    !dependencies.contains(&file.path)
                        && old
                            .as_ref()
                            .is_some_and(|old| old.state == LocalFileState::Accepted)
                })
                .map(|(file, _)| file.path.clone())
                .collect::<Vec<_>>();
            if !retained.is_empty() {
                scan.begin_batch().await?;
                scan.retain_local_media_paths(&retained).await?;
                scan.finish_batch().await?;
            }
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
            while let Some((mut file, reusable, parsed)) = files.next().await {
                if cancelled() {
                    return Err(SourceError::Cancelled);
                }
                if dependencies.contains(&file.path) {
                    if file.state == LocalFileState::Observed
                        && let Some(old) = candidates
                            .iter()
                            .find(|old| old.path == file.path && old.revision == file.revision)
                    {
                        file.state = old.state;
                        file.picture_index = old.picture_index;
                        scan.write_local_files(&[(file, old.dependencies.clone())])
                            .await?;
                    }
                    continue;
                }
                let exact = candidates.iter().find(|old| old.path == file.path);
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
                let mut prior = exact.or_else(|| renamed.first());
                if prior.is_none()
                    && let Some(id) = &file.native_id
                {
                    let mut matches = candidates
                        .iter()
                        .filter(|old| old.native_id.as_ref() == Some(id));
                    let first = matches.next();
                    if matches.next().is_none()
                        && scan.local_inventory_native_paths(id).await? == [file.path.clone()]
                    {
                        prior = first;
                    }
                }
                if let Some(old) = reusable {
                    file.state = old.state;
                    file.picture_index = old.picture_index;
                    scan.begin_batch().await?;
                    scan.write_local_files(&[(file, old.dependencies.clone())])
                        .await?;
                    scan.finish_batch().await?;
                } else {
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
                                track.id = id.clone();
                            } else {
                                track.id =
                                    format!("file:{:016x}", crate::policy::stable_hash(&file.path));
                            }
                            file.picture_index = match &track.local_artwork {
                                Some(LocalImageRef::Embedded { picture_index, .. }) => {
                                    Some(i64::from(*picture_index))
                                }
                                _ => None,
                            };
                            self.locate_track(&mut track, &file);
                            file.state = LocalFileState::Accepted;
                            scan.begin_batch().await?;
                            stage_audio_tracks_batch(scan, &[*track]).await?;
                            scan.write_local_files(&[(file, vec![])]).await?;
                            scan.finish_batch().await?;
                            completed += 1;
                            report(completed);
                        }
                        media::MediaRead::Rejected => {
                            file.state = LocalFileState::Rejected;
                            scan.write_local_files(&[(file, vec![])]).await?;
                        }
                        media::MediaRead::Unreadable => {
                            file.state = LocalFileState::Unreadable;
                            scan.incomplete();
                            scan.write_local_files(&[(file, vec![])]).await?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn retain_synced_directory(
        &self,
        database: &Database,
        input: &FileInputServer,
        scan: &mut Scan,
        path: &str,
    ) -> SourceResult<bool> {
        let FileInput::WebDav(client) = input.input() else {
            return Ok(false);
        };
        let Some(source) = scan.existing_source() else {
            return Ok(false);
        };
        let Some(file) = scan.local_inventory_files(&[path.into()]).await?.pop() else {
            return Ok(false);
        };
        let previous = database
            .local_file_reuse_candidates(source, &[file], &ReadCancellation::new())
            .await?;
        let Some(token) = previous
            .iter()
            .find(|old| old.path == path)
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
                scan.retain_file_directory_children(path).await?;
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
        let input = self.input().await?;
        let relative = self.relative(&file.path)?;
        let path = self.input_path(input.input(), &relative)?;
        let stream = input
            .playback_stream(
                &path,
                file.revision.as_deref().unwrap_or_default(),
                &file.path,
            )
            .await?;
        let uri = stream.uri().to_string();
        let mut reader = FileReader::open(stream).await?;
        let source_id = self.source_id.clone();
        let artwork_path = file.path.clone();
        let revision = file.revision.clone().unwrap_or_default();
        tokio::task::spawn_blocking(move || {
            let mut parsed = media::read_media_input(
                &mut worker.lock().unwrap_or_else(|p| p.into_inner()),
                relative.clone().into(),
                &mut reader,
                &uri,
                None,
            );
            if let media::MediaRead::Accepted(track) = &mut parsed {
                track.local_artwork = crate::file::artwork::inspect_embedded_input(
                    &mut crate::file::discovery::Reader::network(),
                    &mut reader,
                    &uri,
                )
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
        input: &Arc<FileInputServer>,
        scan: &mut Scan,
        relative: &str,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let (send, receive) = async_channel::bounded(128);
        let listing = async {
            match input.input() {
                FileInput::Smb(client) => {
                    client
                        .list(relative, move |entry: crate::file::remote::smb::Entry| {
                            let send = send.clone();
                            async move {
                                if super::metadata::is_write_temporary(&entry.path) {
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
                                    if super::metadata::is_write_temporary(&relative) {
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
            while let Ok(file) = receive.recv().await {
                if cancelled() {
                    return Err(SourceError::Cancelled);
                }
                batch.push((file, vec![]));
                if batch.len() == 128 {
                    scan.write_local_files(&batch).await?;
                    batch.clear();
                }
            }
            scan.write_local_files(&batch).await?;
            Ok::<_, SourceError>(())
        };
        tokio::try_join!(listing, staging)?;
        if let Some(mut directory) = scan
            .local_inventory_files(&[self.location(relative)?])
            .await?
            .pop()
        {
            directory.state = LocalFileState::Accepted;
            scan.write_local_files(&[(directory, vec![])]).await?;
        }
        Ok(())
    }

    pub(crate) fn observation(
        &self,
        relative: &str,
        directory: bool,
        size: Option<u64>,
        revision: Option<String>,
        native_id: Option<String>,
    ) -> SourceResult<LocalFileWrite> {
        let extension = relative
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let kind = if directory {
            LocalFileKind::Directory
        } else {
            match extension.as_str() {
                "cue" => LocalFileKind::Cue,
                "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" => LocalFileKind::Image,
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
            parse_version: Some(PARSER_VERSION),
            state: LocalFileState::Observed,
        })
    }
}
