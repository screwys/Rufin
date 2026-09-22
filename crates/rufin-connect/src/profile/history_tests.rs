use super::*;
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
        .fetch_one(&mut *store.connection.lock().await)
        .await
        .unwrap()
}

async fn edit_history(store: &ProfileStore) {
    for value in 0..80 {
        store.write_records(&[preference(value)]).await.unwrap();
    }
}

#[tokio::test]
async fn converged_playlist_history_prunes_without_changing_sync_or_projection() {
    let directory = tempfile::tempdir().unwrap();
    let a_path = directory.path().join("a.sqlite");
    let a = ProfileStore::open(&a_path, 1).await.unwrap();
    let b = ProfileStore::open(&directory.path().join("b.sqlite"), 2)
        .await
        .unwrap();
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
    let name = "playlist:[null,\"mix\"]";
    let old_file = directory.path().join("old");
    a.export_snapshot(&old_file).await.unwrap();
    b.import_snapshot(std::fs::File::open(&old_file).unwrap())
        .await
        .unwrap();
    let file = directory.path().join("b-profile");
    b.export_device_snapshot(std::fs::File::create(&file).unwrap(), "b")
        .await
        .unwrap();
    b.finish_device_snapshot(
        std::fs::OpenOptions::new().write(true).open(&file).unwrap(),
        &members,
    )
    .await
    .unwrap();
    assert!(
        !a.import_snapshot(std::fs::File::open(&file).unwrap())
            .await
            .unwrap()
    );
    let before = snapshot(&a, name).await;
    let versions = a.changes(0, 10).await.unwrap();
    let file_revision = a.revision().await.unwrap();
    assert!(!a.projection_pending().await.unwrap());
    drop(a);
    let a = ProfileStore::open(&a_path, 1).await.unwrap();
    assert_eq!(a.prune_history("a", &members, 10).await.unwrap(), 1);
    let compact = snapshot(&a, name).await;
    assert!(compact.len() < before.len());
    let doc = LoroDoc::new();
    doc.import(&compact).unwrap();
    assert!(doc.is_shallow());
    assert_eq!(a.changes(0, 10).await.unwrap(), versions);
    assert!(a.revision().await.unwrap() > file_revision);
    assert!(!a.projection_pending().await.unwrap());
    assert!(
        !a.import_snapshot(std::fs::File::open(&old_file).unwrap())
            .await
            .unwrap()
    );
    assert_eq!(snapshot(&a, name).await, compact);

    // An unpruned peer's full file includes old operations and new descendants.
    b.write_records(&[entry("three", 2)]).await.unwrap();
    b.export_snapshot(&file).await.unwrap();
    assert!(
        a.import_snapshot(std::fs::File::open(&file).unwrap())
            .await
            .unwrap()
    );
    let updated = snapshot(&a, name).await;
    let updated_doc = LoroDoc::new();
    updated_doc.import(&updated).unwrap();
    assert!(updated_doc.is_shallow());
    let full_doc = LoroDoc::new();
    full_doc.import(&snapshot(&b, name).await).unwrap();
    assert_eq!(updated_doc.get_deep_value(), full_doc.get_deep_value());

    let c = ProfileStore::open(&directory.path().join("c.sqlite"), 3)
        .await
        .unwrap();
    a.export_snapshot(&file).await.unwrap();
    assert!(
        c.import_snapshot(std::fs::File::open(&file).unwrap())
            .await
            .unwrap()
    );
    let bootstrapped = LoroDoc::new();
    bootstrapped.import(&snapshot(&c, name).await).unwrap();
    assert_eq!(bootstrapped.get_deep_value(), updated_doc.get_deep_value());
    assert_eq!(bootstrapped.oplog_vv(), updated_doc.oplog_vv());
}

#[tokio::test]
async fn offline_member_blocks_pruning_until_removed() {
    let directory = tempfile::tempdir().unwrap();
    let a = ProfileStore::open(&directory.path().join("a.sqlite"), 1)
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
    let a = ProfileStore::open(&directory.path().join("a.sqlite"), 1)
        .await
        .unwrap();
    let b = ProfileStore::open(&directory.path().join("b.sqlite"), 2)
        .await
        .unwrap();
    let members = vec!["a".into(), "b".into()];
    a.register_members(&members).await.unwrap();
    a.write_records(&[preference(0)]).await.unwrap();
    let file = directory.path().join("profile");
    a.export_snapshot(&file).await.unwrap();
    b.import_snapshot(std::fs::File::open(&file).unwrap())
        .await
        .unwrap();
    b.write_records(&[preference(100)]).await.unwrap();
    edit_history(&a).await;
    a.export_snapshot(&file).await.unwrap();
    b.import_snapshot(std::fs::File::open(&file).unwrap())
        .await
        .unwrap();
    let before = snapshot(&a, "preference:theme").await;
    a.acknowledge_versions("b", &b.changes(0, 10).await.unwrap(), &members)
        .await
        .unwrap();
    a.prune_history("a", &members, 10).await.unwrap();
    assert_eq!(snapshot(&a, "preference:theme").await, before);
    b.export_snapshot(&file).await.unwrap();
    a.import_snapshot(std::fs::File::open(&file).unwrap())
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
