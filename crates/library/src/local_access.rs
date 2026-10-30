//! Folder mapping: where a server's files are on this device. The saved rule is resolved when a
//! track is read, so it covers every track the server lists, including ones added later.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use crate::LocalMediaLocation;

/// A source's saved mapping: its server paths under `root` on this device.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalAccess {
    /// A canonical folder, or a document tree URI.
    pub root: String,
    pub server_prefix: Option<String>,
    pub local_prefix: Option<String>,
}

/// Where one track's file would be under a mapping, before anything looked at it.
#[derive(Clone, Debug, PartialEq)]
pub struct MappedFile {
    pub root: String,
    /// The projected path; under a document tree, the path relative to it.
    pub path: String,
    /// The size the server reports for the file.
    pub size: Option<u64>,
}

impl LocalAccess {
    pub fn mapped(&self, source_path: &str, size: Option<u64>) -> Option<MappedFile> {
        let path = if self.root.starts_with("content:") {
            let relative = project_local_access_path(
                Path::new(""),
                self.server_prefix.as_deref(),
                self.local_prefix.as_deref(),
                source_path,
            )?;
            if relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
            {
                return None;
            }
            relative
        } else {
            project_local_access_path(
                Path::new(&self.root),
                self.server_prefix.as_deref(),
                self.local_prefix.as_deref(),
                source_path,
            )?
        };
        Some(MappedFile {
            root: self.root.clone(),
            path: path.to_string_lossy().into_owned(),
            size,
        })
    }
}

impl MappedFile {
    /// The file when it is a regular file inside the mapped folder with the size the server
    /// reports. A path that cannot be looked at is not there. This blocks on the file system.
    pub fn locate(&self) -> Option<LocalMediaLocation> {
        let sized = |actual: Option<u64>| {
            self.size
                .is_none_or(|size| actual.is_none_or(|actual| actual == size))
        };
        if self.root.starts_with("content:") {
            let components = self.path.split('/').map(str::to_owned).collect::<Vec<_>>();
            let (uri, size) = DOCUMENTS.get()?(&self.root, &components)?;
            return sized(size).then_some(LocalMediaLocation::Document(uri));
        }
        let path = std::fs::canonicalize(&self.path).ok()?;
        let metadata = std::fs::metadata(&path).ok()?;
        (path.starts_with(&self.root) && metadata.is_file() && sized(Some(metadata.len())))
            .then_some(LocalMediaLocation::File(path))
    }
}

impl crate::Playable {
    /// The mapped file as a locator while it is there. This blocks on the file system.
    pub fn mapped_locator(&self) -> Option<crate::LocalLocatorWrite> {
        let mapped = self.mapped.as_ref()?;
        let (path, access_uri) = match mapped.locate()? {
            LocalMediaLocation::File(path) => {
                let access_uri = url::Url::from_file_path(&path).ok()?.to_string();
                (path.to_string_lossy().into_owned(), access_uri)
            }
            LocalMediaLocation::Document(uri) => (uri.clone(), uri),
        };
        let relative_path = Path::new(&path).strip_prefix(&mapped.root).map_or_else(
            |_| mapped.path.clone(),
            |relative| relative.to_string_lossy().into_owned(),
        );
        Some(crate::LocalLocatorWrite {
            source_id: self.source_id.as_ref().map(|id| id.as_str().to_owned()),
            media_uri: self.media_uri.clone(),
            origin: "mapping".into(),
            path,
            root: mapped.root.clone(),
            relative_path,
            access_uri,
        })
    }
}

/// Finds a regular document under a tree by its path components: its URI and size.
pub type DocumentLookup = fn(&str, &[String]) -> Option<(String, Option<u64>)>;

static DOCUMENTS: OnceLock<DocumentLookup> = OnceLock::new();

/// The host's document provider installs this once; without it no document tree maps.
pub fn install_document_lookup(lookup: DocumentLookup) {
    DOCUMENTS.get_or_init(|| lookup);
}

/// Applies one saved server-prefix/local-prefix formula to a reported source path.
///
/// File existence and containment within `root` remain the caller's responsibility.
pub fn project_local_access_path(
    root: &Path,
    server_prefix: Option<&str>,
    local_prefix: Option<&str>,
    source_path: &str,
) -> Option<PathBuf> {
    let mut base = root.to_path_buf();
    if let Some(prefix) = local_prefix
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
    {
        base.push(prefix);
    }
    if let Some(prefix) = server_prefix
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
    {
        let relative = source_path
            .strip_prefix(prefix)?
            .trim_start_matches(['/', '\\']);
        return Some(base.join(relative.replace('\\', "/")));
    }
    if reported_path_is_absolute(source_path) {
        Some(PathBuf::from(source_path))
    } else {
        Some(base.join(source_path.replace('\\', "/")))
    }
}

pub fn reported_path_is_absolute(value: &str) -> bool {
    Path::new(value).is_absolute()
        || value.as_bytes().get(1) == Some(&b':')
        || value.starts_with("\\\\")
}
