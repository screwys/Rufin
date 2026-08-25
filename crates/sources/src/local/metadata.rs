//! Atomic metadata reads and writes for exact Local or mapped-Local files.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use lofty::config::WriteOptions;
use lofty::file::TaggedFileExt;
use lofty::prelude::{Accessor, TagExt};
use lofty::tag::{ItemKey, Tag};

use super::lofty_metadata::{MetadataWriter, bpm_key, read_lofty_for_edit};
use crate::{AlbumMetadataValues, ArtistMetadataValues, SourceMetadataError, TrackMetadataValues};

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
    values: &TrackMetadataValues,
) -> Result<(), SourceMetadataError> {
    if revision(path)? != expected_revision {
        return Err(SourceMetadataError::Conflict);
    }
    let prepared = prepare_file(path, source_format, |tag, writer| {
        tag.set_title(values.title.trim().to_string());
        set_text(
            tag,
            ItemKey::TrackTitleSortOrder,
            values.sort_title.as_deref(),
        );
        set_text(tag, ItemKey::TrackArtist, values.artist.as_deref());
        set_text(tag, ItemKey::AlbumTitle, values.album.as_deref());
        set_text(tag, ItemKey::AlbumArtist, values.album_artist.as_deref());
        set_number(tag, ItemKey::TrackNumber, values.track_number);
        set_number(tag, ItemKey::DiscNumber, values.disc_number);
        set_number(tag, ItemKey::RecordingDate, values.year);
        set_text(tag, ItemKey::Genre, values.genre.as_deref());
        set_text(tag, ItemKey::Comment, values.comment.as_deref());
        tag.remove_key(ItemKey::Bpm);
        tag.remove_key(ItemKey::IntegerBpm);
        if let (Some(value), Some(key)) =
            (values.bpm, bpm_key(writer.file_type().primary_tag_type()))
        {
            tag.insert_text(key, value.to_string());
        }
        set_text(
            tag,
            ItemKey::MusicBrainzRecordingId,
            values.musicbrainz_recording_id.as_deref(),
        );
        set_text(
            tag,
            ItemKey::MusicBrainzTrackId,
            values.musicbrainz_release_track_id.as_deref(),
        );
        set_text(
            tag,
            ItemKey::MusicBrainzReleaseId,
            values.musicbrainz_album_id.as_deref(),
        );
        set_text(
            tag,
            ItemKey::MusicBrainzReleaseGroupId,
            values.musicbrainz_release_group_id.as_deref(),
        );
        set_text(
            tag,
            ItemKey::MusicBrainzArtistId,
            values.musicbrainz_artist_id.as_deref(),
        );
    })?;
    commit_batch(vec![prepared])
}

pub(crate) fn write_album_batch(
    targets: &[(PathBuf, Option<String>)],
    expected_revision: &str,
    values: &AlbumMetadataValues,
) -> Result<(), SourceMetadataError> {
    let paths = targets
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    if combined_revision(&paths)? != expected_revision {
        return Err(SourceMetadataError::Conflict);
    }
    let mut prepared = Vec::with_capacity(targets.len());
    for (path, format) in targets {
        prepared.push(prepare_file(path, format.as_deref(), |tag, _| {
            set_text(tag, ItemKey::AlbumTitle, Some(&values.title));
            set_text(
                tag,
                ItemKey::AlbumTitleSortOrder,
                values.sort_title.as_deref(),
            );
            set_text(tag, ItemKey::AlbumArtist, values.album_artist.as_deref());
            set_number(tag, ItemKey::RecordingDate, values.year);
            set_text(tag, ItemKey::Genre, values.genre.as_deref());
            set_text(tag, ItemKey::Comment, values.comment.as_deref());
            set_text(
                tag,
                ItemKey::MusicBrainzReleaseId,
                values.musicbrainz_album_id.as_deref(),
            );
            set_text(
                tag,
                ItemKey::MusicBrainzReleaseGroupId,
                values.musicbrainz_release_group_id.as_deref(),
            );
        })?);
    }
    commit_batch(prepared)
}

