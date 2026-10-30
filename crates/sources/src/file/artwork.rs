//! Shared artwork format, selection, and decoding semantics.

use super::{discovery, local, lofty, remote::FileSource};
use crate::file::check_cancelled;
use crate::{ImageBytes, LocalImageRef, SourceError, SourceResult};
use ::lofty::file::{TaggedFile, TaggedFileExt};
use ::lofty::picture::{Picture, PictureType};
use std::collections::HashMap;
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
    /// page of 128 albums or artists is two reads and one Write.
    pub(crate) async fn acquire(
        &self,
        observation: &mut library::Observation<'_>,
        directories: Option<&[String]>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let catalog = observation.catalog;
        let source_id = observation.source_id;
        let distinct = catalog.distinct_track_covers();
        // Folder lookups, shared by the pass: a folder's cover, and whether it holds one album.
        let mut covers = HashMap::new();
        let mut single = HashMap::new();
        let mut after = String::new();
        loop {
            let albums = catalog
                .source_artwork_album_page(source_id, &after, directories)
                .await?;
            let Some(last) = albums.last() else { break };
            after.clone_from(last);
            let tracks = catalog
                .source_artwork_album_tracks(source_id, &albums)
                .await?;
            let (mut images, mut track_art, mut album_art) = (Vec::new(), Vec::new(), Vec::new());
            for album in &albums {
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
                        cover = cached(&mut covers, &prefix, async {
                            self.folder_image(catalog, source_id, &prefix, None).await
                        })
                        .await?;
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
            if albums.len() < 128 {
                break;
            }
        }
        let mut after = String::new();
        loop {
            let artists = catalog
                .source_artwork_artist_page(source_id, &after, directories)
                .await?;
            let Some((last, _)) = artists.last() else {
                break;
            };
            after.clone_from(last);
            let ids = artists
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>();
            let tracks = catalog
                .source_artwork_artist_tracks(source_id, &ids)
                .await?;
            let (mut images, mut artist_art) = (Vec::new(), Vec::new());
            for (artist, name) in &artists {
                check_cancelled(cancelled)?;
                let tracks = owned(&tracks, artist);
                // Folder portraits in any folder above a track come before embedded pictures.
                let mut portraits = HashMap::new();
                let mut image = None;
                'folders: for track in tracks {
                    check_cancelled(cancelled)?;
                    for prefix in self.directories(track)? {
                        image = cached(&mut portraits, &prefix, async {
                            self.directory_artist_image(catalog, source_id, &prefix, artist, name)
                                .await
                        })
                        .await?;
                        if image.is_some() {
                            break 'folders;
                        }
                    }
                }
                if image.is_none() {
                    for track in tracks {
                        check_cancelled(cancelled)?;
                        let (found, observed) =
                            self.embedded_artist_image(source_id, track, name)?;
                        if let Some((index, pictures)) = observed {
                            images.push((track.path.clone(), index, pictures));
                        }
                        if found.is_some() {
                            image = found;
                            break;
                        }
                    }
                }
                artist_art.push((artist.clone(), image.map(|image| image.artwork_binding())));
            }
            check_cancelled(cancelled)?;
            observation
                .set_source_artwork(&images, &[("artist", &artist_art)])
                .await?;
            if artists.len() < 128 {
                break;
            }
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
                    track
                        .root
                        .as_ref()
                        .is_some_and(|root| directory.starts_with(root))
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

    async fn folder_image(
        &self,
        catalog: &library::Catalog,
        source_id: &str,
        prefix: &str,
        artist: Option<&str>,
    ) -> SourceResult<Option<LocalImageRef>> {
        match self.for_path(prefix) {
            Self::Local => {
                let directory = Path::new(prefix);
                let path = match artist {
                    Some(name) => local::artwork::directory_artist_image_for(directory, name, true),
                    None => local::artwork::directory_image(directory),
                };
                Ok(path.and_then(|path| {
                    local::artwork::revision(&path)
                        .map(|revision| local::artwork::file_reference(source_id, &path, revision))
                }))
            }
            Self::Documents => {
                crate::file::remote::artwork::directory_image_by_role(
                    catalog,
                    source_id,
                    prefix,
                    artist.map(|name| (name, true)),
                )
                .await
            }
            Self::Remote(source) => match artist {
                Some(name) => {
                    source
                        .directory_artist_image_for(catalog, source_id, prefix, name, true)
                        .await
                }
                None => source.directory_image(catalog, source_id, prefix).await,
            },
        }
    }

    async fn directory_artist_image(
        &self,
        catalog: &library::Catalog,
        source_id: &str,
        prefix: &str,
        artist: &str,
        name: &str,
    ) -> SourceResult<Option<LocalImageRef>> {
        let found = self
            .folder_image(catalog, source_id, prefix, Some(name))
            .await?;
        if let Some(LocalImageRef::File { path, .. }) = &found {
            let filename = match self.for_path(path) {
                Self::Local => Path::new(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                Self::Documents => {
                    let uri = url::Url::parse(path).map_err(|_| SourceError::NotFound)?;
                    percent_encoding::percent_decode_str(
                        uri.path().rsplit('/').next().unwrap_or_default(),
                    )
                    .decode_utf8()
                    .map_err(|_| SourceError::NotFound)?
                    .into_owned()
                }
                Self::Remote(source) => {
                    let relative = source.relative(path)?;
                    let relative = if source.document_access().is_some() {
                        crate::file::documents::display_path(&relative)
                    } else {
                        relative
                    };
                    relative.rsplit('/').next().unwrap_or_default().to_string()
                }
            };
            if artist_image_rank_for(&filename, name, false) == usize::MAX
                && !catalog
                    .source_artwork_directory_has_artist(source_id, prefix, artist)
                    .await?
            {
                return Ok(None);
            }
        }
        Ok(found)
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
