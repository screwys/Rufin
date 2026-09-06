//! Shared artwork format, selection, and decoding semantics.

use super::{discovery, lofty};
use crate::{ImageBytes, SourceError, SourceResult};
use ::lofty::file::{TaggedFile, TaggedFileExt};
use ::lofty::picture::{Picture, PictureType};
use std::io::{Read, Seek};
use std::path::Path;
const LOCAL_IMAGE_MAX_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["jpg", "jpeg", "png", "webp"]
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

pub(crate) fn image_rank(name: &str) -> usize {
    [
        "cover.jpg",
        "cover.jpeg",
        "cover.png",
        "cover.webp",
        "folder.jpg",
        "folder.jpeg",
        "folder.png",
        "folder.webp",
        "front.jpg",
        "front.jpeg",
        "front.png",
        "front.webp",
        "album.jpg",
        "album.jpeg",
        "album.png",
        "album.webp",
    ]
    .iter()
    .position(|candidate| candidate.eq_ignore_ascii_case(name))
    .unwrap_or(usize::MAX)
}

pub(crate) fn inspect_embedded_input(
    discoverer: &mut discovery::Reader,
    file: &mut (impl Read + Seek),
    uri: &str,
) -> Option<u32> {
    file.rewind().ok()?;
    if let Ok(Some(tagged)) = self::lofty::read_lofty_file(
        &mut *file,
        ::lofty::config::ParseOptions::new().read_cover_art(true),
    ) {
        return best_picture_index(&tagged, tagged.primary_tag().or_else(|| tagged.first_tag()));
    }
    discoverer
        .read_input(file, uri)
        .and_then(|metadata| metadata.artwork_index)
}

pub(crate) fn best_picture_index(
    file: &TaggedFile,
    preferred: Option<&::lofty::tag::Tag>,
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

pub(crate) fn read_image_input(
    discoverer: &mut discovery::Reader,
    file: &mut (impl Read + Seek),
    uri: &str,
    picture_index: u32,
) -> SourceResult<ImageBytes> {
    let index = usize::try_from(picture_index).map_err(|_| SourceError::NotFound)?;
    if let Some(tagged) = self::lofty::read_lofty_file(
        &mut *file,
        ::lofty::config::ParseOptions::new().read_cover_art(true),
    )
    .map_err(|error| SourceError::Other(error.to_string()))?
        && let Some(picture) = tagged
            .tags()
            .iter()
            .flat_map(|tag| tag.pictures())
            .nth(index)
    {
        if picture.data().len() > LOCAL_IMAGE_MAX_BYTES {
            return Err(SourceError::Other(format!(
                "Artwork exceeds {} MiB",
                LOCAL_IMAGE_MAX_BYTES / (1024 * 1024)
            )));
        }
        return Ok(ImageBytes {
            bytes: picture.data().to_vec(),
            content_type: picture.mime_type().map(|mime| mime.as_str().to_string()),
        });
    }
    discovery::read_image_input(discoverer, file, uri, picture_index)
}

pub(crate) fn content_type(path: &Path) -> Option<String> {
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
