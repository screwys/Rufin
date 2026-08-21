use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Waker};

use library::{
    AcceptedPlay, CandidateBatch, CandidateFinish, CandidateHeader, FolderContents, HomeFacts,
    MetadataChange, MetadataEdit, MetadataEditing, MetadataItemId, MusicFolder, MusicFolderId,
    Track, TrackData, TrackRelations, TrackSort,
};
use secrets::{MemorySecretStore, SwitchableSecretStore};
use sources::{
    LocalFolderHostInput, NativeSourceResult, ObservedSourceChange, PreparedSourceChange,
    SourceConfiguration, SourceError, SourceSetupInput,
};

use super::*;

#[test]
fn open_subsonic_api_key_mode_crosses_the_ui_boundary_explicitly() {
    let setup = source_setup_input(
        SourceSetup::OpenSubsonic {
            kind: OpenSubsonicKind::OpenSubsonic,
            authentication: OpenSubsonicAuthentication::ApiKey,
            credentials: CredentialInput {
                source_name: Some("Cloud Music".to_string()),
                server_url: "https://cloud.example/apps/music/subsonic".to_string(),
                username: String::new(),
                secret: "server-issued-key".to_string(),
                trust_invalid_cert: false,
            },
        },
        "unused-jellyfin-device",
    );

    let SourceSetupInput::Subsonic {
        flavor,
        authentication,
        credentials,
    } = setup
    else {
        panic!("OpenSubsonic setup must remain provider owned")
    };
    assert_eq!(flavor, SubsonicFlavor::Subsonic);
    assert_eq!(authentication, SubsonicAuthentication::ApiKey);
    assert_eq!(credentials.username, "");
    assert_eq!(credentials.password, "server-issued-key");
}

#[test]
fn saved_open_subsonic_authentication_is_projected_for_editing() {
    let configuration = SourceConfiguration {
        source_id: SourceId::new("subsonic:server:test"),
        kind: "subsonic".to_string(),
        name: "Cloud Music".to_string(),
        provider_payload: serde_json::json!({
            "version": 1,
            "base_url": "https://cloud.example/apps/music/subsonic",
            "username": "listener",
            "trust_invalid_cert": false,
            "authentication": "api_key"
        })
        .to_string(),
    };

    let editable = editable_source(&configuration).expect("editable OpenSubsonic source");
    assert_eq!(
        editable.credentials.open_subsonic_authentication,
        Some(OpenSubsonicAuthentication::ApiKey)
    );
    assert_eq!(editable.credentials.username, "listener");
}

fn test_jellyfin_source(source_id: SourceId) -> (SourceConfiguration, Arc<Source>) {
    let configuration = SourceConfiguration {
        source_id,
        kind: "jellyfin".to_string(),
        name: "Jellyfin".to_string(),
        provider_payload: serde_json::json!({
            "version": 1,
            "base_url": "http://127.0.0.1:9",
            "server_id": null,
            "user_id": "listener",
            "username": "listener",
            "trust_invalid_cert": false,
            "use_jellyfin_instant_mix": false,
        })
        .to_string(),
    };
    let source = Arc::new(
        Source::open(
            configuration.clone(),
            Some("test-token".to_string()),
            Some("test-device".to_string()),
        )
        .expect("open Jellyfin fixture"),
    );
    (configuration, source)
}

fn jellyfin_change(id: &str) -> ObservedSourceChange {
    ObservedSourceChange::Jellyfin {
        upserts: BTreeSet::from([id.to_string()]),
        removals: BTreeSet::new(),
    }
}

#[test]
fn configured_feed_reinstall_preserves_pending_changes() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let settings = SettingsFile::memory();
    let (bootstrap, _events) = test_owner(directory.path(), &runtime, libraries, settings);
    let source_id = SourceId::new("jellyfin:server:feed-reinstall");
    let (_configuration, source) = test_jellyfin_source(source_id.clone());
    let feed =
        ConfiguredJellyfinFeed::test(Arc::clone(&source), Some(jellyfin_change("first")), true);
    let changes = Arc::clone(&feed.changes);
    bootstrap
        .owner
        .shared
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .jellyfin_feeds
        .insert(source_id.clone(), feed);

    bootstrap
        .owner
        .install_configured_jellyfin_feed(source, true);

    assert_eq!(
        changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take(),
        Some(jellyfin_change("first"))
    );
}

#[test]
fn disconnected_baseline_keeps_full_reconcile_after_the_feed_connects() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let settings = SettingsFile::memory();
    let (bootstrap, _events) = test_owner(directory.path(), &runtime, libraries, settings);
    let source_id = SourceId::new("jellyfin:server:baseline-gap");
    let (_configuration, source) = test_jellyfin_source(source_id.clone());
    let feed = ConfiguredJellyfinFeed::test(source, None, false);
    let changes = Arc::clone(&feed.changes);
    bootstrap
        .owner
        .shared
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .jellyfin_feeds
        .insert(source_id.clone(), feed);

    bootstrap.owner.begin_configured_baseline(&source_id);
    let mut pending = changes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pending.connected = true;
    pending.merge(jellyfin_change("arrived-during-baseline"));
    assert_eq!(pending.take(), Some(ObservedSourceChange::Full));
}

#[test]
fn cancelled_feed_drain_requeues_and_bounds_pending_work() {
    let changes = Arc::new(Mutex::new(PendingChanges::new(
        Some(jellyfin_change("first")),
        true,
    )));
    let active = changes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .expect("claim pending change");
    let run = ObservedChangeRun::test(Arc::clone(&changes), active);
    changes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .merge(jellyfin_change("second"));
    drop(run);

    let expected = ObservedSourceChange::Jellyfin {
        upserts: BTreeSet::from(["first".to_string(), "second".to_string()]),
        removals: BTreeSet::new(),
    };
    assert_eq!(
        changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take(),
        Some(expected)
    );

    let mut overflow = PendingChanges::new(None, true);
    overflow.merge(ObservedSourceChange::Jellyfin {
        upserts: (0..=PendingChanges::MAXIMUM_IDS)
            .map(|id| format!("track-{id}"))
            .collect(),
        removals: BTreeSet::new(),
    });
    assert_eq!(overflow.take(), Some(ObservedSourceChange::Full));
}

#[test]
fn deselected_jellyfin_feed_retains_its_source_without_retaining_the_library() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("jellyfin:server:feed-owner");
    let library = accept_library(&libraries, source_id.clone(), Vec::new(), Vec::new(), 1);
    let library_weak = Arc::downgrade(&library);
    let settings = SettingsFile::memory();
    let (bootstrap, events) = test_owner(directory.path(), &runtime, libraries, settings);
    let (configuration, source) = test_jellyfin_source(source_id.clone());
    let source_weak = Arc::downgrade(&source);
    install_selected_for_test(
        &bootstrap.owner,
        configuration,
        Some(Arc::clone(&source)),
        Arc::clone(&library),
        SourceSessionEpoch::new(1),
    );
    bootstrap
        .owner
        .shared
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .jellyfin_feeds
        .insert(
            source_id.clone(),
            ConfiguredJellyfinFeed::test(Arc::clone(&source), None, true),
        );
    drop(source);
    drop(library);

    runtime.spawn(async move {
        if let Ok(SourceEvent::ReleaseSelected { acknowledged }) = events.recv().await {
            let _ = acknowledged.send(()).await;
        }
    });
    runtime.block_on(bootstrap.owner.shared.release_selected());

    assert!(source_weak.upgrade().is_some());
    assert!(library_weak.upgrade().is_none());
    bootstrap.owner.remove_configured_feed(&source_id);
    assert!(source_weak.upgrade().is_none());
}

#[test]
fn failed_transition_resumes_requeued_selected_jellyfin_change() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("jellyfin:server:failed-transition");
    let library = accept_library(&libraries, source_id.clone(), Vec::new(), Vec::new(), 1);
    let settings = SettingsFile::memory();
    let (bootstrap, _events) = test_owner(directory.path(), &runtime, libraries, settings);
    let (configuration, source) = test_jellyfin_source(source_id.clone());
    install_selected_for_test(
        &bootstrap.owner,
        configuration,
        Some(Arc::clone(&source)),
        library,
        SourceSessionEpoch::new(1),
    );
    let feed = ConfiguredJellyfinFeed::test(source, Some(jellyfin_change("requeued")), true);
    let changes = Arc::clone(&feed.changes);
    {
        let mut state = bootstrap
            .owner
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.selected_revealed = true;
        state.jellyfin_feeds.insert(source_id.clone(), feed);
    }

    runtime.block_on(async {
        let lane = bootstrap.owner.shared.lane.lock().await;
        bootstrap
            .owner
            .as_ref()
            .clone()
            .fail_transition(
                Some(SourceId::new("jellyfin:server:failed-target")),
                "target failed".to_string(),
                false,
            )
            .await;
        assert_eq!(
            changes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take(),
            None,
            "the restored selected feed must claim its queued change immediately",
        );
        drop(lane);
    });
}

