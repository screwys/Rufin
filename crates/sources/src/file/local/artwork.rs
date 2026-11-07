use lofty::file::TaggedFileExt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::LocalImageRef;

use crate::{ImageBytes, SourceError, SourceResult};

use crate::file::artwork::{
    FolderImage, best_picture_index, content_type, folder_cover, folder_portrait, read_image_input,
    supported_image,
};
use crate::file::discovery;
use crate::file::lofty::read_lofty;

const LOCAL_IMAGE_MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) enum ArtworkReference {
    File(PathBuf),
    Embedded { path: PathBuf, picture_index: u32 },
}

impl ArtworkReference {
    pub(crate) fn path(&self) -> &Path {
        match self {
            Self::File(path) | Self::Embedded { path, .. } => path,
        }
    }
}

/// The supported images in `directory`.
pub(crate) fn directory_images(directory: &Path) -> Vec<FolderImage> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| supported_image(path) && path.is_file())
        .map(|path| FolderImage {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path: path.to_string_lossy().into_owned(),
            revision: None,
        })
        .collect()
}

pub(crate) fn directory_image(directory: &Path) -> Option<PathBuf> {
    folder_cover(&directory_images(directory)).map(|image| PathBuf::from(&image.path))
}

pub(crate) fn directory_artist_image_for(
    directory: &Path,
    artist: &str,
    allow_generic: bool,
) -> Option<PathBuf> {
    folder_portrait(&directory_images(directory), artist, allow_generic)
        .map(|image| PathBuf::from(&image.path))
}

pub(crate) fn file_reference(source_id: &str, path: &Path, revision: String) -> LocalImageRef {
    LocalImageRef::File {
        source_id: crate::SourceId::new(source_id),
        path: path.to_string_lossy().into_owned(),
        revision,
    }
}

pub(crate) fn embedded_reference(
    source_id: &str,
    path: &Path,
    picture_index: u32,
    revision: String,
) -> LocalImageRef {
    LocalImageRef::Embedded {
        source_id: crate::SourceId::new(source_id),
        path: path.to_string_lossy().into_owned(),
        picture_index,
        revision,
    }
}

pub(crate) fn inspect_embedded(
    source_id: &str,
    discoverer: &mut discovery::Reader,
    path: &Path,
    revision: String,
) -> Option<LocalImageRef> {
    if let Ok(Some(file)) = read_lofty(path, true) {
        let picture_index =
            best_picture_index(&file, file.primary_tag().or_else(|| file.first_tag()))?;
        return Some(embedded_reference(source_id, path, picture_index, revision));
    }
    discoverer
        .read(path)
        .and_then(|metadata| metadata.artwork_index)
        .map(|picture_index| embedded_reference(source_id, path, picture_index, revision))
}

pub(crate) fn read_image(reference: &ArtworkReference) -> SourceResult<ImageBytes> {
    match reference {
        ArtworkReference::File(path) => Ok(ImageBytes {
            bytes: read_bounded(fs::File::open(path).map_err(file_error)?)?,
            content_type: content_type(path),
        }),
        ArtworkReference::Embedded {
            path,
            picture_index,
        } => {
            let mut file = fs::File::open(path).map_err(file_error)?;
            let uri = url::Url::from_file_path(path).map_err(|_| SourceError::NotFound)?;
            read_image_input(
                &mut discovery::Reader::default(),
                &mut file,
                uri.as_str(),
                *picture_index,
            )
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

fn file_error(error: std::io::Error) -> SourceError {
    SourceError::Other(error.to_string())
}

pub(crate) fn revision(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(format!("{}-{}", metadata.len(), modified.as_nanos()))
}
