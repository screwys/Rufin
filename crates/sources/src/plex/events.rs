use super::*;
use crate::source::{FEED_PING_INTERVAL, FEED_SILENCE_LIMIT, RemoteItemChange};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

impl PlexSource {
    async fn listen_once(
        &self,
        ready: &mut (impl FnMut() -> bool + Send),
        changed: &mut (impl FnMut(RemoteItemChange) -> bool + Send),
    ) -> SourceResult<bool> {
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|error| SourceError::Other(error.to_string()))?;
        let response = self
            .request(Method::GET, "/:/websockets/notifications", &[])
            .await?
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                base64::engine::general_purpose::STANDARD.encode(random),
            )
            .send()
            .await
            .map_err(|error| remote_http::map_reqwest_error(error, HTTP))?;
        if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
            return Err(SourceError::Server {
                status: response.status().as_u16(),
                message: "Plex notification connection was rejected".into(),
            });
        }
        let upgraded = response
            .upgrade()
            .await
            .map_err(|error| remote_http::map_reqwest_error(error, HTTP))?;
        let mut socket = WebSocketStream::from_raw_socket(upgraded, Role::Client, None).await;
        if !ready() {
            return Ok(false);
        }
        let network =
            |error: tokio_tungstenite::tungstenite::Error| SourceError::Network(error.to_string());
        let mut ping = tokio::time::interval(FEED_PING_INTERVAL);
        let mut heard = std::time::Instant::now();
        loop {
            let message = tokio::select! {
                _ = ping.tick() => {
                    socket.send(Message::Ping(Vec::new().into())).await.map_err(network)?;
                    continue;
                }
                () = tokio::time::sleep_until((heard + FEED_SILENCE_LIMIT).into()) => {
                    return Err(SourceError::Network("Plex notifications went silent".into()));
                }
                message = socket.next() => message,
            };
            let Some(message) = message else {
                return Ok(true);
            };
            heard = std::time::Instant::now();
            match message.map_err(network)? {
                Message::Text(text) => {
                    if let Some(change) = notification(&text)
                        && !changed(change)
                    {
                        return Ok(false);
                    }
                }
                Message::Ping(payload) => {
                    socket.send(Message::Pong(payload)).await.map_err(network)?
                }
                Message::Close(_) => return Ok(true),
                _ => {}
            }
        }
    }
    /// Listens until `ready` or `changed` returns false. `ready` runs on each connect; the
    /// changes missed while disconnected are caught up from there.
    pub(crate) async fn listen_library_changes(
        &self,
        mut ready: impl FnMut() -> bool + Send,
        mut changed: impl FnMut(RemoteItemChange) -> bool + Send,
    ) {
        loop {
            match self.listen_once(&mut ready, &mut changed).await {
                Ok(false) => return,
                Ok(true) => {}
                Err(error) => tracing::warn!(%error, "Plex notification feed disconnected"),
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }
}

fn notification(text: &str) -> Option<RemoteItemChange> {
    let value: Value = serde_json::from_str(text).ok()?;
    let container = &value["NotificationContainer"];
    let mut upserts = Vec::new();
    let mut removals = Vec::new();
    for entry in crate::remote_json::items(&container["TimelineEntry"])
        .iter()
        .chain(crate::remote_json::items(&container["_children"]))
    {
        if entry["identifier"] != "com.plexapp.plugins.library" {
            continue;
        }
        let state = crate::remote_json::field::<i64>(entry, "state");
        let Some(id) = crate::remote_json::id(&entry["itemID"])
            .or_else(|| crate::remote_json::id(&entry["ratingKey"]))
        else {
            continue;
        };
        let id = if crate::remote_json::field::<u8>(entry, "type") == Some(15) {
            super::plex_id("playlist", &id)
        } else {
            id
        };
        if state == Some(9) {
            removals.push(id);
        } else if state.is_some_and(|state| (0..=5).contains(&state)) {
            upserts.push(id);
            for key in ["parentItemID", "rootItemID"] {
                if let Some(id) = crate::remote_json::id(&entry[key]) {
                    upserts.push(id);
                }
            }
        }
    }
    (!upserts.is_empty() || !removals.is_empty())
        .then(|| RemoteItemChange::items(upserts, removals))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_web_details_does_not_invalidate_the_catalog() {
        let mut input = serde_json::json!({"NotificationContainer":{
            "type":"activity",
            "ActivityNotification":[{"event":"ended","Activity":{
                "type":"library.refresh.items","progress":100,
                "Context":{"key":"/library/metadata/1763/children",
                    "accessible":true,"exists":true,"analyzed":false,"refreshed":false}
            }}]
        }});
        assert_eq!(notification(&input.to_string()), None);
        input["NotificationContainer"]["TimelineEntry"] = serde_json::json!([
            {"identifier":"com.plexapp.plugins.library","state":5},
            {"identifier":"com.plexapp.plugins.library","state":5,"itemID":"1763"}
        ]);
        assert_eq!(
            notification(&input.to_string()),
            Some(RemoteItemChange::items(vec!["1763".into()], vec![]))
        );
    }

    #[test]
    fn library_hints_include_all_processing_states_and_ignore_playback() {
        let entries:Vec<_>=(0..=5).map(|state|serde_json::json!({"identifier":"com.plexapp.plugins.library","state":state,"itemID":state.to_string()})).collect();
        let input = serde_json::json!({"NotificationContainer":{"TimelineEntry":entries,"PlaySessionStateNotification":[{"ratingKey":"not-catalog","state":"stopped"}]}});
        let Some(RemoteItemChange::Items(items)) = notification(&input.to_string()) else {
            panic!("item hints");
        };
        assert_eq!(
            items.keys().map(String::as_str).collect::<Vec<_>>(),
            ["0", "1", "2", "3", "4", "5"]
        );
        assert!(
            items
                .values()
                .all(|value| matches!(value, crate::RemoteItemUpdate::Metadata))
        );
        let input = serde_json::json!({"NotificationContainer":{"_children":[{"identifier":"com.plexapp.plugins.library","state":9,"itemID":"gone"}]}});
        assert_eq!(
            notification(&input.to_string()),
            Some(RemoteItemChange::items(vec![], vec!["gone".into()]))
        );
    }
}
