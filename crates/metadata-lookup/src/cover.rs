//! External album artwork discovery.

use reqwest::Url;
use serde_json::Value;

use crate::http::{client, download, fetch_optional_json};
use crate::musicbrainz::{search_album_release_group_ids, search_album_release_ids, usable_mbid};

const LASTFM_API_URL: &str = "https://ws.audioscrobbler.com/2.0/";
const LASTFM_IMAGE_HOST: &str = "lastfm.freetls.fastly.net";
const LASTFM_PLACEHOLDER_IMAGE_ID: &str = "2a96cbd8b46e442fc41c2b86b821562f";
const RELEASE_URL: &str = "https://coverartarchive.org/release";
const RELEASE_GROUP_URL: &str = "https://coverartarchive.org/release-group";

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AlbumCover {
    artist: Option<String>,
    album: Option<String>,
    release_group_id: Option<String>,
    release_id: Option<String>,
}

impl AlbumCover {
    pub fn new(
        artist: &str,
        album: &str,
        release_group_id: Option<&str>,
        release_id: Option<&str>,
    ) -> Option<Self> {
        let artist = lookup_text(artist).map(ToString::to_string);
        let album = lookup_text(album).map(ToString::to_string);
        let release_group_id = release_group_id
            .and_then(usable_mbid)
            .map(ToString::to_string);
        let release_id = release_id.and_then(usable_mbid).map(ToString::to_string);
        (release_group_id.is_some() || release_id.is_some() || artist.is_some() && album.is_some())
            .then_some(Self {
                artist,
                album,
                release_group_id,
                release_id,
            })
    }

    pub fn stable_identity(&self) -> String {
        format!(
            "album\0{}\0{}\0{}\0{}",
            self.artist.as_deref().unwrap_or_default(),
            self.album.as_deref().unwrap_or_default(),
            self.release_group_id.as_deref().unwrap_or_default(),
            self.release_id.as_deref().unwrap_or_default()
        )
    }

