use crate::config::{decode_provider_payload, require_payload_version};
use crate::policy::{raw_item_id, stable_hash};
use crate::remote_json::{field, id, items};
use crate::{
    ConnectedSource, CredentialHostInput, ImageBytes, JellyfinEmbySettingsInput,
    JellyfinEmbySetupInput, SourceConfiguration, SourceEditResult, SourceError, SourceId,
    SourceResult,
};
use item::{
    ALBUM_FIELDS, ImageRef, MIXED_ITEM_FIELDS, PLAYLIST_FIELDS, TRACK_FIELDS, album_from_item,
    artist_from_item, genre_from_item, is_audio_item, playlist_from_item, primary_image_ref,
    stage_album, stage_artist, stage_genre, stage_track, track_from_item,
};
use lyrics::{LyricsBundle, LyricsDocument, LyricsLine, LyricsOrigin, LyricsRole};
use playback::{RepeatMode, ResolvedStream, SourceReportFact, SourceReportPhase, StreamQuality};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

    fn object_id(self, entity: &str, raw: &str) -> String {
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
pub(crate) const JELLYFIN_TRANSCODED_DOWNLOAD_BITRATE_LIMIT_KBPS: u32 = 256;
const SOURCE_CONFIG_VERSION: u32 = 1;

#[derive(Deserialize)]
struct JellyfinEmbySourcePayload {
    version: u32,
    base_url: String,
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
            server_id: payload.server_id,
            user_id: payload.user_id,
            username: payload.username,
            trust_invalid_cert: payload.trust_invalid_cert,
            use_instant_mix: payload
                .use_instant_mix
                .or(payload.use_jellyfin_instant_mix)
                .unwrap_or(kind == ServerKind::Emby),
            emby_connect: payload.emby_connect,
        })
    }

    pub(crate) fn into_payload(self) -> serde_json::Value {
        serde_json::json!({
            "version": SOURCE_CONFIG_VERSION,
            "base_url": self.base_url,
            "server_id": self.server_id,
            "user_id": self.user_id,
            "username": self.username,
            "trust_invalid_cert": self.trust_invalid_cert,
            "use_instant_mix": self.use_instant_mix,
            "emby_connect": self.emby_connect,
        })
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
    input: JellyfinEmbySetupInput,
) -> SourceResult<ConnectedSource> {
    JellyfinEmbySource::authenticate(source_id, input)
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
    input: JellyfinEmbySettingsInput,
    device_id: Option<String>,
) -> SourceResult<SourceEditResult> {
    let JellyfinEmbySettingsInput {
        connect_manually,
        credentials,
        use_instant_mix,
    } = input;
    let saved = JellyfinEmbySourceConfig::from_configuration(&current)?;
    let name = crate::source::edited_source_name(&credentials.name, &current.name);
    let address_changed = crate::source::comparable_address(&credentials.base_url)
        != crate::source::comparable_address(&saved.base_url);
    let username_changed = credentials.username.trim() != saved.username;
    let has_password = !credentials.password.is_empty();
    let switching_to_manual = saved.emby_connect && connect_manually;

    if (address_changed || username_changed) && !has_password && !switching_to_manual {
        return Err(SourceError::Other(
            "Enter the server password to save address or username changes.".to_string(),
        ));
    }

    if has_password || switching_to_manual {
        let device_id = device_id.ok_or_else(|| {
            SourceError::InvalidConfig("the app-wide Jellyfin device ID is missing".to_string())
        })?;
        let authenticated = JellyfinEmbySource::authenticate(
            current.source_id,
            JellyfinEmbySetupInput {
                kind: saved.kind,
                credentials: CredentialHostInput {
                    server_name: Some(name),
                    server_url: credentials.base_url,
                    username: credentials.username,
                    password: credentials.password,
                    trust_invalid_cert: credentials.trust_invalid_cert,
                },
                use_instant_mix,
                device_id,
            },
        )
        .await?;
        return Ok(SourceEditResult::Connected(Box::new(
            authenticated.connected(),
        )));
    }

    let reopen = credentials.trust_invalid_cert != saved.trust_invalid_cert
        || use_instant_mix != saved.use_instant_mix;
    let configuration = crate::config::encode_provider_payload(
        current.source_id.clone(),
        saved.kind.source_kind(),
        name,
        JellyfinEmbySourceConfig {
            emby_connect: saved.emby_connect,
            kind: saved.kind,
            base_url: saved.base_url,
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

#[derive(Clone, Debug, Eq, PartialEq)]
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
    socket_base_url: Url,
    client: Client,
    base_url: Url,
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
        let socket_base_url = normalize_base_url(&client_config.base_url)?;
        let base_url = config.kind.api_base(&socket_base_url)?;
        let client = build_client(client_config.trust_invalid_cert)?;
        let authorization = authenticated_header(&client_config, &access_token)?;
        Ok(Self {
            connect_credential,
            connect_token: tokio::sync::OnceCell::new(),
            kind: config.kind,
            socket_base_url,
            client,
            base_url,
            user_id: config.user_id,
            access_token: Arc::from(access_token),
            device_id: Arc::from(client_config.device_id),
            authorization,
            use_instant_mix: config.use_instant_mix,
            trust_invalid_cert: client_config.trust_invalid_cert,
        })
    }

    #[instrument(skip(input), fields(base_url = %input.credentials.server_url, username = %input.credentials.username, trust_invalid_cert = input.credentials.trust_invalid_cert))]
    async fn authenticate(
        source_id: SourceId,
        input: JellyfinEmbySetupInput,
    ) -> SourceResult<AuthenticatedJellyfinEmby> {
        let CredentialHostInput {
            server_name: submitted_name,
            server_url,
            username,
            password,
            trust_invalid_cert,
        } = input.credentials;
        let mut config =
            JellyfinEmbyClientConfig::new(&server_url, trust_invalid_cert, input.device_id)?;
        config.kind = input.kind;
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
            input.use_instant_mix,
            config,
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
                server_id,
                user_id: user_id.clone(),
                username,
                trust_invalid_cert,
                use_instant_mix,
            }
            .into_payload(),
        );
        let source = Self {
            connect_credential: None,
            connect_token: tokio::sync::OnceCell::new(),
            kind: config.kind,
            socket_base_url,
            client,
            base_url,
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
            ("jellyfin", None, false),
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

impl std::fmt::Debug for JellyfinEmbySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JellyfinEmbySource")
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}
