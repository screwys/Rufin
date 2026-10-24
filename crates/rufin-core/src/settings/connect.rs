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
        "download_artwork_automatically",
        "download_lyrics_automatically",
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SharedSettings {
    #[serde(default)]
    actor: u64,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    stamps: std::collections::BTreeMap<String, library::RecordStamp>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    unknown: std::collections::BTreeMap<String, Credential>,
}

impl SharedSettings {
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }
    pub(super) fn remove_session_credentials(&mut self) {
        self.unknown.retain(|_, credential| !credential.session());
    }

    pub(super) fn persist_credentials(&mut self) {
        for credential in self.unknown.values_mut() {
            if let Credential::Sealed { session, .. } = credential {
                *session = false;
            }
        }
    }
}

fn cell_key(kind: &str, key: &str) -> String {
    format!("{kind}:{key}")
}
fn shared_preference(key: &str) -> bool {
    matches!(
        key,
        "prefer_distinct_track_covers" | "external_metadata_enabled"
    )
}
fn portable_configuration(
    configuration: &SourceConfiguration,
) -> Result<SourceConfiguration, String> {
    let mut configuration = configuration.clone();
    if configuration.is_local() {
        let mut payload: Value = serde_json::from_str(&configuration.provider_payload)
            .map_err(|error| error.to_string())?;
        for field in ["roots", "document_roots", "excluded_folders"] {
            payload[field] = serde_json::json!([]);
        }
        if let Some(fields) = payload.as_object_mut() {
            fields.remove("base_url");
            fields.remove("legacy_root");
        }
        configuration.provider_payload =
            serde_json::to_string(&payload).map_err(|error| error.to_string())?;
    }
    Ok(configuration)
}
fn source_value(
    source: &ConfiguredSource,
    secrets: Option<&dyn SecretStore>,
) -> Result<Value, String> {
    let credential = match secrets {
        Some(secrets) => serde_json::to_value(
            source
                .credential
                .as_ref()
                .map(|credential| load_provider_secret(secrets, credential))
                .transpose()?
                .flatten(),
        ),
        None => serde_json::to_value(&source.credential),
    }
    .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({
        "configuration": portable_configuration(&source.configuration)?,
        "music_folder_id": source.music_folder_id,
        "enable_half_stars": source.enable_half_stars,
        "credential": credential,
    }))
}
fn cells(
    stored: &StoredSettings,
    secrets: Option<&dyn SecretStore>,
    initial: bool,
) -> Result<std::collections::BTreeMap<String, Value>, String> {
    let mut cells = std::collections::BTreeMap::new();
    for (kind, sources) in [
        ("source", &stored.sources.configured),
        ("integration", &stored.sources.integrations),
    ] {
        for source in sources {
            cells.insert(
                cell_key(kind, source.configuration.source_id.as_str()),
                source_value(source, secrets)?,
            );
        }
    }
    for (key, value) in shared_ui(&stored.ui)? {
        if initial || shared_preference(&key) {
            cells.insert(cell_key("preference", &key), value);
        }
    }
    let scrobbling = match secrets {
        Some(secrets) => serde_json::to_value(read_scrobbling(stored, secrets)?),
        None => {
            let mut value = serde_json::to_value(stored.scrobbling_runtime_settings())
                .map_err(|error| error.to_string())?;
            for descriptor in scrobbling::secret_descriptors() {
                let service = descriptor.kind().split('-').next().unwrap();
                let field = match descriptor.kind() {
                    "lastfm-api-secret" => "api_secret",
                    "lastfm-session" | "librefm-session" => "session_key",
                    _ => "user_token",
                };
                value[service][field] =
                    serde_json::to_value(stored.scrobbling_credentials.get(descriptor.kind()))
                        .map_err(|error| error.to_string())?;
            }
            Ok(value)
        }
    }
    .map_err(|error| error.to_string())?;
    for (key, value) in scrobbling.as_object().unwrap() {
        cells.insert(cell_key("scrobbling", key), value.clone());
    }
    Ok(cells)
}