#[test]
fn cancelling_interruptible_work_aborts_the_lane_task() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let settings = SettingsFile::memory();
    let (bootstrap, _events) = test_owner(directory.path(), &runtime, libraries, settings);
    let cancelled = Arc::new(AtomicBool::new(false));
    let qualifier = SourceQualifier {
        source_id: SourceId::new("navidrome:server:interruptible-lane"),
        epoch: SourceSessionEpoch::new(1),
    };
    let refresh = Arc::new(RefreshRequest {
        qualifier: qualifier.clone(),
        visible: AtomicBool::new(false),
        started: AtomicBool::new(false),
        announced: AtomicBool::new(false),
        cancelled: Arc::clone(&cancelled),
    });
    {
        let mut state = bootstrap
            .owner
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.refresh = Some(Arc::clone(&refresh));
        assert!(state.freshness.admit(1, true, tokio::time::Instant::now()));
    }
    let (started, started_receiver) = async_channel::bounded(1);
    let (_release, release_receiver) = async_channel::bounded::<()>(1);
    let (acquired, acquired_receiver) = async_channel::bounded(1);

    runtime.block_on(async {
        bootstrap.owner.spawn_serialized_with_cancel(
            true,
            Arc::clone(&cancelled),
            move |_, _| async move {
                started.send(()).await.expect("report lane acquisition");
                let _ = release_receiver.recv().await;
            },
        );
        started_receiver
            .recv()
            .await
            .expect("interruptible work acquired the lane");

        bootstrap
            .owner
            .spawn_serialized(false, move |_, _| async move {
                acquired.send(()).await.expect("report lane acquisition");
            });
        bootstrap.owner.shared.cancel_interruptible();

        acquired_receiver
            .recv()
            .await
            .expect("queued normal work acquired the released lane");
    });

    assert!(cancelled.load(Ordering::Acquire));
    let state = bootstrap
        .owner
        .shared
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(state.refresh.is_none());
    assert!(state.freshness.pending.is_none());
}

#[test]
fn protected_commit_finishes_before_a_newer_transition_starts() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let settings = SettingsFile::memory();
    let (bootstrap, events) = test_owner(directory.path(), &runtime, libraries, settings);
    let cancelled = Arc::new(AtomicBool::new(false));
    let committed = Arc::new(AtomicBool::new(false));
    let (blocking_started, blocking_started_receiver) = async_channel::bounded(1);
    let (release, release_receiver) = std::sync::mpsc::channel();
    let (order, order_receiver) = async_channel::unbounded();

    runtime.block_on(async {
        let committed_in_task = Arc::clone(&committed);
        let published = order.clone();
        bootstrap.owner.spawn_serialized_with_cancel(
            true,
            Arc::clone(&cancelled),
            move |operations, cancelled| async move {
                assert!(operations.shared.protect_interruptible_commit(&cancelled));
                tokio::task::spawn_blocking(move || {
                    blocking_started
                        .try_send(())
                        .expect("report blocking commit start");
                    release_receiver.recv().expect("release blocking commit");
                    committed_in_task.store(true, Ordering::Release);
                })
                .await
                .expect("join blocking commit");
                operations
                    .shared
                    .send_event(SourceEvent::Operation(SourceOperation::Switching {
                        target: SourceId::new("local:server:older-commit"),
                        progress: initial_progress(),
                    }))
                    .await;
                operations.shared.publish_configured().await;
                published
                    .send("published")
                    .await
                    .expect("report commit publication");
            },
        );
        blocking_started_receiver
            .recv()
            .await
            .expect("protected commit started");

        let moved_on = order.clone();
        bootstrap.owner.spawn_transition(
            SourceOperation::Switching {
                target: SourceId::new("local:server:newer-transition"),
                progress: initial_progress(),
            },
            None,
            false,
            move |_, _| async move {
                moved_on.send("next").await.expect("report next lane task");
                Ok(())
            },
        );
        release.send(()).expect("release protected commit");

        assert_eq!(order_receiver.recv().await.as_deref(), Ok("published"));
        assert_eq!(order_receiver.recv().await.as_deref(), Ok("next"));
        assert!(matches!(events.recv().await, Ok(SourceEvent::Operation(
            SourceOperation::Switching { target, .. }
        )) if target == SourceId::new("local:server:older-commit")));
        assert!(matches!(
            events.recv().await,
            Ok(SourceEvent::Configured(_))
        ));
        assert!(matches!(events.recv().await, Ok(SourceEvent::Operation(
            SourceOperation::Switching { target, .. }
        )) if target == SourceId::new("local:server:newer-transition")));
    });

    assert!(cancelled.load(Ordering::Acquire));
    assert!(committed.load(Ordering::Acquire));
}

#[test]
fn freshness_admission_throttles_normal_requests_and_reopens_after_cancellation() {
    let now = tokio::time::Instant::now();
    let mut admission = FreshnessAdmission::new(now);
    admission.defer(now);

    assert!(!admission.admit(1, false, now));
    let catch_up = now + Duration::from_secs(1);
    assert!(admission.admit(1, true, catch_up));
    assert!(!admission.admit(2, true, catch_up));

    admission.cancel();
    assert!(admission.admit(3, true, catch_up));
    admission.finish(1);
    assert!(!admission.admit(4, true, catch_up));
    admission.finish(3);

    let next = catch_up + SOURCE_CHECK_INTERVAL;
    assert!(!admission.admit(5, false, next - Duration::from_nanos(1)));
    assert!(admission.admit(5, false, next));
    admission.finish(5);
    assert!(!admission.admit(6, false, next));
}

#[test]
fn failed_forget_settings_write_keeps_the_selected_runtime() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let settings_directory = directory.path().join("settings");
    std::fs::create_dir(&settings_directory).expect("create settings directory");
    let settings_path = settings_directory.join("settings.json");
    let settings = SettingsFile::open(settings_path.clone()).expect("open settings");
    let source_id = SourceId::new("local:server:failed-forget");
    let configuration = test_configuration(source_id.clone(), "Forget failure");
    settings
        .update(|stored| {
            stored.sources.configured = vec![ConfiguredSource {
                configuration: configuration.clone(),
                credential_ref: None,
                music_folder_id: None,
                local_access: None,
            }];
            stored.sources.selected_source_id = Some(source_id.clone());
            Ok(())
        })
        .expect("save configured source");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let library = accept_library(&libraries, source_id.clone(), Vec::new(), Vec::new(), 1);
    let (bootstrap, events) = test_owner(directory.path(), &runtime, libraries, settings.clone());
    let session = install_selected_for_test(
        &bootstrap.owner,
        configuration,
        None,
        library,
        SourceSessionEpoch::new(1),
    );
    runtime.spawn(async move {
        while let Ok(event) = events.recv().await {
            if let SourceEvent::ReleaseSelected { acknowledged } = event {
                let _ = acknowledged.send(()).await;
            }
        }
    });

    std::fs::remove_file(settings_path).expect("remove writable settings file");
    std::fs::remove_dir(&settings_directory).expect("remove writable settings directory");
    std::fs::write(&settings_directory, "not a directory").expect("block settings directory");

    runtime.block_on(async {
        bootstrap
            .owner
            .as_ref()
            .clone()
            .forget_now(source_id.clone())
            .await;
    });

    assert!(session.resolve().is_some());
    assert_eq!(
        bootstrap
            .owner
            .shared
            .selected()
            .map(|selected| selected.source_id().clone()),
        Some(source_id.clone())
    );
    let stored = settings.load();
    assert_eq!(stored.sources.selected_source_id, Some(source_id.clone()));
    assert!(
        stored
            .sources
            .configured
            .iter()
            .any(|configured| configured.configuration.source_id == source_id)
    );
}

#[test]
fn failed_forget_replacement_publishes_the_remaining_sources_and_failure() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let removed_id = SourceId::new("local:server:forget-selected");
    let replacement_id = SourceId::new("invalid:server:forget-replacement");
    let removed = test_configuration(removed_id.clone(), "Removed");
    let replacement = SourceConfiguration {
        source_id: replacement_id.clone(),
        kind: "invalid".to_string(),
        name: "Invalid replacement".to_string(),
        provider_payload: "{}".to_string(),
    };
    let settings = SettingsFile::memory();
    settings
        .update(|stored| {
            stored.sources.configured = vec![
                ConfiguredSource {
                    configuration: removed.clone(),
                    credential_ref: None,
                    music_folder_id: None,
                    local_access: None,
                },
                ConfiguredSource {
                    configuration: replacement,
                    credential_ref: None,
                    music_folder_id: None,
                    local_access: None,
                },
            ];
            stored.sources.selected_source_id = Some(removed_id.clone());
            Ok(())
        })
        .expect("save configured sources");
    let library = accept_library(&libraries, removed_id.clone(), Vec::new(), Vec::new(), 1);
    let (bootstrap, events) = test_owner(directory.path(), &runtime, libraries, settings.clone());
    let session = install_selected_for_test(
        &bootstrap.owner,
        removed,
        None,
        library,
        SourceSessionEpoch::new(1),
    );
    let (observed, observed_receiver) = async_channel::unbounded();
    runtime.spawn(async move {
        while let Ok(event) = events.recv().await {
            if let SourceEvent::ReleaseSelected { acknowledged } = &event {
                let _ = acknowledged.send(()).await;
            }
            let _ = observed.send(event).await;
        }
    });

    let events = runtime.block_on(async {
        bootstrap
            .owner
            .as_ref()
            .clone()
            .forget_now(removed_id.clone())
            .await;
        let mut events = Vec::new();
        for _ in 0..4 {
            events.push(
                observed_receiver
                    .recv()
                    .await
                    .expect("forget replacement event"),
            );
        }
        events
    });

    assert!(matches!(&events[0], SourceEvent::Operation(
        SourceOperation::Switching { target, .. }
    ) if target == &replacement_id));
    assert!(matches!(&events[1], SourceEvent::ReleaseSelected { .. }));
    assert!(matches!(&events[2], SourceEvent::Configured(_)));
    assert!(matches!(&events[3], SourceEvent::Operation(
        SourceOperation::Failed { source_id, .. }
    ) if source_id.as_ref() == Some(&replacement_id)));
    assert!(session.resolve().is_none());
    assert!(bootstrap.owner.shared.selected().is_none());
    let stored = settings.load();
    assert!(stored.sources.selected_source_id.is_none());
    assert_eq!(stored.sources.configured.len(), 1);
    assert_eq!(
        stored.sources.configured[0].configuration.source_id,
        replacement_id
    );
}

