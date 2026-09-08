use super::*;

impl JellyfinEmbySource {
    pub(super) async fn emby_stream(
        &self,
        track: &str,
        quality: StreamQuality,
        session: Option<&str>,
        download: bool,
    ) -> SourceResult<ResolvedStream> {
        let session = match session {
            Some(session) => session.to_owned(),
            None => {
                let mut bytes = [0; 16];
                getrandom::fill(&mut bytes)
                    .map_err(|error| SourceError::Other(error.to_string()))?;
                bytes.iter().map(|byte| format!("{byte:02x}")).collect()
            }
        };
        let raw = raw_item_id(track);
        let transcoded = quality != StreamQuality::Original;
        let path = if !transcoded {
            format!("Audio/{raw}/stream")
        } else if download {
            format!("Audio/{raw}/stream.mp3")
        } else {
            format!("Audio/{raw}/universal")
        };
        let token = self.session_access_token().await?;
        let mut url = endpoint(&self.base_url, &path)?;
        url.query_pairs_mut()
            .append_pair("UserId", &self.user_id)
            .append_pair("DeviceId", &self.device_id)
            .append_pair("api_key", token)
            .append_pair("PlaySessionId", &session);
        if let StreamQuality::MaxBitrateKbps(kbps) = quality {
            let bitrate = kbps.saturating_mul(1000).to_string();
            if download {
                url.query_pairs_mut()
                    .append_pair("Static", "false")
                    .append_pair("AudioCodec", "mp3")
                    .append_pair("AudioBitRate", &bitrate);
            } else {
                url.query_pairs_mut()
                    .append_pair("MaxStreamingBitrate", &bitrate)
                    .append_pair("TranscodingProtocol", "hls")
                    .append_pair("TranscodingContainer", "ts")
                    .append_pair("AudioCodec", "aac");
            }
        } else {
            url.query_pairs_mut().append_pair("Static", "true");
        }
        let mut stream = ResolvedStream::new(url.to_string())
            .with_trust_invalid_certificate(self.trust_invalid_cert)
            .with_transcoding(transcoded)
            .with_content_type(transcoded.then(|| {
                if download {
                    "audio/mpeg"
                } else {
                    "application/vnd.apple.mpegurl"
                }
                .to_owned()
            }));
        if transcoded {
            let mut stop = endpoint(&self.base_url, "Videos/ActiveEncodings")?;
            stop.query_pairs_mut()
                .append_pair("DeviceId", &self.device_id)
                .append_pair("PlaySessionId", &session);
            let stop = self.authenticated(self.client.delete(stop)).await?;
            let (release, released) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let _ = released.await;
                if let Err(error) = send_unit(ServerKind::Emby, stop).await {
                    tracing::debug!(%error, "Emby transcode release failed");
                }
            });
            stream = stream.with_resource(Arc::new(TranscodeResource {
                release: Some(release),
            }));
        }
        Ok(stream)
    }

    pub(super) async fn emby_create_playlist(
        &self,
        name: &str,
        tracks: &[String],
    ) -> SourceResult<PlaylistId> {
        let mut url = endpoint(&self.base_url, "Playlists")?;
        url.query_pairs_mut()
            .append_pair("Name", name)
            .append_pair("Ids", &raw_track_ids(tracks).join(","))
            .append_pair("UserId", &self.user_id)
            .append_pair("MediaType", "Audio");
        let result: PlaylistCreationResult = self.send_json(self.client.post(url)).await?;
        Ok(self.kind.object_id("playlist", &result.id))
    }

    pub(super) async fn emby_rename_playlist(
        &self,
        playlist: &str,
        name: &str,
    ) -> SourceResult<()> {
        let raw = raw_item_id(playlist);
        let mut item: Value = self.get_json(self.item_url(raw)?).await?;
        item["Name"] = Value::String(name.to_owned());
        self.send_unit(
            self.client
                .post(endpoint(&self.base_url, &format!("Items/{raw}"))?)
                .json(&item),
        )
        .await
    }

    pub(super) async fn emby_lyrics(&self, track: &str) -> SourceResult<Option<LyricsBundle>> {
        let raw = raw_item_id(track);
        let mut url = self.item_url(raw)?;
        url.query_pairs_mut()
            .append_pair("Fields", "MediaSources,MediaStreams");
        let item: Value = match self.get_json(url).await {
            Ok(item) => item,
            Err(SourceError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let selected = items(&item["MediaSources"]).iter().find_map(|source| {
            let source_id = id(&source["Id"])?;
            let subtitles = || {
                items(&source["MediaStreams"])
                    .iter()
                    .filter(|stream| stream["Type"].as_str() == Some("Subtitle"))
                    .filter_map(|stream| Some((field::<u32>(stream, "Index")?, stream)))
            };
            let default = field::<u32>(source, "DefaultSubtitleStreamIndex");
            let (index, subtitle) = subtitles()
                .find(|(index, _)| Some(*index) == default)
                .or_else(|| subtitles().next())?;
            Some((source_id, index, subtitle))
        });
        let Some((source_id, index, subtitle)) = selected else {
            return Ok(None);
        };
        let url = endpoint(
            &self.base_url,
            &format!("Items/{raw}/{source_id}/Subtitles/{index}/Stream.js"),
        )?;
        let dto: Value = match self.get_json(url).await {
            Ok(dto) => dto,
            Err(SourceError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let lines = items(&dto["TrackEvents"])
            .iter()
            .filter_map(|event| {
                let text: String = field(event, "Text")?;
                (!text.trim().is_empty()).then(|| LyricsLine {
                    text,
                    start_millis: ticks_to_millis(field(event, "StartPositionTicks")),
                    end_millis: ticks_to_millis(field(event, "EndPositionTicks")),
                    cue_lines: Vec::new(),
                })
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            return Ok(None);
        }
        Ok(Some(LyricsBundle::from_documents(
            LyricsOrigin::Native,
            vec![LyricsDocument {
                role: LyricsRole::Original,
                language: field(subtitle, "Language"),
                offset_millis: 0,
                lines,
                agents: Vec::new(),
            }],
        )))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn source(base: String) -> JellyfinEmbySource {
        JellyfinEmbySource::open(
            JellyfinEmbySourceConfig {
                emby_connect: false,
                kind: ServerKind::Emby,
                base_url: base,
                server_id: Some("server".into()),
                user_id: "listener".into(),
                username: "Listener".into(),
                trust_invalid_cert: false,
                use_instant_mix: false,
            },
            "secret".into(),
            "device".into(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn authentication_reopens_the_same_emby_identity_and_imports_shared_catalog() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/music/emby/Users/AuthenticateByName"))
            .and(body_json(json!({"Username":"Listener","Pw":"password"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"AccessToken":"secret","ServerId":"server","User":{"Id":"listener","Name":"Listener"}}))).mount(&server).await;
        Mock::given(path("/music/emby/System/Info/Public"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ServerName":"Music"})))
            .mount(&server)
            .await;
        let authenticated = JellyfinEmbySource::authenticate(
            SourceId::new("source"),
            JellyfinEmbySetupInput {
                kind: ServerKind::Emby,
                device_id: "device".into(),
                use_instant_mix: false,
                credentials: CredentialHostInput {
                    server_name: None,
                    server_url: format!("{}/music", server.uri()),
                    username: "Listener".into(),
                    password: "password".into(),
                    trust_invalid_cert: false,
                },
            },
        )
        .await
        .unwrap();
        let (configuration, _, credential) = authenticated.connected().into_parts();
        assert_eq!(configuration.kind, "emby");
        assert!(
            !configuration
                .provider_payload
                .contains("use_jellyfin_instant_mix")
        );
        assert!(configuration.playlist_tracks_can_repeat());
        let identity = configuration.input_identity().unwrap();
        let source = super::super::open(&configuration, credential, Some("device".into())).unwrap();
        assert_eq!(configuration.input_identity().unwrap(), identity);
        let album = json!({"Id":"10","Type":"MusicAlbum","Name":"Album","AlbumArtists":[{"Id":"artist","Name":"Artist"}],"GenreItems":[{"Id":"genre","Name":"Genre"}]});
        let track = json!({"Id":"11","Type":"Audio","Name":"Song","AlbumId":"10","Album":"Album","AlbumArtists":[{"Id":"artist","Name":"Artist"}],"ArtistItems":[{"Id":"artist","Name":"Artist"}],"GenreItems":[{"Id":"genre","Name":"Genre"}],"UserData":{"LastPlayedDate":"2026-01-01T00:00:00Z","Rating":7},"ParentBackdropItemId":"10","ParentBackdropImageTags":["cover"]});
        for (kind, item) in [
            ("MusicAlbum", album),
            ("Audio", track),
            (
                "Playlist",
                json!({"Id":"playlist","Name":"Playlist","Type":"Playlist"}),
            ),
        ] {
            Mock::given(path("/music/emby/Items"))
                .and(query_param("IncludeItemTypes", kind))
                .and(header("X-Emby-Token", "secret"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"Items":[item],"TotalRecordCount":1})),
                )
                .mount(&server)
                .await;
        }
        for route in ["Artists", "Artists/AlbumArtists"] {
            Mock::given(path(format!("/music/emby/{route}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    json!({"Items":[{"Id":"artist","Name":"Artist"}],"TotalRecordCount":1}),
                ))
                .mount(&server)
                .await;
        }
        Mock::given(path("/music/emby/MusicGenres"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"Items":[{"Id":"genre","Name":"Genre"}],"TotalRecordCount":1}),
            ))
            .mount(&server)
            .await;
        Mock::given(path("/music/emby/Users/listener/Views"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"Items":[]})))
            .mount(&server)
            .await;
        Mock::given(path("/music/emby/Playlists/playlist/Items")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"Items":[{"Id":"11","PlaylistItemId":"first"},{"Id":"11","PlaylistItemId":"second"}],"TotalRecordCount":2}))).mount(&server).await;
        let root = tempfile::tempdir().unwrap();
        let database = library::Database::open(root.path().join("library.sqlite"))
            .await
            .unwrap();
        let mut scan = library::Scan::begin(&database, "source", "Music", "music", None)
            .await
            .unwrap();
        source
            .stage_catalog(&mut scan, &|_| {}, &|| false)
            .await
            .unwrap();
        scan.finish().await.unwrap();
        let uri = library::source_entity_uri(&SourceId::new("source"), "track", "emby:track:11");
        let track = database
            .track_row_by_uri(&uri, &library::ReadCancellation::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(track.object_id, "emby:track:11");
        assert_eq!(track.rating, Some(7));
        assert!(track.last_played.is_some());
        database
            .set_rating(&library::FavoriteTarget::Track(uri.clone()), Some(9))
            .await
            .unwrap();
        source.set_rating("emby:track:11", Some(9)).await.unwrap();
        let mut scan = library::Scan::begin(&database, "source", "Music", "music", None)
            .await
            .unwrap();
        source
            .stage_catalog(&mut scan, &|_| {}, &|| false)
            .await
            .unwrap();
        scan.finish().await.unwrap();
        let rated = database
            .track_row_by_uri(&uri, &library::ReadCancellation::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rated.rating, Some(9));
        assert_eq!(rated.last_played, track.last_played);
        let requests = server.received_requests().await.unwrap();
        assert!(
            !requests
                .iter()
                .any(|r| r.url.path().ends_with("/UserData") || r.url.path().ends_with("/Rating"))
        );
        assert!(
            requests
                .iter()
                .filter(|r| r.url.path().ends_with("/Items")
                    && r.url.query_pairs().any(|(k, _)| k == "IncludeItemTypes"))
                .all(|r| r.url.query_pairs().any(|(k, v)| k == "Fields"
                    && v.contains("UserDataLastPlayedDate")
                    && !v.contains("NormalizationGain")))
        );
    }

    #[tokio::test]
    async fn overlapping_streams_and_downloads_release_only_their_own_encoding() {
        let server = MockServer::start().await;
        let source = source(server.uri());
        Mock::given(method("DELETE"))
            .and(path("/emby/Videos/ActiveEncodings"))
            .and(header("X-Emby-Token", "secret"))
            .respond_with(ResponseTemplate::new(204))
            .expect(2)
            .mount(&server)
            .await;
        let original = source
            .resolve_stream("emby:track:11", StreamQuality::Original, Some("original"))
            .await
            .unwrap();
        assert!(!original.transcoded());
        assert!(original.uri().contains("Static=true"));
        let playing = source
            .resolve_stream(
                "emby:track:11",
                StreamQuality::MaxBitrateKbps(128),
                Some("playing"),
            )
            .await
            .unwrap();
        let retained = playing.clone();
        assert_eq!(
            playing.content_type.as_deref(),
            Some("application/vnd.apple.mpegurl")
        );
        assert!(playing.transcoded());
        assert!(!format!("{playing:?}").contains("secret"));
        let url = Url::parse(playing.uri()).unwrap();
        assert_eq!(url.path(), "/emby/Audio/11/universal");
        assert!(
            url.query_pairs()
                .any(|(k, v)| k == "PlaySessionId" && v == "playing")
        );
        let download = source
            .resolve_download("emby:track:11", StreamQuality::MaxBitrateKbps(320))
            .await
            .unwrap();
        assert_eq!(download.transcoded_extension(), Some("mp3"));
        assert!(download.stream().uri().contains("AudioBitRate=320000"));
        let download_id = Url::parse(download.stream().uri())
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "PlaySessionId")
            .unwrap()
            .1
            .into_owned();
        assert_ne!(download_id, "playing");
        drop(playing);
        drop(download);
        for _ in 0..100 {
            if !server.received_requests().await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0]
                .url
                .query_pairs()
                .any(|(k, v)| k == "PlaySessionId" && v == download_id)
        );
        drop(retained);
        for _ in 0..100 {
            if server.received_requests().await.unwrap().len() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        server.verify().await;
    }

    #[tokio::test]
    async fn native_lyrics_and_playlist_rename_use_user_scoped_complete_items() {
        let server = MockServer::start().await;
        let source = source(format!("{}/emby", server.uri()));
        let playlist = json!({"Id":"playlist","Name":"Old","Type":"Playlist","GenreItems":[{"Id":"genre","Name":"Rock"}],"ProviderIds":{"Custom":"kept"}});
        Mock::given(path("/emby/Users/listener/Items/playlist"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&playlist))
            .mount(&server)
            .await;
        let mut renamed = playlist;
        renamed["Name"] = json!("New");
        Mock::given(method("POST"))
            .and(path("/emby/Items/playlist"))
            .and(body_json(renamed))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        source
            .rename_playlist("emby:playlist:playlist", "New")
            .await
            .unwrap();
        Mock::given(path("/emby/Users/listener/Items/11")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"Id":"11","MediaSources":[null,{"Id":"without-lyrics"},{"Id":"media-source","DefaultSubtitleStreamIndex":"2","MediaStreams":[{"Type":"Audio","Index":0},{"Type":"Subtitle","Index":{}},{"Type":"Subtitle","Index":1},{"Type":"Subtitle","Index":2,"Language":"eng"}]}]}))).mount(&server).await;
        Mock::given(path("/emby/Items/11/media-source/Subtitles/2/Stream.js")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"TrackEvents":[{"Text":"Timed","StartPositionTicks":1250000,"EndPositionTicks":2250000},{"Text":"Untimed"},{"Text":" "}]}))).mount(&server).await;
        let lyrics = source.lyrics("emby:track:11").await.unwrap().unwrap();
        let document = &lyrics.documents()[0];
        assert_eq!(document.language.as_deref(), Some("eng"));
        assert_eq!(document.lines.len(), 2);
        assert_eq!(document.lines[0].start_millis, Some(125));
        assert_eq!(document.lines[0].end_millis, Some(225));
        assert_eq!(document.lines[1].start_millis, None);
        server.verify().await;
    }
}
