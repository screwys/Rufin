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
    ScrobblingConnectionEvent, ScrobblingPreferences, ScrobblingServerPreferences,
    ScrobblingServerRequest,
};
use async_channel::{Receiver, bounded};
use library::{Change, Scope};
mod retry;
mod services;
mod settings;

pub use retry::Scrobbler;
use secrets::{SecretStore, SwitchableSecretStore};
pub use services::audioscrobbler::{AudioscrobblerAuthorization, AudioscrobblerSession};
use services::{audioscrobbler, listenbrainz};
pub use settings::{
    AudioscrobblerSettings, ListenBrainzSettings, ScrobblingProtocol, SecretDescriptor,
    ServerSettings, Settings, lastfm_application, secret_descriptors,
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
        Ok(preferences(&self.scrobbler.settings(), &self.scrobbler))
    }

    /// Edits the accounts with their secrets and saves them, on a blocking thread.
    async fn edit_accounts(
        &self,
        edit: impl FnOnce(&mut Settings) -> Result<(), String> + Send + 'static,
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
                edit(&mut settings)?;
                owner.commit(settings, &scope)
            })
            .await
            .map_err(|error| crate::settings::SettingsError::from(error.to_string()))?
            .map_err(Into::into)
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
    /// `server` signed in: with the user token it accepts for ListenBrainz, or with the session
    /// key a username and password give for Audioscrobbler. The password is not kept.
    async fn signed_in(
        &self,
        mut server: ServerSettings,
        username: String,
        secret: String,
    ) -> Result<ServerSettings, crate::settings::SettingsError> {
        let (name, url) = (server.name.clone(), server.url.clone());
        let protocol = server.protocol;
        let (username, secret) = self
            .runtime
            .spawn_blocking(move || match protocol {
                ScrobblingProtocol::ListenBrainz => {
                    let secret = secret.trim().to_owned();
                    if secret.is_empty() {
                        return Err(localization::tr("Enter your user token first"));
                    }
                    let service = listenbrainz::Service::server(&name, &url);
                    listenbrainz::connect(&service, &secret).map(|username| (username, secret))
                }
                ScrobblingProtocol::Audioscrobbler => {
                    if username.trim().is_empty() || secret.is_empty() {
                        return Err(localization::tr("Enter your username and password."));
                    }
                    let service = audioscrobbler::Service::server(&name, &url);
                    audioscrobbler::mobile_session(&service, &username, &secret)
                        .map(|session| (session.username, session.session_key))
                }
            })
            .await
            .map_err(|error| crate::settings::SettingsError::from(error.to_string()))??;
        server.username = username;
        server.secret = secret;
        Ok(server)
    }

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
    for server in &mut settings.servers {
        if let Some(switches) = preferences.servers.iter().find(|s| s.id == server.id) {
            server.enabled = switches.enabled;
            server.now_playing_enabled = switches.now_playing_enabled;
        }
    }
}

