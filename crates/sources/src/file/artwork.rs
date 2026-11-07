//! Shared artwork format, selection, and decoding semantics.

use super::{discovery, local, lofty, remote::FileSource};
use crate::file::check_cancelled;
use crate::{ImageBytes, LocalImageRef, SourceError, SourceResult};
use ::lofty::file::{TaggedFile, TaggedFileExt};
use ::lofty::picture::{Picture, PictureType};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::Arc;
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
    ranked_image(name, &["cover", "folder", "front", "album"])
}

pub(crate) fn artist_image_rank(name: &str) -> usize {
    ranked_image(
        name,
        &[
            "artist",
            "artist-thumb",
            "artist-fanart",
            "fanart",
            "performer",
            "band",
        ],
    )
}

pub(crate) fn artist_image_stem(artist: &str) -> String {
    let mut stem = String::from("artist-");
    for character in artist.trim().to_lowercase().chars() {
        if character.is_control() || "<>:\"/\\|?*%".contains(character) {
            let mut encoded = [0; 4];
            for byte in character.encode_utf8(&mut encoded).bytes() {
                use std::fmt::Write;
                let _ = write!(stem, "%{byte:02x}");
            }
        } else {
            stem.push(character);
        }
    }
    stem
}

pub(crate) fn artist_image_rank_for(name: &str, artist: &str, allow_generic: bool) -> usize {
    let named = ranked_image(name, &[&artist_image_stem(artist)]);
    if named != usize::MAX {
        return named;
    }
    if allow_generic {
        artist_image_rank(name).saturating_add(IMAGE_FORMATS.len())
    } else {
        usize::MAX
    }
}

/// A supported image in a folder: its file name and its path or location, with the revision
/// the catalog holds for a remote file.
#[derive(Clone, Debug)]
pub(crate) struct FolderImage {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) revision: Option<String>,
}

/// A folder's cover: its best-named image that is not an artist's, or its only such image.
pub(crate) fn folder_cover(images: &[FolderImage]) -> Option<&FolderImage> {
    let covers = images.iter().filter(|image| !is_artist_image(&image.name));
    let only = covers.clone().nth(1).is_none();
    covers
        .map(|image| (image_rank(&image.name), &image.path, image))
        .min_by(|left, right| (left.0, left.1).cmp(&(right.0, right.1)))
        .filter(|(rank, _, _)| *rank != usize::MAX || only)
        .map(|(_, _, image)| image)
}

/// A folder's portrait of `artist`: one named for them, else a generic one when allowed.
pub(crate) fn folder_portrait<'i>(
    images: &'i [FolderImage],
    artist: &str,
    allow_generic: bool,
) -> Option<&'i FolderImage> {
    images
        .iter()
        .map(|image| {
            (
                artist_image_rank_for(&image.name, artist, allow_generic),
                &image.path,
                image,
            )
        })
        .filter(|(rank, _, _)| *rank != usize::MAX)
        .min_by(|left, right| (left.0, left.1).cmp(&(right.0, right.1)))
        .map(|(_, _, image)| image)
}

pub(crate) fn is_artist_image(name: &str) -> bool {
    artist_image_rank(name) != usize::MAX
        || Path::new(name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.to_lowercase().starts_with("artist-"))
}

fn ranked_image(name: &str, names: &[&str]) -> usize {
    let path = Path::new(name);
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return usize::MAX;
    };
    names
        .iter()
        .position(|candidate| candidate.to_lowercase() == stem.to_lowercase())
        .zip(image_format(path))
        .map_or(usize::MAX, |(name, format)| {
            name * IMAGE_FORMATS.len() + format
        })
}

