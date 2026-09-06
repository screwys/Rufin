//! One Local media candidate: Lofty metadata with container-specific GStreamer admission.

use std::fs;
use std::path::{Path, PathBuf};

use crate::LocalImageRef;
use lofty::config::ParseOptions;
use lofty::file::FileType;
use lofty::file::TaggedFileExt;
use lofty::prelude::*;
use lofty::tag::{ItemKey, Tag};

use crate::policy::stable_hash;

use super::discovery;
use super::lofty_metadata::{read_lofty, read_lofty_file, source_format};

#[derive(Clone, Debug)]
pub(super) struct ScannedTrack {
    pub(super) id: String,
    pub(super) album_id: String,
    pub(super) title: String,
    pub(super) artist: String,
    pub(super) album: String,
    pub(super) year: u16,
    pub(super) duration_seconds: u32,
    pub(super) disc_number: u16,
    pub(super) track_number: u16,
    pub(super) local_artwork: Option<LocalImageRef>,
    pub(super) musicbrainz_recording_id: Option<String>,
    pub(super) musicbrainz_release_track_id: Option<String>,
    pub(super) source_path: String,
    pub(super) cue_path: Option<String>,
    pub(super) cue_start_millis: Option<i64>,
    pub(super) cue_end_millis: Option<i64>,
    pub(super) source_format: Option<String>,
    pub(super) comment: Option<String>,
    pub(super) bpm: Option<u16>,
    pub(super) user_rating: Option<u8>,
    pub(super) artists: Vec<ArtistCredit>,
    pub(super) album_artists: Vec<ArtistCredit>,
    pub(super) genres: Vec<NamedCredit>,
    pub(super) moods: Vec<NamedCredit>,
    pub(super) album_artist: String,
    pub(super) release_types: Vec<String>,
    pub(super) is_compilation: Option<bool>,
    pub(super) musicbrainz_album_id: Option<String>,
    pub(super) musicbrainz_release_group_id: Option<String>,
    pub(super) track_r128_lufs: Option<f64>,
    pub(super) album_r128_lufs: Option<f64>,
    pub(super) replay_gain_track_db: Option<f64>,
    pub(super) replay_gain_track_peak: Option<f64>,
    pub(super) replay_gain_album_db: Option<f64>,
    pub(super) replay_gain_album_peak: Option<f64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ArtistCredit {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) musicbrainz_artist_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NamedCredit {
    pub(super) id: String,
    pub(super) name: String,
}

#[derive(Clone, Debug)]
pub(super) enum MediaRead {
    Accepted(Box<ScannedTrack>),
    Rejected,
    Unreadable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BasicAudioMetadata {
    pub(super) title: String,
    pub(super) album: String,
    pub(super) artist: String,
    pub(super) disc_number: u16,
    pub(super) track_number: u16,
    pub(super) duration_seconds: u32,
}

struct MetadataArtist {
    name: String,
    musicbrainz_id: Option<String>,
}

struct AudioMetadata {
    basic: BasicAudioMetadata,
    album_artist: String,
    artists: Vec<MetadataArtist>,
    album_artists: Vec<MetadataArtist>,
    genres: Vec<String>,
    moods: Vec<String>,
    year: u16,
    comment: Option<String>,
    bpm: Option<u16>,
    musicbrainz_album_id: Option<String>,
    musicbrainz_release_group_id: Option<String>,
    musicbrainz_recording_id: Option<String>,
    musicbrainz_release_track_id: Option<String>,
    release_types: Vec<String>,
    is_compilation: Option<bool>,
    local_artwork: Option<LocalImageRef>,
    source_format: Option<String>,
    user_rating: Option<u8>,
    track_r128_lufs: Option<f64>,
    album_r128_lufs: Option<f64>,
    replay_gain_track_db: Option<f64>,
    replay_gain_track_peak: Option<f64>,
    replay_gain_album_db: Option<f64>,
    replay_gain_album_peak: Option<f64>,
}

#[derive(Default)]
pub(super) struct Worker {
    discovery: discovery::Reader,
}

pub(super) fn read_media(
    worker: &mut Worker,
    path: PathBuf,
    sidecar: Option<LocalImageRef>,
) -> MediaRead {
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(_) => return MediaRead::Unreadable,
    };
    // Recognize a frame sync at the start or immediately after ID3 metadata;
    // searching binary interiors can mistake executable bytes for MPEG audio.
    let tagged_file = read_lofty_file(
        file,
        ParseOptions::new().read_cover_art(false).max_junk_bytes(2),
    )
    .ok()
    .flatten();
    let topology_admitted = tagged_file
        .as_ref()
        .filter(|file| requires_topology_admission(file.file_type()))
        .map(|_| worker.discovery.read(&path));
    let mut tagged_file = tagged_file.filter(|file| {
        if requires_topology_admission(file.file_type()) {
            topology_admitted.as_ref().is_some_and(Option::is_some)
        } else {
            lofty_supplies_required_audio(file)
        }
    });
    let discovered = if tagged_file.is_none() {
        if topology_admitted.is_some_and(|admitted| admitted.is_none()) {
            return MediaRead::Rejected;
        }
        let Some(discovered) = worker.discovery.read(&path) else {
            return MediaRead::Rejected;
        };
        Some(discovered)
    } else {
        None
    };
    if tagged_file.is_none() {
        // Discovery has confirmed audio, so retain Lofty's tag recovery for
        // playable files whose frames or ID3 metadata follow leading junk.
        tagged_file = read_lofty(&path, false)
            .ok()
            .flatten()
            .filter(lofty_supplies_required_audio);
    }
    let metadata = if let Some(tagged_file) = tagged_file.as_ref() {
        audio_metadata_from_lofty(&path, tagged_file, sidecar)
    } else {
        audio_metadata_from_discoverer(&path, discovered.as_ref(), sidecar)
    };
    MediaRead::Accepted(Box::new(scanned_track(&path, metadata)))
}

fn audio_metadata_from_lofty(
    path: &Path,
    tagged_file: &lofty::file::TaggedFile,
    local_artwork: Option<LocalImageRef>,
) -> AudioMetadata {
    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());
    let duration_seconds = lofty_duration_seconds(tagged_file);
    let basic = basic_audio_metadata(path, tag, duration_seconds);
    let artist = &basic.artist;
    let album_artist = tag
        .and_then(|tag| tag.get_string(ItemKey::AlbumArtist))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| artist.clone());
    let artist_names = artist_names(tag, artist);
    let artist_mbids = aligned_mbids(&artist_names, tag_mbids(tag, ItemKey::MusicBrainzArtistId));
    let artists = artist_names
        .into_iter()
        .zip(artist_mbids)
        .map(|(name, musicbrainz_id)| MetadataArtist {
            name,
            musicbrainz_id,
        })
        .collect();
    let album_artist_names = split_names(&album_artist);
    let album_artist_mbids = aligned_mbids(
        &album_artist_names,
        tag_mbids(tag, ItemKey::MusicBrainzReleaseArtistId),
    );
    let album_artists = album_artist_names
        .into_iter()
        .zip(album_artist_mbids)
        .map(|(name, musicbrainz_id)| MetadataArtist {
            name,
            musicbrainz_id,
        })
        .collect();
    let genres = tag
        .and_then(|tag| tag.genre().map(|value| split_names(&value)))
        .unwrap_or_default();
    let moods = tag_values_optional(tag, ItemKey::Mood)
        .into_iter()
        .flat_map(|value| split_names(&value))
        .collect();
    let musicbrainz_album_id = tag.and_then(|tag| tag_mbid(tag, ItemKey::MusicBrainzReleaseId));
    let musicbrainz_release_group_id =
        tag.and_then(|tag| tag_mbid(tag, ItemKey::MusicBrainzReleaseGroupId));
    let release_types = album_release_types(tag);
    let is_compilation = album_compilation(tag, &release_types);
    AudioMetadata {
        basic,
        album_artist,
        artists,
        album_artists,
        genres,
        moods,
        year: tag
            .and_then(|tag| tag.date())
            .map(|date| date.year)
            .unwrap_or_default(),
        comment: tag
            .and_then(|tag| tag.get_string(ItemKey::Comment))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
        bpm: tag_bpm(tag),
        musicbrainz_album_id,
        musicbrainz_release_group_id,
        musicbrainz_recording_id: tag
            .and_then(|tag| tag_mbid(tag, ItemKey::MusicBrainzRecordingId)),
        musicbrainz_release_track_id: tag
            .and_then(|tag| tag_mbid(tag, ItemKey::MusicBrainzTrackId)),
        release_types,
        is_compilation,
        local_artwork,
        source_format: source_format(path, tagged_file.file_type()),
        user_rating: tag
            .and_then(|tag| tag.ratings().next())
            .map(|rating| (rating.rating() as u8) * 2),
        track_r128_lufs: r128_integrated_lufs(tag, ItemKey::R128TrackGain),
        album_r128_lufs: r128_integrated_lufs(tag, ItemKey::R128AlbumGain),
        replay_gain_track_db: replay_gain_db(tag, ItemKey::ReplayGainTrackGain),
        replay_gain_track_peak: replay_gain_peak(tag, ItemKey::ReplayGainTrackPeak),
        replay_gain_album_db: replay_gain_db(tag, ItemKey::ReplayGainAlbumGain),
        replay_gain_album_peak: replay_gain_peak(tag, ItemKey::ReplayGainAlbumPeak),
    }
}

