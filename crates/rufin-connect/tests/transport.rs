use library::ConnectRecord;
use loro::{LoroDoc, ToJson, VersionVector};
use rufin_connect::{
    ConnectNetwork, Credentials, NetworkConfig, NetworkEvent, profile::ProfileStore,
};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

async fn node(root: &Path, name: &str) -> (Arc<ConnectNetwork>, mpsc::Receiver<NetworkEvent>) {
    ConnectNetwork::spawn(
        NetworkConfig {
            database: root.join("network.sqlite"),
            media_directory: root.join("media"),
            name: name.into(),
            nearby: false,
            relay: None,
            public_relay: false,
        },
        Credentials::generate(),
    )
    .await
    .unwrap()
}

async fn pending(events: &mut mpsc::Receiver<NetworkEvent>) -> String {
    loop {
        match events.recv().await.unwrap() {
            NetworkEvent::Pairing { session, .. } => return session,
            NetworkEvent::PairingFailed { error, .. } | NetworkEvent::Error(error) => {
                panic!("{error}")
            }
            _ => {}
        }
    }
}

async fn verified(events: &mut mpsc::Receiver<NetworkEvent>) {
    loop {
        match events.recv().await.unwrap() {
            NetworkEvent::PairingVerified { .. } => return,
            NetworkEvent::PairingFailed { error, .. } | NetworkEvent::Error(error) => {
                panic!("{error}")
            }
            _ => {}
        }
    }
}

async fn ended(events: &mut mpsc::Receiver<NetworkEvent>, expected_session: &str) {
    loop {
        match events.recv().await.unwrap() {
            NetworkEvent::PairingFailed { session, .. } => {
                assert_eq!(session.as_deref(), Some(expected_session));
                return;
            }
            NetworkEvent::MemberAdded { .. } | NetworkEvent::Paired { .. } => {
                panic!("cancelled pairing completed")
            }
            _ => {}
        }
    }
}

async fn documents(root: &Path, peer: u64) -> Arc<ProfileStore> {
    Arc::new(
        ProfileStore::open(&root.join("profile.sqlite"), peer)
            .await
            .unwrap(),
    )
}

async fn pair(
    host: &Arc<ConnectNetwork>,
    host_events: &mut mpsc::Receiver<NetworkEvent>,
    joiner: &Arc<ConnectNetwork>,
    joiner_events: &mut mpsc::Receiver<NetworkEvent>,
    profile: &str,
) {
    joiner
        .begin_pairing(&host.invitation().await.unwrap())
        .await
        .unwrap();
    let (host_session, joiner_session) = tokio::join!(pending(host_events), pending(joiner_events));
    assert_eq!(host_session, joiner_session);
    host.confirm_pairing(&host_session, true).await.unwrap();
    joiner.confirm_pairing(&joiner_session, true).await.unwrap();
    let joined = async {
        loop {
            match joiner_events.recv().await.unwrap() {
                NetworkEvent::Paired {
                    profile_id,
                    enrollment_data,
                    ..
                } => {
                    assert_eq!(profile_id, profile);
                    joiner
                        .open_profile(&profile_id, false, enrollment_data)
                        .await
                        .unwrap();
                    break;
                }
                NetworkEvent::PairingFailed { error, .. } => panic!("joiner: {error}"),
                _ => {}
            }
        }
    };
    let added = async {
        loop {
            match host_events.recv().await.unwrap() {
                NetworkEvent::MemberAdded { peer, .. } => {
                    assert_eq!(peer, joiner.identity());
                    break;
                }
                NetworkEvent::PairingFailed { error, .. } => panic!("host: {error}"),
                _ => {}
            }
        }
    };
    tokio::join!(joined, added);
}

