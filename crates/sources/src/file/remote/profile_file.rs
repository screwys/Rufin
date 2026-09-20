use super::{RemoteSource, input::FileInput, webdav::client::Body};
use crate::{SourceError, SourceResult};
use reqwest::{
    Method, StatusCode,
    header::{ETAG, HeaderMap, HeaderValue, IF_MATCH, IF_NONE_MATCH},
};
use std::path::Path;
use tokio::io::AsyncWriteExt;

impl RemoteSource {
    pub(crate) async fn profile_files(
        &self,
        relative: &str,
    ) -> SourceResult<Vec<(String, Option<String>)>> {
        self.location(relative)?;
        let input = self.input().await?;
        // Create each missing parent, using the existing account's root.
        let mut folder = String::new();
        for component in relative
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
        {
            if !folder.is_empty() {
                folder.push('/');
            }
            folder.push_str(component);
            match self.stat(&input, &folder).await {
                Ok(entry) if entry.kind == library::LocalFileKind::Directory => continue,
                Ok(_) => {
                    return Err(SourceError::InvalidRequest(
                        "Choose a folder for Connect storage",
                    ));
                }
                Err(SourceError::NotFound) => {}
                Err(error) => return Err(error),
            }
            match input.input() {
                FileInput::Smb(client) => client.create_directory(&folder).await?,
                FileInput::WebDav(client) => {
                    let url = url::Url::parse(&self.input_path(input.input(), &folder)?)
                        .map_err(error)?;
                    let response = client
                        .request(
                            Method::from_bytes(b"MKCOL").unwrap(),
                            &url,
                            HeaderMap::new(),
                            Body::Empty,
                        )
                        .await?;
                    if response.status() != StatusCode::CREATED {
                        // Another device may have created the same folder.
                        if response.status() != StatusCode::METHOD_NOT_ALLOWED
                            || !client.stat(&url).await?.directory
                        {
                            return Err(SourceError::Server {
                                status: response.status().as_u16(),
                                message: "Could not create the Connect folder".into(),
                            });
                        }
                    }
                }
            }
        }
        let mut files = Vec::new();
        match input.input() {
            FileInput::Smb(client) => {
                client
                    .list(relative.trim_matches('/'), |entry| {
                        if !entry.directory && entry.path.ends_with(".rufin-connect") {
                            files.push((entry.path, Some(entry.revision)));
                        }
                        std::future::ready(Ok(()))
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
                                return Ok(());
                            }
                            let child = client.resolve_href(&url, &entry.href)?;
                            let encoded = child
                                .path()
                                .strip_prefix(client.root().path())
                                .ok_or(SourceError::NotFound)?;
                            let path = percent_encoding::percent_decode_str(encoded)
                                .decode_utf8()
                                .map_err(error)?;
                            if path.ends_with(".rufin-connect") {
                                files.push((path.into_owned(), entry.revision()));
                            }
                            Ok(())
                        })();
                        std::future::ready(result)
                    })
                    .await?;
            }
        }
        Ok(files)
    }

    pub(crate) async fn read_profile_file(
        &self,
        relative: &str,
        destination: &Path,
    ) -> SourceResult<Option<Option<String>>> {
        self.location(relative)?;
        let input = self.input().await?;
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

    pub(crate) async fn write_profile_file(
        &self,
        relative: &str,
        file: tempfile::NamedTempFile,
        previous: Option<Option<String>>,
    ) -> SourceResult<()> {
        self.location(relative)?;
        let input = self.input().await?;
        if matches!(input.input(), FileInput::Smb(_)) {
            let previous = if previous == Some(None) {
                match self.stat(&input, relative).await {
                    Ok(entry) => Some(entry.revision),
                    Err(SourceError::NotFound) => None,
                    Err(error) => return Err(error),
                }
            } else {
                previous
            };
            return self
                .save_file(&super::metadata::WorkingFile {
                    file: file.into_temp_path(),
                    relative: relative.into(),
                    existed: previous.is_some(),
                    revision: previous.flatten(),
                    input,
                })
                .await
                .map_err(error);
        }
        let FileInput::WebDav(client) = input.input() else {
            return Err(SourceError::InvalidRequest(
                "Choose a WebDAV or SMB connection",
            ));
        };
        let url = url::Url::parse(&self.input_path(input.input(), relative)?).map_err(error)?;
        let mut headers = HeaderMap::new();
        match previous {
            None => {
                headers.insert(IF_NONE_MATCH, HeaderValue::from_static("*"));
            }
            Some(Some(etag)) => {
                headers.insert(IF_MATCH, HeaderValue::from_str(&etag).map_err(error)?);
            }
            Some(None) => {}
        }
        let response = client
            .request(Method::PUT, &url, headers, Body::File(file.path()))
            .await?;
        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::PRECONDITION_FAILED => Err(SourceError::Other(
                "The Connect file changed. Refresh to merge it before writing again".into(),
            )),
            status => Err(SourceError::Server {
                status: status.as_u16(),
                message: "Could not write the Connect file".into(),
            }),
        }
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
        matchers::{header, method, path},
    };

    #[tokio::test]
    async fn profile_exchange_uses_existing_dav_login_and_conditional_write() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND")).respond_with(ResponseTemplate::new(207).set_body_string(
            "<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"
        )).mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/profile.rufin-connect"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"v1\"")
                    .set_body_bytes(b"encrypted profile"),
            )
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/profile.rufin-connect"))
            .and(header("If-Match", "\"v1\""))
            .respond_with(ResponseTemplate::new(412))
            .expect(1)
            .mount(&server)
            .await;
        let settings = FileSourceSettings {
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
        let source = RemoteSource::open(&config, None).unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        let version = source
            .read_profile_file("profile.rufin-connect", file.path())
            .await
            .unwrap();
        assert_eq!(version, Some(Some("\"v1\"".into())));
        assert_eq!(std::fs::read(file.path()).unwrap(), b"encrypted profile");
        let error = source
            .write_profile_file("profile.rufin-connect", file, version)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("changed"));
    }
}
