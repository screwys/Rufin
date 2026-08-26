//! MusicBrainz identity lookup and metadata enrichment.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use reqwest::Url;
use reqwest::blocking::Client;
use serde_json::Value;
use sources::{AlbumMetadataValues, ArtistMetadataValues, TrackMetadataValues};

use crate::http::{client, fetch_json, fetch_optional_json};

const MUSICBRAINZ_RELEASE_SEARCH_URL: &str = "https://musicbrainz.org/ws/2/release/";
const MUSICBRAINZ_RELEASE_GROUP_SEARCH_URL: &str = "https://musicbrainz.org/ws/2/release-group/";
const MUSICBRAINZ_RECORDING_URL: &str = "https://musicbrainz.org/ws/2/recording/";
const MUSICBRAINZ_ARTIST_URL: &str = "https://musicbrainz.org/ws/2/artist/";
const MUSICBRAINZ_MIN_INTERVAL: Duration = Duration::from_millis(1100);

pub fn lookup_album_release(
    release_group_id: Option<&str>,
    release_id: Option<&str>,
) -> Result<Option<AlbumReleaseMetadata>, String> {
    match fetch_album_release_metadata(release_group_id, release_id) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if is_expected_release_type_lookup_miss(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn identify_track_metadata(
    values: &TrackMetadataValues,
) -> Result<Option<TrackMetadataValues>, String> {
    identify_track(values)
}

pub fn identify_album_metadata(
    values: &AlbumMetadataValues,
) -> Result<Option<AlbumMetadataValues>, String> {
    identify_album(values)
}

pub fn identify_artist_metadata(
    values: &ArtistMetadataValues,
) -> Result<Option<ArtistMetadataValues>, String> {
    identify_artist(values)
}

fn identify_track(values: &TrackMetadataValues) -> Result<Option<TrackMetadataValues>, String> {
    if let (Some(release_id), Some(track_id)) = (
        usable(values.musicbrainz_album_id.as_deref()),
        usable(values.musicbrainz_release_track_id.as_deref()),
    ) {
        let Some(release) = fetch_musicbrainz_entity(
            MUSICBRAINZ_RELEASE_SEARCH_URL,
            release_id,
            "artist-credits+recordings+release-groups+genres+media",
            "MusicBrainz release identification",
        )?
        else {
            return Ok(None);
        };
        return Ok(track_from_release(
            &release,
            Some(track_id),
            values.musicbrainz_recording_id.as_deref(),
        ));
    }
    let recording_id = usable(values.musicbrainz_recording_id.as_deref()).ok_or_else(|| {
        "Add a MusicBrainz recording ID before identifying this track.".to_string()
    })?;
    let Some(recording) = fetch_musicbrainz_entity(
        MUSICBRAINZ_RECORDING_URL,
        recording_id,
        "artist-credits+genres",
        "MusicBrainz recording identification",
    )?
    else {
        return Ok(None);
    };
    Ok(Some(TrackMetadataValues {
        title: text(&recording, "title").unwrap_or_default(),
        artist: artist_credit(&recording),
        genre: genres(&recording),
        musicbrainz_recording_id: Some(recording_id.to_string()),
        ..TrackMetadataValues::default()
    }))
}

fn identify_album(values: &AlbumMetadataValues) -> Result<Option<AlbumMetadataValues>, String> {
    if let Some(release_id) = usable(values.musicbrainz_album_id.as_deref()) {
        let Some(release) = fetch_musicbrainz_entity(
            MUSICBRAINZ_RELEASE_SEARCH_URL,
            release_id,
            "artist-credits+release-groups+genres",
            "MusicBrainz release identification",
        )?
        else {
            return Ok(None);
        };
        return Ok(Some(album_from_release(&release, release_id)));
    }
    let release_group_id =
        usable(values.musicbrainz_release_group_id.as_deref()).ok_or_else(|| {
            "Add a MusicBrainz release or release group ID before identifying this album."
                .to_string()
        })?;
    let Some(group) = fetch_musicbrainz_entity(
        MUSICBRAINZ_RELEASE_GROUP_SEARCH_URL,
        release_group_id,
        "artist-credits+genres",
        "MusicBrainz release-group identification",
    )?
    else {
        return Ok(None);
    };
    Ok(Some(AlbumMetadataValues {
        title: text(&group, "title").unwrap_or_default(),
        artist: artist_credit(&group),
        album_artist: artist_credit(&group),
        year: year(&group, "first-release-date"),
        genre: genres(&group),
        musicbrainz_release_group_id: Some(release_group_id.to_string()),
        ..AlbumMetadataValues::default()
    }))
}

fn identify_artist(values: &ArtistMetadataValues) -> Result<Option<ArtistMetadataValues>, String> {
    let artist_id = usable(values.musicbrainz_artist_id.as_deref())
        .ok_or_else(|| "Add a MusicBrainz artist ID before identifying this artist.".to_string())?;
    let Some(artist) = fetch_musicbrainz_entity(
        MUSICBRAINZ_ARTIST_URL,
        artist_id,
        "genres",
        "MusicBrainz artist identification",
    )?
    else {
        return Ok(None);
    };
    Ok(Some(ArtistMetadataValues {
        name: text(&artist, "name").unwrap_or_default(),
        sort_name: text(&artist, "sort-name"),
        genre: genres(&artist),
        musicbrainz_artist_id: Some(artist_id.to_string()),
        ..ArtistMetadataValues::default()
    }))
}

fn fetch_musicbrainz_entity(
    root: &str,
    id: &str,
    inc: &str,
    context: &str,
) -> Result<Option<Value>, String> {
    let url = Url::parse_with_params(&format!("{root}{id}"), [("fmt", "json"), ("inc", inc)])
        .map_err(|error| error.to_string())?;
    fetch_optional_musicbrainz_json(client()?, url, context)
}

fn track_from_release(
    release: &Value,
    release_track_id: Option<&str>,
    recording_id: Option<&str>,
) -> Option<TrackMetadataValues> {
    let media = release.get("media")?.as_array()?;
    for medium in media {
        let disc_number = positive_u16(medium.get("position"));
        let Some(tracks) = medium.get("tracks").and_then(Value::as_array) else {
            continue;
        };
        for track in tracks {
            let track_matches = release_track_id
                .is_some_and(|id| track.get("id").and_then(Value::as_str) == Some(id));
            let recording_matches = recording_id.is_some_and(|id| {
                track.pointer("/recording/id").and_then(Value::as_str) == Some(id)
            });
            if !track_matches && !recording_matches {
                continue;
            }
            let recording = track.get("recording").unwrap_or(track);
            let release_id = text(release, "id");
            return Some(TrackMetadataValues {
                title: text(recording, "title")
                    .or_else(|| text(track, "title"))
                    .unwrap_or_default(),
                artist: artist_credit(recording).or_else(|| artist_credit(track)),
                album: text(release, "title"),
                album_artist: artist_credit(release),
                track_number: positive_u16(track.get("position")),
                disc_number,
                year: year(release, "date"),
                genre: genres(recording).or_else(|| genres(release)),
                musicbrainz_recording_id: text(recording, "id"),
                musicbrainz_release_track_id: text(track, "id"),
                musicbrainz_album_id: release_id,
                musicbrainz_release_group_id: release
                    .pointer("/release-group/id")
                    .and_then(Value::as_str)
                    .and_then(clean),
                ..TrackMetadataValues::default()
            });
        }
    }
    None
}

fn album_from_release(release: &Value, release_id: &str) -> AlbumMetadataValues {
    AlbumMetadataValues {
        title: text(release, "title").unwrap_or_default(),
        artist: artist_credit(release),
        album_artist: artist_credit(release),
        year: year(release, "date"),
        genre: genres(release).or_else(|| release.get("release-group").and_then(genres)),
        musicbrainz_album_id: Some(release_id.to_string()),
        musicbrainz_release_group_id: release
            .pointer("/release-group/id")
            .and_then(Value::as_str)
            .and_then(clean),
        ..AlbumMetadataValues::default()
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).and_then(clean)
}

fn clean(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn usable(value: Option<&str>) -> Option<&str> {
    value
        .map(str::trim)
        .filter(|value| is_musicbrainz_id(value))
}

fn is_musicbrainz_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn artist_credit(value: &Value) -> Option<String> {
    let values = value
        .get("artist-credit")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|credit| {
            credit
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| credit.pointer("/artist/name").and_then(Value::as_str))
        })
        .filter_map(clean)
        .collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.join("; "))
}

