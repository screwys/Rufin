use std::time::{Duration, Instant};

use reqwest::{blocking::Client, header::AUTHORIZATION};
use serde_json::{Value, json};
use tracing::debug;

use crate::retry::{DeliveryError, Submission, SubmissionTrack, USER_AGENT};
use crate::settings::ListenBrainzSettings;

const API_URL: &str = "https://api.listenbrainz.org/1/submit-listens";

pub fn connect(user_token: &str) -> Result<String, String> {
    let response = Client::builder()
        .timeout(Duration::from_secs(6))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|error| error.to_string())?
        .get("https://api.listenbrainz.org/1/validate-token")
        .header(AUTHORIZATION, authorization_header(user_token.trim()))
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| error.to_string())?
        .json::<Value>()
        .map_err(|error| error.to_string())?;
    if response.get("valid").and_then(Value::as_bool) != Some(true) {
        return Err("The API token was not accepted. Please connect again.".into());
    }
    Ok(response
        .get("user_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

pub(crate) fn submit(
    client: &Client,
    settings: &ListenBrainzSettings,
    submission: &Submission,
) -> Result<(), DeliveryError> {
    let operation = match submission {
        Submission::NowPlaying(_) => "playing_now",
        Submission::Scrobble { .. } => "single",
    };
    debug!(
        service = "ListenBrainz",
        method = "POST",
        public_url = API_URL,
        operation,
        "sending remote request"
    );
    let started = Instant::now();
    let response = client
        .post(API_URL)
        .header(AUTHORIZATION, authorization_header(&settings.user_token))
        .json(&payload(submission))
        .send()
        .map_err(|error| DeliveryError::retry(error.to_string()))?;
    let status = response.status();
    debug!(
        service = "ListenBrainz",
        method = "POST",
        operation,
        status = status.as_u16(),
        elapsed_ms = started.elapsed().as_millis(),
        "received remote response"
    );
    if let Some(error) = delivery_error(status) {
        return Err(error);
    }
    debug!("submitted ListenBrainz event");
    Ok(())
}

fn delivery_error(status: reqwest::StatusCode) -> Option<DeliveryError> {
    if status.as_u16() == 429 || status.is_server_error() {
        Some(DeliveryError::retry(format!(
            "ListenBrainz returned HTTP {status}"
        )))
    } else if status.as_u16() == 401 {
        Some(DeliveryError::credential_blocked(format!(
            "ListenBrainz returned HTTP {status}"
        )))
    } else if !status.is_success() {
        Some(DeliveryError::stop(format!(
            "ListenBrainz returned HTTP {status}"
        )))
    } else {
        None
    }
}

fn payload(submission: &Submission) -> Value {
    let (listen_type, listened_at, track) = match submission {
        Submission::NowPlaying(track) => ("playing_now", None, track),
        Submission::Scrobble {
            track,
            started_at_unix_seconds,
        } => ("single", Some(*started_at_unix_seconds), track),
    };
    let mut listen = listen(track);
    if let Some(listened_at) = listened_at {
        listen["listened_at"] = json!(listened_at);
    }
    json!({
        "listen_type": listen_type,
        "payload": [listen],
    })
}

fn listen(track: &SubmissionTrack) -> Value {
    let mut listen = json!({
        "track_metadata": {
            "artist_name": track.artist.as_str(),
            "track_name": track.title.as_str(),
            "additional_info": {
                "duration_ms": track.duration_millis,
                "submission_client": "Rufin",
            },
        },
    });
    if !track.album.is_empty() {
        listen["track_metadata"]["release_name"] = json!(track.album.as_str());
    }
    listen
}

fn authorization_header(user_token: &str) -> String {
    format!("Token {}", user_token.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playing_now_and_single_payloads_have_distinct_protocol_shapes() {
        let track = SubmissionTrack {
            title: "Track".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            duration_millis: 180_000,
        };
        let now = payload(&Submission::NowPlaying(track.clone()));
        assert_eq!(now["listen_type"], "playing_now");
        assert!(now["payload"][0].get("listened_at").is_none());

        let single = payload(&Submission::Scrobble {
            track,
            started_at_unix_seconds: 1_700_000_000,
        });
        assert_eq!(single["listen_type"], "single");
        assert_eq!(single["payload"][0]["listened_at"], 1_700_000_000_i64);
        assert_eq!(
            single["payload"][0]["track_metadata"]["additional_info"]["duration_ms"],
            180_000_u64
        );
    }

    #[test]
    fn authorization_uses_token_scheme() {
        assert_eq!(authorization_header(" user-token "), "Token user-token");
    }

    #[test]
    fn response_status_separates_credentials_retry_and_rejection() {
        assert!(matches!(
            delivery_error(reqwest::StatusCode::UNAUTHORIZED),
            Some(DeliveryError::CredentialBlocked(_))
        ));
        assert!(matches!(
            delivery_error(reqwest::StatusCode::TOO_MANY_REQUESTS),
            Some(DeliveryError::Retry(_))
        ));
        assert!(matches!(
            delivery_error(reqwest::StatusCode::BAD_REQUEST),
            Some(DeliveryError::Stop(_))
        ));
    }
}
