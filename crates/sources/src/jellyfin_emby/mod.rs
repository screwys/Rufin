use crate::config::{decode_provider_payload, require_payload_version};
use crate::policy::raw_item_id;
use crate::remote_json::{field, id, items};
use crate::{
    ConnectedSource, CredentialInput, ImageBytes, SourceConfiguration, SourceEditResult,
    SourceError, SourceId, SourceResult,
};
use audio_processing::ResolvedStream;
use item::{
    ALBUM_FIELDS, ImageRef, MIXED_ITEM_FIELDS, PLAYLIST_FIELDS, TRACK_FIELDS, album_from_item,
    artist_from_item, genre_from_item, home_entry_from_item, is_audio_item, playlist_from_item,
    primary_image_ref, track_from_item,
};
use lyrics::{LyricsBundle, LyricsDocument, LyricsLine, LyricsOrigin, LyricsRole};
use playback::{QueueRepeatMode, SourceReportFact, SourceReportPhase, StreamQuality};
use reqwest::{Client, Url, header};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use tracing::instrument;

mod client;
pub(crate) mod connect;
pub(crate) mod quick_connect;
pub use connect::{EmbyConnectLogin, EmbyConnectPin, EmbyConnectServer};
pub use quick_connect::{JellyfinQuickConnect, JellyfinQuickConnectLogin};
mod emby;
mod events;
mod item;
mod jellyfin;
pub(crate) mod metadata;
mod refresh;

type PlaylistId = String;

pub(crate) use client::normalize_base_url;
use client::*;
use jellyfin::*;

const CLIENT_NAME: &str = "Rufin";
const DEVICE_NAME: &str = "Rufin";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const COLLECTION_PAGE_SIZE: usize = 500;

pub const JELLYFIN_SOURCE_ID: &str = "jellyfin";
pub const EMBY_SOURCE_ID: &str = "emby";

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ServerKind {
    Jellyfin,
    Emby,
}

impl ServerKind {
    pub fn source_kind(self) -> &'static str {
        match self {
            Self::Jellyfin => JELLYFIN_SOURCE_ID,
            Self::Emby => EMBY_SOURCE_ID,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Jellyfin => "Jellyfin",
            Self::Emby => "Emby",
        }
    }

    pub fn from_source_kind(kind: &str) -> SourceResult<Self> {
        match kind {
            JELLYFIN_SOURCE_ID => Ok(Self::Jellyfin),
            EMBY_SOURCE_ID => Ok(Self::Emby),
            _ => Err(SourceError::InvalidConfig(format!(
                "expected Jellyfin or Emby, found {kind}"
            ))),
        }
    }

    pub(crate) fn object_id(self, entity: &str, raw: &str) -> String {
        format!("{}:{entity}:{raw}", self.source_kind())
    }

    fn api_base(self, base: &Url) -> SourceResult<Url> {
        if self == Self::Emby && !base.path().trim_end_matches('/').ends_with("/emby") {
            endpoint(base, "emby/")
        } else {
            Ok(base.clone())
        }
    }

    fn authorization_header(self) -> header::HeaderName {
        match self {
            Self::Jellyfin => header::AUTHORIZATION,
            Self::Emby => header::HeaderName::from_static("x-emby-authorization"),
        }
    }
}
const SOURCE_CONFIG_VERSION: u32 = 1;

#[derive(Deserialize)]
struct JellyfinEmbySourcePayload {
    version: u32,
    base_url: String,
    #[serde(default)]
    local_url: Option<String>,
    #[serde(default)]
    server_id: Option<String>,
    user_id: String,
    username: String,
    trust_invalid_cert: bool,
    #[serde(default)]
    use_jellyfin_instant_mix: Option<bool>,
    #[serde(default)]
    use_instant_mix: Option<bool>,
    #[serde(default)]
    emby_connect: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JellyfinEmbySourceConfig {
    pub(crate) kind: ServerKind,
    pub(crate) base_url: String,
    pub(crate) local_url: Option<String>,
    pub(crate) server_id: Option<String>,
    pub(crate) user_id: String,
    pub(crate) username: String,
    pub(crate) trust_invalid_cert: bool,
    pub(crate) use_instant_mix: bool,
    pub(crate) emby_connect: bool,
}

impl JellyfinEmbySourceConfig {
    pub fn from_configuration(stored: &crate::SourceConfiguration) -> SourceResult<Self> {
        let kind = ServerKind::from_source_kind(&stored.kind)?;
        let payload: JellyfinEmbySourcePayload = decode_provider_payload(stored)?;
        require_payload_version(payload.version, SOURCE_CONFIG_VERSION)?;
        Ok(Self {
            kind,
            base_url: payload.base_url,
            local_url: payload.local_url,
            server_id: payload.server_id,
            user_id: payload.user_id,
            username: payload.username,
            trust_invalid_cert: payload.trust_invalid_cert,
            use_instant_mix: payload
                .use_instant_mix
                .or(payload.use_jellyfin_instant_mix)
                .unwrap_or(true),
            emby_connect: payload.emby_connect,
        })
    }

