use std::sync::Arc;

use rufin_core::{SettingsHandle, runtime::ProductHandles};

use crate::host::{AndroidError, error};

#[derive(uniffi::Record)]
pub struct AndroidControllerState {
    pub enabled: bool,
    pub remote: bool,
    pub port: u16,
    pub address: Option<String>,
    pub links: Vec<String>,
    pub error: Option<String>,
}

#[derive(uniffi::Object)]
pub struct AndroidController {
    owner: Arc<web::Controller>,
    settings: SettingsHandle,
    runtime: tokio::runtime::Handle,
}

impl AndroidController {
    pub(crate) fn new(owner: Arc<web::Controller>, products: &ProductHandles) -> Self {
        Self {
            owner,
            settings: products.settings.clone(),
            runtime: products.runtime.clone(),
        }
    }
}

#[uniffi::export]
impl AndroidController {
    pub fn subscribe(&self) -> Arc<AndroidControllerSubscription> {
        let mut status = self.owner.status();
        let mut settings = self.settings.web_controller_changes();
        status.mark_changed();
        settings.mark_changed();
        Arc::new(AndroidControllerSubscription {
            changes: tokio::sync::Mutex::new((status, settings)),
            runtime: self.runtime.clone(),
        })
    }

    pub async fn set_enabled(&self, enabled: bool) -> Result<(), AndroidError> {
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.set_controller_enabled(enabled))
            .await
            .map_err(error)?
            .map_err(error)?;
        Ok(())
    }

    pub async fn set_remote(&self, remote: bool) -> Result<(), AndroidError> {
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.set_controller_remote(remote))
            .await
            .map_err(error)?
            .map_err(error)?;
        Ok(())
    }

    pub async fn set_port(&self, port: u16) -> Result<(), AndroidError> {
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.set_controller_port(port))
            .await
            .map_err(error)?
            .map_err(error)?;
        Ok(())
    }

    pub async fn token(&self) -> Result<String, AndroidError> {
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.web_controller_token(false))
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn regenerate_token(&self) -> Result<String, AndroidError> {
        self.owner
            .regenerate_token()
            .await
            .map_err(error)?
            .map_err(error)
    }
}

#[derive(uniffi::Object)]
pub struct AndroidControllerSubscription {
    changes: tokio::sync::Mutex<(
        tokio::sync::watch::Receiver<web::ControllerStatus>,
        tokio::sync::watch::Receiver<rufin_core::settings::ControllerSettings>,
    )>,
    runtime: tokio::runtime::Handle,
}

#[uniffi::export]
impl AndroidControllerSubscription {
    pub async fn next(&self) -> Result<AndroidControllerState, AndroidError> {
        let mut changes = self.changes.lock().await;
        let (status, settings) = &mut *changes;
        tokio::select! {
            result = status.changed() => result.map_err(error)?,
            result = settings.changed() => result.map_err(error)?,
        }
        let status = status.borrow_and_update().clone();
        let settings = settings.borrow_and_update().clone();
        drop(changes);
        self.runtime
            .spawn_blocking(move || {
                let (links, address_error) = match status.addresses() {
                    Ok(addresses) => (
                        addresses
                            .into_iter()
                            .map(|address| format!("http://{address}/#token={}", status.token))
                            .collect(),
                        None,
                    ),
                    Err(error) => (Vec::new(), Some(error)),
                };
                AndroidControllerState {
                    enabled: settings.enabled,
                    remote: !settings.address.is_loopback(),
                    port: settings.port,
                    address: status.address.map(|address| format!("http://{address}/")),
                    links,
                    error: status.error.or(address_error),
                }
            })
            .await
            .map_err(error)
    }
}
