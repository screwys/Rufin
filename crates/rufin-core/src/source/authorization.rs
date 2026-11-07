use crate::runtime::source::{EmbyConnectLoginMethod, PlexLoginMethod, SourceHandle};
use async_channel::Receiver;
use sources::{SourceSettingsChange, SourceSetup};
use std::sync::{Arc, Mutex};

#[derive(Debug, thiserror::Error)]
#[cfg_attr(feature = "bindings", derive(uniffi::Error))]
pub enum SourceAuthorizationError {
    #[error("{reason}")]
    Failed { reason: String },
}

fn error(value: impl std::fmt::Display) -> SourceAuthorizationError {
    SourceAuthorizationError::Failed {
        reason: value.to_string(),
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SourceChoice {
    pub index: u32,
    pub title: String,
    pub address: Option<String>,
    pub pin_required: bool,
}

fn choices<'a>(values: impl Iterator<Item = (&'a str, bool)>) -> Vec<SourceChoice> {
    values
        .enumerate()
        .map(|(index, (title, pin_required))| SourceChoice {
            index: index as u32,
            title: title.into(),
            address: None,
            pin_required,
        })
        .collect()
}

#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SourceAuthorizationEvent {
    Code { code: String, url: String },
    OpenBrowser { url: String },
    Ready { kind: String },
}

#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SourceAuthorizationInput {
    Jellyfin {
        use_instant_mix: bool,
    },
    Emby {
        server_index: u32,
        use_instant_mix: bool,
    },
    Plex {
        server_index: u32,
        address_override: Option<String>,
        trust_invalid_cert: bool,
    },
    Files {
        settings: sources::FileSourceSettings,
        credentials: sources::FileCredentialsEdit,
    },
}

#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SourceAuthorizationMethod {
    Jellyfin {
        url: String,
        trust_invalid_cert: bool,
    },
    Emby {
        method: EmbyConnectLoginMethod,
    },
    Plex {
        method: PlexLoginMethod,
    },
    Nextcloud {
        settings: sources::FileSourceSettings,
        credentials: sources::FileCredentials,
    },
}

pub(super) enum LoginEvent {
    Code { code: String, url: String },
    OpenBrowser(String),
    Authorized(Box<Authorization>),
}

#[derive(Clone, Default)]
pub(super) enum Authorization {
    #[default]
    Pending,
    Jellyfin(sources::JellyfinQuickConnectLogin),
    Emby(Vec<sources::EmbyConnectServer>),
    Plex {
        login: sources::PlexLogin,
        profiles: Vec<sources::PlexProfile>,
        profile: Option<String>,
        servers: Vec<sources::PlexServer>,
    },
    SavedPlex(Vec<(String, sources::PlexLogin)>),
    Files {
        settings: sources::FileSourceSettings,
        credentials: sources::FileCredentials,
    },
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct SourceAuthorization {
    source: SourceHandle,
    events: Receiver<Result<SourceAuthorizationEvent, String>>,
    state: Arc<Mutex<Authorization>>,
}

impl Drop for SourceAuthorization {
    fn drop(&mut self) {
        self.events.close();
    }
}

impl SourceAuthorization {
    pub fn new(source: SourceHandle, method: SourceAuthorizationMethod) -> Arc<Self> {
        let state = Arc::new(Mutex::new(Authorization::Pending));
        let saved = state.clone();
        let events = source.login_events(method, move |event| match event {
            LoginEvent::Code { code, url } => SourceAuthorizationEvent::Code { code, url },
            LoginEvent::OpenBrowser(url) => SourceAuthorizationEvent::OpenBrowser { url },
            LoginEvent::Authorized(authorization) => {
                let authorization = *authorization;
                let kind = match &authorization {
                    Authorization::Jellyfin(_) => "jellyfin",
                    Authorization::Emby(_) => "emby",
                    Authorization::Plex { .. } => "plex",
                    Authorization::Files { .. } => "webdav",
                    _ => unreachable!(),
                };
                *saved.lock().expect("Source authorization") = authorization;
                SourceAuthorizationEvent::Ready { kind: kind.into() }
            }
        });
        Arc::new(Self {
            source,
            events,
            state,
        })
    }

