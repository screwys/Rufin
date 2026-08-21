use super::*;

enum PreparedConnectionLibrary {
    Candidate(Box<PreparedSourceCandidate>),
    Accepted {
        library: Arc<Library>,
        cache_match: SourceCacheMatch,
    },
}

struct SavedSourceConnection {
    configured: ConfiguredSource,
    previous: Option<ConfiguredSource>,
    previous_selected_source_id: Option<SourceId>,
    staged_credential: Option<CredentialRef>,
}

impl SourceOwner {
    pub(super) async fn apply_secret_storage_change(
        &mut self,
        mode: SecretStorageMode,
    ) -> Result<(), String> {
        let previous = self.shared.settings.load();
        if previous.ui.secret_storage_mode == mode {
            return Ok(());
        }
        let transition_source_id = self
            .shared
            .selected()
            .filter(|selected| {
                matches!(
                    selected.configuration.editable(),
                    Ok(sources::EditableSource::Credentials { .. })
                )
            })
            .map(|selected| selected.source_id().clone());
        if let Some(source_id) = transition_source_id.as_ref() {
            self.shared
                .send_event(SourceEvent::Operation(SourceOperation::Switching {
                    target: source_id.clone(),
                    progress: initial_progress(),
                }))
                .await;
        }

        let keys = all_secret_keys(&previous);
        let settings = self.shared.settings.clone();
        let changed = blocking(move || {
            let scope = fresh_secret_scope_id()?;
            settings.update(|stored| {
                stored.ui.secret_storage_mode = mode;
                stored.secret_scope_id = scope;
                for descriptor in scrobbling::secret_descriptors() {
                    descriptor.value_mut(&mut stored.scrobbling).clear();
                }
                Ok(stored.clone())
            })
        })
        .await;
        let changed = match changed {
            Ok(changed) => changed,
            Err(error) => {
                if transition_source_id.is_some() {
                    self.fail_transition(transition_source_id, error.clone(), false)
                        .await;
                }
                return Err(error);
            }
        };

        let previous_secrets = self.shared.secrets.replace(platform_secret_store(&changed));
        self.clear_configured_feeds();
        let _ = blocking(move || {
            for key in keys {
                if let Err(error) = previous_secrets.delete_secret(&key) {
                    warn!(%error, ?key, "failed to remove a secret from the previous backend");
                }
            }
            Ok(())
        })
        .await;

        let scrobbling = load_scrobbling_settings(&self.shared.settings, &self.shared.secrets);
        if let Err(error) = self
            .shared
            .scrobbler
            .update_settings(scrobbling, changed.ui.private_mode)
        {
            warn!(%error, "could not clear external scrobbling accounts");
        }

        if let Some(source_id) = transition_source_id {
            let configured = configured_source(&changed.sources, &source_id)?;
            let cancelled = Arc::new(AtomicBool::new(false));
            let progress = Arc::new(|_: SourceReadProgress| {});
            if let Err(error) = select_source(self, configured, progress, cancelled, false).await {
                self.begin_transition().await;
                self.fail_transition(Some(source_id), error.clone(), false)
                    .await;
                return Err(error);
            }
        }
        Ok(())
    }

