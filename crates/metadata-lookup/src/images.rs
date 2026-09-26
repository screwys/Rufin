//! Selectable album covers and artist portraits.

use reqwest::Url;
use serde_json::Value;

use crate::http::{client, download, fetch_json, fetch_optional_json, ipv4_client};
use crate::musicbrainz::{fetch_musicbrainz_entity, fetch_musicbrainz_json, usable_mbid};

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
                if let Some(id) = id.as_deref().and_then(usable_mbid) {
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
                    let Some(id) = release["id"].as_str().and_then(usable_mbid) else {
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
        } => artist_results(name, musicbrainz_id.as_deref(), false),
    }
}

pub fn lookup_artist_image(
    name: &str,
    musicbrainz_id: Option<&str>,
    original: bool,
) -> Result<Option<Vec<u8>>, String> {
    for result in artist_results(name, musicbrainz_id, true)? {
        if let Some(bytes) = download(
            client()?,
            if original {
                &result.image_url
            } else {
                &result.thumbnail_url
            },
            "artist portrait",
        )? {
            return Ok(Some(bytes));
        }
    }
    Ok(None)
}

fn artist_results(
    name: &str,
    musicbrainz_id: Option<&str>,
    automatic: bool,
) -> Result<Vec<ArtworkResult>, String> {
    let mut artists = Vec::new();
    if let Some(id) = musicbrainz_id.and_then(usable_mbid) {
        artists.push((id.to_string(), name.to_string(), String::new()));
    }
    if (!automatic || artists.is_empty()) && !name.trim().is_empty() {
        let query = format!("artist:\"{}\"", phrase(name));
        let url = Url::parse_with_params(
            "https://musicbrainz.org/ws/2/artist/",
            [
                ("query", query.as_str()),
                ("fmt", "json"),
                ("limit", if automatic { "1" } else { "8" }),
            ],
        )
        .map_err(|error| error.to_string())?;
        let data = fetch_musicbrainz_json(ipv4_client()?, url, "MusicBrainz artist search")?;
        for artist in data["artists"].as_array().into_iter().flatten() {
            let Some(id) = artist["id"].as_str().and_then(usable_mbid) else {
                continue;
            };
            if artists.iter().any(|(current, _, _)| current == id) {
                continue;
            }
            artists.push((
                id.into(),
                artist["name"].as_str().unwrap_or(name).into(),
                artist["disambiguation"].as_str().unwrap_or_default().into(),
            ));
        }
    }
    let mut results = Vec::new();
    let mut failure = None;
    for (id, title, detail) in artists {
        match artist_images(&id, &title, &detail) {
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

fn artist_images(id: &str, title: &str, detail: &str) -> Result<Vec<ArtworkResult>, String> {
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
            let url = Url::parse_with_params(
                "https://commons.wikimedia.org/w/api.php",
                [
                    ("action", "query"),
                    ("format", "json"),
                    ("prop", "imageinfo"),
                    ("titles", file.as_str()),
                    ("iiprop", "url"),
                    ("iiurlwidth", "512"),
                ],
            )
            .map_err(|error| error.to_string())?;
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

fn phrase(value: &str) -> String {
    value.replace('\\', " ").replace('"', "\\\"")
}
