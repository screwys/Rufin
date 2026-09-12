use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use secrets::{SecretKey, SecretStore};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::runtime::ProductHandles;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ControllerSettings {
    pub enabled: bool,
    pub address: IpAddr,
    pub port: u16,
}

impl Default for ControllerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            address: Ipv4Addr::LOCALHOST.into(),
            port: 1717,
        }
    }
}

#[derive(Clone, Default)]
pub struct ControllerStatus {
    pub address: Option<SocketAddr>,
    pub token: String,
    pub error: Option<String>,
}

impl ControllerStatus {
    pub fn addresses(&self) -> Result<Vec<SocketAddr>, String> {
        let Some(address) = self.address else {
            return Ok(Vec::new());
        };
        if !address.ip().is_unspecified() {
            return Ok(vec![address]);
        }
        let mut addresses = playback_cast::network_addresses()?
            .into_iter()
            .filter(|ip| ip.is_ipv4() == address.is_ipv4())
            .map(|ip| SocketAddr::new(ip, address.port()))
            .collect::<Vec<_>>();
        addresses.sort_unstable();
        addresses.dedup();
        Ok(addresses)
    }
}

/// Owned by the host, independently of the playback and library it exposes.
pub struct Controller {
    task: tokio::task::JoinHandle<()>,
    status: watch::Receiver<ControllerStatus>,
    runtime: tokio::runtime::Handle,
    settings: crate::settings::SettingsFile,
    secrets: Arc<secrets::SwitchableSecretStore>,
}

impl Controller {
    pub fn new(products: ProductHandles) -> Self {
        let file = products.source.shared.settings.clone();
        let runtime = products.runtime.clone();
        let secrets = Arc::clone(&products.source.shared.secrets);
        let mut changes = file.web_controller_changes();
        let (status, receiver) = watch::channel(ControllerStatus::default());
        let task = products.runtime.clone().spawn(async move {
            loop {
                let config = changes.borrow_and_update().clone();
                status.send_replace(ControllerStatus::default());
                let run = async {
                    if !config.enabled {
                        return Ok(());
                    }
                    let secrets = Arc::clone(&products.source.shared.secrets);
                    let token = tokio::task::spawn_blocking(move || {
                        let store = secrets.current().map_err(|error| error.to_string())?;
                        access_token(store, false)
                    })
                        .await.map_err(|error| error.to_string())??;
                    let listener = tokio::net::TcpListener::bind(SocketAddr::new(config.address, config.port))
                        .await.map_err(|error| error.to_string())?;
                    let address = listener.local_addr().map_err(|error| error.to_string())?;
                    status.send_replace(ControllerStatus {
                        address: Some(address), token: token.clone(), error: None,
                    });
                    super::serve(listener, products.clone(), token)
                        .await.map_err(|error| error.to_string())
                };
                tokio::select! {
                    result = run => {
                        if let Err(error) = result {
                            tracing::warn!(%error, "web controller stopped");
                            status.send_replace(ControllerStatus { error: Some(error), ..Default::default() });
                        }
                        if changes.changed().await.is_err() { break; }
                    }
                    changed = changes.changed() => { if changed.is_err() { break; } }
                }
            }
        });
        Self {
            task,
            status: receiver,
            runtime,
            settings: file,
            secrets,
        }
    }

    pub fn status(&self) -> watch::Receiver<ControllerStatus> {
        self.status.clone()
    }

    pub fn stop(&self) {
        self.task.abort();
    }

    pub fn regenerate_token(&self) -> tokio::task::JoinHandle<Result<String, String>> {
        let secrets = Arc::clone(&self.secrets);
        let settings = self.settings.clone();
        self.runtime.spawn_blocking(move || {
            let token = access_token(secrets.current().map_err(|error| error.to_string())?, true)?;
            settings.web_controller_credentials_changed();
            Ok(token)
        })
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.stop();
    }
}

fn access_token(store: Arc<dyn SecretStore>, regenerate: bool) -> Result<String, String> {
    let key = SecretKey::namespaced("web-controller", "token", "Rufin Web Controller token");
    if !regenerate
        && let Some(token) = store.load_secret(&key).map_err(|error| error.to_string())?
        && !token.is_empty()
    {
        return Ok(token);
    }
    use std::fmt::Write;
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    let mut token = String::with_capacity(64);
    for byte in bytes {
        write!(&mut token, "{byte:02x}").expect("write token");
    }
    store
        .save_secret(&key, &token)
        .map_err(|error| error.to_string())?;
    Ok(token)
}
