use serde::{Deserialize, Serialize};

/// The application key and secret Rufin signs in to Libre.fm and the Audioscrobbler servers the
/// user adds with.
pub const RUFIN_API_KEY: &str = "rufin";
pub const RUFIN_API_SECRET: &str = "rufin";
/// Rufin's own Last.fm API account, used unless the user enters their own key.
const LASTFM_API_KEY: &str = "f87d6ee922e02eb3cb2168ea0ad3319e";
const LASTFM_API_SECRET: &str = "4bb5df4f3975ff9f27203cbd80e8a231";

/// The Last.fm API account Rufin signs, connects and finds covers with: the user's own key and
/// secret when they entered a key, Rufin's otherwise.
pub fn lastfm_application<'a>(api_key: &'a str, api_secret: &'a str) -> (&'a str, &'a str) {
    match api_key.trim() {
        "" => (LASTFM_API_KEY, LASTFM_API_SECRET),
        api_key => (api_key, api_secret.trim()),
    }
}

const SECRET_NAMESPACE: &str = "scrobbling";

#[derive(Clone, Debug, Eq, PartialEq)]
enum SecretField {
    LastFmApiSecret,
    LastFmSession,
    LibreFmSession,
    ListenBrainzToken,
    /// A server's token or session, by the server's id.
    Server(String),
}

/// A secret of an account, kept in secret storage rather than the settings file. The
/// settings name the accounts, so the secrets are as many as the servers they hold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecretDescriptor {
    field: SecretField,
}

impl SecretDescriptor {
    pub const fn namespace(&self) -> &'static str {
        SECRET_NAMESPACE
    }

    pub fn kind(&self) -> String {
        match &self.field {
            SecretField::LastFmApiSecret => "lastfm-api-secret".into(),
            SecretField::LastFmSession => "lastfm-session".into(),
            SecretField::LibreFmSession => "librefm-session".into(),
            SecretField::ListenBrainzToken => "listenbrainz-token".into(),
            SecretField::Server(id) => format!("server-{id}"),
        }
    }

    pub const fn label(&self) -> &'static str {
        match self.field {
            SecretField::LastFmApiSecret => "Rufin Last.fm API secret",
            SecretField::LastFmSession => "Rufin Last.fm session",
            SecretField::LibreFmSession => "Rufin Libre.fm session",
            SecretField::ListenBrainzToken => "Rufin ListenBrainz token",
            SecretField::Server(_) => "Rufin scrobbling server",
        }
    }

    /// The account the secret belongs to, as `Settings::accounts` names it.
    pub fn account(&self) -> String {
        match &self.field {
            SecretField::LastFmApiSecret | SecretField::LastFmSession => LASTFM.into(),
            SecretField::LibreFmSession => LIBREFM.into(),
            SecretField::ListenBrainzToken => LISTENBRAINZ.into(),
            SecretField::Server(id) => server_account(id),
        }
    }

    /// The secret in `settings`; empty when its server is not there.
    pub fn value<'a>(&self, settings: &'a Settings) -> &'a str {
        match &self.field {
            SecretField::LastFmApiSecret => &settings.lastfm.api_secret,
            SecretField::LastFmSession => &settings.lastfm.session_key,
            SecretField::LibreFmSession => &settings.librefm.session_key,
            SecretField::ListenBrainzToken => &settings.listenbrainz.user_token,
            SecretField::Server(id) => settings
                .server(id)
                .map_or("", |server| server.secret.as_str()),
        }
    }

    /// Sets the secret in `settings`, whose server must be there for it.
    pub fn set(&self, settings: &mut Settings, value: String) {
        let slot = match &self.field {
            SecretField::LastFmApiSecret => &mut settings.lastfm.api_secret,
            SecretField::LastFmSession => &mut settings.lastfm.session_key,
            SecretField::LibreFmSession => &mut settings.librefm.session_key,
            SecretField::ListenBrainzToken => &mut settings.listenbrainz.user_token,
            SecretField::Server(id) => match settings.servers.iter_mut().find(|s| s.id == *id) {
                Some(server) => &mut server.secret,
                None => return,
            },
        };
        *slot = value;
    }
}

pub const LASTFM: &str = "lastfm";
pub const LIBREFM: &str = "librefm";
pub const LISTENBRAINZ: &str = "listenbrainz";
const SERVER_PREFIX: &str = "server:";

/// The account name of a server, which is also the service its scrobbles wait for.
pub fn server_account(id: &str) -> String {
    format!("{SERVER_PREFIX}{id}")
}

