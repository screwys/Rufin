use super::*;
use crate::{app, diagnostics::Diagnostics, paths::Paths, runtime::RuntimeInputs};
use playback::{BackendCommand, BackendError, BackendEvent, PlaybackBackend};

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
    for (saved, expected) in [
        (r#"{}"#, false),
        (r#"{"name":"Work Laptop"}"#, false),
        (r#"{"profile":null}"#, false),
        (r#"{"profile":"shared-profile"}"#, true),
        (r#"{"profile":"shared-profile","enabled":false}"#, false),
        (r#"{"enabled":true}"#, true),
    ] {
        let config = ConnectSettings::from_device_file(saved.as_bytes()).unwrap();
        assert_eq!(config.enabled, expected, "{saved}");
    }
}

struct IdleBackend;
impl PlaybackBackend for IdleBackend {
    fn send(&mut self, _: BackendCommand) -> Result<(), BackendError> {
        Ok(())
    }
    fn drain_events(&mut self) -> Vec<BackendEvent> {
        Vec::new()
    }
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
            execute(a, Action::Create).await.unwrap();
            pair(a, b).await;
            let peer = b.status().identity.unwrap();
            execute(a, Action::TestConnection { peer: peer.clone() })
                .await
                .unwrap();
            assert!(
                a.status()
                    .devices
                    .iter()
                    .any(|device| device.id == peer && device.reachable)
            );
            execute(b, Action::Leave).await.unwrap();
            // Discovery may be enabled again before joining a profile. The
            // transport can accept a connection, but Rufin must reject the test.
            execute(b, Action::Discover).await.unwrap();
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
            assert!(
                a.status()
                    .devices
                    .iter()
                    .any(|device| device.id == peer && !device.reachable)
            );
            for inputs in [guest, host] {
                inputs.products.connect.close_network().await.unwrap();
                inputs.receivers.visualizer.close();
                let playback = inputs.products.playback.transport.clone();
                tokio::task::spawn_blocking(move || playback.shutdown())
                    .await
                    .unwrap();
            }
        }))
    })
    .unwrap();
}