fn lofty_duration_seconds(tagged_file: &lofty::file::TaggedFile) -> u32 {
    tagged_file
        .properties()
        .duration()
        .as_secs()
        .min(u64::from(u32::MAX)) as u32
}

fn lofty_supplies_required_audio(file: &lofty::file::TaggedFile) -> bool {
    !file.properties().duration().is_zero()
}

fn requires_topology_admission(file_type: FileType) -> bool {
    matches!(
        file_type,
        FileType::Mp4 | FileType::Opus | FileType::Vorbis | FileType::Speex
    )
}

// Vorbis-style R128 gain is a signed Q7.8 dB adjustment to the -23 LUFS target.
fn r128_integrated_lufs(tag: Option<&Tag>, key: ItemKey) -> Option<f64> {
    let gain_q8 = tag?.get_string(key)?.trim().parse::<i32>().ok()?;
    let integrated_lufs = -23.0 - f64::from(gain_q8) / 256.0;
    integrated_lufs.is_finite().then_some(integrated_lufs)
}

fn replay_gain_db(tag: Option<&Tag>, key: ItemKey) -> Option<f64> {
    let value = tag?.get_string(key)?.trim();
    let value = value
        .strip_suffix("dB")
        .or_else(|| value.strip_suffix("db"))
        .unwrap_or(value)
        .trim()
        .parse::<f64>()
        .ok()?;
    value.is_finite().then_some(value)
}

