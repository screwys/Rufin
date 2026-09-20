mod common;
use common::*;
use serial_test::serial;
use smb::*;
use std::result::Result;

#[test_log::test(maybe_async::test(
    not(feature = "async"),
    async(feature = "async", tokio::test(flavor = "multi_thread"))
))]
#[serial] // Run only in a full-feature test, because it takes a while
async fn test_file_query_information() -> Result<(), Box<dyn std::error::Error>> {
    use smb::connection::EncryptionMode;

    let (client, share_path) = make_server_connection(
        TestConstants::DEFAULT_SHARE,
        ConnectionConfig {
            encryption_mode: EncryptionMode::Disabled,
            ..Default::default()
        }
        .into(),
    )
    .await?;

    let file = client
        .create_file(
            &share_path.clone().with_path("query_info_on.txt"),
            &FileCreateArgs::make_create_new(Default::default(), Default::default()),
        )
        .await?;
    let file = file.as_file().unwrap();
    file.set_info(FileDispositionInformation {
        delete_pending: true.into(),
    })
    .await?;

    let test_result = do_test_query_information(&file).await;

    file.close().await?;
    client.close().await?;

    Ok(test_result?)
}

#[maybe_async::maybe_async]
async fn do_test_query_information(file: &File) -> smb::Result<()> {
    const TEST_DATA: &[u8] = b"Hello, world!";
    file.write_at(TEST_DATA, 0).await?;

    file.query_info::<FileAccessInformation>().await?;
    file.query_info::<FileAccessInformation>().await?;
    file.query_info::<FileAlignmentInformation>().await?;
    file.query_info::<FileAllInformation>().await?;
    file.query_info::<FileAlternateNameInformation>().await?;
    file.query_info::<FileAttributeTagInformation>().await?;
    file.query_info::<FileBasicInformation>().await?;
    file.query_info::<FileCompressionInformation>().await?;
    file.query_info::<FileEaInformation>().await?;
    // file.query_info::<FileFullEaInformation>().await?;
    // file.query_info::<FileIdInformation>().await?; // Supported on Windows Server 2012, NTFS/ReFS only
    file.query_info::<FileInternalInformation>().await?;
    file.query_info::<FileModeInformation>().await?;
    file.query_info::<FileNetworkOpenInformation>().await?;
    file.query_info::<FileNormalizedNameInformation>().await?;

    let position_info = file.query_info::<FilePositionInformation>().await?;
    assert_eq!(
        position_info,
        FilePositionInformation {
            current_byte_offset: 0
        }
    );

    let std_info = file.query_info::<FileStandardInformation>().await?;
    assert_eq!(std_info.end_of_file, TEST_DATA.len() as u64);
    assert_eq!(std_info.allocation_size >= TEST_DATA.len() as u64, true);
    assert_eq!(std_info.delete_pending, true.into());

    file.query_info::<FileStreamInformation>().await?;

    file.query_fs_info::<FileFsSizeInformation>().await?;
    file.query_fs_info::<FileFsFullSizeInformation>().await?;
    file.query_fs_info::<FileFsAttributeInformation>().await?;
    // file.query_fs_info::<FileFsControlInformation>().await?;
    file.query_fs_info::<FileFsDeviceInformation>().await?;
    file.query_fs_info::<FileFsVolumeInformation>().await?;
    file.query_fs_info::<FileFsSectorSizeInformation>().await?;
    file.query_fs_info::<FileFsObjectIdInformation>().await?;
    Ok(())
}

// Samba does not support FilePipe*Information classes
// #[test_log::test(maybe_async::test(
//     not(feature = "async"),
//     async(feature = "async", tokio::test(flavor = "multi_thread"))
// ))]
// #[serial] // Run only in a full-feature test, because it takes a while
// async fn test_pipe_query_information() -> Result<(), Box<dyn std::error::Error>> {
//     use smb::connection::EncryptionMode;

//     let (client, share_path) = make_server_connection(
//         UncPath::SMB_IPC_SHARE,
//         ConnectionConfig {
//             encryption_mode: EncryptionMode::Disabled,
//             ..Default::default()
//         }
//         .into(),
//     )
//     .await?;

//     let srvsvc_pipe = client.open_pipe(share_path.server(), "srvsvc").await?;

//     let test_result = do_test_pipe_query_information(&srvsvc_pipe).await;

//     srvsvc_pipe.close().await?;
//     client.close().await?;

//     Ok(test_result?)
// }

// #[maybe_async::maybe_async]
// async fn do_test_pipe_query_information(pipe: &smb::Pipe) -> smb::Result<()> {
//     pipe.query_info::<FilePipeInformation>().await?;
//     pipe.query_info::<FilePipeLocalInformation>().await?;
//     pipe.query_info::<FilePipeRemoteInformation>().await?;
//     Ok(())
// }