    pub(super) async fn apply_source_update(
        &mut self,
        source_id: SourceId,
        input: SourceSettingsInput,
        local_roots_changed: bool,
        cancelled: Arc<AtomicBool>,
    ) {
        let configured = match configured_source(&self.shared.settings.load().sources, &source_id) {
            Ok(configured) => configured,
            Err(error) => return self.shared.warn_nonfatal(&error),
        };
        let selected = self
            .shared
            .selected()
            .is_some_and(|current| current.source_id() == &source_id);
        if selected && local_roots_changed {
            self.shared
                .send_event(SourceEvent::Operation(SourceOperation::Refreshing {
                    source_id: source_id.clone(),
                    progress: initial_progress(),
                }))
                .await;
        }
        let progress_source = (selected && local_roots_changed).then_some(source_id);
        let progress = self.progress(Arc::clone(&cancelled), move |progress| {
            progress_source
                .as_ref()
                .map(|source_id| SourceOperation::Refreshing {
                    source_id: source_id.clone(),
                    progress,
                })
        });
        let result: Result<(), String> = async {
            let credential = load_credential(&self.shared, &configured).await?;
            match Source::edit(
                configured.configuration.clone(),
                credential,
                input,
                Some(self.shared.settings.load().jellyfin_device_id),
            )
            .await
            .map_err(string_error)?
            {
                SourceEditResult::Unchanged => {
                    self.shared.publish_configured().await;
                    Ok(())
                }
                SourceEditResult::ConfigurationOnly(configuration) => {
                    if !self.shared.protect_interruptible_commit(&cancelled) {
                        return Ok(());
                    }
                    let saved = self
                        .save_source_connection(
                            Some(&configured),
                            configuration.clone(),
                            None,
                            false,
                            configured.music_folder_id.clone(),
                            configured.local_access.clone(),
                        )
                        .await?;
                    if selected && let Some(active) = self.shared.selected() {
                        let mut active = (*active).clone();
                        active.configuration = configuration;
                        self.shared.replace_selected(active);
                    }
                    self.finish_source_connection(saved).await;
                    self.shared.publish_configured().await;
                    Ok(())
                }
                SourceEditResult::Connected(connected) => {
                    let (configuration, source, credential) = connected.into_parts();
                    let same_account =
                        configuration.source_id == configured.configuration.source_id;
                    if !same_account
                        && self
                            .shared
                            .settings
                            .load()
                            .sources
                            .configured
                            .iter()
                            .any(|saved| saved.configuration.source_id == configuration.source_id)
                    {
                        return Err("this source account is already configured".to_string());
                    }
                    if !selected {
                        if !self.shared.protect_interruptible_commit(&cancelled) {
                            return Ok(());
                        }
                        let feed = (configuration.kind == "jellyfin"
                            && self
                                .shared
                                .configured_feed_source(&configured.configuration.source_id)
                                .is_some())
                        .then(|| Arc::new(source));
                        let saved = self
                            .save_source_connection(
                                Some(&configured),
                                configuration,
                                credential,
                                false,
                                configured.music_folder_id.clone().filter(|_| same_account),
                                configured.local_access.clone(),
                            )
                            .await?;
                        if let Some(feed) = feed {
                            self.install_configured_jellyfin_feed(feed, true);
                        }
                        self.finish_source_connection(saved).await;
                        self.shared.publish_configured().await;
                        return Ok(());
                    }
                    let source = Arc::new(source);
                    let identity = configuration.input_identity().map_err(string_error)?;
                    let current = self
                        .shared
                        .selected()
                        .ok_or_else(|| "the selected source is no longer active".to_string())?;
                    let prepared_library =
                        if same_account && cache_input_matches(&identity, &current.library) {
                            PreparedConnectionLibrary::Accepted {
                                library: Arc::clone(&current.library),
                                cache_match: SourceCacheMatch::Exact,
                            }
                        } else {
                            PreparedConnectionLibrary::Candidate(Box::new(
                                prepare_source_candidate(
                                    &self.shared,
                                    Arc::clone(&source),
                                    identity,
                                    same_account.then(|| Arc::clone(&current.library)),
                                    progress,
                                    Arc::clone(&cancelled),
                                )
                                .await?,
                            ))
                        };
                    if cancelled.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    self.commit_selected_connection(
                        Some(configured),
                        configuration,
                        Some(source),
                        credential,
                        prepared_library,
                        Arc::clone(&cancelled),
                        true,
                    )
                    .await
                }
            }
        }
        .await;
        if let Err(error) = result
            && !cancelled.load(Ordering::Acquire)
        {
            self.selected_or_inactive_failure(selected, error).await;
        } else if local_roots_changed && !cancelled.load(Ordering::Acquire) {
            self.shared
                .send_event(SourceEvent::Operation(SourceOperation::Idle))
                .await;
        }
    }

    pub(super) async fn selected_or_inactive_failure(&mut self, selected: bool, error: String) {
        if selected {
            self.selected_update_failed(error).await;
        } else {
            self.shared.warn_nonfatal(&error);
        }
    }

