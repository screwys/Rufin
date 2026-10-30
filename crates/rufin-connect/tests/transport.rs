use futures_util::future::BoxFuture;
use rufin_connect::{
    ConnectNetwork, Credentials, NetworkConfig, NetworkEvent, Profile, Store,
    network::{INCOMPATIBLE, REMOVED_FROM_PROFILE},
    store::{Change, Cursor, Row},
    sync::{Frame, Values},
};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

type Events = mpsc::UnboundedReceiver<NetworkEvent>;

/// Values that live outside Connect's records, as the Store's do.
#[derive(Default)]
struct Items(Mutex<HashMap<String, serde_json::Value>>);

impl Values for Items {
    fn encode<'a>(&'a self, rows: &'a mut [Row]) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let items = self.0.lock().unwrap();
            for row in rows
                .iter_mut()
                .filter(|row| !row.deleted && row.value.is_none())
            {
                row.value = items.get(&row.id).cloned();
            }
            Ok(())
        })
    }
}

async fn node(name: &str, credentials: Credentials) -> (Arc<ConnectNetwork>, Events) {
    let (network, mut events) = ConnectNetwork::spawn(
        NetworkConfig {
            name: name.into(),
            nearby: false,
            relay: None,
            public_relay: false,
        },
        credentials,
    )
    .await
    .unwrap();
    // Every exchange reports reachability; keep those from filling the network's queue.
    let (forward, received) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if !matches!(event, NetworkEvent::Reachable { .. }) && forward.send(event).is_err() {
                break;
            }
        }
    });
    (network, received)
}

async fn host(network: &ConnectNetwork, root: &Path, items: Arc<Items>) -> (String, Store) {
    let store = Store::create(&root.join("connect.sqlite"), &network.identity())
        .await
        .unwrap();
    let profile = rufin_connect::store::new_profile();
    store.turn_on(&profile).await.unwrap();
    serve(network, &profile, &store, items).await;
    (profile, store)
}

async fn serve(network: &ConnectNetwork, profile: &str, store: &Store, items: Arc<Items>) {
    network
        .serve(Some(Profile {
            id: profile.into(),
            key: "profile key".into(),
            store: store.clone(),
            values: items,
        }))
        .await
        .unwrap();
}

/// What the core's device records say.
fn trust(network: &ConnectNetwork, devices: &[(&ConnectNetwork, bool)]) {
    network.set_devices(
        devices
            .iter()
            .map(|(device, trusted)| (device.identity(), *trusted))
            .collect(),
    );
}

async fn next<T>(events: &mut Events, mut wanted: impl FnMut(NetworkEvent) -> Option<T>) -> T {
    loop {
        match events.recv().await.unwrap() {
            NetworkEvent::Error(error) => panic!("{error}"),
            event => {
                if let Some(value) = wanted(event) {
                    return value;
                }
            }
        }
    }
}

async fn pending(events: &mut Events) -> String {
    next(events, |event| match event {
        NetworkEvent::Pairing { session, .. } => Some(session),
        NetworkEvent::PairingFailed { error, .. } => panic!("{error}"),
        _ => None,
    })
    .await
}

async fn ended(events: &mut Events, expected: &str) {
    next(events, |event| match event {
        NetworkEvent::PairingFailed { session, .. } => {
            assert_eq!(session.as_deref(), Some(expected));
            Some(())
        }
        NetworkEvent::MemberAdded { .. } | NetworkEvent::Paired { .. } => {
            panic!("cancelled pairing completed")
        }
        _ => None,
    })
    .await
}

