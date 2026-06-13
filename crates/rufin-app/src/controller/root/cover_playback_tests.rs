use super::*;

struct DeleteFailingSecretStore;
impl SecretStore for DeleteFailingSecretStore {
    fn save_secret(
        &self,
        _key: &rufin_secrets::SecretKey,
        _secret: &str,
    ) -> rufin_secrets::SecretResult<()> {
        Ok(())
    }

    fn load_secret(
        &self,
        _key: &rufin_secrets::SecretKey,
    ) -> rufin_secrets::SecretResult<Option<String>> {
        Ok(Some("token".to_string()))
    }

    fn delete_secret(&self, _key: &rufin_secrets::SecretKey) -> rufin_secrets::SecretResult<()> {
        Err(rufin_secrets::SecretError::Backend(
            "delete failed".to_string(),
        ))
    }
}

struct QueuedPlaybackEvents {
    events: Vec<PlaybackEvent>,
}

impl QueuedPlaybackEvents {
    fn new(events: Vec<PlaybackEvent>) -> Self {
        Self { events }
    }
}

impl PlaybackBackend for QueuedPlaybackEvents {
    fn send(&mut self, _command: PlaybackCommand) -> Result<(), rufin_playback::PlaybackError> {
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<PlaybackEvent> {
        std::mem::take(&mut self.events)
    }
}

struct RejectingPlaybackBackend {
    commands: Arc<Mutex<Vec<PlaybackCommand>>>,
}

impl RejectingPlaybackBackend {
    fn new(commands: Arc<Mutex<Vec<PlaybackCommand>>>) -> Self {
        Self { commands }
    }
}

impl PlaybackBackend for RejectingPlaybackBackend {
    fn send(&mut self, command: PlaybackCommand) -> Result<(), rufin_playback::PlaybackError> {
        self.commands
            .lock()
            .expect("commands")
            .push(command.clone());
        match command {
            PlaybackCommand::Play { .. } | PlaybackCommand::PlayPrepared { .. } => Err(
                rufin_playback::PlaybackError::Backend("start rejected".to_string()),
            ),
            _ => Ok(()),
        }
    }

    fn drain_events(&mut self) -> Vec<PlaybackEvent> {
        Vec::new()
    }
}

fn wait_for_queue_matching(
    events: &Receiver<ControllerEvent>,
    mut matches: impl FnMut(&QueueSnapshot) -> bool,
) -> QueueSnapshot {
    for _ in 0..8 {
        let queue = wait_for_queue(events).expect("queue");
        if matches(&queue) {
            return queue;
        }
    }
    panic!("matching queue event was not emitted");
}

fn wait_for_playback_matching(
    controller: &AppController,
    events: &Receiver<ControllerEvent>,
    mut matches: impl FnMut(&PlaybackSnapshot) -> bool,
) -> PlaybackSnapshot {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for playback"
        );
        controller.poll_playback_events();
        match events.recv_timeout(Duration::from_millis(50)) {
            Ok(ControllerEvent::Playback(playback)) if matches(&playback) => return *playback,
            Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("controller event channel closed")
            }
        }
    }
}

fn wait_for_repeat_without_queue(
    events: &Receiver<ControllerEvent>,
    repeat_mode: RepeatMode,
) -> PlaybackSnapshot {
    loop {
        match events
            .recv_timeout(Duration::from_secs(5))
            .expect("repeat event")
        {
            ControllerEvent::Playback(playback) if playback.repeat_mode == repeat_mode => {
                return *playback;
            }
            ControllerEvent::Playback(_) => {}
            ControllerEvent::Queue(_) => panic!("repeat mode emitted a queue event"),
            ControllerEvent::Error(error) => panic!("controller error: {error}"),
            _ => {}
        }
    }
}

fn wait_for_shuffle_without_queue(
    events: &Receiver<ControllerEvent>,
    enabled: bool,
) -> PlaybackSnapshot {
    loop {
        match events
            .recv_timeout(Duration::from_secs(5))
            .expect("shuffle event")
        {
            ControllerEvent::Playback(playback) if playback.shuffle_enabled == enabled => {
                return *playback;
            }
            ControllerEvent::Playback(_) => {}
            ControllerEvent::Queue(_) => panic!("shuffle mode emitted a queue event"),
            ControllerEvent::Error(error) => panic!("controller error: {error}"),
            _ => {}
        }
    }
}

pub(in crate::controller) fn wait_for_token_deleted(
    secrets: &Arc<dyn SecretStore>,
    server_id: &ServerId,
) {
    for _ in 0..100 {
        if secrets.load_token(server_id).expect("load token").is_none() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(secrets.load_token(server_id).expect("load token"), None);
}

fn provider_cover_ref() -> ImageRef {
    ImageRef::new("jellyfin:album:one", Some("tag-one".to_string()))
}

fn external_cover_ref() -> ImageRef {
    ImageRef::new(
        "external:album:Example%20Artist:Example%20Album",
        Some("external-v1-test".to_string()),
    )
}

fn test_cover_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "rufin-cover-test-{}-{label}.jpg",
        std::process::id()
    ))
}

fn seed_cover_cache(
    controller: &AppController,
    image_ref: &ImageRef,
    size: u32,
    path: &std::path::Path,
) -> ServerId {
    let saved = saved_server();
    let server_id = saved.server.id.clone();
    let image_tag = image_ref
        .tag
        .as_deref()
        .unwrap_or(IMAGE_TAG_UNTAGGED)
        .to_string();
    controller
        .store
        .with_store(|store| {
            store.save_server(&saved)?;
            store.set_active_server(&server_id)?;
            store.save_cover_cache_entry(&CoverCacheEntry {
                server_id: server_id.clone(),
                item_id: image_ref.item_id.clone(),
                image_tag,
                size,
                path: path.to_string_lossy().to_string(),
            })
        })
        .expect("seed cover cache");
    server_id
}

fn seed_external_cover_miss(
    controller: &AppController,
    image_ref: &ImageRef,
    size: u32,
) -> ServerId {
    let saved = saved_server();
    let server_id = saved.server.id.clone();
    let image_tag = image_ref.tag.as_deref().unwrap_or(IMAGE_TAG_UNTAGGED);
    controller
        .store
        .with_store(|store| {
            store.save_server(&saved)?;
            store.set_active_server(&server_id)?;
            store.save_external_image_lookup_miss(
                &server_id,
                &image_ref.item_id,
                image_tag,
                size,
                "external cover lookup found no usable image",
            )
        })
        .expect("seed external miss");
    server_id
}

