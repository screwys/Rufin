//! Hosted Emby account authorization and the server-local Connect exchange.
use super::*;
use serde::{Deserialize, Serialize};

const CONNECT_URL: &str = "https://connect.emby.media/service/";

#[derive(Clone, Eq, PartialEq)]
pub struct EmbyConnectServer {
    pub id: String,
    pub name: String,
    pub addresses: Vec<String>,
    connect_user_id: String,
    access_key: String,
}

pub struct EmbyConnectLogin {
    client: Client,
    service_url: Url,
    user_id: String,
    access_token: String,
}

impl EmbyConnectLogin {
    pub async fn password(username: &str, password: &str) -> SourceResult<Self> {
        Self::password_at(Url::parse(CONNECT_URL).unwrap(), username, password).await
    }

    async fn password_at(service_url: Url, username: &str, password: &str) -> SourceResult<Self> {
        let client = build_client(false)?;
        let response: Value = send_json(
            ServerKind::Emby,
            cloud_request(client.post(endpoint(&service_url, "user/authenticate")?))
                .form(&[("nameOrEmail", username), ("rawpw", password)]),
        )
        .await?;
        Ok(Self {
            client,
            service_url,
            user_id: id(&response["User"]["Id"])
                .or_else(|| id(&response["ConnectUserId"]))
                .ok_or_else(|| {
                    SourceError::Auth("Emby Connect returned no account identity".into())
                })?,
            access_token: required(&response, "AccessToken")
                .or_else(|_| required(&response, "ConnectAccessToken"))?,
        })
    }

    pub async fn servers(&self) -> SourceResult<Vec<EmbyConnectServer>> {
        let response: Vec<Value> = send_json(
            ServerKind::Emby,
            cloud_request(self.client.get(endpoint(&self.service_url, "servers")?))
                .header("X-Connect-UserToken", &self.access_token)
                .query(&[("userId", &self.user_id)]),
        )
        .await?;
        response
            .into_iter()
            .map(|server| {
                let addresses = ["LocalAddress", "Url"]
                    .iter()
                    .filter_map(|key| field::<String>(&server, key))
                    .filter(|address| !address.trim().is_empty())
                    .collect();
                Ok(EmbyConnectServer {
                    id: required(&server, "SystemId")?,
                    name: required(&server, "Name")?,
                    addresses,
                    connect_user_id: self.user_id.clone(),
                    access_key: required(&server, "AccessKey")?,
                })
            })
            .collect()
    }
}

pub struct EmbyConnectPin {
    client: Client,
    service_url: Url,
    device_id: String,
    pub code: String,
}

impl EmbyConnectPin {
    pub async fn start(device_id: String) -> SourceResult<Self> {
        Self::start_at(Url::parse(CONNECT_URL).unwrap(), device_id).await
    }

    async fn start_at(service_url: Url, device_id: String) -> SourceResult<Self> {
        let client = build_client(false)?;
        let response: Value = send_json(
            ServerKind::Emby,
            cloud_request(client.post(endpoint(&service_url, "pin")?))
                .form(&[("deviceId", &device_id)]),
        )
        .await?;
        Ok(Self {
            client,
            service_url,
            device_id: field(&response, "DeviceId").unwrap_or(device_id),
            code: required(&response, "Pin")?,
        })
    }

    pub fn approval_url(&self) -> &'static str {
        "https://emby.media/pin"
    }

    pub async fn poll(&self) -> SourceResult<Option<EmbyConnectLogin>> {
        let params = [("deviceId", &self.device_id), ("pin", &self.code)];
        let status: Value = send_json(
            ServerKind::Emby,
            cloud_request(self.client.get(endpoint(&self.service_url, "pin")?)).query(&params),
        )
        .await?;
        if status["IsExpired"].as_bool() == Some(true) {
            return Err(SourceError::Auth(
                "Emby Connect code expired. Request a new code.".into(),
            ));
        }
        if status["IsConfirmed"].as_bool() != Some(true) {
            return Ok(None);
        }
        let response: Value = send_json(
            ServerKind::Emby,
            cloud_request(
                self.client
                    .post(endpoint(&self.service_url, "pin/authenticate")?),
            )
            .form(&params),
        )
        .await?;
        Ok(Some(EmbyConnectLogin {
            client: self.client.clone(),
            service_url: self.service_url.clone(),
            user_id: required(&response, "UserId")?,
            access_token: required(&response, "AccessToken")?,
        }))
    }
}

