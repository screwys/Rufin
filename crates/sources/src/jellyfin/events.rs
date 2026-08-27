//! Jellyfin's concrete library-change feed.
//!
//! The socket carries only hints. HTTP resolution in `refresh` produces the
//! finite canonical update; disconnected or folder-wide intervals widen to a
//! complete source read owned by Rufin.

use base64::{Engine as _, engine::general_purpose};
use futures_util::{SinkExt, StreamExt};
use getrandom::fill;
use reqwest::StatusCode;
use serde::Deserialize;
use std::time::Instant;
use tokio::time::{Duration, interval, sleep};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};
use tracing::{debug, warn};

use super::*;
use crate::JellyfinLiveChange;
use crate::source::LIVE_CHANGE_LIMIT;

const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);
const JELLYFIN_WEBSOCKET_KEY_BYTES: usize = 16;
const FEED_RETRY_MIN: Duration = Duration::from_secs(5);
const FEED_RETRY_MAX: Duration = Duration::from_secs(60);

impl JellyfinSource {
    async fn connect_library_socket(&self) -> SourceResult<WebSocketStream<reqwest::Upgraded>> {
        let key = websocket_key()?;
        let url = endpoint(&self.base_url, "socket")?;
        debug!(
            service = "jellyfin",
            method = "GET",
            %url,
            "sending WebSocket upgrade request"
        );
        let started = Instant::now();
        let response = build_websocket_client(self.trust_invalid_cert)?
            .get(url)
            .header(header::AUTHORIZATION, self.authorization.clone())
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", key)
            .send()
            .await
            .map_err(|error| SourceError::Network(error.to_string()))?;
        debug!(
            service = "jellyfin",
            method = "GET",
            endpoint = "/socket",
            status = response.status().as_u16(),
            elapsed_ms = started.elapsed().as_millis(),
            "received WebSocket upgrade response"
        );
        if response.status() != StatusCode::SWITCHING_PROTOCOLS {
            return Err(SourceError::Server {
                status: response.status().as_u16(),
                message: "Jellyfin WebSocket upgrade was rejected".to_string(),
            });
        }
        let upgraded = response
            .upgrade()
            .await
            .map_err(|error| SourceError::Network(error.to_string()))?;
        Ok(WebSocketStream::from_raw_socket(upgraded, Role::Client, None).await)
    }

    pub(crate) async fn listen_library_changes(
        &self,
        on_ready: &mut (dyn FnMut() -> bool + Send),
        on_gap: &mut (dyn FnMut() -> bool + Send),
        on_change: &mut (dyn FnMut(JellyfinLiveChange) -> bool + Send),
    ) -> SourceResult<()> {
        let mut delay = FEED_RETRY_MIN;
        let mut boundary_established = false;
        let mut gap_reported = false;
        loop {
            let ready = &mut || {
                boundary_established = true;
                gap_reported = false;
                on_ready()
            };
            let keep_listening = match self.listen_library_changes_once(ready, on_change).await {
                Ok(keep_listening) => keep_listening,
                Err(error) => {
                    warn!(%error, "Jellyfin library change feed disconnected");
                    true
                }
            };
            if !keep_listening {
                return Ok(());
            }
            if boundary_established && !gap_reported {
                gap_reported = true;
                if !on_gap() {
                    return Ok(());
                }
            }
            sleep(delay).await;
            delay = delay.saturating_mul(2).min(FEED_RETRY_MAX);
        }
    }

