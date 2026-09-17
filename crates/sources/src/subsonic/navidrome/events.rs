use super::*;
use crate::source::{LIVE_CHANGE_LIMIT, RemoteItemChange};
use futures_util::TryStreamExt;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::time::{Duration, sleep};
use tokio_util::io::StreamReader;

impl SubsonicSource {
    async fn connect_navidrome_events(
        &self,
        client: &reqwest::Client,
    ) -> SourceResult<reqwest::Response> {
        let token = self.navidrome_token().await?;
        let response = client
            .get(navidrome_endpoint(&self.base_url, "api/events")?)
            .header(&NAVIDROME_AUTH_HEADER, format!("Bearer {token}"))
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(|error| remote_http::map_reqwest_error(error, NAVIDROME_HTTP))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            let mut current = self.navidrome_session.0.lock().await;
            if current.as_deref() == Some(&token) {
                *current = None;
            }
        }
        let response = response
            .error_for_status()
            .map_err(|error| remote_http::map_reqwest_error(error, NAVIDROME_HTTP))?;
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
            })
        {
            return Err(SourceError::Other(
                "Navidrome event stream is unavailable".into(),
            ));
        }
        if let Some(token) = response
            .headers()
            .get(&NAVIDROME_AUTH_HEADER)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.trim().is_empty())
        {
            *self.navidrome_session.0.lock().await = Some(token.to_string());
        }
        Ok(response)
    }

    pub(crate) async fn listen_navidrome_changes(
        &self,
        mut changed: impl FnMut(RemoteItemChange) -> bool + Send,
    ) -> SourceResult<()> {
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(self.trust_invalid_cert)
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(45))
            .build()
            .map_err(|error| remote_http::map_reqwest_error(error, NAVIDROME_HTTP))?;
        let mut established = false;
        let mut delay = Duration::from_secs(5);
        loop {
            match self.connect_navidrome_events(&client).await {
                Ok(response) => {
                    if established && !changed(RemoteItemChange::BoundaryLost) {
                        return Ok(());
                    }
                    established = true;
                    delay = Duration::from_secs(5);
                    match read_events(response, &mut changed).await {
                        Ok(false) => return Ok(()),
                        Ok(true) => {}
                        Err(error) => tracing::warn!(%error, "Navidrome event stream disconnected"),
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "Navidrome event stream unavailable; polling remains active")
                }
            }
            sleep(delay).await;
            delay = delay.saturating_mul(2).min(Duration::from_secs(60));
        }
    }

    pub(crate) async fn apply_navidrome_changes(
        &self,
        database: &library::Database,
        source_id: &str,
        upserts: Vec<String>,
    ) -> SourceResult<library::ScanOutcome> {
        let mut scan = Scan::begin_items(database, source_id).await?;
        for (resource, kind) in [("artist", "artist"), ("album", "album"), ("song", "track")] {
            let ids: Vec<_> = upserts
                .iter()
                .filter_map(|object| {
                    let (item_kind, id) = object.split_once(':')?;
                    (item_kind == resource).then(|| serde_json::json!({"id":id}))
                })
                .collect();
            let values = self.navidrome_items(resource, &ids).await?;
            scan.begin_batch().await?;
            for value in &values {
                let Some(id) = id(&value["id"]) else { continue };
                match resource {
                    "artist" => {
                        stage_artist(&mut scan, artist_from_navidrome(self, &id, value)).await?
                    }
                    "album" => {
                        stage_album(&mut scan, album_from_navidrome(self, &id, value)).await?
                    }
                    _ => {
                        stage_track(&mut scan, track_from_navidrome(self, &id, value)).await?;
                    }
                }
            }
            for id in ids.iter().filter_map(|value| id(&value["id"])) {
                if !values.iter().any(|value| {
                    crate::remote_json::id(&value["id"]).as_deref() == Some(id.as_str())
                }) {
                    let object = self.id(kind, &id);
                    match kind {
                        "artist" => scan.remove_artist(&object).await?,
                        "album" => scan.remove_album(&object).await?,
                        _ => scan.remove_track(&object).await?,
                    }
                }
            }
            scan.finish_batch().await?;
        }
        for id in upserts
            .iter()
            .filter_map(|object| object.strip_prefix("playlist:"))
        {
            match self
                .stage_playlist_snapshot(&mut scan, &self.id("playlist", id))
                .await
            {
                Ok(()) => {}
                Err(SourceError::NotFound) => {
                    scan.remove_playlist(&self.id("playlist", id)).await?
                }
                Err(error) => return Err(error),
            }
        }
        let section = crate::SourceHomeSection::NewlyAdded;
        match self.home_section(section).await {
            Ok(entries) => scan.replace_home_section(section.id(), &entries).await?,
            Err(error) => crate::source::optional_collection_error(error)?,
        }
        Ok(scan.finish().await?)
    }
}

