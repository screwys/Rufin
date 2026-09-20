#![cfg(feature = "test-ndr64")]

mod common;
use serial_test::serial;

#[test_log::test(maybe_async::test(
    not(feature = "async"),
    async(feature = "async", tokio::test(flavor = "multi_thread"))
))]
#[serial]
async fn test_shares_enum() -> smb::Result<()> {
    let (client, path) = make_server_connection("IPC$", None).await?;
    let shares = client.list_shares(&path.server).await?;
    assert!(
        shares
            .iter()
            .any(
                |s| s.netname.as_ref().unwrap().to_string() == TestConstants::DEFAULT_SHARE
                    && s.share_type.kind() == ShareKind::Disk
            )
    );
    Ok(())
}
