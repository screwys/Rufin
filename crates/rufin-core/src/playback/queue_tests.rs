use super::queue::Sequence;
use super::{PlaybackStoreWork, enqueue_queue_save, save_pending_queue};
use library::{Database, QueueInput, QueueRestore};
use playback::{QueueSelection, RepeatMode};
use std::sync::Mutex;

async fn replace_queue(
    sequence: &mut Sequence,
    database: &Database,
    replacement: usize,
    count: usize,
) {
    let page = database
        .read_queue(library::QueueReadRequest {
            context_title: None,
            input: QueueInput::Uris {
                order: (0..count)
                    .map(|index| format!("https://example.test/{replacement}/{index}"))
                    .collect(),
                context_id: format!("replacement-{replacement}").into(),
                source_start: 0,
            },
            anchor_index: 0,
            random_start: None,
            shuffled: None,
        })
        .await
        .unwrap();
    sequence.add_page(page, library::QueueReorderTarget::End, true, None);
}

fn assert_restored_state(restored: &QueueRestore, expected: &QueueRestore) {
    assert_eq!(restored.ids, expected.ids);
    assert_eq!(restored.entries, expected.entries);
    assert_eq!(restored.order, expected.order);
    assert_eq!(restored.current_index, expected.current_index);
    assert_eq!(restored.progress_millis, expected.progress_millis);
    assert_eq!(restored.repeat_mode, expected.repeat_mode);
    assert_eq!(restored.shuffled, expected.shuffled);
}

#[tokio::test]
async fn paused_queue_writer_retains_only_latest_membership_and_one_wakeup() {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(directory.path().join("queue.sqlite3"))
        .await
        .unwrap();
    let mut sequence = Sequence::new();
    let pending = Mutex::new(None);
    let (sender, work) = async_channel::unbounded();

    for replacement in 0..12 {
        replace_queue(&mut sequence, &database, replacement, 512).await;
        let update = sequence.take_persistence().unwrap();
        assert!(update.replace);
        enqueue_queue_save(&pending, &sender, update);

        sequence.remove(&QueueSelection::Positions(vec![511]));
        let update = sequence.take_persistence().unwrap();
        assert!(!update.replace);
        assert_eq!(update.removed.len(), 1);
        assert!(update.entries.is_empty());
        enqueue_queue_save(&pending, &sender, update);

        assert_eq!(work.len(), 1, "a pending save needs only one Store wakeup");
        let pending = pending.lock().unwrap();
        let update = pending.as_ref().unwrap();
        assert_eq!(update.entries.len(), 511);
        assert_eq!(update.ordinary.as_ref().unwrap().rows.len(), 511);
        assert_eq!(update.order.as_ref().unwrap().rows.len(), 511);
        let current = sequence.snapshot();
        assert!(
            update
                .entries
                .values()
                .all(|entry| current.entries.contains(entry)),
            "superseded membership must not remain in pending Store work"
        );
    }

    let expected = sequence.snapshot();
    assert!(matches!(
        work.try_recv().unwrap(),
        PlaybackStoreWork::Settings
    ));
    save_pending_queue(&database, &pending, &mut None)
        .await
        .unwrap();
    assert!(pending.lock().unwrap().is_none());
    assert!(work.is_empty());
    assert_restored_state(&database.restore_queue().await.unwrap(), &expected);
}

