use crate::config::{decode_provider_payload, require_payload_version};
use crate::policy::{raw_item_id, stable_hash};
use crate::{
    ConnectedSource, CredentialHostInput, ImageBytes, JellyfinSettingsInput, JellyfinSetupInput,
    LyricsSearch, NativeLyricLine, NativeLyrics, NativeLyricsDocument, NativeLyricsRole,
    SourceConfiguration, SourceEditResult, SourceError, SourceId, SourceResult,
};
pub use discovery::{DiscoveredJellyfinServer, discover_jellyfin_servers};
use item::{
    ALBUM_FIELDS, ImageRef, ItemQueryResult, JellyfinItem, MIXED_ITEM_FIELDS, PLAYLIST_FIELDS,
    TRACK_FIELDS, album_from_item, artist_from_item, genre_from_item, is_audio_item,
    playlist_from_item, primary_image_ref, stage_album, stage_artist, stage_genre, stage_track,
    track_from_item,
};
use playback::{
    RepeatMode, ResolvedStream, SourceReportFact, SourceReportPhase, StreamQuality, StreamRequest,
};
use reqwest::{Client, Url, header};
use serde::Deserialize;
use std::sync::Arc;
use tracing::instrument;

mod client;
mod discovery;
mod events;
mod item;
pub(crate) mod metadata;
mod refresh;

type PlaylistId = String;

use client::*;
pub(crate) use client::{jellyfin_id, normalize_base_url};

const CLIENT_NAME: &str = "Rufin";
const DEVICE_NAME: &str = "Rufin";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const COLLECTION_PAGE_SIZE: usize = 500;

pub const JELLYFIN_SOURCE_ID: &str = "jellyfin";
pub(crate) const JELLYFIN_TRANSCODED_DOWNLOAD_BITRATE_LIMIT_KBPS: u32 = 256;
const SOURCE_CONFIG_VERSION: u32 = 1;

#[derive(Deserialize)]
struct JellyfinSourcePayload {
    version: u32,
    base_url: String,
    #[serde(default)]
    server_id: Option<String>,
    user_id: String,
    username: String,
    trust_invalid_cert: bool,
    use_jellyfin_instant_mix: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JellyfinSourceConfig {
    pub(crate) base_url: String,
    pub(crate) server_id: Option<String>,
    pub(crate) user_id: String,
    pub(crate) username: String,
    pub(crate) trust_invalid_cert: bool,
    pub(crate) use_instant_mix: bool,
}

impl JellyfinSourceConfig {
    pub fn from_configuration(stored: &crate::SourceConfiguration) -> SourceResult<Self> {
        if stored.kind != JELLYFIN_SOURCE_ID {
            return Err(SourceError::InvalidConfig(format!(
                "expected {JELLYFIN_SOURCE_ID}, found {}",
                stored.kind
            )));
        }
        let payload: JellyfinSourcePayload = decode_provider_payload(stored)?;
        require_payload_version(payload.version, SOURCE_CONFIG_VERSION)?;
        Ok(Self {
            base_url: payload.base_url,
            server_id: payload.server_id,
            user_id: payload.user_id,
            username: payload.username,
            trust_invalid_cert: payload.trust_invalid_cert,
            use_instant_mix: payload.use_jellyfin_instant_mix,
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
            "use_jellyfin_instant_mix": self.use_instant_mix,
        })
    }

    pub(crate) fn same_account(&self, other: &Self) -> SourceResult<bool> {
        if self.user_id != other.user_id {
            return Ok(false);
        }
        match (&self.server_id, &other.server_id) {
            (Some(current), Some(next)) => Ok(current == next),
            _ => Ok(normalize_base_url(&self.base_url)? == normalize_base_url(&other.base_url)?),
        }
    }
}

struct AuthenticatedJellyfin {
    configuration: SourceConfiguration,
    source: JellyfinSource,
    credential: String,
}

impl AuthenticatedJellyfin {
    fn connected(mut self, source_id: Option<SourceId>) -> ConnectedSource {
        if let Some(source_id) = source_id {
            self.configuration.source_id = source_id;
        }
        ConnectedSource::jellyfin(self.configuration, self.source, Some(self.credential))
    }
}

pub(crate) async fn connect(input: JellyfinSetupInput) -> SourceResult<ConnectedSource> {
    JellyfinSource::authenticate(input)
        .await
        .map(|authenticated| authenticated.connected(None))
}

pub(crate) fn open(
    configuration: &SourceConfiguration,
    credential: Option<String>,
    device_id: Option<String>,
) -> SourceResult<JellyfinSource> {
    let config = JellyfinSourceConfig::from_configuration(configuration)?;
    let credential = credential.ok_or_else(|| {
        SourceError::InvalidConfig("saved Jellyfin credentials are missing".to_string())
    })?;
    let device_id = device_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            SourceError::InvalidConfig("the app-wide Jellyfin device ID is missing".to_string())
        })?;
    JellyfinSource::open(config, credential, device_id)
}

