use serde::{Deserialize, Serialize};

pub const LIBREFM_API_KEY: &str = "rufin";
pub const LIBREFM_API_SECRET: &str = "rufin";

const SECRET_NAMESPACE: &str = "scrobbling";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SecretField {
    LastFmApiSecret,
    LastFmSession,
    LibreFmSession,
    ListenBrainzToken,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SecretDescriptor {
    field: SecretField,
    namespace: &'static str,
    kind: &'static str,
    label: &'static str,
}

impl SecretDescriptor {
    const fn new(field: SecretField, kind: &'static str, label: &'static str) -> Self {
        Self {
            field,
            namespace: SECRET_NAMESPACE,
            kind,
            label,
        }
    }

    pub const fn namespace(self) -> &'static str {
        self.namespace
    }

    pub const fn kind(self) -> &'static str {
        self.kind
    }

    pub const fn label(self) -> &'static str {
        self.label
    }

    pub fn value(self, settings: &Settings) -> &str {
        match self.field {
            SecretField::LastFmApiSecret => &settings.lastfm.api_secret,
            SecretField::LastFmSession => &settings.lastfm.session_key,
            SecretField::LibreFmSession => &settings.librefm.session_key,
            SecretField::ListenBrainzToken => &settings.listenbrainz.user_token,
        }
    }

    pub fn value_mut(self, settings: &mut Settings) -> &mut String {
        match self.field {
            SecretField::LastFmApiSecret => &mut settings.lastfm.api_secret,
            SecretField::LastFmSession => &mut settings.lastfm.session_key,
            SecretField::LibreFmSession => &mut settings.librefm.session_key,
            SecretField::ListenBrainzToken => &mut settings.listenbrainz.user_token,
        }
    }
}

pub const LASTFM_API_SECRET: SecretDescriptor = SecretDescriptor::new(
    SecretField::LastFmApiSecret,
    "lastfm-api-secret",
    "Rufin Last.fm API secret",
);
pub const LASTFM_SESSION: SecretDescriptor = SecretDescriptor::new(
    SecretField::LastFmSession,
    "lastfm-session",
    "Rufin Last.fm session",
);
pub const LIBREFM_SESSION: SecretDescriptor = SecretDescriptor::new(
    SecretField::LibreFmSession,
    "librefm-session",
    "Rufin Libre.fm session",
);
pub const LISTENBRAINZ_TOKEN: SecretDescriptor = SecretDescriptor::new(
    SecretField::ListenBrainzToken,
    "listenbrainz-token",
    "Rufin ListenBrainz token",
);

const SECRET_DESCRIPTORS: [SecretDescriptor; 4] = [
    LASTFM_API_SECRET,
    LASTFM_SESSION,
    LIBREFM_SESSION,
    LISTENBRAINZ_TOKEN,
];

pub const fn secret_descriptors() -> &'static [SecretDescriptor] {
    &SECRET_DESCRIPTORS
}

fn default_now_playing_enabled() -> bool {
    true
}

fn default_librefm_settings() -> AudioscrobblerSettings {
    AudioscrobblerSettings {
        api_key: LIBREFM_API_KEY.to_string(),
        api_secret: LIBREFM_API_SECRET.to_string(),
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

    pub(crate) fn configured(&self, now_playing: bool) -> bool {
        self.enabled
            && (!now_playing || self.now_playing_enabled)
            && !self.api_key.is_empty()
            && !self.api_secret.is_empty()
            && !self.session_key.is_empty()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListenBrainzSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub user_token: String,
    #[serde(default = "default_now_playing_enabled")]
    pub now_playing_enabled: bool,
}

impl Default for ListenBrainzSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            user_token: String::new(),
            now_playing_enabled: true,
        }
    }
}

impl ListenBrainzSettings {
    pub fn sanitize(&mut self) {
        self.user_token = self.user_token.trim().to_string();
    }

    pub(crate) fn configured(&self, now_playing: bool) -> bool {
        self.enabled && (!now_playing || self.now_playing_enabled) && !self.user_token.is_empty()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Settings {
    #[serde(default)]
    pub lastfm: AudioscrobblerSettings,
    #[serde(default = "default_librefm_settings")]
    pub librefm: AudioscrobblerSettings,
    #[serde(default)]
    pub listenbrainz: ListenBrainzSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lastfm: AudioscrobblerSettings::default(),
            librefm: default_librefm_settings(),
            listenbrainz: ListenBrainzSettings::default(),
        }
    }
}

impl Settings {
    pub fn sanitize(&mut self) {
        self.lastfm.sanitize();
        self.librefm.sanitize();
        if self.librefm.api_key.is_empty() {
            self.librefm.api_key = LIBREFM_API_KEY.to_string();
        }
        if self.librefm.api_secret.is_empty() {
            self.librefm.api_secret = LIBREFM_API_SECRET.to_string();
        }
        self.listenbrainz.sanitize();
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
        assert_eq!(settings.librefm.api_key, LIBREFM_API_KEY);
    }

    #[test]
    fn sanitize_restores_librefm_application_credentials() {
        let mut settings = Settings::default();
        settings.librefm.api_key = "  ".to_string();
        settings.librefm.api_secret = String::new();
        settings.sanitize();
        assert_eq!(settings.librefm.api_key, LIBREFM_API_KEY);
        assert_eq!(settings.librefm.api_secret, LIBREFM_API_SECRET);
    }

    #[test]
    fn secret_descriptors_preserve_storage_contract() {
        let storage_contract = secret_descriptors()
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
                    "lastfm-api-secret",
                    "Rufin Last.fm API secret",
                ),
                ("scrobbling", "lastfm-session", "Rufin Last.fm session",),
                ("scrobbling", "librefm-session", "Rufin Libre.fm session",),
                (
                    "scrobbling",
                    "listenbrainz-token",
                    "Rufin ListenBrainz token",
                ),
            ]
        );
    }
}
