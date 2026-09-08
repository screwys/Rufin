use super::*;

#[derive(Clone, Eq, PartialEq)]
pub struct JellyfinQuickConnectLogin {
    config: JellyfinEmbyClientConfig,
    response: Value,
}

pub struct JellyfinQuickConnect {
    client: Client,
    config: JellyfinEmbyClientConfig,
    secret: String,
    pub code: String,
    pub approval_url: String,
}

impl JellyfinQuickConnect {
    pub async fn start(
        server_url: String,
        trust_invalid_cert: bool,
        device_id: String,
    ) -> SourceResult<Self> {
        let config = JellyfinEmbyClientConfig::new(server_url, trust_invalid_cert, device_id)?;
        let base = normalize_base_url(&config.base_url)?;
        let client = build_client(trust_invalid_cert)?;
        let enabled: bool = send_json(
            ServerKind::Jellyfin,
            client.get(endpoint(&base, "QuickConnect/Enabled")?),
        )
        .await?;
        if !enabled {
            return Err(SourceError::Auth(
                "Quick Connect is disabled on this Jellyfin server.".into(),
            ));
        }
        let response: Value = send_json(
            ServerKind::Jellyfin,
            client
                .post(endpoint(&base, "QuickConnect/Initiate")?)
                .header(header::AUTHORIZATION, auth_header(&config, None)),
        )
        .await?;
        let mut approval_url = endpoint(&base, "web/")?;
        approval_url.set_fragment(Some("/quickconnect"));
        Ok(Self {
            client,
            config,
            secret: connect::required(&response, "Secret")?,
            code: connect::required(&response, "Code")?,
            approval_url: approval_url.to_string(),
        })
    }

    pub async fn poll(&self) -> SourceResult<Option<JellyfinQuickConnectLogin>> {
        let base = normalize_base_url(&self.config.base_url)?;
        let status: Value = send_json(
            ServerKind::Jellyfin,
            self.client
                .get(endpoint(&base, "QuickConnect/Connect")?)
                .query(&[("secret", &self.secret)]),
        )
        .await?;
        if status["Authenticated"].as_bool() != Some(true) {
            return Ok(None);
        }
        let response: Value = send_json(
            ServerKind::Jellyfin,
            self.client
                .post(endpoint(&base, "Users/AuthenticateWithQuickConnect")?)
                .header(header::AUTHORIZATION, auth_header(&self.config, None))
                .json(&serde_json::json!({"Secret": self.secret})),
        )
        .await?;
        Ok(Some(JellyfinQuickConnectLogin {
            config: self.config.clone(),
            response,
        }))
    }
}

pub(crate) async fn connect_quick(
    source_id: SourceId,
    login: JellyfinQuickConnectLogin,
    name: Option<String>,
    use_instant_mix: bool,
) -> SourceResult<ConnectedSource> {
    JellyfinEmbySource::finish_authentication(
        source_id,
        name,
        use_instant_mix,
        login.config,
        String::new(),
        login.response,
    )
    .await
    .map(AuthenticatedJellyfinEmby::connected)
}

impl std::fmt::Debug for JellyfinQuickConnectLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JellyfinQuickConnectLogin")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, header_exists, method, path, query_param},
    };

    #[tokio::test]
    async fn quick_connect_checks_availability_then_exchanges_only_approved_secret() {
        let server = MockServer::start().await;
        Mock::given(path("/music/QuickConnect/Enabled"))
            .respond_with(ResponseTemplate::new(200).set_body_json(true))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/music/QuickConnect/Initiate"))
            .and(header_exists("Authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"Code":"123456","Secret":"approval-secret","Authenticated":false}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/music/QuickConnect/Connect"))
            .and(query_param("secret", "approval-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"Authenticated":false})))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        let quick =
            JellyfinQuickConnect::start(format!("{}/music", server.uri()), false, "device".into())
                .await
                .unwrap();
        assert_eq!(quick.code, "123456");
        assert_eq!(
            quick.approval_url,
            format!("{}/music/web/#/quickconnect", server.uri())
        );
        assert!(quick.poll().await.unwrap().is_none());
        Mock::given(method("GET"))
            .and(path("/music/QuickConnect/Connect"))
            .and(query_param("secret", "approval-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"Authenticated":true})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST")).and(path("/music/Users/AuthenticateWithQuickConnect"))
            .and(header_exists("Authorization")).and(body_json(json!({"Secret":"approval-secret"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ServerId":"server","AccessToken":"local-token","User":{"Id":"user","Name":"Listener"}})))
            .expect(1).mount(&server).await;
        Mock::given(path("/music/System/Info/Public"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ServerName":"Home"})))
            .expect(1)
            .mount(&server)
            .await;
        let login = quick.poll().await.unwrap().unwrap();
        assert!(!format!("{login:?}").contains("local-token"));
        let connected = connect_quick(SourceId::new("source"), login, None, false)
            .await
            .unwrap();
        let (configuration, _, credential) = connected.into_parts();
        assert_eq!(configuration.name, "Home");
        assert_eq!(credential.as_deref(), Some("local-token"));
        assert!(!configuration.provider_payload.contains("approval-secret"));
        let saved = JellyfinEmbySourceConfig::from_configuration(&configuration).unwrap();
        assert_eq!(saved.user_id, "user");
        assert!(!saved.emby_connect);
        let reopened = open(&configuration, credential, Some("device".into())).unwrap();
        assert_eq!(
            reopened.session_access_token().await.unwrap(),
            "local-token"
        );
    }

    #[tokio::test]
    async fn disabled_quick_connect_does_not_initiate() {
        let server = MockServer::start().await;
        Mock::given(path("/QuickConnect/Enabled"))
            .respond_with(ResponseTemplate::new(200).set_body_json(false))
            .expect(1)
            .mount(&server)
            .await;
        let result = JellyfinQuickConnect::start(server.uri(), false, "device".into()).await;
        assert!(matches!(result, Err(SourceError::Auth(_))));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}