fn cloud_request(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    request.header("X-Application", format!("{CLIENT_NAME}/{CLIENT_VERSION}"))
}

pub(super) fn required(response: &Value, name: &str) -> SourceResult<String> {
    id(&response[name])
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| SourceError::Auth(format!("Authentication returned no {name}")))
}

#[derive(Deserialize, Serialize)]
pub(super) struct EmbyConnectCredential {
    connect_user_id: String,
    access_key: String,
    access_token: String,
}

pub(super) fn decode_credential(
    credential: &str,
    is_connect: bool,
) -> SourceResult<(String, Option<EmbyConnectCredential>)> {
    if !is_connect {
        return Ok((credential.to_owned(), None));
    }
    let saved: EmbyConnectCredential = serde_json::from_str(credential).map_err(|error| {
        SourceError::InvalidConfig(format!("Invalid saved Emby Connect credentials: {error}"))
    })?;
    Ok((saved.access_token.clone(), Some(saved)))
}

async fn exchange(
    client: &Client,
    config: &JellyfinEmbyClientConfig,
    user_id: &str,
    access_key: &str,
) -> SourceResult<Value> {
    let base = ServerKind::Emby.api_base(&normalize_base_url(&config.base_url)?)?;
    send_json(
        ServerKind::Emby,
        client
            .get(endpoint(&base, "Connect/Exchange")?)
            .query(&[("format", "json"), ("ConnectUserId", user_id)])
            .header("X-Emby-Token", access_key)
            .header(
                ServerKind::Emby.authorization_header(),
                auth_header(config, None),
            ),
    )
    .await
}

pub(crate) async fn connect_server(
    source_id: SourceId,
    server: EmbyConnectServer,
    name: Option<String>,
    use_instant_mix: bool,
    device_id: String,
) -> SourceResult<ConnectedSource> {
    let client = build_client(false)?;
    let mut last_error = SourceError::InvalidConfig("Emby Connect server has no address".into());
    for address in &server.addresses {
        let mut config = JellyfinEmbyClientConfig::new(address, false, &device_id)?;
        config.kind = ServerKind::Emby;
        let response = match exchange(
            &client,
            &config,
            &server.connect_user_id,
            &server.access_key,
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                last_error = error;
                continue;
            }
        };
        let user_id = required(&response, "LocalUserId")?;
        let access_token = required(&response, "AccessToken")?;
        let base = ServerKind::Emby.api_base(&normalize_base_url(address)?)?;
        let user: Value = send_json(
            ServerKind::Emby,
            client
                .get(endpoint(&base, &format!("Users/{user_id}"))?)
                .header("X-Emby-Token", &access_token)
                .header(
                    ServerKind::Emby.authorization_header(),
                    auth_header(&config, Some(&access_token)),
                ),
        )
        .await?;
        let credential = serde_json::to_string(&EmbyConnectCredential {
            connect_user_id: server.connect_user_id,
            access_key: server.access_key,
            access_token: access_token.clone(),
        })
        .map_err(|error| SourceError::Other(error.to_string()))?;
        let configuration = crate::config::encode_provider_payload(
            source_id,
            EMBY_SOURCE_ID,
            crate::source::configured_source_name(name, server.name),
            JellyfinEmbySourceConfig {
                kind: ServerKind::Emby,
                base_url: normalize_base_url(address)?
                    .as_str()
                    .trim_end_matches('/')
                    .to_owned(),
                server_id: Some(server.id),
                user_id,
                username: required(&user, "Name")?,
                trust_invalid_cert: false,
                use_instant_mix,
                emby_connect: true,
            }
            .into_payload(),
        );
        let source = open(&configuration, Some(credential.clone()), Some(device_id))?;
        let header = authenticated_header(&config, &access_token)?;
        let _ = source.connect_token.set((access_token, header));
        return Ok(ConnectedSource::jellyfin_emby(
            configuration,
            source,
            Some(credential),
        ));
    }
    Err(last_error)
}