    pub(crate) fn into_payload(self) -> serde_json::Value {
        let mut payload = serde_json::json!({
            "version": SOURCE_CONFIG_VERSION,
            "base_url": self.base_url,
            "server_id": self.server_id,
            "user_id": self.user_id,
            "username": self.username,
            "trust_invalid_cert": self.trust_invalid_cert,
            "use_instant_mix": self.use_instant_mix,
            "emby_connect": self.emby_connect,
        });
        if let Some(local) = self.local_url {
            payload["local_url"] = local.into();
        }
        payload
    }
}

struct AuthenticatedJellyfinEmby {
    configuration: SourceConfiguration,
    source: JellyfinEmbySource,
    credential: String,
}

impl AuthenticatedJellyfinEmby {
    fn connected(self) -> ConnectedSource {
        ConnectedSource::jellyfin_emby(self.configuration, self.source, Some(self.credential))
    }
}

pub(crate) async fn connect(
    source_id: SourceId,
    kind: ServerKind,
    credentials: CredentialInput,
    use_instant_mix: bool,
    device_id: String,
) -> SourceResult<ConnectedSource> {
    JellyfinEmbySource::authenticate(source_id, kind, credentials, use_instant_mix, device_id)
        .await
        .map(AuthenticatedJellyfinEmby::connected)
}

pub(crate) fn open(
    configuration: &SourceConfiguration,
    credential: Option<String>,
    device_id: Option<String>,
) -> SourceResult<JellyfinEmbySource> {
    let config = JellyfinEmbySourceConfig::from_configuration(configuration)?;
    let credential = credential.ok_or_else(|| {
        SourceError::InvalidConfig("saved Jellyfin credentials are missing".to_string())
    })?;
    let device_id = device_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            SourceError::InvalidConfig("the app-wide Jellyfin device ID is missing".to_string())
        })?;
    JellyfinEmbySource::open(config, credential, device_id)
}

