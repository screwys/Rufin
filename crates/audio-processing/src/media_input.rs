use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamWindow {
    pub start_millis: u64,
    pub end_millis: u64,
}

/// How an HTTP input that cannot seek in its bytes starts again at a source time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeSeek {
    /// `start_ms` and an optional `end_ms`.
    Millis,
    /// The named query parameter, in 100 ns ticks.
    Ticks(&'static str),
    /// The named query parameter, in whole seconds.
    Seconds(&'static str),
}

#[derive(Clone)]
pub struct ResolvedStream {
    pub local_input: bool,
    transcoded: bool,
    pub content_type: Option<String>,
    uri: String,
    redacted_uri: String,
    trust_invalid_certificate: bool,
    window: Option<StreamWindow>,
    time_seek: Option<TimeSeek>,
    time_offset_millis: Option<u64>,
    resource: Option<std::sync::Arc<dyn Send + Sync>>,
}

impl PartialEq for ResolvedStream {
    fn eq(&self, other: &Self) -> bool {
        self.uri == other.uri
            && self.local_input == other.local_input
            && self.transcoded == other.transcoded
            && self.redacted_uri == other.redacted_uri
            && self.trust_invalid_certificate == other.trust_invalid_certificate
            && self.window == other.window
            && self.time_seek == other.time_seek
            && self.time_offset_millis == other.time_offset_millis
            && self.content_type == other.content_type
    }
}

impl Eq for ResolvedStream {}

impl ResolvedStream {
    pub fn new(uri: impl Into<String>) -> Self {
        let uri = uri.into();
        Self {
            local_input: uri.starts_with("file:") || uri.starts_with("content:"),
            redacted_uri: redact_sensitive_uri(&uri),
            transcoded: false,
            uri,
            trust_invalid_certificate: false,
            window: None,
            time_seek: None,
            time_offset_millis: None,
            resource: None,
            content_type: None,
        }
    }

    pub fn with_redacted(uri: impl Into<String>, redacted_uri: impl Into<String>) -> Self {
        let uri = uri.into();
        Self {
            local_input: uri.starts_with("file:") || uri.starts_with("content:"),
            transcoded: false,
            uri,
            redacted_uri: redacted_uri.into(),
            trust_invalid_certificate: false,
            window: None,
            time_seek: None,
            time_offset_millis: None,
            resource: None,
            content_type: None,
        }
    }

    pub fn with_trust_invalid_certificate(mut self, trust: bool) -> Self {
        self.trust_invalid_certificate = trust;
        self
    }

    pub fn with_transcoding(mut self, transcoded: bool) -> Self {
        self.transcoded = transcoded;
        self
    }

    pub fn transcoded(&self) -> bool {
        self.transcoded
    }

    pub fn with_content_type(mut self, content_type: Option<String>) -> Self {
        self.content_type = content_type;
        self
    }

    /// Retain temporary input or its serving endpoint while this stream is in use.
    /// Resource ownership does not participate in stream identity or equality.
    pub fn with_resource(mut self, resource: std::sync::Arc<dyn Send + Sync>) -> Self {
        self.resource = Some(resource);
        self
    }

    pub fn with_window(mut self, start_millis: u64, end_millis: u64) -> Self {
        if end_millis > start_millis {
            self.window = Some(StreamWindow {
                start_millis,
                end_millis,
            });
        }
        self
    }

    pub fn without_window(mut self) -> Self {
        self.window = None;
        self
    }

    /// The HTTP input starts at a source time when asked this way.
    pub fn with_time_seek(mut self, time_seek: TimeSeek) -> Self {
        self.time_seek = Some(time_seek);
        self
    }

    pub fn time_seekable(&self) -> bool {
        self.time_seek.is_some()
    }
    pub fn time_offset_millis(&self) -> Option<u64> {
        self.time_offset_millis
    }

    pub fn reopened_at(&self, logical_millis: u64) -> Result<Self, url::ParseError> {
        let mut stream = self.clone();
        let Some(time_seek) = self.time_seek else {
            return Ok(stream);
        };
        let start = self.start_millis().saturating_add(logical_millis);
        let start = self.end_millis().map_or(start, |end| start.min(end));
        let (name, start, value) = match time_seek {
            TimeSeek::Millis => ("start_ms", start, start),
            TimeSeek::Ticks(name) => (name, start, start.saturating_mul(10_000)),
            TimeSeek::Seconds(name) => (name, start / 1000 * 1000, start / 1000),
        };
        let mut uri = url::Url::parse(self.uri())?;
        let pairs = uri
            .query_pairs()
            .filter(|(key, _)| key != name && key != "end_ms")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        uri.set_query(None);
        uri.query_pairs_mut()
            .extend_pairs(pairs)
            .append_pair(name, &value.to_string());
        if let Some(end) = self.end_millis().filter(|_| time_seek == TimeSeek::Millis) {
            uri.query_pairs_mut()
                .append_pair("end_ms", &end.to_string());
        }
        stream.uri = uri.into();
        stream.redacted_uri = redact_sensitive_uri(&stream.uri);
        stream.time_offset_millis = Some(start);
        Ok(stream)
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }
    pub fn redacted_uri(&self) -> &str {
        &self.redacted_uri
    }
    pub fn trust_invalid_certificate(&self) -> bool {
        self.trust_invalid_certificate
    }
    pub fn start_millis(&self) -> u64 {
        self.window.as_ref().map_or(0, |window| window.start_millis)
    }
    pub fn end_millis(&self) -> Option<u64> {
        self.window.as_ref().map(|window| window.end_millis)
    }
    pub fn window(&self) -> Option<&StreamWindow> {
        self.window.as_ref()
    }
}

impl fmt::Debug for ResolvedStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedStream")
            .field("uri", &self.redacted_uri)
            .field("window", &self.window)
            .finish()
    }
}

fn redact_sensitive_uri(uri: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(uri) else {
        return "<redacted>".into();
    };
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_fragment(None);
    let query = parsed.query().map(|query| {
        query
            .split('&')
            .map(|pair| format!("{}=<redacted>", pair.split('=').next().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("&")
    });
    parsed.set_query(None);
    let mut redacted = parsed.to_string();
    if let Some(query) = query {
        redacted.push('?');
        redacted.push_str(&query);
    }
    redacted
}
