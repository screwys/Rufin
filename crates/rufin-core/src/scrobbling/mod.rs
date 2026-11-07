//! Scrobbling settings, credentials, and account authorization.
//!
//! The service protocols and retry delivery live beside this owner, which joins them to
//! Rufin's settings and secret storage while the UI sees only editable preferences and
//! connection progress.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::runtime::{
    LastFmPreferences, LibreFmPreferences, ListenBrainzPreferences, ScrobblingConnection,
    ScrobblingConnectionEvent, ScrobblingPreferences,
};
use async_channel::{Receiver, bounded};
use library::{Change, Scope};
mod retry;
mod services;
mod settings;

pub use retry::Scrobbler;
use secrets::{SecretStore, SwitchableSecretStore};
pub use services::audioscrobbler::{AudioscrobblerAuthorization, AudioscrobblerSession};
pub use services::listenbrainz::connect as connect_listenbrainz;
pub use settings::{
    AudioscrobblerSettings, LASTFM_API_SECRET, LASTFM_SESSION, LIBREFM_SESSION, LISTENBRAINZ_TOKEN,
    ListenBrainzSettings, SecretDescriptor, Settings, secret_descriptors,
};
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

/// Recorded listens are sent as they are heard while an account is signed in.
pub(crate) struct Listens(pub(crate) std::sync::Weak<Scrobbler>);

impl crate::features::Feature for Listens {
    fn wanted(&self, settings: &crate::settings::StoredSettings, _: &library::Catalog) -> bool {
        settings.scrobbling_secrets_present || !settings.scrobbling_credentials.is_empty()
    }

    fn hears(&self, scope: &Scope) -> bool {
        matches!(scope, Scope::Listened { .. })
    }

    fn apply(self: Arc<Self>, _: Option<Vec<Arc<Change>>>) -> crate::features::Pass {
        if let Some(scrobbler) = self.0.upgrade() {
            scrobbler.wake();
        }
        Box::pin(std::future::ready(()))
    }
}

#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct ScrobblingOwner {
    settings: SettingsFile,
    secrets: Arc<SwitchableSecretStore>,
    runtime: tokio::runtime::Handle,
    scrobbler: Arc<Scrobbler>,
    /// Held while the accounts' secrets are read or written.
    credential_work: Arc<Mutex<()>>,
    /// Set while a read of the secrets waits to start.
    reload_queued: Arc<AtomicBool>,
}

impl ScrobblingOwner {
    pub(crate) fn new(
        settings: SettingsFile,
        secrets: Arc<SwitchableSecretStore>,
        runtime: tokio::runtime::Handle,
        scrobbler: Arc<Scrobbler>,
    ) -> Arc<Self> {
        let owner = Arc::new(Self {
            settings,
            secrets,
            runtime,
            scrobbler,
            credential_work: Arc::new(Mutex::new(())),
            reload_queued: Arc::new(AtomicBool::new(false)),
        });
        // The worker reads the switches and private mode when it sends; the secrets are read
        // again when the settings name other accounts, as a restore or Connect does.
        crate::settings::follow(
            &owner.runtime,
            &owner.settings,
            Arc::downgrade(&owner),
            |owner, previous, current| {
                if owner.scrobbler.stale(current) {
                    owner.reload_credentials();
                }
                if previous.ui.application.private_mode != current.ui.application.private_mode
                    || previous.scrobbling != current.scrobbling
                {
                    owner.scrobbler.wake();
                }
                std::future::ready(())
            },
        );
        owner
    }

    fn reload_credentials(&self) {
        if self.reload_queued.swap(true, Ordering::AcqRel) {
            return;
        }
        let owner = self.clone();
        self.runtime.spawn_blocking(move || {
            owner.reload_queued.store(false, Ordering::Release);
            owner.load_credentials();
        });
    }

    pub(crate) fn start(&self) {
        self.reload_credentials();
    }

    /// Reads the accounts' secrets, which waits while the keyring is locked. Accounts the
    /// settings name differently by then are not installed: the change reads them again.
    fn load_credentials(&self) {
        let _work = self
            .credential_work
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let read_from = self.settings.load();
        let settings = load_scrobbling_settings(&self.settings, &self.secrets);
        if !read_from.same_scrobbling_accounts(&self.settings.load()) {
            return;
        }
        if let Err(error) = self.scrobbler.install(settings, read_from) {
            warn!(%error, "could not load saved scrobbling credentials");
        }
    }

    fn commit(&self, settings: Settings, scope: &str) -> Result<ScrobblingPreferences, String> {
        persist_scrobbling_settings(&self.settings, &self.secrets, &settings, scope)?;
        if let Err(error) = self.scrobbler.install(settings, self.settings.load()) {
            warn!(%error, "could not update external scrobbling settings");
        }
        Ok(preferences(&self.scrobbler.settings()))
    }

