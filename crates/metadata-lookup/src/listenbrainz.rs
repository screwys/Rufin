//! The artists ListenBrainz's listeners play alongside an artist.

use reqwest::Url;

use crate::cache::{LookupCache, cache_key};
use crate::http::{client, fetch_optional_json};

/// The MusicBrainz ids of the artists similar to `artist_mbid`, as ListenBrainz's artist radio
/// names them; none when it knows the artist no neighbours.
pub fn similar_artists(cache: &LookupCache, artist_mbid: &str) -> Result<Vec<String>, String> {
    let key = cache_key("listenbrainz-similar-artists", &artist_mbid);
    let similar = cache.fetch(&key, || {
        let mut url = Url::parse("https://api.listenbrainz.org/1/lb-radio/artist")
            .map_err(|error| error.to_string())?;
        url.path_segments_mut()
            .map_err(|()| "ListenBrainz address has no path".to_owned())?
            .push(artist_mbid);
        url.query_pairs_mut()
            .append_pair("mode", "easy")
            .append_pair("max_similar_artists", "30")
            .append_pair("max_recordings_per_artist", "1")
            .append_pair("pop_begin", "0")
            .append_pair("pop_end", "100");
        // The answer is keyed by artist, the seed among them.
        Ok(
            fetch_optional_json(client()?, url, "ListenBrainz similar artists")?
                .as_ref()
                .and_then(|artists| artists.as_object())
                .map(|artists| {
                    artists
                        .keys()
                        .filter(|mbid| *mbid != artist_mbid)
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        )
    })?;
    Ok(similar.unwrap_or_default())
}
