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
            execute(owner, Action::Create).await.unwrap();
            let session = owner.active().await.unwrap();
            let identity = owner.status().identity;
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
            owner.close_network().await.unwrap();
            inputs.receivers.visualizer.close();
            let playback = inputs.products.playback.transport.clone();
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
                prepare_profile_snapshot(owner).await;
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
            assert!(after.metadata().unwrap().len() < before.metadata().unwrap().len());
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
            prepare_profile_snapshot(owner).await;
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

// Finish the writes that background startup and pruning would otherwise race
// against assertions about an unchanged profile file.
async fn prepare_profile_snapshot(owner: &Arc<ConnectOwner>) {
    loop {
        let seeded = owner.database.connect_seed_page().await.unwrap();
        let captured = owner.synchronize().await.unwrap();
        if !seeded && !captured {
            break;
        }
    }
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
