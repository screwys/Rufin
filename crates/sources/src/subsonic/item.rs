use super::*;
use crate::policy::normalized_date;
use crate::remote_json as json;
use serde_json::Value;

pub(super) fn image_ref(source: &SubsonicSource, cover_art: Option<String>) -> Option<ImageRef> {
    cover_art.map(|id| ImageRef::new(source.id("cover", &id), None))
}

fn genre_credits_from_item(source: &SubsonicSource, item: &Value) -> Vec<library::GenreInput> {
    let mut names = Vec::new();
    if let Some(genre) = json::field::<String>(item, "genre").filter(|name| !name.trim().is_empty())
    {
        names.push(genre);
    }
    for genre in json::items(&item["genres"]) {
        if let Some(name) = json::field::<String>(genre, "name")
            .filter(|name| !name.trim().is_empty() && !names.contains(name))
        {
            names.push(name);
        }
    }
    names
        .into_iter()
        .map(|name| library::GenreInput {
            object_id: String::from(source.id("genre", &name)),
            name,
            ..Default::default()
        })
        .collect()
}

pub(super) fn moods_from_item(
    source: &SubsonicSource,
    moods: Vec<String>,
) -> Vec<library::MoodInput> {
    let mut values = Vec::new();
    for mood in moods {
        let mood = mood.trim();
        if !mood.is_empty()
            && !values
                .iter()
                .any(|value: &library::MoodInput| value.name.eq_ignore_ascii_case(mood))
        {
            values.push(library::MoodInput {
                object_id: String::from(source.id("mood", mood)),
                name: mood.to_string(),
                sort_text: mood.to_lowercase(),
            });
        }
    }
    values
}

fn artist_credits_from_item(source: &SubsonicSource, artists: &Value) -> Vec<library::ArtistInput> {
    json::items(artists)
        .iter()
        .filter_map(|artist| {
            Some(library::ArtistInput {
                object_id: String::from(source.id("artist", &json::id(&artist["id"])?)),
                name: json::field(artist, "name").unwrap_or_default(),
                sort_text: clean_optional(json::field::<String>(artist, "sortName"))
                    .map(|name| name.to_lowercase()),
                ..Default::default()
            })
        })
        .collect()
}

fn scalar_artist_credit(
    source: &SubsonicSource,
    item: &Value,
    name: &str,
) -> Vec<library::ArtistInput> {
    json::id(&item["artistId"])
        .map(|id| library::ArtistInput {
            object_id: String::from(source.id("artist", &id)),
            name: name.to_string(),
            ..Default::default()
        })
        .into_iter()
        .collect()
}

pub(super) fn joined_artist_names(artists: &[library::ArtistInput]) -> Option<String> {
    let names = artists
        .iter()
        .map(|artist| artist.name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| names.join(", "))
}

