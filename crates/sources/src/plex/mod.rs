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
/// How long plex.tv may take to list a server's connections before the saved list is used.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PlexSourceConfig {
    pub version: u32,
    pub server_id: String,
    pub profile_id: String,
    /// The connection chosen at setup, which requests use while no other is found: `local`,
    /// `relay` and `require_valid_certificate` describe it.
    pub base_url: String,
    pub address_override: Option<String>,
    pub local: bool,
    pub relay: bool,
    pub owned: bool,
    pub trust_invalid_cert: bool,
    #[serde(default)]
    pub require_valid_certificate: bool,
}
impl PlexSourceConfig {
    pub(crate) fn from_configuration(configuration: &SourceConfiguration) -> SourceResult<Self> {
        let config: Self = crate::config::decode_provider_payload(configuration)?;
        crate::config::require_payload_version(config.version, 1)?;
        Ok(config)
    }
}

pub struct PlexSource {
    pub(crate) login: Arc<tokio::sync::Mutex<PlexLogin>>,
    config: PlexSourceConfig,
    /// The connection requests use now.
    connection: crate::address::Chosen<PlexConnection>,
    /// Set once a connection was chosen in this run. A saved source's first request waits
    /// for that: the saved connection may be another network's.
    chosen: tokio::sync::OnceCell<()>,
    /// Follows the certificate setting.
    client: Client,
    /// Accepts only a valid certificate, for the connections that must present one.
    strict_client: Client,
    media_client: Client,
}
impl PlexSource {
    fn new(config: PlexSourceConfig, login: PlexLogin) -> SourceResult<Self> {
        let timeouts = RemoteTimeouts {
            connect: Duration::from_secs(10),
            request: Duration::from_secs(60),
        };
        let strict_client = remote_http::build_client(false, timeouts, HTTP)?;
        let client = if config.trust_invalid_cert {
            remote_http::build_client(true, timeouts, HTTP)?
        } else {
            strict_client.clone()
        };
        let connection = crate::address::Chosen::new(PlexConnection {
            address: reqwest::Url::parse(&config.base_url)
                .map_err(|error| SourceError::InvalidConfig(error.to_string()))?,
            local: config.local,
            relay: config.relay,
            require_valid_certificate: config.require_valid_certificate,
        });
        let media_client = remote_http::build_url_client(
            config.trust_invalid_cert,
            RemoteTimeouts {
                connect: Duration::from_secs(10),
                request: Duration::from_secs(60),
            },
            HTTP,
        )?;
        Ok(Self {
            config,
            connection,
            chosen: tokio::sync::OnceCell::new_with(Some(())),
            client,
            strict_client,
            media_client,
            login: Arc::new(tokio::sync::Mutex::new(login)),
        })
    }

    /// Uses the best of the server's connections that answers now: those plex.tv lists, or the
    /// saved ones while plex.tv does not answer. An address typed for the server stays.
    /// Returns whether the connection changed.
    pub(crate) async fn select_address(&self) -> bool {
        if self.chosen.initialized() {
            return self.choose_connection().await;
        }
        // The choice a first request waits for is this one.
        let mut moved = false;
        self.chosen
            .get_or_init(async || moved = self.choose_connection().await)
            .await;
        moved
    }

    async fn choose_connection(&self) -> bool {
        if self
            .config
            .address_override
            .as_deref()
            .is_some_and(|address| !address.trim().is_empty())
        {
            return false;
        }
        self.connection
            .choose(async || {
                let profile = &self.config.profile_id;
                let login = self.login.lock().await.for_setup();
                let listed =
                    tokio::time::timeout(LIST_TIMEOUT, login.listed_servers(profile, &[])).await;
                let login = match listed {
                    Ok(Ok((servers, tokens))) => {
                        let mut login = self.login.lock().await;
                        if let Err(error) = login.remember_servers(profile, &servers, tokens) {
                            tracing::warn!(%error, "Plex connections were not saved");
                        }
                        login.for_setup()
                    }
                    _ => login,
                };
                let server = login.server(&self.config.server_id)?;
                login
                    .select_connection(profile, &server, None, self.config.trust_invalid_cert)
                    .await
                    .ok()
            })
            .await
    }

    /// The connection requests use now, once one was chosen in this run.
    async fn connection(&self) -> PlexConnection {
        self.chosen
            .get_or_init(async || {
                self.choose_connection().await;
            })
            .await;
        self.connection.current()
    }

    /// The client a connection's requests use.
    fn client(&self, connection: &PlexConnection) -> &Client {
        if connection.require_valid_certificate {
            &self.strict_client
        } else {
            &self.client
        }
    }

    /// Whether a connection's stream may present an invalid certificate.
    fn trust_invalid_certificate(&self, connection: &PlexConnection) -> bool {
        self.config.trust_invalid_cert && !connection.require_valid_certificate
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        params: &[(&str, String)],
    ) -> SourceResult<RequestBuilder> {
        let connection = self.connection().await;
        let base = &connection.address;
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
                self.client(&connection),
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

    /// A GET with the server's clock, for the start of a listing or catch-up.
    pub(crate) async fn get_dated(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> SourceResult<(Value, Option<i64>)> {
        remote_http::json_dated(
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
        require_valid_certificate: connection.require_valid_certificate,
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
    let mut source = PlexSource::new(
        PlexSourceConfig::from_configuration(configuration)?,
        PlexLogin::decode(
            &credential.ok_or_else(|| SourceError::Auth("Saved Plex login is missing".into()))?,
        )?,
    )?;
    source.chosen = tokio::sync::OnceCell::new();
    Ok(source)
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
        config.require_valid_certificate = connection.require_valid_certificate;
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
