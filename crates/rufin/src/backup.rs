use crate::{
    playback::PlaybackOwner,
    settings::{
        SettingsFile, SettingsUiPort, StoredSettings, backup_password_key, backup_password_store,
        fresh_credential_ref, provider_secret_key, scrobbling_secret_key,
    },
    source::SourceOwner,
};
use async_channel::Receiver;
use gio::prelude::*;
use playback::TransportCommandPort;
use secrets::{SecretStore, SwitchableSecretStore};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use ui::runtime::{BackupPort, BackupPreview};

#[derive(Default, Serialize, Deserialize)]
struct SavedLogins {
    sources: BTreeMap<sources::SourceId, Option<String>>,
    scrobbling: BTreeMap<String, Option<String>>,
    lastfm_username: String,
    lastfm_api_key: String,
    librefm_username: String,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct PlaylistPins {
    pins: Vec<ui::SidebarPin>,
    imported_sources: Vec<sources::SourceId>,
}

#[derive(Clone)]
pub(crate) struct BackupOwner {
    database: Arc<library::Database>,
    settings: SettingsFile,
    settings_apply: SettingsUiPort,
    secrets: Arc<SwitchableSecretStore>,
    source: Arc<SourceOwner>,
    playback: Arc<PlaybackOwner>,
    runtime: tokio::runtime::Handle,
    schedule_error: Arc<Mutex<Option<String>>>,
    lane: Arc<tokio::sync::Mutex<()>>,
}
impl BackupOwner {
    pub(crate) fn new(
        database: Arc<library::Database>,
        settings: SettingsFile,
        settings_apply: SettingsUiPort,
        secrets: Arc<SwitchableSecretStore>,
        source: Arc<SourceOwner>,
        playback: Arc<PlaybackOwner>,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        Arc::new(Self {
            database,
            settings,
            settings_apply,
            secrets,
            source,
            playback,
            runtime,
            schedule_error: Arc::new(Mutex::new(None)),
            lane: Arc::new(tokio::sync::Mutex::new(())),
        })
    }
    pub(crate) fn start(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.runtime.spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                let Some(owner) = weak.upgrade() else { return };
                let backup = owner.settings.load().ui.backup;
                if !backup.enabled || backup.schedule.due_at(now()).is_none() {
                    continue;
                }
                let status = Arc::clone(&owner.schedule_error);
                let runtime = owner.runtime.clone();
                let result =
                    tokio::task::spawn_blocking(move || runtime.block_on(owner.scheduled())).await;
                let error = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(error),
                    Err(error) => Some(error.to_string()),
                };
                if let Some(error) = &error {
                    tracing::warn!(%error, "scheduled backup failed");
                }
                *status.lock().unwrap_or_else(|p| p.into_inner()) = error;
            }
        });
    }
    async fn export_now(
        &self,
        passphrase: Option<String>,
        scheduled: bool,
    ) -> Result<tempfile::TempPath, String> {
        let stored = self.settings.load();
        if scheduled && stored.ui.backup.encrypt && passphrase.as_ref().is_none_or(|v| v.is_empty())
        {
            return Err("A password is required for encrypted backups".into());
        }
        let contents = stored.ui.backup.contents;
        let logins = if contents.saved_logins {
            let owner = self.clone();
            let snapshot = stored.clone();
            Some(
                tokio::task::spawn_blocking(move || {
                    saved_logins(&snapshot, owner.secrets.as_ref())
                        .and_then(|value| serde_json::to_vec(&value).map_err(|e| e.to_string()))
                })
                .await
                .map_err(|e| e.to_string())??,
            )
        } else {
            None
        };
        let settings = if contents.settings {
            let mut export = stored.clone();
            export.ui.downloads.clear();
            export.ui.sidebar.pins.clear();
            export.ui.sidebar.playlist_pin_imported_sources.clear();
            export.secret_scope_id.clear();
            export.scrobbling_secrets_present = false;
            for descriptor in scrobbling::secret_descriptors() {
                descriptor.value_mut(&mut export.scrobbling).clear();
            }
            Some(serde_json::to_vec_pretty(&export).map_err(|e| e.to_string())?)
        } else {
            None
        };
        let pins = if contents.playlists {
            Some(
                serde_json::to_vec(&PlaylistPins {
                    pins: stored.ui.sidebar.pins.clone(),
                    imported_sources: stored.ui.sidebar.playlist_pin_imported_sources.clone(),
                })
                .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        let mut output = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
        self.database
            .write_backup(
                &mut output,
                library::BackupOptions {
                    contents,
                    settings: settings.as_deref(),
                    saved_logins: logins.as_deref(),
                    playlist_pins: pins.as_deref(),
                    passphrase: passphrase.as_deref(),
                    schedule_id: scheduled
                        .then_some(stored.ui.backup.schedule.schedule_id.as_str()),
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        output.as_file().sync_all().map_err(|e| e.to_string())?;
        Ok(output.into_temp_path())
    }
    async fn scheduled(&self) -> Result<(), String> {
        let _lane = self.lane.lock().await;
        let settings = self.settings.load().ui.backup;
        if !settings.enabled || settings.schedule.due_at(now()).is_none() {
            return Ok(());
        }
        let directory = match settings.destination_uri.as_deref() {
            Some(uri) => gio::File::for_uri(uri),
            None => {
                let path = self.default_directory();
                fs::create_dir_all(&path).map_err(|e| e.to_string())?;
                gio::File::for_path(path)
            }
        };
        let passphrase = if settings.encrypt {
            let stored = self.settings.load();
            tokio::task::spawn_blocking(move || {
                backup_password_store(&stored)
                    .load_secret(&backup_password_key())
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| e.to_string())??
        } else {
            None
        };
        let output = self.export_now(passphrase, true).await?;
        let completed = now();
        let name = library::backup_filename(Some(&settings.schedule.schedule_id), completed);
        let destination = directory.child(&name);
        let pending = directory.child(format!(".{name}.partial"));
        let input = gio::File::for_path(&output)
            .read(None::<&gio::Cancellable>)
            .map_err(|e| e.to_string())?;
        let stream = pending
            .replace(
                None,
                false,
                gio::FileCreateFlags::PRIVATE | gio::FileCreateFlags::REPLACE_DESTINATION,
                None::<&gio::Cancellable>,
            )
            .map_err(|e| e.to_string())?;
        stream
            .splice(
                &input,
                gio::OutputStreamSpliceFlags::CLOSE_SOURCE
                    | gio::OutputStreamSpliceFlags::CLOSE_TARGET,
                None::<&gio::Cancellable>,
            )
            .map_err(|e| e.to_string())?;
        pending
            .move_(
                &destination,
                gio::FileCopyFlags::NONE,
                None::<&gio::Cancellable>,
                None,
            )
            .map_err(|e| e.to_string())?;
        self.settings.update(|stored| {
            if stored.ui.backup.schedule.schedule_id == settings.schedule.schedule_id {
                stored.ui.backup.schedule.last_successful_at = Some(completed);
            }
            Ok(())
        })?;
        prune_scheduled_backups(
            &directory,
            &settings.schedule.schedule_id,
            settings.retention_count,
        )
    }

    async fn restore_now(
        &self,
        preview: BackupPreview,
    ) -> Result<library::BackupRestoreReport, String> {
        let _lane = self.lane.lock().await;
        let contents = preview.contents.intersect(preview.staged.manifest.contents);
        let previous = self.settings.load();
        let incoming = if contents.settings {
            let mut incoming: StoredSettings =
                serde_json::from_slice(&preview.staged.settings().map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            incoming.migrate_defaults();
            if removed_sources(&previous, &incoming) != preview.removed_sources {
                return Err("Configured sources changed; open the backup again to review the sources being removed".into());
            }
            Some(incoming)
        } else {
            None
        };
        let logins: Option<SavedLogins> = if contents.saved_logins {
            Some(
                serde_json::from_slice(&preview.staged.saved_logins().map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        let pins = if contents.playlists {
            Some(
                serde_json::from_slice(&preview.staged.playlist_pins().map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        self.source
            .restore_user_state(contents.settings || contents.saved_logins, || async {
                if contents.queue {
                    let playback = Arc::clone(&self.playback);
                    tokio::task::spawn_blocking(move || playback.shutdown())
                        .await
                        .map_err(|e| e.to_string())?;
                }
                let result = self
                    .database
                    .restore_backup(&preview.staged, contents)
                    .await
                    .map_err(|e| e.to_string());
                if let Err(error) = result {
                    if contents.queue {
                        self.playback.start().await?;
                    }
                    return Err(error);
                }
                let mut report = result?;
                if contents.settings || contents.saved_logins || contents.playlists {
                    let owner = self.clone();
                    report.warnings = tokio::task::spawn_blocking(move || {
                        restore_preferences_and_logins(
                            &owner.settings,
                            &owner.settings_apply,
                            owner.secrets.as_ref(),
                            incoming,
                            pins,
                            logins,
                        )
                    })
                    .await
                    .map_err(|e| e.to_string())?;
                }
                if contents.queue {
                    if let Err(error) = self.playback.start().await {
                        report.warnings.push(format!(
                            "Queue restored; playback could not resume: {error}"
                        ));
                    }
                } else {
                    self.playback.catalog_changed();
                }
                Ok(report)
            })
            .await
    }
}
impl BackupPort for BackupOwner {
    fn default_directory(&self) -> std::path::PathBuf {
        crate::paths::data_dir().join("backups")
    }

    fn export(&self, passphrase: Option<String>) -> Receiver<Result<tempfile::TempPath, String>> {
        let (send, receive) = async_channel::bounded(1);
        let owner = self.clone();
        let runtime = self.runtime.clone();
        self.runtime.spawn_blocking(move || {
            let result = runtime.block_on(async {
                let _lane = owner.lane.lock().await;
                owner.export_now(passphrase, false).await
            });
            let _ = send.send_blocking(result);
        });
        receive
    }
    fn stage(
        &self,
        path: std::path::PathBuf,
        passphrase: Option<String>,
        contents: library::BackupContents,
    ) -> Receiver<Result<BackupPreview, String>> {
        let (send, receive) = async_channel::bounded(1);
        let settings = self.settings.clone();
        self.runtime.spawn_blocking(move || {
            let result = (|| {
                let input = fs::File::open(path).map_err(|e| e.to_string())?;
                let staged = library::stage_backup(input, passphrase.as_deref())
                    .map_err(|e| e.to_string())?;
                let removed_sources = if contents.settings && staged.manifest.contents.settings {
                    let incoming: StoredSettings =
                        serde_json::from_slice(&staged.settings().map_err(|e| e.to_string())?)
                            .map_err(|e| e.to_string())?;
                    removed_sources(&settings.load(), &incoming)
                } else {
                    Vec::new()
                };
                Ok(BackupPreview {
                    contents: contents.intersect(staged.manifest.contents),
                    staged: Arc::new(staged),
                    removed_sources,
                })
            })();
            let _ = send.send_blocking(result);
        });
        receive
    }
    fn restore(
        &self,
        preview: BackupPreview,
    ) -> Receiver<Result<library::BackupRestoreReport, String>> {
        let (send, receive) = async_channel::bounded(1);
        let owner = self.clone();
        let runtime = self.runtime.clone();
        self.runtime.spawn_blocking(move || {
            let result = runtime.block_on(owner.restore_now(preview));
            let _ = send.send_blocking(result);
        });
        receive
    }
    fn schedule_error(&self) -> Option<String> {
        self.schedule_error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    fn save_schedule_password(&self, passphrase: String) -> Receiver<Result<(), String>> {
        let (send, receive) = async_channel::bounded(1);
        let owner = self.clone();
        self.runtime.spawn_blocking(move || {
            let result = if passphrase.is_empty() {
                Err("The scheduled backup password cannot be empty".into())
            } else {
                backup_password_store(&owner.settings.load())
                    .save_secret(&backup_password_key(), &passphrase)
                    .map_err(|e| e.to_string())
            };
            let _ = send.send_blocking(result);
        });
        receive
    }
}
fn restore_preferences_and_logins(
    file: &SettingsFile,
    settings_apply: &SettingsUiPort,
    secrets: &dyn SecretStore,
    incoming: Option<StoredSettings>,
    pins: Option<PlaylistPins>,
    logins: Option<SavedLogins>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if (incoming.is_some() || pins.is_some())
        && let Err(error) = settings_apply.restore(
            |current| {
                if let Some(mut incoming) = incoming {
                    preserve_destination_settings(current, &mut incoming)?;
                    *current = incoming;
                }
                if let Some(pins) = pins {
                    current.ui.sidebar.pins = pins.pins;
                    current.ui.sidebar.playlist_pin_imported_sources = pins.imported_sources;
                }
                Ok(())
            },
            false,
        )
    {
        warnings.push(format!(
            "Database contents restored; settings could not be applied: {error}"
        ));
        return warnings;
    }
    if let Some(logins) = logins {
        let mut restored = file.load();
        restore_logins(secrets, &mut restored, logins, &mut warnings);
        if let Err(error) = settings_apply.restore(
            |current| {
                copy_scrobbling_logins(&restored, current);
                for source in &mut current.sources.configured {
                    if let Some(restored) = restored.sources.configured.iter().find(|restored| {
                        restored.configuration.source_id == source.configuration.source_id
                    }) {
                        source.credential_ref = restored.credential_ref.clone();
                    }
                }
                Ok(())
            },
            true,
        ) {
            warnings.push(format!(
                "Saved logins were written; settings could not be applied: {error}"
            ));
        }
    }
    warnings
}

fn copy_scrobbling_logins(previous: &StoredSettings, incoming: &mut StoredSettings) {
    incoming.scrobbling_secrets_present = previous.scrobbling_secrets_present;
    incoming.ui.lastfm_api_key = previous.ui.lastfm_api_key.clone();
    incoming.scrobbling.lastfm.username = previous.scrobbling.lastfm.username.clone();
    incoming.scrobbling.librefm.username = previous.scrobbling.librefm.username.clone();
    for descriptor in scrobbling::secret_descriptors() {
        *descriptor.value_mut(&mut incoming.scrobbling) =
            descriptor.value(&previous.scrobbling).to_owned();
    }
}

fn preserve_destination_settings(
    previous: &StoredSettings,
    incoming: &mut StoredSettings,
) -> Result<(), String> {
    incoming.secret_scope_id = previous.secret_scope_id.clone();
    incoming.ui.secret_storage_mode = previous.ui.secret_storage_mode;
    incoming.jellyfin_device_id = previous.jellyfin_device_id.clone();
    incoming.ui.downloads = previous.ui.downloads.clone();
    incoming.ui.sidebar.pins = previous.ui.sidebar.pins.clone();
    incoming.ui.sidebar.playlist_pin_imported_sources =
        previous.ui.sidebar.playlist_pin_imported_sources.clone();
    incoming.ui.backup.destination_uri = previous.ui.backup.destination_uri.clone();
    incoming.ui.backup.schedule.schedule_id = previous.ui.backup.schedule.schedule_id.clone();
    incoming.ui.backup.schedule.last_successful_at = previous.ui.backup.schedule.last_successful_at;
    for source in &mut incoming.sources.configured {
        source.credential_ref = match previous
            .sources
            .configured
            .iter()
            .find(|old| old.configuration.source_id == source.configuration.source_id)
        {
            Some(old) => old.credential_ref.clone(),
            None if source.credential_ref.is_some() => Some(fresh_credential_ref()?),
            None => None,
        };
    }
    copy_scrobbling_logins(previous, incoming);
    Ok(())
}
fn prune_scheduled_backups(
    directory: &gio::File,
    schedule_id: &str,
    retention_count: u32,
) -> Result<(), String> {
    let entries = directory
        .enumerate_children(
            "standard::name,standard::type",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            None::<&gio::Cancellable>,
        )
        .map_err(|e| e.to_string())?;
    let mut owned = Vec::new();
    while let Some(info) = entries
        .next_file(None::<&gio::Cancellable>)
        .map_err(|e| e.to_string())?
    {
        if info.file_type() != gio::FileType::Regular {
            continue;
        }
        let name = info.name();
        if let Some(timestamp) =
            library::scheduled_backup_timestamp(&name.to_string_lossy(), schedule_id)
        {
            owned.push((timestamp, name));
        }
    }
    owned.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, name) in owned.into_iter().skip(retention_count.max(1) as usize) {
        directory
            .child(name)
            .delete(None::<&gio::Cancellable>)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
fn removed_sources(previous: &StoredSettings, incoming: &StoredSettings) -> Vec<String> {
    previous
        .sources
        .configured
        .iter()
        .filter(|old| {
            !incoming
                .sources
                .configured
                .iter()
                .any(|new| new.configuration.source_id == old.configuration.source_id)
        })
        .map(|old| {
            format!(
                "{} ({})",
                old.configuration.name, old.configuration.source_id
            )
        })
        .collect()
}

fn saved_logins(stored: &StoredSettings, secrets: &dyn SecretStore) -> Result<SavedLogins, String> {
    let mut logins = SavedLogins {
        lastfm_username: stored.scrobbling.lastfm.username.clone(),
        lastfm_api_key: stored.ui.lastfm_api_key.clone(),
        librefm_username: stored.scrobbling.librefm.username.clone(),
        ..SavedLogins::default()
    };
    for source in &stored.sources.configured {
        let value = source
            .credential_ref
            .as_ref()
            .map(|reference| secrets.load_secret(&provider_secret_key(reference)))
            .transpose()
            .map_err(|e| e.to_string())?
            .flatten();
        logins
            .sources
            .insert(source.configuration.source_id.clone(), value);
    }
    for descriptor in scrobbling::secret_descriptors() {
        let inline = descriptor.value(&stored.scrobbling);
        let value = if !inline.is_empty() {
            Some(inline.to_owned())
        } else if stored.scrobbling_secrets_present {
            secrets
                .load_secret(&scrobbling_secret_key(*descriptor))
                .map_err(|e| e.to_string())?
        } else {
            None
        };
        logins.scrobbling.insert(
            format!("{}:{}", descriptor.namespace(), descriptor.kind()),
            value,
        );
    }
    Ok(logins)
}

fn restore_logins(
    secrets: &dyn SecretStore,
    incoming: &mut StoredSettings,
    logins: SavedLogins,
    warnings: &mut Vec<String>,
) {
    incoming.scrobbling.lastfm.username = logins.lastfm_username;
    incoming.ui.lastfm_api_key = logins.lastfm_api_key;
    incoming.scrobbling.librefm.username = logins.librefm_username;
    for (source_id, value) in logins.sources {
        let Some(source) = incoming
            .sources
            .configured
            .iter_mut()
            .find(|source| source.configuration.source_id == source_id)
        else {
            warnings.push(format!("Saved login has no configured source: {source_id}"));
            continue;
        };
        if source.credential_ref.is_none() && value.is_some() {
            match fresh_credential_ref() {
                Ok(reference) => source.credential_ref = Some(reference),
                Err(error) => {
                    warnings.push(error);
                    continue;
                }
            }
        }
        if let Some(reference) = &source.credential_ref {
            let key = provider_secret_key(reference);
            let result = match value {
                Some(value) => secrets.save_secret(&key, &value),
                None => secrets.delete_secret(&key),
            };
            if let Err(error) = result {
                warnings.push(format!(
                    "Could not restore login for {}: {error}",
                    source.configuration.name
                ));
            }
        }
    }
    for descriptor in scrobbling::secret_descriptors() {
        let name = format!("{}:{}", descriptor.namespace(), descriptor.kind());
        let Some(value) = logins.scrobbling.get(&name) else {
            continue;
        };
        let key = scrobbling_secret_key(*descriptor);
        let result = match value {
            Some(value) => secrets.save_secret(&key, value),
            None => secrets.delete_secret(&key),
        };
        match result {
            Ok(()) => {
                descriptor.value_mut(&mut incoming.scrobbling).clear();
            }
            Err(error) => warnings.push(format!("Could not restore {name}: {error}")),
        }
    }
    incoming.scrobbling_secrets_present = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{ConfiguredSource, CredentialRef};
    use secrets::{ConfigSecretStore, MemorySecretStore, SecretStorageMode};

    fn source(id: &str, reference: &str) -> ConfiguredSource {
        ConfiguredSource {
            configuration: sources::SourceConfiguration {
                source_id: sources::SourceId::new(id),
                kind: "subsonic".into(),
                name: "Same server".into(),
                provider_payload: "{}".into(),
            },
            credential_ref: Some(CredentialRef::new(reference)),
            music_folder_id: None,
            local_access: None,
            enable_half_stars: false,
        }
    }

    #[test]
    fn restore_applies_privacy_before_login_unlock_and_preserves_later_preferences() {
        use std::sync::mpsc;
        use std::time::Duration;

        struct HeldStore {
            inner: ConfigSecretStore,
            writing: mpsc::SyncSender<()>,
            resume: Mutex<mpsc::Receiver<()>>,
        }
        impl SecretStore for HeldStore {
            fn save_secret(
                &self,
                key: &secrets::SecretKey,
                value: &str,
            ) -> secrets::SecretResult<()> {
                self.writing.send(()).unwrap();
                self.resume
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                self.inner.save_secret(key, value)
            }
            fn load_secret(
                &self,
                key: &secrets::SecretKey,
            ) -> secrets::SecretResult<Option<String>> {
                self.inner.load_secret(key)
            }
            fn delete_secret(&self, key: &secrets::SecretKey) -> secrets::SecretResult<()> {
                self.inner.delete_secret(key)
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let file = SettingsFile::open(directory.path().join("settings.json")).unwrap();
        file.update(|current| {
            current.sources.configured = vec![source("exact", "destination-reference")];
            current.scrobbling.listenbrainz.enabled = true;
            Ok(())
        })
        .unwrap();
        let mut incoming = file.load();
        incoming.ui.private_mode = true;
        incoming.scrobbling.listenbrainz.enabled = false;
        let applied = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&applied);
        let settings_apply =
            (*SettingsUiPort::new(file.clone(), move |_, current, credentials| {
                observed.lock().unwrap().push((
                    current.ui.private_mode,
                    current.scrobbling.listenbrainz.enabled,
                    credentials,
                ));
            }))
            .clone();
        let (writing, written) = mpsc::sync_channel(1);
        let (resume, resumed) = mpsc::sync_channel(1);
        let store = HeldStore {
            inner: ConfigSecretStore::new(directory.path().join("secrets.json")),
            writing,
            resume: Mutex::new(resumed),
        };
        let mut logins = SavedLogins::default();
        logins.sources.insert(
            sources::SourceId::new("exact"),
            Some("restored-login".into()),
        );
        let restore_file = file.clone();
        let restore_apply = settings_apply.clone();
        let restored = std::thread::spawn(move || {
            restore_preferences_and_logins(
                &restore_file,
                &restore_apply,
                &store,
                Some(incoming),
                None,
                Some(logins),
            )
        });
        written.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(file.load().ui.private_mode);
        assert!(!file.load().scrobbling.listenbrainz.enabled);
        assert_eq!(*applied.lock().unwrap(), vec![(true, false, false)]);
        settings_apply
            .restore(
                |current| {
                    current.ui.private_mode = false;
                    current.scrobbling.listenbrainz.enabled = true;
                    Ok(())
                },
                false,
            )
            .unwrap();
        resume.send(()).unwrap();
        assert!(restored.join().unwrap().is_empty());
        assert!(!file.load().ui.private_mode);
        assert!(file.load().scrobbling.listenbrainz.enabled);
        assert_eq!(
            *applied.lock().unwrap(),
            vec![
                (true, false, false),
                (false, true, false),
                (false, true, true),
            ]
        );
    }

    #[test]
    fn scheduled_retention_keeps_configured_count_and_unrelated_files() {
        for retention_count in [1, 2, 3, 6] {
            let directory = tempfile::tempdir().unwrap();
            for timestamp in 1..=4 {
                fs::write(
                    directory
                        .path()
                        .join(library::backup_filename(Some("mine"), timestamp)),
                    [],
                )
                .unwrap();
            }
            let foreign = library::backup_filename(Some("other"), 1);
            let manual = library::backup_filename(None, 1);
            let incomplete = format!(".{}.partial", library::backup_filename(Some("mine"), 5));
            for name in [&foreign, &manual, &incomplete] {
                fs::write(directory.path().join(name), []).unwrap();
            }
            prune_scheduled_backups(
                &gio::File::for_path(directory.path()),
                "mine",
                retention_count,
            )
            .unwrap();
            for timestamp in 1..=4 {
                assert_eq!(
                    directory
                        .path()
                        .join(library::backup_filename(Some("mine"), timestamp))
                        .exists(),
                    timestamp > 4 - i64::from(retention_count)
                );
            }
            for name in [foreign, manual, incomplete] {
                assert!(directory.path().join(name).exists());
            }
        }
    }

    #[test]
    fn setup_restore_retains_destination_namespace_and_exact_source_credentials() {
        let mut previous = StoredSettings::default();
        previous.secret_scope_id = "destination".into();
        previous.ui.secret_storage_mode = SecretStorageMode::ConfigFile;
        previous.sources.configured = vec![source("exact", "destination-reference")];
        previous.scrobbling.lastfm.session_key = "destination-session".into();
        previous.ui.lastfm_api_key = "destination-api-key".into();
        previous.ui.sidebar.pins = vec![ui::SidebarPin::Playlist {
            source_id: None,
            playlist_id: "keep".into(),
        }];
        let mut incoming = StoredSettings::default();
        incoming.secret_scope_id = "archive".into();
        incoming.sources.configured = vec![
            source("exact", "archive-reference"),
            source("different-account", "archive-other"),
        ];
        preserve_destination_settings(&previous, &mut incoming).unwrap();
        assert_eq!(incoming.secret_scope_id, "destination");
        assert_eq!(
            incoming.ui.secret_storage_mode,
            SecretStorageMode::ConfigFile
        );
        assert_eq!(
            incoming.sources.configured[0].credential_ref,
            previous.sources.configured[0].credential_ref
        );
        assert_ne!(
            incoming.sources.configured[1]
                .credential_ref
                .as_ref()
                .unwrap()
                .as_str(),
            "archive-other"
        );
        assert_eq!(
            incoming.scrobbling.lastfm.session_key,
            "destination-session"
        );
        assert_eq!(incoming.ui.lastfm_api_key, "destination-api-key");
        assert_eq!(incoming.ui.sidebar.pins, previous.ui.sidebar.pins);
    }

    #[test]
    fn known_login_export_excludes_backup_password_and_unrelated_secrets() {
        let directory = tempfile::tempdir().unwrap();
        let credentials =
            ConfigSecretStore::with_scope(directory.path().join("secrets.json"), "installation");
        let password_path = directory.path().join("backup-password.json");
        ConfigSecretStore::with_scope(password_path.clone(), "installation")
            .save_secret(&backup_password_key(), "scheduled-password")
            .unwrap();
        let password = ConfigSecretStore::with_scope(password_path, "installation")
            .load_secret(&backup_password_key())
            .unwrap();
        assert_eq!(password.as_deref(), Some("scheduled-password"));
        let mut settings = StoredSettings::default();
        settings.sources.configured = vec![source("source", "reference")];
        settings.scrobbling_secrets_present = true;
        credentials
            .save_secret(
                &provider_secret_key(&CredentialRef::new("reference")),
                "provider-login",
            )
            .unwrap();
        credentials
            .save_secret(&backup_password_key(), "keyring-scheduled-password")
            .unwrap();
        credentials
            .save_secret(
                &secrets::SecretKey::namespaced("other", "account", "Other"),
                "unrelated-secret",
            )
            .unwrap();
        let logins = saved_logins(&settings, &credentials).unwrap();
        assert_eq!(
            logins.sources[&sources::SourceId::new("source")].as_deref(),
            Some("provider-login")
        );
        let bytes = serde_json::to_string(&logins).unwrap();
        assert!(!bytes.contains("scheduled-password"));
        assert!(!bytes.contains("unrelated-secret"));
        assert!(!bytes.contains("reference"));
    }

    #[test]
    fn saved_logins_restore_exact_source_association_without_matching_another_account() {
        let credentials = MemorySecretStore::new();
        let mut incoming = StoredSettings::default();
        incoming.sources.configured = vec![source("exact", "destination-reference")];
        let mut logins = SavedLogins::default();
        logins.sources.insert(
            sources::SourceId::new("exact"),
            Some("correct-login".into()),
        );
        logins.sources.insert(
            sources::SourceId::new("absent-account"),
            Some("wrong-login".into()),
        );
        let mut warnings = Vec::new();
        restore_logins(&credentials, &mut incoming, logins, &mut warnings);
        assert_eq!(
            credentials
                .load_token("destination-reference")
                .unwrap()
                .as_deref(),
            Some("correct-login")
        );
        assert!(credentials.load_token("absent-account").unwrap().is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("absent-account"));
    }
}
