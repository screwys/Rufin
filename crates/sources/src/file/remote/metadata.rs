use std::sync::Arc;

use futures_util::TryStreamExt;
use tokio::io::AsyncReadExt;

use super::RemoteSource;
use crate::file::remote::input::{FileInput, FileInputServer};
use crate::{SourceError, SourceMetadataError, SourceResult, TrackMetadata, TrackMetadataEdit};

/// One working copy retains the selected server through read, edit and replacement.
pub(crate) struct WorkingFile {
    pub file: tempfile::TempPath,
    pub relative: String,
    pub revision: Option<String>,
    pub existed: bool,
    pub input: Arc<FileInputServer>,
}

impl RemoteSource {
    pub(crate) async fn write_tags(
        &self,
        database: &library::Database,
        track: &library::TrackRow,
        write: impl FnOnce(&std::path::Path, Option<&str>) -> Result<(), SourceMetadataError>
        + Send
        + 'static,
    ) -> Result<library::ScanOutcome, SourceMetadataError> {
        let copy = self.working_file(database, track).await?;
        let format = track.source_format.clone();
        let copy = tokio::task::spawn_blocking(move || {
            write(&copy.file, format.as_deref())?;
            Ok::<_, SourceMetadataError>(copy)
        })
        .await
        .map_err(|e| SourceMetadataError::Write(e.to_string()))??;
        self.save_file(&copy).await?;
        self.publish_saved_file(database, &track.media_uri, &copy)
            .await
            .map_err(|e| SourceMetadataError::SavedRefreshFailed(e.to_string()))
    }

    pub(crate) async fn read_track_metadata(
        &self,
        database: &library::Database,
        track: &library::TrackRow,
    ) -> Result<TrackMetadata, SourceMetadataError> {
        let copy = self.working_file(database, track).await?;
        let format = track.source_format.clone();
        let (copy, mut metadata) = tokio::task::spawn_blocking(move || {
            let metadata =
                crate::file::metadata::read_track_metadata(&copy.file, format.as_deref())?;
            Ok::<_, SourceMetadataError>((copy, metadata))
        })
        .await
        .map_err(|e| SourceMetadataError::Write(e.to_string()))??;
        metadata.revision = copy.revision;
        Ok(metadata)
    }

    pub(crate) async fn write_track_metadata(
        &self,
        database: &library::Database,
        track: &library::TrackRow,
        expected_revision: &str,
        edit: &TrackMetadataEdit,
    ) -> Result<library::ScanOutcome, SourceMetadataError> {
        let copy = self.working_file(database, track).await?;
        if copy.revision.as_deref().unwrap_or_default() != expected_revision {
            return Err(SourceMetadataError::Conflict);
        }
        let format = track.source_format.clone();
        let edit = edit.clone();
        let copy = tokio::task::spawn_blocking(move || {
            let revision = crate::file::metadata::revision(&copy.file)?;
            crate::file::metadata::write_track(&copy.file, format.as_deref(), &revision, &edit)?;
            Ok::<_, SourceMetadataError>(copy)
        })
        .await
        .map_err(|e| SourceMetadataError::Write(e.to_string()))??;
        self.save_file(&copy).await?;
        self.publish_saved_file(database, &track.media_uri, &copy)
            .await
            .map_err(|error| SourceMetadataError::SavedRefreshFailed(error.to_string()))
    }

    pub(crate) async fn working_file(
        &self,
        database: &library::Database,
        track: &library::TrackRow,
    ) -> Result<WorkingFile, SourceMetadataError> {
        if track.cue_path.is_some()
            || library::source_entity_parts(&track.media_uri)
                .is_none_or(|(source, _, _)| source != self.source_id)
        {
            return Err(SourceMetadataError::Unavailable);
        }
        let observation = database
            .observed_media_file(&track.media_uri)
            .await
            .map_err(|e| SourceMetadataError::Write(e.to_string()))?
            .ok_or(SourceMetadataError::Unavailable)?;
        let relative = self.relative(&observation.path).map_err(metadata_error)?;
        self.working_copy(&relative, &track.media_uri).await
    }

