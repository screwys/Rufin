use super::support;
use library::{Database, SourceId};
const PROFILE_KEY: &str =
    "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33";

#[tokio::test]
async fn connect_uses_scanned_originals_and_seeds_saved_media_references() {
    let fixture = support::fixture().await;
    let root = fixture._directory.path().join("Music");
    std::fs::create_dir(&root).unwrap();
    let original = root.join("track.flac");
    std::fs::write(&original, b"original audio").unwrap();
    let uri = &fixture.track_uris[0];
    let mut connection = support::connection(&fixture.path).await;
    sqlx::query("UPDATE sources SET kind='local' WHERE source_key=?1")
        .bind(fixture.source)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET source_path=?1 WHERE media_uri=?2")
        .bind(original.to_str().unwrap())
        .bind(uri)
        .execute(&mut connection)
        .await
        .unwrap();
    let mut catalog = fixture.database.catalog("source", None).await.unwrap();
    catalog
        .write_file_observations(
            &[(
                library::LocalFileWrite {
                    path: original.to_string_lossy().into_owned(),
                    root: root.to_string_lossy().into_owned(),
                    relative_path: "track.flac".into(),
                    kind: library::LocalFileKind::Media,
                    size_bytes: None,
                    mtime_ns: 0,
                    device_id: None,
                    inode: None,
                    native_id: None,
                    picture_index: None,
                    artist_pictures: None,
                    revision: None,
                    parse_version: Some(1),
                    state: library::LocalFileState::Accepted,
                },
                Vec::new(),
            )],
            Vec::new(),
            true,
        )
        .await
        .unwrap();
    drop(catalog);
    let file_key: i64 = sqlx::query_scalar(
        "SELECT local_file_key FROM local_files WHERE source_key=?1 AND path=?2",
    )
    .bind(fixture.source)
    .bind(original.to_str().unwrap())
    .fetch_one(&mut connection)
    .await
    .unwrap();
    sqlx::query("UPDATE tracks SET local_file_key=(SELECT local_file_key FROM local_files WHERE source_key=tracks.source_key AND path=tracks.source_path) WHERE media_uri=?1").bind(uri).execute(&mut connection).await.unwrap();
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
        .create_playlist(
            None,
            "Saved",
            &library::CatalogQuery::uris(
                library::CatalogKind::Track,
                (std::slice::from_ref(uri)).to_vec(),
            )
            .selection(),
        )
        .await
        .unwrap();
    let original_reference = fixture
        .database
        .connect_track_reference(uri)
        .await
        .unwrap()
        .unwrap();
    let value = &original_reference;
    assert_eq!(value["media_uri"], *uri);
    assert_eq!(value["root_label"], "Music");
    assert_eq!(value["relative_path"], "track.flac");
    assert!(value["root_id"].as_str().is_some());
    assert!(value.get("_local_root").is_none());
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
    let receipt = library::ConnectMediaFile {
        media_uri: uri.clone(),
        encoding: "original".into(),
        revision: "1".into(),
        path: url::Url::from_file_path(&copy).unwrap().to_string(),
        managed: true,
        ..Default::default()
    };
    fixture
        .database
        .commit_download(
            &receipt,
            Some(fixture.source),
            &library::LocalAccessWrite {
                media_uri: uri.clone(),
                origin: library::LocalAccessOrigin::Download,
                path: copy.to_string_lossy().into_owned(),
                root: fixture._directory.path().to_string_lossy().into_owned(),
                relative_path: "download.audio".into(),
                access_uri: receipt.path.clone(),
                size_bytes: 16,
                mtime_ns: 0,
                device_id: None,
                inode: None,
                parser_version: 1,
                loudness_analysis_key: None,
            },
        )
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
    assert_eq!(std::fs::read(&original).unwrap(), b"original audio");

    sqlx::query("UPDATE local_files SET parsed_stamp='[null,0,null,7]' WHERE path=?1")
        .bind(original.to_str().unwrap())
        .execute(&mut connection)
        .await
        .unwrap();
    let cover = library::ArtworkCandidate::new(
        library::ArtworkImage::Local {
            source_id: SourceId::new("source"),
            path: original.to_string_lossy().into_owned(),
            revision: "1".into(),
            picture_index: Some(0),
        },
        None,
    );
    let preview_hash = "LEHV6nWB2yk8pyo0adR*.7kCMdnj";
    fixture
        .database
        .write_artwork_preview(&cover.key, preview_hash, false)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET native_artwork=?2,artwork_kind=1 WHERE media_uri=?1")
        .bind(uri)
        .bind(serde_json::to_vec(std::slice::from_ref(&cover)).unwrap())
        .execute(&mut connection)
        .await
        .unwrap();
    let before: Vec<(String, String)> =
        sqlx::query_as("SELECT name,version FROM connect_versions ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    fixture
        .database
        .catalog("source", Some(0))
        .await
        .unwrap()
        .complete_source_observation(0, &[], None)
        .await
        .unwrap();
    let profile = library::profile::ProfileStore::open(
        fixture.database.clone(),
        1,
        PROFILE_KEY.parse().unwrap(),
    )
    .await
    .unwrap();
    profile.bind_profile("shared", "source").await.unwrap();
    let exported = fixture._directory.path().join("connect.sqlite");
    profile
        .export_database(&exported, &PROFILE_KEY.parse().unwrap(), &[], &[])
        .await
        .unwrap();
    let after: Vec<(String, String)> =
        sqlx::query_as("SELECT name,version FROM connect_versions ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(after, before);
    let receiver_path = fixture._directory.path().join("receiver.sqlite");
    let receiver = Database::open(&receiver_path).await.unwrap();
    let receiver_cover = library::ArtworkCandidate::new(
        library::ArtworkImage::Artist {
            name: "Cached artist".into(),
            musicbrainz_id: None,
        },
        None,
    );
    for family in [&cover.key, &receiver_cover.key] {
        receiver
            .write_artwork_preview(family, "LEHV6nWB2yk8pyo0adR*.7kCMdnk", false)
            .await
            .unwrap();
    }
    let receiving_profile =
        library::profile::ProfileStore::open(receiver.clone(), 2, PROFILE_KEY.parse().unwrap())
            .await
            .unwrap();
    receiving_profile
        .bind_profile("shared", "receiver")
        .await
        .unwrap();
    receiver.connect_set_active(true).await.unwrap();
    receiver
        .set_favorite(&library::FavoriteTarget::Track(uri.clone()), true)
        .await
        .unwrap();
    assert!(
        receiver
            .favorite(&library::FavoriteTarget::Track(uri.clone()))
            .await
            .unwrap()
    );
    receiving_profile.close().await.unwrap();
    library::profile::ProfileStore::prepare_database(
        &exported,
        &PROFILE_KEY.parse().unwrap(),
        &PROFILE_KEY.parse().unwrap(),
    )
    .await
    .unwrap();
    library::profile::ProfileStore::install_prepared_database(receiver.clone(), &exported)
        .await
        .unwrap();
    let receiving_profile =
        library::profile::ProfileStore::open(receiver.clone(), 2, PROFILE_KEY.parse().unwrap())
            .await
            .unwrap();
    assert_eq!(
        receiver
            .artwork_preview(&cover.key)
            .await
            .unwrap()
            .as_deref(),
        Some(preview_hash)
    );
    assert_eq!(
        receiver
            .artwork_preview(&receiver_cover.key)
            .await
            .unwrap()
            .as_deref(),
        Some("LEHV6nWB2yk8pyo0adR*.7kCMdnk")
    );
    let bindings = receiver
        .track_artwork_bindings(std::slice::from_ref(uri), &library::ReadCancellation::new())
        .await
        .unwrap();
    let candidates: Vec<library::ArtworkCandidate> = serde_json::from_slice(&bindings[0]).unwrap();
    assert_eq!(candidates[0].blur_hash.as_deref(), Some(preview_hash));
    assert!(
        receiver
            .cached_source("source", &library::ReadCancellation::new())
            .await
            .unwrap()
            .unwrap()
            .catalog_complete
    );
    let mut received = support::connection(&receiver_path).await;
    let (key, indexed, path): (i64, bool, Option<String>) =
        sqlx::query_as("SELECT track_key,indexed,source_path FROM tracks WHERE media_uri=?1")
            .bind(uri)
            .fetch_one(&mut received)
            .await
            .unwrap();
    assert_eq!(key, fixture.tracks[0].raw());
    assert!(indexed);
    assert_eq!(path.as_deref(), original.to_str());
    assert!(receiver.observed_media_file(uri).await.unwrap().is_none());
    assert!(
        receiver
            .connect_original_file(uri, |path| path)
            .await
            .unwrap()
            .is_none()
    );
    assert!(receiver.file_media_is_remote(uri, false).await.unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM local_files")
            .fetch_one(&mut received)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM connect_media_files")
            .fetch_one(&mut received)
            .await
            .unwrap(),
        0
    );
    assert!(receiver.connect_local_file(uri).await.unwrap().is_none());
    assert!(
        !receiver
            .favorite(&library::FavoriteTarget::Track(uri.clone()))
            .await
            .unwrap()
    );
    receiver
        .set_favorite(&library::FavoriteTarget::Track(uri.clone()), true)
        .await
        .unwrap();
    let file_identity = serde_json::json!(["source", original.to_str().unwrap()]).to_string();
    let mut mapped = [
        Database::connect_record_on(&mut received, "catalog:file", &file_identity)
            .await
            .unwrap(),
    ];
    assert_eq!(mapped[0].value.as_ref().unwrap()["parsed"][1], 7);
    let mapped_root = fixture._directory.path().join("ReceivingMusic");
    std::fs::create_dir(&mapped_root).unwrap();
    let mapped_path = mapped_root.join("track.flac");
    let value = mapped[0].value.as_mut().unwrap();
    value["file"]["path"] = mapped_path.to_str().unwrap().into();
    value["file"]["root"] = mapped_root.to_str().unwrap().into();
    value["file"]["relative_path"] = "track.flac".into();
    std::fs::write(&mapped_path, b"original audio").unwrap();
    let mut mapped_file: library::CatalogFile =
        serde_json::from_value(mapped[0].value.clone().unwrap()).unwrap();
    mapped_file.file.parse_version = Some(18);
    sqlx::query("INSERT INTO local_file_access_roots(source_key,root,root_id) SELECT source_key,?2,?3 FROM sources WHERE object_id=?1")
        .bind("source").bind(mapped_root.to_str().unwrap()).bind(mapped_file.root_id.as_deref().unwrap()).execute(&mut received).await.unwrap();
    let reused = receiver
        .source_file_reuse_candidates("source", std::slice::from_ref(&mapped_file.file))
        .await
        .unwrap();
    assert_eq!(reused.len(), 1);
    assert_eq!(reused[0].path, mapped_path.to_str().unwrap());
    assert_eq!(reused[0].parse_version, Some(7));
    assert_eq!(reused[0].state, library::LocalFileState::Accepted);
    assert!(reused[0].current_output);
    let mut catalog = receiver.catalog("source", None).await.unwrap();
    let mut observed = mapped_file.file.clone();
    observed.state = library::LocalFileState::Observed;
    observed.parse_version = None;
    catalog
        .write_file_observations(&[(observed, Vec::new())], Vec::new(), true)
        .await
        .unwrap();
    let reused = receiver
        .source_file_reuse_candidates("source", std::slice::from_ref(&mapped_file.file))
        .await
        .unwrap();
    assert_eq!(reused[0].parse_version, Some(7));
    assert_eq!(reused[0].state, library::LocalFileState::Accepted);
    assert!(reused[0].current_output);
    catalog
        .write_file_observations(&[(mapped_file.file, Vec::new())], Vec::new(), true)
        .await
        .unwrap();
    drop(catalog);
    receiving_profile
        .merge_database(&exported, &PROFILE_KEY.parse().unwrap())
        .await
        .unwrap();
    assert!(
        receiver
            .favorite(&library::FavoriteTarget::Track(uri.clone()))
            .await
            .unwrap()
    );
    let (path, parsed): (String, String) = sqlx::query_as("SELECT access.source_path,file.parsed_stamp FROM tracks track JOIN local_track_access access USING(track_key) JOIN local_files file ON file.local_file_key=access.local_file_key WHERE media_uri=?1")
        .bind(uri).fetch_one(&mut received).await.unwrap();
    assert_eq!(path, mapped_path.to_str().unwrap());
    assert_eq!(parsed, "[null,0,null,18]");
    assert_eq!(
        sqlx::query_as::<_, (String, i64)>(
            "SELECT source_path,local_file_key FROM tracks WHERE media_uri=?1"
        )
        .bind(uri)
        .fetch_one(&mut received)
        .await
        .unwrap(),
        (original.to_str().unwrap().to_owned(), file_key)
    );
    assert_eq!(receiver.connect_roots("source").await.unwrap().len(), 1);
    let file_identity = serde_json::json!(["source", original.to_str().unwrap()]).to_string();
    let version: Option<String> = sqlx::query_scalar(
        "SELECT version FROM connect_versions WHERE kind='catalog:file' AND object_key=?1",
    )
    .bind(&file_identity)
    .fetch_optional(&mut received)
    .await
    .unwrap();
    let current = [
        Database::connect_record_on(&mut received, "catalog:file", &file_identity)
            .await
            .unwrap(),
    ];
    assert_eq!(current[0].value.as_ref().unwrap()["parsed"][1], 18);
    let forwarded = fixture._directory.path().join("forwarded.sqlite");
    receiving_profile
        .export_database(&forwarded, &PROFILE_KEY.parse().unwrap(), &[], &[])
        .await
        .unwrap();
    let mut forwarded = support::connection(&forwarded).await;
    let file = Database::connect_record_on(&mut forwarded, "catalog:file", &file_identity)
        .await
        .unwrap();
    assert_eq!(file, current[0]);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT version FROM connect_versions WHERE kind='catalog:file' AND object_key=?1"
        )
        .bind(&file_identity)
        .fetch_optional(&mut forwarded)
        .await
        .unwrap(),
        version
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, String, String)>(
            "SELECT local_file_key,path,parsed_stamp FROM local_files WHERE local_file_key=?1"
        )
        .bind(file_key)
        .fetch_one(&mut forwarded)
        .await
        .unwrap(),
        (
            file_key,
            original.to_str().unwrap().to_owned(),
            "[null,0,null,18]".into()
        )
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM local_files WHERE path=?1")
            .bind(mapped_path.to_str().unwrap())
            .fetch_one(&mut forwarded)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        receiver
            .observed_media_file(uri)
            .await
            .unwrap()
            .unwrap()
            .path,
        mapped_path.to_str().unwrap()
    );
    for scheme in ["http", "https", "smb"] {
        let source = SourceId::new(format!("added-{scheme}"));
        let root = format!("{scheme}://music.example/Music/");
        let path = format!("{root}song.flac");
        let media = library::source_entity_uri(&source, "track", "song");
        let mut file: library::LocalFileWrite =
            serde_json::from_value(current[0].value.as_ref().unwrap()["file"].clone()).unwrap();
        file.path = path.clone();
        file.root = root.clone();
        file.relative_path = "song.flac".into();
        let track = library::TrackObservation {
            object_id: "song".into(),
            media_uri: Some(media.clone()),
            album_object_id: Some("album-a".into()),
            source_path: Some(path.clone()),
            metadata: serde_json::from_value(original_reference.clone()).unwrap(),
            ..Default::default()
        };
        fixture
            .database
            .reconcile_source(
                &source,
                if scheme == "smb" { "smb" } else { "webdav" },
                "Added source",
            )
            .await
            .unwrap();
        fixture
            .database
            .catalog(source.as_str(), None)
            .await
            .unwrap()
            .write_file_observations(&[(file, Vec::new())], vec![track], true)
            .await
            .unwrap();
    }
    let remote_snapshot = fixture._directory.path().join("remote-index.sqlite");
    profile
        .export_database(&remote_snapshot, &PROFILE_KEY.parse().unwrap(), &[], &[])
        .await
        .unwrap();
    receiving_profile.close().await.unwrap();
    library::profile::ProfileStore::prepare_database(
        &remote_snapshot,
        &PROFILE_KEY.parse().unwrap(),
        &PROFILE_KEY.parse().unwrap(),
    )
    .await
    .unwrap();
    library::profile::ProfileStore::install_prepared_database(receiver.clone(), &remote_snapshot)
        .await
        .unwrap();
    let _receiving_profile =
        library::profile::ProfileStore::open(receiver.clone(), 2, PROFILE_KEY.parse().unwrap())
            .await
            .unwrap();
    for scheme in ["http", "https", "smb"] {
        let source = SourceId::new(format!("added-{scheme}"));
        let root = format!("{scheme}://music.example/Music/");
        let path = format!("{root}song.flac");
        let media = library::source_entity_uri(&source, "track", "song");
        assert_eq!(
            receiver
                .observed_media_file(&media)
                .await
                .unwrap()
                .unwrap()
                .path,
            path
        );
        let source_key = receiver
            .source_identity_key(&source)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            receiver.local_file_roots(source_key).await.unwrap(),
            vec![root]
        );
    }

    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT indexed FROM albums WHERE source_key=(SELECT source_key FROM sources WHERE object_id='added-https') AND object_id='album-a'")
            .fetch_one(&mut received).await.unwrap(),
        0
    );
}