/// Pair `joiner` into `host`'s profile. Returns the profile and key the host handed over.
async fn pair(
    host: &ConnectNetwork,
    host_events: &mut Events,
    joiner: &Arc<ConnectNetwork>,
    joiner_events: &mut Events,
) -> (String, String) {
    joiner
        .begin_pairing(&host.invitation().await.unwrap())
        .await
        .unwrap();
    let (host_session, joiner_session) = tokio::join!(pending(host_events), pending(joiner_events));
    assert_eq!(host_session, joiner_session);
    host.confirm_pairing(&host_session, true).await.unwrap();
    joiner.confirm_pairing(&joiner_session, true).await.unwrap();
    let joined = next(joiner_events, |event| match event {
        NetworkEvent::Paired {
            profile, key, peer, ..
        } => {
            assert_eq!(peer, host.identity());
            Some((profile, key))
        }
        NetworkEvent::PairingFailed { error, .. } => panic!("joiner: {error}"),
        _ => None,
    });
    let added = next(host_events, |event| match event {
        NetworkEvent::MemberAdded { peer, .. } => {
            assert_eq!(peer, joiner.identity());
            Some(())
        }
        NetworkEvent::PairingFailed { error, .. } => panic!("host: {error}"),
        _ => None,
    });
    tokio::join!(joined, added).0
}

/// Pull from `peer` after the saved cursor and store the newer rows, as the core applies them.
async fn pull(
    network: &ConnectNetwork,
    store: &Store,
    peer: &ConnectNetwork,
    profile: &str,
) -> anyhow::Result<(u64, Vec<Row>)> {
    let device = peer.identity();
    let after = store
        .peers()
        .await?
        .into_iter()
        .find(|known| known.device == device)
        .map_or(0, |known| known.pulled);
    let mut pull = network.pull(&device, profile, after).await?;
    let mut applied = Vec::new();
    loop {
        match pull.next().await? {
            Frame::Page(rows) => {
                let sequence = rows.last().unwrap().sequence;
                let rows = store.newer(rows).await?;
                let cursor = Cursor::Pulled {
                    device: &device,
                    sequence,
                };
                store.receive(&rows, cursor).await?;
                applied.extend(rows);
            }
            Frame::End { sequence } => {
                let cursor = Cursor::Pulled {
                    device: &device,
                    sequence,
                };
                store.receive(&[], cursor).await?;
                return Ok((pull.total, applied));
            }
        }
    }
}

fn change(id: &str, value: Option<serde_json::Value>) -> Change {
    Change {
        id: id.into(),
        parent: None,
        digest: rufin_connect::store::digest(&value).unwrap(),
        deleted: false,
        value,
    }
}