    pub(super) async fn working_copy(
        &self,
        relative: &str,
        identity: &str,
    ) -> Result<WorkingFile, SourceMetadataError> {
        let input = self.input().await.map_err(metadata_error)?;
        let before = self.stat(&input, &relative).await.map_err(metadata_error)?;
        let file = tempfile::NamedTempFile::new()
            .map_err(|e| SourceMetadataError::Write(e.to_string()))?;
        let stream = input.stream(
            &self
                .input_path(input.input(), &relative)
                .map_err(metadata_error)?,
            identity,
        );
        let response = reqwest::Client::builder()
            .no_proxy()
            .read_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| SourceMetadataError::Write(e.to_string()))?
            .get(stream.uri())
            .send()
            .await
            .map_err(|e| SourceMetadataError::Write(e.without_url().to_string()))?;
        if !response.status().is_success() {
            return Err(SourceMetadataError::Write(format!(
                "File read failed with status {}",
                response.status()
            )));
        }
        let mut reader = tokio_util::io::StreamReader::new(
            response.bytes_stream().map_err(std::io::Error::other),
        );
        let mut output = tokio::fs::File::from_std(
            file.reopen()
                .map_err(|e| SourceMetadataError::Write(e.to_string()))?,
        );
        tokio::io::copy(&mut reader, &mut output)
            .await
            .map_err(|e| SourceMetadataError::Write(e.to_string()))?;
        drop(output);
        let after = self.stat(&input, &relative).await.map_err(metadata_error)?;
        if before.revision != after.revision {
            return Err(SourceMetadataError::Conflict);
        }
        Ok(WorkingFile {
            file: file.into_temp_path(),
            relative: relative.into(),
            revision: after.revision,
            existed: true,
            input,
        })
    }

    pub(super) async fn save_contents(
        &self,
        relative: String,
        file: tempfile::TempPath,
    ) -> Result<(), SourceMetadataError> {
        let input = self.input().await.map_err(metadata_error)?;
        let (existed, revision) = match self.stat(&input, &relative).await {
            Ok(file) => (true, file.revision),
            Err(SourceError::NotFound) => (false, None),
            Err(error) => return Err(metadata_error(error)),
        };
        self.save_file(&WorkingFile {
            file,
            relative,
            revision,
            existed,
            input,
        })
        .await
    }

    pub(crate) async fn save_file(&self, copy: &WorkingFile) -> Result<(), SourceMetadataError> {
        self.check_file_revision(copy)
            .await
            .map_err(metadata_error)?;
        let mut random = [0_u8; 24];
        getrandom::fill(&mut random).map_err(|e| SourceMetadataError::Write(e.to_string()))?;
        let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let parent = copy
            .relative
            .rsplit_once('/')
            .map(|(parent, _)| format!("{parent}/"))
            .unwrap_or_default();
        let temporary = format!("{parent}.rufin-write-{token}.tmp");
        match copy.input.input() {
            FileInput::Smb(client) => {
                let result: SourceResult<()> = async {
                    let destination = client.create(&temporary).await?;
                    let mut file = tokio::fs::File::open(&copy.file)
                        .await
                        .map_err(|e| SourceError::Other(e.to_string()))?;
                    let mut bytes = vec![0; 65536];
                    let mut offset = 0;
                    loop {
                        let count = file
                            .read(&mut bytes)
                            .await
                            .map_err(|e| SourceError::Other(e.to_string()))?;
                        if count == 0 {
                            break;
                        }
                        let mut written = 0;
                        while written < count {
                            written += crate::file::remote::smb::SmbClient::write(
                                &destination,
                                offset + written as u64,
                                &bytes[written..count],
                            )
                            .await?;
                        }
                        offset += count as u64;
                    }
                    destination.flush().await?;
                    destination.close().await?;
                    self.check_file_revision(copy).await?;
                    client
                        .rename(&temporary, &copy.relative, copy.existed)
                        .await
                }
                .await;
                let cleanup = client.remove(&temporary).await;
                if let Err(error) = cleanup
                    && !matches!(error, SourceError::NotFound)
                {
                    tracing::warn!(%error, "could not remove an SMB write temporary file");
                }
                result.map_err(metadata_error)
            }
            FileInput::WebDav(client) => {
                let destination = url::Url::parse(
                    &self
                        .input_path(copy.input.input(), &copy.relative)
                        .map_err(metadata_error)?,
                )
                .map_err(|e| SourceMetadataError::Write(e.to_string()))?;
                let temporary = url::Url::parse(
                    &self
                        .input_path(copy.input.input(), &temporary)
                        .map_err(metadata_error)?,
                )
                .map_err(|e| SourceMetadataError::Write(e.to_string()))?;
                let result: SourceResult<()> = async {
                    client.upload(&temporary, &copy.file, true).await?;
                    let lock = if copy.existed {
                        self.check_file_revision(copy).await?;
                        client.lock(&destination, copy.revision.as_deref()).await?
                    } else {
                        None
                    };
                    let replaced = async {
                        let mut conditions = Vec::new();
                        if let Some(lock) = &lock {
                            // Nextcloud changes the ETag on LOCK. The conditional
                            // lock acquired above now protects this replacement.
                            conditions.push(lock.clone());
                        } else {
                            self.check_file_revision(copy).await?;
                            if let Some(etag) = copy
                                .revision
                                .as_deref()
                                .filter(|value| value.starts_with('"'))
                            {
                                conditions.push(format!("[{etag}]"));
                            }
                        }
                        let condition = (!conditions.is_empty())
                            .then(|| format!("<{}> ({})", destination, conditions.join(" ")));
                        client
                            .move_file(&temporary, &destination, copy.existed, condition.as_deref())
                            .await
                    }
                    .await;
                    if let Some(lock) = lock
                        && let Err(error) = client.unlock(&destination, &lock).await
                    {
                        tracing::warn!(%error, "could not release the WebDAV file lock");
                    }
                    replaced
                }
                .await;
                let cleanup = client.remove(&temporary).await;
                if let Err(error) = cleanup
                    && !matches!(error, SourceError::NotFound)
                {
                    tracing::warn!(%error, "could not remove a WebDAV write temporary file");
                }
                result.map_err(metadata_error)
            }
        }
    }

    async fn check_file_revision(&self, copy: &WorkingFile) -> SourceResult<()> {
        let same = match self.stat(&copy.input, &copy.relative).await {
            Ok(file) => copy.existed && file.revision == copy.revision,
            Err(SourceError::NotFound) => !copy.existed,
            Err(error) => return Err(error),
        };
        if same {
            Ok(())
        } else {
            Err(SourceError::Server {
                status: 412,
                message: "The file changed before replacement".into(),
            })
        }
    }

    pub(crate) async fn write_lyrics(
        &self,
        database: &library::Database,
        track: &library::TrackRow,
        lyrics: &str,
        sidecar: bool,
    ) -> Result<(), SourceMetadataError> {
        if track.cue_path.is_some()
            || library::source_entity_parts(&track.media_uri)
                .is_none_or(|(source, _, _)| source != self.source_id)
        {
            return Err(SourceMetadataError::Unavailable);
        }
        if sidecar {
            let observation = database
                .observed_media_file(&track.media_uri)
                .await
                .map_err(|e| SourceMetadataError::Write(e.to_string()))?
                .ok_or(SourceMetadataError::Unavailable)?;
            let audio = self.relative(&observation.path).map_err(metadata_error)?;
            let relative = sidecar_path(&audio);
            let file = tempfile::NamedTempFile::new()
                .map_err(|e| SourceMetadataError::Write(e.to_string()))?
                .into_temp_path();
            tokio::fs::write(&file, lyrics)
                .await
                .map_err(|e| SourceMetadataError::Write(e.to_string()))?;
            self.save_contents(relative, file).await
        } else {
            let copy = self.working_file(database, track).await?;
            let lyrics = lyrics.to_string();
            let copy = tokio::task::spawn_blocking(move || {
                crate::file::metadata::write_embedded_lyrics(&copy.file, &lyrics)?;
                Ok::<_, SourceMetadataError>(copy)
            })
            .await
            .map_err(|e| SourceMetadataError::Write(e.to_string()))??;
            self.save_file(&copy).await?;
            self.publish_saved_file(database, &track.media_uri, &copy)
                .await
                .map_err(|error| SourceMetadataError::SavedRefreshFailed(error.to_string()))?;
            Ok(())
        }
    }

    pub(crate) async fn lyrics(
        &self,
        database: &library::Database,
        media_uri: &str,
    ) -> SourceResult<Option<String>> {
        if library::source_entity_parts(media_uri)
            .is_none_or(|(source, kind, _)| source != self.source_id || kind != "track")
        {
            return Err(SourceError::NotFound);
        }
        let track = database
            .track_row_by_uri(media_uri, &library::ReadCancellation::new())
            .await?
            .ok_or(SourceError::NotFound)?;
        let file = database
            .observed_media_file(media_uri)
            .await?
            .ok_or(SourceError::NotFound)?;
        let relative = self.relative(&file.path)?;
        let input = self.input().await?;
        let mut candidates = Vec::new();
        if track.cue_path.is_none() {
            let sidecar = sidecar_path(&relative);
            candidates.push(sidecar.clone());
            candidates.push(format!(
                "{}.LRC",
                sidecar.strip_suffix(".lrc").unwrap_or(&sidecar)
            ));
        }
        if !track.title.is_empty()
            && !matches!(track.title.as_str(), "." | "..")
            && !track.title.contains(['/', '\\', '\0'])
        {
            let parent = relative
                .rsplit_once('/')
                .map_or(String::new(), |(parent, _)| format!("{parent}/"));
            for extension in ["lrc", "LRC"] {
                let path = format!("{parent}{}.{extension}", track.title);
                if !candidates.contains(&path) {
                    candidates.push(path);
                }
            }
        }
        for candidate in candidates {
            match self.small_file(&input, &candidate, 2 * 1024 * 1024).await {
                Ok(bytes) => {
                    if let Ok(text) = String::from_utf8(bytes)
                        && !text.trim().is_empty()
                    {
                        return Ok(Some(text));
                    }
                }
                Err(SourceError::NotFound) => {}
                Err(error) => return Err(error),
            }
        }
        if track.cue_path.is_some() {
            return Ok(None);
        }
        let stream = input
            .playback_stream(
                &self.input_path(input.input(), &relative)?,
                file.revision.as_deref().unwrap_or_default(),
                media_uri,
            )
            .await?;
        let reader = crate::file::remote::reader::FileReader::open(stream).await?;
        tokio::task::spawn_blocking(move || {
            crate::file::metadata::read_embedded_lyrics_input(reader)
        })
        .await
        .map_err(|e| SourceError::Other(e.to_string()))?
        .or_else(|error| {
            if matches!(error, SourceMetadataError::Unavailable) {
                Ok(None)
            } else {
                Err(error)
            }
        })
        .map_err(|e| SourceError::Other(e.to_string()))
    }
}

