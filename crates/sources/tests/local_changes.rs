use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use library::{Collection, Catalog, MemberRead, ReadCancellation};
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;
use lofty::tag::{Tag, TagType};
use sources::{LocalLiveChange, Source, SourceId, SourceSetup};
use sqlx::Connection as _;

#[tokio::test]
async fn unavailable_local_root_preserves_catalog_and_can_retry() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("music");
    let offline = directory.path().join("offline");
    let removed = directory.path().join("removed");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&removed).unwrap();
    write_silent_wav(&removed.join("removed.wav"), 1).unwrap();
    let track = root.join("track.wav");
    write_silent_wav(&track, 1).unwrap();
    let catalog = Catalog::open(directory.path().join("library.sqlite"))
        .await
        .unwrap();
    let mut updates = catalog.changes();
    let (configuration, source, _) = Source::connect(
        SourceId::new("test-source"),
        SourceSetup::Local {
            roots: vec![root.clone(), removed.clone()],
        },
        "",
    )
    .await
    .unwrap()
    .into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    fs::rename(&root, &offline).unwrap();
    assert!(matches!(
        source
            .acquire_catalog(&catalog, &|_| {}, cancelled.clone(), sources::Refresh::Changed)
            .await,
        Err(sources::SourceError::Other(_))
    ));
    fs::rename(&offline, &root).unwrap();
    source
        .acquire_catalog(&catalog, &|_| {}, cancelled.clone(), sources::Refresh::Changed)
        .await
        .unwrap();
    let initial = sources::published(&mut updates).expect("catalog update");
    assert!(initial.1.intersects(sources::Shown::CATALOG));
    assert_eq!(track_count(&catalog, &initial).await, 2);
    let (_, source, _) = Source::connect(
        configuration.source_id.clone(),
        SourceSetup::Local {
            roots: vec![root.clone()],
        },
        "",
    )
    .await
    .unwrap()
    .into_parts();
    fs::rename(&root, &offline).unwrap();
    assert!(
        source
            .acquire_catalog(&catalog, &|_| {}, cancelled.clone(), sources::Refresh::Changed)
            .await
            .is_err()
    );
    assert_eq!(track_count(&catalog, &initial).await, 1);
    assert!(
        source
            .acquire_catalog(&catalog, &|_| {}, cancelled.clone(), sources::Refresh::Changed)
            .await
            .is_err()
    );
    assert!(
        apply_paths(&source, &catalog, vec![track], None)
            .await
            .is_err()
    );
    assert_eq!(track_count(&catalog, &initial).await, 1);
    fs::rename(&offline, &root).unwrap();
    source
        .acquire_catalog(&catalog, &|_| {}, cancelled, sources::Refresh::Changed)
        .await
        .unwrap();
    assert_eq!(track_count(&catalog, &initial).await, 1);
}

