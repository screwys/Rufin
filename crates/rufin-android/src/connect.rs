use std::sync::Arc;

use rufin_core::connect::{self, Action, ActionError, Control, Encoding, portable::Destination};
use rufin_core::runtime::{ProductHandles, SourceHandle};

use crate::host::{AndroidError, error};

#[derive(Clone, Copy, uniffi::Enum)]
pub enum AndroidConnectEncoding {
    Original,
    Mp3,
}

impl From<AndroidConnectEncoding> for Encoding {
    fn from(value: AndroidConnectEncoding) -> Self {
        match value {
            AndroidConnectEncoding::Original => Self::Original,
            AndroidConnectEncoding::Mp3 => Self::Mp3,
        }
    }
}

#[derive(Clone, uniffi::Enum)]
pub enum AndroidConnectDestination {
    Local { path: String },
    Document { uri: String },
    Remote { source_id: String, path: String },
}

impl From<AndroidConnectDestination> for Destination {
    fn from(value: AndroidConnectDestination) -> Self {
        match value {
            AndroidConnectDestination::Local { path } => Self::Local { path: path.into() },
            AndroidConnectDestination::Document { uri } => Self::Document { uri },
            AndroidConnectDestination::Remote { source_id, path } => Self::Remote {
                source_id: sources::SourceId::new(source_id),
                path,
            },
        }
    }
}

#[derive(Clone, uniffi::Enum)]
pub enum AndroidConnectControl {
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    Seek { millis: u64 },
    Volume { value: f64 },
}

impl From<AndroidConnectControl> for Control {
    fn from(value: AndroidConnectControl) -> Self {
        match value {
            AndroidConnectControl::Play => Self::Play,
            AndroidConnectControl::Pause => Self::Pause,
            AndroidConnectControl::Stop => Self::Stop,
            AndroidConnectControl::Next => Self::Next,
            AndroidConnectControl::Previous => Self::Previous,
            AndroidConnectControl::Seek { millis } => Self::Seek { millis },
            AndroidConnectControl::Volume { value } => Self::Volume { value },
        }
    }
}

#[derive(Clone, uniffi::Enum)]
pub enum AndroidConnectAction {
    Enable {
        enabled: bool,
    },
    Create,
    Join {
        invitation: String,
        replace: bool,
    },
    Invite {
        peer: String,
    },
    Pair {
        session: String,
        approve: bool,
        replace: bool,
    },
    CancelPairing,
    FinishSetup,
    Rename {
        name: String,
    },
    Remove {
        peer: String,
    },
    Leave,
    Refresh,
    Discover,
    TestConnection {
        peer: String,
    },
    Folder {
        source: String,
        root_id: String,
        uri: Option<String>,
    },
    Network {
        nearby: bool,
        relay: Option<String>,
        public_relay: bool,
    },
    Continue {
        peer: String,
    },
    Control {
        peer: String,
        command: AndroidConnectControl,
    },
    Encoding {
        encoding: AndroidConnectEncoding,
        confirm: bool,
    },
    Destination {
        destination: Option<AndroidConnectDestination>,
    },
    ImportRemote {
        source_id: String,
        path: String,
        replace: bool,
    },
    ExportRemote {
        source_id: String,
        path: String,
    },
}