fn replay_gain_peak(tag: Option<&Tag>, key: ItemKey) -> Option<f64> {
    let value = tag?.get_string(key)?.trim().parse::<f64>().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn audio_metadata_from_discoverer(
    path: &Path,
    metadata: Option<&discovery::Metadata>,
    local_artwork: Option<LocalImageRef>,
) -> AudioMetadata {
    let basic = basic_audio_metadata_from_discoverer(path, metadata);
    let artist = &basic.artist;
    let album_artist = metadata
        .and_then(|metadata| metadata.album_artist.clone())
        .unwrap_or_else(|| artist.clone());
    let artist_names = split_names(artist);
    let artist_mbids = aligned_mbids(
        &artist_names,
        metadata
            .map(|metadata| metadata.artist_mbids.clone())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| clean_mbid(&value))
            .collect(),
    );
    let artists = artist_names
        .into_iter()
        .zip(artist_mbids)
        .map(|(name, musicbrainz_id)| MetadataArtist {
            name,
            musicbrainz_id,
        })
        .collect();
    let album_artist_names = split_names(&album_artist);
    let album_artist_mbids = aligned_mbids(
        &album_artist_names,
        metadata
            .map(|metadata| metadata.album_artist_mbids.clone())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| clean_mbid(&value))
            .collect(),
    );
    let album_artists = album_artist_names
        .into_iter()
        .zip(album_artist_mbids)
        .map(|(name, musicbrainz_id)| MetadataArtist {
            name,
            musicbrainz_id,
        })
        .collect();
    AudioMetadata {
        basic,
        album_artist,
        artists,
        album_artists,
        genres: metadata
            .map(|metadata| metadata.genres.clone())
            .unwrap_or_default(),
        moods: metadata
            .map(|metadata| metadata.moods.clone())
            .unwrap_or_default(),
        year: metadata
            .and_then(|metadata| metadata.year)
            .unwrap_or_default(),
        comment: metadata.and_then(|metadata| metadata.comment.clone()),
        bpm: metadata.and_then(|metadata| metadata.bpm),
        musicbrainz_album_id: metadata
            .and_then(|metadata| metadata.musicbrainz_album_id.as_deref())
            .and_then(clean_mbid),
        musicbrainz_release_group_id: metadata
            .and_then(|metadata| metadata.musicbrainz_release_group_id.as_deref())
            .and_then(clean_mbid),
        musicbrainz_recording_id: metadata
            .and_then(|metadata| metadata.musicbrainz_recording_id.as_deref())
            .and_then(clean_mbid),
        musicbrainz_release_track_id: metadata
            .and_then(|metadata| metadata.musicbrainz_release_track_id.as_deref())
            .and_then(clean_mbid),
        release_types: normalize_release_types(
            metadata
                .map(|metadata| metadata.release_types.clone())
                .unwrap_or_default(),
        ),
        is_compilation: metadata.and_then(|metadata| metadata.is_compilation),
        local_artwork,
        source_format: metadata
            .and_then(|metadata| metadata.source_format.clone())
            .or_else(|| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string)
            }),
        user_rating: None,
        track_r128_lufs: None,
        album_r128_lufs: None,
        replay_gain_track_db: None,
        replay_gain_track_peak: None,
        replay_gain_album_db: None,
        replay_gain_album_peak: None,
    }
}

