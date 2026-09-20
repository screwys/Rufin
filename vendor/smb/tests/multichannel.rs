#![cfg(feature = "test-multichannel")]

mod common;
use common::*;
use serial_test::serial;

/// This test is a bit special.
///
/// It must run in the same LAN as the server - otherwise, network interface
/// enumeration will output some unexpected results.
///
/// For example, you cannot run this test from outside of a docker container against a samba container,
/// since the transport is routed through a virtual interface (that we usually bind to localhost:445 on the host).
/// meanwhile, the server detects interfaces with IP addresses in the docker network range.
#[test_log::test(maybe_async::test(
    not(feature = "async"),
    async(feature = "async", tokio::test(flavor = "multi_thread"))
))]
#[serial]
async fn test_multichannel_connection() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = default_connection_config();
    config.multichannel.enabled = true;
    let (client, share_path) =
        make_server_connection(TestConstants::DEFAULT_SHARE, Some(config)).await?;

    let channels = client.get_channels(&share_path).await?;
    assert!(
        channels.len() > 1,
        "Expected multiple channels, got {}",
        channels.len()
    );

    Ok(())
}
