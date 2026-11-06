use super::super::{
    PlaybackOwner, PlaybackStoreWork,
    actor::{self, Message, Playback},
};
use super::*;
use crate::playback::QueuePlacement;
use crate::{
    lyrics::LyricsService, settings::SettingsFile, source::SourceOwner, waveform::WaveformOwner,
};
use playback::{BackendError, PlaybackBackend};
use playback::{QueueProvenance, TrackReference};
use std::sync::Mutex;

pub(in crate::playback) struct IdleBackend(pub(in crate::playback) Arc<Mutex<Vec<BackendCommand>>>);
impl PlaybackBackend for IdleBackend {
    fn send(&mut self, command: BackendCommand) -> Result<(), BackendError> {
        self.0.lock().unwrap().push(command);
        Ok(())
    }
    fn drain_events(&mut self) -> Vec<BackendEvent> {
        Vec::new()
    }
}

pub(in crate::playback) async fn seeded(
    items: Vec<(TrackReference, QueueProvenance)>,
) -> (
    tempfile::TempDir,
    library::Catalog,
    Sequence,
    Arc<PlaybackOwner>,
    Arc<Mutex<Vec<BackendCommand>>>,
    Playback,
    std::sync::mpsc::Receiver<Message>,
) {
    let directory = tempfile::tempdir().unwrap();
    let catalog = library::Catalog::open(directory.path().join("queue.sqlite3"))
        .await
        .unwrap();
    let mut sequence = Sequence::new();
    if !items.is_empty() {
        let page = crate::playback::PlayRequest::from_input(
            library::QueueLoad::items(items),
            QueuePlacement::Now,
        )
        .prepare(&catalog, false, 0)
        .await
        .unwrap();
        sequence.add_page(page, library::QueueReorderTarget::End, true);
    }
    if let Some(request) = sequence.read_request() {
        let items = catalog
            .queue_rows_for_entries(
                &request
                    .iter()
                    .enumerate()
                    .map(|(position, (_, entry))| (position, entry.clone()))
                    .collect::<Vec<_>>(),
                &library::ReadCancellation::new(),
            )
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.track.clone());
        sequence.hydrate(request.into_iter().map(|(id, _)| id).zip(items).collect());
    }
    catalog.save_queue(&sequence.snapshot()).await.unwrap();
    let runtime = tokio::runtime::Handle::current();
    let settings = SettingsFile::open(directory.path().join("settings.json")).unwrap();
    let downloads = directory.path().join("downloads");
    let source = SourceOwner::open_dormant(
        directory.path().join("artwork"),
        Arc::new(catalog.clone()),
        downloads,
        settings.clone(),
        Arc::new(secrets::SwitchableSecretStore::new(Arc::new(
            secrets::MemorySecretStore::new(),
        ))),
        runtime.clone(),
        crate::source::SourceOutputs {
            events: async_channel::unbounded().0,
            downloads: async_channel::unbounded().0,
        },
    )
    .expect("the source owner opens");
    let waveform = WaveformOwner::new(
        runtime.clone(),
        async_channel::unbounded().0,
        &directory.path().join("playback"),
        &settings,
    );
    settings
        .update(|stored| {
            stored.ui.lyrics.preferences.external_lyrics_enabled = false;
            Ok(())
        })
        .unwrap();
    let lyrics = LyricsService::new(
        catalog.clone(),
        source.clone(),
        runtime.clone(),
        &settings,
        async_channel::unbounded().0,
        directory.path().join("dictionary"),
    );
    let scrobbler = Arc::new(crate::scrobbling::Scrobbler::new(
        catalog.clone(),
        runtime.clone(),
        settings.clone(),
    ));
    let commands = Arc::new(Mutex::new(Vec::new()));
    let backend_commands = commands.clone();
    let (visualizer, visualizer_drain) = async_channel::bounded(1);
    let owner = PlaybackOwner::new(
        Arc::new(catalog.clone()),
        settings,
        runtime,
        visualizer,
        visualizer_drain,
        waveform,
        lyrics,
        Vec::new,
        scrobbler,
        source,
        // Like the GStreamer backend, a start spawns on the runtime of the calling thread.
        move |_| {
            tokio::runtime::Handle::try_current().map_err(|error| error.to_string())?;
            Ok(Box::new(IdleBackend(backend_commands.clone())))
        },
    );
    let (playback, inbox) = actor::channel();
    (
        directory, catalog, sequence, owner, commands, playback, inbox,
    )
}
async fn finish_queue(
    session: &mut Player,
    inbox: &std::sync::mpsc::Receiver<Message>,
    sample: &ClockSample,
) {
    session.publish(sample);
    while session.pending_queue.is_some() || session.pending_hydration.is_some() {
        let message =
            tokio::task::block_in_place(|| inbox.recv_timeout(std::time::Duration::from_secs(5)))
                .unwrap();
        message.apply(session, sample);
        session.publish(sample);
    }
    let (sent, received) = std::sync::mpsc::sync_channel(1);
    session
        .owner()
        .store_sender
        .send(PlaybackStoreWork::Flush(sent))
        .await
        .unwrap();
    tokio::task::block_in_place(|| received.recv_timeout(std::time::Duration::from_secs(5)))
        .unwrap();
}

fn restored_item() -> TrackReference {
    let mut item = TrackReference::direct(
        "https://example.test/track-1",
        "Track",
        "Artist",
        "Album",
        180_000,
    );
    item.metadata.disc_number = 1;
    item.metadata.track_number = 1;
    item
}

fn batch_item(track: i64, provenance: QueueProvenance) -> (TrackReference, QueueProvenance) {
    let mut item = restored_item();
    item.media_uri = format!("https://example.test/track-{track}");
    (item, provenance)
}

