//! File tag semantics and atomic edits for originals and remote working copies.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ::lofty::config::WriteOptions;
use ::lofty::file::TaggedFileExt;
use ::lofty::picture::{MimeType, Picture, PictureType};
use ::lofty::prelude::TagExt;
use ::lofty::tag::items::popularimeter::{Popularimeter, StarRating};
use ::lofty::tag::{ItemKey, Tag};

use super::lofty::{MetadataWriter, read_lofty_for_edit};
use crate::{MetadataDraft, MetadataEdit, MetadataField, SourceEntityKind, SourceMetadataError};

mod fields;

/// A provider working copy keeps the original revision and connection until replacement.
pub(crate) struct WorkingFile {
    pub file: tempfile::TempPath,
    pub relative: String,
    pub revision: Option<String>,
    pub existed: bool,
    pub input: std::sync::Arc<crate::file::remote::input::FileInputServer>,
}

pub(crate) struct CollectionTracks<'a> {
    database: &'a library::Database,
    uri: &'a str,
    after: Option<library::TrackKey>,
    page: std::vec::IntoIter<(library::TrackKey, String)>,
    finished: bool,
}

impl<'a> CollectionTracks<'a> {
    pub(crate) fn new(database: &'a library::Database, uri: &'a str) -> Self {
        Self {
            database,
            uri,
            after: None,
            page: Vec::new().into_iter(),
            finished: false,
        }
    }

    pub(crate) async fn next(&mut self) -> Result<Option<library::TrackRow>, SourceMetadataError> {
        if self.page.len() == 0 && !self.finished {
            let page = self
                .database
                .file_metadata_track_page(self.uri, self.after)
                .await
                .map_err(|error| SourceMetadataError::Write(error.to_string()))?;
            self.after = page.last().map(|(key, _)| *key);
            self.finished = page.is_empty();
            self.page = page.into_iter();
        }
        let Some((_, uri)) = self.page.next() else {
            return Ok(None);
        };
        self.database
            .track_row_by_uri(&uri, &library::ReadCancellation::new())
            .await
            .map_err(|error| SourceMetadataError::Write(error.to_string()))?
            .ok_or(SourceMetadataError::Unavailable)
            .map(Some)
    }
}

pub(crate) fn hash_file_revision(hash: &mut blake3::Hasher, uri: &str, revision: Option<&str>) {
    for value in [uri, revision.unwrap_or_default()] {
        hash.update(&(value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
}

pub(crate) fn revision(path: &Path) -> Result<String, SourceMetadataError> {
    if path.to_string_lossy().starts_with("content:") {
        return super::documents::stat_document(&path.to_string_lossy())
            .map(|entry| entry.revision.unwrap_or_default())
            .map_err(write_error);
    }
    let metadata = fs::metadata(path).map_err(write_error)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |value| value.as_nanos());
    Ok(format!("{}:{modified}", metadata.len()))
}

pub(crate) fn combined_revision(
    paths: impl IntoIterator<Item = impl AsRef<Path>>,
) -> Result<String, SourceMetadataError> {
    let mut value = String::from("v1");
    for path in paths {
        let path = path.as_ref();
        let revision = revision(path)?;
        let path = path.to_string_lossy();
        value.push_str(&format!(":{}:{path}:{revision}", path.len()));
    }
    Ok(value)
}

pub(crate) fn write_metadata(
    targets: &[MetadataFileTarget],
    expected_revision: &str,
    previous_artist: &str,
    edit: &crate::MetadataEdit,
    folder_image: Option<&Path>,
) -> Result<(), SourceMetadataError> {
    let current = if edit.kind == SourceEntityKind::Track {
        revision(&targets[0].path)?
    } else {
        combined_revision(targets.iter().map(|target| &target.path))?
    };
    if current != expected_revision {
        return Err(SourceMetadataError::Conflict);
    }
    let embedded = edit
        .artwork
        .as_ref()
        .is_some_and(|art| art.storage == crate::ArtworkStorage::Embedded);
    if targets.iter().any(|target| !target.tag_writable) && (!edit.changes.is_empty() || embedded) {
        return Err(SourceMetadataError::Unavailable);
    }
    if edit
        .artwork
        .as_ref()
        .is_some_and(|art| art.storage == crate::ArtworkStorage::Server)
    {
        return Err(SourceMetadataError::Unavailable);
    }
    let mut prepared = Vec::new();
    let mut mp4_image = None;
    if !edit.changes.is_empty() || embedded {
        for target in targets {
            prepared.push(prepare_file(
                &target.path,
                target.format.as_deref(),
                None,
                |tag, writer| apply_metadata(tag, writer, edit, previous_artist, &mut mp4_image),
            )?);
        }
    }
    let renamed_artwork = if artist_image_needs_rename(edit, previous_artist, folder_image) {
        let image = folder_image.unwrap();
        Some(crate::ArtworkEdit {
            change: crate::ArtworkChange::Replace(std::sync::Arc::new(crate::ImageBytes {
                bytes: fs::read(image).map_err(write_error)?,
                content_type: super::artwork::content_type(image),
            })),
            storage: crate::ArtworkStorage::Folder,
        })
    } else {
        None
    };
    finish_artwork(
        prepared,
        targets,
        edit.artwork.as_ref().or(renamed_artwork.as_ref()),
        artist_name(edit, previous_artist),
        folder_image,
    )
}

/// Edits a disposable remote copy; uploading it owns the actual file replacement.
pub(crate) fn write_metadata_copy(
    path: &Path,
    format: Option<&str>,
    edit: &crate::MetadataEdit,
    previous_artist: &str,
    mp4_image: &mut Option<std::sync::Arc<crate::ImageBytes>>,
) -> Result<(), SourceMetadataError> {
    let writer = format
        .and_then(MetadataWriter::for_source_format)
        .or_else(|| MetadataWriter::for_path(path))
        .ok_or(SourceMetadataError::Unavailable)?;
    edit_tag(path, writer, None, |tag, writer| {
        apply_metadata(tag, writer, edit, previous_artist, mp4_image)
    })
}

fn apply_metadata(
    tag: &mut Tag,
    writer: MetadataWriter,
    edit: &crate::MetadataEdit,
    previous_artist: &str,
    mp4_image: &mut Option<std::sync::Arc<crate::ImageBytes>>,
) -> Result<(), SourceMetadataError> {
    let picture = embedded_picture(
        edit.artwork.as_ref(),
        tag,
        writer,
        artist_name(edit, previous_artist),
        previous_artist,
        mp4_image,
    )?;
    if edit
        .artwork
        .as_ref()
        .is_some_and(|art| art.storage == crate::ArtworkStorage::Embedded)
    {
        replace_picture(
            tag,
            picture,
            artist_name(edit, previous_artist),
            previous_artist,
        );
    }
    if edit.kind == SourceEntityKind::Artist {
        apply_artist_edit(tag, previous_artist, &edit.changes);
    }
    fields::apply(tag, writer, edit)
}

pub(crate) fn artist_image_needs_rename(
    edit: &crate::MetadataEdit,
    previous_artist: &str,
    image: Option<&Path>,
) -> bool {
    edit.kind == SourceEntityKind::Artist
        && edit.changes.contains_key("title")
        && edit.artwork.is_none()
        && image
            .and_then(Path::file_stem)
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| {
                stem.eq_ignore_ascii_case(&super::artwork::artist_image_stem(previous_artist))
            })
}

pub(crate) fn artist_name<'a>(edit: &'a MetadataEdit, previous: &'a str) -> Option<&'a str> {
    (edit.kind == SourceEntityKind::Artist)
        .then(|| edit.changes.get("title").map_or(previous, String::as_str))
}

fn apply_artist_edit(tag: &mut Tag, previous_name: &str, changes: &crate::MetadataChanges) {
    if let Some(name) = changes.get("title") {
        let previous = format!("Artist: {}", previous_name.trim());
        let current = format!("Artist: {}", name.trim());
        for index in 0..tag.pictures().len() {
            let picture = &tag.pictures()[index];
            if matches!(
                picture.pic_type(),
                PictureType::Artist | PictureType::LeadArtist | PictureType::Band
            ) && picture
                .description()
                .is_some_and(|description| description.eq_ignore_ascii_case(&previous))
            {
                let mut picture = picture.clone();
                picture.set_description(Some(current.clone()));
                tag.set_picture(index, picture);
            }
        }
    }
    for (names, sort, id) in [
        (
            &[ItemKey::TrackArtist, ItemKey::TrackArtists][..],
            ItemKey::TrackArtistSortOrder,
            ItemKey::MusicBrainzArtistId,
        ),
        (
            &[ItemKey::AlbumArtist][..],
            ItemKey::AlbumArtistSortOrder,
            ItemKey::MusicBrainzReleaseArtistId,
        ),
    ] {
        let names_for = |key| {
            tag.get_strings(key)
                .flat_map(super::media::split_names)
                .collect::<Vec<_>>()
        };
        let role_names = names
            .iter()
            .rev()
            .map(|key| names_for(*key))
            .find(|values| !values.is_empty())
            .unwrap_or_default();
        let matches = role_names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(previous_name.trim()));
        if !matches {
            continue;
        }
        if let Some(name) = changes.get("title") {
            for key in names {
                let names = tag
                    .get_strings(*key)
                    .map(|value| replace_name(value, previous_name, name))
                    .collect::<Vec<_>>();
                tag.remove_key(*key);
                for name in names {
                    tag.push(::lofty::tag::TagItem::new(
                        *key,
                        ::lofty::tag::ItemValue::Text(name),
                    ));
                }
            }
        }
        if let Some(value) = changes.get("sort_title") {
            set_artist_value(
                tag,
                sort,
                &role_names,
                previous_name,
                (!value.is_empty()).then_some(value.as_str()),
            );
        }
        if let Some(value) = changes.get("musicbrainz_artist_id") {
            set_artist_value(
                tag,
                id,
                &role_names,
                previous_name,
                (!value.is_empty()).then_some(value.as_str()),
            );
        }
    }
}