/// The secrets of the accounts `settings` names.
pub fn secret_descriptors(settings: &Settings) -> Vec<SecretDescriptor> {
    [
        SecretField::LastFmApiSecret,
        SecretField::LastFmSession,
        SecretField::LibreFmSession,
        SecretField::ListenBrainzToken,
    ]
    .into_iter()
    .chain(
        settings
            .servers
            .iter()
            .map(|server| SecretField::Server(server.id.clone())),
    )
    .map(|field| SecretDescriptor { field })
    .collect()
}

fn default_now_playing_enabled() -> bool {
    true
}

fn default_librefm_settings() -> AudioscrobblerSettings {
    AudioscrobblerSettings {
        api_key: RUFIN_API_KEY.to_string(),
        api_secret: RUFIN_API_SECRET.to_string(),
        ..AudioscrobblerSettings::default()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AudioscrobblerSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_secret: String,
    #[serde(default)]
    pub session_key: String,
    #[serde(default = "default_now_playing_enabled")]
    pub now_playing_enabled: bool,
}

impl Default for AudioscrobblerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            username: String::new(),
            api_key: String::new(),
            api_secret: String::new(),
            session_key: String::new(),
            now_playing_enabled: true,
        }
    }
}

impl AudioscrobblerSettings {
    pub fn sanitize(&mut self) {
        self.username = self.username.trim().to_string();
        self.api_key = self.api_key.trim().to_string();
        self.api_secret = self.api_secret.trim().to_string();
        self.session_key = self.session_key.trim().to_string();
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListenBrainzSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub user_token: String,
    #[serde(default = "default_now_playing_enabled")]
    pub now_playing_enabled: bool,
}

impl Default for ListenBrainzSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            username: String::new(),
            user_token: String::new(),
            now_playing_enabled: true,
        }
    }
}

impl ListenBrainzSettings {
    pub fn sanitize(&mut self) {
        self.username = self.username.trim().to_string();
        self.user_token = self.user_token.trim().to_string();
    }
}

/// How a scrobbling server takes listens.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ScrobblingProtocol {
    /// ListenBrainz's API, with a user token: Koito, Maloja and others.
    #[serde(rename = "listenbrainz")]
    ListenBrainz,
    /// The Last.fm 2.0 API, signed in with the server's username and password: GNU FM, Maloja
    /// and others.
    Audioscrobbler,
}

/// A scrobbling server the user added.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServerSettings {
    pub id: String,
    pub name: String,
    pub protocol: ScrobblingProtocol,
    /// The API root: where `1/submit-listens` is for ListenBrainz, the `2.0/` endpoint for
    /// Audioscrobbler.
    pub url: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_now_playing_enabled")]
    pub now_playing_enabled: bool,
    #[serde(default)]
    pub username: String,
    /// The user token, or the Audioscrobbler session key the password was exchanged for.
    #[serde(default)]
    pub secret: String,
}

impl ServerSettings {
    fn sanitize(&mut self) {
        self.name = self.name.trim().to_string();
        self.url = self.url.trim().to_string();
        self.username = self.username.trim().to_string();
        self.secret = self.secret.trim().to_string();
    }
}

/// The scrobbling accounts Rufin signs in to. Their secrets share one storage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Settings {
    #[serde(default)]
    pub lastfm: AudioscrobblerSettings,
    #[serde(default = "default_librefm_settings")]
    pub librefm: AudioscrobblerSettings,
    #[serde(default)]
    pub listenbrainz: ListenBrainzSettings,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<ServerSettings>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lastfm: AudioscrobblerSettings::default(),
            librefm: default_librefm_settings(),
            listenbrainz: ListenBrainzSettings::default(),
            servers: Vec::new(),
        }
    }
}

impl Settings {
    pub fn server(&self, id: &str) -> Option<&ServerSettings> {
        self.servers.iter().find(|server| server.id == id)
    }

