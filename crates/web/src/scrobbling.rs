//! Integrations › Scrobbling: the accounts and servers GTK's Integrations page shows, through
//! the same scrobbling owner.
use super::*;
use rufin_core::runtime::{
    ScrobblingConnection, ScrobblingConnectionEvent, ScrobblingPreferences, ScrobblingProtocol,
    ScrobblingServerRequest,
};
use rufin_core::scrobbling::ScrobblingCredentialEdit;

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/api/scrobbling", get(read).post(write))
        .route("/api/scrobbling/connect", post(connect))
}

fn preferences_json(preferences: &ScrobblingPreferences) -> Value {
    let lastfm = &preferences.lastfm;
    let librefm = &preferences.librefm;
    let listenbrainz = &preferences.listenbrainz;
    json!({
        "lastfm": {
            "enabled": lastfm.enabled, "now_playing": lastfm.now_playing_enabled,
            "connected": lastfm.connected, "username": lastfm.username,
            "api_key": lastfm.api_key, "api_secret": lastfm.api_secret,
        },
        "listenbrainz": {
            "enabled": listenbrainz.enabled, "now_playing": listenbrainz.now_playing_enabled,
            "connected": listenbrainz.connected, "username": listenbrainz.username,
            "user_token": listenbrainz.user_token,
        },
        "librefm": {
            "enabled": librefm.enabled, "now_playing": librefm.now_playing_enabled,
            "connected": librefm.connected, "username": librefm.username,
        },
        "servers": preferences.servers.iter().map(|server| json!({
            "id": server.id, "name": server.name, "protocol": server.protocol, "url": server.url,
            "enabled": server.enabled, "now_playing": server.now_playing_enabled,
            "connected": server.connected, "username": server.username,
        })).collect::<Vec<_>>(),
    })
}

async fn read(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    let preferences = products
        .scrobbling
        .load_preferences()
        .await
        .map_err(internal)?;
    Ok(json_response(
        StatusCode::OK,
        preferences_json(&preferences),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Switch {
    Enabled,
    NowPlaying,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Edit {
    /// A switch of `account`: lastfm, librefm, listenbrainz or a server's id.
    Switch {
        account: String,
        switch: Switch,
        value: bool,
    },
    /// The Last.fm API key or shared secret.
    Credential {
        api_key: Option<String>,
        api_secret: Option<String>,
    },
    /// ListenBrainz with a user token.
    ConnectListenbrainz {
        user_token: String,
    },
    /// `secret` is a ListenBrainz server's user token or an Audioscrobbler server's password.
    AddServer {
        name: String,
        protocol: ScrobblingProtocol,
        url: String,
        #[serde(default)]
        username: String,
        secret: String,
    },
    SignInServer {
        id: String,
        #[serde(default)]
        username: String,
        secret: String,
    },
    RemoveServer {
        id: String,
    },
}

async fn write(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let edit: Edit = body(request).await?;
    let owner = &products.scrobbling;
    let preferences = match edit {
        Edit::Switch {
            account,
            switch,
            value,
        } => {
            let owner = owner.clone();
            tokio::task::spawn_blocking(move || {
                let mut preferences = owner.preferences();
                let (enabled, now_playing) = match account.as_str() {
                    "lastfm" => (
                        &mut preferences.lastfm.enabled,
                        &mut preferences.lastfm.now_playing_enabled,
                    ),
                    "librefm" => (
                        &mut preferences.librefm.enabled,
                        &mut preferences.librefm.now_playing_enabled,
                    ),
                    "listenbrainz" => (
                        &mut preferences.listenbrainz.enabled,
                        &mut preferences.listenbrainz.now_playing_enabled,
                    ),
                    id => {
                        let server = preferences
                            .servers
                            .iter_mut()
                            .find(|server| server.id == id)
                            .ok_or_else(|| bad_request("The scrobbling server was removed."))?;
                        (&mut server.enabled, &mut server.now_playing_enabled)
                    }
                };
                *match switch {
                    Switch::Enabled => enabled,
                    Switch::NowPlaying => now_playing,
                } = value;
                owner.save(preferences).map_err(bad_request)
            })
            .await
            .map_err(internal)??
        }
        Edit::Credential {
            api_key,
            api_secret,
        } => {
            let previous = owner.preferences().lastfm.api_key;
            let committed = owner
                .save_credential(ScrobblingCredentialEdit::LastFm {
                    api_key: api_key.map(|key| key.trim().to_owned()),
                    api_secret: api_secret.map(|secret| secret.trim().to_owned()),
                })
                .await
                .map_err(bad_request)?;
            // The key also finds covers.
            if committed.lastfm.api_key != previous {
                retry_artwork(&products);
            }
            committed
        }
        Edit::ConnectListenbrainz { user_token } => owner
            .connect_listenbrainz(user_token)
            .await
            .map_err(bad_request)?,
        Edit::AddServer {
            name,
            protocol,
            url,
            username,
            secret,
        } => owner
            .add_server(ScrobblingServerRequest {
                name,
                protocol,
                url,
                username,
                secret,
            })
            .await
            .map_err(bad_request)?,
        Edit::SignInServer {
            id,
            username,
            secret,
        } => owner
            .sign_in_server(id, username, secret)
            .await
            .map_err(bad_request)?,
        Edit::RemoveServer { id } => owner.remove_server(id).await.map_err(bad_request)?,
    };
    Ok(json_response(
        StatusCode::OK,
        preferences_json(&preferences),
    ))
}

fn retry_artwork(products: &ProductHandles) {
    if let Err(error) = products.artwork.retry_external() {
        tracing::warn!(%error, "failed to retry external artwork");
    }
}

#[derive(Deserialize)]
#[serde(tag = "service")]
enum Connection {
    #[serde(rename = "lastfm")]
    LastFm { api_key: String, api_secret: String },
    #[serde(rename = "librefm")]
    LibreFm,
}

/// Signs in through the service's page in the browser: the events are {open:url}, then
/// {connected:username} or {error}.
async fn connect(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let connection: Connection = body(request).await?;
    let lastfm = matches!(connection, Connection::LastFm { .. });
    let session = products.scrobbling.connect(match connection {
        Connection::LastFm {
            api_key,
            api_secret,
        } => ScrobblingConnection::LastFm {
            api_key: api_key.trim().to_owned(),
            api_secret: api_secret.trim().to_owned(),
        },
        Connection::LibreFm => ScrobblingConnection::LibreFm,
    });
    let stream = futures_util::stream::unfold(Some((session, products)), move |state| async move {
        let (session, products) = state?;
        let (value, done) = match session.next().await {
            // The browser opens the page itself.
            Ok(ScrobblingConnectionEvent::OpenUrl { url, opened }) => {
                opened.complete(None).await;
                (json!({"open": url}), false)
            }
            Ok(ScrobblingConnectionEvent::Connected { username }) => {
                if lastfm {
                    retry_artwork(&products);
                }
                (json!({"connected": username}), true)
            }
            Ok(ScrobblingConnectionEvent::Failed(error)) => (json!({"error": error}), true),
            Ok(ScrobblingConnectionEvent::TimedOut) | Err(_) => {
                (json!({"error": localization::tr("Couldn't connect")}), true)
            }
        };
        Some((
            Ok::<_, Infallible>(Frame::data(Bytes::from(format!(
                "event: connect\ndata: {value}\n\n"
            )))),
            (!done).then_some((session, products)),
        ))
    });
    Ok(event_response(Body::new(StreamBody::new(stream))))
}