#[test]
pub(in crate::controller) fn cover_use_states() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let server_id = snapshot.server.expect("server").id;
    assert_eq!(controller.startup_sync_delay_ms(), None);
    controller
        .store
        .with_store(|store| store.fail_sync(&server_id, "previous sync failed"))
        .expect("mark sync failed");
    assert_eq!(controller.startup_sync_delay_ms(), Some(8_000));
    controller.clear_active_server_cache();
    let _snapshot = wait_for_snapshot(&events);
    assert_eq!(controller.startup_sync_delay_ms(), Some(500));
}
#[test]
pub(in crate::controller) fn cover_emit_fetching() {
    let (controller, events, _snapshot, _queue, _player) =
        AppController::bootstrap_memory_for_test();
    let path = test_cover_path("ready-cached");
    fs::write(&path, [1_u8, 2, 3]).expect("write cover");
    let image_ref = provider_cover_ref();
    seed_cover_cache(&controller, &image_ref, 256, &path);
    let key = controller.cover_key(&image_ref, 256).expect("cover key");
    controller.request_cover(image_ref, 256);
    assert_eq!(wait_for_cover_ready(&events, &key), path);
    let _cleanup = fs::remove_file(path);
}
#[test]
pub(in crate::controller) fn cover_fetch_missing() {
    let (controller, events, _snapshot, _queue, _player) =
        AppController::bootstrap_memory_for_test();
    let root = unique_test_dir("local-cover-request");
    let _cleanup = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("local root");
    fs::write(root.join("track.mp3"), []).expect("track file");
    let cover_bytes = [0xff_u8, 0xd8, 0xff, 0xd9];
    fs::write(root.join("cover.jpg"), cover_bytes).expect("cover file");

    let mut settings = AppSettings::default();
    settings.sources.selected = Some(LibrarySourceSelection::Local);
    settings.sources.local_folders = vec![LocalLibraryFolder {
        path: root.to_string_lossy().to_string(),
    }];
    controller
        .store
        .save_settings(&settings)
        .expect("save settings");

    let saved = local_source_saved();
    let server_id = saved.server.id.clone();
    controller
        .store
        .with_store(|store| {
            store.save_server(&saved)?;
            store.set_active_server(&server_id)
        })
        .expect("seed local server");
    let provider = provider_for_saved(
        &controller.store,
        &controller.runtime,
        &controller.secrets,
        &saved,
    )
    .expect("local provider");
    controller
        .runtime
        .block_on(sync_provider(
            &controller.store,
            &server_id,
            provider.as_music_provider(),
        ))
        .expect("sync local provider");
    let image_ref = controller
        .store
        .with_store(|store| store.load_albums(&server_id, 0, 1))
        .expect("load albums")
        .items
        .into_iter()
        .next()
        .and_then(|album| album.image_ref)
        .expect("album image ref");
    let key = controller.cover_key(&image_ref, 256).expect("cover key");

    controller.request_cover_for_key(key.clone(), image_ref, 256);
    let path = wait_for_cover_ready(&events, &key);

    assert_eq!(fs::read(&path).expect("cached cover"), cover_bytes);
    let _cleanup = fs::remove_file(path);
    let _cleanup = fs::remove_dir_all(root);
}
#[test]
pub(in crate::controller) fn external_cache_cover() {
    let (controller, _events, _snapshot, _queue, _player) =
        AppController::bootstrap_memory_for_test();
    let path = test_cover_path("external-cached");
    fs::write(&path, [1_u8, 2, 3]).expect("write cover");
    let image_ref = external_cover_ref();
    seed_cover_cache(&controller, &image_ref, 256, &path);
    assert_eq!(
        controller.cached_cover_path(&image_ref, 256),
        Some(path.clone())
    );
    assert_eq!(controller.cached_cover_path(&image_ref, 512), None);
    assert_eq!(
        controller.cached_cover_path(&image_ref, 96),
        Some(path.clone())
    );
    let _cleanup = fs::remove_file(path);
}
#[test]
pub(in crate::controller) fn cover_external_size() {
    let (controller, _events, _snapshot, _queue, _player) =
        AppController::bootstrap_memory_for_test();
    let image_ref = external_cover_ref();
    seed_external_cover_miss(&controller, &image_ref, 256);

    assert!(controller.external_cover_lookup_known_missing(&image_ref, 96));
    assert!(controller.external_cover_lookup_known_missing(&image_ref, 512));
    assert!(!controller.external_cover_lookup_known_missing(
        &ImageRef::new("jellyfin:album:one", Some("tag-one".to_string())),
        256
    ));
}

#[test]
pub(in crate::controller) fn cover_clear_key() {
    let (controller, _events, _snapshot, _queue, _player) =
        AppController::bootstrap_memory_for_test();
    let image_ref = external_cover_ref();
    seed_external_cover_miss(&controller, &image_ref, 256);
    let key = controller.cover_key(&image_ref, 256).expect("cover key");
    let generation_before = controller
        .external_cover_retry_generation
        .load(Ordering::SeqCst);
    controller
        .cover_in_flight
        .lock()
        .expect("cover in-flight lock")
        .insert(key, generation_before);

    controller
        .retry_external_cover_lookups()
        .expect("retry external covers");

    assert!(!controller.external_cover_lookup_known_missing(&image_ref, 256));
    assert!(
        controller
            .cover_in_flight
            .lock()
            .expect("cover in-flight lock")
            .is_empty()
    );
    assert_eq!(
        controller
            .external_cover_retry_generation
            .load(Ordering::SeqCst),
        generation_before.saturating_add(1)
    );
}

#[test]
pub(in crate::controller) fn cover_emit_unavailable() {
    let (controller, events, _snapshot, _queue, _player) =
        AppController::bootstrap_memory_for_test();
    let image_ref = external_cover_ref();
    seed_external_cover_miss(&controller, &image_ref, 256);
    let key = controller.cover_key(&image_ref, 256).expect("cover key");

    controller.request_cover_for_key(key.clone(), image_ref, 256);

    loop {
        match events
            .recv_timeout(Duration::from_secs(5))
            .expect("controller event")
        {
            ControllerEvent::CoverUnavailable {
                key: event_key,
                external_retry_generation,
            } if event_key == key => {
                assert_eq!(external_retry_generation, Some(0));
                return;
            }
            ControllerEvent::CoverReady { key: event_key, .. } if event_key == key => {
                panic!("known missing cover unexpectedly became ready");
            }
            ControllerEvent::Snapshot(_)
            | ControllerEvent::LibrarySyncStatus(_)
            | ControllerEvent::LibraryDelta(_)
            | ControllerEvent::HomeSectionsUpdated { .. }
            | ControllerEvent::PlaylistChanged { .. }
            | ControllerEvent::SmartPlaylistChanged { .. }
            | ControllerEvent::FavoriteChanged { .. }
            | ControllerEvent::Queue(_)
            | ControllerEvent::Playback(_)
            | ControllerEvent::Visualizer(_)
            | ControllerEvent::Lyrics(_)
            | ControllerEvent::LyricsSearchResults { .. }
            | ControllerEvent::LyricsSearchFailed { .. }
            | ControllerEvent::LyricsSaved { .. }
            | ControllerEvent::FolderLoaded { .. }
            | ControllerEvent::FolderLoadFailed { .. }
            | ControllerEvent::HomeSectionPrefetched { .. }
            | ControllerEvent::ServerDiscovery { .. }
            | ControllerEvent::CoverReady { .. }
            | ControllerEvent::CoverUnavailable { .. }
            | ControllerEvent::CoverDeferred { .. }
            | ControllerEvent::LoginStatus(_) => {}
            ControllerEvent::Error(error) => panic!("controller error: {error}"),
        }
    }
}
#[test]
pub(in crate::controller) fn cache_cover_reuse() {
    let (controller, _events, _snapshot, _queue, _player) =
        AppController::bootstrap_memory_for_test();
    let path = test_cover_path("provider-cached");
    fs::write(&path, [1_u8, 2, 3]).expect("write cover");
    let image_ref = provider_cover_ref();
    seed_cover_cache(&controller, &image_ref, 256, &path);
    assert_eq!(
        controller.cached_cover_path(&image_ref, 256),
        Some(path.clone())
    );
    assert_eq!(controller.cached_cover_path(&image_ref, 512), None);
    assert_eq!(
        controller.cached_cover_path(&image_ref, 96),
        Some(path.clone())
    );
    let _cleanup = fs::remove_file(path);
}
#[test]
pub(in crate::controller) fn cover_thumbnail_request() {
    let (controller, _events, _snapshot, _queue, _player) =
        AppController::bootstrap_memory_for_test();
    let path = test_cover_path("thumbnail");
    fs::write(&path, [1_u8, 2, 3]).expect("write cover");
    let image_ref = provider_cover_ref();
    seed_cover_cache(&controller, &image_ref, 96, &path);
    assert_eq!(controller.cached_cover_path(&image_ref, 256), None);
    assert_eq!(
        controller.cached_cover_path(&image_ref, 96),
        Some(path.clone())
    );
    let _cleanup = fs::remove_file(path);
}
#[test]
pub(in crate::controller) fn cover_read_lookup() {
    let (controller, _events, _snapshot, _queue, _player) =
        AppController::bootstrap_memory_for_test();
    let path = test_cover_path("missing-cached");
    let _cleanup = fs::remove_file(&path);
    let image_ref = provider_cover_ref();
    let server_id = seed_cover_cache(&controller, &image_ref, 256, &path);
    assert_eq!(controller.cached_cover_path(&image_ref, 256), None);
    assert_eq!(
        controller
            .store
            .with_store(|store| store.load_cover_cache_entry(
                &server_id,
                &image_ref.item_id,
                "tag-one",
                256
            ))
            .expect("load cover cache"),
        Some(CoverCacheEntry {
            server_id,
            item_id: image_ref.item_id,
            image_tag: "tag-one".to_string(),
            size: 256,
            path: path.to_string_lossy().to_string(),
        })
    );
}

