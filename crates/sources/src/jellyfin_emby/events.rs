//! Jellyfin's concrete library-change feed.
//!
//! The socket carries only hints. HTTP resolution in `refresh` produces the
//! finite canonical update; disconnected or folder-wide intervals widen to a
//! complete source read owned by Rufin.

use base64::{Engine as _, engine::general_purpose};
use futures_util::{SinkExt, StreamExt};
use getrandom::fill;
use reqwest::StatusCode;
use serde_json::Value;
use std::time::Instant;
use tokio::time::{Duration, interval, sleep};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};
use tracing::{debug, warn};

use super::*;
use crate::RemoteItemChange;
use crate::remote_json::{id, items};
use crate::source::LIVE_CHANGE_LIMIT;

const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);
const JELLYFIN_WEBSOCKET_KEY_BYTES: usize = 16;
const FEED_RETRY_MIN: Duration = Duration::from_secs(5);
const FEED_RETRY_MAX: Duration = Duration::from_secs(60);

impl JellyfinEmbySource {
    async fn connect_library_socket(&self) -> SourceResult<WebSocketStream<reqwest::Upgraded>> {
        let key = websocket_key()?;
        let mut url = endpoint(
            &self.socket_base_url,
            match self.kind {
                ServerKind::Jellyfin => "socket",
                ServerKind::Emby => "embywebsocket",
            },
        )?;
        if self.kind == ServerKind::Emby {
            self.send_unit(self.client.post(endpoint(&self.base_url, "Sessions/Capabilities/Full")?).json(&serde_json::json!({
                "PlayableMediaTypes": ["Audio"], "SupportedCommands": [], "SupportsMediaControl": false, "SupportsSync": false
            }))).await?;
            let token = self.session_access_token().await?;
            url.query_pairs_mut()
                .append_pair("api_key", token)
                .append_pair("deviceId", &self.device_id);
        }
        debug!(
            service = self.kind.source_kind(),
            method = "GET",
            endpoint = url.path(),
            "sending WebSocket upgrade request"
        );
        let started = Instant::now();
        let response = self
            .authenticated(build_websocket_client(self.trust_invalid_cert)?.get(url))
            .await?
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", key)
            .send()
            .await
            .map_err(|error| SourceError::Network(error.without_url().to_string()))?;
        debug!(
            service = self.kind.source_kind(),
            method = "GET",
            endpoint = "/socket",
            status = response.status().as_u16(),
            elapsed_ms = started.elapsed().as_millis(),
            "received WebSocket upgrade response"
        );
        if response.status() != StatusCode::SWITCHING_PROTOCOLS {
            return Err(SourceError::Server {
                status: response.status().as_u16(),
                message: format!("{} WebSocket upgrade was rejected", self.kind.name()),
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
        on_change: &mut (dyn FnMut(RemoteItemChange) -> bool + Send),
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
                    warn!(%error, "Library change feed disconnected");
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
        on_change: &mut (dyn FnMut(RemoteItemChange) -> bool + Send),
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
                        Message::Text(text) => match library_socket_message(&text, &self.user_id)? {
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

#[derive(Debug, Eq, PartialEq)]
enum JellyfinSocketMessage {
    Change(RemoteItemChange),
    ForceKeepAlive,
    Other,
}

fn library_socket_message(text: &str, user_id: &str) -> SourceResult<JellyfinSocketMessage> {
    let message = serde_json::from_str::<Value>(text)
        .map_err(|error| SourceError::Other(error.to_string()))?;
    match message["MessageType"].as_str() {
        Some("UserDataChanged") => {
            let data = &message["Data"];
            if id(&data["UserId"]).as_deref() != Some(user_id) {
                return Ok(JellyfinSocketMessage::Other);
            }
            let mut upserts = items(&data["UserDataList"])
                .iter()
                .filter_map(|item| id(&item["ItemId"]))
                .collect::<Vec<_>>();
            upserts.sort();
            upserts.dedup();
            if upserts.is_empty() {
                Ok(JellyfinSocketMessage::Other)
            } else if upserts.len() > LIVE_CHANGE_LIMIT {
                Ok(JellyfinSocketMessage::Change(
                    RemoteItemChange::BoundaryLost,
                ))
            } else {
                Ok(JellyfinSocketMessage::Change(RemoteItemChange::UserData {
                    upserts,
                }))
            }
        }
        Some("LibraryChanged") => {
            let data = &message["Data"];
            let ids = |key: &str| items(&data[key]).iter().filter_map(id);
            let folder_change = ["FoldersAddedTo", "FoldersRemovedFrom", "CollectionFolders"]
                .iter()
                .any(|key| ids(key).next().is_some());
            let mut upserts = ids("ItemsAdded")
                .chain(ids("ItemsUpdated"))
                .collect::<Vec<_>>();
            upserts.sort();
            upserts.dedup();
            let mut removals = ids("ItemsRemoved").collect::<Vec<_>>();
            removals.sort();
            removals.dedup();
            if upserts.len().saturating_add(removals.len()) > LIVE_CHANGE_LIMIT {
                Ok(JellyfinSocketMessage::Change(
                    RemoteItemChange::BoundaryLost,
                ))
            } else if upserts.is_empty() && removals.is_empty() && folder_change {
                Ok(JellyfinSocketMessage::Change(
                    RemoteItemChange::BoundaryLost,
                ))
            } else if upserts.is_empty() && removals.is_empty() {
                Ok(JellyfinSocketMessage::Other)
            } else if upserts.iter().any(|id| removals.binary_search(id).is_ok()) {
                Ok(JellyfinSocketMessage::Change(
                    RemoteItemChange::BoundaryLost,
                ))
            } else {
                Ok(JellyfinSocketMessage::Change(RemoteItemChange::Items {
                    upserts,
                    removals,
                }))
            }
        }
        Some("ForceKeepAlive") => Ok(JellyfinSocketMessage::ForceKeepAlive),
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
    fn user_data_changes_refresh_only_the_connected_users_items() {
        let event = r#"{"MessageType":"UserDataChanged","Data":{"UserId":"user","UserDataList":[{"ItemId":"album","IsFavorite":true},{"ItemId":42,"PlayCount":3},{"ItemId":"album"},{"ItemId":null}]}}"#;
        assert_eq!(
            library_socket_message(event, "user").unwrap(),
            JellyfinSocketMessage::Change(RemoteItemChange::UserData {
                upserts: vec!["42".into(), "album".into()],
            })
        );
        assert_eq!(
            library_socket_message(event, "another-user").unwrap(),
            JellyfinSocketMessage::Other
        );
        for event in [
            r#"{"MessageType":"UserDataChanged","Data":{"UserId":"user","UserDataList":[]}}"#,
            r#"{"MessageType":"UserDataChanged","Data":{"UserDataList":[{"ItemId":"album"}]}}"#,
        ] {
            assert_eq!(
                library_socket_message(event, "user").unwrap(),
                JellyfinSocketMessage::Other
            );
        }
    }

    #[tokio::test]
    async fn user_data_event_refreshes_one_album_without_replacing_the_catalog() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        for kind in [ServerKind::Jellyfin, ServerKind::Emby] {
            let server = MockServer::start().await;
            let item_path = match kind {
                ServerKind::Jellyfin => "/Items/3272",
                ServerKind::Emby => "/emby/Users/user/Items/3272",
            };
            Mock::given(method("GET"))
                .and(path(item_path))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "Id":"3272", "Name":"AG! Calling", "Type":"MusicAlbum",
                    "UserData":{"IsFavorite":true}
                })))
                .expect(2)
                .mount(&server)
                .await;
            let source = JellyfinEmbySource::open(
                JellyfinEmbySourceConfig {
                    kind,
                    emby_connect: false,
                    base_url: server.uri(),
                    server_id: None,
                    user_id: "user".into(),
                    username: "listener".into(),
                    trust_invalid_cert: false,
                    use_instant_mix: false,
                },
                "token".into(),
                "device".into(),
            )
            .unwrap();
            let root = tempfile::tempdir().unwrap();
            let database = library::Database::open(root.path().join("library.sqlite"))
                .await
                .unwrap();
            let mut scan =
                library::Scan::begin(&database, "source", kind.name(), kind.source_kind(), None)
                    .await
                    .unwrap();
            for (id, name) in [("3272", "AG! Calling"), ("other", "Another Album")] {
                stage_album(
                    &mut scan,
                    album_from_item(kind, serde_json::json!({"Id":id,"Name":name})).unwrap(),
                )
                .await
                .unwrap();
            }
            scan.finish().await.unwrap();
            let event = r#"{"MessageType":"UserDataChanged","Data":{"UserId":"user","UserDataList":[{"ItemId":"3272","IsFavorite":true}]}}"#;
            for repeated in [false, true] {
                let JellyfinSocketMessage::Change(RemoteItemChange::UserData { upserts }) =
                    library_socket_message(event, "user").unwrap()
                else {
                    panic!("user data must retain the item boundary");
                };
                let outcome = source
                    .apply_user_data(&database, "source", upserts)
                    .await
                    .unwrap();
                assert_eq!(
                    matches!(outcome, library::ScanOutcome::Identical(_)),
                    repeated
                );
            }
            let cancellation = library::ReadCancellation::new();
            for (raw, favorite) in [("3272", true), ("other", false)] {
                let uri = library::source_entity_uri(
                    &crate::SourceId::new("source"),
                    "album",
                    &kind.object_id("album", raw),
                );
                let album = database
                    .album_row_by_media_uri(&uri, &cancellation)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(album.favorite, favorite);
            }
        }
    }

    #[test]
    fn optional_notification_fields_do_not_discard_usable_changes() {
        let message = library_socket_message(
            r#"{"MessageType":"LibraryChanged","Data":{"ItemsAdded":[null,"added",{},42],"ItemsUpdated":false,"ItemsRemoved":[false,"removed", ""],"CollectionFolders":{},"FoldersAddedTo":[null]}}"#,
            "user",
        )
        .unwrap();
        assert_eq!(
            message,
            JellyfinSocketMessage::Change(RemoteItemChange::Items {
                upserts: vec!["42".into(), "added".into()],
                removals: vec!["removed".into()],
            })
        );
        for text in [
            r#"{"MessageType":42,"Data":{"ItemsAdded":false}}"#,
            r#"{"MessageType":"LibraryChanged","Data":null}"#,
            r#"{"MessageType":"SomethingElse","Data":false}"#,
        ] {
            assert_eq!(
                library_socket_message(text, "user").unwrap(),
                JellyfinSocketMessage::Other
            );
        }
        assert_eq!(
            library_socket_message(r#"{"MessageType":"ForceKeepAlive","Data":false}"#, "user")
                .unwrap(),
            JellyfinSocketMessage::ForceKeepAlive
        );
        assert!(library_socket_message("not JSON", "user").is_err());
    }

    #[test]
    fn exact_item_ids_remain_authoritative_with_folder_context() {
        let message = library_socket_message(
            r#"{"MessageType":"LibraryChanged","Data":{"ItemsAdded":["item-one"],"ItemsUpdated":["item-two","item-one"],"ItemsRemoved":["item-three"],"FoldersAddedTo":["folder-one"]}}"#,
            "user",
        )
        .expect("parse message");

        assert_eq!(
            message,
            JellyfinSocketMessage::Change(RemoteItemChange::Items {
                upserts: vec!["item-one".to_string(), "item-two".to_string()],
                removals: vec!["item-three".to_string()],
            })
        );
    }

    #[test]
    fn folder_only_change_loses_the_item_boundary() {
        let message = library_socket_message(
            r#"{"MessageType":"LibraryChanged","Data":{"FoldersAddedTo":["folder-one"]}}"#,
            "user",
        )
        .expect("parse message");

        assert_eq!(
            message,
            JellyfinSocketMessage::Change(RemoteItemChange::BoundaryLost)
        );
    }

    #[test]
    fn library_update_preserves_upserts_and_removals() {
        let message = library_socket_message(
            r#"{"MessageType":"LibraryChanged","Data":{"ItemsAdded":["item-one"],"ItemsUpdated":["item-two","item-one"],"ItemsRemoved":["item-three"]}}"#,
            "user",
        )
        .expect("parse message");

        assert_eq!(
            message,
            JellyfinSocketMessage::Change(RemoteItemChange::Items {
                upserts: vec!["item-one".to_string(), "item-two".to_string()],
                removals: vec!["item-three".to_string()],
            })
        );
    }

    #[test]
    fn conflicting_item_change_widens_to_full() {
        let message = library_socket_message(
            r#"{"MessageType":"LibraryChanged","Data":{"ItemsUpdated":["item-one"],"ItemsRemoved":["item-one"]}}"#,
            "user",
        )
        .expect("parse message");

        assert_eq!(
            message,
            JellyfinSocketMessage::Change(RemoteItemChange::BoundaryLost)
        );
    }

    #[test]
    fn empty_library_update_emits_no_change() {
        let message = library_socket_message(
            r#"{"MessageType":"LibraryChanged","Data":{"ItemsAdded":[],"ItemsUpdated":[],"ItemsRemoved":[]}}"#,
            "user",
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
        let message = library_socket_message(
            &format!(r#"{{"MessageType":"LibraryChanged","Data":{{"ItemsUpdated":[{ids}]}}}}"#),
            "user",
        )
        .expect("parse message");

        assert_eq!(
            message,
            JellyfinSocketMessage::Change(RemoteItemChange::BoundaryLost)
        );
    }
}