    fn connect_account(
        &self,
        request: ScrobblingConnection,
    ) -> Receiver<ScrobblingConnectionEvent> {
        let (events, receiver) = bounded(2);
        let owner = self.clone();
        let scope = self.settings.load().secret_scope_id.clone();
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
                    opened: Arc::new(crate::runtime::scrobbling::BrowserOpening(opened)),
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
    fn loaded_settings(&self) -> Settings {
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
        let scope = self.settings.load().secret_scope_id.clone();
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

    pub fn shutdown(&self) {
        self.scrobbler.shutdown();
        self.scrobbler.wake();
    }
}

/// The service switches `preferences` set.
fn switches(settings: &mut Settings, preferences: &ScrobblingPreferences) {
    settings.lastfm.enabled = preferences.lastfm.enabled;
    settings.lastfm.now_playing_enabled = preferences.lastfm.now_playing_enabled;
    settings.librefm.enabled = preferences.librefm.enabled;
    settings.librefm.now_playing_enabled = preferences.librefm.now_playing_enabled;
    settings.listenbrainz.enabled = preferences.listenbrainz.enabled;
    settings.listenbrainz.now_playing_enabled = preferences.listenbrainz.now_playing_enabled;
}

fn preferences(settings: &Settings) -> ScrobblingPreferences {
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
            username: settings.listenbrainz.username.clone(),
            user_token: settings.listenbrainz.user_token.clone(),
            now_playing_enabled: settings.listenbrainz.now_playing_enabled,
        },
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ScrobblingOwner {
    pub fn preferences(&self) -> ScrobblingPreferences {
        preferences(&self.scrobbler.settings())
    }

    pub fn save(
        &self,
        preferences: ScrobblingPreferences,
    ) -> Result<ScrobblingPreferences, crate::settings::SettingsError> {
        self.settings
            .edit(|stored| {
                switches(&mut stored.scrobbling, &preferences);
                Ok(())
            })
            .map_err(crate::settings::SettingsError::from)?;
        // The worker reads the switches when it sends; credentials it lacks are read here.
        let applied = self.scrobbler.settings();
        let stored = self.settings.load();
        let needs_credentials = (stored.scrobbling_secrets_present
            || !stored.scrobbling_credentials.is_empty())
            && ((preferences.lastfm.enabled && applied.lastfm.session_key.is_empty())
                || (preferences.librefm.enabled && applied.librefm.session_key.is_empty())
                || (preferences.listenbrainz.enabled
                    && applied.listenbrainz.user_token.is_empty()));
        if needs_credentials {
            self.reload_credentials();
        }
        Ok(self::preferences(&applied))
    }

    pub async fn connect_listenbrainz(
        &self,
        user_token: String,
    ) -> Result<ScrobblingPreferences, crate::settings::SettingsError> {
        let owner = self.clone();
        let scope = self.settings.load().secret_scope_id.clone();
        self.runtime
            .spawn_blocking(move || {
                let result = connect_listenbrainz(&user_token).and_then(|username| {
                    let _work = owner
                        .credential_work
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    let mut settings = owner.loaded_settings();
                    settings.listenbrainz.user_token = user_token;
                    settings.listenbrainz.username = username;
                    owner.commit(settings, &scope)
                });
                result
            })
            .await
            .map_err(|error| crate::settings::SettingsError::from(error.to_string()))?
            .map_err(Into::into)
    }

    pub async fn load_preferences(
        &self,
    ) -> Result<ScrobblingPreferences, crate::settings::SettingsError> {
        let owner = self.clone();
        self.runtime
            .spawn_blocking(move || {
                if owner.scrobbler.stale(&owner.settings.load()) {
                    owner.load_credentials();
                }
                owner.preferences()
            })
            .await
            .map_err(|error| crate::settings::SettingsError::from(error.to_string()))
    }

    pub fn connect(
        &self,
        request: ScrobblingConnection,
    ) -> Arc<crate::runtime::scrobbling::ScrobblingSession> {
        Arc::new(crate::runtime::scrobbling::ScrobblingSession(
            self.connect_account(request),
        ))
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ScrobblingCredentialEdit {
    LastFm {
        api_key: Option<String>,
        api_secret: Option<String>,
    },
    ListenBrainz {
        user_token: String,
    },
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl ScrobblingOwner {
    pub async fn save_credential(
        &self,
        edit: ScrobblingCredentialEdit,
    ) -> Result<ScrobblingPreferences, crate::settings::SettingsError> {
        let owner = self.clone();
        let scope = self.settings.load().secret_scope_id.clone();
        self.runtime
            .spawn_blocking(move || {
                let _work = owner
                    .credential_work
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let mut settings = owner.loaded_settings();
                let mut preferences = preferences(&settings);
                match edit {
                    ScrobblingCredentialEdit::LastFm {
                        api_key,
                        api_secret,
                    } => {
                        if let Some(value) = api_key {
                            preferences.lastfm.api_key = value;
                        }
                        if let Some(value) = api_secret {
                            preferences.lastfm.api_secret = value;
                        }
                    }
                    ScrobblingCredentialEdit::ListenBrainz { user_token } => {
                        preferences.listenbrainz.user_token = user_token
                    }
                }
                if settings.lastfm.api_key != preferences.lastfm.api_key
                    || settings.lastfm.api_secret != preferences.lastfm.api_secret
                {
                    settings.lastfm.username.clear();
                    settings.lastfm.session_key.clear();
                }
                settings.lastfm.api_key = preferences.lastfm.api_key.trim().into();
                settings.lastfm.api_secret = preferences.lastfm.api_secret.trim().into();
                if settings.listenbrainz.user_token != preferences.listenbrainz.user_token.trim() {
                    settings.listenbrainz.username.clear();
                }
                settings.listenbrainz.user_token =
                    preferences.listenbrainz.user_token.trim().into();
                owner.commit(settings, &scope)
            })
            .await
            .map_err(|error| crate::settings::SettingsError::from(error.to_string()))?
            .map_err(Into::into)
    }
}
