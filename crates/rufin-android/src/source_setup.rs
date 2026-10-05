use std::sync::{Arc, Mutex};

use crate::host::{AndroidError, error};
use async_channel::Receiver;
use rufin_core::runtime::source::{
    DiscoveryStatus, DiscoveryUpdate, EmbyConnectLoginEvent, EmbyConnectLoginMethod,
    JellyfinQuickConnectEvent, NextcloudLoginEvent, PlexLoginEvent, PlexLoginMethod,
    SourceSettingsChange, SourceSetup,
};
use rufin_core::runtime::{ProductHandles, SourceHandle};

#[derive(Clone, Copy, uniffi::Enum)]
pub enum AndroidSourceDiscoveryProvider {
    Jellyfin,
    Emby,
    Plex,
}

impl From<AndroidSourceDiscoveryProvider> for sources::DiscoveryProvider {
    fn from(value: AndroidSourceDiscoveryProvider) -> Self {
        match value {
            AndroidSourceDiscoveryProvider::Jellyfin => Self::Jellyfin,
            AndroidSourceDiscoveryProvider::Emby => Self::Emby,
            AndroidSourceDiscoveryProvider::Plex => Self::Plex,
        }
    }
}

#[derive(uniffi::Record)]
pub struct AndroidSourceProvider {
    pub kind: String,
    pub title: String,
}
#[derive(uniffi::Record)]
pub struct AndroidSourceChoice {
    pub index: u32,
    pub title: String,
    pub address: Option<String>,
    pub pin_required: bool,
}
#[derive(uniffi::Record)]
pub struct AndroidSourceEditable {
    pub kind: String,
    pub name: String,
    pub form: String,
    pub integration: bool,
}
#[derive(uniffi::Record)]
pub struct AndroidSourceFileConnection {
    pub id: String,
    pub name: String,
    pub kind: String,
}
#[derive(uniffi::Record)]
pub struct AndroidSourceDiscovery {
    pub provider: AndroidSourceDiscoveryProvider,
    pub phase: String,
    pub failure: Option<String>,
    pub servers: Vec<AndroidSourceChoice>,
}
#[derive(uniffi::Enum)]
pub enum AndroidSourceAuthorizationEvent {
    Code { code: String, url: String },
    OpenBrowser { url: String },
    Ready { kind: String },
}

#[derive(Clone, Default)]
enum Authorized {
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

#[derive(uniffi::Object)]
pub struct AndroidSourceSetup {
    source: SourceHandle,
    runtime: tokio::runtime::Handle,
    discovery: tokio::sync::watch::Sender<Option<DiscoveryUpdate>>,
    plex_lan: Arc<Mutex<Vec<sources::DiscoveredServer>>>,
}

impl AndroidSourceSetup {
    pub(crate) fn new(products: &ProductHandles, updates: Receiver<DiscoveryUpdate>) -> Self {
        let (discovery, _) = tokio::sync::watch::channel(None);
        let publish = discovery.clone();
        let plex_lan = Arc::new(Mutex::new(Vec::new()));
        let lan = plex_lan.clone();
        products.runtime.spawn(async move {
            while let Ok(update) = updates.recv().await {
                if update.provider == sources::DiscoveryProvider::Plex {
                    *lan.lock().expect("Plex discovery") = update.servers.to_vec();
                }
                publish.send_replace(Some(update));
            }
        });
        Self {
            source: products.source.clone(),
            runtime: products.runtime.clone(),
            discovery,
            plex_lan,
        }
    }

    fn authorization<E: Send + 'static>(
        &self,
        events: Receiver<Result<E, String>>,
        project: impl Fn(E, &mut Authorized) -> AndroidSourceAuthorizationEvent + Send + 'static,
    ) -> Arc<AndroidSourceAuthorization> {
        let (send, receive) = async_channel::bounded(2);
        let state = Arc::new(Mutex::new(Authorized::Pending));
        let saved = state.clone();
        self.runtime.spawn(async move {
            loop {
                let event =
                    tokio::select! { _ = send.closed() => break, event = events.recv() => event };
                let Ok(event) = event else { break };
                let projected = event
                    .map(|value| project(value, &mut saved.lock().expect("Source authorization")))
                    .map_err(error);
                if send.send(projected).await.is_err() {
                    break;
                }
            }
        });
        Arc::new(AndroidSourceAuthorization {
            source: self.source.clone(),
            events: receive,
            state,
            plex_lan: self.plex_lan.clone(),
        })
    }
}

