use super::*;
use crate::remote_json::{field, items};
use ::lyrics::{LyricsBundle, LyricsDocument, LyricsLine, LyricsOrigin, LyricsRole};

impl PlexSource {
    pub(crate) async fn lyrics(&self, object: &str) -> SourceResult<Option<LyricsBundle>> {
        let mut metadata = self.metadata(object).await?;
        if lyric_key(&metadata).is_none() {
            let response = self
                .get(
                    &format!("/library/metadata/{}", crate::policy::raw_item_id(object)),
                    &[("checkFiles", "1".into()), ("includeStreams", "1".into())],
                )
                .await?;
            if let Some(item) = items(&response["MediaContainer"]["Metadata"]).first() {
                metadata = item.clone();
            }
        }
        let Some(key) = lyric_key(&metadata) else {
            return Ok(None);
        };
        let mut request = self
            .request(Method::GET, key, &[("format", "xml".into())])
            .await?
            .build()
            .map_err(|error| remote_http::map_reqwest_error(error, HTTP))?;
        request.headers_mut().insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/xml"),
        );
        let response = self
            .client
            .execute(request)
            .await
            .map_err(|error| remote_http::map_reqwest_error(error, HTTP))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = response
            .error_for_status()
            .map_err(|error| remote_http::map_reqwest_error(error, HTTP))?;
        let body = remote_http::bounded_response_body(response, HTTP, RESPONSE).await?;
        Ok(parse(&body))
    }
}
fn lyric_key(item: &Value) -> Option<&str> {
    let mut other = None;
    for media in items(&item["Media"]) {
        for part in items(&media["Part"]) {
            for stream in items(&part["Stream"]) {
                if field::<u8>(stream, "streamType") != Some(4) {
                    continue;
                }
                if let Some(key) = stream["key"].as_str() {
                    if stream["format"] == "lrc" || stream["codec"] == "lrc" {
                        return Some(key);
                    }
                    other.get_or_insert(key);
                }
            }
        }
    }
    other
}
fn parse(bytes: &[u8]) -> Option<LyricsBundle> {
    let mut lines = Vec::new();
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut current: Option<LyricsLine> = None;
    let mut timed = true;
    loop {
        use quick_xml::events::Event;
        match reader.read_event().ok()? {
            Event::Start(element) | Event::Empty(element) => {
                let attribute = |name: &str| {
                    element
                        .attributes()
                        .flatten()
                        .find(|attribute| attribute.key.as_ref() == name)
                        .and_then(|attribute| {
                            attribute
                                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                .ok()
                                .map(|value| value.into_owned())
                        })
                };
                match element.name().as_ref() {
                    "Lyrics" => {
                        timed =
                            attribute("timed").is_none_or(|timed| timed != "0" && timed != "false")
                    }
                    "Line" => {
                        if let Some(line) = current.take().filter(|line| !line.text.is_empty()) {
                            lines.push(line);
                        }
                        current = Some(LyricsLine {
                            text: attribute("text").unwrap_or_default(),
                            start_millis: attribute("startOffset")
                                .and_then(|value| value.parse().ok())
                                .filter(|_| timed),
                            end_millis: None,
                            cue_lines: Vec::new(),
                        });
                    }
                    "Span" => {
                        if let Some(line) = &mut current {
                            line.text.push_str(&attribute("text").unwrap_or_default());
                            if timed && line.start_millis.is_none() {
                                line.start_millis =
                                    attribute("startOffset").and_then(|value| value.parse().ok());
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(text) => {
                if let Some(line) = &mut current {
                    line.text
                        .push_str(&text.xml_content(quick_xml::XmlVersion::Implicit1_0));
                }
            }
            Event::CData(text) => {
                if let Some(line) = &mut current {
                    line.text.push_str(&text);
                }
            }
            Event::GeneralRef(value) => {
                if let Some(line) = &mut current {
                    if let Some(character) = value.resolve_char_ref().ok()? {
                        line.text.push(character);
                    } else {
                        line.text.push_str(match &*value {
                            "amp" => "&",
                            "lt" => "<",
                            "gt" => ">",
                            "quot" => "\"",
                            "apos" => "'",
                            _ => return None,
                        });
                    }
                }
            }
            Event::End(element) if element.name().as_ref() == "Line" => {
                if let Some(line) = current.take().filter(|line| !line.text.is_empty()) {
                    lines.push(line);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if let Some(line) = current.filter(|line| !line.text.is_empty()) {
        lines.push(line);
    }
    (!lines.is_empty()).then(|| {
        LyricsBundle::from_documents(
            LyricsOrigin::Native,
            vec![LyricsDocument {
                role: LyricsRole::Original,
                language: None,
                offset_millis: 0,
                lines,
                agents: Vec::new(),
            }],
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn missing_stream_details_are_reloaded_before_reading_server_lyrics() {
        use wiremock::matchers::{header, method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let source = super::super::catalog::tests::source(&server);
        Mock::given(method("GET")).and(path("/library/metadata/track")).and(query_param("includeLyrics","1")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"MediaContainer":{"Metadata":[{"ratingKey":"track","Media":[{"Part":[{}]}]}]}}))).expect(1).mount(&server).await;
        Mock::given(method("GET")).and(path("/library/metadata/track")).and(query_param("checkFiles","1")).and(query_param("includeStreams","1")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"MediaContainer":{"Metadata":[{"ratingKey":"track","Media":[{"Part":[{"Stream":[{"streamType":4,"key":"/library/streams/7","format":"lrc"}]}]}]}]}}))).expect(1).mount(&server).await;
        Mock::given(method("GET")).and(path("/library/streams/7")).and(query_param("format","xml")).and(header("Accept","application/xml")).respond_with(ResponseTemplate::new(200).set_body_string("<MediaContainer><Lyrics timed=\"1\"><Line startOffset=\"1000\" text=\"Verse\"/></Lyrics></MediaContainer>")).expect(1).mount(&server).await;
        let bundle = source.lyrics("plex:track:track").await.unwrap().unwrap();
        assert_eq!(bundle.documents()[0].lines[0].text, "Verse");
        assert_eq!(bundle.documents()[0].lines[0].start_millis, Some(1000));
    }
    #[test]
    fn chooses_lrc_and_preserves_structured_line_timing() {
        let item = serde_json::json!({"Media":[{"Part":[{"Stream":[{"streamType":4,"key":"txt","format":"txt"},{"streamType":"4","key":"lrc","format":"lrc"}]}]}]});
        assert_eq!(lyric_key(&item), Some("lrc"));
        let lyrics = parse(br#"<MediaContainer><Lyrics timed="1"><Line><Span startOffset="250" text="Hello "/><Span text="world"/></Line><Line startOffset="700" text="Next"/></Lyrics></MediaContainer>"#).unwrap();
        assert_eq!(lyrics.documents()[0].lines[0].text, "Hello world");
        assert_eq!(lyrics.documents()[0].lines[0].start_millis, Some(250));
        assert_eq!(lyrics.documents()[0].lines[1].start_millis, Some(700));
    }
}
