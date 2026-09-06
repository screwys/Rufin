//! Scrobbling settings, credentials, and account authorization.
//!
//! The Scrobbling crate owns service protocols and retry delivery. This owner
//! joins those operations to Rufin's settings and secret storage while the UI
//! sees only editable preferences and connection progress.

use std::sync::Arc;
use std::thread;

use async_channel::{Receiver, bounded};
use scrobbling::{AudioscrobblerAuthorization, AudioscrobblerSession, Scrobbler};
use secrets::SwitchableSecretStore;
use tracing::warn;
use ui::runtime::{
    LastFmPreferences, LibreFmPreferences, ListenBrainzPreferences, ScrobblingConnection,
    ScrobblingConnectionEvent, ScrobblingPort, ScrobblingPreferences,
};

use crate::playback::PlaybackOwner;
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
pub(crate) struct ScrobblingOwner {
    settings: SettingsFile,
    secrets: Arc<SwitchableSecretStore>,
    runtime: tokio::runtime::Handle,
    scrobbler: Arc<Scrobbler>,
    playback: Arc<PlaybackOwner>,
}

impl ScrobblingOwner {
    pub(crate) fn new(
        settings: SettingsFile,
        secrets: Arc<SwitchableSecretStore>,
        runtime: tokio::runtime::Handle,
        scrobbler: Arc<Scrobbler>,
        playback: Arc<PlaybackOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings,
            secrets,
            runtime,
            scrobbler,
            playback,
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
            let settings = load_scrobbling_settings(&self.settings, &self.secrets);
            if let Err(error) = self.scrobbler.update_credentials(settings) {
                warn!(%error, "could not update external scrobbling credentials");
            }
        }
    }

    pub(crate) fn start(self: &Arc<Self>) {
        let owner = Arc::clone(self);
        self.runtime.spawn_blocking(move || {
            if let Err(error) = owner.scrobbler.load_credentials(|| {
                crate::settings::startup_scrobbling_settings(&owner.settings, &owner.secrets)
            }) {
                warn!(%error, "could not load saved scrobbling credentials");
            }
        });
    }

    fn save_preferences(
        &self,
        preferences: &ScrobblingPreferences,
    ) -> Result<ScrobblingPreferences, String> {
        let mut settings = load_scrobbling_settings(&self.settings, &self.secrets);
        let api_key = preferences.lastfm.api_key.trim().to_string();
        let api_secret = preferences.lastfm.api_secret.trim().to_string();
        if settings.lastfm.api_key != api_key || settings.lastfm.api_secret != api_secret {
            settings.lastfm.username.clear();
            settings.lastfm.session_key.clear();
        }
        settings.lastfm.enabled = preferences.lastfm.enabled;
        settings.lastfm.api_key = api_key;
        settings.lastfm.api_secret = api_secret;
        settings.lastfm.now_playing_enabled = preferences.lastfm.now_playing_enabled;
        settings.librefm.enabled = preferences.librefm.enabled;
        settings.librefm.now_playing_enabled = preferences.librefm.now_playing_enabled;
        settings.listenbrainz.enabled = preferences.listenbrainz.enabled;
        settings.listenbrainz.user_token = preferences.listenbrainz.user_token.trim().to_string();
        settings.listenbrainz.now_playing_enabled = preferences.listenbrainz.now_playing_enabled;
        self.commit(settings)
    }

    fn commit(&self, settings: scrobbling::Settings) -> Result<ScrobblingPreferences, String> {
        let committed = persist_scrobbling_settings(&self.settings, &self.secrets, &settings)?;
        let private_mode = self.settings.load().ui.private_mode;
        if let Err(error) = self
            .scrobbler
            .update_settings(committed.clone(), private_mode)
        {
            warn!(%error, "could not update external scrobbling settings");
        }
        self.playback.update_discord_settings();
        Ok(preferences(&committed))
    }

    fn connect_account(
        &self,
        request: ScrobblingConnection,
    ) -> Receiver<ScrobblingConnectionEvent> {
        let (events, receiver) = bounded(2);
        let owner = self.clone();
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
                Ok(Some(session)) => match owner.save_session(&request, session) {
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
    ) -> Result<String, String> {
        let mut settings = load_scrobbling_settings(&self.settings, &self.secrets);
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
        self.commit(settings)?;
        Ok(session.username)
    }
}

impl ScrobblingPort for ScrobblingOwner {
    fn preferences(&self) -> ScrobblingPreferences {
        preferences(&load_scrobbling_settings(&self.settings, &self.secrets))
    }

    fn save(&self, preferences: &ScrobblingPreferences) -> Result<ScrobblingPreferences, String> {
        self.save_preferences(preferences)
    }

    fn connect(&self, request: ScrobblingConnection) -> Receiver<ScrobblingConnectionEvent> {
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