pub(crate) fn inspect_embedded_input(
    discoverer: &mut discovery::Reader,
    file: &mut (impl Read + Seek),
    uri: &str,
) -> (Option<u32>, Vec<ArtistPicture>) {
    if file.rewind().is_err() {
        return (None, Vec::new());
    }
    if let Ok(Some(tagged)) = self::lofty::read_lofty_file(
        &mut *file,
        ::lofty::config::ParseOptions::new().read_cover_art(true),
    ) {
        return (
            best_picture_index(&tagged, tagged.primary_tag().or_else(|| tagged.first_tag())),
            artist_pictures(&tagged),
        );
    }
    let cover = discoverer
        .read_input(file, uri)
        .ok()
        .flatten()
        .and_then(|metadata| metadata.artwork_index);
    (cover, Vec::new())
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

pub(super) fn best_picture(pictures: &[Picture]) -> Option<&Picture> {
    pictures
        .iter()
        .find(|picture| picture.pic_type() == PictureType::CoverFront)
        .or_else(|| pictures.iter().find(|picture| !is_artist_picture(picture)))
}

fn is_artist_picture(picture: &Picture) -> bool {
    matches!(
        picture.pic_type(),
        PictureType::Artist | PictureType::LeadArtist | PictureType::Band
    )
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ArtistPicture {
    pub(crate) index: u32,
    pub(crate) artist: Option<String>,
}

pub(crate) fn artist_pictures(file: &TaggedFile) -> Vec<ArtistPicture> {
    file.tags()
        .iter()
        .flat_map(|tag| tag.pictures())
        .enumerate()
        .filter(|(_, picture)| is_artist_picture(picture))
        .filter_map(|(index, picture)| {
            Some(ArtistPicture {
                index: u32::try_from(index).ok()?,
                artist: picture
                    .description()
                    .and_then(|description| description.strip_prefix("Artist: "))
                    .map(|name| name.trim().to_string()),
            })
        })
        .collect()
}

pub(crate) fn artist_picture_index(
    pictures: &[ArtistPicture],
    artist: &str,
    allow_generic: bool,
) -> Option<u32> {
    let name = artist.trim().to_lowercase();
    pictures
        .iter()
        .find(|picture| {
            picture
                .artist
                .as_ref()
                .is_some_and(|artist| artist.to_lowercase() == name)
        })
        .or_else(|| {
            allow_generic
                .then(|| pictures.iter().find(|picture| picture.artist.is_none()))
                .flatten()
        })
        .map(|picture| picture.index)
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

/// The run of `tracks` read for `owner`; the reads return them grouped in page order.
fn owned<'t>(
    tracks: &'t [library::LocalArtworkCandidate],
    owner: &str,
) -> &'t [library::LocalArtworkCandidate] {
    let start = tracks.partition_point(|track| track.owner.as_str() < owner);
    let end = start + tracks[start..].partition_point(|track| track.owner == owner);
    &tracks[start..end]
}

/// A folder lookup, read once per pass.
async fn cached<T: Clone>(
    cache: &mut HashMap<String, T>,
    prefix: &str,
    read: impl Future<Output = SourceResult<T>>,
) -> SourceResult<T> {
    if let Some(found) = cache.get(prefix) {
        return Ok(found.clone());
    }
    let found = read.await?;
    cache.insert(prefix.to_owned(), found.clone());
    Ok(found)
}

/// Selects file artwork while each source owns file access and path semantics.
#[derive(Clone, Copy)]
pub(crate) enum ArtworkFiles<'a> {
    Local,
    Documents,
    Remote(&'a FileSource),
}

impl<'a> ArtworkFiles<'a> {
    fn for_path(&self, path: &str) -> ArtworkFiles<'a> {
        if path.starts_with("rufin-document://") || path.starts_with("content://") {
            return Self::Documents;
        }
        match *self {
            Self::Documents => Self::Local,
            Self::Local => Self::Local,
            Self::Remote(source) if source.document_access().is_some() => Self::Local,
            Self::Remote(source) => Self::Remote(source),
        }
    }

    /// Album, track and artist pictures of the albums with tracks in `directories` or the
    /// directory below, and of the artists those tracks credit; every album when None. Each
    /// page of 128 albums is two reads and one Write; an artist's tracks are read in pages
    /// until a picture is found, and each 128 artists are one Write.
    pub(crate) async fn acquire(
        &self,
        observation: &mut library::Observation<'_>,
        directories: Option<&[String]>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let catalog = observation.catalog;
        let source_id = observation.source_id;
        let distinct = catalog.distinct_track_covers();
        // Folder lookups, shared by the pass: a folder's images, and whether it holds one album.
        let mut listings = HashMap::new();
        let mut single = HashMap::new();
        let albums = catalog
            .source_artwork_albums(source_id, directories)
            .await?;
        for albums in albums.chunks(128) {
            let tracks = catalog
                .source_artwork_album_tracks(source_id, albums)
                .await?;
            let (mut images, mut track_art, mut album_art) = (Vec::new(), Vec::new(), Vec::new());
            for album in albums {
                check_cancelled(cancelled)?;
                let tracks = owned(&tracks, album);
                let mut cover = None;
                'tracks: for track in tracks {
                    // The track's folder, or the folder above it when that holds one album.
                    for (slot, prefix) in self.directories(track)?.into_iter().take(2).enumerate() {
                        if slot == 1
                            && !cached(&mut single, &prefix, async {
                                catalog
                                    .source_artwork_directory_is_single_album(source_id, &prefix)
                                    .await
                                    .map_err(SourceError::from)
                            })
                            .await?
                        {
                            continue;
                        }
                        let images = cached(&mut listings, &prefix, async {
                            self.folder_images(catalog, source_id, &prefix).await
                        })
                        .await?;
                        cover =
                            folder_cover(&images).and_then(|image| self.image(source_id, image));
                        if cover.is_some() {
                            break 'tracks;
                        }
                    }
                }
                if cover.is_none() || distinct {
                    for track in tracks {
                        check_cancelled(cancelled)?;
                        let (index, _, observed) = self.embedded_file_images(track)?;
                        if let Some((index, pictures)) = observed {
                            images.push((track.path.clone(), index, pictures));
                        }
                        let image = index.map(|picture_index| LocalImageRef::Embedded {
                            source_id: crate::SourceId::new(source_id),
                            path: track.path.clone(),
                            picture_index,
                            revision: track.revision.clone().unwrap_or_default(),
                        });
                        let found = image.is_some();
                        track_art.push((
                            track.object_id.clone(),
                            image.map(|image| image.artwork_binding()),
                        ));
                        if found && !distinct {
                            break;
                        }
                    }
                }
                album_art.push((album.clone(), cover.map(|image| image.artwork_binding())));
            }
            check_cancelled(cancelled)?;
            observation
                .set_source_artwork(&images, &[("track", &track_art), ("album", &album_art)])
                .await?;
        }
        let artists = catalog
            .source_artwork_artists(source_id, directories)
            .await?;
        for artists in artists.chunks(128) {
            let (mut images, mut artist_art) = (Vec::new(), Vec::new());
            for (artist, name) in artists {
                // Folder portraits in any folder above a track come before embedded pictures.
                let mut visited = HashSet::new();
                let mut image = None;
                let mut after = 0;
                'folders: loop {
                    let page = catalog
                        .source_artwork_artist_tracks(source_id, artist, after)
                        .await?;
                    let Some(last) = page.last() else { break };
                    after = last.key;
                    for track in &page {
                        check_cancelled(cancelled)?;
                        for prefix in self.directories(track)? {
                            if !visited.insert(prefix.clone()) {
                                continue;
                            }
                            let images = cached(&mut listings, &prefix, async {
                                self.folder_images(catalog, source_id, &prefix).await
                            })
                            .await?;
                            image = self
                                .directory_portrait(
                                    catalog, source_id, &prefix, &images, artist, name,
                                )
                                .await?;
                            if image.is_some() {
                                break 'folders;
                            }
                        }
                    }
                }
                let mut after = 0;
                'embedded: while image.is_none() {
                    let page = catalog
                        .source_artwork_artist_tracks(source_id, artist, after)
                        .await?;
                    let Some(last) = page.last() else { break };
                    after = last.key;
                    for track in &page {
                        check_cancelled(cancelled)?;
                        let (found, observed) =
                            self.embedded_artist_image(source_id, track, name)?;
                        if let Some((index, pictures)) = observed {
                            images.push((track.path.clone(), index, pictures));
                        }
                        if found.is_some() {
                            image = found;
                            break 'embedded;
                        }
                    }
                }
                artist_art.push((artist.clone(), image.map(|image| image.artwork_binding())));
            }
            check_cancelled(cancelled)?;
            observation
                .set_source_artwork(&images, &[("artist", &artist_art)])
                .await?;
        }
        if matches!(self, Self::Local) && directories.is_none() {
            observation.clear_partial_file_image_observations().await?;
        }
        Ok(())
    }

    fn directories(&self, track: &library::LocalArtworkCandidate) -> SourceResult<Vec<String>> {
        match self.for_path(&track.path) {
            Self::Local => Ok(Path::new(&track.path)
                .ancestors()
                .skip(1)
                .filter(|directory| {
                    track.root.as_ref().is_some_and(|root| {
                        directory.starts_with(root)
                            || *root == track.path && Path::new(root).parent() == Some(*directory)
                    })
                })
                .map(|directory| {
                    format!(
                        "{}{}",
                        directory
                            .to_string_lossy()
                            .trim_end_matches(std::path::is_separator),
                        std::path::MAIN_SEPARATOR
                    )
                })
                .collect()),
            Self::Documents => {
                if track.path.starts_with("content://") {
                    return Ok(Vec::new());
                }
                let mut uri = url::Url::parse(&track.path).map_err(|_| SourceError::NotFound)?;
                uri.set_fragment(None);
                let mut directories = Vec::new();
                loop {
                    uri.path_segments_mut()
                        .map_err(|_| SourceError::NotFound)?
                        .pop_if_empty()
                        .pop()
                        .push("");
                    directories.push(uri.to_string());
                    if uri.path() == "/" {
                        break;
                    }
                }
                Ok(directories)
            }
            Self::Remote(source) => {
                let relative = source.relative(&track.path)?;
                let parent = relative.rsplit_once('/').map_or("", |(parent, _)| parent);
                std::iter::successors(Some(parent), |directory| {
                    (!directory.is_empty())
                        .then(|| directory.rsplit_once('/').map_or("", |(parent, _)| parent))
                })
                .map(|directory| {
                    Ok(format!(
                        "{}/",
                        source.location(directory)?.trim_end_matches('/')
                    ))
                })
                .collect()
            }
        }
    }

    async fn folder_images(
        &self,
        catalog: &library::Catalog,
        source_id: &str,
        prefix: &str,
    ) -> SourceResult<Arc<[FolderImage]>> {
        Ok(match self.for_path(prefix) {
            Self::Local => local::artwork::directory_images(Path::new(prefix)),
            Self::Documents | Self::Remote(_) => {
                crate::file::remote::artwork::directory_images(catalog, source_id, prefix).await?
            }
        }
        .into())
    }

    fn image(&self, source_id: &str, image: &FolderImage) -> Option<LocalImageRef> {
        match self.for_path(&image.path) {
            Self::Local => {
                let path = Path::new(&image.path);
                local::artwork::revision(path)
                    .map(|revision| local::artwork::file_reference(source_id, path, revision))
            }
            Self::Documents | Self::Remote(_) => Some(LocalImageRef::File {
                source_id: crate::SourceId::new(source_id),
                path: image.path.clone(),
                revision: image.revision.clone().unwrap_or_default(),
            }),
        }
    }

    /// A portrait of the artist in a folder: one named for them, or a generic one when every
    /// track in the folder is theirs.
    async fn directory_portrait(
        &self,
        catalog: &library::Catalog,
        source_id: &str,
        prefix: &str,
        images: &[FolderImage],
        artist: &str,
        name: &str,
    ) -> SourceResult<Option<LocalImageRef>> {
        let Some(found) = folder_portrait(images, name, true) else {
            return Ok(None);
        };
        if artist_image_rank_for(&found.name, name, false) == usize::MAX
            && !catalog
                .source_artwork_directory_has_artist(source_id, prefix, artist)
                .await?
        {
            return Ok(None);
        }
        Ok(self.image(source_id, found))
    }

    fn embedded_file_images(
        &self,
        track: &library::LocalArtworkCandidate,
    ) -> SourceResult<(
        Option<u32>,
        Vec<ArtistPicture>,
        Option<(Option<i64>, String)>,
    )> {
        let owner = self.for_path(&track.path);
        let revision = match owner {
            // A file deleted since it was indexed has no pictures; the walk withdraws its track.
            Self::Local => match local::artwork::revision(Path::new(&track.path)) {
                Some(revision) => Some(revision),
                None => return Ok((None, Vec::new(), None)),
            },
            Self::Remote(_) | Self::Documents => track.revision.clone(),
        };
        if let Some(cached) = track.artist_pictures.as_deref().filter(|_| {
            !matches!(owner, Self::Local)
                || revision == track.revision && track.picture_index.is_some()
        }) {
            return Ok((
                track.picture_index.and_then(|i| u32::try_from(i).ok()),
                serde_json::from_str(cached)?,
                None,
            ));
        }
        if !matches!(owner, Self::Local) {
            return Ok((
                track.picture_index.and_then(|i| u32::try_from(i).ok()),
                Vec::new(),
                None,
            ));
        }
        let mut file = std::fs::File::open(&track.path)
            .map_err(|error| SourceError::Other(error.to_string()))?;
        let uri = url::Url::from_file_path(&track.path).map_err(|_| SourceError::NotFound)?;
        let (cover, pictures) =
            inspect_embedded_input(&mut discovery::Reader::default(), &mut file, uri.as_str());
        // NULL predates cover inspection; -1 remembers an inspected file without a cover.
        let observed = Some((
            Some(cover.map_or(-1, i64::from)),
            serde_json::to_string(&pictures)?,
        ));
        Ok((cover, pictures, observed))
    }

    fn embedded_artist_image(
        &self,
        source_id: &str,
        track: &library::LocalArtworkCandidate,
        artist: &str,
    ) -> SourceResult<(Option<LocalImageRef>, Option<(Option<i64>, String)>)> {
        let (_, pictures, observed) = self.embedded_file_images(track)?;
        Ok((
            artist_picture_index(&pictures, artist, track.primary_artist)
                .zip(track.revision.clone())
                .map(|(picture_index, revision)| LocalImageRef::Embedded {
                    source_id: crate::SourceId::new(source_id),
                    path: track.path.clone(),
                    revision,
                    picture_index,
                }),
            observed,
        ))
    }
}
