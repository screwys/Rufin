//! Lofty content probing, metadata reading, and exact writer capability.

use std::fs;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use lofty::config::{GlobalOptions, ParseOptions, apply_global_options};
use lofty::file::{FileType, TaggedFile};
use lofty::probe::Probe;
use lofty::tag::{ItemKey, TagType};

use super::tags::{RawTags, push};

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
/// Reads a file for its scan, with the values of its own tag under that format's keys:
/// Lofty's generic tag keeps only the keys it maps. The tag is the format's own, or else the
/// fullest other one the file carries.
pub(crate) fn read_lofty_scan(
    file: impl Read + Seek,
    options: ParseOptions,
) -> Result<Option<(TaggedFile, RawTags)>, lofty::error::FileParseError> {
    use lofty::aac::AacFile;
    use lofty::ape::{ApeFile, ApeTag};
    use lofty::file::AudioFile;
    use lofty::flac::FlacFile;
    use lofty::id3::v1::Id3v1Tag;
    use lofty::id3::v2::{Frame, Id3v2Tag};
    use lofty::iff::{aiff::AiffFile, wav::WavFile};
    use lofty::mp4::{AtomData, AtomIdent, Ilst, Mp4File};
    use lofty::mpeg::MpegFile;
    use lofty::musepack::MpcFile;
    use lofty::ogg::tag::VorbisComments;
    use lofty::ogg::{OpusFile, SpeexFile, VorbisFile};
    use lofty::wavpack::WavPackFile;

    fn vorbis(tag: &VorbisComments, raw: &mut RawTags) {
        for (key, value) in tag.items() {
            push(raw, key, value);
        }
    }
    /// A TCON value as a genre name: `17` and `(17)` name a genre of ID3v1's list, and
    /// `(17)Rock` refines one.
    fn id3_genre(value: &str) -> String {
        let listed = |number: &str| {
            let index = number.trim().parse::<usize>().ok()?;
            lofty::id3::v1::GENRES.get(index).map(ToString::to_string)
        };
        match value
            .strip_prefix('(')
            .and_then(|rest| rest.split_once(')'))
        {
            Some((_, refinement)) if !refinement.trim().is_empty() => {
                Some(refinement.trim().to_owned())
            }
            Some((number, _)) => listed(number),
            None => listed(value),
        }
        .unwrap_or_else(|| value.to_owned())
    }
    fn id3v2(tag: &Id3v2Tag, raw: &mut RawTags) {
        for frame in tag {
            let id = frame.id_str();
            match frame {
                Frame::Text(text) => {
                    for value in text.value.split('\0') {
                        if id == "TCON" {
                            push(raw, id, &id3_genre(value));
                        } else {
                            push(raw, id, value);
                        }
                    }
                }
                Frame::UserText(text) if !text.description.is_empty() => {
                    for value in text.content.split('\0') {
                        push(raw, &text.description, value);
                        push(raw, &format!("txxx:{}", text.description), value);
                    }
                }
                Frame::Comment(comment) if comment.description.is_empty() => {
                    push(raw, "comment", &comment.content);
                }
                Frame::UniqueFileIdentifier(file) => {
                    if let Ok(identifier) = std::str::from_utf8(&file.identifier) {
                        push(raw, &format!("ufid:{}", file.owner), identifier);
                    }
                }
                Frame::KeyValue(people) => {
                    for (role, name) in people.key_value_pairs.iter() {
                        push(raw, &format!("{id}:{role}"), name);
                    }
                }
                Frame::Timestamp(time) => push(raw, id, &time.timestamp.to_string()),
                Frame::Url(link) => push(raw, id, link.url()),
                _ => {}
            }
        }
    }
    fn id3v1(tag: &Id3v1Tag, raw: &mut RawTags) {
        for (key, value) in [
            ("title", &tag.title),
            ("artist", &tag.artist),
            ("album", &tag.album),
            ("comment", &tag.comment),
        ] {
            if let Some(value) = value {
                push(raw, key, value);
            }
        }
        if let Some(year) = tag.year {
            push(raw, "date", &year.to_string());
        }
        if let Some(number) = tag.track_number {
            push(raw, "tracknumber", &number.to_string());
        }
        if let Some(genre) = tag
            .genre
            .and_then(|genre| lofty::id3::v1::GENRES.get(usize::from(genre)))
        {
            push(raw, "genre", genre);
        }
    }
    fn ape(tag: &ApeTag, raw: &mut RawTags) {
        for item in tag {
            for value in item.text_values().into_iter().flatten() {
                push(raw, item.key(), value);
            }
        }
    }
    fn ilst(tag: &Ilst, raw: &mut RawTags) {
        for atom in tag {
            let key = match atom.ident() {
                AtomIdent::Fourcc(fourcc) => fourcc.iter().map(|byte| char::from(*byte)).collect(),
                AtomIdent::Freeform { mean, name } => format!("----:{mean}:{name}"),
            };
            for data in atom.data() {
                match data {
                    AtomData::UTF8(text) | AtomData::UTF16(text) => push(raw, &key, text),
                    AtomData::SignedInteger(number) => push(raw, &key, &number.to_string()),
                    AtomData::UnsignedInteger(number) => push(raw, &key, &number.to_string()),
                    AtomData::Bool(flag) => push(raw, &key, if *flag { "1" } else { "0" }),
                    // A track or disc number and its total, two bytes each after two of padding.
                    AtomData::Unknown { data, .. }
                        if matches!(key.as_str(), "trkn" | "disk") && data.len() >= 6 =>
                    {
                        let number = u16::from_be_bytes([data[2], data[3]]);
                        let total = u16::from_be_bytes([data[4], data[5]]);
                        push(raw, &key, &format!("{number}/{total}"));
                    }
                    _ => {}
                }
            }
        }
    }

    fn read<F: AudioFile + Into<TaggedFile>>(
        reader: &mut (impl Read + Seek),
        options: ParseOptions,
        tags: impl FnOnce(&F, &mut RawTags),
    ) -> Result<(TaggedFile, RawTags), lofty::error::FileParseError> {
        let file = F::read_from(reader, options)?;
        let mut raw = RawTags::new();
        tags(&file, &mut raw);
        Ok((file.into(), raw))
    }

    let probe = probe(file, options)?;
    let Some(file_type) = probe.file_type() else {
        return Ok(None);
    };
    let reader = &mut probe.into_inner();
    match file_type {
        FileType::Flac => read(reader, options, |file: &FlacFile, raw| {
            if let Some(tag) = file.vorbis_comments() {
                vorbis(tag, raw);
            } else if let Some(tag) = file.id3v2() {
                id3v2(tag, raw);
            }
        }),
        FileType::Vorbis => read(reader, options, |file: &VorbisFile, raw| {
            vorbis(file.vorbis_comments(), raw);
        }),
        FileType::Opus => read(reader, options, |file: &OpusFile, raw| {
            vorbis(file.vorbis_comments(), raw);
        }),
        FileType::Speex => read(reader, options, |file: &SpeexFile, raw| {
            vorbis(file.vorbis_comments(), raw);
        }),
        FileType::Mpeg => read(reader, options, |file: &MpegFile, raw| {
            if let Some(tag) = file.id3v2() {
                id3v2(tag, raw);
            } else if let Some(tag) = file.ape() {
                ape(tag, raw);
            } else if let Some(tag) = file.id3v1() {
                id3v1(tag, raw);
            }
        }),
        FileType::Aac => read(reader, options, |file: &AacFile, raw| {
            if let Some(tag) = file.id3v2() {
                id3v2(tag, raw);
            } else if let Some(tag) = file.id3v1() {
                id3v1(tag, raw);
            }
        }),
        FileType::Mp4 => read(reader, options, |file: &Mp4File, raw| {
            if let Some(tag) = file.ilst() {
                ilst(tag, raw);
            }
        }),
        FileType::Aiff => read(reader, options, |file: &AiffFile, raw| {
            if let Some(tag) = file.id3v2() {
                id3v2(tag, raw);
            } else if let Some(chunks) = file.text_chunks() {
                for (key, value) in [
                    ("title", &chunks.name),
                    ("artist", &chunks.author),
                    ("copyright", &chunks.copyright),
                ] {
                    if let Some(value) = value {
                        push(raw, key, value);
                    }
                }
                for text in chunks.annotations.iter().flatten() {
                    push(raw, "comment", text);
                }
                for comment in chunks.comments.iter().flatten() {
                    push(raw, "comment", &comment.text);
                }
            }
        }),
        FileType::Wav => read(reader, options, |file: &WavFile, raw| {
            if let Some(tag) = file.id3v2() {
                id3v2(tag, raw);
            } else if let Some(tag) = file.riff_info() {
                for (key, value) in tag {
                    push(raw, key, value);
                }
            }
        }),
        FileType::Ape => read(reader, options, |file: &ApeFile, raw| {
            if let Some(tag) = file.ape() {
                ape(tag, raw);
            } else if let Some(tag) = file.id3v2() {
                id3v2(tag, raw);
            } else if let Some(tag) = file.id3v1() {
                id3v1(tag, raw);
            }
        }),
        FileType::Mpc => read(reader, options, |file: &MpcFile, raw| {
            if let Some(tag) = file.ape() {
                ape(tag, raw);
            } else if let Some(tag) = file.id3v2() {
                id3v2(tag, raw);
            } else if let Some(tag) = file.id3v1() {
                id3v1(tag, raw);
            }
        }),
        FileType::WavPack => read(reader, options, |file: &WavPackFile, raw| {
            if let Some(tag) = file.ape() {
                ape(tag, raw);
            } else if let Some(tag) = file.id3v1() {
                id3v1(tag, raw);
            }
        }),
        _ => Probe::with_file_type(reader, file_type)
            .options(options)
            .read()
            .map(|file| (file, RawTags::new())),
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