#[tokio::test]
async fn one_local_path_republishes_only_its_component() {
    let root = tempfile::tempdir().expect("music root");
    let first = root.path().join("first.wav");
    let second = root.path().join("second.wav");
    write_silent_wav(&first, 1).expect("first WAV");
    write_silent_wav(&second, 1).expect("second WAV");
    let store = tempfile::tempdir().expect("Store root");
    let database_path = store.path().join("library.sqlite");
    let catalog = Catalog::open(&database_path).await.expect("Catalog");
    let mut updates = catalog.changes();
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetup::Local {
            roots: vec![root.path().to_path_buf()],
        },
        "",
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .expect("initial scan");
    let first_publication = sources::published(&mut updates).expect("catalog update");
    let before = catalog
        .selected_track_uris(
            &MemberRead::new(Collection::Source(first_publication.0)).selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .expect("before order");
    assert_eq!(before.len(), 2);

    write_silent_wav(&second, 2).expect("edit second WAV");
    apply_paths(&source, &catalog, vec![second.clone()], None)
        .await
        .expect("exact change");
    let changed = sources::published(&mut updates).expect("catalog update");
    assert!(changed.1.intersects(sources::Shown::CATALOG));
    let after = catalog
        .selected_track_uris(
            &MemberRead::new(Collection::Source(changed.0)).selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .expect("after order");
    assert_eq!(after, before);
}

#[tokio::test]
async fn local_image_change_updates_artwork_without_changing_metadata() {
    let root = tempfile::tempdir().expect("music root");
    let track = root.path().join("track.wav");
    let image = root.path().join("cover.jpg");
    write_silent_wav(&track, 1).expect("WAV");
    fs::write(&image, b"image-change").expect("image observation");
    let store = tempfile::tempdir().expect("Store");
    let catalog = Catalog::open(store.path().join("library.sqlite"))
        .await
        .expect("Catalog");
    let mut updates = catalog.changes();
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetup::Local {
            roots: vec![root.path().to_path_buf()],
        },
        "",
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .expect("scan");
    let initial = sources::published(&mut updates).expect("catalog update");
    let album = sources::listed_albums(
        &catalog,
        library::ListRead {
            playlist_scope: Default::default(),
            kind: library::ListKind::Albums,
            source: Some(initial.0),
            sort: library::LibraryField::Title,
            folder: None,
            filter: ("").to_string(),
            descending: false,
            favorites_only: false,
            activity: false,
        },
    )
    .await[0]
        .album_key;
    let before = album_view(&catalog, album, "initial Album artwork").await
        .row
        .artwork_binding
        .clone();
    assert!(before.is_some());
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = Arc::clone(&cancelled);
    let interrupted = source
        .acquire_catalog(
            &catalog,
            &move |progress| {
                if progress.stage == sources::SourceProgressStage::Tracks {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            },
            cancelled,
            sources::Refresh::Changed,
        )
        .await;
    // An unchanged walk reports no progress, so it finishes before the cancellation.
    assert!(matches!(
        interrupted,
        Ok(_) | Err(sources::SourceError::Cancelled)
    ));
    assert_eq!(
        album_view(&catalog, album, "Album cover after cancelled discovery").await
            .row
            .artwork_binding,
        before
    );
    assert!(
        sources::published(&mut updates).is_none(),
        "cancelled no-op does not publish"
    );
    fs::write(&image, b"changed-image").expect("change image");
    apply_paths(&source, &catalog, vec![image], None)
        .await
        .expect("image change");
    let publication = sources::published(&mut updates).expect("catalog update");
    // A picture is part of the catalog rows that show it.
    assert!(publication.1.contains(sources::Shown::CATALOG));
    assert!(
        !publication
            .1
            .intersects(sources::Shown::PLAYLIST | sources::Shown::HOME)
    );
    assert_ne!(
        album_view(&catalog, album, "Album row").await
            .row
            .artwork_binding,
        before
    );
}

#[tokio::test]
async fn restart_catch_up_detects_an_in_place_file_edit() {
    let root = tempfile::tempdir().expect("music root");
    let track = root.path().join("track.wav");
    write_silent_wav(&track, 1).expect("WAV");
    let store = tempfile::tempdir().expect("Store");
    let database_path = store.path().join("library.sqlite");
    let catalog = Catalog::open(&database_path).await.expect("Catalog");
    let mut updates = catalog.changes();
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetup::Local {
            roots: vec![root.path().to_path_buf()],
        },
        "",
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .expect("scan");
    let initial = sources::published(&mut updates).expect("catalog update");
    let order = catalog
        .selected_track_uris(
            &MemberRead::new(Collection::Source(initial.0)).selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .expect("Track order");
    assert_eq!(
        catalog
            .tracks_by_uris(&((&order).to_vec()), &ReadCancellation::new())
            .await
            .expect("initial Track row")[0]
            .metadata
            .duration_millis,
        1_000
    );

    let mut stored = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&database_path),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE track_details SET metadata=jsonb_remove(metadata,'$.album_tags')")
        .execute(&mut stored)
        .await
        .unwrap();
    stored.close().await.unwrap();
    catalog
        .close()
        .await
        .expect("close before unchanged restarts");
    for _ in 0..2 {
        let catalog = Catalog::open(&database_path).await.unwrap();
        source
            .acquire_catalog(
                &catalog,
                &|progress| panic!("unchanged catch-up started parsing: {progress:?}"),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                sources::Refresh::Changed,
            )
            .await
            .unwrap();
        let observation = stored_files(&catalog, configuration.source_id.as_str()).await
            .into_iter()
            .find(|file| file.kind == library::LocalFileKind::Media)
            .unwrap();
        assert_eq!(observation.parse_version, Some(20));
        assert_eq!(observation.state, library::LocalFileState::Accepted);
        catalog.close().await.unwrap();
    }
    write_silent_wav(&track, 2).expect("edit WAV in place");
    let catalog = Catalog::open(&database_path)
        .await
        .expect("reopen after edit");
    let mut updates = catalog.changes();
    let progress = Mutex::new(Vec::new());
    source
        .acquire_catalog(
            &catalog,
            &|update| progress.lock().unwrap().push(update),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .expect("restart catch-up");
    let progress = progress.into_inner().unwrap();
    assert!(progress.iter().any(|update| {
        update.stage == sources::SourceProgressStage::Tracks && update.completed > 0
    }));
    assert!(progress.iter().all(|update| {
        update.stage == sources::SourceProgressStage::Tracks && update.total.is_none()
    }));
    assert_eq!(progress.last().unwrap().completed, 1);
    let changed = sources::published(&mut updates).expect("catalog update");
    assert!(changed.1.intersects(sources::Shown::CATALOG));
    assert_eq!(
        catalog
            .tracks_by_uris(&((&order).to_vec()), &ReadCancellation::new())
            .await
            .expect("changed Track row")[0]
            .metadata
            .duration_millis,
        2_000
    );
    catalog.close().await.expect("close before file removal");
    // An empty root reads as an unmounted one and keeps its rows.
    fs::create_dir(root.path().join("Other")).unwrap();
    fs::remove_file(&track).expect("remove cached file while stopped");
    let catalog = Catalog::open(&database_path)
        .await
        .expect("reopen after removal");
    let mut updates = catalog.changes();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .expect("remove missing file during catch-up");
    let removed = sources::published(&mut updates).expect("catalog update");
    assert_eq!(
        catalog
            .all_source_counts()
            .await
            .unwrap()
            .get(&catalog.source_id(removed.0).unwrap())
            .map_or(0, |counts| counts.1),
        0
    );
    let malformed = root.path().join("truncated.wma");
    let mut bytes = vec![
        0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa, 0x00, 0x62, 0xce,
        0x6c,
    ];
    bytes.extend_from_slice(&1_000_u64.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&[1, 2]);
    fs::write(&malformed, bytes).unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .unwrap();
    let paths = [malformed
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned()];
    assert_eq!(
        stored_files(&catalog, configuration.source_id.as_str())
                .await
                .into_iter()
                .find(|file| file.path == paths[0])
                .unwrap()
            .state,
        library::LocalFileState::Rejected
    );
    catalog.close().await.unwrap();
    for _ in 0..2 {
        let catalog = Catalog::open(&database_path).await.unwrap();
        source
            .acquire_catalog(
                &catalog,
                &|progress| panic!("cached rejection started parsing: {progress:?}"),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                sources::Refresh::Changed,
            )
            .await
            .unwrap();
        assert_eq!(
            stored_files(&catalog, configuration.source_id.as_str())
                .await
                .into_iter()
                .find(|file| file.path == paths[0])
                .unwrap()
                .state,
            library::LocalFileState::Rejected
        );
        catalog.close().await.unwrap();
    }
    write_silent_wav(&malformed, 1).expect("replace malformed input with audio");
    let catalog = Catalog::open(&database_path).await.unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .unwrap();
    assert_eq!(
        stored_files(&catalog, configuration.source_id.as_str())
                .await
                .into_iter()
                .find(|file| file.path == paths[0])
                .unwrap()
            .state,
        library::LocalFileState::Accepted
    );
}

#[tokio::test]
async fn restart_catch_up_publishes_folder_changes_without_reparsing_tracks() {
    let root = tempfile::tempdir().expect("music root");
    let track = root.path().join("track.wav");
    write_silent_wav(&track, 1).expect("WAV");
    let store = tempfile::tempdir().expect("Store");
    let database_path = store.path().join("library.sqlite");
    let catalog = Catalog::open(&database_path).await.expect("Catalog");
    let mut updates = catalog.changes();
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetup::Local {
            roots: vec![root.path().to_path_buf()],
        },
        "",
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .expect("scan");
    sources::published(&mut updates).expect("catalog update");

    fs::create_dir(root.path().join("empty")).expect("change only Local inventory bookkeeping");
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .expect("restart catch-up");

    assert!(
        sources::published(&mut updates)
            .expect("new folder publication")
            .1
            .intersects(sources::Shown::CATALOG)
    );
    assert!(
        source
            .browse_folder(&catalog, None, None)
            .await
            .unwrap()
            .folders
            .iter()
            .any(|folder| folder.name == "empty")
    );

    for index in 0..130 {
        fs::create_dir_all(root.path().join(format!("pending-{index:03}/nested"))).unwrap();
    }
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = Arc::clone(&cancelled);
    assert!(matches!(
        source
            .acquire_catalog(
                &catalog,
                &move |progress| {
                    if progress.stage == sources::SourceProgressStage::Tracks {
                        stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                },
                cancelled,
                sources::Refresh::Changed
            )
            .await,
        Err(sources::SourceError::Cancelled)
    ));
    assert!(
        { catalog.loaded().await; catalog.cached_source(configuration.source_id.as_str()) }
            .unwrap()
            .catalog_complete
    );
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&database_path).await.unwrap();
    let mut updates = catalog.changes();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .unwrap();
    if let Some(update) = sources::published(&mut updates) {
        assert!(update.1.intersects(sources::Shown::CATALOG));
    }
    let files = stored_files(&catalog, configuration.source_id.as_str()).await;
    assert_eq!(files.len(), 263);
    assert!(
        files
            .iter()
            .filter(|file| file.kind == library::LocalFileKind::Directory)
            .all(|file| file.state == library::LocalFileState::Accepted)
    );
    let progress = Mutex::new(Vec::new());
    source
        .acquire_catalog(
            &catalog,
            &|update| progress.lock().unwrap().push(update),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .unwrap();
    assert!(
        sources::published(&mut updates).is_none(),
        "unchanged directories stay quiet"
    );
    assert!(progress.into_inner().unwrap().is_empty());
}

#[tokio::test]
async fn cue_catch_up_advances_a_shared_backing_media_observation() {
    let root = tempfile::tempdir().expect("music root");
    let media = root.path().join("album.wav");
    let first_cue = root.path().join("first.cue");
    let second_cue = root.path().join("second.cue");
    write_silent_wav(&media, 2).expect("WAV");
    fs::write(
        &first_cue,
        "PERFORMER \"Artist\"\nTITLE \"First Album\"\nFILE \"album.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"First Track\"\n    INDEX 01 00:00:00\n",
    )
    .expect("first CUE");
    fs::write(
        &second_cue,
        "PERFORMER \"Artist\"\nTITLE \"Second Album\"\nFILE \"album.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Second Track\"\n    INDEX 01 00:00:00\n",
    )
    .expect("second CUE");
    let store = tempfile::tempdir().expect("Store");
    let database_path = store.path().join("library.sqlite");
    let catalog = Catalog::open(&database_path).await.expect("Catalog");
    let mut updates = catalog.changes();
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetup::Local {
            roots: vec![root.path().to_path_buf()],
        },
        "",
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .expect("scan");
    let initial = sources::published(&mut updates).expect("catalog update");
    let albums = sources::listed_albums(
        &catalog,
        library::ListRead {
            playlist_scope: Default::default(),
            kind: library::ListKind::Albums,
            source: Some(initial.0),
            sort: library::LibraryField::Title,
            folder: None,
            filter: ("").to_string(),
            descending: false,
            favorites_only: false,
            activity: false,
        },
    )
    .await;
    assert_eq!(albums.len(), 2);
    let mut stored = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&database_path),
    )
    .await
    .unwrap();
    let retired_uri = library::cue_media_uri(
        "retired-cue",
        url::Url::from_file_path(&media).unwrap().as_str(),
        0,
        2_000,
    );
    let retired: i64 = sqlx::query_scalar("INSERT INTO tracks(source_key,media_uri) SELECT source_key,?1 FROM tracks JOIN track_details USING(track_key) WHERE cue_path=?2 LIMIT 1 RETURNING track_key")
        .bind(&retired_uri)
        .bind(first_cue.canonicalize().unwrap().to_string_lossy().as_ref())
        .fetch_one(&mut stored)
        .await
        .unwrap();
    sqlx::query("INSERT INTO track_details(track_key,source_path,cue_path,cue_start_millis,cue_end_millis) SELECT ?1,source_path,cue_path,cue_start_millis,cue_end_millis FROM track_details WHERE cue_path=?2 LIMIT 1")
        .bind(retired)
        .bind(first_cue.canonicalize().unwrap().to_string_lossy().as_ref())
        .execute(&mut stored)
        .await
        .unwrap();
    // Rows written straight to the Store reach the Catalog when it opens again.
    sqlx::query("UPDATE store_commits SET commits=commits+1")
        .execute(&mut stored)
        .await
        .unwrap();
    catalog.close().await.expect("close Catalog");
    let catalog = Catalog::open(&database_path).await.expect("reopen Catalog");
    let mut updates = catalog.changes();
    write_silent_wav(&media, 3).expect("edit CUE backing media");
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .expect("first catch-up");
    sources::published(&mut updates).expect("catalog update");
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .expect("second catch-up");

    assert!(
        sources::published(&mut updates).is_none(),
        "unchanged observations stay quiet"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT indexed FROM tracks WHERE track_key=?1")
            .bind(retired)
            .fetch_one(&mut stored)
            .await
            .unwrap(),
        0,
        "retired CUE aliases stay outside the source catalog"
    );
}

#[tokio::test]
async fn encoded_local_paths_publish_folder_art_and_watcher_replacements() {
    let music = tempfile::tempdir().unwrap();
    let root = music.path().join("Müzik 100% saved");
    let disc = root.join("Disc 1");
    fs::create_dir_all(&disc).unwrap();
    let path = disc.join("01 café %.wav");
    write_tagged_wav(&path, "Track", "Artist", "Album", "Rock").unwrap();
    let cover = root.join("cover.jpg");
    fs::write(&cover, b"first image").unwrap();
    let store = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(store.path().join("library.sqlite"))
        .await
        .unwrap();
    let mut updates = catalog.changes();
    let connected = Source::connect(
        SourceId::new("local-art"),
        SourceSetup::Local { roots: vec![root] },
        "",
    )
    .await
    .unwrap();
    let (configuration, source, _) = connected.into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .unwrap();
    sources::published(&mut updates).expect("catalog update");
    let uri = library::normalize_direct_media_uri(
        url::Url::from_file_path(path.canonicalize().unwrap())
            .unwrap()
            .as_str(),
    )
    .unwrap();
    assert!(uri.contains("%25"));
    let row = catalog
        .tracks_by_uris(&([(&uri).to_string()]), &ReadCancellation::new())
        .await
        .map(|rows| {
            let mut rows = rows;
            rows.pop()
        })
        .unwrap()
        .unwrap();
    let binding: Vec<library::ArtworkCandidate> =
        serde_json::from_slice(row.artwork_binding.as_ref().unwrap()).unwrap();
    assert_eq!(binding[0].source_id().unwrap(), source.source_id());
    assert!(
        matches!(&binding[0].image, library::ArtworkImage::Local { path, picture_index: None, .. } if path == & cover.canonicalize().unwrap().to_string_lossy())
    );
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .unwrap();
    assert!(
        sources::published(&mut updates).is_none(),
        "unchanged observations stay quiet"
    );
    fs::write(&cover, b"changed image bytes").unwrap();
    apply_paths(&source, &catalog, vec![cover], None)
        .await
        .unwrap();
    let update = sources::published(&mut updates).expect("catalog update");
    assert!(update.1.contains(sources::Shown::CATALOG)
            && !update.1.intersects(sources::Shown::PLAYLIST | sources::Shown::HOME));
    let changed = catalog
        .tracks_by_uris(&([(&uri).to_string()]), &ReadCancellation::new())
        .await
        .map(|rows| {
            let mut rows = rows;
            rows.pop()
        })
        .unwrap()
        .unwrap();
    assert_ne!(row.artwork_binding, changed.artwork_binding);
    assert_eq!(row.track_key, changed.track_key);
}

#[tokio::test]
async fn local_default_cover_reaches_later_track_and_distinct_rescan_preserves_group_fallback() {
    let parent = tempfile::tempdir().unwrap();
    fs::write(parent.path().join("cover.png"), b"outside the music root").unwrap();
    let root = tempfile::tempdir_in(parent.path()).unwrap();
    fs::write(root.path().join("artist.jpg"), b"portrait").unwrap();
    let mut paths = Vec::new();
    for number in 1..=3 {
        let path = root.path().join(format!("{number} café %.wav"));
        write_tagged_wav(&path, &format!("Track {number}"), "Artist", "Album", "Rock").unwrap();
        let mut tagged = Probe::open(&path).unwrap().read().unwrap();
        let tag = tagged.primary_tag_mut().unwrap();
        tag.set_track(number);
        if number > 1 {
            tag.push_picture(
                lofty::picture::Picture::unchecked(vec![number as u8; 32])
                    .pic_type(lofty::picture::PictureType::CoverFront)
                    .mime_type(lofty::picture::MimeType::Png)
                    .build(),
            );
        }
        tagged.save_to_path(&path, WriteOptions::default()).unwrap();
        paths.push(path.canonicalize().unwrap());
    }
    let store = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(store.path().join("library.sqlite"))
        .await
        .unwrap();
    let connected = Source::connect(
        SourceId::new("local-art"),
        SourceSetup::Local {
            roots: vec![root.path().to_path_buf()],
        },
        "",
    )
    .await
    .unwrap();
    let (configuration, source, _) = connected.into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .unwrap();
    let mut stored = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(store.path().join("library.sqlite")),
    )
    .await
    .unwrap();
    let mut updates = catalog.changes();
    let mut expected_group = None;
    for distinct in [false, true, false] {
        catalog.set_distinct_track_covers(distinct);
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        source
            .acquire_catalog(&catalog, &|_| {}, cancelled, sources::Refresh::Resync)
            .await
            .unwrap();
        for (index, path) in paths.iter().enumerate() {
            let uri = url::Url::from_file_path(path).unwrap().to_string();
            let row = catalog
                .tracks_by_uris(&([(&uri).to_string()]), &ReadCancellation::new())
                .await
                .map(|rows| {
                    let mut rows = rows;
                    rows.pop()
                })
                .unwrap()
                .unwrap();
            let bytes = row
                .artwork_binding
                .unwrap_or_else(|| panic!("missing cover distinct={distinct} track={index}"));
            let binding: Vec<library::ArtworkCandidate> = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(binding[0].source_id().unwrap(), source.source_id());
            let expected = if distinct && index == 2 {
                &paths[2]
            } else {
                &paths[1]
            };
            assert!(
                matches!(&binding[0].image, library::ArtworkImage::Local { path, picture_index: Some(_), .. } if path == &expected.to_string_lossy()),
                "distinct={distinct}, track={index}, binding={binding:?}, expected={expected:?}"
            );
            if index == 0 {
                if let Some(group) = &expected_group {
                    assert_eq!(&bytes, group);
                }
                expected_group = Some(bytes);
            }
        }
    }
    assert_eq!(
        sqlx::query_scalar::<_, Option<i64>>("SELECT picture_index FROM files WHERE path=?1")
            .bind(paths[0].to_string_lossy().as_ref())
            .fetch_one(&mut stored)
            .await
            .unwrap(),
        Some(-1),
        "an inspected file without a cover stays negatively cached"
    );
    sources::published(&mut updates);
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .unwrap();
    assert!(
        sources::published(&mut updates).is_none(),
        "unchanged image facts stay quiet"
    );
}

#[tokio::test]
async fn local_artist_cover_keeps_album_order_across_point_updates() {
    let root = tempfile::tempdir().unwrap();
    let mut covers = Vec::new();
    for album in ["First", "Second"] {
        let directory = root.path().join(album);
        fs::create_dir(&directory).unwrap();
        write_tagged_wav(
            &directory.join("track.wav"),
            "Track",
            "Artist",
            album,
            "Rock",
        )
        .unwrap();
        let cover = directory.join("cover.jpg");
        fs::write(&cover, album.as_bytes()).unwrap();
        covers.push(cover.canonicalize().unwrap());
    }
    let store = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(store.path().join("library.sqlite"))
        .await
        .unwrap();
    let mut updates = catalog.changes();
    let connected = Source::connect(
        SourceId::new("local-art"),
        SourceSetup::Local {
            roots: vec![root.path().to_path_buf()],
        },
        "",
    )
    .await
    .unwrap();
    let (configuration, source, _) = connected.into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .unwrap();
    let initial = sources::published(&mut updates).expect("catalog update");
    let artist = sources::listed_artists(
        &catalog,
        library::ListRead {
            playlist_scope: Default::default(),
            kind: library::ListKind::Artists {
                album_artist: false,
            },
            source: Some(initial.0),
            sort: library::LibraryField::Title,
            folder: None,
            filter: ("").to_string(),
            descending: false,
            favorites_only: false,
            activity: false,
        },
    )
    .await
    .remove(0);
    for changed in [&covers[1], &covers[0]] {
        fs::write(changed, b"replacement artwork").unwrap();
        apply_paths(&source, &catalog, vec![changed.clone()], None)
            .await
            .unwrap();
        let update = sources::published(&mut updates).expect("catalog update");
        assert!(update.1.contains(sources::Shown::CATALOG)
            && !update.1.intersects(sources::Shown::PLAYLIST | sources::Shown::HOME));
        let current = catalog
            .artist(&artist.media_uri, false, false, &ReadCancellation::new())
            .await
            .unwrap()
            .unwrap();
        let artwork: Vec<library::ArtworkCandidate> =
            serde_json::from_slice(current.row.artwork_binding.as_ref().unwrap()).unwrap();
        let binding = &artwork;
        assert_eq!(binding[0].source_id().unwrap(), source.source_id());
        assert!(
            matches!(&binding[0].image, library::ArtworkImage::Local { path, picture_index: None, .. } if path == & covers[0].to_string_lossy())
        );
        if changed == &covers[1] {
            assert_eq!(current.row.artwork_binding, artist.artwork_binding);
        } else {
            assert_ne!(current.row.artwork_binding, artist.artwork_binding);
        }
    }
}

#[tokio::test]
async fn local_refresh_after_a_deleted_album_track_removes_it() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("Album");
    fs::create_dir(&directory).unwrap();
    let deleted = directory.join("1 first.wav");
    write_tagged_wav(&deleted, "First", "Artist", "Album", "Rock").unwrap();
    write_tagged_wav(&directory.join("2 second.wav"), "Second", "Artist", "Album", "Rock").unwrap();
    let store = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(store.path().join("library.sqlite"))
        .await
        .unwrap();
    let mut updates = catalog.changes();
    let connected = Source::connect(
        SourceId::new("local-deleted"),
        SourceSetup::Local {
            roots: vec![root.path().to_path_buf()],
        },
        "",
    )
    .await
    .unwrap();
    let (configuration, source, _) = connected.into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    let refresh = async || {
        source
            .acquire_catalog(
                &catalog,
                &|_| {},
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                sources::Refresh::Changed,
            )
            .await
    };
    refresh().await.unwrap();
    let initial = sources::published(&mut updates).expect("catalog update");
    assert_eq!(track_count(&catalog, &initial).await, 2);
    fs::remove_file(&deleted).unwrap();
    // The artwork pass must not open the deleted file before its absence is removed.
    refresh().await.expect("refresh after a deletion");
    let removed = sources::published(&mut updates).expect("catalog update");
    assert_eq!(track_count(&catalog, &removed).await, 1);
    refresh().await.expect("later refresh");
}

#[tokio::test]
async fn a_moved_folder_larger_than_a_write_keeps_every_track_identity() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("a")).unwrap();
    for index in 0..160 {
        write_silent_wav(&root.path().join(format!("a/{index:03}.wav")), 1).unwrap();
    }
    let store = tempfile::tempdir().unwrap();
    let database_path = store.path().join("library.sqlite");
    let catalog = Catalog::open(&database_path).await.unwrap();
    let (configuration, source, _) = Source::connect(
        SourceId::new("local-moved"),
        SourceSetup::Local {
            roots: vec![root.path().to_path_buf()],
        },
        "",
    )
    .await
    .unwrap()
    .into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    let refresh = async || {
        source
            .acquire_catalog(
                &catalog,
                &|_| {},
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                sources::Refresh::Changed,
            )
            .await
            .unwrap()
    };
    refresh().await;
    let mut stored = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&database_path),
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO user_state SELECT media_uri,1,80,0 FROM tracks WHERE indexed=1")
        .execute(&mut stored)
        .await
        .unwrap();
    // All 160 renames are staged at once and written over two batches.
    fs::create_dir(root.path().join("b")).unwrap();
    fs::rename(root.path().join("a"), root.path().join("b/a")).unwrap();
    refresh().await;
    let favorites: (i64, i64) = sqlx::query_as(
        "SELECT count(*),count(user_state.media_uri) FROM tracks LEFT JOIN user_state USING(media_uri) WHERE indexed=1",
    )
    .fetch_one(&mut stored)
    .await
    .unwrap();
    assert_eq!(favorites, (160, 160));
}

