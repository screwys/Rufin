use super::{PlaybackStoreWork, enqueue_queue_save, save_pending_queue};
use library::{Database, QueueInput, QueueRestore};
use playback::{
    BackendCommand, BackendError, BackendEvent, ClockSample, PlayRequest, Playback,
    PlaybackBackend, PlaybackOutput, PlaybackSettings, PlaybackUpdate, QueuePersistence,
    QueuePersistenceKind, QueuePlacement, RepeatMode, Sequence, SessionCommand, SessionEffect,
};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

struct IdleBackend;

impl PlaybackBackend for IdleBackend {
    fn send(&mut self, _: BackendCommand) -> Result<(), BackendError> {
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        Vec::new()
    }
}

fn start_playback() -> (Playback, mpsc::Receiver<PlaybackUpdate>) {
    let (sender, updates) = mpsc::channel();
    let (playback, _) = Playback::start(
        Sequence::new(),
        "queue-writer-test",
        PlaybackSettings::default(),
        false,
        1,
        PlaybackOutput::Local,
        Box::new(IdleBackend),
        Arc::new(|| ClockSample {
            monotonic_millis: 0,
            unix_seconds: 0,
            local_period: "1970-01".into(),
        }),
        move |update| {
            let _ = sender.send(update);
        },
    )
    .unwrap();
    (playback, updates)
}

fn replace_queue(playback: &Playback, replacement: usize, count: usize) {
    let request = PlayRequest::ordered(
        QueueInput::Uris {
            order: (0..count)
                .map(|index| format!("https://example.test/{replacement}/{index}"))
                .collect(),
            context_id: format!("replacement-{replacement}").into(),
            source_start: 0,
        },
        0,
        QueuePlacement::Now,
        false,
    );
    let reservation = playback.admit_play(&request).unwrap().unwrap();
    let (batch, placement) = request.compact_batch(7);
    assert!(
        playback
            .complete_materialization(reservation.id, batch, placement)
            .unwrap()
    );
}

async fn next_snapshot(
    playback: &Playback,
    updates: &mpsc::Receiver<PlaybackUpdate>,
    database: &Database,
) -> QueuePersistence {
    loop {
        let update = updates.recv_timeout(Duration::from_secs(5)).unwrap();
        for effect in update.effects {
            if let SessionEffect::Queue { id, request } = effect {
                let result = database.read_queue(request).await.unwrap();
                playback
                    .command(SessionCommand::QueueComplete {
                        id,
                        result: Box::new(Ok(result)),
                    })
                    .unwrap();
            }
        }
        if let Some(snapshot) = update.queue_persistence {
            return snapshot;
        }
    }
}

fn assert_restored_state(restored: &QueueRestore, expected: &QueueRestore) {
    assert_eq!(restored.entries, expected.entries);
    assert_eq!(restored.order, expected.order);
    assert_eq!(restored.current(), expected.current());
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
    let (playback, updates) = start_playback();
    let pending = Mutex::new(None);
    let (sender, work) = async_channel::unbounded();
    let mut retired_memberships = Vec::new();
    let mut retired_orders = Vec::new();

    for replacement in 0..12 {
        replace_queue(&playback, replacement, 512);
        let snapshot = next_snapshot(&playback, &updates, &database).await;
        assert_eq!(snapshot.kind(), QueuePersistenceKind::Membership);
        let remove = snapshot.state().entries.last().unwrap().occurrence.clone();
        retired_memberships.push(Arc::downgrade(&snapshot.state().entries));
        retired_orders.push(Arc::downgrade(&snapshot.state().order));
        enqueue_queue_save(&pending, &sender, snapshot);

        playback.command(SessionCommand::Remove(remove)).unwrap();
        let snapshot = next_snapshot(&playback, &updates, &database).await;
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
    playback.shutdown().unwrap();
}

#[tokio::test]
async fn queue_save_coalesces_membership_order_and_latest_scalar_state() {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(directory.path().join("queue.sqlite3"))
        .await
        .unwrap();
    let (playback, updates) = start_playback();
    let pending = Mutex::new(None);
    let (sender, work) = async_channel::unbounded();

    replace_queue(&playback, 0, 24);
    let membership = next_snapshot(&playback, &updates, &database).await;
    assert_eq!(membership.kind(), QueuePersistenceKind::Membership);
    enqueue_queue_save(&pending, &sender, membership);

    for (command, expected_kind) in [
        (
            SessionCommand::SetShuffle {
                enabled: true,
                seed: 19,
            },
            QueuePersistenceKind::Order,
        ),
        (SessionCommand::Next, QueuePersistenceKind::State),
        (
            SessionCommand::SetRepeat(RepeatMode::All),
            QueuePersistenceKind::State,
        ),
        (SessionCommand::Stop, QueuePersistenceKind::State),
        (SessionCommand::Seek(42_000), QueuePersistenceKind::State),
    ] {
        playback.command(command).unwrap();
        let snapshot = next_snapshot(&playback, &updates, &database).await;
        assert_eq!(snapshot.kind(), expected_kind);
        enqueue_queue_save(&pending, &sender, snapshot);
    }
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

    for command in [
        SessionCommand::SetShuffle {
            enabled: false,
            seed: 0,
        },
        SessionCommand::SetRepeat(RepeatMode::One),
    ] {
        playback.command(command).unwrap();
        enqueue_queue_save(
            &pending,
            &sender,
            next_snapshot(&playback, &updates, &database).await,
        );
    }
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

    playback.command(SessionCommand::Seek(43_000)).unwrap();
    let snapshot = next_snapshot(&playback, &updates, &database).await;
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
    playback.shutdown().unwrap();
}
