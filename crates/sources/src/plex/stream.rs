use super::*;
use crate::remote_json::{boolean, field, items};
use playback::{ResolvedStream, SourceReportFact, SourceReportPhase, StreamQuality};

impl PlexSource {
    pub(crate) async fn resolve_stream(
        &self,
        object: &str,
        quality: StreamQuality,
        session_identifier: Option<&str>,
    ) -> SourceResult<ResolvedStream> {
        let metadata = self.metadata(object).await?;
        let media = items(&metadata["Media"])
            .first()
            .ok_or(SourceError::NotFound)?;
        let part = items(&media["Part"]).first().ok_or(SourceError::NotFound)?;
        let bitrate = field::<u32>(media, "bitrate");
        let transcode = quality
            .max_bitrate_kbps()
            .filter(|limit| bitrate.is_none_or(|bitrate| bitrate > *limit));
        let (path, params, content_type) = if let Some(bitrate) = transcode {
            let session = new_session()?;
            let mut params = music_params(object, bitrate, &session);
            params.push(("X-Plex-Platform", "Generic".into()));
            if let Some(identifier) = session_identifier {
                if let Some((_, value)) = params
                    .iter_mut()
                    .find(|(key, _)| *key == "X-Plex-Session-Identifier")
                {
                    *value = identifier.into();
                }
            }
            let decision = self
                .get("/music/:/transcode/universal/decision", &params)
                .await?;
            let container = &decision["MediaContainer"];
            if [
                "generalDecisionCode",
                "transcodeDecisionCode",
                "mdeDecisionCode",
            ]
            .iter()
            .any(|key| field::<u32>(container, key).is_some_and(|code| code >= 2000))
            {
                return Err(SourceError::Other(
                    container["generalDecisionText"]
                        .as_str()
                        .unwrap_or("Plex could not transcode this track")
                        .into(),
                ));
            }
            (
                "/music/:/transcode/universal/start.mp3".to_string(),
                params,
                Some("audio/mpeg".into()),
            )
        } else {
            (
                part["key"]
                    .as_str()
                    .ok_or(SourceError::NotFound)?
                    .to_string(),
                // Serve original bytes without the relay's streaming decision.
                vec![("download", "1".into())],
                mime(
                    media["container"]
                        .as_str()
                        .or_else(|| part["container"].as_str()),
                )
                .map(str::to_string),
            )
        };
        let request = self
            .request(Method::GET, &path, &params)
            .await?
            .build()
            .map_err(|error| remote_http::map_reqwest_error(error, HTTP))?;
        let mut url = request.url().clone();
        let login = self.login.lock().await;
        url.query_pairs_mut()
            .append_pair(
                "X-Plex-Token",
                login.server_token(&self.config.profile_id, &self.config.server_id)?,
            )
            .append_pair("X-Plex-Client-Identifier", login.client_id());
        if transcode.is_none() {
            if let Some(identifier) = session_identifier {
                url.query_pairs_mut()
                    .append_pair("X-Plex-Session-Identifier", identifier);
            }
        }
        let mut stream = ResolvedStream::new(url.to_string())
            .with_content_type(content_type)
            .with_trust_invalid_certificate(self.config.trust_invalid_cert);
        if let Some((_, session)) = params.iter().find(|(key, _)| *key == "session") {
            let mut ping = reqwest::Url::parse(&self.config.base_url)
                .map_err(|error| SourceError::InvalidConfig(error.to_string()))?
                .join("/video/:/transcode/universal/ping")
                .map_err(|error| SourceError::InvalidConfig(error.to_string()))?;
            ping.query_pairs_mut().append_pair("session", session);
            let mut stop = ping.clone();
            stop.set_path("/video/:/transcode/universal/stop");
            let ping = login
                .server_request(
                    &self.client,
                    Method::GET,
                    ping.as_str(),
                    &self.config.profile_id,
                    &self.config.server_id,
                )?
                .build()
                .map_err(|error| remote_http::map_reqwest_error(error, HTTP))?;
            let stop = login
                .server_request(
                    &self.client,
                    Method::GET,
                    stop.as_str(),
                    &self.config.profile_id,
                    &self.config.server_id,
                )?
                .build()
                .map_err(|error| remote_http::map_reqwest_error(error, HTTP))?;
            let (release, released) = tokio::sync::oneshot::channel();
            let client = self.client.clone();
            tokio::spawn(async move {
                tokio::pin!(released);
                loop {
                    tokio::select! {
                        _ = &mut released => break,
                        _ = tokio::time::sleep(Duration::from_secs(30)) => {
                            if let Some(request) = ping.try_clone() { let _ = client.execute(request).await; }
                        }
                    }
                }
                let _ = client.execute(stop).await;
            });
            stream = stream.with_resource(Arc::new(TranscodeResource {
                release: Some(release),
            }));
        }
        Ok(stream)
    }
    pub(crate) async fn resolve_download(
        &self,
        object: &str,
        quality: StreamQuality,
    ) -> SourceResult<crate::ResolvedDownload> {
        if self.config.relay {
            return Err(SourceError::Other(
                "Plex downloads require a direct server connection; Relay cannot download files"
                    .into(),
            ));
        }
        let root = self.get("/", &[]).await?;
        let subscription = self
            .login
            .lock()
            .await
            .download_subscription(&self.config.profile_id);
        if !subscription {
            return Err(SourceError::Other("Plex downloads require an active Plex Pass for this listening profile or its Plex Home administrator".into()));
        }
        if !self.config.owned
            && !boolean(&root["MediaContainer"]["allowSync"])
                .or_else(|| {
                    field::<u8>(&root["MediaContainer"], "allowSync").map(|value| value != 0)
                })
                .unwrap_or(false)
        {
            return Err(SourceError::Other(
                "This Plex server has not allowed downloads for this listening profile".into(),
            ));
        }
        let stream = self.resolve_stream(object, quality, None).await?;
        let extension = stream
            .uri()
            .contains("/music/:/transcode/")
            .then_some("mp3");
        Ok(crate::ResolvedDownload::new(stream, extension))
    }
    pub(crate) async fn report_playback(
        &self,
        object: &str,
        report: &SourceReportFact,
    ) -> SourceResult<()> {
        let raw = crate::policy::raw_item_id(object);
        if report.phase == SourceReportPhase::QualifiedPlay {
            return Ok(());
        }
        let state = if report.phase == SourceReportPhase::Ended {
            "stopped"
        } else if report.paused {
            "paused"
        } else {
            "playing"
        };
        let mut params = vec![
            ("ratingKey", raw.into()),
            ("key", format!("/library/metadata/{raw}")),
            ("state", state.into()),
            ("time", report.position_millis.to_string()),
            ("type", "music".into()),
            (
                "X-Plex-Session-Identifier",
                report.session_identifier.clone(),
            ),
            ("volume", (report.volume * 100.0).round().to_string()),
            ("shuffle", u8::from(report.shuffle).to_string()),
            (
                "repeat",
                match report.repeat_mode {
                    playback::RepeatMode::Off => "0",
                    playback::RepeatMode::All => "1",
                    playback::RepeatMode::One => "2",
                }
                .into(),
            ),
        ];
        if let Some(duration) = report.duration_millis {
            params.push(("duration", duration.to_string()));
        }
        if let Some(queue) = report.queue_id {
            params.push(("playQueueID", queue.to_string()));
        }
        if let Some(item) = report.queue_item_id {
            params.push(("playQueueItemID", item.to_string()));
        }
        self.unit(Method::POST, "/:/timeline", &params).await
    }
}
struct TranscodeResource {
    release: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Drop for TranscodeResource {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}
fn new_session() -> SourceResult<String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|error| SourceError::Other(error.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn music_params(object: &str, bitrate: u32, session: &str) -> Vec<(&'static str, String)> {
    vec![("hasMDE", "1".into()), ("path", format!("/library/metadata/{}", crate::policy::raw_item_id(object))), ("mediaIndex", "0".into()), ("partIndex", "0".into()), ("protocol", "http".into()), ("directPlay", "0".into()), ("directStream", "0".into()), ("musicBitrate", bitrate.to_string()), ("session", session.into()), ("X-Plex-Session-Identifier", session.into()), ("X-Plex-Client-Profile-Extra", "add-transcode-target(type=musicProfile&context=streaming&protocol=http&container=mp3&audioCodec=mp3)".into())]
}
fn mime(container: Option<&str>) -> Option<&'static str> {
    match container? {
        "flac" => Some("audio/flac"),
        "mp3" => Some("audio/mpeg"),
        "m4a" | "mp4" => Some("audio/mp4"),
        "ogg" | "opus" => Some("audio/ogg"),
        "wav" => Some("audio/wav"),
        "aac" => Some("audio/aac"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn occurrence_reports_keep_positions_sessions_and_single_completion_path() {
        let server = MockServer::start().await;
        let source = super::super::catalog::tests::source(&server);
        Mock::given(method("POST"))
            .and(path("/:/timeline"))
            .respond_with(ResponseTemplate::new(200))
            .expect(3)
            .mount(&server)
            .await;
        let mut report = SourceReportFact {
            run: playback::RunId::new(1),
            media_uri: "plex:test".into(),
            session_identifier: "outgoing".into(),
            duration_millis: Some(180000),
            queue_id: None,
            queue_item_id: None,
            phase: SourceReportPhase::Ended,
            started_at_unix_seconds: 1,
            position_millis: 179000,
            paused: false,
            muted: false,
            volume: 0.5,
            shuffle: false,
            repeat_mode: playback::RepeatMode::Off,
            failed: false,
        };
        source
            .report_playback("plex:track:one", &report)
            .await
            .unwrap();
        report.run = playback::RunId::new(2);
        report.session_identifier = "incoming".into();
        report.phase = SourceReportPhase::Started;
        report.position_millis = 0;
        source
            .report_playback("plex:track:two", &report)
            .await
            .unwrap();
        report.phase = SourceReportPhase::QualifiedPlay;
        source
            .report_playback("plex:track:two", &report)
            .await
            .unwrap();
        report.phase = SourceReportPhase::Progress;
        report.paused = true;
        report.position_millis = 35000;
        source
            .report_playback("plex:track:two", &report)
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        let params: Vec<std::collections::HashMap<_, _>> = requests
            .iter()
            .map(|request| {
                request
                    .url
                    .query_pairs()
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect()
            })
            .collect();
        assert_eq!(params[0]["state"], "stopped");
        assert_eq!(params[0]["time"], "179000");
        assert_eq!(params[1]["time"], "0");
        assert_eq!(params[1]["X-Plex-Session-Identifier"], "incoming");
        assert_eq!(params[2]["state"], "paused");
        assert_eq!(params[2]["time"], "35000");
        assert_eq!(params[2]["duration"], "180000");
    }

    #[tokio::test]
    async fn original_and_overlapping_transcodes_keep_distinct_resource_lifetimes() {
        let server = MockServer::start().await;
        let source = super::super::catalog::tests::source(&server);
        Mock::given(method("GET")).and(path("/library/metadata/track")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"MediaContainer":{"Metadata":[{
            "ratingKey":"track","Media":[{"container":"flac","bitrate":1000,"Part":[{"key":"/library/parts/1/file.flac"}]}]
        }]}}))).mount(&server).await;
        Mock::given(method("GET")).and(path("/music/:/transcode/universal/decision")).and(query_param("musicBitrate","128"))
            .and(query_param("X-Plex-Platform","Generic"))
            .and(query_param("X-Plex-Client-Profile-Extra","add-transcode-target(type=musicProfile&context=streaming&protocol=http&container=mp3&audioCodec=mp3)"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"MediaContainer":{"generalDecisionCode":1001,"transcodeDecisionCode":1001}}))).mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/video/:/transcode/universal/stop"))
            .respond_with(ResponseTemplate::new(200))
            .expect(2)
            .mount(&server)
            .await;
        let original = source
            .resolve_stream("plex:track:track", StreamQuality::Original, None)
            .await
            .unwrap();
        assert_eq!(
            reqwest::Url::parse(original.uri()).unwrap().path(),
            "/library/parts/1/file.flac"
        );
        assert!(
            reqwest::Url::parse(original.uri())
                .unwrap()
                .query_pairs()
                .any(|(key, value)| key == "download" && value == "1")
        );
        let first = source
            .resolve_stream(
                "plex:track:track",
                StreamQuality::MaxBitrateKbps(128),
                Some("rufin-test-first"),
            )
            .await
            .unwrap();
        let second = source
            .resolve_stream(
                "plex:track:track",
                StreamQuality::MaxBitrateKbps(128),
                Some("rufin-test-second"),
            )
            .await
            .unwrap();
        let session = |stream: &ResolvedStream| {
            reqwest::Url::parse(stream.uri())
                .unwrap()
                .query_pairs()
                .find(|(key, _)| key == "session")
                .unwrap()
                .1
                .into_owned()
        };
        let first_session = session(&first);
        let second_session = session(&second);
        assert_ne!(first_session, second_session);
        let retained = first.clone();
        drop(first);
        let requests = server.received_requests().await.unwrap();
        assert!(
            !requests
                .iter()
                .any(|request| request.url.path().ends_with("/stop"))
        );
        drop(retained);
        drop(second);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let requests = server.received_requests().await.unwrap();
                let stopped = requests
                    .iter()
                    .filter(|request| request.url.path().ends_with("/stop"))
                    .map(|request| {
                        request
                            .url
                            .query_pairs()
                            .find(|(key, _)| key == "session")
                            .unwrap()
                            .1
                            .into_owned()
                    })
                    .collect::<Vec<_>>();
                if stopped.len() == 2 {
                    assert!(stopped.contains(&first_session));
                    assert!(stopped.contains(&second_session));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn transcode_error_and_shared_download_permission_are_provider_decisions() {
        let server = MockServer::start().await;
        let mut source = super::super::catalog::tests::source(&server);
        source.config.owned = false;
        let mut login: Value =
            serde_json::from_str(&source.login.lock().await.encode().unwrap()).unwrap();
        login["download_subscriptions"] = json!({"user":true});
        *source.login.lock().await = PlexLogin::decode(&login.to_string()).unwrap();
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"MediaContainer":{"allowSync":"1"}})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/library/metadata/track")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"MediaContainer":{"Metadata":[{
            "ratingKey":"track","Media":[{"container":"flac","bitrate":1000,"Part":[{"key":"/library/parts/1/file.flac"}]}]
        }]}}))).mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/music/:/transcode/universal/decision"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"MediaContainer":{"generalDecisionCode":1001,"transcodeDecisionCode":4005}}),
            ))
            .mount(&server)
            .await;
        assert!(
            source
                .resolve_stream("plex:track:track", StreamQuality::MaxBitrateKbps(128), None)
                .await
                .is_err()
        );
        source
            .resolve_download("plex:track:track", StreamQuality::Original)
            .await
            .unwrap();
        source.config.relay = true;
        assert!(
            source
                .resolve_download("plex:track:track", StreamQuality::Original)
                .await
                .is_err()
        );
        source
            .resolve_stream("plex:track:track", StreamQuality::Original, None)
            .await
            .unwrap();
    }
}