    async fn commit_selected_connection(
        &mut self,
        previous: Option<ConfiguredSource>,
        configuration: SourceConfiguration,
        source: Option<Arc<Source>>,
        credential: Option<String>,
        prepared_library: PreparedConnectionLibrary,
        cancelled: Arc<AtomicBool>,
        protect_commit: bool,
    ) -> Result<(), String> {
        let same_session = self
            .shared
            .selected()
            .is_some_and(|selected| selected.source_id() == &configuration.source_id);
        let acceptance_owner = Arc::clone(&self.shared);
        let _acceptance = if same_session {
            Some(acceptance_owner.acceptance_lane.lock().await)
        } else {
            None
        };
        if protect_commit && !self.shared.protect_interruptible_commit(&cancelled) {
            return Ok(());
        }
        if same_session {
            let current = self
                .shared
                .selected()
                .filter(|selected| selected.source_id() == &configuration.source_id)
                .ok_or_else(|| "the selected source changed while it was prepared".to_string())?;
            let saved = self
                .save_source_connection(
                    previous.as_ref(),
                    configuration.clone(),
                    credential,
                    true,
                    previous
                        .as_ref()
                        .and_then(|configured| configured.music_folder_id.clone()),
                    previous
                        .as_ref()
                        .and_then(|configured| configured.local_access.clone()),
                )
                .await?;
            self.retire_selected_access().await;
            let feed = source
                .as_ref()
                .filter(|_| configuration.kind == "jellyfin")
                .cloned();
            let updated = match prepared_library {
                PreparedConnectionLibrary::Candidate(candidate) => self
                    .accept_same_session_candidate(
                        &current,
                        configuration,
                        source,
                        saved.configured.music_folder_id.clone(),
                        saved.configured.local_access.clone(),
                        *candidate,
                    )
                    .await
                    .map(|_| ()),
                PreparedConnectionLibrary::Accepted { library, .. } => {
                    let mut selected = (*current).clone();
                    selected.configuration = configuration;
                    selected.source = source;
                    selected.library = library;
                    self.shared
                        .replace_selected_runtime(selected)
                        .await
                        .then_some(())
                        .ok_or_else(|| "the selected source changed before cutover".to_string())
                }
            };
            if let Err(error) = updated {
                self.rollback_source_connection(saved).await;
                return Err(error);
            }
            if let Some(feed) = feed {
                self.install_configured_jellyfin_feed(feed, true);
            }
            if !cancelled.load(Ordering::Acquire) {
                self.start_selected_access(true).await;
            }
            self.shared.publish_configured().await;
            self.finish_source_connection(saved).await;
            return Ok(());
        }
        let (library, cache_match) = match prepared_library {
            PreparedConnectionLibrary::Candidate(candidate) => (
                blocking(move || {
                    (*candidate)
                        .accept()
                        .map(|commit| commit.library)
                        .map_err(string_error)
                })
                .await?,
                None,
            ),
            PreparedConnectionLibrary::Accepted {
                library,
                cache_match,
            } => (library, Some(cache_match)),
        };
        let replaces_account = previous
            .as_ref()
            .is_some_and(|previous| previous.configuration.source_id != configuration.source_id);
        let selected_source_id = configuration.source_id.clone();
        if replaces_account || previous.is_none() {
            let library = Arc::clone(&library);
            blocking(move || {
                library
                    .initialize_smart_playlists()
                    .map(|_| ())
                    .map_err(string_error)
            })
            .await?;
        }
        let music_folder_id = normalize_music_folder(
            &library,
            previous
                .as_ref()
                .filter(|_| !replaces_account)
                .and_then(|configured| configured.music_folder_id.clone()),
        )?;
        let local_access = previous
            .as_ref()
            .and_then(|configured| configured.local_access.clone());
        if let Some(access) = local_access.as_ref() {
            let library = Arc::clone(&library);
            let access = access.clone();
            blocking(move || library.configure_local_access(access).map_err(string_error)).await?;
        }
        let home = {
            let library = Arc::clone(&library);
            let folder = music_folder_id.clone();
            blocking(move || library.home(folder.as_ref()).map_err(string_error)).await?
        };
        let feed = source
            .as_ref()
            .filter(|_| configuration.kind == "jellyfin")
            .cloned();
        let selected = Arc::new(SelectedSourceState {
            configuration: configuration.clone(),
            source,
            source_session_epoch: SourceSessionEpoch::new(
                self.shared.next_epoch.fetch_add(1, Ordering::AcqRel),
            ),
            library,
            home,
            music_folder_id,
        });
        let session = ActiveSource::new(&self.shared, &selected);
        let playback = self.shared.playback()?;
        let prepared_playback = {
            let playback = Arc::clone(&playback);
            let session = Arc::clone(&session);
            let selected = Arc::clone(&selected);
            blocking(move || playback.prepare_selected(session, selected)).await?
        };
        let saved = self
            .save_source_connection(
                previous.as_ref(),
                configuration,
                credential,
                true,
                selected.music_folder_id.clone(),
                local_access,
            )
            .await?;
        if saved.previous.as_ref().is_some_and(|previous| {
            previous.configuration.source_id != saved.configured.configuration.source_id
        }) {
            self.shared
                .send_event(SourceEvent::Operation(SourceOperation::Switching {
                    target: selected_source_id.clone(),
                    progress: initial_progress(),
                }))
                .await;
        }
        self.cancel_album_release_lookup(true);
        self.retire_selected_access().await;
        let cutover = {
            let playback = Arc::clone(&playback);
            blocking(move || Ok(playback.stop_for_source_switch())).await?
        };
        self.shared.release_selected().await;
        self.shared
            .install_selected_slot(Arc::clone(&session), Arc::clone(&selected));
        if let Some(feed) = feed {
            self.install_configured_jellyfin_feed(
                feed,
                cache_match == Some(SourceCacheMatch::Exact),
            );
        }
        self.shared.attach_selected_downloads(&selected).await;
        let playback = playback.install_prepared(prepared_playback, cutover);
        self.shared
            .publish_selected(session, Arc::clone(&selected), playback)
            .await;
        if !cancelled.load(Ordering::Acquire) {
            self.start_selected_access(cache_match == Some(SourceCacheMatch::Exact))
                .await;
        }
        self.finish_source_connection(saved).await;
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        if cache_match == Some(SourceCacheMatch::ReaderUpgrade) && selected.source.is_some() {
            SourceOwner {
                shared: Arc::clone(&self.shared),
            }
            .request_refresh(selected_source_id, true);
        } else {
            self.shared
                .send_event(SourceEvent::Operation(SourceOperation::Idle))
                .await;
        }
        SourceOwner {
            shared: Arc::clone(&self.shared),
        }
        .request_favorite_retry();
        Ok(())
    }