#[test]
fn active_source_resolves_replacement_and_rejects_retired_session() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("local:server:session-fence");
    let library = accept_library(
        &libraries,
        source_id.clone(),
        vec![test_track(
            library::TrackId::new("local:track:session-fence"),
            "Session",
            PathBuf::from("Session.flac"),
            None,
        )],
        Vec::new(),
        1,
    );
    let settings = SettingsFile::memory();
    let (bootstrap, _events) = test_owner(directory.path(), &runtime, libraries, settings);
    let mut configuration = test_configuration(source_id.clone(), "First");
    let session = install_selected_for_test(
        &bootstrap.owner,
        configuration.clone(),
        None,
        Arc::clone(&library),
        SourceSessionEpoch::new(1),
    );

    configuration.name = "Replacement".to_string();
    let mut replacement = (*session.resolve().expect("selected session")).clone();
    replacement.configuration = configuration.clone();
    assert!(bootstrap.owner.shared.replace_selected(replacement));
    assert_eq!(
        session
            .resolve()
            .expect("same session replacement")
            .configuration
            .name,
        "Replacement"
    );

    let next = install_selected_for_test(
        &bootstrap.owner,
        configuration,
        None,
        library,
        SourceSessionEpoch::new(2),
    );
    assert!(session.resolve().is_none());
    assert_eq!(
        next.resolve().expect("new session").source_session_epoch,
        SourceSessionEpoch::new(2)
    );
}

#[test]
fn stopping_observer_waits_for_its_thread_to_release_owned_state() {
    let runtime = test_runtime();
    let cancelled = Arc::new(AtomicBool::new(false));
    let thread_cancelled = Arc::clone(&cancelled);
    let sentinel = Arc::new(());
    let released = Arc::downgrade(&sentinel);
    let (cancel_seen, cancelled_seen) = std::sync::mpsc::channel();
    let (release, released_by_test) = std::sync::mpsc::channel();
    let (completed, completion) = tokio::sync::oneshot::channel();
    let handle = std::thread::Builder::new()
        .name("rufin-local-watcher-test".to_string())
        .spawn(move || {
            while !thread_cancelled.load(Ordering::Acquire) {
                std::thread::park();
            }
            let _sentinel = sentinel;
            cancel_seen.send(()).expect("report watcher cancellation");
            released_by_test.recv().expect("release watcher thread");
            drop(_sentinel);
            let _ = completed.send(());
        })
        .expect("start watcher thread");
    let observer = ActiveObserver {
        qualifier: SourceQualifier {
            source_id: SourceId::new("local:server:watcher-stop"),
            epoch: SourceSessionEpoch::new(1),
        },
        cancelled,
        completion: Some(completion),
        handle: Some(handle),
    };

    let mut stop = Box::pin(observer.stop());
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(stop.as_mut().poll(&mut context), Poll::Pending));
    cancelled_seen
        .recv()
        .expect("watcher observed cancellation");
    assert!(
        released.upgrade().is_some(),
        "the watcher must still own its sentinel while stop is pending"
    );
    release.send(()).expect("allow watcher thread to finish");
    runtime.block_on(stop);
    assert!(
        released.upgrade().is_none(),
        "stop must join after the watcher releases its sentinel"
    );
}

#[test]
fn replacing_unavailable_local_folder_recovers_configured_source() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let unavailable = directory.path().join("Unavailable");
    let replacement = directory.path().join("Replacement");
    std::fs::create_dir(&unavailable).expect("create original Local root");
    std::fs::create_dir(&replacement).expect("create replacement Local root");
    let runtime = test_runtime();
    let connected = runtime
        .block_on(Source::connect(SourceSetupInput::Local(
            LocalFolderHostInput {
                roots: vec![unavailable.clone()],
            },
        )))
        .expect("connect original Local source");
    let (configuration, _source, credential) = connected.into_parts();
    assert_eq!(credential, None);
    let source_id = configuration.source_id.clone();
    let configured_root = local_roots(&configuration)
        .expect("configured Local roots")
        .into_iter()
        .next()
        .expect("original Local root");
    std::fs::remove_dir(&unavailable).expect("make original Local root unavailable");

    let settings = SettingsFile::memory();
    settings
        .update(|stored| {
            stored.sources.configured = vec![ConfiguredSource {
                configuration,
                credential_ref: None,
                music_folder_id: None,
                local_access: None,
            }];
            Ok(())
        })
        .expect("save original Local source");
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let (bootstrap, events) = test_owner(directory.path(), &runtime, libraries, settings);

    SourcePort::replace_local_folder(
        bootstrap.owner.as_ref(),
        configured_root.to_string_lossy().into_owned(),
        replacement.clone(),
    );
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(events.recv().await, Ok(SourceEvent::Configured(_))) {
                    break;
                }
            }
        })
        .await
        .expect("folder replacement completes");
    });

    let saved = configured_source(&bootstrap.owner.shared.settings.load().sources, &source_id)
        .expect("replacement keeps the configured Local source");
    assert_eq!(saved.configuration.source_id, source_id);
    assert_eq!(
        local_roots(&saved.configuration).expect("saved replacement Local roots"),
        vec![
            replacement
                .canonicalize()
                .expect("canonical replacement root")
        ]
    );
}

#[test]
fn same_session_executor_change_retires_previous_access_tasks() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let root_a = directory.path().join("A");
    let root_b = directory.path().join("B");
    std::fs::create_dir(&root_a).expect("create first Local root");
    std::fs::create_dir(&root_b).expect("create replacement Local root");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let connected = runtime
        .block_on(Source::connect(SourceSetupInput::Local(
            LocalFolderHostInput {
                roots: vec![root_a],
            },
        )))
        .expect("connect first Local source");
    let (configuration, source, credential) = connected.into_parts();
    assert_eq!(credential, None);
    let source = Arc::new(source);
    let candidate = runtime
        .block_on(
            Arc::clone(&source).prepare_library_candidate(
                libraries.clone(),
                configuration
                    .input_identity()
                    .expect("first Local input identity"),
                None,
                Arc::new(|_| {}),
                Arc::new(AtomicBool::new(false)),
            ),
        )
        .expect("prepare first Local library");
    let library = candidate
        .accept()
        .expect("accept first Local library")
        .library;
    let settings = SettingsFile::memory();
    settings
        .update(|stored| {
            stored.sources.configured = vec![ConfiguredSource {
                configuration: configuration.clone(),
                credential_ref: None,
                music_folder_id: None,
                local_access: None,
            }];
            stored.sources.selected_source_id = Some(configuration.source_id.clone());
            Ok(())
        })
        .expect("save first Local source");
    let (bootstrap, _events) = test_owner(directory.path(), &runtime, libraries, settings);
    let playback = attach_test_playback(&bootstrap.owner, &runtime, directory.path());
    let session = install_selected_for_test(
        &bootstrap.owner,
        configuration.clone(),
        Some(Arc::clone(&source)),
        library,
        SourceSessionEpoch::new(1),
    );
    let prepared = playback
        .prepare_selected(
            Arc::clone(&session),
            session.resolve().expect("selected source for Playback"),
        )
        .expect("prepare selected Playback");
    let cutover = playback.stop_for_source_switch();
    let _projection = playback.install_prepared(prepared, cutover);
    let qualifier = session.resolve().expect("selected source").qualifier();
    let observer_cancelled = Arc::new(AtomicBool::new(false));
    let local_cancelled = Arc::new(AtomicBool::new(false));
    let queued_observer_work = {
        let session = Arc::clone(&session);
        let cancelled = Arc::clone(&observer_cancelled);
        move || resolve_observer_session(&cancelled, &session)
    };
    let observer_thread_cancelled = Arc::clone(&observer_cancelled);
    let (observer_completed, observer_completion) = tokio::sync::oneshot::channel();
    let observer_handle = std::thread::Builder::new()
        .name("rufin-local-watcher-fixture".to_string())
        .spawn(move || {
            while !observer_thread_cancelled.load(Ordering::Acquire) {
                std::thread::park();
            }
            let _ = observer_completed.send(());
        })
        .expect("start observer fixture thread");
    let local_handle = runtime.spawn(std::future::pending::<()>());
    {
        let mut state = bootstrap
            .owner
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.observer = Some(ActiveObserver {
            qualifier: qualifier.clone(),
            cancelled: Arc::clone(&observer_cancelled),
            completion: Some(observer_completion),
            handle: Some(observer_handle),
        });
        state.local_access = Some(ActiveLocalAccess {
            token: 1,
            qualifier,
            cancelled: Arc::clone(&local_cancelled),
            handle: local_handle.abort_handle(),
        });
    }

    let update_cancelled = Arc::new(AtomicBool::new(false));
    let registration = bootstrap
        .owner
        .shared
        .register_interruptible(Arc::clone(&update_cancelled));
    runtime.block_on(async {
        bootstrap
            .owner
            .as_ref()
            .clone()
            .apply_source_update(
                configuration.source_id.clone(),
                SourceSettingsInput::Local {
                    roots: vec![root_b.clone()],
                },
                false,
                update_cancelled,
            )
            .await;
    });
    bootstrap
        .owner
        .shared
        .unregister_interruptible(registration.token);

    assert!(observer_cancelled.load(Ordering::Acquire));
    assert!(local_cancelled.load(Ordering::Acquire));
    assert!(
        queued_observer_work().is_none(),
        "work queued by the retired observer must not resolve the retained session"
    );
    let selected = session.resolve().expect("same selected session");
    assert_eq!(selected.source_session_epoch, SourceSessionEpoch::new(1));
    assert!(!Arc::ptr_eq(
        &source,
        selected
            .source
            .as_ref()
            .expect("replacement source executor")
    ));
    let saved = configured_source(
        &bootstrap.owner.shared.settings.load().sources,
        &configuration.source_id,
    )
    .expect("saved replacement Local source");
    assert_eq!(
        local_roots(&saved.configuration).expect("saved replacement Local roots"),
        vec![root_b.canonicalize().expect("canonical replacement root")]
    );
}