pub(crate) async fn edit(
    current: SourceConfiguration,
    current_credential: Option<String>,
    connect_manually: bool,
    mut credentials: CredentialInput,
    use_instant_mix: bool,
    device_id: Option<String>,
) -> SourceResult<SourceEditResult> {
    let saved = JellyfinEmbySourceConfig::from_configuration(&current)?;
    let name = crate::source::edited_source_name(
        credentials.source_name.as_deref().unwrap_or_default(),
        &current.name,
    );
    let local_url = local_url(credentials.local_server_url.as_deref())?;
    let address_changed = crate::source::comparable_address(&credentials.server_url)
        != crate::source::comparable_address(&saved.base_url)
        || local_url != saved.local_url;
    let username_changed = credentials.username.trim() != saved.username;
    let has_password = !credentials.secret.is_empty();
    let switching_to_manual = saved.emby_connect && connect_manually;

    if username_changed && !has_password && !switching_to_manual {
        return Err(SourceError::Other(
            "Enter the server password to save username changes.".to_string(),
        ));
    }

    if has_password || switching_to_manual {
        let device_id = device_id.ok_or_else(|| {
            SourceError::InvalidConfig("the app-wide Jellyfin device ID is missing".to_string())
        })?;
        credentials.source_name = Some(name);
        let authenticated = JellyfinEmbySource::authenticate(
            current.source_id,
            saved.kind,
            credentials,
            use_instant_mix,
            device_id,
        )
        .await?;
        return Ok(SourceEditResult::Connected(Box::new(
            authenticated.connected(),
        )));
    }

    let reopen = address_changed
        || credentials.trust_invalid_cert != saved.trust_invalid_cert
        || use_instant_mix != saved.use_instant_mix;
    let configuration = crate::config::encode_provider_payload(
        current.source_id.clone(),
        saved.kind.source_kind(),
        name,
        JellyfinEmbySourceConfig {
            emby_connect: saved.emby_connect,
            kind: saved.kind,
            base_url: normalize_base_url(&credentials.server_url)?
                .as_str()
                .trim_end_matches('/')
                .to_owned(),
            local_url,
            server_id: saved.server_id,
            user_id: saved.user_id,
            username: saved.username,
            trust_invalid_cert: credentials.trust_invalid_cert,
            use_instant_mix,
        }
        .into_payload(),
    );
    if configuration == current {
        return Ok(SourceEditResult::Unchanged);
    }
    if !reopen {
        return Ok(SourceEditResult::ConfigurationOnly(configuration));
    }
    let source = open(&configuration, current_credential, device_id)?;
    Ok(SourceEditResult::Connected(Box::new(
        ConnectedSource::jellyfin_emby(configuration, source, None),
    )))
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JellyfinEmbyClientConfig {
    pub kind: ServerKind,
    pub base_url: String,
    pub trust_invalid_cert: bool,
    pub device_id: String,
    pub device_name: String,
    pub client_name: String,
    pub client_version: String,
}
impl JellyfinEmbyClientConfig {
    pub fn new(
        base_url: impl Into<String>,
        trust_invalid_cert: bool,
        device_id: impl Into<String>,
    ) -> SourceResult<Self> {
        let device_id = device_id.into();
        if device_id.trim().is_empty() {
            return Err(SourceError::InvalidConfig(
                "the app-wide Jellyfin device ID is missing".to_string(),
            ));
        }
        Ok(Self {
            kind: ServerKind::Jellyfin,
            base_url: base_url.into(),
            trust_invalid_cert,
            device_id,
            device_name: DEVICE_NAME.to_string(),
            client_name: CLIENT_NAME.to_string(),
            client_version: CLIENT_VERSION.to_string(),
        })
    }
}
pub struct JellyfinEmbySource {
    pub(crate) kind: ServerKind,
    /// The server address, without Emby's API path.
    address: crate::address::ServerAddress<Url>,
    server_id: Option<String>,
    client: Client,
    media_client: Client,
    user_id: String,
    access_token: Arc<str>,
    device_id: Arc<str>,
    authorization: header::HeaderValue,
    use_instant_mix: bool,
    trust_invalid_cert: bool,
    connect_credential: Option<connect::EmbyConnectCredential>,
    connect_token: tokio::sync::OnceCell<(String, header::HeaderValue)>,
}
impl JellyfinEmbySource {
    fn open(
        config: JellyfinEmbySourceConfig,
        credential: String,
        device_id: String,
    ) -> SourceResult<Self> {
        let (access_token, connect_credential) =
            connect::decode_credential(&credential, config.emby_connect)?;
        let mut client_config =
            JellyfinEmbyClientConfig::new(&config.base_url, config.trust_invalid_cert, device_id)?;
        client_config.kind = config.kind;
        let address = crate::address::ServerAddress::new(
            normalize_base_url(&client_config.base_url)?,
            config
                .local_url
                .as_deref()
                .map(normalize_base_url)
                .transpose()?,
        );
        let client = build_client(client_config.trust_invalid_cert)?;
        let media_client = build_media_client(client_config.trust_invalid_cert)?;
        let authorization = authenticated_header(&client_config, &access_token)?;
        Ok(Self {
            connect_credential,
            connect_token: tokio::sync::OnceCell::new(),
            kind: config.kind,
            address,
            server_id: config.server_id,
            client,
            media_client,
            user_id: config.user_id,
            access_token: Arc::from(access_token),
            device_id: Arc::from(client_config.device_id),
            authorization,
            use_instant_mix: config.use_instant_mix,
            trust_invalid_cert: client_config.trust_invalid_cert,
        })
    }

    #[instrument(skip(credentials), fields(base_url = %credentials.server_url, username = %credentials.username, trust_invalid_cert = credentials.trust_invalid_cert))]
    async fn authenticate(
        source_id: SourceId,
        kind: ServerKind,
        credentials: CredentialInput,
        use_instant_mix: bool,
        device_id: String,
    ) -> SourceResult<AuthenticatedJellyfinEmby> {
        let CredentialInput {
            source_name: submitted_name,
            server_url,
            local_server_url,
            username,
            secret: password,
            trust_invalid_cert,
        } = credentials;
        let local_url = local_url(local_server_url.as_deref())?;
        let mut config = JellyfinEmbyClientConfig::new(&server_url, trust_invalid_cert, device_id)?;
        config.kind = kind;
        let socket_base_url = normalize_base_url(&config.base_url)?;
        let base_url = config.kind.api_base(&socket_base_url)?;
        let client = build_client(config.trust_invalid_cert)?;

        let body = AuthenticateByNameRequest { username, password };
        let auth_url = endpoint(&base_url, "Users/AuthenticateByName")?;
        let response = send_json::<Value>(
            config.kind,
            client
                .post(auth_url)
                .header(
                    config.kind.authorization_header(),
                    auth_header(&config, None),
                )
                .json(&body),
        )
        .await?;

        Self::finish_authentication(
            source_id,
            submitted_name,
            use_instant_mix,
            config,
            local_url,
            body.username,
            response,
        )
        .await
    }

    async fn finish_authentication(
        source_id: SourceId,
        submitted_name: Option<String>,
        use_instant_mix: bool,
        config: JellyfinEmbyClientConfig,
        local_url: Option<String>,
        submitted_username: String,
        response: Value,
    ) -> SourceResult<AuthenticatedJellyfinEmby> {
        let socket_base_url = normalize_base_url(&config.base_url)?;
        let base_url = config.kind.api_base(&socket_base_url)?;
        let client = build_client(config.trust_invalid_cert)?;
        let trust_invalid_cert = config.trust_invalid_cert;
        let provider_name = public_server_name(&client, &base_url, &config)
            .await
            .unwrap_or_else(|| config.kind.name().to_string());
        let server_id = id(&response["ServerId"]);
        let canonical_base_url = socket_base_url.as_str().trim_end_matches('/').to_string();
        let user_id = id(&response["User"]["Id"]).ok_or_else(|| {
            SourceError::Auth(format!("{} returned no user identity", config.kind.name()))
        })?;
        let username = field::<String>(&response["User"], "Name").unwrap_or(submitted_username);
        let credential = field::<String>(&response, "AccessToken")
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| {
                SourceError::Auth(format!("{} returned no access token", config.kind.name()))
            })?;
        let authorization = authenticated_header(&config, &credential)?;
        let configuration = crate::config::encode_provider_payload(
            source_id,
            config.kind.source_kind(),
            crate::source::configured_source_name(submitted_name, provider_name),
            JellyfinEmbySourceConfig {
                emby_connect: false,
                kind: config.kind,
                base_url: canonical_base_url,
                local_url: local_url.clone(),
                server_id: server_id.clone(),
                user_id: user_id.clone(),
                username,
                trust_invalid_cert,
                use_instant_mix,
            }
            .into_payload(),
        );
        let source = Self {
            media_client: build_media_client(config.trust_invalid_cert)?,
            connect_credential: None,
            connect_token: tokio::sync::OnceCell::new(),
            kind: config.kind,
            address: crate::address::ServerAddress::new(
                socket_base_url,
                local_url.as_deref().map(normalize_base_url).transpose()?,
            ),
            server_id,
            client,
            user_id,
            access_token: Arc::from(credential.clone()),
            device_id: Arc::from(config.device_id),
            authorization,
            use_instant_mix,
            trust_invalid_cert: config.trust_invalid_cert,
        };
        Ok(AuthenticatedJellyfinEmby {
            configuration,
            source,
            credential,
        })
    }
}

