//! The artists ListenBrainz's listeners play in the same sessions as an artist.

use library::SimilarArtist;
use reqwest::Url;
use serde_json::Value;

use crate::cache::{LookupCache, cache_key};
use crate::http::{client, fetch_optional_json};

/// The session-based data set ListenBrainz's own radio reads.
const ALGORITHM: &str =
    "session_based_days_9000_session_300_contribution_5_threshold_15_limit_50_skip_30";

/// The artists similar to `artist_mbid`, closest first; none when ListenBrainz knows the
/// artist no neighbours.
pub fn similar_artists(
    cache: &LookupCache,
    artist_mbid: &str,
) -> Result<Vec<SimilarArtist>, String> {
    let key = cache_key("listenbrainz-ranked-artists", &artist_mbid);
    let similar = cache.fetch(&key, 30, || {
        let url = Url::parse_with_params(
            "https://labs.api.listenbrainz.org/similar-artists/json",
            [("artist_mbids", artist_mbid), ("algorithm", ALGORITHM)],
        )
        .map_err(|error| error.to_string())?;
        Ok(ranked(
            fetch_optional_json(client()?, url, "ListenBrainz similar artists")?.as_ref(),
            artist_mbid,
        ))
    })?;
    Ok(similar.unwrap_or_default())
}

/// The answer's artists by their score, which it does not always sort by. A score counts
/// shared sessions and runs from tens to thousands, so closeness is its share of the list's
/// best, evened out by a square root.
fn ranked(answer: Option<&Value>, seed: &str) -> Vec<SimilarArtist> {
    let mut scored = answer
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|artist| {
            let mbid = artist.get("artist_mbid")?.as_str()?;
            let score = artist.get("score")?.as_f64().filter(|score| *score > 0.0)?;
            (mbid != seed).then(|| {
                (
                    score,
                    mbid.to_owned(),
                    artist
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                )
            })
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
    });
    scored.dedup_by(|next, kept| next.1 == kept.1);
    let best = scored.first().map_or(1.0, |artist| artist.0);
    scored
        .into_iter()
        .map(|(score, mbid, name)| SimilarArtist {
            mbid,
            name,
            closeness: (score / best).sqrt() as f32,
        })
        .collect()
}
