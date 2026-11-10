//! Media discovery, admission, generic tags and embedded artwork.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek};
use std::path::Path;

use gstreamer_pbutils::gst::{self, glib::translate::IntoGlib};
use gstreamer_pbutils::prelude::*;
use gstreamer_pbutils::{Discoverer, DiscovererInfo, DiscovererResult};

use crate::{ImageBytes, SourceError, SourceResult};

mod process;
pub use process::DiscoveryWorker;
pub use process::run_worker;
#[cfg(target_os = "android")]
pub use process::{DiscoveryHost, DiscoverySession, install_discovery_host};

const DISCOVERER_TIMEOUT_SECONDS: u64 = 30;
const MAX_ATTACHMENT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Metadata {
    #[serde(default)]
    pub(crate) audio_properties: library::AudioProperties,
    pub(crate) title: Option<String>,
    pub(crate) album: Option<String>,
    pub(crate) artist: Option<String>,
    pub(crate) album_artist: Option<String>,
    pub(crate) sort_title: Option<String>,
    pub(crate) sort_album: Option<String>,
    pub(crate) artist_sort: Vec<String>,
    pub(crate) album_artist_sort: Vec<String>,
    pub(crate) artist_mbids: Vec<String>,
    pub(crate) album_artist_mbids: Vec<String>,
    pub(crate) genres: Vec<String>,
    pub(crate) moods: Vec<String>,
    pub(crate) year: Option<u16>,
    pub(crate) release_date: Option<String>,
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
    process: Option<process::Client>,
    timeout_seconds: Option<u64>,
}

impl Reader {
    pub(crate) fn read(&mut self, path: &Path) -> Option<Metadata> {
        let mut file = fs::File::open(path).ok()?;
        let uri = url::Url::from_file_path(path).ok()?;
        self.read_input(&mut file, uri.as_str()).ok().flatten()
    }

    /// Discovers `uri` in the worker process; the caller keeps the input it serves open.
    pub(crate) fn read_input(
        &mut self,
        _input: &mut (impl Read + Seek),
        uri: &str,
    ) -> std::io::Result<Option<Metadata>> {
        match self.request(uri, None)? {
            process::Reply::Metadata(metadata) => Ok(metadata.map(|metadata| *metadata)),
            _ => Err(std::io::Error::other("Unexpected discovery response")),
        }
    }

    fn request(
        &mut self,
        uri: &str,
        picture_index: Option<u32>,
    ) -> std::io::Result<process::Reply> {
        if self.process.is_none() {
            self.process = Some(process::Client::start(
                self.timeout_seconds.unwrap_or(DISCOVERER_TIMEOUT_SECONDS),
            )?);
        }
        let result = self.process.as_mut().unwrap().request(uri, picture_index);
        if result.is_err() {
            self.process = None;
        }
        result
    }
}

pub(crate) fn read_image_input(
    reader: &mut Reader,
    _input: &mut (impl Read + Seek),
    uri: &str,
    picture_index: u32,
) -> SourceResult<ImageBytes> {
    match reader
        .request(uri, Some(picture_index))
        .map_err(|error| SourceError::Other(error.to_string()))?
    {
        process::Reply::Image {
            bytes,
            content_type,
            ..
        } => Ok(ImageBytes {
            bytes,
            content_type,
        }),
        process::Reply::Error(error) => Err(SourceError::Other(error)),
        _ => Err(SourceError::NotFound),
    }
}