/// A saved local address in the same form as the public one.
fn local_url(raw: Option<&str>) -> SourceResult<Option<String>> {
    crate::address::optional_local(raw)
        .map(|address| {
            normalize_base_url(address).map(|url| url.as_str().trim_end_matches('/').to_owned())
        })
        .transpose()
}

impl JellyfinEmbySource {
    /// The address requests use now, with Emby's API path.
    fn base_url(&self) -> Url {
        self.kind
            .api_base(&self.address.current())
            .expect("an API path joins any server address")
    }

    fn socket_base_url(&self) -> Url {
        self.address.current()
    }

    /// Uses the local address while this same server answers there.
    pub(crate) async fn select_address(&self) {
        self.address
            .select(async |local| {
                let Ok(url) = self
                    .kind
                    .api_base(local)
                    .and_then(|base| endpoint(&base, "System/Info/Public"))
                else {
                    return false;
                };
                let request = self
                    .client
                    .get(url)
                    .timeout(crate::address::LOCAL_PROBE_TIMEOUT);
                match send_json::<Value>(self.kind, request).await {
                    Ok(info) => match (&self.server_id, id(&info["Id"])) {
                        (Some(saved), Some(found)) => *saved == found,
                        (None, found) => found.is_some(),
                        (Some(_), None) => false,
                    },
                    Err(_) => false,
                }
            })
            .await;
    }
}

