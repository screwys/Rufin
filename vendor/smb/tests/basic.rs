//! A basic create file test.

mod common;
use std::str::FromStr;
use std::time::Duration;

use common::{TestConstants, TestEnv, make_server_connection};
use serial_test::serial;
use smb::{Client, ClientConfig, UncPath};
use smb::{ConnectionConfig, FileCreateArgs};
use smb_fscc::FileDispositionInformation;
use smb_msg::Status;
use smb_transport::{TransportConfig, TransportError};

#[maybe_async::maybe_async]
async fn _do_minimal_connection_test(
    conn_config: Option<ConnectionConfig>,
    share: Option<&str>,
) -> smb::Result<()> {
    let (client, share_path) =
        make_server_connection(share.unwrap_or(TestConstants::DEFAULT_SHARE), conn_config).await?;

    // Create a file
    let file = client
        .create_file(
            &share_path.with_path("basic.txt"),
            &FileCreateArgs::make_create_new(Default::default(), Default::default()),
        )
        .await?
        .unwrap_file();

    file.set_info(FileDispositionInformation::default()).await?;

    file.close().await?;
    Ok(())
}

#[maybe_async::maybe_async]
async fn _test_basic_integration(
    transport: TransportConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let conn_config = ConnectionConfig {
        transport,
        ..Default::default()
    };
    Ok(_do_minimal_connection_test(Some(conn_config), None).await?)
}

#[test_log::test(maybe_async::test(
    not(feature = "async"),
    async(feature = "async", tokio::test(flavor = "multi_thread"))
))]
#[serial]
async fn test_basic_guest() -> smb::Result<()> {
    with_temp_env!(
        [
            (TestEnv::USER, Some(TestEnv::GUEST_USER.to_string())),
            (TestEnv::PASSWORD, Some(TestEnv::GUEST_PASSWORD.to_string())),
        ],
        _do_minimal_connection_test(
            ConnectionConfig {
                allow_unsigned_guest_access: true,
                ..Default::default()
            }
            .into(),
            Some(TestConstants::PUBLIC_GUEST_SHARE)
        )
    )
}

#[test_log::test(maybe_async::test(
    not(feature = "async"),
    async(feature = "async", tokio::test(flavor = "multi_thread"))
))]
#[serial]
async fn test_basic_auth_fail() -> smb::Result<()> {
    with_temp_env!(
        [(
            TestEnv::PASSWORD,
            Some(TestEnv::DEFAULT_PASSWORD.to_string() + "1")
        ),],
        do_test_basic_auth_fail()
    )
}

#[maybe_async::maybe_async]
async fn do_test_basic_auth_fail() -> smb::Result<()> {
    let res = _do_minimal_connection_test(None, None).await.unwrap_err();
    match res {
        smb::Error::UnexpectedMessageStatus(status) => {
            assert_eq!(status, Status::LogonFailure as u32);
        }
        _ => panic!("Expected LogonFailure error"),
    }
    smb::Result::Ok(())
}

#[maybe_async::maybe_async]
async fn _test_connection_timeout_fail(
    transport_config: TransportConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Instant;

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
    let client = Client::new(ClientConfig {
        connection: ConnectionConfig {
            timeout: Some(CONNECT_TIMEOUT),
            transport: transport_config,
            ..Default::default()
        },
        ..Default::default()
    });

    const UNRESPONSIVE_SMB_HOST: &str = "8.8.8.8"; // unless Google decides they like Microsoft...
    let time_before = Instant::now();
    let share_connect_result = client
        .share_connect(
            &UncPath::from_str(&format!("\\\\{}\\share", UNRESPONSIVE_SMB_HOST)).unwrap(),
            "user",
            "password".to_string(),
        )
        .await
        .map(|_| ());
    let time_after = Instant::now();

    if !matches!(
        share_connect_result,
        Err(smb::Error::TransportError(TransportError::Timeout(
            CONNECT_TIMEOUT
        )))
    ) {
        return Err(format!(
            "Expected OperationTimeout error, got {:?}!",
            share_connect_result
        )
        .into());
    }

    let delta_timeout = time_after.duration_since(time_before);
    let connect_timeout_with_margins_max = CONNECT_TIMEOUT + Duration::from_millis(100);
    let connect_timeout_with_margins_min = CONNECT_TIMEOUT - Duration::from_millis(5);
    if delta_timeout < connect_timeout_with_margins_min
        || delta_timeout > connect_timeout_with_margins_max
    {
        return Err(format!(
            "Expected timeout to be at least {:?}, but it was {:?}!",
            connect_timeout_with_margins_max, delta_timeout
        )
        .into());
    }

    Ok(())
}

/// Generates tests for different transport configurations.
macro_rules! test_transport {
    (
        $($transport_config:ty: $config_value:tt)+
    ) => {
            $(
                pastey::paste!{
#[test_log::test(maybe_async::test(
    not(feature = "async"),
    async(feature = "async", tokio::test(flavor = "multi_thread"))
))]
#[serial]
async fn [<test_basic_integration_ $transport_config:lower>]() -> Result<(), Box<dyn std::error::Error>> {
    _test_basic_integration(TransportConfig::$config_value).await
}

#[test_log::test(maybe_async::test(
    not(feature = "async"),
    async(feature = "async", tokio::test(flavor = "multi_thread"))
))]
#[serial]
async fn [<test_connection_timeout_fail_ $transport_config:lower>]() -> Result<(), Box<dyn std::error::Error>> {
    _test_connection_timeout_fail(TransportConfig::$config_value).await
}

            }
        )+
    };

    // Sugary XxxTransport::XxxTransport syntax
    (
        $($transport_config:ty,)+
    ) => {
        test_transport!($($transport_config: $transport_config)+);
    }
}

test_transport!(Tcp,);

#[cfg(feature = "netbios-transport")]
test_transport!(NetBios,);

#[cfg(feature = "test-quic")]
test_transport!(Quic,);

#[cfg(feature = "test-rdma")]
test_transport!(Rdma,);
