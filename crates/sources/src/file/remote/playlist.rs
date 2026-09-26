use std::io::{BufReader, BufWriter, Write};
use std::path::Path;

use library::{Database, PlaylistFile, PlaylistImportReport, PlaylistKey, PlaylistPathMode};
use url::Url;

use super::{RemoteSource, input::FileInput, metadata::WorkingFile};
use crate::{SourceError, SourceResult};

impl RemoteSource {
    pub(crate) async fn import_playlist_file(
        &self,
        database: &Database,
        path: &str,
        target: Option<PlaylistKey>,
    ) -> SourceResult<PlaylistImportReport> {
        let location = self.location(path)?;
        let copy = self.working_copy(path, &location).await.map_err(error)?;
        let mut playlist = PlaylistFile::read(
            BufReader::new(std::fs::File::open(&copy.file).map_err(error)?),
            Path::new(&location),
        )?;
        if target.is_none() {
            playlist.identity = None;
        }
        let mut skipped = 0;
        let entries = std::mem::take(&mut playlist.entries);
        playlist.entries.reserve(entries.len());
        for mut entry in entries {
            if let Some(location) = self.playlist_location(path, &entry.locator) {
                if database
                    .file_path_is_rejected(&self.source_id, &location)
                    .await?
                {
                    skipped += 1;
                    continue;
                }
                entry.locator = database
                    .media_uri_for_file_path(self.source_id.as_str(), &location)
                    .await?
                    .unwrap_or_else(|| {
                        library::source_entity_uri(
                            &self.source_id,
                            "track",
                            &format!("file:{:016x}", crate::policy::stable_hash(&location)),
                        )
                    });
            }
            playlist.entries.push(entry);
        }
        let mut report = database
            .import_playlist_document(playlist, Path::new(path), target, |_| None)
            .await?;
        report.skipped += skipped;
        Ok(report)
    }

    pub(crate) async fn save_playlist_file(
        &self,
        database: &Database,
        path: &str,
        file: tempfile::TempPath,
        mode: PlaylistPathMode,
        expected_revision: Option<&str>,
    ) -> SourceResult<()> {
        self.location(path)?;
        let input = self.input().await?;
        let (existed, revision) = match self.stat(&input, path).await {
            Ok(file) => {
                let current = format!(
                    "{}:{}",
                    file.size_bytes.unwrap_or_default(),
                    file.revision.as_deref().unwrap_or_default()
                );
                if expected_revision.is_some_and(|expected| expected != current) {
                    return Err(SourceError::Server {
                        status: 412,
                        message: "The playlist file changed before saving".into(),
                    });
                }
                (true, file.revision)
            }
            Err(SourceError::NotFound) if expected_revision.is_none() => (false, None),
            Err(SourceError::NotFound) => {
                return Err(SourceError::Server {
                    status: 412,
                    message: "The playlist file was removed before saving".into(),
                });
            }
            Err(error) => return Err(error),
        };
        let prepared = tempfile::NamedTempFile::new().map_err(error)?;
        let mut playlist = PlaylistFile::read(
            BufReader::new(std::fs::File::open(&file).map_err(error)?),
            Path::new(path),
        )?;
        for entry in &mut playlist.entries {
            let uri = &entry.locator;
            if library::source_entity_parts(uri)
                .is_some_and(|(source, kind, _)| source == self.source_id && kind == "track")
                && let Some(observation) = database.observed_media_file(uri).await?
                && observation.cue_start_millis.is_none()
            {
                let file = self.relative(&observation.path)?;
                if mode == PlaylistPathMode::Absolute {
                    entry.locator = self.location(&file)?;
                    continue;
                }
                let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
                let parents = parent
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>();
                let parts = file.split('/').collect::<Vec<_>>();
                let common = parents
                    .iter()
                    .zip(&parts)
                    .take_while(|(a, b)| a == b)
                    .count();
                if mode == PlaylistPathMode::Automatic && common < parents.len() {
                    entry.locator = self.location(&file)?;
                    continue;
                }
                let mut locator = "../".repeat(parents.len() - common) + &parts[common..].join("/");
                if locator.contains(['\r', '\n']) {
                    locator = self.location(&file)?;
                }
                if locator.starts_with('#')
                    || locator
                        .split('/')
                        .next()
                        .is_some_and(|part| part.contains(':'))
                {
                    locator.insert_str(0, "./");
                }
                entry.locator = locator;
            }
        }
        let mut output = BufWriter::new(prepared.reopen().map_err(error)?);
        playlist.write(Path::new(path), mode, &mut output)?;
        output.flush().map_err(error)?;
        drop(output);
        self.save_file(&WorkingFile {
            file: prepared.into_temp_path(),
            relative: path.into(),
            revision,
            existed,
            input,
        })
        .await
        .map_err(|failure| match failure {
            crate::SourceMetadataError::Conflict => SourceError::Server {
                status: 412,
                message: "The playlist file changed before saving".into(),
            },
            failure => error(failure),
        })
    }

