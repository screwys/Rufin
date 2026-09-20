#![cfg(not(feature = "single_threaded"))]
use serial_test::serial;
use smb::{
    ConnectionConfig, Directory, FileCreateArgs, connection::EncryptionMode, sync_helpers::*,
};
use smb_fscc::*;
use smb_msg::NotifyFilter;
use std::sync::Arc;
mod common;

#[cfg(feature = "multi_threaded")]
use std::thread::sleep;
#[cfg(feature = "async")]
use tokio::time::sleep;

use common::TestConstants;
use common::make_server_connection;
const NEW_FILE_NAME_UNDER_WORKDIR_PREFIX: &str = "notify_file";

const NUM_TEST_FILES: u32 = 5;

macro_rules! make_smb_notify_test {
    (
        $($watch_callback:ident,)+
    ) => {
        pastey::paste!{
            $(
#[test_log::test(maybe_async::test(
    not(feature = "async"),
    async(feature = "async", tokio::test(flavor = "multi_thread"))
))]
#[serial]
async fn [<test_smb_notify $watch_callback>]() -> Result<(), Box<dyn std::error::Error>> {
    do_test_smb_notify($watch_callback).await
}
            )+
        }
    };
}

make_smb_notify_test!(legacy_watch, stream_iter_watch,);

#[maybe_async::maybe_async]
async fn do_test_smb_notify(
    f_start_notify_task: fn(Arc<Semaphore>, Directory),
) -> Result<(), Box<dyn std::error::Error>> {
    let (client, share_path) = make_server_connection(
        TestConstants::DEFAULT_SHARE,
        ConnectionConfig {
            encryption_mode: EncryptionMode::Disabled,
            ..Default::default()
        }
        .into(),
    )
    .await?;

    // Create the files
    for i in 0..NUM_TEST_FILES {
        let file_name = format!("{}_{i}.txt", NEW_FILE_NAME_UNDER_WORKDIR_PREFIX);
        client
            .create_file(
                &share_path.clone().with_path(&file_name),
                &FileCreateArgs::make_create_new(Default::default(), Default::default()),
            )
            .await?
            .unwrap_file()
            .close()
            .await?;
    }
    let dir = client
        .create_file(
            &share_path,
            &FileCreateArgs::make_open_existing(
                DirAccessMask::new().with_list_directory(true).into(),
            ),
        )
        .await?
        .unwrap_dir();

    let notified_sem = Arc::new(Semaphore::new(0));
    f_start_notify_task(notified_sem.clone(), dir);
    // Launch tasks to wait for notifications.
    // Another connection now modifying the file...
    const FIRST_BATCH: u32 = 3;

    delete_many_files(
        TestConstants::DEFAULT_SHARE,
        &((0..FIRST_BATCH).collect::<Vec<u32>>()),
    )
    .await?;
    // Wait for notifications to arrive.
    notified_sem.acquire_many(FIRST_BATCH).await?.forget();

    sleep(std::time::Duration::from_secs(2)).await;
    delete_many_files(
        TestConstants::DEFAULT_SHARE,
        &((FIRST_BATCH..NUM_TEST_FILES).collect::<Vec<u32>>()),
    )
    .await?;
    // Wait for notifications to arrive.
    notified_sem
        .acquire_many(NUM_TEST_FILES - FIRST_BATCH)
        .await?
        .forget();
    Ok(())
}

#[maybe_async::async_impl]
async fn delete_many_files(share_path: &'static str, rng_numbers: &[u32]) -> smb::Result<()> {
    // for each number, iterate and call delete_from_another_connection. Wait for all at the end.
    use futures_util::future::join_all;

    // Connect the client:

    let (client, share_path) = make_server_connection(
        share_path,
        ConnectionConfig {
            encryption_mode: EncryptionMode::Disabled,
            ..Default::default()
        }
        .into(),
    )
    .await?;

    let client = Arc::new(client);

    let futures = rng_numbers.iter().map(|&i| {
        let share_path = share_path.clone();
        let client = client.clone();
        tokio::spawn(async move {
            delete_file_from_another_connection(
                client,
                share_path,
                &format!("{}_{i}.txt", NEW_FILE_NAME_UNDER_WORKDIR_PREFIX),
            )
            .await
            .unwrap();
        })
    });

    let results = join_all(futures).await;
    for r in results {
        r.expect("delete task panicked");
    }
    Ok(())
}