#[test]
fn cached_remote_favorite_is_accepted_and_queued_without_source_access() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("navidrome:server:favorite-offline");
    let track_id = library::TrackId::new("navidrome:track:favorite-offline");
    let library = accept_library(
        &libraries,
        source_id.clone(),
        vec![test_track(
            track_id.clone(),
            "Offline favorite",
            PathBuf::from("Offline favorite.flac"),
            None,
        )],
        Vec::new(),
        1,
    );
    let (bootstrap, events) = test_owner(
        directory.path(),
        &runtime,
        libraries,
        SettingsFile::memory(),
    );
    let session = install_selected_for_test(
        &bootstrap.owner,
        test_remote_configuration(source_id, "Offline favorite"),
        None,
        Arc::clone(&library),
        SourceSessionEpoch::new(1),
    );

    session.set_favorite(FavoriteItemId::Track(track_id.clone()), true);
    let (update, notice) = runtime.block_on(async {
        let update = events.recv().await.expect("optimistic favorite update");
        let notice = events.recv().await.expect("offline favorite notice");
        (update, notice)
    });
    assert!(matches!(
        update,
        SourceEvent::LibraryUpdate(SelectedLibraryUpdate {
            change: AcceptedLibraryChange {
                favorite: Some(library::FavoriteAcknowledgement { favorite: true, .. }),
                ..
            },
            ..
        })
    ));
    assert!(matches!(
        notice,
        SourceEvent::Notice(SourceNotice {
            kind: SourceNoticeKind::ServerUnreachable,
            ..
        })
    ));
    assert!(
        library
            .track(&track_id)
            .expect("read optimistic Track")
            .expect("optimistic Track")
            .favorite
    );
    assert_eq!(
        library
            .due_remote_favorites(i64::MAX, 10)
            .expect("read favorite outbox")
            .len(),
        1
    );
}

#[test]
fn cached_folder_and_search_work_without_source_access() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("navidrome:server:offline");
    let folder_a = MusicFolderId::new("music-folder:a");
    let folder_b = MusicFolderId::new("music-folder:b");
    let track_a = library::TrackId::new("navidrome:track:alpha");
    let track_b = library::TrackId::new("navidrome:track:beta");
    let library = accept_library(
        &libraries,
        source_id.clone(),
        vec![
            test_track(
                track_a.clone(),
                "Alpha",
                PathBuf::from("Alpha.flac"),
                Some(folder_a.clone()),
            ),
            test_track(
                track_b,
                "Beta",
                PathBuf::from("Beta.flac"),
                Some(folder_b.clone()),
            ),
        ],
        vec![
            MusicFolder {
                id: folder_a.clone(),
                name: "A".to_string(),
                image_ref: None,
            },
            MusicFolder {
                id: folder_b,
                name: "B".to_string(),
                image_ref: None,
            },
        ],
        1,
    );
    let (bootstrap, _events) = test_owner(
        directory.path(),
        &runtime,
        libraries,
        SettingsFile::memory(),
    );
    let session = install_selected_for_test(
        &bootstrap.owner,
        test_configuration(source_id, "Offline"),
        None,
        library,
        SourceSessionEpoch::new(1),
    );

    let scoped = runtime
        .block_on(session.folder(None, Some(folder_a.clone())).recv())
        .expect("folder reply")
        .expect("cached folder");
    assert_eq!(
        scoped
            .tracks
            .iter()
            .map(|track| track.id.clone())
            .collect::<Vec<_>>(),
        vec![track_a.clone()]
    );

    let stale = runtime
        .block_on(
            session
                .folder(
                    Some(library::FolderId::new("remote:folder:stale")),
                    Some(folder_a),
                )
                .recv(),
        )
        .expect("stale folder reply")
        .expect("stale folder falls back to scoped cache");
    assert_eq!(stale.tracks.len(), 1);
    assert_eq!(stale.tracks[0].id, track_a);

    let search = runtime
        .block_on(session.search(library::SearchRequest::new("Alpha")).recv())
        .expect("search reply")
        .expect("cached search");
    assert_eq!(search.tracks.len(), 1);
    assert_eq!(search.tracks[0].title, "Alpha");
}

#[test]
fn folder_and_search_fallback_only_for_outages() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let library = accept_library(
        &libraries,
        SourceId::new("navidrome:server:outage-policy"),
        vec![test_track(
            library::TrackId::new("navidrome:track:outage-policy"),
            "Cached",
            PathBuf::from("Cached.flac"),
            None,
        )],
        Vec::new(),
        1,
    );

    let network = route_folder_result(
        Arc::clone(&library),
        None,
        None,
        Some(Err(SourceError::Network("offline".to_string()))),
    )
    .expect("network folder fallback");
    assert_eq!(network.tracks.len(), 1);

    let auth = route_folder_result(
        Arc::clone(&library),
        None,
        None,
        Some(Err(SourceError::Auth("expired".to_string()))),
    )
    .expect_err("authentication errors remain visible");
    assert!(auth.contains("authentication"));

    let unavailable = route_folder_result(
        Arc::clone(&library),
        None,
        None,
        Some(Ok(NativeSourceResult::Unavailable)),
    )
    .expect("provider-unavailable folder fallback");
    assert_eq!(unavailable.tracks.len(), 1);

    let server = runtime
        .block_on(route_search_result(
            Arc::clone(&library),
            library::SearchRequest::new("Cached"),
            Some(Err(SourceError::Server {
                status: 503,
                message: "maintenance".to_string(),
            })),
        ))
        .expect("server outage search fallback");
    assert_eq!(server.tracks.len(), 1);

    let protocol = runtime
        .block_on(route_search_result(
            library,
            library::SearchRequest::new("Cached"),
            Some(Err(SourceError::Other("malformed response".to_string()))),
        ))
        .expect_err("protocol errors remain visible");
    assert!(protocol.contains("malformed response"));

    assert!(source_error_allows_cache(&SourceError::Network(
        "offline".to_string()
    )));
    assert!(source_error_allows_cache(&SourceError::Server {
        status: 500,
        message: String::new(),
    }));
    assert!(source_error_allows_cache(&SourceError::Server {
        status: 599,
        message: String::new(),
    }));
    assert!(!source_error_allows_cache(&SourceError::Server {
        status: 404,
        message: String::new(),
    }));
    assert!(!source_error_allows_cache(
        &SourceError::Auth(String::new())
    ));
}

#[test]
fn failed_target_prepare_leaves_no_selected_session() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let selected_id = SourceId::new("local:server:selected-before-failure");
    let library = accept_library(
        &libraries,
        selected_id.clone(),
        vec![test_track(
            library::TrackId::new("local:track:selected-before-failure"),
            "Selected",
            PathBuf::from("Selected.flac"),
            None,
        )],
        Vec::new(),
        1,
    );
    let target_id = SourceId::new("local:server:missing-target");
    let target = SourceConfiguration {
        source_id: target_id.clone(),
        kind: "local".to_string(),
        name: "Missing".to_string(),
        provider_payload: serde_json::json!({
            "version": 1,
            "roots": [directory.path().join("does-not-exist")],
        })
        .to_string(),
    };
    let settings = SettingsFile::memory();
    settings
        .update(|stored| {
            stored.sources.configured = vec![ConfiguredSource {
                configuration: target,
                credential_ref: None,
                music_folder_id: None,
                local_access: None,
            }];
            Ok(())
        })
        .expect("save target source");
    let (bootstrap, events) = test_owner(directory.path(), &runtime, libraries, settings);
    let selected = install_selected_for_test(
        &bootstrap.owner,
        test_configuration(selected_id, "Selected"),
        None,
        library,
        SourceSessionEpoch::new(1),
    );
    let selected_state = selected.resolve().expect("selected state");
    let retired_state = Arc::downgrade(&selected_state);
    let retired_library = Arc::downgrade(&selected_state.library);
    drop(selected_state);
    let (reply_started, reply_start) = async_channel::bounded(1);
    let (_finish_reply, reply_finish) = async_channel::bounded::<u8>(1);
    let reply = selected.spawn_reply(move |_, _| async move {
        reply_started.send(()).await.expect("report pending reply");
        reply_finish.recv().await.expect("finish pending reply")
    });

    let (failed_source, released) = runtime.block_on(async {
        reply_start.recv().await.expect("pending reply started");
        bootstrap.owner.select_source(target_id.clone());
        let mut released = false;
        let failed_source = loop {
            match events.recv().await.expect("source transition event") {
                SourceEvent::ReleaseSelected { acknowledged } => {
                    released = true;
                    acknowledged.send(()).await.expect("acknowledge release");
                }
                SourceEvent::Operation(SourceOperation::Failed { source_id, .. }) => {
                    break source_id;
                }
                _ => {}
            }
        };
        assert!(reply.recv().await.is_err());
        (failed_source, released)
    });
    assert_eq!(failed_source, Some(target_id));
    assert!(released, "source switching must release before preparation");
    assert!(selected.resolve().is_none());
    assert!(retired_state.upgrade().is_none());
    assert!(retired_library.upgrade().is_none());
}

