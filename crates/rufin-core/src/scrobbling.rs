//! Scrobbling settings, credentials, and account authorization.
//!
//! The Scrobbling crate owns service protocols and retry delivery. This owner
//! joins those operations to Rufin's settings and secret storage while the UI
//! sees only editable preferences and connection progress.

use std::sync::{Arc, Mutex};
use std::thread;

use crate::runtime::{
    LastFmPreferences, LibreFmPreferences, ListenBrainzPreferences, ScrobblingConnection,
    ScrobblingConnectionEvent, ScrobblingPreferences,
};
use async_channel::{Receiver, bounded};
use scrobbling::{AudioscrobblerAuthorization, AudioscrobblerSession, Scrobbler};
use secrets::{SecretStore, SwitchableSecretStore};
use tracing::warn;

use crate::settings::{SettingsFile, load_scrobbling_settings, persist_scrobbling_settings};

const AUTHORIZATION_THREAD_NAME: &str = "rufin-scrobbling-auth";
const AUTHORIZATION_TASK_FAILED: &str = "Scrobbling authorization task failed.";

async fn wait_for_authorization(
    authorization: AudioscrobblerAuthorization,
) -> Result<Option<AudioscrobblerSession>, String> {
    let (completed, session) = tokio::sync::oneshot::channel();
    let thread = thread::Builder::new()
        .name(AUTHORIZATION_THREAD_NAME.to_string())
        .spawn(move || {
            let _ = completed.send(authorization.wait_for_session());
        })
        .map_err(|_| AUTHORIZATION_TASK_FAILED.to_string())?;
    let session = session.await;
    thread
        .join()
        .map_err(|_| AUTHORIZATION_TASK_FAILED.to_string())?;
    session.map_err(|_| AUTHORIZATION_TASK_FAILED.to_string())?
}

#[derive(Clone)]
pub struct ScrobblingOwner {
    settings: SettingsFile,
    secrets: Arc<SwitchableSecretStore>,
    runtime: tokio::runtime::Handle,
    scrobbler: Arc<Scrobbler>,
    settings_committed: Arc<dyn Fn(&crate::settings::Settings) + Send + Sync>,
    credential_work: Arc<Mutex<()>>,
}