    /// Whether `other` names the same accounts with the same secrets; the switches may differ.
    pub fn same_accounts(&self, other: &Settings) -> bool {
        let names = |settings: &Settings| {
            (
                [
                    settings.lastfm.username.clone(),
                    settings.lastfm.api_key.clone(),
                    settings.librefm.username.clone(),
                    settings.listenbrainz.username.clone(),
                ],
                settings
                    .servers
                    .iter()
                    .map(|server| {
                        (
                            server.id.clone(),
                            server.name.clone(),
                            server.protocol,
                            server.url.clone(),
                            server.username.clone(),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        };
        names(self) == names(other)
            && secret_descriptors(self)
                .iter()
                .all(|descriptor| descriptor.value(self) == descriptor.value(other))
    }

    pub fn sanitize(&mut self) {
        self.lastfm.sanitize();
        self.librefm.sanitize();
        if self.librefm.api_key.is_empty() {
            self.librefm.api_key = RUFIN_API_KEY.to_string();
        }
        if self.librefm.api_secret.is_empty() {
            self.librefm.api_secret = RUFIN_API_SECRET.to_string();
        }
        self.listenbrainz.sanitize();
        self.servers.iter_mut().for_each(ServerSettings::sanitize);
    }

    /// The accounts Connect shares, each as one record.
    pub(crate) fn accounts(&self) -> Vec<String> {
        [LASTFM, LIBREFM, LISTENBRAINZ]
            .into_iter()
            .map(String::from)
            .chain(self.servers.iter().map(|server| server_account(&server.id)))
            .collect()
    }

    /// An account's settings, secrets included.
    pub(crate) fn account(&self, name: &str) -> Result<serde_json::Value, String> {
        match name {
            LASTFM => serde_json::to_value(&self.lastfm),
            LIBREFM => serde_json::to_value(&self.librefm),
            LISTENBRAINZ => serde_json::to_value(&self.listenbrainz),
            _ => match name
                .strip_prefix(SERVER_PREFIX)
                .and_then(|id| self.server(id))
            {
                Some(server) => serde_json::to_value(server),
                None => return Err(format!("no scrobbling account {name}")),
            },
        }
        .map_err(|error| error.to_string())
    }

    /// Replaces an account's settings, or adds the server it names.
    pub(crate) fn set_account(
        &mut self,
        name: &str,
        value: serde_json::Value,
    ) -> Result<(), String> {
        let error = |error: serde_json::Error| error.to_string();
        match name {
            LASTFM => self.lastfm = serde_json::from_value(value).map_err(error)?,
            LIBREFM => self.librefm = serde_json::from_value(value).map_err(error)?,
            LISTENBRAINZ => self.listenbrainz = serde_json::from_value(value).map_err(error)?,
            _ => {
                let Some(id) = name.strip_prefix(SERVER_PREFIX) else {
                    return Err(format!("no scrobbling account {name}"));
                };
                let mut server: ServerSettings = serde_json::from_value(value).map_err(error)?;
                server.id = id.to_owned();
                match self.servers.iter_mut().find(|current| current.id == id) {
                    Some(current) => *current = server,
                    None => self.servers.push(server),
                }
            }
        }
        Ok(())
    }

    /// Removes the server an account name names.
    pub(crate) fn remove_account(&mut self, name: &str) {
        if let Some(id) = name.strip_prefix(SERVER_PREFIX) {
            self.servers.retain(|server| server.id != id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_preserve_existing_serialized_shape() {
        let settings: Settings = serde_json::from_str("{}").expect("default settings");
        assert_eq!(settings, Settings::default());
        assert!(settings.lastfm.now_playing_enabled);
        assert!(settings.librefm.now_playing_enabled);
        assert_eq!(settings.librefm.api_key, RUFIN_API_KEY);
    }

    #[test]
    fn sanitize_restores_librefm_application_credentials() {
        let mut settings = Settings::default();
        settings.librefm.api_key = "  ".to_string();
        settings.librefm.api_secret = String::new();
        settings.sanitize();
        assert_eq!(settings.librefm.api_key, RUFIN_API_KEY);
        assert_eq!(settings.librefm.api_secret, RUFIN_API_SECRET);
    }

    #[test]
    fn secret_descriptors_preserve_storage_contract() {
        let storage_contract = secret_descriptors(&Settings::default())
            .iter()
            .map(|descriptor| {
                (
                    descriptor.namespace(),
                    descriptor.kind(),
                    descriptor.label(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            storage_contract,
            vec![
                (
                    "scrobbling",
                    "lastfm-api-secret".into(),
                    "Rufin Last.fm API secret",
                ),
                (
                    "scrobbling",
                    "lastfm-session".into(),
                    "Rufin Last.fm session",
                ),
                (
                    "scrobbling",
                    "librefm-session".into(),
                    "Rufin Libre.fm session",
                ),
                (
                    "scrobbling",
                    "listenbrainz-token".into(),
                    "Rufin ListenBrainz token",
                ),
            ]
        );
    }
}
