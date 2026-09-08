mod auth;
mod companion;
pub use companion::*;

pub use auth::{PlexBrowserLogin, PlexConnection, PlexLogin, PlexProfile, PlexServer};

mod catalog;
mod events;
mod item;
mod lyrics;
mod metadata;
mod operations;
mod stream;

use crate::remote_http::{self, BodyLimit, RemoteHttpPolicy, RemoteTimeouts};
use crate::{SourceConfiguration, SourceError, SourceResult};
use reqwest::{Client, Method, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

pub const PLEX_SOURCE_ID: &str = "plex";
const RESPONSE: BodyLimit = BodyLimit {
    max_bytes: 32 * 1024 * 1024,
    context: "Plex response",
};
const HTTP: RemoteHttpPolicy = RemoteHttpPolicy {
    service: "Plex",
    auth_context: "Plex authorization failed",
    error_body: BodyLimit {
        max_bytes: 65536,
        context: "Plex error",
    },
    redact_error_url: Some(redact),
};
fn redact(url: &mut reqwest::Url) {
    url.set_query(None);
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PlexSourceConfig {
    pub version: u32,
    pub server_id: String,
    pub profile_id: String,
    pub base_url: String,
    pub address_override: Option<String>,
    pub local: bool,
    pub relay: bool,
    pub owned: bool,
    pub trust_invalid_cert: bool,
}
impl PlexSourceConfig {
    pub(crate) fn from_configuration(configuration: &SourceConfiguration) -> SourceResult<Self> {
        let config: Self = crate::config::decode_provider_payload(configuration)?;
        crate::config::require_payload_version(config.version, 1)?;
        Ok(config)
    }
}

pub(crate) struct PlexSource {
    pub(crate) login: Arc<tokio::sync::Mutex<PlexLogin>>,
    config: PlexSourceConfig,
    client: Client,
}
impl PlexSource {
    fn new(config: PlexSourceConfig, login: PlexLogin) -> SourceResult<Self> {
        let client = remote_http::build_client(
            config.trust_invalid_cert,
            RemoteTimeouts {
                connect: Duration::from_secs(10),
                request: Duration::from_secs(60),
            },
            HTTP,
        )?;
        Ok(Self {
            config,
            client,
            login: Arc::new(tokio::sync::Mutex::new(login)),
        })
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        params: &[(&str, String)],
    ) -> SourceResult<RequestBuilder> {
        let base = reqwest::Url::parse(&self.config.base_url)
            .map_err(|error| SourceError::InvalidConfig(error.to_string()))?;
        let url = base
            .join(path)
            .map_err(|error| SourceError::InvalidConfig(error.to_string()))?;
        if url.origin() != base.origin() {
            return Err(SourceError::InvalidRequest(
                "Plex resource belongs to another server",
            ));
        }
        let login = self.login.lock().await;
        Ok(login
            .server_request(
                &self.client,
                method,
                url.as_str(),
                &self.config.profile_id,
                &self.config.server_id,
            )?
            .query(params))
    }
    pub(crate) async fn get(&self, path: &str, params: &[(&str, String)]) -> SourceResult<Value> {
        remote_http::json(
            self.request(Method::GET, path, params).await?,
            HTTP,
            RESPONSE,
        )
        .await
    }
    async fn unit(
        &self,
        method: Method,
        path: &str,
        params: &[(&str, String)],
    ) -> SourceResult<()> {
        remote_http::unit(self.request(method, path, params).await?, HTTP).await
    }
    pub(crate) async fn metadata(&self, object_id: &str) -> SourceResult<Value> {
        let prefix = if object_id.starts_with("plex:playlist:") {
            "/playlists"
        } else {
            "/library/metadata"
        };
        let response = self
            .get(
                &format!("{prefix}/{}", crate::policy::raw_item_id(object_id)),
                &[
                    ("includeLyrics", "1".into()),
                    ("includeStations", "1".into()),
                    ("includeStreams", "1".into()),
                ],
            )
            .await?;
        crate::remote_json::items(&response["MediaContainer"]["Metadata"])
            .first()
            .cloned()
            .ok_or(SourceError::NotFound)
    }
}
pub(crate) fn plex_id(kind: &str, id: &str) -> String {
    format!("plex:{kind}:{id}")
}

pub(crate) async fn connect(
    source_id: crate::SourceId,
    input: crate::PlexSetupInput,
) -> SourceResult<crate::ConnectedSource> {
    let connection = input
        .login
        .select_connection(
            &input.profile_id,
            &input.server,
            input.address_override.as_deref(),
            input.trust_invalid_cert,
        )
        .await?;
    let config = PlexSourceConfig {
        version: 1,
        server_id: input.server.id,
        profile_id: input.profile_id,
        base_url: connection.address.to_string(),
        address_override: input.address_override,
        local: connection.local,
        relay: connection.relay,
        owned: input.server.owned,
        trust_invalid_cert: input.trust_invalid_cert,
    };
    let credential = input.login.encode()?;
    let configuration = crate::config::encode_provider_payload(
        source_id,
        PLEX_SOURCE_ID,
        crate::source::configured_source_name(Some(input.name), input.server.name),
        serde_json::to_value(&config)?,
    );
    Ok(crate::ConnectedSource::plex(
        configuration,
        PlexSource::new(config, input.login)?,
        Some(credential),
    ))
}
pub(crate) fn open(
    configuration: &SourceConfiguration,
    credential: Option<String>,
) -> SourceResult<PlexSource> {
    PlexSource::new(
        PlexSourceConfig::from_configuration(configuration)?,
        PlexLogin::decode(
            &credential.ok_or_else(|| SourceError::Auth("Saved Plex login is missing".into()))?,
        )?,
    )
}
pub(crate) async fn edit(
    current: SourceConfiguration,
    credential: Option<String>,
    input: crate::PlexSettingsInput,
) -> SourceResult<crate::SourceEditResult> {
    let mut config = PlexSourceConfig::from_configuration(&current)?;
    let mut login = PlexLogin::decode(
        &credential
            .as_deref()
            .ok_or_else(|| SourceError::Auth("Saved Plex login is missing".into()))?,
    )?;
    if input.address_override != config.address_override
        || input.trust_invalid_cert != config.trust_invalid_cert
        || (config.relay && input.address_override.is_none())
    {
        let server = login
            .servers(&config.profile_id, &[])
            .await?
            .into_iter()
            .find(|server| server.id == config.server_id)
            .ok_or(SourceError::NotFound)?;
        let connection = login
            .select_connection(
                &config.profile_id,
                &server,
                input.address_override.as_deref(),
                input.trust_invalid_cert,
            )
            .await?;
        config.base_url = connection.address.to_string();
        config.local = connection.local;
        config.relay = connection.relay;
    }
    config.address_override = input.address_override;
    config.trust_invalid_cert = input.trust_invalid_cert;
    let configuration = crate::config::encode_provider_payload(
        current.source_id.clone(),
        PLEX_SOURCE_ID,
        crate::source::edited_source_name(&input.name, &current.name),
        serde_json::to_value(&config)?,
    );
    if configuration == current {
        return Ok(crate::SourceEditResult::Unchanged);
    }
    let credential = login.encode()?;
    Ok(crate::SourceEditResult::Connected(Box::new(
        crate::ConnectedSource::plex(
            configuration,
            PlexSource::new(config, login)?,
            Some(credential),
        ),
    )))
}