pub(crate) async fn edit(
    current: SourceConfiguration,
    current_credential: Option<String>,
    input: JellyfinSettingsInput,
    device_id: Option<String>,
) -> SourceResult<SourceEditResult> {
    let JellyfinSettingsInput {
        credentials,
        use_instant_mix,
    } = input;
    crate::source::require_source_edit(&current, JELLYFIN_SOURCE_ID)?;
    let saved = JellyfinSourceConfig::from_configuration(&current)?;
    let name = crate::source::edited_source_name(&credentials.name, &current.name);
    let address_changed = crate::source::comparable_address(&credentials.base_url)
        != crate::source::comparable_address(&saved.base_url);
    let username_changed = credentials.username.trim() != saved.username;
    let has_password = !credentials.password.is_empty();

    if (address_changed || username_changed) && !has_password {
        return Err(SourceError::Other(
            "Enter the server password to save address or username changes.".to_string(),
        ));
    }

    if has_password {
        let device_id = device_id.ok_or_else(|| {
            SourceError::InvalidConfig("the app-wide Jellyfin device ID is missing".to_string())
        })?;
        let authenticated = JellyfinSource::authenticate(JellyfinSetupInput {
            credentials: CredentialHostInput {
                server_name: Some(name),
                server_url: credentials.base_url,
                username: credentials.username,
                password: credentials.password,
                trust_invalid_cert: credentials.trust_invalid_cert,
            },
            use_instant_mix,
            device_id,
        })
        .await?;
        let next = JellyfinSourceConfig::from_configuration(&authenticated.configuration)?;
        let source_id = if saved.same_account(&next)? {
            Some(current.source_id)
        } else {
            None
        };
        return Ok(SourceEditResult::Connected(Box::new(
            authenticated.connected(source_id),
        )));
    }

    let reopen = credentials.trust_invalid_cert != saved.trust_invalid_cert
        || use_instant_mix != saved.use_instant_mix;
    let configuration = crate::config::encode_provider_payload(
        current.source_id.clone(),
        JELLYFIN_SOURCE_ID,
        name,
        JellyfinSourceConfig {
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
        ConnectedSource::jellyfin(configuration, source, None),
    )))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JellyfinClientConfig {
    pub base_url: String,
    pub trust_invalid_cert: bool,
    pub device_id: String,
    pub device_name: String,
    pub client_name: String,
    pub client_version: String,
}
impl JellyfinClientConfig {
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
            base_url: base_url.into(),
            trust_invalid_cert,
            device_id,
            device_name: DEVICE_NAME.to_string(),
            client_name: CLIENT_NAME.to_string(),
            client_version: CLIENT_VERSION.to_string(),
        })
    }
}
#[derive(Debug)]
pub struct JellyfinSource {
    client: Client,
    base_url: Url,
    user_id: String,
    access_token: Arc<str>,
    device_id: Arc<str>,
    authorization: header::HeaderValue,
    use_instant_mix: bool,
    trust_invalid_cert: bool,
}
impl JellyfinSource {
    fn open(
        config: JellyfinSourceConfig,
        access_token: String,
        device_id: String,
    ) -> SourceResult<Self> {
        let client_config =
            JellyfinClientConfig::new(&config.base_url, config.trust_invalid_cert, device_id)?;
        let base_url = normalize_base_url(&client_config.base_url)?;
        let client = build_client(client_config.trust_invalid_cert)?;
        let authorization = authenticated_header(&client_config, &access_token)?;
        Ok(Self {
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
    async fn authenticate(input: JellyfinSetupInput) -> SourceResult<AuthenticatedJellyfin> {
        let CredentialHostInput {
            server_name: submitted_name,
            server_url,
            username,
            password,
            trust_invalid_cert,
        } = input.credentials;
        let config = JellyfinClientConfig::new(&server_url, trust_invalid_cert, input.device_id)?;
        let base_url = normalize_base_url(&config.base_url)?;
        let client = build_client(config.trust_invalid_cert)?;

        let body = AuthenticateByNameRequest { username, password };
        let auth_url = endpoint(&base_url, "Users/AuthenticateByName")?;
        let response = send_json::<AuthenticationResult>(
            client
                .post(auth_url)
                .header(header::AUTHORIZATION, auth_header(&config, None))
                .json(&body),
        )
        .await?;

        let provider_name = public_server_name(&client, &base_url, &config)
            .await
            .unwrap_or_else(|| "Jellyfin".to_string());
        let server_id = response
            .server_id
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| stable_source_id(base_url.as_str()));
        if response.user.id.trim().is_empty() {
            return Err(SourceError::Auth(
                "Jellyfin returned an empty user ID".to_string(),
            ));
        }
        let source_id = SourceId::new(format!(
            "jellyfin:server:{server_id}:user:{}",
            response.user.id
        ));
        let canonical_base_url = base_url.as_str().trim_end_matches('/').to_string();
        let user_id = response.user.id;
        let username = response.user.name;
        let credential = response.access_token;
        let authorization = authenticated_header(&config, &credential)?;
        let configuration = crate::config::encode_provider_payload(
            source_id,
            JELLYFIN_SOURCE_ID,
            crate::source::configured_source_name(submitted_name, provider_name),
            JellyfinSourceConfig {
                base_url: canonical_base_url,
                server_id: Some(server_id),
                user_id: user_id.clone(),
                username,
                trust_invalid_cert,
                use_instant_mix: input.use_instant_mix,
            }
            .into_payload(),
        );
        let source = Self {
            client,
            base_url,
            user_id,
            access_token: Arc::from(credential.clone()),
            device_id: Arc::from(config.device_id),
            authorization,
            use_instant_mix: input.use_instant_mix,
            trust_invalid_cert: config.trust_invalid_cert,
        };
        Ok(AuthenticatedJellyfin {
            configuration,
            source,
            credential,
        })
    }
}

fn authenticated_header(
    config: &JellyfinClientConfig,
    access_token: &str,
) -> SourceResult<header::HeaderValue> {
    auth_header(config, Some(access_token))
        .parse()
        .map_err(|error| SourceError::InvalidConfig(format!("invalid Jellyfin identity: {error}")))
}
