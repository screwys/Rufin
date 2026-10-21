use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use reqwest::Url;
use reqwest::blocking::{Client, Response};
use serde_json::Value;
use tracing::debug;

const USER_AGENT: &str = concat!(
    "Rufin/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/screwys/Rufin)"
);

pub(crate) const IMAGE_MAX_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const JSON_MAX_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn client() -> Result<&'static Client, String> {
    static CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .timeout(Duration::from_secs(8))
                .user_agent(USER_AGENT)
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub(crate) fn ipv4_client() -> Result<&'static Client, String> {
    static CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
                .timeout(Duration::from_secs(8))
                .user_agent(USER_AGENT)
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub(crate) fn fetch_json(client: &Client, url: Url, context: &str) -> Result<Value, String> {
    decode_json(get(client, url, context)?, context)
}

pub(crate) fn decode_json(response: Response, context: &str) -> Result<Value, String> {
    if !response.status().is_success() {
        return Err(format!(
            "{context} failed with status {}",
            response.status()
        ));
    }
    let bytes = read_response_bounded(response, JSON_MAX_BYTES, context)?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

pub(crate) fn fetch_optional_json(
    client: &Client,
    url: Url,
    context: &str,
) -> Result<Option<Value>, String> {
    let response = get(client, url, context)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    decode_json(response, context).map(Some)
}

pub(crate) fn download(
    client: &Client,
    url: &str,
    context: &str,
) -> Result<Option<Vec<u8>>, String> {
    let url = Url::parse(url).map_err(|error| error.to_string())?;
    let response = get(client, url, context)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!(
            "{context} failed with status {}",
            response.status()
        ));
    }
    let bytes = read_response_bounded(response, IMAGE_MAX_BYTES, context)?;
    Ok((!bytes.is_empty()).then_some(bytes))
}

fn get(client: &Client, url: Url, context: &str) -> Result<Response, String> {
    debug!(
        service = "metadata-lookup",
        method = "GET",
        public_url = %url,
        %context,
        "sending remote request"
    );
    let started = Instant::now();
    let response = client.get(url).send().map_err(|error| {
        debug!(%error, %context, "remote request failed");
        if error.is_timeout() {
            format!("{context} timed out")
        } else {
            format!("{context} request failed")
        }
    })?;
    debug!(
        service = "metadata-lookup",
        method = "GET",
        status = response.status().as_u16(),
        elapsed_ms = started.elapsed().as_millis(),
        %context,
        "received remote response"
    );
    Ok(response)
}

fn read_response_bounded(
    response: Response,
    limit: usize,
    context: &str,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!(
            "{context} exceeded {} MiB limit",
            limit / 1024 / 1024
        ));
    }
    read_bounded(response, limit, context)
}

fn read_bounded(mut reader: impl Read, limit: usize, context: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Ok(bytes);
        }
        if bytes
            .len()
            .checked_add(read)
            .is_none_or(|length| length > limit)
        {
            return Err(format!(
                "{context} exceeded {} MiB limit",
                limit / 1024 / 1024
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
}

pub(crate) fn wait_for_slot(slot: &std::sync::Mutex<Option<Instant>>, interval: Duration) {
    let mut next = slot.lock().unwrap_or_else(|error| error.into_inner());
    let now = Instant::now();
    let start = next.map_or(now, |next| next.max(now));
    *next = Some(start + interval);
    drop(next);
    std::thread::sleep(start.saturating_duration_since(now));
}