#[tokio::test]
async fn local_borrowed_album_cover_follows_replacement_and_removal() {
    let root = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for album in ["First", "Borrower"] {
        let directory = root.path().join(album);
        fs::create_dir(&directory).unwrap();
        let path = directory.join("track.wav");
        write_tagged_wav(&path, "Track", "Artist", album, "Rock").unwrap();
        paths.push(path.canonicalize().unwrap());
    }
    let cover = root.path().join("First/cover.jpg");
    fs::write(&cover, b"first image").unwrap();
    let store = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(store.path().join("library.sqlite"))
        .await
        .unwrap();
    let mut updates = catalog.changes();
    let connected = Source::connect(
        SourceId::new("local-art"),
        SourceSetup::Local {
            roots: vec![root.path().to_path_buf()],
        },
        "",
    )
    .await
    .unwrap();
    let (configuration, source, _) = connected.into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .unwrap();
    sources::published(&mut updates).expect("catalog update");
    let borrower_uri = url::Url::from_file_path(&paths[1]).unwrap().to_string();
    let before = catalog
        .tracks_by_uris(&([(&borrower_uri).to_string()]), &ReadCancellation::new())
        .await
        .map(|rows| {
            let mut rows = rows;
            rows.pop()
        })
        .unwrap()
        .unwrap();
    assert!(before.artwork_binding.is_some());
    for remove in [false, true] {
        if remove {
            fs::remove_file(&cover).unwrap();
        } else {
            fs::write(&cover, b"replacement image").unwrap();
        }
        apply_paths(&source, &catalog, vec![cover.clone()], None)
            .await
            .unwrap();
        let update = sources::published(&mut updates).expect("catalog update");
        assert!(update.1.contains(sources::Shown::CATALOG)
            && !update.1.intersects(sources::Shown::PLAYLIST | sources::Shown::HOME));
        let borrower = catalog
            .tracks_by_uris(&([(&borrower_uri).to_string()]), &ReadCancellation::new())
            .await
            .map(|rows| {
                let mut rows = rows;
                rows.pop()
            })
            .unwrap()
            .unwrap();
        assert_eq!(borrower.track_key, before.track_key);
        assert_ne!(borrower.artwork_binding, before.artwork_binding);
        let borrowed: Vec<library::ArtworkCandidate> = borrower
            .artwork_binding
            .as_deref()
            .map(|bytes| serde_json::from_slice(bytes).unwrap())
            .unwrap_or_default();
        assert_eq!(
            borrowed
                .iter()
                .any(|candidate| matches!(candidate.image, library::ArtworkImage::Local { .. })),
            !remove
        );
        for path in &paths {
            let uri = url::Url::from_file_path(path).unwrap().to_string();
            let binding = catalog
                .track(&(&uri), &ReadCancellation::new())
                .await
                .unwrap()
                .unwrap()
                .artwork_binding;
            let candidates = binding.as_deref().map(|bytes| {
                serde_json::from_slice::<Vec<library::ArtworkCandidate>>(bytes).unwrap()
            });
            let expected = borrower.artwork_binding.as_deref().map(|bytes| {
                serde_json::from_slice::<Vec<library::ArtworkCandidate>>(bytes).unwrap()
            });
            assert_eq!(
                candidates
                    .as_ref()
                    .and_then(|rows| rows.iter().find(|candidate| matches!(
                        candidate.image,
                        library::ArtworkImage::Local { .. }
                    ))),
                expected
                    .as_ref()
                    .and_then(|rows| rows.iter().find(|candidate| matches!(
                        candidate.image,
                        library::ArtworkImage::Local { .. }
                    )))
            );
        }
    }
}

