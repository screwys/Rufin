//! Clears application storage after the host has stopped its workers.
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{SettingsOwner, StoredSettings};
use crate::paths::Paths;

#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageReset {
    KeepCredentials,
    AllData,
}

pub enum StorageResetRequest {
    Database,
    Storage {
        reset: StorageReset,
        stored: Box<StoredSettings>,
        connect: Box<crate::connect::ConnectSettings>,
        secrets: Arc<dyn secrets::SecretStore>,
    },
}

impl SettingsOwner {
    pub(crate) fn prepare_storage_reset(
        &self,
        reset: StorageReset,
        connect: crate::connect::ConnectSettings,
    ) -> Result<StorageResetRequest, String> {
        let stored = self.file.load();
        let secrets = if reset == StorageReset::AllData
            && stored.ui.application.secret_storage_mode
                == secrets::SecretStorageMode::SystemKeyring
        {
            // A reset must report a locked keyring instead of accepting the session fallback.
            super::system_keyring_secret_store(&stored.secret_scope_id)
        } else {
            self.secrets.current().map_err(error)?
        };
        Ok(StorageResetRequest::Storage {
            reset,
            stored: Box::new(stored),
            connect: Box::new(connect),
            secrets,
        })
    }
}

impl StorageResetRequest {
    /// The host must stop playback, close the Store and stop its worker runtime first.
    pub fn apply(self, paths: &Paths) -> Result<(), String> {
        let Self::Storage {
            reset,
            stored,
            mut connect,
            secrets,
        } = self
        else {
            validate_root(&paths.data_dir())?;
            validate_root(&paths.cache_dir())?;
            for path in [
                paths.store_file(),
                paths.legacy_store_file(),
                paths.catalog_file(),
            ] {
                for suffix in ["", "-wal", "-shm"] {
                    let mut file = path.as_os_str().to_os_string();
                    file.push(suffix);
                    match fs::remove_file(file) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(cause) => return Err(error(cause)),
                    }
                }
            }
            return Ok(());
        };
        let roots = [
            paths.cache_dir(),
            paths.data_dir(),
            paths.state_dir(),
            paths.config_dir(),
        ];
        for root in &roots {
            validate_root(root)?;
        }
        // Keep connection records until deletion succeeds so an I/O error does not lose them.
        let mut keep = vec![paths.settings_file()];
        if reset == StorageReset::KeepCredentials {
            keep.extend(
                ["secrets.json", "backup-password.json"].map(|name| paths.config_dir().join(name)),
            );
        }
        let retained = if reset == StorageReset::KeepCredentials {
            retained_connections(*stored)
        } else {
            let mut keys = vec![
                super::settings_encryption_key(),
                super::backup_password_key(),
                super::web_controller::secret_key(),
            ];
            keys.extend(
                stored
                    .sources
                    .connections()
                    .filter_map(|source| source.credential.as_ref())
                    .filter(|credential| matches!(credential, super::Credential::Legacy(_)))
                    .map(super::provider_secret_key),
            );
            keys.extend(
                stored
                    .scrobbling_credentials
                    .values()
                    .filter(|credential| matches!(credential, super::Credential::Legacy(_)))
                    .map(super::provider_secret_key),
            );
            keys.extend(
                scrobbling::secret_descriptors()
                    .iter()
                    .copied()
                    .map(super::scrobbling_secret_key),
            );
            keys.extend(crate::connect::secret_keys(&connect));
            for key in keys {
                secrets.delete_secret(&key).map_err(error)?;
            }
            StoredSettings::default()
        };
        ::lyrics::release_japanese_reader();
        for root in roots {
            clear_directory(&root, &keep)?;
        }
        if reset == StorageReset::KeepCredentials {
            connect.pending_import = None;
            crate::connect::write_device_config(&paths.connect_dir(), &connect)?;
        }
        fs::create_dir_all(paths.config_dir()).map_err(error)?;
        // Replace the whole document, including settings unknown to this version.
        super::write_private(
            &paths.settings_file(),
            &serde_json::to_vec_pretty(&super::persistent_settings(&retained)).map_err(error)?,
        )
    }
}

fn retained_connections(previous: StoredSettings) -> StoredSettings {
    let mut next = StoredSettings {
        sources: previous.sources,
        scrobbling: previous.scrobbling,
        scrobbling_secrets_present: previous.scrobbling_secrets_present,
        scrobbling_credentials: previous.scrobbling_credentials,
        secret_scope_id: previous.secret_scope_id,
        jellyfin_device_id: previous.jellyfin_device_id,
        ..StoredSettings::default()
    };
    next.ui.application.secret_storage_mode = previous.ui.application.secret_storage_mode;
    next.ui.lastfm_api_key = previous.ui.lastfm_api_key;
    next
}

fn validate_root(root: &Path) -> Result<(), String> {
    if !root.is_absolute() || root.parent().is_none() {
        return Err("Rufin's application data directory is unavailable".into());
    }
    Ok(())
}

fn clear_directory(directory: &Path, keep: &[PathBuf]) -> Result<(), String> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(cause) => return Err(error(cause)),
    };
    for entry in entries {
        let entry = entry.map_err(error)?;
        let path = entry.path();
        if keep.contains(&path) {
            continue;
        }
        if keep.iter().any(|kept| kept.starts_with(&path)) {
            clear_directory(&path, keep)?;
        } else if entry.file_type().map_err(error)?.is_dir() {
            fs::remove_dir_all(path).map_err(error)?;
        } else {
            fs::remove_file(path).map_err(error)?;
        }
    }
    Ok(())
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
