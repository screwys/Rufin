//! One Local media candidate: Lofty metadata with GStreamer fallback.

use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use crate::LocalImageRef;
use lofty::config::ParseOptions;
use lofty::file::TaggedFileExt;
use lofty::prelude::*;

use crate::policy::{date_year, stable_hash};

use super::discovery;
use super::lofty::{read_lofty_scan, source_format};
use super::tags::{self, RawTags, Tags};

#[derive(Clone, Debug)]
pub(crate) struct ScannedTrack {
    pub(crate) track: library::TrackObservation,
    pub(crate) local_artwork: Option<LocalImageRef>,
    // None is uninspected; -1 records an inspected file without a cover.
    pub(crate) picture_index: Option<i64>,
    pub(crate) artist_pictures: Vec<super::artwork::ArtistPicture>,
    pub(crate) audio_revision: blake3::Hasher,
    /// The revision stands for the audio hash: the file is not read whole to hash it.
    pub(crate) revision_hashes_audio: bool,
    /// The MD5 of the decoded audio a FLAC STREAMINFO block records.
    pub(crate) flac_md5: Option<[u8; 16]>,
    pub(crate) lyrics: Option<lyrics::LyricsBundle>,
}

impl ScannedTrack {
    pub(crate) fn observe_images(
        &self,
        file: &mut library::LocalFileWrite,
    ) -> crate::SourceResult<()> {
        file.picture_index = self.picture_index;
        file.artist_pictures = self
            .picture_index
            .map(|_| serde_json::to_string(&self.artist_pictures))
            .transpose()?;
        Ok(())
    }

    pub(crate) fn into_observation(mut self, accepted_at: i64) -> library::TrackObservation {
        let track = &mut self.track;
        track.source_lyrics = self
            .lyrics
            .map(|lyrics| lyrics.cache_payload().expect("file lyrics payload"));
        track.artwork_binding = self
            .local_artwork
            .as_ref()
            .map(LocalImageRef::artwork_binding);
        track.first_seen_at = Some(accepted_at);
        self.audio_revision
            .update(track.cue_path.as_deref().unwrap_or_default().as_bytes());
        self.audio_revision
            .update(&track.cue_start_millis.unwrap_or(-1).to_le_bytes());
        self.audio_revision
            .update(&track.cue_end_millis.unwrap_or(-1).to_le_bytes());
        let revision = *self.audio_revision.finalize().as_bytes();
        track.audio_revision = Some(revision);
        track.audio_hash = match self.flac_md5 {
            // The audio hash survives tag edits: it is the decoded audio's, cut to the window.
            Some(md5) => {
                let mut hash = blake3::Hasher::new();
                hash.update(b"flac-md5");
                hash.update(&md5);
                hash.update(&track.cue_start_millis.unwrap_or(-1).to_le_bytes());
                hash.update(&track.cue_end_millis.unwrap_or(-1).to_le_bytes());
                Some(*hash.finalize().as_bytes())
            }
            None => self.revision_hashes_audio.then_some(revision),
        };
        self.track
    }
}

#[derive(Clone, Debug)]
pub(crate) enum MediaRead {
    Accepted(Box<ScannedTrack>),
    Rejected,
    Unreadable,
    /// An online-only cloud file, which a read would download: it is not opened, and the
    /// Store keeps what it holds of it.
    Deferred,
}

#[derive(Default)]
pub(crate) struct Worker {
    pub(crate) discovery: discovery::Reader,
}

pub(crate) fn track_reference(scanned: ScannedTrack, media_uri: String) -> library::TrackReference {
    library::TrackReference {
        media_uri,
        metadata: scanned.track.metadata,
        artwork_binding: scanned
            .local_artwork
            .as_ref()
            .map(LocalImageRef::artwork_binding),
    }
}

pub(crate) async fn read_saved_media(
    path: &Path,
    display_path: PathBuf,
    root: Option<PathBuf>,
) -> crate::SourceResult<ScannedTrack> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&path)
            .map_err(|error| crate::SourceError::Other(error.to_string()))?;
        let uri = url::Url::from_file_path(&path).map_err(|_| crate::SourceError::NotFound)?;
        let mut worker = Worker::default();
        let MediaRead::Accepted(track) = read_media_input(
            &mut worker,
            display_path,
            root.as_deref(),
            &mut file,
            uri.as_str(),
            None,
        ) else {
            return Err(crate::SourceError::Other(
                "Saved media could not be read".into(),
            ));
        };
        Ok(*track)
    })
    .await
    .map_err(|error| crate::SourceError::Other(error.to_string()))?
}