#[tokio::test]
async fn connect_saved_playlist_replay_keeps_duplicate_media_and_source_catalog() {
    let fixture = support::fixture().await;
    let uris = vec![
        fixture.track_uris[0].clone(),
        fixture.track_uris[1].clone(),
        fixture.track_uris[0].clone(),
    ];
    let source_profile = library::profile::ProfileStore::open(
        fixture.database.clone(),
        1,
        PROFILE_KEY.parse().unwrap(),
    )
    .await
    .unwrap();
    source_profile
        .bind_profile("shared", "source")
        .await
        .unwrap();
    fixture.database.connect_set_active(true).await.unwrap();
    let snapshot = fixture._directory.path().join("native-index.sqlite");
    source_profile
        .export_database(&snapshot, &PROFILE_KEY.parse().unwrap(), &[], &[])
        .await
        .unwrap();
    let (playlist, identity) = fixture
        .database
        .create_playlist(
            None,
            "Saved",
            &library::CatalogQuery::uris(library::CatalogKind::Track, (&uris).to_vec()).selection(),
        )
        .await
        .unwrap()
        .unwrap();
    fixture.database.connect_set_active(true).await.unwrap();
    while fixture.database.connect_seed_page().await.unwrap() {}
    let identity_key = serde_json::json!([null, identity]).to_string();
    let entry_keys: Vec<String> = sqlx::query_scalar(
        "SELECT json_array(NULL,?1,object_id) FROM playlist_entries WHERE playlist_key=?2",
    )
    .bind(&identity)
    .bind(playlist)
    .fetch_all(&mut support::connection(&fixture.path).await)
    .await
    .unwrap();
    let keys = std::iter::once(("playlist".into(), identity_key.clone()))
        .chain(entry_keys.into_iter().map(|key| ("entry".into(), key)))
        .collect::<Vec<_>>();
    let records = source_profile.current_records(&keys).await.unwrap();
    assert!(
        records
            .iter()
            .filter(|record| record.kind == "entry")
            .all(|record| {
                let value = record.value.as_ref().unwrap().as_object().unwrap();
                value.len() == 4 && value.contains_key("media_uri") && !value.contains_key("title")
            })
    );
    let references: Vec<String> = sqlx::query_scalar(
        "SELECT object_key FROM connect_versions WHERE kind='reference' AND deleted=0 ORDER BY object_key",
    )
    .fetch_all(&mut support::connection(&fixture.path).await)
    .await
    .unwrap();
    assert_eq!(references, fixture.track_uris[..2]);
    assert!(records.iter().all(|record| !matches!(
        record.kind.as_str(),
        "album" | "artist" | "genre" | "mood" | "folder" | "native_playlist" | "native_entry"
    )));
    let path = fixture._directory.path().join("received.sqlite");
    let received = Database::open(&path).await.unwrap();
    let receiver_profile =
        library::profile::ProfileStore::open(received.clone(), 2, PROFILE_KEY.parse().unwrap())
            .await
            .unwrap();
    let mut cursor = 0;
    loop {
        let page = source_profile
            .changes_in_stream(cursor, 1, library::profile::SyncStream::Authored)
            .await
            .unwrap();
        let Some(version) = page.last() else { break };
        cursor = version.revision;
        let names = vec![version.name.clone()];
        let updates = source_profile
            .updates(&receiver_profile.versions(&names).await.unwrap())
            .await
            .unwrap();
        receiver_profile.import_updates(&updates).await.unwrap();
    }
    let expected: Vec<(String, String)> = sqlx::query_as(
        "SELECT media_uri,json_extract(metadata,'$.title') FROM tracks WHERE media_uri IN (?1,?2) ORDER BY media_uri",
    )
    .bind(&uris[0])
    .bind(&uris[1])
    .fetch_all(&mut support::connection(&fixture.path).await)
    .await
    .unwrap();
    let actual: Vec<(String, String)> = sqlx::query_as(
        "SELECT media_uri,json_extract(metadata,'$.title') FROM tracks ORDER BY media_uri",
    )
    .fetch_all(&mut support::connection(&path).await)
    .await
    .unwrap();
    assert_eq!(actual, expected);
    receiver_profile.close().await.unwrap();
    library::profile::ProfileStore::prepare_database(
        &snapshot,
        &PROFILE_KEY.parse().unwrap(),
        &PROFILE_KEY.parse().unwrap(),
    )
    .await
    .unwrap();
    library::profile::ProfileStore::install_prepared_database(received.clone(), &snapshot)
        .await
        .unwrap();
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
                &library::CatalogQuery::tracks(library::TrackScope::Playlist(imported_playlist))
                    .selection(),
                None,
                &library::ReadCancellation::new()
            )
            .await
            .unwrap(),
        uris
    );
    let mut imported = support::connection(&path).await;
    let expected: Vec<(String, bool)> =
        sqlx::query_as("SELECT media_uri,indexed FROM tracks ORDER BY media_uri")
            .fetch_all(&mut support::connection(&fixture.path).await)
            .await
            .unwrap();
    let actual: Vec<(String, bool)> =
        sqlx::query_as("SELECT media_uri,indexed FROM tracks ORDER BY media_uri")
            .fetch_all(&mut imported)
            .await
            .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut imported)
            .await
            .unwrap(),
        fixture.track_uris.len() as i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks WHERE indexed")
            .fetch_one(&mut imported)
            .await
            .unwrap(),
        expected.iter().filter(|(_, indexed)| *indexed).count() as i64
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
    let tombstones = vec![
        Database::connect_record_on(
            &mut support::connection(&fixture.path).await,
            "playlist",
            &identity_key,
        )
        .await
        .unwrap(),
    ];
    assert!(tombstones[0].value.is_none());
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
        fixture.track_uris.len() as i64
    );

    // A saved file reference can later become a native track during scanning.
    let uri = url::Url::from_file_path(fixture._directory.path().join("discovered.flac"))
        .unwrap()
        .to_string();
    received
        .connect_apply(&[library::ConnectRecord {
            kind: "track".into(),
            key: uri.clone(),
            value: Some(serde_json::json!({"media_uri": uri, "title": "Saved track"})),
        }])
        .await
        .unwrap();
    let received_profile =
        library::profile::ProfileStore::open(received.clone(), 2, PROFILE_KEY.parse().unwrap())
            .await
            .unwrap();
    received_profile
        .bind_profile("shared", "receiver")
        .await
        .unwrap();
    received.connect_set_active(true).await.unwrap();
    received
        .reconcile_source(&SourceId::new("source"), "local", "Files")
        .await
        .unwrap();
    let file_path = fixture._directory.path().join("discovered.flac");
    let path = file_path.to_str().unwrap();
    for object_id in ["discovered", "renamed"] {
        received
            .catalog("source", None)
            .await
            .unwrap()
            .write_file_observations(
                &[(
                    library::LocalFileWrite {
                        path: path.into(),
                        root: fixture._directory.path().to_string_lossy().into_owned(),
                        relative_path: "discovered.flac".into(),
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
                    object_id: object_id.into(),
                    media_uri: Some(uri.clone()),
                    source_path: Some(path.into()),
                    metadata: library::TrackMetadata {
                        title: "Saved track".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                true,
            )
            .await
            .unwrap();
    }
    let identity = serde_json::json!(["source", path]).to_string();
    let changes: Vec<(String, String, bool)> = sqlx::query_as(
        "SELECT kind,object_key,deleted FROM connect_versions WHERE kind IN('catalog:file','catalog:track')",
    )
    .fetch_all(&mut imported)
    .await
    .unwrap();
    assert!(
        changes
            .iter()
            .any(|(_, key, deleted)| key == &identity && !*deleted)
    );
    assert!(!changes.iter().any(|(kind, _, _)| kind == "catalog:track"));
    let result = Database::connect_record_on(&mut imported, "catalog:file", &identity)
        .await
        .unwrap();
    let accepted = result.value.unwrap()["tracks"].as_array().unwrap().clone();
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0]["object_id"], "renamed");
    assert_eq!(accepted[0]["media_uri"], uri);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM tracks WHERE object_id='discovered' AND indexed=1"
        )
        .fetch_one(&mut imported)
        .await
        .unwrap(),
        0
    );
}
