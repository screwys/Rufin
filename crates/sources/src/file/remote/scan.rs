use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use library::{LocalFileKind, LocalFileState, LocalFileWrite};

use super::FileSource;
use super::reader::FileReader;
use crate::file::media;
use crate::file::remote::input::{FileInput, FileInputServer};
use crate::file::walk::{self, Entry, Root};
use crate::{LocalImageRef, SourceError, SourceResult};

pub(crate) const PARSER_VERSION: i64 = 11;

pub(crate) fn audio_revision(
    path: &str,
    revision: Option<&str>,
    size: Option<i64>,
) -> blake3::Hasher {
    let mut hash = blake3::Hasher::new();
    hash.update(path.as_bytes());
    hash.update(revision.unwrap_or_default().as_bytes());
    hash.update(&size.unwrap_or_default().to_le_bytes());
    // Lyrics indexing does not change the interpreted audio.
    hash.update(&9_i64.to_le_bytes());
    hash
}

impl FileSource {
    pub(crate) async fn stat(
        &self,
        input: &FileInputServer,
        relative: &str,
    ) -> SourceResult<LocalFileWrite> {
        match self.stat_entry(input, relative).await? {
            (_, Some((path, document))) => self.document_observation(&path, document),
            (entry, None) => self.observation(
                relative,
                entry.directory,
                entry.size.and_then(|size| u64::try_from(size).ok()),
                entry.revision,
                entry.native_id,
            ),
        }
    }

    /// The facts the protocol reports for one path, named by its last segment. A document
    /// also gives its resolved path and entry.
    async fn stat_entry(
        &self,
        input: &FileInputServer,
        relative: &str,
    ) -> SourceResult<(Entry, Option<(String, crate::DocumentEntry)>)> {
        let name = |path: &str| path.rsplit('/').next().unwrap_or_default().to_owned();
        let entry = match input.input() {
            FileInput::Link { .. } => {
                return Err(SourceError::InvalidRequest(
                    "Peer access provides audio bytes",
                ));
            }
            FileInput::Documents(access) => {
                let access = Arc::clone(access);
                let path = relative.to_owned();
                let (path, document) =
                    tokio::task::spawn_blocking(move || access.resolve_path(&path))
                        .await
                        .map_err(|error| SourceError::Other(error.to_string()))??;
                let entry = Entry {
                    name: name(&crate::file::documents::display_path(&path)),
                    directory: document.directory,
                    size: document.size.and_then(|size| i64::try_from(size).ok()),
                    native_id: Some(document.native_id.clone()),
                    revision: document.revision.clone(),
                    ..Entry::default()
                };
                return Ok((entry, Some((path, document))));
            }
            FileInput::Smb(client) => {
                let entry = client.stat(relative).await?;
                Entry {
                    name: name(relative),
                    directory: entry.directory,
                    size: i64::try_from(entry.size).ok(),
                    native_id: entry.native_id,
                    revision: Some(entry.revision),
                    ..Entry::default()
                }
            }
            FileInput::WebDav(client) => {
                let url = url::Url::parse(&self.input_path(input.input(), relative)?)
                    .map_err(|_| SourceError::NotFound)?;
                let entry = client.stat(&url).await?;
                Entry {
                    name: name(relative),
                    directory: entry.directory,
                    size: entry.size.and_then(|size| i64::try_from(size).ok()),
                    revision: entry.revision(),
                    native_id: entry.native_id,
                    ..Entry::default()
                }
            }
        };
        Ok((entry, None))
    }