pub(crate) fn sidecar_path(audio: &str) -> String {
    let (parent, name) = audio
        .rsplit_once('/')
        .map_or((String::new(), audio), |(parent, name)| {
            (format!("{parent}/"), name)
        });
    let stem = name
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map_or(name, |(stem, _)| stem);
    format!("{parent}{stem}.lrc")
}

fn metadata_error(error: SourceError) -> SourceMetadataError {
    match error {
        SourceError::Server { status: 412, .. } => SourceMetadataError::Conflict,
        other => SourceMetadataError::Write(other.to_string()),
    }
}

pub(crate) fn is_write_temporary(relative: &str) -> bool {
    relative
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_prefix(".rufin-write-"))
        .and_then(|name| name.strip_suffix(".tmp"))
        .is_some_and(|name| name.len() == 48 && name.bytes().all(|b| b.is_ascii_hexdigit()))
}

impl RemoteSource {
    async fn collection_revision(
        &self,
        database: &library::Database,
        uri: &str,
    ) -> Result<String, SourceMetadataError> {
        let input = self.input().await.map_err(metadata_error)?;
        let mut after = None;
        let mut hash = blake3::Hasher::new();
        loop {
            let page = database
                .file_metadata_track_page(uri, after)
                .await
                .map_err(|e| SourceMetadataError::Write(e.to_string()))?;
            if page.is_empty() {
                break;
            }
            after = page.last().map(|(key, _)| *key);
            for (_, media_uri) in page {
                let file = database
                    .observed_media_file(&media_uri)
                    .await
                    .map_err(|e| SourceMetadataError::Write(e.to_string()))?
                    .ok_or(SourceMetadataError::Unavailable)?;
                let relative = self.relative(&file.path).map_err(metadata_error)?;
                let file = self.stat(&input, &relative).await.map_err(metadata_error)?;
                hash_file_revision(&mut hash, &media_uri, file.revision.as_deref());
            }
        }
        Ok(hash.finalize().to_hex().to_string())
    }