pub(crate) fn excluded_from_audio_scan(path: &Path) -> bool {
    super::metadata::is_working_copy(path)
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                [
                    "tmp", "tar", "gz", "bz2", "xz", "tbz", "tgz", "z", "zip", "rar", "wvc", "zst",
                    "lrc",
                ]
                .iter()
                .any(|excluded| extension.eq_ignore_ascii_case(excluded))
            })
        || path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.split('.').next() == Some("qt_temp"))
}

/// `root` is the source folder the file was found in, when the caller knows it.
pub(crate) fn read_media_input(
    worker: &mut Worker,
    path: PathBuf,
    root: Option<&Path>,
    file: &mut (impl Read + Seek),
    uri: &str,
    sidecar: Option<LocalImageRef>,
) -> MediaRead {
    // Mach-O programs can contain audio bytes that decoder typefinding mistakes for a track.
    let mut signature = [0; 4];
    if file
        .rewind()
        .and_then(|()| file.read_exact(&mut signature))
        .is_ok()
        && matches!(
            u32::from_be_bytes(signature),
            0xfeed_face
                | 0xcefa_edfe
                | 0xfeed_facf
                | 0xcffa_edfe
                | 0xcafe_babe
                | 0xbeba_feca
                | 0xcafe_babf
                | 0xbfba_feca
        )
    {
        return MediaRead::Rejected;
    }
    let md5 = flac_md5(file);
    if file.rewind().is_err() {
        return MediaRead::Unreadable;
    }
    // Recognize a frame sync at the start or immediately after ID3 metadata;
    // searching binary interiors can mistake executable bytes for MPEG audio.
    // Optional picture data must not discard usable audio and tags.
    let mut pictures_read = true;
    let mut read = read_lofty_scan(
        &mut *file,
        ParseOptions::new().read_cover_art(true).max_junk_bytes(2),
    )
    .or_else(|_| {
        pictures_read = false;
        file.rewind()?;
        read_lofty_scan(
            &mut *file,
            ParseOptions::new().read_cover_art(false).max_junk_bytes(2),
        )
    })
    .ok()
    .flatten()
    .filter(|(file, _)| lofty_supplies_required_audio(file));
    let discovered = if read.is_none() {
        match worker.discovery.read_input(file, uri) {
            Ok(Some(metadata)) => Some(metadata),
            Ok(None) => return MediaRead::Rejected,
            Err(error) => {
                tracing::warn!(?path, %error, "Media discovery failed");
                return MediaRead::Unreadable;
            }
        }
    } else {
        None
    };
    if read.is_none() {
        // Discovery has confirmed audio, so retain Lofty's tag recovery for
        // playable files whose frames or ID3 metadata follow leading junk.
        if file.rewind().is_err() {
            return MediaRead::Unreadable;
        }
        pictures_read = true;
        read = read_lofty_scan(&mut *file, ParseOptions::new().read_cover_art(true))
            .or_else(|_| {
                pictures_read = false;
                file.rewind()?;
                read_lofty_scan(&mut *file, ParseOptions::new().read_cover_art(false))
            })
            .ok()
            .flatten()
            .filter(|(file, _)| lofty_supplies_required_audio(file));
    }
    let (tagged_file, raw) = read.unzip();
    let folder = folder_album(&path, root);
    let mut track = if let Some(tagged_file) = tagged_file.as_ref() {
        let properties = tagged_file.properties();
        let facts = Facts {
            duration_seconds: lofty_duration_seconds(tagged_file),
            source_format: source_format(&path, tagged_file.file_type()),
            audio_properties: library::AudioProperties {
                bitrate_kbps: properties.audio_bitrate(),
                sample_rate_hz: properties.sample_rate(),
                bit_depth: properties.bit_depth().map(u32::from),
                channels: properties.channels().map(u32::from),
                size_bytes: None,
            },
            user_rating: tagged_file
                .primary_tag()
                .or_else(|| tagged_file.first_tag())
                .and_then(|tag| tag.ratings().next())
                .map(|rating| (rating.rating() as u8) * 2),
        };
        let tags = Tags::clean(&raw.unwrap_or_default());
        tagged_track(&path, folder, &tags, facts, sidecar)
    } else {
        let metadata = discovered.clone().unwrap_or_default();
        let facts = Facts {
            duration_seconds: metadata.duration_seconds,
            source_format: metadata.source_format.clone().or_else(|| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string)
            }),
            audio_properties: metadata.audio_properties.clone(),
            user_rating: None,
        };
        let tags = Tags::clean(&discovered_tags(&metadata));
        tagged_track(&path, folder, &tags, facts, sidecar)
    };
    super::lyrics::set_lyrics(
        &mut track,
        tagged_file
            .as_ref()
            .and_then(super::metadata::embedded_lyrics)
            .and_then(|text| lyrics::lyrics_from_edited_text(&text)),
    );
    if let Some(tagged) = tagged_file.as_ref().filter(|_| pictures_read) {
        track.picture_index = Some(
            super::artwork::best_picture_index(
                tagged,
                tagged.primary_tag().or_else(|| tagged.first_tag()),
            )
            .map_or(-1, i64::from),
        );
        track.artist_pictures = super::artwork::artist_pictures(tagged);
    } else if let Some(discovered) = discovered {
        track.picture_index = discovered
            .artwork_index
            .map(i64::from)
            .or_else(|| tagged_file.is_none().then_some(-1));
    }
    track.flac_md5 = md5;
    MediaRead::Accepted(Box::new(track))
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

