//! Media discovery, admission, generic tags, embedded artwork, and bounded container preflight.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::OnceLock;

use gstreamer_pbutils::gst;
use gstreamer_pbutils::prelude::*;
use gstreamer_pbutils::{Discoverer, DiscovererInfo, DiscovererResult};

use crate::{ImageBytes, SourceError, SourceResult};

const DISCOVERER_TIMEOUT_SECONDS: u64 = 1;
const MAX_ATTACHMENT_COUNT: usize = 256;
const MAX_ATTACHMENT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_EBML_HEADERS: usize = 65_536;
const MAX_EBML_DEPTH: usize = 8;
const MAX_ASF_HEADER_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ASF_HEADER_OBJECTS: u32 = 4_096;

const ID_SEGMENT: u64 = 0x1853_8067;
const ID_SEEK_HEAD: u64 = 0x114D_9B74;
const ID_INFO: u64 = 0x1549_A966;
const ID_CLUSTER: u64 = 0x1F43_B675;
const ID_TRACKS: u64 = 0x1654_AE6B;
const ID_CUES: u64 = 0x1C53_BB6B;
const ID_ATTACHMENTS: u64 = 0x1941_A469;
const ID_CHAPTERS: u64 = 0x1043_A770;
const ID_TAGS: u64 = 0x1254_C367;
const ID_ATTACHED_FILE: u64 = 0x61A7;
const ID_FILE_DATA: u64 = 0x465C;
const ASF_HEADER_GUID: [u8; 16] = [
    0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa, 0x00, 0x62, 0xce, 0x6c,
];
const ASF_FILE_PROPERTIES_GUID: [u8; 16] = [
    0xa1, 0xdc, 0xab, 0x8c, 0x47, 0xa9, 0xcf, 0x11, 0x8e, 0xe4, 0x00, 0xc0, 0x0c, 0x20, 0x53, 0x65,
];
const ASF_STREAM_PROPERTIES_GUID: [u8; 16] = [
    0x91, 0x07, 0xdc, 0xb7, 0xb7, 0xa9, 0xcf, 0x11, 0x8e, 0xe6, 0x00, 0xc0, 0x0c, 0x20, 0x53, 0x65,
];
const ASF_AUDIO_MEDIA_GUID: [u8; 16] = [
    0x40, 0x9e, 0x69, 0xf8, 0x4d, 0x5b, 0xcf, 0x11, 0xa8, 0xfd, 0x00, 0x80, 0x5f, 0x5c, 0x44, 0x2b,
];
const WAVE_FORMAT_WMA_PRO: u16 = 0x0162;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Metadata {
    pub(crate) title: Option<String>,
    pub(crate) album: Option<String>,
    pub(crate) artist: Option<String>,
    pub(crate) album_artist: Option<String>,
    pub(crate) artist_mbids: Vec<String>,
    pub(crate) album_artist_mbids: Vec<String>,
    pub(crate) genres: Vec<String>,
    pub(crate) moods: Vec<String>,
    pub(crate) year: Option<u16>,
    pub(crate) comment: Option<String>,
    pub(crate) bpm: Option<u16>,
    pub(crate) track_number: Option<u16>,
    pub(crate) disc_number: Option<u16>,
    pub(crate) musicbrainz_album_id: Option<String>,
    pub(crate) musicbrainz_release_group_id: Option<String>,
    pub(crate) musicbrainz_recording_id: Option<String>,
    pub(crate) musicbrainz_release_track_id: Option<String>,
    pub(crate) release_types: Vec<String>,
    pub(crate) is_compilation: Option<bool>,
    pub(crate) duration_seconds: u32,
    pub(crate) artwork_index: Option<u32>,
    pub(crate) source_format: Option<String>,
}

#[derive(Default)]
pub(crate) struct Reader {
    discoverer: DiscovererState,
    timeout_seconds: Option<u64>,
}

enum DiscovererState {
    New,
    Ready(Discoverer),
    Unavailable,
}

impl Default for DiscovererState {
    fn default() -> Self {
        Self::New
    }
}

impl Reader {
    pub(crate) fn network() -> Self {
        Self {
            discoverer: DiscovererState::New,
            timeout_seconds: Some(30),
        }
    }
    pub(crate) fn read(&mut self, path: &Path) -> Option<Metadata> {
        let info = self.discover(path)?;
        metadata_from_info(&info)
    }

    pub(crate) fn read_input(
        &mut self,
        file: &mut (impl Read + Seek),
        uri: &str,
    ) -> Option<Metadata> {
        let info = self.discover_input(file, uri)?;
        metadata_from_info(&info)
    }

    fn discover(&mut self, path: &Path) -> Option<DiscovererInfo> {
        let mut file = fs::File::open(path).ok()?;
        let uri = url::Url::from_file_path(path).ok()?;
        self.discover_input(&mut file, uri.as_str())
    }

    fn discover_input(
        &mut self,
        file: &mut (impl Read + Seek),
        uri: &str,
    ) -> Option<DiscovererInfo> {
        preflight_known_container(file).ok()?;
        let info = self.discoverer()?.discover_uri(uri).ok()?;
        admitted_audio_stream(&info).map(|_| info)
    }

    fn discoverer(&mut self) -> Option<&Discoverer> {
        if matches!(self.discoverer, DiscovererState::New) {
            self.discoverer = ensure_gstreamer_initialized()
                .and_then(|()| {
                    Discoverer::new(gst::ClockTime::from_seconds(
                        self.timeout_seconds.unwrap_or(DISCOVERER_TIMEOUT_SECONDS),
                    ))
                    .map_err(|error| error.to_string())
                })
                .map_or(DiscovererState::Unavailable, DiscovererState::Ready);
        }
        match &self.discoverer {
            DiscovererState::Ready(discoverer) => Some(discoverer),
            DiscovererState::New | DiscovererState::Unavailable => None,
        }
    }
}

