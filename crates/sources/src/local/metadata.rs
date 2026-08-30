//! Atomic metadata reads and writes for exact Local or mapped-Local files.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use lofty::config::WriteOptions;
use lofty::file::TaggedFileExt;
use lofty::prelude::{Accessor, TagExt};
use lofty::tag::items::popularimeter::{Popularimeter, StarRating};
use lofty::tag::{ItemKey, Tag};

use super::lofty_metadata::{MetadataWriter, bpm_key, read_lofty, read_lofty_for_edit};
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

pub(super) fn read_embedded_lyrics(path: &Path) -> Result<Option<String>, SourceMetadataError> {
    let tagged = read_lofty(path, false)
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

pub(super) fn write_embedded_lyrics(path: &Path, lyrics: &str) -> Result<(), SourceMetadataError> {
    let writer = MetadataWriter::for_path(path).ok_or(SourceMetadataError::Unavailable)?;
    if matches!(
        writer.file_type(),
        lofty::file::FileType::Wav | lofty::file::FileType::Aiff
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
    tag_type: Option<lofty::tag::TagType>,
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
    use crate::{
        AlbumMetadataValues, AlbumMetadataWritable, TrackMetadataValues, TrackMetadataWritable,
    };

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
        assert!(super::super::metadata_file_available(&path, Some("wav")));
        let before = revision(&path).expect("file revision");
        let values = TrackMetadataValues {
            title: "Updated title".to_string(),
            artist: Some("Updated artist".to_string()),
            album: Some("Updated album".to_string()),
            track_number: Some(3),
            ..TrackMetadataValues::default()
        };

        write_track(&path, Some("wav"), &before, &track_edit(values)).expect("write metadata");
        let read =
            super::super::read_track_metadata(library::TrackKey::from_raw(1), &path, Some("wav"))
                .expect("read metadata");
        assert!(read.writable.title);
        assert_eq!(read.values.title, "Updated title");
        assert_eq!(read.values.artist.as_deref(), Some("Updated artist"));
        assert_eq!(read.values.track_number, Some(3));
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
        assert!(!super::super::embedded_lyrics_writable(&path));
        assert_eq!(
            super::super::read_track_metadata(library::TrackKey::from_raw(1), &path, Some("wav"))
                .expect("read metadata")
                .values
                .title,
            "Track title"
        );

        let mut id3 = Tag::new(lofty::tag::TagType::Id3v2);
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
            let values = super::super::read_album_metadata_values(path, Some("wav"))
                .expect("read Album metadata");
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
            .map(|path| prepare_file(path, Some("wav"), None, |_, _| {}))
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