pub(crate) fn write_rating(
    path: &Path,
    source_format: Option<&str>,
    rating: Option<u8>,
) -> Result<(), SourceMetadataError> {
    let prepared = prepare_file(path, source_format, None, |tag, _| {
        tag.remove_key(ItemKey::Popularimeter);
        if let Some(rating) = rating.filter(|rating| *rating > 0) {
            let stars = rating.div_ceil(2).clamp(1, 5);
            let stars = match stars {
                1 => StarRating::One,
                2 => StarRating::Two,
                3 => StarRating::Three,
                4 => StarRating::Four,
                _ => StarRating::Five,
            };
            tag.insert_text(
                ItemKey::Popularimeter,
                Popularimeter::musicbee(stars, 0).to_string(),
            );
        }
        Ok(())
    })?;
    commit_batch(vec![prepared])
}

pub fn read_embedded_lyrics(path: &Path) -> Result<Option<String>, SourceMetadataError> {
    read_embedded_lyrics_input(fs::File::open(path).map_err(write_error)?)
}

pub(crate) fn read_embedded_lyrics_input(
    file: impl std::io::Read + std::io::Seek,
) -> Result<Option<String>, SourceMetadataError> {
    let tagged = super::lofty::read_lofty_file(
        file,
        ::lofty::config::ParseOptions::new().read_cover_art(false),
    )
    .map_err(write_error)?
    .ok_or(SourceMetadataError::Unavailable)?;
    Ok(tagged.tags().iter().find_map(|tag| {
        [ItemKey::UnsyncLyrics, ItemKey::Lyrics]
            .into_iter()
            .find_map(|key| tag.get_string(key))
            .map(ToString::to_string)
            .filter(|lyrics| !lyrics.trim().is_empty())
    }))
}

pub(crate) fn write_embedded_lyrics(path: &Path, lyrics: &str) -> Result<(), SourceMetadataError> {
    let writer = MetadataWriter::for_path(path).ok_or(SourceMetadataError::Unavailable)?;
    if matches!(
        writer.file_type(),
        ::lofty::file::FileType::Wav | ::lofty::file::FileType::Aiff
    ) {
        return Err(SourceMetadataError::Unavailable);
    }
    let (tag_type, key) = writer
        .lyrics_target()
        .ok_or(SourceMetadataError::Unavailable)?;
    let prepared = prepare_file(path, None, Some(tag_type), |tag, _| {
        set_embedded_lyrics(tag, key, lyrics);
        Ok(())
    })?;
    commit_batch(vec![prepared])
}

pub(crate) fn write_sidecar_lyrics(
    audio_path: &Path,
    lyrics: &str,
) -> Result<(), SourceMetadataError> {
    let path = audio_path.with_extension("lrc");
    let parent = path.parent().ok_or(SourceMetadataError::Unavailable)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(write_error)?;
    temporary
        .write_all(lyrics.as_bytes())
        .map_err(write_error)?;
    if let Ok(permissions) = fs::metadata(&path)
        .or_else(|_| fs::metadata(audio_path))
        .map(|metadata| metadata.permissions())
    {
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(write_error)?;
    }
    temporary.as_file().sync_all().map_err(write_error)?;
    temporary
        .persist(&path)
        .map_err(|error| write_error(error.error))?;
    sync_parent(parent)
}

fn set_embedded_lyrics(tag: &mut Tag, key: ItemKey, lyrics: &str) {
    tag.remove_key(ItemKey::UnsyncLyrics);
    tag.remove_key(ItemKey::Lyrics);
    tag.insert_text(key, lyrics.to_string());
}

pub(crate) fn write_r128(
    path: &Path,
    source_format: Option<&str>,
    track_lufs: Option<f64>,
    album_lufs: Option<f64>,
) -> Result<(), SourceMetadataError> {
    let writer = source_format
        .and_then(MetadataWriter::for_source_format)
        .or_else(|| MetadataWriter::for_path(path))
        .ok_or(SourceMetadataError::Unavailable)?;
    if track_lufs.is_some() && !writer.metadata_key_is_writable(ItemKey::R128TrackGain)
        || album_lufs.is_some() && !writer.metadata_key_is_writable(ItemKey::R128AlbumGain)
    {
        return Err(SourceMetadataError::Unavailable);
    }
    let prepared = prepare_file(path, source_format, None, |tag, _| {
        if let Some(lufs) = track_lufs {
            set_text(tag, ItemKey::R128TrackGain, Some(&r128_gain_text(lufs)));
        }
        if let Some(lufs) = album_lufs {
            set_text(tag, ItemKey::R128AlbumGain, Some(&r128_gain_text(lufs)));
        }
        Ok(())
    })?;
    commit_batch(vec![prepared])
}

fn r128_gain_text(integrated_lufs: f64) -> String {
    let gain = ((-23.0 - integrated_lufs) * 256.0).round();
    (gain.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16).to_string()
}

pub(crate) fn artwork_embeddable(path: &Path, format: Option<&str>, artist: bool) -> bool {
    let writer = format
        .and_then(MetadataWriter::for_source_format)
        .or_else(|| MetadataWriter::for_path(path));
    writer.is_some_and(|writer| match writer.file_type().primary_tag_type() {
        ::lofty::tag::TagType::Id3v2
        | ::lofty::tag::TagType::VorbisComments
        | ::lofty::tag::TagType::Ape => true,
        ::lofty::tag::TagType::Mp4Ilst => !artist,
        _ => false,
    })
}

