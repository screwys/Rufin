use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::{Client, Method, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::remote_http::{self, BodyLimit, RemoteHttpPolicy, RemoteTimeouts};
use crate::{SourceError, SourceResult};

const PINS_URL: &str = "https://plex.tv/api/v2/pins";
const RESPONSE: BodyLimit = BodyLimit {
    max_bytes: 1024 * 1024,
    context: "Plex authentication response",
};
const HTTP: RemoteHttpPolicy = RemoteHttpPolicy {
    service: "Plex",
    auth_context: "Plex authentication returned",
    error_body: RESPONSE,
    redact_error_url: Some(redact),
};

fn redact(url: &mut reqwest::Url) {
    url.set_query(None);
    url.set_fragment(None);
}

/// One device login. Passwords and Home PINs are never part of its saved secret.
#[derive(Clone, Serialize, Deserialize)]
pub struct PlexLogin {
    client_id: String,
    token: String,
    account_id: Option<String>,
    profiles: HashMap<String, String>,
    resources: HashMap<String, HashMap<String, String>>,
    home_admin_subscription: bool,
    download_subscriptions: HashMap<String, bool>,
    #[serde(skip)]
    save: Option<Arc<dyn Fn(String) -> SourceResult<()> + Send + Sync>>,
}

pub struct PlexBrowserLogin {
    login: PlexLogin,
    pin_id: String,
    url: String,
}

impl PlexLogin {
    fn new(client_id: String) -> Self {
        Self {
            client_id,
            token: String::new(),
            account_id: None,
            profiles: HashMap::new(),
            resources: HashMap::new(),
            home_admin_subscription: false,
            download_subscriptions: HashMap::new(),
            save: None,
        }
    }

    pub fn encode(&self) -> SourceResult<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn decode(secret: &str) -> SourceResult<Self> {
        serde_json::from_str(secret)
            .map_err(|_| SourceError::InvalidConfig("invalid Plex login credential".into()))
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn same_device(&self, other: &Self) -> bool {
        self.client_id == other.client_id
    }

    pub fn for_setup(&self) -> Self {
        let mut login = self.clone();
        login.save = None;
        login
    }

    pub fn merge_authorization(&mut self, incoming: &Self) -> SourceResult<()> {
        if !self.same_device(incoming) {
            return Err(auth_error(
                "Plex login belongs to another registered device",
            ));
        }
        self.token.clone_from(&incoming.token);
        if incoming.account_id.is_some() {
            self.account_id.clone_from(&incoming.account_id);
        }
        self.profiles.extend(incoming.profiles.clone());
        self.home_admin_subscription = incoming.home_admin_subscription;
        self.download_subscriptions
            .extend(incoming.download_subscriptions.clone());
        for (profile, resources) in &incoming.resources {
            self.resources
                .entry(profile.clone())
                .or_default()
                .extend(resources.clone());
        }
        self.persist()
    }

    pub(crate) fn download_subscription(&self, profile_id: &str) -> bool {
        self.download_subscriptions
            .get(profile_id)
            .copied()
            .unwrap_or(false)
    }

    pub fn bind_save(&mut self, save: Arc<dyn Fn(String) -> SourceResult<()> + Send + Sync>) {
        self.save = Some(save);
    }

    fn persist(&self) -> SourceResult<()> {
        if let Some(save) = &self.save {
            save(self.encode()?)?;
        }
        Ok(())
    }

    pub(crate) fn server_token(&self, profile_id: &str, server_id: &str) -> SourceResult<&str> {
        self.resources
            .get(profile_id)
            .and_then(|resources| resources.get(server_id))
            .map(String::as_str)
            .ok_or_else(|| auth_error("Plex server has not been authorized for this profile"))
    }

    pub(crate) fn server_request(
        &self,
        client: &Client,
        method: Method,
        url: &str,
        profile_id: &str,
        server_id: &str,
    ) -> SourceResult<RequestBuilder> {
        Ok(self
            .request(client, method, url)
            .header("X-Plex-Token", self.server_token(profile_id, server_id)?))
    }

    pub async fn password(
        client_id: String,
        username: &str,
        password: &str,
        code: Option<&str>,
    ) -> SourceResult<Self> {
        let mut login = Self::new(client_id);
        let client = client()?;
        let mut form = vec![
            ("login", username),
            ("password", password),
            ("rememberMe", "true"),
        ];
        if let Some(code) = code {
            form.push(("verificationCode", code));
        }
        let response = json_response(
            login
                .request(&client, Method::POST, "https://plex.tv/api/v2/users/signin")
                .form(&form),
        )
        .await?;
        login.token = required_string(&response, "authToken")?.to_owned();
        Ok(login)
    }

    pub async fn browser(client_id: String) -> SourceResult<PlexBrowserLogin> {
        let login = Self::new(client_id);
        let client = client()?;
        let pin = json_response(
            login
                .request(&client, Method::POST, PINS_URL)
                .query(&[("strong", "true")]),
        )
        .await?;
        let pin_id = crate::remote_json::id(&pin["id"])
            .ok_or_else(|| auth_error("Plex did not return a PIN ID"))?;
        let code = required_string(&pin, "code")?;
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("clientID", &login.client_id)
            .append_pair("context[device][product]", "Rufin")
            .append_pair("context[device][version]", env!("CARGO_PKG_VERSION"))
            .append_pair("context[device][device]", "Rufin")
            .append_pair("context[device][deviceName]", "Rufin")
            .append_pair("code", code)
            .finish();
        Ok(PlexBrowserLogin {
            login,
            pin_id,
            url: format!("https://app.plex.tv/auth/#!?{query}"),
        })
    }

    pub async fn profiles(&mut self) -> SourceResult<Vec<PlexProfile>> {
        let client = client()?;
        let account = json_response(
            self.request(&client, Method::GET, "https://plex.tv/api/v2/user")
                .header("X-Plex-Token", &self.token),
        )
        .await?;
        let own_profile = profile(&account)
            .ok_or_else(|| auth_error("Plex did not return the account identity"))?;
        self.account_id = Some(own_profile.id.clone());
        let subscription =
            crate::remote_json::boolean(&account["subscription"]["active"]).unwrap_or(false);
        self.home_admin_subscription =
            subscription && crate::remote_json::boolean(&account["homeAdmin"]).unwrap_or(false);
        self.download_subscriptions
            .insert(own_profile.id.clone(), subscription);
        if !crate::remote_json::boolean(&account["home"]).unwrap_or(false) {
            return Ok(vec![own_profile]);
        }
        let home = json_response(
            self.request(
                &client,
                Method::GET,
                "https://clients.plex.tv/api/v2/home/users",
            )
            .header("X-Plex-Token", &self.token),
        )
        .await?;
        Ok(crate::remote_json::items(&home["users"])
            .iter()
            .filter_map(profile)
            .collect())
    }

    pub async fn authorize_profile(
        &mut self,
        profile: &PlexProfile,
        pin: Option<&str>,
    ) -> SourceResult<()> {
        if self.account_id.as_deref() == Some(&profile.id) {
            return Ok(());
        }
        let client = client()?;
        let mut url = reqwest::Url::parse("https://clients.plex.tv/api/v2/home/users/").unwrap();
        url.path_segments_mut()
            .unwrap()
            .pop_if_empty()
            .push(profile.uuid.as_deref().unwrap_or(&profile.id))
            .push("switch");
        url.query_pairs_mut()
            .append_pair("includeSubscriptions", "1");
        if let Some(pin) = pin {
            url.query_pairs_mut().append_pair("pin", pin);
        }
        let response = json_response(
            self.request(&client, Method::POST, url.as_str())
                .header("X-Plex-Token", &self.token)
                .header("Content-Length", "0"),
        )
        .await?;
        self.profiles.insert(
            profile.id.clone(),
            required_string(&response, "authToken")?.to_owned(),
        );
        let subscription =
            crate::remote_json::boolean(&response["subscription"]["active"]).unwrap_or(false);
        let managed = crate::remote_json::boolean(&response["restricted"]).unwrap_or(false);
        self.download_subscriptions.insert(
            profile.id.clone(),
            subscription || (managed && self.home_admin_subscription),
        );
        self.persist()
    }

    pub async fn servers(
        &mut self,
        profile_id: &str,
        lan: &[crate::DiscoveredServer],
    ) -> SourceResult<Vec<PlexServer>> {
        let token = self.profile_token(profile_id)?;
        let client = client()?;
        let response = json_response(
            self.request(
                &client,
                Method::GET,
                "https://plex.tv/api/v2/resources?includeHttps=1&includeRelay=1&includeIPv6=1",
            )
            .header("X-Plex-Token", token),
        )
        .await?;
        let mut servers = Vec::new();
        let mut tokens = HashMap::new();
        for resource in crate::remote_json::items(&response) {
            if !resource["provides"]
                .as_str()
                .is_some_and(|provides| provides.split(',').any(|value| value == "server"))
            {
                continue;
            }
            let Some(id) = crate::remote_json::id(&resource["clientIdentifier"]) else {
                continue;
            };
            let Some(token) = resource["accessToken"]
                .as_str()
                .filter(|token| !token.is_empty())
            else {
                continue;
            };
            let mut server = PlexServer {
                id: id.clone(),
                name: resource["name"].as_str().unwrap_or("Plex").to_owned(),
                owned: crate::remote_json::boolean(&resource["owned"]).unwrap_or(false),
                connections: crate::remote_json::items(&resource["connections"])
                    .iter()
                    .filter_map(connection)
                    .collect(),
            };
            server.merge_lan(lan);
            tokens.insert(id, token.to_owned());
            servers.push(server);
        }
        self.resources.insert(profile_id.to_owned(), tokens);
        self.persist()?;
        Ok(servers)
    }

    fn profile_token(&self, profile_id: &str) -> SourceResult<&str> {
        if self.account_id.as_deref() == Some(profile_id) {
            return Ok(&self.token);
        }
        self.profiles
            .get(profile_id)
            .map(String::as_str)
            .ok_or_else(|| auth_error("Plex listening profile has not been authorized"))
    }

    pub async fn select_connection(
        &self,
        profile_id: &str,
        server: &PlexServer,
        address_override: Option<&str>,
        trust_invalid_cert: bool,
    ) -> SourceResult<PlexConnection> {
        let token = self
            .resources
            .get(profile_id)
            .and_then(|resources| resources.get(&server.id))
            .ok_or_else(|| auth_error("Plex server has not been authorized for this profile"))?;
        let client = remote_http::build_client(
            trust_invalid_cert,
            RemoteTimeouts {
                connect: Duration::from_secs(5),
                request: Duration::from_secs(10),
            },
            HTTP,
        )?;
        let mut server = server.clone();
        if address_override.is_none_or(|address| address.trim().is_empty()) {
            server.merge_lan(&crate::discovery::plex_loopback_servers().await);
        }
        let connections = server.candidates(address_override)?;
        let mut failure =
            SourceError::Network("No connection is available for this Plex server".into());
        for connection in connections {
            let request = self
                .request(&client, Method::GET, connection.address.as_str())
                .header("X-Plex-Token", token);
            match json_response(request).await {
                Ok(response)
                    if response["MediaContainer"]["machineIdentifier"].as_str()
                        == Some(server.id.as_str()) =>
                {
                    return Ok(connection);
                }
                Ok(_) => {
                    failure = SourceError::Other(
                        "Plex connection returned a different server identity".into(),
                    )
                }
                Err(error) => failure = error,
            }
        }
        Err(failure)
    }

    fn request(&self, client: &Client, method: Method, url: &str) -> RequestBuilder {
        client
            .request(method, url)
            .header("Accept", "application/json")
            .header("X-Plex-Client-Identifier", &self.client_id)
            .header("X-Plex-Product", "Rufin")
            .header("X-Plex-Version", env!("CARGO_PKG_VERSION"))
            .header("X-Plex-Device", "Rufin")
            .header("X-Plex-Device-Name", "Rufin")
    }
}

impl std::fmt::Debug for PlexLogin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlexLogin")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

impl PartialEq for PlexLogin {
    fn eq(&self, other: &Self) -> bool {
        self.client_id == other.client_id
            && self.token == other.token
            && self.account_id == other.account_id
            && self.profiles == other.profiles
            && self.resources == other.resources
            && self.home_admin_subscription == other.home_admin_subscription
            && self.download_subscriptions == other.download_subscriptions
    }
}
impl Eq for PlexLogin {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlexProfile {
    pub id: String,
    pub uuid: Option<String>,
    pub name: String,
    pub pin_required: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlexConnection {
    pub address: reqwest::Url,
    pub local: bool,
    pub relay: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlexServer {
    pub id: String,
    pub name: String,
    pub owned: bool,
    connections: Vec<PlexConnection>,
}

impl PlexServer {
    fn merge_lan(&mut self, lan: &[crate::DiscoveredServer]) {
        for found in lan
            .iter()
            .filter(|found| found.id.as_deref() == Some(&self.id))
        {
            let Ok(address) = reqwest::Url::parse(&found.address) else {
                continue;
            };
            if !self
                .connections
                .iter()
                .any(|connection| connection.address == address)
            {
                self.connections.push(PlexConnection {
                    address,
                    local: true,
                    relay: false,
                });
            }
        }
    }

    fn candidates(&self, address_override: Option<&str>) -> SourceResult<Vec<PlexConnection>> {
        if let Some(address) = address_override.filter(|value| !value.trim().is_empty()) {
            let address = reqwest::Url::parse(address.trim())
                .map_err(|error| SourceError::InvalidConfig(error.to_string()))?;
            if !matches!(address.scheme(), "http" | "https") {
                return Err(SourceError::InvalidRequest(
                    "Plex requires an HTTP or HTTPS address",
                ));
            }
            let known = self
                .connections
                .iter()
                .find(|connection| connection.address == address);
            return Ok(vec![PlexConnection {
                address: address.clone(),
                local: known.is_some_and(|connection| connection.local),
                relay: known.is_some_and(|connection| connection.relay),
            }]);
        }
        let mut connections: Vec<_> = self
            .connections
            .iter()
            .filter(|connection| {
                connection.address.scheme() == "https"
                    || (connection.local
                        && !connection.relay
                        && connection.address.scheme() == "http")
            })
            .cloned()
            .collect();
        connections.sort_by_key(|connection| {
            let rank = if connection.relay {
                3
            } else if connection.local && connection.address.scheme() == "https" {
                0
            } else if connection.local {
                1
            } else {
                2
            };
            let loopback = match connection.address.host() {
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
                None => false,
            };
            (rank, !loopback)
        });
        Ok(connections)
    }
}

fn profile(value: &Value) -> Option<PlexProfile> {
    Some(PlexProfile {
        id: crate::remote_json::id(&value["id"])?,
        uuid: value["uuid"].as_str().map(str::to_owned),
        name: value["friendlyName"]
            .as_str()
            .or_else(|| value["title"].as_str())
            .or_else(|| value["username"].as_str())
            .unwrap_or("Plex")
            .to_owned(),
        pin_required: crate::remote_json::boolean(&value["protected"]).unwrap_or(false),
    })
}

fn connection(value: &Value) -> Option<PlexConnection> {
    Some(PlexConnection {
        address: reqwest::Url::parse(value["uri"].as_str()?).ok()?,
        local: crate::remote_json::boolean(&value["local"]).unwrap_or(false),
        relay: crate::remote_json::boolean(&value["relay"]).unwrap_or(false),
    })
}

impl PlexBrowserLogin {
    pub fn url(&self) -> &str {
        &self.url
    }

    /// One poll; the setup form owns polling lifetime and cancellation.
    pub async fn poll(&mut self) -> SourceResult<Option<PlexLogin>> {
        let client = client()?;
        let response = json_response(self.login.request(
            &client,
            Method::GET,
            &format!("{PINS_URL}/{}", self.pin_id),
        ))
        .await?;
        let Some(token) = response["authToken"]
            .as_str()
            .filter(|token| !token.is_empty())
        else {
            return Ok(None);
        };
        self.login.token = token.to_owned();
        Ok(Some(self.login.clone()))
    }
}

fn client() -> SourceResult<Client> {
    remote_http::build_client(
        false,
        RemoteTimeouts {
            connect: Duration::from_secs(10),
            request: Duration::from_secs(30),
        },
        HTTP,
    )
}

async fn json_response(request: RequestBuilder) -> SourceResult<Value> {
    remote_http::json(request, HTTP, RESPONSE).await
}

fn required_string<'a>(value: &'a Value, key: &str) -> SourceResult<&'a str> {
    value[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| auth_error(&format!("Plex authentication response is missing {key}")))
}

fn auth_error(message: &str) -> SourceError {
    SourceError::Auth(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn revoked_server_token_uses_existing_reauthentication_error() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{header, method, path},
        };
        let server = MockServer::start().await;
        let source = super::super::catalog::tests::source(&server);
        Mock::given(method("GET"))
            .and(path("/library/sections"))
            .and(header("X-Plex-Token", "token"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        assert!(matches!(
            source.get("/library/sections", &[]).await,
            Err(SourceError::Auth(_))
        ));
    }

    #[test]
    fn account_connections_keep_https_preference_after_lan_merge() {
        let mut server = PlexServer {
            id: "server-one".into(),
            name: "Music".into(),
            owned: true,
            connections: [
                json!({"uri": "https://relay.example:443", "relay": true, "local": false}),
                json!({"uri": "https://remote.example:32400", "relay": false, "local": false}),
                json!({"uri": "https://local.example:32400", "relay": false, "local": true}),
            ]
            .iter()
            .filter_map(connection)
            .collect(),
        };
        let lan = crate::DiscoveredServer {
            id: Some("server-one".into()),
            name: "LAN Music".into(),
            address: "http://192.0.2.20:32400".into(),
        };
        server.merge_lan(&[lan.clone(), lan]);
        assert_eq!(server.connections.len(), 4);
        server.merge_lan(&[crate::DiscoveredServer {
            id: Some("different-server".into()),
            name: "Other".into(),
            address: "http://127.0.0.1:32400".into(),
        }]);
        assert_eq!(server.connections.len(), 4);
        let candidates = server.candidates(None).unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|connection| connection.address.host_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "local.example",
                "192.0.2.20",
                "remote.example",
                "relay.example"
            ]
        );
        let manual = server.candidates(Some("http://192.0.2.20:32400")).unwrap();
        assert_eq!(manual.len(), 1);
        assert_eq!(manual[0].address.scheme(), "http");
        assert!(manual[0].local);
    }

    #[test]
    fn saved_login_keeps_profile_authorization_separate() {
        let mut login = PlexLogin::new("rufin-test-device".into());
        login.account_id = Some("owner".into());
        login.token = "owner-token".into();
        login
            .profiles
            .insert("managed".into(), "managed-token".into());
        login.resources.insert(
            "managed".into(),
            HashMap::from([("server-one".into(), "resource-token".into())]),
        );
        let saved = PlexLogin::decode(&login.encode().unwrap()).unwrap();
        assert_eq!(saved.profile_token("owner").unwrap(), "owner-token");
        assert_eq!(saved.profile_token("managed").unwrap(), "managed-token");
        assert_eq!(saved.resources["managed"]["server-one"], "resource-token");
        assert!(saved.profile_token("another-profile").is_err());
        assert!(saved.same_device(&login));
    }
}
