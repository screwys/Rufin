use super::*;

const KEY: &str = "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33";
use serde_json::json;

fn preference(value: usize) -> ConnectRecord {
    ConnectRecord {
        kind: "preference".into(),
        key: "theme".into(),
        value: Some(json!(value)),
    }
}

async fn snapshot(store: &ProfileStore, name: &str) -> Vec<u8> {
    sqlx::query_scalar("SELECT snapshot FROM documents WHERE name=?1")
        .bind(name)
        .fetch_one(&mut *store.writer().await.unwrap())
        .await
        .unwrap()
}

async fn edit_history(store: &ProfileStore) {
    for value in 0..80 {
        store.write_records(&[preference(value)]).await.unwrap();
    }
}

#[tokio::test]
async fn converged_playlist_history_prunes_without_changing_saved_values() {
    let directory = tempfile::tempdir().unwrap();
    let a_path = directory.path().join("a.sqlite");
    let a = ProfileStore::open(
        crate::Database::open(&a_path).await.unwrap(),
        1,
        KEY.parse().unwrap(),
    )
    .await
    .unwrap();
    let b = ProfileStore::open(
        crate::Database::open(&directory.path().join("b.sqlite"))
            .await
            .unwrap(),
        2,
        KEY.parse().unwrap(),
    )
    .await
    .unwrap();
    a.bind_profile("profile", "a").await.unwrap();
    b.bind_profile("profile", "b").await.unwrap();
    let empty = directory.path().join("empty.sqlite");
    b.export_database(&empty, &KEY.parse().unwrap(), &[], &[])
        .await
        .unwrap();
    assert!(
        !b.merge_database(&empty, &KEY.parse().unwrap())
            .await
            .unwrap()
    );
    let members = vec!["a".into(), "b".into()];
    a.register_members(&members).await.unwrap();
    let entry = |id: &str, position: usize| ConnectRecord {
        kind: "entry".into(),
        key: json!([null, "mix", id]).to_string(),
        value: Some(
            json!({"playlist":"[null,\"mix\"]","object_id":id,"media_uri":format!("https://example.org/{id}.flac"),"position":position,"snapshot_at":1}),
        ),
    };
    for turn in 0..80 {
        a.write_records(&[entry("one", turn % 2), entry("two", (turn + 1) % 2)])
            .await
            .unwrap();
    }
    a.database.connect_set_active(true).await.unwrap();
    a.database
        .reconcile_source(&crate::SourceId::new("server"), "server", "Server")
        .await
        .unwrap();
    a.database
        .catalog("server", None)
        .await
        .unwrap()
        .apply_catalog(
            &crate::CatalogBatch {
                tracks: vec![crate::TrackObservation {
                    object_id: "observed".into(),
                    metadata: crate::TrackMetadata {
                        title: "Source observation".into(),
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
    let name = "playlist:[null,\"mix\"]";
    let old_file = directory.path().join("old");
    a.export_database(&old_file, &KEY.parse().unwrap(), &[], &[])
        .await
        .unwrap();
    b.merge_database(&old_file, &KEY.parse().unwrap())
        .await
        .unwrap();
    let file = directory.path().join("b-profile");
    b.register_members(&members).await.unwrap();
    b.export_database(&file, &KEY.parse().unwrap(), &[], &[])
        .await
        .unwrap();
    assert!(
        !a.merge_database(&file, &KEY.parse().unwrap())
            .await
            .unwrap()
    );
    let before = snapshot(&a, name).await;
    let versions = a
        .changes_in_stream(0, 10, SyncStream::Authored)
        .await
        .unwrap();
    let file_revision = a.revision().await.unwrap();

    drop(a);
    let a = ProfileStore::open(
        crate::Database::open(&a_path).await.unwrap(),
        1,
        KEY.parse().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(a.prune_history("a", &members, 10).await.unwrap(), 1);
    let compact = snapshot(&a, name).await;
    assert!(compact.len() < before.len());
    let doc = LoroDoc::new();
    doc.import(&compact).unwrap();
    assert!(doc.is_shallow());
    assert_eq!(
        a.changes_in_stream(0, 10, SyncStream::Authored)
            .await
            .unwrap(),
        versions
    );
    assert_eq!(a.revision().await.unwrap(), file_revision);
    assert!(
        !a.merge_database(&old_file, &KEY.parse().unwrap())
            .await
            .unwrap()
    );
    assert_eq!(snapshot(&a, name).await, compact);

    a.write_records(&[entry("four", 2)]).await.unwrap();
    let delta = directory.path().join("pruned-edit.sqlite");
    a.export_database(&delta, &KEY.parse().unwrap(), &[], &[])
        .await
        .unwrap();
    assert!(
        b.merge_database(&delta, &KEY.parse().unwrap())
            .await
            .unwrap()
    );
    let sent = LoroDoc::from_snapshot(&snapshot(&a, name).await).unwrap();
    let received = LoroDoc::from_snapshot(&snapshot(&b, name).await).unwrap();
    assert_eq!(records(&sent).unwrap(), records(&received).unwrap());
    assert_eq!(sent.oplog_vv(), received.oplog_vv());

    // An unpruned peer's full file includes old operations and new descendants.
    b.write_records(&[entry("three", 2)]).await.unwrap();
    b.export_database(&file, &KEY.parse().unwrap(), &[], &[])
        .await
        .unwrap();
    assert!(
        a.merge_database(&file, &KEY.parse().unwrap())
            .await
            .unwrap()
    );
    let updated = snapshot(&a, name).await;
    let updated_doc = LoroDoc::new();
    updated_doc.import(&updated).unwrap();
    assert!(updated_doc.is_shallow());
    let full_doc = LoroDoc::new();
    full_doc.import(&snapshot(&b, name).await).unwrap();
    assert_eq!(records(&updated_doc).unwrap(), records(&full_doc).unwrap());
    assert_eq!(updated_doc.oplog_vv(), full_doc.oplog_vv());

    let c = ProfileStore::open(
        crate::Database::open(&directory.path().join("c.sqlite"))
            .await
            .unwrap(),
        3,
        KEY.parse().unwrap(),
    )
    .await
    .unwrap();
    assert!(
        c.merge_database(&old_file, &KEY.parse().unwrap())
            .await
            .unwrap()
    );
    assert!(
        c.merge_database(&delta, &KEY.parse().unwrap())
            .await
            .unwrap()
    );
    let fresh = LoroDoc::from_snapshot(&snapshot(&c, name).await).unwrap();
    assert_eq!(records(&fresh).unwrap(), records(&sent).unwrap());
    assert_eq!(fresh.oplog_vv(), sent.oplog_vv());
    a.export_database(&file, &KEY.parse().unwrap(), &[], &[])
        .await
        .unwrap();
    assert!(
        c.merge_database(&file, &KEY.parse().unwrap())
            .await
            .unwrap()
    );
    let bootstrapped = LoroDoc::new();
    bootstrapped.import(&snapshot(&c, name).await).unwrap();
    assert_eq!(
        records(&bootstrapped).unwrap(),
        records(&updated_doc).unwrap()
    );
    assert_eq!(bootstrapped.oplog_vv(), updated_doc.oplog_vv());
}

#[tokio::test]
async fn offline_member_blocks_pruning_until_removed() {
    let directory = tempfile::tempdir().unwrap();
    let a = ProfileStore::open(
        crate::Database::open(&directory.path().join("a.sqlite"))
            .await
            .unwrap(),
        1,
        KEY.parse().unwrap(),
    )
    .await
    .unwrap();
    let members = vec!["a".into(), "offline".into()];
    a.register_members(&members).await.unwrap();
    a.write_records(&[ConnectRecord {
        kind: "device".into(),
        key: "offline".into(),
        value: Some(json!("Offline laptop")),
    }])
    .await
    .unwrap();
    edit_history(&a).await;
    let before = snapshot(&a, "preference:theme").await;
    a.prune_history("a", &members, 10).await.unwrap();
    assert_eq!(snapshot(&a, "preference:theme").await, before);
    a.write_records(&[ConnectRecord {
        kind: "device".into(),
        key: "offline".into(),
        value: None,
    }])
    .await
    .unwrap();
    a.prune_history("a", &["a".into()], 10).await.unwrap();
    assert!(snapshot(&a, "preference:theme").await.len() < before.len());
}

#[tokio::test]
async fn acknowledging_an_unseen_concurrent_branch_does_not_prune_it_away() {
    let directory = tempfile::tempdir().unwrap();
    let a = ProfileStore::open(
        crate::Database::open(&directory.path().join("a.sqlite"))
            .await
            .unwrap(),
        1,
        KEY.parse().unwrap(),
    )
    .await
    .unwrap();
    let b = ProfileStore::open(
        crate::Database::open(&directory.path().join("b.sqlite"))
            .await
            .unwrap(),
        2,
        KEY.parse().unwrap(),
    )
    .await
    .unwrap();
    a.bind_profile("profile", "a").await.unwrap();
    b.bind_profile("profile", "b").await.unwrap();
    let members = vec!["a".into(), "b".into()];
    a.register_members(&members).await.unwrap();
    a.write_records(&[preference(0)]).await.unwrap();
    let file = directory.path().join("profile");
    a.export_database(&file, &KEY.parse().unwrap(), &[], &[])
        .await
        .unwrap();
    b.merge_database(&file, &KEY.parse().unwrap())
        .await
        .unwrap();
    b.write_records(&[preference(100)]).await.unwrap();
    edit_history(&a).await;
    a.export_database(&file, &KEY.parse().unwrap(), &[], &[])
        .await
        .unwrap();
    b.merge_database(&file, &KEY.parse().unwrap())
        .await
        .unwrap();
    let before = snapshot(&a, "preference:theme").await;
    a.acknowledge_versions(
        "b",
        &b.changes_in_stream(0, 10, SyncStream::Authored)
            .await
            .unwrap(),
        &members,
    )
    .await
    .unwrap();
    a.prune_history("a", &members, 10).await.unwrap();
    assert_eq!(snapshot(&a, "preference:theme").await, before);
    b.export_database(&file, &KEY.parse().unwrap(), &[], &[])
        .await
        .unwrap();
    a.merge_database(&file, &KEY.parse().unwrap())
        .await
        .unwrap();
    a.prune_history("a", &members, 10).await.unwrap();
    let left = LoroDoc::new();
    left.import(&snapshot(&a, "preference:theme").await)
        .unwrap();
    let right = LoroDoc::new();
    right
        .import(&snapshot(&b, "preference:theme").await)
        .unwrap();
    assert_eq!(left.oplog_vv(), right.oplog_vv());
    assert_eq!(left.get_deep_value(), right.get_deep_value());
}