fn embedded_picture(
    edit: Option<&crate::ArtworkEdit>,
    tag: &Tag,
    writer: MetadataWriter,
    artist_name: Option<&str>,
    previous_artist: &str,
    mp4_image: &mut Option<std::sync::Arc<crate::ImageBytes>>,
) -> Result<Option<Picture>, SourceMetadataError> {
    let Some(edit) = edit else { return Ok(None) };
    match edit.storage {
        crate::ArtworkStorage::Folder => return Ok(None),
        crate::ArtworkStorage::Server => return Err(SourceMetadataError::Unavailable),
        crate::ArtworkStorage::Embedded => {}
    }
    if writer.file_type() == ::lofty::file::FileType::Mp4 && artist_name.is_some() {
        return Err(SourceMetadataError::Unavailable);
    }
    let crate::ArtworkChange::Replace(image) = &edit.change else {
        return Ok(None);
    };
    if let Some(name) = artist_name {
        if tag.tag_type() == ::lofty::tag::TagType::Ape {
            let description = format!("Artist: {}", name.trim());
            let previous_description = format!("Artist: {}", previous_artist.trim());
            let primary_artist = tag
                .get_string(ItemKey::AlbumArtist)
                .or_else(|| tag.get_string(ItemKey::TrackArtist))
                .and_then(|value| super::media::split_names(value).into_iter().next());
            let owns_generic = primary_artist
                .is_some_and(|primary| primary.eq_ignore_ascii_case(previous_artist.trim()));
            if tag.pictures().iter().any(|picture| {
                picture.pic_type() == PictureType::Artist
                    && match picture.description() {
                        Some(current) if current.starts_with("Artist: ") => {
                            !current.eq_ignore_ascii_case(&description)
                                && !current.eq_ignore_ascii_case(&previous_description)
                        }
                        _ => !owns_generic,
                    }
            }) {
                return Err(SourceMetadataError::Write("APE stores one image per artwork role. Use Save in folder to keep the existing artist image.".into()));
            }
        }
    }
    let image = if writer.file_type() == ::lofty::file::FileType::Mp4
        && !matches!(
            image.content_type.as_deref(),
            Some("image/jpeg" | "image/png" | "image/gif" | "image/bmp")
        ) {
        if mp4_image.is_none() {
            *mp4_image = Some(mp4_compatible_image(image)?);
        }
        mp4_image.as_ref().unwrap()
    } else {
        image
    };
    let mut picture = Picture::unchecked(image.bytes.clone()).pic_type(if artist_name.is_some() {
        PictureType::Artist
    } else {
        PictureType::CoverFront
    });
    if let Some(name) = artist_name {
        picture = picture.description(format!("Artist: {}", name.trim()));
    }
    if let Some(mime) = image.content_type.as_deref() {
        picture = picture.mime_type(MimeType::from_str(mime));
    }
    Ok(Some(picture.build()))
}

fn mp4_compatible_image(
    image: &crate::ImageBytes,
) -> Result<std::sync::Arc<crate::ImageBytes>, SourceMetadataError> {
    use image::ImageDecoder;
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        jxl_image_rs_integration::register_image_decoding_hook();
    });
    let reader = image::ImageReader::new(std::io::Cursor::new(&image.bytes))
        .with_guessed_format()
        .map_err(write_error)?;
    let mut decoder = reader.into_decoder().map_err(write_error)?;
    let orientation = decoder.orientation().map_err(write_error)?;
    let mut decoded = image::DynamicImage::from_decoder(decoder).map_err(write_error)?;
    decoded.apply_orientation(orientation);
    let mut bytes = std::io::Cursor::new(Vec::new());
    decoded
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(write_error)?;
    if bytes.get_ref().len() > 32 * 1024 * 1024 {
        return Err(SourceMetadataError::Write("The converted image exceeds the 32 MiB artwork limit. Use Save in folder to keep the original image.".into()));
    }
    Ok(std::sync::Arc::new(crate::ImageBytes {
        bytes: bytes.into_inner(),
        content_type: Some("image/png".into()),
    }))
}

fn replace_picture(
    tag: &mut Tag,
    picture: Option<Picture>,
    artist_name: Option<&str>,
    previous_artist: &str,
) {
    if let Some(name) = artist_name {
        let primary_artist = tag
            .get_string(ItemKey::AlbumArtist)
            .or_else(|| tag.get_string(ItemKey::TrackArtist))
            .and_then(|value| super::media::split_names(value).into_iter().next());
        let owns_generic = primary_artist
            .is_some_and(|primary| primary.eq_ignore_ascii_case(previous_artist.trim()));
        let indices = tag
            .pictures()
            .iter()
            .enumerate()
            .filter_map(|(index, current)| {
                let matches_artist = match current
                    .description()
                    .and_then(|description| description.strip_prefix("Artist: "))
                {
                    Some(artist) => {
                        artist.trim().eq_ignore_ascii_case(name.trim())
                            || artist.trim().eq_ignore_ascii_case(previous_artist.trim())
                    }
                    None => picture.is_none() && owns_generic,
                };
                (matches!(
                    current.pic_type(),
                    PictureType::Artist | PictureType::LeadArtist | PictureType::Band
                ) && matches_artist)
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        for index in indices.into_iter().rev() {
            tag.remove_picture(index);
        }
    } else {
        let removed_type = if picture.is_none() {
            super::artwork::best_picture(tag.pictures()).map(Picture::pic_type)
        } else {
            Some(PictureType::CoverFront)
        };
        if tag.tag_type() == ::lofty::tag::TagType::Mp4Ilst {
            // MP4 covr stores only cover images and reads them as Other.
            tag.remove_picture_type(PictureType::Other);
        }
        if let Some(removed_type) = removed_type {
            tag.remove_picture_type(removed_type);
        }
    }
    if let Some(picture) = picture {
        tag.push_picture(picture);
    }
}

pub(crate) fn artwork_extension(
    image: &crate::ImageBytes,
) -> Result<&'static str, SourceMetadataError> {
    match image.content_type.as_deref() {
        Some("image/jpeg") => Ok("jpg"),
        Some("image/png") => Ok("png"),
        Some("image/webp") => Ok("webp"),
        Some("image/jxl") => Ok("jxl"),
        Some("image/gif") => Ok("gif"),
        Some("image/apng") => Ok("apng"),
        Some("image/bmp") => Ok("bmp"),
        Some("image/tiff") => Ok("tiff"),
        _ => Err(SourceMetadataError::Write(
            "Unsupported artwork image format".into(),
        )),
    }
}

pub(crate) fn folder_artwork_filename(
    current: Option<&std::ffi::OsStr>,
    artist_name: Option<&str>,
    extension: &str,
) -> PathBuf {
    let current = current.map(Path::new);
    let named_artist = current
        .and_then(Path::file_stem)
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.starts_with("artist-"));
    if let Some(name) = artist_name.filter(|_| current.is_none() || named_artist) {
        PathBuf::from(super::artwork::artist_image_stem(name)).with_extension(extension)
    } else {
        current
            .unwrap_or_else(|| Path::new("cover"))
            .with_extension(extension)
    }
}

pub(crate) fn save_folder_artwork(
    targets: &[MetadataFileTarget],
    edit: &crate::ArtworkEdit,
    artist_name: Option<&str>,
    folder_image: Option<&Path>,
) -> Result<(), SourceMetadataError> {
    finish_artwork(Vec::new(), targets, Some(edit), artist_name, folder_image)
}

fn finish_artwork(
    mut prepared: Vec<PreparedFile>,
    targets: &[MetadataFileTarget],
    edit: Option<&crate::ArtworkEdit>,
    artist_name: Option<&str>,
    folder_image: Option<&Path>,
) -> Result<(), SourceMetadataError> {
    let mut replaced = Vec::new();
    if let Some(edit) = edit.filter(|edit| edit.storage == crate::ArtworkStorage::Folder) {
        let directories = if let Some(image) = folder_image {
            image
                .parent()
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        } else {
            targets
                .iter()
                .filter_map(|target| target.path.parent())
                .collect()
        };
        for directory in directories {
            let current = if let Some(image) = folder_image {
                Some(image.to_path_buf())
            } else if let Some(name) = artist_name {
                super::local::artwork::directory_artist_image_for(directory, name, false)
            } else {
                super::local::artwork::directory_image(directory)
            };
            let crate::ArtworkChange::Replace(image) = &edit.change else {
                replaced.extend(current);
                continue;
            };
            let extension = artwork_extension(image)?;
            let target = current
                .as_ref()
                .and_then(|path| path.parent())
                .unwrap_or(directory)
                .join(folder_artwork_filename(
                    current.as_deref().and_then(Path::file_name),
                    artist_name,
                    extension,
                ));
            let mut temp = tempfile::NamedTempFile::new_in(directory).map_err(write_error)?;
            temp.write_all(&image.bytes).map_err(write_error)?;
            if let Some(permissions) = current
                .as_deref()
                .or_else(|| targets.first().map(|target| target.path.as_path()))
                .and_then(|path| fs::metadata(path).ok())
                .map(|metadata| metadata.permissions())
            {
                temp.as_file()
                    .set_permissions(permissions)
                    .map_err(write_error)?;
            }
            temp.as_file().sync_all().map_err(write_error)?;
            prepared.push(PreparedFile {
                existed: target.exists(),
                target: target.clone(),
                temp: temp.into_temp_path(),
            });
            if let Some(current) = current.filter(|current| *current != target) {
                replaced.push(current);
            }
        }
    }
    commit_batch(prepared)?;
    for path in replaced {
        fs::remove_file(&path).map_err(|error| SourceMetadataError::PartiallySaved {
            reason: format!("The previous artwork image could not be removed: {error}"),
        })?;
        if let Some(parent) = path.parent() {
            sync_parent(parent)?;
        }
    }
    Ok(())
}