    pub(super) async fn commit_refresh(
        &mut self,
        previous: Arc<SelectedSourceState>,
        prepared: PreparedSourceCandidate,
    ) -> Result<(), String> {
        if !self.shared.matches_selected(&previous.qualifier()) {
            return Err(
                "the selected source changed before the refreshed library was accepted".to_string(),
            );
        }
        let stored = self.shared.settings.load();
        let configured = stored
            .sources
            .configured
            .iter()
            .find(|configured| configured.configuration.source_id == *previous.source_id());
        let requested_folder = configured.and_then(|configured| configured.music_folder_id.clone());
        let local_access =
            configured.and_then(|configured| configured.local_access.as_ref().cloned());
        let accepted = self
            .accept_same_session_candidate(
                &previous,
                previous.configuration.clone(),
                previous.source.clone(),
                requested_folder.clone(),
                local_access,
                prepared,
            )
            .await?;
        if let Some(selected) = accepted {
            if selected.music_folder_id != requested_folder {
                let settings = self.shared.settings.clone();
                let source_id = previous.source_id().clone();
                let folder = selected.music_folder_id.clone();
                if let Err(error) =
                    blocking(move || save_music_folder(&settings, &source_id, folder)).await
                {
                    warn!(%error, source_id = %previous.source_id(), "could not save the normalized music folder");
                }
            }
            self.start_local_access_refresh(&selected).await;
        }
        Ok(())
    }

    pub(super) async fn accept_same_session_candidate(
        &mut self,
        previous: &SelectedSourceState,
        configuration: SourceConfiguration,
        source: Option<Arc<Source>>,
        requested_folder: Option<MusicFolderId>,
        local_access: Option<library::LocalAccessMapping>,
        candidate: PreparedSourceCandidate,
    ) -> Result<Option<SelectedSourceState>, String> {
        let change = candidate.change();
        if change == CandidateChange::None {
            let commit = blocking(move || candidate.accept().map_err(string_error)).await?;
            let source_changed = match (&previous.source, &source) {
                (Some(previous), Some(next)) => !Arc::ptr_eq(previous, next),
                (None, None) => false,
                _ => true,
            };
            if previous.configuration != configuration || source_changed {
                let mut selected = previous.clone();
                selected.configuration = configuration;
                selected.source = source;
                selected.library = commit.library;
                if !self.shared.replace_selected_runtime(selected.clone()).await {
                    return Err("the selected source changed before cutover".to_string());
                }
                return Ok(Some(selected));
            }
            return Ok(None);
        }
        let folder = normalize_music_folder(candidate.library(), requested_folder)?;
        if let Some(access) = local_access {
            let library = Arc::clone(candidate.library());
            blocking(move || {
                library
                    .configure_local_access(access)
                    .map(|_| ())
                    .map_err(string_error)
            })
            .await?;
        }
        let playback = if change == CandidateChange::Library {
            let playback = self.shared.playback()?;
            let refresh = playback.prepare_track_refresh(previous.source_session_epoch)?;
            Some((playback, refresh))
        } else {
            None
        };
        let commit = blocking(move || candidate.accept().map_err(string_error)).await?;
        let library = Arc::clone(&commit.library);
        let home_folder = folder.clone();
        let home =
            blocking(move || library.home(home_folder.as_ref()).map_err(string_error)).await?;
        let selected = SelectedSourceState {
            configuration,
            source,
            source_session_epoch: previous.source_session_epoch,
            library: commit.library,
            home,
            music_folder_id: folder,
        };
        if let Some((playback, refresh)) = playback {
            self.cancel_album_release_lookup(false);
            self.shared
                .publish_library_replacement(selected.clone())
                .await;
            let library = Arc::clone(&selected.library);
            if let Err(error) =
                blocking(move || playback.apply_track_refresh(refresh, &library)).await
            {
                warn!(%error, "could not update Playback after accepting refreshed source facts");
            }
            self.start_album_release_lookup();
        } else {
            self.shared.publish_home_replacement(selected.clone()).await;
        }
        Ok(Some(selected))
    }

