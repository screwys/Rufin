//! Shared artwork format, selection, and decoding semantics.

use super::{discovery, local, lofty, remote::FileSource};
use crate::file::check_cancelled;
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

    pub(crate) async fn acquire(
        &self,
        catalog: &mut library::Catalog<'_>,
        progress: &(dyn Fn(crate::SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let database = catalog.database;
        let source_id = catalog.source_id;
        let stamp = catalog.observed;
        let mut completed = 0;
        let total = database.source_artwork_count(source_id, stamp).await?;
        let report = |completed| {
            progress(crate::SourceReadProgress {
                stage: crate::SourceReadStage::Artwork,
                completed,
                total: Some(total),
            })
        };
        report(completed);
        let mut after_album = String::new();
        loop {
            let albums = database
                .source_artwork_album_page(source_id, &after_album, stamp)
                .await?;
            if albums.is_empty() {
                break;
            }
            for album in albums {
                check_cancelled(cancelled)?;
                after_album.clone_from(&album);
                let mut after = None;
                let mut folder_cover = None;
                let mut directories: [Option<(String, Option<LocalImageRef>)>; 2] = [None, None];
                loop {
                    let tracks = database
                        .source_artwork_track_page(source_id, &album, after.as_ref())
                        .await?;
                    if tracks.is_empty() {
                        break;
                    }
                    for track in tracks {
                        check_cancelled(cancelled)?;
                        for (slot, prefix) in
                            self.directories(&track)?.into_iter().take(2).enumerate()
                        {
                            let cached = &mut directories[slot];
                            if cached
                                .as_ref()
                                .is_none_or(|(previous, _)| previous != &prefix)
                            {
                                let image = if slot == 0
                                    || database
                                        .source_artwork_directory_is_single_album(
                                            source_id, &prefix,
                                        )
                                        .await?
                                {
                                    self.folder_image(database, source_id, &prefix, None)
                                        .await?
                                } else {
                                    None
                                };
                                *cached = Some((prefix, image));
                            }
                            folder_cover = cached.as_ref().and_then(|(_, image)| image.clone());
                            if folder_cover.is_some() {
                                break;
                            }
                        }
                        after = Some(track);
                        if folder_cover.is_some() {
                            break;
                        }
                    }
                    if folder_cover.is_some() {
                        break;
                    }
                }
                if folder_cover.is_none() || database.distinct_track_covers() {
                    let mut after = None;
                    let mut found = false;
                    loop {
                        let tracks = database
                            .source_artwork_track_page(source_id, &album, after.as_ref())
                            .await?;
                        if tracks.is_empty() {
                            break;
                        }
                        let mut bindings = Vec::new();
                        let mut inspected = Vec::new();
                        for track in tracks {
                            check_cancelled(cancelled)?;
                            let (index, _, observed) = self.embedded_file_images(&track)?;
                            if let Some((cover, pictures)) = observed {
                                inspected.push((track.path.clone(), cover, pictures));
                            }
                            let image = index.map(|picture_index| LocalImageRef::Embedded {
                                source_id: crate::SourceId::new(source_id),
                                path: track.path.clone(),
                                picture_index,
                                revision: track.revision.clone().unwrap_or_default(),
                            });
                            found |= image.is_some();
                            bindings.push((
                                track.object_id.clone(),
                                image.map(|image| image.artwork_binding()),
                            ));
                            after = Some(track);
                            if found && !database.distinct_track_covers() {
                                break;
                            }
                        }
                        catalog.record_source_file_images(&inspected).await?;
                        check_cancelled(cancelled)?;
                        catalog.set_source_artwork("track", &bindings).await?;
                        if found && !database.distinct_track_covers() {
                            break;
                        }
                    }
                }
                check_cancelled(cancelled)?;
                check_cancelled(cancelled)?;
                catalog
                    .set_source_artwork(
                        "album",
                        &[(album, folder_cover.map(|image| image.artwork_binding()))],
                    )
                    .await?;
                completed += 1;
                report(completed);
            }
        }
        self.acquire_artist_artwork(catalog, &mut completed, &report, cancelled)
            .await?;
        if matches!(self, Self::Local) {
            catalog.clear_partial_file_image_observations().await?;
        }
        Ok(())
    }

    async fn acquire_artist_artwork(
        &self,
        catalog: &mut library::Catalog<'_>,
        completed: &mut usize,
        report: &(dyn Fn(usize) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let database = catalog.database;
        let source_id = catalog.source_id;
        let stamp = catalog.observed;
        let mut after_artist = String::new();
        loop {
            let artists = database
                .source_artwork_artist_page(source_id, &after_artist, stamp)
                .await?;
            if artists.is_empty() {
                break;
            }
            let mut bindings = Vec::new();
            let result: SourceResult<()> = async {
                for (artist, name) in artists {
                    after_artist.clone_from(&artist);
                    let mut image = None;
                    // Folder portraits take priority over embedded pictures across all tracks.
                    for embedded in [false, true] {
                        let mut after = String::new();
                        let mut directories: Vec<Option<(String, Option<LocalImageRef>)>> =
                            Vec::new();
                        loop {
                            let tracks = database
                                .source_artwork_artist_track_page(source_id, &artist, &after)
                                .await?;
                            if tracks.is_empty() {
                                break;
                            }
                            let mut pictures = Vec::new();
                            let result: SourceResult<()> = async {
                                for track in tracks {
                                    check_cancelled(cancelled)?;
                                    after.clone_from(&track.object_id);
                                    if embedded {
                                        let (found, observed) =
                                            self.embedded_artist_image(source_id, &track, &name)?;
                                        image = found;
                                        if let Some((cover, observed)) = observed {
                                            pictures.push((track.path.clone(), cover, observed));
                                        }
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
                                                    .directory_artist_image(
                                                        database, source_id, &prefix, &artist,
                                                        &name,
                                                    )
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
                                Ok(())
                            }
                            .await;
                            catalog.record_source_file_images(&pictures).await?;
                            result?;
                            if image.is_some() {
                                break;
                            }
                        }
                        if image.is_some() {
                            break;
                        }
                    }
                    bindings.push((artist, image.map(|image| image.artwork_binding())));
                    *completed += 1;
                    report(*completed);
                }
                Ok(())
            }
            .await;
            check_cancelled(cancelled)?;
            check_cancelled(cancelled)?;
            catalog.set_source_artwork("artist", &bindings).await?;
            result?;
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
        database: &library::Database,
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
                    database,
                    source_id,
                    prefix,
                    artist.map(|name| (name, true)),
                )
                .await
            }
            Self::Remote(source) => match artist {
                Some(name) => {
                    source
                        .directory_artist_image_for(database, source_id, prefix, name, true)
                        .await
                }
                None => source.directory_image(database, source_id, prefix).await,
            },
        }
    }

    async fn directory_artist_image(
        &self,
        database: &library::Database,
        source_id: &str,
        prefix: &str,
        artist: &str,
        name: &str,
    ) -> SourceResult<Option<LocalImageRef>> {
        let found = self
            .folder_image(database, source_id, prefix, Some(name))
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
                && !database
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
            Self::Local => local::artwork::revision(Path::new(&track.path)),
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
