//! Shared artwork format, selection, and decoding semantics.

use super::{discovery, lofty};
use crate::{ImageBytes, SourceError, SourceResult};
use ::lofty::file::{TaggedFile, TaggedFileExt};
use ::lofty::picture::{Picture, PictureType};
use std::io::{Read, Seek};
use std::path::Path;
const LOCAL_IMAGE_MAX_BYTES: usize = 32 * 1024 * 1024;
const IMAGE_FORMATS: &[(&str, &str)] = &[
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("png", "image/png"),
    ("webp", "image/webp"),
    ("jxl", "image/jxl"),
    ("gif", "image/gif"),
    ("apng", "image/apng"),
    ("bmp", "image/bmp"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
];

pub(crate) fn supported_image(path: &Path) -> bool {
    image_format(path).is_some()
}

pub(crate) fn image_rank(name: &str) -> usize {
    let path = Path::new(name);
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return usize::MAX;
    };
    ["cover", "folder", "front", "album"]
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case(stem))
        .zip(image_format(path))
        .map_or(usize::MAX, |(name, format)| {
            name * IMAGE_FORMATS.len() + format
        })
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
        .ok()
        .flatten()
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
    image_format(path).map(|format| IMAGE_FORMATS[format].1.to_string())
}

fn image_format(path: &Path) -> Option<usize> {
    let extension = path.extension()?.to_str()?;
    IMAGE_FORMATS
        .iter()
        .position(|(candidate, _)| extension.eq_ignore_ascii_case(candidate))
}
