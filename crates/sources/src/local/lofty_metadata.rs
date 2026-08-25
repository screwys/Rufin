//! Lofty content probing, metadata reading, and exact writer capability.

use std::fs;
use std::io::BufReader;
use std::path::Path;

use lofty::config::{GlobalOptions, ParseOptions, apply_global_options};
use lofty::file::{FileType, TaggedFile};
use lofty::probe::Probe;
use lofty::tag::ItemKey;

const LOFTY_ALLOCATION_MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MetadataWriter {
    file_type: FileType,
}

impl MetadataWriter {
    pub(super) fn for_path(path: &Path) -> Option<Self> {
        Self::for_file_type(probe_file_type(path)?)
    }

    pub(super) fn for_source_format(source_format: &str) -> Option<Self> {
        Self::for_file_type(FileType::from_ext(source_format)?)
    }

    fn for_file_type(file_type: FileType) -> Option<Self> {
        file_type
            .tag_support(file_type.primary_tag_type())
            .is_writable()
            .then_some(Self { file_type })
    }

    pub(super) const fn file_type(self) -> FileType {
        self.file_type
    }

    pub(super) fn metadata_key_is_writable(self, key: ItemKey) -> bool {
        metadata_key_is_writable(self.file_type.primary_tag_type(), key)
    }
}

fn metadata_key_is_writable(tag_type: lofty::tag::TagType, key: ItemKey) -> bool {
    key.map_key(tag_type).is_some()
        || tag_type == lofty::tag::TagType::Id3v2 && key == ItemKey::MusicBrainzRecordingId
}

pub(super) fn bpm_key(tag_type: lofty::tag::TagType) -> Option<ItemKey> {
    // Lofty's generic MP4 conversion writes `tmpo` as text and can preserve an
    // existing integer atom beside it, so that path is not a safe BPM writer.
    if tag_type == lofty::tag::TagType::Mp4Ilst {
        return None;
    }
    [ItemKey::IntegerBpm, ItemKey::Bpm]
        .into_iter()
        .find(|key| metadata_key_is_writable(tag_type, *key))
}

pub(super) fn probe_file_type(path: &Path) -> Option<FileType> {
    Probe::new(BufReader::new(fs::File::open(path).ok()?))
        .guess_file_type()
        .ok()?
        .file_type()
}

pub(super) fn source_format(path: &Path, file_type: FileType) -> Option<String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::trim)
        .filter(|extension| !extension.is_empty());
    if extension.is_some_and(|extension| FileType::from_ext(extension) == Some(file_type)) {
        return extension.map(ToString::to_string);
    }
    canonical_extension(file_type).map(ToString::to_string)
}

fn canonical_extension(file_type: FileType) -> Option<&'static str> {
    match file_type {
        FileType::Aac => Some("aac"),
        FileType::Aiff => Some("aiff"),
        FileType::Ape => Some("ape"),
        FileType::Flac => Some("flac"),
        FileType::Mpeg => Some("mp3"),
        FileType::Mp4 => Some("mp4"),
        FileType::Mpc => Some("mpc"),
        FileType::Opus => Some("opus"),
        FileType::Vorbis => Some("ogg"),
        FileType::Speex => Some("spx"),
        FileType::Wav => Some("wav"),
        FileType::WavPack => Some("wv"),
        FileType::Custom(_) => None,
        _ => None,
    }
}

pub(super) fn read_lofty(
    path: &Path,
    read_cover_art: bool,
) -> Result<Option<TaggedFile>, lofty::error::FileParseError> {
    read_lofty_file(fs::File::open(path)?, read_cover_art)
}

pub(super) fn read_lofty_file(
    file: fs::File,
    read_cover_art: bool,
) -> Result<Option<TaggedFile>, lofty::error::FileParseError> {
    apply_global_options(
        GlobalOptions::new()
            .allocation_limit(LOFTY_ALLOCATION_MAX_BYTES)
            .preserve_format_specific_items(false),
    );
    let options = ParseOptions::new().read_cover_art(read_cover_art);
    let probe = Probe::new(BufReader::new(file))
        .options(options)
        .guess_file_type()?;
    let Some(_) = probe.file_type() else {
        return Ok(None);
    };
    probe.read().map(Some)
}

pub(super) fn read_lofty_for_edit(
    path: &Path,
    file_type: FileType,
) -> Result<Option<TaggedFile>, lofty::error::FileParseError> {
    apply_global_options(
        GlobalOptions::new()
            .allocation_limit(LOFTY_ALLOCATION_MAX_BYTES)
            .preserve_format_specific_items(true),
    );
    let probe = Probe::new(BufReader::new(fs::File::open(path)?))
        .options(ParseOptions::new().read_cover_art(true))
        .guess_file_type()?;
    let Some(actual_file_type) = probe.file_type() else {
        return Ok(None);
    };
    if actual_file_type != file_type {
        return Ok(None);
    }
    probe.read().map(Some)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn lofty_reader_and_writer_follow_content_instead_of_the_suffix() {
        let directory = tempfile::tempdir().expect("audio fixture directory");
        let path = directory.path().join("mislabeled.bin");
        fs::write(&path, silent_wav()).expect("write WAV fixture");

        assert_eq!(probe_file_type(&path), Some(FileType::Wav));
        assert_eq!(source_format(&path, FileType::Wav).as_deref(), Some("wav"));
        assert!(
            read_lofty(&path, false)
                .expect("read WAV content")
                .is_some()
        );
        let writer = MetadataWriter::for_path(&path).expect("WAV metadata writer");
        assert_eq!(writer.file_type(), FileType::Wav);
        assert!(writer.metadata_key_is_writable(ItemKey::TrackTitle));
    }

    #[test]
    fn mp4_bpm_remains_read_only() {
        let writer = MetadataWriter {
            file_type: FileType::Mp4,
        };
        assert_eq!(bpm_key(writer.file_type().primary_tag_type()), None);
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
