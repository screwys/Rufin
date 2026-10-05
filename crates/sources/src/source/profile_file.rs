use super::{Implementation, Source};
use crate::{SourceError, SourceResult};
use std::path::Path;

pub struct DocumentProfileFiles {
    source: crate::file::remote::FileSource,
}

impl DocumentProfileFiles {
    pub fn new(uri: &str) -> SourceResult<Self> {
        Ok(Self {
            source: crate::file::remote::FileSource::document_profile(crate::DocumentRoot::new(
                uri, "",
            )?),
        })
    }

    pub fn file_name(path: &str) -> &str {
        crate::file::documents::relative_parts(path)
            .0
            .rsplit('/')
            .next()
            .unwrap_or_default()
    }

    pub async fn profile_files(&self) -> SourceResult<Vec<(String, Option<String>)>> {
        self.source.profile_files("").await
    }

    pub async fn read_profile_file(
        &self,
        path: &str,
        destination: &Path,
    ) -> SourceResult<Option<Option<String>>> {
        self.source.read_profile_file(path, destination).await
    }

    pub async fn write_profile_file(
        &self,
        path: &str,
        file: tempfile::NamedTempFile,
        previous: Option<Option<String>>,
    ) -> SourceResult<()> {
        self.source.write_profile_file(path, file, previous).await
    }
}

impl Source {
    pub async fn list_profile_files(
        &self,
        relative: &str,
    ) -> SourceResult<Vec<(String, Option<String>)>> {
        match &self.implementation {
            Implementation::Files(source) => source.list_profile_files(relative).await,
            _ => Err(SourceError::InvalidRequest(
                "Choose a WebDAV or SMB connection",
            )),
        }
    }

    /// Creates the chosen exchange folder when needed and lists its profile files.
    pub async fn profile_files(
        &self,
        relative: &str,
    ) -> SourceResult<Vec<(String, Option<String>)>> {
        match &self.implementation {
            Implementation::Files(source) => source.profile_files(relative).await,
            _ => Err(SourceError::InvalidRequest(
                "Choose a WebDAV or SMB connection",
            )),
        }
    }

    /// Reads a portable profile through an existing remote-file login.
    /// The outer `None` means the destination does not exist yet.
    pub async fn read_profile_file(
        &self,
        relative: &str,
        destination: &Path,
    ) -> SourceResult<Option<Option<String>>> {
        match &self.implementation {
            Implementation::Files(source) => source.read_profile_file(relative, destination).await,
            _ => Err(SourceError::InvalidRequest(
                "Choose a WebDAV or SMB connection",
            )),
        }
    }

    pub async fn write_profile_file(
        &self,
        relative: &str,
        file: tempfile::NamedTempFile,
        previous: Option<Option<String>>,
    ) -> SourceResult<()> {
        match &self.implementation {
            Implementation::Files(source) => {
                source.write_profile_file(relative, file, previous).await
            }
            _ => Err(SourceError::InvalidRequest(
                "Choose a WebDAV or SMB connection",
            )),
        }
    }
}
