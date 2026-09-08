use super::*;
use secrets::SecretStore;

#[test]
fn queue_load_confirmation_waits_through_stop_and_the_wrong_duplicate() {
    use std::io::{Read, Write};
    for (state, received) in [
        (Some("paused"), "paused"),
        (Some("playing"), "buffering"),
        (None, "buffering"),
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for attributes in [
                "state=\"stopped\"",
                "state=\"paused\" machineIdentifier=\"server\" playQueueID=\"7\" playQueueItemID=\"500\" ratingKey=\"42\"",
                &format!(
                    "state=\"{received}\" machineIdentifier=\"server\" playQueueID=\"7\" playQueueItemID=\"501\" ratingKey=\"42\""
                ),
            ] {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    assert_eq!(socket.read(&mut byte).unwrap(), 1);
                    request.push(byte[0]);
                }
                assert!(String::from_utf8(request).unwrap().contains("wait=0"));
                let body = format!(
                    "<MediaContainer><Timeline type=\"music\" {attributes}/></MediaContainer>"
                );
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let client = PlexClient::new(&endpoint, "receiver", "controller").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let timeline = runtime
            .block_on(wait_for_plex_occurrence(
                &client,
                "server",
                7,
                501,
                state,
                &AtomicBool::new(false),
            ))
            .unwrap();
        assert_eq!(timeline.play_queue_item_id, Some(501));
        assert_eq!(timeline.state, received);
        worker.join().unwrap();
    }
}