/// Exercise bilateral SAS enrollment, the profile hand-over, pulls of Store-held and
/// record-held values, relaying through a third device, RPC, member-only media and removal.
#[tokio::test]
async fn enrollment_records_media_and_removal() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let c_dir = tempfile::tempdir().unwrap();
        let (a, mut a_events) = node("Work Laptop", Credentials::generate()).await;
        let (b, mut b_events) = node("Home PC", Credentials::generate()).await;
        let a_items = Arc::new(Items::default());
        let display = serde_json::json!({"content": "x".repeat(128 * 1024)});
        a_items.0.lock().unwrap().insert("setting:preference:display".into(), display.clone());
        let (profile, a_store) = host(&a, a_dir.path(), a_items).await;
        a_store.write(vec![
            change("setting:preference:display", None),
            change(&format!("device:{}", a.identity()), Some(serde_json::json!({"name": "Work Laptop", "trusted": true}))),
        ], None).await.unwrap();
        trust(&a, &[(&a, true)]);

        b.begin_pairing(&a.invitation().await.unwrap()).await.unwrap();
        let mut a_emoji = None;
        let mut b_emoji = None;
        let mut handed = None;
        let mut added = false;
        while handed.is_none() || !added {
            tokio::select! {
                event = a_events.recv() => match event.unwrap() {
                    NetworkEvent::Pairing { session, emoji, name, .. } => {
                        assert_eq!(name, "Home PC");
                        a_emoji = Some(emoji);
                        a.confirm_pairing(&session, true).await.unwrap();
                        a.confirm_pairing(&session, true).await.unwrap();
                    },
                    NetworkEvent::MemberAdded { peer, name } => {
                        assert_eq!((peer, name.as_str()), (b.identity(), "Home PC"));
                        added = true;
                    },
                    NetworkEvent::Error(error) | NetworkEvent::PairingFailed { error, .. } => panic!("host: {error}"),
                    _ => {},
                },
                event = b_events.recv() => match event.unwrap() {
                    NetworkEvent::Pairing { session, emoji, .. } => { b_emoji = Some(emoji); b.confirm_pairing(&session, true).await.unwrap(); },
                    NetworkEvent::Paired { profile, key, peer, name, .. } => handed = Some((profile, key, peer, name)),
                    NetworkEvent::Error(error) | NetworkEvent::PairingFailed { error, .. } => panic!("joiner: {error}"),
                    _ => {},
                },
            }
        }
        assert_eq!(a_emoji, b_emoji);
        assert_eq!(handed, Some((profile.clone(), "profile key".into(), a.identity(), "Work Laptop".into())));
        trust(&a, &[(&a, true), (&b, true)]);
        trust(&b, &[(&a, true), (&b, true)]);

        // A joining device pulls everything before its profile row exists.
        let b_store = Store::create(&b_dir.path().join("connect.sqlite"), &b.identity()).await.unwrap();
        let (total, rows) = pull(&b, &b_store, &a, &profile).await.unwrap();
        assert_eq!(total, 2);
        assert_eq!(rows.iter().find(|row| row.id == "setting:preference:display").unwrap().value, Some(display));
        assert!(rows.iter().all(|row| row.device == a.identity()));
        b_store.turn_on(&profile).await.unwrap();
        assert!(b_store.profile().await.unwrap().unwrap().clock >= rows.iter().map(|row| row.clock).max().unwrap());
        serve(&b, &profile, &b_store, Arc::default()).await;
        assert!(pull(&b, &b_store, &a, &profile).await.unwrap().1.is_empty());

        let requester = b.clone();
        let host = a.identity();
        let answer = tokio::spawn(async move { requester.request(&host, b"fresh player context".to_vec()).await });
        next(&mut a_events, |event| match event {
            NetworkEvent::Request { peer, body, reply } => {
                assert_eq!(peer, b.identity());
                assert_eq!(body, b"fresh player context");
                reply.send(Ok(b"queue and position".to_vec())).unwrap();
                Some(())
            }
            _ => None,
        }).await;
        assert_eq!(answer.await.unwrap().unwrap(), b"queue and position");

        let original = a_dir.path().join("track.flac");
        tokio::fs::write(&original, b"original media bytes").await.unwrap();
        let destination = b_dir.path().join("Music/track.flac");
        let mut validator = None;
        for (previous, offset) in [(None, 0), (Some(b"previous downloaded copy".as_slice()), 0), (Some(b"original ".as_slice()), 9)] {
            if let Some(previous) = previous { tokio::fs::write(&destination, previous).await.unwrap(); }
            let requester = b.clone();
            let host = a.identity();
            let target = destination.clone();
            let body = serde_json::to_vec(&serde_json::json!({"offset":offset,"validator":validator})).unwrap();
            let transfer = tokio::spawn(async move {
                let response = requester.stream_request(&host, body).await.unwrap();
                let metadata: rufin_connect::media::FileMetadata = serde_json::from_value(response.metadata).unwrap();
                assert_eq!(metadata.offset, offset);
                rufin_connect::media::receive_file(response.reader, &metadata, &target, CancellationToken::new(), watch::channel(0).0).await.unwrap();
                metadata.validator
            });
            let (peer, body, reply) = next(&mut a_events, |event| match event {
                NetworkEvent::StreamRequest { peer, body, reply } => Some((peer, body, reply)),
                _ => None,
            }).await;
            assert_eq!(peer, b.identity());
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let (metadata, reader) = rufin_connect::media::open_file(&original, request["offset"].as_u64().unwrap(), request["validator"].as_str()).await.unwrap();
            assert!(reply.send(Ok(rufin_connect::media::PeerByteResponse { metadata: serde_json::to_value(metadata).unwrap(), reader: Box::pin(reader) })).is_ok());
            validator = transfer.await.unwrap();
            assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"original media bytes");
            assert_eq!(tokio::fs::read(&original).await.unwrap(), b"original media bytes");
        }

        // A third device's records reach B through A, keeping their writer.
        let (c, mut c_events) = node("Third device", Credentials::generate()).await;
        assert_eq!(pair(&a, &mut a_events, &c, &mut c_events).await.0, profile);
        trust(&a, &[(&a, true), (&b, true), (&c, true)]);
        trust(&c, &[(&a, true), (&c, true)]);
        let c_store = Store::create(&c_dir.path().join("connect.sqlite"), &c.identity()).await.unwrap();
        pull(&c, &c_store, &a, &profile).await.unwrap();
        c_store.turn_on(&profile).await.unwrap();
        serve(&c, &profile, &c_store, Arc::default()).await;
        let edit = serde_json::json!("made on C");
        let sequence = c_store.write(vec![change("setting:preference:third", Some(edit.clone()))], None).await.unwrap();
        assert_eq!(sequence, Some(c_store.sequence().await.unwrap()));
        let (_, rows) = pull(&a, &a_store, &c, &profile).await.unwrap();
        assert_eq!(rows.len(), 1);
        let (_, rows) = pull(&b, &b_store, &a, &profile).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].value.as_ref(), rows[0].device.as_str()), (Some(&edit), c.identity().as_str()));
        // B's copy loses to nothing it already holds, and its own write wins over C's.
        assert!(b_store.newer(rows).await.unwrap().is_empty());
        b_store.write(vec![change("setting:preference:third", Some(serde_json::json!("made on B")))], None).await.unwrap();
        let (_, rows) = pull(&c, &c_store, &a, &profile).await.unwrap();
        assert!(rows.is_empty());
        pull(&a, &a_store, &b, &profile).await.unwrap();
        let (_, rows) = pull(&c, &c_store, &a, &profile).await.unwrap();
        assert_eq!(rows[0].value, Some(serde_json::json!("made on B")));

        // A removed device is refused and told why.
        trust(&a, &[(&a, true), (&b, false), (&c, true)]);
        let refused = b.stream_request(&a.identity(), b"denied after removal".to_vec()).await;
        assert_eq!(refused.err().unwrap().to_string(), REMOVED_FROM_PROFILE);
        next(&mut b_events, |event| match event {
            NetworkEvent::Removed { peer } => Some(assert_eq!(peer, a.identity())),
            _ => None,
        }).await;
        assert_eq!(pull(&b, &b_store, &a, &profile).await.unwrap_err().to_string(), REMOVED_FROM_PROFILE);
        // C relays B's row back to A, which already holds it.
        assert!(pull(&a, &a_store, &c, &profile).await.unwrap().1.is_empty());
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
        c.shutdown().await.unwrap();
    })
    .await
    .expect("Connect integration timed out");
}