// The same as above, but with threads:
#[maybe_async::sync_impl]
fn delete_many_files(share_path: &'static str, rng_numbers: &[u32]) -> smb::Result<()> {
    // for each number, iterate and call delete_from_another_connection. Wait for all at the end.
    use std::sync::mpsc;
    use std::thread;
    let (tx, rx) = mpsc::channel();
    // Connect the client:
    let (client, share_path) = make_server_connection(
        share_path,
        ConnectionConfig {
            encryption_mode: EncryptionMode::Disabled,
            ..Default::default()
        }
        .into(),
    )
    .unwrap();
    let client = Arc::new(client);
    for &i in rng_numbers {
        let share_path = share_path.clone();
        let client = client.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            delete_file_from_another_connection(
                client,
                share_path,
                &format!("{}_{i}.txt", NEW_FILE_NAME_UNDER_WORKDIR_PREFIX),
            )
            .unwrap();
            tx.send(()).unwrap();
        });
    }
    // Wait for all threads to finish:
    for _ in rng_numbers {
        rx.recv().unwrap();
    }
    Ok(())
}

#[maybe_async::async_impl]
fn legacy_watch(sem: Arc<Semaphore>, r: Directory) {
    tokio::spawn(async move {
        loop {
            for notification in r.watch(NotifyFilter::all(), true).await.unwrap() {
                on_notification(sem.clone(), notification);
            }
        }
    });
}
#[maybe_async::sync_impl]
fn legacy_watch(sem: Arc<Semaphore>, r: Directory) {
    std::thread::spawn(move || {
        loop {
            for notification in r.watch(NotifyFilter::all(), true).unwrap() {
                on_notification(sem.clone(), notification);
            }
        }
    });
}

#[maybe_async::async_impl]
fn stream_iter_watch(sem: Arc<Semaphore>, r: Directory) {
    use futures_util::TryStreamExt;
    tokio::spawn(async move {
        let r = Arc::new(r);

        Directory::watch_stream(&r, NotifyFilter::all(), true)
            .unwrap()
            .try_for_each(|notification| {
                let value = sem.clone();
                async move {
                    on_notification(value, notification);
                    Ok(())
                }
            })
            .await
            .unwrap_or_else(|_| panic!("Error in notification stream"));
        r.close().await.unwrap();
    });
}
#[maybe_async::sync_impl]
fn stream_iter_watch(sem: Arc<Semaphore>, r: Directory) {
    std::thread::spawn(move || {
        let r = Arc::new(r);
        for notification in Directory::watch_stream(&r, NotifyFilter::all(), true).unwrap() {
            on_notification(sem.clone(), notification.unwrap());
        }
        r.close().unwrap();
    });
}

fn on_notification(sem: Arc<Semaphore>, notification: FileNotifyInformation) {
    if notification.action == NotifyAction::Removed {
        sem.add_permits(1);
    }
}

#[maybe_async::maybe_async]
async fn delete_file_from_another_connection(
    client: Arc<smb::Client>,
    share_path: smb::UncPath,
    file_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = client
        .create_file(
            &share_path.with_path(file_name),
            &FileCreateArgs::make_open_existing(
                FileAccessMask::new()
                    .with_delete(true)
                    .with_generic_read(true),
            ),
        )
        .await?
        .unwrap_file();

    file.set_info(FileDispositionInformation {
        delete_pending: true.into(),
    })
    .await?;

    file.close().await?;

    // We are exiting, and file is closed, and deleted!
    Ok(())
}