    fn playlist_location(&self, playlist: &str, value: &str) -> Option<String> {
        if let Ok(uri) = Url::parse(value) {
            if !uri.username().is_empty()
                || uri.password().is_some()
                || uri.query().is_some()
                || uri.fragment().is_some()
            {
                return None;
            }
            for address in std::iter::once(&self.namespace_url)
                .chain(std::iter::once(&self.settings.url))
                .chain(&self.settings.alternate_urls)
            {
                let base = super::collection_url(address).ok()?;
                if uri.scheme() == base.scheme()
                    && uri.host() == base.host()
                    && uri.port() == base.port()
                    && let Some(relative) = uri.path().strip_prefix(base.path())
                {
                    return self
                        .location(
                            &percent_encoding::percent_decode_str(relative)
                                .decode_utf8()
                                .ok()?,
                        )
                        .ok();
                }
            }
            None
        } else {
            self.location(&super::referenced_path(playlist, value).ok()?)
                .ok()
        }
    }
    pub(crate) async fn rename_playlist_file(&self, from: &str, to: &str) -> SourceResult<()> {
        self.location(from)?;
        self.location(to)?;
        let input = self.input().await?;
        match input.input() {
            FileInput::Smb(client) => client.rename(from, to, false).await,
            FileInput::WebDav(client) => {
                let from = url::Url::parse(&self.input_path(input.input(), from)?)
                    .map_err(|error| SourceError::Other(error.to_string()))?;
                let to = url::Url::parse(&self.input_path(input.input(), to)?)
                    .map_err(|error| SourceError::Other(error.to_string()))?;
                client.move_file(&from, &to, false, None).await
            }
        }
    }
    pub(crate) async fn playlist_file_revision(&self, path: &str) -> SourceResult<String> {
        let input = self.input().await?;
        let file = self.stat(&input, path).await?;
        Ok(format!(
            "{}:{}",
            file.size_bytes.unwrap_or_default(),
            file.revision.unwrap_or_default()
        ))
    }

    pub(crate) async fn delete_playlist_file(&self, path: &str) -> SourceResult<()> {
        self.location(path)?;
        let input = self.input().await?;
        match input.input() {
            FileInput::Smb(client) => client.remove(path).await,
            FileInput::WebDav(client) => {
                let url = url::Url::parse(&self.input_path(input.input(), path)?)
                    .map_err(|error| SourceError::Other(error.to_string()))?;
                client.remove(&url).await
            }
        }
    }
}

fn error(error: impl std::fmt::Display) -> SourceError {
    SourceError::Other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::remote::{FileAuthentication, FileSourceSettings};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use wiremock::{Mock, MockServer, Request, ResponseTemplate, matchers::any};

    #[tokio::test]
    async fn dav_playlist_save_retains_revision_across_preparation() {
        for expected in [Some("4:\"old\""), Some("4:\"one\""), None] {
            let server = MockServer::start().await;
            let stats = Arc::new(AtomicUsize::new(0));
            let observed = Arc::clone(&stats);
            Mock::given(any()).respond_with(move |request: &Request| {
                if request.method.as_str() != "PROPFIND" { return ResponseTemplate::new(500); }
                let (path, properties) = if request.url.path() == "/" {
                    ("/", "<d:resourcetype><d:collection/></d:resourcetype>".to_owned())
                } else {
                    let revision = if observed.fetch_add(1, Ordering::SeqCst) == 0 { "one" } else { "two" };
                    ("/mix.m3u8", format!("<d:resourcetype/><d:getcontentlength>4</d:getcontentlength><d:getetag>&quot;{revision}&quot;</d:getetag>"))
                };
                ResponseTemplate::new(207).set_body_string(format!("<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>{path}</d:href><d:propstat><d:prop>{properties}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"))
            }).mount(&server).await;
            let settings = FileSourceSettings {
                excluded_folders: Vec::new(),
                url: format!("{}/", server.uri()),
                alternate_urls: vec![],
                folders: vec![],
                username: String::new(),
                domain: String::new(),
                authentication: FileAuthentication::Anonymous,
                trust_invalid_certificate: false,
                certificate_pem: None,
                require_smb_encryption: false,
            };
            let config = settings
                .configuration(
                    library::SourceId::new("dav:playlist"),
                    "webdav",
                    "DAV".into(),
                )
                .unwrap();
            let source = RemoteSource::open(&config, None).unwrap();
            let directory = tempfile::tempdir().unwrap();
            let database = Database::open(directory.path().join("library.db"))
                .await
                .unwrap();
            let mut output = tempfile::NamedTempFile::new().unwrap();
            writeln!(output, "#EXTM3U\nhttps://example.test/song").unwrap();
            let failure = source
                .save_playlist_file(
                    &database,
                    "mix.m3u8",
                    output.into_temp_path(),
                    PlaylistPathMode::Automatic,
                    expected,
                )
                .await
                .unwrap_err();
            assert!(failure.to_string().contains("changed"), "{failure}");
            assert_eq!(
                stats.load(Ordering::SeqCst),
                if expected == Some("4:\"old\"") { 1 } else { 2 }
            );
            assert!(
                server
                    .received_requests()
                    .await
                    .unwrap()
                    .iter()
                    .all(|request| request.method.as_str() == "PROPFIND")
            );
        }
    }
}