#[tokio::test]
async fn cancelling_before_approval_ends_pairing_on_both_devices_and_allows_retry() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let a_dir = tempfile::tempdir().unwrap();
        let (a, mut a_events) = node("Host", Credentials::generate()).await;
        let (b, mut b_events) = node("Joiner", Credentials::generate()).await;
        let (profile, _a_store) = host(&a, a_dir.path(), Arc::default()).await;
        for cancel_host in [true, false] {
            b.begin_pairing(&a.invitation().await.unwrap())
                .await
                .unwrap();
            let (a_session, b_session) =
                tokio::join!(pending(&mut a_events), pending(&mut b_events));
            assert_eq!(a_session, b_session);
            if cancel_host {
                b.confirm_pairing(&b_session, true).await.unwrap();
                a.confirm_pairing(&a_session, false).await.unwrap();
            } else {
                a.confirm_pairing(&a_session, true).await.unwrap();
                b.confirm_pairing(&b_session, false).await.unwrap();
            }
            tokio::join!(
                ended(&mut a_events, &a_session),
                ended(&mut b_events, &b_session)
            );
        }
        assert_eq!(pair(&a, &mut a_events, &b, &mut b_events).await.0, profile);
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    })
    .await
    .expect("Cancel/retry did not finish");
}

#[tokio::test]
async fn pairing_again_overrides_a_removal_and_turning_off_ends_serving() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let (a, mut a_events) = node("Inviter", Credentials::generate()).await;
        let (b, mut b_events) = node("Joiner", Credentials::generate()).await;
        let (profile, a_store) = host(&a, a_dir.path(), Arc::default()).await;
        pair(&a, &mut a_events, &b, &mut b_events).await;
        trust(&a, &[(&a, true), (&b, true)]);
        trust(&b, &[(&a, true), (&b, true)]);
        let b_store = Store::create(&b_dir.path().join("connect.sqlite"), &b.identity())
            .await
            .unwrap();
        pull(&b, &b_store, &a, &profile).await.unwrap();
        trust(&a, &[(&a, true), (&b, false)]);
        assert_eq!(
            pull(&b, &b_store, &a, &profile)
                .await
                .unwrap_err()
                .to_string(),
            REMOVED_FROM_PROFILE
        );
        // Approving again makes the host write B trusted with a newer clock.
        pair(&a, &mut a_events, &b, &mut b_events).await;
        trust(&a, &[(&a, true), (&b, true)]);
        pull(&b, &b_store, &a, &profile).await.unwrap();
        a.serve(None).await.unwrap();
        a_store.delete().await.unwrap();
        assert!(!a_dir.path().join("connect.sqlite").exists());
        assert!(pull(&b, &b_store, &a, &profile).await.is_err());
        let (profile, _a_store) = host(&a, a_dir.path(), Arc::default()).await;
        assert_eq!(pair(&a, &mut a_events, &b, &mut b_events).await.0, profile);
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    })
    .await
    .expect("Pairing again did not finish");
}