#[uniffi::export]
impl AndroidSourceSetup {
    pub fn providers(&self) -> Vec<AndroidSourceProvider> {
        [
            "local",
            "jellyfin",
            "emby",
            "plex",
            "navidrome",
            "subsonic",
            "webdav",
            "smb",
        ]
        .into_iter()
        .map(|kind| AndroidSourceProvider {
            kind: kind.into(),
            title: localization::tr(
                rufin_core::runtime::source::source_kind_title(kind)
                    .expect("Supported source title"),
            ),
        })
        .collect()
    }

    pub fn subscribe_discovery(&self) -> Arc<AndroidSourceDiscoverySubscription> {
        let mut changes = self.discovery.subscribe();
        changes.mark_changed();
        Arc::new(AndroidSourceDiscoverySubscription {
            changes: tokio::sync::Mutex::new(changes),
        })
    }
    pub fn discover(&self, provider: AndroidSourceDiscoveryProvider) {
        self.source.discover_servers(provider.into());
    }

    pub async fn editable(&self, id: String) -> Result<AndroidSourceEditable, AndroidError> {
        let source = self.source.clone();
        self.runtime
            .spawn_blocking(move || {
                let id = sources::SourceId::new(id);
                if let Some(local) = source
                    .list_sources()
                    .sources
                    .iter()
                    .find(|row| row.id == id && row.kind == "local")
                {
                    return Ok(AndroidSourceEditable {
                        kind: local.kind.clone(),
                        name: local.name.clone(),
                        form: serde_json::json!({"source":local}).to_string(),
                        integration: false,
                    });
                }
                let (form, integration) = match source.configured_source(&id).map_err(error)? {
                    Some(form) => (form, false),
                    None => (
                        source
                            .file_integration_settings(&id)
                            .map_err(error)?
                            .ok_or_else(|| error("Source is no longer available"))?,
                        true,
                    ),
                };
                Ok(AndroidSourceEditable {
                    kind: form.source.kind.clone(),
                    name: form.source.name.clone(),
                    form: serde_json::to_string(&form).map_err(error)?,
                    integration,
                })
            })
            .await
            .map_err(error)?
    }

    pub async fn submit(
        &self,
        form: String,
        editing: bool,
        integration: bool,
    ) -> Result<(), AndroidError> {
        if editing {
            let input: SourceSettingsChange = serde_json::from_str(&form).map_err(error)?;
            let result = if integration {
                self.source.update_file_integration(input)
            } else {
                self.source.update_source(input)
            };
            result.recv().await.map_err(error)?.map_err(error)
        } else {
            let input: SourceSetup = serde_json::from_str(&form).map_err(error)?;
            if integration {
                self.source
                    .configure_file_integration(input)
                    .recv()
                    .await
                    .map_err(error)?
                    .map(|_| ())
                    .map_err(error)
            } else {
                self.source
                    .configure_source(input)
                    .recv()
                    .await
                    .map_err(error)?
                    .map(|_| ())
                    .map_err(error)
            }
        }
    }

