//! Lofty content probing, metadata reading, and exact writer capability.

use std::fs;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use lofty::config::{GlobalOptions, ParseOptions, apply_global_options};
use lofty::file::{FileType, TaggedFile};
use lofty::probe::Probe;
use lofty::tag::{ItemKey, TagType};

const LOFTY_ALLOCATION_MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MetadataWriter {
    file_type: FileType,
}

impl MetadataWriter {
    pub(crate) fn for_path(path: &Path) -> Option<Self> {
        Self::for_file_type(probe_file_type(path)?)
    }

    pub(crate) fn for_source_format(source_format: &str) -> Option<Self> {
        Self::for_file_type(FileType::from_ext(source_format)?)
    }

    pub(crate) fn for_file_type(file_type: FileType) -> Option<Self> {
        file_type
            .tag_support(file_type.primary_tag_type())
            .is_writable()
            .then_some(Self { file_type })
    }

    pub(crate) const fn file_type(self) -> FileType {
        self.file_type
    }

    pub(crate) fn metadata_key_is_writable(self, key: ItemKey) -> bool {
        metadata_key_is_writable(self.file_type.primary_tag_type(), key)
    }

    pub(crate) fn lyrics_target(self) -> Option<(TagType, ItemKey)> {
        let primary = self.file_type.primary_tag_type();
        for key in [ItemKey::UnsyncLyrics, ItemKey::Lyrics] {
            if metadata_key_is_writable(primary, key) {
                return Some((primary, key));
            }
        }
        if self.file_type.tag_support(TagType::Id3v2).is_writable() {
            for key in [ItemKey::UnsyncLyrics, ItemKey::Lyrics] {
                if metadata_key_is_writable(TagType::Id3v2, key) {
                    return Some((TagType::Id3v2, key));
                }
            }
        }
        None
    }
}

fn metadata_key_is_writable(tag_type: lofty::tag::TagType, key: ItemKey) -> bool {
    key.map_key(tag_type).is_some()
        || tag_type == lofty::tag::TagType::Id3v2
            && matches!(
                key,
                ItemKey::MusicBrainzRecordingId
                    | ItemKey::Producer
                    | ItemKey::Arranger
                    | ItemKey::Engineer
                    | ItemKey::MixDj
                    | ItemKey::MixEngineer
            )
}

/// Grouping's item: ID3v2 keeps it in GRP1, where iTunes moved it when TIT1 became the work,
/// as Navidrome reads it.
pub(crate) fn grouping_key(tag_type: lofty::tag::TagType) -> ItemKey {
    if tag_type == lofty::tag::TagType::Id3v2 {
        ItemKey::AppleId3v2ContentGroup
    } else {
        ItemKey::ContentGroup
    }
}

pub(crate) fn bpm_key(tag_type: lofty::tag::TagType) -> Option<ItemKey> {
    // Lofty's generic MP4 conversion writes `tmpo` as text and can preserve an
    // existing integer atom beside it, so that path is not a safe BPM writer.
    if tag_type == lofty::tag::TagType::Mp4Ilst {
        return None;
    }
    [ItemKey::IntegerBpm, ItemKey::Bpm]
        .into_iter()
        .find(|key| metadata_key_is_writable(tag_type, *key))
}

pub(crate) fn probe_file_type(path: &Path) -> Option<FileType> {
    Probe::new(BufReader::new(fs::File::open(path).ok()?))
        .guess_file_type()
        .ok()?
        .file_type()
}