impl From<AndroidConnectAction> for Action {
    fn from(value: AndroidConnectAction) -> Self {
        match value {
            AndroidConnectAction::Enable { enabled } => Self::Enable { enabled },
            AndroidConnectAction::Create => Self::Create,
            AndroidConnectAction::Join {
                invitation,
                replace,
            } => Self::Join {
                invitation,
                replace,
            },
            AndroidConnectAction::Invite { peer } => Self::Invite { peer },
            AndroidConnectAction::Pair {
                session,
                approve,
                replace,
            } => Self::Pair {
                session,
                approve,
                replace,
            },
            AndroidConnectAction::CancelPairing => Self::CancelPairing,
            AndroidConnectAction::FinishSetup => Self::FinishSetup,
            AndroidConnectAction::Rename { name } => Self::Rename { name },
            AndroidConnectAction::Remove { peer } => Self::Remove { peer },
            AndroidConnectAction::Leave => Self::Leave,
            AndroidConnectAction::Refresh => Self::Refresh,
            AndroidConnectAction::Discover => Self::Discover,
            AndroidConnectAction::TestConnection { peer } => Self::TestConnection { peer },
            AndroidConnectAction::Folder {
                source,
                root_id,
                uri,
            } => Self::Folder {
                source,
                root_id: Some(root_id),
                path: uri.map_or_else(
                    || downloads::DownloadDirectory::Native(Default::default()),
                    |uri| downloads::DownloadDirectory::Document { uri },
                ),
            },
            AndroidConnectAction::Network {
                nearby,
                relay,
                public_relay,
            } => Self::Network {
                nearby,
                relay,
                public_relay,
            },
            AndroidConnectAction::Continue { peer } => Self::Continue { peer },
            AndroidConnectAction::Control { peer, command } => Self::Control {
                peer,
                command: command.into(),
            },
            AndroidConnectAction::Encoding { encoding, confirm } => Self::Encoding {
                encoding: encoding.into(),
                confirm,
            },
            AndroidConnectAction::Destination { destination } => Self::Destination {
                destination: destination.map(Into::into),
            },
            AndroidConnectAction::ImportRemote {
                source_id,
                path,
                replace,
            } => Self::Import {
                path: path.into(),
                source_id: Some(sources::SourceId::new(source_id)),
                replace,
                key: None,
            },
            AndroidConnectAction::ExportRemote { source_id, path } => Self::Export {
                path: path.into(),
                source_id: Some(sources::SourceId::new(source_id)),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum AndroidConnectConfirmation {
    ReplaceProfile,
    RedownloadFiles,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AndroidConnectError {
    #[error("{reason}")]
    Failed { reason: String },
    #[error("{reason}")]
    ConfirmationRequired {
        confirmation: AndroidConnectConfirmation,
        reason: String,
    },
}

impl From<ActionError> for AndroidConnectError {
    fn from(value: ActionError) -> Self {
        match value {
            ActionError::Failed(reason) => Self::Failed { reason },
            ActionError::ConfirmationRequired {
                confirmation,
                message,
            } => Self::ConfirmationRequired {
                confirmation: match confirmation {
                    connect::Confirmation::ReplaceProfile => {
                        AndroidConnectConfirmation::ReplaceProfile
                    }
                    connect::Confirmation::RedownloadFiles => {
                        AndroidConnectConfirmation::RedownloadFiles
                    }
                },
                reason: message,
            },
        }
    }
}

#[derive(uniffi::Record)]
pub struct AndroidConnectDevice {
    pub id: String,
    pub name: String,
    pub reachable: bool,
    pub has_playback: bool,
    pub enrolled: bool,
    pub connection: String,
}

#[derive(uniffi::Record)]
pub struct AndroidConnectPairing {
    pub session: String,
    pub name: String,
    pub emoji: Vec<String>,
    pub approved: bool,
    pub verified: bool,
    pub joining: bool,
}

#[derive(uniffi::Record)]
pub struct AndroidConnectFileSource {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub path: String,
}

#[derive(uniffi::Record)]
pub struct AndroidConnectMusicFolder {
    pub source_id: String,
    pub root_id: String,
    pub title: String,
    pub location: Option<String>,
}

#[derive(uniffi::Record)]
pub struct AndroidConnectStatus {
    pub enabled: bool,
    pub name: String,
    pub profile: Option<String>,
    pub identity: Option<String>,
    pub invitation: Option<String>,
    pub nearby: bool,
    pub relay: Option<String>,
    pub public_relay: bool,
    pub destination: Option<AndroidConnectDestination>,
    pub encoding: AndroidConnectEncoding,
    pub established: bool,
    pub setup_pending: bool,
    pub adopting: bool,
    pub connecting: bool,
    pub receiving_collection: bool,
    pub profile_status: String,
    pub media_status: Option<String>,
    pub error: Option<String>,
    pub devices: Vec<AndroidConnectDevice>,
    pub pairing: Option<AndroidConnectPairing>,
}

impl From<connect::Status> for AndroidConnectStatus {
    fn from(value: connect::Status) -> Self {
        let settings = value.settings;
        Self {
            enabled: settings.enabled,
            name: settings.name,
            profile: settings.profile,
            identity: value.identity,
            invitation: value.invitation,
            nearby: settings.nearby,
            relay: settings.relay,
            public_relay: settings.public_relay,
            destination: settings.destination.map(|destination| match destination {
                Destination::Local { path } => AndroidConnectDestination::Local {
                    path: path.to_string_lossy().into_owned(),
                },
                Destination::Document { uri } => AndroidConnectDestination::Document { uri },
                Destination::Remote { source_id, path } => AndroidConnectDestination::Remote {
                    source_id: source_id.to_string(),
                    path,
                },
            }),
            encoding: match settings.encoding {
                Encoding::Original => AndroidConnectEncoding::Original,
                Encoding::Mp3 => AndroidConnectEncoding::Mp3,
            },
            established: settings.established,
            setup_pending: settings.setup_pending,
            adopting: settings.adopting,
            connecting: value.connecting,
            receiving_collection: value.receiving_collection,
            profile_status: value.profile_status,
            media_status: value.media_status,
            error: value.error,
            devices: value
                .devices
                .into_iter()
                .map(|device| AndroidConnectDevice {
                    id: device.id,
                    name: device.name,
                    reachable: device.reachable,
                    has_playback: device.has_playback,
                    enrolled: device.enrolled,
                    connection: device.connection,
                })
                .collect(),
            pairing: value.pairing.map(|pairing| AndroidConnectPairing {
                session: pairing.session,
                name: pairing.name,
                emoji: pairing.emoji.into(),
                approved: pairing.approved,
                verified: pairing.verified,
                joining: pairing.joining,
            }),
        }
    }
}

#[derive(uniffi::Object)]
pub struct AndroidConnect {
    owner: Arc<connect::ConnectOwner>,
    source: SourceHandle,
    runtime: tokio::runtime::Handle,
}

impl AndroidConnect {
    pub(crate) fn new(products: &ProductHandles) -> Self {
        Self {
            owner: products.connect.clone(),
            source: products.source.clone(),
            runtime: products.runtime.clone(),
        }
    }
}

#[uniffi::export]
impl AndroidConnect {
    pub async fn music_folders(&self) -> Result<Vec<AndroidConnectMusicFolder>, AndroidError> {
        let owner = self.owner.clone();
        let source = self.source.clone();
        self.runtime
            .spawn(async move {
                let status = owner.status();
                let mut folders = Vec::new();
                for source in source.list_sources().sources.iter() {
                    for root in owner.roots(source.id.as_str()).await.map_err(error)? {
                        let key = format!("{}/{}", source.id, root.id);
                        let directory = status
                            .settings
                            .folders
                            .get(&key)
                            .or_else(|| status.settings.folders.get(source.id.as_str()));
                        folders.push(AndroidConnectMusicFolder {
                            source_id: source.id.to_string(),
                            root_id: root.id,
                            title: format!("{} · {}", source.name, root.label),
                            location: directory.map(ToString::to_string),
                        });
                    }
                }
                Ok(folders)
            })
            .await
            .map_err(error)?
    }

    pub fn status(&self) -> AndroidConnectStatus {
        self.owner.status().into()
    }

    pub fn subscribe(&self) -> Arc<AndroidConnectSubscription> {
        let mut changes = self.owner.subscribe();
        changes.mark_changed();
        Arc::new(AndroidConnectSubscription {
            changes: tokio::sync::Mutex::new(changes),
        })
    }

    pub fn file_sources(&self) -> Vec<AndroidConnectFileSource> {
        let status = self.owner.status();
        self.source
            .file_integrations()
            .into_iter()
            .map(|source| AndroidConnectFileSource {
                path: status.storage_path(Some(&source.id)),
                id: source.id.to_string(),
                name: source.name,
                kind: source.kind,
            })
            .collect()
    }

    pub async fn profile_files(
        &self,
        source_id: String,
        folder: String,
    ) -> Result<Vec<String>, AndroidError> {
        let owner = self.owner.clone();
        self.runtime
            .spawn(async move {
                owner
                    .list_profile_files(sources::SourceId::new(source_id), folder)
                    .await
            })
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn execute(
        &self,
        action: AndroidConnectAction,
    ) -> Result<AndroidConnectStatus, AndroidConnectError> {
        let owner = self.owner.clone();
        self.runtime
            .spawn(async move { owner.execute(action.into()).await })
            .await
            .map_err(|error| AndroidConnectError::Failed {
                reason: error.to_string(),
            })?
            .map(Into::into)
            .map_err(Into::into)
    }

    pub async fn import_document(
        &self,
        uri: String,
        replace: bool,
    ) -> Result<AndroidConnectStatus, AndroidConnectError> {
        let owner = self.owner.clone();
        self.runtime
            .spawn(async move { owner.import_document(uri, replace, None).await })
            .await
            .map_err(|error| AndroidConnectError::Failed {
                reason: error.to_string(),
            })?
            .map(Into::into)
            .map_err(Into::into)
    }

    pub async fn export_document(
        &self,
        uri: String,
    ) -> Result<AndroidConnectStatus, AndroidConnectError> {
        let owner = self.owner.clone();
        self.runtime
            .spawn(async move { owner.export_document(uri).await })
            .await
            .map_err(|error| AndroidConnectError::Failed {
                reason: error.to_string(),
            })?
            .map(Into::into)
            .map_err(Into::into)
    }
}

#[derive(uniffi::Object)]
pub struct AndroidConnectSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<connect::Status>>,
}

#[uniffi::export]
impl AndroidConnectSubscription {
    pub async fn next(&self) -> Result<AndroidConnectStatus, AndroidError> {
        let mut changes = self.changes.lock().await;
        changes.changed().await.map_err(error)?;
        let status = changes.borrow_and_update().clone();
        Ok(status.into())
    }
}
