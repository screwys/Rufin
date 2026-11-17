//! LISTEN.moe's song feed. Its streams announce a song's name alone; the feed says when each
//! song began and how long it is, which is what gives a radio song a place to time lyrics by.

use std::time::Duration;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio_websockets::{ClientBuilder, Message};

use crate::{SourceError, SourceResult};

/// A song LISTEN.moe plays, as its feed tells it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListenMoeSong {
    pub title: String,
    pub artists: Vec<String>,
    /// The title as the feed also writes it, in Latin letters.
    pub also_titled: Vec<String>,
    /// Who else the song is credited to: its artists in Latin letters, and the characters
    /// they sing as, under either name.
    pub also_credited: Vec<String>,
    pub duration_millis: u64,
    /// When the station began it, by the station's clock: a listener hears it later by the
    /// stream's delay.
    pub started_unix_millis: i64,
}

/// The feed of the LISTEN.moe channel a stream address plays; None for any other stream.
pub fn listen_moe_feed(stream_url: &str) -> Option<&'static str> {
    let url = url::Url::parse(stream_url).ok()?;
    if !matches!(url.host_str()?, "listen.moe" | "www.listen.moe") {
        return None;
    }
    Some(if url.path().starts_with("/kpop") {
        "https://listen.moe/kpop/gateway_v2"
    } else {
        "https://listen.moe/gateway_v2"
    })
}

/// Hands `heard` each song of `feed` as the station begins it, the one that plays first,
/// until `heard` answers false or the feed ends.
pub async fn listen_moe_songs(
    feed: &str,
    mut heard: impl FnMut(ListenMoeSong) -> bool + Send,
) -> SourceResult<()> {
    let network = |error: reqwest::Error| SourceError::Network(error.without_url().to_string());
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|error| SourceError::Other(error.to_string()))?;
    let response = reqwest::Client::builder()
        .http1_only()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(network)?
        .get(feed)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            base64::engine::general_purpose::STANDARD.encode(random),
        )
        .send()
        .await
        .map_err(network)?;
    if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
        return Err(SourceError::Server {
            status: response.status().as_u16(),
            message: "LISTEN.moe's song feed was refused".into(),
        });
    }
    let upgraded = response.upgrade().await.map_err(network)?;
    let mut socket = ClientBuilder::new().take_over(upgraded);
    let closed = |error: tokio_websockets::Error| SourceError::Network(error.to_string());
    // The feed names the interval of the heartbeat it expects, and ends without one.
    let mut beat = tokio::time::interval(Duration::from_secs(30));
    loop {
        let message = tokio::select! {
            _ = beat.tick() => {
                socket.send(Message::text(r#"{"op":9}"#)).await.map_err(closed)?;
                continue;
            }
            message = socket.next() => message,
        };
        let Some(message) = message else {
            return Ok(());
        };
        let message = message.map_err(closed)?;
        if message.is_close() {
            return Ok(());
        }
        let Some(Ok(message)) = message
            .as_text()
            .map(serde_json::from_str::<serde_json::Value>)
        else {
            continue;
        };
        match message["op"].as_u64() {
            Some(0) => {
                if let Some(millis) = message["d"]["heartbeat"].as_u64().filter(|n| *n > 0) {
                    beat = tokio::time::interval(Duration::from_millis(millis));
                }
            }
            Some(1) => {
                if let Some(song) = song(&message["d"])
                    && !heard(song)
                {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}

/// The song of a feed update. The feed's `startTime` is when the song ends: its beginning is
/// that less its length. A song without a length has no place to tell.
fn song(update: &serde_json::Value) -> Option<ListenMoeSong> {
    let song = &update["song"];
    let duration_millis = song["duration"].as_u64().filter(|seconds| *seconds > 0)? * 1000;
    let ends = time::OffsetDateTime::parse(
        update["startTime"].as_str()?,
        &time::format_description::well_known::Rfc3339,
    )
    .ok()?;
    let ends = i64::try_from(ends.unix_timestamp_nanos() / 1_000_000).ok()?;
    let named = |values: &[&serde_json::Value]| {
        values
            .iter()
            .filter_map(|value| value.as_str())
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>()
    };
    let each = |list: &'static str| song[list].as_array().into_iter().flatten();
    Some(ListenMoeSong {
        title: song["title"].as_str()?.trim().to_string(),
        artists: named(
            &each("artists")
                .map(|artist| &artist["name"])
                .collect::<Vec<_>>(),
        ),
        also_titled: named(&[&song["titleRomaji"]]),
        also_credited: named(
            &each("artists")
                .map(|artist| &artist["nameRomaji"])
                .chain(
                    each("characters")
                        .flat_map(|character| [&character["name"], &character["nameRomaji"]]),
                )
                .collect::<Vec<_>>(),
        ),
        duration_millis,
        started_unix_millis: ends - duration_millis as i64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_feed_update_tells_when_its_song_began() {
        let update = serde_json::json!({
            "song": {
                "title": " GO☆Summer Girl ", "titleRomaji": "GO Summer Girl",
                "artists": [{"name": "茅原実里", "nameRomaji": "Chihara Minori", "characters": [{"id": 1}]}],
                "characters": [{"id": 1, "name": "長門有希", "nameRomaji": null}],
                "duration": 316,
            },
            "startTime": "2026-11-15T05:46:01.000Z",
        });
        let song = song(&update).unwrap();
        assert_eq!(song.title, "GO☆Summer Girl");
        assert_eq!(song.artists, ["茅原実里"]);
        assert_eq!(song.also_titled, ["GO Summer Girl"]);
        assert_eq!(song.also_credited, ["Chihara Minori", "長門有希"]);
        assert_eq!(song.duration_millis, 316_000);
        assert_eq!(
            song.started_unix_millis,
            time::macros::datetime!(2026-11-15 05:40:45 UTC).unix_timestamp() * 1000
        );
        assert_eq!(
            listen_moe_feed("https://listen.moe/kpop/fallback"),
            Some("https://listen.moe/kpop/gateway_v2")
        );
        assert_eq!(
            listen_moe_feed("https://ice1.somafm.com/groovesalad-128-mp3"),
            None
        );
    }
}