    pub(super) async fn forget_now(&mut self, source_id: SourceId) {
        let stored = self.shared.settings.load();
        let removed = stored
            .sources
            .configured
            .iter()
            .find(|source| source.configuration.source_id == source_id)
            .cloned();
        let selected = self
            .shared
            .selected()
            .is_some_and(|selected| selected.source_id() == &source_id);
        let replacement = selected
            .then(|| replacement_source(&stored.sources, &source_id))
            .flatten();
        let settings = self.shared.settings.clone();
        let id_for_settings = source_id.clone();
        let saved = blocking(move || {
            settings.update(|stored| {
                stored
                    .sources
                    .configured
                    .retain(|source| source.configuration.source_id != id_for_settings);
                if stored.sources.selected_source_id.as_ref() == Some(&id_for_settings) {
                    stored.sources.selected_source_id = None;
                }
                Ok(())
            })
        })
        .await;
        if let Err(error) = saved {
            if selected {
                self.fail_transition(Some(source_id), error, false).await;
            } else {
                self.shared.warn_nonfatal(&error);
            }
            return;
        }
        if selected {
            self.shared
                .send_event(SourceEvent::Operation(SourceOperation::Switching {
                    target: replacement
                        .as_ref()
                        .cloned()
                        .unwrap_or_else(|| source_id.clone()),
                    progress: initial_progress(),
                }))
                .await;
            self.begin_transition().await;
        }
        self.remove_replaced_source_data(source_id.clone()).await;
        if let Some(reference) = removed.and_then(|source| source.credential_ref) {
            self.delete_staged_credential(Some(&reference)).await;
        }
        if let Some(replacement) = replacement {
            let configured =
                match configured_source(&self.shared.settings.load().sources, &replacement) {
                    Ok(configured) => configured,
                    Err(error) => {
                        self.shared.publish_configured().await;
                        self.fail_transition(Some(replacement), error, false).await;
                        return;
                    }
                };
            let cancelled = Arc::new(AtomicBool::new(false));
            let progress = self.progress(Arc::clone(&cancelled), |_| None);
            if let Err(error) = select_source(self, configured, progress, cancelled, false).await {
                self.shared.publish_configured().await;
                self.fail_transition(Some(replacement), error, false).await;
            }
            return;
        }
        self.shared.publish_configured().await;
        self.shared
            .send_event(SourceEvent::Operation(SourceOperation::Idle))
            .await;
    }

    pub(super) async fn set_music_folder(
        &mut self,
        selected: Arc<SelectedSourceState>,
        folder_id: Option<MusicFolderId>,
    ) {
        let source_id = selected.source_id().clone();
        let folder_id = match normalize_music_folder(&selected.library, folder_id) {
            Ok(folder) => folder,
            Err(error) => {
                self.shared.warn_nonfatal(&error);
                return;
            }
        };
        let library = Arc::clone(&selected.library);
        let home_folder = folder_id.clone();
        let home = match blocking(move || library.home(home_folder.as_ref()).map_err(string_error))
            .await
        {
            Ok(home) => home,
            Err(error) => {
                self.shared.warn_nonfatal(&error);
                return;
            }
        };
        let settings = self.shared.settings.clone();
        let source_for_settings = source_id.clone();
        let folder_for_settings = folder_id.clone();
        if let Err(error) = blocking(move || {
            save_music_folder(&settings, &source_for_settings, folder_for_settings)
        })
        .await
        {
            self.shared.warn_nonfatal(&error);
            return;
        }
        let mut replacement = (*selected).clone();
        replacement.home = home;
        replacement.music_folder_id = folder_id;
        self.shared.publish_library_replacement(replacement).await;
    }

    pub(super) async fn delete_staged_credential(&self, reference: Option<&CredentialRef>) {
        let Some(reference) = reference.cloned() else {
            return;
        };
        let secrets = Arc::clone(&self.shared.secrets);
        if let Err(error) = blocking(move || delete_provider_secret(&secrets, &reference)).await {
            warn!(%error, "could not delete a replaced source credential");
        }
    }

