use std::thread;
use std::time::{Duration, Instant};

use md5_digest::{Digest, Md5};
use reqwest::blocking::Client;
use serde_json::Value;
use tracing::debug;

use crate::scrobbling::retry::{DeliveryError, Submission, SubmissionTrack};
use crate::scrobbling::settings::{
    AudioscrobblerSettings, RUFIN_API_KEY, RUFIN_API_SECRET, lastfm_application,
};

const LASTFM_API_URL: &str = "https://ws.audioscrobbler.com/2.0/";
const LIBREFM_API_URL: &str = "https://libre.fm/2.0/";
const LASTFM_AUTH_URL: &str = "https://www.last.fm/api/auth/";
const USER_AGENT: &str = concat!("Rufin/", env!("CARGO_PKG_VERSION"));

/// An Audioscrobbler server: where its API and its sign-in page are.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Service {
    name: String,
    api_url: String,
    auth_url: String,
}

impl Service {
    pub(crate) fn lastfm() -> Self {
        Self {
            name: "Last.fm".into(),
            api_url: LASTFM_API_URL.into(),
            auth_url: LASTFM_AUTH_URL.into(),
        }
    }

    pub(crate) fn librefm() -> Self {
        Self::server("Libre.fm", LIBREFM_API_URL)
    }

