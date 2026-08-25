use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{SourceError, SourceInputIdentity, SourceResult, subsonic::SubsonicFlavor};

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SourceId(String);

impl SourceId {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        assert!(!value.is_empty(), "SourceId cannot be empty");
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Credential-free source configuration persisted by Rufin Settings.
///
/// The provider payload is opaque outside Sources. Cache health never affects
/// this value or its separately stored credential reference.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceConfiguration {
    pub source_id: SourceId,
    pub kind: String,
    pub name: String,
    pub provider_payload: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialHostInput {
    pub server_name: Option<String>,
    pub server_url: String,
    pub username: String,
    pub password: String,
    pub trust_invalid_cert: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialHostPreset {
    pub server_name: String,
    pub server_url: String,
    pub username: String,
    pub trust_invalid_cert: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JellyfinSetupInput {
    pub credentials: CredentialHostInput,
    pub use_instant_mix: bool,
    pub device_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalFolderHostInput {
    pub roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialSettingsInput {
    pub name: String,
    pub base_url: String,
    pub username: String,
    pub password: String,
    pub trust_invalid_cert: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JellyfinSettingsInput {
    pub credentials: CredentialSettingsInput,
    pub use_instant_mix: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceSetupInput {
    Jellyfin(JellyfinSetupInput),
    Subsonic {
        flavor: SubsonicFlavor,
        authentication: crate::subsonic::SubsonicAuthentication,
        credentials: CredentialHostInput,
    },
    Local(LocalFolderHostInput),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceSettingsInput {
    Jellyfin(JellyfinSettingsInput),
    Subsonic {
        authentication: crate::subsonic::SubsonicAuthentication,
        credentials: CredentialSettingsInput,
    },
    Local {
        roots: Vec<PathBuf>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditableSource {
    Credentials {
        source_id: SourceId,
        kind: String,
        credentials: CredentialHostPreset,
        jellyfin_use_instant_mix: Option<bool>,
        subsonic_authentication: Option<crate::subsonic::SubsonicAuthentication>,
    },
    Local {
        source_id: SourceId,
        roots: Vec<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceCacheMatch {
    Exact,
    ReaderUpgrade,
    Incompatible,
}

impl SourceConfiguration {
    pub fn is_local(&self) -> bool {
        self.kind == crate::local::LOCAL_SOURCE_ID
    }

    pub fn transcoded_download_bitrate_limit_kbps(&self) -> Option<u32> {
        (self.kind == crate::jellyfin::JELLYFIN_SOURCE_ID)
            .then_some(crate::jellyfin::JELLYFIN_TRANSCODED_DOWNLOAD_BITRATE_LIMIT_KBPS)
    }

    pub fn playlist_tracks_can_repeat(&self) -> bool {
        self.kind != crate::jellyfin::JELLYFIN_SOURCE_ID
    }

    /// Encode an already configured Local source without touching its folders.
    ///
    /// Released-settings migration uses this while a removable or network
    /// filesystem may be offline. Ordinary Local opening validates access
    /// later without changing the configured identity.
    pub fn local(
        source_id: SourceId,
        name: impl Into<String>,
        roots: Vec<PathBuf>,
    ) -> SourceResult<Self> {
        let roots = crate::local::configured_roots(roots)?;
        Ok(encode_provider_payload(
            source_id,
            crate::local::LOCAL_SOURCE_ID,
            name,
            crate::local::LocalSourceConfig { roots }.into_payload(),
        ))
    }

    /// Identify the source inputs that determine rebuildable canonical facts.
    ///
    /// Credentials and presentation settings do not participate, so Rufin can
    /// validate an existing cache even when live source access is unavailable.
    pub fn input_identity(&self) -> SourceResult<SourceInputIdentity> {
        self.input_identity_with_reader_versions(None, true)
    }

    /// Classify an accepted cache against this source's current fact reader.
    ///
    /// Known reader upgrades may show their already-observed facts while an
    /// authoritative refresh rebuilds the affected source representation.
    pub fn cache_match(&self, cached: &SourceInputIdentity) -> SourceResult<SourceCacheMatch> {
        if cached == &self.input_identity()? {
            return Ok(SourceCacheMatch::Exact);
        }
        if self.kind == crate::jellyfin::JELLYFIN_SOURCE_ID
            && cached == &self.input_identity_with_reader_versions(None, false)?
        {
            return Ok(SourceCacheMatch::ReaderUpgrade);
        }
        if self.kind == "navidrome" {
            let config = crate::subsonic::SubsonicSourceConfig::from_configuration(self)?;
            if config.navidrome_library_version > 0
                && cached == &self.input_identity_with_reader_versions(Some(0), true)?
            {
                return Ok(SourceCacheMatch::ReaderUpgrade);
            }
        }
        Ok(SourceCacheMatch::Incompatible)
    }

    fn input_identity_with_reader_versions(
        &self,
        navidrome_library_version: Option<u32>,
        jellyfin_artist_reader_v2: bool,
    ) -> SourceResult<SourceInputIdentity> {
        let mut digest = blake3::Hasher::new();
        digest.update(b"rufin-source-input");
        // Preserve the released digest encoding. Reader freshness belongs to
        // provider markers and Local parser revisions, not source identity.
        digest.update(&1_u32.to_le_bytes());
        digest_part(&mut digest, self.source_id.as_str().as_bytes());
        digest_part(&mut digest, self.kind.as_bytes());
        match self.kind.as_str() {
            crate::local::LOCAL_SOURCE_ID => {
                for root in crate::local::LocalSourceConfig::from_configuration(self)?.roots {
                    digest_part(&mut digest, root.to_string_lossy().as_bytes());
                }
            }
            crate::jellyfin::JELLYFIN_SOURCE_ID => {
                let config = crate::jellyfin::JellyfinSourceConfig::from_configuration(self)?;
                digest_part(&mut digest, config.user_id.as_bytes());
                if jellyfin_artist_reader_v2 {
                    // Jellyfin MusicArtist changed from raw dual-access rows to one
                    // name-aggregate representation. Released caches remain usable
                    // while a live reader upgrade rebuilds their source facts.
                    digest_part(&mut digest, b"jellyfin-artist-reader-v2");
                }
            }
            "navidrome" | "subsonic" => {
                let config = crate::subsonic::SubsonicSourceConfig::from_configuration(self)?;
                digest_part(&mut digest, config.username.as_bytes());
                let library_version =
                    navidrome_library_version.unwrap_or(config.navidrome_library_version);
                if library_version > 0 {
                    digest_part(&mut digest, b"navidrome-library");
                    digest.update(&library_version.to_le_bytes());
                }
            }
            kind => {
                return Err(SourceError::InvalidConfig(format!(
                    "unknown source kind {kind}"
                )));
            }
        }
        Ok(SourceInputIdentity {
            source_id: self.source_id.clone(),
            digest: *digest.finalize().as_bytes(),
        })
    }

    /// Return the fields Rufin may present when editing this source.
    ///
    /// The provider payload remains opaque to Rufin and UI. Sources decodes it
    /// here and accepts the corresponding edit through `Source::edit`.
    pub fn editable(&self) -> SourceResult<EditableSource> {
        match self.kind.as_str() {
            crate::jellyfin::JELLYFIN_SOURCE_ID => {
                let config = crate::jellyfin::JellyfinSourceConfig::from_configuration(self)?;
                Ok(EditableSource::Credentials {
                    source_id: self.source_id.clone(),
                    kind: self.kind.clone(),
                    credentials: CredentialHostPreset {
                        server_name: self.name.clone(),
                        server_url: config.base_url,
                        username: config.username,
                        trust_invalid_cert: config.trust_invalid_cert,
                    },
                    jellyfin_use_instant_mix: Some(config.use_instant_mix),
                    subsonic_authentication: None,
                })
            }
            "navidrome" | "subsonic" => {
                let config = crate::subsonic::SubsonicSourceConfig::from_configuration(self)?;
                Ok(EditableSource::Credentials {
                    source_id: self.source_id.clone(),
                    kind: self.kind.clone(),
                    credentials: CredentialHostPreset {
                        server_name: self.name.clone(),
                        server_url: config.base_url,
                        username: config.username,
                        trust_invalid_cert: config.trust_invalid_cert,
                    },
                    jellyfin_use_instant_mix: None,
                    subsonic_authentication: Some(config.authentication),
                })
            }
            crate::local::LOCAL_SOURCE_ID => {
                let config = crate::local::LocalSourceConfig::from_configuration(self)?;
                Ok(EditableSource::Local {
                    source_id: self.source_id.clone(),
                    roots: config.roots,
                })
            }
            kind => Err(SourceError::InvalidConfig(format!(
                "unknown source kind {kind}"
            ))),
        }
    }
}

fn digest_part(digest: &mut blake3::Hasher, value: &[u8]) {
    digest.update(&(value.len() as u64).to_le_bytes());
    digest.update(value);
}

pub(crate) fn decode_provider_payload<T: DeserializeOwned>(
    stored: &SourceConfiguration,
) -> SourceResult<T> {
    serde_json::from_str(&stored.provider_payload)
        .map_err(|error| SourceError::InvalidConfig(error.to_string()))
}

pub(crate) fn require_payload_version(actual: u32, expected: u32) -> SourceResult<()> {
    if actual != expected {
        return Err(SourceError::InvalidConfig(format!(
            "unsupported payload version {actual}"
        )));
    }
    Ok(())
}

pub(crate) fn encode_provider_payload(
    source_id: SourceId,
    kind: impl Into<String>,
    name: impl Into<String>,
    provider_payload: serde_json::Value,
) -> SourceConfiguration {
    SourceConfiguration {
        source_id,
        kind: kind.into(),
        name: name.into(),
        provider_payload: provider_payload.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::SourceId;

    use super::*;
    use crate::jellyfin::JellyfinSourceConfig;
    use crate::local::LocalSourceConfig;
    use crate::subsonic::SubsonicSourceConfig;

    fn migrated_source(kind: &str, payload: serde_json::Value) -> SourceConfiguration {
        SourceConfiguration {
            source_id: SourceId::new(format!("{kind}:server:test")),
            kind: kind.to_string(),
            name: "Test Source".to_string(),
            provider_payload: payload.to_string(),
        }
    }

    #[test]
    fn provider_payloads_decode_the_credential_free_saved_shape() {
        let jellyfin_stored = migrated_source(
            "jellyfin",
            serde_json::json!({
                "version": 1,
                "base_url": "https://jellyfin.example",
                "user_id": "account-id",
                "username": "listener",
                "trust_invalid_cert": true,
                "use_jellyfin_instant_mix": true,
            }),
        );
        let jellyfin =
            JellyfinSourceConfig::from_configuration(&jellyfin_stored).expect("Jellyfin payload");
        assert_eq!(jellyfin.base_url, "https://jellyfin.example");
        assert_eq!(jellyfin.user_id, "account-id");
        assert_eq!(jellyfin.username, "listener");
        assert!(jellyfin.trust_invalid_cert);
        assert!(jellyfin.use_instant_mix);
        assert_eq!(
            decode_provider_payload::<serde_json::Value>(&encode_provider_payload(
                jellyfin_stored.source_id,
                "jellyfin",
                "Test Source",
                jellyfin.clone().into_payload(),
            ))
            .expect("round-trip Jellyfin payload"),
            jellyfin.into_payload(),
        );

        let subsonic_stored = migrated_source(
            "subsonic",
            serde_json::json!({
                "version": 1,
                "base_url": "https://subsonic.example",
                "user_id": "legacy-listener",
                "trust_invalid_cert": true,
            }),
        );
        let subsonic = SubsonicSourceConfig::from_configuration(&subsonic_stored)
            .expect("OpenSubsonic payload");
        assert_eq!(subsonic.base_url, "https://subsonic.example");
        assert_eq!(subsonic.username, "legacy-listener");
        assert!(subsonic.trust_invalid_cert);
        assert_eq!(
            subsonic.authentication,
            crate::subsonic::SubsonicAuthentication::Password
        );
        let subsonic_payload = subsonic.clone().into_payload();
        assert!(subsonic_payload.get("user_id").is_none());
        assert_eq!(
            decode_provider_payload::<serde_json::Value>(&encode_provider_payload(
                subsonic_stored.source_id,
                "subsonic",
                "Test Source",
                subsonic_payload.clone(),
            ))
            .expect("round-trip OpenSubsonic payload"),
            subsonic_payload,
        );

        let local_stored = migrated_source(
            "local",
            serde_json::json!({
                "version": 1,
                "roots": ["/music", "/archive"],
            }),
        );
        let local = LocalSourceConfig::from_configuration(&local_stored).expect("Local payload");
        assert_eq!(
            local.roots,
            vec![PathBuf::from("/music"), PathBuf::from("/archive")]
        );
        assert_eq!(
            decode_provider_payload::<serde_json::Value>(&encode_provider_payload(
                local_stored.source_id,
                "local",
                "Local",
                local.clone().into_payload(),
            ))
            .expect("round-trip Local payload"),
            local.into_payload(),
        );
    }

    #[test]
    fn legacy_single_local_root_decodes_without_creating_another_identity() {
        let stored = migrated_source(
            "local",
            serde_json::json!({
                "version": 1,
                "base_url": "/music",
            }),
        );
        assert_eq!(
            LocalSourceConfig::from_configuration(&stored)
                .expect("legacy Local payload")
                .roots,
            vec![PathBuf::from("/music")]
        );
    }

    #[test]
    fn unsupported_provider_payload_version_is_rejected() {
        let stored = migrated_source(
            "jellyfin",
            serde_json::json!({
                "version": 2,
                "base_url": "https://music.example",
                "user_id": "account-id",
                "username": "listener",
                "trust_invalid_cert": false,
                "use_jellyfin_instant_mix": false,
            }),
        );

        let error = JellyfinSourceConfig::from_configuration(&stored)
            .expect_err("unsupported payload version");
        assert!(matches!(error, SourceError::InvalidConfig(_)));
    }

    #[test]
    fn released_jellyfin_artist_cache_is_a_reader_upgrade_not_incompatible() {
        let stored = migrated_source(
            "jellyfin",
            JellyfinSourceConfig {
                base_url: "https://music.example".to_string(),
                server_id: Some("server-one".to_string()),
                user_id: "account-id".to_string(),
                username: "listener".to_string(),
                trust_invalid_cert: false,
                use_instant_mix: false,
            }
            .into_payload(),
        );
        let released = stored
            .input_identity_with_reader_versions(None, false)
            .expect("released Jellyfin input identity");

        assert_ne!(
            stored.input_identity().expect("current Jellyfin identity"),
            released
        );
        assert_eq!(
            stored
                .cache_match(&released)
                .expect("classify released cache"),
            SourceCacheMatch::ReaderUpgrade
        );
    }

    #[test]
    fn jellyfin_playlist_adds_do_not_offer_repeated_tracks() {
        assert!(!migrated_source("jellyfin", serde_json::Value::Null).playlist_tracks_can_repeat());
        assert!(migrated_source("subsonic", serde_json::Value::Null).playlist_tracks_can_repeat());
        assert!(migrated_source("local", serde_json::Value::Null).playlist_tracks_can_repeat());
    }
}