fn genres(value: &Value) -> Option<String> {
    let values = value
        .get("genres")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|genre| genre.get("name"))
        .filter_map(Value::as_str)
        .filter_map(clean)
        .collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.join("; "))
}

fn year(value: &Value, key: &str) -> Option<u16> {
    value
        .get(key)
        .and_then(Value::as_str)
        .and_then(|date| date.get(..4))
        .and_then(|year| year.parse::<u16>().ok())
        .filter(|year| *year > 0)
}

fn positive_u16(value: Option<&Value>) -> Option<u16> {
    value
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .map(|value| value.min(u64::from(u16::MAX)) as u16)
}

fn fetch_musicbrainz_json(client: &Client, url: Url, context: &str) -> Result<Value, String> {
    send_with_musicbrainz_retry(|| {
        wait_for_musicbrainz_slot();
        fetch_json(client, url.clone(), context)
    })
}

fn fetch_optional_musicbrainz_json(
    client: &Client,
    url: Url,
    context: &str,
) -> Result<Option<Value>, String> {
    send_with_musicbrainz_retry(|| {
        wait_for_musicbrainz_slot();
        fetch_optional_json(client, url.clone(), context)
    })
}

fn send_with_musicbrainz_retry<T>(
    mut send: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    match send() {
        Err(error) if error.contains("status 503") => send(),
        result => result,
    }
}