    fn text(&self) -> Option<(&str, &str)> {
        Some((self.artist.as_deref()?, self.album.as_deref()?))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AlbumCoverPolicy {
    pub lastfm_api_key: String,
    pub allow_musicbrainz: bool,
}

impl AlbumCoverPolicy {
    pub fn new(lastfm_api_key: impl Into<String>, allow_musicbrainz: bool) -> Self {
        Self {
            lastfm_api_key: lastfm_api_key.into(),
            allow_musicbrainz,
        }
    }
}

/// Finds cover bytes for an ordinary Rufin artwork binding.
///
/// Exact Cover Art Archive identities remain ahead of Last.fm and text
/// searches so a known album identity cannot be replaced by an approximate
/// result.
/// A missing size requests the original Cover Art Archive image.
pub fn lookup_album_cover(
    album: &AlbumCover,
    size: Option<u32>,
    policy: &AlbumCoverPolicy,
) -> Result<Option<Vec<u8>>, String> {
    let client = client()?;
    let mut failures = Vec::new();
    if policy.allow_musicbrainz {
        for (root, id) in [
            (RELEASE_GROUP_URL, album.release_group_id.as_deref()),
            (RELEASE_URL, album.release_id.as_deref()),
        ] {
            if let Some(id) = id {
                match download(client, &cover_art_url(root, id, size), "album cover") {
                    Ok(Some(bytes)) => return Ok(Some(bytes)),
                    Ok(None) => {}
                    Err(error) => failures.push(error),
                }
            }
        }
    }
    if let Some((artist, title)) = album.text() {
        if !policy.lastfm_api_key.trim().is_empty() {
            match lastfm_url(client, artist, title, &policy.lastfm_api_key) {
                Ok(Some(url)) => match download(client, &url, "album cover") {
                    Ok(Some(bytes)) => return Ok(Some(bytes)),
                    Ok(None) => {}
                    Err(error) => failures.push(error),
                },
                Ok(None) => {}
                Err(error) => failures.push(error),
            }
        }
        if policy.allow_musicbrainz {
            match search_album_release_group_ids(artist, title) {
                Ok(ids) => {
                    if let Some(bytes) =
                        download_identities(client, RELEASE_GROUP_URL, ids, size, &mut failures)
                    {
                        return Ok(Some(bytes));
                    }
                }
                Err(error) => failures.push(error),
            }
            match search_album_release_ids(artist, title) {
                Ok(ids) => {
                    if let Some(bytes) =
                        download_identities(client, RELEASE_URL, ids, size, &mut failures)
                    {
                        return Ok(Some(bytes));
                    }
                }
                Err(error) => failures.push(error),
            }
        }
    }
    missing_or_errors(failures)
}

/// Finds a public URL suitable for Discord rich presence.
///
/// Discord cannot use Rufin's cached bytes, so its established Last.fm-first
/// order intentionally differs from visible artwork selection.
pub fn public_album_cover_url(
    album: &AlbumCover,
    size: u32,
    policy: &AlbumCoverPolicy,
) -> Result<Option<String>, String> {
    let client = client()?;
    let mut failures = Vec::new();
    if !policy.lastfm_api_key.trim().is_empty()
        && let Some((artist, title)) = album.text()
    {
        match lastfm_url(client, artist, title, &policy.lastfm_api_key) {
            Ok(Some(url)) => return Ok(Some(url)),
            Ok(None) => {}
            Err(error) => failures.push(error),
        }
    }
    if policy.allow_musicbrainz {
        for (root, id) in [
            (RELEASE_GROUP_URL, album.release_group_id.as_deref()),
            (RELEASE_URL, album.release_id.as_deref()),
        ] {
            if let Some(id) = id {
                return Ok(Some(cover_art_url(root, id, Some(size))));
            }
        }
        if let Some((artist, title)) = album.text() {
            match search_album_release_ids(artist, title) {
                Ok(ids) => {
                    if let Some(id) = ids.into_iter().find_map(|id| {
                        usable_mbid(&id).map(|valid| cover_art_url(RELEASE_URL, valid, Some(size)))
                    }) {
                        return Ok(Some(id));
                    }
                }
                Err(error) => failures.push(error),
            }
        }
    }
    missing_or_errors(failures)
}

fn download_identities(
    client: &reqwest::blocking::Client,
    root: &str,
    ids: Vec<String>,
    size: Option<u32>,
    failures: &mut Vec<String>,
) -> Option<Vec<u8>> {
    for id in ids {
        let Some(id) = usable_mbid(&id) else {
            continue;
        };
        match download(client, &cover_art_url(root, id, size), "album cover") {
            Ok(Some(bytes)) => return Some(bytes),
            Ok(None) => {}
            Err(error) => failures.push(error),
        }
    }
    None
}

fn lastfm_url(
    client: &reqwest::blocking::Client,
    artist: &str,
    album: &str,
    api_key: &str,
) -> Result<Option<String>, String> {
    let url = Url::parse_with_params(
        LASTFM_API_URL,
        &[
            ("method", "album.getinfo"),
            ("api_key", api_key),
            ("artist", artist),
            ("album", album),
            ("format", "json"),
            ("autocorrect", "1"),
        ],
    )
    .map_err(|error| error.to_string())?;
    lastfm_image_url(client, url)
}

fn lastfm_image_url(
    client: &reqwest::blocking::Client,
    url: Url,
) -> Result<Option<String>, String> {
    let Some(value) = fetch_optional_json(client, url, "Last.fm album lookup")? else {
        return Ok(None);
    };
    let images = value
        .pointer("/album/image")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for wanted in ["mega", "extralarge", "large", "medium"] {
        for image in &images {
            if image.get("size").and_then(Value::as_str) != Some(wanted) {
                continue;
            }
            let Some(url) = image.get("#text").and_then(Value::as_str) else {
                continue;
            };
            if let Some(url) = public_lastfm_url(url) {
                return Ok(Some(url));
            }
        }
    }
    Ok(None)
}

fn cover_art_url(root: &str, id: &str, size: Option<u32>) -> String {
    let suffix = match size {
        None => "front",
        Some(size) if size <= 250 => "front-250",
        Some(_) => "front-500",
    };
    format!("{root}/{id}/{suffix}")
}

fn public_lastfm_url(raw: &str) -> Option<String> {
    if raw.contains(LASTFM_PLACEHOLDER_IMAGE_ID) {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    (url.scheme() == "https"
        && url.host_str() == Some(LASTFM_IMAGE_HOST)
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.to_string())
}

fn lookup_text(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty()
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "unknown" | "unknown album" | "unknown artist" | "untitled album" | "untitled track"
        )
    {
        None
    } else {
        Some(value)
    }
}

fn missing_or_errors<T>(failures: Vec<String>) -> Result<Option<T>, String> {
    if failures.is_empty() {
        Ok(None)
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    fn serve_status(status: &'static str) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind metadata server");
        let address = listener.local_addr().expect("metadata server address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept metadata request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).expect("read metadata request");
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write metadata response");
        });
        (
            Url::parse(&format!("http://{address}/2.0/")).expect("metadata URL"),
            server,
        )
    }

    #[test]
    fn lastfm_urls_are_limited_to_the_public_image_host() {
        assert!(
            public_lastfm_url("https://lastfm.freetls.fastly.net/i/u/300x300/cover.jpg").is_some()
        );
        assert!(public_lastfm_url("https://example.com/cover.jpg").is_none());
        assert!(public_lastfm_url("http://lastfm.freetls.fastly.net/cover.jpg").is_none());
        assert!(
            public_lastfm_url(
                "https://lastfm.freetls.fastly.net/i/u/300x300/2a96cbd8b46e442fc41c2b86b821562f.png"
            )
            .is_none()
        );
    }

    #[test]
    fn public_url_prefers_the_release_group_without_a_remote_lookup() {
        let album = AlbumCover::new(
            "Artist",
            "Album",
            Some("11111111-1111-1111-1111-111111111111"),
            Some("22222222-2222-2222-2222-222222222222"),
        )
        .expect("album cover");
        let url = public_album_cover_url(&album, 250, &AlbumCoverPolicy::new("", true))
            .unwrap_or_else(|error| panic!("public URL: {error}"));

        assert_eq!(
            url.as_deref(),
            Some(
                "https://coverartarchive.org/release-group/11111111-1111-1111-1111-111111111111/front-250"
            )
        );
    }

    #[test]
    fn lastfm_not_found_is_missing_but_server_failures_remain_errors() {
        let (not_found, server) = serve_status("404 Not Found");
        assert_eq!(
            lastfm_image_url(client().expect("HTTP client"), not_found),
            Ok(None)
        );
        server.join().expect("404 metadata server");

        let (server_error, server) = serve_status("500 Internal Server Error");
        let error = lastfm_image_url(client().expect("HTTP client"), server_error)
            .expect_err("server failure must remain visible");
        assert!(error.contains("status 500"));
        server.join().expect("500 metadata server");
    }
}
