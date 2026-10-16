use super::super::{
    PlaybackStoreWork,
    state::tests::{IdleBackend, seeded},
};
use super::*;
use crate::playback::{Placement, PlayRequest, RadioPlayRequest};
use playback::{
    BackendEvent, BackendFailure, PlaybackNotice, PlaybackSubscription, Provenance, TrackReference,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canceled_queue_preparation_publishes_loading_completion() {
    let (_directory, _database, sequence, owner, commands, _mailbox, _inbox) =
        seeded(Vec::new()).await;
    let (playback, _) = Playback::start(
        sequence,
        "queue-preparation",
        PlaybackSettings::default(),
        false,
        1,
        PlaybackOutput::Local,
        Box::new(IdleBackend(commands)),
        Arc::new(|| ClockSample {
            monotonic_millis: 0,
            unix_seconds: 0,
            local_period: "1970-01".into(),
        }),
        Arc::downgrade(&owner),
    )
    .unwrap();
    let mut updates = owner.updates.subscribe();
    assert!(!updates.recv().await.unwrap().unwrap().view.queue_loading);
    for placement in [Placement::Now, Placement::End] {
        let (cancel, cancellation) = std::sync::mpsc::channel();
        playback.downgrade().post(move |player, sample| {
            player
                .handle_command(
                    Command::Radio(RadioPlayRequest {
                        placement,
                        seed: library::RadioSeed::Track("https://example.test/radio-seed".into()),
                    }),
                    sample,
                )
                .unwrap();
            player.publish(sample);
            cancellation.recv().unwrap();
            player.shutdown(sample);
        });
        assert!(updates.recv().await.unwrap().unwrap().view.queue_loading);
        cancel.send(()).unwrap();
        assert!(!updates.recv().await.unwrap().unwrap().view.queue_loading);
    }
    playback.shutdown().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_operation_failures_publish_notices_without_a_current_track() {
    let (_directory, _database, sequence, owner, commands, _mailbox, _inbox) =
        seeded(Vec::new()).await;
    let (playback, _) = Playback::start(
        sequence,
        "failed-operation",
        PlaybackSettings::default(),
        false,
        1,
        PlaybackOutput::Local,
        Box::new(IdleBackend(commands)),
        Arc::new(|| ClockSample {
            monotonic_millis: 0,
            unix_seconds: 0,
            local_period: "1970-01".into(),
        }),
        Arc::downgrade(&owner),
    )
    .unwrap();
    let mut updates = owner.updates.subscribe();
    for _ in 0..2 {
        playback
            .command(Command::OperationFailed("Unavailable".into()))
            .unwrap();
        let projection = updates.recv().await.unwrap().unwrap();
        assert!(projection.view.transport.current.is_none());
        assert_eq!(
            projection.notices,
            [PlaybackNotice::OperationFailed("Unavailable".into())]
        );
    }
    playback.shutdown().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_and_backend_failures_publish_notices_and_keep_diagnostics() {
    for backend_failure in [false, true] {
        let (_directory, _database, sequence, owner, commands, _mailbox, _inbox) = seeded(vec![(
            TrackReference::direct(
                "https://example.test/track",
                "Track",
                "Artist",
                "Album",
                180_000,
            ),
            Provenance::Manual,
        )])
        .await;
        let (playback, _) = Playback::start(
            sequence,
            "failure-feedback",
            PlaybackSettings::default(),
            false,
            1,
            PlaybackOutput::Local,
            Box::new(IdleBackend(commands)),
            Arc::new(|| ClockSample {
                monotonic_millis: 0,
                unix_seconds: 0,
                local_period: "1970-01".into(),
            }),
            Arc::downgrade(&owner),
        )
        .unwrap();
        playback.command(Command::Play).unwrap();
        let message = if backend_failure {
            "Resource not found."
        } else {
            "the configured source no longer exists"
        };
        let mut updates = owner.updates.subscribe();
        playback
            .request(move |player, sample| {
                let run = player.current_run().unwrap();
                if backend_failure {
                    player.handle_backend(
                        BackendEvent::Error {
                            run,
                            error: BackendFailure::new(message),
                        },
                        sample,
                    );
                } else {
                    player.stream_failed(run, message.into(), sample);
                }
                Ok(())
            })
            .unwrap();
        let projection = updates.recv().await.unwrap().unwrap();
        assert_eq!(
            projection.notices,
            [PlaybackNotice::OperationFailed(message.into())]
        );
        assert_eq!(projection.view.transport.error.as_deref(), Some(message));
        playback.shutdown().unwrap();
    }
}

async fn published_queue_snapshot(
    updates: &mut PlaybackSubscription,
    owner: &PlaybackOwner,
) -> library::QueueRestore {
    loop {
        let projection = tokio::time::timeout(Duration::from_secs(5), updates.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if projection.view.queue_loading {
            continue;
        }
        assert!(projection.view.queue_window.len() <= library::QUEUE_CONTEXT_LIMIT);
        let (reply, done) = mpsc::sync_channel(1);
        owner
            .store_sender
            .send(PlaybackStoreWork::Flush(reply))
            .await
            .unwrap();
        tokio::task::spawn_blocking(move || done.recv().unwrap())
            .await
            .unwrap();
        return owner.database.restore_queue().await.unwrap();
    }
}

fn replace_queue(owner: &PlaybackOwner, prefix: &str, count: usize) {
    owner.play_selection(PlayRequest::from_input(
        library::QueueLoad::items(
            (0..count)
                .map(|index| {
                    (
                        TrackReference::direct(
                            format!("https://example.test/{prefix}/{index}"),
                            format!("{prefix} {index}"),
                            "Artist",
                            "Album",
                            180_000,
                        ),
                        Provenance::Manual,
                    )
                })
                .collect(),
        ),
        crate::playback::QueuePlacement::Now,
    ));
    owner.snapshot().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_queue_edits_publish_restorable_bounded_snapshots() {
    let (_directory, database, sequence, owner, commands, _mailbox, _inbox) =
        seeded(Vec::new()).await;
    let (playback, _) = Playback::start(
        sequence,
        "persistence-test",
        PlaybackSettings::default(),
        false,
        1,
        PlaybackOutput::Local,
        Box::new(IdleBackend(commands)),
        Arc::new(|| ClockSample {
            monotonic_millis: 0,
            unix_seconds: 0,
            local_period: "1970-01".into(),
        }),
        Arc::downgrade(&owner),
    )
    .unwrap();
    *owner.active.lock().unwrap() = Some(playback.clone());
    let (started, blocked) = mpsc::sync_channel(1);
    let (release, resume) = mpsc::sync_channel(1);
    playback.downgrade().post(move |_, _| {
        started.send(()).unwrap();
        resume.recv().unwrap();
    });
    blocked.recv().unwrap();
    let (returned, done) = mpsc::sync_channel(1);
    let controls = owner.clone();
    let caller = thread::spawn(move || {
        controls.set_volume(0.5);
        controls.remove(0);
        returned.send(()).unwrap();
    });
    let completed = done.recv_timeout(Duration::from_secs(2));
    release.send(()).unwrap();
    caller.join().unwrap();
    completed.expect("ordinary controls return while the playback worker is busy");
    playback.projection().unwrap();
    let mut updates = owner.updates.subscribe();
    replace_queue(&owner, "first", 150);
    let restored = published_queue_snapshot(&mut updates, &owner).await;
    assert_eq!(restored.entries.len(), 150);
    assert_eq!(restored.rows.len(), library::QUEUE_CONTEXT_LIMIT);

    replace_queue(&owner, "replacement", 3);
    let restored = published_queue_snapshot(&mut updates, &owner).await;
    assert_eq!(restored.rows.len(), 3);
    assert!(
        restored
            .rows
            .iter()
            .all(|row| row.track.media_uri.contains("/replacement/"))
    );
    assert_eq!(restored.current_index, Some(0));

    owner.remove(restored.rows[2].position);
    playback.projection().unwrap();
    assert_eq!(
        published_queue_snapshot(&mut updates, &owner)
            .await
            .rows
            .len(),
        2
    );

    owner.set_shuffle(true);
    playback.projection().unwrap();
    let before_volume = published_queue_snapshot(&mut updates, &owner).await;
    assert!(before_volume.shuffled);

    owner.set_volume(0.137);
    playback.projection().unwrap();
    let projection = updates.recv().await.unwrap().unwrap();
    assert_eq!(projection.view.controls.volume, 0.137);
    let after_volume = database.restore_queue().await.unwrap();
    assert_eq!(after_volume.entries, before_volume.entries);
    assert_eq!(after_volume.order, before_volume.order);
    assert_eq!(after_volume.current_index, before_volume.current_index);
    assert_eq!(after_volume.progress_millis, before_volume.progress_millis);
    assert_eq!(after_volume.repeat_mode, before_volume.repeat_mode);
    assert_eq!(after_volume.shuffled, before_volume.shuffled);

    owner.clear(true);
    playback.projection().unwrap();
    let restored = published_queue_snapshot(&mut updates, &owner).await;
    assert!(restored.rows.is_empty());
    playback.shutdown().unwrap();
}
