use crate::http::{client, decode_json, wait_for_slot};
use crate::{MetadataCandidate, MetadataMatchKind};
use serde_json::Value;
use std::{sync::Mutex, time::Duration};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct FingerprintMatches {
    pub candidates: Vec<MetadataCandidate>,
    pub recordings: Vec<String>,
}

pub fn lookup_fingerprint(
    key: &str,
    fingerprint: &str,
    duration: u64,
) -> Result<FingerprintMatches, String> {
    static NEXT: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    wait_for_slot(&NEXT, Duration::from_millis(334));
    let response = client()?
        .post("https://api.acoustid.org/v2/lookup")
        .form(&[
            ("client", key),
            ("fingerprint", fingerprint),
            ("duration", &duration.to_string()),
            ("meta", "recordings releases"),
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
    let mut matches = FingerprintMatches {
        candidates: Vec::new(),
        recordings: Vec::new(),
    };
    for recording in value["results"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|result| result["recordings"].as_array().into_iter().flatten())
    {
        let Some(id) = recording["id"].as_str() else {
            continue;
        };
        if !matches.recordings.iter().any(|known| known == id) {
            matches.recordings.push(id.into());
        }
        let artist = recording["artists"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|artist| artist["name"].as_str())
            .collect::<Vec<_>>()
            .join("; ");
        for release in recording["releases"].as_array().into_iter().flatten() {
            add_candidate(
                &mut matches.candidates,
                release,
                MetadataMatchKind::Release,
                &artist,
            );
        }
        add_candidate(
            &mut matches.candidates,
            recording,
            MetadataMatchKind::Recording,
            &artist,
        );
    }
    Ok(matches)
}

fn add_candidate(
    candidates: &mut Vec<MetadataCandidate>,
    entity: &Value,
    kind: MetadataMatchKind,
    artist: &str,
) {
    let Some(id) = entity["id"].as_str() else {
        return;
    };
    if candidates.len() < 20 && !candidates.iter().any(|candidate| candidate.id == id) {
        candidates.push(MetadataCandidate {
            from_fingerprint: true,
            id: id.into(),
            kind,
            title: entity["title"].as_str().unwrap_or(id).into(),
            detail: artist.into(),
        });
    }
}