/// What the clients show of `settings`. An account whose credentials stopped working shows
/// signed out.
fn preferences(settings: &Settings, scrobbler: &Scrobbler) -> ScrobblingPreferences {
    let signed_out = scrobbler.signed_out(settings);
    ScrobblingPreferences {
        lastfm: LastFmPreferences {
            enabled: settings.lastfm.enabled,
            api_key: settings.lastfm.api_key.clone(),
            api_secret: settings.lastfm.api_secret.clone(),
            username: settings.lastfm.username.clone(),
            connected: !settings.lastfm.session_key.is_empty()
                && !signed_out.iter().any(|account| account == settings::LASTFM),
            now_playing_enabled: settings.lastfm.now_playing_enabled,
        },
        librefm: LibreFmPreferences {
            enabled: settings.librefm.enabled,
            username: settings.librefm.username.clone(),
            connected: !settings.librefm.session_key.is_empty()
                && !signed_out
                    .iter()
                    .any(|account| account == settings::LIBREFM),
            now_playing_enabled: settings.librefm.now_playing_enabled,
        },
        listenbrainz: ListenBrainzPreferences {
            enabled: settings.listenbrainz.enabled,
            username: settings.listenbrainz.username.clone(),
            user_token: settings.listenbrainz.user_token.clone(),
            connected: !settings.listenbrainz.user_token.is_empty()
                && !signed_out
                    .iter()
                    .any(|account| account == settings::LISTENBRAINZ),
            now_playing_enabled: settings.listenbrainz.now_playing_enabled,
        },
        servers: settings
            .servers
            .iter()
            .map(|server| ScrobblingServerPreferences {
                id: server.id.clone(),
                name: server.name.clone(),
                protocol: server.protocol,
                url: server.url.clone(),
                enabled: server.enabled,
                now_playing_enabled: server.now_playing_enabled,
                username: server.username.clone(),
                connected: !server.secret.is_empty()
                    && !signed_out.contains(&settings::server_account(&server.id)),
            })
            .collect(),
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ScrobblingOwner {
    pub fn preferences(&self) -> ScrobblingPreferences {
        preferences(&self.scrobbler.settings(), &self.scrobbler)
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
                    && applied.listenbrainz.user_token.is_empty())
                || applied
                    .servers
                    .iter()
                    .any(|server| server.enabled && server.secret.is_empty()));
        if needs_credentials {
            self.reload_credentials();
        }
        Ok(self::preferences(&applied, &self.scrobbler))
    }

    /// Signs in to ListenBrainz with a user token it accepts.
    pub async fn connect_listenbrainz(
        &self,
        user_token: String,
    ) -> Result<ScrobblingPreferences, crate::settings::SettingsError> {
        let user_token = user_token.trim().to_owned();
        let token = user_token.clone();
        let username = self
            .runtime
            .spawn_blocking(move || {
                listenbrainz::connect(&listenbrainz::Service::listenbrainz(), &token)
            })
            .await
            .map_err(|error| crate::settings::SettingsError::from(error.to_string()))??;
        self.edit_accounts(move |settings| {
            settings.listenbrainz.user_token = user_token;
            settings.listenbrainz.username = username;
            Ok(())
        })
        .await
    }

    /// Adds a server, switched on, once it accepts the sign-in. The new server is the last of
    /// `servers`.
    pub async fn add_server(
        &self,
        request: ScrobblingServerRequest,
    ) -> Result<ScrobblingPreferences, crate::settings::SettingsError> {
        let server = ServerSettings {
            id: crate::settings::random_identity("")?,
            name: request.name.trim().to_owned(),
            protocol: request.protocol,
            url: request.url.trim().to_owned(),
            enabled: true,
            now_playing_enabled: true,
            username: String::new(),
            secret: String::new(),
        };
        if server.name.is_empty() || reqwest::Url::parse(&server.url).is_err() {
            return Err(localization::tr("Enter a name and the server's URL.").into());
        }
        let server = self
            .signed_in(server, request.username, request.secret)
            .await?;
        self.edit_accounts(move |settings| {
            settings.servers.push(server);
            Ok(())
        })
        .await
    }

    /// Signs in to a server again, with the credentials `ScrobblingServerRequest` takes.
    pub async fn sign_in_server(
        &self,
        id: String,
        username: String,
        secret: String,
    ) -> Result<ScrobblingPreferences, crate::settings::SettingsError> {
        let removed = || {
            crate::settings::SettingsError::from("The scrobbling server was removed.".to_string())
        };
        let server = self
            .scrobbler
            .settings()
            .server(&id)
            .cloned()
            .ok_or_else(removed)?;
        let server = self.signed_in(server, username, secret).await?;
        self.edit_accounts(move |settings| {
            let current = settings
                .servers
                .iter_mut()
                .find(|current| current.id == server.id)
                .ok_or("The scrobbling server was removed.")?;
            current.username = server.username;
            current.secret = server.secret;
            Ok(())
        })
        .await
    }

    /// Removes a server; scrobbles still waiting for it are dropped.
    pub async fn remove_server(
        &self,
        id: String,
    ) -> Result<ScrobblingPreferences, crate::settings::SettingsError> {
        self.edit_accounts(move |settings| {
            settings.servers.retain(|server| server.id != id);
            Ok(())
        })
        .await
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
                let mut preferences = preferences(&settings, &owner.scrobbler);
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