    async fn save_source_connection(
        &self,
        previous: Option<&ConfiguredSource>,
        configuration: SourceConfiguration,
        credential: Option<String>,
        select: bool,
        music_folder_id: Option<MusicFolderId>,
        local_access: Option<library::LocalAccessMapping>,
    ) -> Result<SavedSourceConnection, String> {
        let replaced_source_id = previous
            .filter(|previous| previous.configuration.source_id != configuration.source_id)
            .map(|previous| previous.configuration.source_id.clone());
        let previous_credential = previous.and_then(|source| source.credential_ref.clone());
        let staged_credential = if let Some(credential) = credential {
            let reference = fresh_credential_ref()?;
            let secrets = Arc::clone(&self.shared.secrets);
            let saved_reference = reference.clone();
            blocking(move || save_provider_secret(&secrets, &saved_reference, credential)).await?;
            Some(reference)
        } else {
            None
        };
        let configured = ConfiguredSource {
            configuration,
            credential_ref: if replaced_source_id.is_some() {
                staged_credential.clone()
            } else {
                staged_credential
                    .clone()
                    .or_else(|| previous_credential.clone())
            },
            music_folder_id,
            local_access,
        };
        let previous_selected_source_id = self.shared.settings.load().sources.selected_source_id;
        let settings = self.shared.settings.clone();
        let previous_id = previous.map(|source| source.configuration.source_id.clone());
        let saved = configured.clone();
        let source_id = saved.configuration.source_id.clone();
        if let Err(error) = blocking(move || {
            settings.update(|stored| {
                if previous_id
                    .as_ref()
                    .is_some_and(|previous| previous != &source_id)
                    && stored
                        .sources
                        .configured
                        .iter()
                        .any(|source| source.configuration.source_id == source_id)
                {
                    return Err("this source account is already configured".to_string());
                }
                if let Some(previous) = previous_id.as_ref() {
                    let source = stored
                        .sources
                        .configured
                        .iter_mut()
                        .find(|source| &source.configuration.source_id == previous)
                        .ok_or_else(|| "the configured source no longer exists".to_string())?;
                    *source = saved.clone();
                } else {
                    stored.sources.configured.push(saved.clone());
                }
                if select {
                    stored.sources.selected_source_id = Some(source_id.clone());
                }
                Ok(())
            })
        })
        .await
        {
            self.delete_staged_credential(staged_credential.as_ref())
                .await;
            return Err(error);
        }
        Ok(SavedSourceConnection {
            configured,
            previous: previous.cloned(),
            previous_selected_source_id,
            staged_credential,
        })
    }

    async fn rollback_source_connection(&self, saved: SavedSourceConnection) {
        let SavedSourceConnection {
            configured,
            previous,
            previous_selected_source_id,
            staged_credential,
            ..
        } = saved;
        let settings = self.shared.settings.clone();
        let replacement_id = configured.configuration.source_id;
        match blocking(move || {
            settings.update(|stored| {
                let position = stored
                    .sources
                    .configured
                    .iter()
                    .position(|source| source.configuration.source_id == replacement_id)
                    .ok_or_else(|| "the replacement source no longer exists".to_string())?;
                if let Some(previous) = previous.clone() {
                    stored.sources.configured[position] = previous;
                } else {
                    stored.sources.configured.remove(position);
                }
                stored.sources.selected_source_id = previous_selected_source_id.clone();
                Ok(())
            })
        })
        .await
        {
            Ok(()) => {
                self.delete_staged_credential(staged_credential.as_ref())
                    .await;
            }
            Err(error) => {
                warn!(%error, "could not restore source settings after a failed cutover");
            }
        }
    }

    async fn finish_source_connection(&self, saved: SavedSourceConnection) {
        let previous_credential = saved
            .previous
            .as_ref()
            .and_then(|previous| previous.credential_ref.as_ref());
        if previous_credential != saved.configured.credential_ref.as_ref() {
            self.delete_staged_credential(previous_credential).await;
        }
        if let Some(previous) = saved.previous.filter(|previous| {
            previous.configuration.source_id != saved.configured.configuration.source_id
        }) {
            self.remove_replaced_source_data(previous.configuration.source_id)
                .await;
        }
    }

    pub(super) async fn remove_replaced_source_data(&self, source_id: SourceId) {
        self.remove_configured_feed(&source_id);
        self.shared
            .downloads
            .settings_changed(self.shared.settings.load().ui.downloads);
        let library = self
            .shared
            .selected()
            .filter(|selected| selected.source_id() == &source_id)
            .map(|selected| Arc::clone(&selected.library));
        self.shared
            .downloads
            .clear(source_id.clone(), library, false);
        let library = self.shared.library.clone();
        let source_for_store = source_id.clone();
        if let Err(error) = blocking(move || {
            library
                .remove_source_data(&source_for_store)
                .map_err(string_error)
        })
        .await
        {
            self.shared.warn_nonfatal(&error);
        }
        if let Err(error) = self.shared.artwork.invalidate_source(&source_id) {
            self.shared.warn_nonfatal(&error.to_string());
        }
    }
}