    /// Walks the source, then reads the folder artwork of what changed.
    pub(crate) async fn walk(
        &self,
        observation: &mut library::Observation<'_>,
        scope: walk::WalkScope<'_>,
        progress: &(dyn Fn(crate::SourceProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        walk::walk(
            self,
            observation,
            &[self.location("")?],
            scope,
            &self.artwork,
            progress,
            cancelled,
        )
        .await
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
        self.read_track_lyrics(None, relative, &mut track).await?;
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
        track.revision_hashes_audio = true;
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
    /// Reads a media file; a failed lyrics read clears the row's parser version so the next
    /// walk reads it again.
    pub(crate) async fn read_audio(
        &self,
        file: &mut LocalFileWrite,
        worker: Arc<Mutex<media::Worker>>,
        siblings: Option<&BTreeSet<String>>,
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
        let mut parsed = tokio::task::spawn_blocking(move || {
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
        .map_err(|e| SourceError::Other(e.to_string()))?;
        if let media::MediaRead::Accepted(track) = &mut parsed {
            if let Err(error) = self.read_track_lyrics(siblings, &relative, track).await {
                tracing::warn!(%error, "Could not read file lyrics");
                track.lyrics = None;
                file.parse_version = None;
            }
        }
        Ok(parsed)
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
            state: LocalFileState::Accepted,
        })
    }
}

impl walk::FileSource for FileSource {
    fn roots(&self) -> SourceResult<Vec<Root>> {
        Ok(vec![Root {
            root: self.location("")?,
            starts: self
                .folders()
                .into_iter()
                .filter(|folder| !self.excludes(folder))
                .map(|folder| (folder.clone(), folder))
                .collect(),
        }])
    }

    fn excluded(&self, _root: &str, relative: &str) -> bool {
        self.excludes(relative) || super::metadata::is_write_temporary(relative)
    }

    fn artwork(&self) -> crate::file::artwork::ArtworkFiles<'_> {
        crate::file::artwork::ArtworkFiles::Remote(self)
    }

    fn row(
        &self,
        _root: &str,
        relative: &str,
        native: &str,
        entry: &Entry,
    ) -> SourceResult<LocalFileWrite> {
        let mut file = self.observation(
            native,
            entry.directory,
            entry.size.and_then(|size| u64::try_from(size).ok()),
            entry.revision.clone(),
            entry.native_id.clone(),
        )?;
        relative.clone_into(&mut file.relative_path);
        Ok(file)
    }

    fn place(&self, path: &str) -> Option<(String, String, String)> {
        let native = match self.relative(path) {
            Ok(native) => native,
            Err(_) if !path.contains("://") => path.trim_matches('/').to_owned(),
            Err(_) => return None,
        };
        let relative = crate::file::documents::display_path(&native);
        self.includes(&relative)
            .then(|| Some((self.location("").ok()?, relative, native)))
            .flatten()
    }

    /// One directory's entries, as the walk orders and locates them.
    async fn list(&self, _root: &str, native: &str) -> SourceResult<Vec<Entry>> {
        let input = self.input().await?;
        let mut entries = Vec::new();
        match input.input() {
            FileInput::Link { .. } => {
                return Err(SourceError::InvalidRequest(
                    "Peer access provides audio bytes",
                ));
            }
            FileInput::Documents(access) => {
                let access = Arc::clone(access);
                let path = native.to_owned();
                let parent = tokio::task::spawn_blocking(move || access.resolve(&path))
                    .await
                    .map_err(|error| SourceError::Other(error.to_string()))??;
                let mut offset = 0;
                loop {
                    let uri = parent.uri.clone();
                    let page =
                        tokio::task::spawn_blocking(move || crate::list_documents(&uri, offset))
                            .await
                            .map_err(|error| SourceError::Other(error.to_string()))??;
                    for entry in page.entries {
                        let native = if entry.directory {
                            format!(
                                "{}\u{1f}{}",
                                entry.name,
                                percent_encoding::utf8_percent_encode(
                                    &entry.uri,
                                    percent_encoding::NON_ALPHANUMERIC
                                )
                            )
                        } else {
                            crate::file::documents::document_relative(
                                &entry.name,
                                &entry.native_id,
                                &entry.uri,
                            )
                        };
                        entries.push(Entry {
                            name: entry.name,
                            native,
                            directory: entry.directory,
                            size: entry.size.and_then(|size| i64::try_from(size).ok()),
                            native_id: Some(entry.native_id),
                            revision: entry.revision,
                            ..Entry::default()
                        });
                    }
                    match page.next {
                        Some(next) => offset = next,
                        None => break,
                    }
                }
            }
            FileInput::Smb(client) => {
                let found = std::sync::Mutex::new(Vec::new());
                client
                    .list(native, |entry: crate::file::remote::smb::Entry| {
                        let name = entry.path.rsplit('/').next().unwrap_or_default().to_owned();
                        found
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .push(Entry {
                                native: name.clone(),
                                name,
                                directory: entry.directory,
                                size: (!entry.directory)
                                    .then(|| i64::try_from(entry.size).ok())
                                    .flatten(),
                                native_id: entry.native_id,
                                revision: Some(entry.revision),
                                ..Entry::default()
                            });
                        std::future::ready(Ok(()))
                    })
                    .await?;
                entries = found
                    .into_inner()
                    .unwrap_or_else(|error| error.into_inner());
            }
            FileInput::WebDav(client) => {
                let mut collection = url::Url::parse(&self.input_path(input.input(), native)?)
                    .map_err(|_| SourceError::NotFound)?;
                if !collection.path().ends_with('/') {
                    collection
                        .path_segments_mut()
                        .map_err(|_| SourceError::NotFound)?
                        .push("");
                }
                let own = collection.path().trim_end_matches('/').to_string();
                client
                    .list(
                        &collection,
                        1,
                        |entry: crate::file::remote::webdav::dav::Entry| {
                            let result = (|| {
                                let url = client.resolve_href(&collection, &entry.href)?;
                                if url.path().trim_end_matches('/') == own {
                                    return Ok(());
                                }
                                let encoded = url
                                    .path()
                                    .trim_end_matches('/')
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or_default();
                                let name = percent_encoding::percent_decode_str(encoded)
                                    .decode_utf8()
                                    .map_err(|_| SourceError::NotFound)?
                                    .into_owned();
                                entries.push(Entry {
                                    native: name.clone(),
                                    name,
                                    directory: entry.directory,
                                    size: entry.size.and_then(|size| i64::try_from(size).ok()),
                                    revision: entry.revision(),
                                    native_id: entry.native_id.clone(),
                                    ..Entry::default()
                                });
                                Ok(())
                            })();
                            std::future::ready(result)
                        },
                    )
                    .await?;
            }
        }
        Ok(entries)
    }

    /// A CUE sheet and the media files it plays.
    async fn read_cue(
        &self,
        file: &mut LocalFileWrite,
        _native: &str,
        worker: Arc<Mutex<media::Worker>>,
        siblings: Option<&BTreeSet<String>>,
    ) -> SourceResult<walk::CueRead> {
        let relative = self.relative(&file.path)?;
        let input = self.input().await?;
        let text = self.small_file(&input, &relative, 1024 * 1024).await?;
        let Some(sheet) = std::str::from_utf8(&text)
            .ok()
            .and_then(crate::file::cue::parse_cue_sheet)
        else {
            return Ok(walk::CueRead::Rejected);
        };
        let prefix = format!(
            "cue:{:016x}",
            crate::policy::stable_hash(&super::file_identity(&file.path))
        );
        let (mut inputs, mut tracks) = (Vec::new(), Vec::new());
        for entry in &sheet.files {
            let referenced = super::referenced_path(
                crate::file::documents::relative_parts(&relative).0,
                &entry.path,
            )?;
            let mut backing_file = match self.stat(&input, &referenced).await {
                Ok(file) => file,
                Err(SourceError::Cancelled) => return Err(SourceError::Cancelled),
                Err(error) => {
                    tracing::warn!(%error, "Could not inspect CUE backing audio");
                    return Ok(walk::CueRead::Unreadable);
                }
            };
            inputs.push(backing_file.path.clone());
            let mut backing = match self
                .read_audio(&mut backing_file, Arc::clone(&worker), siblings)
                .await?
            {
                media::MediaRead::Accepted(backing) => backing,
                _ => return Ok(walk::CueRead::Unreadable),
            };
            self.locate_track(&mut backing, &backing_file);
            let Some(segments) = crate::file::cue::cue_segments(
                entry,
                backing.track.metadata.duration_millis as u64,
            ) else {
                return Ok(walk::CueRead::Rejected);
            };
            for (cue, end) in segments {
                // The relative CUE name supplies Local's album/tag fallbacks; URLs never become native paths.
                let mut track = crate::file::cue::cue_track(
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
                    .read_track_lyrics(siblings, &referenced, &mut track)
                    .await
                {
                    tracing::warn!(%error, "Could not read CUE track lyrics");
                    track.lyrics = None;
                    file.parse_version = None;
                }
                tracks.push(track.into_observation(media::accepted_at()));
            }
        }
        Ok(walk::CueRead::Accepted { inputs, tracks })
    }

    async fn stat(&self, _root: &str, native: &str) -> SourceResult<Entry> {
        let input = self.input().await?;
        let (mut entry, _) = self.stat_entry(&input, native).await?;
        entry.native = native.rsplit('/').next().unwrap_or_default().to_owned();
        Ok(entry)
    }

    async fn read_media(
        &self,
        file: &mut LocalFileWrite,
        _native: &str,
        worker: Arc<Mutex<media::Worker>>,
        siblings: Option<&BTreeSet<String>>,
    ) -> SourceResult<media::MediaRead> {
        let mut read = self.read_audio(file, worker, siblings).await?;
        if let media::MediaRead::Accepted(track) = &mut read {
            track.track.object_id = format!(
                "file:{:016x}",
                crate::policy::stable_hash(&super::file_identity(&file.path))
            );
            self.locate_track(track, file);
        }
        Ok(read)
    }

    fn unchanged_subtree(&self, stored: &LocalFileWrite, listed: &LocalFileWrite) -> bool {
        stored.state == LocalFileState::Accepted
            && stored.revision.is_some()
            && stored.revision == listed.revision
            && stored.parse_version == listed.parse_version
            && self.input.try_lock().is_ok_and(|input| {
                input.as_ref().is_some_and(|input| {
                    matches!(input.input(), FileInput::WebDav(client) if client.recursive_etags())
                })
            })
    }
}