#[test]
pub(in crate::controller) fn cover_reuses_external_content_for_local_source() {
    let (controller, _events, _snapshot, _queue, _player) =
        AppController::bootstrap_memory_for_test();
    let path = test_cover_path("external-content-local");
    fs::write(&path, [1_u8, 2, 3]).expect("write cover");
    let remote = saved_server();
    let local = local_source_saved();
    let image_ref = ImageRef::new(
        "external:mb-release-group:group-one",
        Some("external-v2-test".to_string()),
    );
    controller
        .store
        .with_store(|store| {
            store.save_server(&remote)?;
            store.save_server(&local)?;
            store.save_cover_cache_entry(&CoverCacheEntry {
                server_id: remote.server.id.clone(),
                item_id: image_ref.item_id.clone(),
                image_tag: image_ref.tag.clone().expect("external tag"),
                size: 256,
                path: path.to_string_lossy().to_string(),
            })?;
            store.set_active_server(&local.server.id)
        })
        .expect("seed external cache");

    assert_eq!(
        controller.cached_cover_path(&image_ref, 256),
        Some(path.clone())
    );
    let _cleanup = fs::remove_file(path);
}

#[test]
pub(in crate::controller) fn cover_delete_token() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let server_id = snapshot.server.expect("server").id;
    controller
        .secrets
        .save_token(&server_id, "token")
        .expect("save token");
    controller.forget_active_server();
    let snapshot = wait_for_snapshot(&events);
    assert!(snapshot.first_run);
    wait_for_token_deleted(&controller.secrets, &server_id);
}
#[test]
pub(in crate::controller) fn cover_emit_run() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let server_id = snapshot.server.expect("server").id;
    controller
        .secrets
        .save_token(&server_id, "token")
        .expect("save token");
    let _permit = controller
        .sync_in_flight
        .acquire(server_id.clone())
        .expect("sync guard")
        .expect("sync permit");

    controller.forget_server(server_id.clone());

    let snapshot = wait_for_snapshot(&events);
    assert!(snapshot.first_run);
    assert!(snapshot.server.is_none());
    assert!(snapshot.servers.is_empty());
    assert_eq!(
        controller
            .store
            .with_store(|store| store.list_servers())
            .expect("servers"),
        Vec::new()
    );
    assert!(!controller.sync_in_flight.contains_or_blocked(&server_id));
    wait_for_token_deleted(&controller.secrets, &server_id);
}
#[test]
pub(in crate::controller) fn cover_delete_fails() {
    let (mut controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let server_id = snapshot.server.expect("server").id;
    controller.secrets = Arc::new(DeleteFailingSecretStore);

    controller.forget_server(server_id);

    let snapshot = wait_for_snapshot(&events);
    assert!(snapshot.first_run);
    assert!(snapshot.server.is_none());
    assert!(snapshot.servers.is_empty());
    assert_eq!(
        controller
            .store
            .with_store(|store| store.list_servers())
            .expect("servers"),
        Vec::new()
    );
}
#[test]
pub(in crate::controller) fn cover_start_sync() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let server_id = snapshot.server.expect("server").id;
    let _permit = controller
        .sync_in_flight
        .acquire(server_id)
        .expect("sync guard")
        .expect("sync permit");
    controller.resync_active_server();
    assert_eq!(wait_for_status(&events), "Sync already running.");
}
#[test]
pub(in crate::controller) fn cover_persist_queue() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let track = snapshot.tracks[0].clone();
    controller.play_now(track.clone());
    let queue = wait_for_queue(&events).expect("queue");
    assert_eq!(queue.entries.len(), 1);
    assert_eq!(queue.entries[0].track_id, track.id.clone());
    assert_eq!(
        controller
            .store
            .with_store(|store| store.load_queue_snapshot(&queue.server_id))
            .expect("store")
            .expect("snapshot")
            .entries
            .len(),
        1
    );
    let queue = wait_for_queue_matching(&events, |queue| {
        queue.entries.len() == 1 + super::AUTO_DJ_ITEM_COUNT
    });
    assert_eq!(queue.entries[0].track_id, track.id);
    let playback = wait_for_playback_state(&controller, &events, PlaybackState::Playing);
    assert_eq!(
        playback.current.expect("current").track_id,
        queue.entries[0].track_id
    );
}
#[test]
pub(in crate::controller) fn cover_start_stream() {
    let (controller, _events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let commands = Arc::new(Mutex::new(Vec::new()));
    *controller.playback.lock().expect("playback") =
        Box::new(RecordingPlaybackBackend::new(Arc::clone(&commands)));
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first.clone(), second.clone()]);
    let command = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PlayPrepared { .. })
    });
    let PlaybackCommand::PlayPrepared { item, next, .. } = command else {
        panic!("expected prepared play command");
    };
    assert_eq!(item.track.id, first.id);
    assert!(next.is_none());
    let command = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PrepareNext(Some(_)))
    });
    let PlaybackCommand::PrepareNext(Some(item)) = command else {
        panic!("expected prepared next command");
    };
    assert_eq!(item.track.id, second.id);
}
#[test]
pub(in crate::controller) fn playback_duplicate_current_start_ignored() {
    let (controller, _events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let commands = Arc::new(Mutex::new(Vec::new()));
    *controller.playback.lock().expect("playback") =
        Box::new(RecordingPlaybackBackend::new(Arc::clone(&commands)));
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first, second]);
    let _play = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PlayPrepared { .. })
    });
    let _prepare = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PrepareNext(Some(_)))
    });
    commands.lock().expect("commands").clear();

    controller.start_current_track();
    std::thread::sleep(std::time::Duration::from_millis(150));

    assert!(commands.lock().expect("commands").is_empty());
}