async fn received(source: &ProfileStore, target: &ProfileStore, records: &[ConnectRecord]) {
    let expected = source.changes(0, 64).await.unwrap();
    let names: Vec<_> = expected
        .iter()
        .map(|version| version.name.clone())
        .collect();
    loop {
        let actual = target.versions(&names).await.unwrap();
        if actual
            .iter()
            .zip(&expected)
            .all(|(a, b)| a.version == b.version)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let missing: Vec<_> = expected
        .into_iter()
        .map(|mut version| {
            version.version = VersionVector::default().encode();
            version
        })
        .collect();
    let mut values = serde_json::Map::new();
    for update in target.updates(&missing).await.unwrap() {
        let update: serde_json::Value = serde_json::from_slice(&update).unwrap();
        let bytes: Vec<u8> = serde_json::from_value(update["bytes"].clone()).unwrap();
        let document = LoroDoc::new();
        document.import(&bytes).unwrap();
        values.extend(
            document
                .get_map("records")
                .get_deep_value()
                .to_json_value()
                .as_object()
                .unwrap()
                .clone(),
        );
    }
    for record in records {
        let key = serde_json::to_string(&(&record.kind, &record.key)).unwrap();
        let actual: serde_json::Value =
            serde_json::from_str(values[&key].as_str().unwrap()).unwrap();
        assert_eq!(Some(actual), record.value);
    }
}

#[tokio::test]
async fn cancel_after_bilateral_approval_ends_blocked_enrollment_and_allows_retry() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let (a, mut a_events) = node(a_dir.path(), "Host").await;
        let (b, mut b_events) = node(b_dir.path(), "Joiner").await;
        let profile = ConnectNetwork::new_profile_id();
        a.open_profile(&profile, true, Vec::new()).await.unwrap();
        for cancel_host in [true, false] {
            b.begin_pairing(&a.invitation().await.unwrap())
                .await
                .unwrap();
            let (a_session, b_session) =
                tokio::join!(pending(&mut a_events), pending(&mut b_events));
            assert_eq!(a_session, b_session);
            // Hold an actual SQLite writer so membership cannot finish after
            // approval. Cancellation must finish before this lock is released.
            let pool = sqlx::SqlitePool::connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(a_dir.path().join("network.sqlite")),
            )
            .await
            .unwrap();
            let mut writer = pool.acquire().await.unwrap();
            sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut *writer)
                .await
                .unwrap();
            a.confirm_pairing(&a_session, true).await.unwrap();
            b.confirm_pairing(&b_session, true).await.unwrap();
            tokio::join!(verified(&mut a_events), verified(&mut b_events));
            if cancel_host {
                a.confirm_pairing(&a_session, false).await.unwrap();
            } else {
                b.confirm_pairing(&b_session, false).await.unwrap();
            }
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(
                    ended(&mut a_events, &a_session),
                    ended(&mut b_events, &b_session)
                );
            })
            .await
            .expect("Cancel waited for the blocked membership write");
            sqlx::query("ROLLBACK").execute(&mut *writer).await.unwrap();
            drop(writer);
            pool.close().await;
        }
        pair(&a, &mut a_events, &b, &mut b_events, &profile).await;
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    })
    .await
    .expect("Cancel/retry did not finish");
}

#[tokio::test]
async fn enrollment_after_collection_publication() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let (a, mut a_events) = node(a_dir.path(), "Host").await;
        let (b, mut b_events) = node(b_dir.path(), "Joiner").await;
        let profile = ConnectNetwork::new_profile_id();
        a.open_profile(&profile, true, Vec::new()).await.unwrap();
        let a_docs = documents(a_dir.path(), 1).await;
        let b_docs = documents(b_dir.path(), 2).await;
        let records: Vec<_> = (0..20).map(|index| ConnectRecord {
            kind: "track".into(), key: format!("track-{index}"),
            value: Some(serde_json::json!({"title": format!("Track {index}"), "metadata": "x".repeat(512 * 1024)})),
        }).collect();
        a_docs.write_records(&records).await.unwrap();
        a.attach_documents(a_docs.clone()).await.unwrap();
        pair(&a, &mut a_events, &b, &mut b_events, &profile).await;
        b.attach_documents(b_docs.clone()).await.unwrap();
        received(&a_docs, &b_docs, &records).await;
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    }).await.expect("Collection publication starved enrollment");
}

