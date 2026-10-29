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
            .acquire_catalog(&catalog, &|_| {}, cancelled.clone(), true)
            .await,
        Err(sources::SourceError::Other(_))
    ));
    fs::rename(&offline, &root).unwrap();
    source
        .acquire_catalog(&catalog, &|_| {}, cancelled.clone(), true)
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
    let profile = library::profile::ProfileStore::open(
        catalog.clone(),
        1,
        "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33"
            .parse()
            .unwrap(),
    )
    .await
    .unwrap();
    profile.bind_profile("shared", "source").await.unwrap();
    catalog.connect_set_active(true).await.unwrap();
    fs::rename(&root, &offline).unwrap();
    assert!(
        source
            .acquire_catalog(&catalog, &|_| {}, cancelled.clone(), true)
            .await
            .is_err()
    );
    assert_eq!(track_count(&catalog, &initial).await, 1);
    assert!(
        source
            .catch_up_local(&catalog, &|_| {}, cancelled.clone())
            .await
            .is_err()
    );
    assert!(
        source
            .apply_local_change(
                &catalog,
                initial.0,
                LocalLiveChange::Paths {
                    paths: vec![track],
                    rename: None
                }
            )
            .await
            .is_err()
    );
    assert_eq!(track_count(&catalog, &initial).await, 1);
    fs::rename(&offline, &root).unwrap();
    source
        .acquire_catalog(&catalog, &|_| {}, cancelled, true)
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
            true,
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
    source
        .apply_local_change(
            &catalog,
            first_publication.0,
            LocalLiveChange::Paths {
                paths: vec![second.clone()],
                rename: None,
            },
        )
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
            true,
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
            .catch_up_local(
                &catalog,
                &|progress| panic!("unchanged catch-up started parsing: {progress:?}"),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        let observation = catalog
            .local_file_page(initial.0, None, 10, &ReadCancellation::new())
            .await
            .unwrap()
            .into_iter()
            .find(|file| file.kind == library::LocalFileKind::Media)
            .unwrap();
        assert_eq!(observation.parse_version, Some(19));
        assert_eq!(observation.state, library::LocalFileState::Accepted);
        catalog.close().await.unwrap();
    }
    let catalog = Catalog::open(&database_path).await.unwrap();
    let received_path = track.canonicalize().unwrap();
    let previous = catalog
        .source_files(
            configuration.source_id.as_str(),
            &[received_path.to_string_lossy().into_owned()],
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    let snapshot = store.path().join("profile.sqlite");
    let profile = library::profile::ProfileStore::open(
        catalog.clone(),
        1,
        "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33"
            .parse()
            .unwrap(),
    )
    .await
    .unwrap();
    profile.bind_profile("shared", "source").await.unwrap();
    let key = "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33"
        .parse()
        .unwrap();
    profile
        .export_database(&snapshot, &key, &[], &[])
        .await
        .unwrap();
    profile.close().await.unwrap();
    library::profile::ProfileStore::prepare_database(&snapshot, &key, &key)
        .await
        .unwrap();
    library::profile::ProfileStore::install_prepared_database(catalog.clone(), &snapshot)
        .await
        .unwrap();
    let received = catalog
        .source_files(
            configuration.source_id.as_str(),
            &[received_path.to_string_lossy().into_owned()],
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(received, previous);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&database_path).await.unwrap();
    source
        .catch_up_local(
            &catalog,
            &|progress| panic!("received index started parsing: {progress:?}"),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    catalog.close().await.unwrap();
    write_silent_wav(&track, 2).expect("edit WAV in place");
    let catalog = Catalog::open(&database_path)
        .await
        .expect("reopen after edit");
    let mut updates = catalog.changes();
    let progress = Mutex::new(Vec::new());
    source
        .catch_up_local(
            &catalog,
            &|update| progress.lock().unwrap().push(update),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("restart catch-up");
    let progress = progress.into_inner().unwrap();
    assert!(progress.iter().any(|update| {
        update.stage == sources::SourceReadStage::Tracks && update.completed > 0
    }));
    assert!(progress.iter().all(|update| {
        update.stage == sources::SourceReadStage::Tracks && update.total.is_none()
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
    fs::remove_file(&track).expect("remove cached file while stopped");
    let catalog = Catalog::open(&database_path)
        .await
        .expect("reopen after removal");
    let mut updates = catalog.changes();
    source
        .catch_up_local(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("remove missing file during catch-up");
    let removed = sources::published(&mut updates).expect("catalog update");
    assert_eq!(
        catalog
            .source_counts(removed.0, &ReadCancellation::new())
            .await
            .unwrap()
            .1,
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
        .catch_up_local(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    let paths = [malformed
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned()];
    assert_eq!(
        catalog
            .source_files(configuration.source_id.as_str(), &paths)
            .await
            .unwrap()[0]
            .state,
        library::LocalFileState::Rejected
    );
    catalog.close().await.unwrap();
    for _ in 0..2 {
        let catalog = Catalog::open(&database_path).await.unwrap();
        source
            .catch_up_local(
                &catalog,
                &|progress| panic!("cached rejection started parsing: {progress:?}"),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert_eq!(
            catalog
                .source_files(configuration.source_id.as_str(), &paths)
                .await
                .unwrap()[0]
                .state,
            library::LocalFileState::Rejected
        );
        catalog.close().await.unwrap();
    }
    write_silent_wav(&malformed, 1).expect("replace malformed input with audio");
    let catalog = Catalog::open(&database_path).await.unwrap();
    source
        .catch_up_local(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert_eq!(
        catalog
            .source_files(configuration.source_id.as_str(), &paths)
            .await
            .unwrap()[0]
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
            true,
        )
        .await
        .expect("scan");
    let initial = sources::published(&mut updates).expect("catalog update");

    fs::create_dir(root.path().join("empty")).expect("change only Local inventory bookkeeping");
    source
        .catch_up_local(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
                    if progress.stage == sources::SourceReadStage::Tracks {
                        stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                },
                cancelled,
                true
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
        .catch_up_local(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    if let Some(update) = sources::published(&mut updates) {
        assert!(update.1.intersects(sources::Shown::CATALOG));
    }
    let mut files = Vec::new();
    loop {
        let page = catalog
            .local_file_page(
                initial.0,
                files
                    .last()
                    .map(|file: &library::LocalFileRow| file.local_file_key),
                128,
                &ReadCancellation::new(),
            )
            .await
            .unwrap();
        if page.is_empty() {
            break;
        }
        files.extend(page);
    }
    assert_eq!(files.len(), 263);
    assert!(
        files
            .iter()
            .filter(|file| file.kind == library::LocalFileKind::Directory)
            .all(|file| file.state == library::LocalFileState::Accepted)
    );
    let progress = Mutex::new(Vec::new());
    source
        .catch_up_local(
            &catalog,
            &|update| progress.lock().unwrap().push(update),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
            true,
        )
        .await
        .unwrap();
    let first = sources::published(&mut updates).expect("catalog update");
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
            true,
        )
        .await
        .unwrap();
    assert!(
        sources::published(&mut updates).is_none(),
        "unchanged observations stay quiet"
    );
    fs::write(&cover, b"changed image bytes").unwrap();
    source
        .apply_local_change(
            &catalog,
            first.0,
            LocalLiveChange::Paths {
                paths: vec![cover],
                rename: None,
            },
        )
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
            true,
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
            .acquire_catalog(&catalog, &|_| {}, cancelled, true)
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
        sqlx::query_scalar::<_, Option<i64>>("SELECT picture_index FROM local_files WHERE path=?1")
            .bind(paths[0].to_string_lossy().as_ref())
            .fetch_one(&mut stored)
            .await
            .unwrap(),
        Some(-1),
        "an inspected file without a cover stays negatively cached"
    );
    sources::published(&mut updates);
    source
        .catch_up_local(
            &catalog,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(
        sources::published(&mut updates).is_none(),
        "unchanged image facts stay quiet"
    );
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
                true,
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
            true,
        )
        .await
        .unwrap();
    let initial = sources::published(&mut updates).expect("catalog update");
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
        source
            .apply_local_change(
                &catalog,
                initial.0,
                LocalLiveChange::Paths {
                    paths: vec![cover.clone()],
                    rename: None,
                },
            )
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
            true,
        )
        .await
        .expect("scan");
    let initial = sources::published(&mut updates).expect("catalog update");
    assert_eq!(track_count(&catalog, &initial).await, 1);

    fs::write(&media, b"temporarily unreadable media content").expect("damage backing media");
    source
        .apply_local_change(
            &catalog,
            initial.0,
            LocalLiveChange::Paths {
                paths: vec![cue.clone()],
                rename: None,
            },
        )
        .await
        .expect("transient CUE change");
    assert!(
        sources::published(&mut updates).is_none(),
        "unreadable backing preserves accepted content without invalidation"
    );
    assert_eq!(track_count(&catalog, &initial).await, 1);

    fs::write(&cue, "this is not a cue sheet").expect("reject CUE");
    source
        .apply_local_change(
            &catalog,
            initial.0,
            LocalLiveChange::Paths {
                paths: vec![cue],
                rename: None,
            },
        )
        .await
        .expect("rejected CUE change");
    let rejected = sources::published(&mut updates).expect("catalog update");
    assert_eq!(track_count(&catalog, &rejected).await, 0);
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
            true,
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
    let media = catalog
        .local_file_page(initial.0, None, 128, &ReadCancellation::new())
        .await
        .expect("Local observations")
        .into_iter()
        .find(|file| file.kind == library::LocalFileKind::Media)
        .expect("media observation");
    let mut observation = catalog
        .observe(configuration.source_id.as_str(), None)
        .await
        .unwrap();
    observation
        .write_file_observations(
            &[(
                library::LocalFileWrite {
                    path: media.path,
                    root: media.root,
                    relative_path: media.relative_path,
                    kind: media.kind,
                    size_bytes: media.size_bytes,
                    mtime_ns: media.mtime_ns,
                    device_id: None,
                    inode: None,
                    native_id: None,
                    picture_index: None,
                    artist_pictures: None,
                    revision: None,
                    parse_version: media.parse_version,
                    state: media.state,
                },
                media.dependencies,
            )],
            Vec::new(),
            true,
        )
        .await
        .expect("remove native identity enrichment");
    assert!(!observation.changed());
    fs::rename(&old, &new).expect("rename file");
    source
        .apply_local_change(
            &catalog,
            initial.0,
            LocalLiveChange::Paths {
                paths: vec![old.clone(), new.clone()],
                rename: Some((old, new)),
            },
        )
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