fn scanned_track(path: &Path, metadata: AudioMetadata) -> ScannedTrack {
    let AudioMetadata {
        basic,
        album_artist,
        artists,
        album_artists,
        genres,
        moods,
        year,
        comment,
        bpm,
        musicbrainz_album_id,
        musicbrainz_release_group_id,
        musicbrainz_recording_id,
        musicbrainz_release_track_id,
        release_types,
        is_compilation,
        local_artwork,
        source_format,
        user_rating,
        track_r128_lufs,
        album_r128_lufs,
        replay_gain_track_db,
        replay_gain_track_peak,
        replay_gain_album_db,
        replay_gain_album_peak,
    } = metadata;
    let BasicAudioMetadata {
        title,
        album,
        artist,
        disc_number,
        track_number,
        duration_seconds,
    } = basic;
    let artists = artists
        .into_iter()
        .map(|artist| artist_credit(&artist.name, artist.musicbrainz_id.as_deref()))
        .collect();
    let album_artists = album_artists
        .into_iter()
        .map(|artist| artist_credit(&artist.name, artist.musicbrainz_id.as_deref()))
        .collect::<Vec<_>>();
    let genres = genres
        .into_iter()
        .map(|name| NamedCredit {
            id: local_id("genre", name.trim()),
            name,
        })
        .collect::<Vec<_>>();
    let moods = moods
        .into_iter()
        .map(|name| NamedCredit {
            id: local_id("mood", name.trim()),
            name,
        })
        .collect::<Vec<_>>();
    let path_text = path.to_string_lossy().into_owned();
    let album_id = album_id(
        &album_artists,
        &album,
        musicbrainz_album_id.as_deref(),
        None,
    );
    ScannedTrack {
        id: track_id(path),
        album_id,
        title,
        artist,
        album,
        year,
        duration_seconds,
        disc_number,
        track_number,
        local_artwork,
        musicbrainz_recording_id,
        musicbrainz_release_track_id,
        source_path: path_text,
        cue_path: None,
        cue_start_millis: None,
        cue_end_millis: None,
        source_format,
        comment,
        bpm,
        user_rating,
        artists,
        album_artists,
        genres,
        moods,
        album_artist,
        release_types,
        is_compilation,
        musicbrainz_album_id,
        musicbrainz_release_group_id,
        track_r128_lufs,
        album_r128_lufs,
        replay_gain_track_db,
        replay_gain_track_peak,
        replay_gain_album_db,
        replay_gain_album_peak,
    }
}

