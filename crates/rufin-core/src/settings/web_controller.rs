use std::net::{IpAddr, Ipv4Addr};

use secrets::SecretKey;
use serde::{Deserialize, Serialize};

use super::SettingsOwner;

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

impl SettingsOwner {
    pub fn web_controller_changes(&self) -> tokio::sync::watch::Receiver<ControllerSettings> {
        self.file.web_controller_changes()
    }

    pub fn web_controller_token(&self, regenerate: bool) -> Result<String, String> {
        let store = self.secrets.current().map_err(|error| error.to_string())?;
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
        if regenerate {
            self.file.web_controller_credentials_changed();
        }
        Ok(token)
    }
}
