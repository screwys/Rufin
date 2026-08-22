//! One Local media candidate: GStreamer admission followed by optional Lofty enrichment.

use std::fs;
use std::path::{Path, PathBuf};

use library::{
    AlbumId, ArtistCredit, ArtistId, GenreCredit, LocalArtworkRef, MoodCredit, Track, TrackData,
    TrackId, TrackRelations,
};
use lofty::file::TaggedFileExt;
use lofty::prelude::*;
use lofty::tag::{ItemKey, Tag};

use crate::policy::stable_hash;

use super::artwork;
use super::discovery;
use super::lofty_metadata::{read_lofty, source_format};

#[derive(Clone, Debug)]
pub(super) struct ScannedTrack {
    pub(super) track: Track,
    pub(super) album_artist: String,
    pub(super) release_types: Vec<String>,
    pub(super) is_compilation: Option<bool>,
    pub(super) musicbrainz_album_id: Option<String>,
    pub(super) musicbrainz_release_group_id: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) enum MediaRead {
    Accepted(ScannedTrack),
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
    local_artwork: Option<LocalArtworkRef>,
    source_format: Option<String>,
    user_rating: Option<u8>,
}

#[derive(Default)]
pub(super) struct Worker {
    discovery: discovery::Reader,
}

/// Reads only the fields used to match a remote track to a local file.
///
/// Local-access discovery deliberately skips pictures, MusicBrainz fields,
/// relationships, and Rufin identities. A readable file with invalid metadata
/// still has a useful filename-based match candidate.
pub(super) fn read_basic_audio(worker: &mut Worker, path: PathBuf) -> Option<BasicAudioMetadata> {
    fs::File::open(&path).ok()?;
    let discovered = worker.discovery.read(&path)?;
    let tagged_file = read_lofty(&path, false).ok().flatten();
    if tagged_file.is_none() {
        return Some(basic_audio_metadata_from_discoverer(
            &path,
            Some(&discovered),
        ));
    }
    let tag = tagged_file
        .as_ref()
        .and_then(|file| file.primary_tag().or_else(|| file.first_tag()));
    let metadata_duration_seconds = tagged_file
        .as_ref()
        .map(|file| {
            file.properties()
                .duration()
                .as_secs()
                .min(u64::from(u32::MAX)) as u32
        })
        .unwrap_or_default();
    let duration_seconds =
        playback_duration_seconds(discovered.duration_seconds, metadata_duration_seconds);
    Some(basic_audio_metadata(&path, tag, duration_seconds))
}

pub(super) fn read_media(
    worker: &mut Worker,
    path: PathBuf,
    sidecar: Option<LocalArtworkRef>,
    revision: String,
) -> MediaRead {
    if fs::File::open(&path).is_err() {
        return MediaRead::Unreadable;
    }

    let Some(discovered) = worker.discovery.read(&path) else {
        return MediaRead::Rejected;
    };
    let tagged_file = read_lofty(&path, false).ok().flatten();
    let local_artwork = sidecar
        .or_else(|| embedded_artwork(&path, revision.clone()))
        .or_else(|| {
            discovered
                .artwork_index
                .map(|picture_index| artwork::embedded_reference(&path, picture_index, revision))
        });
    let metadata = if tagged_file.is_some() {
        let meta = audio_metadata_from_lofty(
            &path,
            tagged_file.as_ref(),
            discovered.duration_seconds,
            local_artwork,
        );
        meta
    } else {
        audio_metadata_from_discoverer(&path, Some(&discovered), local_artwork)
    };
    MediaRead::Accepted(scanned_track(&path, metadata))
}

fn audio_metadata_from_lofty(
    path: &Path,
    tagged_file: Option<&lofty::file::TaggedFile>,
    discovered_duration_seconds: u32,
    local_artwork: Option<LocalArtworkRef>,
) -> AudioMetadata {
    let tag = tagged_file.and_then(|file| file.primary_tag().or_else(|| file.first_tag()));
    let metadata_duration_seconds = tagged_file
        .map(|file| {
            file.properties()
                .duration()
                .as_secs()
                .min(u64::from(u32::MAX)) as u32
        })
        .unwrap_or_default();
    let duration_seconds =
        playback_duration_seconds(discovered_duration_seconds, metadata_duration_seconds);
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
        source_format: tagged_file.and_then(|file| source_format(path, file.file_type())),
        user_rating: tag
            .and_then(|tag| tag.ratings().next())
            .map(|rating| (rating.rating() as u8) * 2),
    }
}

