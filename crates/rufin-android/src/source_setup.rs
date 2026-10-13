use std::sync::Arc;

use crate::host::{AndroidError, error};
use async_channel::Receiver;
use rufin_core::runtime::source::{DiscoveryStatus, DiscoveryUpdate};
use rufin_core::runtime::{ProductHandles, SourceHandle};
use rufin_core::source::authorization::{
    SourceAuthorization, SourceAuthorizationMethod, SourceChoice,
};

use sources::{SourceSettingsChange, SourceSetup};

#[derive(uniffi::Record)]
pub struct AndroidSourceProvider {
    pub kind: String,
    pub title: String,
}
#[derive(uniffi::Record)]
pub struct AndroidSourceEditable {
    pub form: rufin_core::runtime::source::EditableSource,
    pub integration: bool,
}
#[derive(uniffi::Record)]
pub struct AndroidSourceDiscovery {
    pub provider: sources::DiscoveryProvider,
    pub status: DiscoveryStatus,
    pub servers: Vec<sources::DiscoveredServer>,
}
#[derive(uniffi::Object)]
pub struct AndroidSourceSetup {
    source: SourceHandle,
    runtime: tokio::runtime::Handle,
    discovery: tokio::sync::watch::Sender<Option<DiscoveryUpdate>>,
}

impl AndroidSourceSetup {
    pub(crate) fn new(products: &ProductHandles, updates: Receiver<DiscoveryUpdate>) -> Self {
        let (discovery, _) = tokio::sync::watch::channel(None);
        let publish = discovery.clone();
        products.runtime.spawn(async move {
            while let Ok(update) = updates.recv().await {
                publish.send_replace(Some(update));
            }
        });
        Self {
            source: products.source.clone(),
            runtime: products.runtime.clone(),
            discovery,
        }
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
    pub fn discover(&self, provider: sources::DiscoveryProvider) {
        self.source.discover_servers(provider);
    }

    pub async fn editable(&self, id: String) -> Result<AndroidSourceEditable, AndroidError> {
        let source = self.source.clone();
        self.runtime
            .spawn_blocking(move || {
                let id = sources::SourceId::new(id);
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
                Ok(AndroidSourceEditable { form, integration })
            })
            .await
            .map_err(error)?
    }

    pub async fn local_access(
        &self,
        id: String,
    ) -> Result<Option<rufin_core::runtime::source::SourceLocalAccessSummary>, AndroidError> {
        let source = self.source.clone();
        self.runtime
            .spawn_blocking(move || {
                Ok(source
                    .list_sources()
                    .local_access
                    .iter()
                    .find(|summary| summary.source_id.as_str() == id)
                    .cloned())
            })
            .await
            .map_err(error)?
    }

    pub async fn save_local_access(
        &self,
        input: rufin_core::runtime::source::SourceLocalAccess,
    ) -> Result<(), AndroidError> {
        self.source
            .save_local_access(input, true)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub fn clear_local_access(&self, id: String) {
        self.source.clear_local_access(sources::SourceId::new(id));
    }

    pub async fn submit_jellyfin_emby(
        &self,
        source_id: Option<sources::SourceId>,
        kind: sources::ServerKind,
        credentials: sources::CredentialInput,
        use_instant_mix: bool,
        connect_manually: bool,
    ) -> Result<(), AndroidError> {
        if let Some(source_id) = source_id {
            self.source
                .update_source(SourceSettingsChange::JellyfinEmby {
                    source_id,
                    credentials,
                    use_instant_mix,
                    connect_manually,
                })
                .recv()
                .await
                .map_err(error)?
                .map_err(error)
        } else {
            self.source
                .configure_source(SourceSetup::JellyfinEmby {
                    kind,
                    credentials,
                    use_instant_mix,
                })
                .recv()
                .await
                .map_err(error)?
                .map(|_| ())
                .map_err(error)
        }
    }
    pub async fn submit_subsonic(
        &self,
        source_id: Option<sources::SourceId>,
        kind: sources::SubsonicFlavor,
        authentication: sources::SubsonicAuthentication,
        credentials: sources::CredentialInput,
    ) -> Result<(), AndroidError> {
        if let Some(source_id) = source_id {
            self.source
                .update_source(SourceSettingsChange::OpenSubsonic {
                    source_id,
                    kind,
                    authentication,
                    credentials,
                })
                .recv()
                .await
                .map_err(error)?
                .map_err(error)
        } else {
            self.source
                .configure_source(SourceSetup::OpenSubsonic {
                    kind,
                    authentication,
                    credentials,
                })
                .recv()
                .await
                .map_err(error)?
                .map(|_| ())
                .map_err(error)
        }
    }
    pub async fn submit_files(
        &self,
        source_id: Option<sources::SourceId>,
        smb: bool,
        integration: bool,
        name: String,
        settings: sources::FileSourceSettings,
        credentials: sources::FileCredentialsEdit,
    ) -> Result<(), AndroidError> {
        self.source
            .save_files(source_id, smb, integration, name, settings, credentials)
            .await
            .map(|_| ())
            .map_err(error)
    }
    pub async fn submit_plex(
        &self,
        source_id: sources::SourceId,
        settings: sources::PlexSettingsInput,
    ) -> Result<(), AndroidError> {
        self.source
            .update_source(SourceSettingsChange::Plex {
                source_id,
                settings,
            })
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub fn authorize(&self, method: SourceAuthorizationMethod) -> Arc<SourceAuthorization> {
        SourceAuthorization::new(self.source.clone(), method)
    }
    pub async fn saved_plex_logins(&self) -> Result<Arc<SourceAuthorization>, AndroidError> {
        SourceAuthorization::saved_plex(self.source.clone())
            .await
            .map_err(error)
    }
    pub async fn smb_shares(
        &self,
        settings: sources::FileSourceSettings,
        credentials: sources::FileCredentials,
    ) -> Result<Vec<SourceChoice>, AndroidError> {
        self.source
            .smb_shares(settings, credentials)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
            .map(|shares| {
                shares
                    .into_iter()
                    .enumerate()
                    .map(|(index, (title, address))| SourceChoice {
                        index: index as u32,
                        title,
                        address: Some(address),
                        pin_required: false,
                    })
                    .collect()
            })
    }
    pub fn file_connections(&self) -> Vec<rufin_core::source::FileIntegration> {
        self.source
            .file_integrations()
            .into_iter()
            .filter(|source| !source.music_source)
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
                return Ok(AndroidSourceDiscovery {
                    provider: update.provider,
                    status: update.status,
                    servers: update.servers.to_vec(),
                });
            }
        }
    }
}