    async fn fold_collection<T, F: std::future::Future<Output = Result<T, SourceMetadataError>>>(
        &self,
        database: &library::Database,
        uri: &str,
        mut value: T,
        mut accept: impl FnMut(T, library::TrackRow, WorkingFile) -> F,
    ) -> Result<(T, String, usize), SourceMetadataError> {
        if library::source_entity_parts(uri).is_none_or(|(source, kind, _)| {
            source != self.source_id || !matches!(kind.as_str(), "album" | "artist")
        }) {
            return Err(SourceMetadataError::Unavailable);
        }
        let mut after = None;
        let mut count = 0;
        let mut hash = blake3::Hasher::new();
        loop {
            let page = database
                .file_metadata_track_page(uri, after)
                .await
                .map_err(|e| SourceMetadataError::Write(e.to_string()))?;
            if page.is_empty() {
                break;
            }
            after = page.last().map(|(key, _)| *key);
            for (_, media_uri) in page {
                let track = database
                    .track_row_by_uri(&media_uri, &library::ReadCancellation::new())
                    .await
                    .map_err(|e| SourceMetadataError::Write(e.to_string()))?
                    .ok_or(SourceMetadataError::Unavailable)?;
                let copy = self.working_file(database, &track).await?;
                hash_file_revision(&mut hash, &media_uri, copy.revision.as_deref());
                value = accept(value, track, copy).await?;
                count += 1;
            }
        }
        if count == 0 {
            return Err(SourceMetadataError::Unavailable);
        }
        Ok((value, hash.finalize().to_hex().to_string(), count))
    }

