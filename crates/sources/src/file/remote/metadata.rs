use super::FileSource;
use crate::file::remote::input::{FileInput, FileInputServer};
use crate::{SourceError, SourceMetadataError, SourceResult};
use std::sync::Arc;
use tokio::io::AsyncReadExt;

impl FileSource {
    pub(crate) async fn read_track_lyrics(
        &self,
        database: &library::Database,
        relative: &str,
        track: &mut crate::file::media::ScannedTrack,
    ) -> SourceResult<()> {
        let mut candidates = self.lyrics_candidates(
            relative,
            &track.track.metadata.title,
            track.track.cue_path.is_some(),
        );
        track.lyrics_paths = candidates
            .iter()
            .map(|path| self.location(path))
            .collect::<SourceResult<_>>()?;
        let input = self.input().await?;
        if self.document_access().is_none() && track.track.cue_path.is_none() {
            let available = database
                .source_files(self.source_id.as_str(), &track.lyrics_paths)
                .await?;
            candidates.retain(|relative| {
                self.location(relative)
                    .is_ok_and(|path| available.iter().any(|file| file.path == path))
            });
        }
        let sidecar = self.sidecar_lyrics(&input, &candidates).await?;
        let bundle = match sidecar {
            Some(text) => lyrics::lyrics_from_edited_text(&text),
            None if track.track.cue_path.is_none() => track
                .lyrics
                .take()
                .and_then(lyrics::lyrics_with_displayable_content),
            None => None,
        };
        crate::file::lyrics::set_lyrics(track, bundle);
        Ok(())
    }