    pub async fn saved_plex(source: SourceHandle) -> Result<Arc<Self>, SourceAuthorizationError> {
        let logins = source
            .plex_saved_logins()
            .recv()
            .await
            .map_err(error)?
            .map_err(error)?;
        let (send, events) = async_channel::bounded(1);
        drop(send);
        Ok(Arc::new(Self {
            source,
            events,
            state: Arc::new(Mutex::new(Authorization::SavedPlex(logins))),
        }))
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceAuthorization {
    pub async fn next(&self) -> Result<SourceAuthorizationEvent, SourceAuthorizationError> {
        self.events.recv().await.map_err(error)?.map_err(error)
    }
    pub fn cancel(&self) {
        self.events.close();
        *self.state.lock().expect("Source authorization") = Authorization::Pending;
    }
    pub fn choices(&self) -> Vec<SourceChoice> {
        let state = self.state.lock().expect("Source authorization");
        match &*state {
            Authorization::Emby(servers) => {
                choices(servers.iter().map(|server| (server.name.as_str(), false)))
            }
            Authorization::Plex { servers, .. } => {
                choices(servers.iter().map(|server| (server.name.as_str(), false)))
            }
            Authorization::SavedPlex(logins) => {
                choices(logins.iter().map(|(name, _)| (name.as_str(), false)))
            }
            _ => Vec::new(),
        }
    }

    pub fn file_settings(&self) -> Option<sources::FileSourceSettings> {
        match &*self.state.lock().expect("Source authorization") {
            Authorization::Files { settings, .. } => Some(settings.clone()),
            _ => None,
        }
    }
    pub async fn plex_profiles(
        &self,
        saved_index: Option<u32>,
    ) -> Result<Vec<SourceChoice>, SourceAuthorizationError> {
        let login = match &*self.state.lock().expect("Plex authorization") {
            Authorization::Plex { login, .. } => login.clone(),
            Authorization::SavedPlex(logins) => logins
                .get(saved_index.ok_or_else(|| error("Choose an account"))? as usize)
                .ok_or_else(|| error("Account is no longer available"))?
                .1
                .clone(),
            _ => return Err(error("Sign in to Plex first")),
        };
        let (login, profiles) = self
            .source
            .plex_profiles(login)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)?;
        let choices = choices(
            profiles
                .iter()
                .map(|profile| (profile.name.as_str(), profile.pin_required)),
        );
        *self.state.lock().expect("Plex authorization") = Authorization::Plex {
            login,
            profiles,
            profile: None,
            servers: Vec::new(),
        };
        Ok(choices)
    }
    pub async fn plex_servers(
        &self,
        profile_index: u32,
        pin: Option<String>,
        lan: Vec<sources::DiscoveredServer>,
    ) -> Result<Vec<SourceChoice>, SourceAuthorizationError> {
        let (login, profile) = match &*self.state.lock().expect("Plex authorization") {
            Authorization::Plex {
                login, profiles, ..
            } => (
                login.clone(),
                profiles
                    .get(profile_index as usize)
                    .cloned()
                    .ok_or_else(|| error("Profile is no longer available"))?,
            ),
            _ => return Err(error("Choose a Plex profile")),
        };
        let id = profile.id.clone();
        let (login, servers) = self
            .source
            .plex_servers(login, profile, pin, lan)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)?;
        let mut state = self.state.lock().expect("Plex authorization");
        let profiles = match &*state {
            Authorization::Plex { profiles, .. } => profiles.clone(),
            _ => Vec::new(),
        };
        *state = Authorization::Plex {
            login,
            profiles,
            profile: Some(id),
            servers,
        };
        drop(state);
        Ok(self.choices())
    }
    pub async fn finish(
        &self,
        name: String,
        source_id: Option<sources::SourceId>,
        integration: bool,
        input: SourceAuthorizationInput,
    ) -> Result<sources::SourceId, SourceAuthorizationError> {
        let authorized = self.state.lock().expect("Source authorization").clone();
        let (server_index, use_instant_mix, address_override, trust_invalid_cert) = match input {
            SourceAuthorizationInput::Jellyfin { use_instant_mix } => {
                (0, use_instant_mix, None, false)
            }
            SourceAuthorizationInput::Emby {
                server_index,
                use_instant_mix,
            } => (server_index, use_instant_mix, None, false),
            SourceAuthorizationInput::Plex {
                server_index,
                address_override,
                trust_invalid_cert,
            } => (server_index, false, address_override, trust_invalid_cert),
            SourceAuthorizationInput::Files {
                settings,
                credentials: edits,
            } => {
                let Authorization::Files { credentials, .. } = authorized else {
                    return Err(error("Finish authorization first"));
                };
                let retained_headers = source_id.is_none().then_some(credentials.headers);
                return self
                    .source
                    .save_files(
                        source_id,
                        false,
                        integration,
                        name.trim().into(),
                        settings,
                        sources::FileCredentialsEdit {
                            secret: Some(edits.secret.unwrap_or(credentials.secret)),
                            headers: edits.headers.or(retained_headers),
                        },
                    )
                    .await
                    .map_err(error);
            }
        };
        let name = (!name.trim().is_empty()).then(|| name.trim().to_string());
        let id = source_id;
        if let Some(source_id) = id {
            let updated = source_id.clone();
            let input = match authorized {
                Authorization::Jellyfin(login) => SourceSettingsChange::JellyfinQuickConnect {
                    source_id,
                    login,
                    source_name: name,
                    use_instant_mix,
                },
                Authorization::Emby(servers) => SourceSettingsChange::EmbyConnect {
                    source_id,
                    server: servers
                        .get(server_index as usize)
                        .cloned()
                        .ok_or_else(|| error("Choose a server"))?,
                    source_name: name,
                    use_instant_mix,
                },
                Authorization::Plex { .. } => SourceSettingsChange::Plex {
                    source_id,
                    settings: sources::PlexSettingsInput {
                        name: name.unwrap_or_default(),
                        address_override,
                        trust_invalid_cert,
                    },
                },
                _ => return Err(error("Finish authorization first")),
            };
            let receiver = if integration {
                self.source.update_file_integration(input)
            } else {
                self.source.update_source(input)
            };
            receiver
                .recv()
                .await
                .map_err(error)?
                .map(|_| updated)
                .map_err(error)
        } else {
            let input = match authorized {
                Authorization::Jellyfin(login) => SourceSetup::JellyfinQuickConnect {
                    login,
                    source_name: name,
                    use_instant_mix,
                },
                Authorization::Emby(servers) => SourceSetup::EmbyConnect {
                    server: servers
                        .get(server_index as usize)
                        .cloned()
                        .ok_or_else(|| error("Choose a server"))?,
                    source_name: name,
                    use_instant_mix,
                },
                Authorization::Plex {
                    login,
                    profile,
                    servers,
                    ..
                } => SourceSetup::Plex(sources::PlexSetupInput {
                    name: name.unwrap_or_default(),
                    login,
                    profile_id: profile.ok_or_else(|| error("Choose a Plex profile"))?,
                    server: servers
                        .get(server_index as usize)
                        .cloned()
                        .ok_or_else(|| error("Choose a server"))?,
                    address_override,
                    trust_invalid_cert,
                }),
                _ => return Err(error("Finish authorization first")),
            };
            if integration {
                self.source
                    .configure_file_integration(input)
                    .recv()
                    .await
                    .map_err(error)?
                    .map_err(error)
            } else {
                self.source
                    .configure_source(input)
                    .recv()
                    .await
                    .map_err(error)?
                    .map(|selected| selected.source_id)
                    .map_err(error)
            }
        }
    }
}