    pub fn quick_connect(
        &self,
        url: String,
        trust_invalid_cert: bool,
    ) -> Arc<AndroidSourceAuthorization> {
        self.authorization(
            self.source.jellyfin_quick_connect(url, trust_invalid_cert),
            |event, saved| match event {
                JellyfinQuickConnectEvent::Code { code, url } => {
                    AndroidSourceAuthorizationEvent::Code { code, url }
                }
                JellyfinQuickConnectEvent::Authorized(login) => {
                    *saved = Authorized::Jellyfin(login);
                    AndroidSourceAuthorizationEvent::Ready {
                        kind: "jellyfin".into(),
                    }
                }
            },
        )
    }
    pub fn emby_login(
        &self,
        username: Option<String>,
        password: Option<String>,
    ) -> Arc<AndroidSourceAuthorization> {
        let method = match username {
            Some(username) => EmbyConnectLoginMethod::Password {
                username,
                password: password.unwrap_or_default(),
            },
            None => EmbyConnectLoginMethod::Pin,
        };
        self.authorization(
            self.source.emby_connect_login(method),
            |event, saved| match event {
                EmbyConnectLoginEvent::Code { code, url } => {
                    AndroidSourceAuthorizationEvent::Code { code, url }
                }
                EmbyConnectLoginEvent::Servers(servers) => {
                    *saved = Authorized::Emby(servers);
                    AndroidSourceAuthorizationEvent::Ready {
                        kind: "emby".into(),
                    }
                }
            },
        )
    }
    pub fn plex_login(
        &self,
        username: Option<String>,
        password: Option<String>,
        verification_code: Option<String>,
    ) -> Arc<AndroidSourceAuthorization> {
        let method = match username {
            Some(username) => PlexLoginMethod::Password {
                username,
                password: password.unwrap_or_default(),
                verification_code,
            },
            None => PlexLoginMethod::Browser,
        };
        self.authorization(self.source.plex_login(method), |event, saved| match event {
            PlexLoginEvent::OpenBrowser(url) => {
                AndroidSourceAuthorizationEvent::OpenBrowser { url }
            }
            PlexLoginEvent::Authorized(login) => {
                *saved = Authorized::Plex {
                    login: *login,
                    profiles: Vec::new(),
                    profile: None,
                    servers: Vec::new(),
                };
                AndroidSourceAuthorizationEvent::Ready {
                    kind: "plex".into(),
                }
            }
        })
    }
    pub async fn saved_plex_logins(&self) -> Result<Arc<AndroidSourceAuthorization>, AndroidError> {
        let logins = self
            .source
            .plex_saved_logins()
            .recv()
            .await
            .map_err(error)?
            .map_err(error)?;
        let (send, receive) = async_channel::bounded(1);
        drop(send);
        Ok(Arc::new(AndroidSourceAuthorization {
            source: self.source.clone(),
            events: receive,
            state: Arc::new(Mutex::new(Authorized::SavedPlex(logins))),
            plex_lan: self.plex_lan.clone(),
        }))
    }
    pub fn nextcloud_login(
        &self,
        settings: String,
        credentials: String,
    ) -> Result<Arc<AndroidSourceAuthorization>, AndroidError> {
        Ok(self.authorization(
            self.source.nextcloud_login(
                serde_json::from_str(&settings).map_err(error)?,
                serde_json::from_str(&credentials).map_err(error)?,
            ),
            |event, saved| match event {
                NextcloudLoginEvent::OpenBrowser(url) => {
                    AndroidSourceAuthorizationEvent::OpenBrowser { url }
                }
                NextcloudLoginEvent::Authorized {
                    settings,
                    credentials,
                } => {
                    *saved = Authorized::Files {
                        settings,
                        credentials,
                    };
                    AndroidSourceAuthorizationEvent::Ready {
                        kind: "webdav".into(),
                    }
                }
            },
        ))
    }
    pub async fn smb_shares(
        &self,
        settings: String,
        credentials: String,
    ) -> Result<Vec<AndroidSourceChoice>, AndroidError> {
        self.source
            .smb_shares(
                serde_json::from_str(&settings).map_err(error)?,
                serde_json::from_str(&credentials).map_err(error)?,
            )
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
            .map(|shares| {
                shares
                    .into_iter()
                    .enumerate()
                    .map(|(index, (title, address))| AndroidSourceChoice {
                        index: index as u32,
                        title,
                        address: Some(address),
                        pin_required: false,
                    })
                    .collect()
            })
    }
    pub fn file_connections(&self) -> Vec<AndroidSourceFileConnection> {
        self.source
            .file_integrations()
            .into_iter()
            .filter(|source| !source.music_source)
            .map(|source| AndroidSourceFileConnection {
                kind: source.kind,
                name: source.name,
                id: source.id.to_string(),
            })
            .collect()
    }
    pub async fn forget(&self, id: String) -> Result<(), AndroidError> {
        self.source
            .forget_source(sources::SourceId::new(id))
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }
}