#[tokio::test]
async fn pairing_again_overrides_the_two_devices_previous_removal() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let (a, mut a_events) = node(a_dir.path(), "Inviter").await;
        let (b, mut b_events) = node(b_dir.path(), "Joiner").await;
        let profile = ConnectNetwork::new_profile_id();
        a.open_profile(&profile, true, Vec::new()).await.unwrap();
        pair(&a, &mut a_events, &b, &mut b_events, &profile).await;
        // Each device retains its removal while they are disconnected.
        a.remove_member(&b.identity()).await.unwrap();
        b.remove_member(&a.identity()).await.unwrap();
        let unrelated = Credentials::generate().public().to_string();
        b.remove_member(&unrelated).await.unwrap();
        b.close_profile().await.unwrap();
        pair(&a, &mut a_events, &b, &mut b_events, &profile).await;
        assert!(a.members().await.unwrap().contains(&b.identity()));
        assert!(b.members().await.unwrap().contains(&a.identity()));
        assert!(b.check_membership(&unrelated).await.is_err());
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    })
    .await
    .expect("Pairing again did not finish");
}

/// Exercise bilateral SAS enrollment, Loro catch-up, RPC, member-only media,
/// and propagation of trust and removal to a third device.
#[tokio::test]
async fn enrollment_delivery_media_and_removal() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let (a, mut a_events) = node(a_dir.path(), "Work Laptop").await;
        let invitation = a.invitation().await.unwrap();
        let (b, mut b_events) = node(b_dir.path(), "Home PC").await;
        let profile = ConnectNetwork::new_profile_id();
        a.open_profile(&profile, true, b"authorized enrollment secret".to_vec()).await.unwrap();
        let previous_profile = ConnectNetwork::new_profile_id();
        b.open_profile(&previous_profile, true, Vec::new()).await.unwrap();
        b.close_profile().await.unwrap();
        b.begin_pairing(&invitation).await.unwrap();
        let mut a_emoji = None;
        let mut b_emoji = None;
        let mut joined = false;
        let mut added = false;
        while !joined || !added {
            tokio::select! {
                event = a_events.recv() => match event.unwrap() {
                    NetworkEvent::Pairing { session, emoji, .. } => {
                        a_emoji = Some(emoji);
                        a.confirm_pairing(&session, true).await.unwrap();
                        a.confirm_pairing(&session, true).await.unwrap();
                    },
                    NetworkEvent::MemberAdded { .. } => added = true,
                    NetworkEvent::Error(error) | NetworkEvent::PairingFailed { error, .. } => panic!("host: {error}"),
                    _ => {},
                },
                event = b_events.recv() => match event.unwrap() {
                    NetworkEvent::Pairing { session, emoji, .. } => { b_emoji = Some(emoji); b.confirm_pairing(&session, true).await.unwrap(); },
                    NetworkEvent::Paired { profile_id, enrollment_data, .. } => {
                        assert_eq!(profile_id, profile);
                        assert_eq!(enrollment_data, b"authorized enrollment secret");
                        b.open_profile(&profile_id, false, enrollment_data).await.unwrap();
                        joined = true;
                    },
                    NetworkEvent::Error(error) | NetworkEvent::PairingFailed { error, .. } => panic!("joiner: {error}"),
                    _ => {},
                },
            }
        }
        assert_eq!(a_emoji, b_emoji);
        let a_docs = documents(a_dir.path(), 1).await;
        let b_docs = documents(b_dir.path(), 2).await;
        a.attach_documents(a_docs.clone()).await.unwrap();
        b.attach_documents(b_docs.clone()).await.unwrap();
        let records = [ConnectRecord { kind: "preference".into(), key: "display".into(),
            value: Some(serde_json::json!({"content": "x".repeat(128 * 1024)})) }];
        a_docs.write_records(&records).await.unwrap();
        received(&a_docs, &b_docs, &records).await;
        let requester = b.clone();
        let host = a.identity();
        let answer = tokio::spawn(async move { requester.request(&host, b"fresh player context".to_vec()).await });
        loop {
            if let NetworkEvent::Request { peer, body, reply, .. } = a_events.recv().await.unwrap() {
                assert_eq!(peer, b.identity());
                assert_eq!(body, b"fresh player context");
                reply.send(Ok(b"queue and position".to_vec())).unwrap();
                break;
            }
        }
        assert_eq!(answer.await.unwrap().unwrap(), b"queue and position");
        let original = a_dir.path().join("track.flac");
        tokio::fs::write(&original, b"original media bytes").await.unwrap();
        let hash = a.media().publish(&original, "track/original/1").await.unwrap();
        let destination = b_dir.path().join("Music/track.flac");
        let (progress, _) = watch::channel(0);
        b.media().fetch(&a.identity(), &hash, &destination, CancellationToken::new(), progress).await.unwrap();
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"original media bytes");
        assert_eq!(tokio::fs::read(&original).await.unwrap(), b"original media bytes");
        let c_dir = tempfile::tempdir().unwrap();
        let (c, mut c_events) = node(c_dir.path(), "Third device").await;
        pair(&a, &mut a_events, &c, &mut c_events, &profile).await;
        let c_docs = documents(c_dir.path(), 3).await;
        c.attach_documents(c_docs.clone()).await.unwrap();
        received(&a_docs, &c_docs, &records).await;
        let third_record = ConnectRecord { kind: "preference".into(), key: "third-device-edit".into(),
            value: Some(serde_json::json!("made on C")) };
        c_docs.write_records(std::slice::from_ref(&third_record)).await.unwrap();
        received(&c_docs, &b_docs, &[third_record]).await;
        assert!(b.members().await.unwrap().contains(&c.identity()));
        let third = c_dir.path().join("third.flac");
        tokio::fs::write(&third, b"third device media").await.unwrap();
        let third_hash = c.media().publish(&third, "track/original/3").await.unwrap();
        let destination = b_dir.path().join("third.flac");
        let (progress, _) = watch::channel(0);
        // B has never received C's invitation. Its address and trust arrived
        // through A's authenticated sync connection.
        b.media().fetch(&c.identity(), &third_hash, &destination, CancellationToken::new(), progress).await.unwrap();
        assert_eq!(tokio::fs::read(destination).await.unwrap(), b"third device media");
        a.remove_member(&b.identity()).await.unwrap();
        let second = a_dir.path().join("second.flac");
        tokio::fs::write(&second, b"not authorized after removal").await.unwrap();
        let hash = a.media().publish(&second, "track/original/2").await.unwrap();
        let (progress, _) = watch::channel(0);
        assert!(b.media().fetch(&a.identity(), &hash, &b_dir.path().join("denied.flac"), CancellationToken::new(), progress).await.is_err());
        while c.members().await.unwrap().contains(&b.identity()) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::fs::write(&third, b"third device private media after removal").await.unwrap();
        let third_hash = c.media().publish(&third, "track/original/4").await.unwrap();
        let (progress, _) = watch::channel(0);
        assert!(b.media().fetch(&c.identity(), &third_hash, &b_dir.path().join("denied-third.flac"), CancellationToken::new(), progress).await.is_err());
        // The removed device learns that it is no longer enrolled, while its
        // already received library remains available locally.
        while !b.members().await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!b_docs.changes(0, 64).await.unwrap().is_empty());
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
        c.shutdown().await.unwrap();
    }).await.expect("Connect integration timed out");
}