#[tokio::test]
async fn local_metadata_save_publishes_album_identity_changes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap().join("café %.wav");
    write_tagged_wav(&path, "Track", "Artist", "Before", "Rock").unwrap();
    let second_path = root.path().canonicalize().unwrap().join("second.wav");
    write_tagged_wav(&second_path, "Second", "Artist", "Before", "Rock").unwrap();
    let other_path = root.path().canonicalize().unwrap().join("other.wav");
    write_tagged_wav(&other_path, "Other", "Artist", "Aardvark", "Rock").unwrap();
    let mut other = Probe::open(&other_path).unwrap().read().unwrap();
    other
        .primary_tag_mut()
        .unwrap()
        .insert_text(ItemKey::ReleaseDate, "2030".into());
    other
        .save_to_path(&other_path, WriteOptions::default())
        .unwrap();
    let store = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(store.path().join("library.sqlite"))
        .await
        .unwrap();
    let mut updates = catalog.changes();
    let connected = Source::connect(
        SourceId::new("local-metadata"),
        SourceSetup::Local {
            roots: vec![root.path().to_path_buf()],
        },
        "",
    )
    .await
    .unwrap();
    let (configuration, source, _) = connected.into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .unwrap();
    let initial = sources::published(&mut updates).expect("catalog update");
    let uri = url::Url::from_file_path(&path).unwrap().to_string();
    let track = catalog
        .tracks_by_uris(&([(&uri).to_string()]), &ReadCancellation::new())
        .await
        .map(|rows| {
            let mut rows = rows;
            rows.pop()
        })
        .unwrap()
        .unwrap();
    let album = album_view(&catalog, track.links.album_key.unwrap(), "Album").await;
    let source = Arc::new(source);
    let (editor, _metadata) = sources::FileEditSession::open(
        Some(Arc::clone(&source)),
        &catalog,
        album.row.media_uri.clone(),
        sources::SourceEntityKind::Album,
    )
    .await
    .unwrap();
    {
        editor
            .save_tracks(
                &catalog,
                Some(sources::MetadataEdit {
                    kind: sources::SourceEntityKind::Album,
                    changes: [("title".into(), "After".into())].into(),
                    artwork: None,
                }),
                [&path, &second_path]
                    .into_iter()
                    .map(|path| url::Url::from_file_path(path).unwrap().to_string())
                    .collect(),
                |_| async {
                    Ok((
                        sources::MetadataEdit {
                            kind: sources::SourceEntityKind::Track,
                            changes: Default::default(),
                            artwork: None,
                        },
                        None,
                        None,
                    ))
                },
                |_, _, _| async { Ok(()) },
            )
            .await
            .unwrap();
    }
    assert!(
        sources::published(&mut updates)
            .expect("catalog update")
            .1
            .intersects(sources::Shown::CATALOG)
    );
    let current = catalog
        .tracks_by_uris(&([(&uri).to_string()]), &ReadCancellation::new())
        .await
        .map(|rows| {
            let mut rows = rows;
            rows.pop()
        })
        .unwrap()
        .unwrap();
    assert_eq!(current.track_key, track.track_key);
    assert_eq!(current.metadata.album, "After");
    let (editor, _metadata) = sources::FileEditSession::open(
        Some(Arc::clone(&source)),
        &catalog,
        uri.to_string(),
        sources::SourceEntityKind::Track,
    )
    .await
    .unwrap();
    {
        editor
            .save(
                &catalog,
                (sources::MetadataEdit {
                    kind: sources::SourceEntityKind::Track,
                    changes: Default::default(),
                    artwork: None,
                })
                .clone(),
            )
            .await
            .unwrap();
    }
    assert!(
        sources::published(&mut updates).is_none(),
        "no-op save has no catalog change"
    );
    for (date, release, original, expected, year) in [
        ("2001", "", "", Some("2001"), Some(2001)),
        ("2001-02", "", "", Some("2001-02"), Some(2001)),
        (
            "2001-02-03",
            "",
            "1995-07-08",
            Some("2001-02-03"),
            Some(2001),
        ),
        (
            "2001-02-03",
            "2004-05-06",
            "1995-07-08",
            Some("2004-05-06"),
            Some(2004),
        ),
        ("", "2004-05-06", "", Some("2004-05-06"), Some(2004)),
        ("", "", "1995-07-08", Some("1995-07-08"), Some(1995)),
        ("", "", "", None, None),
    ] {
        let album = album_view(&catalog, current.links.album_key.unwrap(), "Album").await;
        let (editor, _metadata) = sources::FileEditSession::open(
            Some(Arc::clone(&source)),
            &catalog,
            album.row.media_uri.clone(),
            sources::SourceEntityKind::Album,
        )
        .await
        .unwrap();
        {
            editor
                .save_tracks(
                    &catalog,
                    Some(sources::MetadataEdit {
                        kind: sources::SourceEntityKind::Album,
                        changes: [
                            ("recording_date".to_string(), date.to_string()),
                            ("release_date".to_string(), release.to_string()),
                            ("original_release_date".to_string(), original.to_string()),
                        ]
                        .into(),
                        artwork: None,
                    }),
                    [&path, &second_path]
                        .into_iter()
                        .map(|path| url::Url::from_file_path(path).unwrap().to_string())
                        .collect(),
                    |_| async {
                        Ok((
                            sources::MetadataEdit {
                                kind: sources::SourceEntityKind::Track,
                                changes: Default::default(),
                                artwork: None,
                            },
                            None,
                            None,
                        ))
                    },
                    |_, _, _| async { Ok(()) },
                )
                .await
                .unwrap();
        }
        let album = album_view(&catalog, current.links.album_key.unwrap(), "Album").await;
        for path in [&path, &second_path] {
            let uri = url::Url::from_file_path(path).unwrap().to_string();
            let (_editor, metadata) = sources::FileEditSession::open(
                Some(Arc::clone(&source)),
                &catalog,
                uri.to_string(),
                sources::SourceEntityKind::Track,
            )
            .await
            .unwrap();
            for (key, value) in [
                ("recording_date", date),
                ("release_date", release),
                ("original_release_date", original),
            ] {
                assert_eq!(
                    metadata
                        .fields
                        .iter()
                        .find(|field| field.key == key)
                        .unwrap()
                        .value,
                    value
                );
            }
            let track = catalog
                .tracks_by_uris(&([(&uri).to_string()]), &ReadCancellation::new())
                .await
                .map(|rows| {
                    let mut rows = rows;
                    rows.pop()
                })
                .unwrap()
                .unwrap();
            assert_eq!(track.metadata.release_date.as_deref(), expected);
            assert_eq!(track.metadata.year, year);
        }
        assert_eq!(album.metadata.release_date.as_deref(), expected);
        assert_eq!(album.metadata.year, year);
        for descending in [false, true] {
            let albums = sources::listed_albums(
                &catalog,
                library::ListRead {
                    playlist_scope: Default::default(),
                    kind: library::ListKind::Albums,
                    source: Some(initial.0),
                    sort: library::LibraryField::ReleaseDate,
                    folder: None,
                    filter: ("").to_string(),
                    descending: descending,
                    favorites_only: false,
                    activity: false,
                },
            )
            .await;
            let titles = albums
                .iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                titles,
                if expected.is_none() || descending {
                    vec!["Aardvark", "After"]
                } else {
                    vec!["After", "Aardvark"]
                }
            );
        }
    }
}