#[test]
pub(in crate::controller) fn rejected_start_keeps_committed_playback_current() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let commands = Arc::new(Mutex::new(Vec::new()));
    *controller.playback.lock().expect("playback") =
        Box::new(RecordingPlaybackBackend::new(Arc::clone(&commands)));
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first.clone(), second.clone()]);
    let _queue = wait_for_queue(&events).expect("queue");
    let _playback = wait_for_playback_state(&controller, &events, PlaybackState::Playing);

    commands.lock().expect("commands").clear();
    *controller.playback.lock().expect("playback") =
        Box::new(RejectingPlaybackBackend::new(Arc::clone(&commands)));
    let second_entry = controller
        .queue_snapshot()
        .expect("queue")
        .entries
        .iter()
        .find(|entry| entry.track_id == second.id)
        .expect("second entry")
        .id
        .clone();

    controller.activate_queue_entry(second_entry);
    let _queue = wait_for_queue(&events).expect("queue");
    let _rejected = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PlayPrepared { .. })
    });

    let playback = controller
        .playback_snapshot
        .lock()
        .expect("playback snapshot")
        .clone();
    assert_eq!(playback.state, PlaybackState::Playing);
    assert_eq!(playback.current.expect("current").track_id, first.id);
}

#[test]
pub(in crate::controller) fn stale_desired_queue_current_does_not_receive_playback_progress() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let commands = Arc::new(Mutex::new(Vec::new()));
    *controller.playback.lock().expect("playback") =
        Box::new(RecordingPlaybackBackend::new(Arc::clone(&commands)));
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first.clone(), second.clone()]);
    let _queue = wait_for_queue(&events).expect("queue");
    let _playback = wait_for_playback_state(&controller, &events, PlaybackState::Playing);

    commands.lock().expect("commands").clear();
    *controller.playback.lock().expect("playback") =
        Box::new(RejectingPlaybackBackend::new(Arc::clone(&commands)));
    let second_entry = controller
        .queue_snapshot()
        .expect("queue")
        .entries
        .iter()
        .find(|entry| entry.track_id == second.id)
        .expect("second entry")
        .id
        .clone();

    controller.activate_queue_entry(second_entry);
    let queue = wait_for_queue(&events).expect("queue");
    assert_eq!(queue.current_index, Some(1));
    let _rejected = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PlayPrepared { .. })
    });

    *controller.playback.lock().expect("playback") = Box::new(QueuedPlaybackEvents::new(vec![
        PlaybackEvent::PositionChanged {
            track_id: Some(first.id.clone()),
            seconds: 42,
            millis: 42_000,
        },
    ]));
    controller.poll_playback_events();

    let playback = controller
        .playback_snapshot
        .lock()
        .expect("playback snapshot")
        .clone();
    assert_eq!(playback.current.expect("current").track_id, first.id);
    assert_eq!(playback.position_seconds, 42);
    let queue = controller.queue_snapshot().expect("queue");
    assert_eq!(queue.current_index, Some(1));
    assert_eq!(
        queue.entries[queue.current_index.expect("current")].track_id,
        second.id
    );
    assert_eq!(queue.progress_seconds, 0);
}