fn image_from_info(info: &DiscovererInfo, picture_index: u32) -> SourceResult<ImageBytes> {
    let audio = admitted_audio_stream(info).ok_or(SourceError::NotFound)?;
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
    metadata.audio_properties = library::AudioProperties {
        bitrate_kbps: (audio.bitrate() > 0).then(|| audio.bitrate() / 1000),
        sample_rate_hz: (audio.sample_rate() > 0).then(|| audio.sample_rate()),
        bit_depth: (audio.depth() > 0).then(|| audio.depth()),
        channels: (audio.channels() > 0).then(|| audio.channels()),
        size_bytes: None,
    };
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
    let audio = info.audio_streams();
    let video_is_image = info
        .video_streams()
        .iter()
        .map(video_is_still)
        .collect::<Vec<_>>();
    if !discovery_is_admitted(info.result(), audio.len(), &video_is_image) {
        return None;
    }
    audio.into_iter().next()
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
        && audio_streams > 0
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
    let release_date = tag_date(tags);
    let year = release_date.as_deref().and_then(crate::policy::date_year);
    let artists = tags.strings::<gst::tags::Artist>();
    let artist = if is_asf {
        artists.first().cloned()
    } else {
        join_tag_values(artists.clone())
    };
    let album_artist = join_tag_values(tags.strings::<gst::tags::AlbumArtist>())
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
        audio_properties: library::AudioProperties::default(),
        title: tags.string::<gst::tags::Title>(),
        sort_title: tags.string::<gst::tags::TitleSortname>(),
        sort_album: tags.string::<gst::tags::AlbumSortname>(),
        artist_sort: tags
            .strings::<gst::tags::ArtistSortname>()
            .iter()
            .flat_map(|value| super::media::split_names(value))
            .collect(),
        album_artist_sort: tags
            .strings::<gst::tags::AlbumArtistSortname>()
            .iter()
            .flat_map(|value| super::media::split_names(value))
            .collect(),
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
        year,
        release_date,
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

fn tag_date(tags: &ScopedTags) -> Option<String> {
    tags.audio
        .as_ref()
        .and_then(|tags| tags.get::<gst::tags::DateTime>())
        .or_else(|| {
            tags.container
                .as_ref()
                .and_then(|tags| tags.get::<gst::tags::DateTime>())
        })
        .and_then(|value| value.get().to_iso8601_string().ok())
        .and_then(|value| crate::policy::normalized_date(Some(value.to_string())))
        .or_else(|| {
            tags.audio
                .as_ref()
                .and_then(|tags| tags.get::<gst::tags::Date>())
                .or_else(|| {
                    tags.container
                        .as_ref()
                        .and_then(|tags| tags.get::<gst::tags::Date>())
                })
                .map(|value| {
                    let date = value.get();
                    format!(
                        "{:04}-{:02}-{:02}",
                        date.year(),
                        date.month().into_glib(),
                        date.day()
                    )
                })
        })
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

#[cfg(test)]
mod tests {

    use super::*;
    use audio_processing::ensure_gstreamer_initialized;

    #[test]
    fn tag_precedence_and_extended_comments_map_to_mka_metadata() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let mut container = gst::TagList::new();
        let container = container.get_mut().expect("writable container tags");
        container.add::<gst::tags::Title>(&"Container title", gst::TagMergeMode::Append);
        container.add::<gst::tags::Artist>(&"Container artist", gst::TagMergeMode::Append);
        container.add::<gst::tags::Album>(&"Album", gst::TagMergeMode::Append);
        container.add::<gst::tags::Date>(
            &gst::glib::Date::from_dmy(8, gst::glib::DateMonth::July, 1995).unwrap(),
            gst::TagMergeMode::Append,
        );
        container.add::<gst::tags::TitleSortname>(&"A Title", gst::TagMergeMode::Append);
        container.add::<gst::tags::AlbumSortname>(&"An Album", gst::TagMergeMode::Append);
        container.add::<gst::tags::ArtistSortname>(&"Artist, Stream", gst::TagMergeMode::Append);
        container
            .add::<gst::tags::AlbumArtistSortname>(&"Artist, Album", gst::TagMergeMode::Append);
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
        assert_eq!(metadata.sort_title.as_deref(), Some("A Title"));
        assert_eq!(metadata.sort_album.as_deref(), Some("An Album"));
        assert_eq!(metadata.artist_sort, ["Artist, Stream"]);
        assert_eq!(metadata.album_artist_sort, ["Artist, Album"]);
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
        assert_eq!(metadata.year, Some(1995));
        assert_eq!(metadata.release_date.as_deref(), Some("1995-07-08"));
        for date in ["2001", "2001-02", "2001-02-03", "2001-02-03T04:05:06Z"] {
            audio.add::<gst::tags::DateTime>(
                &gst::DateTime::from_iso8601_string(date).unwrap(),
                gst::TagMergeMode::Replace,
            );
            let tags = ScopedTags::new(Some(container.to_owned()), Some(audio.to_owned()));
            let metadata = metadata_from_tags(&tags, 42, false);
            assert_eq!(metadata.year, Some(2001));
            assert_eq!(
                metadata.release_date.as_deref(),
                date.get(..date.len().min(10))
            );
        }
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
    fn admission_requires_success_audio_and_only_still_images() {
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
        assert!(discovery_is_admitted(DiscovererResult::Ok, 2, &[]));
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
}
