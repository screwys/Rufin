use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::LocalImageRef;
use lofty::file::TaggedFile;
use lofty::file::TaggedFileExt;
use lofty::picture::{Picture, PictureType};

use crate::{ImageBytes, SourceError, SourceResult};

use super::discovery;
use super::lofty_metadata::read_lofty;

const LOCAL_IMAGE_MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(super) enum ArtworkReference {
    File(PathBuf),
    Embedded { path: PathBuf, picture_index: u32 },
}

impl ArtworkReference {
    pub(super) fn path(&self) -> &Path {
        match self {
            Self::File(path) | Self::Embedded { path, .. } => path,
        }
    }
}

pub(super) fn supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["jpg", "jpeg", "png", "webp"]
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

pub(super) fn file_reference(path: &Path, revision: String) -> LocalImageRef {
    LocalImageRef::File {
        path: path.to_string_lossy().into_owned(),
        revision,
    }
}

pub(super) fn embedded_reference(
    path: &Path,
    picture_index: u32,
    revision: String,
) -> LocalImageRef {
    LocalImageRef::Embedded {
        path: path.to_string_lossy().into_owned(),
        picture_index,
        revision,
    }
}

pub(super) fn inspect_embedded(
    discoverer: &mut discovery::Reader,
    path: &Path,
    revision: String,
) -> Option<LocalImageRef> {
    if let Ok(Some(file)) = read_lofty(path, true) {
        let picture_index =
            best_picture_index(&file, file.primary_tag().or_else(|| file.first_tag()))?;
        return Some(embedded_reference(path, picture_index, revision));
    }
    discoverer
        .read(path)
        .and_then(|metadata| metadata.artwork_index)
        .map(|picture_index| embedded_reference(path, picture_index, revision))
}

pub(super) fn best_picture_index(
    file: &TaggedFile,
    preferred: Option<&lofty::tag::Tag>,
) -> Option<u32> {
    let picture = preferred
        .and_then(|tag| best_picture(tag.pictures()))
        .or_else(|| {
            file.tags()
                .iter()
                .find_map(|tag| best_picture(tag.pictures()))
        })?;
    file.tags()
        .iter()
        .flat_map(|tag| tag.pictures())
        .position(|candidate| std::ptr::eq(candidate, picture))
        .and_then(|index| u32::try_from(index).ok())
}

fn best_picture(pictures: &[Picture]) -> Option<&Picture> {
    pictures
        .iter()
        .find(|picture| picture.pic_type() == PictureType::CoverFront)
        .or_else(|| pictures.first())
}

pub(super) fn read_image(reference: &ArtworkReference) -> SourceResult<ImageBytes> {
    match reference {
        ArtworkReference::File(path) => Ok(ImageBytes {
            bytes: read_bounded(fs::File::open(path).map_err(file_error)?)?,
            content_type: content_type(path),
        }),
        ArtworkReference::Embedded {
            path,
            picture_index,
        } => {
            let index = usize::try_from(*picture_index).map_err(|_| SourceError::NotFound)?;
            if let Some(file) =
                read_lofty(path, true).map_err(|error| SourceError::Other(error.to_string()))?
                && let Some(picture) = file.tags().iter().flat_map(|tag| tag.pictures()).nth(index)
            {
                if picture.data().len() > LOCAL_IMAGE_MAX_BYTES {
                    return Err(SourceError::Other(format!(
                        "Local artwork exceeds {} MiB",
                        LOCAL_IMAGE_MAX_BYTES / (1024 * 1024)
                    )));
                }
                return Ok(ImageBytes {
                    bytes: picture.data().to_vec(),
                    content_type: picture.mime_type().map(|mime| mime.as_str().to_string()),
                });
            }
            discovery::read_image(path, *picture_index)
        }
    }
}

fn read_bounded(mut file: fs::File) -> SourceResult<Vec<u8>> {
    let mut bytes = Vec::new();
    file.by_ref()
        .take((LOCAL_IMAGE_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(file_error)?;
    if bytes.len() > LOCAL_IMAGE_MAX_BYTES {
        return Err(SourceError::Other(format!(
            "Local artwork exceeds {} MiB",
            LOCAL_IMAGE_MAX_BYTES / (1024 * 1024)
        )));
    }
    Ok(bytes)
}

fn content_type(path: &Path) -> Option<String> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => Some("image/jpeg".to_string()),
        Some("png") => Some("image/png".to_string()),
        Some("webp") => Some("image/webp".to_string()),
        _ => None,
    }
}

fn file_error(error: std::io::Error) -> SourceError {
    SourceError::Other(error.to_string())
}