fn basic_audio_metadata(
    path: &Path,
    tag: Option<&Tag>,
    duration_seconds: u32,
) -> BasicAudioMetadata {
    let fallback_title = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Unknown Title")
        .to_string();
    let fallback_album = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("Unknown Album")
        .to_string();
    BasicAudioMetadata {
        title: tag_string(tag, |tag| tag.title().map(|value| value.to_string()))
            .unwrap_or(fallback_title),
        album: tag_string(tag, |tag| tag.album().map(|value| value.to_string()))
            .unwrap_or(fallback_album),
        artist: tag_string(tag, |tag| tag.artist().map(|value| value.to_string()))
            .unwrap_or_else(|| "Unknown Artist".to_string()),
        disc_number: tag
            .and_then(|tag| tag.disk())
            .unwrap_or(1)
            .min(u32::from(u16::MAX)) as u16,
        track_number: tag
            .and_then(|tag| tag.track())
            .unwrap_or_default()
            .min(u32::from(u16::MAX)) as u16,
        duration_seconds,
    }
}

fn basic_audio_metadata_from_discoverer(
    path: &Path,
    metadata: Option<&discovery::Metadata>,
) -> BasicAudioMetadata {
    let fallback_title = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Unknown Title")
        .to_string();
    let fallback_album = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("Unknown Album")
        .to_string();
    BasicAudioMetadata {
        title: metadata
            .and_then(|metadata| metadata.title.clone())
            .unwrap_or(fallback_title),
        album: metadata
            .and_then(|metadata| metadata.album.clone())
            .unwrap_or(fallback_album),
        artist: metadata
            .and_then(|metadata| metadata.artist.clone())
            .unwrap_or_else(|| "Unknown Artist".to_string()),
        disc_number: metadata
            .and_then(|metadata| metadata.disc_number)
            .unwrap_or(1),
        track_number: metadata
            .and_then(|metadata| metadata.track_number)
            .unwrap_or_default(),
        duration_seconds: metadata
            .map(|metadata| metadata.duration_seconds)
            .unwrap_or_default(),
    }
}

pub(super) fn track_id(path: &Path) -> String {
    local_id("track", &path.to_string_lossy())
}

pub(super) fn cue_track_id(cue_path: &Path, track_number: u16) -> String {
    local_id(
        "track",
        &format!("{}:{track_number}", cue_path.to_string_lossy()),
    )
}