#[tokio::test]
async fn a_push_reaches_an_online_device_while_another_is_offline() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let (a, mut a_events) = node("Host", Credentials::generate()).await;
        let (b, mut b_events) = node("Online peer", Credentials::generate()).await;
        let (c, mut c_events) = node("Offline peer", Credentials::generate()).await;
        let (profile, a_store) = host(&a, a_dir.path(), Arc::default()).await;
        pair(&a, &mut a_events, &b, &mut b_events).await;
        pair(&a, &mut a_events, &c, &mut c_events).await;
        trust(&a, &[(&a, true), (&b, true), (&c, true)]);
        trust(&b, &[(&a, true), (&b, true), (&c, true)]);
        let b_store = Store::create(&b_dir.path().join("connect.sqlite"), &b.identity())
            .await
            .unwrap();
        b_store.turn_on(&profile).await.unwrap();
        serve(&b, &profile, &b_store, Arc::default()).await;
        c.shutdown().await.unwrap();
        for value in ["first edit", "second edit"] {
            let sequence = a_store
                .write(
                    vec![change(
                        "setting:preference:theme",
                        Some(serde_json::json!(value)),
                    )],
                    None,
                )
                .await
                .unwrap()
                .unwrap();
            let started = Instant::now();
            let pushing = a.push(sequence);
            let receiving = async {
                let (peer, pushed, pulled) = next(&mut b_events, |event| match event {
                    NetworkEvent::Changed {
                        peer,
                        sequence,
                        pulled,
                    } => Some((peer, sequence, pulled)),
                    _ => None,
                })
                .await;
                assert_eq!((peer, pushed), (a.identity(), sequence));
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "the offline device delayed the push"
                );
                let (_, rows) = pull(&b, &b_store, &a, &profile).await.unwrap();
                assert_eq!(rows.last().unwrap().value, Some(serde_json::json!(value)));
                let _ = pulled.send(());
            };
            tokio::join!(pushing, receiving);
            assert!(started.elapsed() < Duration::from_secs(7));
        }
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    })
    .await
    .expect("Offline-peer regression did not finish");
}