#[test]
fn cached_local_library_is_published_before_an_unavailable_root_is_scanned() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let missing_root = directory.path().join("unavailable");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let configuration = SourceConfiguration::local(
        SourceId::new("local:server:library"),
        "Local",
        vec![missing_root.clone()],
    )
    .expect("Local configuration");
    let identity = configuration
        .input_identity()
        .expect("Local input identity");
    let track_id = library::TrackId::new("local:track:cached-before-scan");
    let mut candidate = libraries
        .begin_source_candidate(CandidateHeader {
            source_id: identity.source_id,
            input_digest: identity.digest,
        })
        .expect("begin cached Local library");
    candidate
        .write(CandidateBatch::Tracks(vec![test_track(
            track_id.clone(),
            "Cached",
            missing_root.join("Cached.flac"),
            None,
        )]))
        .expect("write cached track");
    candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|candidate| candidate.accept())
        .expect("accept cached Local library");
    let source_id = configuration.source_id.clone();
    let settings = SettingsFile::memory();
    settings
        .update(|stored| {
            stored.sources.configured = vec![ConfiguredSource {
                configuration,
                credential_ref: None,
                music_folder_id: None,
                local_access: None,
            }];
            Ok(())
        })
        .expect("save Local source");
    let (bootstrap, events) = test_owner(directory.path(), &runtime, libraries, settings);
    let playback = attach_test_playback(&bootstrap.owner, &runtime, directory.path());

    runtime.block_on(async {
        bootstrap.owner.select_source(source_id.clone());
        loop {
            match events.recv().await.expect("source selection event") {
                SourceEvent::Selected { selected, .. } if selected.source_id == source_id => {
                    assert_eq!(
                        selected
                            .library
                            .track(&track_id)
                            .expect("read cached track")
                            .expect("cached track remains available")
                            .title,
                        "Cached"
                    );
                    break;
                }
                SourceEvent::ReleaseSelected { acknowledged } => {
                    acknowledged.send(()).await.expect("acknowledge release");
                }
                SourceEvent::Operation(SourceOperation::Failed { message, .. }) => {
                    panic!("cached Local source failed before publication: {message}");
                }
                _ => {}
            }
        }
    });

    runtime.block_on(bootstrap.owner.retire_selected_access());
    let _ = playback.stop_for_source_switch();
}

#[test]
fn manual_refresh_accepts_a_nonselected_configured_local_source() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let music_root = directory.path().join("music");
    std::fs::create_dir(&music_root).expect("create Local music folder");
    write_silent_wav(&music_root.join("Background.wav")).expect("write Local Track");
    let source_id = SourceId::new("local:server:background-refresh");
    let configuration = SourceConfiguration::local(source_id.clone(), "Local", vec![music_root])
        .expect("Local configuration");
    let settings = SettingsFile::memory();
    settings
        .update(|stored| {
            stored.sources.configured = vec![ConfiguredSource {
                configuration,
                credential_ref: None,
                music_folder_id: None,
                local_access: None,
            }];
            Ok(())
        })
        .expect("save Local source");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let (bootstrap, events) = test_owner(directory.path(), &runtime, libraries.clone(), settings);

    runtime.block_on(async {
        bootstrap.owner.refresh_source(source_id.clone());
        let mut announced = false;
        loop {
            match events.recv().await.expect("manual refresh event") {
                SourceEvent::Operation(SourceOperation::Refreshing {
                    source_id: refreshing,
                    ..
                }) => {
                    assert_eq!(refreshing, source_id);
                    announced = true;
                }
                SourceEvent::Operation(SourceOperation::Idle) => break,
                SourceEvent::Operation(SourceOperation::Failed { message, .. }) => {
                    panic!("nonselected Local refresh failed: {message}");
                }
                _ => {}
            }
        }
        assert!(announced, "manual refresh must publish visible progress");
    });

    assert!(bootstrap.owner.shared.selected().is_none());
    let loaded = libraries
        .load_source(&source_id)
        .expect("load refreshed Local source")
        .expect("refreshed Local source");
    assert_eq!(
        loaded
            .track_list(None, TrackSort::Title, false)
            .expect("read refreshed Tracks")
            .len(),
        1
    );
}

#[test]
fn completed_source_switch_releases_the_previous_state_and_library() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let previous_root = directory.path().join("previous");
    let target_root = directory.path().join("target");
    std::fs::create_dir(&previous_root).expect("create previous Local folder");
    std::fs::create_dir(&target_root).expect("create target Local folder");
    let runtime = test_runtime();
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let connected = runtime
        .block_on(Source::connect(SourceSetupInput::Local(
            LocalFolderHostInput {
                roots: vec![previous_root],
            },
        )))
        .expect("connect previous Local source");
    let (previous_configuration, previous_source, credential) = connected.into_parts();
    assert_eq!(credential, None);
    let previous_source = Arc::new(previous_source);
    let previous_id = previous_configuration.source_id.clone();
    let target_id = SourceId::new("local:server:target-release");
    let target_configuration = SourceConfiguration {
        source_id: target_id.clone(),
        kind: "local".to_string(),
        name: "Target".to_string(),
        provider_payload: serde_json::json!({
            "version": 1,
            "roots": [target_root],
        })
        .to_string(),
    };
    let previous_library = accept_library(
        &libraries,
        previous_id.clone(),
        vec![test_track(
            library::TrackId::new("local:track:previous-release"),
            "Previous",
            PathBuf::from("Previous.flac"),
            None,
        )],
        Vec::new(),
        1,
    );
    let target_probe = libraries.clone();
    let settings = SettingsFile::memory();
    settings
        .update(|stored| {
            stored.sources.configured = vec![
                ConfiguredSource {
                    configuration: previous_configuration.clone(),
                    credential_ref: None,
                    music_folder_id: None,
                    local_access: None,
                },
                ConfiguredSource {
                    configuration: target_configuration,
                    credential_ref: None,
                    music_folder_id: None,
                    local_access: None,
                },
            ];
            stored.sources.selected_source_id = Some(previous_id.clone());
            Ok(())
        })
        .expect("save configured sources");
    let (bootstrap, events) = test_owner(directory.path(), &runtime, libraries, settings);
    let playback = attach_test_playback(&bootstrap.owner, &runtime, directory.path());
    let previous_session = install_selected_for_test(
        &bootstrap.owner,
        previous_configuration,
        Some(Arc::clone(&previous_source)),
        Arc::clone(&previous_library),
        SourceSessionEpoch::new(1),
    );
    let prepared = playback
        .prepare_selected(
            Arc::clone(&previous_session),
            previous_session.resolve().expect("previous selected state"),
        )
        .expect("prepare previous Playback");
    let cutover = playback.stop_for_source_switch();
    let _ = playback.install_prepared(prepared, cutover);
    let previous_state = previous_session.resolve().expect("previous selected state");
    let retired_state = Arc::downgrade(&previous_state);
    let retired_library = Arc::downgrade(&previous_library);
    let retired_source = Arc::downgrade(&previous_source);
    let reply_source = previous_state.source.clone().expect("previous Source");
    drop(previous_state);
    drop(previous_library);
    drop(previous_source);
    let (reply_started, reply_start) = async_channel::bounded(1);
    let reply = previous_session.spawn_reply(move |_, _| async move {
        let source = reply_source;
        reply_started.send(()).await.expect("report pending reply");
        std::future::pending::<()>().await;
        drop(source);
    });

    runtime.block_on(async {
        reply_start.recv().await.expect("pending reply started");
        bootstrap.owner.select_source(target_id.clone());
        let mut release_requested = false;
        loop {
            let event = events.recv().await.expect("source switch event");
            match event {
                SourceEvent::ReleaseSelected { acknowledged } => {
                    release_requested = true;
                    assert!(retired_state.upgrade().is_some());
                    assert!(retired_library.upgrade().is_some());
                    assert!(retired_source.upgrade().is_some());
                    assert!(
                        target_probe
                            .load_source(&target_id)
                            .expect("inspect target Library")
                            .is_none(),
                        "the target Library must not be built before releasing the previous source"
                    );
                    acknowledged.send(()).await.expect("acknowledge release");
                }
                SourceEvent::Selected { selected, .. } if selected.source_id == target_id => {
                    assert!(release_requested);
                    break;
                }
                SourceEvent::Operation(SourceOperation::Failed { message, .. }) => {
                    panic!("source switch failed: {message}");
                }
                _ => {}
            }
        }
        assert!(reply.recv().await.is_err());
    });

    assert!(previous_session.resolve().is_none());
    assert!(retired_state.upgrade().is_none());
    assert!(retired_library.upgrade().is_none());
    assert!(retired_source.upgrade().is_none());
    runtime.block_on(bootstrap.owner.retire_selected_access());
    let _ = playback.stop_for_source_switch();
}