pub(crate) fn read_image_input(
    reader: &mut Reader,
    file: &mut (impl Read + Seek),
    uri: &str,
    picture_index: u32,
) -> SourceResult<ImageBytes> {
    let info = reader
        .discover_input(file, uri)
        .ok_or(SourceError::NotFound)?;
    let audio = admitted_audio_stream(&info).ok_or(SourceError::NotFound)?;
    let tags = ScopedTags::new(container_tags(&info), audio.tags());
    let sample = tags
        .images()
        .nth(usize::try_from(picture_index).map_err(|_| SourceError::NotFound)?)
        .ok_or(SourceError::NotFound)?;
    let buffer = sample.buffer().ok_or(SourceError::NotFound)?;
    if buffer.size() > MAX_ATTACHMENT_BYTES as usize {
        return Err(SourceError::Other(format!(
            "Local artwork exceeds {} MiB",
            MAX_ATTACHMENT_BYTES / (1024 * 1024)
        )));
    }
    let mapped = buffer
        .map_readable()
        .map_err(|error| SourceError::Other(error.to_string()))?;
    let content_type = sample
        .caps()
        .and_then(|caps| caps.structure(0))
        .map(|structure| structure.name().to_string());
    Ok(ImageBytes {
        bytes: mapped.as_slice().to_vec(),
        content_type,
    })
}

fn ensure_gstreamer_initialized() -> Result<(), String> {
    static INITIALIZED: OnceLock<Result<(), String>> = OnceLock::new();
    INITIALIZED
        .get_or_init(|| gst::init().map_err(|error| error.to_string()))
        .clone()
}

fn metadata_from_info(info: &DiscovererInfo) -> Option<Metadata> {
    let audio = admitted_audio_stream(info)?;
    let tags = ScopedTags::new(container_tags(info), audio.tags());
    let duration_seconds = info
        .duration()
        .map(gst::ClockTime::seconds)
        .unwrap_or_default()
        .min(u64::from(u32::MAX)) as u32;
    let mut metadata = metadata_from_tags(&tags, duration_seconds, container_is_asf(info));
    metadata.source_format = synthetic_audio_format(&audio);
    Some(metadata)
}

fn synthetic_audio_format(audio: &gstreamer_pbutils::DiscovererAudioInfo) -> Option<String> {
    let caps = audio.caps()?;
    let structure = caps.structure(0)?;
    let name = structure.name();
    if name == "audio/x-mod" {
        return structure
            .get::<String>("type")
            .ok()
            .map(|format| format.to_ascii_lowercase());
    }
    let format = name.as_str().strip_prefix("audio/x-")?;
    ["ay", "gbs", "gym", "hes", "kss", "nsf", "sap", "spc", "vgm"]
        .into_iter()
        .find(|candidate| format == *candidate)
        .map(ToString::to_string)
}

fn container_tags(info: &DiscovererInfo) -> Option<gst::TagList> {
    info.stream_info()?
        .downcast::<gstreamer_pbutils::DiscovererContainerInfo>()
        .ok()?
        .tags()
}

fn container_is_asf(info: &DiscovererInfo) -> bool {
    info.stream_info()
        .and_then(|stream| stream.caps())
        .and_then(|caps| {
            caps.structure(0)
                .map(|structure| structure.name().to_string())
        })
        .is_some_and(|name| name == "video/x-ms-asf")
}

fn admitted_audio_stream(info: &DiscovererInfo) -> Option<gstreamer_pbutils::DiscovererAudioInfo> {
    let mut audio = info.audio_streams();
    let video_is_image = info
        .video_streams()
        .iter()
        .map(video_is_still)
        .collect::<Vec<_>>();
    if !discovery_is_admitted(info.result(), audio.len(), &video_is_image) {
        return None;
    }
    audio.pop()
}

fn video_is_still(video: &gstreamer_pbutils::DiscovererVideoInfo) -> bool {
    let caps_name = video.caps().and_then(|caps| {
        caps.structure(0)
            .map(|structure| structure.name().to_string())
    });
    video_shape_is_still(video.is_image(), caps_name.as_deref())
}

fn video_shape_is_still(is_image: bool, caps_name: Option<&str>) -> bool {
    is_image || caps_name.is_some_and(|name| name.starts_with("image/"))
}

fn discovery_is_admitted(
    result: DiscovererResult,
    audio_streams: usize,
    video_is_image: &[bool],
) -> bool {
    result == DiscovererResult::Ok
        && audio_streams == 1
        && video_is_image.iter().all(|is_image| *is_image)
}

struct ScopedTags {
    container: Option<gst::TagList>,
    audio: Option<gst::TagList>,
    container_extended: HashMap<String, Vec<String>>,
    audio_extended: HashMap<String, Vec<String>>,
}

impl ScopedTags {
    fn new(container: Option<gst::TagList>, audio: Option<gst::TagList>) -> Self {
        let container_extended = container
            .as_ref()
            .map(extended_comments)
            .unwrap_or_default();
        let audio_extended = audio.as_ref().map(extended_comments).unwrap_or_default();
        Self {
            container,
            audio,
            container_extended,
            audio_extended,
        }
    }

    fn strings<T>(&self) -> Vec<String>
    where
        for<'a> T: gst::tags::Tag<'a, TagType = &'a str>,
    {
        self.audio
            .as_ref()
            .map(tag_strings::<T>)
            .filter(|values| !values.is_empty())
            .or_else(|| {
                self.container
                    .as_ref()
                    .map(tag_strings::<T>)
                    .filter(|values| !values.is_empty())
            })
            .unwrap_or_default()
    }

    fn string<T>(&self) -> Option<String>
    where
        for<'a> T: gst::tags::Tag<'a, TagType = &'a str>,
    {
        self.strings::<T>().into_iter().next()
    }

    fn number<T>(&self) -> Option<u16>
    where
        for<'a> T: gst::tags::Tag<'a, TagType = u32>,
    {
        self.audio
            .as_ref()
            .and_then(tag_u32::<T>)
            .or_else(|| self.container.as_ref().and_then(tag_u32::<T>))
            .map(|value| value.min(u32::from(u16::MAX)) as u16)
    }

    fn extended(&self, keys: &[&str]) -> Option<String> {
        keys.iter()
            .find_map(|key| extended_value(&self.audio_extended, key))
            .or_else(|| {
                keys.iter()
                    .find_map(|key| extended_value(&self.container_extended, key))
            })
    }