fn structured_release_date(date: &Value) -> Option<String> {
    let year = json::field::<u16>(date, "year").filter(|year| *year > 0)?;
    match (
        json::field::<u8>(date, "month").filter(|month| (1..=12).contains(month)),
        json::field::<u8>(date, "day").filter(|day| (1..=31).contains(day)),
    ) {
        (Some(month), Some(day)) => Some(format!("{year:04}-{month:02}-{day:02}")),
        (Some(month), None) => Some(format!("{year:04}-{month:02}")),
        _ => Some(format!("{year:04}")),
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

pub(super) fn album_from_json(
    source: &SubsonicSource,
    source_id: &str,
    album: &Value,
) -> library::LibraryResult<Option<library::AlbumInput>> {
    let Some(raw_id) = json::id(&album["id"]) else {
        return Ok(None);
    };
    let structured_artists = artist_credits_from_item(source, &album["artists"]);
    let artist = display_artist(album);
    let album_artists = if structured_artists.is_empty() {
        scalar_artist_credit(source, album, &artist)
    } else {
        structured_artists
    };
    let release_date = structured_release_date(&album["releaseDate"])
        .or_else(|| structured_release_date(&album["originalReleaseDate"]));
    let year = {
        let scalar = u16_from_option(json::field(album, "year"));
        if scalar > 0 {
            scalar
        } else {
            release_date
                .as_deref()
                .and_then(|date| date.get(..4))
                .and_then(|year| year.parse().ok())
                .unwrap_or_default()
        }
    };
    let title = album_title(album);
    Ok(Some(library::AlbumInput {
        sort_text: clean_optional(json::field(album, "sortName"))
            .unwrap_or_else(|| title.clone())
            .to_lowercase(),
        object_id: source.id("album", &raw_id),
        title,
        display_artist: artist,
        year: Some(i64::from(year)).filter(|year| *year > 0),
        release_date,
        date_added: normalized_date(json::field(album, "created")),
        rating: json::field::<u32>(album, "userRating")
            .filter(|value| *value > 0)
            .map(|value| i64::from(value.min(5).saturating_mul(2))),
        favorite: favorite(&json::field(album, "starred")),
        artwork_binding: image_ref(source, json::id(&album["coverArt"]))
            .as_ref()
            .map(|image| crate::native_artwork_binding(source_id, image))
            .transpose()?,
        is_compilation: json::boolean(&album["isCompilation"]),
        musicbrainz_release_id: clean_optional(json::field(album, "musicBrainzId")),
        musicbrainz_release_group_id: None,
        relations: Some(library::AlbumRelationsInput {
            album_artists,
            genres: genre_credits_from_item(source, album),
            release_types: normalize_release_types(json::strings(&album["releaseTypes"])),
            ..Default::default()
        }),
        ..Default::default()
    }))
}

pub(super) fn track_from_json(
    source: &SubsonicSource,
    source_id: &str,
    song: &Value,
) -> library::LibraryResult<Option<library::TrackInput>> {
    let Some(raw_id) = json::id(&song["id"]) else {
        return Ok(None);
    };
    let structured_artists = artist_credits_from_item(source, &song["artists"]);
    let artist = display_artist(song);
    let artist_credits = if structured_artists.is_empty() {
        scalar_artist_credit(source, song, &artist)
    } else {
        structured_artists
    };
    let suffix = json::field::<String>(song, "suffix");
    let content_type = json::field::<String>(song, "contentType");
    let path = json::field::<String>(song, "path");
    let source_format =
        source_format_from_song(suffix.as_deref(), content_type.as_deref(), path.as_deref());
    let replay_gain = &song["replayGain"];
    let object_id = source.id("track", &raw_id);
    let title = json::field(song, "title").unwrap_or_else(|| "Untitled Track".to_string());
    let duration_seconds = json::field::<u32>(song, "duration").unwrap_or_default();
    let loudness_analysis_key = audio_key(
        &object_id,
        path.as_deref(),
        source_format.as_deref(),
        duration_seconds,
    );
    Ok(Some(library::TrackInput {
        audio_properties: library::AudioProperties {
            bitrate_kbps: json::field(song, "bitRate"),
            sample_rate_hz: json::field(song, "samplingRate"),
            bit_depth: json::field(song, "bitDepth"),
            channels: json::field(song, "channelCount"),
        },
        sort_text: clean_optional(json::field(song, "sortName"))
            .unwrap_or_else(|| title.clone())
            .to_lowercase(),
        object_id,
        album_object_id: json::id(&song["albumId"]).map(|id| String::from(source.id("album", &id))),
        title,
        display_artist: artist,
        display_album: json::field(song, "album").unwrap_or_else(|| "Unknown Album".to_string()),
        year: Some(i64::from(u16_from_option(json::field(song, "year")))).filter(|year| *year > 0),
        release_date: None,
        date_added: normalized_date(json::field(song, "created")),
        source_last_played: crate::policy::unix_seconds(json::field(song, "played")),
        source_play_count: json::field::<u64>(song, "playCount")
            .map(|value| i64::from(value.min(u64::from(u32::MAX)) as u32)),
        rating: json::field::<u32>(song, "userRating")
            .filter(|value| *value > 0)
            .map(|value| i64::from(value.min(5).saturating_mul(2))),
        duration_millis: i64::from(duration_seconds) * 1000,
        loudness_analysis_key,
        favorite: favorite(&json::field(song, "starred")),
        disc_number: i64::from(u16_from_option(json::field(song, "discNumber"))),
        track_number: i64::from(u16_from_option(json::field(song, "track"))),
        artwork_binding: image_ref(source, json::id(&song["coverArt"]))
            .as_ref()
            .map(|image| crate::native_artwork_binding(source_id, image))
            .transpose()?,
        musicbrainz_recording_id: clean_optional(json::field(song, "musicBrainzId")),
        musicbrainz_release_track_id: None,
        source_path: path,
        source_format,
        comment: clean_optional(json::field(song, "comment")),
        bpm: json::field::<u16>(song, "bpm")
            .filter(|value| *value > 0)
            .map(i64::from),
        source_loudness: finite(json::field(replay_gain, "trackGain")).map(|gain| {
            library::LoudnessInput {
                replay_gain_db: Some(gain),
                replay_gain_peak: positive_finite(json::field(replay_gain, "trackPeak")),
                ..Default::default()
            }
        }),
        album_loudness: finite(json::field(replay_gain, "albumGain")).map(|gain| {
            library::LoudnessInput {
                replay_gain_db: Some(gain),
                replay_gain_peak: positive_finite(json::field(replay_gain, "albumPeak")),
                ..Default::default()
            }
        }),
        relations: Some(library::TrackRelationsInput {
            artists: artist_credits,
            album_artists: artist_credits_from_item(source, &song["albumArtists"]),
            genres: genre_credits_from_item(source, song),
            moods: moods_from_item(source, json::strings(&song["moods"])),
        }),
        ..Default::default()
    }))
}

fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite())
}
fn positive_finite(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value >= 0.0)
}