#[test]
pub(in crate::controller) fn playback_current_queue_activation_restarts() {
    let (controller, _events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let commands = Arc::new(Mutex::new(Vec::new()));
    *controller.playback.lock().expect("playback") =
        Box::new(RecordingPlaybackBackend::new(Arc::clone(&commands)));
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first.clone(), second]);
    let _play = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PlayPrepared { .. })
    });
    let _prepare = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PrepareNext(Some(_)))
    });
    let current_entry_id = controller
        .queue_snapshot()
        .expect("queue")
        .entries
        .first()
        .expect("current entry")
        .id
        .clone();
    commands.lock().expect("commands").clear();

    controller.activate_queue_entry(current_entry_id);

    let command = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PlayPrepared { .. })
    });
    let PlaybackCommand::PlayPrepared { item, .. } = command else {
        panic!("expected prepared play command");
    };
    assert_eq!(item.track.id, first.id);
}
#[test]
pub(in crate::controller) fn cover_change_backend() {
    let (controller, _events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let commands = Arc::new(Mutex::new(Vec::new()));
    *controller.playback.lock().expect("playback") =
        Box::new(RecordingPlaybackBackend::new(Arc::clone(&commands)));
    let server_id = snapshot.server.as_ref().expect("server").id.clone();
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first, second.clone()]);
    let _play = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PlayPrepared { .. })
    });
    let _initial_prepare = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PrepareNext(Some(_)))
    });
    commands.lock().expect("commands").clear();
    let root = unique_test_dir("reprepare-local-access");
    fs::create_dir_all(&root).expect("create root");
    controller.save_server_local_access(
        server_id.clone(),
        root.clone(),
        Some("/server/music".to_string()),
        Some(root.to_string_lossy().into_owned()),
    );
    let command = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PrepareNext(Some(_)))
    });
    let PlaybackCommand::PrepareNext(Some(item)) = command else {
        panic!("expected prepared next command");
    };
    assert_eq!(item.track.id, second.id);
    commands.lock().expect("commands").clear();
    controller.clear_server_local_access(server_id);
    let command = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PrepareNext(Some(_)))
    });
    let PlaybackCommand::PrepareNext(Some(item)) = command else {
        panic!("expected prepared next command");
    };
    assert_eq!(item.track.id, second.id);
    let _cleanup = fs::remove_dir_all(root);
}
#[test]
pub(in crate::controller) fn prepared_send_reject() {
    let (_controller, _events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let server_id = snapshot.server.as_ref().expect("server").id.clone();
    let first = snapshot.tracks[0].clone();
    let repeated = snapshot.tracks[1].clone();
    let mut engine = QueueEngine::new(server_id);
    engine.play_now(&first);
    let initial_next_entry_id = engine.append(&repeated);
    let replacement_next_entry_id = engine.append(&repeated);
    let queue = Arc::new(Mutex::new(Some(engine)));
    let request =
        next_preload_request_from_queue(&queue, &PlaybackSettings::default()).expect("request");
    assert_eq!(request.next_entry_id, initial_next_entry_id);
    {
        let mut queue = queue.lock().expect("queue");
        let queue = queue.as_mut().expect("queue");
        assert!(queue.move_after_current(&replacement_next_entry_id));
    }
    let (current_entry_id, next_entry_id, next_track_id) = {
        let queue = queue.lock().expect("queue");
        let queue = queue.as_ref().expect("queue");
        let current = queue.current().expect("current");
        let next = next_queue_entry_after_current(queue).expect("next");
        (current.id.clone(), next.id, next.track_id)
    };
    assert_eq!(current_entry_id, request.current_entry_id);
    assert_eq!(next_track_id, request.next_entry.track_id);
    assert_ne!(next_entry_id, request.next_entry_id);
    let commands = Arc::new(Mutex::new(Vec::new()));
    let playback = Arc::new(Mutex::new(
        Box::new(RecordingPlaybackBackend::new(Arc::clone(&commands))) as Box<dyn PlaybackBackend>,
    ));
    let (events, _receiver) = channel();
    let prepared = prepared_item_from_entry(
        &request.next_entry,
        StreamDescriptor::new("fake://local/stream/duplicate"),
    );
    assert!(!send_prepared_next(
        &playback, &queue, &events, &request, prepared
    ));
    assert!(commands.lock().expect("commands").is_empty());
}
#[test]
pub(in crate::controller) fn prepared_skip_current_repeat() {
    let (_controller, _events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let server_id = snapshot.server.as_ref().expect("server").id.clone();
    let track = snapshot.tracks[0].clone();
    let mut engine = QueueEngine::new(server_id);
    engine.play_now(&track);
    let queue = Arc::new(Mutex::new(Some(engine)));

    assert!(next_preload_request_from_queue(&queue, &PlaybackSettings::default()).is_none());
}
#[test]
pub(in crate::controller) fn prepared_uses_shuffled_next() {
    let (_controller, _events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let server_id = snapshot.server.as_ref().expect("server").id.clone();
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    let third = snapshot.tracks[2].clone();
    let mut engine = QueueEngine::new(server_id);
    engine.play_now(&first);
    engine.append(&second);
    engine.append(&third);
    engine.set_shuffle(true, 19);
    let expected = next_queue_entry_after_current(&engine)
        .expect("shuffled queue should have next")
        .id;
    let queue = Arc::new(Mutex::new(Some(engine)));

    let request =
        next_preload_request_from_queue(&queue, &PlaybackSettings::default()).expect("request");

    assert_eq!(request.next_entry_id, expected);
}
#[test]
pub(in crate::controller) fn prepared_uses_appended_next() {
    let (_controller, _events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let server_id = snapshot.server.as_ref().expect("server").id.clone();
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    let mut engine = QueueEngine::new(server_id);
    engine.play_now(&first);
    let appended = engine.append(&second);
    let queue = Arc::new(Mutex::new(Some(engine)));

    let request =
        next_preload_request_from_queue(&queue, &PlaybackSettings::default()).expect("request");

    assert_eq!(request.next_entry_id, appended);
}
#[test]
pub(in crate::controller) fn prepared_next_dedupes_until_cleared() {
    let (controller, _events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let server_id = snapshot.server.as_ref().expect("server").id.clone();
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    let mut engine = QueueEngine::new(server_id);
    engine.play_now(&first);
    engine.append(&second);
    let queue = Arc::new(Mutex::new(Some(engine)));
    let commands = Arc::new(Mutex::new(Vec::new()));
    let playback = Arc::new(Mutex::new(
        Box::new(RecordingPlaybackBackend::new(Arc::clone(&commands))) as Box<dyn PlaybackBackend>,
    ));
    let next_preload = Arc::new(Mutex::new(NextPreloadState::default()));
    let events = controller.events.clone();

    prepare_next_stream_from_handles(
        controller.store.clone(),
        Arc::clone(&controller.runtime),
        Arc::clone(&controller.secrets),
        Arc::clone(&playback),
        Arc::clone(&queue),
        Arc::clone(&next_preload),
        events.clone(),
    );
    prepare_next_stream_from_handles(
        controller.store.clone(),
        Arc::clone(&controller.runtime),
        Arc::clone(&controller.secrets),
        Arc::clone(&playback),
        Arc::clone(&queue),
        Arc::clone(&next_preload),
        events.clone(),
    );
    let command = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PrepareNext(Some(_)))
    });
    let PlaybackCommand::PrepareNext(Some(item)) = command else {
        panic!("expected prepared next command");
    };
    assert_eq!(item.track.id, second.id);
    std::thread::sleep(std::time::Duration::from_millis(150));
    let prepare_count = commands
        .lock()
        .expect("commands")
        .iter()
        .filter(|command| matches!(command, PlaybackCommand::PrepareNext(Some(_))))
        .count();
    assert_eq!(prepare_count, 1);

    clear_next_preload(&next_preload);
    commands.lock().expect("commands").clear();
    prepare_next_stream_from_handles(
        controller.store.clone(),
        Arc::clone(&controller.runtime),
        Arc::clone(&controller.secrets),
        Arc::clone(&playback),
        Arc::clone(&queue),
        next_preload,
        events,
    );
    let command = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PrepareNext(Some(_)))
    });
    let PlaybackCommand::PrepareNext(Some(item)) = command else {
        panic!("expected prepared next command");
    };
    assert_eq!(item.track.id, second.id);
}
#[test]
pub(in crate::controller) fn cover_reject_switch() {
    let (_controller, _events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let server_id = snapshot.server.as_ref().expect("server").id.clone();
    let track = snapshot.tracks[0].clone();
    let mut engine = QueueEngine::new(server_id.clone());
    engine.play_now(&track);
    let entry = engine.current().expect("current").clone();
    let queue = Arc::new(Mutex::new(Some(engine)));
    let playback_request_generation = Arc::new(AtomicU64::new(1));

    assert!(request_generation_match(
        &playback_request_generation,
        1,
        &queue,
        &server_id,
        &entry
    ));

    *queue.lock().expect("queue") = Some(QueueEngine::new(ServerId::new("server:other")));
    assert!(!request_generation_match(
        &playback_request_generation,
        1,
        &queue,
        &server_id,
        &entry
    ));
}
#[test]
pub(in crate::controller) fn playback_request_reject() {
    let (_controller, _events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let server_id = snapshot.server.as_ref().expect("server").id.clone();
    let track = snapshot.tracks[0].clone();
    let mut engine = QueueEngine::new(server_id.clone());
    engine.play_now(&track);
    let stale_entry = engine.current().expect("current").clone();
    let mut replacement = QueueEngine::new(server_id.clone());
    replacement.play_now(&track);
    let queue = Arc::new(Mutex::new(Some(replacement)));
    let playback_request_generation = Arc::new(AtomicU64::new(1));
    invalidate_playback_requests(&playback_request_generation);

    assert!(!request_generation_match(
        &playback_request_generation,
        1,
        &queue,
        &server_id,
        &stale_entry
    ));
}
#[test]
pub(in crate::controller) fn cover_track_selected() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first, second.clone()]);
    let queue = wait_for_queue(&events).expect("queue");
    let second_entry = queue
        .entries
        .iter()
        .find(|entry| entry.track_id == second.id)
        .expect("second entry")
        .id
        .clone();
    let _initial_playback = wait_for_playback_state(&controller, &events, PlaybackState::Playing);
    controller.activate_queue_entry(second_entry);
    let queue = wait_for_queue(&events).expect("activated queue");
    assert_eq!(queue.current_index, Some(1));
    let playback = wait_for_playback_matching(&controller, &events, |playback| {
        playback.state == PlaybackState::Playing
            && playback
                .current
                .as_ref()
                .is_some_and(|entry| entry.track_id == second.id)
    });
    assert_eq!(playback.current.expect("current").track_id, second.id);
}
#[test]
pub(in crate::controller) fn cover_emit_position() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    controller.play_now(snapshot.tracks[0].clone());
    let _playback = wait_for_playback_state(&controller, &events, PlaybackState::Playing);
    controller.seek_millis(12_345);
    let playback =
        wait_for_playback_track_position(&controller, &events, &snapshot.tracks[0].id, 12_345);
    assert_eq!(playback.position_seconds, 12);
}
#[test]
pub(in crate::controller) fn cover_ignore_positions() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    controller.play_now(snapshot.tracks[0].clone());
    let playing = wait_for_playback_state(&controller, &events, PlaybackState::Playing);
    let initial_position = playing.position_millis;

    *controller.playback.lock().expect("playback") = Box::new(QueuedPlaybackEvents::new(vec![
        PlaybackEvent::Error("stream failed".to_string()),
        PlaybackEvent::PositionChanged {
            track_id: None,
            seconds: 42,
            millis: 42_000,
        },
    ]));

    controller.poll_playback_events();

    let playback = controller
        .playback_snapshot
        .lock()
        .expect("playback snapshot")
        .clone();
    assert_eq!(playback.state, PlaybackState::Stopped);
    assert_eq!(playback.last_error.as_deref(), Some("stream failed"));
    assert_eq!(playback.position_millis, initial_position);
    assert_ne!(
        controller
            .queue_snapshot()
            .expect("queue snapshot")
            .progress_seconds,
        42
    );
}
#[test]
pub(in crate::controller) fn cover_keep_sync() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first.clone(), second.clone()]);
    let _queue = wait_for_queue(&events).expect("queue");
    controller.next_track();
    let queue = wait_for_queue(&events).expect("next queue");
    assert_eq!(
        queue.entries[queue.current_index.expect("current")].track_id,
        second.id
    );
    controller.previous_track();
    let queue = wait_for_queue(&events).expect("previous queue");
    assert_eq!(
        queue.entries[queue.current_index.expect("current")].track_id,
        first.id
    );
    controller.clear_queue();
    let queue = wait_for_queue(&events).expect("clear queue");
    assert!(queue.entries.is_empty());
}
#[test]
pub(in crate::controller) fn cover_track_first() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.toggle_auto_dj();
    let _playback = wait_for_playback_auto_dj(&events, false);
    controller.play_tracks_now(vec![first.clone(), second.clone()]);
    let _queue = wait_for_queue(&events).expect("queue");
    controller.next_track();
    let _queue = wait_for_queue(&events).expect("next queue");
    controller.seek_millis(12_000);
    let _playback = wait_for_playback_track_position(&controller, &events, &second.id, 12_000);
    controller.next_track();
    let playback = wait_for_playback_track_position(&controller, &events, &first.id, 0);
    assert_eq!(playback.current.expect("current").track_id, first.id);
    assert_ne!(playback.state, PlaybackState::Stopped);
}
#[test]
pub(in crate::controller) fn manual_ten_seconds() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first, second.clone()]);
    let _queue = wait_for_queue(&events).expect("queue");
    controller.next_track();
    let _queue = wait_for_queue(&events).expect("next queue");
    controller.seek_millis(11_000);
    let _playback = wait_for_playback_track_position(&controller, &events, &second.id, 11_000);
    controller.previous_track();
    let playback = wait_for_playback_track_position(&controller, &events, &second.id, 0);
    assert_eq!(playback.current.expect("current").track_id, second.id);
}
#[test]
pub(in crate::controller) fn cover_use_order() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    controller.play_now(snapshot.tracks[0].clone());
    let _queue = wait_for_queue(&events).expect("queue");
    controller.cycle_repeat();
    let playback = wait_for_playback_repeat(&events, RepeatMode::One);
    assert_eq!(playback.repeat_mode, RepeatMode::One);
    controller.cycle_repeat();
    let playback = wait_for_playback_repeat(&events, RepeatMode::Off);
    assert_eq!(playback.repeat_mode, RepeatMode::Off);
    controller.cycle_repeat();
    let playback = wait_for_playback_repeat(&events, RepeatMode::All);
    assert_eq!(playback.repeat_mode, RepeatMode::All);
}

