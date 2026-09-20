//! Shared preferences and coherent source logins. Local roots, hardware, downloads,
//! transport settings, and private device identity remain in this installation.
use super::*;
use library::ConnectRecord;
use serde_json::Value;

#[derive(Serialize, Deserialize)]
struct SharedSource {
    configuration: SourceConfiguration,
    music_folder_id: Option<String>,
    enable_half_stars: bool,
    credential: Option<String>,
}

fn shared_ui(settings: &UiSettings) -> Result<serde_json::Map<String, Value>, String> {
    let Value::Object(mut value) = serde_json::to_value(settings).map_err(|e| e.to_string())?
    else {
        unreachable!()
    };
    for key in [
        "connect",
        "web_controller",
        "backup",
        "downloads",
        "secret_storage_mode",
        "cast_proxy_enabled",
        "cast_network_interface",
        "window_width",
        "window_height",
        "release_notification_seen_version",
        "automatic_updates_enabled",
        "release_check_interval_hours",
        "tray_enabled",
        "keep_running_after_close",
        "start_minimized",
        "shuffle_enabled",
        "repeat_mode",
        "auto_dj_enabled",
        "lastfm_api_key",
    ] {
        value.remove(key);
    }
    if let Some(Value::Object(playback)) = value.get_mut("playback") {
        for key in ["audio_output", "stream_quality", "volume", "muted"] {
            playback.remove(key);
        }
    }
    Ok(value)
}

fn merge_ui(
    current: &UiSettings,
    patch: &serde_json::Map<String, Value>,
) -> Result<UiSettings, String> {
    let mut value = serde_json::to_value(current).map_err(|e| e.to_string())?;
    let allowed = shared_ui(current)?;
    for (key, incoming) in patch {
        if !allowed.contains_key(key) {
            continue;
        }
        if key == "playback" {
            let Some(incoming) = incoming.as_object() else {
                return Err("Connect playback preferences are invalid".into());
            };
            let Some(current) = value[key].as_object_mut() else {
                unreachable!()
            };
            for (key, value) in incoming {
                if !matches!(
                    key.as_str(),
                    "audio_output" | "stream_quality" | "volume" | "muted"
                ) {
                    current.insert(key.clone(), value.clone());
                }
            }
        } else {
            value[key] = incoming.clone();
        }
    }
    serde_json::from_value(value).map_err(|e| e.to_string())
}

fn shared_scrobbling(
    file: &SettingsFile,
    secrets: &Arc<SwitchableSecretStore>,
) -> Result<ScrobblingSettings, String> {
    let stored = file.load();
    let mut value = stored.scrobbling_runtime_settings();
    if stored.scrobbling_secrets_present {
        for descriptor in scrobbling::secret_descriptors() {
            if descriptor.value(&value).is_empty() {
                *descriptor.value_mut(&mut value) = secrets
                    .load_secret(&scrobbling_secret_key(*descriptor))
                    .map_err(|e| e.to_string())?
                    .unwrap_or_default();
            }
        }
    }
    Ok(value)
}

