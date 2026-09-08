//! Authenticated PMS operations used by the native Companion playback owner.
use super::PlexSource;
use crate::remote_json::{field, id, items};
use crate::{SourceError, SourceId, SourceResult};
use reqwest::Method;
use serde_json::Value;

pub const PLEX_QUEUE_WINDOW: usize = 100;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlexCompanionContext {
    pub source_id: SourceId,
    pub server_id: String,
    pub profile_id: String,
    pub base_url: String,
    pub client_id: String,
    pub trust_invalid_certificate: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlexCompanionPlayer {
    pub endpoint: String,
    pub machine_identifier: String,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlexQueueItem {
    pub occurrence_id: u64,
    pub rating_key: String,
    pub key: String,
    pub media_uri: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlexQueueWindow {
    pub id: u64,
    pub version: u64,
    pub total: usize,
    pub offset: usize,
    pub selected_item_id: Option<u64>,
    pub selected_offset: Option<usize>,
    pub shuffled: bool,
    pub items: Vec<PlexQueueItem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlexQueuePlacement {
    End,
    Next,
    After(u64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlexQueueMutation {
    Create {
        rating_keys: Vec<String>,
    },
    Insert {
        queue_id: u64,
        rating_keys: Vec<String>,
        placement: PlexQueuePlacement,
    },
    Remove {
        queue_id: u64,
        occurrence_id: u64,
    },
    Move {
        queue_id: u64,
        occurrence_id: u64,
        after: Option<u64>,
    },
}

fn queue_window(value: &Value, source_id: &SourceId) -> SourceResult<PlexQueueWindow> {
    let root = &value["MediaContainer"];
    Ok(PlexQueueWindow {
        id: field(root, "playQueueID").ok_or(SourceError::InvalidRequest(
            "Plex returned no play queue ID",
        ))?,
        version: field(root, "playQueueVersion").unwrap_or(0),
        total: field(root, "playQueueTotalCount").unwrap_or(0),
        offset: field(root, "offset").unwrap_or(0),
        selected_item_id: field(root, "playQueueSelectedItemID"),
        selected_offset: field(root, "playQueueSelectedItemOffset"),
        shuffled: root["playQueueShuffled"]
            .as_bool()
            .unwrap_or_else(|| field::<u8>(root, "playQueueShuffled").unwrap_or(0) != 0),
        items: items(&root["Metadata"])
            .iter()
            .filter_map(|item| {
                let rating_key = id(&item["ratingKey"])?;
                Some(PlexQueueItem {
                    occurrence_id: field(item, "playQueueItemID")?,
                    key: item["key"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("/library/metadata/{rating_key}")),
                    media_uri: library::source_entity_uri(
                        source_id,
                        "track",
                        &super::plex_id("track", &rating_key),
                    ),
                    rating_key,
                })
            })
            .collect(),
    })
}

const QUEUE_URI_BYTES: usize = 6000;

/// Keep ordered queue writes under the encoded URI budget, independently of the
/// metadata window size. Commas are escaped in the library URI and again in its query.
pub fn plex_queue_write_batches(keys: &[String]) -> Vec<&[String]> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut bytes = 100; // Encoded library URI prefix and query framing.
    for (index, key) in keys.iter().enumerate() {
        let encoded: String = url::form_urlencoded::byte_serialize(key.as_bytes()).collect();
        let cost = url::form_urlencoded::byte_serialize(encoded.as_bytes())
            .map(str::len)
            .sum::<usize>()
            + 5;
        if bytes + cost > QUEUE_URI_BYTES && index > start {
            batches.push(&keys[start..index]);
            start = index;
            bytes = 100;
        }
        bytes += cost;
    }
    if start < keys.len() {
        batches.push(&keys[start..]);
    }
    batches
}

fn library_uri(keys: &[String]) -> SourceResult<String> {
    if keys.is_empty() {
        return Err(SourceError::InvalidRequest(
            "Plex queue writes require tracks",
        ));
    }
    let path = format!("/library/metadata/{}", keys.join(","));
    let encoded: String = url::form_urlencoded::byte_serialize(path.as_bytes()).collect();
    let uri = format!("library:///directory/{encoded}");
    if url::form_urlencoded::byte_serialize(uri.as_bytes())
        .map(str::len)
        .sum::<usize>()
        > QUEUE_URI_BYTES
    {
        return Err(SourceError::InvalidRequest("Plex queue URI is too long"));
    }
    Ok(uri)
}

impl PlexSource {
    pub(crate) async fn companion_context(&self, source_id: &SourceId) -> PlexCompanionContext {
        PlexCompanionContext {
            source_id: source_id.clone(),
            server_id: self.config.server_id.clone(),
            profile_id: self.config.profile_id.clone(),
            base_url: self.config.base_url.clone(),
            client_id: self.login.lock().await.client_id().to_string(),
            trust_invalid_certificate: self.config.trust_invalid_cert,
        }
    }

    pub(crate) async fn companion_players(&self) -> SourceResult<Vec<PlexCompanionPlayer>> {
        let value = self.get("/clients", &[]).await?;
        Ok(items(&value["MediaContainer"]["Server"])
            .iter()
            .filter_map(|item| {
                let host = item["address"].as_str()?;
                let port: u16 = field(item, "port")?;
                let host = host
                    .parse::<std::net::Ipv6Addr>()
                    .map(|ip| format!("[{ip}]"))
                    .unwrap_or_else(|_| host.to_string());
                Some(PlexCompanionPlayer {
                    endpoint: format!("http://{host}:{port}"),
                    machine_identifier: item["machineIdentifier"].as_str()?.into(),
                    name: item["name"].as_str().unwrap_or("Plex player").into(),
                })
            })
            .collect())
    }

    pub(crate) async fn delegation_token(&self) -> SourceResult<String> {
        let value = self
            .get(
                "/security/token",
                &[("type", "delegation".into()), ("scope", "all".into())],
            )
            .await?;
        value["MediaContainer"]["token"]
            .as_str()
            .map(str::to_owned)
            .ok_or(SourceError::InvalidRequest(
                "Plex returned no delegation token",
            ))
    }

    async fn queue_response(
        &self,
        source_id: &SourceId,
        request: reqwest::RequestBuilder,
    ) -> SourceResult<PlexQueueWindow> {
        let (value, offset): (Value, _) = crate::remote_http::json_with_header(
            request,
            super::HTTP,
            super::RESPONSE,
            &reqwest::header::HeaderName::from_static("x-plex-container-start"),
        )
        .await?;
        let mut queue = queue_window(&value, source_id)?;
        if let Some(offset) = offset.and_then(|value| value.parse().ok()) {
            queue.offset = offset;
        } else if let Some(selected) = queue
            .selected_item_id
            .and_then(|id| queue.items.iter().position(|item| item.occurrence_id == id))
        {
            if let Some(offset) = queue
                .selected_offset
                .and_then(|offset| offset.checked_sub(selected))
            {
                queue.offset = offset;
            }
        }
        Ok(queue)
    }

    pub(crate) async fn queue_window(
        &self,
        source_id: &SourceId,
        queue_id: u64,
        center: Option<u64>,
    ) -> SourceResult<PlexQueueWindow> {
        let mut params = vec![
            ("own", "0".into()),
            // PMS returns the center plus this many entries on both sides.
            (
                "window",
                ((library::QUEUE_CONTEXT_LIMIT - 1) / 2).to_string(),
            ),
        ];
        if let Some(center) = center {
            params.push(("center", center.to_string()));
        }
        self.queue_response(
            source_id,
            self.request(Method::GET, &format!("/playQueues/{queue_id}"), &params)
                .await?,
        )
        .await
    }

    /// Complete lightweight membership for an explicit queue transfer. Normal
    /// observations use `queue_window`; metadata is hydrated separately nearby.
    pub(crate) async fn queue_membership(
        &self,
        source_id: &SourceId,
        queue_id: u64,
        total: usize,
    ) -> SourceResult<PlexQueueWindow> {
        let params = [
            ("own", "0".into()),
            ("window", total.to_string()),
            ("includeFields", "key,ratingKey,playQueueItemID,playQueueID,playQueueVersion,playQueueTotalCount,playQueueSelectedItemID,playQueueSelectedItemOffset,playQueueShuffled,offset".into()),
            ("excludeElements", "Media,Mood,Similar,Genre,Style,Country,Collection,Guid,Rating,Image".into()),
            ("excludeFields", "summary".into()),
        ];
        self.queue_response(
            source_id,
            self.request(Method::GET, &format!("/playQueues/{queue_id}"), &params)
                .await?,
        )
        .await
    }

    pub(crate) async fn queue_adjacent(
        &self,
        source_id: &SourceId,
        queue_id: u64,
        center: u64,
        before: bool,
        center_offset: Option<usize>,
    ) -> SourceResult<PlexQueueWindow> {
        let params = [
            ("own", "0".into()),
            ("window", PLEX_QUEUE_WINDOW.to_string()),
            ("center", center.to_string()),
            (
                if before {
                    "includeAfter"
                } else {
                    "includeBefore"
                },
                "0".into(),
            ),
        ];
        let mut window = self
            .queue_response(
                source_id,
                self.request(Method::GET, &format!("/playQueues/{queue_id}"), &params)
                    .await?,
            )
            .await?;
        // PMS 1.43 omits container offsets on adjacent windows. Transfer callers
        // know this boundary already; neighbor-only edits don't consume offsets.
        if let Some(offset) = center_offset {
            window.offset = if before {
                offset.saturating_sub(window.items.len())
            } else {
                offset + 1
            };
        }
        Ok(window)
    }

    pub(crate) async fn queue_mutation(
        &self,
        source_id: &SourceId,
        mutation: PlexQueueMutation,
    ) -> SourceResult<PlexQueueWindow> {
        let (method, path, mut params) = match mutation {
            PlexQueueMutation::Create { rating_keys } => (
                Method::POST,
                "/playQueues".into(),
                vec![
                    ("type", "audio".into()),
                    ("shuffle", "0".into()),
                    ("continuous", "0".into()),
                    ("uri", library_uri(&rating_keys)?),
                ],
            ),
            PlexQueueMutation::Insert {
                queue_id,
                rating_keys,
                placement,
            } => {
                let mut params = vec![("uri", library_uri(&rating_keys)?)];
                params.push(match placement {
                    PlexQueuePlacement::End => ("end", "1".into()),
                    PlexQueuePlacement::Next => ("next", "1".into()),
                    PlexQueuePlacement::After(after) => ("after", after.to_string()),
                });
                (Method::PUT, format!("/playQueues/{queue_id}"), params)
            }
            PlexQueueMutation::Remove {
                queue_id,
                occurrence_id,
            } => (
                Method::DELETE,
                format!("/playQueues/{queue_id}/items/{occurrence_id}"),
                vec![],
            ),
            PlexQueueMutation::Move {
                queue_id,
                occurrence_id,
                after,
            } => (
                Method::PUT,
                format!("/playQueues/{queue_id}/items/{occurrence_id}/move"),
                vec![(
                    "after",
                    after
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "-1".into()),
                )],
            ),
        };
        params.push(("window", PLEX_QUEUE_WINDOW.to_string()));
        self.queue_response(source_id, self.request(method, &path, &params).await?)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn transfer_membership_requests_identity_without_media_metadata() {
        let server = MockServer::start().await;
        let source = super::super::catalog::tests::source(&server);
        let response = serde_json::json!({"MediaContainer": {
            "playQueueID": 8, "playQueueVersion": 3, "playQueueTotalCount": 3000,
            "Metadata": [
                {"ratingKey": "10", "playQueueItemID": 700},
                {"ratingKey": "10", "playQueueItemID": 701}
            ]
        }});
        Mock::given(method("GET"))
            .and(path("/playQueues/8"))
            .and(query_param("window", "3000"))
            .and(query_param("own", "0"))
            .and(query_param("includeFields", "key,ratingKey,playQueueItemID,playQueueID,playQueueVersion,playQueueTotalCount,playQueueSelectedItemID,playQueueSelectedItemOffset,playQueueShuffled,offset"))
            .and(query_param("excludeElements", "Media,Mood,Similar,Genre,Style,Country,Collection,Guid,Rating,Image"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server).await;
        let page = source
            .queue_membership(&SourceId::new("server-profile"), 8, 3000)
            .await
            .unwrap();
        assert_eq!(page.total, 3000);
        assert_eq!(page.items[0].media_uri, page.items[1].media_uri);
        assert_ne!(page.items[0].occurrence_id, page.items[1].occurrence_id);
    }

    #[tokio::test]
    async fn native_queue_pages_follow_occurrences_and_server_offsets() {
        let server = MockServer::start().await;
        let source = super::super::catalog::tests::source(&server);
        let source_id = SourceId::new("server-profile");
        let response = serde_json::json!({"MediaContainer":{"playQueueID":8,"playQueueVersion":3,"playQueueTotalCount":1000,"playQueueSelectedItemID":601,"playQueueSelectedItemOffset":500,"playQueueShuffled":true,"Metadata":[{"ratingKey":"10","playQueueItemID":700},{"ratingKey":"10","playQueueItemID":701}]}});
        Mock::given(method("GET"))
            .and(path("/playQueues/8"))
            .and(query_param("center", "699"))
            .and(query_param("includeBefore", "0"))
            .and(query_param("window", "100"))
            .and(query_param("own", "0"))
            .and(header("X-Plex-Token", "token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("X-Plex-Container-Start", "600")
                    .set_body_json(response),
            )
            .expect(1)
            .mount(&server)
            .await;
        let page = source
            .queue_adjacent(&source_id, 8, 699, false, Some(599))
            .await
            .unwrap();
        assert_eq!(page.offset, 600);
        assert_eq!(page.total, 1000);
        assert!(page.shuffled);
        assert_eq!(page.items[1].occurrence_id, 701);
    }

    #[tokio::test]
    async fn native_queue_mutations_address_exact_duplicates_and_bound_writes() {
        let server = MockServer::start().await;
        let source = super::super::catalog::tests::source(&server);
        let source_id = SourceId::new("server-profile");
        let response = serde_json::json!({"MediaContainer":{"playQueueID":8,"playQueueVersion":4,"playQueueTotalCount":1,"Metadata":[{"ratingKey":"10","playQueueItemID":701}]}});
        Mock::given(method("DELETE"))
            .and(path("/playQueues/8/items/700"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response.clone()))
            .expect(1)
            .mount(&server)
            .await;
        let page = source
            .queue_mutation(
                &source_id,
                PlexQueueMutation::Remove {
                    queue_id: 8,
                    occurrence_id: 700,
                },
            )
            .await
            .unwrap();
        assert_eq!(page.items[0].occurrence_id, 701);
        Mock::given(method("PUT"))
            .and(path("/playQueues/8/items/701/move"))
            .and(query_param("after", "-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;
        source
            .queue_mutation(
                &source_id,
                PlexQueueMutation::Move {
                    queue_id: 8,
                    occurrence_id: 701,
                    after: None,
                },
            )
            .await
            .unwrap();
    }
    #[test]
    fn duplicate_metadata_keeps_server_occurrence_identity() {
        let source = SourceId::new("server-profile");
        let queue=queue_window(&serde_json::json!({"MediaContainer":{"playQueueID":7,"playQueueVersion":4,"playQueueTotalCount":500,"offset":200,"playQueueSelectedItemID":302,"playQueueSelectedItemOffset":201,"Metadata":[{"ratingKey":"10","playQueueItemID":301},{"ratingKey":"10","playQueueItemID":302}]}}),&source).unwrap();
        assert_eq!(queue.items[0].media_uri, queue.items[1].media_uri);
        assert_ne!(queue.items[0].occurrence_id, queue.items[1].occurrence_id);
        assert_eq!(queue.selected_item_id, Some(302));
        assert_eq!(queue.offset, 200);
    }
    #[test]
    fn transfer_batches_preserve_duplicates_and_limit_uri_size() {
        let uri = library_uri(&["10".into(), "20".into(), "10".into()]).unwrap();
        assert_eq!(
            uri,
            "library:///directory/%2Flibrary%2Fmetadata%2F10%2C20%2C10"
        );
        let keys = (0..2800)
            .map(|index| (index % 400).to_string())
            .collect::<Vec<_>>();
        let batches = plex_queue_write_batches(&keys);
        assert!(batches.len() < 5);
        assert_eq!(batches.concat(), keys);
        for batch in batches {
            let uri = library_uri(batch).unwrap();
            assert!(
                url::form_urlencoded::byte_serialize(uri.as_bytes())
                    .map(str::len)
                    .sum::<usize>()
                    <= QUEUE_URI_BYTES
            );
        }
        assert!(library_uri(&["1".repeat(QUEUE_URI_BYTES)]).is_err());
    }

    #[tokio::test]
    async fn repeated_insertions_keep_the_requested_next_order() {
        let server = MockServer::start().await;
        let source = super::super::catalog::tests::source(&server);
        Mock::given(method("PUT")).and(path("/playQueues/8"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"MediaContainer":{"playQueueID":8,"playQueueVersion":2,"playQueueTotalCount":5,"Metadata":[]}}))).expect(1).mount(&server).await;
        source
            .queue_mutation(
                &SourceId::new("profile"),
                PlexQueueMutation::Insert {
                    queue_id: 8,
                    rating_keys: vec!["10".into(), "20".into(), "10".into()],
                    placement: PlexQueuePlacement::Next,
                },
            )
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let uris = requests
            .iter()
            .map(|request| {
                request
                    .url
                    .query_pairs()
                    .find(|(key, _)| key == "uri")
                    .unwrap()
                    .1
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            uris,
            [library_uri(&["10".into(), "20".into(), "10".into()]).unwrap()]
        );
    }
}
