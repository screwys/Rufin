use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use secrets::{ConfigSecretStore, SecretError, SecretKey, SecretResult, SecretStore};
use serde::{Deserialize, Serialize};

use super::{SettingsFile, system_keyring_secret_store};

/// Where saved passwords and tokens are kept.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SecretStorageMode {
    ConfigFile,
    SystemKeyring,
}

impl Default for SecretStorageMode {
    fn default() -> Self {
        default_secret_storage_mode()
    }
}

#[cfg(any(
    target_os = "android",
    target_os = "macos",
    target_os = "windows",
    all(unix, not(any(target_os = "android", target_vendor = "apple")))
))]
fn default_secret_storage_mode() -> SecretStorageMode {
    SecretStorageMode::SystemKeyring
}

#[cfg(not(any(
    target_os = "android",
    target_os = "macos",
    target_os = "windows",
    all(unix, not(any(target_os = "android", target_vendor = "apple")))
)))]
fn default_secret_storage_mode() -> SecretStorageMode {
    SecretStorageMode::ConfigFile
}

/// Keep working when the system keyring is refused, unavailable, or times out.
#[derive(Clone)]
pub struct KeyringSecretStore {
    settings: SettingsFile,
    scope: String,
    keyring: Arc<dyn SecretStore>,
    file: ConfigSecretStore,
    cached: Arc<Mutex<HashMap<SecretKey, Option<String>>>>,
    unavailable: Arc<AtomicBool>,
}

impl KeyringSecretStore {
    pub(super) fn new(settings: SettingsFile) -> Self {
        let scope = settings.load().secret_scope_id.clone();
        Self {
            keyring: system_keyring_secret_store(&scope),
            file: ConfigSecretStore::with_scope(settings.config_dir().join("secrets.json"), &scope),
            settings,
            scope,
            cached: Arc::new(Mutex::new(HashMap::new())),
            unavailable: Arc::new(AtomicBool::new(false)),
        }
    }

    fn run(
        &self,
        key: &SecretKey,
        in_memory: Option<String>,
        operation: impl Fn(&dyn SecretStore) -> SecretResult<Option<String>>,
    ) -> SecretResult<Option<String>> {
        let mut cached = self.cached.lock().map_err(|_| SecretError::Locked)?;
        let stored = self.settings.load();
        if stored.secret_scope_id != self.scope {
            return Err(SecretError::Backend("credential storage was reset".into()));
        }
        let result = if stored.ui.application.secret_storage_mode == SecretStorageMode::ConfigFile {
            operation(&self.file)
        } else if self.unavailable.load(Ordering::Relaxed) {
            Ok(in_memory)
        } else {
            match operation(self.keyring.as_ref()) {
                Ok(value) => Ok(value),
                Err(error) => {
                    if self.settings.load().secret_scope_id != self.scope {
                        return Err(SecretError::Backend("credential storage was reset".into()));
                    }
                    self.unavailable.store(true, Ordering::Relaxed);
                    let _ = self
                        .settings
                        .secret_storage_fallbacks
                        .0
                        .try_send(self.clone());
                    tracing::warn!(%error, "keyring unavailable; keeping credentials in memory");
                    Ok(in_memory)
                }
            }
        }?;
        if self.settings.load().secret_scope_id != self.scope {
            return Err(SecretError::Backend("credential storage was reset".into()));
        }
        cached.insert(key.clone(), result.clone());
        Ok(result)
    }

    /// Save the current session's credentials only after the user confirms.
    pub(crate) fn save_to_file(&self) -> SecretResult<()> {
        let cached = self.cached.lock().map_err(|_| SecretError::Locked)?;
        if self.settings.load().secret_scope_id != self.scope {
            return Err(SecretError::Backend("credential storage was reset".into()));
        }
        for (key, value) in cached.iter() {
            match value {
                Some(value) => self.file.save_secret(key, value)?,
                None => self.file.delete_secret(key)?,
            }
        }
        self.settings
            .update(|stored| {
                if stored.secret_scope_id != self.scope {
                    return Err("credential storage was reset".into());
                }
                stored.ui.application.secret_storage_mode = SecretStorageMode::ConfigFile;
                for source in stored.sources.connections_mut() {
                    if let Some(super::Credential::Sealed { session, .. }) = &mut source.credential
                    {
                        *session = false;
                    }
                }
                for credential in stored.scrobbling_credentials.values_mut() {
                    if let super::Credential::Sealed { session, .. } = credential {
                        *session = false;
                    }
                }
                Ok(())
            })
            .map_err(SecretError::Config)
    }
}

impl SecretStore for KeyringSecretStore {
    fn is_persistent(&self) -> bool {
        !self.unavailable.load(Ordering::Relaxed)
            || self.settings.load().ui.application.secret_storage_mode
                == SecretStorageMode::ConfigFile
    }

    fn save_secret(&self, key: &SecretKey, secret: &str) -> SecretResult<()> {
        self.run(key, Some(secret.into()), |store| {
            store.save_secret(key, secret)?;
            Ok(Some(secret.into()))
        })?;
        Ok(())
    }

    fn load_secret(&self, key: &SecretKey) -> SecretResult<Option<String>> {
        {
            let cached = self.cached.lock().map_err(|_| SecretError::Locked)?;
            if self.settings.load().secret_scope_id != self.scope {
                return Err(SecretError::Backend("credential storage was reset".into()));
            }
            if let Some(value) = cached.get(key).cloned() {
                return Ok(value);
            }
        }
        self.run(key, None, |store| store.load_secret(key))
    }

    fn delete_secret(&self, key: &SecretKey) -> SecretResult<()> {
        self.run(key, None, |store| {
            store.delete_secret(key)?;
            Ok(None)
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::SecretStorageMode;

    #[test]
    fn secret_storage_mode_preserves_stored_names() {
        assert_eq!(
            serde_json::to_string(&SecretStorageMode::ConfigFile)
                .expect("serialize config-file mode"),
            "\"config-file\""
        );
        assert_eq!(
            serde_json::from_str::<SecretStorageMode>("\"system-keyring\"")
                .expect("deserialize system-keyring mode"),
            SecretStorageMode::SystemKeyring
        );
        #[cfg(any(
            target_os = "android",
            target_os = "macos",
            target_os = "windows",
            all(unix, not(any(target_os = "android", target_vendor = "apple")))
        ))]
        assert_eq!(
            SecretStorageMode::default(),
            SecretStorageMode::SystemKeyring
        );
        #[cfg(not(any(
            target_os = "android",
            target_os = "macos",
            target_os = "windows",
            all(unix, not(any(target_os = "android", target_vendor = "apple")))
        )))]
        assert_eq!(SecretStorageMode::default(), SecretStorageMode::ConfigFile);
    }
}
