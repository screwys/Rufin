//! Nextcloud Login Flow v2. The browser owns user authentication; Rufin receives an app password.

use std::time::{Duration, Instant};

use reqwest::{
    Method, StatusCode,
    header::{self, HeaderMap, HeaderValue},
};
use serde::Deserialize;
use url::Url;

use super::client::{Authentication, Body, WebDavClient};
use crate::{FileAuthentication, FileCredentials, FileSourceSettings};
use crate::{SourceError, SourceResult};

pub(crate) struct NextcloudLogin {
    client: WebDavClient,
    login: Url,
    poll: Url,
    token: String,
    expires: Instant,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NextcloudCredentials {
    pub server: String,
    pub login_name: String,
    pub app_password: String,
}

#[derive(Deserialize)]
struct Started {
    login: String,
    poll: Poll,
}

#[derive(Deserialize)]
struct Poll {
    endpoint: String,
    token: String,
}

impl NextcloudLogin {
    pub async fn start(
        server: Url,
        mut headers: HeaderMap,
        trust_invalid_certificate: bool,
        certificate_pem: Option<&[u8]>,
    ) -> SourceResult<Self> {
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_static(concat!("Rufin/", env!("CARGO_PKG_VERSION"))),
        );
        let client = WebDavClient::new(
            server,
            Authentication::Anonymous,
            headers,
            trust_invalid_certificate,
            certificate_pem,
        )?;
        let endpoint = client
            .root()
            .join("index.php/login/v2")
            .map_err(|e| SourceError::InvalidConfig(e.to_string()))?;
        let response = client
            .request(Method::POST, &endpoint, HeaderMap::new(), Body::Empty)
            .await?;
        if response.status() != StatusCode::OK {
            return Err(SourceError::Server {
                status: response.status().as_u16(),
                message: "Nextcloud browser login could not be started".into(),
            });
        }
        let started: Started = response
            .json()
            .await
            .map_err(|e| SourceError::Other(e.without_url().to_string()))?;
        let poll = client.resolve_href(client.root(), &started.poll.endpoint)?;
        let login = Url::parse(&started.login).map_err(|e| SourceError::Other(e.to_string()))?;
        if !matches!(login.scheme(), "http" | "https")
            || !login.username().is_empty()
            || login.password().is_some()
        {
            return Err(SourceError::InvalidRequest(
                "Nextcloud returned an invalid browser login URL",
            ));
        }
        Ok(Self {
            client,
            login,
            poll,
            token: started.poll.token,
            expires: Instant::now() + Duration::from_secs(20 * 60),
        })
    }

    pub fn browser_url(&self) -> &Url {
        &self.login
    }

    /// Called by the existing source setup lifetime; dropping this value cancels the flow.
    pub async fn poll(&self) -> SourceResult<Option<NextcloudCredentials>> {
        if Instant::now() >= self.expires {
            return Err(SourceError::Auth("Nextcloud browser login expired".into()));
        }
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("token", &self.token)
            .finish();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        let response = self
            .client
            .request(
                Method::POST,
                &self.poll,
                headers,
                Body::Bytes(body.as_bytes()),
            )
            .await?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(None),
            StatusCode::OK => response
                .json()
                .await
                .map(Some)
                .map_err(|e| SourceError::Other(e.without_url().to_string())),
            status => Err(SourceError::Server {
                status: status.as_u16(),
                message: "Nextcloud browser login could not be completed".into(),
            }),
        }
    }
}

impl NextcloudCredentials {
    pub async fn dav_root(
        &self,
        headers: HeaderMap,
        trust_invalid_certificate: bool,
        certificate_pem: Option<&[u8]>,
    ) -> SourceResult<Url> {
        // Login Flow may return an email login. Ask Nextcloud for the actual user ID used in DAV URLs.
        let server =
            Url::parse(&self.server).map_err(|e| SourceError::InvalidConfig(e.to_string()))?;
        let client = WebDavClient::new(
            server,
            Authentication::Password {
                username: self.login_name.clone(),
                password: self.app_password.clone(),
            },
            headers,
            trust_invalid_certificate,
            certificate_pem,
        )?;
        let user = client
            .root()
            .join("ocs/v2.php/cloud/user?format=json")
            .map_err(|e| SourceError::Other(e.to_string()))?;
        let mut headers = HeaderMap::new();
        headers.insert("OCS-APIRequest", HeaderValue::from_static("true"));
        let response = client
            .request(Method::GET, &user, headers, Body::Empty)
            .await?;
        if response.status() != StatusCode::OK {
            return Err(SourceError::Server {
                status: response.status().as_u16(),
                message: "Nextcloud user information could not be read".into(),
            });
        }
        #[derive(Deserialize)]
        struct User {
            id: String,
        }
        #[derive(Deserialize)]
        struct Ocs {
            data: User,
        }
        #[derive(Deserialize)]
        struct Response {
            ocs: Ocs,
        }
        let user: Response = response
            .json()
            .await
            .map_err(|e| SourceError::Other(e.without_url().to_string()))?;
        let mut root = client
            .root()
            .join("remote.php/dav/files/")
            .map_err(|e| SourceError::Other(e.to_string()))?;
        root.path_segments_mut()
            .expect("HTTP URL")
            .pop_if_empty()
            .push(&user.ocs.data.id)
            .push("");
        Ok(root)
    }
}

pub async fn authorize_nextcloud(
    mut settings: FileSourceSettings,
    mut credentials: FileCredentials,
    opened: impl FnOnce(&str),
) -> SourceResult<(FileSourceSettings, FileCredentials)> {
    let headers = super::client::custom_headers(&credentials.headers)?;
    let mut server =
        Url::parse(&settings.url).map_err(|e| SourceError::InvalidConfig(e.to_string()))?;
    // An existing source stores the DAV collection; browser login belongs to the
    // Nextcloud installation, including when it lives below a reverse-proxy prefix.
    if let Some((prefix, _)) = server.path().split_once("/remote.php/") {
        let prefix = format!("{prefix}/");
        server.set_path(&prefix);
    }
    let login = NextcloudLogin::start(
        server,
        headers.clone(),
        settings.trust_invalid_certificate,
        settings.certificate_pem.as_deref().map(str::as_bytes),
    )
    .await?;
    opened(login.browser_url().as_str());
    loop {
        if let Some(authorized) = login.poll().await? {
            settings.url = authorized
                .dav_root(
                    headers,
                    settings.trust_invalid_certificate,
                    settings.certificate_pem.as_deref().map(str::as_bytes),
                )
                .await?
                .into();
            settings.username = authorized.login_name;
            settings.authentication = FileAuthentication::Password;
            credentials.secret = authorized.app_password;
            return Ok((settings, credentials));
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}