#[derive(uniffi::Object)]
pub struct AndroidSourceAuthorization {
    source: SourceHandle,
    events: Receiver<Result<AndroidSourceAuthorizationEvent, AndroidError>>,
    state: Arc<Mutex<Authorized>>,
    plex_lan: Arc<Mutex<Vec<sources::DiscoveredServer>>>,
}

impl Drop for AndroidSourceAuthorization {
    fn drop(&mut self) {
        self.events.close();
    }
}

#[uniffi::export]
impl AndroidSourceAuthorization {
    pub async fn next(&self) -> Result<AndroidSourceAuthorizationEvent, AndroidError> {
        self.events.recv().await.map_err(error)?
    }
    pub fn cancel(&self) {
        self.events.close();
        *self.state.lock().expect("Source authorization") = Authorized::Pending;
    }
    pub fn choices(&self) -> Vec<AndroidSourceChoice> {
        let state = self.state.lock().expect("Source authorization");
        match &*state {
            Authorized::Emby(servers) => servers
                .iter()
                .enumerate()
                .map(|(index, server)| AndroidSourceChoice {
                    index: index as u32,
                    title: server.name.clone(),
                    address: None,
                    pin_required: false,
                })
                .collect(),
            Authorized::Plex { servers, .. } => servers
                .iter()
                .enumerate()
                .map(|(index, server)| AndroidSourceChoice {
                    index: index as u32,
                    title: server.name.clone(),
                    address: None,
                    pin_required: false,
                })
                .collect(),
            Authorized::SavedPlex(logins) => logins
                .iter()
                .enumerate()
                .map(|(index, (title, _))| AndroidSourceChoice {
                    index: index as u32,
                    title: title.clone(),
                    address: None,
                    pin_required: false,
                })
                .collect(),
            _ => Vec::new(),
        }
    }
    pub fn file_settings(&self) -> Option<String> {
        match &*self.state.lock().expect("Source authorization") {
            Authorized::Files { settings, .. } => {
                Some(serde_json::to_string(settings).expect("File settings"))
            }
            _ => None,
        }
    }
    pub async fn plex_profiles(
        &self,
        saved_index: Option<u32>,
    ) -> Result<Vec<AndroidSourceChoice>, AndroidError> {
        let login = match &*self.state.lock().expect("Plex authorization") {
            Authorized::Plex { login, .. } => login.clone(),
            Authorized::SavedPlex(logins) => logins
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
        let choices = profiles
            .iter()
            .enumerate()
            .map(|(index, profile)| AndroidSourceChoice {
                index: index as u32,
                title: profile.name.clone(),
                address: None,
                pin_required: profile.pin_required,
            })
            .collect();
        *self.state.lock().expect("Plex authorization") = Authorized::Plex {
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
    ) -> Result<Vec<AndroidSourceChoice>, AndroidError> {
        let (login, profile) = match &*self.state.lock().expect("Plex authorization") {
            Authorized::Plex {
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
        let lan = self.plex_lan.lock().expect("Plex discovery").clone();
        let (login, servers) = self
            .source
            .plex_servers(login, profile, pin, lan)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)?;
        let mut state = self.state.lock().expect("Plex authorization");
        let profiles = match &*state {
            Authorized::Plex { profiles, .. } => profiles.clone(),
            _ => Vec::new(),
        };
        *state = Authorized::Plex {
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
        server_index: u32,
        use_instant_mix: bool,
        address_override: Option<String>,
        trust_invalid_cert: bool,
        source_id: Option<String>,
        integration: bool,
        file_settings: Option<String>,
        file_credentials: Option<String>,
    ) -> Result<(), AndroidError> {
        let mut authorized = self.state.lock().expect("Source authorization").clone();
        if let Authorized::Files {
            settings,
            credentials,
        } = &mut authorized
        {
            if let Some(value) = file_settings {
                *settings = serde_json::from_str(&value).map_err(error)?;
            }
            if let Some(value) = file_credentials {
                let edits: sources::FileCredentialsEdit =
                    serde_json::from_str(&value).map_err(error)?;
                if let Some(secret) = edits.secret {
                    credentials.secret = secret;
                }
                if let Some(headers) = edits.headers {
                    credentials.headers = headers;
                }
            }
        }
        let name = (!name.trim().is_empty()).then(|| name.trim().to_string());
        let id = source_id.map(sources::SourceId::new);
        if let Some(source_id) = id {
            let input = match authorized {
                Authorized::Jellyfin(login) => SourceSettingsChange::JellyfinQuickConnect {
                    source_id,
                    login,
                    source_name: name,
                    use_instant_mix,
                },
                Authorized::Emby(servers) => SourceSettingsChange::EmbyConnect {
                    source_id,
                    server: servers
                        .get(server_index as usize)
                        .cloned()
                        .ok_or_else(|| error("Choose a server"))?,
                    source_name: name,
                    use_instant_mix,
                },
                Authorized::Files {
                    settings,
                    credentials,
                } => SourceSettingsChange::Files {
                    source_id,
                    name: name.unwrap_or_default(),
                    settings,
                    credentials: sources::FileCredentialsEdit {
                        secret: Some(credentials.secret),
                        headers: Some(credentials.headers),
                    },
                },
                Authorized::Plex { .. } => SourceSettingsChange::Plex {
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
            receiver.recv().await.map_err(error)?.map_err(error)
        } else {
            let input = match authorized {
                Authorized::Jellyfin(login) => SourceSetup::JellyfinQuickConnect {
                    login,
                    source_name: name,
                    use_instant_mix,
                },
                Authorized::Emby(servers) => SourceSetup::EmbyConnect {
                    server: servers
                        .get(server_index as usize)
                        .cloned()
                        .ok_or_else(|| error("Choose a server"))?,
                    source_name: name,
                    use_instant_mix,
                },
                Authorized::Files {
                    settings,
                    credentials,
                } => SourceSetup::WebDav {
                    name: name.unwrap_or_default(),
                    settings,
                    credentials,
                },
                Authorized::Plex {
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
                    .map(|_| ())
                    .map_err(error)
            } else {
                self.source
                    .configure_source(input)
                    .recv()
                    .await
                    .map_err(error)?
                    .map(|_| ())
                    .map_err(error)
            }
        }
    }
}

#[derive(uniffi::Object)]
pub struct AndroidSourceDiscoverySubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<Option<DiscoveryUpdate>>>,
}

#[uniffi::export]
impl AndroidSourceDiscoverySubscription {
    pub async fn next(&self) -> Result<AndroidSourceDiscovery, AndroidError> {
        let mut changes = self.changes.lock().await;
        loop {
            changes.changed().await.map_err(error)?;
            if let Some(update) = changes.borrow_and_update().clone() {
                let (phase, failure) = match update.status {
                    DiscoveryStatus::Idle => ("idle", None),
                    DiscoveryStatus::Searching => ("searching", None),
                    DiscoveryStatus::Empty => ("empty", None),
                    DiscoveryStatus::Found(_) => ("found", None),
                    DiscoveryStatus::Failed(reason) => ("failed", Some(reason)),
                };
                return Ok(AndroidSourceDiscovery {
                    provider: match update.provider {
                        sources::DiscoveryProvider::Jellyfin => {
                            AndroidSourceDiscoveryProvider::Jellyfin
                        }
                        sources::DiscoveryProvider::Emby => AndroidSourceDiscoveryProvider::Emby,
                        sources::DiscoveryProvider::Plex => AndroidSourceDiscoveryProvider::Plex,
                    },
                    phase: phase.into(),
                    failure,
                    servers: update
                        .servers
                        .iter()
                        .enumerate()
                        .map(|(index, server)| AndroidSourceChoice {
                            index: index as u32,
                            title: server.name.clone(),
                            address: Some(server.address.clone()),
                            pin_required: false,
                        })
                        .collect(),
                });
            }
        }
    }
}