impl SettingsOwner {
    pub(crate) fn connect_records(
        &self,
        secrets: &Arc<SwitchableSecretStore>,
    ) -> Result<Vec<ConnectRecord>, String> {
        let stored = self.file.load();
        let mut records = Vec::new();
        for (kind, source) in stored
            .sources
            .configured
            .iter()
            .map(|s| ("source", s))
            .chain(
                stored
                    .sources
                    .integrations
                    .iter()
                    .map(|s| ("integration", s)),
            )
        {
            let mut configuration = source.configuration.clone();
            if configuration.kind == "local" {
                let mut payload: Value = serde_json::from_str(&configuration.provider_payload)
                    .map_err(|e| e.to_string())?;
                payload["roots"] = serde_json::json!([]);
                if let Some(payload) = payload.as_object_mut() {
                    payload.remove("base_url");
                    payload.remove("legacy_root");
                }
                configuration.provider_payload =
                    serde_json::to_string(&payload).map_err(|e| e.to_string())?;
            }
            let credential = source
                .credential_ref
                .as_ref()
                .map(|reference| load_provider_secret(secrets, reference))
                .transpose()?
                .flatten();
            let key = configuration.source_id.to_string();
            let value = serde_json::to_value(SharedSource {
                configuration,
                music_folder_id: source.music_folder_id.clone(),
                enable_half_stars: source.enable_half_stars,
                credential,
            })
            .map_err(|e| e.to_string())?;
            records.push(ConnectRecord {
                kind: kind.into(),
                key,
                value: Some(value),
            });
        }
        for (key, value) in shared_ui(&stored.ui)? {
            records.push(ConnectRecord {
                kind: "preference".into(),
                key,
                value: Some(value),
            });
        }
        let Value::Object(scrobbling) =
            serde_json::to_value(shared_scrobbling(&self.file, secrets)?)
                .map_err(|e| e.to_string())?
        else {
            unreachable!()
        };
        for (key, value) in scrobbling {
            records.push(ConnectRecord {
                kind: "scrobbling".into(),
                key,
                value: Some(value),
            });
        }
        Ok(records)
    }