#[tokio::test]
async fn offline_member_does_not_delay_later_edits_between_online_devices() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let c_dir = tempfile::tempdir().unwrap();
        let (a, mut a_events) = node(a_dir.path(), "Host").await;
        let (b, mut b_events) = node(b_dir.path(), "Online peer").await;
        let (c, mut c_events) = node(c_dir.path(), "Offline peer").await;
        let profile = ConnectNetwork::new_profile_id();
        a.open_profile(&profile, true, Vec::new()).await.unwrap();
        let a_docs = documents(a_dir.path(), 1).await;
        let b_docs = documents(b_dir.path(), 2).await;
        let c_docs = documents(c_dir.path(), 3).await;
        let record = |value: &str| ConnectRecord {
            kind: "preference".into(),
            key: "theme".into(),
            value: Some(serde_json::json!(value)),
        };
        a_docs.write_records(&[record("initial")]).await.unwrap();
        a.attach_documents(a_docs.clone()).await.unwrap();
        pair(&a, &mut a_events, &b, &mut b_events, &profile).await;
        b.attach_documents(b_docs.clone()).await.unwrap();
        received(&a_docs, &b_docs, &[record("initial")]).await;
        pair(&a, &mut a_events, &c, &mut c_events, &profile).await;
        c.attach_documents(c_docs.clone()).await.unwrap();
        received(&a_docs, &c_docs, &[record("initial")]).await;
        while !b.members().await.unwrap().contains(&c.identity()) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        c.shutdown().await.unwrap();
        assert!(a.members().await.unwrap().contains(&c.identity()));
        assert!(b.members().await.unwrap().contains(&c.identity()));

        for value in ["first edit", "edit after the first acknowledgement"] {
            while b_events.try_recv().is_ok() {}
            a_docs.write_records(&[record(value)]).await.unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                received(&a_docs, &b_docs, &[record(value)]).await;
                // A completed page has exchanged its final acknowledgement.
                // Make the next edit require a later sync round.
                let mut receiving = false;
                loop {
                    match b_events.recv().await.unwrap() {
                        NetworkEvent::Syncing {
                            peer, active: true, ..
                        } if peer == a.identity() => receiving = true,
                        NetworkEvent::Syncing {
                            peer,
                            active: false,
                            ..
                        } if receiving && peer == a.identity() => break,
                        NetworkEvent::PairingFailed { error, .. } => panic!("{error}"),
                        _ => {}
                    }
                }
            })
            .await
            .expect("An online edit waited for the offline peer's 30-second connection timeout");
        }
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    })
    .await
    .expect("Offline-peer regression did not finish");
}