    pub(crate) async fn refresh(
        &self,
        database: &library::Database,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<library::ScanOutcome> {
        let mut scan = library::Scan::begin(
            database,
            self.source_id.as_str(),
            &self.name,
            &self.name.to_lowercase(),
            None,
        )
        .await?;
        if let Err(error) = self
            .stage_catalog(database, &mut scan, &|_| {}, cancelled)
            .await
        {
            if matches!(error, SourceError::Cancelled) {
                return Err(error);
            }
            scan.incomplete();
            scan.finish().await?;
            return Err(error);
        }
        Ok(scan.finish().await?)
    }

    async fn write_collection(
        &self,
        database: &library::Database,
        uri: &str,
        expected: &str,
        changed: bool,
        write: impl Fn(&std::path::Path, Option<&str>) -> Result<(), SourceMetadataError>
        + Clone
        + Send
        + 'static,
    ) -> Result<library::ScanOutcome, SourceMetadataError> {
        if self.collection_revision(database, uri).await? != expected {
            return Err(SourceMetadataError::Conflict);
        }
        let mut scan = library::Scan::begin_items(database, self.source_id.as_str())
            .await
            .map_err(|e| metadata_error(e.into()))?;
        // Publish once after the batch, so a renamed album/artist cannot change the
        // membership of later pages. Only one remote working copy is held at a time.
        let result: Result<(), SourceMetadataError> = async {
            if !changed {
                return Ok(());
            }
            let mut after = None;
            loop {
                let page = database
                    .file_metadata_track_page(uri, after)
                    .await
                    .map_err(|e| metadata_error(e.into()))?;
                if page.is_empty() {
                    break;
                }
                after = page.last().map(|(key, _)| *key);
                for (_, media_uri) in page {
                    let track = database
                        .track_row_by_uri(&media_uri, &library::ReadCancellation::new())
                        .await
                        .map_err(|e| metadata_error(e.into()))?
                        .ok_or(SourceMetadataError::Unavailable)?;
                    let copy = self.working_file(database, &track).await?;
                    let write = write.clone();
                    let copy = tokio::task::spawn_blocking(move || {
                        write(&copy.file, track.source_format.as_deref())?;
                        Ok::<_, SourceMetadataError>(copy)
                    })
                    .await
                    .map_err(|e| SourceMetadataError::Write(e.to_string()))??;
                    self.save_file(&copy).await?;
                    self.stage_saved_file(&mut scan, &media_uri, &copy)
                        .await
                        .map_err(|e| SourceMetadataError::SavedRefreshFailed(e.to_string()))?;
                }
            }
            Ok(())
        }
        .await;
        // Successful files must remain visible even if a later file cannot be saved.
        self.stage_artwork(database, &mut scan, &|| false)
            .await
            .map_err(|e| SourceMetadataError::SavedRefreshFailed(e.to_string()))?;
        let outcome = scan
            .finish()
            .await
            .map_err(|e| SourceMetadataError::SavedRefreshFailed(e.to_string()))?;
        result?;
        Ok(outcome)
    }

    pub(crate) async fn read_album_metadata(
        &self,
        database: &library::Database,
        owner: library::AlbumRow,
    ) -> Result<crate::AlbumMetadata, SourceMetadataError> {
        let owner = &owner;
        let (combined, revision, count) = self
            .fold_collection(
                database,
                &owner.media_uri,
                None::<crate::AlbumMetadata>,
                |combined, track, copy| {
                    let owner = owner.clone();
                    async move {
                        tokio::task::spawn_blocking(move || {
                            crate::file::metadata::read_album_file(
                                &owner,
                                combined,
                                &copy.file,
                                track.source_format.as_deref(),
                            )
                        })
                        .await
                        .map_err(|e| SourceMetadataError::Write(e.to_string()))?
                        .map(Some)
                    }
                },
            )
            .await?;
        let mut metadata = combined.ok_or(SourceMetadataError::Unavailable)?;
        metadata.revision = Some(revision);
        metadata.track_count = count;
        Ok(metadata)
    }

    pub(crate) async fn write_album_metadata(
        &self,
        database: &library::Database,
        owner: &library::AlbumRow,
        expected: &str,
        edit: &crate::AlbumMetadataEdit,
    ) -> Result<library::ScanOutcome, SourceMetadataError> {
        let edit = edit.clone();
        let changed = edit.changed != Default::default();
        self.write_collection(
            database,
            &owner.media_uri,
            expected,
            changed,
            move |path, format| {
                let revision = crate::file::metadata::combined_revision(&[path.to_path_buf()])?;
                crate::file::metadata::write_album_batch(
                    &[(path.to_path_buf(), format.map(str::to_owned))],
                    &revision,
                    &edit,
                )
            },
        )
        .await
    }

    pub(crate) async fn read_artist_metadata(
        &self,
        database: &library::Database,
        owner: library::ArtistRow,
    ) -> Result<crate::ArtistMetadata, SourceMetadataError> {
        let owner = &owner;
        let (combined, revision, count) = self
            .fold_collection(
                database,
                &owner.media_uri,
                None::<crate::ArtistMetadata>,
                |combined, track, copy| {
                    let owner = owner.clone();
                    async move {
                        tokio::task::spawn_blocking(move || {
                            crate::file::metadata::read_artist_file(
                                &owner,
                                combined,
                                &copy.file,
                                track.source_format.as_deref(),
                            )
                        })
                        .await
                        .map_err(|e| SourceMetadataError::Write(e.to_string()))?
                        .map(Some)
                    }
                },
            )
            .await?;
        let mut metadata = combined.ok_or(SourceMetadataError::Unavailable)?;
        metadata.revision = Some(revision);
        metadata.track_count = count;
        Ok(metadata)
    }

    pub(crate) async fn write_artist_metadata(
        &self,
        database: &library::Database,
        owner: &library::ArtistRow,
        expected: &str,
        edit: &crate::ArtistMetadataEdit,
    ) -> Result<library::ScanOutcome, SourceMetadataError> {
        let edit = edit.clone();
        let changed = edit.changed != Default::default();
        let name = owner.name.clone();
        self.write_collection(
            database,
            &owner.media_uri,
            expected,
            changed,
            move |path, format| {
                let revision = crate::file::metadata::combined_revision(&[path.to_path_buf()])?;
                crate::file::metadata::write_artist_batch(
                    &[(path.to_path_buf(), format.map(str::to_owned))],
                    &revision,
                    &name,
                    &edit,
                )
            },
        )
        .await
    }
}

fn hash_file_revision(hash: &mut blake3::Hasher, uri: &str, revision: Option<&str>) {
    for value in [uri, revision.unwrap_or_default()] {
        hash.update(&(value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
}