#[tokio::test]
async fn queue_save_coalesces_membership_order_and_latest_scalar_state() {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(directory.path().join("queue.sqlite3"))
        .await
        .unwrap();
    let mut sequence = Sequence::new();
    let pending = Mutex::new(None);
    let (sender, work) = async_channel::unbounded();

    replace_queue(&mut sequence, &database, 0, 24).await;
    let membership = sequence.take_persistence().unwrap();
    assert!(membership.replace);
    enqueue_queue_save(&pending, &sender, membership);

    sequence.shuffle(true, 19);
    let update = sequence.take_persistence().unwrap();
    assert!(update.entries.is_empty());
    assert!(update.order.is_some());
    enqueue_queue_save(&pending, &sender, update);

    sequence.advance_index(false);
    sequence.set_repeat_mode(RepeatMode::All);
    let update = sequence.take_persistence().unwrap();
    assert!(update.entries.is_empty());
    assert!(update.order.is_none());
    enqueue_queue_save(&pending, &sender, update);

    sequence.set_progress_millis(42_000);
    sequence.persist_state();
    enqueue_queue_save(&pending, &sender, sequence.take_persistence().unwrap());
    assert_eq!(work.len(), 1);
    {
        let pending = pending.lock().unwrap();
        let update = pending.as_ref().unwrap();
        assert!(update.replace);
        assert_eq!(update.current_index, Some(1));
        assert_eq!(update.progress_millis, 42_000);
        assert_eq!(update.repeat_mode, RepeatMode::All);
        assert!(update.shuffled);
    }
    let expected = sequence.snapshot();
    assert!(matches!(
        work.try_recv().unwrap(),
        PlaybackStoreWork::Settings
    ));
    save_pending_queue(&database, &pending, &mut None)
        .await
        .unwrap();
    assert_restored_state(&database.restore_queue().await.unwrap(), &expected);

    sequence.shuffle(false, 0);
    enqueue_queue_save(&pending, &sender, sequence.take_persistence().unwrap());
    sequence.set_repeat_mode(RepeatMode::One);
    enqueue_queue_save(&pending, &sender, sequence.take_persistence().unwrap());
    {
        let pending = pending.lock().unwrap();
        let update = pending.as_ref().unwrap();
        assert!(!update.replace);
        assert!(update.entries.is_empty());
        assert!(update.order.is_some());
    }
    let expected = sequence.snapshot();
    assert_eq!(work.len(), 1);
    assert!(matches!(
        work.try_recv().unwrap(),
        PlaybackStoreWork::Settings
    ));
    save_pending_queue(&database, &pending, &mut None)
        .await
        .unwrap();
    assert_restored_state(&database.restore_queue().await.unwrap(), &expected);

    sequence.set_progress_millis(43_000);
    sequence.persist_state();
    let update = sequence.take_persistence().unwrap();
    assert!(update.entries.is_empty());
    assert!(update.order.is_none());
    let expected = sequence.snapshot();
    enqueue_queue_save(&pending, &sender, update);
    assert!(matches!(
        work.try_recv().unwrap(),
        PlaybackStoreWork::Settings
    ));
    save_pending_queue(&database, &pending, &mut None)
        .await
        .unwrap();
    assert_restored_state(&database.restore_queue().await.unwrap(), &expected);
    assert!(work.is_empty());

    let mut failed = None;
    let later = database
        .read_queue(library::QueueReadRequest {
            context_title: None,
            input: QueueInput::MediaUris {
                order: ["failed", "later"]
                    .map(|suffix| format!("https://example.test/{suffix}"))
                    .into(),
                provenance: playback::Provenance::Manual,
            },
            anchor_index: 0,
            random_start: None,
            shuffled: None,
        })
        .await
        .unwrap();
    database.clone().close().await.unwrap();
    for (index, entry) in later.entries.into_iter().enumerate() {
        sequence.add_page(
            library::QueueReadPage {
                entries: vec![entry],
                rows: Vec::new(),
                current_index: 0,
            },
            library::QueueReorderTarget::End,
            false,
            None,
        );
        enqueue_queue_save(&pending, &sender, sequence.take_persistence().unwrap());
        if index == 0 {
            assert!(
                save_pending_queue(&database, &pending, &mut failed)
                    .await
                    .is_err()
            );
            assert!(failed.is_some());
        }
    }
    let reopened = Database::open(directory.path().join("queue.sqlite3"))
        .await
        .unwrap();
    save_pending_queue(&reopened, &pending, &mut failed)
        .await
        .unwrap();
    assert!(failed.is_none());
    assert_restored_state(
        &reopened.restore_queue().await.unwrap(),
        &sequence.snapshot(),
    );
}
