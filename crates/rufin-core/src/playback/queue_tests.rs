use super::queue::{QueuePersistence, QueuePersistenceKind, Sequence};
use super::{PlaybackStoreWork, enqueue_queue_save, save_pending_queue};
use library::{Database, QueueInput, QueueRestore};
use playback::RepeatMode;
use std::sync::{Arc, Mutex};

async fn replace_queue(
    sequence: &mut Sequence,
    database: &Database,
    replacement: usize,
    count: usize,
) {
    let page = database
        .read_queue(library::QueueReadRequest::Capture {
            context_title: None,
            input: Box::new(QueueInput::Uris {
                order: (0..count)
                    .map(|index| format!("https://example.test/{replacement}/{index}"))
                    .collect(),
                context_id: format!("replacement-{replacement}").into(),
                source_start: 0,
            }),
            anchor_index: 0,
            random_start: None,
            shuffled: None,
        })
        .await
        .unwrap();
    sequence.add_page(page, library::QueueReorderTarget::End, true, None, &mut []);
}

fn assert_restored_state(restored: &QueueRestore, expected: &QueueRestore) {
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
    let mut retired_memberships = Vec::new();
    let mut retired_orders = Vec::new();

    for replacement in 0..12 {
        replace_queue(&mut sequence, &database, replacement, 512).await;
        let kind = sequence.take_persistence().unwrap();
        let snapshot = QueuePersistence::capture(&sequence, kind);
        assert_eq!(snapshot.kind(), QueuePersistenceKind::Membership);
        let remove = snapshot.state().order.len() - 1;
        retired_memberships.push(Arc::downgrade(&snapshot.state().entries));
        retired_orders.push(Arc::downgrade(&snapshot.state().order));
        enqueue_queue_save(&pending, &sender, snapshot);

        sequence.remove(&[remove], &mut []);
        let kind = sequence.take_persistence().unwrap();
        let snapshot = QueuePersistence::capture(&sequence, kind);
        assert_eq!(snapshot.kind(), QueuePersistenceKind::Membership);
        assert_eq!(snapshot.state().entries.len(), 511);
        let current_membership = Arc::downgrade(&snapshot.state().entries);
        let current_order = Arc::downgrade(&snapshot.state().order);
        enqueue_queue_save(&pending, &sender, snapshot);

        assert_eq!(work.len(), 1, "a pending save needs only one Store wakeup");
        assert!(
            retired_memberships
                .iter()
                .all(|owner| owner.upgrade().is_none()),
            "superseded membership must not remain in pending Store work"
        );
        assert!(
            retired_orders.iter().all(|owner| owner.upgrade().is_none()),
            "superseded order must not remain in pending Store work"
        );
        retired_memberships.push(current_membership);
        retired_orders.push(current_order);
    }

    assert!(retired_memberships.last().unwrap().upgrade().is_some());
    assert!(retired_orders.last().unwrap().upgrade().is_some());
    let expected = pending.lock().unwrap().as_ref().unwrap().state().clone();
    assert!(
        expected
            .entries
            .iter()
            .all(|entry| entry.media_uri.starts_with("https://example.test/11/"))
    );
    assert!(matches!(
        work.try_recv().unwrap(),
        PlaybackStoreWork::Settings
    ));
    save_pending_queue(&database, &pending).await.unwrap();
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
    let kind = sequence.take_persistence().unwrap();
    let membership = QueuePersistence::capture(&sequence, kind);
    assert_eq!(membership.kind(), QueuePersistenceKind::Membership);
    enqueue_queue_save(&pending, &sender, membership);

    sequence.shuffle(true, 19, &mut []);
    let kind = sequence.take_persistence().unwrap();
    assert_eq!(kind, QueuePersistenceKind::Order);
    enqueue_queue_save(
        &pending,
        &sender,
        QueuePersistence::capture(&sequence, kind),
    );

    sequence.advance_index(false);
    sequence.set_repeat_mode(RepeatMode::All);
    let kind = sequence.take_persistence().unwrap();
    assert_eq!(kind, QueuePersistenceKind::State);
    enqueue_queue_save(
        &pending,
        &sender,
        QueuePersistence::capture(&sequence, kind),
    );

    sequence.set_progress_millis(42_000);
    enqueue_queue_save(
        &pending,
        &sender,
        QueuePersistence::capture(&sequence, QueuePersistenceKind::State),
    );
    assert_eq!(work.len(), 1);
    let expected = {
        let pending = pending.lock().unwrap();
        let snapshot = pending.as_ref().unwrap();
        assert_eq!(snapshot.kind(), QueuePersistenceKind::Membership);
        snapshot.state().clone()
    };
    assert_eq!(expected.current_index, Some(1));
    assert_eq!(expected.progress_millis, 42_000);
    assert_eq!(expected.repeat_mode, RepeatMode::All);
    assert!(expected.shuffled);
    assert!(matches!(
        work.try_recv().unwrap(),
        PlaybackStoreWork::Settings
    ));
    save_pending_queue(&database, &pending).await.unwrap();
    assert_restored_state(&database.restore_queue().await.unwrap(), &expected);

    sequence.shuffle(false, 0, &mut []);
    let kind = sequence.take_persistence().unwrap();
    enqueue_queue_save(
        &pending,
        &sender,
        QueuePersistence::capture(&sequence, kind),
    );
    sequence.set_repeat_mode(RepeatMode::One);
    let kind = sequence.take_persistence().unwrap();
    enqueue_queue_save(
        &pending,
        &sender,
        QueuePersistence::capture(&sequence, kind),
    );
    let expected = {
        let pending = pending.lock().unwrap();
        let snapshot = pending.as_ref().unwrap();
        assert_eq!(snapshot.kind(), QueuePersistenceKind::Order);
        snapshot.state().clone()
    };
    assert_eq!(work.len(), 1);
    assert!(matches!(
        work.try_recv().unwrap(),
        PlaybackStoreWork::Settings
    ));
    save_pending_queue(&database, &pending).await.unwrap();
    assert_restored_state(&database.restore_queue().await.unwrap(), &expected);

    sequence.set_progress_millis(43_000);
    let snapshot = QueuePersistence::capture(&sequence, QueuePersistenceKind::State);
    assert_eq!(snapshot.kind(), QueuePersistenceKind::State);
    let expected = snapshot.state().clone();
    enqueue_queue_save(&pending, &sender, snapshot);
    assert!(matches!(
        work.try_recv().unwrap(),
        PlaybackStoreWork::Settings
    ));
    save_pending_queue(&database, &pending).await.unwrap();
    assert_restored_state(&database.restore_queue().await.unwrap(), &expected);
    assert!(work.is_empty());
}
