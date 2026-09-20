#![allow(dead_code)]
use smb::*;
use std::env::var;

pub struct TestEnv;

impl TestEnv {
    pub const SERVER: &'static str = "SMB_RUST_TESTS_SERVER";
    pub const USER: &'static str = "SMB_RUST_TESTS_USER_NAME";
    pub const DEFAULT_USER: &'static str = "LocalAdmin";
    pub const PASSWORD: &'static str = "SMB_RUST_TESTS_PASSWORD";
    pub const DEFAULT_PASSWORD: &'static str = "123456";

    pub const GUEST_USER: &'static str = "/GUEST";
    pub const GUEST_PASSWORD: &'static str = "";
}

pub struct TestConstants;

impl TestConstants {
    pub const DEFAULT_SHARE: &'static str = "MyShare";
    pub const PUBLIC_GUEST_SHARE: &'static str = "PublicShare";
}

pub fn default_connection_config() -> ConnectionConfig {
    let mut conn_config = ConnectionConfig::default();
    conn_config.timeout = Some(std::time::Duration::from_secs(10));
    conn_config.auth_methods.kerberos = false;
    conn_config.auth_methods.ntlm = true;
    conn_config
}

/// Creates a new SMB client and connects to the specified share on the server.
/// Returns the client and the UNC path used for the connection's share.
#[maybe_async::maybe_async]
pub async fn make_server_connection(
    share: &str,
    config: Option<ConnectionConfig>,
) -> smb::Result<(Client, UncPath)> {
    make_server_connection_ex(
        share,
        ClientConfig {
            connection: config.unwrap_or(default_connection_config()),
            ..Default::default()
        },
    )
    .await
}

/// Returns the server address for the tests connection.
pub fn smb_tests_server() -> String {
    var(TestEnv::SERVER).unwrap_or("127.0.0.1".to_string())
}

#[maybe_async::maybe_async]
pub async fn make_server_connection_ex(
    share: &str,
    config: ClientConfig,
) -> smb::Result<(Client, UncPath)> {
    let server = smb_tests_server();
    let user = var(TestEnv::USER).unwrap_or(TestEnv::DEFAULT_USER.to_string());
    let password = var(TestEnv::PASSWORD).unwrap_or(TestEnv::DEFAULT_PASSWORD.to_string());
    let smb = Client::new(config);
    log::info!("Connecting to {server}");

    let unc_path = UncPath::new(&server)?.with_share(share)?;
    // Connect & Authenticate
    smb.share_connect(&unc_path, user.as_str(), password.clone())
        .await?;
    log::info!("Connected to {unc_path}");
    Ok((smb, unc_path))
}

// Re-Export correct version for async or sync

#[cfg(feature = "async")]
#[macro_export]
macro_rules! with_temp_env {
    (
        [
            $(
                ($name:expr, $value:expr),
            )*
        ],
        $body:expr
    ) => {
        temp_env::async_with_vars(
            [
                $(($name, $value)),*
            ],
            $body
        ).await
    };
}
#[cfg(not(feature = "async"))]
#[macro_export]
macro_rules! with_temp_env {
    (
        [
            $(
                ($name:expr, $value:expr),
            )*
        ],
        $body:expr
    ) => {
        temp_env::with_vars(
            [
                $(($name, $value)),*
            ],
            move || { $body }
        )
    };
}