pub(super) fn source_format_from_song(
    suffix: Option<&str>,
    content_type: Option<&str>,
    path: Option<&str>,
) -> Option<String> {
    suffix
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            content_type
                .and_then(|value| value.rsplit('/').next())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
        .or_else(|| {
            let raw_path = path?;
            let path = raw_path.split(['?', '#']).next().unwrap_or(raw_path);
            std::path::Path::new(path)
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
}
pub(super) fn artist_from_json(
    source: &SubsonicSource,
    source_id: &str,
    artist: &Value,
) -> library::LibraryResult<Option<library::ArtistInput>> {
    let Some(raw_id) = json::id(&artist["id"]) else {
        return Ok(None);
    };
    let name = json::field(artist, "name").unwrap_or_else(|| "Unknown Artist".to_string());
    Ok(Some(library::ArtistInput {
        object_id: source.id("artist", &raw_id),
        sort_text: Some(
            clean_optional(json::field(artist, "sortName"))
                .unwrap_or_else(|| name.clone())
                .to_lowercase(),
        ),
        name,
        favorite: favorite(&json::field(artist, "starred")),
        rating: json::field::<u32>(artist, "userRating")
            .filter(|value| *value > 0)
            .map(|value| i64::from(value.min(5).saturating_mul(2))),
        musicbrainz_artist_id: clean_optional(json::field(artist, "musicBrainzId")),
        artwork_binding: image_ref(source, json::id(&artist["coverArt"]))
            .as_ref()
            .map(|image| crate::native_artwork_binding(source_id, image))
            .transpose()?,
    }))
}

pub(super) fn audio_key(
    object_id: &str,
    path: Option<&str>,
    format: Option<&str>,
    duration_seconds: u32,
) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"rufin-subsonic-audio-v1\0");
    hash.update(object_id.as_bytes());
    hash.update(path.unwrap_or_default().as_bytes());
    hash.update(format.unwrap_or_default().as_bytes());
    hash.update(&duration_seconds.to_le_bytes());
    *hash.finalize().as_bytes()
}

pub(super) fn album_title(album: &Value) -> String {
    json::field(album, "title")
        .or_else(|| json::field(album, "name"))
        .or_else(|| json::field(album, "album"))
        .unwrap_or_else(|| "Untitled Album".to_string())
}

pub(super) fn display_artist(item: &Value) -> String {
    clean_optional(json::field(item, "displayArtist"))
        .or_else(|| clean_optional(json::field(item, "artist")))
        .or_else(|| {
            let names = json::items(&item["artists"])
                .iter()
                .filter(|artist| json::id(&artist["id"]).is_some())
                .filter_map(|artist| json::field::<String>(artist, "name"))
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>();
            (!names.is_empty()).then(|| names.join(", "))
        })
        .unwrap_or_else(|| "Unknown Artist".to_string())
}

pub(super) fn playlist_from_json(
    source: &SubsonicSource,
    source_id: &str,
    playlist: &Value,
) -> library::LibraryResult<Option<Playlist>> {
    let Some(raw_id) = json::id(&playlist["id"]) else {
        return Ok(None);
    };
    let writable = !json::boolean(&playlist["readonly"]).unwrap_or(false);
    // Navidrome's readonly also covers owned playlists with file-managed or smart tracks.
    let metadata_writable = writable
        || (source.flavor == SubsonicFlavor::Navidrome
            && playlist["owner"].as_str() == Some(source.username.as_str()));
    let name =
        json::field::<String>(playlist, "name").unwrap_or_else(|| "Untitled Playlist".to_string());
    Ok(Some(Playlist {
        header: library::PlaylistInput {
            object_id: source.id("playlist", &raw_id),
            sort_text: name.to_lowercase(),
            name,
            artwork_binding: image_ref(source, json::id(&playlist["coverArt"]))
                .as_ref()
                .map(|image| crate::native_artwork_binding(source_id, image))
                .transpose()?,
            writable: Some(writable),
            metadata_writable: Some(metadata_writable),
        },
        revision: json::field::<String>(playlist, "changed").filter(|value| !value.is_empty()),
        valid_until: crate::policy::unix_seconds(json::field(playlist, "validUntil")),
    }))
}

pub(super) fn playlist_entries_from_json(
    source: &SubsonicSource,
    playlist_id: &str,
    entries: &Value,
) -> Vec<PlaylistEntry> {
    json::items(entries)
        .iter()
        .enumerate()
        .filter_map(|(index, song)| {
            let raw_track_id = json::id(&song["id"])?;
            Some(PlaylistEntry {
                occurrence_id: playlist_entry_id(playlist_id, index, &raw_track_id),
                track_id: source.id("track", &raw_track_id),
            })
        })
        .collect()
}
