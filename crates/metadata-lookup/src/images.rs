//! Selectable album covers and artist portraits.

use reqwest::Url;
use serde_json::Value;

use crate::http::{client, download, fetch_json, fetch_optional_json, ipv4_client};
use crate::musicbrainz::{fetch_musicbrainz_entity, fetch_musicbrainz_json};
use crate::{ArtistImageProvider, LookupPolicy};

const FANART_URL: &str = "https://webservice.fanart.tv/v3/music";
/// Rufin's fanart.tv project key, which ships in the binary.
const FANART_PROJECT_KEY: &str = "733c108ee90839310bfe200ce13f6435";

#[derive(Clone, Debug)]
pub enum ArtworkQuery {
    Album {
        artist: String,
        album: String,
        release_id: Option<String>,
        release_group_id: Option<String>,
    },
    Artist {
        name: String,
        musicbrainz_id: Option<String>,
    },
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArtworkResult {
    pub title: String,
    pub detail: String,
    pub thumbnail_url: String,
    pub image_url: String,
    pub source_url: String,
}

pub fn download_artwork(url: &str) -> Result<Vec<u8>, String> {
    download(client()?, url, "artwork")?.ok_or_else(|| "Artwork is no longer available".into())
}

pub fn search_artwork(query: &ArtworkQuery) -> Result<Vec<ArtworkResult>, String> {
    match query {
        ArtworkQuery::Album {
            artist,
            album,
            release_id,
            release_group_id,
        } => {
            let mut identities = Vec::new();
            let mut failure = None;
            for (kind, id) in [("release", release_id), ("release-group", release_group_id)] {
                if let Some(id) = id.as_deref().and_then(library::AlbumCover::usable_mbid) {
                    identities.push((kind, id.to_string(), album.clone(), artist.clone()));
                }
            }
            if !album.trim().is_empty() || !artist.trim().is_empty() {
                let query = [("artist", artist), ("release", album)]
                    .into_iter()
                    .filter(|(_, value)| !value.trim().is_empty())
                    .map(|(field, value)| format!("{field}:\"{}\"", phrase(value)))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                let url = Url::parse_with_params(
                    "https://musicbrainz.org/ws/2/release/",
                    [("query", query.as_str()), ("fmt", "json"), ("limit", "12")],
                )
                .map_err(|error| error.to_string())?;
                let data =
                    match fetch_musicbrainz_json(ipv4_client()?, url, "MusicBrainz artwork search")
                    {
                        Ok(data) => data,
                        Err(error) => {
                            failure = Some(error);
                            Value::Null
                        }
                    };
                for release in data["releases"].as_array().into_iter().flatten() {
                    let Some(id) = release["id"]
                        .as_str()
                        .and_then(library::AlbumCover::usable_mbid)
                    else {
                        continue;
                    };
                    if identities
                        .iter()
                        .any(|(kind, current, _, _)| *kind == "release" && current == id)
                    {
                        continue;
                    }
                    let detail = [
                        release["date"].as_str(),
                        release["country"].as_str(),
                        release["disambiguation"].as_str(),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                    identities.push((
                        "release",
                        id.into(),
                        release["title"].as_str().unwrap_or(album).into(),
                        detail,
                    ));
                }
            }
            let mut results = Vec::new();
            for batch in identities.chunks(4) {
                let responses = std::thread::scope(|scope| {
                    let jobs = batch
                        .iter()
                        .map(|(kind, id, _, _)| {
                            scope.spawn(move || {
                                let url =
                                    Url::parse(&format!("https://coverartarchive.org/{kind}/{id}"))
                                        .map_err(|error| error.to_string())?;
                                fetch_optional_json(client()?, url, "Cover Art Archive")
                            })
                        })
                        .collect::<Vec<_>>();
                    jobs.into_iter()
                        .map(|job| {
                            job.join()
                                .unwrap_or_else(|_| Err("Artwork search ended unexpectedly".into()))
                        })
                        .collect::<Vec<_>>()
                });
                for ((kind, id, title, detail), response) in batch.iter().zip(responses) {
                    match response {
                        Ok(Some(data)) => {
                            for image in data["images"].as_array().into_iter().flatten() {
                                if image["front"].as_bool() != Some(true) {
                                    continue;
                                }
                                let Some(original) = image["image"].as_str() else {
                                    continue;
                                };
                                let thumbnail = image
                                    .pointer("/thumbnails/250")
                                    .or_else(|| image.pointer("/thumbnails/small"))
                                    .and_then(Value::as_str)
                                    .unwrap_or(original);
                                let original = original.replacen("http://", "https://", 1);
                                if results
                                    .iter()
                                    .any(|result: &ArtworkResult| result.image_url == original)
                                {
                                    continue;
                                }
                                results.push(ArtworkResult {
                                    title: title.clone(),
                                    detail: detail.clone(),
                                    thumbnail_url: thumbnail.replacen("http://", "https://", 1),
                                    image_url: original,
                                    source_url: format!("https://musicbrainz.org/{kind}/{id}"),
                                });
                            }
                        }
                        Ok(None) => {}
                        Err(error) => failure = Some(error),
                    }
                }
            }
            if results.is_empty()
                && let Some(error) = failure
            {
                return Err(error);
            }
            Ok(results)
        }
        ArtworkQuery::Artist {
            name,
            musicbrainz_id,
        } => artist_results(name, musicbrainz_id.as_deref(), Some(512)),
    }
}

/// Finds an artist portrait from the providers in the policy's order. fanart.tv and
/// Wikimedia Commons take the artist's MusicBrainz ID: the one in the tags, or the first
/// MusicBrainz search result.
pub fn lookup_artist_image(
    name: &str,
    musicbrainz_id: Option<&str>,
    size: Option<u32>,
    policy: &LookupPolicy,
) -> Result<Option<Vec<u8>>, String> {
    let mut failures = Vec::new();
    let mut artist_id = None;
    let mut resolve = |failures: &mut Vec<String>| -> Option<String> {
        artist_id
            .get_or_insert_with(|| match musicbrainz_artist_id(name, musicbrainz_id) {
                Ok(id) => id,
                Err(error) => {
                    failures.push(error);
                    None
                }
            })
            .clone()
    };
    for provider in &policy.artist_images {
        let urls = match provider {
            ArtistImageProvider::FanartTv => match resolve(&mut failures) {
                Some(id) => fanart_url(&id, size, &policy.fanart_api_key).map(Vec::from_iter),
                None => continue,
            },
            ArtistImageProvider::WikimediaCommons => match resolve(&mut failures) {
                Some(id) => artist_images(&id, name, "", size).map(|images| {
                    images
                        .into_iter()
                        .map(|image| {
                            if size.is_none() {
                                image.image_url
                            } else {
                                image.thumbnail_url
                            }
                        })
                        .collect()
                }),
                None => continue,
            },
        };
        match urls {
            Ok(urls) => {
                for url in urls {
                    match download(client()?, &url, "artist portrait") {
                        Ok(Some(bytes)) => return Ok(Some(bytes)),
                        Ok(None) => {}
                        Err(error) => failures.push(error),
                    }
                }
            }
            Err(error) => failures.push(error),
        }
    }
    if failures.is_empty() {
        Ok(None)
    } else {
        Err(failures.join("; "))
    }
}

/// The artist's MusicBrainz ID: the tagged one, or the first search result.
fn musicbrainz_artist_id(name: &str, tagged: Option<&str>) -> Result<Option<String>, String> {
    if let Some(id) = tagged.and_then(library::AlbumCover::usable_mbid) {
        return Ok(Some(id.to_string()));
    }
    Ok(musicbrainz_artists(name, 1)?
        .into_iter()
        .next()
        .map(|(id, _, _)| id))
}

/// The best rated artist thumbnail fanart.tv has for a MusicBrainz artist.
fn fanart_url(id: &str, size: Option<u32>, personal_key: &str) -> Result<Option<String>, String> {
    let mut url = Url::parse_with_params(
        &format!("{FANART_URL}/{id}"),
        [("api_key", FANART_PROJECT_KEY)],
    )
    .map_err(|error| error.to_string())?;
    if !personal_key.trim().is_empty() {
        url.query_pairs_mut()
            .append_pair("client_key", personal_key.trim());
    }
    let Some(data) = fetch_optional_json(client()?, url, "fanart.tv artist images")? else {
        return Ok(None);
    };
    let best = data["artistthumb"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|image| {
            let likes = image["likes"]
                .as_str()
                .and_then(|likes| likes.parse::<u64>().ok())
                .unwrap_or_default();
            Some((likes, image["url"].as_str()?))
        })
        .max_by_key(|(likes, _)| *likes)
        .map(|(_, url)| url.replacen("http://", "https://", 1));
    // fanart.tv serves a small preview of every image beside the original.
    Ok(best.map(|url| match size {
        Some(size) if size <= 250 => url.replacen("/fanart/", "/preview/", 1),
        _ => url,
    }))
}

fn artist_results(
    name: &str,
    musicbrainz_id: Option<&str>,
    size: Option<u32>,
) -> Result<Vec<ArtworkResult>, String> {
    let mut artists = Vec::new();
    if let Some(id) = musicbrainz_id.and_then(library::AlbumCover::usable_mbid) {
        artists.push((id.to_string(), name.to_string(), String::new()));
    }
    for artist in musicbrainz_artists(name, 8)? {
        if !artists.iter().any(|(current, _, _)| *current == artist.0) {
            artists.push(artist);
        }
    }
    let mut results = Vec::new();
    let mut failure = None;
    for (id, title, detail) in artists {
        match artist_images(&id, &title, &detail, size) {
            Ok(images) => results.extend(images),
            Err(error) => failure = Some(error),
        }
    }
    if results.is_empty()
        && let Some(error) = failure
    {
        return Err(error);
    }
    Ok(results)
}

/// MusicBrainz artists named like `name`: their IDs, names and disambiguations.
fn musicbrainz_artists(name: &str, limit: u8) -> Result<Vec<(String, String, String)>, String> {
    if name.trim().is_empty() {
        return Ok(Vec::new());
    }
    let query = format!("artist:\"{}\"", phrase(name));
    let url = Url::parse_with_params(
        "https://musicbrainz.org/ws/2/artist/",
        [
            ("query", query.as_str()),
            ("fmt", "json"),
            ("limit", &limit.to_string()),
        ],
    )
    .map_err(|error| error.to_string())?;
    let data = fetch_musicbrainz_json(ipv4_client()?, url, "MusicBrainz artist search")?;
    Ok(data["artists"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|artist| {
            let id = artist["id"]
                .as_str()
                .and_then(library::AlbumCover::usable_mbid)?;
            Some((
                id.into(),
                artist["name"].as_str().unwrap_or(name).into(),
                artist["disambiguation"].as_str().unwrap_or_default().into(),
            ))
        })
        .collect())
}

fn artist_images(
    id: &str,
    title: &str,
    detail: &str,
    size: Option<u32>,
) -> Result<Vec<ArtworkResult>, String> {
    let mut results = Vec::new();
    let Some(artist) = fetch_musicbrainz_entity(
        "https://musicbrainz.org/ws/2/artist/",
        id,
        "url-rels",
        "MusicBrainz artist images",
    )?
    else {
        return Ok(results);
    };
    for relation in artist["relations"].as_array().into_iter().flatten() {
        if relation["type"].as_str() != Some("wikidata") {
            continue;
        }
        let Some(resource) = relation.pointer("/url/resource").and_then(Value::as_str) else {
            continue;
        };
        let entity = resource.rsplit('/').next().unwrap_or_default();
        if entity
            .strip_prefix('Q')
            .is_none_or(|id| !id.bytes().all(|byte| byte.is_ascii_digit()))
        {
            continue;
        }
        let url = Url::parse_with_params(
            "https://www.wikidata.org/w/api.php",
            [
                ("action", "wbgetentities"),
                ("ids", entity),
                ("props", "claims"),
                ("format", "json"),
            ],
        )
        .map_err(|error| error.to_string())?;
        let data = fetch_json(client()?, url, "Wikidata artist images")?;
        for claim in data["entities"][entity]["claims"]["P18"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let Some(file) = claim
                .pointer("/mainsnak/datavalue/value")
                .and_then(Value::as_str)
            else {
                continue;
            };
            let file = format!("File:{file}");
            let url = artist_image_url(&file, size)?;
            let image = fetch_json(client()?, url, "Wikimedia artist image")?;
            for page in image["query"]["pages"]
                .as_object()
                .into_iter()
                .flat_map(|pages| pages.values())
            {
                let Some(info) = page["imageinfo"].as_array().and_then(|items| items.first())
                else {
                    continue;
                };
                let Some(original) = info["url"].as_str() else {
                    continue;
                };
                results.push(ArtworkResult {
                    title: title.into(),
                    detail: detail.into(),
                    thumbnail_url: info["thumburl"].as_str().unwrap_or(original).into(),
                    image_url: original.into(),
                    source_url: info["descriptionurl"].as_str().unwrap_or(resource).into(),
                });
            }
        }
    }
    Ok(results)
}

pub(super) fn artist_image_url(file: &str, size: Option<u32>) -> Result<Url, String> {
    let mut url = Url::parse_with_params(
        "https://commons.wikimedia.org/w/api.php",
        [
            ("action", "query"),
            ("format", "json"),
            ("prop", "imageinfo"),
            ("titles", file),
            ("iiprop", "url"),
        ],
    )
    .map_err(|error| error.to_string())?;
    if let Some(size) = size {
        url.query_pairs_mut()
            .append_pair("iiurlwidth", &size.to_string());
    }
    Ok(url)
}

fn phrase(value: &str) -> String {
    value.replace('\\', " ").replace('"', "\\\"")
}