#[tokio::test]
async fn transient_cue_read_failure_retains_tracks_but_rejected_content_removes_them() {
    let root = tempfile::tempdir().expect("music root");
    let media = root.path().join("album.wav");
    let cue = root.path().join("album.cue");
    write_silent_wav(&media, 2).expect("WAV");
    fs::write(
        &cue,
        "PERFORMER \"Artist\"\nTITLE \"Album\"\nFILE \"album.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n",
    )
    .expect("CUE");
    let store = tempfile::tempdir().expect("Store");
    let catalog = Catalog::open(store.path().join("library.sqlite"))
        .await
        .expect("Catalog");
    let mut updates = catalog.changes();
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetup::Local {
            roots: vec![root.path().to_path_buf()],
        },
        "",
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .expect("scan");
    let initial = sources::published(&mut updates).expect("catalog update");
    assert_eq!(track_count(&catalog, &initial).await, 1);

    fs::write(&media, b"temporarily unreadable media content").expect("damage backing media");
    apply_paths(&source, &catalog, vec![cue.clone()], None)
        .await
        .expect("transient CUE change");
    assert!(
        sources::published(&mut updates).is_none(),
        "unreadable backing preserves accepted content without invalidation"
    );
    assert_eq!(track_count(&catalog, &initial).await, 1);

    fs::write(&cue, "this is not a cue sheet").expect("reject CUE");
    apply_paths(&source, &catalog, vec![cue], None)
        .await
        .expect("rejected CUE change");
    let rejected = sources::published(&mut updates).expect("catalog update");
    assert_eq!(track_count(&catalog, &rejected).await, 0);
}