#[test]
fn activity_publishes_while_candidate_acquisition_is_blocked_and_rebases_once() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:activity-refresh");
    let track_id = library::TrackId::new("local:track:activity-refresh");
    let libraries = Libraries::open(&path).expect("open Library");
    let initial = accept_library(
        &libraries,
        source_id.clone(),
        vec![test_track(
            track_id.clone(),
            "Before Refresh",
            directory.path().join("Track.flac"),
            None,
        )],
        Vec::new(),
        1,
    );
    let smart_playlist_id = initial
        .create_smart_playlist(
            "Played".to_string(),
            library::SmartPlaylistDefinition {
                match_all: vec![library::SmartPlaylistRule {
                    field: library::SmartPlaylistRuleField::PlayCount,
                    operator: library::SmartPlaylistRuleOperator::Above,
                    value: Some(library::SmartPlaylistRuleValue::Number(0)),
                }],
                match_any: Vec::new(),
                sort_field: library::SmartPlaylistSortField::PlayCount,
                descending: true,
                limit: None,
            },
        )
        .expect("create activity smart playlist")
        .expect("new activity smart playlist")
        .smart_playlists
        .into_iter()
        .next()
        .expect("created activity smart playlist ID");
    let mut replacement = libraries
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: [2; 32],
        })
        .expect("begin replacement candidate");
    replacement
        .write(CandidateBatch::Tracks(vec![test_track(
            track_id.clone(),
            "After Refresh",
            directory.path().join("Track.flac"),
            None,
        )]))
        .expect("write replacement Track");
    let replacement = replacement
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 2,
            },
            Some(&initial),
        )
        .expect("prepare replacement source");

    let runtime = test_runtime();
    let (bootstrap, events) = test_owner(
        directory.path(),
        &runtime,
        libraries.clone(),
        SettingsFile::memory(),
    );
    let epoch = SourceSessionEpoch::new(1);
    let session = install_selected_for_test(
        &bootstrap.owner,
        test_configuration(source_id.clone(), "Activity refresh"),
        None,
        Arc::clone(&initial),
        epoch,
    );
    let (started, candidate_started) = async_channel::bounded(1);
    let (resume, candidate_resume) = async_channel::bounded(1);
    let (accepted, candidate_accepted) = async_channel::bounded(1);
    bootstrap
        .owner
        .spawn_serialized(false, move |operations, _| async move {
            started
                .send(())
                .await
                .expect("signal candidate acquisition");
            candidate_resume
                .recv()
                .await
                .expect("finish candidate acquisition");
            let acceptance_owner = Arc::clone(&operations.shared);
            let _acceptance = acceptance_owner.acceptance_lane.lock().await;
            let result = replacement
                .accept()
                .map_err(string_error)
                .and_then(|commit| {
                    let current = operations
                        .shared
                        .selected()
                        .ok_or_else(|| "the selected source was retired".to_string())?;
                    let home = commit.library.home(None).map_err(string_error)?;
                    let library = Arc::clone(&commit.library);
                    let mut next = (*current).clone();
                    next.library = commit.library;
                    next.home = home;
                    operations
                        .shared
                        .replace_selected(next)
                        .then_some(library)
                        .ok_or_else(|| "the selected source changed".to_string())
                });
            accepted
                .send(result)
                .await
                .expect("report candidate acceptance");
        });
    let replacement = runtime.block_on(async {
        candidate_started
            .recv()
            .await
            .expect("candidate acquisition started");
        let activity = initial
            .record_play(AcceptedPlay {
                play_id: "refresh-play".to_string(),
                track_id: track_id.clone(),
                played_at: 1_700_000_000,
                month: "2023-11".to_string(),
            })
            .expect("record play during refresh")
            .expect("new play during refresh");
        bootstrap
            .owner
            .publish_activity(source_id.clone(), epoch, activity);
        let publication = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let SourceEvent::LibraryUpdate(update) =
                    events.recv().await.expect("activity publication")
                {
                    break update;
                }
            }
        })
        .await
        .expect("activity must publish while candidate acquisition is blocked");
        assert_eq!(
            publication.change.smart_playlists.as_slice(),
            std::slice::from_ref(&smart_playlist_id)
        );
        assert!(
            publication.home.is_some(),
            "accepted play must publish Home"
        );
        let current = session.resolve().expect("current selected source");
        assert!(Arc::ptr_eq(&current.library, &initial));
        assert_eq!(
            current
                .library
                .track(&track_id)
                .expect("read current Track")
                .expect("current Track")
                .play_count,
            Some(1)
        );
        assert_eq!(
            current
                .library
                .smart_playlist_detail(&smart_playlist_id, None)
                .expect("read current activity smart playlist")
                .expect("current activity smart playlist")
                .tracks
                .len(),
            1
        );
        resume.send(()).await.expect("finish candidate acquisition");
        let replacement = candidate_accepted
            .recv()
            .await
            .expect("candidate acceptance result")
            .expect("accept replacement source");
        let lane = Arc::clone(&bootstrap.owner.shared);
        let _finished = lane.lane.lock().await;
        replacement
    });
    assert_eq!(
        replacement
            .track(&track_id)
            .expect("read replacement Track")
            .expect("replacement Track")
            .play_count,
        Some(1)
    );
    assert_eq!(
        replacement
            .history_track_list(None)
            .expect("read replacement History")
            .len(),
        1
    );
    assert_eq!(
        replacement
            .smart_playlist_detail(&smart_playlist_id, None)
            .expect("read replacement activity smart playlist")
            .expect("replacement activity smart playlist")
            .tracks
            .len(),
        1
    );

    drop(replacement);
    drop(session);
    drop(bootstrap);
    drop(initial);
    drop(libraries);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load replacement source")
        .expect("replacement source");
    assert_eq!(
        reopened
            .track(&track_id)
            .expect("read reopened Track")
            .expect("reopened Track")
            .play_count,
        Some(1)
    );
    assert_eq!(
        reopened
            .history_track_list(None)
            .expect("read reopened History")
            .len(),
        1
    );
    assert_eq!(
        reopened
            .smart_playlist_detail(&smart_playlist_id, None)
            .expect("read reopened activity smart playlist")
            .expect("reopened activity smart playlist")
            .tracks
            .len(),
        1
    );
}

#[test]
fn local_file_change_updates_only_the_changed_component() {
    let directory = tempfile::tempdir().expect("temporary Local source");
    let music_root = directory.path().join("music");
    std::fs::create_dir(&music_root).expect("create Local music folder");
    let music_root = std::fs::canonicalize(music_root).expect("canonical Local music folder");
    write_silent_wav(&music_root.join("First.wav")).expect("write first Local Track");
    let other_directory = music_root.join("Other");
    std::fs::create_dir(&other_directory).expect("create unrelated Local directory");
    write_silent_wav(&other_directory.join("Outside.wav")).expect("write unrelated Local Track");
    let runtime = test_runtime();
    let connected = runtime
        .block_on(Source::connect(SourceSetupInput::Local(
            LocalFolderHostInput {
                roots: vec![music_root.clone()],
            },
        )))
        .expect("open Local source");
    let (configuration, source, credential) = connected.into_parts();
    assert_eq!(credential, None);
    let identity = configuration.input_identity().expect("source identity");
    let source = Arc::new(source);
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let accepted = runtime
        .block_on(Arc::clone(&source).prepare_library_candidate(
            libraries,
            identity,
            None,
            Arc::new(|_: SourceReadProgress| {}),
            Arc::new(AtomicBool::new(false)),
        ))
        .map_err(string_error)
        .and_then(|candidate| candidate.accept().map_err(string_error))
        .expect("accept initial Local source")
        .library;
    assert_eq!(
        accepted
            .track_list(None, TrackSort::Title, false)
            .expect("read initial Tracks")
            .len(),
        2
    );

    let second_path = music_root.join("Second.wav");
    write_silent_wav(&second_path).expect("write changed Local Track");
    let prepared = runtime
        .block_on(source.prepare_change(
            Arc::clone(&accepted),
            ObservedSourceChange::LocalPaths(BTreeSet::from([second_path])),
            Arc::new(|_: SourceReadProgress| {}),
            Arc::new(AtomicBool::new(false)),
        ))
        .expect("read changed Local component");
    let PreparedSourceChange::LocalReplacement(replacement) = prepared else {
        panic!("changed Local path did not produce an exact component");
    };
    assert_eq!(replacement.tracks.len(), 1);
    assert!(
        replacement
            .tracks
            .iter()
            .any(|track| track.title == "Second")
    );
    assert!(
        replacement
            .tracks
            .iter()
            .all(|track| track.title != "Outside")
    );
    let changed = accepted
        .accept_local_component(replacement)
        .expect("accept changed Local component")
        .expect("changed Local component");
    assert!(changed.tracks.iter().any(|replacement| {
        replacement
            .track
            .as_ref()
            .is_some_and(|track| track.title == "Second")
    }));
    assert_eq!(
        accepted
            .track_list(None, TrackSort::Title, false)
            .expect("read changed Tracks")
            .len(),
        3
    );

    let unchanged = runtime
        .block_on(source.prepare_change(
            accepted,
            ObservedSourceChange::LocalRescan,
            Arc::new(|_: SourceReadProgress| {}),
            Arc::new(AtomicBool::new(false)),
        ))
        .expect("verify unchanged Local source");
    assert!(matches!(unchanged, PreparedSourceChange::Ignored));
}