// Vorbis-style R128 gain is a signed Q7.8 dB adjustment to the -23 LUFS target.
/// What a read knows of a file beside its tags.
struct Facts {
    duration_seconds: u32,
    source_format: Option<String>,
    audio_properties: library::AudioProperties,
    user_rating: Option<u8>,
}

/// The names of a credit, each once: those in the values of its plural tag, else in those of
/// its singular one.
fn artist_names(tags: &Tags, single: &str, plural: &str) -> Vec<String> {
    let values = match tags.all(plural) {
        [] => tags.all(single),
        values => values,
    };
    let mut names = Vec::<String>::new();
    for name in values.iter().flat_map(|value| split_names(value)) {
        if !names.iter().any(|known| known.eq_ignore_ascii_case(&name)) {
            names.push(name);
        }
    }
    names
}

/// A credit as it shows: its singular tag's values, else its plural one's.
fn display_names(tags: &Tags, single: &str, plural: &str) -> Option<String> {
    [single, plural]
        .into_iter()
        .map(|name| tags.all(name))
        .find(|values| !values.is_empty())
        .map(|values| values.join("; "))
}

/// The number a track or disc tag leads with, as `3` and `3/12` do.
fn tag_number(value: Option<&str>) -> Option<i64> {
    let digits = value?
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits
        .parse::<u32>()
        .ok()
        .map(|number| i64::from(number.min(u32::from(u16::MAX))))
}

fn tag_bpm(value: Option<&str>) -> Option<i64> {
    let rounded = value?.trim().parse::<f64>().ok()?.round();
    (1.0..=f64::from(u16::MAX))
        .contains(&rounded)
        .then_some(rounded as i64)
}

/// Whether the album is a compilation: its flag, else what its release types say.
fn album_compilation(flag: Option<&str>, release_types: &[String]) -> Option<bool> {
    let flag = flag.map(str::trim);
    let explicit_true = flag.is_some_and(|flag| flag == "1" || flag.eq_ignore_ascii_case("true"));
    let explicit_false = flag.is_some_and(|flag| flag == "0" || flag.eq_ignore_ascii_case("false"));
    if explicit_true || release_types.iter().any(|value| value == "compilation") {
        Some(true)
    } else if explicit_false || !release_types.is_empty() {
        Some(false)
    } else {
        None
    }
}

fn r128_integrated_lufs(value: Option<&str>) -> Option<f64> {
    let gain_q8 = value?.trim().parse::<i32>().ok()?;
    let integrated_lufs = -23.0 - f64::from(gain_q8) / 256.0;
    integrated_lufs.is_finite().then_some(integrated_lufs)
}

fn replay_gain_db(value: Option<&str>) -> Option<f64> {
    let value = value?.trim();
    let value = value
        .strip_suffix("dB")
        .or_else(|| value.strip_suffix("db"))
        .unwrap_or(value)
        .trim()
        .parse::<f64>()
        .ok()?;
    value.is_finite().then_some(value)
}