#[tokio::test]
async fn offline_edits_catch_up_after_restarting_with_the_saved_cursor() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let (a, mut a_events) = node("Host", Credentials::generate()).await;
        let credentials = Credentials::generate();
        let (b, mut b_events) = node("Laptop", credentials.clone()).await;
        let (profile, a_store) = host(&a, a_dir.path(), Arc::default()).await;
        a_store
            .write(
                vec![change(
                    "setting:preference:theme",
                    Some(serde_json::json!("initial")),
                )],
                None,
            )
            .await
            .unwrap();
        pair(&a, &mut a_events, &b, &mut b_events).await;
        trust(&a, &[(&a, true), (&b, true)]);
        trust(&b, &[(&a, true), (&b, true)]);
        let path = b_dir.path().join("connect.sqlite");
        let b_store = Store::create(&path, &b.identity()).await.unwrap();
        assert_eq!(pull(&b, &b_store, &a, &profile).await.unwrap().1.len(), 1);
        b_store.turn_on(&profile).await.unwrap();
        serve(&b, &profile, &b_store, Arc::default()).await;
        b.shutdown().await.unwrap();
        drop((b, b_events, b_store));

        a_store
            .write(
                vec![change(
                    "setting:preference:theme",
                    Some(serde_json::json!("edited while offline")),
                )],
                None,
            )
            .await
            .unwrap();
        let (b, mut b_events) = node("Laptop", credentials).await;
        let (b_store, row) = Store::open(&path).await.unwrap().unwrap();
        assert_eq!(
            (row.profile.as_str(), row.device),
            (profile.as_str(), b.identity())
        );
        trust(&b, &[(&a, true), (&b, true)]);
        serve(&b, &profile, &b_store, Arc::default()).await;
        let (total, rows) = pull(&b, &b_store, &a, &profile).await.unwrap();
        assert_eq!(total, 1);
        assert_eq!(
            rows[0].value,
            Some(serde_json::json!("edited while offline"))
        );
        // B listens on another port now; its pull told A where.
        let sequence = a_store.sequence().await.unwrap();
        let pushing = a.push(sequence);
        let receiving = next(&mut b_events, |event| match event {
            NetworkEvent::Changed {
                sequence, pulled, ..
            } => Some((sequence, pulled)),
            _ => None,
        });
        let ((), (pushed, pulled)) = tokio::join!(pushing, receiving);
        assert_eq!(pushed, sequence);
        drop(pulled);
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    })
    .await
    .expect("Offline edit did not catch up after restart");
}

#[tokio::test]
async fn an_older_rufin_is_reported_as_incompatible() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let old = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(vec![b"rufin/connect/pair/3".to_vec()])
            .bind()
            .await
            .unwrap();
        let accepting = old.clone();
        tokio::spawn(async move {
            while let Some(incoming) = accepting.accept().await {
                let _ = incoming.await;
            }
        });
        let (b, mut b_events) = node("Joiner", Credentials::generate()).await;
        b.begin_pairing(&serde_json::to_string(&old.addr()).unwrap())
            .await
            .unwrap();
        let error = next(&mut b_events, |event| match event {
            NetworkEvent::PairingFailed { error, .. } => Some(error),
            _ => None,
        })
        .await;
        assert_eq!(error, INCOMPATIBLE);
        b.shutdown().await.unwrap();
        old.close().await;
    })
    .await
    .expect("The incompatible device did not answer");
}