fn wait_for_musicbrainz_slot() {
    static NEXT_REQUEST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    let lock = NEXT_REQUEST.get_or_init(|| Mutex::new(None));
    let Ok(mut next_request) = lock.lock() else {
        return;
    };
    let now = Instant::now();
    let slot = next_request.map_or(now, |next| next.max(now));
    *next_request = Some(slot + MUSICBRAINZ_MIN_INTERVAL);
    drop(next_request);
    let delay = slot.saturating_duration_since(now);
    if !delay.is_zero() {
        std::thread::sleep(delay);
    }
}

fn json_ids(value: &Value, collection_pointer: &str) -> Vec<String> {
    let Some(items) = value.pointer(collection_pointer).and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for id in items
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        if !ids.iter().any(|existing| existing == id) {
            ids.push(id.to_string());
        }
    }
    ids
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlbumReleaseMetadata {
    pub release_types: Vec<String>,
}

fn fetch_album_release_metadata(
    release_group_id: Option<&str>,
    release_id: Option<&str>,
) -> Result<AlbumReleaseMetadata, String> {
    let client = client()?;
    if let Some(release_group_id) = release_group_id.and_then(usable_mbid) {
        return fetch_release_group_metadata(client, release_group_id);
    }
    if let Some(release_id) = release_id.and_then(usable_mbid) {
        return fetch_release_metadata(client, release_id);
    }
    Err("album has no MusicBrainz release or release-group id".to_string())
}

fn is_expected_release_type_lookup_miss(error: &str) -> bool {
    if error.contains("error sending request")
        || error.contains("timed out")
        || error.contains("status 401")
        || error.contains("status 403")
        || error.contains("status 429")
        || error.contains("status 500")
        || error.contains("status 502")
        || error.contains("status 503")
        || error.contains("status 504")
    {
        return false;
    }

    error.contains("404 Not Found") || error.contains("did not return release group type")
}

pub(crate) fn search_album_release_group_ids(
    artist: &str,
    album: &str,
) -> Result<Vec<String>, String> {
    search_album_identity_ids(
        artist,
        album,
        "releasegroup",
        MUSICBRAINZ_RELEASE_GROUP_SEARCH_URL,
        "/release-groups",
        "MusicBrainz release-group lookup",
    )
}

pub(crate) fn search_album_release_ids(artist: &str, album: &str) -> Result<Vec<String>, String> {
    search_album_identity_ids(
        artist,
        album,
        "release",
        MUSICBRAINZ_RELEASE_SEARCH_URL,
        "/releases",
        "MusicBrainz release lookup",
    )
}

fn search_album_identity_ids(
    artist: &str,
    album: &str,
    album_field: &str,
    endpoint: &str,
    collection_pointer: &str,
    context: &str,
) -> Result<Vec<String>, String> {
    let query = format!(
        "artist:\"{}\" AND {album_field}:\"{}\"",
        musicbrainz_phrase(artist),
        musicbrainz_phrase(album)
    );
    let url = Url::parse_with_params(
        endpoint,
        [("query", query.as_str()), ("fmt", "json"), ("limit", "5")],
    )
    .map_err(|error| error.to_string())?;
    let value = fetch_musicbrainz_json(client()?, url, context)?;
    Ok(json_ids(&value, collection_pointer))
}

fn musicbrainz_phrase(value: &str) -> String {
    value.replace('\\', " ").replace('"', "\\\"")
}

fn fetch_release_group_metadata(
    client: &Client,
    release_group_id: &str,
) -> Result<AlbumReleaseMetadata, String> {
    let url = Url::parse_with_params(
        &format!("{MUSICBRAINZ_RELEASE_GROUP_SEARCH_URL}{release_group_id}"),
        [("fmt", "json")],
    )
    .map_err(|error| error.to_string())?;
    let value = fetch_musicbrainz_json(client, url, "MusicBrainz release-group lookup")?;
    release_metadata_from_group(&value)
        .ok_or_else(|| "MusicBrainz did not return release group type".to_string())
}