pub(super) fn stamp_changes(
    previous: &StoredSettings,
    next: &mut StoredSettings,
) -> Result<(), String> {
    let replacing = previous.shared_settings.actor != 0 && next.shared_settings.actor == 0;
    if next.shared_settings.actor == 0 {
        let mut actor = [0; 8];
        getrandom::fill(&mut actor).map_err(|error| error.to_string())?;
        next.shared_settings.actor = u64::from_le_bytes(actor).max(1);
    }
    let before = if replacing {
        std::collections::BTreeMap::new()
    } else {
        cells(previous, None, false)?
    };
    let after = cells(next, None, false)?;
    let mut changed = false;
    for key in before
        .keys()
        .chain(after.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if !next.shared_settings.stamps.contains_key(key)
            || (!replacing
                && before.get(key) != after.get(key)
                && next.shared_settings.stamps.get(key) == previous.shared_settings.stamps.get(key))
        {
            let stamp = next
                .shared_settings
                .stamps
                .values()
                .copied()
                .max()
                .unwrap_or_default()
                .next(next.shared_settings.actor);
            next.shared_settings.stamps.insert(key.clone(), stamp);
            changed = true;
        }
    }
    if replacing || changed || next.shared_settings.stamps != previous.shared_settings.stamps {
        next.shared_settings.revision = previous.shared_settings.revision.saturating_add(1);
    }
    Ok(())
}

fn unknown_fields(incoming: &Value, known: &Value) -> Option<Value> {
    let (Some(incoming), Some(known)) = (incoming.as_object(), known.as_object()) else {
        return None;
    };
    let mut unknown = serde_json::Map::new();
    for (key, value) in incoming {
        match known.get(key) {
            None => {
                unknown.insert(key.clone(), value.clone());
            }
            Some(known) => {
                if let Some(value) = unknown_fields(value, known) {
                    unknown.insert(key.clone(), value);
                }
            }
        }
    }
    (!unknown.is_empty()).then_some(Value::Object(unknown))
}
fn add_unknown(value: &mut Value, unknown: &Value) {
    if let (Some(value), Some(unknown)) = (value.as_object_mut(), unknown.as_object()) {
        for (key, incoming) in unknown {
            if let Some(value) = value.get_mut(key) {
                add_unknown(value, incoming);
            } else {
                value.insert(key.clone(), incoming.clone());
            }
        }
    }
}

