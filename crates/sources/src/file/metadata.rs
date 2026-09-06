//! File tag semantics and atomic edits for originals and remote working copies.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ::lofty::config::WriteOptions;
use ::lofty::file::TaggedFileExt;
use ::lofty::prelude::{Accessor, TagExt};
use ::lofty::tag::items::popularimeter::{Popularimeter, StarRating};
use ::lofty::tag::{ItemKey, Tag};

use super::lofty::{MetadataWriter, bpm_key, read_lofty_for_edit};
use crate::{AlbumMetadataEdit, ArtistMetadataEdit, SourceMetadataError, TrackMetadataEdit};

pub(crate) fn revision(path: &Path) -> Result<String, SourceMetadataError> {
    let metadata = fs::metadata(path).map_err(write_error)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |value| value.as_nanos());
    Ok(format!("{}:{modified}", metadata.len()))
}

pub(crate) fn combined_revision(paths: &[PathBuf]) -> Result<String, SourceMetadataError> {
    let mut value = String::from("v1");
    for path in paths {
        let revision = revision(path)?;
        let path = path.to_string_lossy();
        value.push_str(&format!(":{}:{path}:{revision}", path.len()));
    }
    Ok(value)
}

pub(crate) fn write_track(
    path: &Path,
    source_format: Option<&str>,
    expected_revision: &str,
    edit: &TrackMetadataEdit,
) -> Result<(), SourceMetadataError> {
    if revision(path)? != expected_revision {
        return Err(SourceMetadataError::Conflict);
    }
    if edit.changed == Default::default() {
        return Ok(());
    }
    let values = &edit.values;
    let changed = &edit.changed;
    let prepared = prepare_file(path, source_format, None, |tag, writer| {
        if changed.title {
            tag.set_title(values.title.trim().to_string());
        }
        if changed.sort_title {
            set_text(
                tag,
                ItemKey::TrackTitleSortOrder,
                values.sort_title.as_deref(),
            );
        }
        if changed.artist {
            set_text(tag, ItemKey::TrackArtist, values.artist.as_deref());
        }
        if changed.album {
            set_text(tag, ItemKey::AlbumTitle, values.album.as_deref());
        }
        if changed.album_artist {
            set_text(tag, ItemKey::AlbumArtist, values.album_artist.as_deref());
        }
        if changed.track_number {
            set_number(tag, ItemKey::TrackNumber, values.track_number);
        }
        if changed.disc_number {
            set_number(tag, ItemKey::DiscNumber, values.disc_number);
        }
        if changed.year {
            set_number(tag, ItemKey::RecordingDate, values.year);
        }
        if changed.genre {
            set_text(tag, ItemKey::Genre, values.genre.as_deref());
        }
        if changed.comment {
            set_text(tag, ItemKey::Comment, values.comment.as_deref());
        }
        if changed.bpm {
            tag.remove_key(ItemKey::Bpm);
            tag.remove_key(ItemKey::IntegerBpm);
            if let (Some(value), Some(key)) =
                (values.bpm, bpm_key(writer.file_type().primary_tag_type()))
            {
                tag.insert_text(key, value.to_string());
            }
        }
        if changed.musicbrainz_recording_id {
            set_text(
                tag,
                ItemKey::MusicBrainzRecordingId,
                values.musicbrainz_recording_id.as_deref(),
            );
        }
        if changed.musicbrainz_release_track_id {
            set_text(
                tag,
                ItemKey::MusicBrainzTrackId,
                values.musicbrainz_release_track_id.as_deref(),
            );
        }
        if changed.musicbrainz_album_id {
            set_text(
                tag,
                ItemKey::MusicBrainzReleaseId,
                values.musicbrainz_album_id.as_deref(),
            );
        }
        if changed.musicbrainz_release_group_id {
            set_text(
                tag,
                ItemKey::MusicBrainzReleaseGroupId,
                values.musicbrainz_release_group_id.as_deref(),
            );
        }
        if changed.musicbrainz_artist_id {
            set_text(
                tag,
                ItemKey::MusicBrainzArtistId,
                values.musicbrainz_artist_id.as_deref(),
            );
        }
    })?;
    commit_batch(vec![prepared])
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
    })?;
    commit_batch(vec![prepared])
}

