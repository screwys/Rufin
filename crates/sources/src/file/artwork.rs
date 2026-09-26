//! Shared artwork format, selection, and decoding semantics.

use super::{discovery, local, lofty, remote::RemoteSource};
use crate::{ImageBytes, LocalImageRef, SourceError, SourceResult};
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

/// Selects file artwork while each source owns file access and path semantics.
pub(crate) enum ArtworkFiles<'a> {
    Local,
    Remote(&'a RemoteSource),
}

impl ArtworkFiles<'_> {
    pub(crate) async fn stage(
        &self,
        database: &library::Database,
        scan: &mut library::Scan,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        scan.clear_local_artwork_candidates().await?;
        let distinct = database.distinct_track_covers();
        let mut after_album = String::new();
        let mut discoverer = discovery::Reader::default();
        loop {
            let albums = scan.local_artwork_album_page(&after_album).await?;
            if albums.is_empty() {
                break;
            }
            for album in albums {
                if cancelled() {
                    return Err(SourceError::Cancelled);
                }
                after_album.clone_from(&album);
                let mut group = None;
                let mut after = None;
                let mut directories: [Option<(String, Option<LocalImageRef>)>; 2] = [None, None];
                'tracks: loop {
                    let tracks = scan
                        .local_artwork_track_page(&album, after.as_ref())
                        .await?;
                    if tracks.is_empty() {
                        break;
                    }
                    for track in tracks {
                        if cancelled() {
                            return Err(SourceError::Cancelled);
                        }
                        if group.is_none() {
                            for (slot, prefix) in
                                self.directories(&track)?.into_iter().take(2).enumerate()
                            {
                                let cached = &mut directories[slot];
                                if cached
                                    .as_ref()
                                    .is_none_or(|(previous, _)| previous != &prefix)
                                {
                                    // A sibling cover also serves CUE and standalone copies in this folder.
                                    let image = if slot == 0
                                        || scan
                                            .local_artwork_directory_is_single_album(&prefix)
                                            .await?
                                    {
                                        self.folder_image(scan, &prefix, None).await?
                                    } else {
                                        None
                                    };
                                    *cached = Some((prefix, image));
                                }
                                group = cached.as_ref().and_then(|(_, image)| image.clone());
                                if group.is_some() {
                                    break;
                                }
                            }
                        }
                        let embedded = if distinct || group.is_none() {
                            self.embedded_album_image(scan.source_id(), &mut discoverer, &track)
                        } else {
                            None
                        };
                        if group.is_none() {
                            group.clone_from(&embedded);
                        }
                        let group_bytes = group.as_ref().map(serde_json::to_vec).transpose()?;
                        let track_bytes = distinct
                            .then_some(embedded.as_ref())
                            .flatten()
                            .map(serde_json::to_vec)
                            .transpose()?;
                        if group_bytes.is_some() || track_bytes.is_some() {
                            scan.write_local_artwork_candidate(
                                &album,
                                &track.object_id,
                                group_bytes.as_deref(),
                                track_bytes.as_deref(),
                            )
                            .await?;
                        }
                        after = Some(track);
                        if !distinct && group.is_some() {
                            break 'tracks;
                        }
                    }
                }
            }
        }
        self.stage_artist_artwork(scan, cancelled).await
    }

    fn embedded_album_image(
        &self,
        source_id: &str,
        discoverer: &mut discovery::Reader,
        track: &library::LocalArtworkCandidate,
    ) -> Option<LocalImageRef> {
        match self {
            Self::Local => {
                let path = Path::new(&track.path);
                local::artwork::inspect_embedded(
                    source_id,
                    discoverer,
                    path,
                    local::artwork::revision(path)?,
                )
            }
            Self::Remote(_) => Some(LocalImageRef::Embedded {
                source_id: crate::SourceId::new(source_id),
                path: track.path.clone(),
                revision: track.revision.clone().unwrap_or_default(),
                picture_index: u32::try_from(track.picture_index?).ok()?,
            }),
        }
    }

    async fn stage_artist_artwork(
        &self,
        scan: &mut library::Scan,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let mut after_artist = String::new();
        loop {
            let artists = scan.local_artwork_artist_page(&after_artist).await?;
            if artists.is_empty() {
                break;
            }
            for (artist, name) in artists {
                after_artist.clone_from(&artist);
                let mut image = None;
                // Folder portraits take priority over embedded pictures across all tracks.
                for embedded in [false, true] {
                    let mut after = String::new();
                    let mut directories: Vec<Option<(String, Option<LocalImageRef>)>> = Vec::new();
                    loop {
                        let tracks = scan
                            .local_artwork_artist_track_page(&artist, &after)
                            .await?;
                        if tracks.is_empty() {
                            break;
                        }
                        for track in tracks {
                            if cancelled() {
                                return Err(SourceError::Cancelled);
                            }
                            after.clone_from(&track.object_id);
                            if embedded {
                                image = self.embedded_artist_image(scan, &track, &name).await?;
                            } else {
                                for (slot, prefix) in
                                    self.directories(&track)?.into_iter().enumerate()
                                {
                                    if slot == directories.len() {
                                        directories.push(None);
                                    }
                                    if directories[slot]
                                        .as_ref()
                                        .is_none_or(|(previous, _)| previous != &prefix)
                                    {
                                        let found = self
                                            .directory_artist_image(scan, &prefix, &artist, &name)
                                            .await?;
                                        directories[slot] = Some((prefix, found));
                                    }
                                    image = directories[slot]
                                        .as_ref()
                                        .and_then(|(_, image)| image.clone());
                                    if image.is_some() {
                                        break;
                                    }
                                }
                            }
                            if image.is_some() {
                                break;
                            }
                        }
                        if image.is_some() {
                            break;
                        }
                    }
                    if image.is_some() {
                        break;
                    }
                }
                if let Some(image) = image {
                    scan.write_local_artist_artwork(&artist, &serde_json::to_vec(&image)?)
                        .await?;
                }
            }
        }
        Ok(())
    }

    fn directories(&self, track: &library::LocalArtworkCandidate) -> SourceResult<Vec<String>> {
        match self {
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
        scan: &library::Scan,
        prefix: &str,
        artist: Option<&str>,
    ) -> SourceResult<Option<LocalImageRef>> {
        match self {
            Self::Local => {
                let directory = Path::new(prefix);
                let path = match artist {
                    Some(name) => local::artwork::directory_artist_image_for(directory, name, true),
                    None => local::artwork::directory_image(directory),
                };
                Ok(path.and_then(|path| {
                    local::artwork::revision(&path).map(|revision| {
                        local::artwork::file_reference(scan.source_id(), &path, revision)
                    })
                }))
            }
            Self::Remote(source) => match artist {
                Some(name) => {
                    source
                        .directory_artist_image_for(scan, prefix, name, true)
                        .await
                }
                None => source.directory_image(scan, prefix).await,
            },
        }
    }

    async fn directory_artist_image(
        &self,
        scan: &library::Scan,
        prefix: &str,
        artist: &str,
        name: &str,
    ) -> SourceResult<Option<LocalImageRef>> {
        let found = self.folder_image(scan, prefix, Some(name)).await?;
        if let Some(LocalImageRef::File { path, .. }) = &found {
            let filename = match self {
                Self::Local => Path::new(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                Self::Remote(source) => source
                    .relative(path)?
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            };
            if artist_image_rank_for(&filename, name, false) == usize::MAX
                && !scan
                    .local_artwork_directory_has_artist(prefix, artist)
                    .await?
            {
                return Ok(None);
            }
        }
        Ok(found)
    }

    async fn embedded_artist_image(
        &self,
        scan: &mut library::Scan,
        track: &library::LocalArtworkCandidate,
        artist: &str,
    ) -> SourceResult<Option<LocalImageRef>> {
        let revision = match self {
            Self::Local => local::artwork::revision(Path::new(&track.path)),
            Self::Remote(_) => Some(track.revision.clone().unwrap_or_default()),
        };
        let pictures = match track
            .artist_pictures
            .as_deref()
            .filter(|_| matches!(self, Self::Remote(_)) || revision == track.revision)
        {
            Some(cached) => serde_json::from_str(cached)?,
            None if matches!(self, Self::Local) => {
                let pictures = local::artwork::inspect_artist_pictures(Path::new(&track.path));
                scan.record_artist_pictures(&track.path, &serde_json::to_string(&pictures)?)
                    .await?;
                pictures
            }
            None => Vec::new(),
        };
        Ok(
            artist_picture_index(&pictures, artist, track.primary_artist)
                .zip(revision)
                .map(|(picture_index, revision)| LocalImageRef::Embedded {
                    source_id: crate::SourceId::new(scan.source_id()),
                    path: track.path.clone(),
                    revision,
                    picture_index,
                }),
        )
    }
}