    fn extended_values(&self, keys: &[&str]) -> Vec<String> {
        keys.iter()
            .find_map(|key| self.audio_extended.get(&normalized_key(key)).cloned())
            .filter(|values| !values.is_empty())
            .or_else(|| {
                keys.iter()
                    .find_map(|key| self.container_extended.get(&normalized_key(key)).cloned())
            })
            .unwrap_or_default()
    }

    fn images(&self) -> impl Iterator<Item = gst::Sample> + '_ {
        self.audio
            .iter()
            .flat_map(|tags| tag_samples::<gst::tags::Image>(tags))
            .chain(
                self.container
                    .iter()
                    .flat_map(|tags| tag_samples::<gst::tags::Image>(tags)),
            )
            .chain(
                self.audio
                    .iter()
                    .flat_map(|tags| tag_samples::<gst::tags::PreviewImage>(tags)),
            )
            .chain(
                self.container
                    .iter()
                    .flat_map(|tags| tag_samples::<gst::tags::PreviewImage>(tags)),
            )
    }
}

fn metadata_from_tags(tags: &ScopedTags, duration_seconds: u32, is_asf: bool) -> Metadata {
    let artists = tags.strings::<gst::tags::Artist>();
    let artist = if is_asf {
        artists.first().cloned()
    } else {
        join_tag_values(artists.clone())
    };
    let album_artist = tags
        .string::<gst::tags::AlbumArtist>()
        .or_else(|| tags.extended(&["albumartist", "album_artist"]))
        .or_else(|| is_asf.then(|| artists.get(1).cloned()).flatten());
    let track_number = tags
        .number::<gst::tags::TrackNumber>()
        .or_else(|| parse_number(tags.extended(&["tracknumber", "track_number", "track"])));
    let disc_number = tags
        .number::<gst::tags::AlbumVolumeNumber>()
        .or_else(|| parse_number(tags.extended(&["discnumber", "disc_number", "disc"])));
    let release_types = split_values(
        tags.extended_values(&["releasetype", "musicbrainz_albumtype"])
            .iter()
            .map(String::as_str),
    );
    let explicit_compilation = tags
        .extended(&["compilation"])
        .and_then(|value| parse_bool(&value));
    let is_compilation = explicit_compilation.or_else(|| {
        release_types
            .iter()
            .any(|value| value.eq_ignore_ascii_case("compilation"))
            .then_some(true)
    });
    Metadata {
        title: tags.string::<gst::tags::Title>(),
        album: tags.string::<gst::tags::Album>(),
        artist,
        album_artist,
        artist_mbids: split_values(
            tags.extended_values(&["musicbrainz_artistid"])
                .iter()
                .map(String::as_str),
        ),
        album_artist_mbids: split_values(
            tags.extended_values(&["musicbrainz_albumartistid"])
                .iter()
                .map(String::as_str),
        ),
        genres: split_values(
            tags.strings::<gst::tags::Genre>()
                .iter()
                .map(String::as_str),
        ),
        moods: split_values(tags.extended_values(&["mood"]).iter().map(String::as_str)),
        year: tag_year(tags),
        comment: tags.string::<gst::tags::Comment>().or_else(|| {
            is_asf
                .then(|| tags.string::<gst::tags::Description>())
                .flatten()
        }),
        bpm: tag_bpm(tags),
        track_number,
        disc_number,
        musicbrainz_album_id: tags.extended(&["musicbrainz_albumid"]),
        musicbrainz_release_group_id: tags.extended(&["musicbrainz_releasegroupid"]),
        musicbrainz_recording_id: tags
            .extended(&["musicbrainz_recordingid", "musicbrainz_trackid"]),
        musicbrainz_release_track_id: tags.extended(&["musicbrainz_releasetrackid"]),
        release_types,
        is_compilation,
        duration_seconds,
        artwork_index: tags.images().next().map(|_| 0),
        source_format: None,
    }
}