async fn read_events(
    response: reqwest::Response,
    changed: &mut (impl FnMut(RemoteItemChange) -> bool + Send),
) -> SourceResult<bool> {
    let stream = response.bytes_stream().map_err(std::io::Error::other);
    let mut lines = BufReader::new(StreamReader::new(stream)).lines();
    let mut event = String::new();
    let mut data = String::new();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| SourceError::Network(error.to_string()))?
    {
        if line.is_empty() {
            if let Some(change) = event_change(&event, &data) {
                if !changed(change) {
                    return Ok(false);
                }
            }
            event.clear();
            data.clear();
        } else if let Some(value) = line.strip_prefix("event:") {
            event = value.trim_start_matches(' ').to_string();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
            data.push('\n');
        }
    }
    Ok(true)
}

fn event_change(event: &str, data: &str) -> Option<RemoteItemChange> {
    if event != "refreshResource" {
        return None;
    }
    let value: Value = serde_json::from_str(data).ok()?;
    let resources = value.as_object()?;
    let mut upserts = Vec::new();
    for (resource, ids) in resources {
        match resource.as_str() {
            "*" | "library" => return Some(RemoteItemChange::BoundaryLost),
            "song" | "album" | "artist" | "playlist" => {}
            _ => continue,
        }
        if ids == "*" || items(ids).iter().any(|value| value == "*") {
            return Some(RemoteItemChange::BoundaryLost);
        }
        upserts.extend(
            items(ids)
                .iter()
                .filter_map(id)
                .map(|id| format!("{resource}:{id}")),
        );
    }
    upserts.sort();
    upserts.dedup();
    if upserts.len() > LIVE_CHANGE_LIMIT {
        Some(RemoteItemChange::BoundaryLost)
    } else if upserts.is_empty() {
        None
    } else {
        Some(RemoteItemChange::Items {
            upserts,
            removals: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::navidrome_source_with_token;
    use super::*;
    use crate::subsonic::{NAVIDROME_LIBRARY_VERSION, SOURCE_CONFIG_VERSION, SubsonicCredential};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn live_updates_refresh_newly_added_and_retain_it_on_failure() {
        let server = MockServer::start().await;
        let source = navidrome_source_with_token(&server, "token");
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let scan = Scan::begin(&database, "source", "Source", "source", None)
            .await
            .unwrap();
        let library::ScanOutcome::Changed(publication) = scan.finish().await.unwrap() else {
            panic!("initial publication");
        };
        let cancellation = library::ReadCancellation::new();
        for (id, status, empty, identical, expected) in [
            ("old", 200, false, false, vec!["old"]),
            ("new", 200, false, false, vec!["new"]),
            ("new", 200, false, true, vec!["new"]),
            ("third", 503, false, false, vec!["new"]),
            ("third", 200, true, false, vec!["new", "old", "third"]),
        ] {
            server.reset().await;
            let item = serde_json::json!({"id":id,"name":id});
            Mock::given(method("GET"))
                .and(path("/api/album".to_string()))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!([item.clone()])),
                )
                .expect(1)
                .mount(&server)
                .await;
            let entries = if empty { vec![] } else { vec![item] };
            Mock::given(method("GET")).and(path("/rest/getAlbumList2.view"))
                .and(wiremock::matchers::query_param("type", "newest")).and(wiremock::matchers::query_param("size", "24"))
                .respond_with(ResponseTemplate::new(status).set_body_json(serde_json::json!({"subsonic-response":{"status":"ok","albumList2":{"album":entries}}})))
                .expect(1).mount(&server).await;
            let outcome = source
                .apply_navidrome_changes(&database, "source", vec![format!("album:{id}")])
                .await
                .unwrap();
            assert_eq!(
                matches!(outcome, library::ScanOutcome::Identical(_)),
                identical
            );
            assert!(matches!(
                outcome,
                library::ScanOutcome::Changed(_) | library::ScanOutcome::Identical(_)
            ));
            let home = database
                .home_page(
                    publication.source,
                    None,
                    0,
                    0,
                    &[library::HomeBlockKind::NewlyAdded],
                    &cancellation,
                )
                .await
                .unwrap();
            let mut titles = home
                .newly_added
                .albums
                .iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>();
            titles.sort_unstable();
            assert_eq!(titles, expected);
            server.verify().await;
        }
    }

    #[tokio::test]
    async fn native_events_update_ratings_and_favorites_through_the_selected_feed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/login"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"token":"token"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/api/events")).and(header("x-nd-authorization", "Bearer token"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "text/event-stream")
                .insert_header("x-nd-authorization", "renewed")
                .set_body_raw("event: serverStart\r\ndata: {}\r\n\r\n: comment\r\nevent: refreshResource\r\ndata: {\"song\":\r\ndata: [\"track\"]}\r\n\r\nevent: keepAlive\r\ndata: {}\r\n\r\n".as_bytes(), "text/event-stream"))
            .expect(1).mount(&server).await;
        let mut item = serde_json::json!({"id":"track","title":"Track","duration":42,"starred":false,"rating":1});
        let baseline = navidrome_source_with_token(&server, "unused");
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let marker = library::Freshness::new(b"catalog".to_vec()).unwrap();
        let mut scan = Scan::begin(
            &database,
            "source",
            "Source",
            "source",
            Some(marker.clone()),
        )
        .await
        .unwrap();
        stage_track(&mut scan, track_from_navidrome(&baseline, "track", &item))
            .await
            .unwrap();
        let library::ScanOutcome::Changed(publication) = scan.finish().await.unwrap() else {
            panic!("initial publication")
        };
        item["starred"] = true.into();
        item["rating"] = 4.into();
        Mock::given(method("GET"))
            .and(path("/api/song"))
            .and(header("x-nd-authorization", "Bearer renewed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([item])))
            .expect(1)
            .mount(&server)
            .await;
        let source = std::sync::Arc::new(crate::Source::open(crate::SourceConfiguration {
            source_id: crate::SourceId::new("source"), kind:"navidrome".into(), name:"Source".into(),
            provider_payload: serde_json::json!({"version":SOURCE_CONFIG_VERSION,"base_url":server.uri(),
                "username":"listener","trust_invalid_cert":false,"navidrome_library_version":NAVIDROME_LIBRARY_VERSION,
                "authentication":"password"}).to_string(),
        }, Some(SubsonicCredential::from_navidrome_password("password").serialize()), None).unwrap());
        let feed = source
            .start_selected_feed(&tokio::runtime::Handle::current())
            .expect("native feed");
        assert!(
            tokio::time::timeout(Duration::from_secs(2), feed.wait_for_change())
                .await
                .unwrap()
        );
        assert!(matches!(
            feed.apply_pending(&database, publication.source)
                .await
                .unwrap(),
            Some(library::ScanOutcome::Changed(_))
        ));
        feed.cancel();
        let cancellation = library::ReadCancellation::new();
        let uri = library::source_entity_uri(
            &crate::SourceId::new("source"),
            "track",
            "navidrome:track:track",
        );
        let row = database
            .track_row_by_uri(&uri, &cancellation)
            .await
            .unwrap()
            .unwrap();
        assert!(row.favorite);
        assert_eq!(row.rating, Some(8));
        assert_eq!(row.title, "Track");
        assert!(
            Scan::accept_freshness(&database, "source", &marker, &cancellation)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn reconnect_recovers_once_and_initial_server_start_is_not_a_rescan() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/events"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_raw(
                        "event: serverStart\ndata: {}\n\n".as_bytes(),
                        "text/event-stream",
                    ),
            )
            .expect(2)
            .mount(&server)
            .await;
        let source = navidrome_source_with_token(&server, "token");
        let mut recoveries = 0;
        tokio::time::timeout(
            Duration::from_secs(10),
            source.listen_navidrome_changes(|change| {
                assert_eq!(change, RemoteItemChange::BoundaryLost);
                recoveries += 1;
                false
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(recoveries, 1);
    }
}
