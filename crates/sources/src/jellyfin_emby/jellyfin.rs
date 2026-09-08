use super::*;
use serde::Serialize;

impl JellyfinEmbySource {
    pub(crate) fn jellyfin_download(
        &self,
        track_object_id: &str,
        quality: StreamQuality,
    ) -> SourceResult<crate::ResolvedDownload> {
        if quality == StreamQuality::Original {
            let stream = stream_descriptor(
                &self.base_url,
                &self.user_id,
                &self.device_id,
                &self.access_token,
                self.trust_invalid_cert,
                track_object_id,
                quality,
            )?;
            return Ok(crate::ResolvedDownload::new(stream, None));
        }

        let StreamQuality::MaxBitrateKbps(kbps) = quality else {
            unreachable!("original downloads return before transcoding")
        };
        let raw_track_id = raw_item_id(track_object_id);
        let bitrate = kbps
            .min(super::JELLYFIN_TRANSCODED_DOWNLOAD_BITRATE_LIMIT_KBPS)
            .saturating_mul(1_000)
            .to_string();
        let mut url = endpoint(&self.base_url, &format!("Audio/{raw_track_id}/Universal"))?;
        url.query_pairs_mut()
            .append_pair("UserId", &self.user_id)
            .append_pair("DeviceId", &self.device_id)
            .append_pair("api_key", &self.access_token)
            .append_pair("transcodingContainer", "ogg")
            .append_pair("audioCodec", "opus")
            .append_pair("audioBitRate", &bitrate);
        let mut redacted_url = url.clone();
        redacted_url
            .query_pairs_mut()
            .clear()
            .append_pair("UserId", &self.user_id)
            .append_pair("DeviceId", &self.device_id)
            .append_pair("api_key", "<redacted>")
            .append_pair("transcodingContainer", "ogg")
            .append_pair("audioCodec", "opus")
            .append_pair("audioBitRate", &bitrate);
        let stream = ResolvedStream::with_redacted(url.to_string(), redacted_url.to_string())
            .with_trust_invalid_certificate(self.trust_invalid_cert);
        Ok(crate::ResolvedDownload::new(stream, Some("ogg")))
    }

    pub(crate) async fn jellyfin_create_playlist(
        &self,
        name: &str,
        track_ids: &[String],
    ) -> SourceResult<PlaylistId> {
        let url = endpoint(&self.base_url, "Playlists")?;
        let body = CreatePlaylistDto {
            name: name.to_string(),
            ids: raw_track_ids(track_ids),
            user_id: Some(self.user_id.clone()),
            media_type: Some("Audio".to_string()),
            is_public: false,
        };
        let result = self
            .send_json::<PlaylistCreationResult>(self.client.post(url).json(&body))
            .await?;
        Ok(String::from(self.kind.object_id("playlist", &result.id)))
    }

    pub(crate) async fn jellyfin_rename_playlist(
        &self,
        playlist_id: &str,
        name: &str,
    ) -> SourceResult<()> {
        let url = endpoint(
            &self.base_url,
            &format!("Playlists/{}", raw_item_id(playlist_id)),
        )?;
        let body = UpdatePlaylistDto {
            name: Some(name.to_string()),
        };
        self.send_unit(self.client.post(url).json(&body)).await
    }

    pub(crate) async fn write_lyrics(&self, track_id: &str, lyrics: &str) -> SourceResult<()> {
        let raw_track_id = raw_item_id(track_id);
        let mut url = endpoint(&self.base_url, &format!("Audio/{raw_track_id}/Lyrics"))?;
        url.query_pairs_mut().append_pair("fileName", "lyrics.lrc");
        self.send_unit(
            self.client
                .post(url)
                .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(lyrics.to_string()),
        )
        .await
    }

