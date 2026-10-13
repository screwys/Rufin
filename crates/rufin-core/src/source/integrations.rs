use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FileIntegration {
    pub id: SourceId,
    pub name: String,
    pub kind: String,
    pub music_source: bool,
}

impl SourceOwner {
    pub async fn save_files(
        &self,
        source_id: Option<SourceId>,
        smb: bool,
        integration: bool,
        name: String,
        settings: sources::FileSourceSettings,
        credentials: sources::FileCredentialsEdit,
    ) -> Result<SourceId, String> {
        if let Some(source_id) = source_id {
            let input = SourceSettingsChange::Files {
                source_id: source_id.clone(),
                name,
                settings,
                credentials,
            };
            let result = if integration {
                self.update_file_integration(input)
            } else {
                self.update_source(input)
            };
            result.recv().await.map_err(string_error)??;
            Ok(source_id)
        } else {
            let credentials = sources::FileCredentials {
                secret: credentials.secret.unwrap_or_default(),
                headers: credentials.headers.unwrap_or_default(),
            };
            let input = if smb {
                SourceSetup::Smb {
                    name,
                    settings,
                    credentials,
                }
            } else {
                SourceSetup::WebDav {
                    name,
                    settings,
                    credentials,
                }
            };
            if integration {
                self.configure_file_integration(input)
                    .recv()
                    .await
                    .map_err(string_error)?
            } else {
                self.configure_source(input)
                    .recv()
                    .await
                    .map_err(string_error)?
                    .map(|selected| selected.source_id)
            }
        }
    }

    pub fn file_integrations(&self) -> Vec<FileIntegration> {
        let stored = self.shared.settings.load();
        stored
            .sources
            .configured
            .iter()
            .map(|s| (s, true))
            .chain(stored.sources.integrations.iter().map(|s| (s, false)))
            .filter(|(s, _)| matches!(s.configuration.kind.as_str(), "webdav" | "smb"))
            .map(|(s, music_source)| FileIntegration {
                id: s.configuration.source_id.clone(),
                name: s.configuration.name.clone(),
                kind: s.configuration.kind.clone(),
                music_source,
            })
            .collect()
    }

    pub fn file_integration_settings(
        &self,
        id: &SourceId,
    ) -> Result<Option<EditableSource>, String> {
        let stored = self.shared.settings.load();
        stored
            .sources
            .configured
            .iter()
            .chain(&stored.sources.integrations)
            .find(|s| &s.configuration.source_id == id)
            .map(|s| editable_source(&s.configuration))
            .transpose()
    }

    /// Authorize a reusable connection without selecting or scanning a music source.
    pub fn configure_file_integration(
        &self,
        input: SourceSetup,
    ) -> Receiver<Result<SourceId, String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let result = async {
                if !matches!(input, SourceSetup::WebDav { .. } | SourceSetup::Smb { .. }) {
                    return Err("Choose WebDAV or SMB / Samba".into());
                }
                let connected = Source::connect(fresh_source_id()?, input, "")
                    .await
                    .map_err(string_error)?;
                let (configuration, _, credential) = connected.into_parts();
                let id = configuration.source_id.clone();
                owner
                    .persist_file_integration(configuration, credential)
                    .await?;
                Ok(id)
            }
            .await;
            let _ = sender.send(result).await;
        });
        receiver
    }

    pub fn update_file_integration(
        &self,
        input: SourceSettingsChange,
    ) -> Receiver<Result<(), String>> {
        let id = input.source_id().clone();
        if self
            .shared
            .settings
            .load()
            .sources
            .configured
            .iter()
            .any(|s| s.configuration.source_id == id)
        {
            return self.update_source(input);
        }
        let (sender, receiver) = async_channel::bounded(1);
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let result = async {
                let stored = owner.shared.settings.load();
                let saved = stored
                    .sources
                    .integrations
                    .iter()
                    .find(|s| s.configuration.source_id == id)
                    .ok_or("The connection no longer exists")?;
                let credential = saved
                    .credential_ref
                    .as_ref()
                    .map(|r| load_provider_secret(&owner.shared.secrets, r))
                    .transpose()?
                    .flatten();
                match Source::edit(saved.configuration.clone(), credential, input, None)
                    .await
                    .map_err(string_error)?
                {
                    sources::SourceEditResult::Unchanged => Ok(()),
                    sources::SourceEditResult::ConfigurationOnly(configuration) => {
                        owner.shared.settings.update(|stored| {
                            if let Some(saved) = stored
                                .sources
                                .integrations
                                .iter_mut()
                                .find(|s| s.configuration.source_id == id)
                            {
                                saved.configuration = configuration;
                            }
                            Ok(())
                        })
                    }
                    sources::SourceEditResult::Connected(connected) => {
                        let (configuration, _, credential) = connected.into_parts();
                        owner
                            .persist_file_integration(configuration, credential)
                            .await
                    }
                }
            }
            .await;
            let _ = sender.send(result).await;
        });
        receiver
    }

    async fn persist_file_integration(
        &self,
        configuration: SourceConfiguration,
        credential: Option<String>,
    ) -> Result<(), String> {
        let settings = self.shared.settings.clone();
        let secrets = self.shared.secrets.clone();
        tokio::task::spawn_blocking(move || {
            let reference = if credential.is_some() {
                Some(
                    settings
                        .load()
                        .sources
                        .integrations
                        .iter()
                        .find(|saved| saved.configuration.source_id == configuration.source_id)
                        .and_then(|saved| saved.credential_ref.clone())
                        .map(Ok)
                        .unwrap_or_else(fresh_credential_ref)?,
                )
            } else {
                None
            };
            if let (Some(reference), Some(credential)) = (&reference, credential) {
                save_provider_secret(&secrets, reference, credential)?;
            }
            settings.update(|stored| {
                let next = ConfiguredSource {
                    configuration,
                    credential_ref: reference,
                    music_folder_id: None,
                    local_access: None,
                    enable_half_stars: false,
                };
                if let Some(saved) = stored
                    .sources
                    .integrations
                    .iter_mut()
                    .find(|s| s.configuration.source_id == next.configuration.source_id)
                {
                    *saved = next;
                } else {
                    stored.sources.integrations.push(next);
                }
                Ok(())
            })
        })
        .await
        .map_err(string_error)?
    }
}
