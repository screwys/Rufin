use super::*;
use crate::{app, diagnostics::Diagnostics, paths::Paths, runtime::RuntimeInputs};
use playback::{BackendCommand, BackendError, BackendEvent, PlaybackBackend};
use sources::DownloadQuality;

fn diagnostics() -> Arc<Diagnostics> {
    static DIAGNOSTICS: std::sync::OnceLock<(
        Arc<Diagnostics>,
        Mutex<crate::diagnostics::StderrGuard>,
        tempfile::TempDir,
    )> = std::sync::OnceLock::new();
    DIAGNOSTICS
        .get_or_init(|| {
            let directory = tempfile::tempdir().unwrap();
            let (diagnostics, stderr) = Diagnostics::install(directory.path().to_owned());
            (diagnostics, Mutex::new(stderr), directory)
        })
        .0
        .clone()
}

#[test]
fn saved_settings_enable_only_remembered_legacy_profiles() {
    assert!(!ConnectSettings::default().enabled);
    let default_name = if cfg!(target_os = "android") {
        "Rufin Android"
    } else {
        "Rufin Desktop"
    };
    assert_eq!(ConnectSettings::default().name, default_name);
    for (saved, expected) in [
        (r#"{}"#, false),
        (r#"{"name":"Work Laptop"}"#, false),
        (r#"{"profile":null}"#, false),
        (r#"{"profile":"shared-profile"}"#, true),
        (r#"{"profile":"shared-profile","enabled":false}"#, false),
        (r#"{"enabled":true}"#, true),
    ] {
        let (config, encoding) = ConnectSettings::from_device_file(saved.as_bytes()).unwrap();
        assert_eq!(config.enabled, expected, "{saved}");
        assert_eq!(encoding, None, "{saved}");
        assert_eq!(
            config.name,
            if saved.contains("Work Laptop") {
                "Work Laptop"
            } else {
                default_name
            }
        );
    }
    for saved in [
        DownloadQuality::Original,
        DownloadQuality::Transcoded(library::AudioEncoding::mp3()),
    ] {
        let bytes = serde_json::to_vec(&serde_json::json!({"encoding": saved})).unwrap();
        assert_eq!(
            ConnectSettings::from_device_file(&bytes).unwrap().1,
            Some(saved)
        );
    }
    assert_eq!(
        ConnectSettings::from_device_file(br#"{"encoding":"mp3"}"#)
            .unwrap()
            .1,
        Some(DownloadQuality::Transcoded(library::AudioEncoding::mp3()))
    );
    assert_eq!(
        ConnectSettings::from_device_file(br#"{"name":"Rufin"}"#)
            .unwrap()
            .0
            .name,
        "Rufin"
    );
}

struct IdleBackend;

#[test]
fn cue_direct_continuation_streams_without_enrollment() {
    let root = tempfile::tempdir().unwrap();
    app::with_runtime(|runtime| {
        runtime.block_on(Box::pin(async {
            let host = device(
                root.path(),
                "direct-host",
                diagnostics(),
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let guest = device(
                root.path(),
                "direct-guest",
                diagnostics(),
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let a = &host.products.connect;
            let b = &guest.products.connect;
            execute(a, Action::Create).await.unwrap();
            pair(a, b).await;
            let source = root.path().join("whole.flac");
            std::fs::write(&source, b"complete backing audio").unwrap();
            let remote = url::Url::from_file_path(&source).unwrap().to_string();
            let mut files = Vec::new();
            for index in 0..2 {
                let uri = library::cue_media_uri(
                    &format!("direct:{index}"),
                    &remote,
                    index * 1000,
                    (index + 1) * 1000,
                );
                let mut item =
                    library::TrackReference::direct(&uri, "Cue track", "Artist", "Album", 1000);
                item.metadata.source_format = Some("flac".into());
                host.products
                    .playback
                    .play_selection(crate::playback::PlayRequest::one(
                        item.clone(),
                        library::QueuePlacement::Now,
                    ));
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let queue = host.products.playback.clone();
                        let saved = tokio::task::spawn_blocking(move || queue.snapshot())
                            .await
                            .unwrap()
                            .unwrap();
                        if saved.entries.first().map(|entry| entry.track_key)
                            == a.database
                                .catalog_identity(
                                    library::CatalogKind::Track,
                                    &uri,
                                    &library::ReadCancellation::new(),
                                )
                                .await
                                .map(|identity| {
                                    identity
                                        .map(|identity| library::TrackKey::from_raw(identity.key))
                                })
                                .unwrap()
                            && !saved.entries.is_empty()
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                b.database
                    .remember_media_uris(std::slice::from_ref(&uri))
                    .await
                    .unwrap();
                let mut entries = b
                    .database
                    .queue_entries(
                        library::QueueLoad::media_uris(
                            vec![uri.clone()],
                            library::QueueProvenance::Manual,
                        ),
                        None,
                    )
                    .await
                    .unwrap()
                    .entries;
                for entry in &mut entries {
                    entry.received_from_connect = true;
                }
                let rows = b
                    .database
                    .queue_rows_for_entries(
                        &entries.into_iter().enumerate().collect::<Vec<_>>(),
                        &library::ReadCancellation::new(),
                    )
                    .await
                    .unwrap();
                let stream = crate::playback::prepare_stream(
                    &b.database,
                    playback::StreamRequest::original(&uri),
                    None,
                    Some((b.as_ref(), rows[0].as_ref())),
                    |_| async { panic!("peer file") },
                )
                .await
                .unwrap();
                let response = reqwest::get(stream.uri()).await.unwrap();
                assert!(response.status().is_success());
                assert_eq!(
                    response.bytes().await.unwrap().as_ref(),
                    b"complete backing audio"
                );
                assert!(
                    b.database
                        .connect_track_reference(&uri)
                        .await
                        .unwrap()
                        .is_none()
                );
                assert!(
                    b.database
                        .connect_media_file(&uri, "original")
                        .await
                        .unwrap()
                        .is_none()
                );
                assert_eq!(stream.window().unwrap().start_millis, index as u64 * 1000);
                assert_eq!(
                    stream.window().unwrap().end_millis,
                    (index as u64 + 1) * 1000
                );
                files.push(uri);
            }
            guest.products.downloads.remove(files, false);
            guest
                .products
                .downloads
                .queue_page(downloads::DownloadQueueScope::All, 0, 1)
                .await
                .unwrap();
            assert!(source.is_file());
            assert!(host.products.source.list_sources().sources.is_empty());
            guest.products.downloads.set_paused(true);
            guest
                .products
                .downloads
                .queue_page(downloads::DownloadQueueScope::All, 0, 1)
                .await
                .unwrap();
            let mut shared = Vec::new();
            for index in 0..2 {
                let path = root.path().join(format!("outside-{index}.flac"));
                let bytes = format!("shared playlist audio {index}").into_bytes();
                std::fs::write(&path, &bytes).unwrap();
                let uri = url::Url::from_file_path(&path).unwrap().to_string();
                a.database
                    .write_direct_track_metadata(
                        &uri,
                        &library::TrackMetadata {
                            title: format!("Outside {index}"),
                            album: "Shared playlist".into(),
                            artist: "Artist".into(),
                            disc_number: 1,
                            track_number: index + 1,
                            duration_millis: 1000,
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                a.database
                    .write_local_access(
                        None,
                        &[&library::LocalAccessWrite {
                            media_uri: uri.clone(),
                            origin: library::LocalAccessOrigin::Import,
                            path: path.to_string_lossy().into_owned(),
                            root: root.path().to_string_lossy().into_owned(),
                            relative_path: format!("outside-{index}.flac"),
                            access_uri: uri.clone(),
                            size_bytes: bytes.len() as i64,
                            mtime_ns: 1,
                            device_id: None,
                            inode: None,
                            parser_version: 18,
                            loudness_analysis_key: None,
                        }],
                    )
                    .await
                    .unwrap();
                shared.push((uri, bytes));
            }
            let (host_playlist, playlist) = a
                .database
                .create_playlist(
                    None,
                    "Outside sources",
                    &library::MemberSelection::Uris(
                        shared.iter().map(|(uri, _)| uri.clone()).collect(),
                    ),
                )
                .await
                .unwrap()
                .unwrap();
            let host_queue = host.products.playback.clone();
            let host_queue = tokio::task::spawn_blocking(move || host_queue.snapshot())
                .await
                .unwrap()
                .unwrap();
            let shared_keys = a
                .database
                .selected_track_keys(
                    &library::MemberSelection::Uris(
                        shared.iter().map(|(uri, _)| uri.clone()).collect(),
                    ),
                    &library::ReadCancellation::new(),
                )
                .await
                .unwrap();
            assert!(
                host_queue
                    .entries
                    .iter()
                    .all(|entry| !shared_keys.contains(&entry.track_key))
            );
            let snapshot = a.export().await.unwrap();
            let session = a.active().await.unwrap();
            let key = a
                .secret(file_key(&a.identity_reference().unwrap(), &session.profile))
                .await
                .unwrap()
                .unwrap();
            b.import_file(snapshot.path().to_owned(), true, Some(key))
                .await
                .unwrap();
            let cancellation = library::ReadCancellation::new();
            let playlist = b
                .database
                .playlist_key_by_identity(None, &playlist, &cancellation)
                .await
                .unwrap()
                .unwrap();
            let entries = b
                .database
                .queue_entries(
                    library::QueueLoad::tracks(
                        library::MemberRead::new(library::Collection::Playlist(playlist))
                            .selection(),
                    ),
                    None,
                )
                .await
                .unwrap()
                .entries;
            let rows = b
                .database
                .queue_rows_for_entries(
                    &entries.into_iter().enumerate().collect::<Vec<_>>(),
                    &cancellation,
                )
                .await
                .unwrap();
            assert_eq!(rows.len(), 2);
            for (row, (uri, bytes)) in rows.iter().zip(&shared) {
                assert!(!row.received_from_connect);
                assert_eq!(&row.track.media_uri, uri);
                assert!(
                    b.database
                        .connect_track_reference(uri)
                        .await
                        .unwrap()
                        .is_some()
                );
                let stream = crate::playback::prepare_stream(
                    &b.database,
                    playback::StreamRequest::original(uri),
                    None,
                    Some((b.as_ref(), row.as_ref())),
                    |_| async { panic!("shared file uses peer access") },
                )
                .await
                .unwrap();
                let response = reqwest::get(stream.uri()).await.unwrap();
                assert!(response.status().is_success());
                assert_eq!(response.bytes().await.unwrap().as_ref(), bytes);
                assert!(
                    b.database
                        .connect_media_files(uri)
                        .await
                        .unwrap()
                        .is_empty(),
                    "playing does not retain audio"
                );
            }
            let unadmitted = root.path().join("unadmitted.flac");
            std::fs::write(&unadmitted, b"not shared").unwrap();
            let uri = url::Url::from_file_path(unadmitted).unwrap().to_string();
            a.database
                .remember_media_uris(std::slice::from_ref(&uri))
                .await
                .unwrap();
            a.database
                .add_playlist_tracks(
                    None,
                    host_playlist,
                    &library::MemberSelection::Uris((vec![uri.clone()]).into()),
                    false,
                )
                .await
                .unwrap();
            let peer = a.status().identity.unwrap();
            assert!(
                b.request(&peer, Request::TrackReference { uri: uri.clone() })
                    .await
                    .unwrap()
                    .is_null()
            );
            assert!(
                b.connected_network()
                    .await
                    .unwrap()
                    .stream_request(
                        &peer,
                        serde_json::to_vec(&rufin_connect::media::PeerMediaRequest {
                            uri,
                            encoding: None,
                            range: None,
                            window: None
                        })
                        .unwrap()
                    )
                    .await
                    .is_err()
            );
            for inputs in [guest, host] {
                inputs.products.connect.leave_profile().await.unwrap();
                inputs.receivers.visualizer.close();
                let playback = inputs.products.playback.clone();
                tokio::task::spawn_blocking(move || playback.shutdown())
                    .await
                    .unwrap();
            }
        }))
    })
    .unwrap();
}

impl PlaybackBackend for IdleBackend {
    fn send(&mut self, _: BackendCommand) -> Result<(), BackendError> {
        Ok(())
    }
    fn drain_events(&mut self) -> Vec<BackendEvent> {
        Vec::new()
    }
}

#[test]
fn relay_changes_preserve_the_open_profile_and_device_identity() {
    let root = tempfile::tempdir().unwrap();
    app::with_runtime(|runtime| {
        runtime.block_on(Box::pin(async {
            let inputs = device(
                root.path(),
                "relay",
                diagnostics(),
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let owner = &inputs.products.connect;
            {
                let _action = owner.actions.lock().await;
                let result = tokio::time::timeout(
                    Duration::from_secs(1),
                    execute(
                        owner,
                        Action::Join {
                            invitation: "unused".into(),
                            replace: false,
                        },
                    ),
                )
                .await
                .expect("the local confirmation must not wait for network work");
                assert!(result.unwrap_err().contains("remove all data"));
            }
            execute(owner, Action::Create).await.unwrap();
            let session = owner.active().await.unwrap();
            let identity = owner.status().identity;
            let nearby_peer = "nearby-device".to_owned();
            owner
                .event(NetworkEvent::Discovered {
                    peer: nearby_peer.clone(),
                    name: "Rufin Android".into(),
                })
                .await
                .unwrap();
            execute(owner, Action::Refresh).await.unwrap();
            let nearby = owner
                .status()
                .devices
                .into_iter()
                .find(|device| device.id == nearby_peer)
                .unwrap();
            assert_eq!(nearby.name, "Rufin Android");
            assert!(!nearby.enrolled);
            owner
                .event(NetworkEvent::Discovered {
                    peer: nearby_peer.clone(),
                    name: "Phone".into(),
                })
                .await
                .unwrap();
            let nearby = owner
                .status()
                .devices
                .into_iter()
                .find(|device| device.id == nearby_peer)
                .unwrap();
            assert_eq!(nearby.name, "Phone");
            assert!(!nearby.enrolled);
            owner
                .event(NetworkEvent::DiscoveryExpired {
                    peer: nearby_peer.clone(),
                })
                .await
                .unwrap();
            assert!(
                owner
                    .status()
                    .devices
                    .iter()
                    .all(|device| device.id != nearby_peer)
            );
            for relay in [Some("http://127.0.0.1:9/".to_owned()), None] {
                let previous = owner.connected_network().await.unwrap();
                execute(
                    owner,
                    Action::Network {
                        nearby: false,
                        relay: relay.clone(),
                        public_relay: false,
                    },
                )
                .await
                .unwrap();
                let current = owner.connected_network().await.unwrap();
                assert!(!Arc::ptr_eq(&previous, &current));
                assert!(
                    current
                        .matches_configuration(false, relay.as_deref(), false)
                        .unwrap()
                );
                assert!(Arc::ptr_eq(&session, &owner.active().await.unwrap()));
                assert_eq!(owner.status().identity, identity);
            }
            execute(
                owner,
                Action::Rename {
                    name: "Living room".into(),
                },
            )
            .await
            .unwrap();
            owner.leave_profile().await.unwrap();
            execute(owner, Action::Discover).await.unwrap();
            assert_eq!(owner.status().identity, identity);
            assert_eq!(owner.status().settings.name, "Living room");
            assert!(owner.status().settings.profile.is_none());
            execute(owner, Action::Enable { enabled: true })
                .await
                .unwrap();
            assert_eq!(owner.status().identity, identity);
            assert_ne!(
                owner.status().settings.profile,
                Some(session.profile.clone())
            );
            owner.leave_profile().await.unwrap();
            inputs.receivers.visualizer.close();
            let playback = inputs.products.playback.clone();
            tokio::task::spawn_blocking(move || playback.shutdown())
                .await
                .unwrap();
        }))
    })
    .unwrap();
}

#[test]
fn connection_test_requires_the_peer_to_still_share_the_profile() {
    let root = tempfile::tempdir().unwrap();
    app::with_runtime(|runtime| {
        runtime.block_on(Box::pin(async {
            let host = device(
                root.path(),
                "host",
                diagnostics(),
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let guest = device(
                root.path(),
                "guest",
                diagnostics(),
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let a = &host.products.connect;
            let b = &guest.products.connect;
            let former = device(
                root.path(),
                "former-profile",
                diagnostics(),
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            execute(b, Action::Create).await.unwrap();
            pair(b, &former.products.connect).await;
            let former_id = former.products.connect.status().identity.unwrap();
            let music = root.path().join("music");
            std::fs::create_dir(&music).unwrap();
            let source_id = library::SourceId::new("cached-local");
            a.source
                .shared
                .settings
                .update(|stored| {
                    stored
                        .sources
                        .configured
                        .push(crate::settings::ConfiguredSource {
                            configuration: sources::SourceConfiguration::local(
                                source_id.clone(),
                                "Cached",
                                vec![music],
                            )
                            .unwrap(),
                            credential: None,
                            music_folder_id: None,
                            local_access: None,
                            enable_half_stars: false,
                        });
                    stored.sources.selected_source_id = Some(source_id.clone());
                    Ok(())
                })
                .unwrap();
            a.database
                .reconcile_source(&source_id, "local", "Cached")
                .await
                .unwrap();
            execute(a, Action::Create).await.unwrap();
            prepare_profile_snapshot(a).await;
            let retrying = device(
                root.path(),
                "retrying-profile",
                diagnostics(),
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let retry_owner = &retrying.products.connect;
            pair(b, retry_owner).await;
            let saved_keys = a
                .secrets
                .replace(Arc::new(secrets::UnavailableSecretStore::new(
                    "keyring unavailable",
                )));
            approve_pair(a, retry_owner).await;
            let failed = wait(retry_owner, |status| {
                status.error.is_some() && !status.connecting
            })
            .await;
            assert!(
                failed
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("keyring unavailable")
            );
            assert_eq!(failed.settings.profile, b.status().settings.profile);
            assert!(matches!(&failed.settings.pending_import, Some(PendingImport::Peer { profile, .. }) if Some(profile) == a.status().settings.profile.as_ref()));
            assert_eq!(ProfileStore::database_profile(retry_owner.database.path()).await.unwrap(), failed.settings.profile);
            assert!(!retry_owner.active().await.unwrap().stop.is_cancelled());
            assert!(execute(retry_owner, Action::Retry).await.is_err());
            assert!(
                retry_owner
                    .status()
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("keyring unavailable")
            );
            execute(retry_owner, Action::CancelPairing).await.unwrap();
            assert_eq!(retry_owner.status().error, failed.error);
            a.secrets.replace(saved_keys);
            execute(retry_owner, Action::Retry).await.unwrap();
            assert!(retry_owner.status().error.is_none());
            assert!(retry_owner.status().settings.pending_import.is_none());
            assert!(!retry_owner.directory.join(format!(
                "{}.pending.sqlite",
                blake3::hash(a.status().settings.profile.as_ref().unwrap().as_bytes()).to_hex()
            )).exists());
            assert_eq!(
                ProfileStore::database_profile(retry_owner.database.path())
                    .await
                    .unwrap(),
                a.status().settings.profile
            );
            let working = retry_owner.active().await.unwrap();
            let retained_keys = a.secrets.replace(Arc::new(secrets::UnavailableSecretStore::new("snapshot export must not run")));
            prepare_pair(a, retry_owner).await;
            assert!(Arc::ptr_eq(&working, &retry_owner.active().await.unwrap()));
            assert!(!working.stop.is_cancelled());
            a.secrets.replace(retained_keys);
            retry_owner.leave_profile().await.unwrap();
            retrying.receivers.visualizer.close();
            let retry_playback = retrying.products.playback.clone();
            tokio::task::spawn_blocking(move || retry_playback.shutdown())
                .await
                .unwrap();
            prepare_pair(a, b).await;
            assert!(
                !b.status()
                    .devices
                    .iter()
                    .any(|device| device.id == former_id && device.enrolled)
            );
            wait(&former.products.connect, |status| {
                !status.devices.iter().any(|device| {
                    Some(&device.id) == b.status().identity.as_ref() && device.enrolled
                })
            })
            .await;
            b.database
                .reconcile_source(&source_id, "local", "Cached")
                .await
                .unwrap();
            let mut catalog = b.database.catalog(source_id.as_str(), None).await.unwrap();
            let observed = catalog.start_observation().await.unwrap();
            catalog
                .apply_catalog(
                    &library::CatalogBatch {
                        tracks: vec![library::TrackObservation {
                            object_id: "cached-track".into(),
                            metadata: library::TrackMetadata {
                                title: "Cached track".into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    true,
                )
                .await
                .unwrap();
            catalog
                .complete_source_observation(observed, &[], None)
                .await
                .unwrap();
            drop(catalog);
            let paired = b.status().settings;
            let download_settings = b.source.shared.settings.load().ui.download_settings(&source_id);
            b.source
                .shared
                .settings
                .update(|stored| {
                    stored.sources.selected_source_id = Some(source_id.clone());
                    Ok(())
                })
                .unwrap();
            let resumed = crate::source::SourceOwner::open_dormant(
                guest.products.artwork.clone(),
                guest.products.library.clone(),
                guest.products.downloads.clone(),
                b.source.shared.settings.clone(),
                b.secrets.clone(),
                guest.products.runtime.clone(),
                crate::source::SourceOutputs {
                    events: async_channel::unbounded().0,
                    discovery: async_channel::unbounded().0,
                },
            );
            resumed.install_connect(b);
            let mut operations = resumed.operation();
            resumed.start().unwrap();
            tokio::time::timeout(
                Duration::from_secs(2),
                operations.wait_for(|operation| {
                    matches!(operation, crate::runtime::source::SourceOperation::Idle)
                }),
            )
            .await
            .expect("Connect must not hide the cached library")
            .unwrap();
            assert_eq!(resumed.selected_library().unwrap().source_id, source_id);
            let selected = resumed.selected_library().unwrap();
            assert_eq!(
                resumed
                    .shared
                    .database
                    .source_counts(selected.source_key, &library::ReadCancellation::new())
                    .await
                    .unwrap()
                    .1,
                1
            );
            execute(b, Action::CancelPairing).await.unwrap();
            let retained = b.status().settings;
            assert_eq!(retained.profile, paired.profile);
            assert_eq!(b.source.shared.settings.load().ui.download_settings(&source_id), download_settings);
            assert!(retained.profile.is_some() && retained.pending_import.is_none());
            assert!(!b.active().await.unwrap().stop.is_cancelled());
            let favorite = library::FavoriteTarget::Track(library::source_entity_uri(
                &source_id,
                "track",
                "cached-track",
            ));
            b.database.set_favorite(&favorite, true).await.unwrap();
            a.connected_network()
                .await
                .unwrap()
                .close_profile()
                .await
                .unwrap();
            execute(b, Action::Retry).await.unwrap();
            a.connected_network().await.unwrap().open_profile(&a.active().await.unwrap().profile, false, serde_json::to_vec(&Enrollment { file_key: a.secret(file_key(&a.identity_reference().unwrap(), &a.active().await.unwrap().profile)).await.unwrap().unwrap() }).unwrap()).await.unwrap();
            a.connected_network().await.unwrap().attach_documents(a.active().await.unwrap().documents.clone()).await.unwrap();
            assert!(b.status().error.is_none());
            assert_eq!(b.status().settings.profile, paired.profile);
            assert!(b.status().settings.pending_import.is_none());
            assert!(b.database.favorite(&favorite).await.unwrap());
            let rows = b.database.tracks_by_uris(&[favorite.media_uri().to_owned()], &library::ReadCancellation::new()).await.unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].metadata.title, "Cached track");
            execute(b, Action::Retry).await.unwrap();
            assert_eq!(b.status().settings.profile, paired.profile);
            let peer = b.status().identity.unwrap();
            a.event(NetworkEvent::DiscoveryExpired { peer: peer.clone() })
                .await
                .unwrap();
            assert!(
                a.status()
                    .devices
                    .iter()
                    .any(|device| device.id == peer && device.enrolled)
            );
            execute(a, Action::TestConnection { peer: peer.clone() })
                .await
                .unwrap();
            assert!(
                a.status()
                    .devices
                    .iter()
                    .any(|device| device.id == peer && device.reachable)
            );
            let image_key = format!("originals/native/{:032x}/{:032x}/original.png", 1, 2);
            let image_query = artwork::CacheImageRequest {
                family: format!("native/{:032x}/{:032x}", 1, 2),
                size: sources::ImageSize::Original,
            };
            let image = image::RgbaImage::from_fn(3072, 2048, |x, y| {
                let value = (x + y * 3072 + 1).wrapping_mul(0x9e3779b9);
                let value = (value ^ (value >> 16)).wrapping_mul(0x85ebca6b);
                let value = value ^ (value >> 13);
                image::Rgba([value as u8, (value >> 8) as u8, (value >> 16) as u8, 255])
            });
            let mut encoded = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image)
                .write_to(&mut encoded, image::ImageFormat::Png)
                .unwrap();
            let encoded = encoded.into_inner();
            assert!(encoded.len() > 16 * 1024 * 1024);
            let image_file = root.path().join("published.png");
            std::fs::write(&image_file, &encoded).unwrap();
            a.source
                .shared
                .artwork
                .import_missing_cache_file(&image_key, &image_file)
                .unwrap();
            let host_id = a.status().identity.unwrap();
            execute(
                b,
                Action::TestConnection {
                    peer: host_id.clone(),
                },
            )
            .await
            .unwrap();
            let (received_key, received) = b.fetch_cached_artwork(image_query).await.unwrap();
            assert_eq!(received_key, image_key);
            assert_eq!(std::fs::read(&received).unwrap(), encoded);
            assert!(
                b.source
                    .shared
                    .artwork
                    .import_missing_cache_file(&image_key, &received)
                    .unwrap()
            );
            assert!(
                !b.source
                    .shared
                    .artwork
                    .import_missing_cache_file(&image_key, &image_file)
                    .unwrap()
            );
            let audio = root.path().join("transferred.flac");
            std::fs::write(&audio, b"audio").unwrap();
            std::fs::write(audio.with_extension("lrc"), "[00:00.00]Source only").unwrap();
            let uri = url::Url::from_file_path(&audio).unwrap().to_string();
            a.database
                .remember_media_uris(std::slice::from_ref(&uri))
                .await
                .unwrap();
            b.database
                .remember_media_uris(std::slice::from_ref(&uri))
                .await
                .unwrap();
            assert!(
                b.fetch_cached_lyrics(LyricsCacheQuery {
                    media_uri: uri.clone(),
                    authority: "source".into(),
                    role: "original".into(),
                    language: String::new(),
                    script: String::new(),
                    input: [0; 32],
                })
                .await
                .is_none(),
                "cache reads do not acquire the available sidecar"
            );
            b.peer_access()
                .cache_assets(&b.database, &uri, [0; 32])
                .await
                .unwrap();
            let cached = b
                .database
                .lyrics_cache_for_role(
                    &uri,
                    "original",
                    "",
                    "",
                    [0; 32],
                    "source",
                    &library::ReadCancellation::new(),
                )
                .await
                .unwrap()
                .unwrap();
            assert!(lyrics::LyricsBundle::from_cache_payload(&cached.lyrics).is_some());
            assert!(cached.lyrics.contains("Source only"));
            let cached_uri = "https://example.invalid/shared-lyrics.flac";
            for (authority, origin) in [
                ("source", lyrics::LyricsOrigin::Native),
                (
                    "external",
                    lyrics::LyricsOrigin::External(lyrics::ExternalLyricsProvider::Lrclib),
                ),
            ] {
                let mut bundle =
                    lyrics::lyrics_from_edited_text("[00:00.00]Shared lyrics").unwrap();
                bundle.origin = origin;
                let payload = bundle.cache_payload().unwrap();
                a.database
                    .write_lyrics_cache(
                        cached_uri, authority, "original", "", "", [7; 32], &payload, 42,
                    )
                    .await
                    .unwrap();
                let query = LyricsCacheQuery {
                    media_uri: cached_uri.into(),
                    authority: authority.into(),
                    role: "original".into(),
                    language: String::new(),
                    script: String::new(),
                    input: [7; 32],
                };
                let shared = b.fetch_cached_lyrics(query.clone()).await.unwrap();
                assert_eq!(shared.lyrics, payload);
                assert_eq!(shared.cache_input_digest, [7; 32]);
                assert_eq!(shared.updated_at, 42);
                assert_eq!(
                    lyrics::LyricsBundle::from_cache_payload(&shared.lyrics)
                        .unwrap()
                        .origin,
                    origin
                );
                if authority == "source" {
                    assert!(
                        b.fetch_cached_lyrics(LyricsCacheQuery {
                            input: [8; 32],
                            ..query
                        })
                        .await
                        .is_none()
                    );
                }
            }
            let documents = a.active().await.unwrap().documents.clone();
            let mut after = 0;
            loop {
                let changes = documents
                    .changes_in_stream(after, library::CONNECT_PAGE_SIZE, library::profile::SyncStream::Authored)
                    .await
                    .unwrap();
                assert!(
                    changes
                        .iter()
                        .all(|change| !change.name.starts_with("cache_lyrics:"))
                );
                let Some(last) = changes.last() else { break };
                after = last.revision;
            }
            let saved_keys = a
                .secrets
                .replace(Arc::new(secrets::UnavailableSecretStore::new(
                    "keyring unavailable",
                )));
            pair(a, b).await;
            a.secrets.replace(saved_keys);
            assert_eq!(
                ProfileStore::database_profile(b.database.path())
                    .await
                    .unwrap(),
                a.status().settings.profile
            );
            assert!(b.database.favorite(&favorite).await.unwrap());
            let previous_identity = b.status().identity;
            b.status.send_modify(|status| {
                for device in &mut status.devices {
                    device.reachable = false;
                }
            });
            execute(b, Action::Leave).await.unwrap();
            wait(a, |status| {
                !status
                    .devices
                    .iter()
                    .any(|device| device.id == peer && device.enrolled)
            })
            .await;
            pair(a, b).await;
            assert_eq!(b.status().identity, previous_identity);
            // Keep the same transport reachable after closing its profile.
            b.connected_network()
                .await
                .unwrap()
                .close_profile()
                .await
                .unwrap();
            assert_eq!(b.status().identity.as_deref(), Some(peer.as_str()));
            a.connected_network()
                .await
                .unwrap()
                .remember_peer(&b.status().invitation.unwrap())
                .await
                .unwrap();
            assert!(
                execute(a, Action::TestConnection { peer: peer.clone() })
                    .await
                    .is_err()
            );
            // Presence events from pairing can still update reachability after the probe.
            wait(a, |status| {
                status
                    .devices
                    .iter()
                    .any(|device| device.id == peer && !device.reachable)
            })
            .await;
            for inputs in [guest, host, former] {
                inputs.products.connect.leave_profile().await.unwrap();
                inputs.receivers.visualizer.close();
                let playback = inputs.products.playback.clone();
                tokio::task::spawn_blocking(move || playback.shutdown())
                    .await
                    .unwrap();
            }
        }))
    })
    .unwrap();
}

#[test]
fn profile_files_and_pairing_preserve_the_ready_catalog() {
    use axum::{body::Bytes, extract::State, http::StatusCode, response::IntoResponse};
    let root = tempfile::tempdir().unwrap();
    let diagnostics = diagnostics();
    app::with_runtime(|runtime| {
        runtime.block_on(Box::pin(async {
            type Files = Arc<Mutex<(BTreeMap<String, Vec<u8>>, usize, BTreeSet<String>, usize, usize, usize)>>;
            let file: Files = Arc::new(Mutex::new((BTreeMap::new(), 0, BTreeSet::from(["/".into(), "/profiles".into()]), 0, 0, 0)));
            let router = axum::Router::new().fallback(axum::routing::any(
                |State(files): State<Files>, method: axum::http::Method, uri: axum::http::Uri, body: Bytes| async move {
                    let path = uri.path().to_owned();
                    let mut files = files.lock().unwrap();
                    match method.as_str() {
                        "MKCOL" => {
                            files.2.insert(path.trim_end_matches('/').to_owned());
                            StatusCode::CREATED.into_response()
                        }
                        "PROPFIND" => {
                            let base = if path.trim_end_matches('/').is_empty() { "/" } else { path.trim_end_matches('/') };
                            let directory = files.2.contains(base);
                            if !directory && !files.0.contains_key(&path) { return StatusCode::NOT_FOUND.into_response(); }
                            let kind = if directory { "<d:collection/>" } else { "" };
                            let mut entries = format!("<d:response><d:href>{path}</d:href><d:propstat><d:prop><d:resourcetype>{kind}</d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>");
                            if directory {
                                for name in files.0.keys().chain(files.2.iter()) {
                                    let parent = name.rsplit_once('/').map_or("/", |(parent, _)| if parent.is_empty() { "/" } else { parent });
                                    if name == base || parent != base { continue; }
                                    let kind = if files.2.contains(name) { "<d:collection/>" } else { "" };
                                    entries.push_str(&format!("<d:response><d:href>{name}</d:href><d:propstat><d:prop><d:resourcetype>{kind}</d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"));
                                }
                            }
                            (StatusCode::MULTI_STATUS, format!("<d:multistatus xmlns:d=\"DAV:\">{entries}</d:multistatus>")).into_response()
                        }
                        "GET" => {
                            if path.contains("/cache/") { files.3 += 1; }
                            if path.contains(".delta-") { files.4 += 1; }
                            if path.contains(".base-") { files.5 += 1; }
                            match files.0.get(&path) {
                                Some(bytes) => bytes.clone().into_response(),
                                None => StatusCode::NOT_FOUND.into_response(),
                            }
                        }
                        "DELETE" => {
                            if files.0.remove(&path).is_some() { StatusCode::NO_CONTENT.into_response() }
                            else { StatusCode::NOT_FOUND.into_response() }
                        }
                        "PUT" => {
                            assert!(path.starts_with("/profiles/"));
                            assert!(files.2.contains(path.rsplit_once('/').unwrap().0), "create the chosen folder before uploading");
                            files.0.insert(path, body.to_vec());
                            files.1 += 1;
                            StatusCode::CREATED.into_response()
                        }
                        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
                    }
                }
            )).layer(axum::extract::DefaultBodyLimit::disable()).with_state(file.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let inputs = device(
                root.path(),
                "file-exchange",
                diagnostics,
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let settings = sources::FileSourceSettings {
                excluded_folders: Vec::new(),
                url: format!("http://{address}/"),
                alternate_urls: vec![], folders: vec![], username: String::new(), domain: String::new(),
                authentication: sources::FileAuthentication::Anonymous,
                trust_invalid_certificate: false, certificate_pem: None, require_smb_encryption: false,
            };
            let source_id = inputs.products.source.configure_file_integration(
                sources::SourceSetup::WebDav {
                    name: "Profile storage".into(), settings,
                    credentials: sources::FileCredentials { secret: String::new(), headers: vec![] },
                },
            ).recv().await.unwrap().unwrap();
            assert!(inputs.products.source.list_sources().sources.is_empty());
            assert!(inputs.products.source.selected_library().is_none());
            assert_eq!(inputs.products.source.file_integrations(), vec![crate::source::FileIntegration {
                id: source_id.clone(), name: "Profile storage".into(), kind: "webdav".into(), music_source: false,
            }]);
            let preset = inputs.products.source.editable(source_id.clone()).unwrap().unwrap();
            inputs.products.source.update_file_integration(sources::SourceSettingsChange::Files {
                source_id: source_id.clone(), name: "Files".into(), settings: preset.file_settings.unwrap(),
                credentials: sources::FileCredentialsEdit { secret: None, headers: None },
            }).recv().await.unwrap().unwrap();
            assert_eq!(inputs.products.source.file_integrations()[0].name, "Files");
            assert!(inputs.products.source.list_sources().sources.is_empty());
            let settings = inputs.products.source.shared.settings.clone();
            tokio::task::spawn_blocking(move || settings.update(|stored| {
                let mut music = stored.sources.integrations[0].clone();
                music.configuration.source_id = sources::SourceId::new("music-server");
                music.configuration.name = "Music server".into();
                stored.sources.configured.push(music);
                Ok(())
            })).await.unwrap().unwrap();
            let connections = inputs.products.source.file_integrations();
            assert_eq!(connections.len(), 2);
            assert_eq!(connections[0].id.as_str(), "music-server");
            assert!(connections[0].music_source);
            assert_eq!(inputs.products.source.list_sources().sources[0].id, connections[0].id);
            assert_eq!(inputs.products.source.shared.settings.load().sources.integrations.len(), 1);
            let owner = &inputs.products.connect;
            let music = library::SourceId::new("music-server");
            owner.database.reconcile_source(&music, "webdav", "Music server").await.unwrap();
            let music_root = format!("http://{address}/music/");
            file.lock().unwrap().0.insert("/music/copied.m3u".into(), b"#EXTM3U\n#PLAYLIST:Disk version\ntrack-0.flac\n".to_vec());
            let playlist_file = library::LocalFileWrite {
                path: format!("{music_root}copied.m3u"), root: music_root.clone(), relative_path: "copied.m3u".into(),
                kind: library::LocalFileKind::Media, size_bytes: Some(64), mtime_ns: 1,
                device_id: None, inode: None, native_id: None, picture_index: None, artist_pictures: None,
                revision: Some("playlist-v1".into()), parse_version: Some(1), state: library::LocalFileState::Rejected,
            };
            let track_file = library::LocalFileWrite {
                path: format!("{music_root}track-0.flac"),
                root: music_root,
                relative_path: "track-0.flac".into(),
                kind: library::LocalFileKind::Media,
                size_bytes: Some(16),
                mtime_ns: 1,
                device_id: None,
                inode: None,
                native_id: None,
                picture_index: None,
                artist_pictures: None,
                revision: Some("track-v1".into()),
                parse_version: Some(1),
                state: library::LocalFileState::Accepted,
            };
            let mut tracks = (0..130).map(|index| library::TrackObservation {
                object_id: format!("track-{index}"),
                album_object_id: Some("album".into()),
                metadata: library::TrackMetadata {
                    title: format!("Track {index}"),
                    sort_text: format!("track {index:03}"),
                    album: "Shared album".into(),
                    artist: "Shared artist".into(),
                    duration_millis: 120_000 + index,
                    disc_number: 1,
                    track_number: index + 1,
                    source_format: Some("flac".into()),
                    ..Default::default()
                },
                ..Default::default()
            }).collect::<Vec<_>>();
            tracks[0].user_data = library::NativeUserState { favorite: Some(true), rating: Some(None), play_count: Some(7), ..Default::default() };
            tracks[0].source_path = Some(track_file.path.clone());
            let freshness = library::Freshness::new(b"received-native-catalog".to_vec()).unwrap();
            let mut catalog = owner.database.catalog(music.as_str(), None).await.unwrap();
            let observed = catalog.start_observation().await.unwrap();
            catalog.apply_catalog(&library::CatalogBatch {
                tracks: tracks.clone(),
                albums: vec![library::AlbumObservation {
                    object_id: "album".into(),
                    metadata: library::AlbumMetadata {
                        title: "Shared album".into(),
                        artist: "Shared artist".into(),
                        sort_text: "shared album".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                playlists: vec![library::PlaylistObservation { object_id: "server-mix".into(), name: "Server mix".into(), sort_text: "server mix".into(), writable: Some(false), metadata_writable: Some(true), ..Default::default() }],
                ..Default::default()
            }, true).await.unwrap();
            catalog.write_file_observations(&[(track_file.clone(), Vec::new()), (playlist_file, Vec::new())], vec![tracks[0].clone()], true).await.unwrap();
            catalog.write_source_playlist_page("server-mix", 0, &(0..130).map(|index| library::PlaylistEntryObservation { object_id: format!("entry-{index}"), track_id: format!("track-{index}") }).collect::<Vec<_>>(), true).await.unwrap();
            catalog.write_source_playlist_freshness("server-mix", Some("revision-one"), Some(7_000_000_000)).await.unwrap();
            catalog.replace_source_home_section("featured", Some("Featured"), &[library::HomeEntryInput { position: 0, kind: library::HomeEntryKind::Track, entity_object_id: "track-0".into(), title: "Featured track".into(), subtitle: String::new() }]).await.unwrap();
            catalog.complete_source_observation(observed, &[], Some(&freshness)).await.unwrap();
            drop(catalog);
            let shared_uris = vec![library::source_entity_uri(&music, "track", "track-0"), library::source_entity_uri(&music, "track", "track-1")];
            let (shared_playlist, shared_id) = owner.database.create_playlist(None, "Shared edit", &library::MemberSelection::Uris((shared_uris.clone()).into())).await.unwrap().unwrap();
            execute(owner, Action::Create).await.unwrap();
            // A released device link has no canonical file association yet.
            owner.database.save_playlist_file_link(&library::PlaylistFileLink {
                playlist: shared_playlist, source_id: Some(music.clone()), path: "music/copied.m3u".into(),
                auto_refresh: true, auto_save: None, path_mode: library::PlaylistPathMode::Automatic,
                revision: Some("older disk revision".into()), dirty: true, error: None,
            }).await.unwrap();
            let music_key = owner.database.cached_source(music.as_str(), &library::ReadCancellation::new()).await.unwrap().unwrap().source;
            assert!(owner.database.observed_playlist_file_page(music_key, None).await.unwrap()[0].playlist_identity.is_none());
            let previous = owner.session.write().await.take().unwrap();
            let profile = previous.profile.clone();
            previous.stop.cancel();
            previous.documents.close().await.unwrap();
            owner.connected_network().await.unwrap().close_profile().await.unwrap();
            owner.open(profile, false).await.unwrap();
            let reopened = owner.database.observed_playlist_file_page(music_key, None).await.unwrap();
            assert!(reopened[0].playlist_identity.is_some(), "opening an existing profile publishes released file associations without another scan or export");
            assert_eq!(reopened[0].playlist, Some(shared_playlist));
            tracks[0].metadata.title = "Edited track".into();
            owner.database.catalog(music.as_str(), None).await.unwrap()
                .write_file_observations(&[(track_file, Vec::new())], vec![tracks[0].clone()], true).await.unwrap();
            let uri = library::source_entity_uri(&music, "track", "track-0");
            let cancellation = library::ReadCancellation::new();
            let rows = owner.database.tracks_by_uris(std::slice::from_ref(&uri), &cancellation).await.unwrap();
            let expected_metadata = rows[0].metadata.clone();
            assert_eq!(expected_metadata.title, "Edited track");
            let exported = owner.export().await.unwrap();
            let session = owner.active().await.unwrap();
            let key = owner.secret(file_key(&owner.identity_reference().unwrap(), &session.profile)).await.unwrap().unwrap();
            execute(owner, Action::Export { path: "profiles/manual.rufin-connect".into(), source_id: Some(source_id.clone()) }).await.unwrap();
            assert_eq!(owner.list_profile_files(source_id.clone(), "profiles".into()).await.unwrap(), vec!["profiles/manual.rufin-connect"]);
            execute(owner, Action::Import { path: "profiles/manual.rufin-connect".into(), source_id: Some(source_id), replace: false, key: None }).await.unwrap();
            for (name, paired) in [
                ("native-file-receiver", false),
                ("native-network-receiver", true),
            ] {
                let receiving = device(root.path(), name, self::diagnostics(), Arc::new(secrets::MemorySecretStore::new())).await;
                let receiving_owner = &receiving.products.connect;
                receiving_owner.database.reconcile_source(&music, "webdav", "Music server").await.unwrap();
                let mut previous = tracks[0].clone();
                previous.user_data = library::NativeUserState { favorite: Some(false), rating: Some(Some(70)), ..Default::default() };
                receiving_owner.database.catalog(music.as_str(), Some(0)).await.unwrap().apply_catalog(&library::CatalogBatch { tracks: vec![previous], ..Default::default() }, true).await.unwrap();
                if paired {
                    pair(owner, receiving_owner).await;
                } else {
                    receiving_owner.import_file(exported.path().to_owned(), true, Some(key.clone())).await.unwrap();
                }
                let received = &receiving_owner.database;
                let source = received.cached_source(music.as_str(), &cancellation).await.unwrap().unwrap();
                assert!(source.catalog_complete, "the native catalog is ready before provider refresh");
                assert!(received.accept_freshness(music.as_str(), &freshness, &cancellation).await.unwrap());
                assert_eq!(received.source_counts(source.source, &cancellation).await.unwrap(), (1, 130));
                let rows = received.tracks_by_uris(std::slice::from_ref(&uri), &cancellation).await.unwrap();
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].media_uri, uri);
                assert_eq!(rows[0].metadata, expected_metadata);
                assert!(rows[0].state.favorite);
                assert_eq!(rows[0].state.rating, None, "native rating null clears the previous server rating");
                let playlist = received.playlist_key_by_object(source.source, "server-mix", &cancellation).await.unwrap().unwrap();
                let access = received.playlist_access(playlist, &cancellation).await.unwrap().unwrap();
                assert!(!access.writable);
                assert!(access.metadata_writable);
                let entries = received.playlist_entry_uri_page(playlist, -1).await.unwrap();
                assert_eq!(entries.len(), 130);
                assert_eq!(entries[0].2, uri);
                assert!(received.source_playlist_is_fresh(music.as_str(), "server-mix", Some("revision-one"), 0).await.unwrap());
                let home = received.home_page(source.source, None, 0, 0, &[], &cancellation).await.unwrap();
                assert_eq!(home.provider_sections[0].section_id, "featured");
                assert_eq!(home.provider_sections[0].rows.tracks[0].track.media_uri, uri);
                let shared = received.playlist_key_by_identity(None, &shared_id, &cancellation).await.unwrap().unwrap();
                assert!(received.get_playlist_file_link(shared).await.unwrap().is_none(), "device file permissions are not imported");
                crate::playlist_files::discover(&receiving.products.source, &music, source.source).await;
                let associated = received.find_playlist_file_link(Some(&music), "music/copied.m3u").await.unwrap().unwrap();
                assert_eq!(associated.playlist, shared, "automatic discovery reuses the existing random playlist identity");
                assert_eq!(received.playlist_entry_uri_page(shared, -1).await.unwrap().into_iter().map(|entry| entry.2).collect::<Vec<_>>(), shared_uris, "the stale physical playlist does not overwrite shared edits");
                let copy = receiving.products.source.client(&music).unwrap().import_playlist_file(received, "music/copied.m3u", None).await.unwrap();
                assert_ne!(copy.playlist, shared, "explicit import as a copy remains independent");
                receiving_owner.leave_profile().await.unwrap();
                receiving.receivers.visualizer.close();
                let playback = receiving.products.playback.clone();
                tokio::task::spawn_blocking(move || playback.shutdown()).await.unwrap();
            }

            owner.leave_profile().await.unwrap();
            inputs.receivers.visualizer.close();
            let playback = inputs.products.playback.clone();
            tokio::task::spawn_blocking(move || playback.shutdown())
                .await
                .unwrap();
            server.abort();
        }))
    })
    .unwrap();
}

async fn execute(owner: &Arc<ConnectOwner>, action: Action) -> Result<Status, String> {
    let owner = Arc::clone(owner);
    tokio::spawn(async move { owner.execute(action).await })
        .await
        .unwrap()
        .map_err(|error| error.to_string())
}

// Finish background startup and history pruning before capturing the profile.
async fn prepare_profile_snapshot(owner: &Arc<ConnectOwner>) {
    while owner.database.connect_seed_page().await.unwrap() {}
    owner.synchronize().await.unwrap();
    let session = owner.active().await.unwrap();
    while session
        .documents
        .prune_history(&session.identity, &[], library::CONNECT_PAGE_SIZE)
        .await
        .unwrap()
        > 0
    {}
}

async fn device(
    root: &std::path::Path,
    name: &str,
    diagnostics: crate::runtime::DiagnosticsHandle,
    secret_store: Arc<dyn SecretStore>,
) -> RuntimeInputs {
    let root = root.join(name);
    let paths = Paths {
        config: root.join("config"),
        cache: root.join("cache"),
        data: root.join("data"),
        state: root.join("state"),
    };
    let settings = app::startup_settings(&paths);
    let inputs = app::runtime_inputs(
        diagnostics,
        false,
        settings,
        paths,
        |_| Ok(Box::new(IdleBackend) as Box<dyn PlaybackBackend>),
        Vec::new,
        Arc::new(|_| {}),
        Arc::new(|_, _| {}),
    )
    .await
    .unwrap();
    inputs
        .products
        .source
        .change_secret_storage(secrets::SecretStorageMode::ConfigFile)
        .await
        .unwrap();
    inputs.products.connect.secrets.replace(secret_store);
    inputs
        .products
        .connect
        .save(|config| {
            config.nearby = false;
        })
        .unwrap();
    inputs
}

async fn wait(owner: &ConnectOwner, predicate: impl Fn(&Status) -> bool) -> Status {
    let mut status = owner.subscribe();
    tokio::time::timeout(Duration::from_secs(90), status.wait_for(predicate))
        .await
        .unwrap_or_else(|_| panic!("Connect did not settle: {:?}", owner.status()))
        .unwrap()
        .clone()
}

async fn approve_pair(host: &Arc<ConnectOwner>, guest: &Arc<ConnectOwner>) {
    let invitation = host.status().invitation.unwrap();
    execute(
        guest,
        Action::Join {
            invitation,
            replace: true,
        },
    )
    .await
    .unwrap();
    let a = wait(host, |status| status.pairing.is_some())
        .await
        .pairing
        .unwrap();
    let b = wait(guest, |status| status.pairing.is_some())
        .await
        .pairing
        .unwrap();
    assert_eq!(a.emoji, b.emoji);
    let already_enrolled = host
        .status()
        .devices
        .iter()
        .any(|device| device.enrolled && device.id == a.peer);
    for _ in 0..2 {
        let confirmed = execute(
            host,
            Action::Pair {
                session: a.session.clone(),
                approve: true,
                replace: false,
            },
        )
        .await
        .unwrap();
        assert!(confirmed.pairing.as_ref().unwrap().approved);
        assert_eq!(
            confirmed
                .devices
                .iter()
                .any(|device| device.enrolled && device.id == a.peer),
            already_enrolled,
        );
    }
    execute(
        guest,
        Action::Pair {
            session: b.session,
            approve: true,
            replace: false,
        },
    )
    .await
    .unwrap();
}

async fn prepare_pair(host: &Arc<ConnectOwner>, guest: &Arc<ConnectOwner>) {
    approve_pair(host, guest).await;
    wait(guest, |status| {
        !status.connecting
            && status.pairing.is_none()
            && status.settings.pending_import.is_none()
            && !status.receiving_collection
            && status.devices.iter().any(|device| {
                Some(&device.id) == host.status().identity.as_ref() && device.enrolled
            })
    })
    .await;
    assert!(guest.status().settings.pending_import.is_none());
    assert!(!guest.status().receiving_collection);
    guest.synchronize().await.unwrap();
}

async fn pair(host: &Arc<ConnectOwner>, guest: &Arc<ConnectOwner>) {
    prepare_pair(host, guest).await;
}