impl JellyfinEmbySource {
    async fn connect_session(&self) -> SourceResult<&(String, header::HeaderValue)> {
        self.connect_token
            .get_or_try_init(|| async {
                let saved = self
                    .connect_credential
                    .as_ref()
                    .expect("Connect session has saved credentials");
                let mut config = JellyfinEmbyClientConfig::new(
                    self.socket_base_url.as_str(),
                    self.trust_invalid_cert,
                    self.device_id.as_ref(),
                )?;
                config.kind = ServerKind::Emby;
                let response = exchange(
                    &self.client,
                    &config,
                    &saved.connect_user_id,
                    &saved.access_key,
                )
                .await?;
                if required(&response, "LocalUserId")? != self.user_id {
                    return Err(SourceError::Auth(
                        "Emby Connect is linked to a different server user. Sign in again.".into(),
                    ));
                }
                let token = required(&response, "AccessToken")?;
                let header = authenticated_header(&config, &token)?;
                Ok((token, header))
            })
            .await
    }

    pub(super) async fn session_access_token(&self) -> SourceResult<&str> {
        if self.connect_credential.is_some() {
            Ok(&self.connect_session().await?.0)
        } else {
            Ok(&self.access_token)
        }
    }

    pub(super) async fn session_authorization(&self) -> SourceResult<header::HeaderValue> {
        if self.connect_credential.is_some() {
            Ok(self.connect_session().await?.1.clone())
        } else {
            Ok(self.authorization.clone())
        }
    }
}

