//! File tag semantics and atomic edits for originals and remote working copies.

use std::fs;
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

pub(crate) const WORKING_COPY_PREFIX: &str = ".rufin-metadata-";

pub(crate) fn is_working_copy(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(WORKING_COPY_PREFIX))
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

pub(crate) type MetadataImageCache = Option<(
    std::sync::Arc<crate::ImageBytes>,
    std::sync::Arc<crate::ImageBytes>,
)>;

#[derive(Clone)]
pub(crate) enum TagUpdate {
    Metadata(Vec<(crate::MetadataEdit, String)>),
    Rating(Option<u8>),
    Lyrics(String),
    R128 {
        track: Option<f64>,
        album: Option<f64>,
    },
}

pub(crate) fn edit_copy(
    path: &Path,
    format: Option<&str>,
    update: &TagUpdate,
    mp4_image: &mut MetadataImageCache,
) -> Result<(), SourceMetadataError> {
    let writer = format
        .and_then(MetadataWriter::for_source_format)
        .or_else(|| MetadataWriter::for_path(path))
        .ok_or(SourceMetadataError::Unavailable)?;
    let tag_type = if matches!(update, TagUpdate::Lyrics(_)) {
        if !lyrics_writer_supported(Some(writer)) {
            return Err(SourceMetadataError::Unavailable);
        }
        Some(
            writer
                .lyrics_target()
                .ok_or(SourceMetadataError::Unavailable)?
                .0,
        )
    } else {
        None
    };
    edit_tag(path, writer, tag_type, |tag, writer| {
        match update {
            TagUpdate::Metadata(edits) => {
                for (edit, artist) in edits {
                    apply_metadata(tag, writer, edit, artist, mp4_image)?;
                }
            }
            TagUpdate::Rating(rating) => {
                tag.remove_key(ItemKey::Popularimeter);
                if let Some(rating) = rating.filter(|rating| *rating > 0) {
                    let stars = match rating.div_ceil(2).clamp(1, 5) {
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
            }
            TagUpdate::Lyrics(lyrics) => {
                let (_, key) = writer
                    .lyrics_target()
                    .ok_or(SourceMetadataError::Unavailable)?;
                set_embedded_lyrics(tag, key, lyrics);
            }
            TagUpdate::R128 { track, album } => {
                for (value, key) in [
                    (track, ItemKey::R128TrackGain),
                    (album, ItemKey::R128AlbumGain),
                ] {
                    if let Some(value) = value {
                        if !writer.metadata_key_is_writable(key) {
                            return Err(SourceMetadataError::Unavailable);
                        }
                        set_text(tag, key, Some(&r128_gain_text(*value)));
                    }
                }
            }
        }
        Ok(())
    })
}

fn apply_metadata(
    tag: &mut Tag,
    writer: MetadataWriter,
    edit: &crate::MetadataEdit,
    previous_artist: &str,
    mp4_image: &mut MetadataImageCache,
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

pub fn read_embedded_lyrics(path: &Path) -> Result<Option<String>, SourceMetadataError> {
    read_embedded_lyrics_input(fs::File::open(path).map_err(write_error)?)
}

pub(crate) fn read_embedded_lyrics_input(
    file: impl std::io::Read + std::io::Seek,
) -> Result<Option<String>, SourceMetadataError> {
    let tagged = super::lofty::read_lofty_file(
        file,
        ::lofty::config::ParseOptions::new()
            .read_cover_art(false)
            .read_properties(false),
    )
    .map_err(write_error)?
    .ok_or(SourceMetadataError::Unavailable)?;
    Ok(embedded_lyrics(&tagged))
}

pub(crate) fn embedded_lyrics(tagged: &::lofty::file::TaggedFile) -> Option<String> {
    tagged.tags().iter().find_map(|tag| {
        [ItemKey::UnsyncLyrics, ItemKey::Lyrics]
            .into_iter()
            .find_map(|key| tag.get_string(key))
            .map(ToString::to_string)
            .filter(|lyrics| !lyrics.trim().is_empty())
    })
}

fn set_embedded_lyrics(tag: &mut Tag, key: ItemKey, lyrics: &str) {
    tag.remove_key(ItemKey::UnsyncLyrics);
    tag.remove_key(ItemKey::Lyrics);
    tag.insert_text(key, lyrics.to_string());
}

fn r128_gain_text(integrated_lufs: f64) -> String {
    let gain = ((-23.0 - integrated_lufs) * 256.0).round();
    (gain.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16).to_string()
}

fn embedded_picture(
    edit: Option<&crate::ArtworkEdit>,
    tag: &Tag,
    writer: MetadataWriter,
    artist_name: Option<&str>,
    previous_artist: &str,
    mp4_image: &mut MetadataImageCache,
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
        if mp4_image
            .as_ref()
            .is_none_or(|(original, _)| !std::sync::Arc::ptr_eq(original, image))
        {
            *mp4_image = Some((image.clone(), mp4_compatible_image(image)?));
        }
        &mp4_image.as_ref().unwrap().1
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

pub(crate) fn project_edit(
    values: &crate::MetadataChanges,
    edit: &MetadataEdit,
    previous_artist: &str,
) -> crate::MetadataChanges {
    let mut changes = fields::project(edit);
    if edit.kind == SourceEntityKind::Artist {
        for (names_key, sort_key, id_key) in [
            ("artist", "artist_sort", "musicbrainz_artist_id"),
            (
                "album_artist",
                "album_artist_sort",
                "musicbrainz_album_artist_id",
            ),
        ] {
            let names_value = values
                .get(names_key)
                .map(String::as_str)
                .unwrap_or_default();
            let names = super::media::split_names(names_value);
            if !names
                .iter()
                .any(|name| name.eq_ignore_ascii_case(previous_artist.trim()))
            {
                continue;
            }
            if let Some(name) = edit.changes.get("title") {
                changes.insert(
                    names_key.into(),
                    replace_name(names_value, previous_artist, name),
                );
            }
            for (scope_key, track_key) in
                [("sort_title", sort_key), ("musicbrainz_artist_id", id_key)]
            {
                if let Some(value) = edit.changes.get(scope_key) {
                    changes.insert(
                        track_key.into(),
                        artist_values(
                            values
                                .get(track_key)
                                .map(String::as_str)
                                .unwrap_or_default(),
                            &names,
                            previous_artist,
                            Some(value),
                        ),
                    );
                }
            }
        }
    }
    changes
}

pub(crate) fn scope_metadata(
    kind: SourceEntityKind,
    artist: &str,
    original: &MetadataDraft,
    current: &crate::MetadataChanges,
) -> MetadataDraft {
    let before = original.values();
    let values = scope_values(kind, artist, &before, current);
    let mut fields = fields::scope_fields(kind, original);
    if kind == SourceEntityKind::Artist && values.contains_key("title") {
        for key in ["title", "sort_title", "musicbrainz_artist_id"] {
            let edit = MetadataEdit {
                kind,
                changes: [(key.into(), String::new())].into(),
                artwork: None,
            };
            let projected = project_edit(&before, &edit, artist);
            let writable = projected.keys().all(|key| {
                original
                    .fields
                    .iter()
                    .any(|field| field.key == *key && field.writable)
            });
            fields.push(MetadataField::new(key, "", writable));
        }
    }
    for field in &mut fields {
        field.value = values.get(&field.key).cloned().unwrap_or_default();
        field.mixed = false;
    }
    let mut draft = MetadataDraft::new(kind, fields);
    draft.artwork = original.artwork.clone();
    draft.track_count = 1;
    draft
}

pub(crate) fn scope_values(
    kind: SourceEntityKind,
    artist: &str,
    original: &crate::MetadataChanges,
    current: &crate::MetadataChanges,
) -> crate::MetadataChanges {
    let mut values = fields::scope_values(kind, current);
    if kind == SourceEntityKind::Artist {
        for (names, sort, id) in [
            ("artist", "artist_sort", "musicbrainz_artist_id"),
            (
                "album_artist",
                "album_artist_sort",
                "musicbrainz_album_artist_id",
            ),
        ] {
            let names_before = super::media::split_names(
                original.get(names).map(String::as_str).unwrap_or_default(),
            );
            if !names_before
                .iter()
                .any(|name| name.eq_ignore_ascii_case(artist.trim()))
            {
                continue;
            }
            let current_names = super::media::split_names(
                current.get(names).map(String::as_str).unwrap_or_default(),
            );
            let index = current_names
                .iter()
                .position(|name| name.eq_ignore_ascii_case(artist.trim()))
                .or_else(|| {
                    let replacements = current_names
                        .iter()
                        .enumerate()
                        .filter(|(_, name)| {
                            !names_before.iter().any(|previous| {
                                !previous.eq_ignore_ascii_case(artist.trim())
                                    && previous.eq_ignore_ascii_case(name)
                            })
                        })
                        .map(|(index, _)| index)
                        .collect::<Vec<_>>();
                    (replacements.len() == 1).then(|| replacements[0])
                });
            let Some(index) = index else {
                continue;
            };
            for (scope_key, track_key) in [
                ("title", names),
                ("sort_title", sort),
                ("musicbrainz_artist_id", id),
            ] {
                let text = current
                    .get(track_key)
                    .map(String::as_str)
                    .unwrap_or_default();
                let entries = if scope_key == "title" {
                    super::media::split_names(text)
                } else {
                    text.split([';', '/'])
                        .map(str::trim)
                        .map(str::to_string)
                        .collect()
                };
                values.insert(
                    scope_key.into(),
                    entries.get(index).cloned().unwrap_or_default(),
                );
            }
            break;
        }
    }
    values
}

fn set_artist_value(
    tag: &mut Tag,
    key: ItemKey,
    names: &[String],
    selected: &str,
    value: Option<&str>,
) {
    let current = tag.get_strings(key).collect::<Vec<_>>().join("; ");
    let next = artist_values(&current, names, selected, value);
    set_text(tag, key, (!next.is_empty()).then_some(next.as_str()));
}

fn artist_values(current: &str, names: &[String], selected: &str, value: Option<&str>) -> String {
    if names.len() == 1 {
        return value.unwrap_or_default().to_string();
    }
    let mut values = current
        .split([';', '/'])
        .map(str::trim)
        .map(str::to_string)
        .collect::<Vec<_>>();
    if values.len() < names.len() {
        values.resize(names.len(), String::new());
    }
    for (name, current) in names.iter().zip(values.iter_mut()) {
        if name.eq_ignore_ascii_case(selected.trim()) {
            *current = value.unwrap_or_default().to_string();
        }
    }
    values.join("; ")
}

#[cfg(any(target_os = "android", target_os = "linux", target_os = "macos"))]
pub(crate) fn replace(
    replacement: &mut tempfile::TempPath,
    target: &Path,
) -> Result<(), SourceMetadataError> {
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
pub(crate) fn replace(
    replacement: &mut tempfile::TempPath,
    target: &Path,
) -> Result<(), SourceMetadataError> {
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
pub(crate) fn replace(
    _replacement: &mut tempfile::TempPath,
    _target: &Path,
) -> Result<(), SourceMetadataError> {
    Err(SourceMetadataError::Unavailable)
}

#[cfg(unix)]
pub(crate) fn sync_parent(parent: &Path) -> Result<(), SourceMetadataError> {
    fs::File::open(parent)
        .map_err(write_error)?
        .sync_all()
        .map_err(write_error)
}

#[cfg(not(unix))]
pub(crate) fn sync_parent(_parent: &Path) -> Result<(), SourceMetadataError> {
    Ok(())
}

fn write_error(error: impl std::fmt::Display) -> SourceMetadataError {
    SourceMetadataError::Write(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            edit_copy(
                &file,
                Some("wav"),
                &TagUpdate::Metadata(vec![(edit, String::new())]),
                &mut None,
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
        let edit = track_edit(&[
            ("title", "Updated title"),
            ("artist", "Updated artist"),
            ("album", "Updated album"),
            ("track_number", "3"),
            ("recording_date", "2020-04-03"),
            ("musicbrainz_recording_id", "recording"),
        ]);

        edit_copy(
            &path,
            Some("wav"),
            &TagUpdate::Metadata(vec![((edit).clone(), String::new())]),
            &mut None,
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

    #[tokio::test]
    async fn unindexed_file_uri_metadata_edits_the_original_and_rejects_segments() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("original.wav");
        fs::write(&path, silent_wav()).unwrap();
        let uri = url::Url::from_file_path(&path).unwrap().to_string();
        let database = library::Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let key = database
            .remember_media_uris(std::slice::from_ref(&uri))
            .await
            .unwrap()[0];
        let before = revision(&path).unwrap();
        let (editor, draft) =
            crate::FileEditSession::open(None, &database, uri.clone(), SourceEntityKind::Track)
                .await
                .unwrap();
        assert_eq!(draft.revision.as_deref(), Some(before.as_str()));
        editor
            .save(&database, track_edit(&[("title", "Direct original")]))
            .await
            .unwrap();
        let rows = database
            .tracks_by_uris(
                std::slice::from_ref(&uri),
                &library::ReadCancellation::new(),
            )
            .await
            .unwrap();
        assert_eq!(rows[0].track_key, key);
        assert_eq!(rows[0].metadata.title, "Direct original");
        assert!(rows[0].source_key.is_none());
        assert_eq!(
            read_track_metadata(&path, None)
                .unwrap()
                .fields
                .into_iter()
                .find(|field| field.key == "title")
                .unwrap()
                .value,
            "Direct original"
        );
        assert!(crate::source::metadata::NativeFile::direct(&format!("{uri}#cue=1")).is_err());
        assert!(
            crate::source::metadata::NativeFile::direct("https://example.com/song.wav").is_err()
        );
    }

    #[test]
    fn embedded_lyrics_replace_id3_values_and_refuse_unsafe_wav_writes() {
        let directory = tempfile::tempdir().expect("metadata directory");
        let path = directory.path().join("track.wav");
        fs::write(&path, silent_wav()).expect("write WAV");
        edit_copy(
            &path,
            Some("wav"),
            &TagUpdate::Metadata(vec![(
                (track_edit(&[("title", "Track title")])).clone(),
                String::new(),
            )]),
            &mut None,
        )
        .expect("seed metadata");

        assert_eq!(
            edit_copy(
                &path,
                Some("wav"),
                &TagUpdate::Lyrics("[00:01.000]A line".into()),
                &mut None
            ),
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
        let mut tag = Vec::new();
        id3.dump_to(&mut tag, WriteOptions::default()).unwrap();
        let mut audio = silent_wav();
        audio.extend_from_slice(b"id3 ");
        audio.extend_from_slice(&(tag.len() as u32).to_le_bytes());
        audio.extend_from_slice(&tag);
        if tag.len() % 2 != 0 {
            audio.push(0);
        }
        let size = (audio.len() - 8) as u32;
        audio[4..8].copy_from_slice(&size.to_le_bytes());
        fs::write(&path, audio).unwrap();
        let crate::file::media::MediaRead::Accepted(scanned) =
            crate::file::local::media::read_media(
                &mut crate::file::media::Worker::default(),
                path,
                None,
            )
        else {
            panic!("tagged WAV was not read");
        };
        let bundle = lyrics::LyricsBundle::from_cache_payload(
            scanned
                .into_observation(0)
                .source_lyrics
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            lyrics::lyrics_to_lrc_text(&bundle.documents()[0], 0),
            "updated"
        );
    }

    #[tokio::test]
    async fn sidecar_lyrics_replace_the_neighbor_without_changing_audio() {
        use std::sync::{Arc, atomic::AtomicBool};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("track.wav");
        let audio = silent_wav();
        fs::write(&path, &audio).unwrap();
        let uri = url::Url::from_file_path(&path).unwrap();
        let file = crate::source::metadata::NativeFile::direct(uri.as_str()).unwrap();
        file.write_sidecar("[00:01.000]First").await.unwrap();
        file.write_sidecar("[00:02.000]Updated").await.unwrap();
        assert_eq!(
            fs::read_to_string(path.with_extension("lrc")).unwrap(),
            "[00:02.000]Updated"
        );
        assert_eq!(fs::read(&path).unwrap(), audio);
        let database = library::Database::open(directory.path().join("lyrics.sqlite"))
            .await
            .unwrap();
        let (configuration, source, _) = crate::Source::connect(
            crate::SourceId::new("lyrics-scan"),
            crate::SourceSetup::Local {
                roots: vec![directory.path().to_path_buf()],
            },
            "",
        )
        .await
        .unwrap()
        .into_parts();
        let source = Arc::new(source);
        database
            .reconcile_source(
                &configuration.source_id,
                &configuration.kind,
                &configuration.name,
            )
            .await
            .unwrap();
        source
            .acquire_catalog(&database, &|_| {}, Arc::new(AtomicBool::new(false)), true)
            .await
            .unwrap();
        let cancellation = library::ReadCancellation::new();
        let result = database
            .search_tracks_by_lyrics(&library::SearchRequest::new("updated"), &cancellation)
            .await
            .unwrap();
        assert_eq!(
            result.tracks.len(),
            1,
            "sidecars are searchable after scanning"
        );
        source
            .write_lyrics(&database, uri.as_str(), "[00:02.000]Replacement", true)
            .await
            .unwrap();
        assert_eq!(
            database
                .search_tracks_by_lyrics(&library::SearchRequest::new("replacement"), &cancellation)
                .await
                .unwrap()
                .tracks
                .len(),
            1
        );
        assert!(
            database
                .search_tracks_by_lyrics(&library::SearchRequest::new("updated"), &cancellation)
                .await
                .unwrap()
                .tracks
                .is_empty()
        );
        fs::remove_file(path.with_extension("lrc")).unwrap();
        source
            .acquire_catalog(&database, &|_| {}, Arc::new(AtomicBool::new(false)), true)
            .await
            .unwrap();
        assert!(
            database
                .search_tracks_by_lyrics(&library::SearchRequest::new("replacement"), &cancellation)
                .await
                .unwrap()
                .tracks
                .is_empty()
        );
        fs::write(path.with_extension("lrc"), "[00:03.000]Returned").unwrap();
        source
            .acquire_catalog(&database, &|_| {}, Arc::new(AtomicBool::new(false)), true)
            .await
            .unwrap();
        assert_eq!(
            database
                .search_tracks_by_lyrics(&library::SearchRequest::new("returned"), &cancellation)
                .await
                .unwrap()
                .tracks
                .len(),
            1
        );
        assert_eq!(fs::read(&path).unwrap(), audio);
    }

    #[tokio::test]
    async fn local_album_metadata_updates_each_backing_track_as_one_batch() {
        let directory = tempfile::tempdir().unwrap();
        let music = directory.path().join("music");
        fs::create_dir(&music).unwrap();
        let music = music.canonicalize().unwrap();
        let paths = (0..130)
            .map(|index| music.join(format!("track-{index:03}.wav")))
            .collect::<Vec<_>>();
        for (index, path) in paths.iter().enumerate() {
            fs::write(path, silent_wav()).unwrap();
            edit_copy(
                path,
                Some("wav"),
                &TagUpdate::Metadata(vec![(
                    track_edit(&[
                        ("title", &format!("Track {index}")),
                        ("album", "Before"),
                        ("artist", "Guest"),
                        ("album_artist", "Album Artist"),
                        ("genre", &format!("Genre {index}")),
                        (
                            "musicbrainz_album_id",
                            "11111111-1111-4111-8111-111111111111",
                        ),
                    ]),
                    String::new(),
                )]),
                &mut None,
            )
            .unwrap();
        }
        let database = library::Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        let connected = crate::Source::connect(
            crate::SourceId::new("metadata-pages"),
            crate::SourceSetup::Local { roots: vec![music] },
            "",
        )
        .await
        .unwrap();
        let (configuration, source, _) = connected.into_parts();
        database
            .reconcile_source(
                &configuration.source_id,
                &configuration.kind,
                &configuration.name,
            )
            .await
            .unwrap();
        let source = std::sync::Arc::new(source);
        source
            .acquire_catalog(
                &database,
                &|_| {},
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                true,
            )
            .await
            .unwrap();
        let track = database
            .track(
                &(url::Url::from_file_path(&paths[0]).unwrap().as_str()),
                &library::ReadCancellation::new(),
            )
            .await
            .unwrap()
            .unwrap();
        let artist = crate::file::media::artist_credit("Album Artist".into(), None);
        let artist_uri =
            library::source_entity_uri(&configuration.source_id, "artist", &artist.object_id);
        let (send, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |_, update| {
            let _ = send.send(update);
            std::future::ready(())
        });
        for (uri, kind, title) in [
            (artist_uri, SourceEntityKind::Artist, "Renamed Artist"),
            (
                track.links.album_media_uri.unwrap(),
                SourceEntityKind::Album,
                "After",
            ),
        ] {
            let (editor, _draft) =
                crate::FileEditSession::open(Some(source.clone()), &database, uri, kind)
                    .await
                    .unwrap();
            editor
                .save_tracks(
                    &database,
                    Some(MetadataEdit {
                        kind,
                        changes: [("title".into(), title.into())].into(),
                        artwork: None,
                    }),
                    paths
                        .iter()
                        .map(|path| url::Url::from_file_path(path).unwrap().to_string())
                        .collect(),
                    |_| async { Ok((track_edit(&[]), None, None)) },
                    |_, _, _| async { Ok(()) },
                )
                .await
                .unwrap();
            assert!(updates.try_recv().unwrap().metadata);
            assert!(
                updates.try_recv().is_err(),
                "one publication for the collection save"
            );
        }
        for (index, path) in paths.iter().enumerate() {
            let values = read_track_metadata(path, Some("wav"))
                .unwrap()
                .fields
                .into_iter()
                .map(|field| (field.key, field.value))
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(values["album"], "After");
            assert_eq!(values["album_artist"], "Renamed Artist");
            assert_eq!(values["artist"], "Guest");
            assert_eq!(
                values["genre"],
                format!("Genre {index}"),
                "untouched mixed values stay with each file"
            );
        }
    }

    #[tokio::test]
    async fn local_metadata_rejects_a_concurrent_file_change_without_overwriting_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("track.wav");
        fs::write(&path, silent_wav()).unwrap();
        let stale = revision(&path).unwrap();
        let mut concurrent = silent_wav();
        concurrent.extend_from_slice(b"concurrent-change");
        fs::write(&path, &concurrent).unwrap();
        let uri = url::Url::from_file_path(&path).unwrap();
        let file = crate::source::metadata::NativeFile::direct(uri.as_str()).unwrap();
        assert!(matches!(
            file.prepare(Some(&stale)).await,
            Err(SourceMetadataError::Conflict)
        ));
        assert_eq!(fs::read(&path).unwrap(), concurrent);
    }

    #[tokio::test]
    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    ))]
    async fn later_commit_failure_preserves_earlier_successful_file() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.wav");
        let second = directory.path().join("second.wav");
        for path in [&first, &second] {
            fs::write(path, silent_wav()).unwrap();
        }
        let first_file = crate::source::metadata::NativeFile::direct(
            url::Url::from_file_path(&first).unwrap().as_str(),
        )
        .unwrap();
        let second_file = crate::source::metadata::NativeFile::direct(
            url::Url::from_file_path(&second).unwrap().as_str(),
        )
        .unwrap();
        let (prepared_first, _) = first_file
            .prepare(None)
            .await
            .unwrap()
            .mutate(
                TagUpdate::Metadata(vec![(track_edit(&[("title", "After")]), String::new())]),
                None,
            )
            .await
            .unwrap();
        let (prepared_second, _) = second_file
            .prepare(None)
            .await
            .unwrap()
            .mutate(
                TagUpdate::Metadata(vec![(track_edit(&[("title", "After")]), String::new())]),
                None,
            )
            .await
            .unwrap();
        prepared_first.commit(&|| false).await.unwrap();
        let second_original = directory.path().join("second.original");
        fs::rename(&second, &second_original).unwrap();
        assert!(prepared_second.commit(&|| false).await.is_err());
        assert_eq!(
            read_track_metadata(&first, None)
                .unwrap()
                .fields
                .into_iter()
                .find(|field| field.key == "title")
                .unwrap()
                .value,
            "After"
        );
        assert_eq!(fs::read(second_original).unwrap(), silent_wav());
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
        let (configuration, source, _) = connected.into_parts();
        database
            .reconcile_source(
                &configuration.source_id,
                &configuration.kind,
                &configuration.name,
            )
            .await
            .unwrap();
        source
            .acquire_catalog(
                &database,
                &|_| {},
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                true,
            )
            .await
            .unwrap();
        let uri = url::Url::from_file_path(&audio).unwrap().to_string();
        let before = database
            .track(&(&uri), &library::ReadCancellation::new())
            .await
            .unwrap()
            .unwrap();
        for path in [
            root.join(".").join("café %.wav"),
            library::file_media_path(&uri).unwrap(),
        ] {
            let mut catalog = database
                .catalog(source.source_id().as_str(), None)
                .await
                .unwrap();
            let (observation, facts) =
                crate::file::local::scan::read_saved_file(&catalog, &uri, &path)
                    .await
                    .unwrap();
            catalog
                .write_file_observations(
                    &observation.into_iter().collect::<Vec<_>>(),
                    vec![facts],
                    true,
                )
                .await
                .unwrap();
            database
                .publish_catalog_update(source.source_id().as_str(), catalog.into_changes())
                .await;
            let after = database
                .track(&(&uri), &library::ReadCancellation::new())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(after.track_key, before.track_key);
            assert_eq!(after.object_id, before.object_id);
        }
    }

    #[tokio::test]
    async fn removing_last_local_root_clears_catalog_without_deleting_files() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let old_root = root.join("old");
        let new_root = root.join("new");
        fs::create_dir(&old_root).unwrap();
        fs::create_dir(&new_root).unwrap();
        let excluded = old_root.join("excluded");
        fs::create_dir(&excluded).unwrap();
        let audio = excluded.join("track.wav");
        let imported = excluded.join("imported.wav");
        let new_audio = new_root.join("new.wav");
        let file_root = root.join("individual.wav");
        let paths = [&audio, &imported, &new_audio, &file_root];
        for path in paths {
            fs::write(path, silent_wav()).unwrap();
            edit_copy(
                path,
                Some("wav"),
                &TagUpdate::Metadata(vec![(
                    (track_edit(&[("album", "Album"), ("artist", "Artist")])).clone(),
                    String::new(),
                )]),
                &mut None,
            )
            .unwrap();
        }
        let original_files = paths.map(|path| fs::read(path).unwrap());
        let database_path = root.join("library.db");
        let database = library::Database::open(&database_path).await.unwrap();
        let connected = crate::Source::connect(
            crate::SourceId::new("remove-local-root"),
            crate::SourceSetup::Local {
                roots: vec![old_root.clone()],
            },
            "",
        )
        .await
        .unwrap();
        let (configuration, source, _) = connected.into_parts();
        database
            .reconcile_source(
                &configuration.source_id,
                &configuration.kind,
                &configuration.name,
            )
            .await
            .unwrap();
        source
            .acquire_catalog(&database, &|_| {}, Arc::new(AtomicBool::new(false)), true)
            .await
            .unwrap();
        let key = database
            .source_identity_key(source.source_id())
            .await
            .unwrap()
            .unwrap();
        let mut physical = database
            .source_files(
                source.source_id().as_str(),
                &[audio.to_string_lossy().into_owned()],
            )
            .await
            .unwrap()
            .pop()
            .unwrap();
        physical.parse_version = Some(14);
        physical.root = audio
            .ancestors()
            .last()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        database
            .catalog(source.source_id().as_str(), None)
            .await
            .unwrap()
            .write_file_observations(&[(physical, Vec::new())], Vec::new(), true)
            .await
            .unwrap();
        let progress = std::sync::Mutex::new(Vec::new());
        source
            .catch_up_local(
                &database,
                &|update| progress.lock().unwrap().push(update),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert!(
            progress.into_inner().unwrap().is_empty(),
            "accepted released metadata does not need a new read"
        );
        assert_eq!(
            library::MemberRead::new(library::Collection::Source(key))
                .count(&database, &library::ReadCancellation::new())
                .await
                .unwrap(),
            2,
            "a historical point-import root must not withdraw the configured music tree"
        );
        let uri = url::Url::from_file_path(&audio).unwrap().to_string();
        assert!(
            database
                .tracks_by_uris(
                    std::slice::from_ref(&uri),
                    &library::ReadCancellation::new()
                )
                .await
                .map(|rows| { rows.into_iter().next() })
                .unwrap()
                .is_some()
        );
        let playlist = source
            .import_local_paths(&database, vec![imported.clone()], "Imported", &|_| {})
            .await
            .unwrap();
        let imported_uri = url::Url::from_file_path(&imported).unwrap().to_string();
        assert_eq!(
            database
                .playlist_entry_uri_page(playlist, -1)
                .await
                .unwrap()
                .into_iter()
                .map(|(_, _, uri)| uri)
                .collect::<Vec<_>>(),
            vec![imported_uri.clone()],
            "import retains the selected already-indexed file"
        );
        let new_uri = url::Url::from_file_path(&new_audio).unwrap().to_string();
        let key = database
            .source_identity_key(source.source_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            database
                .local_file_page(key, None, 128, &library::ReadCancellation::new())
                .await
                .unwrap()
                .into_iter()
                .find(|file| file.path == imported.to_string_lossy())
                .unwrap()
                .root,
            old_root.to_string_lossy()
        );
        source
            .import_playlist_files(&database, playlist)
            .await
            .unwrap();
        assert_eq!(
            database
                .local_file_page(key, None, 128, &library::ReadCancellation::new())
                .await
                .unwrap()
                .into_iter()
                .find(|file| file.path == imported.to_string_lossy())
                .unwrap()
                .root,
            old_root.to_string_lossy()
        );
        let source_id = configuration.source_id.clone();
        let edit = crate::Source::edit(
            configuration,
            None,
            crate::SourceSettingsChange::Local {
                source_id,
                roots: vec![new_root.clone()],
                excluded_folders: None,
            },
            None,
        )
        .await
        .unwrap();
        let crate::SourceEditResult::Connected(connected) = edit else {
            panic!("expected changed Local source")
        };
        let (configuration, source, _) = (*connected).into_parts();
        let first_tracks = tokio::sync::Notify::new();
        let progress = |progress: crate::SourceReadProgress| {
            if progress.stage == crate::SourceReadStage::Tracks {
                first_tracks.notify_one();
            }
        };
        tokio::select! {
            biased;
            () = first_tracks.notified() => {}
            result = source.acquire_catalog(&database, &progress, Arc::new(AtomicBool::new(false)), true) => panic!("refresh completed before interruption: {result:?}"),
        }
        assert!(
            database
                .cached_source(
                    source.source_id().as_str(),
                    &library::ReadCancellation::new()
                )
                .await
                .unwrap()
                .unwrap()
                .catalog_complete
        );
        drop(source);
        database.close().await.unwrap();

        let database = library::Database::open(&database_path).await.unwrap();
        let source = crate::Source::open(configuration.clone(), None, None).unwrap();
        source
            .catch_up_local(&database, &|_| {}, Arc::new(AtomicBool::new(false)))
            .await
            .expect("catch up the edited roots after an interrupted refresh");
        let tracks = (&library::MemberRead::new(library::Collection::Source(key)))
            .rows(&database, 0, 10, &library::ReadCancellation::new())
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(library::MemberRow::into_track)
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(tracks.len(), 2);
        assert!(tracks.iter().any(|track| track.media_uri == new_uri));
        assert!(tracks.iter().any(|track| track.media_uri == imported_uri));
        assert!(tracks.iter().all(|track| track.media_uri != uri));
        let entries = database
            .playlist_entry_uri_page(playlist, -1)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].2, imported_uri);
        let inventory = database
            .local_file_page(key, None, 128, &library::ReadCancellation::new())
            .await
            .unwrap();
        assert!(!inventory.is_empty());
        assert!(
            inventory
                .iter()
                .any(|file| Path::new(&file.root) == new_root)
        );
        let folders = source.browse_folder(&database, None, None).await.unwrap();
        assert!(
            folders
                .folders
                .iter()
                .all(|folder| !folder.name.contains("old"))
        );
        let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |_, update| {
            let _ = sender.send(update);
            std::future::ready(())
        });
        source
            .catch_up_local(&database, &|_| {}, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        while let Ok(update) = updates.try_recv() {
            assert!(!update.changed());
        }

        let edit = crate::Source::edit(
            configuration,
            None,
            crate::SourceSettingsChange::Local {
                source_id: source.source_id().clone(),
                roots: vec![new_root.clone(), file_root.clone()],
                excluded_folders: None,
            },
            None,
        )
        .await
        .unwrap();
        let crate::SourceEditResult::Connected(connected) = edit else {
            panic!("expected added Local file root")
        };
        let (configuration, source, _) = (*connected).into_parts();
        drop(source);
        database.close().await.unwrap();
        let database = library::Database::open(&database_path).await.unwrap();
        let source = crate::Source::open(configuration.clone(), None, None).unwrap();
        source
            .catch_up_local(&database, &|_| {}, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        let file_uri = url::Url::from_file_path(&file_root).unwrap().to_string();
        let tracks = (&library::MemberRead::new(library::Collection::Source(key)))
            .rows(&database, 0, 10, &library::ReadCancellation::new())
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(library::MemberRow::into_track)
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(tracks.len(), 3);
        assert!(tracks.iter().any(|track| track.media_uri == file_uri));
        let inventory = database
            .local_file_page(key, None, 128, &library::ReadCancellation::new())
            .await
            .unwrap();
        assert!(
            inventory
                .iter()
                .any(|file| Path::new(&file.root) == file_root)
        );
        let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |_, update| {
            let _ = sender.send(update);
            std::future::ready(())
        });
        source
            .catch_up_local(&database, &|_| {}, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        while let Ok(update) = updates.try_recv() {
            assert!(!update.changed());
        }

        database.delete_playlist(None, playlist).await.unwrap();
        let edit = crate::Source::edit(
            configuration,
            None,
            crate::SourceSettingsChange::Local {
                source_id: source.source_id().clone(),
                roots: Vec::new(),
                excluded_folders: Some(vec![excluded.clone()]),
            },
            None,
        )
        .await
        .unwrap();
        let crate::SourceEditResult::Connected(connected) = edit else {
            panic!("expected removed Local roots")
        };
        let (configuration, source, _) = (*connected).into_parts();
        source
            .acquire_catalog(&database, &|_| {}, Arc::new(AtomicBool::new(false)), true)
            .await
            .unwrap();
        let key = database
            .source_identity_key(source.source_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (&library::MemberRead::new(library::Collection::Source(key)))
                .count(&database, &library::ReadCancellation::new())
                .await
                .unwrap(),
            1
        );
        let cached = database
            .tracks_by_uris(
                std::slice::from_ref(&uri),
                &library::ReadCancellation::new(),
            )
            .await
            .unwrap();
        assert_eq!(cached.len(), 1);
        let physical = database
            .local_file_page(key, None, 128, &library::ReadCancellation::new())
            .await
            .unwrap();
        assert!(
            physical
                .iter()
                .find(|file| Path::new(&file.path) == audio)
                .unwrap()
                .current_output,
            "removing a configured root keeps its accepted parsed output"
        );
        let edit = crate::Source::edit(
            configuration,
            None,
            crate::SourceSettingsChange::Local {
                source_id: source.source_id().clone(),
                roots: vec![old_root.clone()],
                excluded_folders: None,
            },
            None,
        )
        .await
        .unwrap();
        let crate::SourceEditResult::Connected(connected) = edit else {
            panic!("expected restored Local root")
        };
        let (_, source, _) = (*connected).into_parts();
        source
            .acquire_catalog(&database, &|_| {}, Arc::new(AtomicBool::new(false)), true)
            .await
            .unwrap();
        assert_eq!(
            library::MemberRead::new(library::Collection::Source(key))
                .count(&database, &library::ReadCancellation::new())
                .await
                .unwrap(),
            1,
            "the excluded descendant must not be re-admitted with its parent root"
        );
        let members = library::MemberRead::new(library::Collection::Source(key))
            .rows(&database, 0, 10, &library::ReadCancellation::new())
            .await
            .unwrap();
        assert_eq!(
            members[0].track().media_uri,
            imported_uri,
            "the explicit import overrides the excluded folder"
        );
        let after_edit = database
            .tracks_by_uris(
                std::slice::from_ref(&uri),
                &library::ReadCancellation::new(),
            )
            .await
            .unwrap();
        assert_eq!(after_edit[0].track_key, cached[0].track_key);
        assert_eq!(after_edit[0].object_id, cached[0].object_id);
        for (path, original) in paths.into_iter().zip(original_files) {
            assert_eq!(fs::read(path).unwrap(), original);
        }
    }

    #[tokio::test]
    async fn imported_local_files_survive_rescan_and_playlist_deletion() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let audio = root.join("Jóga #1.wav");
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
        let (configuration, source, _) = connected.into_parts();
        database
            .reconcile_source(
                &configuration.source_id,
                &configuration.kind,
                &configuration.name,
            )
            .await
            .unwrap();
        let imported_path = root.join("Jóga #1.wav");
        #[cfg(unix)]
        let imported_path = {
            let alias = root.join("alias.wav");
            std::os::unix::fs::symlink(&imported_path, &alias).unwrap();
            alias
        };
        let path = root.join("mix.m3u8");
        let contents = format!("{0}\n{0}\n", imported_path.display());
        fs::write(&path, &contents).unwrap();
        let playlist = crate::import_local_playlist_file(&database, &path, None, |_| None)
            .await
            .unwrap();
        source
            .import_playlist_files(&database, playlist.playlist)
            .await
            .unwrap();
        let uri = url::Url::from_file_path(&audio).unwrap().to_string();
        let entries = database
            .playlist_entry_uri_page(playlist.playlist, -1)
            .await
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|(_, _, value)| value == &uri));
        fs::write(&path, format!("#EXTM3U\n{contents}")).unwrap();
        let reloaded =
            crate::import_local_playlist_file(&database, &path, Some(playlist.playlist), |_| None)
                .await
                .unwrap();
        assert!(!reloaded.changed);
        assert_eq!(
            database
                .playlist_entry_uri_page(playlist.playlist, -1)
                .await
                .unwrap(),
            entries
        );
        #[cfg(unix)]
        {
            let other = root.join("other.wav");
            fs::write(&other, silent_wav()).unwrap();
            fs::remove_file(&imported_path).unwrap();
            std::os::unix::fs::symlink(&other, &imported_path).unwrap();
            crate::import_local_playlist_file(&database, &path, Some(playlist.playlist), |_| None)
                .await
                .unwrap();
            let other_uri = url::Url::from_file_path(&other).unwrap().to_string();
            let retargeted = database
                .playlist_entry_uri_page(playlist.playlist, -1)
                .await
                .unwrap();
            assert_eq!(retargeted.len(), 2);
            assert!(retargeted.iter().all(|(_, _, value)| value == &other_uri));
            fs::remove_file(&imported_path).unwrap();
            std::os::unix::fs::symlink(&audio, &imported_path).unwrap();
            crate::import_local_playlist_file(&database, &path, Some(playlist.playlist), |_| None)
                .await
                .unwrap();
        }
        assert!(
            database
                .tracks_by_uris(
                    std::slice::from_ref(&uri),
                    &library::ReadCancellation::new()
                )
                .await
                .map(|rows| { rows.into_iter().next() })
                .unwrap()
                .is_some()
        );
        source
            .acquire_catalog(
                &database,
                &|_| {},
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                true,
            )
            .await
            .unwrap();
        assert!(
            database
                .tracks_by_uris(
                    std::slice::from_ref(&uri),
                    &library::ReadCancellation::new()
                )
                .await
                .map(|rows| { rows.into_iter().next() })
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
        assert_eq!(
            library::MemberRead::new(library::Collection::Source(source_key))
                .count(&database, &library::ReadCancellation::new())
                .await
                .unwrap(),
            1
        );
        let folders = source.browse_folder(&database, None, None).await.unwrap();
        assert!(
            folders.folders.is_empty() && folders.tracks.is_empty(),
            "outside-root imports remain outside library folders"
        );
        database
            .delete_playlist(None, playlist.playlist)
            .await
            .unwrap();
        assert!(
            database
                .tracks_by_uris(
                    std::slice::from_ref(&uri),
                    &library::ReadCancellation::new()
                )
                .await
                .map(|rows| { rows.into_iter().next() })
                .unwrap()
                .is_some()
        );
        database.delete_playlist(None, second).await.unwrap();
        let key = database
            .source_identity_key(source.source_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (&library::MemberRead::new(library::Collection::Source(key)))
                .count(&database, &library::ReadCancellation::new())
                .await
                .unwrap(),
            1
        );
        assert!(
            database
                .tracks_by_uris(
                    std::slice::from_ref(&uri),
                    &library::ReadCancellation::new()
                )
                .await
                .map(|rows| { rows.into_iter().next() })
                .unwrap()
                .is_some()
        );
        assert!(audio.is_file());
        database.close().await.unwrap();
        let database = library::Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        source
            .catch_up_local(
                &database,
                &|_| {},
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert_eq!(
            library::MemberRead::new(library::Collection::Source(key))
                .count(&database, &library::ReadCancellation::new())
                .await
                .unwrap(),
            1
        );
        fs::remove_file(&audio).unwrap();
        source
            .catch_up_local(
                &database,
                &|_| {},
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert_eq!(
            library::MemberRead::new(library::Collection::Source(key))
                .count(&database, &library::ReadCancellation::new())
                .await
                .unwrap(),
            0
        );
        fs::write(&audio, silent_wav()).unwrap();
        source
            .catch_up_local(
                &database,
                &|_| {},
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert_eq!(
            library::MemberRead::new(library::Collection::Source(key))
                .count(&database, &library::ReadCancellation::new())
                .await
                .unwrap(),
            1
        );
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

#[cfg(test)]
pub(crate) fn metadata_file_available(path: &Path, source_format: Option<&str>) -> bool {
    source_format
        .and_then(super::lofty::MetadataWriter::for_source_format)
        .or_else(|| super::lofty::MetadataWriter::for_path(path))
        .is_some()
}

pub(crate) fn read_metadata_input(
    input: impl std::io::Read + std::io::Seek,
    path: &Path,
) -> Result<MetadataDraft, SourceMetadataError> {
    let tagged = super::lofty::read_lofty_file(
        input,
        ::lofty::config::ParseOptions::new().read_cover_art(false),
    )
    .map_err(write_error)?;
    let writer = tagged
        .as_ref()
        .and_then(|file| MetadataWriter::for_file_type(file.file_type()));
    let tag = tagged
        .as_ref()
        .and_then(|file| file.primary_tag().or_else(|| file.first_tag()));
    let mut values = fields::read(tag, writer, 1);
    if writer.is_none() {
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
    let mut metadata = MetadataDraft::new(SourceEntityKind::Track, values);
    metadata.track_count = 1;
    metadata.artwork.can_embed = writer.is_some_and(|writer| {
        writer
            .file_type()
            .tag_support(writer.file_type().primary_tag_type())
            .is_writable()
            && match writer.file_type().primary_tag_type() {
                ::lofty::tag::TagType::Id3v2
                | ::lofty::tag::TagType::VorbisComments
                | ::lofty::tag::TagType::Ape => true,
                ::lofty::tag::TagType::Mp4Ilst => true,
                _ => false,
            }
    });
    Ok(metadata)
}

#[cfg(test)]
pub(crate) fn read_track_metadata(
    path: &Path,
    _format: Option<&str>,
) -> Result<MetadataDraft, SourceMetadataError> {
    let mut draft = read_metadata_input(fs::File::open(path).map_err(write_error)?, path)?;
    draft.revision = Some(revision(path)?);
    Ok(draft)
}

pub(crate) fn readonly_track_metadata(
    track: &library::TrackView,
    revision: Option<String>,
) -> MetadataDraft {
    let mut metadata = MetadataDraft::new(
        SourceEntityKind::Track,
        [
            ("title", track.metadata.title.clone()),
            ("artist", track.metadata.artist.clone()),
            ("album", track.metadata.album.clone()),
            (
                "album_artist",
                track
                    .metadata
                    .album_display_artist
                    .clone()
                    .unwrap_or_default(),
            ),
            (
                "track_number",
                (track.metadata.track_number > 0)
                    .then(|| track.metadata.track_number.to_string())
                    .unwrap_or_default(),
            ),
            (
                "disc_number",
                (track.metadata.disc_number > 0)
                    .then(|| track.metadata.disc_number.to_string())
                    .unwrap_or_default(),
            ),
            (
                "recording_date",
                track
                    .metadata
                    .year
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            (
                "genre",
                track
                    .metadata
                    .genres
                    .iter()
                    .map(|genre| genre.name.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            (
                "comment",
                track.metadata.comment.clone().unwrap_or_default(),
            ),
            (
                "bpm",
                track
                    .metadata
                    .bpm
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            (
                "musicbrainz_recording_id",
                track
                    .metadata
                    .musicbrainz_recording_id
                    .clone()
                    .unwrap_or_default(),
            ),
            (
                "musicbrainz_release_track_id",
                track
                    .metadata
                    .musicbrainz_release_track_id
                    .clone()
                    .unwrap_or_default(),
            ),
            (
                "musicbrainz_album_id",
                track
                    .metadata
                    .musicbrainz_album_id
                    .clone()
                    .unwrap_or_default(),
            ),
            (
                "musicbrainz_release_group_id",
                track
                    .metadata
                    .musicbrainz_release_group_id
                    .clone()
                    .unwrap_or_default(),
            ),
            (
                "musicbrainz_artist_id",
                track
                    .metadata
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
    album: &library::AlbumView,
    revision: Option<String>,
) -> MetadataDraft {
    let mut metadata = MetadataDraft::new(
        SourceEntityKind::Album,
        [
            ("title", album.metadata.title.clone()),
            ("artist", album.metadata.artist.clone()),
            ("album_artist", album.metadata.artist.clone()),
            (
                "recording_date",
                album
                    .metadata
                    .year
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            (
                "genre",
                album
                    .metadata
                    .genres
                    .iter()
                    .map(|genre| genre.name.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            (
                "musicbrainz_album_id",
                album
                    .metadata
                    .musicbrainz_release_id
                    .clone()
                    .unwrap_or_default(),
            ),
            (
                "musicbrainz_release_group_id",
                album
                    .metadata
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
    metadata.track_count = album.summary.track_count.max(0) as usize;
    metadata
}

pub(crate) fn readonly_artist_metadata(
    artist: &library::ArtistView,
    revision: Option<String>,
) -> MetadataDraft {
    let mut metadata = MetadataDraft::new(
        SourceEntityKind::Artist,
        vec![
            MetadataField::new("title", artist.metadata.name.clone(), false),
            MetadataField::new(
                "musicbrainz_artist_id",
                artist
                    .metadata
                    .musicbrainz_artist_id
                    .clone()
                    .unwrap_or_default(),
                false,
            ),
        ],
    );
    metadata.revision = revision;
    metadata.track_count = artist.summary.track_count.max(0) as usize;
    metadata
}