pub(crate) fn source_format(path: &Path, file_type: FileType) -> Option<String> {
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

pub(crate) fn read_lofty(
    path: &Path,
    read_cover_art: bool,
) -> Result<Option<TaggedFile>, lofty::error::FileParseError> {
    read_lofty_file(
        fs::File::open(path)?,
        ParseOptions::new().read_cover_art(read_cover_art),
    )
}

pub(crate) fn read_lofty_file(
    file: impl Read + Seek,
    options: ParseOptions,
) -> Result<Option<TaggedFile>, lofty::error::FileParseError> {
    let probe = probe(file, options)?;
    let Some(_) = probe.file_type() else {
        return Ok(None);
    };
    probe.read().map(Some)
}

/// The tag keys of an album's version, as Navidrome reads them.
const ALBUM_VERSION_KEYS: [&str; 3] = [
    "ALBUMVERSION",
    "MUSICBRAINZ_ALBUMCOMMENT",
    "MusicBrainz Album Comment",
];

/// Reads a file for its scan, with its album version: Lofty's generic tag drops keys it
/// does not map, so the version comes from the format's own tag.
pub(crate) fn read_lofty_scan(
    file: impl Read + Seek,
    options: ParseOptions,
) -> Result<Option<(TaggedFile, Option<String>)>, lofty::error::FileParseError> {
    use lofty::ape::{ApeFile, ApeTag};
    use lofty::file::AudioFile;
    use lofty::flac::FlacFile;
    use lofty::id3::v2::{Frame, Id3v2Tag};
    use lofty::iff::{aiff::AiffFile, wav::WavFile};
    use lofty::mp4::{AtomData, AtomIdent, Ilst, Mp4File};
    use lofty::mpeg::MpegFile;
    use lofty::musepack::MpcFile;
    use lofty::ogg::tag::VorbisComments;
    use lofty::ogg::{OpusFile, SpeexFile, VorbisFile};
    use lofty::wavpack::WavPackFile;

    fn version_key(key: &str) -> bool {
        ALBUM_VERSION_KEYS
            .iter()
            .any(|version| version.eq_ignore_ascii_case(key))
    }
    fn vorbis(tag: &VorbisComments) -> Option<String> {
        ALBUM_VERSION_KEYS
            .iter()
            .find_map(|key| tag.get(key))
            .map(str::to_owned)
    }
    fn id3(tag: Option<&Id3v2Tag>) -> Option<String> {
        tag?.into_iter().find_map(|frame| match frame {
            Frame::UserText(text) if version_key(&text.description) => {
                Some(text.content.to_string())
            }
            _ => None,
        })
    }
    fn ape(tag: Option<&ApeTag>) -> Option<String> {
        let tag = tag?;
        ALBUM_VERSION_KEYS
            .iter()
            .find_map(|key| tag.get(key)?.value().text())
            .map(str::to_owned)
    }
    fn ilst(tag: Option<&Ilst>) -> Option<String> {
        tag?.into_iter().find_map(|atom| match atom.ident() {
            AtomIdent::Freeform { name, .. } if version_key(name) => {
                atom.data().find_map(|data| match data {
                    AtomData::UTF8(text) => Some(text.clone()),
                    _ => None,
                })
            }
            _ => None,
        })
    }

    fn read<F: AudioFile + Into<TaggedFile>>(
        reader: &mut (impl Read + Seek),
        options: ParseOptions,
        version: impl FnOnce(&F) -> Option<String>,
    ) -> Result<(TaggedFile, Option<String>), lofty::error::FileParseError> {
        let file = F::read_from(reader, options)?;
        let version = version(&file);
        Ok((file.into(), version))
    }

    let probe = probe(file, options)?;
    let Some(file_type) = probe.file_type() else {
        return Ok(None);
    };
    let reader = &mut probe.into_inner();
    match file_type {
        FileType::Flac => read(reader, options, |file: &FlacFile| {
            (file.vorbis_comments().and_then(vorbis)).or_else(|| id3(file.id3v2()))
        }),
        FileType::Vorbis => read(reader, options, |file: &VorbisFile| {
            vorbis(file.vorbis_comments())
        }),
        FileType::Opus => read(reader, options, |file: &OpusFile| {
            vorbis(file.vorbis_comments())
        }),
        FileType::Speex => read(reader, options, |file: &SpeexFile| {
            vorbis(file.vorbis_comments())
        }),
        FileType::Mpeg => read(reader, options, |file: &MpegFile| {
            id3(file.id3v2()).or_else(|| ape(file.ape()))
        }),
        FileType::Mp4 => read(reader, options, |file: &Mp4File| ilst(file.ilst())),
        FileType::Aiff => read(reader, options, |file: &AiffFile| id3(file.id3v2())),
        FileType::Wav => read(reader, options, |file: &WavFile| id3(file.id3v2())),
        FileType::Ape => read(reader, options, |file: &ApeFile| {
            ape(file.ape()).or_else(|| id3(file.id3v2()))
        }),
        FileType::Mpc => read(reader, options, |file: &MpcFile| {
            ape(file.ape()).or_else(|| id3(file.id3v2()))
        }),
        FileType::WavPack => read(reader, options, |file: &WavPackFile| ape(file.ape())),
        _ => Probe::with_file_type(reader, file_type)
            .options(options)
            .read()
            .map(|file| (file, None)),
    }
    .map(Some)
}

fn probe<R: Read + Seek>(
    file: R,
    options: ParseOptions,
) -> Result<Probe<BufReader<R>>, lofty::error::FileParseError> {
    apply_global_options(
        GlobalOptions::new()
            .allocation_limit(LOFTY_ALLOCATION_MAX_BYTES)
            .preserve_format_specific_items(false),
    );
    Probe::new(BufReader::new(file))
        .options(options)
        .guess_file_type()
        .map_err(Into::into)
}

pub(crate) fn read_lofty_for_edit(
    path: &Path,
    file_type: FileType,
) -> Result<Option<TaggedFile>, lofty::error::FileParseError> {
    apply_global_options(
        GlobalOptions::new()
            .allocation_limit(LOFTY_ALLOCATION_MAX_BYTES)
            .preserve_format_specific_items(true),
    );
    let probe = Probe::new(BufReader::new(fs::File::open(path)?))
        .options(
            ParseOptions::new()
                .read_cover_art(true)
                .read_properties(false),
        )
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