#[test]
fn local_metadata_edit_prepares_the_written_file_for_library_acceptance() {
    let directory = tempfile::tempdir().expect("temporary Local source");
    let music_root = directory.path().join("music");
    std::fs::create_dir(&music_root).expect("create Local music folder");
    let path = music_root.join("Before.wav");
    write_silent_wav(&path).expect("write WAV");
    let runtime = test_runtime();
    let connected = runtime
        .block_on(Source::connect(SourceSetupInput::Local(
            LocalFolderHostInput {
                roots: vec![music_root],
            },
        )))
        .expect("open Local source");
    let (configuration, source, _) = connected.into_parts();
    let identity = configuration.input_identity().expect("source identity");
    let source = Arc::new(source);
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let accepted = runtime
        .block_on(Arc::clone(&source).prepare_library_candidate(
            libraries,
            identity,
            None,
            Arc::new(|_: SourceReadProgress| {}),
            Arc::new(AtomicBool::new(false)),
        ))
        .map_err(string_error)
        .and_then(|candidate| candidate.accept().map_err(string_error))
        .expect("accept initial Local source")
        .library;
    let edited_track = accepted
        .track_list(None, TrackSort::Title, false)
        .expect("read initial Tracks")
        .track(0)
        .expect("resolve initial Track")
        .expect("initial Track");
    let draft = runtime
        .block_on(source.read_metadata(library::MetadataSubject::track(edited_track.clone()), None))
        .expect("read metadata draft");
    let refresh = runtime
        .block_on(source.write_metadata(
            Arc::clone(&accepted),
            library::MetadataSubject::track(edited_track.clone()),
            MetadataEdit {
                item_id: MetadataItemId::Track(edited_track.id.clone()),
                revision: draft.revision,
                application: None,
                changes: vec![MetadataChange::Title("After".to_string())],
            },
            None,
            Arc::new(|_: SourceReadProgress| {}),
            Arc::new(AtomicBool::new(false)),
        ))
        .expect("write Local metadata");
    let PreparedSourceChange::LocalReplacement(replacement) = refresh else {
        panic!("Local metadata write did not prepare an exact replacement");
    };
    let change = accepted
        .accept_local_component(replacement)
        .expect("accept metadata component")
        .expect("changed metadata component");

    assert!(change.tracks.iter().any(|replacement| {
        replacement
            .track
            .as_ref()
            .is_some_and(|track| track.id == edited_track.id && track.title == "After")
    }));
    assert_eq!(
        accepted
            .track(&edited_track.id)
            .expect("read accepted Track")
            .expect("accepted Track")
            .title,
        "After"
    );
}

#[test]
fn private_mode_still_uses_source_metadata_search() {
    let runtime = test_runtime();
    let editing = MetadataEditing::new(vec![library::MetadataField::Title]);
    let current = library::MetadataValues {
        title: "Current".to_string(),
        ..library::MetadataValues::default()
    };
    let source_candidate = library::MetadataValues {
        title: "Source candidate".to_string(),
        ..library::MetadataValues::default()
    };

    let identified = runtime
        .block_on(resolve_identification(
            false,
            true,
            &editing,
            &current,
            async { panic!("private mode must not poll direct MusicBrainz lookup") },
            async {
                Ok(Some(library::MetadataIdentification::values(
                    source_candidate,
                )))
            },
        ))
        .expect("source metadata search")
        .expect("source metadata search candidate");
    assert_eq!(identified.values.title, "Source candidate");
}

#[test]
fn source_metadata_candidate_short_circuits_direct_lookup() {
    let runtime = test_runtime();
    let editing = MetadataEditing::new(vec![library::MetadataField::Title]);
    let current = library::MetadataValues {
        title: "Current".to_string(),
        ..library::MetadataValues::default()
    };
    let source_candidate = library::MetadataValues {
        title: "Source candidate".to_string(),
        ..library::MetadataValues::default()
    };

    let identified = runtime
        .block_on(resolve_identification(
            true,
            true,
            &editing,
            &current,
            async { panic!("an applicable source candidate must not poll direct MusicBrainz") },
            async {
                Ok(Some(library::MetadataIdentification::values(
                    source_candidate,
                )))
            },
        ))
        .expect("source identification")
        .expect("source candidate");
    assert_eq!(identified.values.title, "Source candidate");
}

#[test]
fn source_miss_or_unchanged_candidate_falls_back_once() {
    let runtime = test_runtime();
    let editing = MetadataEditing::new(vec![library::MetadataField::Title]);
    let current = library::MetadataValues {
        title: "Current".to_string(),
        ..library::MetadataValues::default()
    };
    let direct_candidate = || {
        library::MetadataIdentification::values(library::MetadataValues {
            title: "Direct candidate".to_string(),
            ..library::MetadataValues::default()
        })
    };

    let identified = runtime
        .block_on(resolve_identification(
            true,
            true,
            &editing,
            &current,
            async { Ok(Some(direct_candidate())) },
            async { Ok(None) },
        ))
        .expect("direct fallback after source miss")
        .expect("direct fallback candidate");
    assert_eq!(identified.values.title, "Direct candidate");

    let identified = runtime
        .block_on(resolve_identification(
            true,
            true,
            &editing,
            &current,
            async { Ok(Some(direct_candidate())) },
            async {
                Ok(Some(library::MetadataIdentification::values(
                    current.clone(),
                )))
            },
        ))
        .expect("direct fallback after unchanged source candidate")
        .expect("direct fallback candidate");
    assert_eq!(identified.values.title, "Direct candidate");
}

#[test]
fn metadata_identification_failure_arbitration_uses_the_applicable_request() {
    let runtime = test_runtime();
    let editing = MetadataEditing::new(vec![library::MetadataField::Title]);
    let current = library::MetadataValues {
        title: "Current".to_string(),
        ..library::MetadataValues::default()
    };

    let identified = runtime
        .block_on(resolve_identification(
            true,
            true,
            &editing,
            &current,
            async { Err("MusicBrainz request failed".to_string()) },
            async { Ok(None) },
        ))
        .expect("successful source miss suppresses a direct failure");
    assert_eq!(identified, None);

    let error = runtime
        .block_on(resolve_identification(
            true,
            true,
            &editing,
            &current,
            async { Err("MusicBrainz request failed".to_string()) },
            async { Err("Jellyfin request failed".to_string()) },
        ))
        .expect_err("native failure wins");
    assert_eq!(error, "Jellyfin request failed");

    let error = runtime
        .block_on(resolve_identification(
            true,
            false,
            &editing,
            &current,
            async { Err("MusicBrainz request failed".to_string()) },
            async { panic!("unsupported native search must not be polled") },
        ))
        .expect_err("direct-only failure remains visible");
    assert_eq!(error, "MusicBrainz request failed");

    let identified = runtime
        .block_on(resolve_identification(
            false,
            false,
            &editing,
            &current,
            async { panic!("inapplicable direct search must not be polled") },
            async { panic!("inapplicable native search must not be polled") },
        ))
        .expect("no applicable lookup is silent");
    assert_eq!(identified, None);
}