async fn active_two_track_session() -> (
    tempfile::TempDir,
    library::Catalog,
    Player,
    Arc<PlaybackOwner>,
    Arc<Mutex<Vec<BackendCommand>>>,
    Playback,
    std::sync::mpsc::Receiver<Message>,
) {
    let (_directory, _catalog, sequence, owner, commands, playback, inbox) = seeded(vec![
        batch_item(1, QueueProvenance::Manual),
        batch_item(2, QueueProvenance::Manual),
    ])
    .await;
    let mut session = Player::new(
        sequence,
        "clear-queue-law",
        PlaybackSettings::default(),
        PlaybackOutput::Local,
        false,
        3,
        Some(Box::new(IdleBackend(commands.clone()))),
        Arc::downgrade(&owner),
        playback.downgrade(),
    );
    session.set_playing(true);
    assert!(session.current_run().is_some());
    (
        _directory, _catalog, session, owner, commands, playback, inbox,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loaded_advancement_reuses_positions_without_a_store_read() {
    let (_directory, _catalog, mut session, _owner, _commands, _playback, inbox) =
        active_two_track_session().await;
    let sample = ClockSample {
        monotonic_millis: 1_000,
        unix_seconds: 1,
        local_period: "1970-01".into(),
    };
    session
        .handle_command(
            Command::ApplyQueue(crate::playback::PlayRequest::from_input(
                library::QueueLoad::items(
                    (0..240)
                        .map(|index| batch_item(index, QueueProvenance::Manual))
                        .collect(),
                ),
                QueuePlacement::Now,
            )),
            &sample,
        )
        .unwrap();
    assert!(session.view().queue_loading);
    while session.sequence.total() != 240 {
        inbox
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .apply(&mut session, &sample);
    }
    assert!(session.view().queue_loading);
    assert_eq!(session.view().queue_window.len(), 1);
    finish_queue(&mut session, &inbox, &sample).await;
    assert!(!session.view().queue_loading);
    assert!(session.current_run().is_some());
    assert_eq!(
        session.view().queue_window.len(),
        library::QUEUE_CONTEXT_LIMIT
    );
    let entries = session.sequence.entries().to_vec();
    for (command, index) in [(Command::Next, 1), (Command::Previous, 0)] {
        session.handle_command(command, &sample).unwrap();
        assert!(session.pending_queue.is_none());
        assert_eq!(
            session.sequence.selected().unwrap().track.media_uri,
            entries[index].track.media_uri,
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_queue_replacement_keeps_the_existing_run_and_can_be_retried() {
    let (_directory, _catalog, mut session, owner, _commands, _playback, inbox) =
        active_two_track_session().await;
    let sample = ClockSample {
        monotonic_millis: 1_000,
        unix_seconds: 1,
        local_period: "1970-01".into(),
    };
    let run = session.current_run();
    let before = session.view().queue_window;
    session
        .handle_command(
            Command::ApplyQueue(crate::playback::PlayRequest::from_input(
                library::QueueLoad::items(vec![batch_item(10, QueueProvenance::Manual)]),
                QueuePlacement::Now,
            )),
            &sample,
        )
        .unwrap();
    let id = session.pending_queue.as_ref().unwrap().0;
    assert!(session.view().queue_loading);
    let mut updates = owner.updates.subscribe();
    session
        .complete_queue(id, Err("write failed".into()), &sample)
        .unwrap();
    session.publish(&sample);
    assert_eq!(
        updates.recv().await.unwrap().unwrap().notices,
        [PlaybackNotice::OperationFailed("write failed".into())]
    );
    assert_eq!(session.current_run(), run);
    assert_eq!(session.view().queue_window, before);
    assert!(!session.view().queue_loading);
    session
        .handle_command(
            Command::ApplyQueue(crate::playback::PlayRequest::from_input(
                library::QueueLoad::items(vec![batch_item(20, QueueProvenance::Manual)]),
                QueuePlacement::Now,
            )),
            &sample,
        )
        .unwrap();
    finish_queue(&mut session, &inbox, &sample).await;
    assert_eq!(
        session.sequence.selected().unwrap().track.media_uri,
        "https://example.test/track-20"
    );
    assert_ne!(session.current_run(), run);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refill_preserves_live_progress_and_prepared_positions() {
    let (_directory, _catalog, sequence, owner, commands, playback, inbox) = seeded(
        (0..240)
            .map(|index| batch_item(index, QueueProvenance::Manual))
            .collect(),
    )
    .await;
    let mut session = Player::new(
        sequence,
        "refill",
        PlaybackSettings::default(),
        PlaybackOutput::Local,
        false,
        3,
        Some(Box::new(IdleBackend(commands.clone()))),
        Arc::downgrade(&owner),
        playback.downgrade(),
    );
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".into(),
    };
    session.set_playing(true);
    for _ in 0..20 {
        session.handle_command(Command::Next, &sample).unwrap();
    }
    session.hydrate_queue();
    assert!(
        session.pending_hydration.is_some(),
        "replenish after retained history grows"
    );
    assert!(!session.view().queue_loading);
    session.handle_command(Command::Next, &sample).unwrap();
    session.sequence.set_progress_millis(12_000);
    let current = session.sequence.selected().unwrap().clone();
    let next = session.sequence.peek_next_eos().unwrap().clone();
    let run = session.current_run();
    let next_run = session.next_plan.as_ref().map(|plan| plan.next_run);
    commands.lock().unwrap().clear();
    finish_queue(&mut session, &inbox, &sample).await;
    assert!(!session.view().queue_loading);
    assert_eq!(session.current_run(), run);
    assert_eq!(
        session.next_plan.as_ref().map(|plan| plan.next_run),
        next_run
    );
    assert_eq!(session.sequence.progress_millis(), 12_000);
    assert!(Arc::ptr_eq(session.sequence.selected().unwrap(), &current));
    assert!(Arc::ptr_eq(
        session.sequence.peek_next_eos().unwrap(),
        &next
    ));
    assert!(
        !commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| matches!(command, BackendCommand::Stop { .. }))
    );
    assert_eq!(
        session.sequence.entries().len(),
        library::QUEUE_CONTEXT_LIMIT
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn newer_captured_play_supersedes_pending_completion_and_stop_is_preserved() {
    let (_directory, catalog, mut session, _owner, _commands, _playback, inbox) =
        active_two_track_session().await;
    let sample = ClockSample {
        monotonic_millis: 1_000,
        unix_seconds: 1,
        local_period: "1970-01".into(),
    };
    let mut first = None;
    for identity in ["first", "latest"] {
        let input = library::QueueLoad::items(vec![(
            TrackReference::direct(
                format!("https://example.test/{identity}"),
                identity,
                "",
                "",
                120_000,
            ),
            QueueProvenance::Manual,
        )]);
        session
            .handle_command(
                Command::ApplyQueue(crate::playback::PlayRequest::from_input(
                    input.clone(),
                    QueuePlacement::Now,
                )),
                &sample,
            )
            .unwrap();
        if identity == "first" {
            first = Some((
                session.pending_queue.as_ref().unwrap().0,
                crate::playback::PlayRequest::from_input(input, QueuePlacement::Now)
                    .prepare(&catalog, false, 0)
                    .await
                    .unwrap(),
            ));
        }
    }
    let run = session.current_run();
    let (id, page) = first.unwrap();
    assert!(!session.complete_queue(id, Ok(page), &sample).unwrap());
    assert_eq!(session.current_run(), run);
    session.handle_command(Command::Stop, &sample).unwrap();
    finish_queue(&mut session, &inbox, &sample).await;
    assert_eq!(session.current_run(), None);
    assert_eq!(
        session.sequence.selected().unwrap().track.metadata.title,
        "latest"
    );

    session
        .handle_command(
            Command::ApplyQueue(crate::playback::PlayRequest::from_input(
                library::QueueLoad::items(
                    (0..240)
                        .map(|index| batch_item(index, QueueProvenance::Manual))
                        .collect(),
                ),
                QueuePlacement::Now,
            )),
            &sample,
        )
        .unwrap();
    finish_queue(&mut session, &inbox, &sample).await;
    session
        .handle_command(Command::Activate(239), &sample)
        .unwrap();
    session.publish(&sample);
    assert!(session.pending_hydration.is_some());
    session.handle_command(Command::Stop, &sample).unwrap();
    finish_queue(&mut session, &inbox, &sample).await;
    assert_eq!(session.current_run(), None);
    assert_eq!(session.sequence.selected_index(), Some(239));

    session
        .handle_command(Command::Activate(0), &sample)
        .unwrap();
    session.publish(&sample);
    assert!(session.pending_hydration.is_some());
    let old_id = session.sequence.id_at(0).unwrap();
    let mut old_rows = catalog
        .tracks_by_uris(
            &["https://example.test/track-0".into()],
            &library::ReadCancellation::new(),
        )
        .await
        .unwrap();
    let old_item = old_rows.pop().unwrap();
    session
        .handle_command(
            Command::ApplyQueue(crate::playback::PlayRequest::from_input(
                library::QueueLoad::items(vec![batch_item(999, QueueProvenance::Manual)]),
                QueuePlacement::Now,
            )),
            &sample,
        )
        .unwrap();
    session.complete_queue_metadata(Ok(vec![(old_id, old_item)]), &sample);
    assert_eq!(
        session.current_run(),
        None,
        "late metadata must not start the superseded selection"
    );
    finish_queue(&mut session, &inbox, &sample).await;
    assert_eq!(
        session.sequence.selected().unwrap().track.media_uri,
        "https://example.test/track-999"
    );
    assert!(session.current_run().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn library_changes_preserve_run_progress_prepared_next_and_positions() {
    let (_directory, _catalog, mut session, owner, commands, _playback, _inbox) =
        active_two_track_session().await;
    let sample = ClockSample {
        monotonic_millis: 1_000,
        unix_seconds: 1,
        local_period: "1970-01".into(),
    };
    let before = session.view();
    let run = session.current_run();
    let next_run = session.next_plan.as_ref().map(|plan| plan.next_run);
    session.library_changed(None, &sample);
    let rows = session
        .sequence
        .read_request()
        .unwrap()
        .into_iter()
        .map(|(id, entry)| {
            let mut song = before
                .queue_window
                .iter()
                .find(|row| row.track.track_key == entry.track_key)
                .unwrap()
                .track
                .clone();
            song.artwork_binding = Some(vec![1, 2, 3]);
            song.state.favorite = true;
            (id, song)
        })
        .collect::<Vec<_>>();
    commands.lock().unwrap().clear();
    session.complete_queue_metadata(Ok(rows.clone()), &sample);
    session.publish(&sample);
    let after = owner.updates.current().unwrap();
    assert_eq!(after.queue.revision, before.queue.revision + 1);
    assert_eq!(after.queue.total, before.queue.total);
    assert_eq!(after.queue.current_index, before.queue.current_index);
    assert_eq!(after.queue.next_index, before.queue.next_index);
    assert_eq!(
        after.transport.position_millis,
        before.transport.position_millis
    );
    assert_eq!(
        after.transport.current.as_ref().unwrap().id,
        before.transport.current.as_ref().unwrap().id
    );
    assert_eq!(session.current_run(), run);
    assert_eq!(
        session.next_plan.as_ref().map(|plan| plan.next_run),
        next_run
    );
    assert_eq!(
        after
            .transport
            .current
            .as_ref()
            .unwrap()
            .row
            .track
            .artwork_binding,
        Some(vec![1, 2, 3])
    );
    assert!(
        after
            .transport
            .current
            .as_ref()
            .unwrap()
            .row
            .track
            .state
            .favorite
    );
    assert!(
        commands.lock().unwrap().is_empty(),
        "presentation does not restart or reprepare audio"
    );
    session.complete_queue_metadata(Ok(rows), &sample);
    assert!(!session.dirty);
    session.publish(&sample);
    assert!(Arc::ptr_eq(&after, &owner.updates.current().unwrap()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clearing_queue_without_current_keeps_the_active_run() {
    let (_directory, _catalog, mut session, _owner, commands, _playback, inbox) =
        active_two_track_session().await;
    let run = session.current_run().unwrap();
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".into(),
    };
    commands.lock().unwrap().clear();
    session
        .handle_command(
            Command::Clear {
                include_current: false,
            },
            &sample,
        )
        .unwrap();
    finish_queue(&mut session, &inbox, &sample).await;
    assert_eq!(session.sequence.entries().len(), 1);
    assert_eq!(session.current_run(), Some(run));
    assert!(session.view().transport.current.is_some());
    assert!(
        !commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| matches!(command, BackendCommand::Stop { .. }))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn volume_commit_persists_the_requested_value_after_older_backend_feedback() {
    let (_directory, _catalog, mut session, owner, _commands, _playback, inbox) =
        active_two_track_session().await;
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".into(),
    };
    session
        .handle_command(Command::SetVolume(0.6), &sample)
        .unwrap();
    session
        .handle_command(Command::SetVolume(0.3), &sample)
        .unwrap();
    session.handle_backend(
        BackendEvent::AudioApplied {
            volume: 0.6,
            muted: false,
            output: None,
        },
        &sample,
    );
    session
        .handle_command(Command::PersistVolume(0.3), &sample)
        .unwrap();
    finish_queue(&mut session, &inbox, &sample).await;
    assert_eq!(session.view().controls.volume, 0.3);
    assert_eq!(owner.settings.load().ui.playback.volume, 0.3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_access_and_visualizer_settings_leave_the_audible_run_untouched() {
    let (_directory, _catalog, mut session, _owner, commands, _playback, _inbox) =
        active_two_track_session().await;
    let sample = ClockSample {
        monotonic_millis: 10_000,
        unix_seconds: 10,
        local_period: "1970-01".into(),
    };
    let run = session.current_run().unwrap();
    session.stream_resolved(
        run,
        audio_processing::ResolvedStream::new("https://example.test/track.flac"),
    );
    session.handle_backend(BackendEvent::Started { run }, &sample);
    let position = session.sequence.selected().unwrap().position;
    session.sequence.set_progress_millis(42_000);
    for command in [
        Command::StreamInputsChanged,
        Command::UpdateSettings(session.settings.clone()),
    ] {
        commands.lock().unwrap().clear();
        session.handle_command(command, &sample).unwrap();
        assert!(
            commands
                .lock()
                .unwrap()
                .iter()
                .all(|command| matches!(command, BackendCommand::PrepareNext { .. })),
            "background access changes may only replace prepared-next"
        );
        assert_eq!(session.current_run(), Some(run));
        assert_eq!(session.sequence.selected().unwrap().position, position);
        assert_eq!(session.sequence.progress_millis(), 42_000);
    }
    for (ignore_volume, fft_size) in [(false, 8_192), (true, 16_384), (true, 32_768)] {
        commands.lock().unwrap().clear();
        let mut settings = session.settings.clone();
        settings.visualizer_ignore_volume = ignore_volume;
        settings.visualizer_fft_size = fft_size;
        session
            .handle_command(Command::UpdateSettings(settings), &sample)
            .unwrap();
        let commands = commands.lock().unwrap();
        assert!(commands.iter().any(|command| matches!(command,
            BackendCommand::ConfigureAudio(settings) if settings.visualizer_ignore_volume == ignore_volume && settings.visualizer_fft_size == fft_size)));
        assert!(commands.iter().all(|command| matches!(
            command,
            BackendCommand::ConfigureAudio(_) | BackendCommand::PrepareNext { .. }
        )));
        assert_eq!(session.current_run(), Some(run));
        assert_eq!(session.sequence.selected().unwrap().position, position);
        assert_eq!(session.sequence.progress_millis(), 42_000);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clearing_queue_with_current_stops_the_run_and_empties_the_session() {
    let (_directory, catalog, mut session, _owner, commands, _playback, inbox) =
        active_two_track_session().await;
    let run = session.current_run().unwrap();
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".into(),
    };
    session
        .handle_command(
            Command::SetAutoDj {
                enabled: true,
                refill_threshold: 3,
            },
            &sample,
        )
        .unwrap();
    session.publish(&sample);
    let request = session.auto_dj_in_flight.clone().unwrap();
    commands.lock().unwrap().clear();
    session
        .handle_command(
            Command::Clear {
                include_current: true,
            },
            &sample,
        )
        .unwrap();
    finish_queue(&mut session, &inbox, &sample).await;
    assert!(session.sequence.entries().is_empty());
    assert_eq!(session.current_run(), None);
    assert!(session.sequence.selected().is_none());
    assert_eq!(session.status(), TransportStatus::Stopped);
    assert!(session.view().transport.current.is_none());
    assert!(!session.view().queue_loading);
    assert_eq!(catalog.restore_queue().await.unwrap().rows.len(), 0);
    assert!(commands.lock().unwrap().iter().any(
        |command| matches!(command, BackendCommand::Stop { run: stopped } if *stopped == run)
    ));
    session
        .handle_command(
            Command::ApplyQueue(crate::playback::PlayRequest::from_input(
                library::QueueLoad::items(vec![batch_item(10, QueueProvenance::Manual)]),
                QueuePlacement::End,
            )),
            &sample,
        )
        .unwrap();
    finish_queue(&mut session, &inbox, &sample).await;
    assert!(
        !session
            .complete_auto_dj_candidates(
                request.seed_run,
                &request.seed_media_uri,
                vec![session.sequence.entries()[0].track.track_key],
                5,
                &sample,
            )
            .unwrap()
    );
    finish_queue(&mut session, &inbox, &sample).await;
    assert_eq!(catalog.restore_queue().await.unwrap().rows.len(), 1);
    assert_eq!(
        session.sequence.selected().unwrap().track.media_uri,
        "https://example.test/track-10"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restored_media_stays_paused_until_first_play_starts_one_resolution() {
    let (_directory, _catalog, mut sequence, owner, commands, playback, _inbox) =
        seeded(vec![batch_item(1, QueueProvenance::Manual)]).await;
    sequence.set_progress_millis(42_000);
    let mut session = Player::new(
        sequence,
        "restore-law",
        PlaybackSettings::default(),
        PlaybackOutput::Local,
        false,
        3,
        Some(Box::new(IdleBackend(commands.clone()))),
        Arc::downgrade(&owner),
        playback.downgrade(),
    );
    assert_eq!(session.current_run(), None);
    assert_eq!(session.status(), TransportStatus::Paused);
    assert_eq!(session.position_millis(), 42_000);
    assert!(session.view().transport.current.is_some());
    assert!(session.stream_tasks.is_empty());
    assert!(commands.lock().unwrap().is_empty());
    session
        .handle_command(
            Command::Play,
            &ClockSample {
                monotonic_millis: 0,
                unix_seconds: 0,
                local_period: "1970-01".into(),
            },
        )
        .unwrap();
    assert_eq!(session.stream_tasks.len(), 1);
    assert!(session.current_run().is_some());
    assert_eq!(session.position_millis(), 42_000);
    assert_eq!(
        session.view().transport.effective_state(),
        TransportStatus::Resolving
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playback_modes_publish_their_state_without_an_active_run() {
    let (_directory, _catalog, sequence, owner, commands, playback, inbox) =
        seeded(Vec::new()).await;
    let mut session = Player::new(
        sequence,
        "mode-law",
        PlaybackSettings::default(),
        PlaybackOutput::Local,
        false,
        3,
        Some(Box::new(IdleBackend(commands.clone()))),
        Arc::downgrade(&owner),
        playback.downgrade(),
    );
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".into(),
    };
    session
        .handle_command(
            Command::SetShuffle {
                enabled: true,
                seed: 7,
            },
            &sample,
        )
        .unwrap();
    finish_queue(&mut session, &inbox, &sample).await;
    assert!(owner.updates.current().unwrap().controls.shuffle_enabled);
    session
        .handle_command(Command::SetRepeat(QueueRepeatMode::One), &sample)
        .unwrap();
    session.publish(&sample);
    assert_eq!(
        owner.updates.current().unwrap().controls.repeat_mode,
        QueueRepeatMode::One
    );
    session
        .handle_command(
            Command::SetAutoDj {
                enabled: true,
                refill_threshold: 3,
            },
            &sample,
        )
        .unwrap();
    session.publish(&sample);
    assert!(owner.updates.current().unwrap().controls.auto_dj_enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shuffled_batches_keep_placement_and_global_shuffle() {
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".into(),
    };
    for global_shuffle in [false, true] {
        for placement in [
            QueuePlacement::Now,
            QueuePlacement::Next,
            QueuePlacement::Last,
        ] {
            for count in [1, 12] {
                let existing = (1..=4)
                    .map(|id| batch_item(id, QueueProvenance::Manual))
                    .collect();
                let (_directory, catalog, sequence, owner, commands, playback, inbox) =
                    seeded(existing).await;
                let mut session = Player::new(
                    sequence,
                    "shuffled-batch",
                    PlaybackSettings::default(),
                    PlaybackOutput::Local,
                    false,
                    3,
                    Some(Box::new(IdleBackend(commands.clone()))),
                    Arc::downgrade(&owner),
                    playback.downgrade(),
                );
                session
                    .handle_command(
                        Command::SetShuffle {
                            enabled: global_shuffle,
                            seed: 7,
                        },
                        &sample,
                    )
                    .unwrap();
                finish_queue(&mut session, &inbox, &sample).await;
                let before = session.sequence().snapshot();
                // Keep duplicate tracks as distinct positions.
                let items = (0..count)
                    .map(|rank| {
                        batch_item(
                            100 + rank / 2,
                            QueueProvenance::Context {
                                context_title: None,
                                context_id: "selected-collection".into(),
                                source_rank: rank as usize,
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                let request = crate::playback::PlayRequest::from_input(
                    library::QueueLoad::items(items),
                    placement,
                )
                .with_anchor_index(count as usize - 1)
                .shuffled(true);
                assert!(request.activation_context(global_shuffle).is_none());
                session.apply_batch(request, &sample).unwrap();
                finish_queue(&mut session, &inbox, &sample).await;
                let after = session.sequence().snapshot();
                assert_eq!(after.shuffled, global_shuffle);
                let incoming = after
                    .entries
                    .iter()
                    .filter_map(|entry| match &entry.provenance {
                        QueueProvenance::Context { source_rank, .. } => Some(*source_rank),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let expected = (0..count as usize).collect::<Vec<_>>();
                let mut sorted = incoming.clone();
                sorted.sort_unstable();
                assert_eq!(sorted, expected);
                if count > 1 {
                    assert_ne!(incoming, expected);
                }
                let ordered = |state: &library::QueueRestore| {
                    state
                        .order
                        .iter()
                        .map(|index| state.entries[*index as usize].clone())
                        .collect::<Vec<_>>()
                };
                let original = ordered(&before);
                let result = ordered(&after);
                if placement == QueuePlacement::Now {
                    assert_eq!(result.len(), count as usize);
                    assert_eq!(after.current_index, Some(0));
                } else {
                    assert_eq!(after.current_index, before.current_index);
                    let at = if placement == QueuePlacement::Next {
                        1
                    } else {
                        original.len()
                    };
                    assert_eq!(&result[..at], &original[..at]);
                    assert_eq!(&result[at + count as usize..], &original[at..]);
                }
                let restored = catalog.restore_queue().await.unwrap();
                assert_eq!(restored.shuffled, global_shuffle);
                assert_eq!(ordered(&restored), result);
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collection_starts_and_selected_rows_keep_distinct_shuffle_anchors() {
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".to_string(),
    };
    for shuffled in [false, true] {
        for (collection_start, anchor) in [(true, 0), (false, 0), (false, 1)] {
            let items = (0..4)
                .map(|rank| {
                    batch_item(
                        rank as i64 + 1,
                        QueueProvenance::Context {
                            context_title: None,
                            context_id: "collection".into(),
                            source_rank: rank,
                        },
                    )
                })
                .collect::<Vec<_>>();
            let (_directory, catalog, mut sequence, owner, commands, playback, inbox) =
                seeded(items.clone()).await;
            let input = library::MemberSelection::Keys {
                source: None,
                keys: sequence
                    .snapshot()
                    .entries
                    .iter()
                    .map(|entry| entry.track_key)
                    .collect(),
            };
            sequence.set_progress_millis(42_000);
            let mut session = Player::new(
                sequence,
                "collection-start-law",
                PlaybackSettings::default(),
                PlaybackOutput::Local,
                false,
                3,
                Some(Box::new(IdleBackend(commands.clone()))),
                Arc::downgrade(&owner),
                playback.downgrade(),
            );
            session
                .handle_command(
                    Command::SetShuffle {
                        enabled: shuffled,
                        seed: 7,
                    },
                    &sample,
                )
                .unwrap();
            finish_queue(&mut session, &inbox, &sample).await;
            session.handle_command(Command::Play, &sample).unwrap();
            let previous_run = session.current_run();
            assert_eq!(session.sequence().selected().unwrap().position, 0);

            let request = if collection_start {
                crate::playback::PlayRequest::ordered
            } else {
                crate::playback::PlayRequest::captured
            };
            let request = request(
                input,
                anchor,
                QueuePlacement::Now,
                collection_start && shuffled,
            )
            .with_context("collection".into());
            assert_eq!(
                request.activation_context(shuffled).is_none(),
                collection_start
            );
            session.play(request, &sample).unwrap();
            let random_start = collection_start && shuffled;
            finish_queue(&mut session, &inbox, &sample).await;
            assert_eq!(session.position_millis(), 0);
            assert_ne!(session.current_run(), previous_run);
            let selected = session.sequence().selected().unwrap();
            if !random_start {
                assert_eq!(
                    selected.track.media_uri,
                    format!("https://example.test/track-{}", anchor + 1)
                );
            }
            assert!(matches!(
                selected.provenance,
                QueueProvenance::Context {
                    source_rank: 0..4,
                    ..
                }
            ));
            assert_eq!(session.view().controls.shuffle_enabled, shuffled);
            let restored = catalog.restore_queue().await.unwrap();
            assert_eq!(restored.current_index, Some(selected.position));
            assert_eq!(restored.shuffled, shuffled);
            assert_eq!(
                session.current_run.as_ref().unwrap().row.position,
                selected.position
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gapless_preparation_resolves_exactly_the_next_position() {
    use secrets::SecretStore;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (reports, mut received) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                if socket.read_exact(&mut byte).await.is_err() {
                    break;
                }
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).unwrap();
            if let Some(path) = request.split_whitespace().nth(1) {
                let url = url::Url::parse(&format!("http://localhost{path}")).unwrap();
                if url.path() == "/:/timeline" {
                    reports
                        .send(url.query_pairs().into_owned().collect::<HashMap<_, _>>())
                        .unwrap();
                }
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 17\r\nConnection: close\r\n\r\n<MediaContainer/>").await.unwrap();
        }
    });
    let source_id = sources::SourceId::new("gapless");
    let items = (1..=2)
        .map(|number| {
            let mut item = batch_item(number, QueueProvenance::Manual);
            item.0.media_uri =
                library::source_entity_uri(&source_id, "track", &format!("plex:track:{number}"));
            item
        })
        .collect();
    let (_directory, _catalog, sequence, owner, commands, playback, _inbox) = seeded(items).await;
    let credential = crate::settings::Credential::new("gapless");
    owner.settings.update(|stored| {
        stored.sources.configured = vec![crate::settings::ConfiguredSource {
            configuration: sources::SourceConfiguration {
                source_id: source_id.clone(), kind: "plex".into(), name: "Gapless".into(),
                provider_payload: serde_json::json!({"version":1,"server_id":"server","profile_id":"user","base_url":endpoint,"address_override":null,"local":true,"relay":false,"owned":true,"trust_invalid_cert":false}).to_string(),
            },
            credential: Some(credential.clone()), music_folder_id: None, local_access: None, enable_half_stars: true,
        }];
        Ok(())
    }).unwrap();
    owner.source.shared.secrets.save_secret(&crate::settings::provider_secret_key(&credential),
        &serde_json::json!({"client_id":"gapless","token":"fixture","account_id":"user","profiles":{},"resources":{"user":{"server":"fixture"}},"home_admin_subscription":false,"download_subscriptions":{}}).to_string()).unwrap();
    let mut session = Player::new(
        sequence,
        "gapless-law",
        PlaybackSettings::default(),
        PlaybackOutput::Local,
        false,
        3,
        Some(Box::new(IdleBackend(commands.clone()))),
        Arc::downgrade(&owner),
        playback.downgrade(),
    );
    session.set_playing(true);
    let current_run = session.current_run().unwrap();
    let current_identifier = session.current_run.as_ref().unwrap().play_id.clone();
    let plan = session.next_plan.as_ref().unwrap();
    let next_run = plan.next_run;
    let next_identifier = session.play_id(next_run);
    assert_eq!(Some(plan.occurrence), session.sequence.id_at(1));
    assert_eq!(
        plan.request.media_uri,
        library::source_entity_uri(&source_id, "track", "plex:track:2")
    );
    assert_ne!(current_identifier, next_identifier);
    for (_, task) in session.stream_tasks.drain() {
        task.abort();
        let _ = task.await;
    }
    session.stream_resolved(
        current_run,
        audio_processing::ResolvedStream::new("file:///current.flac"),
    );
    session.stream_resolved(
        next_run,
        audio_processing::ResolvedStream::new("https://music.example/next.m3u8")
            .with_transcoding(true),
    );
    assert!(commands.lock().unwrap().iter().any(|command| matches!(command,
        BackendCommand::PrepareNext { next: Some(prepared), .. }
        if prepared.run == next_run && prepared.transition == NextTransition::Gapless
            && prepared.stream.transcoded() && prepared.stream.row.as_ref().unwrap().position == 1)));
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 1,
        local_period: "1970-01".into(),
    };
    session.handle_backend(BackendEvent::Started { run: current_run }, &sample);
    session.publish(&sample);
    session.handle_backend(
        BackendEvent::Position {
            run: current_run,
            millis: 1_000,
        },
        &sample,
    );
    session.publish(&sample);
    session.handle_command(Command::Pause, &sample).unwrap();
    session.publish(&sample);
    session.handle_backend(
        BackendEvent::State {
            run: current_run,
            state: BackendState::Paused,
        },
        &sample,
    );
    session.publish(&sample);
    session
        .handle_command(Command::Seek(7_000), &sample)
        .unwrap();
    session.publish(&sample);
    session.handle_backend(
        BackendEvent::Position {
            run: current_run,
            millis: 7_000,
        },
        &sample,
    );
    session.publish(&sample);
    session.handle_command(Command::Play, &sample).unwrap();
    session.handle_backend(
        BackendEvent::State {
            run: current_run,
            state: BackendState::Playing,
        },
        &sample,
    );
    session.publish(&sample);
    session
        .handle_command(Command::SetVolume(0.25), &sample)
        .unwrap();
    session.publish(&sample);
    session
        .handle_command(Command::SetMuted(true), &sample)
        .unwrap();
    session.publish(&sample);

    session.handle_backend(
        BackendEvent::Position {
            run: current_run,
            millis: 179_500,
        },
        &sample,
    );
    session.handle_backend(
        BackendEvent::Transitioned {
            old_run: current_run,
            new_run: next_run,
        },
        &sample,
    );
    assert_eq!(session.current_run(), Some(next_run));
    assert!(
        session
            .current_run
            .as_ref()
            .unwrap()
            .resolved_stream
            .as_ref()
            .unwrap()
            .transcoded()
    );
    let mut observed = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            observed.push(received.recv().await.unwrap());
            let current_started = observed.iter().any(|report| {
                report["X-Plex-Session-Identifier"] == current_identifier
                    && report["state"] == "playing"
                    && report["time"] == "0"
            });
            let current_ended = observed.iter().any(|report| {
                report["X-Plex-Session-Identifier"] == current_identifier
                    && report["state"] == "stopped"
                    && report["time"] == "179500"
            });
            let next_started = observed.iter().any(|report| {
                report["X-Plex-Session-Identifier"] == next_identifier
                    && report["state"] == "playing"
                    && report["time"] == "0"
            });
            if current_started && current_ended && next_started {
                break;
            }
        }
    })
    .await
    .unwrap_or_else(|error| panic!("Plex timelines {observed:?}: {error}"));
    assert!(
        observed
            .iter()
            .any(|report| report["state"] == "paused" && report["time"] == "1000")
    );
    assert!(
        observed
            .iter()
            .any(|report| report["state"] == "paused" && report["time"] == "7000")
    );
    assert!(
        observed
            .iter()
            .any(|report| report["state"] == "playing" && report["time"] == "7000")
    );
    assert!(observed.iter().any(|report| report["volume"] == "25"));
    assert!(observed.iter().any(|report| report["volume"] == "0"));
    let ended = observed
        .iter()
        .position(|report| {
            report["X-Plex-Session-Identifier"] == current_identifier
                && report["state"] == "stopped"
        })
        .unwrap();
    let started = observed
        .iter()
        .position(|report| report["X-Plex-Session-Identifier"] == next_identifier)
        .unwrap();
    assert!(ended < started);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiver_window_survives_local_hydration_ticks() {
    let (_directory, _catalog, sequence, owner, commands, playback, _inbox) = seeded(
        (0..30)
            .map(|id| batch_item(id, QueueProvenance::Manual))
            .collect(),
    )
    .await;
    let mut queue = sequence.snapshot();
    queue.current_index = Some(20);
    let mut session = Player::new(
        sequence,
        "receiver",
        PlaybackSettings::default(),
        PlaybackOutput::Local,
        false,
        3,
        Some(Box::new(IdleBackend(commands.clone()))),
        Arc::downgrade(&owner),
        playback.downgrade(),
    );
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 1,
        local_period: "1970-01".into(),
    };
    session
        .observe_external(
            PlaybackOutput::Remote(playback::RemoteOutput {
                id: "plexamp:test".into(),
                name: "Plexamp".into(),
                protocol: playback::RemoteOutputProtocol::PlexCompanion,
            }),
            ExternalPlayback {
                queue: Some(queue),
                queue_total: 1000,
                queue_offset: 600,
                status: TransportStatus::Playing,
                position_millis: Some(42000),
                duration_millis: 180000,
                volume: 1.0,
                muted: false,
                repeat: QueueRepeatMode::Off,
                shuffle: false,
            },
            &sample,
        )
        .unwrap();
    let before = session.view();
    assert_eq!(before.queue_window.len(), 30);
    session.hydrate_queue();
    assert!(session.pending_queue.is_none());
    let after = session.view();
    assert_eq!(after.queue_window, before.queue_window);
    assert_eq!(after.queue.current_index, Some(620));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiver_observation_keeps_global_queue_facts_without_commands_or_duplicate_reports() {
    let (_directory, catalog, sequence, owner, commands, playback, inbox) = seeded(vec![
        batch_item(1, QueueProvenance::Manual),
        batch_item(1, QueueProvenance::Manual),
    ])
    .await;
    let mut queue = sequence.snapshot();
    let mut session = Player::new(
        sequence,
        "receiver",
        PlaybackSettings::default(),
        PlaybackOutput::Local,
        false,
        3,
        Some(Box::new(IdleBackend(commands.clone()))),
        Arc::downgrade(&owner),
        playback.downgrade(),
    );
    let report = session
        .handoff_snapshot()
        .1
        .expect("restored paused position can select a PMS queue item");
    assert_eq!(report.phase, SourceReportPhase::Progress);
    assert!(report.paused);
    let mut sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 1,
        local_period: "1970-01".into(),
    };
    queue.current_index = Some(1);
    let selected = queue.current_index.unwrap();
    let output = PlaybackOutput::Remote(playback::RemoteOutput {
        id: "plexamp:test".into(),
        name: "Plexamp".into(),
        protocol: playback::RemoteOutputProtocol::PlexCompanion,
    });
    let observation = ExternalPlayback {
        queue: Some(queue.clone()),
        queue_total: 1000,
        queue_offset: 600,
        status: TransportStatus::Paused,
        position_millis: Some(42000),
        duration_millis: 180000,
        volume: 0.4,
        muted: false,
        repeat: QueueRepeatMode::All,
        shuffle: true,
    };
    session
        .observe_external(output.clone(), observation.clone(), &sample)
        .unwrap();
    session.publish(&sample);
    assert!(commands.lock().unwrap().is_empty());
    let view = owner.updates.current().unwrap();
    assert_eq!(view.queue.total, 1000);
    assert_eq!(view.queue.current_index, Some(601));
    assert_eq!(
        view.transport.current.as_ref().unwrap().row.position,
        selected + 600
    );
    assert_eq!(view.transport.position_millis, 42000);
    assert_eq!(view.transport.state, TransportStatus::Paused);
    assert!(view.controls.shuffle_enabled);
    session
        .observe_external(
            output,
            ExternalPlayback {
                queue: None,
                ..observation
            },
            &sample,
        )
        .unwrap();
    session.publish(&sample);
    assert!(Arc::ptr_eq(&view, &owner.updates.current().unwrap()));
    assert!(session.handoff_snapshot().1.is_none());
    let mut preview = session.external.clone().unwrap();
    preview.status = TransportStatus::Playing;
    preview.position_millis = None;
    preview.queue = Some(session.sequence.snapshot());
    session
        .observe_external(session.playback_output.clone(), preview, &sample)
        .unwrap();
    assert_eq!(session.view().transport.position_millis, 42000);
    sample.monotonic_millis = 1000;
    session.advance_external_clock(&sample);
    assert_eq!(session.view().transport.position_millis, 43000);
    let mut preview = session.external.clone().unwrap();
    preview.position_millis = None;
    sample.monotonic_millis = 1500;
    session
        .observe_external(session.playback_output.clone(), preview, &sample)
        .unwrap();
    assert_eq!(session.view().transport.position_millis, 43500);
    let mut committed = session.external.clone().unwrap();
    committed.position_millis = Some(90000);
    session
        .observe_external(session.playback_output.clone(), committed, &sample)
        .unwrap();
    assert_eq!(session.view().transport.position_millis, 90000);
    let mut buffering = session.external.clone().unwrap();
    buffering.status = TransportStatus::Buffering;
    buffering.duration_millis = 0;
    for old_position in [0, 7000] {
        buffering.position_millis = Some(old_position);
        session
            .observe_external(session.playback_output.clone(), buffering.clone(), &sample)
            .unwrap();
        assert_eq!(session.view().transport.position_millis, 90000);
    }
    sample.monotonic_millis = 5000;
    session.publish(&sample);
    session.advance_external_clock(&sample);
    assert!(!session.dirty);
    let view = session.view();
    assert_eq!(view.transport.state, TransportStatus::Buffering);
    assert!(view.transport.desired_playing);
    assert_eq!(view.transport.position_millis, 90000);
    assert_eq!(
        view.transport.duration_millis,
        session
            .sequence
            .selected()
            .unwrap()
            .track
            .metadata
            .duration_millis as u64
    );
    assert!(view.transport.can_seek);
    assert_eq!(view.transport.current.unwrap().row.position, selected + 600);
    session.set_auto_dj(true, 3);
    assert!(
        session.auto_dj_in_flight.is_none(),
        "end of a receiver window is not end of its queue"
    );
    let mut near_end = session.external.clone().unwrap();
    near_end.queue_total = 602;
    near_end.queue = Some(session.sequence.snapshot());
    session
        .observe_external(session.playback_output.clone(), near_end, &sample)
        .unwrap();
    let request = session.auto_dj_in_flight.as_ref().unwrap();
    assert!(request.seed_run.is_none());
    assert_eq!(
        request.seed_media_uri,
        session.sequence.selected().unwrap().track.media_uri
    );
    assert!(
        !session
            .complete_auto_dj(
                None,
                &session.sequence.selected().unwrap().track.media_uri.clone(),
                library::QueueLoad::items(vec![batch_item(9, QueueProvenance::AutoDj)]),
                &sample
            )
            .unwrap(),
        "native candidates are inserted through PMS, not into the observed window"
    );
    finish_queue(&mut session, &inbox, &sample).await;
    assert_eq!(
        catalog
            .export_activity_jsonl(Vec::new(), None)
            .await
            .unwrap(),
        0
    );
    assert!(commands.lock().unwrap().is_empty());
    session
        .adopt_local(
            queue,
            audio_processing::ResolvedStream::new("https://example.test/return.flac").into(),
            false,
            None,
            Box::new(IdleBackend(commands.clone())),
            &sample,
        )
        .unwrap();
    assert!(
        !commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| matches!(command, BackendCommand::Start { .. }))
    );
    assert_eq!(session.view().transport.state, TransportStatus::Paused);
    assert!(session.take_queue_persistence().unwrap().replace);
    assert_eq!(
        session.view().transport.current.unwrap().row.position,
        selected
    );
    session
        .adopt_local(
            session.sequence.snapshot(),
            audio_processing::ResolvedStream::new("https://example.test/return.flac").into(),
            true,
            None,
            Box::new(IdleBackend(commands.clone())),
            &sample,
        )
        .unwrap();
    assert!(
        commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| matches!(command, BackendCommand::Start { .. }))
    );
    assert_eq!(session.playback_output, PlaybackOutput::Local);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_dj_refills_after_track_changes_and_delayed_completion() {
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".into(),
    };
    for (repeat, queue_length, request_position, delayed_completion) in [
        (QueueRepeatMode::Off, 1, 0, true),
        (QueueRepeatMode::Off, 4, 3, false),
        (QueueRepeatMode::All, 4, 3, false),
        (QueueRepeatMode::Off, 4, 2, false),
        (QueueRepeatMode::All, 4, 2, false),
    ] {
        let (_directory, catalog, mut sequence, owner, commands, playback, inbox) = seeded(
            (1..=queue_length)
                .map(|track| batch_item(track as i64, QueueProvenance::Manual))
                .collect(),
        )
        .await;
        sequence.set_repeat_mode(repeat);
        let refill_threshold = if delayed_completion {
            3
        } else {
            queue_length - request_position
        };
        let mut session = Player::new(
            sequence,
            "autodj-law",
            PlaybackSettings::default(),
            PlaybackOutput::Local,
            !delayed_completion,
            refill_threshold,
            Some(Box::new(IdleBackend(commands.clone()))),
            Arc::downgrade(&owner),
            playback.downgrade(),
        );
        if delayed_completion {
            session
                .handle_command(
                    Command::SetAutoDj {
                        enabled: true,
                        refill_threshold,
                    },
                    &sample,
                )
                .unwrap();
            session.publish(&sample);
        }
        session.handle_command(Command::Play, &sample).unwrap();
        session.publish(&sample);
        if !delayed_completion {
            assert!(session.auto_dj_in_flight.is_none());
            session
                .handle_command(Command::Activate(queue_length - 2), &sample)
                .unwrap();
            session.publish(&sample);
            if request_position == queue_length - 1 {
                assert!(session.auto_dj_in_flight.is_none());
                session
                    .handle_command(Command::Activate(request_position), &sample)
                    .unwrap();
                session.publish(&sample);
            }
        }
        let request = session
            .auto_dj_in_flight
            .clone()
            .expect("playing near the queue end requests Auto DJ before the track ends");
        assert!(!owner.updates.current().unwrap().queue_loading);
        if request_position < queue_length - 1 {
            session
                .handle_command(Command::Activate(queue_length - 1), &sample)
                .unwrap();
            session.publish(&sample);
        }
        if delayed_completion {
            session.accept_ended(session.current_run().unwrap(), &sample);
            session.publish(&sample);
            assert!(session.current_run().is_none());
        }
        assert!(
            session
                .complete_auto_dj_candidates(
                    request.seed_run,
                    &request.seed_media_uri,
                    catalog
                        .remember_media_uris(&[
                            batch_item(queue_length as i64 + 1, QueueProvenance::AutoDj)
                                .0
                                .media_uri,
                            batch_item(queue_length as i64 + 2, QueueProvenance::AutoDj)
                                .0
                                .media_uri,
                        ])
                        .await
                        .unwrap(),
                    5,
                    &sample
                )
                .unwrap()
        );
        finish_queue(&mut session, &inbox, &sample).await;
        assert_eq!(
            catalog.restore_queue().await.unwrap().entries.len(),
            queue_length + 2
        );
        assert_eq!(
            owner.updates.current().unwrap().queue.total,
            queue_length + 2
        );
        if !delayed_completion {
            assert_eq!(session.sequence.selected_index(), Some(queue_length - 1));
            session.accept_ended(session.current_run().unwrap(), &sample);
            session.publish(&sample);
        }
        assert_eq!(
            session.sequence.selected().unwrap().track.media_uri,
            format!("https://example.test/track-{}", queue_length + 1)
        );
        assert!(session.current_run().is_some());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_auto_dj_keeps_the_ready_queue_visible() {
    let (_directory, _catalog, mut session, owner, _commands, _playback, _inbox) =
        active_two_track_session().await;
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".into(),
    };
    session.publish(&sample);
    session.set_auto_dj(true, 3);
    session.publish(&sample);
    let request = session.auto_dj_in_flight.clone().unwrap();
    assert!(!session.view().queue_loading);
    assert!(session.auto_dj_unavailable(request.seed_run, &request.seed_media_uri, None));
    session.publish(&sample);
    assert!(!owner.updates.current().unwrap().queue_loading);
    assert_eq!(owner.updates.current().unwrap().queue.total, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_stream_preserves_selected_media_and_player_controls() {
    let (_directory, _catalog, sequence, owner, commands, playback, _inbox) =
        seeded(vec![batch_item(1, QueueProvenance::Manual)]).await;
    let mut session = Player::new(
        sequence,
        "failure-law",
        PlaybackSettings::default(),
        PlaybackOutput::Local,
        false,
        3,
        Some(Box::new(IdleBackend(commands.clone()))),
        Arc::downgrade(&owner),
        playback.downgrade(),
    );
    session.set_playing(true);
    let run = session.current_run().unwrap();
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".into(),
    };
    session.stream_failed(run, "missing".into(), &sample);
    session.publish(&sample);
    let view = owner.updates.current().unwrap();
    assert!(view.transport.current.is_some());
    assert_eq!(view.queue.total, 1);
    assert!(view.transport.error.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playback_crosses_window_edges_and_wraps_without_truncating_the_queue() {
    let (_directory, _catalog, sequence, owner, commands, playback, inbox) = seeded(
        (0..500)
            .map(|rank| batch_item(rank, QueueProvenance::Manual))
            .collect(),
    )
    .await;
    let mut session = Player::new(
        sequence,
        "full-traversal",
        PlaybackSettings::default(),
        PlaybackOutput::Local,
        false,
        3,
        Some(Box::new(IdleBackend(commands.clone()))),
        Arc::downgrade(&owner),
        playback.downgrade(),
    );
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".into(),
    };
    session
        .handle_command(Command::SetRepeat(QueueRepeatMode::All), &sample)
        .unwrap();
    session.set_playing(true);
    for rank in 0..500 {
        assert_eq!(
            session.sequence().selected().unwrap().track.media_uri,
            format!("https://example.test/track-{rank}")
        );
        assert_eq!(session.sequence().total(), 500);
        assert!(session.sequence().entries().len() <= library::QUEUE_CONTEXT_LIMIT);
        session.handle_command(Command::Next, &sample).unwrap();
        finish_queue(&mut session, &inbox, &sample).await;
    }
    assert_eq!(
        session.sequence().selected().unwrap().track.media_uri,
        "https://example.test/track-0"
    );
    session.handle_command(Command::Previous, &sample).unwrap();
    finish_queue(&mut session, &inbox, &sample).await;
    assert_eq!(
        session.sequence().selected().unwrap().track.media_uri,
        "https://example.test/track-499"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backend_advancing_during_removal_uses_the_exact_surviving_successor() {
    for removed_rank in [1, 2] {
        let (_directory, _catalog, sequence, owner, commands, playback, inbox) = seeded(
            (0..3)
                .map(|rank| batch_item(rank, QueueProvenance::Manual))
                .collect(),
        )
        .await;
        let mut session = Player::new(
            sequence,
            "removal-transition",
            PlaybackSettings::default(),
            PlaybackOutput::Local,
            false,
            3,
            Some(Box::new(IdleBackend(commands.clone()))),
            Arc::downgrade(&owner),
            playback.downgrade(),
        );
        let sample = ClockSample {
            monotonic_millis: 0,
            unix_seconds: 0,
            local_period: "1970-01".into(),
        };
        session.sequence.activate_index(removed_rank - 1);
        session.set_playing(true);
        let removed = session.sequence.at(removed_rank).unwrap().position.clone();
        session
            .handle_command(Command::Remove(removed), &sample)
            .unwrap();
        let old_run = session.current_run().unwrap();
        session.accept_ended(old_run, &sample);
        finish_queue(&mut session, &inbox, &sample).await;
        if removed_rank == 1 {
            assert_eq!(
                session.sequence.selected().unwrap().track.media_uri,
                "https://example.test/track-2"
            );
        } else {
            assert_eq!(
                session.sequence.selected().unwrap().track.media_uri,
                "https://example.test/track-1"
            );
            assert!(session.current_run().is_none());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_and_stop_during_queue_preparation_apply_to_the_requested_track() {
    for stop in [false, true] {
        let (_directory, _catalog, mut session, _owner, _commands, _playback, inbox) =
            active_two_track_session().await;
        let sample = ClockSample {
            monotonic_millis: 0,
            unix_seconds: 0,
            local_period: "1970-01".into(),
        };
        session
            .apply_batch(
                crate::playback::PlayRequest::from_input(
                    library::QueueLoad::items(vec![batch_item(99, QueueProvenance::Manual)]),
                    QueuePlacement::Now,
                ),
                &sample,
            )
            .unwrap();
        if stop {
            session.stop(&sample, true);
        } else {
            session.set_playing(false);
        }
        finish_queue(&mut session, &inbox, &sample).await;
        assert_eq!(
            session.sequence.selected().unwrap().track.media_uri,
            "https://example.test/track-99"
        );
        assert_eq!(
            session.status(),
            if stop {
                TransportStatus::Stopped
            } else {
                TransportStatus::Paused
            }
        );
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeat_wraps_when_source_exhaustion_is_discovered_after_a_full_page() {
    let (_directory, catalog, mut sequence, owner, commands, playback, inbox) =
        seeded(vec![batch_item(0, QueueProvenance::Manual)]).await;
    let uris = (0..100)
        .map(|i| format!("https://example.test/{i}"))
        .collect::<Vec<_>>();
    let (key, _) = catalog
        .create_playlist(
            "Source",
            &library::MemberSelection::Uris((uris.to_vec()).into()),
        )
        .await
        .unwrap()
        .unwrap();
    let page = crate::playback::PlayRequest::ordered(
        library::MemberRead::new(library::Collection::Playlist(key)).selection(),
        0,
        QueuePlacement::Now,
        false,
    )
    .with_context("source".into())
    .prepare(&catalog, false, 0)
    .await
    .unwrap();
    sequence.add_page(page, QueueReorderTarget::End, true);
    let mut session = Player::new(
        sequence,
        "repeat",
        PlaybackSettings::default(),
        PlaybackOutput::Local,
        false,
        3,
        Some(Box::new(IdleBackend(commands.clone()))),
        Arc::downgrade(&owner),
        playback.downgrade(),
    );
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".into(),
    };
    session.set_repeat(QueueRepeatMode::All);
    session.set_playing(true);
    for _ in 0..100 {
        session.handle_command(Command::Next, &sample).unwrap();
        finish_queue(&mut session, &inbox, &sample).await;
    }
    assert_eq!(
        session.sequence.selected().unwrap().track.media_uri,
        uris[0]
    );
    assert!(session.current_run().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_the_visible_window_continues_with_the_unbuffered_source() {
    let (_directory, _db, sequence, owner, commands, playback, inbox) = seeded(
        (0..101)
            .map(|i| batch_item(i, QueueProvenance::Manual))
            .collect(),
    )
    .await;
    let mut session = Player::new(
        sequence,
        "remove-window",
        PlaybackSettings::default(),
        PlaybackOutput::Local,
        false,
        3,
        Some(Box::new(IdleBackend(commands.clone()))),
        Arc::downgrade(&owner),
        playback.downgrade(),
    );
    let sample = ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".into(),
    };
    session.set_playing(true);
    let ids = session
        .sequence
        .entries()
        .iter()
        .map(|row| row.position.clone())
        .collect::<Vec<_>>();
    session.remove_many(&crate::playback::QueueSelection::Positions(ids), &sample);
    finish_queue(&mut session, &inbox, &sample).await;
    assert_eq!(
        session.sequence.selected().unwrap().track.media_uri,
        "https://example.test/track-100"
    );
    assert!(session.current_run().is_some());
}

fn skip_session_sample() -> ClockSample {
    ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".into(),
    }
}

async fn skip_session(
    settings: PlaybackSettings,
) -> (Player, Arc<Mutex<Vec<BackendCommand>>>, impl Sized) {
    let (directory, catalog, sequence, owner, commands, playback, inbox) = seeded(
        (0..8)
            .map(|track| batch_item(track, QueueProvenance::Manual))
            .collect(),
    )
    .await;
    let mut session = Player::new(
        sequence,
        "skip-law",
        settings,
        PlaybackOutput::Local,
        false,
        3,
        Some(Box::new(IdleBackend(commands.clone()))),
        Arc::downgrade(&owner),
        playback.downgrade(),
    );
    session.set_playing(true);
    (
        session,
        commands,
        (directory, catalog, owner, playback, inbox),
    )
}

fn fail_current(session: &mut Player, kind: FailureKind) {
    let run = session.current_run().unwrap();
    session.handle_backend(
        BackendEvent::Error {
            run,
            error: BackendFailure::with_kind(kind, kind.summary()),
        },
        &skip_session_sample(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unplayable_tracks_are_skipped_until_the_failure_cap() {
    let (mut session, _commands, _guards) = skip_session(PlaybackSettings::default()).await;
    let start = session.sequence.selected_index().unwrap();
    for skipped in 1..=MAX_CONSECUTIVE_SKIPS {
        fail_current(&mut session, FailureKind::Decode);
        assert_eq!(session.sequence.selected_index(), Some(start + skipped));
        assert!(session.current_run.as_ref().unwrap().desired_playing);
        assert!(session.last_error.is_none());
    }
    fail_current(&mut session, FailureKind::UnavailableMedia);
    assert_eq!(
        session.sequence.selected_index(),
        Some(start + MAX_CONSECUTIVE_SKIPS)
    );
    assert!(session.current_run().is_none());
    assert!(session.last_error.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_failures_disabled_skipping_and_repeat_one_stop_playback() {
    for (kind, skip, repeat) in [
        (FailureKind::Output, true, QueueRepeatMode::Off),
        (FailureKind::DeniedAccess, true, QueueRepeatMode::Off),
        (FailureKind::Decode, false, QueueRepeatMode::Off),
        (FailureKind::Decode, true, QueueRepeatMode::One),
    ] {
        let settings = PlaybackSettings {
            skip_unplayable_tracks: skip,
            ..PlaybackSettings::default()
        };
        let (mut session, _commands, _guards) = skip_session(settings).await;
        session.sequence.set_repeat_mode(repeat);
        let start = session.sequence.selected_index();
        fail_current(&mut session, kind);
        assert_eq!(session.sequence.selected_index(), start, "{kind:?}");
        assert!(session.current_run().is_none(), "{kind:?}");
        assert!(session.last_error.is_some(), "{kind:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_next_preparation_is_skipped_at_the_track_boundary() {
    let (mut session, commands, _guards) = skip_session(PlaybackSettings::default()).await;
    let start = session.sequence.selected_index().unwrap();
    let current_run = session.current_run().unwrap();
    let next_run = session.next_plan.as_ref().unwrap().next_run;
    let kind = FailureKind::UnavailableMedia;
    session.handle_backend(
        BackendEvent::NextPreparationFailed {
            current_run,
            next_run,
            error: BackendFailure::with_kind(kind, kind.summary()),
        },
        &skip_session_sample(),
    );
    assert!(commands.lock().unwrap().iter().any(|command| matches!(
        command,
        BackendCommand::PrepareNext { current_run: run, next: None } if *run == current_run
    )));
    session.handle_backend(
        BackendEvent::Ended { run: current_run },
        &skip_session_sample(),
    );
    assert_eq!(session.sequence.selected_index(), Some(start + 2));
    let current = session.current_run.as_ref().unwrap();
    assert_ne!(current.id, next_run);
    assert!(current.desired_playing);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seeks_before_the_backend_loads_a_run_become_its_start_position() {
    let (mut session, commands, _guards) = skip_session(PlaybackSettings::default()).await;
    let run = session.current_run().unwrap();
    session.seek(42_000);
    assert_eq!(session.sequence.progress_millis(), 42_000);
    assert!(
        !commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| matches!(command, BackendCommand::Seek { .. }))
    );

    session.stream_resolved(
        run,
        PreparedStream::from(audio_processing::ResolvedStream::new(
            "https://example.test/track-0",
        )),
    );
    assert!(commands.lock().unwrap().iter().any(|command| matches!(
        command,
        BackendCommand::Start {
            start_position_millis: 42_000,
            ..
        }
    )));
}