impl ScrobblingOwner {
    pub(crate) fn new(
        settings: SettingsFile,
        secrets: Arc<SwitchableSecretStore>,
        runtime: tokio::runtime::Handle,
        scrobbler: Arc<Scrobbler>,
        settings_committed: Arc<dyn Fn(&crate::settings::Settings) + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings,
            secrets,
            runtime,
            scrobbler,
            settings_committed,
            credential_work: Arc::new(Mutex::new(())),
        })
    }

    pub(crate) fn settings_changed(&self, credentials_changed: bool) {
        let stored = self.settings.load();
        if let Err(error) = self.scrobbler.update_preferences(
            &stored.scrobbling_runtime_settings(),
            stored.ui.private_mode,
        ) {
            warn!(%error, "could not update external scrobbling settings");
        }
        if credentials_changed {
            self.load_preferences();
        }
    }

    pub(crate) fn start(self: &Arc<Self>) {
        let owner = Arc::clone(self);
        self.runtime.spawn_blocking(move || {
            let _work = owner
                .credential_work
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Err(error) = owner.scrobbler.load_credentials(|| {
                crate::settings::startup_scrobbling_settings(&owner.settings, &owner.secrets)
            }) {
                warn!(%error, "could not load saved scrobbling credentials");
            }
        });
    }

    fn commit(
        &self,
        settings: scrobbling::Settings,
        scope: &str,
    ) -> Result<ScrobblingPreferences, String> {
        let committed =
            persist_scrobbling_settings(&self.settings, &self.secrets, &settings, scope)?;
        if let Err(error) = self.scrobbler.update_credentials(committed.clone()) {
            warn!(%error, "could not update external scrobbling settings");
        }
        (self.settings_committed)(&self.settings.load().ui);
        Ok(preferences(&committed))
    }

    fn connect_account(
        &self,
        request: ScrobblingConnection,
    ) -> Receiver<ScrobblingConnectionEvent> {
        let (events, receiver) = bounded(2);
        let owner = self.clone();
        let scope = self.settings.load().secret_scope_id;
        self.runtime.spawn(async move {
            let authorization = match request {
                ScrobblingConnection::LastFm {
                    ref api_key,
                    ref api_secret,
                } => {
                    let api_key = api_key.clone();
                    let api_secret = api_secret.clone();
                    tokio::task::spawn_blocking(move || {
                        AudioscrobblerAuthorization::lastfm(&api_key, &api_secret)
                    })
                    .await
                    .map_err(|_| "Last.fm authorization task failed.".to_string())
                    .and_then(|result| result)
                }
                ScrobblingConnection::LibreFm => {
                    tokio::task::spawn_blocking(AudioscrobblerAuthorization::librefm)
                        .await
                        .map_err(|_| "Libre.fm authorization task failed.".to_string())
                        .and_then(|result| result)
                }
            };
            let authorization = match authorization {
                Ok(authorization) => authorization,
                Err(error) => {
                    let _ = events.send(ScrobblingConnectionEvent::Failed(error)).await;
                    return;
                }
            };

            let (opened, opened_result) = bounded(1);
            if events
                .send(ScrobblingConnectionEvent::OpenUrl {
                    url: authorization.url().to_string(),
                    opened,
                })
                .await
                .is_err()
            {
                return;
            }
            match opened_result.recv().await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    let _ = events.send(ScrobblingConnectionEvent::Failed(error)).await;
                    return;
                }
                Err(_) => return,
            }

            let session = wait_for_authorization(authorization).await;
            match session {
                Ok(Some(session)) => match tokio::task::spawn_blocking(move || {
                    owner.save_session(&request, session, &scope)
                })
                .await
                .unwrap_or_else(|error| Err(error.to_string()))
                {
                    Ok(username) => {
                        let _ = events
                            .send(ScrobblingConnectionEvent::Connected { username })
                            .await;
                    }
                    Err(error) => {
                        let _ = events.send(ScrobblingConnectionEvent::Failed(error)).await;
                    }
                },
                Ok(None) => {
                    let _ = events.send(ScrobblingConnectionEvent::TimedOut).await;
                }
                Err(error) => {
                    let _ = events.send(ScrobblingConnectionEvent::Failed(error)).await;
                }
            }
        });
        receiver
    }

    fn save_session(
        &self,
        request: &ScrobblingConnection,
        session: AudioscrobblerSession,
        scope: &str,
    ) -> Result<String, String> {
        let _work = self
            .credential_work
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut settings = self.loaded_settings();
        match request {
            ScrobblingConnection::LastFm {
                api_key,
                api_secret,
            } => {
                settings.lastfm.api_key = api_key.trim().to_string();
                settings.lastfm.api_secret = api_secret.trim().to_string();
                settings.lastfm.username = session.username.clone();
                settings.lastfm.session_key = session.session_key;
            }
            ScrobblingConnection::LibreFm => {
                settings.librefm.username = session.username.clone();
                settings.librefm.session_key = session.session_key;
            }
        }
        self.commit(settings, scope)?;
        Ok(session.username)
    }
}

impl ScrobblingOwner {
    fn loaded_settings(&self) -> scrobbling::Settings {
        if self.secrets.is_persistent() {
            load_scrobbling_settings(&self.settings, &self.secrets)
        } else {
            self.scrobbler.settings()
        }
    }

    pub fn save_credentials_to_file(
        &self,
        storage: crate::settings::KeyringSecretStore,
    ) -> Receiver<Result<(), String>> {
        let (sender, receiver) = bounded(1);
        let owner = self.clone();
        let scope = self.settings.load().secret_scope_id;
        self.runtime.spawn_blocking(move || {
            let _work = owner
                .credential_work
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let result = storage
                .save_to_file()
                .map_err(|error| error.to_string())
                .and_then(|()| owner.commit(owner.scrobbler.settings(), &scope).map(|_| ()));
            let _ = sender.send_blocking(result);
        });
        receiver
    }

    pub fn preferences(&self) -> ScrobblingPreferences {
        preferences(&self.scrobbler.settings())
    }

