use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use tokio::sync::watch;

use rufin_core::runtime::ProductHandles;

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
        let loopback = if address.is_ipv4() {
            Ipv4Addr::LOCALHOST.into()
        } else {
            Ipv6Addr::LOCALHOST.into()
        };
        addresses.push(SocketAddr::new(loopback, address.port()));
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
    settings: rufin_core::SettingsHandle,
}

impl Controller {
    pub fn new(products: ProductHandles) -> Self {
        let settings = products.settings.clone();
        let runtime = products.runtime.clone();
        let mut changes = settings.web_controller_changes();
        let (status, receiver) = watch::channel(ControllerStatus::default());
        let task = products.runtime.clone().spawn(async move {
            loop {
                let config = changes.borrow_and_update().clone();
                status.send_replace(ControllerStatus::default());
                let run = async {
                    if !config.enabled {
                        return Ok(());
                    }
                    let settings = products.settings.clone();
                    let token = tokio::task::spawn_blocking(move || settings.web_controller_token(false))
                        .await.map_err(|error| error.to_string())??;
                    let listener = bind_available_port(SocketAddr::new(config.address, config.port))
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
            settings,
        }
    }

    pub fn status(&self) -> watch::Receiver<ControllerStatus> {
        self.status.clone()
    }

    pub fn stop(&self) {
        self.task.abort();
    }

    pub fn regenerate_token(&self) -> tokio::task::JoinHandle<Result<String, String>> {
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.web_controller_token(true))
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn bind_available_port(mut address: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    loop {
        match tokio::net::TcpListener::bind(address).await {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                let Some(port) = address.port().checked_add(1) else {
                    return Err(error);
                };
                address.set_port(port);
            }
            Err(error) => return Err(error),
        }
    }
}