pub(super) fn album_id(
    album_artists: &[ArtistCredit],
    album: &str,
    musicbrainz_album_id: Option<&str>,
    cue_path: Option<&Path>,
) -> String {
    let credits = album_artists
        .iter()
        .map(|credit| credit.id.as_str())
        .collect::<Vec<_>>()
        .join("\u{1f}");
    let name = normalized_identity(album);
    let identity = if let Some(cue_path) = cue_path {
        format!(
            "{credits}:{name}:cue:{}:{}",
            cue_path.to_string_lossy(),
            musicbrainz_album_id.unwrap_or_default()
        )
    } else if let Some(musicbrainz_album_id) = musicbrainz_album_id {
        format!("musicbrainz:{musicbrainz_album_id}")
    } else {
        format!("{credits}:{name}")
    };
    local_id("album", &identity)
}

pub(super) fn artist_credit(name: &str, musicbrainz_artist_id: Option<&str>) -> ArtistCredit {
    let musicbrainz_artist_id = musicbrainz_artist_id.and_then(clean_mbid);
    let id = musicbrainz_artist_id
        .as_deref()
        .map(|mbid| format!("local:artist:musicbrainz:{mbid}"))
        .unwrap_or_else(|| local_id("artist", &normalized_identity(name)));
    ArtistCredit {
        id,
        name: name.to_string(),
        musicbrainz_artist_id,
    }
}