    pub fn save(
        &self,
        preferences: &ScrobblingPreferences,
    ) -> Result<ScrobblingPreferences, String> {
        self.settings.update(|stored| {
            stored.scrobbling.lastfm.enabled = preferences.lastfm.enabled;
            stored.scrobbling.lastfm.now_playing_enabled = preferences.lastfm.now_playing_enabled;
            stored.scrobbling.librefm.enabled = preferences.librefm.enabled;
            stored.scrobbling.librefm.now_playing_enabled = preferences.librefm.now_playing_enabled;
            stored.scrobbling.listenbrainz.enabled = preferences.listenbrainz.enabled;
            stored.scrobbling.listenbrainz.now_playing_enabled =
                preferences.listenbrainz.now_playing_enabled;
            Ok(())
        })?;
        let loaded = self.scrobbler.settings();
        let needs_credentials = self.settings.load().scrobbling_secrets_present
            && ((preferences.lastfm.enabled && loaded.lastfm.session_key.is_empty())
                || (preferences.librefm.enabled && loaded.librefm.session_key.is_empty())
                || (preferences.listenbrainz.enabled && loaded.listenbrainz.user_token.is_empty()));
        self.settings_changed(needs_credentials);
        Ok(self.preferences())
    }

    pub fn save_credential(
        &self,
        update: impl FnOnce(&mut ScrobblingPreferences) + Send + 'static,
    ) -> Receiver<Result<ScrobblingPreferences, String>> {
        let (sender, receiver) = bounded(1);
        let owner = self.clone();
        let scope = self.settings.load().secret_scope_id;
        self.runtime.spawn_blocking(move || {
            let _work = owner
                .credential_work
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let mut settings = owner.loaded_settings();
            let mut preferences = preferences(&settings);
            update(&mut preferences);
            if settings.lastfm.api_key != preferences.lastfm.api_key
                || settings.lastfm.api_secret != preferences.lastfm.api_secret
            {
                settings.lastfm.username.clear();
                settings.lastfm.session_key.clear();
            }
            settings.lastfm.api_key = preferences.lastfm.api_key.trim().into();
            settings.lastfm.api_secret = preferences.lastfm.api_secret.trim().into();
            settings.listenbrainz.user_token = preferences.listenbrainz.user_token.trim().into();
            let _ = sender.send_blocking(owner.commit(settings, &scope));
        });
        receiver
    }

    pub fn load_preferences(&self) -> Receiver<ScrobblingPreferences> {
        let (sender, receiver) = bounded(1);
        let owner = self.clone();
        self.runtime.spawn_blocking(move || {
            let _work = owner
                .credential_work
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Err(error) = owner.scrobbler.load_credentials(|| owner.loaded_settings()) {
                warn!(%error, "could not load scrobbling credentials");
            }
            let _ = sender.send_blocking(owner.preferences());
        });
        receiver
    }

    pub fn storage_reset(&self) {
        if let Err(error) = self
            .scrobbler
            .update_credentials(self.settings.load().scrobbling_runtime_settings())
        {
            warn!(%error, "could not reset scrobbling credentials");
        }
    }

    pub fn connect(&self, request: ScrobblingConnection) -> Receiver<ScrobblingConnectionEvent> {
        self.connect_account(request)
    }
}

fn preferences(settings: &scrobbling::Settings) -> ScrobblingPreferences {
    ScrobblingPreferences {
        lastfm: LastFmPreferences {
            enabled: settings.lastfm.enabled,
            api_key: settings.lastfm.api_key.clone(),
            api_secret: settings.lastfm.api_secret.clone(),
            username: settings.lastfm.username.clone(),
            connected: !settings.lastfm.session_key.is_empty(),
            now_playing_enabled: settings.lastfm.now_playing_enabled,
        },
        librefm: LibreFmPreferences {
            enabled: settings.librefm.enabled,
            username: settings.librefm.username.clone(),
            connected: !settings.librefm.session_key.is_empty(),
            now_playing_enabled: settings.librefm.now_playing_enabled,
        },
        listenbrainz: ListenBrainzPreferences {
            enabled: settings.listenbrainz.enabled,
            user_token: settings.listenbrainz.user_token.clone(),
            now_playing_enabled: settings.listenbrainz.now_playing_enabled,
        },
    }
}
