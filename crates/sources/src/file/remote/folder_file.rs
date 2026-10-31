//! Files of a sync folder on a file connection.
use super::{
    FileSource,
    input::{FileInput, FileInputServer},
};
use crate::{SourceError, SourceResult};
use reqwest::{StatusCode, header::ETAG};
use std::path::Path;
use tokio::io::AsyncWriteExt;

impl FileSource {
    /// The files directly in `relative` whose names end with `suffix`, as (name, path); a
    /// missing folder holds none.
    pub async fn list_folder_files(
        &self,
        relative: &str,
        suffix: &str,
    ) -> SourceResult<Vec<(String, String)>> {
        let mut files = Vec::new();
        let listed = self
            .visit_files(relative, suffix, |(path, _)| {
                let name = crate::file::documents::relative_parts(&path).0;
                let name = name.rsplit('/').next().unwrap_or(name).to_owned();
                files.push((name, path));
                std::future::ready(Ok(()))
            })
            .await;
        match listed {
            Ok(()) => Ok(files),
            Err(SourceError::NotFound) => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    /// Creates `relative` and the folders above it that are missing.
    async fn create_folders(&self, input: &FileInputServer, relative: &str) -> SourceResult<()> {
        let parts = relative
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        for depth in 1..=parts.len() {
            let path = parts[..depth].join("/");
            match input.input() {
                FileInput::Smb(client) => match client.stat(&path).await {
                    Ok(_) => {}
                    Err(SourceError::NotFound) => client.create_directory(&path).await?,
                    Err(error) => return Err(error),
                },
                FileInput::WebDav(client) => {
                    let mut url =
                        url::Url::parse(&self.input_path(input.input(), &path)?).map_err(error)?;
                    url.path_segments_mut()
                        .map_err(|_| SourceError::NotFound)?
                        .push("");
                    client.create_collection(&url).await?;
                }
                FileInput::Documents(_) | FileInput::Link { .. } => {
                    return Err(SourceError::InvalidRequest(
                        "Choose a WebDAV or SMB connection",
                    ));
                }
            }
        }
        Ok(())
    }

    pub async fn visit_files<F, Fut>(
        &self,
        relative: &str,
        suffix: &str,
        mut visit: F,
    ) -> SourceResult<()>
    where
        F: FnMut((String, Option<String>)) -> Fut + Send,
        Fut: std::future::Future<Output = SourceResult<()>> + Send,
    {
        self.location(relative)?;
        let input = self.input().await?;
        match input.input() {
            FileInput::Link { .. } => {
                return Err(SourceError::InvalidRequest(
                    "Peer access provides audio bytes",
                ));
            }
            FileInput::Documents(access) => {
                let access = std::sync::Arc::clone(access);
                let relative_path = relative.to_owned();
                let entry = tokio::task::spawn_blocking(move || access.resolve(&relative_path))
                    .await
                    .map_err(error)??;
                let mut offset = 0;
                loop {
                    let uri = entry.uri.clone();
                    let page =
                        tokio::task::spawn_blocking(move || crate::list_documents(&uri, offset))
                            .await
                            .map_err(error)??;
                    for child in page.entries {
                        if !child.directory && child.name.ends_with(suffix) {
                            visit((
                                crate::file::documents::document_relative(
                                    &format!("{}/{}", relative.trim_end_matches('/'), child.name),
                                    &child.native_id,
                                    &child.uri,
                                ),
                                child.revision,
                            ))
                            .await?;
                        }
                    }
                    match page.next {
                        Some(next) => offset = next,
                        None => break,
                    }
                }
            }
            FileInput::Smb(client) => {
                client
                    .list(relative.trim_matches('/'), |entry| {
                        let next = (!entry.directory && entry.path.ends_with(suffix))
                            .then(|| visit((entry.path, Some(entry.revision))));
                        async move {
                            match next {
                                Some(next) => next.await,
                                None => Ok(()),
                            }
                        }
                    })
                    .await?;
            }
            FileInput::WebDav(client) => {
                let mut url =
                    url::Url::parse(&self.input_path(input.input(), relative)?).map_err(error)?;
                if !url.path().ends_with('/') {
                    url.path_segments_mut()
                        .map_err(|_| SourceError::NotFound)?
                        .push("");
                }
                client
                    .list(&url, 1, |entry| {
                        let result = (|| {
                            if entry.directory {
                                return Ok(None);
                            }
                            let child = client.resolve_href(&url, &entry.href)?;
                            let encoded = child
                                .path()
                                .strip_prefix(client.root().path())
                                .ok_or(SourceError::NotFound)?;
                            let path = percent_encoding::percent_decode_str(encoded)
                                .decode_utf8()
                                .map_err(error)?;
                            Ok::<_, SourceError>(
                                path.ends_with(suffix)
                                    .then(|| (path.into_owned(), entry.revision())),
                            )
                        })();
                        let next = result.map(|entry| entry.map(&mut visit));
                        async move {
                            match next? {
                                Some(next) => next.await,
                                None => Ok(()),
                            }
                        }
                    })
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn copy_file(
        &self,
        relative: &str,
        destination: &Path,
    ) -> SourceResult<Option<Option<String>>> {
        self.location(relative)?;
        let input = self.input().await?;
        if let FileInput::Documents(access) = input.input() {
            let access = std::sync::Arc::clone(access);
            let relative = relative.to_owned();
            let destination = destination.to_owned();
            return tokio::task::spawn_blocking(move || {
                let entry = match access.resolve(&relative) {
                    Ok(entry) => entry,
                    Err(SourceError::NotFound) => return Ok(None),
                    Err(error) => return Err(error),
                };
                crate::copy_document(&entry.uri, &destination)?;
                Ok(Some(entry.revision))
            })
            .await
            .map_err(error)?;
        }
        if let FileInput::Smb(client) = input.input() {
            let (file, entry) = match client.open_read(relative).await {
                Ok(value) => value,
                Err(SourceError::NotFound) => return Ok(None),
                Err(error) => return Err(error),
            };
            let mut output = tokio::fs::File::create(destination).await.map_err(error)?;
            let mut offset = 0;
            while offset < entry.size {
                let bytes = super::smb::SmbClient::read(
                    &file,
                    offset,
                    (entry.size - offset).min(65536) as usize,
                )
                .await?;
                if bytes.is_empty() {
                    break;
                }
                output.write_all(&bytes).await.map_err(error)?;
                offset += bytes.len() as u64;
            }
            file.close().await?;
            output.sync_all().await.map_err(error)?;
            return Ok(Some(Some(entry.revision)));
        }
        let FileInput::WebDav(client) = input.input() else {
            return Err(SourceError::InvalidRequest(
                "Choose a WebDAV or SMB connection",
            ));
        };
        let url = url::Url::parse(&self.input_path(input.input(), relative)?).map_err(error)?;
        let mut response = client.read(&url, None, None).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if response.status() != StatusCode::OK {
            return Err(SourceError::Server {
                status: response.status().as_u16(),
                message: "Could not read the Connect file".into(),
            });
        }
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut file = tokio::fs::File::create(destination).await.map_err(error)?;
        while let Some(bytes) = response.chunk().await.map_err(error)? {
            file.write_all(&bytes).await.map_err(error)?;
        }
        file.sync_all().await.map_err(error)?;
        Ok(Some(etag))
    }

    /// Writes the file at `relative`, replacing one already there. The folders above it are
    /// created only when a write finds them missing.
    pub async fn write_file(&self, relative: &str, file: &Path) -> SourceResult<()> {
        self.location(relative)?;
        let input = self.input().await?;
        let written = self.put_file(&input, relative, file).await;
        let Err(failed) = written else {
            return Ok(());
        };
        let parent = relative.rsplit_once('/').map_or("", |(parent, _)| parent);
        if parent.is_empty()
            || !matches!(self.stat(&input, parent).await, Err(SourceError::NotFound))
        {
            return Err(failed);
        }
        self.create_folders(&input, parent).await?;
        self.put_file(&input, relative, file).await
    }

    async fn put_file(
        &self,
        input: &FileInputServer,
        relative: &str,
        file: &Path,
    ) -> SourceResult<()> {
        if let FileInput::WebDav(client) = input.input() {
            let url = url::Url::parse(&self.input_path(input.input(), relative)?).map_err(error)?;
            return client.upload(&url, file, false).await;
        }
        let revision = match self.stat(input, relative).await {
            Ok(entry) => Some(entry.revision),
            Err(SourceError::NotFound) => None,
            Err(error) => return Err(error),
        };
        self.commit_file(
            relative,
            file,
            revision.as_ref().and_then(|revision| revision.as_deref()),
            revision.is_some(),
        )
        .await
        .map_err(error)
    }
}

fn error(error: impl std::fmt::Display) -> SourceError {
    SourceError::Other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileAuthentication, FileSourceSettings, SourceId};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[tokio::test]
    async fn sync_folder_files_use_the_dav_login() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(207).set_body_string(
                "<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"
            ))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/Rufin%20Connect/"))
            .respond_with(ResponseTemplate::new(207).set_body_string(
                "<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/Rufin%20Connect/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:response><d:href>/Rufin%20Connect/000000000001-000000000002.rcs</d:href><d:propstat><d:prop><d:resourcetype/><d:getetag>\"v1\"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:response><d:href>/Rufin%20Connect/notes.txt</d:href><d:propstat><d:prop><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"
            ))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/Rufin%20Connect/peer"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("MKCOL"))
            .and(path("/Rufin%20Connect/"))
            .respond_with(ResponseTemplate::new(405))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("MKCOL"))
            .and(path("/Rufin%20Connect/peer/"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/Rufin%20Connect/000000000001-000000000002.rcs"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"v1\"")
                    .set_body_bytes(b"encrypted segment"),
            )
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/Rufin%20Connect/000000000003-000000000003.rcs"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;
        // A write into a missing folder creates it, then writes again.
        Mock::given(method("PUT"))
            .and(path("/Rufin%20Connect/peer/000000000004-000000000004.rcs"))
            .respond_with(ResponseTemplate::new(409))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/Rufin%20Connect/peer/000000000004-000000000004.rcs"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;
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
            .configuration(SourceId::new("dav"), "webdav", "Files".into())
            .unwrap();
        let source = FileSource::open(&config, None).unwrap();
        let files = source
            .list_folder_files("Rufin Connect", ".rcs")
            .await
            .unwrap();
        assert_eq!(
            files,
            vec![(
                "000000000001-000000000002.rcs".to_owned(),
                "Rufin Connect/000000000001-000000000002.rcs".to_owned()
            )]
        );
        let file = tempfile::NamedTempFile::new().unwrap();
        source.copy_file(&files[0].1, file.path()).await.unwrap();
        assert_eq!(std::fs::read(file.path()).unwrap(), b"encrypted segment");
        source
            .write_file("Rufin Connect/000000000003-000000000003.rcs", file.path())
            .await
            .unwrap();
        source
            .write_file(
                "Rufin Connect/peer/000000000004-000000000004.rcs",
                file.path(),
            )
            .await
            .unwrap();
    }
}