impl SettingsOwner {
    pub(crate) fn shared_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.file.shared.subscribe()
    }

    pub(crate) fn shared_revision(&self) -> u64 {
        self.file.load().shared_settings.revision
    }

    pub(crate) fn shared_records(&self) -> Result<Vec<library::StampedRecord>, String> {
        // Existing installations acquire a baseline once, in their normal atomic save.
        self.file.update(|_| Ok(()))?;
        let stored = self.file.load();
        let mut current = cells(&stored, Some(self.secrets.as_ref()), false)?;
        for (key, unknown) in &stored.shared_settings.unknown {
            let Some(unknown) = load_provider_secret(self.secrets.as_ref(), unknown)? else {
                continue;
            };
            let unknown: Value =
                serde_json::from_str(&unknown).map_err(|error| error.to_string())?;
            match current.get_mut(key) {
                Some(value) => add_unknown(value, &unknown),
                None => {
                    current.insert(key.clone(), unknown);
                }
            }
        }
        Ok(stored
            .shared_settings
            .stamps
            .iter()
            .map(|(name, stamp)| {
                let (kind, key) = name.split_once(':').unwrap();
                library::StampedRecord {
                    record: ConnectRecord {
                        kind: kind.to_owned(),
                        key: key.to_owned(),
                        value: current.remove(name),
                    },
                    stamp: *stamp,
                }
            })
            .collect())
    }

    pub(crate) fn merge_shared(
        &self,
        records: &[library::StampedRecord],
        personal: Option<&[ConnectRecord]>,
    ) -> Result<bool, String> {
        let initial = personal.is_some();
        let previous = self.file.load();
        let accepted = records
            .iter()
            .filter(|incoming| {
                let record = &incoming.record;
                (record.kind != "preference" || initial || shared_preference(&record.key))
                    && (initial
                        || previous
                            .shared_settings
                            .stamps
                            .get(&cell_key(&record.kind, &record.key))
                            .is_none_or(|stamp| stamp < &incoming.stamp))
            })
            .collect::<Vec<_>>();
        if accepted.is_empty() && !initial {
            return Ok(false);
        }
        // Prepare only accepted cells. Keyring work stays outside the save lock.
        let prepared = accepted
            .iter()
            .map(|incoming| {
                let record = &incoming.record;
                let (cell, canonical) = prepare_cell(record, &previous, self.secrets.as_ref())?;
                let unknown = record.value.as_ref().and_then(|value| match canonical {
                    Some(known) => unknown_fields(value, &known),
                    None => Some(value.clone()),
                });
                let unknown = unknown
                    .filter(|_| record.kind != "preference" || shared_preference(&record.key))
                    .map(|unknown| {
                        seal_credential(
                            self.secrets.as_ref(),
                            &serde_json::to_string(&unknown).map_err(|error| error.to_string())?,
                        )
                    })
                    .transpose()?;
                Ok((incoming, cell, unknown))
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.file.update(|stored| {
            let mut preferences = if initial {
                for (kind, sources) in [
                    ("source", &mut stored.sources.configured),
                    ("integration", &mut stored.sources.integrations),
                ] {
                    sources.retain(|source| {
                        accepted.iter().any(|incoming| {
                            incoming.record.kind == kind
                                && incoming.record.key == source.configuration.source_id.as_str()
                                && incoming.record.value.is_some()
                        })
                    });
                }
                stored.shared_settings = SharedSettings::default();
                stored.scrobbling = ScrobblingSettings::default();
                stored.scrobbling_credentials.clear();
                stored.scrobbling_secrets_present = false;
                stored.ui.lastfm_api_key.clear();
                shared_ui(&UiSettings::default())?
            } else {
                serde_json::Map::new()
            };
            for (incoming, cell, unknown) in prepared {
                let record = &incoming.record;
                let name = cell_key(&record.kind, &record.key);
                if !initial
                    && stored
                        .shared_settings
                        .stamps
                        .get(&name)
                        .is_some_and(|stamp| stamp >= &incoming.stamp)
                {
                    continue;
                }
                match cell {
                    PreparedCell::Source(mut next) => {
                        let sources = if record.kind == "integration" {
                            &mut stored.sources.integrations
                        } else {
                            &mut stored.sources.configured
                        };
                        let position = sources.iter().position(|source| {
                            source.configuration.source_id.as_str() == record.key
                        });
                        if let Some(next) = &mut next {
                            preserve_access(next, position.map(|position| &sources[position]))?;
                        }
                        match (position, next) {
                            (Some(position), Some(next)) => sources[position] = next,
                            (Some(position), None) => {
                                sources.remove(position);
                            }
                            (None, Some(next)) => {
                                if next.configuration.is_local()
                                    && !stored.ui.downloads.iter().any(|entry| {
                                        entry.source_id == next.configuration.source_id
                                    })
                                {
                                    stored.ui.set_download_quality(
                                        next.configuration.source_id.clone(),
                                        sources::DownloadQuality::Transcoded(
                                            library::AudioEncoding {
                                                codec: library::AudioCodec::Opus,
                                                bitrate_kbps: Some(128),
                                                channels: library::AudioChannels::Preserve,
                                            },
                                        ),
                                    );
                                }
                                sources.push(next);
                            }
                            (None, None) => {}
                        }
                    }
                    PreparedCell::Preference(value) => {
                        preferences.insert(record.key.clone(), value);
                    }
                    PreparedCell::Scrobbling {
                        value,
                        credentials,
                        api_key,
                    } => {
                        let mut current = serde_json::to_value(&stored.scrobbling)
                            .map_err(|error| error.to_string())?;
                        current[&record.key] = value;
                        stored.scrobbling =
                            serde_json::from_value(current).map_err(|error| error.to_string())?;
                        for (kind, credential) in credentials {
                            stored.scrobbling_credentials.remove(kind);
                            if let Some(credential) = credential {
                                stored
                                    .scrobbling_credentials
                                    .insert(kind.to_owned(), credential);
                            }
                        }
                        if let Some(api_key) = api_key {
                            stored.ui.lastfm_api_key = api_key;
                        }
                    }
                    PreparedCell::Unknown => {}
                }
                if record.kind != "preference" || shared_preference(&record.key) {
                    stored
                        .shared_settings
                        .stamps
                        .insert(name.clone(), incoming.stamp);
                    stored.shared_settings.unknown.remove(&name);
                    if let Some(unknown) = unknown {
                        stored.shared_settings.unknown.insert(name, unknown);
                    }
                }
            }
            if let Some(personal) = personal {
                for record in personal
                    .iter()
                    .filter(|record| record.kind == "preference" && !shared_preference(&record.key))
                {
                    if let Some(value) = &record.value {
                        preferences.insert(record.key.clone(), value.clone());
                    }
                }
            }
            stored.ui = merge_ui(&stored.ui, &preferences)?;
            stored.migrate_defaults();
            if stored
                .sources
                .selected_source_id
                .as_ref()
                .is_some_and(|id| {
                    !stored
                        .sources
                        .configured
                        .iter()
                        .any(|source| &source.configuration.source_id == id)
                })
            {
                stored.sources.selected_source_id = None;
            }
            Ok(())
        })?;
        let current = self.file.load();
        let source_changed = previous.sources.configured != current.sources.configured;
        let credentials_changed = previous.scrobbling_credentials != current.scrobbling_credentials
            || previous.sources != current.sources;
        if previous.ui != current.ui
            || previous.sources != current.sources
            || previous.scrobbling != current.scrobbling
            || credentials_changed
        {
            (self.on_change)(&previous, &current, credentials_changed);
        }
        Ok(source_changed)
    }

    pub(crate) fn migrate_legacy_settings(
        &self,
        records: &[library::StampedRecord],
        previous: &[ConnectRecord],
    ) -> Result<(), String> {
        if records.is_empty() {
            return Ok(());
        }
        self.file.update(|_| Ok(()))?;
        let mut incoming_stamp = library::RecordStamp::default();
        self.file.update(|stored| {
            let current = cells(stored, None, false)?;
            let baseline = records
                .iter()
                .map(|record| record.stamp)
                .chain(stored.shared_settings.stamps.values().copied())
                .max()
                .unwrap_or_default()
                .next(stored.shared_settings.actor);
            for name in current.keys().cloned().chain(
                records
                    .iter()
                    .filter(|incoming| {
                        incoming.record.kind != "preference"
                            || shared_preference(&incoming.record.key)
                    })
                    .map(|incoming| cell_key(&incoming.record.kind, &incoming.record.key)),
            ) {
                stored.shared_settings.stamps.insert(name, baseline);
            }
            incoming_stamp = baseline.next(stored.shared_settings.actor);
            Ok(())
        })?;
        let current = self
            .shared_records()?
            .into_iter()
            .map(|value| {
                (
                    cell_key(&value.record.kind, &value.record.key),
                    value.record.value,
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let previous = previous
            .iter()
            .map(|record| (cell_key(&record.kind, &record.key), record.value.as_ref()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let accepted = records
            .iter()
            .filter(|incoming| {
                let record = &incoming.record;
                let name = cell_key(&record.kind, &record.key);
                (record.kind != "preference" || shared_preference(&record.key))
                    && current.get(&name).and_then(Option::as_ref)
                        == previous.get(&name).copied().flatten()
            })
            .map(|incoming| library::StampedRecord {
                record: incoming.record.clone(),
                stamp: incoming_stamp,
            })
            .collect::<Vec<_>>();
        self.merge_shared(&accepted, None)?;
        Ok(())
    }

    pub(crate) fn initial_preferences(&self) -> Result<Vec<ConnectRecord>, String> {
        Ok(shared_ui(&self.file.load().ui)?
            .into_iter()
            .filter(|(key, _)| !shared_preference(key))
            .map(|(key, value)| ConnectRecord {
                kind: "preference".into(),
                key,
                value: Some(value),
            })
            .collect())
    }
}

enum PreparedCell {
    Source(Option<ConfiguredSource>),
    Preference(Value),
    Scrobbling {
        value: Value,
        credentials: Vec<(&'static str, Option<Credential>)>,
        api_key: Option<String>,
    },
    Unknown,
}

fn prepare_cell(
    record: &ConnectRecord,
    stored: &StoredSettings,
    secrets: &dyn SecretStore,
) -> Result<(PreparedCell, Option<Value>), String> {
    let canonical;
    let cell = match record.kind.as_str() {
        "source" | "integration" => {
            let Some(value) = &record.value else {
                return Ok((PreparedCell::Source(None), None));
            };
            let incoming: SharedSource =
                serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
            if incoming.configuration.source_id.as_str() != record.key {
                return Err("Connect source identity does not match its configuration".into());
            }
            canonical = Some(serde_json::to_value(&incoming).map_err(|error| error.to_string())?);
            let sources = if record.kind == "integration" {
                &stored.sources.integrations
            } else {
                &stored.sources.configured
            };
            let old = sources
                .iter()
                .find(|source| source.configuration.source_id.as_str() == record.key)
                .and_then(|source| source.credential.as_ref());
            let credential = prepare_credential(secrets, old, incoming.credential.as_deref())?;
            PreparedCell::Source(Some(ConfiguredSource {
                configuration: incoming.configuration,
                credential,
                music_folder_id: incoming.music_folder_id,
                local_access: None,
                enable_half_stars: incoming.enable_half_stars,
            }))
        }
        "preference" => {
            let defaults = UiSettings::default();
            let mut patch = shared_ui(&defaults)?;
            let Some(value) = record.value.clone().or_else(|| patch.remove(&record.key)) else {
                return Ok((PreparedCell::Unknown, None));
            };
            canonical = shared_ui(&merge_ui(
                &defaults,
                &serde_json::Map::from_iter([(record.key.clone(), value.clone())]),
            )?)?
            .remove(&record.key);
            PreparedCell::Preference(value)
        }
        "scrobbling" => {
            let mut value = serde_json::to_value(ScrobblingSettings::default())
                .map_err(|error| error.to_string())?;
            if value.get(&record.key).is_none() {
                return Ok((PreparedCell::Unknown, None));
            }
            if let Some(incoming) = &record.value {
                value[&record.key] = incoming.clone();
            }
            let mut settings: ScrobblingSettings =
                serde_json::from_value(value).map_err(|error| error.to_string())?;
            settings.sanitize();
            canonical = serde_json::to_value(&settings)
                .map_err(|error| error.to_string())?
                .get(&record.key)
                .cloned();
            let credentials = scrobbling::secret_descriptors()
                .iter()
                .filter(|descriptor| descriptor.kind().starts_with(&record.key))
                .map(|descriptor| {
                    let value = descriptor.value(&settings);
                    let credential = prepare_credential(
                        secrets,
                        stored.scrobbling_credentials.get(descriptor.kind()),
                        // Shadow a legacy keyring slot even when this cell clears its secret.
                        (!value.is_empty() || stored.scrobbling_secrets_present).then_some(value),
                    )?;
                    descriptor.value_mut(&mut settings).clear();
                    Ok((descriptor.kind(), credential))
                })
                .collect::<Result<_, String>>()?;
            let api_key =
                (record.key == "lastfm").then(|| std::mem::take(&mut settings.lastfm.api_key));
            PreparedCell::Scrobbling {
                value: serde_json::to_value(settings)
                    .map_err(|error| error.to_string())?
                    .get(&record.key)
                    .cloned()
                    .unwrap(),
                credentials,
                api_key,
            }
        }
        _ => return Ok((PreparedCell::Unknown, None)),
    };
    Ok((cell, canonical))
}

fn prepare_credential(
    secrets: &dyn SecretStore,
    old: Option<&Credential>,
    value: Option<&str>,
) -> Result<Option<Credential>, String> {
    if old
        .map(|credential| load_provider_secret(secrets, credential))
        .transpose()?
        .flatten()
        .as_deref()
        == value
    {
        Ok(old.cloned())
    } else {
        value
            .map(|value| seal_credential(secrets, value))
            .transpose()
    }
}

fn preserve_access(
    incoming: &mut ConfiguredSource,
    old: Option<&ConfiguredSource>,
) -> Result<(), String> {
    incoming.local_access = old.and_then(|source| source.local_access.clone());
    if incoming.configuration.is_local() {
        let mut payload: Value = serde_json::from_str(&incoming.configuration.provider_payload)
            .map_err(|error| error.to_string())?;
        let local = old
            .filter(|source| source.configuration.is_local())
            .map(|source| serde_json::from_str::<Value>(&source.configuration.provider_payload))
            .transpose()
            .map_err(|error| error.to_string())?;
        for field in ["roots", "document_roots", "excluded_folders"] {
            payload[field] = local
                .as_ref()
                .and_then(|payload| payload.get(field))
                .cloned()
                .unwrap_or_else(|| serde_json::json!([]));
        }
        incoming.configuration.provider_payload =
            serde_json::to_string(&payload).map_err(|error| error.to_string())?;
    }
    Ok(())
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
                        stored.sources.configured[0].credential =
                            Some(Credential::new("new-credential"));
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
        let reference = Credential::new("file-login");
        secrets
            .save_secret(&provider_secret_key(&reference), "saved-login")
            .unwrap();
        let connection = ConfiguredSource {
            configuration: SourceConfiguration {
                source_id: SourceId::new("files"),
                kind: "webdav".into(),
                name: "Files".into(),
                provider_payload: "{}".into(),
            },
            credential: Some(reference),
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
        let sender = SettingsOwner::new(sender, Arc::clone(&secrets), |_, _, _| {});
        let mut records = sender.shared_records().unwrap();
        records
            .iter_mut()
            .find(|value| value.record.kind == "integration")
            .unwrap()
            .record
            .value
            .as_mut()
            .unwrap()["future_login"] = serde_json::json!({"token":"future-secret"});
        records.push(library::StampedRecord {
            stamp: records[0].stamp,
            record: ConnectRecord {
                kind: "scrobbling".into(),
                key: "future-service".into(),
                value: Some(serde_json::json!({"token":"future-service-secret"})),
            },
        });
        let personal = sender.initial_preferences().unwrap();
        assert!(
            records
                .iter()
                .any(|r| r.record.kind == "integration" && r.record.key == "files")
        );
        assert!(!records.iter().any(|r| r.record.kind == "source"));
        let receiver_file = SettingsFile::memory();
        let receiver_secrets = Arc::new(SwitchableSecretStore::new(Arc::new(
            secrets::MemorySecretStore::new(),
        )));
        let receiver = SettingsOwner::new(
            receiver_file.clone(),
            Arc::clone(&receiver_secrets),
            |_, _, _| {},
        );
        assert!(
            !receiver.merge_shared(&records, Some(&personal)).unwrap(),
            "a connection does not invalidate the music library"
        );
        let stored = receiver_file.load();
        assert!(stored.sources.configured.is_empty());
        assert!(stored.sources.selected_source_id.is_none());
        assert_eq!(stored.sources.integrations.len(), 1);
        assert_eq!(
            load_provider_secret(
                receiver_secrets.as_ref(),
                stored.sources.integrations[0].credential.as_ref().unwrap()
            )
            .unwrap()
            .as_deref(),
            Some("saved-login")
        );
        let exported = receiver.shared_records().unwrap();
        assert_eq!(
            exported
                .iter()
                .find(|value| value.record.kind == "integration")
                .unwrap()
                .record
                .value
                .as_ref()
                .unwrap()["future_login"]["token"],
            "future-secret",
        );
        assert_eq!(
            exported
                .iter()
                .find(|value| value.record.key == "future-service")
                .unwrap()
                .record
                .value,
            Some(serde_json::json!({"token":"future-service-secret"})),
        );
        assert!(
            !serde_json::to_string(&stored)
                .unwrap()
                .contains("future-secret")
        );
        assert!(
            !serde_json::to_string(&stored)
                .unwrap()
                .contains("future-service-secret")
        );
        assert!(!receiver.merge_shared(&records, None).unwrap());
        let baseline = receiver
            .shared_records()
            .unwrap()
            .into_iter()
            .map(|value| value.record)
            .collect::<Vec<_>>();
        let mut pending = records.clone();
        let incoming = pending
            .iter_mut()
            .find(|value| value.record.kind == "integration")
            .unwrap()
            .record
            .value
            .as_mut()
            .unwrap();
        incoming["configuration"]["name"] = "Received name".into();
        incoming["credential"] = "received-login".into();
        receiver
            .migrate_legacy_settings(&pending, &baseline)
            .unwrap();
        let stored = receiver_file.load();
        assert_eq!(
            stored.sources.integrations[0].configuration.name,
            "Received name"
        );
        assert_eq!(
            load_provider_secret(
                receiver_secrets.as_ref(),
                stored.sources.integrations[0].credential.as_ref().unwrap()
            )
            .unwrap()
            .as_deref(),
            Some("received-login")
        );
        receiver_file
            .update(|stored| {
                stored.sources.integrations[0].configuration.name = "Local name".into();
                Ok(())
            })
            .unwrap();
        receiver
            .migrate_legacy_settings(&pending, &baseline)
            .unwrap();
        assert_eq!(
            receiver_file.load().sources.integrations[0]
                .configuration
                .name,
            "Local name"
        );
    }

    #[test]
    fn incoming_settings_preserve_source_and_hardware_edits_during_secret_reads() {
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
                credential: Some(Credential::new("old-credential")),
                music_folder_id: None,
                local_access: None,
                enable_half_stars: false,
            });
            Ok(())
        })
        .unwrap();
        let secrets = Arc::new(SwitchableSecretStore::new(Arc::new(ConcurrentEditStore {
            file: file.clone(),
            edited: std::sync::atomic::AtomicBool::new(false),
        })));
        let owner = SettingsOwner::new(file.clone(), Arc::clone(&secrets), |_, _, _| {});
        let stamp = file
            .load()
            .shared_settings
            .stamps
            .values()
            .copied()
            .max()
            .unwrap_or_default()
            .next(1);
        owner
            .merge_shared(
                &[
                    library::StampedRecord {
                        record: ConnectRecord {
                            kind: "scrobbling".into(),
                            key: "lastfm".into(),
                            value: Some(
                                serde_json::to_value(scrobbling::Settings::default().lastfm)
                                    .unwrap(),
                            ),
                        },
                        stamp,
                    },
                    library::StampedRecord {
                        record: ConnectRecord {
                            kind: "preference".into(),
                            key: "prefer_distinct_track_covers".into(),
                            value: Some(Value::Bool(true)),
                        },
                        stamp,
                    },
                ],
                None,
            )
            .unwrap();
        let stored = file.load();
        assert!(stored.ui.application.prefer_distinct_track_covers);
        assert_eq!(
            stored.sources.configured[0]
                .credential
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
                    credential: None,
                    music_folder_id: None,
                    local_access: None,
                    enable_half_stars: false,
                });
                stored.ui.application.private_mode = true;
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
                stored.ui.lyrics.preferences.lyrics_font_family = Some("Custom font".into());
                stored.ui.queue_lyrics_height = Some(200);
                Ok(())
            })
            .unwrap();
        let device_identity = receiver.load().jellyfin_device_id;
        let observed = Arc::new(AtomicUsize::new(0));
        let callback = observed.clone();
        let secrets = Arc::new(SwitchableSecretStore::new(Arc::new(
            secrets::MemorySecretStore::new(),
        )));
        let sender = SettingsOwner::new(sender, Arc::clone(&secrets), |_, _, _| {});
        let receiver_owner =
            SettingsOwner::new(receiver.clone(), Arc::clone(&secrets), move |_, _, _| {
                callback.fetch_add(1, Ordering::Relaxed);
            });
        let records = sender.shared_records().unwrap();
        let personal = sender.initial_preferences().unwrap();
        let source = records
            .iter()
            .find(|r| r.record.kind == "source")
            .unwrap()
            .record
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
            .merge_shared(&records, Some(&personal))
            .unwrap();
        let value = receiver.load();
        assert!(value.ui.application.private_mode);
        assert_eq!(
            value.ui.playback.audio_output.as_deref(),
            Some("receiver-device")
        );
        assert_eq!(value.ui.playback.stream_quality, StreamQuality::Original);
        assert_eq!(value.ui.random_play, RandomPlaySettings::default());
        assert!(value.ui.lyrics.preferences.lyrics_font_family.is_none());
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
        receiver_owner.merge_shared(&records, None).unwrap();
        assert_eq!(observed.load(Ordering::Relaxed), before);
        let fresh = SettingsFile::memory();
        let fresh_owner = SettingsOwner::new(fresh.clone(), Arc::clone(&secrets), |_, _, _| {});
        fresh_owner.merge_shared(&records, Some(&personal)).unwrap();
        let source_id = SourceId::new("music");
        assert_eq!(
            fresh.load().ui.download_settings(&source_id).quality,
            sources::DownloadQuality::Transcoded(library::AudioEncoding {
                codec: library::AudioCodec::Opus,
                bitrate_kbps: Some(128),
                channels: library::AudioChannels::Preserve,
            })
        );
        fresh_owner
            .set_download_quality(source_id.clone(), sources::DownloadQuality::Original)
            .unwrap();
        fresh_owner.merge_shared(&records, Some(&personal)).unwrap();
        assert_eq!(
            fresh.load().ui.download_settings(&source_id).quality,
            sources::DownloadQuality::Original
        );
    }
}