pub(crate) fn write_artist_batch(
    targets: &[(PathBuf, Option<String>)],
    expected_revision: &str,
    previous_name: &str,
    values: &ArtistMetadataValues,
) -> Result<(), SourceMetadataError> {
    let paths = targets
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    if combined_revision(&paths)? != expected_revision {
        return Err(SourceMetadataError::Conflict);
    }
    let mut prepared = Vec::with_capacity(targets.len());
    for (path, format) in targets {
        prepared.push(prepare_file(path, format.as_deref(), |tag, _| {
            let artists = tag
                .artist()
                .map(|value| replace_name(&value, previous_name, &values.name))
                .unwrap_or_else(|| values.name.clone());
            tag.set_artist(artists);
            set_text(
                tag,
                ItemKey::TrackArtistSortOrder,
                values.sort_name.as_deref(),
            );
            set_text(tag, ItemKey::Genre, values.genre.as_deref());
            set_text(tag, ItemKey::Comment, values.comment.as_deref());
            set_text(
                tag,
                ItemKey::MusicBrainzArtistId,
                values.musicbrainz_artist_id.as_deref(),
            );
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
    let mut tag = writable_tag(&tagged);
    mutate(&mut tag, writer);
    save_tag(&tag, temp.path())?;
    temp.as_file().sync_all().map_err(write_error)?;
    Ok(PreparedFile {
        target: path.to_path_buf(),
        temp: temp.into_temp_path(),
    })
}

fn commit_batch(prepared: Vec<PreparedFile>) -> Result<(), SourceMetadataError> {
    let mut committed = 0;
    for file in &prepared {
        if let Err(error) = replace(file.temp.as_ref(), &file.target) {
            for rollback in prepared[..committed].iter().rev() {
                let _ = replace(rollback.temp.as_ref(), &rollback.target);
            }
            return Err(error);
        }
        committed += 1;
    }
    for file in &prepared {
        if let Some(parent) = file.target.parent() {
            sync_parent(parent)?;
        }
    }
    Ok(())
}

fn writable_tag(tagged: &lofty::file::TaggedFile) -> Tag {
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
fn replace(replacement: &Path, target: &Path) -> Result<(), SourceMetadataError> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        replacement,
        rustix::fs::CWD,
        target,
        rustix::fs::RenameFlags::EXCHANGE,
    )
    .map_err(write_error)
}

#[cfg(target_os = "windows")]
fn replace(replacement: &Path, target: &Path) -> Result<(), SourceMetadataError> {
    let replacement = replacement
        .to_str()
        .ok_or(SourceMetadataError::Unavailable)?;
    let target = target.to_str().ok_or(SourceMetadataError::Unavailable)?;
    winsafe::ReplaceFile(
        target,
        replacement,
        None,
        winsafe::co::REPLACEFILE::default(),
    )
    .map_err(write_error)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn replace(_replacement: &Path, _target: &Path) -> Result<(), SourceMetadataError> {
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

    #[test]
    fn available_local_track_metadata_reads_and_writes_the_same_file() {
        let directory = tempfile::tempdir().expect("metadata directory");
        let path = directory.path().join("track.wav");
        fs::write(&path, silent_wav()).expect("write WAV");
        assert!(super::super::metadata_file_available(&path, Some("wav")));
        let before = revision(&path).expect("file revision");
        let values = TrackMetadataValues {
            title: "Updated title".to_string(),
            artist: Some("Updated artist".to_string()),
            album: Some("Updated album".to_string()),
            track_number: Some(3),
            ..TrackMetadataValues::default()
        };

        write_track(&path, Some("wav"), &before, &values).expect("write metadata");
        let read =
            super::super::read_track_metadata(library::TrackKey::from_raw(1), &path, Some("wav"))
                .expect("read metadata");
        assert!(read.writable.title);
        assert_eq!(read.values.title, "Updated title");
        assert_eq!(read.values.artist.as_deref(), Some("Updated artist"));
        assert_eq!(read.values.track_number, Some(3));
    }

    #[test]
    fn local_album_metadata_updates_each_backing_track_as_one_batch() {
        let directory = tempfile::tempdir().expect("metadata directory");
        let paths = [
            directory.path().join("one.wav"),
            directory.path().join("two.wav"),
        ];
        for path in &paths {
            fs::write(path, silent_wav()).expect("write WAV");
            let before = revision(path).expect("file revision");
            write_track(
                path,
                Some("wav"),
                &before,
                &TrackMetadataValues {
                    title: path.file_stem().unwrap().to_string_lossy().into_owned(),
                    album: Some("Before".to_string()),
                    ..TrackMetadataValues::default()
                },
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
            &AlbumMetadataValues {
                title: "After".to_string(),
                album_artist: Some("Album Artist".to_string()),
                ..AlbumMetadataValues::default()
            },
        )
        .expect("write Album metadata");

        for path in &paths {
            let values = super::super::read_album_metadata_values(path, Some("wav"))
                .expect("read Album metadata");
            assert_eq!(values.title, "After");
            assert_eq!(values.album_artist.as_deref(), Some("Album Artist"));
        }
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
