use common::{TestConstants, make_server_connection};
#[cfg(feature = "async")]
use futures_util::StreamExt;
use serial_test::serial;
use smb::{
    ConnectionConfig, Directory, FileCreateArgs, ReadAt, WriteAt, connection::EncryptionMode,
};
use smb_fscc::*;
use smb_msg::{AdditionalInfo, CreateOptions, Dialect};
use std::sync::Arc;
mod common;

macro_rules! basic_test {
    ([$dialect:ident], [$($encrypt_mode:ident),*]) => {
        $(
            pastey::paste! {
                #[test_log::test(maybe_async::test(
                    not(feature = "async"),
                    async(feature = "async", tokio::test(flavor = "multi_thread"))
                ))]
                #[serial]
                pub async fn [<test_smbint_ $dialect:lower _e $encrypt_mode:lower>]() -> Result<(), Box<dyn std::error::Error>> {
                    test_smb_integration_dialect_encrpytion_mode(Dialect::$dialect, EncryptionMode::$encrypt_mode).await
                }
            }
        )*
    };

    ([$($dialect:ident),*], $encrypt_modes:tt) => {
        $(
            basic_test!([$dialect],  $encrypt_modes);
        )*
    };

}

// Encryption tests, adapt to current features
#[cfg(feature = "__encrypt_core")]
basic_test!([Smb030, Smb0302, Smb0311], [Disabled, Required]);
#[cfg(not(feature = "__encrypt_core"))]
basic_test!([Smb030, Smb0302, Smb0311], [Disabled]);

basic_test!([Smb0202, Smb021], [Disabled]);

#[maybe_async::maybe_async]
async fn test_smb_integration_dialect_encrpytion_mode(
    force_dialect: Dialect,
    encryption_mode: EncryptionMode,
) -> Result<(), Box<dyn std::error::Error>> {
    log::info!("Testing with dialect: {force_dialect:?}, enc? {encryption_mode:?}",);

    let connection_config = ConnectionConfig {
        min_dialect: Some(force_dialect),
        max_dialect: Some(force_dialect),
        encryption_mode,
        ..Default::default()
    };

    let (client, share_path) =
        make_server_connection(TestConstants::DEFAULT_SHARE, Some(connection_config)).await?;

    const TEST_FILE: &str = "test.txt";
    const TEST_DATA: &[u8] = b"Hello, World!";

    let test_file_path = share_path.clone().with_path(TEST_FILE);

    // Hello, World! > test.txt
    let security = {
        let file = client
            .create_file(
                &test_file_path,
                &FileCreateArgs::make_create_new(
                    FileAttributes::new().with_archive(true),
                    CreateOptions::new(),
                ),
            )
            .await?
            .unwrap_file();

        file.write_at(TEST_DATA, 0).await?;

        // Query security info (owner only)
        let r = file
            .query_security_info(AdditionalInfo::new().with_owner_security_information(true))
            .await?;

        file.close().await?;

        r
    };

    if security.owner_sid.is_none() {
        return Err("No owner SID found".into());
    }

    // Query directory and make sure our file exists there:
    {
        let directory = client
            .create_file(
                &share_path,
                &FileCreateArgs::make_open_existing(
                    DirAccessMask::new().with_list_directory(true).into(),
                ),
            )
            .await?
            .unwrap_dir();
        let directory = Arc::new(directory);
        let ds = Directory::query::<FileDirectoryInformation>(&directory, TEST_FILE).await?;
        let mut found = false;

        ds.for_each(|entry| {
            if entry.unwrap().file_name.to_string() == TEST_FILE {
                found = true;
            }
            async {}
        })
        .await;

        if !found {
            return Err("File not found in directory".into());
        }

        directory.close().await?;
    }

    let file = client
        .create_file(
            &test_file_path,
            &FileCreateArgs::make_open_existing(
                FileAccessMask::new()
                    .with_delete(true)
                    .with_file_read_data(true)
                    .with_file_read_attributes(true),
            ),
        )
        .await?
        .unwrap_file();

    // So anyway it will be deleted at the end.
    file.set_info(FileDispositionInformation {
        delete_pending: true.into(),
    })
    .await?;

    let mut buf = [0u8; TEST_DATA.len() + 2];
    let read_length = file.read_at(&mut buf, 0).await?;
    assert_eq!(read_length, TEST_DATA.len());
    assert_eq!(&buf[..read_length], TEST_DATA);

    // Query file info.
    let all_info = file.query_info::<FileAllInformation>().await?;
    assert_eq!(
        all_info.name.file_name.to_string(),
        "\\".to_string() + TEST_FILE
    );

    // Query filesystem info.
    file.query_fs_info::<FileFsSizeInformation>().await?;

    assert_eq!(all_info.standard.end_of_file, TEST_DATA.len() as u64);

    file.close().await?;

    Ok(())
}
