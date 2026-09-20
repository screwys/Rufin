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
        sqlx::query("UPDATE catalog.tracks SET media_uri=?1 WHERE media_uri=?2")
            .bind(new)
            .bind(old)
            .execute(&mut connection)
            .await
            .unwrap();
    }
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
async fn connect_uses_scanned_originals_and_repairs_existing_folder_references() {
    let fixture = support::fixture().await;
    let root = fixture._directory.path().join("Music");
    std::fs::create_dir(&root).unwrap();
    let original = root.join("track.flac");
    std::fs::write(&original, b"original audio").unwrap();
    let uri = &fixture.track_uris[0];
    let mut connection = support::connection(&fixture.path).await;
    sqlx::query("UPDATE catalog.tracks SET source_path=?1 WHERE media_uri=?2")
        .bind(original.to_str().unwrap())
        .bind(uri)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO catalog.local_files(source_key,path,root,relative_path,kind,mtime_ns,state) VALUES(?1,?2,?3,'track.flac','media',0,'accepted')")
        .bind(fixture.source).bind(original.to_str().unwrap()).bind(root.to_str().unwrap())
        .execute(&mut connection).await.unwrap();
    assert_eq!(
        fixture.database.connect_original_file(uri).await.unwrap(),
        Some(original.clone())
    );
    assert!(
        fixture
            .database
            .connect_local_file(uri)
            .await
            .unwrap()
            .is_none()
    );
    // Repair already-seeded profiles using the existing index, without rescanning files.
    sqlx::query("UPDATE main.connect_seed SET cursor=0,complete=(kind<>'local_reference')")
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
    assert_eq!(
        fixture.database.connect_local_file(uri).await.unwrap(),
        Some(copy)
    );
    assert_eq!(std::fs::read(original).unwrap(), b"original audio");
}

#[tokio::test]
async fn connect_catalog_projection_preserves_collection_relations_without_local_copies() {
    let fixture = support::fixture().await;
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
        records.extend(page.iter().map(|c| c.record.clone()));
        fixture.database.connect_acknowledge(&page).await.unwrap();
    }
    records.sort_by_key(|r| match r.kind.as_str() {
        "playlist" => 0,
        "album" | "artist" | "genre" | "mood" | "folder" => 1,
        "track" => 2,
        "entry" => 3,
        _ => 4,
    });
    let path = fixture._directory.path().join("replica.sqlite");
    let replica = Database::open(&path).await.unwrap();
    replica.connect_apply(&records).await.unwrap();
    assert_eq!(
        fixture.database.all_source_counts().await.unwrap(),
        replica.all_source_counts().await.unwrap()
    );
    let mut original = support::connection(&fixture.path).await;
    let mut imported = support::connection(&path).await;
    for table in [
        "tracks",
        "albums",
        "artists",
        "genres",
        "moods",
        "folders",
        "track_artists",
        "album_artists",
        "track_genres",
        "album_genres",
        "track_moods",
        "track_folders",
    ] {
        let query = format!("SELECT count(*) FROM catalog.{table}");
        let a = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(query.clone()))
            .fetch_one(&mut original)
            .await
            .unwrap();
        let b = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(query))
            .fetch_one(&mut imported)
            .await
            .unwrap();
        assert_eq!(a, b, "{table}");
    }
    assert!(
        replica
            .source_identity_key(&SourceId::new("source"))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM catalog.tracks WHERE album_key IS NOT NULL"
        )
        .fetch_one(&mut imported)
        .await
        .unwrap(),
        fixture.tracks.len() as i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM main.local_locators")
            .fetch_one(&mut imported)
            .await
            .unwrap(),
        0
    );
    drop(original);
    drop(imported);
    fixture
        .database
        .remove_source(&SourceId::new("source"))
        .await
        .unwrap();
    let mut removed = std::collections::BTreeSet::new();
    loop {
        let page = fixture.database.connect_changes().await.unwrap();
        if page.is_empty() {
            break;
        }
        for change in &page {
            if change.record.value.is_none() {
                removed.insert((change.record.kind.clone(), change.record.key.clone()));
            }
        }
        fixture.database.connect_acknowledge(&page).await.unwrap();
    }
    for record in records.iter().filter(|r| {
        matches!(
            r.kind.as_str(),
            "track"
                | "album"
                | "artist"
                | "genre"
                | "mood"
                | "folder"
                | "track_artists"
                | "track_genres"
                | "track_moods"
                | "track_folders"
                | "album_artists"
                | "album_genres"
        )
    }) {
        assert!(
            removed.contains(&(record.kind.clone(), record.key.clone())),
            "missing cascade identity: {record:?}"
        );
    }
}
