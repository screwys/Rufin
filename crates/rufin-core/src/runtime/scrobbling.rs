use std::sync::Arc;

use async_channel::Sender;

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LastFmPreferences {
    pub enabled: bool,
    /// The user's own API account; empty when Rufin's is used.
    pub api_key: String,
    pub api_secret: String,
    pub username: String,
    pub connected: bool,
    pub now_playing_enabled: bool,
}

impl Default for LastFmPreferences {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key: String::new(),
            api_secret: String::new(),
            username: String::new(),
            connected: false,
            now_playing_enabled: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LibreFmPreferences {
    pub enabled: bool,
    pub username: String,
    pub connected: bool,
    pub now_playing_enabled: bool,
}

impl Default for LibreFmPreferences {
    fn default() -> Self {
        Self {
            enabled: false,
            username: String::new(),
            connected: false,
            now_playing_enabled: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ListenBrainzPreferences {
    pub enabled: bool,
    pub username: String,
    pub user_token: String,
    pub connected: bool,
    pub now_playing_enabled: bool,
}

impl Default for ListenBrainzPreferences {
    fn default() -> Self {
        Self {
            enabled: false,
            username: String::new(),
            user_token: String::new(),
            connected: false,
            now_playing_enabled: true,
        }
    }
}

pub use crate::scrobbling::ScrobblingProtocol;

/// A scrobbling server the user added.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ScrobblingServerPreferences {
    pub id: String,
    pub name: String,
    pub protocol: ScrobblingProtocol,
    pub url: String,
    pub enabled: bool,
    pub now_playing_enabled: bool,
    pub username: String,
    pub connected: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ScrobblingPreferences {
    pub lastfm: LastFmPreferences,
    pub librefm: LibreFmPreferences,
    pub listenbrainz: ListenBrainzPreferences,
    /// In the order they were added.
    pub servers: Vec<ScrobblingServerPreferences>,
}

/// A server to add, with its sign-in: a ListenBrainz server takes the user token as `secret`;
/// an Audioscrobbler server takes the username and the password as `secret`, which is sent once
/// for a session and not kept.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ScrobblingServerRequest {
    pub name: String,
    pub protocol: ScrobblingProtocol,
    pub url: String,
    pub username: String,
    pub secret: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ScrobblingConnection {
    LastFm { api_key: String, api_secret: String },
    LibreFm,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ScrobblingConnectionEvent {
    OpenUrl {
        url: String,
        opened: Arc<BrowserOpening>,
    },
    Connected {
        username: String,
    },
    TimedOut,
    Failed(String),
}

pub type ScrobblingHandle = Arc<crate::scrobbling::ScrobblingOwner>;

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct BrowserOpening(pub(crate) Sender<Result<(), String>>);
#[cfg_attr(feature = "bindings", uniffi::export)]
impl BrowserOpening {
    pub async fn complete(&self, failure: Option<String>) {
        let _ = self.0.send(failure.map_or(Ok(()), Err)).await;
    }
}
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct ScrobblingSession(pub(crate) async_channel::Receiver<ScrobblingConnectionEvent>);
#[cfg_attr(feature = "bindings", uniffi::export)]
impl ScrobblingSession {
    pub async fn next(&self) -> Result<ScrobblingConnectionEvent, crate::settings::SettingsError> {
        self.0
            .recv()
            .await
            .map_err(|error| error.to_string().into())
    }
}
