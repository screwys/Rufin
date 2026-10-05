use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Native paths keep their released JSON representation. Document roots are URI grants.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum DownloadDirectory {
    Native(PathBuf),
    Document { uri: String },
}

impl DownloadDirectory {
    pub fn native(&self) -> Option<&Path> {
        match self {
            Self::Native(path) => Some(path),
            Self::Document { .. } => None,
        }
    }
}

impl std::fmt::Display for DownloadDirectory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native(path) => path.display().fmt(formatter),
            Self::Document { uri } => uri.fmt(formatter),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum DownloadAudio {
    Native {
        root: Option<PathBuf>,
        path: PathBuf,
    },
    Document {
        root: String,
        relative: Vec<String>,
        file: Option<String>,
    },
}

pub(super) struct DownloadFileMetadata {
    pub size: Option<u64>,
    pub mtime_ns: i64,
}

impl DownloadAudio {
    pub fn native(&self) -> Option<&Path> {
        match self {
            Self::Native { path, .. } => Some(path),
            Self::Document { .. } => None,
        }
    }

    pub fn location(&self) -> Result<String, String> {
        match self {
            Self::Native { path, .. } => Ok(path.to_string_lossy().into_owned()),
            Self::Document {
                file: Some(file), ..
            } => Ok(file.clone()),
            Self::Document { file: None, .. } => {
                Err("The document download has not been published".into())
            }
        }
    }

    pub fn access_uri(&self) -> Result<String, String> {
        match self {
            Self::Native { path, .. } => reqwest::Url::from_file_path(path)
                .map(String::from)
                .map_err(|()| "Download path is not absolute".into()),
            Self::Document { .. } => self.location(),
        }
    }

    pub fn metadata(&self) -> Result<Option<DownloadFileMetadata>, String> {
        match self {
            Self::Native { path, .. } => match std::fs::metadata(path) {
                Ok(metadata) if metadata.is_file() => Ok(Some(DownloadFileMetadata {
                    size: Some(metadata.len()),
                    mtime_ns: metadata
                        .modified()
                        .ok()
                        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |value| {
                            i64::try_from(value.as_nanos()).unwrap_or(i64::MAX)
                        }),
                })),
                Ok(_) => Ok(None),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.to_string()),
            },
            Self::Document {
                file: Some(file), ..
            } => match sources::stat_document(file) {
                Ok(entry) if !entry.directory => Ok(Some(DownloadFileMetadata {
                    size: entry.size,
                    mtime_ns: 0,
                })),
                Ok(_) | Err(sources::SourceError::NotFound) => Ok(None),
                Err(error) => Err(error.to_string()),
            },
            Self::Document { file: None, .. } => Ok(None),
        }
    }

    pub async fn inspect(&self) -> Result<Option<DownloadFileMetadata>, String> {
        let file = self.clone();
        tokio::task::spawn_blocking(move || file.metadata())
            .await
            .map_err(|error| error.to_string())?
    }

    pub fn projection(&self, private_root: &Path) -> Result<(String, String), String> {
        match self {
            Self::Native { root, path } => {
                let root = root.as_deref().unwrap_or(private_root);
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| "The download is outside its managed storage")?;
                Ok((
                    root.to_string_lossy().into_owned(),
                    relative.to_string_lossy().into_owned(),
                ))
            }
            Self::Document { root, relative, .. } => Ok((root.clone(), relative.join("/"))),
        }
    }

    pub async fn publish(&self, partial: &Path) -> Result<Self, String> {
        match self {
            Self::Native { path, .. } => {
                tokio::fs::rename(partial, path)
                    .await
                    .map_err(|error| format!("Could not save the downloaded track: {error}"))?;
                Ok(self.clone())
            }
            Self::Document {
                root,
                relative,
                file,
            } => {
                let (root, mut relative, file, partial) = (
                    root.clone(),
                    relative.clone(),
                    file.clone(),
                    partial.to_owned(),
                );
                tokio::task::spawn_blocking(move || {
                    let parent = sources::create_document_directories(
                        &root,
                        &relative[..relative.len() - 1],
                    )
                    .map_err(|error| error.to_string())?
                    .uri;
                    let name = relative
                        .last()
                        .ok_or("The document download has no filename")?;
                    let existing = if file.is_some() {
                        None
                    } else {
                        sources::find_document_child(&parent, name)
                            .map_err(|error| error.to_string())?
                    };
                    let entry = if let Some(entry) = existing {
                        sources::save_document(&entry.uri, &partial, entry.revision.as_deref())
                            .map_err(|error| error.to_string())?;
                        entry
                    } else {
                        sources::create_document(
                            &parent,
                            name,
                            "application/octet-stream",
                            &partial,
                        )
                        .map_err(|error| error.to_string())?
                    };
                    *relative.last_mut().expect("generated download filename") = entry.name;
                    Ok(Self::Document {
                        root,
                        relative,
                        file: Some(entry.uri),
                    })
                })
                .await
                .map_err(|error| error.to_string())?
            }
        }
    }

    pub async fn remove(&self) -> Result<bool, String> {
        match self {
            Self::Native { path, .. } => match tokio::fs::remove_file(path).await {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error.to_string()),
            },
            Self::Document {
                file: Some(file), ..
            } => {
                let file = file.clone();
                tokio::task::spawn_blocking(move || {
                    let present = match sources::delete_document(&file) {
                        Ok(()) => true,
                        Err(sources::SourceError::NotFound) => false,
                        Err(error) => return Err(error.to_string()),
                    };
                    Ok(present)
                })
                .await
                .map_err(|error| error.to_string())?
            }
            Self::Document { file: None, .. } => Ok(false),
        }
    }
}