fn fetch_release_metadata(
    client: &Client,
    release_id: &str,
) -> Result<AlbumReleaseMetadata, String> {
    let url = Url::parse_with_params(
        &format!("{MUSICBRAINZ_RELEASE_SEARCH_URL}{release_id}"),
        [("fmt", "json"), ("inc", "release-groups")],
    )
    .map_err(|error| error.to_string())?;
    let value = fetch_musicbrainz_json(client, url, "MusicBrainz release lookup")?;
    let Some(group) = value.get("release-group") else {
        return Err("MusicBrainz did not return release group type".to_string());
    };
    release_metadata_from_group(group)
        .ok_or_else(|| "MusicBrainz did not return release group type".to_string())
}

fn release_metadata_from_group(group: &Value) -> Option<AlbumReleaseMetadata> {
    let mut raw_types = Vec::new();
    if let Some(primary_type) = group.get("primary-type").and_then(Value::as_str) {
        raw_types.push(primary_type.to_string());
    }
    if let Some(secondary_types) = group.get("secondary-types").and_then(Value::as_array) {
        raw_types.extend(
            secondary_types
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string),
        );
    }
    let release_types = normalize_release_types(raw_types);
    if release_types.is_empty() {
        return None;
    }
    Some(AlbumReleaseMetadata { release_types })
}

fn normalize_release_types(types: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<String> {
    let mut values = Vec::new();
    for release_type in types {
        let value = release_type.as_ref().trim().to_ascii_lowercase();
        if !value.is_empty() && !values.iter().any(|existing| existing == &value) {
            values.push(value);
        }
    }
    values
}

pub(crate) fn usable_mbid(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn musicbrainz_retries_service_unavailable_once() {
        let mut responses = [
            Err("MusicBrainz failed with status 503 Service Unavailable".to_string()),
            Ok("identified"),
        ]
        .into_iter();
        let result = send_with_musicbrainz_retry(|| responses.next().expect("bounded request"));

        assert_eq!(result.as_deref(), Ok("identified"));
        assert!(responses.next().is_none());
    }

    #[test]
    fn parses_primary_and_secondary_release_group_types() {
        let value = json!({
            "primary-type": "Album",
            "secondary-types": ["Compilation", "Live"]
        });

        assert_eq!(
            release_metadata_from_group(&value),
            Some(AlbumReleaseMetadata {
                release_types: vec![
                    "album".to_string(),
                    "compilation".to_string(),
                    "live".to_string(),
                ],
            })
        );
    }

    #[test]
    fn parses_single_without_compilation() {
        let value = json!({
            "primary-type": "Single",
            "secondary-types": []
        });

        assert_eq!(
            release_metadata_from_group(&value),
            Some(AlbumReleaseMetadata {
                release_types: vec!["single".to_string()],
            })
        );
    }

    #[test]
    fn identity_results_ignore_empty_and_duplicate_ids() {
        let value = json!({
            "release-groups": [
                { "id": "first" },
                { "id": "" },
                { "id": "first" },
                { "id": "second" }
            ]
        });

        assert_eq!(json_ids(&value, "/release-groups"), vec!["first", "second"]);
    }

    #[test]
    fn identified_release_track_maps_music_fields_and_exact_ids() {
        let release = json!({
            "id": "release-id",
            "title": "Album",
            "date": "2025-03-01",
            "artist-credit": [{ "name": "Album Artist" }],
            "release-group": { "id": "group-id" },
            "media": [{
                "position": 2,
                "tracks": [{
                    "id": "release-track-id",
                    "position": 4,
                    "recording": {
                        "id": "recording-id",
                        "title": "Track",
                        "artist-credit": [
                            { "name": "First" },
                            { "name": "Second" }
                        ],
                        "genres": [{ "name": "Rock" }]
                    }
                }]
            }]
        });

        let identified =
            track_from_release(&release, Some("release-track-id"), None).expect("identified Track");

        assert_eq!(identified.title, "Track");
        assert_eq!(identified.artist.as_deref(), Some("First; Second"));
        assert_eq!(identified.album.as_deref(), Some("Album"));
        assert_eq!(identified.album_artist.as_deref(), Some("Album Artist"));
        assert_eq!(identified.track_number, Some(4));
        assert_eq!(identified.disc_number, Some(2));
        assert_eq!(identified.year, Some(2025));
        assert_eq!(identified.genre.as_deref(), Some("Rock"));
        assert_eq!(
            identified.musicbrainz_release_group_id.as_deref(),
            Some("group-id")
        );
    }

    #[test]
    fn identified_artist_maps_sort_name_and_genres() {
        let artist = json!({
            "name": "Display Name",
            "sort-name": "Name, Display",
            "genres": [{ "name": "Jazz" }, { "name": "Fusion" }]
        });

        assert_eq!(text(&artist, "name").as_deref(), Some("Display Name"));
        assert_eq!(text(&artist, "sort-name").as_deref(), Some("Name, Display"));
        assert_eq!(genres(&artist).as_deref(), Some("Jazz; Fusion"));
    }
}