pub(super) async fn add_source(
    owner: &mut SourceOwner,
    input: SourceSetup,
    progress: Arc<dyn Fn(SourceReadProgress) + Send + Sync>,
    cancelled: Arc<AtomicBool>,
) -> Result<(), String> {
    let shared = Arc::clone(&owner.shared);
    let setup = source_setup_input(input, &shared.settings.load().jellyfin_device_id);
    let connected = Source::connect(setup).await.map_err(string_error)?;
    if cancelled.load(Ordering::Acquire) {
        return Err("source connection was cancelled".to_string());
    }
    let (configuration, source, credential) = connected.into_parts();
    let identity = configuration.input_identity().map_err(string_error)?;
    let source = Arc::new(source);
    let prepared = prepare_source_candidate(
        &shared,
        Arc::clone(&source),
        identity,
        None,
        progress,
        Arc::clone(&cancelled),
    )
    .await?;
    if cancelled.load(Ordering::Acquire) {
        return Err("source connection was cancelled".to_string());
    }
    let previous = shared
        .settings
        .load()
        .sources
        .configured
        .iter()
        .find(|configured| configured.configuration.source_id == configuration.source_id)
        .cloned();
    if cancelled.load(Ordering::Acquire) {
        return Err("source selection was cancelled".to_string());
    }
    owner
        .commit_selected_connection(
            previous,
            configuration,
            Some(source),
            credential,
            PreparedConnectionLibrary::Candidate(Box::new(prepared)),
            Arc::clone(&cancelled),
            true,
        )
        .await
}
pub(super) async fn select_source(
    owner: &mut SourceOwner,
    configured: ConfiguredSource,
    progress: Arc<dyn Fn(SourceReadProgress) + Send + Sync>,
    cancelled: Arc<AtomicBool>,
    protect_commit: bool,
) -> Result<(), String> {
    let shared = Arc::clone(&owner.shared);
    let configuration = configured.configuration.clone();
    let identity = configuration.input_identity().map_err(string_error)?;
    let library = shared.library.clone();
    let source_id = configuration.source_id.clone();
    let source_id_for_store = source_id.clone();
    let cached = blocking(move || {
        library
            .load_source(&source_id_for_store)
            .map_err(string_error)
    })
    .await
    .unwrap_or_else(|error| {
        warn!(%error, "the selected source cache will be rebuilt");
        None
    });
    let cached = match cached {
        Some(loaded) => {
            let cache_match = configuration
                .cache_match(&loaded_input_identity(&loaded))
                .map_err(string_error)?;
            (cache_match != SourceCacheMatch::Incompatible).then_some((loaded, cache_match))
        }
        None => None,
    };
    let opened = if let Some(source) = shared.configured_feed_source(&source_id) {
        Ok(source)
    } else {
        match load_credential(&shared, &configured).await {
            Ok(credential) => Source::open(
                configuration.clone(),
                credential,
                Some(shared.settings.load().jellyfin_device_id),
            )
            .map(Arc::new)
            .map_err(string_error),
            Err(error) => Err(error),
        }
    };
    let source = match opened {
        Ok(source) => Some(source),
        Err(error) if cached.is_some() => {
            warn!(%error, %source_id, "live source access is unavailable; using cached library");
            None
        }
        Err(error) => return Err(error),
    };
    let cached_exact = cached
        .as_ref()
        .is_some_and(|(_, cache_match)| *cache_match == SourceCacheMatch::Exact);
    if configuration.kind == "jellyfin"
        && let Some(source) = source.as_ref()
    {
        owner.install_configured_jellyfin_feed(Arc::clone(source), cached_exact);
    }
    let prepared_library = if let Some((library, cache_match)) = cached {
        PreparedConnectionLibrary::Accepted {
            library,
            cache_match,
        }
    } else {
        let source = source.as_ref().ok_or_else(source_access_unavailable)?;
        owner.begin_configured_baseline(&source_id);
        let candidate = prepare_source_candidate(
            &shared,
            Arc::clone(source),
            identity,
            None,
            progress,
            Arc::clone(&cancelled),
        )
        .await?;
        if cancelled.load(Ordering::Acquire) {
            return Err("source selection was cancelled".to_string());
        }
        PreparedConnectionLibrary::Candidate(Box::new(candidate))
    };
    if cancelled.load(Ordering::Acquire) {
        return Err("source selection was cancelled".to_string());
    }
    owner
        .commit_selected_connection(
            Some(configured),
            configuration,
            source,
            None,
            prepared_library,
            Arc::clone(&cancelled),
            protect_commit,
        )
        .await
}
pub(super) async fn prepare_refresh_candidate(
    shared: Arc<Shared>,
    selected: SelectedSourceState,
    progress: Arc<dyn Fn(SourceReadProgress) + Send + Sync>,
    cancelled: Arc<AtomicBool>,
) -> Result<PreparedSourceCandidate, String> {
    let source = selected
        .source
        .as_ref()
        .cloned()
        .ok_or_else(source_access_unavailable)?;
    let identity = selected
        .configuration
        .input_identity()
        .map_err(string_error)?;
    prepare_source_candidate(
        &shared,
        source,
        identity,
        Some(Arc::clone(&selected.library)),
        progress,
        cancelled,
    )
    .await
}