#[test]
fn plex_queue_admission_retains_occurrences_and_rejects_mixed_sources() {
    let source = sources::SourceId::new("plex-test");
    let entry = |index, source: &sources::SourceId| library::QueueEntry {
        occurrence: OccurrenceId::new(format!("local:{index}")),
        media_uri: library::source_entity_uri(source, "track", "plex:track:42").into(),
        playlist_entry_id: None,
        provenance: playback::Provenance::Manual,
    };
    let entries = vec![entry(0, &source), entry(1, &source), entry(2, &source)];
    assert_eq!(queue_keys(&entries, &source).unwrap(), ["42", "42", "42"]);
    let mut mixed = entries.clone();
    mixed.push(entry(3, &sources::SourceId::new("another-profile")));
    assert!(queue_keys(&mixed, &source).is_err());
    assert_eq!(entries.len(), 3);
    assert_eq!(plex_occurrence(&occurrence(7, 18)), Some(18));
    assert_eq!(plex_occurrence(&OccurrenceId::new("local:18")), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_server_change_uses_the_configured_profile_and_preserves_unavailable_receiver() {
    struct Idle;
    impl PlaybackBackend for Idle {
        fn send(&mut self, _: playback::BackendCommand) -> Result<(), playback::BackendError> {
            Ok(())
        }
        fn drain_events(&mut self) -> Vec<playback::BackendEvent> {
            Vec::new()
        }
    }
    let configuration = |id: &str, server: &str, profile: &str| {
        sources::SourceConfiguration {
        source_id:sources::SourceId::new(id),kind:"plex".into(),name:id.into(),
        provider_payload:serde_json::json!({"version":1,"server_id":server,"profile_id":profile,"base_url":"http://127.0.0.1:1","address_override":null,"local":true,"relay":false,"owned":true,"trust_invalid_cert":false}).to_string(),
    }
    };
    let first = configuration("first", "server-a", "profile-a");
    let login=serde_json::json!({"client_id":"source-switch-fixture","token":"fixture-token","account_id":"user","profiles":{},"resources":{},"home_admin_subscription":false,"download_subscriptions":{}}).to_string();
    let root = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Handle::current();
    let settings = SettingsFile::open(root.path().join("settings.json")).unwrap();
    let credential_ref = crate::settings::CredentialRef::new("plex-fixture");
    settings
        .update(|stored| {
            stored.sources.configured = vec![crate::settings::ConfiguredSource {
                configuration: first.clone(),
                credential_ref: Some(credential_ref.clone()),
                music_folder_id: None,
                local_access: None,
                enable_half_stars: true,
            }];
            Ok(())
        })
        .unwrap();
    let database = Arc::new(
        Database::open(root.path().join("store.sqlite"))
            .await
            .unwrap(),
    );
    let memory = Arc::new(secrets::MemorySecretStore::new());
    memory
        .save_secret(
            &crate::settings::provider_secret_key(&credential_ref),
            &login,
        )
        .unwrap();
    let secrets = Arc::new(secrets::SwitchableSecretStore::new(memory));
    let artwork = artwork::Artwork::new(root.path().join("artwork"), runtime.clone()).unwrap();
    let downloads = downloads::Downloads::new(
        root.path().join("downloads"),
        database.as_ref().clone(),
        runtime.clone(),
        async_channel::unbounded().0,
        Vec::new(),
    );
    let bootstrap = SourceOwner::open_dormant(
        artwork.clone(),
        database.clone(),
        downloads,
        settings.clone(),
        secrets,
        runtime.clone(),
        crate::source::SourceOutputs {
            events: async_channel::unbounded().0,
            discovery: async_channel::unbounded().0,
        },
    );
    let source = bootstrap.owner;
    let waveform = WaveformOwner::new(
        runtime.clone(),
        async_channel::unbounded().0,
        root.path().join("playback"),
        false,
    );
    let stored = settings.load();
    let lyrics = LyricsService::new(
        database.as_ref().clone(),
        source.clone(),
        runtime.clone(),
        stored.ui.lyrics.clone(),
        true,
        async_channel::unbounded().0,
        root.path().join("dictionary"),
    );
    let scrobbler = Arc::new(tokio::task::block_in_place(|| {
        Scrobbler::new(
            database.as_ref().clone(),
            runtime.clone(),
            stored.scrobbling_runtime_settings(),
            true,
        )
        .unwrap()
    }));
    let (events, drain) = async_channel::bounded(1);
    let (visualizer, visualizer_drain) = async_channel::bounded(1);
    let owner = PlaybackOwner::new(
        database,
        settings,
        runtime,
        events,
        drain,
        visualizer,
        visualizer_drain,
        artwork,
        waveform,
        lyrics,
        Arc::new(|_, _| {}),
        Vec::new,
        scrobbler,
        || Ok(Box::new(Idle)),
    );
    owner.install_source_owner(&source);
    owner.start().await.unwrap();
    owner
        .settings
        .update(|stored| {
            let mut second = stored.sources.configured[0].clone();
            second.configuration = configuration("second", "server-b", "profile-b");
            let mut preferred = second.clone();
            preferred.configuration = configuration("preferred", "server-b", "profile-preferred");
            stored.sources.selected_source_id = Some(preferred.configuration.source_id.clone());
            stored.sources.configured.extend([second, preferred]);
            Ok(())
        })
        .unwrap();
    let source_owner = owner.source_owner().unwrap();
    let source = source_owner.client(&first.source_id).unwrap();
    let context = source.plex_companion_context().await.unwrap();
    let client = tokio::task::spawn_blocking(|| {
        PlexClient::new("http://127.0.0.1:1", "receiver", "controller").unwrap()
    })
    .await
    .unwrap();
    let cached = PlexQueueWindow {
        id: 7,
        version: 1,
        total: 0,
        offset: 0,
        selected_item_id: None,
        selected_offset: None,
        shuffled: false,
        items: Vec::new(),
    };
    let plex = PlexPlayback {
        client,
        connection: Mutex::new((source, context)),
        source_owner: Arc::downgrade(&source_owner),
        settings: owner.settings.clone(),
        output: PlaybackOutput::Remote(RemoteOutput {
            id: "receiver".into(),
            name: "receiver".into(),
            protocol: RemoteOutputProtocol::PlexCompanion,
        }),
        playback: owner.active().unwrap().playback,
        database: owner.database.clone(),
        queue: tokio::sync::Mutex::new(Some(cached)),
        timeline: Mutex::new(stopped_timeline()),
        commands: tokio::sync::Mutex::new(()),
        prior_volume: Mutex::new(None),
        cancelled: AtomicBool::new(false),
        observer: Mutex::new(None),
    };
    let mut timeline = stopped_timeline();
    timeline.machine_identifier = Some("server-b".into());
    {
        let _guard = plex.commands.lock().await;
        plex.resolve_timeline_source(&timeline).await.unwrap();
        assert_eq!(plex.connection().1.source_id.as_str(), "preferred");
        assert_eq!(plex.connection().1.profile_id, "profile-preferred");
        assert!(plex.queue.lock().await.is_none());
        timeline.machine_identifier = Some("unconfigured-server".into());
        assert!(plex.resolve_timeline_source(&timeline).await.is_err());
        assert_eq!(plex.connection().1.source_id.as_str(), "preferred");
    }
    assert_ne!(
        receiver_occurrence(&first.source_id, 7, 1),
        receiver_occurrence(&sources::SourceId::new("preferred"), 7, 1)
    );
    assert_eq!(
        plex_occurrence(&receiver_occurrence(&first.source_id, 7, 1)),
        Some(1)
    );
    // The receiver endpoint refuses connections. Returning to Rufin must still work.
    plex.observe(stopped_timeline(), false).await.unwrap();
    let plex = Arc::new(plex);
    *owner.plex.lock().unwrap() = Some(plex.clone());
    owner.output.lock().unwrap().selected = plex.output.clone();
    tokio::task::spawn_blocking(move || {
        owner.leave_plex(Arc::new(AtomicBool::new(false))).unwrap();
        assert!(owner.plex.lock().unwrap().is_none());
        assert_eq!(owner.output.lock().unwrap().selected, PlaybackOutput::Local);
        assert!(plex.cancelled.load(Ordering::Acquire));
        drop(plex);
        owner.shutdown();
    })
    .await
    .unwrap();
}