impl std::fmt::Debug for EmbyConnectServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbyConnectServer")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("addresses", &self.addresses)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for EmbyConnectCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbyConnectCredential")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, header, method, path, query_param},
    };

    #[tokio::test]
    async fn account_and_pin_authorization_resolve_the_same_linked_servers() {
        let cloud = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/authenticate"))
            .and(body_string_contains("nameOrEmail=listener%40example.com"))
            .and(body_string_contains("rawpw=account-password"))
            .and(header(
                "X-Application",
                format!("{CLIENT_NAME}/{CLIENT_VERSION}"),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"AccessToken":"cloud-token","User":{"Id":"cloud-user"}})),
            )
            .expect(1)
            .mount(&cloud)
            .await;
        Mock::given(method("POST"))
            .and(path("/pin"))
            .and(body_string_contains("deviceId=device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"Pin":"123456","DeviceId":"device","IsExpired":false,"IsConfirmed":false}),
            ))
            .expect(1)
            .mount(&cloud)
            .await;
        Mock::given(method("GET"))
            .and(path("/pin"))
            .and(query_param("pin", "123456"))
            .and(query_param("deviceId", "device"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"IsConfirmed":true,"IsExpired":false})),
            )
            .expect(1)
            .mount(&cloud)
            .await;
        Mock::given(method("POST"))
            .and(path("/pin/authenticate"))
            .and(body_string_contains("pin=123456"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"AccessToken":"cloud-token","UserId":"cloud-user"})),
            )
            .expect(1)
            .mount(&cloud)
            .await;
        Mock::given(method("GET")).and(path("/servers"))
            .and(header("X-Connect-UserToken", "cloud-token")).and(query_param("userId", "cloud-user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"SystemId":"server-one","Name":"Home","AccessKey":"key-one","LocalAddress":"http://local","Url":"https://remote"},
                {"SystemId":"server-two","Name":"Shared","AccessKey":"key-two","Url":"https://shared"}
            ]))).expect(2).mount(&cloud).await;
        let url = Url::parse(&format!("{}/", cloud.uri())).unwrap();
        let password =
            EmbyConnectLogin::password_at(url.clone(), "listener@example.com", "account-password")
                .await
                .unwrap();
        let pin = EmbyConnectPin::start_at(url, "device".into())
            .await
            .unwrap();
        let approved = pin.poll().await.unwrap().unwrap();
        let servers = password.servers().await.unwrap();
        assert_eq!(servers, approved.servers().await.unwrap());
        assert_eq!(servers[0].addresses, ["http://local", "https://remote"]);
        assert!(!format!("{servers:?}").contains("key-one"));
    }

    #[tokio::test]
    async fn connect_exchange_persists_reopens_and_preserves_settings_and_identity() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/emby/Connect/Exchange"))
            .and(query_param("ConnectUserId", "cloud-user"))
            .and(header("X-Emby-Token", "exchange-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"LocalUserId":"local-user","AccessToken":"first-token"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/emby/Users/local-user"))
            .and(header("X-Emby-Token", "first-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"Id":"local-user","Name":"Listener"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let connected = connect_server(
            SourceId::new("source"),
            EmbyConnectServer {
                id: "server".into(),
                name: "Home music".into(),
                addresses: vec![server.uri()],
                connect_user_id: "cloud-user".into(),
                access_key: "exchange-key".into(),
            },
            None,
            true,
            "device".into(),
        )
        .await
        .unwrap();
        let (configuration, _, credential) = connected.into_parts();
        assert_eq!(configuration.name, "Home music");
        assert!(!configuration.provider_payload.contains("exchange-key"));
        assert!(!configuration.provider_payload.contains("first-token"));
        server.verify().await;
        server.reset().await;
        Mock::given(path("/emby/Connect/Exchange"))
            .and(header("X-Emby-Token", "exchange-key"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"LocalUserId":"local-user","AccessToken":"reopened-token"}),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        let edited = edit(
            configuration.clone(),
            credential.clone(),
            JellyfinEmbySettingsInput {
                connect_manually: false,
                use_instant_mix: false,
                credentials: crate::CredentialSettingsInput {
                    name: "Renamed".into(),
                    base_url: server.uri(),
                    username: "Listener".into(),
                    password: String::new(),
                    trust_invalid_cert: false,
                },
            },
            Some("device".into()),
        )
        .await
        .unwrap();
        let SourceEditResult::Connected(edited) = edited else {
            panic!("reopened settings")
        };
        let (next, _, next_credential) = edited.into_parts();
        assert_eq!(
            next.input_identity().unwrap(),
            configuration.input_identity().unwrap()
        );
        assert_eq!(next.name, "Renamed");
        assert!(next_credential.is_none());
        assert!(
            JellyfinEmbySourceConfig::from_configuration(&next)
                .unwrap()
                .emby_connect
        );
        let source = open(&next, credential, Some("device".into())).unwrap();
        assert_eq!(
            source.session_access_token().await.unwrap(),
            "reopened-token"
        );
        assert_eq!(
            source.session_access_token().await.unwrap(),
            "reopened-token"
        );
        let stream = source
            .emby_stream("emby:track:track", StreamQuality::Original, None, false)
            .await
            .unwrap();
        assert!(stream.uri().contains("reopened-token"));
    }

    #[tokio::test]
    async fn replacing_connect_with_a_passwordless_local_login_keeps_source_identity() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/emby/Users/AuthenticateByName"))
            .and(wiremock::matchers::body_json(json!({"Username":"Listener","Pw":""})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"AccessToken":"manual-token","ServerId":"server","User":{"Id":"local-user","Name":"Listener"}})))
            .expect(1).mount(&server).await;
        Mock::given(path("/emby/System/Info/Public"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ServerName":"Home"})))
            .mount(&server)
            .await;
        let current = crate::config::encode_provider_payload(
            SourceId::new("source"),
            EMBY_SOURCE_ID,
            "Home",
            JellyfinEmbySourceConfig {
                kind: ServerKind::Emby,
                base_url: server.uri(),
                server_id: Some("server".into()),
                user_id: "local-user".into(),
                username: "Listener".into(),
                trust_invalid_cert: false,
                use_instant_mix: true,
                emby_connect: true,
            }
            .into_payload(),
        );
        let result = edit(
            current.clone(),
            Some("old-credential".into()),
            JellyfinEmbySettingsInput {
                connect_manually: true,
                use_instant_mix: false,
                credentials: crate::CredentialSettingsInput {
                    name: "Home".into(),
                    base_url: server.uri(),
                    username: "Listener".into(),
                    password: String::new(),
                    trust_invalid_cert: false,
                },
            },
            Some("device".into()),
        )
        .await
        .unwrap();
        let SourceEditResult::Connected(connected) = result else {
            panic!("replacement login")
        };
        let (configuration, _, credential) = connected.into_parts();
        assert_eq!(configuration.source_id, current.source_id);
        assert_eq!(
            configuration.input_identity().unwrap(),
            current.input_identity().unwrap()
        );
        assert!(
            !JellyfinEmbySourceConfig::from_configuration(&configuration)
                .unwrap()
                .emby_connect
        );
        assert_eq!(credential.as_deref(), Some("manual-token"));
    }
}