    pub(super) async fn jellyfin_lyrics(
        &self,
        track_id: &str,
    ) -> SourceResult<Option<LyricsBundle>> {
        let raw_track_id = raw_item_id(track_id);
        let local_url = endpoint(&self.base_url, &format!("Audio/{raw_track_id}/Lyrics"))?;
        match self.send_json::<Value>(self.client.get(local_url)).await {
            Ok(item) => Ok(Some(lyrics_from_item(&item))),
            Err(SourceError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

pub(super) fn stream_descriptor(
    base_url: &Url,
    user_id: &str,
    device_id: &str,
    access_token: &str,
    trust_invalid_certificate: bool,
    track_object_id: &str,
    quality: StreamQuality,
) -> SourceResult<ResolvedStream> {
    let raw_track_id = raw_item_id(track_object_id);
    let max_bitrate = quality
        .max_bitrate_kbps()
        .map(|kbps| kbps.saturating_mul(1_000).to_string());

    let mut url = endpoint(base_url, &format!("Audio/{raw_track_id}/stream"))?;
    let static_stream = if max_bitrate.is_some() {
        "false"
    } else {
        "true"
    };
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("UserId", user_id)
            .append_pair("DeviceId", device_id)
            .append_pair("Static", static_stream)
            .append_pair("api_key", access_token);
        if let Some(max_bitrate) = &max_bitrate {
            query
                .append_pair("MaxStreamingBitrate", max_bitrate)
                .append_pair("TranscodingContainer", "mp3")
                .append_pair("AudioCodec", "mp3");
        }
    }
    let mut redacted_url = url.clone();
    {
        let mut redacted_query = redacted_url.query_pairs_mut();
        redacted_query
            .clear()
            .append_pair("UserId", user_id)
            .append_pair("DeviceId", device_id)
            .append_pair("Static", static_stream)
            .append_pair("api_key", "<redacted>");
        if let Some(max_bitrate) = &max_bitrate {
            redacted_query
                .append_pair("MaxStreamingBitrate", max_bitrate)
                .append_pair("TranscodingContainer", "mp3")
                .append_pair("AudioCodec", "mp3");
        }
    }
    Ok(
        ResolvedStream::with_redacted(url.to_string(), redacted_url.to_string())
            .with_content_type(max_bitrate.map(|_| "audio/mpeg".to_string()))
            .with_trust_invalid_certificate(trust_invalid_certificate),
    )
}

fn lyrics_from_item(item: &Value) -> LyricsBundle {
    LyricsBundle::from_documents(
        LyricsOrigin::Native,
        vec![LyricsDocument {
            role: LyricsRole::Original,
            language: None,
            offset_millis: 0,
            lines: items(&item["Lyrics"])
                .iter()
                .filter_map(|line| {
                    let text: String = field(line, "Text")?;
                    (!text.trim().is_empty()).then_some(LyricsLine {
                        text,
                        start_millis: ticks_to_millis(field(line, "Start")),
                        end_millis: None,
                        cue_lines: Vec::new(),
                    })
                })
                .collect(),
            agents: Vec::new(),
        }],
    )
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct CreatePlaylistDto {
    pub(super) name: String,
    pub(super) ids: Vec<String>,
    pub(super) user_id: Option<String>,
    pub(super) media_type: Option<String>,
    pub(super) is_public: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct UpdatePlaylistDto {
    pub(super) name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn jellyfin_lyrics_preserve_usable_lines_and_accept_successful_uploads() {
        let server = MockServer::start().await;
        let source = JellyfinEmbySource::open(
            JellyfinEmbySourceConfig {
                emby_connect: false,
                kind: ServerKind::Jellyfin,
                base_url: server.uri(),
                server_id: Some("server".into()),
                user_id: "listener".into(),
                username: "Listener".into(),
                trust_invalid_cert: false,
                use_instant_mix: false,
            },
            "token".into(),
            "device".into(),
        )
        .unwrap();
        Mock::given(method("GET"))
            .and(path("/Audio/track/Lyrics"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"Lyrics":[
                {"Text":"First","Start":"10000"}, null, {"Text":{}},
                {"Text":"Second","Start":{}}, {"Text":"Third","Start":30000}
            ]})))
            .mount(&server)
            .await;
        let bundle = source
            .jellyfin_lyrics("jellyfin:track:track")
            .await
            .unwrap()
            .unwrap();
        let lines = &bundle.documents()[0].lines;
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["First", "Second", "Third"]
        );
        assert_eq!(
            lines
                .iter()
                .map(|line| line.start_millis)
                .collect::<Vec<_>>(),
            [Some(1), None, Some(3)]
        );
        Mock::given(method("POST"))
            .and(path("/Audio/track/Lyrics"))
            .respond_with(ResponseTemplate::new(200).set_body_string("saved"))
            .mount(&server)
            .await;
        source
            .write_lyrics("jellyfin:track:track", "First")
            .await
            .unwrap();
    }
}