#[test]
fn folder_exchange_preserves_disconnected_edits_without_rewriting_peer_files() {
    let root = tempfile::tempdir().unwrap();
    let diagnostics = diagnostics();
    app::with_runtime(|runtime| {
        runtime.block_on(Box::pin(async {
            let host = device(
                root.path(),
                "folder-host",
                diagnostics.clone(),
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let guest = device(
                root.path(),
                "folder-guest",
                diagnostics,
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let a = &host.products.connect;
            let b = &guest.products.connect;
            execute(a, Action::Create).await.unwrap();
            pair(a, b).await;
            a.close_network().await.unwrap();
            b.close_network().await.unwrap();
            let folders = [root.path().join("sync-a"), root.path().join("sync-b")];
            for (owner, folder) in [(a, &folders[0]), (b, &folders[1])] {
                execute(
                    owner,
                    Action::Destination {
                        destination: Some(portable::Destination::Local {
                            path: folder.clone(),
                        }),
                    },
                )
                .await
                .unwrap();
            }
            let first = library::FavoriteTarget::Track("https://example.invalid/first.flac".into());
            let second =
                library::FavoriteTarget::Track("https://example.invalid/second.flac".into());
            a.database.set_favorite(&first, true).await.unwrap();
            b.database.set_favorite(&second, true).await.unwrap();
            for owner in [a, b] {
                owner.synchronize().await.unwrap();
                owner.exchange().await.unwrap();
            }
            assert!(!a.database.favorite(&second).await.unwrap());
            assert!(!b.database.favorite(&first).await.unwrap());
            let names = [
                format!("{}.rufin-connect", a.status().identity.unwrap()),
                format!("{}.rufin-connect", b.status().identity.unwrap()),
            ];
            // Deliver the two independent writes as a file-sync tool would.
            std::fs::copy(folders[0].join(&names[0]), folders[1].join(&names[0])).unwrap();
            std::fs::copy(folders[1].join(&names[1]), folders[0].join(&names[1])).unwrap();
            let peer_files = [
                std::fs::read(folders[0].join(&names[1])).unwrap(),
                std::fs::read(folders[1].join(&names[0])).unwrap(),
            ];
            for (index, owner) in [a, b].into_iter().enumerate() {
                owner.exchange().await.unwrap();
                assert!(owner.database.favorite(&first).await.unwrap());
                assert!(owner.database.favorite(&second).await.unwrap());
                let session = owner.active().await.unwrap();
                while session
                    .documents
                    .prune_history(&session.identity, &[], library::CONNECT_PAGE_SIZE)
                    .await
                    .unwrap()
                    > 0
                {}
                owner.exchange().await.unwrap();
                let own = folders[index].join(&names[index]);
                let bytes = std::fs::read(&own).unwrap();
                owner.exchange().await.unwrap();
                *owner.active().await.unwrap().file_exchange.lock().await = None;
                owner.exchange().await.unwrap();
                assert!(
                    std::fs::read(&own).unwrap() == bytes,
                    "no rewrite after a no-op or reopening"
                );
                assert_eq!(
                    std::fs::read(folders[index].join(&names[1 - index])).unwrap(),
                    peer_files[index]
                );
            }
            // Pruning changes representation, not document versions. The next
            // exchange must still replace this device's old, larger file.
            let session = a.active().await.unwrap();
            for index in 0..80 {
                session
                    .documents
                    .write_records(&[ConnectRecord {
                        kind: "device".into(),
                        key: session.identity.clone(),
                        value: Some(serde_json::json!(format!("Laptop {index}"))),
                    }])
                    .await
                    .unwrap();
            }
            a.exchange().await.unwrap();
            let before = a
                .exchange_import(&session, folders[0].join(&names[0]))
                .await
                .unwrap();
            std::fs::copy(folders[0].join(&names[0]), folders[1].join(&names[0])).unwrap();
            b.exchange().await.unwrap();
            std::fs::copy(folders[1].join(&names[1]), folders[0].join(&names[1])).unwrap();
            a.exchange().await.unwrap();
            while session
                .documents
                .prune_history(&session.identity, &[], library::CONNECT_PAGE_SIZE)
                .await
                .unwrap()
                > 0
            {}
            *session.file_exchange.lock().await = None;
            a.exchange().await.unwrap();
            let after = a
                .exchange_import(&session, folders[0].join(&names[0]))
                .await
                .unwrap();
            assert!(
                std::fs::metadata(after.path()).unwrap().len()
                    < std::fs::metadata(before.path()).unwrap().len()
            );
            let compact = std::fs::read(folders[0].join(&names[0])).unwrap();
            a.exchange().await.unwrap();
            assert!(std::fs::read(folders[0].join(&names[0])).unwrap() == compact);
            for inputs in [host, guest] {
                inputs.receivers.visualizer.close();
                let playback = inputs.products.playback.transport.clone();
                tokio::task::spawn_blocking(move || playback.shutdown())
                    .await
                    .unwrap();
            }
        }))
    })
    .unwrap();
}

#[test]
fn webdav_exchange_leaves_an_unchanged_profile_file_untouched() {
    use axum::{body::Bytes, extract::State, http::StatusCode, response::IntoResponse};
    let root = tempfile::tempdir().unwrap();
    let diagnostics = diagnostics();
    app::with_runtime(|runtime| {
        runtime.block_on(Box::pin(async {
            type Files = Arc<Mutex<(BTreeMap<String, Vec<u8>>, usize, bool)>>;
            let file: Files = Arc::new(Mutex::new((BTreeMap::new(), 0, false)));
            let router = axum::Router::new().fallback(axum::routing::any(
                |State(files): State<Files>, method: axum::http::Method, uri: axum::http::Uri, body: Bytes| async move {
                    let path = uri.path().to_owned();
                    let mut files = files.lock().unwrap();
                    match method.as_str() {
                        "MKCOL" if path == "/profiles" => {
                            files.2 = true;
                            StatusCode::CREATED.into_response()
                        }
                        "PROPFIND" => {
                            if path.trim_end_matches('/') == "/profiles" && !files.2 {
                                return StatusCode::NOT_FOUND.into_response();
                            }
                            let mut entries = format!("<d:response><d:href>{path}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>");
                            if path.trim_end_matches('/') == "/profiles" {
                                for name in files.0.keys() {
                                    entries.push_str(&format!("<d:response><d:href>{name}</d:href><d:propstat><d:prop><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"));
                                }
                            }
                            (StatusCode::MULTI_STATUS, format!("<d:multistatus xmlns:d=\"DAV:\">{entries}</d:multistatus>")).into_response()
                        }
                        "GET" => match files.0.get(&path) {
                            Some(bytes) => bytes.clone().into_response(),
                            None => StatusCode::NOT_FOUND.into_response(),
                        },
                        "PUT" => {
                            assert!(path.starts_with("/profiles/"));
                            assert!(files.2, "create the chosen folder before uploading");
                            files.0.insert(path, body.to_vec());
                            files.1 += 1;
                            StatusCode::CREATED.into_response()
                        }
                        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
                    }
                }
            )).with_state(file.clone());
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
                url: format!("http://{address}/"),
                alternate_urls: vec![], folders: vec![], username: String::new(), domain: String::new(),
                authentication: sources::FileAuthentication::Anonymous,
                trust_invalid_certificate: false, certificate_pem: None, require_smb_encryption: false,
            };
            let source_id = inputs.products.source.configure_file_integration(
                crate::runtime::source::SourceSetup::WebDav {
                    name: "Profile storage".into(), settings,
                    credentials: sources::FileCredentials { secret: String::new(), headers: vec![] },
                },
            ).recv().await.unwrap().unwrap();
            assert!(inputs.products.source.list_sources().sources.is_empty());
            assert!(inputs.products.source.selected_library().is_none());
            assert_eq!(inputs.products.source.file_integrations(), vec![crate::source::FileIntegration {
                id: source_id.clone(), name: "Profile storage".into(), kind: "webdav".into(), music_source: false,
            }]);
            let preset = inputs.products.source.file_integration_settings(&source_id).unwrap().unwrap();
            inputs.products.source.update_file_integration(crate::runtime::source::SourceSettingsChange::Files {
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
            execute(owner, Action::Create).await.unwrap();
            execute(
                owner,
                Action::Destination {
                    destination: Some(portable::Destination::Remote {
                        source_id,
                        path: "profiles".into(),
                    }),
                },
            )
            .await
            .unwrap();
            while owner.database.connect_seed_page().await.unwrap() {}
            while owner.synchronize().await.unwrap() {}
            let session = owner.active().await.unwrap();
            while session.documents.prune_history(&session.identity, &[], library::CONNECT_PAGE_SIZE).await.unwrap() > 0 {}
            owner.exchange().await.unwrap();
            let uploads = file.lock().unwrap().1;
            assert!(uploads > 0);
            for _ in 0..3 {
                owner.exchange().await.unwrap();
            }
            assert_eq!(
                file.lock().unwrap().1,
                uploads,
                "reading the same profile must not PUT it again, even without an ETag"
            );
            execute(
                owner,
                Action::Rename {
                    name: "Renamed".into(),
                },
            )
            .await
            .unwrap();
            owner.exchange().await.unwrap();
            assert_eq!(file.lock().unwrap().1, uploads + 1, "local edits must reach the file");
            owner.exchange().await.unwrap();
            assert_eq!(file.lock().unwrap().1, uploads + 1);
            owner.close_network().await.unwrap();
            inputs.receivers.visualizer.close();
            let playback = inputs.products.playback.transport.clone();
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
        || Ok(Box::new(IdleBackend) as Box<dyn PlaybackBackend>),
        Vec::new,
        Arc::new(|_, _| {}),
        Arc::new(|_, _| {}),
    )
    .await
    .unwrap();
    inputs
        .products
        .source
        .change_secret_storage(secrets::SecretStorageMode::ConfigFile)
        .recv()
        .await
        .unwrap()
        .unwrap();
    inputs.products.connect.secrets.replace(secret_store);
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

async fn private(inputs: &RuntimeInputs, enabled: bool) {
    let settings = inputs.settings.clone();
    tokio::task::spawn_blocking(move || {
        let mut ui = settings.load();
        ui.private_mode = enabled;
        settings.save(&ui).unwrap();
    })
    .await
    .unwrap();
}

async fn queue_one(inputs: &RuntimeInputs) {
    inputs.products.playback.queue.insert(
        library::QueueInput::Items(vec![(
            library::QueueItem::direct(
                "https://example.invalid/music.flac",
                "Music",
                "Artist",
                "Album",
                180_000,
            ),
            library::QueueProvenance::Manual,
        )]),
        library::QueueReorderTarget::End,
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while inputs
            .products
            .playback
            .updates
            .current()
            .is_none_or(|view| view.queue.total != 1)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
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
            && status.devices.iter().any(|device| {
                Some(&device.id) == host.status().identity.as_ref() && device.enrolled
            })
    })
    .await;
    assert!(guest.status().settings.setup_pending);
    assert!(!guest.status().receiving_collection);
    assert!(!guest.synchronize().await.unwrap());
    guest.exchange().await.unwrap();
}

async fn pair(host: &Arc<ConnectOwner>, guest: &Arc<ConnectOwner>) {
    prepare_pair(host, guest).await;
    execute(guest, Action::FinishSetup).await.unwrap();
    assert!(!guest.status().settings.setup_pending);
}

async fn failed_verified_adoption_clears_pairing_progress_and_can_retry(
    root: &std::path::Path,
    diagnostics: crate::runtime::DiagnosticsHandle,
) {
    let secrets: Arc<dyn SecretStore> = Arc::new(secrets::MemorySecretStore::new());
    let host = device(root, "host", diagnostics.clone(), Arc::clone(&secrets)).await;
    let guest = device(root, "guest", diagnostics, secrets).await;
    let a = &host.products.connect;
    let b = &guest.products.connect;
    execute(a, Action::Enable { enabled: true }).await.unwrap();
    let settings = a.settings_apply.clone();
    let secrets = Arc::clone(&a.secrets);
    let valid = tokio::task::spawn_blocking(move || settings.connect_records(&secrets))
        .await
        .unwrap()
        .unwrap()
        .into_iter()
        .find(|record| record.kind == "preference" && record.key == "playback")
        .unwrap();
    let mut invalid = valid.clone();
    invalid.value = Some(serde_json::json!(false));
    let documents = a.active().await.unwrap().documents.clone();
    {
        let _sync = a.sync.lock().await;
        documents
            .write_records(std::slice::from_ref(&invalid))
            .await
            .unwrap();
        // Keep the host's actual settings acknowledged so exporting does
        // not overwrite the invalid document before the guest imports it.
        documents
            .acknowledge_projection(std::slice::from_ref(&valid))
            .await
            .unwrap();
    }
    approve_pair(a, b).await;
    let failed = wait(b, |status| {
        status.error.as_deref() == Some("Connect playback preferences are invalid")
    })
    .await;
    assert!(!failed.connecting);
    assert!(failed.pairing.is_none());
    assert!(!failed.receiving_collection);
    assert!(!b.joining.load(std::sync::atomic::Ordering::Acquire));
    assert!(failed.settings.adopting);
    {
        let _sync = a.sync.lock().await;
        documents
            .acknowledge_projection(std::slice::from_ref(&invalid))
            .await
            .unwrap();
        documents
            .write_records(std::slice::from_ref(&valid))
            .await
            .unwrap();
    }
    pair(a, b).await;
    assert!(!b.status().settings.adopting);
    assert!(b.status().error.is_none());
    assert!(!b.joining.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(a.status().settings.profile, b.status().settings.profile);
    for inputs in [guest, host] {
        inputs.products.connect.close_network().await.unwrap();
        inputs.receivers.visualizer.close();
        let playback = inputs.products.playback.transport.clone();
        tokio::task::spawn_blocking(move || playback.shutdown())
            .await
            .unwrap();
    }
}

async fn collection_media_downloads_and_reuses_configured_folders(
    host: &Arc<ConnectOwner>,
    guest: &Arc<ConnectOwner>,
    source: &sources::SourceId,
    root: &std::path::Path,
    events: &async_channel::Receiver<downloads::DownloadEvent>,
) {
    let folder = root.join("native-audio");
    std::fs::create_dir(&folder).unwrap();
    let original = folder.join("track.wav");
    let samples = 44_100u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + samples * 2).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    bytes.extend_from_slice(&44_100u32.to_le_bytes());
    bytes.extend_from_slice(&88_200u32.to_le_bytes());
    bytes.extend_from_slice(b"\x02\0\x10\0data");
    bytes.extend_from_slice(&(samples * 2).to_le_bytes());
    bytes.resize(44 + samples as usize * 2, 0);
    std::fs::write(&original, &bytes).unwrap();
    let uri = url::Url::from_file_path(&original).unwrap().to_string();
    let mut scan = library::Scan::begin(&host.database, source.as_str(), "Local", "local", None)
        .await
        .unwrap();
    scan.write_track(
        "native-track",
        None,
        "Track",
        "track",
        "Album",
        "Artist",
        "track",
        1000,
        1,
        1,
        None,
        None,
        None,
        Some(&uri),
        Some("wav"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        None,
        None,
        None,
        None,
        None,
        original.to_str(),
        [1; 32],
    )
    .await
    .unwrap();
    scan.write_local_files(&[(
        library::LocalFileWrite {
            path: original.to_str().unwrap().into(),
            root: folder.to_str().unwrap().into(),
            relative_path: "track.wav".into(),
            kind: library::LocalFileKind::Media,
            size_bytes: Some(bytes.len() as i64),
            mtime_ns: 0,
            device_id: None,
            inode: None,
            native_id: None,
            picture_index: None,
            revision: Some("1".into()),
            parse_version: Some(1),
            state: library::LocalFileState::Accepted,
        },
        Vec::new(),
    )])
    .await
    .unwrap();
    scan.finish().await.unwrap();
    while host.synchronize().await.unwrap() {}
    tokio::time::timeout(Duration::from_secs(10), async {
        while guest
            .database
            .connect_track_reference(&uri)
            .await
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        guest
            .database
            .connect_original_file(&uri)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        host.database
            .connect_local_file(&uri)
            .await
            .unwrap()
            .is_none()
    );
    // Adding a Local track must acquire its media without playing or asking to download it.
    tokio::time::timeout(Duration::from_secs(15), async {
        while guest
            .database
            .connect_local_file(&uri)
            .await
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let copy = guest
        .database
        .connect_local_file(&uri)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(copy, original);
    assert_eq!(std::fs::read(&copy).unwrap(), bytes);
    let queue = guest
        .database
        .read_queue(library::QueueReadRequest::Capture {
            input: Box::new(library::QueueInput::Uris {
                order: Arc::from([uri.clone()]),
                context_id: "local-tracks".into(),
                source_start: 0,
            }),
            anchor_index: 0,
            random_start: None,
            shuffled: None,
        })
        .await
        .unwrap();
    let occurrence = &queue.occurrences[0];
    guest.resolve_media(occurrence).await.unwrap();
    let stream = crate::playback::prepare_stream(
        &guest.database,
        playback::StreamRequest::new(uri.clone(), playback::StreamQuality::Original),
        |_| panic!("a Local track must resolve to its local copy"),
    )
    .await
    .unwrap();
    assert_eq!(library::file_media_path(stream.uri()), Some(copy.clone()));
    playback::QueueCommandPort::play(
        guest.playback.as_ref(),
        playback::PlayRequest::captured(
            library::QueueInput::Uris {
                order: Arc::from([uri.clone()]),
                context_id: "local-tracks".into(),
                source_start: 0,
            },
            0,
            playback::QueuePlacement::Now,
            false,
        ),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = guest.playback.current_media();
            if current.is_some_and(|item| item.media_uri == uri && item.id.run.is_some()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        execute(
            guest,
            Action::Encoding {
                encoding: Encoding::Mp3,
                confirm: false
            }
        )
        .await
        .unwrap_err(),
        "Changing this will redownload all local files."
    );
    assert_eq!(guest.status().settings.encoding, Encoding::Original);
    let reference = guest
        .database
        .connect_track_reference(&uri)
        .await
        .unwrap()
        .unwrap();
    let root_id = reference["root_id"].as_str().unwrap().to_owned();
    let destination = folder.parent().unwrap().join("downloaded-music");
    execute(
        guest,
        Action::Folder {
            source: source.to_string(),
            root_id: Some(root_id.clone()),
            path: destination.clone(),
        },
    )
    .await
    .unwrap();
    let relocated = destination.join("track.wav");
    tokio::time::timeout(Duration::from_secs(15), async {
        while guest.database.connect_local_file(&uri).await.unwrap() != Some(relocated.clone())
            || copy.exists()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(&relocated).unwrap(), bytes);
    assert_eq!(std::fs::read(&original).unwrap(), bytes);
    // The selected folder also receives new automatic transfers, without playback.
    downloads::ConnectDownload::remove(guest.as_ref(), &uri)
        .await
        .unwrap();
    guest.source.connect_media_changed();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !relocated.is_file() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for encoding in [Encoding::Mp3, Encoding::Original] {
        execute(
            guest,
            Action::Encoding {
                encoding,
                confirm: true,
            },
        )
        .await
        .unwrap();
        let completed = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let files = guest.database.connect_media_files(&uri).await.unwrap();
                if files.len() == 1
                    && files[0].encoding == encoding.name()
                    && library::file_media_path(&files[0].path).is_some_and(|p| p.is_file())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        if completed.is_err() {
            let mut updates = Vec::new();
            while let Ok(event) = events.try_recv() {
                updates.push(event);
            }
            panic!(
                "quality {encoding:?}: files={:?}, status={:?}, downloads={updates:?}",
                guest.database.connect_media_files(&uri).await.unwrap(),
                guest.status()
            );
        }
        assert_eq!(std::fs::read(&original).unwrap(), bytes);
    }
    assert_eq!(std::fs::read(&relocated).unwrap(), bytes);
    execute(
        guest,
        Action::Folder {
            source: source.to_string(),
            root_id: Some(root_id),
            path: folder,
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while guest.database.connect_local_file(&uri).await.unwrap() != Some(original.clone())
            || relocated.exists()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        guest.database.connect_local_file(&uri).await.unwrap(),
        Some(original.clone())
    );
    assert!(
        !guest
            .database
            .connect_media_file(&uri, "original")
            .await
            .unwrap()
            .unwrap()
            .managed
    );
    assert_eq!(
        guest
            .database
            .playback_access(&uri)
            .await
            .unwrap()
            .unwrap()
            .0,
        uri
    );
    assert_eq!(std::fs::read(original).unwrap(), bytes);
    playback::QueueCommandPort::clear(guest.playback.as_ref(), true);
    tokio::time::timeout(Duration::from_secs(5), async {
        while guest.playback.queue_content_id().await.unwrap()
            != library::queue_content_id(&[], &[])
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn profiles_adopt_only_after_verification_and_offline_edits_survive_enrollment() {
    let root = tempfile::tempdir().unwrap();
    let diagnostics = diagnostics();
    app::with_runtime(|runtime| {
        runtime.block_on(Box::pin(async {
            let secret_store: Arc<dyn SecretStore> = Arc::new(secrets::MemorySecretStore::new());
            let host = device(
                root.path(),
                "host",
                diagnostics.clone(),
                Arc::clone(&secret_store),
            )
            .await;
            let guest = device(
                root.path(),
                "guest",
                diagnostics.clone(),
                Arc::clone(&secret_store),
            )
            .await;
            let a = &host.products.connect;
            let b = &guest.products.connect;
            let shared_folder = root.path().join("shared-music");
            std::fs::create_dir(&shared_folder).unwrap();
            let shared_source = host
                .products
                .source
                .configure_source(crate::runtime::source::SourceSetup::Local {
                    roots: vec![shared_folder],
                })
                .recv()
                .await
                .unwrap()
                .unwrap()
                .source_id;
            private(&host, true).await;
            assert!(!a.status().settings.enabled);
            let enabled = execute(a, Action::Enable { enabled: true }).await.unwrap();
            assert!(enabled.settings.enabled);
            assert!(enabled.invitation.is_some());
            assert_ne!(
                enabled.profile_status,
                localization::tr("Connect is not enabled")
            );
            let created_profile = a.status().settings.profile;
            let repeated_track = "https://example.invalid/repeated.flac".to_owned();
            let (shared_playlist, shared_playlist_id) = a
                .database
                .create_playlist(
                    None,
                    "Shared duplicates",
                    &[repeated_track.clone(), repeated_track.clone()],
                )
                .await
                .unwrap()
                .unwrap();
            let selected = execute(
                a,
                Action::Destination {
                    destination: Some(portable::Destination::Local {
                        path: PathBuf::new(),
                    }),
                },
            )
            .await
            .unwrap();
            assert_eq!(
                selected.settings.destination,
                Some(a.local_destination(created_profile.as_deref().unwrap()))
            );
            let local_file = selected.local_folder.clone().unwrap();
            let remote = sources::SourceId::new("storage-account");
            assert_eq!(selected.storage_path(None), local_file.to_string_lossy());
            assert_eq!(selected.storage_path(Some(&remote)), "Rufin Connect");
            let mut remote_destination = selected.clone();
            remote_destination.settings.destination = Some(portable::Destination::Remote {
                source_id: remote.clone(),
                path: "profiles/music".into(),
            });
            assert_eq!(
                remote_destination.storage_path(Some(&remote)),
                "profiles/music"
            );
            assert_eq!(
                remote_destination.storage_path(None),
                local_file.to_string_lossy()
            );
            a.exchange().await.unwrap();
            // The first export can finish capturing the initial profile.
            a.exchange().await.unwrap();
            let portable::Destination::Local { path } = a.status().settings.destination.unwrap()
            else {
                panic!("local destination");
            };
            let path = path.join(format!("{}.rufin-connect", a.status().identity.unwrap()));
            let before = std::fs::read(&path).unwrap();
            a.exchange().await.unwrap();
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "unchanged profiles must not be re-encrypted"
            );
            *a.active().await.unwrap().file_exchange.lock().await = None;
            a.exchange().await.unwrap();
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "an existing complete profile must remain untouched after reopening"
            );
            // Opening Connect with a populated profile must not leave pairing
            // queued behind publication or replay of its catalog.
            let records = (0..1024)
                .map(|index| ConnectRecord {
                    kind: "future-feature".into(),
                    key: format!("catalog-{index}"),
                    value: Some(serde_json::json!({"title":format!("Track {index}")})),
                })
                .collect::<Vec<_>>();
            a.active()
                .await
                .unwrap()
                .documents
                .write_records(&records)
                .await
                .unwrap();
            execute(a, Action::Enable { enabled: true }).await.unwrap();
            assert_eq!(a.status().settings.profile, created_profile);
            execute(b, Action::Create).await.unwrap();
            assert!(!a.status().settings.established);
            assert!(!b.status().settings.established);
            let guest_profile = b.status().settings.profile;
            tokio::time::timeout(
                Duration::from_secs(3),
                execute(
                    b,
                    Action::Join {
                        invitation: ConnectNetwork::new_profile_id(),
                        replace: true,
                    },
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(b.status().connecting);
            tokio::time::timeout(Duration::from_secs(3), execute(b, Action::CancelPairing))
                .await
                .unwrap()
                .unwrap();
            assert!(!b.status().connecting);
            assert_eq!(b.status().settings.profile, guest_profile);
            assert_eq!(b.status().completed_pairings, 0);
            let (_, old_playlist_id) = b
                .database
                .create_playlist(
                    None,
                    "Guest playlist",
                    std::slice::from_ref(&repeated_track),
                )
                .await
                .unwrap()
                .unwrap();
            assert_ne!(
                a.status().identity,
                b.status().identity,
                "separate installations sharing an OS secret service need separate identities"
            );
            queue_one(&guest).await;
            let originals = root.path().join("originals");
            std::fs::create_dir(&originals).unwrap();
            let original_file = originals.join("original.txt");
            std::fs::write(&original_file, b"untouched").unwrap();
            let old_source = guest
                .products
                .source
                .configure_source(crate::runtime::source::SourceSetup::Local {
                    roots: vec![originals],
                })
                .recv()
                .await
                .unwrap()
                .unwrap()
                .source_id;
            let original = b.status().settings.profile.unwrap();
            let invitation = a.status().invitation.unwrap();
            assert!(
                execute(
                    b,
                    Action::Join {
                        invitation: invitation.clone(),
                        replace: false
                    }
                )
                .await
                .is_err()
            );
            assert_eq!(
                b.status().settings.profile.as_deref(),
                Some(original.as_str())
            );
            let sync = b.sync.lock().await;
            let joining = Arc::clone(b);
            let join = tokio::spawn(async move {
                joining
                    .execute(Action::Join {
                        invitation,
                        replace: true,
                    })
                    .await
            });
            tokio::time::timeout(Duration::from_secs(3), async {
                while !b.joining.load(std::sync::atomic::Ordering::Acquire) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(3), join)
                .await
                .expect("starting pairing must not wait for catalog projection")
                .unwrap()
                .unwrap();
            let pairing = wait(b, |status| status.pairing.is_some())
                .await
                .pairing
                .unwrap();
            drop(sync);
            assert_eq!(
                b.status().settings.profile.as_deref(),
                Some(original.as_str())
            );
            assert!(!guest.settings.load().private_mode);
            assert_eq!(
                guest
                    .products
                    .playback
                    .updates
                    .current()
                    .unwrap()
                    .queue
                    .total,
                1
            );
            execute(
                b,
                Action::Pair {
                    session: pairing.session,
                    approve: false,
                },
            )
            .await
            .unwrap();
            wait(a, |status| status.pairing.is_none()).await;
            assert_eq!(
                b.status().settings.profile.as_deref(),
                Some(original.as_str())
            );
            let remote_file = |path: &str| portable::Destination::Remote {
                source_id: sources::SourceId::new("shared-webdav"),
                path: path.into(),
            };
            execute(
                a,
                Action::Destination {
                    destination: Some(remote_file("profile.connect")),
                },
            )
            .await
            .unwrap();
            prepare_pair(a, b).await;
            let versions = b
                .active()
                .await
                .unwrap()
                .documents
                .versions(&["future-feature:catalog-0".into()])
                .await
                .unwrap();
            assert_eq!(
                versions[0].revision, 0,
                "collection transfer waits for setup"
            );
            assert!(guest.products.source.selected_library().is_none());
            execute(
                b,
                Action::Encoding {
                    encoding: Encoding::Mp3,
                    confirm: false,
                },
            )
            .await
            .unwrap();
            let reused_folder = root.path().join("reused-music");
            execute(
                b,
                Action::Folder {
                    source: shared_source.to_string(),
                    root_id: Some("music".into()),
                    path: reused_folder.clone(),
                },
            )
            .await
            .unwrap();
            execute(b, Action::FinishSetup).await.unwrap();
            assert_eq!(b.status().settings.encoding, Encoding::Mp3);
            assert_eq!(
                b.status().settings.folders[&format!("{shared_source}/music")],
                reused_folder
            );
            assert!(!b.status().settings.setup_pending);
            assert!(a.status().settings.established);
            assert!(b.status().settings.established);
            assert!(b.status().completed_pairings > 0);
            // An empty source must not become the automatic landing page while
            // the collection is still being received.
            assert!(guest.products.source.selected_library().is_none());
            let cancellation = library::ReadCancellation::new();
            let received_playlist = tokio::time::timeout(Duration::from_secs(90), async {
                loop {
                    if let Some(playlist) = b
                        .database
                        .playlist_key_by_identity(None, &shared_playlist_id, &cancellation)
                        .await
                        .unwrap()
                    {
                        let media = b
                            .database
                            .playlist_media_uri_order(playlist, None, &cancellation)
                            .await
                            .unwrap();
                        if media == [repeated_track.clone(), repeated_track.clone()] {
                            break playlist;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            assert!(
                b.database
                    .playlist_key_by_identity(None, &old_playlist_id, &cancellation)
                    .await
                    .unwrap()
                    .is_none()
            );
            let original_occurrences = b
                .database
                .playlist_entry_order(
                    received_playlist,
                    None,
                    library::PlaylistEntrySort::Position,
                    false,
                    "",
                    &cancellation,
                )
                .await
                .unwrap();
            assert_eq!(original_occurrences.len(), 2);
            assert_ne!(original_occurrences[0], original_occurrences[1]);
            assert_eq!(
                a.database
                    .add_playlist_media(
                        None,
                        shared_playlist,
                        std::slice::from_ref(&repeated_track),
                        false
                    )
                    .await
                    .unwrap(),
                1
            );
            a.synchronize().await.unwrap();
            let added_occurrences = tokio::time::timeout(Duration::from_secs(90), async {
                loop {
                    let occurrences = b
                        .database
                        .playlist_entry_order(
                            received_playlist,
                            None,
                            library::PlaylistEntrySort::Position,
                            false,
                            "",
                            &cancellation,
                        )
                        .await
                        .unwrap();
                    if occurrences.len() == 3 {
                        break occurrences;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(&added_occurrences[..2], original_occurrences);
            assert_eq!(
                b.database
                    .playlist_media_uri_order(received_playlist, None, &cancellation)
                    .await
                    .unwrap(),
                vec![repeated_track.clone(); 3]
            );
            assert_eq!(
                b.status().settings.destination,
                Some(remote_file("profile.connect"))
            );
            execute(
                b,
                Action::Destination {
                    destination: Some(remote_file("renamed.connect")),
                },
            )
            .await
            .unwrap();
            wait(a, |status| {
                status.settings.destination == Some(remote_file("renamed.connect"))
            })
            .await;
            execute(a, Action::Destination { destination: None })
                .await
                .unwrap();
            wait(b, |status| {
                matches!(
                    status.settings.destination,
                    Some(portable::Destination::Local { .. })
                )
            })
            .await;
            let local_file = root.path().join("guest-profile.connect");
            execute(
                b,
                Action::Destination {
                    destination: Some(portable::Destination::Local {
                        path: local_file.clone(),
                    }),
                },
            )
            .await
            .unwrap();
            a.synchronize().await.unwrap();
            assert!(matches!(
                a.status().settings.destination,
                Some(portable::Destination::Local { .. })
            ));
            assert_eq!(
                b.status().settings.destination,
                Some(portable::Destination::Local { path: local_file })
            );
            execute(b, Action::Destination { destination: None })
                .await
                .unwrap();
            assert_eq!(a.status().settings.profile, b.status().settings.profile);
            assert!(
                guest.settings.load().private_mode,
                "join must not echo guest defaults over the host"
            );
            assert!(host.settings.load().private_mode);
            assert_eq!(
                guest
                    .products
                    .playback
                    .updates
                    .current()
                    .unwrap()
                    .queue
                    .total,
                0
            );
            assert_eq!(std::fs::read(&original_file).unwrap(), b"untouched");
            assert!(guest.products.source.configuration(&old_source).is_none());
            assert_eq!(
                b.connected_network()
                    .await
                    .unwrap()
                    .members()
                    .await
                    .unwrap()
                    .len(),
                2
            );

            let retained_profile = b.status().settings.profile;
            let retained_identity = b.status().identity;
            execute(b, Action::Enable { enabled: false }).await.unwrap();
            assert!(!b.status().settings.enabled);
            assert!(b.network.lock().await.is_none());
            assert_eq!(b.status().settings.profile, retained_profile);
            assert_eq!(b.status().identity, retained_identity);
            execute(
                b,
                Action::Rename {
                    name: "Offline Laptop".into(),
                },
            )
            .await
            .unwrap();
            b.synchronize().await.unwrap();
            assert!(
                !b.active()
                    .await
                    .unwrap()
                    .documents
                    .changes(0, library::CONNECT_PAGE_SIZE)
                    .await
                    .unwrap()
                    .is_empty()
            );
            let offline_export = b.export().await.unwrap();
            assert!(offline_export.path().metadata().unwrap().len() > 0);
            assert!(
                b.network.lock().await.is_none(),
                "export must not resume networking"
            );
            let saved: ConnectSettings =
                serde_json::from_slice(&std::fs::read(b.directory.join("device.json")).unwrap())
                    .unwrap();
            assert!(!saved.enabled);
            assert_eq!(saved.profile, retained_profile);
            execute(b, Action::Enable { enabled: true }).await.unwrap();
            assert!(b.status().settings.enabled);
            assert_eq!(b.status().identity, retained_identity);
            assert_eq!(b.status().settings.profile, retained_profile);
            assert_eq!(
                b.connected_network()
                    .await
                    .unwrap()
                    .members()
                    .await
                    .unwrap()
                    .len(),
                2
            );
            wait(a, |status| {
                status
                    .devices
                    .iter()
                    .any(|device| device.name == "Offline Laptop")
            })
            .await;

            let profile = a.status().settings.profile.unwrap();
            let key = a
                .secret(file_key(&a.identity_reference().unwrap(), &profile))
                .await
                .unwrap()
                .unwrap();
            let exported = root.path().join("profile.rufin-connect");
            execute(
                a,
                Action::Export {
                    source_id: None,
                    path: exported.clone(),
                },
            )
            .await
            .unwrap();
            let offline = device(root.path(), "offline", diagnostics.clone(), secret_store).await;
            let c = &offline.products.connect;
            execute(
                c,
                Action::Import {
                    source_id: None,
                    path: exported.clone(),
                    replace: true,
                    key: Some(key.clone()),
                },
            )
            .await
            .unwrap();
            assert!(offline.settings.load().private_mode);
            assert_ne!(
                c.status().identity,
                a.status().identity,
                "files never clone device credentials"
            );
            private(&offline, false).await;
            c.synchronize().await.unwrap();
            assert_eq!(
                c.status().profile_status,
                localization::tr("Connect is not enabled")
            );
            assert!(!c.status().settings.enabled);
            assert!(c.network.lock().await.is_none());
            assert!(
                !c.active()
                    .await
                    .unwrap()
                    .documents
                    .changes(0, library::CONNECT_PAGE_SIZE)
                    .await
                    .unwrap()
                    .is_empty()
            );
            execute(
                c,
                Action::Import {
                    source_id: None,
                    path: exported.clone(),
                    replace: false,
                    key: None,
                },
            )
            .await
            .unwrap();
            assert!(
                !offline.settings.load().private_mode,
                "an older snapshot must preserve newer local edits"
            );
            queue_one(&offline).await;
            let corrupt = root.path().join("invalid.rufin-connect");
            let bytes = std::fs::read(&exported).unwrap();
            std::fs::write(&corrupt, &bytes[..bytes.len() - 1]).unwrap();
            assert!(
                execute(
                    c,
                    Action::Import {
                        source_id: None,
                        path: corrupt,
                        replace: true,
                        key: Some(key)
                    }
                )
                .await
                .is_err()
            );
            assert_eq!(
                c.status().settings.profile.as_deref(),
                Some(profile.as_str())
            );
            assert!(!offline.settings.load().private_mode);
            assert_eq!(
                offline
                    .products
                    .playback
                    .updates
                    .current()
                    .unwrap()
                    .queue
                    .total,
                1
            );
            let retained = root.path().join("offline.rufin-connect");
            execute(
                c,
                Action::Export {
                    source_id: None,
                    path: retained.clone(),
                },
            )
            .await
            .unwrap();
            execute(c, Action::Leave).await.unwrap();
            execute(
                c,
                Action::Import {
                    source_id: None,
                    path: retained,
                    replace: true,
                    key: None,
                },
            )
            .await
            .unwrap();
            assert!(
                !offline.settings.load().private_mode,
                "leaving keeps authorized profile-file access material"
            );
            // A device that joined can invite the next device with the same controls.
            pair(b, c).await;
            assert!(b.status().settings.established);
            assert!(c.status().settings.established);
            assert!(
                !offline.settings.load().private_mode,
                "membership completion must preserve adopted edits"
            );
            c.synchronize().await.unwrap();
            let delivered = tokio::time::timeout(Duration::from_secs(90), async {
                while host.settings.load().private_mode {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            delivered.unwrap();
            while offline.receivers.source.try_recv().is_ok() {}
            c.synchronize().await.unwrap();
            assert!(
                offline.receivers.source.try_recv().is_err(),
                "a no-op profile pass must not invalidate the source view"
            );

            collection_media_downloads_and_reuses_configured_folders(
                a,
                c,
                &shared_source,
                root.path(),
                &offline.receivers.downloads,
            )
            .await;
            let selected = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(selected) = offline.products.source.selected_library() {
                        break selected;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("received tracks should select a library automatically");
            assert_eq!(selected.source_id, shared_source);
            let home = selected
                .database
                .home_page(
                    selected.source_key,
                    selected.music_folder_key,
                    0,
                    0,
                    &[crate::settings::HomeBlockKind::Explore],
                    &library::ReadCancellation::new(),
                )
                .await
                .unwrap();
            assert!(!home.explore.is_empty());
            let bytes = b"original direct queue media";
            let path = root.path().join("direct.flac");
            std::fs::write(&path, bytes).unwrap();
            let uri = url::Url::from_file_path(&path).unwrap().to_string();
            let mut item = library::QueueItem::direct(&uri, "Direct", "Artist", "Album", 180_000);
            item.source_format = Some("flac".into());
            host.products
                .playback
                .queue
                .play(playback::PlayRequest::one(
                    item.clone(),
                    playback::QueuePlacement::Now,
                ));
            let peer = a.status().identity.unwrap();
            let busy = c.actions.lock().await;
            tokio::time::timeout(
                Duration::from_secs(3),
                execute(
                    c,
                    Action::Control {
                        peer: peer.clone(),
                        command: Control::Volume { value: 0.4 },
                    },
                ),
            )
            .await
            .unwrap()
            .unwrap();
            drop(busy);
            let header = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Ok(value) = c.request(&peer, Request::Continuation).await {
                        break serde_json::from_value::<playback::ContinuationHeader>(value)
                            .unwrap();
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(c.continuation_offer().await.unwrap().unwrap().id, peer);
            offline.products.playback.queue.insert(
                library::QueueInput::Items(vec![(item.clone(), library::QueueProvenance::Manual)]),
                library::QueueReorderTarget::End,
            );
            tokio::time::timeout(Duration::from_secs(5), async {
                let expected = a.playback.queue_content_id().await.unwrap();
                while c.playback.queue_content_id().await.unwrap() != expected {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(
                c.continuation_offer().await.unwrap().is_none(),
                "the same queue must not produce another continuation offer"
            );
            c.fetch_direct_continuation(&peer, &item, &header.current)
                .await
                .unwrap();
            let downloaded = c.database.connect_local_file(&uri).await.unwrap().unwrap();
            assert_eq!(std::fs::read(downloaded).unwrap(), bytes);
            let unknown = root.path().join("not-in-queue.flac");
            std::fs::write(&unknown, b"private").unwrap();
            assert!(
                c.request(
                    &peer,
                    Request::Media {
                        id: "unrelated".into(),
                        uri: url::Url::from_file_path(unknown).unwrap().into(),
                        encoding: Encoding::Original,
                        occurrence: None
                    }
                )
                .await
                .is_err()
            );
            assert!(
                a.answer(
                    "previous-profile",
                    &c.status().identity.unwrap(),
                    &serde_json::to_vec(&Request::Presence).unwrap()
                )
                .await
                .is_err()
            );
            offline
                .products
                .source
                .change_secret_storage(secrets::SecretStorageMode::SystemKeyring)
                .recv()
                .await
                .unwrap()
                .unwrap();
            assert!(c.status().settings.profile.is_none());
            assert!(c.session.read().await.is_none());
            assert!(c.network.lock().await.is_none());
            // Removing the guest disconnects it automatically, even with a file
            // exchange pending, without that exchange adopting the old profile.
            {
                b.exchange().await.unwrap();
                let previous = b.active().await.unwrap();
                let file_guard = previous.file_exchange.lock().await;
                let exchange = b.exchange();
                tokio::pin!(exchange);
                assert!(futures_util::poll!(&mut exchange).is_pending());
                execute(
                    a,
                    Action::Remove {
                        peer: b.status().identity.unwrap(),
                    },
                )
                .await
                .unwrap();
                tokio::time::timeout(Duration::from_secs(5), async {
                    let mut status = b.subscribe();
                    status
                        .wait_for(|state| state.settings.profile.is_none())
                        .await
                        .unwrap();
                })
                .await
                .unwrap();
                drop(file_guard);
                tokio::time::timeout(Duration::from_secs(3), exchange)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(b.status().settings.profile.is_none());
                assert!(!b.status().settings.enabled);
                assert!(!b.status().settings.established);
                assert!(b.status().error.is_none());
            }
            for inputs in [offline, guest, host] {
                inputs.products.connect.close_network().await.unwrap();
                inputs.receivers.visualizer.close();
                let playback = inputs.products.playback.transport.clone();
                tokio::task::spawn_blocking(move || playback.shutdown())
                    .await
                    .unwrap();
            }
            failed_verified_adoption_clears_pairing_progress_and_can_retry(
                &root.path().join("failed-adoption"),
                diagnostics,
            )
            .await;
        }))
    })
    .unwrap();
}