struct PreparedFile {
    target: PathBuf,
    temp: tempfile::TempPath,
    existed: bool,
}

fn prepare_file(
    path: &Path,
    source_format: Option<&str>,
    tag_type: Option<::lofty::tag::TagType>,
    mutate: impl FnOnce(&mut Tag, MetadataWriter) -> Result<(), SourceMetadataError>,
) -> Result<PreparedFile, SourceMetadataError> {
    let writer = source_format
        .and_then(MetadataWriter::for_source_format)
        .or_else(|| MetadataWriter::for_path(path))
        .ok_or(SourceMetadataError::Unavailable)?;
    let parent = path.parent().ok_or_else(|| {
        SourceMetadataError::Write("metadata file has no parent folder".to_string())
    })?;
    let temp = tempfile::Builder::new()
        .prefix(".rufin-metadata-")
        .tempfile_in(parent)
        .map_err(write_error)?;
    fs::copy(path, temp.path()).map_err(write_error)?;
    edit_tag(temp.path(), writer, tag_type, mutate)?;
    temp.as_file().sync_all().map_err(write_error)?;
    Ok(PreparedFile {
        target: path.to_path_buf(),
        temp: temp.into_temp_path(),
        existed: true,
    })
}

fn edit_tag(
    path: &Path,
    writer: MetadataWriter,
    tag_type: Option<::lofty::tag::TagType>,
    mutate: impl FnOnce(&mut Tag, MetadataWriter) -> Result<(), SourceMetadataError>,
) -> Result<(), SourceMetadataError> {
    let tagged = read_lofty_for_edit(path, writer.file_type())
        .map_err(write_error)?
        .ok_or(SourceMetadataError::Unavailable)?;
    let mut tag = tag_type
        .map(|tag_type| {
            tagged
                .tags()
                .iter()
                .find(|tag| tag.tag_type() == tag_type)
                .cloned()
                .unwrap_or_else(|| Tag::new(tag_type))
        })
        .unwrap_or_else(|| writable_tag(&tagged));
    mutate(&mut tag, writer)?;
    if tag.tag_type() == ::lofty::tag::TagType::Id3v2
        && matches!(
            writer.file_type(),
            ::lofty::file::FileType::Wav | ::lofty::file::FileType::Aiff
        )
    {
        // Lofty 0.25.1 subtracts growth when resizing an existing terminal ID3
        // chunk. Remove that chunk before writing its complete edited tag so
        // RIFF/FORM length stays valid and subsequent edits read the new values.
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(write_error)?;
        tag.tag_type()
            .remove_from(&mut file, WriteOptions::new())
            .map_err(write_error)?;
    }
    save_tag(tag, path)
}

fn commit_batch(mut prepared: Vec<PreparedFile>) -> Result<(), SourceMetadataError> {
    for index in 0..prepared.len() {
        let file = &mut prepared[index];
        let result = if file.existed {
            replace(&mut file.temp, &file.target)
        } else {
            fs::rename(&file.temp, &file.target).map_err(write_error)
        };
        if let Err(error) = result {
            for rollback in prepared[..index].iter_mut().rev() {
                if rollback.existed {
                    let _ = replace(&mut rollback.temp, &rollback.target);
                } else {
                    let _ = fs::remove_file(&rollback.target);
                }
            }
            return Err(error);
        }
    }
    for file in &prepared {
        if let Some(parent) = file.target.parent() {
            sync_parent(parent).map_err(|error| SourceMetadataError::PartiallySaved {
                reason: format!("Saved files could not be synced: {error}"),
            })?;
        }
    }
    Ok(())
}

fn writable_tag(tagged: &::lofty::file::TaggedFile) -> Tag {
    tagged.primary_tag().cloned().unwrap_or_else(|| {
        let primary = tagged.file_type().primary_tag_type();
        let mut tag = tagged
            .first_tag()
            .cloned()
            .unwrap_or_else(|| Tag::new(primary));
        tag.re_map(primary);
        tag
    })
}

fn save_tag(mut tag: Tag, path: &Path) -> Result<(), SourceMetadataError> {
    if tag.tag_type() == ::lofty::tag::TagType::Id3v2 {
        // Lofty 0.25.1 writes WORK as a frame ID and omits the MVIN total.
        let work = tag
            .get_strings(ItemKey::Work)
            .collect::<Vec<_>>()
            .join("\0");
        let movement = match (
            tag.get_string(ItemKey::MovementNumber),
            tag.get_string(ItemKey::MovementTotal),
        ) {
            (Some(number), Some(total)) => Some(format!("{number}/{total}")),
            (None, Some(total)) => Some(format!("0/{total}")),
            (Some(number), None) => Some(number.to_string()),
            (None, None) => None,
        };
        tag.remove_key(ItemKey::Work);
        tag.remove_key(ItemKey::MovementNumber);
        tag.remove_key(ItemKey::MovementTotal);
        let mut id3 = ::lofty::id3::v2::Id3v2Tag::from(tag);
        if !work.is_empty() {
            id3.insert_user_text("WORK".into(), work);
        }
        if let Some(movement) = movement {
            id3.insert(::lofty::id3::v2::Frame::Text(
                ::lofty::id3::v2::TextInformationFrame::new(
                    ::lofty::id3::v2::FrameId::Valid(std::borrow::Cow::Borrowed("MVIN")),
                    ::lofty::TextEncoding::UTF8,
                    movement,
                ),
            ));
        }
        return id3
            .save_to_path(path, WriteOptions::new().remove_others(false))
            .map_err(write_error);
    }
    tag.save_to_path(path, WriteOptions::new().remove_others(false))
        .map_err(write_error)
}

fn set_text(tag: &mut Tag, key: ItemKey, value: Option<&str>) {
    tag.remove_key(key);
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        // MusicBrainz's ID3 UFID has no ordinary key mapping.
        tag.insert_unchecked(::lofty::tag::TagItem::new(
            key,
            ::lofty::tag::ItemValue::Text(value.to_string()),
        ));
    }
}