#[tokio::test]
async fn exact_track_change_rebuilds_relations_from_the_complete_album() {
    let root = tempfile::tempdir().expect("music root");
    fs::write(root.path().join("cover.jpg"), b"album cover").unwrap();
    let first = root.path().join("first.wav");
    let second = root.path().join("second.wav");
    write_tagged_wav(&first, "First", "First Artist", "Shared Album", "Rock")
        .expect("first tagged WAV");
    write_tagged_wav(&second, "Second", "Second Artist", "Shared Album", "Jazz")
        .expect("second tagged WAV");
    let store = tempfile::tempdir().expect("Store");
    let database_path = store.path().join("library.sqlite");
    let catalog = Catalog::open(&database_path).await.expect("Catalog");
    let mut updates = catalog.changes();
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetup::Local {
            roots: vec![root.path().to_path_buf()],
        },
        "",
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .expect("scan");
    let initial = sources::published(&mut updates).expect("catalog update");
    let albums = sources::listed_albums(
        &catalog,
        library::ListRead {
            playlist_scope: Default::default(),
            kind: library::ListKind::Albums,
            source: Some(initial.0),
            sort: library::LibraryField::Title,
            folder: None,
            filter: ("").to_string(),
            descending: false,
            favorites_only: false,
            activity: false,
        },
    )
    .await
    .into_iter()
    .map(|row| row.album_key)
    .collect::<Vec<_>>();
    assert_eq!(albums.len(), 1);
    assert_eq!(album_genres(&catalog, albums[0]).await, ["Rock", "Jazz"]);

    let mut stored = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&database_path),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE files SET parse_version=16 WHERE kind IN ('media','cue')")
        .execute(&mut stored)
        .await
        .unwrap();
    sqlx::query("UPDATE track_details SET metadata=jsonb_remove(metadata,'$.album_tags')")
        .execute(&mut stored)
        .await
        .unwrap();
    stored.close().await.unwrap();
    set_genre(&first, "Folk").expect("change a Track from the old cache");
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .expect("refresh the changed album and its old cached sibling");
    sources::published(&mut updates).expect("catalog update");
    assert_eq!(album_genres(&catalog, albums[0]).await, ["Folk", "Jazz"]);

    set_genre(&first, "Electronic").expect("change one Track genre");
    apply_paths(&source, &catalog, vec![first], None)
        .await
        .expect("exact Track change");
    sources::published(&mut updates).expect("catalog update");
    assert_eq!(
        album_genres(&catalog, albums[0]).await,
        ["Electronic", "Jazz"]
    );
}