    /// Returns whether sources changed so the existing source owner can reload them.
    pub(crate) fn apply_connect_records(
        &self,
        records: &[ConnectRecord],
        secrets: &Arc<SwitchableSecretStore>,
    ) -> Result<bool, String> {
        let previous = self.file.load();
        let mut sources = previous.sources.clone();
        let mut ui = serde_json::Map::new();
        let old_scrobbling = shared_scrobbling(&self.file, secrets)?;
        let mut scrobbling = serde_json::to_value(&old_scrobbling).map_err(|e| e.to_string())?;
        let mut scrobbling_changed = false;
        let mut credential_changed = false;
        let mut touched_sources = std::collections::BTreeSet::new();
        let mut touched_scrobbling = std::collections::BTreeSet::new();
        for record in records {
            match record.kind.as_str() {
                "source" | "integration" => {
                    touched_sources.insert((record.kind.clone(), record.key.clone()));
                    let configured = if record.kind == "integration" {
                        &mut sources.integrations
                    } else {
                        &mut sources.configured
                    };
                    let old = configured
                        .iter()
                        .find(|source| source.configuration.source_id.as_str() == record.key)
                        .cloned();
                    let Some(value) = &record.value else {
                        configured
                            .retain(|source| source.configuration.source_id.as_str() != record.key);
                        continue;
                    };
                    let mut incoming: SharedSource =
                        serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
                    if incoming.configuration.source_id.as_str() != record.key {
                        return Err(
                            "Connect source identity does not match its configuration".into()
                        );
                    }
                    if incoming.configuration.kind == "local" {
                        let mut payload: Value =
                            serde_json::from_str(&incoming.configuration.provider_payload)
                                .map_err(|e| e.to_string())?;
                        let roots = old
                            .as_ref()
                            .filter(|s| s.configuration.kind == "local")
                            .map(|s| {
                                serde_json::from_str::<Value>(&s.configuration.provider_payload)
                            })
                            .transpose()
                            .map_err(|e| e.to_string())?
                            .and_then(|p| p.get("roots").cloned())
                            .unwrap_or_else(|| serde_json::json!([]));
                        payload["roots"] = roots;
                        incoming.configuration.provider_payload =
                            serde_json::to_string(&payload).map_err(|e| e.to_string())?;
                    }
                    let old_secret = old
                        .as_ref()
                        .and_then(|s| s.credential_ref.as_ref())
                        .map(|reference| load_provider_secret(secrets, reference))
                        .transpose()?
                        .flatten();
                    let reference = if old_secret == incoming.credential {
                        old.as_ref().and_then(|s| s.credential_ref.clone())
                    } else if let Some(secret) = incoming.credential {
                        let reference = fresh_credential_ref()?;
                        save_provider_secret(secrets, &reference, secret)?;
                        credential_changed = true;
                        Some(reference)
                    } else {
                        credential_changed = true;
                        None
                    };
                    let next = ConfiguredSource {
                        configuration: incoming.configuration,
                        credential_ref: reference,
                        music_folder_id: incoming.music_folder_id,
                        local_access: old.as_ref().and_then(|s| s.local_access.clone()),
                        enable_half_stars: incoming.enable_half_stars,
                    };
                    if let Some(current) = configured
                        .iter_mut()
                        .find(|s| s.configuration.source_id == next.configuration.source_id)
                    {
                        *current = next;
                    } else {
                        configured.push(next);
                    }
                }
                "preference" => {
                    if let Some(value) = &record.value {
                        ui.insert(record.key.clone(), value.clone());
                    }
                }
                "scrobbling" => {
                    if let Some(value) = &record.value {
                        if scrobbling.get(&record.key) != Some(value) {
                            scrobbling[&record.key] = value.clone();
                            scrobbling_changed = true;
                            touched_scrobbling.insert(record.key.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        let sources_changed = sources.configured != previous.sources.configured;
        let integrations_changed = sources.integrations != previous.sources.integrations;
        if sources.selected_source_id.is_some()
            && !sources
                .configured
                .iter()
                .any(|s| Some(&s.configuration.source_id) == sources.selected_source_id.as_ref())
        {
            sources.selected_source_id = None;
        }
        let mut next_ui = merge_ui(&previous.ui, &ui)?;
        if !sources_changed
            && !integrations_changed
            && next_ui == previous.ui
            && !scrobbling_changed
        {
            return Ok(false);
        }
        let next_scrobbling = if scrobbling_changed {
            let mut incoming: ScrobblingSettings =
                serde_json::from_value(scrobbling).map_err(|e| e.to_string())?;
            incoming.sanitize();
            next_ui.lastfm_api_key = incoming.lastfm.api_key.clone();
            // Fixed service-secret keys are an existing owner boundary. Disable
            // submission before replacing them so a restart cannot send an old
            // account's pending work using an incoming account's credential.
            let lastfm_changed = old_scrobbling.lastfm.username != incoming.lastfm.username
                || old_scrobbling.lastfm.api_key != incoming.lastfm.api_key
                || old_scrobbling.lastfm.api_secret != incoming.lastfm.api_secret
                || old_scrobbling.lastfm.session_key != incoming.lastfm.session_key;
            let librefm_changed = old_scrobbling.librefm.username != incoming.librefm.username
                || old_scrobbling.librefm.session_key != incoming.librefm.session_key;
            let listenbrainz_changed =
                old_scrobbling.listenbrainz.user_token != incoming.listenbrainz.user_token;
            if lastfm_changed || librefm_changed || listenbrainz_changed {
                self.restore(
                    |current| {
                        if lastfm_changed {
                            current.scrobbling.lastfm.enabled = false;
                        }
                        if librefm_changed {
                            current.scrobbling.librefm.enabled = false;
                        }
                        if listenbrainz_changed {
                            current.scrobbling.listenbrainz.enabled = false;
                        }
                        Ok(())
                    },
                    false,
                )?;
            }
            // Use existing credential storage. The following SettingsOwner commit
            // makes the corresponding configuration active and notifies scrobbling.
            for descriptor in scrobbling::secret_descriptors()
                .iter()
                .filter(|descriptor| {
                    touched_scrobbling
                        .iter()
                        .any(|service| descriptor.kind().starts_with(service))
                })
            {
                let value = descriptor.value(&incoming);
                if value.is_empty() {
                    secrets
                        .delete_secret(&scrobbling_secret_key(*descriptor))
                        .map_err(|e| e.to_string())?;
                } else {
                    secrets
                        .save_secret(&scrobbling_secret_key(*descriptor), value)
                        .map_err(|e| e.to_string())?;
                }
            }
            let present = scrobbling_secrets_present(&incoming);
            for descriptor in scrobbling::secret_descriptors() {
                descriptor.value_mut(&mut incoming).clear();
            }
            incoming.lastfm.api_key.clear();
            Some((incoming, present))
        } else {
            None
        };
        self.restore(
            |current| {
                for (kind, source_id) in touched_sources {
                    let (configured, incoming_sources) = if kind == "integration" {
                        (&mut current.sources.integrations, &sources.integrations)
                    } else {
                        (&mut current.sources.configured, &sources.configured)
                    };
                    let old = configured
                        .iter()
                        .find(|source| source.configuration.source_id.as_str() == source_id)
                        .cloned();
                    let incoming = incoming_sources
                        .iter()
                        .find(|source| source.configuration.source_id.as_str() == source_id)
                        .cloned();
                    if let Some(mut incoming) = incoming {
                        incoming.local_access =
                            old.as_ref().and_then(|source| source.local_access.clone());
                        if incoming.configuration.kind == "local" {
                            let mut payload: Value =
                                serde_json::from_str(&incoming.configuration.provider_payload)
                                    .map_err(|e| e.to_string())?;
                            payload["roots"] = old
                                .as_ref()
                                .filter(|source| source.configuration.kind == "local")
                                .map(|source| {
                                    serde_json::from_str::<Value>(
                                        &source.configuration.provider_payload,
                                    )
                                })
                                .transpose()
                                .map_err(|e| e.to_string())?
                                .and_then(|payload| payload.get("roots").cloned())
                                .unwrap_or_else(|| serde_json::json!([]));
                            incoming.configuration.provider_payload =
                                serde_json::to_string(&payload).map_err(|e| e.to_string())?;
                        }
                        if let Some(existing) = configured
                            .iter_mut()
                            .find(|source| source.configuration.source_id.as_str() == source_id)
                        {
                            *existing = incoming;
                        } else {
                            configured.push(incoming);
                        }
                    } else {
                        configured
                            .retain(|source| source.configuration.source_id.as_str() != source_id);
                    }
                }
                if current
                    .sources
                    .selected_source_id
                    .as_ref()
                    .is_some_and(|id| {
                        !current
                            .sources
                            .configured
                            .iter()
                            .any(|source| &source.configuration.source_id == id)
                    })
                {
                    current.sources.selected_source_id = None;
                }
                current.ui = merge_ui(&current.ui, &ui)?;
                if let Some((incoming, present)) = next_scrobbling {
                    if touched_scrobbling.contains("lastfm") {
                        current.scrobbling.lastfm = incoming.lastfm;
                        current.ui.lastfm_api_key = next_ui.lastfm_api_key;
                    }
                    if touched_scrobbling.contains("librefm") {
                        current.scrobbling.librefm = incoming.librefm;
                    }
                    if touched_scrobbling.contains("listenbrainz") {
                        current.scrobbling.listenbrainz = incoming.listenbrainz;
                    }
                    current.scrobbling_secrets_present |= present;
                }
                Ok(())
            },
            credential_changed || scrobbling_changed,
        )?;
        Ok(sources_changed)
    }

    pub(crate) fn connect_replace_profile(
        &self,
        _secrets: &Arc<SwitchableSecretStore>,
    ) -> Result<(), String> {
        let defaults = shared_ui(&UiSettings::default())?;
        self.restore(
            |stored| {
                stored.ui = merge_ui(&stored.ui, &defaults)?;
                stored.ui.lastfm_api_key = UiSettings::default().lastfm_api_key;
                stored.sources = SourceSettings::default();
                stored.scrobbling = ScrobblingSettings::default();
                stored.scrobbling_secrets_present = false;
                Ok(())
            },
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ConcurrentEditStore {
        file: SettingsFile,
        edited: std::sync::atomic::AtomicBool,
    }
    impl SecretStore for ConcurrentEditStore {
        fn load_secret(&self, _: &SecretKey) -> secrets::SecretResult<Option<String>> {
            if !self.edited.swap(true, Ordering::SeqCst) {
                self.file
                    .update(|stored| {
                        stored.sources.configured[0].credential_ref =
                            Some(CredentialRef::new("new-credential"));
                        stored.ui.playback.audio_output = Some("new-output".into());
                        Ok(())
                    })
                    .unwrap();
            }
            Ok(None)
        }
        fn save_secret(&self, _: &SecretKey, _: &str) -> secrets::SecretResult<()> {
            Ok(())
        }
        fn delete_secret(&self, _: &SecretKey) -> secrets::SecretResult<()> {
            Ok(())
        }
    }

    #[test]
    fn file_connections_share_credentials_without_becoming_music_sources() {
        let sender = SettingsFile::memory();
        let secrets = Arc::new(SwitchableSecretStore::new(Arc::new(
            secrets::MemorySecretStore::new(),
        )));
        let reference = CredentialRef::new("file-login");
        save_provider_secret(&secrets, &reference, "saved-login".into()).unwrap();
        let connection = ConfiguredSource {
            configuration: SourceConfiguration {
                source_id: SourceId::new("files"),
                kind: "webdav".into(),
                name: "Files".into(),
                provider_payload: "{}".into(),
            },
            credential_ref: Some(reference),
            music_folder_id: None,
            local_access: None,
            enable_half_stars: false,
        };
        sender
            .update(|stored| {
                stored.sources.integrations.push(connection);
                Ok(())
            })
            .unwrap();
        let sender = SettingsOwner::new(sender, |_, _, _| {});
        let records = sender.connect_records(&secrets).unwrap();
        assert!(
            records
                .iter()
                .any(|r| r.kind == "integration" && r.key == "files")
        );
        assert!(!records.iter().any(|r| r.kind == "source"));
        let receiver_file = SettingsFile::memory();
        let receiver = SettingsOwner::new(receiver_file.clone(), |_, _, _| {});
        let receiver_secrets = Arc::new(SwitchableSecretStore::new(Arc::new(
            secrets::MemorySecretStore::new(),
        )));
        assert!(
            !receiver
                .apply_connect_records(&records, &receiver_secrets)
                .unwrap(),
            "a connection does not invalidate the music library"
        );
        let stored = receiver_file.load();
        assert!(stored.sources.configured.is_empty());
        assert!(stored.sources.selected_source_id.is_none());
        assert_eq!(stored.sources.integrations.len(), 1);
        assert_eq!(
            load_provider_secret(
                &receiver_secrets,
                stored.sources.integrations[0]
                    .credential_ref
                    .as_ref()
                    .unwrap()
            )
            .unwrap()
            .as_deref(),
            Some("saved-login")
        );
        assert!(
            !receiver
                .apply_connect_records(&records, &receiver_secrets)
                .unwrap()
        );
    }

    #[test]
    fn incoming_preference_preserves_source_and_hardware_edits_during_secret_reads() {
        let file = SettingsFile::memory();
        file.update(|stored| {
            stored.scrobbling_secrets_present = true;
            stored.sources.configured.push(ConfiguredSource {
                configuration: SourceConfiguration {
                    source_id: SourceId::new("server"),
                    kind: "jellyfin".into(),
                    name: "Server".into(),
                    provider_payload: "{}".into(),
                },
                credential_ref: Some(CredentialRef::new("old-credential")),
                music_folder_id: None,
                local_access: None,
                enable_half_stars: false,
            });
            Ok(())
        })
        .unwrap();
        let owner = SettingsOwner::new(file.clone(), |_, _, _| {});
        let secrets = Arc::new(SwitchableSecretStore::new(Arc::new(ConcurrentEditStore {
            file: file.clone(),
            edited: std::sync::atomic::AtomicBool::new(false),
        })));
        owner
            .apply_connect_records(
                &[ConnectRecord {
                    kind: "preference".into(),
                    key: "private_mode".into(),
                    value: Some(Value::Bool(true)),
                }],
                &secrets,
            )
            .unwrap();
        let stored = file.load();
        assert!(stored.ui.private_mode);
        assert_eq!(
            stored.sources.configured[0]
                .credential_ref
                .as_ref()
                .unwrap()
                .as_str(),
            "new-credential"
        );
        assert_eq!(
            stored.ui.playback.audio_output.as_deref(),
            Some("new-output")
        );
    }

    #[test]
    fn shared_settings_keep_native_roots_hardware_and_identity_and_apply_without_echo() {
        let directory = tempfile::tempdir().unwrap();
        let original_root = directory.path().join("original");
        let receiver_root = directory.path().join("receiver");
        let sender = SettingsFile::memory();
        sender
            .update(|stored| {
                stored.sources.configured.push(ConfiguredSource {
                    configuration: SourceConfiguration {
                        source_id: SourceId::new("music"),
                        kind: "local".into(),
                        name: "Music".into(),
                        provider_payload: serde_json::json!({"version":1,"roots":[original_root]})
                            .to_string(),
                    },
                    credential_ref: None,
                    music_folder_id: None,
                    local_access: None,
                    enable_half_stars: false,
                });
                stored.ui.private_mode = true;
                stored.ui.playback.audio_output = Some("sender-device".into());
                stored.ui.playback.stream_quality = StreamQuality::MaxBitrateKbps(128);
                Ok(())
            })
            .unwrap();
        let receiver = SettingsFile::memory();
        receiver
            .update(|stored| {
                stored.sources = sender.load().sources;
                stored.sources.configured[0].configuration.provider_payload =
                    serde_json::json!({"version":1,"roots":[receiver_root]}).to_string();
                stored.ui.playback.audio_output = Some("receiver-device".into());
                stored.ui.playback.stream_quality = StreamQuality::Original;
                stored.ui.random_play.limit = 200;
                stored.ui.lyrics.lyrics_font_family = Some("Custom font".into());
                stored.ui.queue_lyrics_height = Some(200);
                Ok(())
            })
            .unwrap();
        let device_identity = receiver.load().jellyfin_device_id;
        let observed = Arc::new(AtomicUsize::new(0));
        let callback = observed.clone();
        let sender = SettingsOwner::new(sender, |_, _, _| {});
        let receiver_owner = SettingsOwner::new(receiver.clone(), move |_, _, _| {
            callback.fetch_add(1, Ordering::Relaxed);
        });
        let secrets = Arc::new(SwitchableSecretStore::new(Arc::new(
            secrets::MemorySecretStore::new(),
        )));
        let records = sender.connect_records(&secrets).unwrap();
        let source = records
            .iter()
            .find(|r| r.kind == "source")
            .unwrap()
            .value
            .as_ref()
            .unwrap();
        let shared_payload: Value = serde_json::from_str(
            source["configuration"]["provider_payload"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(shared_payload["roots"], serde_json::json!([]));
        receiver_owner
            .apply_connect_records(&records, &secrets)
            .unwrap();
        let value = receiver.load();
        assert!(value.ui.private_mode);
        assert_eq!(
            value.ui.playback.audio_output.as_deref(),
            Some("receiver-device")
        );
        assert_eq!(value.ui.playback.stream_quality, StreamQuality::Original);
        assert_eq!(value.ui.random_play, RandomPlaySettings::default());
        assert!(value.ui.lyrics.lyrics_font_family.is_none());
        assert!(value.ui.queue_lyrics_height.is_none());
        assert_eq!(value.jellyfin_device_id, device_identity);
        assert_eq!(
            serde_json::from_str::<Value>(
                &value.sources.configured[0].configuration.provider_payload
            )
            .unwrap()["roots"],
            serde_json::json!([receiver_root])
        );
        let before = observed.load(Ordering::Relaxed);
        receiver_owner
            .apply_connect_records(&records, &secrets)
            .unwrap();
        assert_eq!(observed.load(Ordering::Relaxed), before);
    }
}