fn authenticated_header(
    config: &JellyfinEmbyClientConfig,
    access_token: &str,
) -> SourceResult<header::HeaderValue> {
    auth_header(config, Some(access_token))
        .parse()
        .map_err(|error| SourceError::InvalidConfig(format!("invalid Jellyfin identity: {error}")))
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn saved_mix_preferences_and_manual_tokens_survive_migration() {
        for (kind, legacy, expected) in [
            ("emby", None, true),
            ("jellyfin", None, true),
            ("jellyfin", Some(false), false),
            ("jellyfin", Some(true), true),
        ] {
            let mut payload = serde_json::json!({"version":1,"base_url":"https://music.example","server_id":"server","user_id":"user","username":"Listener","trust_invalid_cert":false});
            if let Some(value) = legacy {
                payload["use_jellyfin_instant_mix"] = value.into();
            }
            let configuration = crate::config::encode_provider_payload(
                SourceId::new("source"),
                kind,
                "Music",
                payload,
            );
            let config = JellyfinEmbySourceConfig::from_configuration(&configuration).unwrap();
            assert_eq!(config.use_instant_mix, expected);
            assert!(!config.emby_connect);
            let migrated = config.into_payload();
            assert_eq!(migrated["use_instant_mix"], expected);
            assert!(migrated.get("use_jellyfin_instant_mix").is_none());
            let reopened = open(
                &configuration,
                Some("legacy-token".into()),
                Some("device".into()),
            )
            .unwrap();
            assert_eq!(reopened.access_token.as_ref(), "legacy-token");
            assert!(reopened.connect_credential.is_none());
        }
    }
}

#[cfg(test)]
mod address_tests {
    use super::*;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

    #[tokio::test]
    async fn local_address_serves_only_while_the_same_server_answers_there() {
        let (public, local, other) = (
            MockServer::start().await,
            MockServer::start().await,
            MockServer::start().await,
        );
        for (server, id) in [(&local, "server"), (&other, "another")] {
            Mock::given(path("/System/Info/Public"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({"Id": id})),
                )
                .mount(server)
                .await;
        }
        let open_with = |local_url: String| {
            let payload = serde_json::json!({"version":1,"base_url":public.uri(),"local_url":local_url,"server_id":"server","user_id":"user","username":"Listener","trust_invalid_cert":false});
            let configuration = crate::config::encode_provider_payload(
                SourceId::new("source"),
                "jellyfin",
                "Music",
                payload,
            );
            open(&configuration, Some("token".into()), Some("device".into())).unwrap()
        };
        let address = |server: &MockServer| normalize_base_url(&server.uri()).unwrap();

        let home = open_with(local.uri());
        assert_eq!(home.socket_base_url(), address(&public));
        home.select_address().await;
        assert_eq!(home.socket_base_url(), address(&local));

        let elsewhere = open_with(other.uri());
        elsewhere.select_address().await;
        assert_eq!(elsewhere.socket_base_url(), address(&public));

        local.reset().await;
        home.select_address().await;
        assert_eq!(home.socket_base_url(), address(&public));
    }
}

impl std::fmt::Debug for JellyfinEmbySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JellyfinEmbySource")
            .field("kind", &self.kind)
            .field("address", &self.address)
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}