    /// A GNU FM style server: the API at `…/2.0/`, Libre.fm's sign-in page at `…/api/auth/`.
    pub(crate) fn server(name: &str, url: &str) -> Self {
        let root = url.trim().trim_end_matches('/');
        let root = root.strip_suffix("/2.0").unwrap_or(root);
        Self {
            name: name.into(),
            api_url: format!("{root}/2.0/"),
            auth_url: format!("{root}/api/auth/"),
        }
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn api_url(&self) -> &str {
        &self.api_url
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioscrobblerSession {
    pub username: String,
    pub session_key: String,
}

pub struct AudioscrobblerAuthorization {
    service: Service,
    api_key: String,
    api_secret: String,
    token: String,
    url: String,
}

impl AudioscrobblerAuthorization {
    /// Signs in to Last.fm with the user's own API account, or Rufin's when they entered none.
    pub fn lastfm(api_key: &str, api_secret: &str) -> Result<Self, String> {
        let (api_key, api_secret) = lastfm_application(api_key, api_secret);
        if api_secret.is_empty() {
            return Err("Enter the shared secret of your API key, or clear the key.".to_string());
        }
        Self::start(Service::lastfm(), api_key, api_secret)
    }

    /// Signs in to Libre.fm with Rufin's application key.
    pub(crate) fn librefm() -> Result<Self, String> {
        Self::start(Service::librefm(), RUFIN_API_KEY, RUFIN_API_SECRET)
    }

    fn start(service: Service, api_key: &str, api_secret: &str) -> Result<Self, String> {
        let token = request_auth_token(&service, api_key, api_secret)?;
        Ok(Self {
            url: auth_url(&service, api_key, &token),
            service,
            api_key: api_key.to_string(),
            api_secret: api_secret.to_string(),
            token,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn wait_for_session(self) -> Result<Option<AudioscrobblerSession>, String> {
        for _ in 0..30 {
            thread::sleep(Duration::from_secs(2));
            if let Some(session) =
                request_session(&self.service, &self.api_key, &self.api_secret, &self.token)?
            {
                return Ok(Some(session));
            }
        }
        Ok(None)
    }
}

fn auth_url(service: &Service, api_key: &str, token: &str) -> String {
    format!(
        "{}?api_key={}&token={}",
        service.auth_url,
        api_key.trim(),
        token.trim()
    )
}

fn request_auth_token(
    service: &Service,
    api_key: &str,
    api_secret: &str,
) -> Result<String, String> {
    let params = vec![
        ("api_key".to_string(), api_key.to_string()),
        ("method".to_string(), "auth.getToken".to_string()),
    ];
    let mut form = params.clone();
    form.push(("api_sig".to_string(), api_signature(&params, api_secret)));
    form.push(("format".to_string(), "json".to_string()));
    let value = post_form(service.api_url(), form)?;
    api_error(&value, service.name())?;
    value
        .get("token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .map(|token| token.trim().to_string())
        .ok_or_else(|| format!("{} did not return an auth token.", service.name()))
}

fn request_session(
    service: &Service,
    api_key: &str,
    api_secret: &str,
    token: &str,
) -> Result<Option<AudioscrobblerSession>, String> {
    let params = vec![
        ("api_key".to_string(), api_key.to_string()),
        ("method".to_string(), "auth.getSession".to_string()),
        ("token".to_string(), token.to_string()),
    ];
    let mut form = params.clone();
    form.push(("api_sig".to_string(), api_signature(&params, api_secret)));
    form.push(("format".to_string(), "json".to_string()));
    let value = post_form(service.api_url(), form)?;
    if let Some((code, message)) = api_error_value(&value) {
        if code == 14 {
            return Ok(None);
        }
        return Err(format!("{} API error {code}: {message}", service.name()));
    }
    session_from_value(&value, service.name()).map(Some)
}

/// Signs in to a server with the user's name and password, signed with Rufin's application key.
/// The password is sent once, for the session key; Maloja takes one of its API keys as it.
pub(crate) fn mobile_session(
    service: &Service,
    username: &str,
    password: &str,
) -> Result<AudioscrobblerSession, String> {
    let params = vec![
        ("api_key".to_string(), RUFIN_API_KEY.to_string()),
        ("method".to_string(), "auth.getMobileSession".to_string()),
        ("password".to_string(), password.to_string()),
        ("username".to_string(), username.trim().to_string()),
    ];
    let mut form = params.clone();
    form.push((
        "api_sig".to_string(),
        api_signature(&params, RUFIN_API_SECRET),
    ));
    form.push(("format".to_string(), "json".to_string()));
    let value = post_form(service.api_url(), form)?;
    api_error(&value, service.name())?;
    let mut session = session_from_value(&value, service.name())?;
    // Maloja answers the key without the name.
    if session.username.is_empty() {
        session.username = username.trim().to_string();
    }
    Ok(session)
}

pub(crate) fn submit(
    client: &Client,
    service: &Service,
    settings: &AudioscrobblerSettings,
    submission: &Submission,
) -> Result<(), DeliveryError> {
    let (operation, params) = match submission {
        Submission::NowPlaying(track) => (
            "track.updateNowPlaying",
            submission_params("track.updateNowPlaying", settings, track, None),
        ),
        Submission::Scrobble {
            track,
            started_at_unix_seconds,
        } => (
            "track.scrobble",
            submission_params(
                "track.scrobble",
                settings,
                track,
                Some(*started_at_unix_seconds),
            ),
        ),
    };
    debug!(
        service = service.name(),
        method = "POST",
        public_url = service.api_url(),
        operation,
        "sending remote request"
    );
    let started = Instant::now();
    let response = client
        .post(service.api_url())
        .form(&params)
        .send()
        .map_err(|error| DeliveryError::retry(error.to_string()))?;
    let status = response.status();
    debug!(
        service = service.name(),
        method = "POST",
        operation,
        status = status.as_u16(),
        elapsed_ms = started.elapsed().as_millis(),
        "received remote response"
    );
    let value = response.json::<Value>().unwrap_or(Value::Null);
    if status.as_u16() == 429 || status.is_server_error() {
        return Err(DeliveryError::retry(format!(
            "{} returned HTTP {status}",
            service.name()
        )));
    }
    if let Some((code, message)) = api_error_value(&value) {
        return delivery_result(service, submission, code, &message);
    }
    if !status.is_success() {
        return Err(DeliveryError::stop(format!(
            "{} returned HTTP {status}",
            service.name()
        )));
    }
    debug!(
        service = service.name(),
        operation, "submitted remote playback event"
    );
    Ok(())
}

/// What an API error answer means. A server without now playing, such as Maloja, answers its
/// update with error 3 (invalid method): there is nothing to update there.
fn delivery_result(
    service: &Service,
    submission: &Submission,
    code: i64,
    message: &str,
) -> Result<(), DeliveryError> {
    let error = format!("{} API error {code}: {message}", service.name());
    Err(match code {
        3 if matches!(submission, Submission::NowPlaying(_)) => {
            debug!(service = service.name(), "server has no now playing");
            return Ok(());
        }
        11 | 16 | 29 => DeliveryError::retry(error),
        4 | 9 | 10 | 13 => DeliveryError::credential_blocked(error),
        _ => DeliveryError::stop(error),
    })
}

fn submission_params(
    method: &str,
    settings: &AudioscrobblerSettings,
    track: &SubmissionTrack,
    started_at_unix_seconds: Option<i64>,
) -> Vec<(String, String)> {
    let mut params = vec![
        ("api_key".to_string(), settings.api_key.clone()),
        ("artist".to_string(), track.artist.clone()),
        (
            "duration".to_string(),
            (track.duration_millis / 1_000).to_string(),
        ),
        ("method".to_string(), method.to_string()),
        ("sk".to_string(), settings.session_key.clone()),
        ("track".to_string(), track.title.clone()),
    ];
    if !track.album.is_empty() {
        params.push(("album".to_string(), track.album.clone()));
    }
    if let Some(started_at) = started_at_unix_seconds {
        params.push(("timestamp".to_string(), started_at.to_string()));
    }
    let signature = api_signature(&params, &settings.api_secret);
    params.push(("api_sig".to_string(), signature));
    params.push(("format".to_string(), "json".to_string()));
    params
}

fn post_form(api_url: &str, form: Vec<(String, String)>) -> Result<Value, String> {
    let operation = form
        .iter()
        .find_map(|(key, value)| (key == "method").then_some(value.as_str()))
        .unwrap_or("unknown");
    let client = Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|error| error.to_string())?;
    debug!(
        service = "audioscrobbler",
        method = "POST",
        public_url = api_url,
        operation,
        "sending remote request"
    );
    let started = Instant::now();
    let response = client
        .post(api_url)
        .form(&form)
        .send()
        .map_err(|error| error.to_string())?;
    let status = response.status();
    debug!(
        service = "audioscrobbler",
        method = "POST",
        operation,
        status = status.as_u16(),
        elapsed_ms = started.elapsed().as_millis(),
        "received remote response"
    );
    let value = response.json::<Value>().unwrap_or(Value::Null);
    if !status.is_success() && api_error_value(&value).is_none() {
        return Err(format!("Audioscrobbler auth returned HTTP {status}"));
    }
    Ok(value)
}

fn api_error(value: &Value, service: &str) -> Result<(), String> {
    if let Some((code, message)) = api_error_value(value) {
        return Err(format!("{service} API error {code}: {message}"));
    }
    Ok(())
}

fn api_error_value(value: &Value) -> Option<(i64, String)> {
    let code = value.get("error").and_then(Value::as_i64)?;
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error")
        .to_string();
    Some((code, message))
}

fn session_from_value(value: &Value, service: &str) -> Result<AudioscrobblerSession, String> {
    let session = value
        .get("session")
        .ok_or_else(|| format!("{service} did not return a session."))?;
    let username = session
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let session_key = session
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if session_key.is_empty() {
        return Err(format!("{service} did not return a session key."));
    }
    Ok(AudioscrobblerSession {
        username,
        session_key,
    })
}

fn api_signature(params: &[(String, String)], secret: &str) -> String {
    let mut params = params
        .iter()
        .filter(|(key, _)| key != "format" && key != "callback" && key != "api_sig")
        .collect::<Vec<_>>();
    params.sort_by(|left, right| left.0.cmp(&right.0));
    let mut input = String::new();
    for (key, value) in params {
        input.push_str(key);
        input.push_str(value);
    }
    input.push_str(secret);
    format!("{:032x}", u128::from_be_bytes(Md5::digest(input).into()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn signature_excludes_transport_parameters() {
        let params = vec![
            ("track".to_string(), "Track".to_string()),
            ("format".to_string(), "json".to_string()),
            ("method".to_string(), "track.scrobble".to_string()),
            ("artist".to_string(), "Artist".to_string()),
            ("api_key".to_string(), "key".to_string()),
            ("timestamp".to_string(), "10".to_string()),
            ("sk".to_string(), "session".to_string()),
        ];
        assert_eq!(
            api_signature(&params, "secret"),
            "9ea42092c79325d9e328dcc8c3fa4eeb"
        );
    }

    #[test]
    fn session_mapping_requires_a_key() {
        let value = json!({"session": {"name": "listener", "key": "session-key"}});
        assert_eq!(
            session_from_value(&value, "Last.fm").expect("session"),
            AudioscrobblerSession {
                username: "listener".to_string(),
                session_key: "session-key".to_string(),
            }
        );
    }

    #[test]
    fn submission_form_preserves_wire_fields() {
        let settings = AudioscrobblerSettings {
            api_key: "key".to_string(),
            api_secret: "secret".to_string(),
            session_key: "session".to_string(),
            ..AudioscrobblerSettings::default()
        };
        let track = SubmissionTrack {
            title: "Track".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            duration_millis: 180_000,
        };
        let params = submission_params("track.scrobble", &settings, &track, Some(10));
        assert!(params.contains(&("duration".to_string(), "180".to_string())));
        assert!(params.contains(&("timestamp".to_string(), "10".to_string())));
        assert!(params.iter().any(|(key, _)| key == "api_sig"));
    }

    #[test]
    fn auth_urls_preserve_existing_endpoints() {
        assert_eq!(
            auth_url(&Service::lastfm(), "api-key", "auth-token"),
            "https://www.last.fm/api/auth/?api_key=api-key&token=auth-token"
        );
        assert_eq!(
            auth_url(&Service::librefm(), RUFIN_API_KEY, "auth-token"),
            "https://libre.fm/api/auth/?api_key=rufin&token=auth-token"
        );
    }

    #[test]
    fn submission_errors_keep_credentials_separate_from_retry_and_rejection() {
        let track = SubmissionTrack {
            title: "Track".to_string(),
            artist: "Artist".to_string(),
            album: String::new(),
            duration_millis: 180_000,
        };
        let scrobble = Submission::Scrobble {
            track: track.clone(),
            started_at_unix_seconds: 10,
        };
        let result = |submission: &Submission, code| {
            delivery_result(&Service::lastfm(), submission, code, "message")
        };
        assert!(matches!(
            result(&scrobble, 9),
            Err(DeliveryError::CredentialBlocked(_))
        ));
        assert!(matches!(
            result(&scrobble, 11),
            Err(DeliveryError::Retry(_))
        ));
        assert!(matches!(result(&scrobble, 6), Err(DeliveryError::Stop(_))));
        assert!(matches!(result(&scrobble, 3), Err(DeliveryError::Stop(_))));
        assert!(result(&Submission::NowPlaying(track), 3).is_ok());
    }
}
