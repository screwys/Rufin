//! Tests for closing resources properly.

use std::sync::Arc;

use maybe_async::maybe_async;
use serial_test::serial;
mod common;
use common::{TestConstants, make_server_connection};
use smb::{Client, Connection, File, FileCreateArgs, Session, Tree};
use smb_fscc::{FileBasicInformation, FileDispositionInformation};

#[maybe_async]
async fn _close_tests_helper()
-> smb::Result<(Client, Arc<Connection>, Arc<Session>, Arc<Tree>, File)> {
    let (client, unc) =
        make_server_connection(TestConstants::DEFAULT_SHARE, Default::default()).await?;
    let file_path = unc.with_path("file.txt");
    let file = client
        .create_file(
            &file_path,
            &FileCreateArgs::make_create_new(Default::default(), Default::default()),
        )
        .await?;
    let file = file.unwrap_file();
    file.set_info(FileDispositionInformation::default()).await?;
    let connection = client.get_connection(file_path.server()).await?;
    let session = client.get_session(&file_path).await?;
    let tree = client.get_tree(&file_path).await?;

    Ok((client, connection, session, tree, file))
}

#[test_log::test(maybe_async::test(
    not(feature = "async"),
    async(feature = "async", tokio::test(flavor = "multi_thread"))
))]
#[serial]
async fn test_client_close() -> smb::Result<()> {
    let (client, _connection, _session, _tree, file) = _close_tests_helper().await?;
    client.close().await?;

    file.query_info::<FileBasicInformation>()
        .await
        .expect_err("Expected error after closing client");

    Ok(())
}

#[test_log::test(maybe_async::test(
    not(feature = "async"),
    async(feature = "async", tokio::test(flavor = "multi_thread"))
))]
#[serial]
async fn test_conn_close() -> smb::Result<()> {
    let (_client, connection, _session, _tree, file) = _close_tests_helper().await?;
    connection.close().await?;

    file.query_info::<FileBasicInformation>()
        .await
        .expect_err("Expected error after closing connection");

    Ok(())
}

#[test_log::test(maybe_async::test(
    not(feature = "async"),
    async(feature = "async", tokio::test(flavor = "multi_thread"))
))]
#[serial]
async fn test_session_logoff() -> smb::Result<()> {
    let (_client, _connection, session, _tree, file) = _close_tests_helper().await?;
    session.logoff().await?;

    file.query_info::<FileBasicInformation>()
        .await
        .expect_err("Expected error after closing session");

    Ok(())
}

#[test_log::test(maybe_async::test(
    not(feature = "async"),
    async(feature = "async", tokio::test(flavor = "multi_thread"))
))]
#[serial]
async fn test_tree_close() -> smb::Result<()> {
    let (_client, _connection, _session, tree, file) = _close_tests_helper().await?;
    tree.disconnect().await?;

    file.query_info::<FileBasicInformation>()
        .await
        .expect_err("Expected error after closing tree");

    Ok(())
}