fn tag_strings<'a, T>(tags: &'a gst::TagList) -> Vec<String>
where
    T: gst::tags::Tag<'a, TagType = &'a str> + 'a,
{
    tags.iter_tag::<T>()
        .map(|value| value.get().trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn tag_u32<T>(tags: &gst::TagList) -> Option<u32>
where
    for<'a> T: gst::tags::Tag<'a, TagType = u32>,
{
    tags.get::<T>().map(|value| value.get())
}

fn tag_samples<'a, T>(tags: &'a gst::TagList) -> impl Iterator<Item = gst::Sample> + 'a
where
    for<'b> T: gst::tags::Tag<'b, TagType = gst::Sample> + 'a,
{
    tags.iter_tag::<T>().map(|value| value.get())
}

fn extended_comments(tags: &gst::TagList) -> HashMap<String, Vec<String>> {
    let mut fields = HashMap::<String, Vec<String>>::new();
    for comment in tag_strings::<gst::tags::ExtendedComment>(tags) {
        let Some((key, value)) = comment.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        fields
            .entry(normalized_key(key))
            .or_default()
            .push(value.to_string());
    }
    fields
}

fn extended_value(fields: &HashMap<String, Vec<String>>, key: &str) -> Option<String> {
    fields
        .get(&normalized_key(key))
        .and_then(|values| values.first())
        .cloned()
}

fn normalized_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn parse_number(value: Option<String>) -> Option<u16> {
    let value = value?;
    value
        .split_once('/')
        .map_or(value.as_str(), |(number, _)| number)
        .trim()
        .parse::<u32>()
        .ok()
        .map(|number| number.min(u32::from(u16::MAX)) as u16)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn split_values<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    values
        .flat_map(|value| value.split([';', ',', '\0']))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn join_tag_values(values: Vec<String>) -> Option<String> {
    (!values.is_empty()).then(|| values.join("; "))
}

fn tag_year(tags: &ScopedTags) -> Option<u16> {
    tags.audio
        .as_ref()
        .and_then(|tags| tags.get::<gst::tags::DateTime>())
        .or_else(|| {
            tags.container
                .as_ref()
                .and_then(|tags| tags.get::<gst::tags::DateTime>())
        })
        .map(|value| value.get().year())
        .or_else(|| {
            tags.audio
                .as_ref()
                .and_then(|tags| tags.get::<gst::tags::Date>())
                .or_else(|| {
                    tags.container
                        .as_ref()
                        .and_then(|tags| tags.get::<gst::tags::Date>())
                })
                .map(|value| i32::from(value.get().year()))
        })
        .and_then(|year| u16::try_from(year).ok())
}

fn tag_bpm(tags: &ScopedTags) -> Option<u16> {
    tags.audio
        .as_ref()
        .and_then(|tags| tags.get::<gst::tags::BeatsPerMinute>())
        .or_else(|| {
            tags.container
                .as_ref()
                .and_then(|tags| tags.get::<gst::tags::BeatsPerMinute>())
        })
        .map(|value| value.get())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value.round().min(f64::from(u16::MAX)) as u16)
        .or_else(|| {
            tags.extended(&["bpm"])
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value >= 0.0)
                .map(|value| value.round().min(f64::from(u16::MAX)) as u16)
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scope {
    Root,
    Segment,
    Attachments,
    AttachedFile,
}

#[derive(Debug, Eq, PartialEq)]
enum PreflightError {
    Io,
    InvalidAsf,
    InvalidEbml,
    AsfHeaderTooLarge,
    TooManyHeaders,
    TooManyAsfObjects,
    TooManyAttachments,
    AttachmentTooLarge,
    AttachmentsTooLarge,
    UnknownSize,
    TooDeep,
    MissingSegment,
}

fn preflight_known_container(file: &mut (impl Read + Seek)) -> Result<(), PreflightError> {
    file.rewind().map_err(|_| PreflightError::Io)?;
    let mut prefix = Vec::with_capacity(16);
    file.take(16)
        .read_to_end(&mut prefix)
        .map_err(|_| PreflightError::Io)?;
    if prefix.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        preflight_mka(file)
    } else if prefix == ASF_HEADER_GUID {
        preflight_asf(file)
    } else {
        Ok(())
    }
}

#[derive(Default)]
struct PreflightState {
    header_count: usize,
    attachment_count: usize,
    attachment_bytes: u64,
    segment_found: bool,
}

fn preflight_mka(mut file: &mut (impl Read + Seek)) -> Result<(), PreflightError> {
    let file_len = file
        .seek(SeekFrom::End(0))
        .map_err(|_| PreflightError::Io)?;
    file.rewind().map_err(|_| PreflightError::Io)?;
    let mut state = PreflightState::default();
    scan_elements(&mut file, file_len, Scope::Root, 0, &mut state)?;
    if !state.segment_found {
        return Err(PreflightError::MissingSegment);
    }
    Ok(())
}

fn preflight_asf(mut file: &mut (impl Read + Seek)) -> Result<(), PreflightError> {
    let file_len = file
        .seek(SeekFrom::End(0))
        .map_err(|_| PreflightError::Io)?;
    file.rewind().map_err(|_| PreflightError::Io)?;
    let mut guid = [0_u8; 16];
    file.read_exact(&mut guid)
        .map_err(|_| PreflightError::InvalidAsf)?;
    if guid != ASF_HEADER_GUID {
        return Err(PreflightError::InvalidAsf);
    }
    let header_size = read_u64_le(&mut file)?;
    if header_size > MAX_ASF_HEADER_BYTES {
        return Err(PreflightError::AsfHeaderTooLarge);
    }
    if header_size < 30 || header_size > file_len {
        return Err(PreflightError::InvalidAsf);
    }
    let object_count = read_u32_le(&mut file)?;
    if object_count > MAX_ASF_HEADER_OBJECTS {
        return Err(PreflightError::TooManyAsfObjects);
    }
    let mut reserved = [0_u8; 2];
    file.read_exact(&mut reserved)
        .map_err(|_| PreflightError::InvalidAsf)?;
    if reserved != [1, 2] {
        return Err(PreflightError::InvalidAsf);
    }
    let mut position = 30_u64;
    let mut declared_file_size = None;
    let mut contains_wma_pro = false;
    for _ in 0..object_count {
        let object_start = position;
        let next = position
            .checked_add(24)
            .filter(|next| *next <= header_size)
            .ok_or(PreflightError::InvalidAsf)?;
        file.seek(SeekFrom::Start(position))
            .map_err(|_| PreflightError::Io)?;
        file.read_exact(&mut guid)
            .map_err(|_| PreflightError::InvalidAsf)?;
        let object_size = read_u64_le(&mut file)?;
        if object_size < 24 {
            return Err(PreflightError::InvalidAsf);
        }
        if guid == ASF_FILE_PROPERTIES_GUID {
            if object_size < 104 {
                return Err(PreflightError::InvalidAsf);
            }
            file.seek(SeekFrom::Start(object_start + 40))
                .map_err(|_| PreflightError::Io)?;
            let size = read_u64_le(&mut file)?;
            if size != 0 {
                declared_file_size = Some(size);
            }
        }
        if guid == ASF_STREAM_PROPERTIES_GUID {
            if object_size < 78 {
                return Err(PreflightError::InvalidAsf);
            }
            file.seek(SeekFrom::Start(object_start + 24))
                .map_err(|_| PreflightError::Io)?;
            let mut stream_type = [0_u8; 16];
            file.read_exact(&mut stream_type)
                .map_err(|_| PreflightError::InvalidAsf)?;
            if stream_type == ASF_AUDIO_MEDIA_GUID {
                file.seek(SeekFrom::Start(object_start + 64))
                    .map_err(|_| PreflightError::Io)?;
                let type_specific_size = u64::from(read_u32_le(&mut file)?);
                if type_specific_size < 2
                    || 78_u64
                        .checked_add(type_specific_size)
                        .filter(|end| *end <= object_size)
                        .is_none()
                {
                    return Err(PreflightError::InvalidAsf);
                }
                file.seek(SeekFrom::Start(object_start + 78))
                    .map_err(|_| PreflightError::Io)?;
                contains_wma_pro |= read_u16_le(&mut file)? == WAVE_FORMAT_WMA_PRO;
            }
        }
        position = position
            .checked_add(object_size)
            .filter(|position| *position <= header_size && *position >= next)
            .ok_or(PreflightError::InvalidAsf)?;
    }
    if contains_wma_pro && declared_file_size.is_some_and(|size| size > file_len) {
        return Err(PreflightError::InvalidAsf);
    }
    (position == header_size)
        .then_some(())
        .ok_or(PreflightError::InvalidAsf)
}

fn read_u16_le(reader: &mut impl Read) -> Result<u16, PreflightError> {
    let mut bytes = [0_u8; 2];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| PreflightError::InvalidAsf)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32_le(reader: &mut impl Read) -> Result<u32, PreflightError> {
    let mut bytes = [0_u8; 4];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| PreflightError::InvalidAsf)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64_le(reader: &mut impl Read) -> Result<u64, PreflightError> {
    let mut bytes = [0_u8; 8];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| PreflightError::InvalidAsf)?;
    Ok(u64::from_le_bytes(bytes))
}

fn scan_elements(
    file: &mut (impl Read + Seek),
    end: u64,
    scope: Scope,
    depth: usize,
    state: &mut PreflightState,
) -> Result<(), PreflightError> {
    if depth > MAX_EBML_DEPTH {
        return Err(PreflightError::TooDeep);
    }
    while file.stream_position().map_err(|_| PreflightError::Io)? < end {
        state.header_count += 1;
        if state.header_count > MAX_EBML_HEADERS {
            return Err(PreflightError::TooManyHeaders);
        }
        let (id, _) = read_id(file)?;
        let size = read_size(file)?;
        let data_start = file.stream_position().map_err(|_| PreflightError::Io)?;

        if scope == Scope::AttachedFile && id == ID_FILE_DATA {
            let ElementSize::Known(size) = size else {
                return Err(PreflightError::UnknownSize);
            };
            if size > MAX_ATTACHMENT_BYTES {
                return Err(PreflightError::AttachmentTooLarge);
            }
            state.attachment_bytes = state
                .attachment_bytes
                .checked_add(size)
                .ok_or(PreflightError::AttachmentsTooLarge)?;
            if state.attachment_bytes > MAX_ATTACHMENT_BYTES {
                return Err(PreflightError::AttachmentsTooLarge);
            }
        }

        let data_end = match size {
            ElementSize::Known(size) => data_start
                .checked_add(size)
                .filter(|data_end| *data_end <= end)
                .ok_or(PreflightError::InvalidEbml)?,
            ElementSize::Unknown if scope == Scope::Root && id == ID_SEGMENT => end,
            ElementSize::Unknown if scope == Scope::Segment && id == ID_CLUSTER => {
                scan_unknown_cluster(file, end, state)?;
                continue;
            }
            ElementSize::Unknown => return Err(PreflightError::UnknownSize),
        };

        match (scope, id) {
            (Scope::Root, ID_SEGMENT) => {
                state.segment_found = true;
                scan_elements(file, data_end, Scope::Segment, depth + 1, state)?;
            }
            (Scope::Segment, ID_ATTACHMENTS) => {
                scan_elements(file, data_end, Scope::Attachments, depth + 1, state)?;
            }
            (Scope::Attachments, ID_ATTACHED_FILE) => {
                state.attachment_count += 1;
                if state.attachment_count > MAX_ATTACHMENT_COUNT {
                    return Err(PreflightError::TooManyAttachments);
                }
                scan_elements(file, data_end, Scope::AttachedFile, depth + 1, state)?;
            }
            (Scope::AttachedFile, ID_FILE_DATA) => {
                file.seek(SeekFrom::Start(data_end))
                    .map_err(|_| PreflightError::Io)?;
            }
            _ => {
                file.seek(SeekFrom::Start(data_end))
                    .map_err(|_| PreflightError::Io)?;
            }
        }
    }
    if file.stream_position().map_err(|_| PreflightError::Io)? != end {
        return Err(PreflightError::InvalidEbml);
    }
    Ok(())
}

fn scan_unknown_cluster(
    file: &mut (impl Read + Seek),
    segment_end: u64,
    state: &mut PreflightState,
) -> Result<(), PreflightError> {
    while file.stream_position().map_err(|_| PreflightError::Io)? < segment_end {
        let element_start = file.stream_position().map_err(|_| PreflightError::Io)?;
        let (id, width) = read_id(file)?;
        if width == 4 && is_top_level_matroska_element(id) {
            file.seek(SeekFrom::Start(element_start))
                .map_err(|_| PreflightError::Io)?;
            return Ok(());
        }
        state.header_count += 1;
        if state.header_count > MAX_EBML_HEADERS {
            return Err(PreflightError::TooManyHeaders);
        }
        let ElementSize::Known(size) = read_size(file)? else {
            return Err(PreflightError::UnknownSize);
        };
        let data_start = file.stream_position().map_err(|_| PreflightError::Io)?;
        let data_end = data_start
            .checked_add(size)
            .filter(|data_end| *data_end <= segment_end)
            .ok_or(PreflightError::InvalidEbml)?;
        file.seek(SeekFrom::Start(data_end))
            .map_err(|_| PreflightError::Io)?;
    }
    Ok(())
}

fn is_top_level_matroska_element(id: u64) -> bool {
    matches!(
        id,
        ID_SEEK_HEAD
            | ID_INFO
            | ID_CLUSTER
            | ID_TRACKS
            | ID_CUES
            | ID_ATTACHMENTS
            | ID_CHAPTERS
            | ID_TAGS
    )
}

#[derive(Clone, Copy, Debug)]
enum ElementSize {
    Known(u64),
    Unknown,
}

fn read_id(reader: &mut impl Read) -> Result<(u64, usize), PreflightError> {
    let (value, width, _) = read_vint(reader, 4, false)?;
    if width == 0 {
        return Err(PreflightError::InvalidEbml);
    }
    Ok((value, width))
}

fn read_size(reader: &mut impl Read) -> Result<ElementSize, PreflightError> {
    let (value, width, unknown) = read_vint(reader, 8, true)?;
    if width == 0 {
        return Err(PreflightError::InvalidEbml);
    }
    Ok(if unknown {
        ElementSize::Unknown
    } else {
        ElementSize::Known(value)
    })
}

fn read_vint(
    reader: &mut impl Read,
    max_width: usize,
    strip_marker: bool,
) -> Result<(u64, usize, bool), PreflightError> {
    let mut first = [0_u8; 1];
    reader
        .read_exact(&mut first)
        .map_err(|_| PreflightError::InvalidEbml)?;
    let leading = first[0].leading_zeros() as usize;
    let width = leading + 1;
    if first[0] == 0 || width > max_width {
        return Err(PreflightError::InvalidEbml);
    }
    let marker = 1_u8 << (8 - width);
    let mut value = if strip_marker {
        u64::from(first[0] & !marker)
    } else {
        u64::from(first[0])
    };
    for _ in 1..width {
        let mut next = [0_u8; 1];
        reader
            .read_exact(&mut next)
            .map_err(|_| PreflightError::InvalidEbml)?;
        value = (value << 8) | u64::from(next[0]);
    }
    let unknown = strip_marker && value == ((1_u64 << (7 * width)) - 1);
    Ok((value, width, unknown))
}

#[cfg(test)]
mod tests {
    use std::io::{Seek, Write};

    use super::*;

    #[test]
    fn tag_precedence_and_extended_comments_map_to_mka_metadata() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let mut container = gst::TagList::new();
        let container = container.get_mut().expect("writable container tags");
        container.add::<gst::tags::Title>(&"Container title", gst::TagMergeMode::Append);
        container.add::<gst::tags::Artist>(&"Container artist", gst::TagMergeMode::Append);
        container.add::<gst::tags::Album>(&"Album", gst::TagMergeMode::Append);
        container.add::<gst::tags::ExtendedComment>(
            &"ALBUM_ARTIST=Album artist",
            gst::TagMergeMode::Append,
        );
        container.add::<gst::tags::ExtendedComment>(&"TRACKNUMBER=7/12", gst::TagMergeMode::Append);
        container.add::<gst::tags::ExtendedComment>(&"DISC_NUMBER=2/3", gst::TagMergeMode::Append);
        container.add::<gst::tags::ExtendedComment>(
            &"MUSICBRAINZ_ALBUMID=album-id",
            gst::TagMergeMode::Append,
        );
        container.add::<gst::tags::ExtendedComment>(
            &"MUSICBRAINZ_TRACKID=recording-id",
            gst::TagMergeMode::Append,
        );
        container.add::<gst::tags::ExtendedComment>(
            &"MUSICBRAINZ_RELEASETRACKID=release-track-id",
            gst::TagMergeMode::Append,
        );
        container.add::<gst::tags::ExtendedComment>(
            &"MUSICBRAINZ_ALBUMTYPE=album; compilation",
            gst::TagMergeMode::Append,
        );

        let mut audio = gst::TagList::new();
        let audio = audio.get_mut().expect("writable audio tags");
        audio.add::<gst::tags::Title>(&"Stream title", gst::TagMergeMode::Append);
        audio.add::<gst::tags::Artist>(&"Stream artist", gst::TagMergeMode::Append);

        let tags = ScopedTags::new(Some(container.to_owned()), Some(audio.to_owned()));
        let metadata = metadata_from_tags(&tags, 42, false);

        assert_eq!(metadata.title.as_deref(), Some("Stream title"));
        assert_eq!(metadata.artist.as_deref(), Some("Stream artist"));
        assert_eq!(metadata.album.as_deref(), Some("Album"));
        assert_eq!(metadata.album_artist.as_deref(), Some("Album artist"));
        assert_eq!(metadata.track_number, Some(7));
        assert_eq!(metadata.disc_number, Some(2));
        assert_eq!(metadata.musicbrainz_album_id.as_deref(), Some("album-id"));
        assert_eq!(
            metadata.musicbrainz_recording_id.as_deref(),
            Some("recording-id")
        );
        assert_eq!(
            metadata.musicbrainz_release_track_id.as_deref(),
            Some("release-track-id")
        );
        assert_eq!(metadata.release_types, ["album", "compilation"]);
        assert_eq!(metadata.is_compilation, Some(true));
        assert_eq!(metadata.duration_seconds, 42);
    }

    #[test]
    fn asf_artist_mapping_recovers_the_album_artist_value() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let mut container = gst::TagList::new();
        let container = container.get_mut().expect("writable container tags");
        container.add::<gst::tags::Artist>(&"Track artist", gst::TagMergeMode::Append);
        container.add::<gst::tags::Artist>(&"Album artist", gst::TagMergeMode::Append);
        container.add::<gst::tags::Description>(&"Track comment", gst::TagMergeMode::Append);

        let tags = ScopedTags::new(Some(container.to_owned()), None);
        let metadata = metadata_from_tags(&tags, 1, true);

        assert_eq!(metadata.artist.as_deref(), Some("Track artist"));
        assert_eq!(metadata.album_artist.as_deref(), Some("Album artist"));
        assert_eq!(metadata.comment.as_deref(), Some("Track comment"));
    }

    #[test]
    fn admission_requires_success_one_audio_and_only_still_images() {
        assert!(discovery_is_admitted(DiscovererResult::Ok, 1, &[]));
        assert!(discovery_is_admitted(DiscovererResult::Ok, 1, &[true]));
        for result in [
            DiscovererResult::Error,
            DiscovererResult::Timeout,
            DiscovererResult::MissingPlugins,
        ] {
            assert!(!discovery_is_admitted(result, 1, &[]));
        }
        assert!(!discovery_is_admitted(DiscovererResult::Ok, 0, &[]));
        assert!(!discovery_is_admitted(DiscovererResult::Ok, 2, &[]));
        assert!(!discovery_is_admitted(DiscovererResult::Ok, 1, &[false]));
        assert!(!discovery_is_admitted(
            DiscovererResult::Ok,
            1,
            &[true, false]
        ));
        assert!(video_shape_is_still(true, None));
        assert!(video_shape_is_still(false, Some("image/png")));
        assert!(!video_shape_is_still(false, Some("video/x-h264")));
    }

    #[test]
    fn preflight_accepts_bounded_attachments() {
        let directory = tempfile::tempdir().expect("MKA fixture directory");
        let path = directory.path().join("bounded.mka");
        fs::write(&path, mka_with_attachment(&[1, 2, 3], false)).expect("write MKA fixture");

        assert_eq!(
            preflight_mka(&mut fs::File::open(&path).expect("open fixture")),
            Ok(())
        );
    }

    #[test]
    fn preflight_rejects_unknown_and_oversized_attachments() {
        let directory = tempfile::tempdir().expect("MKA fixture directory");
        let unknown = directory.path().join("unknown.mka");
        fs::write(&unknown, mka_with_attachment(&[], true)).expect("write unknown fixture");
        assert_eq!(
            preflight_mka(&mut fs::File::open(&unknown).expect("open fixture")),
            Err(PreflightError::UnknownSize)
        );

        let oversized = directory.path().join("oversized.mka");
        let mut bytes = Vec::new();
        bytes.extend(element_header(ID_SEGMENT, None));
        let file_data = element_header(ID_FILE_DATA, Some(MAX_ATTACHMENT_BYTES + 1));
        let attached_file = element(ID_ATTACHED_FILE, &file_data);
        bytes.extend(element(ID_ATTACHMENTS, &attached_file));
        fs::write(&oversized, bytes).expect("write oversized fixture");
        assert_eq!(
            preflight_mka(&mut fs::File::open(&oversized).expect("open fixture")),
            Err(PreflightError::AttachmentTooLarge)
        );
    }

    #[test]
    fn preflight_bounds_attachment_count_and_total_bytes() {
        let directory = tempfile::tempdir().expect("MKA fixture directory");
        let excessive_count = directory.path().join("many-attachments.mka");
        let attachments = (0..=MAX_ATTACHMENT_COUNT)
            .flat_map(|_| element(ID_ATTACHED_FILE, &[]))
            .collect::<Vec<_>>();
        let mut bytes = element_header(ID_SEGMENT, None);
        bytes.extend(element(ID_ATTACHMENTS, &attachments));
        fs::write(&excessive_count, bytes).expect("write attachment-count fixture");
        assert_eq!(
            preflight_mka(&mut fs::File::open(&excessive_count).expect("open fixture")),
            Err(PreflightError::TooManyAttachments)
        );

        let excessive_bytes = directory.path().join("large-attachments.mka");
        write_sparse_attachments(&excessive_bytes, &[MAX_ATTACHMENT_BYTES / 2 + 1; 2]);
        assert_eq!(
            preflight_mka(&mut fs::File::open(&excessive_bytes).expect("open fixture")),
            Err(PreflightError::AttachmentsTooLarge)
        );
    }

    #[test]
    fn preflight_bounds_element_header_work() {
        let directory = tempfile::tempdir().expect("MKA fixture directory");
        let path = directory.path().join("many-elements.mka");
        let mut bytes = element_header(ID_SEGMENT, None);
        for _ in 0..MAX_EBML_HEADERS {
            bytes.extend(element_header(0x81, Some(0)));
        }
        fs::write(&path, bytes).expect("write element-count fixture");

        assert_eq!(
            preflight_mka(&mut fs::File::open(&path).expect("open fixture")),
            Err(PreflightError::TooManyHeaders)
        );
    }

    #[test]
    fn preflight_does_not_limit_the_audio_payload_size() {
        let directory = tempfile::tempdir().expect("MKA fixture directory");
        let path = directory.path().join("large-audio.mka");
        let mut file = fs::File::create(&path).expect("create sparse MKA fixture");
        file.write_all(&element_header(ID_SEGMENT, None))
            .expect("write Segment");
        let payload_size = MAX_ATTACHMENT_BYTES * 2;
        file.write_all(&element_header(0x1F43_B675, Some(payload_size)))
            .expect("write Cluster");
        file.seek(SeekFrom::Current(
            i64::try_from(payload_size).expect("test payload fits i64") - 1,
        ))
        .expect("seek sparse payload");
        file.write_all(&[0]).expect("finish sparse payload");

        assert_eq!(
            preflight_mka(&mut fs::File::open(&path).expect("open fixture")),
            Ok(())
        );
    }

    #[test]
    fn preflight_accepts_unknown_clusters_and_checks_later_attachments() {
        let directory = tempfile::tempdir().expect("MKA fixture directory");
        let valid = directory.path().join("streaming.mka");
        let mut bytes = element_header(ID_SEGMENT, None);
        bytes.extend(element_header(ID_CLUSTER, None));
        bytes.extend(element(0xE7, &[0]));
        fs::write(&valid, &bytes).expect("write streaming MKA fixture");
        assert_eq!(
            preflight_mka(&mut fs::File::open(&valid).expect("open fixture")),
            Ok(())
        );

        let oversized = directory.path().join("streaming-oversized-artwork.mka");
        bytes.extend(element(
            ID_ATTACHMENTS,
            &element(
                ID_ATTACHED_FILE,
                &element_header(ID_FILE_DATA, Some(MAX_ATTACHMENT_BYTES + 1)),
            ),
        ));
        fs::write(&oversized, bytes).expect("write streaming MKA with oversized artwork");
        assert_eq!(
            preflight_mka(&mut fs::File::open(&oversized).expect("open fixture")),
            Err(PreflightError::AttachmentTooLarge)
        );
    }

    #[test]
    fn asf_preflight_bounds_only_the_metadata_header() {
        let directory = tempfile::tempdir().expect("ASF fixture directory");
        let path = directory.path().join("large-audio.wma");
        let mut file = fs::File::create(&path).expect("create sparse ASF fixture");
        file.write_all(&asf_header(30, 0))
            .expect("write ASF header");
        file.set_len(MAX_ASF_HEADER_BYTES * 2)
            .expect("extend sparse audio payload");

        assert_eq!(
            preflight_asf(&mut fs::File::open(&path).expect("open fixture")),
            Ok(())
        );
    }

    #[test]
    fn asf_preflight_rejects_oversized_or_excessive_headers() {
        let directory = tempfile::tempdir().expect("ASF fixture directory");
        let oversized = directory.path().join("oversized.wma");
        fs::write(&oversized, asf_header(MAX_ASF_HEADER_BYTES + 1, 0))
            .expect("write oversized ASF header");
        assert_eq!(
            preflight_asf(&mut fs::File::open(&oversized).expect("open fixture")),
            Err(PreflightError::AsfHeaderTooLarge)
        );

        let excessive = directory.path().join("excessive.wma");
        fs::write(&excessive, asf_header(30, MAX_ASF_HEADER_OBJECTS + 1))
            .expect("write excessive ASF header");
        assert_eq!(
            preflight_asf(&mut fs::File::open(&excessive).expect("open fixture")),
            Err(PreflightError::TooManyAsfObjects)
        );
    }

    #[test]
    fn asf_preflight_rejects_truncated_wma_pro_but_accepts_partial_wma2() {
        let directory = tempfile::tempdir().expect("ASF fixture directory");
        let complete = directory.path().join("complete.wma");
        fs::write(
            &complete,
            asf_with_declared_file_size(214, WAVE_FORMAT_WMA_PRO),
        )
        .expect("write complete ASF fixture");
        assert_eq!(
            preflight_asf(&mut fs::File::open(&complete).expect("open fixture")),
            Ok(())
        );

        let truncated_pro = directory.path().join("truncated-pro.wma");
        fs::write(
            &truncated_pro,
            asf_with_declared_file_size(1_000, WAVE_FORMAT_WMA_PRO),
        )
        .expect("write truncated WMA Pro fixture");
        assert_eq!(
            preflight_asf(&mut fs::File::open(&truncated_pro).expect("open fixture")),
            Err(PreflightError::InvalidAsf)
        );

        let partial_wma2 = directory.path().join("partial-wma2.wma");
        fs::write(&partial_wma2, asf_with_declared_file_size(1_000, 0x0161))
            .expect("write partial WMA2 fixture");
        assert_eq!(
            preflight_asf(&mut fs::File::open(&partial_wma2).expect("open fixture")),
            Ok(())
        );
    }

    fn mka_with_attachment(data: &[u8], unknown_data_size: bool) -> Vec<u8> {
        let mut file_data = element_header(
            ID_FILE_DATA,
            (!unknown_data_size).then_some(data.len() as u64),
        );
        file_data.extend_from_slice(data);
        let attached_file = element(ID_ATTACHED_FILE, &file_data);
        let attachments = element(ID_ATTACHMENTS, &attached_file);
        let mut bytes = element_header(ID_SEGMENT, None);
        bytes.extend(attachments);
        bytes
    }

    fn asf_header(size: u64, object_count: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&ASF_HEADER_GUID);
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&object_count.to_le_bytes());
        bytes.extend_from_slice(&[1, 2]);
        bytes
    }

    fn asf_with_declared_file_size(file_size: u64, codec: u16) -> Vec<u8> {
        let mut bytes = asf_header(214, 2);
        bytes.extend_from_slice(&ASF_FILE_PROPERTIES_GUID);
        bytes.extend_from_slice(&104_u64.to_le_bytes());
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&file_size.to_le_bytes());
        bytes.extend_from_slice(&[0; 56]);
        bytes.extend_from_slice(&ASF_STREAM_PROPERTIES_GUID);
        bytes.extend_from_slice(&80_u64.to_le_bytes());
        bytes.extend_from_slice(&ASF_AUDIO_MEDIA_GUID);
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&2_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&codec.to_le_bytes());
        bytes
    }

    fn write_sparse_attachments(path: &Path, sizes: &[u64]) {
        let entries = sizes
            .iter()
            .map(|size| {
                let file_data = element_header(ID_FILE_DATA, Some(*size));
                let payload_size = file_data.len() as u64 + size;
                (
                    element_header(ID_ATTACHED_FILE, Some(payload_size)),
                    file_data,
                )
            })
            .collect::<Vec<_>>();
        let attachments_size = entries
            .iter()
            .zip(sizes)
            .map(|((attached_file, file_data), size)| {
                attached_file.len() as u64 + file_data.len() as u64 + size
            })
            .sum();
        let mut file = fs::File::create(path).expect("create sparse attachment fixture");
        file.write_all(&element_header(ID_SEGMENT, None))
            .expect("write Segment");
        file.write_all(&element_header(ID_ATTACHMENTS, Some(attachments_size)))
            .expect("write Attachments");
        for ((attached_file, file_data), size) in entries.iter().zip(sizes) {
            file.write_all(attached_file).expect("write AttachedFile");
            file.write_all(file_data).expect("write FileData");
            file.seek(SeekFrom::Current(
                i64::try_from(*size).expect("test attachment size fits i64"),
            ))
            .expect("seek sparse attachment data");
        }
        let end = file.stream_position().expect("read fixture length");
        file.set_len(end).expect("finish sparse attachment fixture");
    }

    fn element(id: u64, data: &[u8]) -> Vec<u8> {
        let mut bytes = element_header(id, Some(data.len() as u64));
        bytes.extend_from_slice(data);
        bytes
    }

    fn element_header(id: u64, size: Option<u64>) -> Vec<u8> {
        let mut bytes = encode_id(id);
        bytes.extend(match size {
            Some(size) => encode_size(size),
            None => vec![0xff],
        });
        bytes
    }

    fn encode_id(id: u64) -> Vec<u8> {
        let bytes = id.to_be_bytes();
        bytes[bytes.iter().position(|byte| *byte != 0).unwrap_or(7)..].to_vec()
    }

    fn encode_size(size: u64) -> Vec<u8> {
        for width in 1..=8 {
            let max = (1_u64 << (7 * width)) - 2;
            if size <= max {
                let value = size | (1_u64 << (7 * width));
                return value.to_be_bytes()[8 - width..].to_vec();
            }
        }
        panic!("test EBML size is encodable");
    }
}