#[test]
fn failed_metadata_access_setting_save_restores_the_accepted_mapping() {
    let directory = tempfile::tempdir().expect("temporary Local access transaction");
    let store_path = directory.path().join("library.db");
    let libraries = Libraries::open(&store_path).expect("open Library");
    let source_id = SourceId::new("navidrome:server:local-access-transaction");
    let track_id = library::TrackId::new("navidrome:track:local-access-transaction");
    let library = accept_library(
        &libraries,
        source_id.clone(),
        vec![test_track(
            track_id.clone(),
            "Track",
            PathBuf::from("/server/music/Artist/Track.wav"),
            None,
        )],
        Vec::new(),
        1,
    );
    let previous_root = directory.path().join("previous");
    let previous_path = previous_root.join("Artist/Track.wav");
    let previous_access = library::LocalAccessMapping {
        root_path: previous_root.clone(),
        server_prefix: Some("/server/music".to_string()),
        local_prefix: Some(previous_root.to_string_lossy().into_owned()),
    };
    let previous_files = vec![library::LocalAccessFile {
        path: previous_path.to_string_lossy().into_owned(),
        root: previous_root.to_string_lossy().into_owned(),
        relative_path: "Artist/Track.wav".to_string(),
        size_bytes: 1,
        mtime_ns: 1,
        device_id: None,
        inode: None,
        parser_version: 1,
        title: "Track".to_string(),
        album: String::new(),
        artist: "Artist".to_string(),
        disc_number: 1,
        track_number: 1,
        duration_seconds: 180,
    }];
    library
        .replace_local_access(previous_access.clone(), previous_files.clone())
        .expect("accept previous Local access");

    let proposed_root = directory.path().join("proposed");
    let error = accept_metadata_local_access_mapping(
        &library,
        library::LocalAccessMapping {
            root_path: proposed_root.clone(),
            server_prefix: Some("/server/music".to_string()),
            local_prefix: Some(proposed_root.to_string_lossy().into_owned()),
        },
        Some(previous_access),
        || Err("settings write failed".to_string()),
    )
    .expect_err("failed Settings save rolls back Local access");

    assert_eq!(error, "settings write failed");
    assert_eq!(
        library
            .local_access_files()
            .expect("read restored Local access"),
        previous_files
    );
    let (_, targets) = library
        .metadata_subject_with_local_access(&MetadataItemId::Track(track_id), None)
        .expect("resolve restored Local access")
        .expect("restored metadata Track");
    assert_eq!(
        targets
            .first()
            .expect("previous Local access remains accepted")
            .path(),
        previous_path
    );

    drop(library);
    drop(libraries);
    let reopened = Libraries::open(store_path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load restored source")
        .expect("restored source");
    assert_eq!(
        reopened
            .local_access_files()
            .expect("read durable restored Local access"),
        previous_files
    );
}

#[test]
fn standardized_results_reuse_accepted_track_facts_without_a_source_mirror() {
    let directory = tempfile::tempdir().expect("temporary Rufin data directory");
    let libraries = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let track_id = library::TrackId::new("navidrome:track:known");
    let accepted_track = test_track(
        track_id.clone(),
        "Accepted",
        PathBuf::from("/music/Artist/Accepted.flac"),
        None,
    );
    let library = accept_library(
        &libraries,
        SourceId::new("navidrome:server:test"),
        vec![accepted_track],
        Vec::new(),
        1,
    );
    let reported = test_track(
        track_id,
        "Reported",
        PathBuf::from("generated/Reported.flac"),
        None,
    );
    let unknown = test_track(
        library::TrackId::new("navidrome:track:unknown"),
        "Unknown",
        PathBuf::from("generated/Unknown.flac"),
        None,
    );

    let search = hydrate_search_tracks(
        &library,
        library::SearchResults {
            tracks: vec![reported.clone(), unknown.clone()],
            ..library::SearchResults::default()
        },
    )
    .expect("reconcile search");
    assert_eq!(search.tracks[0].title, "Accepted");
    assert_eq!(
        search.tracks[0].source_path.as_deref(),
        Some("/music/Artist/Accepted.flac")
    );
    assert_eq!(search.tracks[1], unknown);

    let folder = reconcile_folder_contents(
        &library,
        FolderContents {
            folders: Arc::from([]),
            tracks: vec![reported].into(),
        },
    )
    .expect("reconcile folder");
    assert_eq!(folder.tracks[0].title, "Accepted");
}

fn test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("build test runtime")
}

fn test_owner(
    directory: &Path,
    runtime: &tokio::runtime::Runtime,
    libraries: Libraries,
    settings: SettingsFile,
) -> (SourceBootstrap, async_channel::Receiver<SourceEvent>) {
    let (bootstrap, events, _download_events) =
        test_owner_with_download_events(directory, runtime, libraries, settings);
    (bootstrap, events)
}

fn test_owner_with_download_events(
    directory: &Path,
    runtime: &tokio::runtime::Runtime,
    libraries: Libraries,
    settings: SettingsFile,
) -> (
    SourceBootstrap,
    async_channel::Receiver<SourceEvent>,
    async_channel::Receiver<downloads::DownloadEvent>,
) {
    let artwork = artwork::Artwork::new(directory.join("artwork"), runtime.handle().clone())
        .expect("open Artwork");
    let (events, event_receiver) = async_channel::unbounded();
    let (discovery, _discovery_receiver) = async_channel::unbounded();
    let (download_events, download_event_receiver) = async_channel::unbounded();
    let secrets = Arc::new(SwitchableSecretStore::new(Arc::new(
        MemorySecretStore::new(),
    )));
    let scrobbler = Arc::new(
        Scrobbler::new(libraries.clone(), ::scrobbling::Settings::default(), false)
            .expect("open Scrobbler"),
    );
    let bootstrap = SourceOwner::open_dormant(
        artwork,
        libraries,
        downloads::Downloads::new(
            directory.join("downloads"),
            runtime.handle().clone(),
            download_events,
            Vec::new(),
        ),
        settings,
        secrets,
        scrobbler,
        runtime.handle().clone(),
        SourceOutputs { events, discovery },
    );
    (bootstrap, event_receiver, download_event_receiver)
}

#[derive(Default)]
struct AcceptingPlaybackBackend;

impl ::playback::PlaybackBackend for AcceptingPlaybackBackend {
    fn send(
        &mut self,
        _command: ::playback::BackendCommand,
    ) -> Result<(), ::playback::BackendError> {
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<::playback::BackendEvent> {
        Vec::new()
    }
}

fn attach_test_playback(
    owner: &Arc<SourceOwner>,
    runtime: &tokio::runtime::Runtime,
    directory: &Path,
) -> Arc<PlaybackOwner> {
    let (playback_events, _playback_event_receiver) = async_channel::unbounded();
    let (waveform_events, _waveform_event_receiver) = async_channel::unbounded();
    let waveform = crate::waveform::WaveformOwner::new(
        runtime.handle().clone(),
        waveform_events,
        directory.join("waveforms"),
        false,
    );
    let (lyrics_events, _lyrics_event_receiver) = async_channel::unbounded();
    let stored = owner.shared.settings.load();
    let lyrics = ::lyrics::LyricsService::new(
        owner.shared.library.clone(),
        runtime.handle().clone(),
        stored.ui.lyrics,
        stored.ui.private_mode,
        lyrics_events,
    );
    let playback = PlaybackOwner::new(
        owner.shared.library.clone(),
        owner.shared.settings.clone(),
        runtime.handle().clone(),
        playback_events,
        owner.acceptance_sender(),
        artwork::Artwork::new(directory.join("artwork"), runtime.handle().clone())
            .expect("artwork service"),
        waveform,
        lyrics,
        Arc::new(desktop_integration::Discord::new()),
        Arc::clone(&owner.shared.scrobbler),
        || Ok(Box::<AcceptingPlaybackBackend>::default()),
    );
    owner.attach_playback(&playback);
    playback
}

fn install_selected_for_test(
    owner: &Arc<SourceOwner>,
    configuration: SourceConfiguration,
    source: Option<Arc<Source>>,
    library: Arc<Library>,
    epoch: SourceSessionEpoch,
) -> Arc<ActiveSource> {
    let home = library.home(None).expect("prepare selected Home");
    let selected = Arc::new(SelectedSourceState {
        configuration,
        source,
        source_session_epoch: epoch,
        library,
        home,
        music_folder_id: None,
    });
    let session = ActiveSource::new(&owner.shared, &selected);
    owner
        .shared
        .install_selected_slot(Arc::clone(&session), selected);
    session
}

fn accept_library(
    libraries: &Libraries,
    source_id: SourceId,
    tracks: Vec<Track>,
    music_folders: Vec<MusicFolder>,
    digest: u8,
) -> Arc<Library> {
    let mut candidate = libraries
        .begin_source_candidate(CandidateHeader {
            source_id,
            input_digest: [digest; 32],
        })
        .expect("begin source candidate");
    if !tracks.is_empty() {
        candidate
            .write(CandidateBatch::Tracks(tracks))
            .expect("write candidate Tracks");
    }
    if !music_folders.is_empty() {
        candidate
            .write(CandidateBatch::MusicFolders(music_folders))
            .expect("write candidate music folders");
    }
    candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: i64::from(digest),
            },
            None,
        )
        .and_then(|candidate| candidate.accept())
        .expect("accept source candidate")
        .library
}

fn test_configuration(source_id: SourceId, name: &str) -> SourceConfiguration {
    SourceConfiguration {
        source_id,
        kind: "local".to_string(),
        name: name.to_string(),
        provider_payload: serde_json::json!({
            "version": 1,
            "roots": [],
        })
        .to_string(),
    }
}

fn test_remote_configuration(source_id: SourceId, name: &str) -> SourceConfiguration {
    let mut configuration = test_configuration(source_id, name);
    configuration.kind = "navidrome".to_string();
    configuration
}

fn test_track(
    id: library::TrackId,
    title: &str,
    path: PathBuf,
    music_folder: Option<MusicFolderId>,
) -> Track {
    Track::new(TrackData {
        id,
        album_id: None,
        title: title.to_string(),
        artist: "Artist".to_string(),
        album: String::new(),
        album_artwork: None,
        year: 2024,
        release_date: None,
        date_added: None,
        last_played: None,
        play_count: None,
        user_rating: None,
        duration_seconds: 180,
        favorite: false,
        disc_number: 1,
        track_number: 1,
        image_ref: None,
        local_artwork: None,
        musicbrainz_recording_id: None,
        musicbrainz_release_track_id: None,
        source_path: Some(path.to_string_lossy().into_owned()),
        cue: None,
        source_format: Some("flac".to_string()),
        comment: None,
        skip_count: None,
        bpm: None,
        relations: TrackRelations {
            music_folders: music_folder.into_iter().collect(),
            ..TrackRelations::default()
        },
    })
}

fn write_silent_wav(path: &Path) -> std::io::Result<()> {
    let sample_rate = 8_000_u32;
    let bits_per_sample = 16_u16;
    let channels = 1_u16;
    let data_len = sample_rate * u32::from(channels) * u32::from(bits_per_sample / 8);
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample / 8);
    let block_align = channels * (bits_per_sample / 8);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(bytes.len() + data_len as usize, 0);
    std::fs::write(path, bytes)
}