#[tokio::test]
async fn explicit_rename_preserves_track_identity_without_native_file_identity() {
    let root = tempfile::tempdir().expect("music root");
    let old = root.path().join("old café %.wav");
    let new = root.path().join("new café %.wav");
    write_tagged_wav(&old, "Track", "Artist", "Album", "Genre").expect("tagged WAV");
    let store = tempfile::tempdir().expect("Store");
    let database_path = store.path().join("library.sqlite");
    let catalog = Catalog::open(&database_path).await.expect("Catalog");
    let mut updates = catalog.changes();
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetup::Local {
            roots: vec![root.path().to_path_buf()],
        },
        "",
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    catalog
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sources::Refresh::Changed,
        )
        .await
        .expect("scan");
    let initial = sources::published(&mut updates).expect("catalog update");
    let order = catalog
        .selected_track_uris(
            &MemberRead::new(Collection::Source(initial.0)).selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .expect("Track order");
    let object_id = catalog
        .tracks_by_uris(&((&order).to_vec()), &ReadCancellation::new())
        .await
        .expect("Track row")[0]
        .object_id
        .clone();
    let mut stored = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&database_path),
    )
    .await
    .unwrap();
    // Without a native identity, only the reported rename links the two paths.
    sqlx::query("UPDATE files SET device_id=NULL,inode=NULL WHERE kind='media'")
        .execute(&mut stored)
        .await
        .expect("remove native identity enrichment");
    stored.close().await.unwrap();
    fs::rename(&old, &new).expect("rename file");
    apply_paths(&source, &catalog, vec![old.clone(), new.clone()], Some((old, new)))
        .await
        .expect("exact rename");
    let changed = sources::published(&mut updates).expect("catalog update");
    let changed_order = catalog
        .selected_track_uris(
            &MemberRead::new(Collection::Source(changed.0)).selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .expect("changed Track order");
    let uri = &changed_order[0];
    let source = Arc::new(source);
    let (editor, _metadata) = sources::FileEditSession::open(
        Some(Arc::clone(&source)),
        &catalog,
        uri.to_string(),
        sources::SourceEntityKind::Track,
    )
    .await
    .unwrap();
    {
        editor
            .save(
                &catalog,
                (sources::MetadataEdit {
                    kind: sources::SourceEntityKind::Track,
                    changes: [("title".into(), "After rename".into())].into(),
                    artwork: None,
                })
                .clone(),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        catalog
            .tracks_by_uris(std::slice::from_ref(uri), &ReadCancellation::new())
            .await
            .map(|rows| { rows.into_iter().next() })
            .unwrap()
            .unwrap()
            .object_id,
        object_id
    );
}

#[tokio::test]
async fn initial_local_walk_reports_real_total_free_file_progress() {
    let root = tempfile::tempdir().expect("music root");
    let other = tempfile::tempdir().expect("second music root");
    fs::create_dir(root.path().join("nested")).unwrap();
    write_silent_wav(&root.path().join("first.wav"), 1).expect("first WAV");
    write_silent_wav(&root.path().join("second.wav"), 1).expect("second WAV");
    write_silent_wav(&root.path().join("nested/third.wav"), 1).unwrap();
    write_silent_wav(&other.path().join("backing.wav"), 1).unwrap();
    fs::write(root.path().join("notes.txt"), "Not audio").unwrap();
    fs::write(root.path().join("cover.jpg"), "Not audio").unwrap();
    fs::write(
        other.path().join("album.cue"),
        "FILE \"backing.wav\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nINDEX 01 00:00:37\n",
    )
    .unwrap();
    let store = tempfile::tempdir().expect("Store");
    let catalog = Catalog::open(store.path().join("library.sqlite"))
        .await
        .expect("Catalog");
    let mut identities = None;
    for source_id in ["test-source", "readded-source"] {
        let (configuration, source, _) = Source::connect(
            SourceId::new(source_id),
            SourceSetup::Local {
                roots: vec![root.path().to_path_buf(), other.path().to_path_buf()],
            },
            "",
        )
        .await
        .unwrap()
        .into_parts();
        catalog
            .reconcile_source(
                &configuration.source_id,
                &configuration.kind,
                &configuration.name,
            )
            .await
            .unwrap();
        if source_id == "test-source" {
            catalog
                .set_favorite(
                    &library::FavoriteTarget::Track(
                        url::Url::from_file_path(
                            root.path().join("first.wav").canonicalize().unwrap(),
                        )
                        .unwrap()
                        .into(),
                    ),
                    true,
                    false,
                )
                .await
                .unwrap();
        }
        let source_key = {
            catalog.loaded().await;
            catalog.source_key(source.source_id())
        }
        .unwrap();
        for _ in 0..2 {
            let progress = Mutex::new(Vec::new());
            source
                .acquire_catalog(
                    &catalog,
                    &|update| progress.lock().unwrap().push(update),
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    sources::Refresh::Changed,
                )
                .await
                .unwrap();
            let progress = progress.into_inner().unwrap();
            assert!(progress.iter().all(|update| {
                update.stage == sources::SourceProgressStage::Tracks && update.total.is_none()
            }));
            assert!(
                progress
                    .windows(2)
                    .all(|pair| pair[0].completed <= pair[1].completed)
            );
            // A walk that reads nothing reports nothing.
            assert!(progress.last().is_none_or(|last| last.completed == 5));
            assert_eq!(
                sources::listed_tracks(&catalog, MemberRead::new(Collection::Source(source_key)))
                    .await
                    .len(),
                5,
                "accepted tracks must also be indexed, including retained tracks from the old source"
            );
            let current = catalog
                .mapping_track_page(source_key, None, None, 10, &ReadCancellation::new())
                .await
                .unwrap()
                .into_iter()
                .map(|track| (track.track_key, track.media_uri))
                .collect::<Vec<_>>();
            if let Some(expected) = &identities {
                assert_eq!(
                    &current, expected,
                    "source reassignment keeps track identities"
                );
            } else {
                assert_eq!(current.len(), 5);
                identities = Some(current);
            }
        }
        if source_id == "test-source" {
            write_silent_wav(&root.path().join("first.wav"), 2).unwrap();
            apply_paths(&source, &catalog, vec![root.path().join("first.wav")], None)
                .await
                .unwrap();
            assert!(catalog.remove_source(source.source_id()).await.unwrap());
        }
    }
}

async fn track_count(
    catalog: &Catalog,
    publication: &(library::SourceKey, sources::Shown),
) -> usize {
    catalog
        .order(
            &library::Read::Members(MemberRead::new(Collection::Source(publication.0))),
            &ReadCancellation::new(),
        )
        .await
        .expect("Track count")
        .len()
}

/// An album as its page reads it, by URI.
async fn album_view(catalog: &Catalog, album: library::AlbumKey, read: &str) -> library::AlbumView {
    catalog.loaded().await;
    let uri = catalog.uri(library::Item::Album(album)).expect(read);
    catalog
        .album(&uri, false, &ReadCancellation::new())
        .await
        .expect(read)
        .expect(read)
}

async fn album_genres(catalog: &Catalog, album: library::AlbumKey) -> Vec<String> {
    album_view(&catalog, album, "Album row").await
        .row
        .links
        .genres
        .iter()
        .map(|genre| genre.name.clone())
        .collect()
}

fn write_tagged_wav(
    path: &Path,
    title: &str,
    artist: &str,
    album: &str,
    genre: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    write_silent_wav(path, 1)?;
    let mut tagged = Probe::open(path)?.read()?;
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title(title.to_string());
    tag.set_artist(artist.to_string());
    tag.set_album(album.to_string());
    tag.insert_text(ItemKey::AlbumArtist, "Album Artist".to_string());
    tag.set_genre(genre.to_string());
    tagged.insert_tag(tag);
    tagged.save_to_path(path, WriteOptions::default())?;
    Ok(())
}

fn set_genre(path: &Path, genre: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut tagged = Probe::open(path)?.read()?;
    tagged
        .primary_tag_mut()
        .ok_or("missing primary tag")?
        .set_genre(genre.to_string());
    tagged.save_to_path(path, WriteOptions::default())?;
    Ok(())
}

fn write_silent_wav(path: &Path, seconds: u32) -> std::io::Result<()> {
    let sample_rate = 8_000_u32;
    let bits_per_sample = 16_u16;
    let channels = 1_u16;
    let sample_count = sample_rate.saturating_mul(seconds);
    let data_len = sample_count * u32::from(channels) * u32::from(bits_per_sample / 8);
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample / 8);
    let block_align = channels * (bits_per_sample / 8);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(bytes.len() + data_len as usize, 0);
    fs::write(path, bytes)
}

/// Every stored file row of a source, in walk order.
async fn stored_files(catalog: &Catalog, source: &str) -> Vec<library::LocalFileWrite> {
    catalog
        .observe(source)
        .await
        .unwrap()
        .file_rows(&[], &[], None, false, 8192)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.file)
        .collect()
}

async fn apply_paths(
    source: &Source,
    catalog: &Catalog,
    paths: Vec<std::path::PathBuf>,
    rename: Option<(std::path::PathBuf, std::path::PathBuf)>,
) -> sources::SourceResult<()> {
    let (paths, rename) = LocalLiveChange::path_strings(paths, rename);
    source
        .apply_file_paths(catalog, &paths, rename.as_ref(), &|| false)
        .await
}