fn r128_gain_text(integrated_lufs: f64) -> String {
    let gain = ((-23.0 - integrated_lufs) * 256.0).round();
    (gain.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16).to_string()
}

pub(crate) fn write_album_batch(
    targets: &[(PathBuf, Option<String>)],
    expected_revision: &str,
    edit: &AlbumMetadataEdit,
) -> Result<(), SourceMetadataError> {
    let paths = targets
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    if combined_revision(&paths)? != expected_revision {
        return Err(SourceMetadataError::Conflict);
    }
    if edit.changed == Default::default() {
        return Ok(());
    }
    let values = &edit.values;
    let changed = &edit.changed;
    let mut prepared = Vec::with_capacity(targets.len());
    for (path, format) in targets {
        prepared.push(prepare_file(path, format.as_deref(), None, |tag, _| {
            if changed.title {
                set_text(tag, ItemKey::AlbumTitle, Some(&values.title));
            }
            if changed.sort_title {
                set_text(
                    tag,
                    ItemKey::AlbumTitleSortOrder,
                    values.sort_title.as_deref(),
                );
            }
            if changed.artist {
                set_text(tag, ItemKey::TrackArtist, values.artist.as_deref());
            }
            if changed.album_artist {
                set_text(tag, ItemKey::AlbumArtist, values.album_artist.as_deref());
            }
            if changed.year {
                set_number(tag, ItemKey::RecordingDate, values.year);
            }
            if changed.genre {
                set_text(tag, ItemKey::Genre, values.genre.as_deref());
            }
            if changed.comment {
                set_text(tag, ItemKey::Comment, values.comment.as_deref());
            }
            if changed.musicbrainz_album_id {
                set_text(
                    tag,
                    ItemKey::MusicBrainzReleaseId,
                    values.musicbrainz_album_id.as_deref(),
                );
            }
            if changed.musicbrainz_release_group_id {
                set_text(
                    tag,
                    ItemKey::MusicBrainzReleaseGroupId,
                    values.musicbrainz_release_group_id.as_deref(),
                );
            }
        })?);
    }
    commit_batch(prepared)
}

pub(crate) fn write_artist_batch(
    targets: &[(PathBuf, Option<String>)],
    expected_revision: &str,
    previous_name: &str,
    edit: &ArtistMetadataEdit,
) -> Result<(), SourceMetadataError> {
    let paths = targets
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    if combined_revision(&paths)? != expected_revision {
        return Err(SourceMetadataError::Conflict);
    }
    if edit.changed == Default::default() {
        return Ok(());
    }
    let values = &edit.values;
    let changed = &edit.changed;
    let mut prepared = Vec::with_capacity(targets.len());
    for (path, format) in targets {
        prepared.push(prepare_file(path, format.as_deref(), None, |tag, _| {
            if changed.name {
                let artists = tag
                    .artist()
                    .map(|value| replace_name(&value, previous_name, &values.name))
                    .unwrap_or_else(|| values.name.clone());
                tag.set_artist(artists);
            }
            if changed.sort_name {
                set_text(
                    tag,
                    ItemKey::TrackArtistSortOrder,
                    values.sort_name.as_deref(),
                );
            }
            if changed.genre {
                set_text(tag, ItemKey::Genre, values.genre.as_deref());
            }
            if changed.comment {
                set_text(tag, ItemKey::Comment, values.comment.as_deref());
            }
            if changed.musicbrainz_artist_id {
                set_text(
                    tag,
                    ItemKey::MusicBrainzArtistId,
                    values.musicbrainz_artist_id.as_deref(),
                );
            }
        })?);
    }
    commit_batch(prepared)
}

struct PreparedFile {
    target: PathBuf,
    temp: tempfile::TempPath,
}

