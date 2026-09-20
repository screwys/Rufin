use serial_test::serial;
use smb::{ConnectionConfig, Directory, connection::EncryptionMode, sync_helpers::*, tree::Tree};
use smb_fscc::*;
use smb_msg::CreateOptions;
use std::sync::Arc;

#[cfg(feature = "async")]
use futures_util::StreamExt;
mod common;
use common::TestConstants;
use common::make_server_connection;
use smb::FileCreateArgs;

const LONG_DIR: &str = "longdir";
const FILE_PREFIX: &'static str = "test_file_with_a_long_name_to_take_up_some_space_when_dir_query_performed_and_consume_buffer_size_";
const NUM_FILES: usize = 100;

/// This test is to check if we can iterate over a long directory
/// To make sure it works properly, since dealing with streams can be tricky.
#[test_log::test(maybe_async::test(
    not(feature = "async"),
    async(feature = "async", tokio::test(flavor = "multi_thread"))
))]
#[serial] // Run only in a full-feature test, because it takes a while
async fn test_smb_iterating_long_directory() -> Result<(), Box<dyn std::error::Error>> {
    let (client, share_path) = make_server_connection(
        TestConstants::DEFAULT_SHARE,
        ConnectionConfig {
            encryption_mode: EncryptionMode::Disabled,
            ..Default::default()
        }
        .into(),
    )
    .await?;

    let client = Arc::new(Mutex::new(client));
    let long_dir_path = share_path.clone().with_path(LONG_DIR);
    // Mkdir
    client
        .lock()
        .await
        .unwrap()
        .create_file(
            &long_dir_path,
            &FileCreateArgs::make_create_new(
                FileAttributes::new().with_directory(true),
                CreateOptions::new().with_directory_file(true),
            ),
        )
        .await?;

    // Create NUM_ITEMS files
    for i in 0..NUM_FILES {
        let file_name = format!("{}\\{}{}", LONG_DIR, FILE_PREFIX, i);
        let file = client
            .lock()
            .await
            .unwrap()
            .create_file(
                &share_path.clone().with_path(&file_name),
                &FileCreateArgs::make_create_new(Default::default(), Default::default()),
            )
            .await?
            .unwrap_file();
        file.close().await?;
    }

    // Query directory and make sure our files exist there, delete each file found.
    let directory = client
        .lock()
        .await
        .unwrap()
        .create_file(
            &long_dir_path,
            &FileCreateArgs::make_open_existing(
                DirAccessMask::new()
                    .with_list_directory(true)
                    .with_synchronize(true)
                    .into(),
            ),
        )
        .await?
        .unwrap_dir();
    let directory = Arc::new(directory);
    const SMALL_BUFFER_SIZE_FOR_MANY_ITERATIONS: u32 = 0x300;
    let found = Directory::query_with_options::<FileFullDirectoryInformation>(
        &directory,
        &format!("{}*", FILE_PREFIX),
        SMALL_BUFFER_SIZE_FOR_MANY_ITERATIONS,
    )
    .await?
    .fold(0, |sum, entry| {
        let client = client.clone();
        let share_path = share_path.clone();
        async move {
            let entry = entry.unwrap();
            let file_name = entry.file_name.to_string();
            assert!(file_name.starts_with(FILE_PREFIX));
            let file_number: usize = file_name[FILE_PREFIX.len()..].parse().unwrap();
            assert!(file_number < NUM_FILES);

            // .. And delete the file!
            let full_file_path = share_path.with_path(&format!("{}\\{}", LONG_DIR, file_name));
            let file = client
                .lock()
                .await
                .unwrap()
                .create_file(
                    &full_file_path,
                    &FileCreateArgs::make_open_existing(
                        FileAccessMask::new()
                            .with_generic_read(true)
                            .with_delete(true),
                    ),
                )
                .await
                .unwrap()
                .unwrap_file();
            file.set_info(FileDispositionInformation {
                delete_pending: true.into(),
            })
            .await
            .unwrap();
            file.close().await.unwrap();
            sum + 1
        }
    })
    .await;

    assert_eq!(found, NUM_FILES);
    directory.close().await?;

    // Cleanup
    let directory = client
        .lock()
        .await
        .unwrap()
        .create_file(
            &long_dir_path,
            &FileCreateArgs::make_open_existing(FileAccessMask::new().with_delete(true)),
        )
        .await?
        .unwrap_dir();
    directory
        .set_info(FileDispositionInformation {
            delete_pending: true.into(),
        })
        .await?;
    directory.close().await?;
    // Wait for the delete to be processed

    Ok(())
}

#[maybe_async::maybe_async]
pub async fn remove_file_by_name(tree: &Tree, file_name: &str) -> smb::Result<()> {
    let file = tree
        .open_existing(
            file_name,
            FileAccessMask::new()
                .with_generic_read(true)
                .with_delete(true),
        )
        .await?
        .unwrap_file();
    file.set_info(FileDispositionInformation {
        delete_pending: true.into(),
    })
    .await?;
    Ok(())
}