fn replace_name(value: &str, previous: &str, replacement: &str) -> String {
    super::media::split_names(value)
        .into_iter()
        .map(|name| {
            if name.eq_ignore_ascii_case(previous.trim()) {
                replacement.to_string()
            } else {
                name
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn set_artist_value(
    tag: &mut Tag,
    key: ItemKey,
    names: &[String],
    selected: &str,
    value: Option<&str>,
) {
    if names.len() == 1 {
        set_text(tag, key, value);
        return;
    }
    let mut values = tag
        .get_strings(key)
        .flat_map(|value| value.split([';', '/']).map(str::trim).map(str::to_string))
        .collect::<Vec<_>>();
    if values.len() < names.len() {
        values.resize(names.len(), String::new());
    }
    for (name, current) in names.iter().zip(values.iter_mut()) {
        if name.eq_ignore_ascii_case(selected.trim()) {
            *current = value.unwrap_or_default().to_string();
        }
    }
    set_text(tag, key, Some(&values.join("; ")));
}

#[cfg(any(target_os = "android", target_os = "linux", target_os = "macos"))]
fn replace(replacement: &mut tempfile::TempPath, target: &Path) -> Result<(), SourceMetadataError> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        &**replacement,
        rustix::fs::CWD,
        target,
        rustix::fs::RenameFlags::EXCHANGE,
    )
    .map_err(write_error)
}

#[cfg(target_os = "windows")]
fn replace(replacement: &mut tempfile::TempPath, target: &Path) -> Result<(), SourceMetadataError> {
    let backup = tempfile::Builder::new()
        .prefix(".rufin-metadata-")
        .tempfile_in(target.parent().ok_or(SourceMetadataError::Unavailable)?)
        .map_err(write_error)?
        .into_temp_path();
    let target = target.to_str().ok_or(SourceMetadataError::Unavailable)?;
    winsafe::ReplaceFile(
        target,
        replacement
            .to_str()
            .ok_or(SourceMetadataError::Unavailable)?,
        Some(backup.to_str().ok_or(SourceMetadataError::Unavailable)?),
        winsafe::co::REPLACEFILE::default(),
    )
    .map_err(write_error)?;
    *replacement = backup;
    Ok(())
}

#[cfg(not(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "windows"
)))]
fn replace(
    _replacement: &mut tempfile::TempPath,
    _target: &Path,
) -> Result<(), SourceMetadataError> {
    Err(SourceMetadataError::Unavailable)
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<(), SourceMetadataError> {
    fs::File::open(parent)
        .map_err(write_error)?
        .sync_all()
        .map_err(write_error)
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<(), SourceMetadataError> {
    Ok(())
}

fn write_error(error: impl std::fmt::Display) -> SourceMetadataError {
    SourceMetadataError::Write(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lofty::prelude::Accessor;

    #[test]
    fn repeated_album_edits_replace_the_previous_tag_in_a_working_copy() {
        let file = tempfile::NamedTempFile::new().unwrap().into_temp_path();
        fs::write(&file, silent_wav()).unwrap();
        {
            use lofty::prelude::{AudioFile, TaggedFileExt};
            let mut tagged = lofty::probe::Probe::open(&file)
                .unwrap()
                .guess_file_type()
                .unwrap()
                .read()
                .unwrap();
            let mut tag = lofty::tag::Tag::new(lofty::tag::TagType::Id3v2);
            tag.push_picture(
                lofty::picture::Picture::unchecked(vec![7; 32])
                    .pic_type(lofty::picture::PictureType::CoverFront)
                    .mime_type(lofty::picture::MimeType::Png)
                    .build(),
            );
            tagged.insert_tag(tag);
            tagged
                .save_to_path(&file, lofty::config::WriteOptions::default())
                .unwrap();
        }
        for title in ["First album", "A longer second album title", "Short"] {
            let edit = MetadataEdit {
                kind: SourceEntityKind::Album,
                changes: [("title".into(), title.into())].into(),
                artwork: None,
            };
            let paths = [file.to_path_buf()];
            let revision = combined_revision(&paths).unwrap();
            write_metadata(
                &(&[(file.to_path_buf(), Some("wav".into()))])
                    .iter()
                    .map(|(path, format)| crate::file::metadata::MetadataFileTarget {
                        path: path.clone(),
                        format: format.clone(),
                        tag_writable: true,
                    })
                    .collect::<Vec<_>>(),
                &revision,
                "",
                &edit,
                None,
            )
            .unwrap();
            let observed = read_track_metadata(&file, Some("wav")).unwrap();
            assert_eq!(
                observed
                    .fields
                    .iter()
                    .find(|field| field.key == "album")
                    .unwrap()
                    .value,
                title
            );
            let saved = std::fs::read(&file).unwrap();
            assert_eq!(
                u32::from_le_bytes(saved[4..8].try_into().unwrap()) as usize + 8,
                saved.len(),
                "RIFF length remains valid after growing and shrinking tags"
            );
            let original = silent_wav();
            assert_eq!(
                &saved[12..original.len()],
                &original[12..],
                "Audio and format chunks remain byte-for-byte unchanged"
            );
            use lofty::prelude::TaggedFileExt;
            let tagged = lofty::probe::Probe::open(&file)
                .unwrap()
                .guess_file_type()
                .unwrap()
                .read()
                .unwrap();
            assert_eq!(tagged.primary_tag().unwrap().pictures()[0].data(), &[7; 32]);
        }
    }

    fn track_edit(changes: &[(&str, &str)]) -> MetadataEdit {
        MetadataEdit {
            kind: SourceEntityKind::Track,
            changes: changes
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            artwork: None,
        }
    }

    #[test]
    fn r128_gain_uses_signed_q7_8_at_the_minus_23_lufs_reference() {
        assert_eq!(r128_gain_text(-23.0), "0");
        assert_eq!(r128_gain_text(-21.0), "-512");
        assert_eq!(r128_gain_text(-25.5), "640");
    }

    #[test]
    fn available_local_track_metadata_reads_and_writes_the_same_file() {
        let directory = tempfile::tempdir().expect("metadata directory");
        let path = directory.path().join("track.wav");
        fs::write(&path, silent_wav()).expect("write WAV");
        assert!(super::metadata_file_available(&path, Some("wav")));
        let before = revision(&path).expect("file revision");
        let edit = track_edit(&[
            ("title", "Updated title"),
            ("artist", "Updated artist"),
            ("album", "Updated album"),
            ("track_number", "3"),
            ("recording_date", "2020-04-03"),
            ("musicbrainz_recording_id", "recording"),
        ]);

        write_metadata(
            &[crate::file::metadata::MetadataFileTarget {
                path: (&path).to_path_buf(),
                format: Some("wav").map(str::to_owned),
                tag_writable: true,
            }],
            &before,
            "",
            &edit,
            None,
        )
        .expect("write metadata");
        let read = super::read_track_metadata(&path, Some("wav")).expect("read metadata");
        assert!(
            read.fields
                .iter()
                .find(|field| field.key == "title")
                .unwrap()
                .writable
        );
        let values = read
            .fields
            .into_iter()
            .map(|field| (field.key, field.value))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(values["title"], "Updated title");
        assert_eq!(values["artist"], "Updated artist");
        assert_eq!(values["track_number"], "3");
        assert_eq!(values["recording_date"], "2020-04-03");
        assert_eq!(values["musicbrainz_recording_id"], "recording");
    }

    #[test]
    fn unindexed_file_uri_metadata_edits_the_original_and_rejects_segments() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("original.wav");
        fs::write(&path, silent_wav()).unwrap();
        let uri = url::Url::from_file_path(&path).unwrap().to_string();
        let metadata = crate::Source::read_direct_file_metadata(&uri).unwrap();
        crate::Source::write_direct_file_metadata(
            &uri,
            metadata.revision.as_deref().unwrap(),
            &track_edit(&[("title", "Direct original")]),
        )
        .unwrap();
        assert_eq!(
            crate::Source::read_direct_file_metadata(&uri)
                .unwrap()
                .fields
                .into_iter()
                .find(|field| field.key == "title")
                .unwrap()
                .value,
            "Direct original"
        );
        assert!(
            crate::Source::write_direct_file_metadata(
                &format!("{uri}#cue=1"),
                "",
                &track_edit(&[])
            )
            .is_err()
        );
        assert!(crate::Source::read_direct_file_metadata("https://example.com/song.wav").is_err());
    }

    #[test]
    fn embedded_lyrics_replace_id3_values_and_refuse_unsafe_wav_writes() {
        let directory = tempfile::tempdir().expect("metadata directory");
        let path = directory.path().join("track.wav");
        fs::write(&path, silent_wav()).expect("write WAV");
        let before = revision(&path).expect("file revision");
        write_metadata(
            &[crate::file::metadata::MetadataFileTarget {
                path: (&path).to_path_buf(),
                format: Some("wav").map(str::to_owned),
                tag_writable: true,
            }],
            &before,
            "",
            &track_edit(&[("title", "Track title")]),
            None,
        )
        .expect("seed metadata");

        assert_eq!(
            write_embedded_lyrics(&path, "[00:01.000]A line"),
            Err(SourceMetadataError::Unavailable)
        );
        assert!(!super::embedded_lyrics_writable(&path));
        assert_eq!(
            super::read_track_metadata(&path, Some("wav"))
                .expect("read metadata")
                .fields
                .into_iter()
                .find(|field| field.key == "title")
                .unwrap()
                .value,
            "Track title"
        );

        let mut id3 = Tag::new(::lofty::tag::TagType::Id3v2);
        set_embedded_lyrics(&mut id3, ItemKey::UnsyncLyrics, "first");
        set_embedded_lyrics(&mut id3, ItemKey::UnsyncLyrics, "updated");
        assert_eq!(id3.get_string(ItemKey::UnsyncLyrics), Some("updated"));
    }

    #[test]
    fn sidecar_lyrics_replace_the_neighbor_without_changing_audio() {
        let directory = tempfile::tempdir().expect("metadata directory");
        let path = directory.path().join("track.wav");
        let audio = silent_wav();
        fs::write(&path, &audio).expect("write WAV");

        write_sidecar_lyrics(&path, "[00:01.000]First").expect("write sidecar");
        write_sidecar_lyrics(&path, "[00:02.000]Updated").expect("replace sidecar");

        assert_eq!(
            fs::read_to_string(directory.path().join("track.lrc")).expect("read sidecar"),
            "[00:02.000]Updated"
        );
        assert_eq!(fs::read(path).expect("read WAV"), audio);
    }

    #[test]
    fn local_album_metadata_updates_each_backing_track_as_one_batch() {
        let directory = tempfile::tempdir().expect("metadata directory");
        let paths = [
            directory.path().join("one.wav"),
            directory.path().join("two.wav"),
        ];
        for (index, path) in paths.iter().enumerate() {
            fs::write(path, silent_wav()).expect("write WAV");
            let before = revision(path).expect("file revision");
            write_metadata(
                &[crate::file::metadata::MetadataFileTarget {
                    path: (path).to_path_buf(),
                    format: Some("wav").map(str::to_owned),
                    tag_writable: true,
                }],
                &before,
                "",
                &track_edit(&[
                    ("title", &path.file_stem().unwrap().to_string_lossy()),
                    ("album", "Before"),
                    ("genre", &format!("Genre {index}")),
                ]),
                None,
            )
            .expect("seed Track metadata");
        }
        let targets = paths
            .iter()
            .cloned()
            .map(|path| (path, Some("wav".to_string())))
            .collect::<Vec<_>>();
        let before = combined_revision(&paths).expect("Album revision");
        write_metadata(
            &(&targets)
                .iter()
                .map(|(path, format)| crate::file::metadata::MetadataFileTarget {
                    path: path.clone(),
                    format: format.clone(),
                    tag_writable: true,
                })
                .collect::<Vec<_>>(),
            &before,
            "",
            &MetadataEdit {
                kind: SourceEntityKind::Album,
                changes: [
                    ("title".into(), "After".into()),
                    ("album_artist".into(), "Album Artist".into()),
                ]
                .into(),
                artwork: None,
            },
            None,
        )
        .expect("write Album metadata");

        for (index, path) in paths.iter().enumerate() {
            let values = read_track_metadata(path, Some("wav"))
                .expect("read Album metadata")
                .fields
                .into_iter()
                .map(|field| (field.key, field.value))
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(values["album"], "After");
            assert_eq!(values["album_artist"], "Album Artist");
            assert_eq!(
                values["genre"],
                format!("Genre {index}"),
                "an untouched mixed aggregate field keeps each Track's value"
            );
        }
    }

    #[test]
    fn local_metadata_rejects_a_concurrent_file_change_without_overwriting_it() {
        let directory = tempfile::tempdir().expect("metadata directory");
        let path = directory.path().join("track.wav");
        fs::write(&path, silent_wav()).expect("write WAV");
        let stale = revision(&path).expect("file revision");
        let mut concurrent = silent_wav();
        concurrent.extend_from_slice(b"concurrent-change");
        fs::write(&path, &concurrent).expect("change WAV");

        let result = write_metadata(
            &[crate::file::metadata::MetadataFileTarget {
                path: (&path).to_path_buf(),
                format: Some("wav").map(str::to_owned),
                tag_writable: true,
            }],
            &stale,
            "",
            &track_edit(&[("title", "Should not replace")]),
            None,
        );

        assert!(matches!(result, Err(SourceMetadataError::Conflict)));
        assert_eq!(
            fs::read(&path).expect("unchanged concurrent file"),
            concurrent
        );
    }

    #[test]
    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    ))]
    fn aggregate_commit_failure_restores_every_replaced_file() {
        let directory = tempfile::tempdir().expect("metadata directory");
        let first = directory.path().join("first.wav");
        let second = directory.path().join("second.wav");
        fs::write(&first, silent_wav()).expect("first WAV");
        let mut second_bytes = silent_wav();
        second_bytes.extend_from_slice(b"second");
        fs::write(&second, &second_bytes).expect("second WAV");
        let first_bytes = fs::read(&first).expect("first original");
        let second_original = directory.path().join("second.original");
        let prepared = [&first, &second]
            .into_iter()
            .map(|path| {
                prepare_file(path, Some("wav"), None, |tag, _| {
                    tag.set_title("After".into());
                    Ok(())
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .expect("prepared metadata files");
        fs::rename(&second, &second_original).expect("make second replacement fail");

        assert!(commit_batch(prepared).is_err());
        assert_eq!(fs::read(&first).expect("restored first"), first_bytes);
        assert_eq!(
            fs::read(&second_original).expect("untouched second"),
            second_bytes
        );
    }

    #[tokio::test]
    async fn metadata_publication_keeps_identity_for_equivalent_file_paths() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let audio = root.join("café %.wav");
        fs::write(&audio, silent_wav()).unwrap();
        let database = library::Database::open(root.join("library.db"))
            .await
            .unwrap();
        let connected = crate::Source::connect(
            crate::SourceId::new("metadata-paths"),
            crate::SourceSetup::Local {
                roots: vec![root.clone()],
            },
            "",
        )
        .await
        .unwrap();
        let (_, source, _) = connected.into_parts();
        source
            .manual_refresh(
                &database,
                "Local",
                &|_| {},
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        let uri = url::Url::from_file_path(&audio).unwrap().to_string();
        let before = database
            .track_row_by_uri(&uri, &library::ReadCancellation::new())
            .await
            .unwrap()
            .unwrap();
        for path in [
            root.join(".").join("café %.wav"),
            library::file_media_path(&uri).unwrap(),
        ] {
            crate::file::local::scan::publish_metadata_paths(
                &database,
                source.source_id().as_str(),
                &[path],
                None,
                None,
            )
            .await
            .unwrap();
            let after = database
                .track_row_by_uri(&uri, &library::ReadCancellation::new())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(after.track_key, before.track_key);
            assert_eq!(after.object_id, before.object_id);
        }
    }

    #[tokio::test]
    async fn removing_last_local_root_clears_catalog_without_deleting_files() {
        let directory = tempfile::tempdir().unwrap();
        let audio = directory.path().canonicalize().unwrap().join("track.wav");
        fs::write(&audio, silent_wav()).unwrap();
        let database = library::Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        let connected = crate::Source::connect(
            crate::SourceId::new("remove-local-root"),
            crate::SourceSetup::Local {
                roots: vec![directory.path().to_path_buf()],
            },
            "",
        )
        .await
        .unwrap();
        let (configuration, source, _) = connected.into_parts();
        source
            .manual_refresh(
                &database,
                "Local",
                &|_| {},
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        let uri = url::Url::from_file_path(&audio).unwrap().to_string();
        assert!(
            database
                .track_row_by_uri(&uri, &library::ReadCancellation::new())
                .await
                .unwrap()
                .is_some()
        );
        let source_id = configuration.source_id.clone();
        let edit = crate::Source::edit(
            configuration,
            None,
            crate::SourceSettingsChange::Local {
                source_id,
                roots: Vec::new(),
                excluded_folders: None,
            },
            None,
        )
        .await
        .unwrap();
        let crate::SourceEditResult::Connected(connected) = edit else {
            panic!("expected changed Local source")
        };
        let (_, source, _) = (*connected).into_parts();
        source
            .manual_refresh(
                &database,
                "Local",
                &|_| {},
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        let key = database
            .source_identity_key(source.source_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            database
                .song_count(
                    &library::SongQuery::new(library::SongScope::Tracks(key)),
                    &library::ReadCancellation::new()
                )
                .await
                .unwrap(),
            0
        );
        assert!(
            database
                .track_row_by_uri(&uri, &library::ReadCancellation::new())
                .await
                .unwrap()
                .is_some()
        );
        assert!(audio.exists());
    }

    #[tokio::test]
    async fn imported_local_files_survive_rescan_and_leave_catalog_after_last_playlist_reference() {
        let directory = tempfile::tempdir().unwrap();
        let audio = directory.path().join("Jóga #1.wav");
        fs::write(&audio, silent_wav()).unwrap();
        let database = library::Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        let source_id = crate::SourceId::new("import-local");
        let connected = crate::Source::connect(
            source_id.clone(),
            crate::SourceSetup::Local { roots: Vec::new() },
            "",
        )
        .await
        .unwrap();
        let (_, source, _) = connected.into_parts();
        library::Scan::begin(&database, source_id.as_str(), "Local", "local", None)
            .await
            .unwrap()
            .finish()
            .await
            .unwrap();
        let playlist = database
            .import_playlist_file(
                std::io::Cursor::new("Jóga #1.wav\nJóga #1.wav\n"),
                &directory.path().join("mix.m3u8"),
                None,
                |_| None,
            )
            .await
            .unwrap();
        source
            .import_playlist_files(&database, playlist.playlist)
            .await
            .unwrap();
        let uri = url::Url::from_file_path(&audio).unwrap().to_string();
        assert!(
            database
                .track_row_by_uri(&uri, &library::ReadCancellation::new())
                .await
                .unwrap()
                .is_some()
        );
        source
            .manual_refresh(
                &database,
                "Local",
                &|_| {},
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert!(
            database
                .track_row_by_uri(&uri, &library::ReadCancellation::new())
                .await
                .unwrap()
                .is_some()
        );
        let second = source
            .import_local_paths(&database, vec![audio.clone()], "Second", &|_| {})
            .await
            .unwrap();
        let source_key = database
            .source_identity_key(source.source_id())
            .await
            .unwrap()
            .unwrap();
        assert!(
            database
                .local_file_page(source_key, None, 128, &library::ReadCancellation::new())
                .await
                .unwrap()
                .is_empty()
        );
        database
            .delete_playlist(None, playlist.playlist)
            .await
            .unwrap();
        source.prune_imported_files(&database).await.unwrap();
        assert!(
            database
                .track_row_by_uri(&uri, &library::ReadCancellation::new())
                .await
                .unwrap()
                .is_some()
        );
        database.delete_playlist(None, second).await.unwrap();
        source.prune_imported_files(&database).await.unwrap();
        let key = database
            .source_identity_key(source.source_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            database
                .song_count(
                    &library::SongQuery::new(library::SongScope::Tracks(key)),
                    &library::ReadCancellation::new()
                )
                .await
                .unwrap(),
            0
        );
        assert!(
            database
                .track_row_by_uri(&uri, &library::ReadCancellation::new())
                .await
                .unwrap()
                .is_some()
        );
        assert!(audio.is_file());
    }

    fn silent_wav() -> Vec<u8> {
        let data_len = 16_000_u32;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&8_000_u32.to_le_bytes());
        bytes.extend_from_slice(&16_000_u32.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.resize(44 + data_len as usize, 0);
        bytes
    }
}

pub(crate) fn embedded_lyrics_writable(path: &Path) -> bool {
    lyrics_writer_supported(super::lofty::MetadataWriter::for_path(path))
}

pub(crate) fn embedded_lyrics_format_writable(format: &str) -> bool {
    lyrics_writer_supported(super::lofty::MetadataWriter::for_source_format(format))
}

fn lyrics_writer_supported(writer: Option<super::lofty::MetadataWriter>) -> bool {
    writer.is_some_and(|writer| {
        !matches!(
            writer.file_type(),
            ::lofty::file::FileType::Wav | ::lofty::file::FileType::Aiff
        ) && writer.lyrics_target().is_some()
    })
}

pub(crate) fn metadata_file_available(path: &Path, source_format: Option<&str>) -> bool {
    source_format
        .and_then(super::lofty::MetadataWriter::for_source_format)
        .or_else(|| super::lofty::MetadataWriter::for_path(path))
        .is_some()
}

fn read_metadata_file(
    path: &Path,
    source_format: Option<&str>,
) -> Result<(Option<::lofty::file::TaggedFile>, Option<MetadataWriter>), SourceMetadataError> {
    if path.to_string_lossy().starts_with("content:") {
        let reader =
            super::documents::open_document_input(&path.to_string_lossy()).map_err(write_error)?;
        let tagged = super::lofty::read_lofty_file(
            reader,
            ::lofty::config::ParseOptions::new().read_cover_art(false),
        )
        .map_err(write_error)?;
        let writer = tagged
            .as_ref()
            .and_then(|file| MetadataWriter::for_file_type(file.file_type()));
        return Ok((tagged, writer));
    }
    let writer = source_format
        .and_then(MetadataWriter::for_source_format)
        .or_else(|| MetadataWriter::for_path(path));
    let tagged = match writer {
        Some(writer) => read_lofty_for_edit(path, writer.file_type()),
        None => super::lofty::read_lofty(path, false),
    }
    .map_err(write_error)?;
    let writer = writer.filter(|_| tagged.is_some());
    Ok((tagged, writer))
}

fn read_file(
    kind: SourceEntityKind,
    path: &Path,
    format: Option<&str>,
    artist: Option<&str>,
) -> Result<MetadataDraft, SourceMetadataError> {
    let (tagged, writer) = read_metadata_file(path, format)?;
    let tag = tagged
        .as_ref()
        .and_then(|file| file.primary_tag().or_else(|| file.first_tag()));
    let scope = match kind {
        SourceEntityKind::Track => 1,
        SourceEntityKind::Album => 2,
        SourceEntityKind::Artist => 4,
    };
    let mut values = fields::read(tag, writer, scope);
    if let Some(name) = artist {
        values.splice(0..0, artist_fields(tag, writer, name));
    } else if writer.is_none() && kind == SourceEntityKind::Track {
        if let Some(title) = values
            .iter_mut()
            .find(|field| field.key == "title" && field.value.is_empty())
        {
            title.value = path
                .file_stem()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
    }
    let mut metadata = MetadataDraft::new(kind, values);
    metadata.track_count = 1;
    metadata.artwork.can_embed = artwork_embeddable(path, format, artist.is_some());
    metadata.revision = Some(revision(path)?);
    Ok(metadata)
}

pub(crate) fn read_track_metadata(
    path: &Path,
    source_format: Option<&str>,
) -> Result<MetadataDraft, SourceMetadataError> {
    read_file(SourceEntityKind::Track, path, source_format, None)
}

fn artist_fields(
    tag: Option<&Tag>,
    writer: Option<MetadataWriter>,
    name: &str,
) -> Vec<MetadataField> {
    let names = |key| {
        tag.into_iter()
            .flat_map(|tag| tag.get_strings(key))
            .flat_map(super::media::split_names)
            .collect::<Vec<_>>()
    };
    let roles = [
        (
            ItemKey::TrackArtist,
            ItemKey::TrackArtistSortOrder,
            ItemKey::MusicBrainzArtistId,
        ),
        (
            ItemKey::AlbumArtist,
            ItemKey::AlbumArtistSortOrder,
            ItemKey::MusicBrainzReleaseArtistId,
        ),
    ]
    .into_iter()
    .filter_map(|(key, sort, id)| {
        let mut values = names(key);
        if key == ItemKey::TrackArtist {
            let credits = names(ItemKey::TrackArtists);
            if !credits.is_empty() {
                values = credits;
            }
        }
        values
            .iter()
            .any(|value| value.eq_ignore_ascii_case(name))
            .then_some((key, sort, id, values))
    })
    .collect::<Vec<_>>();
    let selected_text = |key, names: &[String]| {
        let position = names
            .iter()
            .position(|value| value.eq_ignore_ascii_case(name))?;
        tag.into_iter()
            .flat_map(|tag| tag.get_strings(key))
            .flat_map(|value| value.split([';', '/']).map(str::trim))
            .nth(position)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let can = |key| writer.is_some_and(|writer| writer.metadata_key_is_writable(key));
    vec![
        MetadataField::new(
            "title",
            name,
            !roles.is_empty() && roles.iter().all(|(key, _, _, _)| can(*key)),
        ),
        MetadataField::new(
            "sort_title",
            roles
                .iter()
                .find_map(|(_, key, _, names)| selected_text(*key, names))
                .unwrap_or_default(),
            !roles.is_empty() && roles.iter().all(|(_, key, _, _)| can(*key)),
        ),
        MetadataField::new(
            "musicbrainz_artist_id",
            roles
                .iter()
                .find_map(|(_, _, key, names)| selected_text(*key, names))
                .unwrap_or_default(),
            !roles.is_empty() && roles.iter().all(|(_, _, key, _)| can(*key)),
        ),
    ]
}

pub(crate) struct MetadataFileTarget {
    pub path: PathBuf,
    pub format: Option<String>,
    pub tag_writable: bool,
}

pub(crate) fn readonly_track_metadata(
    track: &library::TrackRow,
    revision: Option<String>,
) -> MetadataDraft {
    let mut metadata = MetadataDraft::new(
        SourceEntityKind::Track,
        [
            ("title", track.title.clone()),
            ("artist", track.artist.clone()),
            ("album", track.album.clone()),
            (
                "album_artist",
                track.album_display_artist.clone().unwrap_or_default(),
            ),
            (
                "track_number",
                (track.track_number > 0)
                    .then(|| track.track_number.to_string())
                    .unwrap_or_default(),
            ),
            (
                "disc_number",
                (track.disc_number > 0)
                    .then(|| track.disc_number.to_string())
                    .unwrap_or_default(),
            ),
            (
                "recording_date",
                track
                    .year
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            (
                "genre",
                track
                    .genres
                    .iter()
                    .map(|genre| genre.name.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            ("comment", track.comment.clone().unwrap_or_default()),
            (
                "bpm",
                track.bpm.map(|value| value.to_string()).unwrap_or_default(),
            ),
            (
                "musicbrainz_recording_id",
                track.musicbrainz_recording_id.clone().unwrap_or_default(),
            ),
            (
                "musicbrainz_release_track_id",
                track
                    .musicbrainz_release_track_id
                    .clone()
                    .unwrap_or_default(),
            ),
            (
                "musicbrainz_album_id",
                track.musicbrainz_album_id.clone().unwrap_or_default(),
            ),
            (
                "musicbrainz_release_group_id",
                track
                    .musicbrainz_release_group_id
                    .clone()
                    .unwrap_or_default(),
            ),
            (
                "musicbrainz_artist_id",
                track
                    .primary_artist_musicbrainz_id
                    .clone()
                    .unwrap_or_default(),
            ),
        ]
        .into_iter()
        .map(|(key, value)| MetadataField::new(key, value, false))
        .collect(),
    );
    metadata.revision = revision;
    metadata.track_count = 1;
    metadata
}

pub(crate) fn readonly_album_metadata(
    album: &library::AlbumRow,
    revision: Option<String>,
) -> MetadataDraft {
    let mut metadata = MetadataDraft::new(
        SourceEntityKind::Album,
        [
            ("title", album.title.clone()),
            ("artist", album.display_artist.clone()),
            ("album_artist", album.display_artist.clone()),
            (
                "recording_date",
                album
                    .year
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            (
                "genre",
                album
                    .genres
                    .iter()
                    .map(|genre| genre.name.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            (
                "musicbrainz_album_id",
                album.musicbrainz_release_id.clone().unwrap_or_default(),
            ),
            (
                "musicbrainz_release_group_id",
                album
                    .musicbrainz_release_group_id
                    .clone()
                    .unwrap_or_default(),
            ),
        ]
        .into_iter()
        .map(|(key, value)| MetadataField::new(key, value, false))
        .collect(),
    );
    metadata.revision = revision;
    metadata.track_count = album.track_count.max(0) as usize;
    metadata
}

pub(crate) fn readonly_artist_metadata(
    artist: &library::ArtistRow,
    revision: Option<String>,
) -> MetadataDraft {
    let mut metadata = MetadataDraft::new(
        SourceEntityKind::Artist,
        vec![
            MetadataField::new("title", artist.name.clone(), false),
            MetadataField::new(
                "musicbrainz_artist_id",
                artist.musicbrainz_artist_id.clone().unwrap_or_default(),
                false,
            ),
        ],
    );
    metadata.revision = revision;
    metadata.track_count = artist.track_count.max(0) as usize;
    metadata
}

pub(crate) fn read_album_file(
    album: &library::AlbumRow,
    current: Option<MetadataDraft>,
    path: &Path,
    format: Option<&str>,
) -> Result<MetadataDraft, SourceMetadataError> {
    let mut next = read_file(SourceEntityKind::Album, path, format, None)?;
    if current.is_none() {
        for field in &mut next.fields {
            if field.value.is_empty() {
                match field.key.as_str() {
                    "title" => field.value = album.title.clone(),
                    "artist" | "album_artist" => field.value = album.display_artist.clone(),
                    _ => {}
                }
            }
        }
        next.suggest("musicbrainz_album_id", album.musicbrainz_release_id.clone());
        next.suggest(
            "musicbrainz_release_group_id",
            album.musicbrainz_release_group_id.clone(),
        );
    }
    Ok(merge_file(current, next))
}

pub(crate) fn read_artist_file(
    artist: &library::ArtistRow,
    current: Option<MetadataDraft>,
    path: &Path,
    format: Option<&str>,
) -> Result<MetadataDraft, SourceMetadataError> {
    let mut next = read_file(SourceEntityKind::Artist, path, format, Some(&artist.name))?;
    if current.is_none() {
        next.suggest(
            "musicbrainz_artist_id",
            artist.musicbrainz_artist_id.clone(),
        );
    }
    Ok(merge_file(current, next))
}

fn merge_file(current: Option<MetadataDraft>, next: MetadataDraft) -> MetadataDraft {
    if let Some(mut current) = current {
        fields::merge(&mut current.fields, next.fields);
        current.artwork.can_embed &= next.artwork.can_embed;
        current.track_count += 1;
        current
    } else {
        next
    }
}

pub(crate) fn album_metadata_from_targets(
    album: library::AlbumRow,
    targets: &[MetadataFileTarget],
) -> Result<MetadataDraft, SourceMetadataError> {
    let revision = Some(combined_revision(
        targets.iter().map(|target| &target.path),
    )?);
    if targets.iter().any(|target| !target.tag_writable) {
        return Ok(readonly_album_metadata(&album, revision));
    }
    let mut metadata = None;
    for target in targets {
        metadata = Some(read_album_file(
            &album,
            metadata,
            &target.path,
            target.format.as_deref(),
        )?);
    }
    let mut metadata = metadata.ok_or(SourceMetadataError::Unavailable)?;
    metadata.revision = revision;
    Ok(metadata)
}

pub(crate) fn artist_metadata_from_targets(
    artist: library::ArtistRow,
    targets: &[MetadataFileTarget],
) -> Result<MetadataDraft, SourceMetadataError> {
    let revision = Some(combined_revision(
        targets.iter().map(|target| &target.path),
    )?);
    if targets.iter().any(|target| !target.tag_writable) {
        return Ok(readonly_artist_metadata(&artist, revision));
    }
    let mut metadata = None;
    for target in targets {
        metadata = Some(read_artist_file(
            &artist,
            metadata,
            &target.path,
            target.format.as_deref(),
        )?);
    }
    let mut metadata = metadata.ok_or(SourceMetadataError::Unavailable)?;
    metadata.revision = revision;
    Ok(metadata)
}
