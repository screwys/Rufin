use crate::http::{client, decode_json, wait_for_slot};
use crate::{MetadataCandidate, MetadataMatchKind};
use serde_json::Value;
use std::{sync::Mutex, time::Duration};

const CLIENT_KEY: &str = "Bz6ACWI5Hm";

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct FingerprintMatches {
    pub candidates: Vec<MetadataCandidate>,
    pub recordings: Vec<String>,
}

pub fn lookup_fingerprint(
    cache: &crate::LookupCache,
    uri: &str,
    fingerprint: &str,
    duration: u64,
) -> Result<Option<FingerprintMatches>, String> {
    let key = crate::cache::cache_key("acoustid-releases", &(fingerprint, duration));
    let matches = cache.fetch(&key, 7, || fetch_matches(fingerprint, duration))?;
    if matches.is_some() && cache.mode != crate::CacheMode::Only {
        cache.link_fingerprint(uri, &key)?;
    }
    Ok(matches)
}

fn fetch_matches(fingerprint: &str, duration: u64) -> Result<FingerprintMatches, String> {
    static NEXT: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    wait_for_slot(&NEXT, Duration::from_millis(334));
    let response = client()?
        .post("https://api.acoustid.org/v2/lookup")
        .form(&[
            ("client", CLIENT_KEY),
            ("fingerprint", fingerprint),
            ("duration", &duration.to_string()),
            ("meta", "recordings releasegroups releases tracks"),
            ("format", "json"),
        ])
        .send()
        .map_err(|error| error.to_string())?;
    let value = decode_json(response, "AcoustID lookup")?;
    parse_matches(value)
}

fn parse_matches(value: Value) -> Result<FingerprintMatches, String> {
    if value["status"] != "ok" {
        return Err(value["error"]["message"]
            .as_str()
            .unwrap_or("AcoustID lookup failed")
            .into());
    }
    let mut recordings = Vec::new();
    let mut releases = Vec::new();
    let mut lone = Vec::new();
    for recording in value["results"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|result| result["recordings"].as_array().into_iter().flatten())
    {
        let Some(id) = recording["id"].as_str() else {
            continue;
        };
        if !recordings.iter().any(|known| known == id) {
            recordings.push(id.to_string());
        }
        let mut found = false;
        for group in recording["releasegroups"].as_array().into_iter().flatten() {
            for release in group["releases"].as_array().into_iter().flatten() {
                found = true;
                releases.push(release_row(recording, group, release));
            }
        }
        // A recording on no release can only be matched as itself.
        if !found {
            lone.push(Some(MetadataCandidate {
                from_fingerprint: true,
                id: id.into(),
                kind: MetadataMatchKind::Recording,
                title: recording["title"].as_str().unwrap_or(id).into(),
                detail: artists(recording),
            }));
        }
    }
    // The original album comes first, so a lookup that picks the first row fills its tags.
    releases.sort_by_key(|(rank, _)| *rank);
    let mut candidates = Vec::new();
    for found in releases
        .into_iter()
        .map(|(_, candidate)| candidate)
        .chain(lone)
        .flatten()
    {
        if candidates.len() < 20
            && !candidates
                .iter()
                .any(|c: &MetadataCandidate| c.id == found.id)
        {
            candidates.push(found);
        }
    }
    Ok(FingerprintMatches {
        candidates,
        recordings,
    })
}

/// A release the recording is on, ranked: studio albums, then EPs, singles and other types,
/// compilations and live releases last, each oldest first.
fn release_row(
    recording: &Value,
    group: &Value,
    release: &Value,
) -> ((bool, u8, u16), Option<MetadataCandidate>) {
    let kind = group["type"].as_str().unwrap_or_default();
    let secondary = group["secondarytypes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let year = release["date"]["year"]
        .as_u64()
        .and_then(|year| u16::try_from(year).ok());
    let rank = (
        !secondary.is_empty(),
        match kind {
            "Album" => 0,
            "EP" => 1,
            "Single" => 2,
            _ => 3,
        },
        year.unwrap_or(u16::MAX),
    );
    let mut detail = [release, group, recording]
        .into_iter()
        .map(artists)
        .filter(|artist| !artist.is_empty())
        .take(1)
        .collect::<Vec<_>>();
    detail.extend(year.map(|year| year.to_string()));
    if !kind.is_empty() {
        detail.push(
            std::iter::once(kind)
                .chain(secondary)
                .collect::<Vec<_>>()
                .join(" + "),
        );
    }
    detail.extend(release["country"].as_str().map(str::to_string));
    let mut formats = Vec::new();
    for format in release["mediums"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|medium| medium["format"].as_str())
    {
        if !formats.contains(&format) {
            formats.push(format);
        }
    }
    if !formats.is_empty() {
        detail.push(formats.join(" + "));
    }
    let row = release["id"].as_str().map(|id| MetadataCandidate {
        from_fingerprint: true,
        id: id.into(),
        kind: MetadataMatchKind::Release,
        title: release["title"]
            .as_str()
            .or(group["title"].as_str())
            .unwrap_or(id)
            .into(),
        detail: detail.join(" · "),
    });
    (rank, row)
}

fn artists(entity: &Value) -> String {
    entity["artists"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|artist| artist["name"].as_str())
        .collect::<Vec<_>>()
        .join("; ")
}