fn playback_duration_seconds(discovered: u32, metadata: u32) -> u32 {
    if discovered == 0 {
        metadata
    } else {
        discovered
    }
}

fn audio_metadata_from_discoverer(
    path: &Path,
    metadata: Option<&discovery::Metadata>,
    local_artwork: Option<LocalArtworkRef>,
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
        release_types: library::normalize_release_types(
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
        .map(|name| GenreCredit {
            id: local_id("genre", name.trim()),
            name,
        })
        .collect::<Vec<_>>();
    let moods = moods
        .into_iter()
        .map(|name| MoodCredit {
            id: library::MoodId::new(name.trim().to_string()),
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
        track: Track::new(TrackData {
            id: track_id(path),
            album_id: Some(album_id),
            title,
            artist,
            album,
            album_artwork: None,
            year,
            release_date: None,
            date_added: None,
            last_played: None,
            play_count: None,
            user_rating,
            duration_seconds,
            favorite: false,
            disc_number,
            track_number,
            image_ref: None,
            local_artwork,
            musicbrainz_recording_id,
            musicbrainz_release_track_id,
            source_path: Some(path_text),
            cue: None,
            source_format,
            comment,
            skip_count: None,
            bpm,
            relations: TrackRelations {
                artists,
                album_artists,
                genres,
                moods,
                music_folders: Vec::new(),
            },
        }),
        album_artist,
        release_types,
        is_compilation,
        musicbrainz_album_id,
        musicbrainz_release_group_id,
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

fn embedded_artwork(path: &Path, revision: String) -> Option<LocalArtworkRef> {
    let file = read_lofty(path, true).ok().flatten()?;
    let tag = file.primary_tag().or_else(|| file.first_tag());
    let picture_index = artwork::best_picture_index(&file, tag)?;
    Some(artwork::embedded_reference(path, picture_index, revision))
}

pub(super) fn track_id(path: &Path) -> TrackId {
    local_id("track", &path.to_string_lossy())
}

pub(super) fn cue_track_id(cue_path: &Path, track_number: u16) -> TrackId {
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
) -> AlbumId {
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
        .map(|mbid| ArtistId::new(format!("local:artist:musicbrainz:{mbid}")))
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

pub(super) fn local_id<T>(kind: &str, value: &str) -> T
where
    T: From<String>,
{
    T::from(format!("local:{kind}:{:016x}", stable_hash(value)))
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
    library::normalize_release_types(values)
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
    use super::*;

    #[test]
    fn gstreamer_duration_owns_the_playback_length() {
        assert_eq!(playback_duration_seconds(190, 0), 190);
        assert_eq!(playback_duration_seconds(190, 191), 190);
        assert_eq!(playback_duration_seconds(0, 191), 191);
    }

    #[test]
    fn discovered_metadata_preserves_its_embedded_artwork_locator() {
        let metadata = discovery::Metadata {
            title: Some("Track".to_string()),
            artist: Some("Artist".to_string()),
            artwork_index: Some(2),
            ..discovery::Metadata::default()
        };
        let local_artwork = metadata.artwork_index.map(|picture_index| {
            artwork::embedded_reference(
                Path::new("/Music/Album/Track.wma"),
                picture_index,
                "file-revision".to_string(),
            )
        });
        let read = MediaRead::Accepted(scanned_track(
            Path::new("/Music/Album/Track.wma"),
            audio_metadata_from_discoverer(
                Path::new("/Music/Album/Track.wma"),
                Some(&metadata),
                local_artwork,
            ),
        ));

        let MediaRead::Accepted(scanned) = read else {
            panic!("accepted media")
        };
        assert_eq!(
            scanned.track.local_artwork,
            Some(LocalArtworkRef::Embedded {
                path: "/Music/Album/Track.wma".to_string(),
                picture_index: 2,
                revision: "file-revision".to_string(),
            })
        );
    }
}