#[test]
pub(in crate::controller) fn playback_modes_do_not_emit_queue() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    controller.play_tracks_now(vec![snapshot.tracks[0].clone(), snapshot.tracks[1].clone()]);
    let _queue = wait_for_queue(&events).expect("queue");
    let _playback = wait_for_playback_repeat(&events, RepeatMode::All);

    controller.cycle_repeat();
    let playback = wait_for_repeat_without_queue(&events, RepeatMode::One);
    assert_eq!(playback.repeat_mode, RepeatMode::One);

    controller.toggle_shuffle();
    let playback = wait_for_shuffle_without_queue(&events, true);
    assert!(playback.shuffle_enabled);
}

#[test]
pub(in crate::controller) fn cover_use_sqlite() {
    let dir = unique_test_dir("settings-round-trip");
    let settings_path = dir.join("config").join(SETTINGS_FILE_NAME);
    let cache_database_path = dir.join(CACHE_DATABASE_FILE_NAME);
    let store = StoreHandle::Path {
        cache_database_path: cache_database_path.clone(),
        settings_path: settings_path.clone(),
    };
    let settings = AppSettings {
        theme_preference: ThemePreference::Dark,
        auto_dj_enabled: true,
        ..AppSettings::default()
    };
    store.save_settings(&settings).expect("save settings");
    assert_eq!(load_settings_from_store(&store), settings);
    assert!(settings_path.exists());
    assert!(!cache_database_path.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(&settings_path)
                .expect("settings metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let _cleanup = fs::remove_dir_all(dir);
}
#[test]
pub(in crate::controller) fn cover_app_root() {
    let root = PathBuf::from("/tmp/rufin-path-layout");
    assert_eq!(
        cache_db_path(&root.join("cache")),
        root.join("cache")
            .join("store")
            .join(CACHE_DATABASE_FILE_NAME)
    );
    assert_eq!(
        settings_file_path(&root.join("config")),
        root.join("config").join(SETTINGS_FILE_NAME)
    );
    assert_eq!(
        cover_cache_path(&root.join("cache")),
        root.join("cache").join("covers")
    );
    assert_eq!(
        lyrics_cache_dir(&root.join("cache")),
        root.join("cache").join("lyrics")
    );
    assert_eq!(
        playback_cache_dir(&root.join("cache")),
        root.join("cache").join("playback")
    );
}
#[test]
pub(in crate::controller) fn cover_create_folder() {
    let root = unique_test_dir("cache-layout");
    ensure_app_cache_dirs(&root).expect("ensure cache layout");
    assert!(root.join("store").is_dir());
    assert!(root.join("covers").is_dir());
    assert!(root.join("lyrics").is_dir());
    assert!(root.join("playback").is_dir());
    assert!(!root.join("tmp").exists());
    assert!(!root.join("playlists").exists());
    let _cleanup = fs::remove_dir_all(root);
}
#[test]
pub(in crate::controller) fn cover_remove_waveform_tmp() {
    let root = unique_test_dir("waveform-tmp");
    let waveform_tmp = root.join("tmp").join("waveforms");
    fs::create_dir_all(&waveform_tmp).expect("create waveform tmp");
    fs::write(waveform_tmp.join("track.audio"), b"audio").expect("write waveform tmp");

    remove_waveform_tmp(&root).expect("remove waveform tmp");

    assert!(!root.join("tmp").exists());
    let _cleanup = fs::remove_dir_all(root);
}
#[test]
pub(in crate::controller) fn cover_emit_state() {
    let (controller, events, _snapshot, _queue, player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    assert!(player.auto_dj_enabled);
    controller.toggle_auto_dj();
    let playback = wait_for_playback_auto_dj(&events, false);
    assert!(!playback.auto_dj_enabled);
    assert!(!controller.load_settings().auto_dj_enabled);
}
#[test]
pub(in crate::controller) fn random_play_now() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let expected = random_track_ids(&snapshot.tracks, 3);
    controller.play_random_tracks(random_request(RandomPlayAction::PlayNow, 3));
    let queue = wait_for_queue(&events).expect("random queue");
    assert_eq!(queue.current_index, Some(0));
    assert_eq!(
        queue
            .entries
            .iter()
            .map(|entry| entry.track_id.clone())
            .collect::<Vec<_>>(),
        expected
    );
}
#[test]
pub(in crate::controller) fn random_play_next() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    let expected_random = random_track_ids(&snapshot.tracks, 2);
    controller.play_tracks_now(vec![first.clone(), second.clone()]);
    let _queue = wait_for_queue(&events).expect("initial queue");
    controller.play_random_tracks(random_request(RandomPlayAction::PlayNext, 2));
    let queue = wait_for_queue(&events).expect("random next queue");
    let ids = queue
        .entries
        .iter()
        .map(|entry| entry.track_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(queue.current_index, Some(0));
    assert_eq!(ids[0], first.id);
    assert_eq!(&ids[1..3], expected_random.as_slice());
    assert_eq!(ids[3], second.id);
}
#[test]
pub(in crate::controller) fn random_add_append() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    let expected_random = random_track_ids(&snapshot.tracks, 2);
    controller.play_tracks_now(vec![first.clone(), second.clone()]);
    let _queue = wait_for_queue(&events).expect("initial queue");
    controller.play_random_tracks(random_request(RandomPlayAction::AddLast, 2));
    let queue = wait_for_queue(&events).expect("random append queue");
    let ids = queue
        .entries
        .iter()
        .map(|entry| entry.track_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(queue.current_index, Some(0));
    assert_eq!(ids[0], first.id);
    assert_eq!(ids[1], second.id);
    assert_eq!(&ids[2..4], expected_random.as_slice());
}
#[test]
pub(in crate::controller) fn play_append_track() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    let third = snapshot.tracks[2].clone();
    let fourth = snapshot.tracks[3].clone();
    controller.play_tracks_now(vec![first.clone(), second.clone()]);
    let _queue = wait_for_queue(&events).expect("initial queue");
    controller.play_last(vec![third.clone(), fourth.clone()]);
    let queue = wait_for_queue(&events).expect("append queue");
    let ids = queue
        .entries
        .iter()
        .map(|entry| entry.track_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(queue.current_index, Some(0));
    assert_eq!(ids, vec![first.id, second.id, third.id, fourth.id]);
}
#[test]
pub(in crate::controller) fn cover_auto_library() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let first = snapshot.tracks[0].clone();
    controller.play_now(first.clone());
    let queue = wait_for_queue(&events).expect("initial queue");
    assert_eq!(queue.entries.len(), 1);
    let queue = wait_for_queue_matching(&events, |queue| {
        queue.entries.len() == 1 + super::AUTO_DJ_ITEM_COUNT
    });
    assert_eq!(queue.entries.len(), 1 + super::AUTO_DJ_ITEM_COUNT);
    assert_eq!(queue.entries[0].track_id, first.id);
    assert_eq!(
        queue
            .entries
            .iter()
            .map(|entry| entry.track_id.clone())
            .collect::<HashSet<_>>()
            .len(),
        queue.entries.len()
    );
}
#[test]
pub(in crate::controller) fn cover_auto_timing() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let mut settings = controller.load_settings();
    settings.auto_dj_refill_threshold = 1;
    controller
        .save_settings(&settings)
        .expect("save Auto DJ settings");
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first, second]);
    let queue = wait_for_queue(&events).expect("queue before threshold refill");
    assert_eq!(queue.entries.len(), 2);

    let mut settings = controller.load_settings();
    settings.auto_dj_refill_threshold = 2;
    controller
        .save_settings(&settings)
        .expect("save Auto DJ settings");
    controller.refill_auto_dj_queue();
    let queue = wait_for_queue(&events).expect("queue after threshold refill");
    assert_eq!(queue.entries.len(), 2 + super::AUTO_DJ_ITEM_COUNT);
}
#[test]
pub(in crate::controller) fn cover_auto_end() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let mut settings = controller.load_settings();
    settings.auto_dj_refill_threshold = 0;
    controller
        .save_settings(&settings)
        .expect("save Auto DJ settings");
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first, second.clone()]);
    let _queue = wait_for_queue(&events).expect("queue");
    controller.next_track();
    let queue = wait_for_queue_matching(&events, |queue| {
        queue.entries[queue.current_index.expect("current")].track_id == second.id
    });
    assert_eq!(
        queue.entries[queue.current_index.expect("current")].track_id,
        second.id
    );
    let mut settings = controller.load_settings();
    settings.auto_dj_refill_threshold = 2;
    controller
        .save_settings(&settings)
        .expect("save Auto DJ settings");
    controller.next_track();
    let queue = wait_for_queue_matching(&events, |queue| {
        queue.entries.len() == 2 + super::AUTO_DJ_ITEM_COUNT
            && queue.entries[queue.current_index.expect("current")].track_id != second.id
    });
    assert_eq!(queue.entries.len(), 2 + super::AUTO_DJ_ITEM_COUNT);
    assert_ne!(
        queue.entries[queue.current_index.expect("current")].track_id,
        second.id
    );
}

