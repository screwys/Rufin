use super::{Implementation, Source};
use crate::{SourceError, SourceResult};
use std::path::Path;

impl Source {
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