pub(super) async fn prepare_configured_refresh_candidate(
    shared: &Arc<Shared>,
    source_id: &SourceId,
    progress: Arc<dyn Fn(SourceReadProgress) + Send + Sync>,
    cancelled: Arc<AtomicBool>,
) -> Result<PreparedSourceCandidate, String> {
    let configured = configured_source(&shared.settings.load().sources, source_id)?;
    let configuration = configured.configuration.clone();
    let credential = load_credential(shared, &configured).await?;
    let source = if let Some(source) = shared.configured_feed_source(source_id) {
        source
    } else {
        Arc::new(
            Source::open(
                configuration.clone(),
                credential,
                Some(shared.settings.load().jellyfin_device_id),
            )
            .map_err(string_error)?,
        )
    };
    let library = shared.library.clone();
    let source_for_store = source_id.clone();
    let base =
        blocking(move || library.load_source(&source_for_store).map_err(string_error)).await?;
    let identity = configuration.input_identity().map_err(string_error)?;
    prepare_source_candidate(shared, source, identity, base, progress, cancelled).await
}
pub(super) async fn prepare_source_candidate(
    shared: &Shared,
    source: Arc<Source>,
    identity: SourceInputIdentity,
    base: Option<Arc<Library>>,
    progress: Arc<dyn Fn(SourceReadProgress) + Send + Sync>,
    cancelled: Arc<AtomicBool>,
) -> Result<PreparedSourceCandidate, String> {
    let prepared = Arc::clone(&source)
        .prepare_library_candidate(
            shared.library.clone(),
            identity,
            base,
            Arc::clone(&progress),
            Arc::clone(&cancelled),
        )
        .await
        .map_err(string_error)?;
    prepare_candidate_artwork(shared, source, prepared, progress, cancelled).await
}
pub(super) async fn prepare_candidate_artwork(
    shared: &Shared,
    source: Arc<Source>,
    prepared: PreparedSourceCandidate,
    progress: Arc<dyn Fn(SourceReadProgress) + Send + Sync>,
    cancelled: Arc<AtomicBool>,
) -> Result<PreparedSourceCandidate, String> {
    if prepared.change() != CandidateChange::Library {
        return Ok(prepared);
    }
    let artwork = shared.artwork.clone();
    blocking(move || {
        if cancelled.load(Ordering::Acquire) {
            return Err("source artwork preparation was cancelled".to_string());
        }
        let source_artwork = prepared.library().source_artwork().map_err(string_error)?;
        let total = source_artwork.len();
        progress(SourceReadProgress {
            stage: SourceReadStage::Artwork,
            completed: 0,
            total: Some(total),
        });
        let progress_update = Arc::clone(&progress);
        let cancellation = Arc::clone(&cancelled);
        let summary = artwork
            .prepare_source_artwork(
                SourceImages::new(Arc::clone(&source)),
                Arc::clone(&source_artwork),
                &move |completed, total| {
                    progress_update(SourceReadProgress {
                        stage: SourceReadStage::Artwork,
                        completed,
                        total: Some(total),
                    });
                },
                &move || cancellation.load(Ordering::Acquire),
            )
            .map_err(string_error)?;
        if summary.failed > 0 {
            warn!(
                source_id = %source.source_id(),
                failed = summary.failed,
                total = summary.total,
                "some source artwork remains available for retry"
            );
        }
        Ok(prepared)
    })
    .await
}
pub(super) async fn load_credential(
    shared: &Shared,
    configured: &ConfiguredSource,
) -> Result<Option<String>, String> {
    let Some(reference) = configured.credential_ref.clone() else {
        return Ok(None);
    };
    let secrets = Arc::clone(&shared.secrets);
    blocking(move || load_provider_secret(&secrets, &reference)).await
}
pub(super) fn cache_input_matches(identity: &SourceInputIdentity, loaded: &Library) -> bool {
    loaded_input_identity(loaded) == *identity
}
pub(super) fn loaded_input_identity(loaded: &Library) -> SourceInputIdentity {
    SourceInputIdentity {
        source_id: loaded.source_id().clone(),
        digest: *loaded.input_digest(),
    }
}
pub(super) fn configured_source(
    settings: &SourceSettings,
    source_id: &SourceId,
) -> Result<ConfiguredSource, String> {
    settings
        .configured
        .iter()
        .find(|source| &source.configuration.source_id == source_id)
        .cloned()
        .ok_or_else(|| "the configured source no longer exists".to_string())
}
pub(super) fn replacement_source(
    settings: &SourceSettings,
    removed: &SourceId,
) -> Option<SourceId> {
    settings
        .configured
        .iter()
        .find(|source| &source.configuration.source_id != removed)
        .map(|source| source.configuration.source_id.clone())
}
pub(super) fn save_music_folder(
    settings: &SettingsFile,
    source_id: &SourceId,
    folder_id: Option<MusicFolderId>,
) -> Result<(), String> {
    settings.update(|stored| {
        let configured = stored
            .sources
            .configured
            .iter_mut()
            .find(|configured| configured.configuration.source_id == *source_id)
            .ok_or_else(|| "the configured source no longer exists".to_string())?;
        configured.music_folder_id = folder_id;
        Ok(())
    })
}