#[test]
pub(in crate::controller) fn cover_auto_next() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let mut settings = controller.load_settings();
    settings.auto_dj_refill_threshold = 0;
    controller
        .save_settings(&settings)
        .expect("save Auto DJ settings");
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first, second.clone()]);
    let _queue = wait_for_queue(&events).expect("queue");

    let mut settings = controller.load_settings();
    settings.auto_dj_refill_threshold = 2;
    controller
        .save_settings(&settings)
        .expect("save Auto DJ settings");
    controller.advance_after_end_of_stream();
    let queue = wait_for_queue_matching(&events, |queue| {
        queue.entries[queue.current_index.expect("current")].track_id == second.id
    });
    assert_eq!(queue.entries.len(), 2);
    assert_eq!(
        queue.entries[queue.current_index.expect("current")].track_id,
        second.id
    );

    let queue = wait_for_queue_matching(&events, |queue| {
        queue.entries.len() == 2 + super::AUTO_DJ_ITEM_COUNT
    });
    assert_eq!(
        queue.entries[queue.current_index.expect("current")].track_id,
        second.id
    );
}
#[test]
pub(in crate::controller) fn cover_track_related() {
    let current = library_track(
        1,
        Some(ArtistId::fake(1)),
        AlbumId::fake(1),
        "Artist",
        &["Rock"],
    );
    let related = library_track(
        2,
        Some(ArtistId::fake(1)),
        AlbumId::fake(1),
        "Artist",
        &["Rock"],
    );
    let genre_only = library_track(
        3,
        Some(ArtistId::fake(2)),
        AlbumId::fake(2),
        "Other",
        &["Rock"],
    );
    let unrelated = library_track(
        4,
        Some(ArtistId::fake(3)),
        AlbumId::fake(3),
        "Other",
        &["Jazz"],
    );
    let mut queue = QueueEngine::new(ServerId::fake(1));
    queue.play_now(&current);
    let current_entry = queue.current().expect("current").clone();
    let queued = HashSet::from([current.id.clone()]);
    let candidates = auto_dj_candidates(
        &[
            unrelated.clone(),
            current.clone(),
            genre_only,
            related.clone(),
        ],
        &current_entry,
        &queued,
        7,
    );
    assert_eq!(candidates[0].id, related.id);
    assert!(candidates.iter().all(|track| track.id != current.id));
}
#[test]
pub(in crate::controller) fn end_stream_repeat() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first.clone(), second]);
    let _queue = wait_for_queue(&events).expect("queue");
    controller.cycle_repeat();
    let _playback = wait_for_playback_repeat(&events, RepeatMode::One);
    controller.advance_after_end_of_stream();
    let queue = wait_for_queue(&events).expect("repeated queue");
    assert_eq!(
        queue.entries[queue.current_index.expect("current")].track_id,
        first.id
    );
}
#[test]
pub(in crate::controller) fn end_of_stream_advances_queue() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first, second.clone()]);
    let _queue = wait_for_queue(&events).expect("queue");
    controller.advance_after_end_of_stream();
    let queue = wait_for_queue(&events).expect("next queue");
    assert_eq!(
        queue.entries[queue.current_index.expect("current")].track_id,
        second.id
    );
}
#[test]
pub(in crate::controller) fn cover_track_event() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first.clone(), second.clone()]);
    let _queue = wait_for_queue(&events).expect("queue");
    let _playback = wait_for_playback_state(&controller, &events, PlaybackState::Playing);
    *controller.playback.lock().expect("playback") = Box::new(QueuedPlaybackEvents::new(vec![
        PlaybackEvent::EndOfStream,
        PlaybackEvent::StateChanged(PlaybackState::Stopped),
        PlaybackEvent::PositionChanged {
            track_id: Some(first.id.clone()),
            seconds: first.duration_seconds,
            millis: u64::from(first.duration_seconds) * 1_000,
        },
        PlaybackEvent::DurationChanged {
            track_id: Some(first.id.clone()),
            seconds: first.duration_seconds,
        },
    ]));

    controller.poll_playback_events();

    let playback = wait_for_playback_matching(&controller, &events, |playback| {
        playback.state == PlaybackState::Buffering
            && playback
                .current
                .as_ref()
                .is_some_and(|entry| entry.track_id == second.id)
    });
    assert_eq!(playback.current.expect("current").track_id, second.id);
    assert_eq!(playback.state, PlaybackState::Buffering);
    assert_eq!(playback.position_seconds, 0);
    assert_eq!(playback.position_millis, 0);
    assert_eq!(playback.duration_seconds, second.duration_seconds);
    let queue = controller.queue_snapshot().expect("queue snapshot");
    assert_eq!(queue.progress_seconds, 0);
}
#[test]
pub(in crate::controller) fn cover_track_queue() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first.clone(), second.clone()]);
    let _queue = wait_for_queue(&events).expect("queue");
    let _playback = wait_for_playback_state(&controller, &events, PlaybackState::Playing);
    controller.advance_after_end_of_stream();
    let _queue = wait_for_queue(&events).expect("next queue");
    let _playback = wait_for_playback_matching(&controller, &events, |playback| {
        playback
            .current
            .as_ref()
            .is_some_and(|entry| entry.track_id == second.id)
    });
    *controller.playback.lock().expect("playback") = Box::new(QueuedPlaybackEvents::new(vec![
        PlaybackEvent::PositionChanged {
            track_id: Some(first.id.clone()),
            seconds: first.duration_seconds,
            millis: u64::from(first.duration_seconds) * 1_000,
        },
        PlaybackEvent::DurationChanged {
            track_id: Some(first.id.clone()),
            seconds: first.duration_seconds,
        },
    ]));

    controller.poll_playback_events();

    let playback = controller
        .playback_snapshot
        .lock()
        .expect("playback snapshot")
        .clone();
    assert_eq!(playback.current.expect("current").track_id, second.id);
    assert_eq!(playback.position_seconds, 0);
    assert_eq!(playback.position_millis, 0);
    assert_eq!(playback.duration_seconds, second.duration_seconds);
}
#[test]
pub(in crate::controller) fn cover_track_ignored() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    controller.play_tracks_now(vec![first.clone(), second.clone()]);
    let _queue = wait_for_queue(&events).expect("queue");
    let _playback = wait_for_playback_state(&controller, &events, PlaybackState::Playing);
    *controller.playback.lock().expect("playback") = Box::new(QueuedPlaybackEvents::new(vec![
        PlaybackEvent::DurationChanged {
            track_id: Some(second.id.clone()),
            seconds: second.duration_seconds,
        },
    ]));

    controller.poll_playback_events();

    let playback = controller
        .playback_snapshot
        .lock()
        .expect("playback snapshot")
        .clone();
    assert_eq!(playback.current.expect("current").track_id, first.id);
    assert_eq!(playback.duration_seconds, first.duration_seconds);
}
#[test]
pub(in crate::controller) fn cover_ignores_implausible_backend_duration() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let first = snapshot.tracks[0].clone();
    controller.play_tracks_now(vec![first.clone()]);
    let _queue = wait_for_queue(&events).expect("queue");
    let _playback = wait_for_playback_state(&controller, &events, PlaybackState::Playing);
    *controller.playback.lock().expect("playback") = Box::new(QueuedPlaybackEvents::new(vec![
        PlaybackEvent::DurationChanged {
            track_id: Some(first.id.clone()),
            seconds: 99 * 60 * 60 + 99 * 60 + 99,
        },
    ]));

    controller.poll_playback_events();

    let playback = controller
        .playback_snapshot
        .lock()
        .expect("playback snapshot")
        .clone();
    assert_eq!(playback.current.expect("current").track_id, first.id);
    assert_eq!(playback.duration_seconds, first.duration_seconds);
}
#[test]
pub(in crate::controller) fn cover_advance_playback() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let commands = Arc::new(Mutex::new(Vec::new()));
    *controller.playback.lock().expect("playback") =
        Box::new(RecordingPlaybackBackend::new(Arc::clone(&commands)));
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    let third = snapshot.tracks[2].clone();
    controller.play_tracks_now(vec![first, second.clone(), third.clone()]);
    let _initial_queue = wait_for_queue(&events).expect("initial queue");
    let _command = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PlayPrepared { .. })
    });
    let command = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PrepareNext(Some(_)))
    });
    let PlaybackCommand::PrepareNext(Some(item)) = command else {
        panic!("expected prepared next command");
    };
    assert_eq!(item.track.id, second.id);
    commands.lock().expect("commands").clear();
    controller.advance_after_prepared_track_started(PlaybackTrack {
        id: second.id.clone(),
        album_id: Some(second.album_id.clone()),
        title: second.title.clone(),
        artist: second.artist.clone(),
        album: second.album.clone(),
        duration_seconds: second.duration_seconds,
    });
    let queue = wait_for_queue(&events).expect("queue");
    assert_eq!(
        queue.entries[queue.current_index.expect("current")].track_id,
        second.id
    );
    let playback = controller
        .playback_snapshot
        .lock()
        .expect("playback snapshot")
        .clone();
    assert_eq!(playback.state, PlaybackState::Playing);
    assert_eq!(playback.current.expect("current").track_id, second.id);
    assert_eq!(playback.position_seconds, 0);
    assert_eq!(playback.position_millis, 0);
    assert_eq!(playback.duration_seconds, second.duration_seconds);
    let command = wait_for_recorded_command(&commands, |command| {
        matches!(command, PlaybackCommand::PrepareNext(Some(_)))
    });
    let PlaybackCommand::PrepareNext(Some(item)) = command else {
        panic!("expected prepared next command");
    };
    assert_eq!(item.track.id, third.id);
    assert!(
        commands
            .lock()
            .expect("commands")
            .iter()
            .all(|command| !matches!(
                command,
                PlaybackCommand::Play { .. } | PlaybackCommand::PlayPrepared { .. }
            ))
    );
}
#[test]
pub(in crate::controller) fn cover_update_snapshot() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap_with_fake(FakeScale::Small);
    let track = snapshot
        .tracks
        .iter()
        .find(|track| !track.favorite)
        .expect("non-favorite track")
        .clone();
    controller.play_now(track.clone());
    let _playback = wait_for_playback_state(&controller, &events, PlaybackState::Playing);
    controller.toggle_current_favorite();
    let playback = wait_for_playback_current_favorite(&controller, &events, true);
    assert_eq!(playback.current.expect("current").track_id, track.id);
    let (item_id, favorite, snapshot) = wait_for_favorite_changed(&events);
    assert_eq!(item_id, FavoriteItemId::Track(track.id.clone()));
    assert!(favorite);
    assert!(
        snapshot
            .tracks
            .iter()
            .find(|candidate| candidate.id == track.id)
            .expect("cached track")
            .favorite
    );
    assert!(
        snapshot
            .favorites
            .iter()
            .any(|candidate| candidate.id == track.id)
    );
}