fn replay_gain_peak(value: Option<&str>) -> Option<f64> {
    let value = value?.trim().parse::<f64>().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

/// A file's track from its cleaned tags. A file without an album artist takes its own artists.
fn tagged_track(
    path: &Path,
    folder: String,
    tags: &Tags,
    facts: Facts,
    local_artwork: Option<LocalImageRef>,
) -> ScannedTrack {
    let title = tags
        .first("title")
        .map_or_else(|| file_title(path), str::to_owned);
    let album = tags.first("album").map_or(folder, str::to_owned);
    let artist =
        display_names(tags, "artist", "artists").unwrap_or_else(|| "Unknown Artist".to_string());
    let mut names = artist_names(tags, "artist", "artists");
    if names.is_empty() {
        names.push(artist.clone());
    }
    let artists = credits(
        &names,
        tags.all("musicbrainz_artistid").to_vec(),
        artist_names(tags, "artistsort", "artistssort"),
        &[],
    );
    let tagged_album_artist = display_names(tags, "albumartist", "albumartists");
    let album_artist_tagged = tagged_album_artist.is_some();
    let album_names = if album_artist_tagged {
        artist_names(tags, "albumartist", "albumartists")
    } else {
        names
    };
    let album_artists = credits(
        &album_names,
        tags.all("musicbrainz_albumartistid").to_vec(),
        artist_names(tags, "albumartistsort", "albumartistssort"),
        &artists,
    );
    let album_artist = tagged_album_artist.unwrap_or_else(|| artist.clone());
    let release_types = library::normalize_release_types(
        tags.all("releasetype")
            .iter()
            .flat_map(|value| value.split(';')),
    );
    let is_compilation = album_compilation(tags.first("compilation"), &release_types);
    let release_date = ["releasedate", "recordingdate", "originaldate", "year"]
        .into_iter()
        .find_map(|name| tags.first(name))
        .map(str::to_owned);
    let mbid = |name: &str| tags.first(name).map(str::to_owned);
    let metadata = library::TrackMetadata {
        album_tags: Some(library::AlbumTags {
            sort_text: tags.first("albumsort").unwrap_or(&album).to_lowercase(),
            is_compilation,
            release_types,
            version: tags.first("albumversion").map(str::to_owned),
            disc_subtitle: tags.first("discsubtitle").map(str::to_owned),
        }),
        groupings: tags.all("grouping").to_vec(),
        sort_text: tags.first("titlesort").unwrap_or(&title).to_lowercase(),
        title,
        artist,
        album,
        album_display_artist: Some(album_artist),
        year: release_date.as_deref().and_then(date_year).map(i64::from),
        release_date,
        duration_millis: i64::from(facts.duration_seconds) * 1_000,
        disc_number: tag_number(tags.first("disc"))
            .filter(|disc| *disc > 0)
            .unwrap_or(1),
        track_number: tag_number(tags.first("track")).unwrap_or_default(),
        musicbrainz_recording_id: mbid("musicbrainz_recordingid"),
        musicbrainz_release_track_id: mbid("musicbrainz_trackid"),
        musicbrainz_album_id: mbid("musicbrainz_albumid"),
        musicbrainz_release_group_id: mbid("musicbrainz_releasegroupid"),
        source_format: facts.source_format,
        comment: tags.first("comment").map(str::to_owned),
        bpm: tag_bpm(tags.first("bpm")),
        audio_properties: facts.audio_properties,
        artists,
        album_artists,
        genres: tag_references("genre", tags.all("genre").to_vec()),
        moods: tag_references("mood", tags.all("mood").to_vec()),
        source_loudness: native_gain(
            r128_integrated_lufs(tags.first("r128_track_gain")),
            replay_gain_db(tags.first("replaygain_track_gain")),
            replay_gain_peak(tags.first("replaygain_track_peak")),
        ),
        album_loudness: native_gain(
            r128_integrated_lufs(tags.first("r128_album_gain")),
            replay_gain_db(tags.first("replaygain_album_gain")),
            replay_gain_peak(tags.first("replaygain_album_peak")),
        ),
        ..Default::default()
    };
    let mut track = scanned_track(
        path,
        metadata,
        album_artist_tagged,
        facts.user_rating,
        local_artwork,
    );
    track.track.tags = tags.stored();
    track
}

/// The tags discovery read, under keys the table names them by.
fn discovered_tags(metadata: &discovery::Metadata) -> RawTags {
    let mut raw = RawTags::new();
    let one = [
        ("title", &metadata.title),
        ("album", &metadata.album),
        ("artist", &metadata.artist),
        ("albumartist", &metadata.album_artist),
        ("titlesort", &metadata.sort_title),
        ("albumsort", &metadata.sort_album),
        ("date", &metadata.release_date),
        ("comment", &metadata.comment),
        ("musicbrainz_albumid", &metadata.musicbrainz_album_id),
        (
            "musicbrainz_releasegroupid",
            &metadata.musicbrainz_release_group_id,
        ),
        ("musicbrainz_trackid", &metadata.musicbrainz_recording_id),
        (
            "musicbrainz_releasetrackid",
            &metadata.musicbrainz_release_track_id,
        ),
        ("albumversion", &metadata.album_version),
        ("discsubtitle", &metadata.disc_subtitle),
    ];
    for (key, value) in one {
        if let Some(value) = value {
            tags::push(&mut raw, key, value);
        }
    }
    let many = [
        ("artistsort", &metadata.artist_sort),
        ("albumartistsort", &metadata.album_artist_sort),
        ("musicbrainz_artistid", &metadata.artist_mbids),
        ("musicbrainz_albumartistid", &metadata.album_artist_mbids),
        ("genre", &metadata.genres),
        ("mood", &metadata.moods),
        ("releasetype", &metadata.release_types),
        ("grouping", &metadata.groupings),
    ];
    for (key, values) in many {
        for value in values {
            tags::push(&mut raw, key, value);
        }
    }
    let numbers = [
        ("track", metadata.track_number),
        ("disc", metadata.disc_number),
        ("bpm", metadata.bpm),
        (
            "date",
            metadata.year.filter(|_| metadata.release_date.is_none()),
        ),
    ];
    for (key, number) in numbers {
        if let Some(number) = number.filter(|number| *number > 0) {
            tags::push(&mut raw, key, &number.to_string());
        }
    }
    if let Some(compilation) = metadata.is_compilation {
        tags::push(&mut raw, "compilation", if compilation { "1" } else { "0" });
    }
    raw
}

fn scanned_track(
    path: &Path,
    metadata: library::TrackMetadata,
    album_artist_tagged: bool,
    user_rating: Option<u8>,
    local_artwork: Option<LocalImageRef>,
) -> ScannedTrack {
    let album_id = album_id(&metadata, album_artist_tagged, path, None);
    ScannedTrack {
        track: library::TrackObservation {
            metadata,
            user_data: library::NativeUserState {
                favorite: Some(false),
                rating: Some(user_rating.map(|rating| i64::from(rating) * 10)),
                ..Default::default()
            },
            object_id: track_id(path),
            album_object_id: Some(album_id),
            source_path: Some(path.to_string_lossy().into_owned()),
            ..Default::default()
        },
        local_artwork,
        picture_index: None,
        artist_pictures: Vec::new(),
        audio_revision: blake3::Hasher::new(),
        revision_hashes_audio: false,
        flac_md5: None,
        lyrics: None,
    }
}

/// An album is its MusicBrainz release, or else its tagged album artists with its title and
/// version, wherever its files are. Without an album artist tag its folder stands in for one:
/// the tracks of a compilation stay one album, and same-titled albums in other folders stay
/// apart. A cue sheet's album is its sheet's.
pub(crate) fn album_id(
    metadata: &library::TrackMetadata,
    album_artist_tagged: bool,
    path: &Path,
    cue_path: Option<&Path>,
) -> String {
    let credits = metadata
        .album_artists
        .iter()
        .filter(|_| album_artist_tagged)
        .map(|credit| credit.object_id.as_str())
        .collect::<Vec<_>>()
        .join("\u{1f}");
    let name = match metadata
        .album_tags
        .as_ref()
        .and_then(|tags| tags.version.as_deref())
    {
        Some(version) => normalized_identity(&format!("{} ({version})", metadata.album)),
        None => normalized_identity(&metadata.album),
    };
    let release = metadata.musicbrainz_album_id.as_deref();
    let identity = if let Some(cue_path) = cue_path {
        format!(
            "{credits}:{name}:cue:{}:{}",
            cue_path.to_string_lossy(),
            release.unwrap_or_default()
        )
    } else if let Some(release) = release {
        format!("musicbrainz:{release}")
    } else if album_artist_tagged {
        format!("{credits}:{name}")
    } else {
        format!(":{name}:{}", album_folder(path).to_string_lossy())
    };
    local_id("album", &identity)
}

/// The rest of `text` after the number it starts with: digits, or a number word.
fn after_number(text: &str) -> Option<&str> {
    let rest = text.trim_start_matches(|character: char| character.is_ascii_digit());
    if rest.len() < text.len() {
        return Some(rest);
    }
    [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ]
    .iter()
    .find_map(|word| text.strip_prefix(word))
}

/// The rest of `text` after the disc it starts with, as `cd1`, `disc 2` and `vol. three` do.
fn after_disc(text: &str) -> Option<&str> {
    ["cd", "disc", "disk", "dvd", "volume", "vol"]
        .iter()
        .find_map(|marker| {
            after_number(
                text.strip_prefix(marker)?
                    .trim_start_matches([' ', '-', '_', '.', '#']),
            )
        })
}

/// The folder an album's files share. A folder of one disc is its parent's: one named for the
/// disc, as CD1, Disc 2 - Live and 3 are. A folder named for an album and one of its discs,
/// as Album (Disc 1) and Album [CD2] are, is that album's beside it.
fn album_folder(path: &Path) -> PathBuf {
    let folder = path.parent().unwrap_or(Path::new(""));
    let (Some(name), Some(parent)) = (
        folder.file_name().and_then(|name| name.to_str()),
        folder.parent(),
    ) else {
        return folder.to_owned();
    };
    let name = name.to_lowercase();
    let ends = |rest: &str| !rest.starts_with(|character: char| character.is_alphanumeric());
    if after_disc(&name).is_some_and(ends)
        || (!name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return parent.to_owned();
    }
    for (at, _) in name.char_indices().skip(1) {
        let (Some(stem), Some(disc)) = (name.get(..at), name.get(at..)) else {
            continue;
        };
        let closed = |rest: &str| rest.trim_matches([' ', ')', ']']).is_empty();
        let stem = stem.trim_end_matches([' ', '-', '_', ',', '(', '[']);
        if after_disc(disc).is_some_and(closed)
            && !stem.is_empty()
            && !name
                .get(..at)
                .is_some_and(|before| before.ends_with(char::is_alphanumeric))
        {
            return parent.join(stem);
        }
    }
    folder.to_owned()
}

/// The names in one credit value, empty parts included.
pub(crate) fn name_parts(value: &str) -> impl Iterator<Item = &str> {
    tags::parts(value, &tags::ARTIST_SEPARATORS).into_iter()
}

pub(crate) fn split_names(value: &str) -> Vec<String> {
    tags::split(value, &tags::ARTIST_SEPARATORS)
}

/// The STREAMINFO MD5 of a FLAC stream, after an ID3v2 tag if one leads; None when the encoder
/// left it zero.
fn flac_md5(file: &mut (impl Read + Seek)) -> Option<[u8; 16]> {
    let mut head = [0; 10];
    file.rewind().ok()?;
    file.read_exact(&mut head).ok()?;
    let start = if &head[..3] == b"ID3" {
        let size = head[6..10]
            .iter()
            .fold(0_u64, |size, byte| size << 7 | u64::from(byte & 0x7f));
        10 + size + if head[5] & 0x10 != 0 { 10 } else { 0 }
    } else {
        0
    };
    let mut block = [0; 42];
    file.seek(std::io::SeekFrom::Start(start)).ok()?;
    file.read_exact(&mut block).ok()?;
    if &block[..4] != b"fLaC" || block[4] & 0x7f != 0 {
        return None;
    }
    let md5: [u8; 16] = block[26..42].try_into().ok()?;
    (md5 != [0; 16]).then_some(md5)
}

/// The title of a file whose tags name none.
fn file_title(path: &Path) -> String {
    path.file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Unknown Title")
        .to_string()
}

/// The album of a file whose tags name none: its folder, unless that is the source's own folder.
fn folder_album(path: &Path, root: Option<&Path>) -> String {
    path.parent()
        .filter(|folder| Some(*folder) != root)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("Unknown Album")
        .to_string()
}

/// Credits for `names`, with MusicBrainz ids and sort names that count only when there is one
/// per name. A name without a sort name sorts as the same name in `fallback`.
fn credits(
    names: &[String],
    mbids: Vec<String>,
    sorts: Vec<String>,
    fallback: &[library::ArtistCredit],
) -> Vec<library::ArtistCredit> {
    names
        .iter()
        .cloned()
        .zip(aligned_values(names, mbids))
        .zip(aligned_values(names, sorts))
        .map(|((name, mbid), sort)| library::ArtistCredit {
            sort_text: Some(sort.map_or_else(
                || {
                    fallback
                        .iter()
                        .find(|credit| credit.name == name)
                        .and_then(|credit| credit.sort_text.clone())
                        .unwrap_or_else(|| name.to_lowercase())
                },
                |sort| sort.to_lowercase(),
            )),
            ..artist_credit(name, mbid.as_deref())
        })
        .collect()
}

fn tag_references(kind: &str, names: Vec<String>) -> Vec<library::TagReference> {
    names
        .into_iter()
        .map(|name| library::TagReference {
            object_id: local_id(kind, name.trim()),
            sort_text: Some(name.to_lowercase()),
            name,
        })
        .collect()
}

fn native_gain(
    integrated_lufs: Option<f64>,
    replay_gain_db: Option<f64>,
    replay_gain_peak: Option<f64>,
) -> Option<library::NativeGain> {
    (integrated_lufs.is_some() || replay_gain_db.is_some()).then_some(library::NativeGain {
        integrated_lufs,
        replay_gain_db,
        replay_gain_peak,
        ..Default::default()
    })
}

pub(crate) fn track_id(path: &Path) -> String {
    local_id("track", &path.to_string_lossy())
}

pub(crate) fn cue_track_id(cue_path: &Path, track_number: u16) -> String {
    local_id(
        "track",
        &format!("{}:{track_number}", cue_path.to_string_lossy()),
    )
}

/// An artist is its name; its MusicBrainz id is metadata, so tracks tagged with and without it
/// credit one artist.
pub(crate) fn artist_credit(
    name: String,
    musicbrainz_artist_id: Option<&str>,
) -> library::ArtistCredit {
    library::ArtistCredit {
        object_id: local_id("artist", &normalized_identity(&name)),
        sort_text: Some(name.to_lowercase()),
        name,
        musicbrainz_artist_id: musicbrainz_artist_id.and_then(tags::clean_mbid),
    }
}

pub(crate) fn local_id(kind: &str, value: &str) -> String {
    format!("local:{kind}:{:016x}", stable_hash(value))
}

fn aligned_values(names: &[String], values: Vec<String>) -> Vec<Option<String>> {
    if names.len() == values.len() {
        values.into_iter().map(Some).collect()
    } else {
        names.iter().map(|_| None).collect()
    }
}

fn normalized_identity(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use lofty::config::WriteOptions;
    use lofty::tag::{ItemKey, Tag, TagType};

    use super::*;
    use crate::file::local::media::read_media;
    use std::fs;

    #[test]
    fn executable_contents_are_not_local_audio() {
        let path = std::env::current_exe().expect("test executable");
        let result = read_media(&mut Worker::default(), path, None, None);
        assert!(matches!(result, MediaRead::Rejected), "{result:?}");
    }

    #[test]
    fn mpeg_content_keeps_metadata_with_arbitrary_suffixes_and_leading_junk() {
        let directory = tempfile::tempdir().expect("audio directory");
        for (name, prefixed) in [
            ("track.mp3", false),
            ("track.txt", false),
            ("track", false),
            ("track.bin", false),
            ("prefixed.bin", true),
            ("large-art.mp3", false),
            ("bad-art.mp3", false),
        ] {
            let path = directory.path().join(name);
            let mut frame = vec![0; 417];
            frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
            fs::write(&path, frame.repeat(40)).expect("MPEG silence");
            let mut tag = Tag::new(TagType::Id3v2);
            tag.set_title("Original".into());
            tag.set_album("The Album".into());
            tag.set_artist("The Cure".into());
            tag.insert_text(ItemKey::TrackTitleSortOrder, "A Song".into());
            tag.insert_text(ItemKey::AlbumTitleSortOrder, "Album, The".into());
            tag.insert_text(ItemKey::TrackArtistSortOrder, "Cure, The".into());
            tag.insert_text(ItemKey::RecordingDate, "2001-02-03".into());
            tag.insert_text(ItemKey::ReleaseDate, "2004-05".into());
            tag.insert_text(ItemKey::OriginalReleaseDate, "1995".into());
            tag.set_comment("Retained comment".into());
            if name != "track.txt" {
                let bytes = if name == "large-art.mp3" {
                    33 * 1024 * 1024
                } else {
                    32
                };
                tag.push_picture(
                    lofty::picture::Picture::unchecked(vec![7; bytes])
                        .mime_type(lofty::picture::MimeType::Png)
                        .pic_type(lofty::picture::PictureType::CoverFront)
                        .build(),
                );
                tag.push_picture(
                    lofty::picture::Picture::unchecked(vec![8; 32])
                        .mime_type(lofty::picture::MimeType::Png)
                        .pic_type(lofty::picture::PictureType::Artist)
                        .description("Artist: The Cure")
                        .build(),
                );
            }
            tag.save_to_path(&path, WriteOptions::new().preferred_padding(0))
                .expect("audio tags");
            if name == "bad-art.mp3" {
                let mut bytes = fs::read(&path).unwrap();
                let picture = bytes.windows(4).position(|part| part == b"APIC").unwrap();
                bytes[picture + 10] = 0xff;
                fs::write(&path, bytes).unwrap();
                assert!(super::super::lofty::read_lofty(&path, true).is_err());
                assert!(super::super::lofty::read_lofty(&path, false).is_ok());
            }
            if prefixed {
                let mut bytes = vec![0; 32];
                bytes.extend(fs::read(&path).expect("tagged audio"));
                fs::write(&path, bytes).expect("leading junk");
            }
            let MediaRead::Accepted(track) =
                read_media(&mut Worker::default(), path.clone(), None, None)
            else {
                panic!("audio content was rejected: {name}");
            };
            assert_eq!(track.track.metadata.title, "Original", "{name}");
            assert_eq!(track.track.metadata.sort_text, "a song", "{name}");
            assert_eq!(track.track.metadata.album, "The Album", "{name}");
            assert_eq!(
                track.track.metadata.album_tags.as_ref().unwrap().sort_text,
                "album, the",
                "{name}"
            );
            assert_eq!(track.track.metadata.artists[0].name, "The Cure", "{name}");
            assert_eq!(
                track.track.metadata.artists[0].sort_text.as_deref(),
                Some("cure, the"),
                "{name}"
            );
            assert_eq!(
                track.track.metadata.album_artists[0].sort_text.as_deref(),
                Some("cure, the"),
                "{name}"
            );
            assert_eq!(
                track.track.metadata.comment.as_deref(),
                Some("Retained comment"),
                "{name}"
            );
            assert_eq!(track.track.metadata.year, Some(2004), "{name}");
            assert_eq!(
                track.track.metadata.release_date.as_deref(),
                Some("2004-05"),
                "{name}"
            );
            let uri = url::Url::from_file_path(&path).unwrap().to_string();
            assert_eq!(
                track_reference((*track).clone(), uri)
                    .metadata
                    .release_date
                    .as_deref(),
                Some("2004-05"),
                "{name}"
            );
            if name == "bad-art.mp3" {
                assert!(
                    track.picture_index.is_none(),
                    "failed picture parsing stays unknown"
                );
                assert!(track.artist_pictures.is_empty());
                continue;
            }
            if name == "track.txt" {
                assert_eq!(track.picture_index, Some(-1));
                assert!(track.artist_pictures.is_empty());
            } else {
                assert!(track.picture_index.is_some_and(|index| index >= 0));
                assert_eq!(track.artist_pictures.len(), 1);
                assert_eq!(track.artist_pictures[0].artist.as_deref(), Some("The Cure"));
                assert_ne!(
                    track.picture_index,
                    Some(i64::from(track.artist_pictures[0].index))
                );
            }
            if name == "large-art.mp3" {
                continue;
            }
            let metadata = crate::file::metadata::read_track_metadata(&path, Some("mp3"))
                .expect("read audio metadata");
            assert!(
                metadata
                    .fields
                    .iter()
                    .find(|field| field.key == "title")
                    .unwrap()
                    .writable,
                "{name}"
            );
            crate::file::metadata::edit_copy(
                &path,
                Some("mp3"),
                &crate::file::metadata::TagUpdate::Metadata(vec![(
                    crate::MetadataEdit {
                        kind: crate::SourceEntityKind::Track,
                        changes: [("title".into(), "Updated".into())].into(),
                        artwork: None,
                    },
                    String::new(),
                )]),
                &mut None,
            )
            .expect("write audio metadata");
            let updated = crate::file::metadata::read_track_metadata(&path, Some("mp3"))
                .expect("read updated audio metadata");
            assert_eq!(
                updated
                    .fields
                    .iter()
                    .find(|field| field.key == "title")
                    .unwrap()
                    .value,
                "Updated",
                "{name}"
            );
            assert_eq!(
                updated
                    .fields
                    .iter()
                    .find(|field| field.key == "comment")
                    .unwrap()
                    .value,
                "Retained comment",
                "{name}"
            );
        }
    }

    #[test]
    fn local_loudness_parses_replay_gain_and_r128_independently() {
        assert_eq!(replay_gain_db(Some("-4.25 dB")), Some(-4.25));
        assert_eq!(replay_gain_peak(Some("0.91")), Some(0.91));
        assert_eq!(r128_integrated_lufs(Some("-512")), Some(-21.0));
    }
}

pub(crate) fn accepted_at() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_secs() as i64)
}