#[tokio::test]
async fn membership_checks_fail_after_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let (network, _events) = node(directory.path(), "Work Laptop").await;
    network
        .open_profile(&ConnectNetwork::new_profile_id(), false, Vec::new())
        .await
        .unwrap();
    let peer = network.identity();
    network.check_membership(&peer).await.unwrap();
    network.shutdown().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), network.check_membership(&peer))
            .await
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
async fn offline_edits_catch_up_after_restarting_with_saved_trust() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let peer_directory = tempfile::tempdir().unwrap();
        let config = NetworkConfig {
            database: directory.path().join("network.sqlite"),
            media_directory: directory.path().join("media"),
            name: "Work Laptop".into(),
            nearby: false,
            relay: None,
            public_relay: false,
        };
        let credentials = Credentials::generate();
        let (network, mut events) = ConnectNetwork::spawn(config.clone(), credentials.clone())
            .await
            .unwrap();
        let identity = network.identity();
        let profile = ConnectNetwork::new_profile_id();
        network
            .open_profile(&profile, true, Vec::new())
            .await
            .unwrap();
        let (peer, mut peer_events) = node(peer_directory.path(), "Peer").await;
        let local_docs = documents(directory.path(), 1).await;
        let peer_docs = documents(peer_directory.path(), 2).await;
        let record = |value: &str| ConnectRecord {
            kind: "preference".into(),
            key: "theme".into(),
            value: Some(serde_json::json!(value)),
        };
        local_docs
            .write_records(&[record("initial")])
            .await
            .unwrap();
        network.attach_documents(local_docs.clone()).await.unwrap();
        pair(&network, &mut events, &peer, &mut peer_events, &profile).await;
        peer.attach_documents(peer_docs.clone()).await.unwrap();
        received(&local_docs, &peer_docs, &[record("initial")]).await;
        network.shutdown().await.unwrap();
        drop(events);
        drop(network);
        local_docs
            .write_records(&[record("edited while offline")])
            .await
            .unwrap();
        drop(local_docs);
        let (network, _events) = ConnectNetwork::spawn(config, credentials).await.unwrap();
        assert_eq!(network.identity(), identity);
        network
            .open_profile(&profile, false, Vec::new())
            .await
            .unwrap();
        assert!(network.members().await.unwrap().contains(&peer.identity()));
        let local_docs = documents(directory.path(), 1).await;
        network.attach_documents(local_docs.clone()).await.unwrap();
        // The endpoint has a new listening port after restart. Refresh its
        // advertised address without pairing again or replacing either roster.
        peer.remember_peer(&network.invitation().await.unwrap())
            .await
            .unwrap();
        received(&local_docs, &peer_docs, &[record("edited while offline")]).await;
        network.shutdown().await.unwrap();
        peer.shutdown().await.unwrap();
    })
    .await
    .expect("Offline edit did not catch up after restart");
}