pub(super) fn split_names(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    for value in value
        .split([';', '/'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        values.push(value.to_string());
    }
    values
}

pub(super) fn local_id(kind: &str, value: &str) -> String {
    format!("local:{kind}:{:016x}", stable_hash(value))
}

fn artist_names(tag: Option<&Tag>, fallback: &str) -> Vec<String> {
    let tagged = tag
        .map(|tag| {
            tag.get_strings(ItemKey::TrackArtists)
                .flat_map(split_names)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let fallback = split_names(fallback);
    if tagged.is_empty()
        || (tagged.len() == 1
            && fallback.len() == 1
            && tagged[0].eq_ignore_ascii_case(&fallback[0]))
    {
        fallback
    } else {
        tagged
    }
}

fn tag_string(tag: Option<&Tag>, read: impl FnOnce(&Tag) -> Option<String>) -> Option<String> {
    tag.and_then(read)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn tag_mbid(tag: &Tag, key: ItemKey) -> Option<String> {
    tag_values(tag, key)
        .into_iter()
        .find_map(|value| clean_mbid(&value))
}

fn tag_mbids(tag: Option<&Tag>, key: ItemKey) -> Vec<String> {
    tag_values_optional(tag, key)
        .into_iter()
        .flat_map(|value| split_names(&value))
        .filter_map(|value| clean_mbid(&value))
        .collect()
}

fn tag_values_optional(tag: Option<&Tag>, key: ItemKey) -> Vec<String> {
    tag.map(|tag| tag_values(tag, key)).unwrap_or_default()
}

fn tag_values(tag: &Tag, key: ItemKey) -> Vec<String> {
    tag.get_items(key)
        .filter_map(|item| item.value().text().map(ToString::to_string))
        .collect()
}

fn album_release_types(tag: Option<&Tag>) -> Vec<String> {
    let mut values = Vec::new();
    for value in tag_values_optional(tag, ItemKey::MusicBrainzReleaseType) {
        values.extend(
            value
                .split([';', '\0'])
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
        );
    }
    normalize_release_types(values)
}

fn normalize_release_types(values: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim().to_lowercase();
        if !value.is_empty() && !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    normalized
}

fn album_compilation(tag: Option<&Tag>, release_types: &[String]) -> Option<bool> {
    let mut explicit_true = false;
    let mut explicit_false = false;
    for value in tag_values_optional(tag, ItemKey::FlagCompilation) {
        match value.trim() {
            "1" => explicit_true = true,
            "0" => explicit_false = true,
            _ => {}
        }
    }
    if explicit_true || release_types.iter().any(|value| value == "compilation") {
        Some(true)
    } else if explicit_false || !release_types.is_empty() {
        Some(false)
    } else {
        None
    }
}

fn aligned_mbids(names: &[String], mbids: Vec<String>) -> Vec<Option<String>> {
    if names.len() == mbids.len() {
        mbids.into_iter().map(Some).collect()
    } else {
        names.iter().map(|_| None).collect()
    }
}

fn tag_bpm(tag: Option<&Tag>) -> Option<u16> {
    tag_values_optional(tag, ItemKey::IntegerBpm)
        .into_iter()
        .chain(tag_values_optional(tag, ItemKey::Bpm))
        .find_map(|value| {
            let rounded = value.trim().parse::<f64>().ok()?.round();
            (1.0..=f64::from(u16::MAX))
                .contains(&rounded)
                .then_some(rounded as u16)
        })
}

fn normalized_identity(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn clean_mbid(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
    .then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use lofty::config::WriteOptions;
    use lofty::tag::TagType;

    use super::*;

    #[test]
    fn executable_contents_are_not_local_audio() {
        let path = std::env::current_exe().expect("test executable");
        assert!(matches!(
            read_media(&mut Worker::default(), path, None),
            MediaRead::Rejected
        ));
    }

    #[test]
    fn mpeg_content_keeps_metadata_with_arbitrary_suffixes_and_leading_junk() {
        let directory = tempfile::tempdir().expect("audio directory");
        for (name, prefixed) in [
            ("track.mp3", false),
            ("track.bin", false),
            ("prefixed.bin", true),
        ] {
            let path = directory.path().join(name);
            let mut frame = vec![0; 417];
            frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
            fs::write(&path, frame.repeat(40)).expect("MPEG silence");
            let mut tag = Tag::new(TagType::Id3v2);
            tag.set_title("Original".into());
            tag.set_comment("Retained comment".into());
            tag.save_to_path(&path, WriteOptions::new().preferred_padding(0))
                .expect("audio tags");
            if prefixed {
                let mut bytes = vec![0; 32];
                bytes.extend(fs::read(&path).expect("tagged audio"));
                fs::write(&path, bytes).expect("leading junk");
            }
            let MediaRead::Accepted(track) = read_media(&mut Worker::default(), path.clone(), None)
            else {
                panic!("audio content was rejected: {name}");
            };
            assert_eq!(track.title, "Original", "{name}");
            assert_eq!(track.comment.as_deref(), Some("Retained comment"), "{name}");
            let mut metadata =
                super::super::read_track_metadata(&path, Some("mp3")).expect("read audio metadata");
            assert!(metadata.writable.title, "{name}");
            metadata.values.title = "Updated".into();
            super::super::metadata::write_track(
                &path,
                Some("mp3"),
                metadata.revision.as_deref().expect("file revision"),
                &crate::TrackMetadataEdit {
                    values: metadata.values,
                    changed: crate::TrackMetadataWritable {
                        title: true,
                        ..Default::default()
                    },
                },
            )
            .expect("write audio metadata");
            let updated = super::super::read_track_metadata(&path, Some("mp3"))
                .expect("read updated audio metadata");
            assert_eq!(updated.values.title, "Updated", "{name}");
            assert_eq!(
                updated.values.comment.as_deref(),
                Some("Retained comment"),
                "{name}"
            );
        }
    }

    #[test]
    fn local_loudness_parses_replay_gain_and_r128_independently() {
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::ReplayGainTrackGain, "-4.25 dB".to_string());
        tag.insert_text(ItemKey::ReplayGainTrackPeak, "0.91".to_string());
        tag.insert_text(ItemKey::R128TrackGain, "-512".to_string());

        assert_eq!(
            replay_gain_db(Some(&tag), ItemKey::ReplayGainTrackGain),
            Some(-4.25)
        );
        assert_eq!(
            replay_gain_peak(Some(&tag), ItemKey::ReplayGainTrackPeak),
            Some(0.91)
        );
        assert_eq!(
            r128_integrated_lufs(Some(&tag), ItemKey::R128TrackGain),
            Some(-21.0)
        );
    }
}