    async fn listen_library_changes_once(
        &self,
        on_ready: &mut (dyn FnMut() -> bool + Send),
        on_change: &mut (dyn FnMut(JellyfinLiveChange) -> bool + Send),
    ) -> SourceResult<bool> {
        let mut socket = self.connect_library_socket().await?;
        if !on_ready() {
            return Ok(false);
        }
        let mut keep_alive = interval(KEEP_ALIVE_INTERVAL);
        loop {
            tokio::select! {
                _ = keep_alive.tick() => {
                    send_keep_alive(&mut socket).await?;
                }
                message = socket.next() => {
                    let Some(message) = message else {
                        return Ok(true);
                    };
                    match message.map_err(websocket_error)? {
                        Message::Text(text) => match library_socket_message(&text)? {
                            JellyfinSocketMessage::Change(change) => {
                                if !on_change(change) {
                                    return Ok(false);
                                }
                            }
                            JellyfinSocketMessage::ForceKeepAlive => {
                                send_keep_alive(&mut socket).await?;
                            }
                            JellyfinSocketMessage::Other => {}
                        },
                        Message::Close(_) => return Ok(true),
                        Message::Ping(payload) => socket
                            .send(Message::Pong(payload))
                            .await
                            .map_err(websocket_error)?,
                        Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {}
                    }
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct SocketMessage {
    #[serde(rename = "MessageType")]
    message_type: String,
    #[serde(rename = "Data", default)]
    data: Option<serde_json::Value>,
}

#[derive(Debug, Eq, PartialEq)]
enum JellyfinSocketMessage {
    Change(JellyfinLiveChange),
    ForceKeepAlive,
    Other,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct LibraryChangedData {
    #[serde(default)]
    items_added: Vec<String>,
    #[serde(default)]
    items_updated: Vec<String>,
    #[serde(default)]
    items_removed: Vec<String>,
    #[serde(default)]
    folders_added_to: Vec<String>,
    #[serde(default)]
    folders_removed_from: Vec<String>,
    #[serde(default)]
    collection_folders: Vec<String>,
}

fn library_socket_message(text: &str) -> SourceResult<JellyfinSocketMessage> {
    let message = serde_json::from_str::<SocketMessage>(text)
        .map_err(|error| SourceError::Other(error.to_string()))?;
    match message.message_type.as_str() {
        "LibraryChanged" => {
            let Some(data) = message.data else {
                return Ok(JellyfinSocketMessage::Other);
            };
            let data = serde_json::from_value::<LibraryChangedData>(data)
                .map_err(|error| SourceError::Other(error.to_string()))?;
            let folder_change = !data.folders_added_to.is_empty()
                || !data.folders_removed_from.is_empty()
                || !data.collection_folders.is_empty();
            let mut upserts = data
                .items_added
                .into_iter()
                .chain(data.items_updated)
                .collect::<Vec<_>>();
            upserts.sort();
            upserts.dedup();
            let mut removals = data.items_removed;
            removals.sort();
            removals.dedup();
            if upserts.len().saturating_add(removals.len()) > LIVE_CHANGE_LIMIT {
                Ok(JellyfinSocketMessage::Change(
                    JellyfinLiveChange::BoundaryLost,
                ))
            } else if upserts.is_empty() && removals.is_empty() && folder_change {
                Ok(JellyfinSocketMessage::Change(
                    JellyfinLiveChange::BoundaryLost,
                ))
            } else if upserts.is_empty() && removals.is_empty() {
                Ok(JellyfinSocketMessage::Other)
            } else if upserts.iter().any(|id| removals.binary_search(id).is_ok()) {
                Ok(JellyfinSocketMessage::Change(
                    JellyfinLiveChange::BoundaryLost,
                ))
            } else {
                Ok(JellyfinSocketMessage::Change(JellyfinLiveChange::Items {
                    upserts,
                    removals,
                }))
            }
        }
        "ForceKeepAlive" => Ok(JellyfinSocketMessage::ForceKeepAlive),
        _ => Ok(JellyfinSocketMessage::Other),
    }
}

async fn send_keep_alive(socket: &mut WebSocketStream<reqwest::Upgraded>) -> SourceResult<()> {
    socket
        .send(Message::Text(
            r#"{"MessageType":"KeepAlive"}"#.to_string().into(),
        ))
        .await
        .map_err(websocket_error)
}

fn websocket_key() -> SourceResult<String> {
    let mut bytes = [0_u8; JELLYFIN_WEBSOCKET_KEY_BYTES];
    fill(&mut bytes).map_err(|error| SourceError::Other(error.to_string()))?;
    Ok(general_purpose::STANDARD.encode(bytes))
}

fn websocket_error(error: tokio_tungstenite::tungstenite::Error) -> SourceError {
    SourceError::Network(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_item_ids_remain_authoritative_with_folder_context() {
        let message = library_socket_message(
            r#"{"MessageType":"LibraryChanged","Data":{"ItemsAdded":["item-one"],"ItemsUpdated":["item-two","item-one"],"ItemsRemoved":["item-three"],"FoldersAddedTo":["folder-one"]}}"#,
        )
        .expect("parse message");

        assert_eq!(
            message,
            JellyfinSocketMessage::Change(JellyfinLiveChange::Items {
                upserts: vec!["item-one".to_string(), "item-two".to_string()],
                removals: vec!["item-three".to_string()],
            })
        );
    }

    #[test]
    fn folder_only_change_loses_the_item_boundary() {
        let message = library_socket_message(
            r#"{"MessageType":"LibraryChanged","Data":{"FoldersAddedTo":["folder-one"]}}"#,
        )
        .expect("parse message");

        assert_eq!(
            message,
            JellyfinSocketMessage::Change(JellyfinLiveChange::BoundaryLost)
        );
    }

    #[test]
    fn library_update_preserves_upserts_and_removals() {
        let message = library_socket_message(
            r#"{"MessageType":"LibraryChanged","Data":{"ItemsAdded":["item-one"],"ItemsUpdated":["item-two","item-one"],"ItemsRemoved":["item-three"]}}"#,
        )
        .expect("parse message");

        assert_eq!(
            message,
            JellyfinSocketMessage::Change(JellyfinLiveChange::Items {
                upserts: vec!["item-one".to_string(), "item-two".to_string()],
                removals: vec!["item-three".to_string()],
            })
        );
    }

    #[test]
    fn conflicting_item_change_widens_to_full() {
        let message = library_socket_message(
            r#"{"MessageType":"LibraryChanged","Data":{"ItemsUpdated":["item-one"],"ItemsRemoved":["item-one"]}}"#,
        )
        .expect("parse message");

        assert_eq!(
            message,
            JellyfinSocketMessage::Change(JellyfinLiveChange::BoundaryLost)
        );
    }

    #[test]
    fn empty_library_update_emits_no_change() {
        let message = library_socket_message(
            r#"{"MessageType":"LibraryChanged","Data":{"ItemsAdded":[],"ItemsUpdated":[],"ItemsRemoved":[]}}"#,
        )
        .expect("parse message");

        assert_eq!(message, JellyfinSocketMessage::Other);
    }

    #[test]
    fn source_sized_item_evidence_loses_the_exact_boundary() {
        let ids = (0..=LIVE_CHANGE_LIMIT)
            .map(|index| format!(r#""item-{index}""#))
            .collect::<Vec<_>>()
            .join(",");
        let message = library_socket_message(&format!(
            r#"{{"MessageType":"LibraryChanged","Data":{{"ItemsUpdated":[{ids}]}}}}"#
        ))
        .expect("parse message");

        assert_eq!(
            message,
            JellyfinSocketMessage::Change(JellyfinLiveChange::BoundaryLost)
        );
    }
}