    async fn sidecar_lyrics(
        &self,
        input: &Arc<FileInputServer>,
        candidates: &[String],
    ) -> SourceResult<Option<String>> {
        for path in candidates {
            match self.small_file(input, path, 2 * 1024 * 1024).await {
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
        Ok(None)
    }

    pub(crate) async fn observe_lyrics_dependencies(
        &self,
        catalog: &mut library::Catalog<'_>,
        paths: &[String],
    ) -> SourceResult<()> {
        let input = self.input().await?;
        let mut observations = Vec::new();
        let mut missing = Vec::new();
        for path in paths
            .iter()
            .filter(|path| crate::file::lyrics::is_sidecar(path))
            .collect::<std::collections::BTreeSet<_>>()
        {
            match self.stat(&input, &self.relative(path)?).await {
                Ok(mut file) => {
                    file.path = path.clone();
                    file.state = library::LocalFileState::Rejected;
                    observations.push((file, Vec::new()));
                }
                Err(SourceError::NotFound) => missing.push(path.clone()),
                Err(SourceError::Cancelled) => return Err(SourceError::Cancelled),
                Err(error) => tracing::warn!(%error, "Could not inspect lyrics sidecar"),
            }
        }
        catalog
            .write_file_observations(&observations, Vec::new(), true)
            .await?;
        let missing = catalog
            .database
            .source_files(catalog.source_id, &missing)
            .await?
            .into_iter()
            .map(|file| file.path)
            .collect::<Vec<_>>();
        catalog
            .remove_source_files(&missing, catalog.observed.unwrap_or(i64::MAX))
            .await?;
        Ok(())
    }

    pub(crate) fn lyrics_candidates(&self, relative: &str, title: &str, cue: bool) -> Vec<String> {
        let relative = crate::file::documents::relative_parts(relative).0;
        let mut candidates = Vec::new();
        if !cue {
            let sidecar = sidecar_path(relative);
            candidates.push(sidecar.clone());
            candidates.push(format!(
                "{}.LRC",
                sidecar.strip_suffix(".lrc").unwrap_or(&sidecar)
            ));
        }
        if !title.is_empty() && !matches!(title, "." | "..") && !title.contains(['/', '\\', '\0']) {
            let parent = relative
                .rsplit_once('/')
                .map_or(String::new(), |(parent, _)| format!("{parent}/"));
            for extension in ["lrc", "LRC"] {
                let path = format!("{parent}{title}.{extension}");
                if !candidates.contains(&path) {
                    candidates.push(path);
                }
            }
        }
        candidates
    }

    pub(crate) async fn commit_file(
        &self,
        relative: &str,
        file: &std::path::Path,
        revision: Option<&str>,
        existed: bool,
    ) -> Result<(), SourceMetadataError> {
        let input = self.input().await.map_err(metadata_error)?;
        self.check_file_revision(&input, relative, revision, existed)
            .await
            .map_err(metadata_error)?;
        let mut random = [0_u8; 24];
        getrandom::fill(&mut random).map_err(|e| SourceMetadataError::Write(e.to_string()))?;
        let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let parent = relative
            .rsplit_once('/')
            .map(|(parent, _)| format!("{parent}/"))
            .unwrap_or_default();
        let temporary = format!("{parent}.rufin-write-{token}.tmp");
        match input.input() {
            FileInput::Peer { .. } => {
                return Err(SourceMetadataError::Write(
                    "Peer access provides audio bytes".into(),
                ));
            }
            FileInput::Documents(access) => {
                let access = Arc::clone(access);
                let relative = relative.to_owned();
                let file = file.to_path_buf();
                let revision = revision.map(str::to_owned);
                tokio::task::spawn_blocking(move || {
                    access.save(&relative, &file, revision.as_deref(), existed)
                })
                .await
                .map_err(|error| SourceMetadataError::Write(error.to_string()))?
                .map_err(metadata_error)
            }
            FileInput::Smb(client) => {
                let result: SourceResult<()> = async {
                    let destination = client.create(&temporary).await?;
                    let mut file = tokio::fs::File::open(&file)
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
                    self.check_file_revision(&input, relative, revision, existed)
                        .await?;
                    client.rename(&temporary, &relative, existed).await
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
                        .input_path(input.input(), &relative)
                        .map_err(metadata_error)?,
                )
                .map_err(|e| SourceMetadataError::Write(e.to_string()))?;
                let temporary = url::Url::parse(
                    &self
                        .input_path(input.input(), &temporary)
                        .map_err(metadata_error)?,
                )
                .map_err(|e| SourceMetadataError::Write(e.to_string()))?;
                let result: SourceResult<()> = async {
                    client.upload(&temporary, &file, true).await?;
                    let lock = if existed {
                        self.check_file_revision(&input, relative, revision, existed)
                            .await?;
                        client.lock(&destination, revision.as_deref()).await?
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
                            self.check_file_revision(&input, relative, revision, existed)
                                .await?;
                            if let Some(etag) =
                                revision.as_deref().filter(|value| value.starts_with('"'))
                            {
                                conditions.push(format!("[{etag}]"));
                            }
                        }
                        let condition = (!conditions.is_empty())
                            .then(|| format!("<{}> ({})", destination, conditions.join(" ")));
                        client
                            .move_file(&temporary, &destination, existed, condition.as_deref())
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

    pub(crate) async fn check_file_revision(
        &self,
        input: &Arc<FileInputServer>,
        relative: &str,
        revision: Option<&str>,
        existed: bool,
    ) -> SourceResult<()> {
        let same = match self.stat(&input, &relative).await {
            Ok(file) => existed && file.revision.as_deref() == revision,
            Err(SourceError::NotFound) => !existed,
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
            .track(&(media_uri), &library::ReadCancellation::new())
            .await?
            .ok_or(SourceError::NotFound)?;
        let file = database
            .observed_media_file(media_uri)
            .await?
            .ok_or(SourceError::NotFound)?;
        let relative = self.relative(&file.path)?;
        let input = self.input().await?;
        let candidates = self.lyrics_candidates(
            &relative,
            &track.metadata.title,
            track.access.cue_path.is_some(),
        );
        if let Some(text) = self.sidecar_lyrics(&input, &candidates).await? {
            return Ok(Some(text));
        }
        if track.access.cue_path.is_some() {
            return Ok(None);
        }
        let reader = crate::file::remote::reader::FileReader::open(
            &input,
            &self.input_path(input.input(), &relative)?,
            file.revision.as_deref().unwrap_or_default(),
            media_uri,
        )
        .await?;
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
