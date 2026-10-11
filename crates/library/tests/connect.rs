use super::support;
use library::{Database, SourceId};

#[tokio::test]
async fn local_media_download_pages_include_files_and_cue_segments_without_copies() {
    let fixture = support::fixture().await;
    let file = url::Url::from_file_path(fixture._directory.path().join("album.flac"))
        .unwrap()
        .to_string();
    let cue = library::cue_media_uri("album.cue", &file, 0, 1000);
    let mut connection = support::connection(&fixture.path).await;
    for (old, new) in fixture.track_uris.iter().zip([&file, &cue]) {
        sqlx::query("UPDATE tracks SET media_uri=?1 WHERE media_uri=?2")
            .bind(new)
            .bind(old)
            .execute(&mut connection)
            .await
            .unwrap();
    }
    assert!(
        fixture
            .database
            .connect_local_media_page("")
            .await
            .unwrap()
            .is_empty()
    );
    fixture
        .database
        .create_playlist(None, "Saved", &[file.clone(), cue.clone()])
        .await
        .unwrap();
    assert_eq!(
        fixture.database.connect_local_media_page("").await.unwrap(),
        vec![file.clone(), cue.clone()]
    );
    assert_eq!(
        fixture
            .database
            .connect_local_media_page(&file)
            .await
            .unwrap(),
        vec![cue.clone()]
    );
    assert!(
        fixture
            .database
            .connect_local_media_page(&cue)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn connect_uses_scanned_originals_and_seeds_saved_media_references() {
    let fixture = support::fixture().await;
    let root = fixture._directory.path().join("Music");
    std::fs::create_dir(&root).unwrap();
    let original = root.join("track.flac");
    std::fs::write(&original, b"original audio").unwrap();
    let uri = &fixture.track_uris[0];
    let mut connection = support::connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET source_path=?1 WHERE media_uri=?2")
        .bind(original.to_str().unwrap())
        .bind(uri)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO local_files(source_key,path,root,relative_path,kind,mtime_ns,state) VALUES(?1,?2,?3,'track.flac','media',0,'accepted')")
        .bind(fixture.source).bind(original.to_str().unwrap()).bind(root.to_str().unwrap())
        .execute(&mut connection).await.unwrap();
    assert!(matches!(
        fixture
            .database
            .connect_original_file(uri, |path| path)
            .await
            .unwrap(),
        Some(library::LocalMediaLocation::File(path)) if path == original
    ));
    assert!(
        fixture
            .database
            .connect_local_file(uri)
            .await
            .unwrap()
            .is_none()
    );
    fixture
        .database
        .create_playlist(None, "Saved", std::slice::from_ref(uri))
        .await
        .unwrap();
    sqlx::query("UPDATE main.connect_seed SET cursor=0,complete=(kind<>'track')")
        .execute(&mut connection)
        .await
        .unwrap();
    fixture.database.connect_seed_page().await.unwrap();
    let changes = fixture.database.connect_changes().await.unwrap();
    let value = changes
        .iter()
        .find(|change| change.record.key == *uri)
        .unwrap()
        .record
        .value
        .as_ref()
        .unwrap();
    assert_eq!(value["root_label"], "Music");
    assert_eq!(value["relative_path"], "track.flac");
    assert!(value["root_id"].as_str().is_some());
    assert!(value.get("_local_root").is_none());
    fixture
        .database
        .connect_acknowledge(&changes)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .database
            .connect_roots("source")
            .await
            .unwrap()
            .len(),
        1
    );

    // Quality changes affect downloaded copies, not the scanned original.
    let copy = fixture._directory.path().join("download.audio");
    std::fs::write(&copy, b"downloaded audio").unwrap();
    fixture
        .database
        .connect_set_local_file(uri, &copy, true)
        .await
        .unwrap();
    let receipt = library::ConnectMediaFile {
        media_uri: uri.clone(),
        encoding: "original".into(),
        revision: "1".into(),
        path: url::Url::from_file_path(&copy).unwrap().to_string(),
        managed: true,
        hash: None,
    };
    fixture
        .database
        .connect_save_media_file(&receipt)
        .await
        .unwrap();
    let next = library::ConnectMediaFile {
        encoding: "mp3".into(),
        ..receipt.clone()
    };
    fixture
        .database
        .connect_save_media_file(&next)
        .await
        .unwrap();
    fixture
        .database
        .connect_forget_media_file(&receipt)
        .await
        .unwrap();
    assert!(matches!(
        fixture.database.connect_local_file(uri).await.unwrap(),
        Some(library::LocalMediaLocation::File(path)) if path == copy
    ));
    assert_eq!(std::fs::read(original).unwrap(), b"original audio");
}

#[tokio::test]
async fn connect_saved_playlist_replay_keeps_duplicate_media_without_catalog_membership() {
    let fixture = support::fixture().await;
    let uris = vec![
        fixture.track_uris[0].clone(),
        fixture.track_uris[1].clone(),
        fixture.track_uris[0].clone(),
    ];
    let (playlist, identity) = fixture
        .database
        .create_playlist(None, "Saved", &uris)
        .await
        .unwrap()
        .unwrap();
    fixture
        .database
        .connect_capture_enabled(true)
        .await
        .unwrap();
    while fixture.database.connect_seed_page().await.unwrap() {}
    let mut records = Vec::new();
    loop {
        let page = fixture.database.connect_changes().await.unwrap();
        if page.is_empty() {
            break;
        }
        records.extend(page.iter().map(|change| change.record.clone()));
        fixture.database.connect_acknowledge(&page).await.unwrap();
    }
    assert!(records.iter().all(|record| !matches!(
        record.kind.as_str(),
        "album" | "artist" | "genre" | "mood" | "folder" | "native_playlist" | "native_entry"
    )));
    let path = fixture._directory.path().join("received.sqlite");
    let received = Database::open(&path).await.unwrap();
    assert!(received.connect_apply(&records).await.unwrap());
    assert!(!received.connect_apply(&records).await.unwrap());
    let imported_playlist = received
        .playlist_key_by_identity(None, &identity, &library::ReadCancellation::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        received
            .selected_track_uris(
                &library::SongQuery::new(library::SongScope::Playlist(imported_playlist))
                    .selection(),
                &library::ReadCancellation::new()
            )
            .await
            .unwrap(),
        uris
    );
    let mut imported = support::connection(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut imported)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks WHERE indexed")
            .fetch_one(&mut imported)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM local_locators")
            .fetch_one(&mut imported)
            .await
            .unwrap(),
        0
    );
    assert!(
        received
            .source_identity_key(&SourceId::new("source"))
            .await
            .unwrap()
            .is_some()
    );
    fixture
        .database
        .delete_playlist(None, playlist)
        .await
        .unwrap();
    let changes = fixture.database.connect_changes().await.unwrap();
    let tombstones = changes
        .into_iter()
        .map(|change| change.record)
        .collect::<Vec<_>>();
    assert!(
        tombstones
            .iter()
            .any(|record| record.kind == "playlist" && record.value.is_none())
    );
    assert!(received.connect_apply(&tombstones).await.unwrap());
    assert!(
        received
            .playlist_key_by_identity(None, &identity, &library::ReadCancellation::new())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut imported)
            .await
            .unwrap(),
        2
    );
}
