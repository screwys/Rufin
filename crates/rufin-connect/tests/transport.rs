use library::ConnectRecord;
use library::profile::{ProfileStore, SyncStream};
use loro::{LoroDoc, ToJson, VersionVector};
use rufin_connect::{ConnectNetwork, Credentials, NetworkConfig, NetworkEvent};
use std::{path::Path, sync::Arc, time::Duration};

const KEY: &str = "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33";
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

async fn node(root: &Path, name: &str) -> (Arc<ConnectNetwork>, mpsc::Receiver<NetworkEvent>) {
    ConnectNetwork::spawn(
        NetworkConfig {
            database: root.join("network.sqlite"),
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
        ProfileStore::open(
            library::Database::open(&root.join("profile.sqlite"))
                .await
                .unwrap(),
            peer,
            KEY.parse().unwrap(),
        )
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
    let mut expected = source
        .changes_in_stream(0, 64, SyncStream::Authored)
        .await
        .unwrap();
    expected.extend(
        source
            .changes_in_stream(0, 64, SyncStream::NativeCache)
            .await
            .unwrap(),
    );
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
        if !update["current"].is_null() {
            let current: ConnectRecord = serde_json::from_value(update["current"].clone()).unwrap();
            let key = serde_json::to_string(&(&current.kind, &current.key)).unwrap();
            values.insert(
                key,
                serde_json::Value::String(serde_json::to_string(&current.value).unwrap()),
            );
            continue;
        }
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
        assert!(
            Some(actual) == record.value,
            "received record {} differs",
            record.key
        );
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
        let a_database = library::Database::open(a_dir.path().join("profile.sqlite"))
            .await
            .unwrap();
        let b_database = library::Database::open(b_dir.path().join("profile.sqlite"))
            .await
            .unwrap();
        let a_docs = Arc::new(
            ProfileStore::open(a_database.clone(), 1, KEY.parse().unwrap())
                .await
                .unwrap(),
        );
        let b_docs = Arc::new(
            ProfileStore::open(b_database.clone(), 2, KEY.parse().unwrap())
                .await
                .unwrap(),
        );
        let source = library::SourceId::new("music");
        for (database, documents, identity) in [
            (&a_database, &a_docs, a.identity()),
            (&b_database, &b_docs, b.identity()),
        ] {
            database
                .reconcile_source(&source, "webdav", "Files")
                .await
                .unwrap();
            documents.bind_profile(&profile, &identity).await.unwrap();
            database.connect_set_active(true).await.unwrap();
        }
        let mut records = Vec::new();
        for index in 0..20 {
            let object_id = format!("track-{index}");
            let path = format!("https://files.example.test/music/{object_id}.flac");
            a_database
                .catalog("music", None)
                .await
                .unwrap()
                .write_file_observations(
                    &[(
                        library::LocalFileWrite {
                            path: path.clone(),
                            root: "https://files.example.test/music".into(),
                            relative_path: format!("{object_id}.flac"),
                            kind: library::LocalFileKind::Media,
                            size_bytes: Some(12),
                            mtime_ns: 0,
                            device_id: None,
                            inode: None,
                            native_id: None,
                            picture_index: None,
                            artist_pictures: None,
                            revision: Some("v1".into()),
                            parse_version: Some(1),
                            state: library::LocalFileState::Accepted,
                        },
                        Vec::new(),
                    )],
                    vec![library::TrackObservation {
                        object_id: object_id.clone(),
                        media_uri: Some(library::source_entity_uri(&source, "track", &object_id)),
                        source_path: Some(path.clone()),
                        metadata: library::TrackMetadata {
                            title: format!("Track {index}"),
                            comment: Some("x".repeat(512 * 1024)),
                            ..Default::default()
                        },
                        ..Default::default()
                    }],
                    true,
                )
                .await
                .unwrap();
            records.push(ConnectRecord {
                kind: "catalog:file".into(),
                key: serde_json::to_string(&("music", path)).unwrap(),
                value: None,
            });
        }
        let keys: Vec<_> = records
            .iter()
            .map(|record| (record.kind.clone(), record.key.clone()))
            .collect();
        let records = a_docs.current_records(&keys).await.unwrap();
        assert_eq!(records.len(), 20);
        for (index, record) in records.iter().enumerate() {
            let value = record.value.as_ref().unwrap();
            assert_eq!(
                value["tracks"][0]["metadata"]["title"],
                format!("Track {index}")
            );
            assert!(
                value["tracks"][0]["metadata"]["comment"].as_str()
                    == Some("x".repeat(512 * 1024).as_str()),
                "track {index} must retain its full 512 KiB comment"
            );
        }
        a.attach_documents(a_docs.clone()).await.unwrap();
        pair(&a, &mut a_events, &b, &mut b_events, &profile).await;
        b.attach_documents(b_docs.clone()).await.unwrap();
        received(&a_docs, &b_docs, &records).await;
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    })
    .await
    .expect("Collection publication starved enrollment");
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
        let records = [ConnectRecord { kind: "preference".into(), key: "display".into(),
            value: Some(serde_json::json!({"content": "x".repeat(128 * 1024)})) }];
        a_docs.write_records(&records).await.unwrap();
        a.attach_documents(a_docs.clone()).await.unwrap();
        b.attach_documents(b_docs.clone()).await.unwrap();
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
            loop {
                if let NetworkEvent::StreamRequest { peer, body, reply, .. } = a_events.recv().await.unwrap() {
                    assert_eq!(peer, b.identity());
                    let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let (metadata, reader) = rufin_connect::media::open_file(&original, request["offset"].as_u64().unwrap(), request["validator"].as_str()).await.unwrap();
                    assert!(reply.send(Ok(rufin_connect::media::PeerByteResponse { metadata: serde_json::to_value(metadata).unwrap(), reader: Box::pin(reader) })).is_ok());
                    break;
                }
            }
            validator = transfer.await.unwrap();
            assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"original media bytes");
            assert_eq!(tokio::fs::read(&original).await.unwrap(), b"original media bytes");
        }
        let c_dir = tempfile::tempdir().unwrap();
        let (c, mut c_events) = node(c_dir.path(), "Third device").await;
        pair(&a, &mut a_events, &c, &mut c_events, &profile).await;
        let c_docs = documents(c_dir.path(), 3).await;
        c.attach_documents(c_docs.clone()).await.unwrap();
        received(&a_docs, &c_docs, &records).await;
        let third_record = ConnectRecord { kind: "preference".into(), key: "third-device-edit".into(),
            value: Some(serde_json::json!("made on C")) };
        c_docs.write_records(std::slice::from_ref(&third_record)).await.unwrap();
        a.refresh();
        b.refresh();
        received(&c_docs, &b_docs, &[third_record]).await;
        assert!(b.members().await.unwrap().contains(&c.identity()));
        let third = c_dir.path().join("third.flac");
        tokio::fs::write(&third, b"third device media").await.unwrap();
        let destination = b_dir.path().join("third.flac");
        // B has never received C's invitation. Its address and trust arrived
        // through A's authenticated sync connection.
        let requester = b.clone();
        let host = c.identity();
        let target = destination.clone();
        let transfer = tokio::spawn(async move {
            let response = requester.stream_request(&host, b"third device media".to_vec()).await.unwrap();
            let metadata = serde_json::from_value(response.metadata).unwrap();
            rufin_connect::media::receive_file(response.reader, &metadata, &target, CancellationToken::new(), watch::channel(0).0).await.unwrap();
        });
        loop {
            if let NetworkEvent::StreamRequest { peer, body, reply, .. } = c_events.recv().await.unwrap() {
                assert_eq!(peer, b.identity());
                assert_eq!(body, b"third device media");
                let (metadata, reader) = rufin_connect::media::open_file(&third, 0, None).await.unwrap();
                assert!(reply.send(Ok(rufin_connect::media::PeerByteResponse { metadata: serde_json::to_value(metadata).unwrap(), reader: Box::pin(reader) })).is_ok());
                break;
            }
        }
        transfer.await.unwrap();
        assert_eq!(tokio::fs::read(destination).await.unwrap(), b"third device media");
        a.remove_member(&b.identity()).await.unwrap();
        assert!(a_docs.device_name(&b.identity()).await.unwrap().is_none());
        c.refresh();
        b.refresh();
        let second = a_dir.path().join("second.flac");
        tokio::fs::write(&second, b"not authorized after removal").await.unwrap();
        assert!(b.stream_request(&a.identity(), b"denied after removal".to_vec()).await.is_err());
        while c.members().await.unwrap().contains(&b.identity()) || c_docs.device_name(&b.identity()).await.unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(c_docs.device_name(&b.identity()).await.unwrap().is_none());
        tokio::fs::write(&third, b"third device private media after removal").await.unwrap();
        assert!(b.stream_request(&c.identity(), b"denied after removal".to_vec()).await.is_err());
        // The removed device learns that it is no longer enrolled, while its
        // already received library remains available locally.
        while !b.members().await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!b_docs.changes_in_stream(0, 64, SyncStream::Authored).await.unwrap().is_empty());
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
        b.refresh();
        while !b.members().await.unwrap().contains(&c.identity()) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        c.shutdown().await.unwrap();
        assert!(a.members().await.unwrap().contains(&c.identity()));
        assert!(b.members().await.unwrap().contains(&c.identity()));
        a_docs
            .write_records(&[ConnectRecord {
                kind: "device".into(),
                key: "file-only".into(),
                value: Some(serde_json::json!("Profile file device")),
            }])
            .await
            .unwrap();

        for value in ["first edit", "edit after the first acknowledgement"] {
            while b_events.try_recv().is_ok() {}
            a_docs.write_records(&[record(value)]).await.unwrap();
            b.refresh();
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
        let retained = b_docs.revision().await.unwrap();
        b.close_profile().await.unwrap();
        tokio::time::timeout(Duration::from_secs(20), b.leave_profile(&profile))
            .await
            .expect("Departure notification exceeded the network timeout")
            .unwrap();
        assert!(a.check_membership(&b.identity()).await.is_err());
        assert!(a.members().await.unwrap().contains(&c.identity()));
        assert!(a_docs.device_name(&b.identity()).await.unwrap().is_none());
        assert_eq!(
            a_docs.device_name(&c.identity()).await.unwrap().as_deref(),
            Some("Offline peer")
        );
        assert_eq!(
            a_docs.device_name("file-only").await.unwrap().as_deref(),
            Some("Profile file device")
        );
        b.open_profile(&profile, false, Vec::new()).await.unwrap();
        assert!(b.members().await.unwrap().is_empty());
        assert_eq!(b_docs.revision().await.unwrap(), retained);
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
        peer.refresh();
        received(&local_docs, &peer_docs, &[record("edited while offline")]).await;
        network.shutdown().await.unwrap();
        peer.shutdown().await.unwrap();
    })
    .await
    .expect("Offline edit did not catch up after restart");
}