fn prepare_file(
    path: &Path,
    source_format: Option<&str>,
    tag_type: Option<::lofty::tag::TagType>,
    mutate: impl FnOnce(&mut Tag, MetadataWriter),
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
    let tagged = read_lofty_for_edit(temp.path(), writer.file_type())
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
    mutate(&mut tag, writer);
    if tag.tag_type() == ::lofty::tag::TagType::Id3v2
        && matches!(
            writer.file_type(),
            ::lofty::file::FileType::Wav | ::lofty::file::FileType::Aiff
        )
    {
        // Lofty 0.25.1 subtracts growth when resizing an existing terminal ID3
        // chunk. Remove that chunk before writing its complete edited tag so
        // RIFF/FORM length stays valid and subsequent edits read the new values.
        let mut file = temp.reopen().map_err(write_error)?;
        tag.tag_type()
            .remove_from(&mut file, WriteOptions::new())
            .map_err(write_error)?;
    }
    save_tag(&tag, temp.path())?;
    temp.as_file().sync_all().map_err(write_error)?;
    Ok(PreparedFile {
        target: path.to_path_buf(),
        temp: temp.into_temp_path(),
    })
}

fn commit_batch(mut prepared: Vec<PreparedFile>) -> Result<(), SourceMetadataError> {
    for index in 0..prepared.len() {
        let file = &mut prepared[index];
        if let Err(error) = replace(&mut file.temp, &file.target) {
            for rollback in prepared[..index].iter_mut().rev() {
                let _ = replace(&mut rollback.temp, &rollback.target);
            }
            return Err(error);
        }
    }
    for file in &prepared {
        if let Some(parent) = file.target.parent() {
            sync_parent(parent)?;
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

fn save_tag(tag: &Tag, path: &Path) -> Result<(), SourceMetadataError> {
    tag.save_to_path(path, WriteOptions::new().remove_others(false))
        .map_err(write_error)
}

fn set_text(tag: &mut Tag, key: ItemKey, value: Option<&str>) {
    tag.remove_key(key);
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        tag.insert_text(key, value.to_string());
    }
}

fn set_number(tag: &mut Tag, key: ItemKey, value: Option<u16>) {
    set_text(
        tag,
        key,
        value.as_ref().map(|value| value.to_string()).as_deref(),
    );
}

fn replace_name(value: &str, previous: &str, replacement: &str) -> String {
    value
        .split([';', ','])
        .map(str::trim)
        .map(|name| if name == previous { replacement } else { name })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
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
    use crate::{
        AlbumMetadataValues, AlbumMetadataWritable, TrackMetadataValues, TrackMetadataWritable,
    };

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
            let edit = crate::AlbumMetadataEdit {
                values: crate::AlbumMetadataValues {
                    title: title.into(),
                    ..Default::default()
                },
                changed: crate::AlbumMetadataWritable {
                    title: true,
                    ..Default::default()
                },
            };
            let paths = [file.to_path_buf()];
            let revision = combined_revision(&paths).unwrap();
            write_album_batch(
                &[(file.to_path_buf(), Some("wav".into()))],
                &revision,
                &edit,
            )
            .unwrap();
            let observed = read_track_metadata(&file, Some("wav")).unwrap();
            assert_eq!(observed.values.album.as_deref(), Some(title));
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

    fn track_edit(values: TrackMetadataValues) -> TrackMetadataEdit {
        TrackMetadataEdit {
            values,
            changed: TrackMetadataWritable {
                title: true,
                sort_title: true,
                artist: true,
                album: true,
                album_artist: true,
                track_number: true,
                disc_number: true,
                year: true,
                genre: true,
                comment: true,
                bpm: true,
                locked: true,
                musicbrainz_recording_id: true,
                musicbrainz_release_track_id: true,
                musicbrainz_album_id: true,
                musicbrainz_release_group_id: true,
                musicbrainz_artist_id: true,
            },
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
        let values = TrackMetadataValues {
            title: "Updated title".to_string(),
            artist: Some("Updated artist".to_string()),
            album: Some("Updated album".to_string()),
            track_number: Some(3),
            ..TrackMetadataValues::default()
        };

        write_track(&path, Some("wav"), &before, &track_edit(values)).expect("write metadata");
        let read = super::read_track_metadata(&path, Some("wav")).expect("read metadata");
        assert!(read.writable.title);
        assert_eq!(read.values.title, "Updated title");
        assert_eq!(read.values.artist.as_deref(), Some("Updated artist"));
        assert_eq!(read.values.track_number, Some(3));
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
            &track_edit(TrackMetadataValues {
                title: "Direct original".to_string(),
                ..TrackMetadataValues::default()
            }),
        )
        .unwrap();
        assert_eq!(
            crate::Source::read_direct_file_metadata(&uri)
                .unwrap()
                .values
                .title,
            "Direct original"
        );
        assert!(
            crate::Source::write_direct_file_metadata(
                &format!("{uri}#cue=1"),
                "",
                &track_edit(TrackMetadataValues::default())
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
        write_track(
            &path,
            Some("wav"),
            &before,
            &track_edit(TrackMetadataValues {
                title: "Track title".to_string(),
                ..TrackMetadataValues::default()
            }),
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
                .values
                .title,
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
            write_track(
                path,
                Some("wav"),
                &before,
                &track_edit(TrackMetadataValues {
                    title: path.file_stem().unwrap().to_string_lossy().into_owned(),
                    album: Some("Before".to_string()),
                    genre: Some(format!("Genre {index}")),
                    ..TrackMetadataValues::default()
                }),
            )
            .expect("seed Track metadata");
        }
        let targets = paths
            .iter()
            .cloned()
            .map(|path| (path, Some("wav".to_string())))
            .collect::<Vec<_>>();
        let before = combined_revision(&paths).expect("Album revision");
        write_album_batch(
            &targets,
            &before,
            &AlbumMetadataEdit {
                values: AlbumMetadataValues {
                    title: "After".to_string(),
                    album_artist: Some("Album Artist".to_string()),
                    ..AlbumMetadataValues::default()
                },
                changed: AlbumMetadataWritable {
                    title: true,
                    album_artist: true,
                    ..AlbumMetadataWritable::default()
                },
            },
        )
        .expect("write Album metadata");

        for (index, path) in paths.iter().enumerate() {
            let values =
                super::read_album_metadata_values(path, Some("wav")).expect("read Album metadata");
            assert_eq!(values.title, "After");
            assert_eq!(values.album_artist.as_deref(), Some("Album Artist"));
            assert_eq!(
                values.genre.as_deref(),
                Some(format!("Genre {index}").as_str()),
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

        let result = write_track(
            &path,
            Some("wav"),
            &stale,
            &track_edit(TrackMetadataValues {
                title: "Should not replace".to_string(),
                ..TrackMetadataValues::default()
            }),
        );

        assert!(matches!(result, Err(SourceMetadataError::Conflict)));
        assert_eq!(
            fs::read(&path).expect("unchanged concurrent file"),
            concurrent
        );
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
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
                    tag.set_title("After".into())
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
            crate::SourceSetupInput::Local(crate::LocalFolderHostInput {
                roots: vec![root.clone()],
            }),
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
            crate::SourceSetupInput::Local(crate::LocalFolderHostInput {
                roots: vec![directory.path().to_path_buf()],
            }),
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
        let edit = crate::Source::edit(
            configuration,
            None,
            crate::SourceSettingsInput::Local { roots: Vec::new() },
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
        assert!(
            database
                .track_row_by_uri(&uri, &library::ReadCancellation::new())
                .await
                .unwrap()
                .is_none()
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
            crate::SourceSetupInput::Local(crate::LocalFolderHostInput { roots: Vec::new() }),
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
            .import_playlist_m3u(
                std::io::Cursor::new("Jóga #1.wav\nJóga #1.wav\n"),
                &directory.path().join("mix.m3u8"),
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
        let second = database
            .create_playlist(None, "Second", std::slice::from_ref(&uri))
            .await
            .unwrap()
            .unwrap()
            .0;
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
        assert!(
            database
                .track_row_by_uri(&uri, &library::ReadCancellation::new())
                .await
                .unwrap()
                .is_none()
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

pub(crate) fn read_track_metadata(
    path: &Path,
    source_format: Option<&str>,
) -> Result<crate::TrackMetadata, crate::SourceMetadataError> {
    let writer = source_format
        .and_then(super::lofty::MetadataWriter::for_source_format)
        .or_else(|| super::lofty::MetadataWriter::for_path(path))
        .ok_or(crate::SourceMetadataError::Unavailable)?;
    let tagged = super::lofty::read_lofty_for_edit(path, writer.file_type())
        .map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?
        .ok_or(crate::SourceMetadataError::Unavailable)?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let text = |key| {
        tag.and_then(|tag| tag.get_string(key))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    let number = |key| text(key).and_then(|value| value.parse::<u16>().ok());
    let values = crate::TrackMetadataValues {
        title: tag
            .and_then(|tag| tag.title())
            .map(|value| value.trim().to_string())
            .unwrap_or_default(),
        sort_title: text(ItemKey::TrackTitleSortOrder),
        artist: tag
            .and_then(|tag| tag.artist())
            .map(|value| value.trim().to_string()),
        album: tag
            .and_then(|tag| tag.album())
            .map(|value| value.trim().to_string()),
        album_artist: text(ItemKey::AlbumArtist),
        track_number: tag.and_then(|tag| tag.track()).map(|value| value as u16),
        disc_number: tag.and_then(|tag| tag.disk()).map(|value| value as u16),
        year: tag.and_then(|tag| tag.date()).map(|value| value.year),
        genre: tag
            .and_then(|tag| tag.genre())
            .map(|value| value.trim().to_string()),
        comment: tag
            .and_then(|tag| tag.comment())
            .map(|value| value.trim().to_string()),
        bpm: number(ItemKey::IntegerBpm).or_else(|| number(ItemKey::Bpm)),
        locked: None,
        musicbrainz_recording_id: text(ItemKey::MusicBrainzRecordingId),
        musicbrainz_release_track_id: text(ItemKey::MusicBrainzTrackId),
        musicbrainz_album_id: text(ItemKey::MusicBrainzReleaseId),
        musicbrainz_release_group_id: text(ItemKey::MusicBrainzReleaseGroupId),
        musicbrainz_artist_id: text(ItemKey::MusicBrainzArtistId),
    };
    let can = |key| writer.metadata_key_is_writable(key);
    let writable = crate::TrackMetadataWritable {
        title: can(ItemKey::TrackTitle),
        sort_title: can(ItemKey::TrackTitleSortOrder),
        artist: can(ItemKey::TrackArtist),
        album: can(ItemKey::AlbumTitle),
        album_artist: can(ItemKey::AlbumArtist),
        track_number: can(ItemKey::TrackNumber),
        disc_number: can(ItemKey::DiscNumber),
        year: can(ItemKey::RecordingDate),
        genre: can(ItemKey::Genre),
        comment: can(ItemKey::Comment),
        bpm: super::lofty::bpm_key(writer.file_type().primary_tag_type()).is_some(),
        locked: false,
        musicbrainz_recording_id: can(ItemKey::MusicBrainzRecordingId),
        musicbrainz_release_track_id: can(ItemKey::MusicBrainzTrackId),
        musicbrainz_album_id: can(ItemKey::MusicBrainzReleaseId),
        musicbrainz_release_group_id: can(ItemKey::MusicBrainzReleaseGroupId),
        musicbrainz_artist_id: can(ItemKey::MusicBrainzArtistId),
    };
    let metadata =
        fs::metadata(path).map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |value| value.as_nanos());
    Ok(crate::TrackMetadata {
        writable,
        source_search: false,
        revision: Some(format!("{}:{modified}", metadata.len())),
        source_values: values.clone(),
        values,
        rufin_filled: crate::TrackMetadataWritable::default(),
    })
}

pub(crate) fn read_album_metadata_values(
    path: &Path,
    source_format: Option<&str>,
) -> Result<crate::AlbumMetadataValues, crate::SourceMetadataError> {
    let writer = source_format
        .and_then(super::lofty::MetadataWriter::for_source_format)
        .or_else(|| super::lofty::MetadataWriter::for_path(path))
        .ok_or(crate::SourceMetadataError::Unavailable)?;
    let tagged = super::lofty::read_lofty_for_edit(path, writer.file_type())
        .map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?
        .ok_or(crate::SourceMetadataError::Unavailable)?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let text = |key| {
        tag.and_then(|tag| tag.get_string(key))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    Ok(crate::AlbumMetadataValues {
        title: tag
            .and_then(|tag| tag.album())
            .map(|value| value.trim().to_string())
            .unwrap_or_default(),
        sort_title: text(ItemKey::AlbumTitleSortOrder),
        artist: text(ItemKey::TrackArtist),
        album_artist: text(ItemKey::AlbumArtist),
        year: tag.and_then(|tag| tag.date()).map(|value| value.year),
        genre: tag
            .and_then(|tag| tag.genre())
            .map(|value| value.trim().to_string()),
        comment: tag
            .and_then(|tag| tag.comment())
            .map(|value| value.trim().to_string()),
        locked: None,
        musicbrainz_album_id: text(ItemKey::MusicBrainzReleaseId),
        musicbrainz_release_group_id: text(ItemKey::MusicBrainzReleaseGroupId),
    })
}

pub(crate) fn read_artist_metadata_values(
    path: &Path,
    source_format: Option<&str>,
    fallback_name: &str,
) -> Result<crate::ArtistMetadataValues, crate::SourceMetadataError> {
    let writer = source_format
        .and_then(super::lofty::MetadataWriter::for_source_format)
        .or_else(|| super::lofty::MetadataWriter::for_path(path))
        .ok_or(crate::SourceMetadataError::Unavailable)?;
    let tagged = super::lofty::read_lofty_for_edit(path, writer.file_type())
        .map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?
        .ok_or(crate::SourceMetadataError::Unavailable)?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let text = |key| {
        tag.and_then(|tag| tag.get_string(key))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    Ok(crate::ArtistMetadataValues {
        name: fallback_name.to_string(),
        sort_name: text(ItemKey::TrackArtistSortOrder),
        genre: tag
            .and_then(|tag| tag.genre())
            .map(|value| value.trim().to_string()),
        comment: tag
            .and_then(|tag| tag.comment())
            .map(|value| value.trim().to_string()),
        locked: None,
        musicbrainz_artist_id: text(ItemKey::MusicBrainzArtistId),
    })
}

pub(crate) struct MetadataFileTarget {
    pub path: PathBuf,
    pub format: Option<String>,
}

pub(crate) fn read_album_file(
    album: &library::AlbumRow,
    current: Option<crate::AlbumMetadata>,
    path: &std::path::Path,
    format: Option<&str>,
) -> Result<crate::AlbumMetadata, crate::SourceMetadataError> {
    let track = crate::file::metadata::read_track_metadata(path, format)?;
    let mut values = crate::file::metadata::read_album_metadata_values(path, format)?;
    if let Some(mut metadata) = current {
        metadata.mixed.title |= values.title != metadata.source_values.title;
        metadata.mixed.sort_title |= values.sort_title != metadata.source_values.sort_title;
        metadata.mixed.artist |= values.artist != metadata.source_values.artist;
        metadata.mixed.album_artist |= values.album_artist != metadata.source_values.album_artist;
        metadata.mixed.year |= values.year != metadata.source_values.year;
        metadata.mixed.genre |= values.genre != metadata.source_values.genre;
        metadata.mixed.comment |= values.comment != metadata.source_values.comment;
        metadata.mixed.musicbrainz_album_id |=
            values.musicbrainz_album_id != metadata.source_values.musicbrainz_album_id;
        metadata.mixed.musicbrainz_release_group_id |= values.musicbrainz_release_group_id
            != metadata.source_values.musicbrainz_release_group_id;
        metadata.writable.title &= track.writable.album;
        metadata.writable.sort_title &= track.writable.sort_title;
        metadata.writable.artist &= track.writable.artist;
        metadata.writable.album_artist &= track.writable.album_artist;
        metadata.writable.year &= track.writable.year;
        metadata.writable.genre &= track.writable.genre;
        metadata.writable.comment &= track.writable.comment;
        metadata.writable.musicbrainz_album_id &= track.writable.musicbrainz_album_id;
        metadata.writable.musicbrainz_release_group_id &=
            track.writable.musicbrainz_release_group_id;
        metadata.track_count += 1;
        return Ok(metadata);
    }
    if values.title.is_empty() {
        values.title = album.title.clone();
    }
    if values.album_artist.is_none() {
        values.album_artist = Some(album.display_artist.clone());
    }
    if values.artist.is_none() {
        values.artist = Some(album.display_artist.clone());
    }
    let source_values = values.clone();
    let mut rufin_filled = crate::AlbumMetadataWritable::default();
    if values.musicbrainz_album_id.is_none() && album.musicbrainz_release_id.is_some() {
        values.musicbrainz_album_id = album.musicbrainz_release_id.clone();
        rufin_filled.musicbrainz_album_id = true;
    }
    if values.musicbrainz_release_group_id.is_none() && album.musicbrainz_release_group_id.is_some()
    {
        values.musicbrainz_release_group_id = album.musicbrainz_release_group_id.clone();
        rufin_filled.musicbrainz_release_group_id = true;
    }
    Ok(crate::AlbumMetadata {
        writable: crate::AlbumMetadataWritable {
            title: track.writable.album,
            sort_title: track.writable.sort_title,
            artist: track.writable.artist,
            album_artist: track.writable.album_artist,
            year: track.writable.year,
            genre: track.writable.genre,
            comment: track.writable.comment,
            locked: false,
            musicbrainz_album_id: track.writable.musicbrainz_album_id,
            musicbrainz_release_group_id: track.writable.musicbrainz_release_group_id,
        },
        source_search: false,
        revision: None,
        source_values,
        values,
        rufin_filled,
        track_count: 1,
        mixed: crate::AlbumMetadataMixed::default(),
    })
}

pub(crate) fn album_metadata_from_targets(
    album: library::AlbumRow,
    targets: &[MetadataFileTarget],
) -> Result<crate::AlbumMetadata, crate::SourceMetadataError> {
    let mut metadata = None;
    for target in targets {
        metadata = Some(read_album_file(
            &album,
            metadata,
            &target.path,
            target.format.as_deref(),
        )?);
    }
    let mut metadata = metadata.ok_or(crate::SourceMetadataError::Unavailable)?;
    let paths = targets
        .iter()
        .map(|target| target.path.clone())
        .collect::<Vec<_>>();
    metadata.revision = Some(combined_revision(&paths)?);
    Ok(metadata)
}
pub(crate) fn read_artist_file(
    artist: &library::ArtistRow,
    current: Option<crate::ArtistMetadata>,
    path: &std::path::Path,
    format: Option<&str>,
) -> Result<crate::ArtistMetadata, crate::SourceMetadataError> {
    let track = crate::file::metadata::read_track_metadata(path, format)?;
    let mut values =
        crate::file::metadata::read_artist_metadata_values(path, format, &artist.name)?;
    if let Some(mut metadata) = current {
        metadata.mixed.sort_name |= values.sort_name != metadata.source_values.sort_name;
        metadata.mixed.genre |= values.genre != metadata.source_values.genre;
        metadata.mixed.comment |= values.comment != metadata.source_values.comment;
        metadata.mixed.musicbrainz_artist_id |=
            values.musicbrainz_artist_id != metadata.source_values.musicbrainz_artist_id;
        metadata.writable.name &= track.writable.artist;
        metadata.writable.sort_name &= track.writable.sort_title;
        metadata.writable.genre &= track.writable.genre;
        metadata.writable.comment &= track.writable.comment;
        metadata.writable.musicbrainz_artist_id &= track.writable.musicbrainz_artist_id;
        metadata.track_count += 1;
        return Ok(metadata);
    }
    let source_values = values.clone();
    let mut rufin_filled = crate::ArtistMetadataWritable::default();
    if values.musicbrainz_artist_id.is_none() && artist.musicbrainz_artist_id.is_some() {
        values.musicbrainz_artist_id = artist.musicbrainz_artist_id.clone();
        rufin_filled.musicbrainz_artist_id = true;
    }
    Ok(crate::ArtistMetadata {
        writable: crate::ArtistMetadataWritable {
            name: track.writable.artist,
            sort_name: track.writable.sort_title,
            genre: track.writable.genre,
            comment: track.writable.comment,
            locked: false,
            musicbrainz_artist_id: track.writable.musicbrainz_artist_id,
        },
        source_search: false,
        revision: None,
        source_values,
        values,
        rufin_filled,
        track_count: 1,
        mixed: crate::ArtistMetadataMixed::default(),
    })
}

pub(crate) fn artist_metadata_from_targets(
    artist: library::ArtistRow,
    targets: &[MetadataFileTarget],
) -> Result<crate::ArtistMetadata, crate::SourceMetadataError> {
    let mut metadata = None;
    for target in targets {
        metadata = Some(read_artist_file(
            &artist,
            metadata,
            &target.path,
            target.format.as_deref(),
        )?);
    }
    let mut metadata = metadata.ok_or(crate::SourceMetadataError::Unavailable)?;
    let paths = targets
        .iter()
        .map(|target| target.path.clone())
        .collect::<Vec<_>>();
    metadata.revision = Some(combined_revision(&paths)?);
    Ok(metadata)
}
