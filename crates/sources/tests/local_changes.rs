use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use library::{CatalogQuery, Database, ReadCancellation, TrackScope};
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
    fs::create_dir(&root).unwrap();
    let track = root.join("track.wav");
    write_silent_wav(&track, 1).unwrap();
    let database = Database::open(directory.path().join("library.sqlite"))
        .await
        .unwrap();
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
    let (configuration, source, _) = Source::connect(
        SourceId::new("test-source"),
        SourceSetup::Local {
            roots: vec![root.clone()],
        },
        "",
    )
    .await
    .unwrap()
    .into_parts();
    database
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
            .acquire_catalog(&database, &|_| {}, cancelled.clone(), true)
            .await,
        Err(sources::SourceError::Other(_))
    ));
    fs::rename(&offline, &root).unwrap();
    source
        .acquire_catalog(&database, &|_| {}, cancelled.clone(), true)
        .await
        .unwrap();
    let initial = updates.try_recv().expect("catalog update");
    assert!(initial.1.metadata);
    fs::rename(&root, &offline).unwrap();
    assert!(
        source
            .acquire_catalog(&database, &|_| {}, cancelled.clone(), true)
            .await
            .is_err()
    );
    assert!(
        source
            .catch_up_local(&database, &|_| {}, cancelled.clone())
            .await
            .is_err()
    );
    assert!(
        source
            .apply_local_change(
                &database,
                initial.0,
                LocalLiveChange::Paths {
                    paths: vec![track],
                    rename: None
                }
            )
            .await
            .is_err()
    );
    assert_eq!(track_count(&database, &initial).await, 1);
    fs::rename(&offline, &root).unwrap();
    source
        .acquire_catalog(&database, &|_| {}, cancelled, true)
        .await
        .unwrap();
    assert_eq!(track_count(&database, &initial).await, 1);
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
    let database = Database::open(&database_path).await.expect("Database");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
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
    database
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .expect("initial scan");
    let first_publication = updates.try_recv().expect("catalog update");
    let before = database
        .selected_track_uris(
            &CatalogQuery::tracks(TrackScope::Tracks(first_publication.0)).selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .expect("before order");
    assert_eq!(before.len(), 2);

    write_silent_wav(&second, 2).expect("edit second WAV");
    source
        .apply_local_change(
            &database,
            first_publication.0,
            LocalLiveChange::Paths {
                paths: vec![second.clone()],
                rename: None,
            },
        )
        .await
        .expect("exact change");
    let changed = updates.try_recv().expect("catalog update");
    assert!(changed.1.metadata);
    let after = database
        .selected_track_uris(
            &CatalogQuery::tracks(TrackScope::Tracks(changed.0)).selection(),
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
    let database = Database::open(store.path().join("library.sqlite"))
        .await
        .expect("Database");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
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
    database
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .expect("scan");
    let initial = updates.try_recv().expect("catalog update");
    let album = library::CatalogQuery {
        subject: library::CatalogSubject::Albums {
            source: initial.0,
            sort: library::CatalogSort::Title,
            inline_tracks: None,
        },
        folder: None,
        filter: ("").to_string(),
        descending: false,
        favorites_only: false,
        downloaded_only: false,
        activity: false,
    }
    .page(
        &database,
        library::RouteSeedWindow::top(),
        &ReadCancellation::new(),
    )
    .await
    .map(|page| match page.rows {
        library::CatalogRows::Albums(rows) => (page.count, page.first_row_position, rows),
        _ => unreachable!(),
    })
    .expect("Album order")
    .2[0]
        .album_key;
    let before = {
        let mut query = library::CatalogQuery::keys(
            library::CatalogKind::Album,
            Some(initial.0),
            (&[album]).iter().map(|key| key.raw()).collect(),
        );
        query.folder = None;
        query
            .rows(&database, 0, (&[album]).len(), &ReadCancellation::new())
            .await
            .map(|rows| match rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!("typed catalog projection"),
            })
    }
    .expect("initial Album artwork")[0]
        .artwork_binding
        .clone();
    assert!(before.is_some());
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = Arc::clone(&cancelled);
    let interrupted = source
        .acquire_catalog(
            &database,
            &move |progress| {
                if progress.stage == sources::SourceReadStage::Tracks {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            },
            cancelled,
            true,
        )
        .await;
    assert!(matches!(interrupted, Err(sources::SourceError::Cancelled)));
    assert_eq!(
        library::CatalogQuery::keys(
            library::CatalogKind::Album,
            Some(initial.0),
            (&[album]).iter().map(|v| v.raw()).collect()
        )
        .rows(&database, 0, 256, &ReadCancellation::new())
        .await
        .map(|rows| match rows {
            library::CatalogRows::Albums(rows) => rows,
            _ => unreachable!(),
        })
        .expect("Album cover after cancelled discovery")[0]
            .artwork_binding,
        before
    );
    assert!(
        updates.try_recv().is_err(),
        "cancelled no-op does not publish"
    );
    fs::write(&image, b"changed-image").expect("change image");
    source
        .apply_local_change(
            &database,
            initial.0,
            LocalLiveChange::Paths {
                paths: vec![image],
                rename: None,
            },
        )
        .await
        .expect("image change");
    let publication = updates.try_recv().expect("catalog update");
    assert!(publication.1.artwork);
    assert!(!publication.1.metadata && !publication.1.playlists && !publication.1.home);
    assert_ne!(
        library::CatalogQuery::keys(
            library::CatalogKind::Album,
            Some(initial.0),
            (&[album]).iter().map(|v| v.raw()).collect()
        )
        .rows(&database, 0, 256, &ReadCancellation::new())
        .await
        .map(|rows| match rows {
            library::CatalogRows::Albums(rows) => rows,
            _ => unreachable!(),
        })
        .expect("Album row")[0]
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
    let database = Database::open(&database_path).await.expect("Database");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
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
    database
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .expect("scan");
    let initial = updates.try_recv().expect("catalog update");
    let order = database
        .selected_track_uris(
            &CatalogQuery::tracks(TrackScope::Tracks(initial.0)).selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .expect("Track order");
    assert_eq!(
        library::CatalogQuery::uris(library::CatalogKind::Track, (&order).to_vec())
            .rows(&database, 0, 256, &ReadCancellation::new())
            .await
            .map(|rows| match rows {
                library::CatalogRows::Tracks(rows) => rows,
                _ => unreachable!(),
            })
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
    sqlx::query("UPDATE local_files SET parse_version=16,parsed_stamp=json_set(parsed_stamp,'$[3]',16) WHERE kind IN ('media','cue')")
        .execute(&mut stored)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET metadata=jsonb_remove(metadata,'$.album_tags')")
        .execute(&mut stored)
        .await
        .unwrap();
    stored.close().await.unwrap();
    database
        .close()
        .await
        .expect("close before unchanged restarts");
    for _ in 0..2 {
        let database = Database::open(&database_path).await.unwrap();
        source
            .catch_up_local(
                &database,
                &|progress| panic!("unchanged catch-up started parsing: {progress:?}"),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        let observation = database
            .local_file_page(initial.0, None, 10, &ReadCancellation::new())
            .await
            .unwrap()
            .into_iter()
            .find(|file| file.kind == library::LocalFileKind::Media)
            .unwrap();
        assert_eq!(observation.parse_version, Some(16));
        assert_eq!(observation.state, library::LocalFileState::Accepted);
        database.close().await.unwrap();
    }
    write_silent_wav(&track, 2).expect("edit WAV in place");
    let database = Database::open(&database_path)
        .await
        .expect("reopen after edit");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
    let progress = Mutex::new(Vec::new());
    source
        .catch_up_local(
            &database,
            &|update| progress.lock().unwrap().push(update),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("restart catch-up");
    let progress = progress.into_inner().unwrap();
    assert!(progress.iter().any(|update| {
        update.stage == sources::SourceReadStage::Tracks && update.completed > 0
    }));
    assert!(
        progress
            .iter()
            .any(|update| update.stage == sources::SourceReadStage::Artwork)
    );
    assert_eq!(
        progress.last().unwrap().stage,
        sources::SourceReadStage::Finalizing
    );
    let changed = updates.try_recv().expect("catalog update");
    assert!(changed.1.metadata);
    assert_eq!(
        library::CatalogQuery::uris(library::CatalogKind::Track, (&order).to_vec())
            .rows(&database, 0, 256, &ReadCancellation::new())
            .await
            .map(|rows| match rows {
                library::CatalogRows::Tracks(rows) => rows,
                _ => unreachable!(),
            })
            .expect("changed Track row")[0]
            .metadata
            .duration_millis,
        2_000
    );
    database.close().await.expect("close before file removal");
    fs::remove_file(&track).expect("remove cached file while stopped");
    let database = Database::open(&database_path)
        .await
        .expect("reopen after removal");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
    source
        .catch_up_local(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("remove missing file during catch-up");
    let removed = updates.try_recv().expect("catalog update");
    assert_eq!(
        database
            .source_counts(removed.0, &ReadCancellation::new())
            .await
            .unwrap()
            .1,
        0
    );
}

#[tokio::test]
async fn restart_catch_up_publishes_folder_changes_without_reparsing_tracks() {
    let root = tempfile::tempdir().expect("music root");
    let track = root.path().join("track.wav");
    write_silent_wav(&track, 1).expect("WAV");
    let store = tempfile::tempdir().expect("Store");
    let database_path = store.path().join("library.sqlite");
    let database = Database::open(&database_path).await.expect("Database");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
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
    database
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .expect("scan");
    let initial = updates.try_recv().expect("catalog update");

    fs::create_dir(root.path().join("empty")).expect("change only Local inventory bookkeeping");
    source
        .catch_up_local(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("restart catch-up");

    assert!(
        updates
            .try_recv()
            .expect("new folder publication")
            .1
            .metadata
    );
    assert!(
        source
            .browse_folder(&database, None, None)
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
                &database,
                &move |progress| {
                    if progress.stage == sources::SourceReadStage::Files {
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
        database
            .cached_source(configuration.source_id.as_str(), &ReadCancellation::new())
            .await
            .unwrap()
            .unwrap()
            .catalog_complete
    );
    database.close().await.unwrap();
    let database = Database::open(&database_path).await.unwrap();
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
    source
        .catch_up_local(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    while let Ok(update) = updates.try_recv() {
        assert!(update.1.metadata);
    }
    let mut files = Vec::new();
    loop {
        let page = database
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
            &database,
            &|update| progress.lock().unwrap().push(update),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(
        updates.try_recv().is_err(),
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
    let database = Database::open(store.path().join("library.sqlite"))
        .await
        .expect("Database");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
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
    database
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .expect("scan");
    let initial = updates.try_recv().expect("catalog update");
    let albums = library::CatalogQuery {
        subject: library::CatalogSubject::Albums {
            source: initial.0,
            sort: library::CatalogSort::Title,
            inline_tracks: None,
        },
        folder: None,
        filter: ("").to_string(),
        descending: false,
        favorites_only: false,
        downloaded_only: false,
        activity: false,
    }
    .page(
        &database,
        library::RouteSeedWindow::top(),
        &ReadCancellation::new(),
    )
    .await
    .map(|page| match page.rows {
        library::CatalogRows::Albums(rows) => (page.count, page.first_row_position, rows),
        _ => unreachable!(),
    })
    .expect("Album order")
    .2;
    assert_eq!(albums.len(), 2);
    write_silent_wav(&media, 3).expect("edit CUE backing media");
    source
        .catch_up_local(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("first catch-up");
    updates.try_recv().expect("catalog update");
    source
        .catch_up_local(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("second catch-up");

    assert!(
        updates.try_recv().is_err(),
        "unchanged observations stay quiet"
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
    let database = Database::open(store.path().join("library.sqlite"))
        .await
        .unwrap();
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
    let connected = Source::connect(
        SourceId::new("local-art"),
        SourceSetup::Local { roots: vec![root] },
        "",
    )
    .await
    .unwrap();
    let (configuration, source, _) = connected.into_parts();
    database
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .unwrap();
    let first = updates.try_recv().expect("catalog update");
    let uri = library::normalize_direct_media_uri(
        url::Url::from_file_path(path.canonicalize().unwrap())
            .unwrap()
            .as_str(),
    )
    .unwrap();
    assert!(uri.contains("%25"));
    let row = library::CatalogQuery::uris(library::CatalogKind::Track, vec![(&uri).to_string()])
        .rows(&database, 0, 1, &ReadCancellation::new())
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(mut rows) => rows.pop(),
            _ => unreachable!("typed catalog projection"),
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
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .unwrap();
    assert!(
        updates.try_recv().is_err(),
        "unchanged observations stay quiet"
    );
    fs::write(&cover, b"changed image bytes").unwrap();
    source
        .apply_local_change(
            &database,
            first.0,
            LocalLiveChange::Paths {
                paths: vec![cover],
                rename: None,
            },
        )
        .await
        .unwrap();
    let update = updates.try_recv().expect("catalog update");
    assert!(update.1.artwork && !update.1.metadata && !update.1.playlists && !update.1.home);
    let changed =
        library::CatalogQuery::uris(library::CatalogKind::Track, vec![(&uri).to_string()])
            .rows(&database, 0, 1, &ReadCancellation::new())
            .await
            .map(|rows| match rows {
                library::CatalogRows::Tracks(mut rows) => rows.pop(),
                _ => unreachable!("typed catalog projection"),
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
    let database = Database::open(store.path().join("library.sqlite"))
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
    database
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &database,
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
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |_, update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    let mut expected_group = None;
    for distinct in [false, true, false] {
        database.set_distinct_track_covers(distinct).await.unwrap();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        source
            .acquire_catalog(&database, &|_| {}, cancelled, true)
            .await
            .unwrap();
        for (index, path) in paths.iter().enumerate() {
            let uri = url::Url::from_file_path(path).unwrap().to_string();
            let row =
                library::CatalogQuery::uris(library::CatalogKind::Track, vec![(&uri).to_string()])
                    .rows(&database, 0, 1, &ReadCancellation::new())
                    .await
                    .map(|rows| match rows {
                        library::CatalogRows::Tracks(mut rows) => rows.pop(),
                        _ => unreachable!("typed catalog projection"),
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
    while updates.try_recv().is_ok() {}
    source
        .catch_up_local(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(
        updates.try_recv().is_err(),
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
    let database = Database::open(store.path().join("library.sqlite"))
        .await
        .unwrap();
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
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
    database
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .unwrap();
    let initial = updates.try_recv().expect("catalog update");
    let artist = library::CatalogQuery {
        subject: library::CatalogSubject::Artists {
            source: initial.0,
            sort: library::CatalogSort::Title,
            album_artist: false,
        },
        folder: None,
        filter: ("").to_string(),
        descending: false,
        favorites_only: false,
        downloaded_only: false,
        activity: false,
    }
    .page(
        &database,
        library::RouteSeedWindow::top(),
        &ReadCancellation::new(),
    )
    .await
    .map(|page| match page.rows {
        library::CatalogRows::Artists(rows) => (page.count, page.first_row_position, rows),
        _ => unreachable!(),
    })
    .unwrap()
    .2
    .remove(0);
    for changed in [&covers[1], &covers[0]] {
        fs::write(changed, b"replacement artwork").unwrap();
        source
            .apply_local_change(
                &database,
                initial.0,
                LocalLiveChange::Paths {
                    paths: vec![changed.clone()],
                    rename: None,
                },
            )
            .await
            .unwrap();
        let update = updates.try_recv().expect("catalog update");
        assert!(update.1.artwork && !update.1.metadata && !update.1.playlists && !update.1.home);
        let current = library::CatalogQuery::uris(
            library::CatalogKind::Artist,
            vec![(&artist.media_uri).to_string()],
        )
        .rows(&database, 0, 1, &ReadCancellation::new())
        .await
        .map(|rows| match rows {
            library::CatalogRows::Artists(mut rows) => rows.pop(),
            _ => unreachable!("typed catalog projection"),
        })
        .unwrap()
        .unwrap();
        let artwork: Vec<library::ArtworkCandidate> =
            serde_json::from_slice(current.artwork_binding.as_ref().unwrap()).unwrap();
        let binding = &artwork;
        assert_eq!(binding[0].source_id().unwrap(), source.source_id());
        assert!(
            matches!(&binding[0].image, library::ArtworkImage::Local { path, picture_index: None, .. } if path == & covers[0].to_string_lossy())
        );
        if changed == &covers[1] {
            assert_eq!(current.artwork_binding, artist.artwork_binding);
        } else {
            assert_ne!(current.artwork_binding, artist.artwork_binding);
        }
    }
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
    let database = Database::open(store.path().join("library.sqlite"))
        .await
        .unwrap();
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
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
    database
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .unwrap();
    let initial = updates.try_recv().expect("catalog update");
    let borrower_uri = url::Url::from_file_path(&paths[1]).unwrap().to_string();
    let before = library::CatalogQuery::uris(
        library::CatalogKind::Track,
        vec![(&borrower_uri).to_string()],
    )
    .rows(&database, 0, 1, &ReadCancellation::new())
    .await
    .map(|rows| match rows {
        library::CatalogRows::Tracks(mut rows) => rows.pop(),
        _ => unreachable!("typed catalog projection"),
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
                &database,
                initial.0,
                LocalLiveChange::Paths {
                    paths: vec![cover.clone()],
                    rename: None,
                },
            )
            .await
            .unwrap();
        let update = updates.try_recv().expect("catalog update");
        assert!(update.1.artwork && !update.1.metadata && !update.1.playlists && !update.1.home);
        let borrower = library::CatalogQuery::uris(
            library::CatalogKind::Track,
            vec![(&borrower_uri).to_string()],
        )
        .rows(&database, 0, 1, &ReadCancellation::new())
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(mut rows) => rows.pop(),
            _ => unreachable!("typed catalog projection"),
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
            let binding =
                library::CatalogQuery::uris(library::CatalogKind::Track, vec![(&uri).to_owned()])
                    .rows(&database, 0, 256, &ReadCancellation::new())
                    .await
                    .map(|rows| match rows {
                        library::CatalogRows::Tracks(rows) => rows.into_iter().next(),
                        _ => unreachable!(),
                    })
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
    let database = Database::open(store.path().join("library.sqlite"))
        .await
        .unwrap();
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
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
    database
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .unwrap();
    let initial = updates.try_recv().expect("catalog update");
    let uri = url::Url::from_file_path(&path).unwrap().to_string();
    let track = library::CatalogQuery::uris(library::CatalogKind::Track, vec![(&uri).to_string()])
        .rows(&database, 0, 1, &ReadCancellation::new())
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(mut rows) => rows.pop(),
            _ => unreachable!("typed catalog projection"),
        })
        .unwrap()
        .unwrap();
    let album = {
        let mut query = library::CatalogQuery::keys(
            library::CatalogKind::Album,
            Some(initial.0),
            (&[track.links.album_key.unwrap()])
                .iter()
                .map(|key| key.raw())
                .collect(),
        );
        query.folder = None;
        query
            .rows(
                &database,
                0,
                (&[track.links.album_key.unwrap()]).len(),
                &ReadCancellation::new(),
            )
            .await
            .map(|rows| match rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!("typed catalog projection"),
            })
    }
    .unwrap()
    .remove(0);
    let source = Arc::new(source);
    let (editor, _metadata) = sources::FileEditSession::open(
        Some(Arc::clone(&source)),
        &database,
        album.media_uri.clone(),
        sources::SourceEntityKind::Album,
    )
    .await
    .unwrap();
    {
        editor
            .save(
                &database,
                (sources::MetadataEdit {
                    kind: sources::SourceEntityKind::Album,
                    changes: [("title".into(), "After".into())].into(),
                    artwork: None,
                })
                .clone(),
            )
            .await
            .unwrap();
    }
    assert!(updates.try_recv().expect("catalog update").1.metadata);
    let current =
        library::CatalogQuery::uris(library::CatalogKind::Track, vec![(&uri).to_string()])
            .rows(&database, 0, 1, &ReadCancellation::new())
            .await
            .map(|rows| match rows {
                library::CatalogRows::Tracks(mut rows) => rows.pop(),
                _ => unreachable!("typed catalog projection"),
            })
            .unwrap()
            .unwrap();
    assert_eq!(current.track_key, track.track_key);
    assert_eq!(current.metadata.album, "After");
    let (editor, _metadata) = sources::FileEditSession::open(
        Some(Arc::clone(&source)),
        &database,
        uri.to_string(),
        sources::SourceEntityKind::Track,
    )
    .await
    .unwrap();
    {
        editor
            .save(
                &database,
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
        updates.try_recv().is_err(),
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
            Some(2001),
        ),
        ("", "2004-05-06", "", Some("2004-05-06"), Some(2004)),
        ("", "", "1995-07-08", Some("1995-07-08"), Some(1995)),
        ("", "", "", None, None),
    ] {
        let album = {
            let mut query = library::CatalogQuery::keys(
                library::CatalogKind::Album,
                Some(initial.0),
                (&[current.links.album_key.unwrap()])
                    .iter()
                    .map(|key| key.raw())
                    .collect(),
            );
            query.folder = None;
            query
                .rows(
                    &database,
                    0,
                    (&[current.links.album_key.unwrap()]).len(),
                    &ReadCancellation::new(),
                )
                .await
                .map(|rows| match rows {
                    library::CatalogRows::Albums(rows) => rows,
                    _ => unreachable!("typed catalog projection"),
                })
        }
        .unwrap()
        .remove(0);
        let (editor, _metadata) = sources::FileEditSession::open(
            Some(Arc::clone(&source)),
            &database,
            album.media_uri.clone(),
            sources::SourceEntityKind::Album,
        )
        .await
        .unwrap();
        {
            editor
                .save(
                    &database,
                    (sources::MetadataEdit {
                        kind: sources::SourceEntityKind::Album,
                        changes: [
                            ("recording_date".to_string(), date.to_string()),
                            ("release_date".to_string(), release.to_string()),
                            ("original_release_date".to_string(), original.to_string()),
                        ]
                        .into(),
                        artwork: None,
                    })
                    .clone(),
                )
                .await
                .unwrap();
        }
        let album = {
            let mut query = library::CatalogQuery::keys(
                library::CatalogKind::Album,
                Some(initial.0),
                (&[current.links.album_key.unwrap()])
                    .iter()
                    .map(|key| key.raw())
                    .collect(),
            );
            query.folder = None;
            query
                .rows(
                    &database,
                    0,
                    (&[current.links.album_key.unwrap()]).len(),
                    &ReadCancellation::new(),
                )
                .await
                .map(|rows| match rows {
                    library::CatalogRows::Albums(rows) => rows,
                    _ => unreachable!("typed catalog projection"),
                })
        }
        .unwrap()
        .remove(0);
        for path in [&path, &second_path] {
            let uri = url::Url::from_file_path(path).unwrap().to_string();
            let (_editor, metadata) = sources::FileEditSession::open(
                Some(Arc::clone(&source)),
                &database,
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
            let track =
                library::CatalogQuery::uris(library::CatalogKind::Track, vec![(&uri).to_string()])
                    .rows(&database, 0, 1, &ReadCancellation::new())
                    .await
                    .map(|rows| match rows {
                        library::CatalogRows::Tracks(mut rows) => rows.pop(),
                        _ => unreachable!("typed catalog projection"),
                    })
                    .unwrap()
                    .unwrap();
            assert_eq!(track.metadata.release_date.as_deref(), expected);
            assert_eq!(track.metadata.year, year);
        }
        assert_eq!(album.metadata.release_date.as_deref(), expected);
        assert_eq!(album.metadata.year, year);
        for descending in [false, true] {
            let albums = library::CatalogQuery {
                subject: library::CatalogSubject::Albums {
                    source: initial.0,
                    sort: library::CatalogSort::ReleaseDate,
                    inline_tracks: None,
                },
                folder: None,
                filter: ("").to_string(),
                descending: descending,
                favorites_only: false,
                downloaded_only: false,
                activity: false,
            }
            .rows(&database, 0, 10, &ReadCancellation::new())
            .await
            .map(|rows| match rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!(),
            })
            .unwrap();
            let titles = albums
                .iter()
                .map(|row| row.metadata.title.as_str())
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
    let database = Database::open(store.path().join("library.sqlite"))
        .await
        .expect("Database");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
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
    database
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .expect("scan");
    let initial = updates.try_recv().expect("catalog update");
    assert_eq!(track_count(&database, &initial).await, 1);

    fs::write(&media, b"temporarily unreadable media content").expect("damage backing media");
    source
        .apply_local_change(
            &database,
            initial.0,
            LocalLiveChange::Paths {
                paths: vec![cue.clone()],
                rename: None,
            },
        )
        .await
        .expect("transient CUE change");
    assert!(
        updates.try_recv().is_err(),
        "unreadable backing preserves accepted content without invalidation"
    );
    assert_eq!(track_count(&database, &initial).await, 1);

    fs::write(&cue, "this is not a cue sheet").expect("reject CUE");
    source
        .apply_local_change(
            &database,
            initial.0,
            LocalLiveChange::Paths {
                paths: vec![cue],
                rename: None,
            },
        )
        .await
        .expect("rejected CUE change");
    let rejected = updates.try_recv().expect("catalog update");
    assert_eq!(track_count(&database, &rejected).await, 0);
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
    let database = Database::open(&database_path).await.expect("Database");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
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
    database
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .expect("scan");
    let initial = updates.try_recv().expect("catalog update");
    let albums = library::CatalogQuery {
        subject: library::CatalogSubject::Albums {
            source: initial.0,
            sort: library::CatalogSort::Title,
            inline_tracks: None,
        },
        folder: None,
        filter: ("").to_string(),
        descending: false,
        favorites_only: false,
        downloaded_only: false,
        activity: false,
    }
    .page(
        &database,
        library::RouteSeedWindow::top(),
        &ReadCancellation::new(),
    )
    .await
    .map(|page| match page.rows {
        library::CatalogRows::Albums(rows) => (page.count, page.first_row_position, rows),
        _ => unreachable!(),
    })
    .expect("Album order")
    .2
    .into_iter()
    .map(|row| row.album_key)
    .collect::<Vec<_>>();
    assert_eq!(albums.len(), 1);
    assert_eq!(
        album_genres(&database, &initial, albums[0]).await,
        ["Rock", "Jazz"]
    );

    let mut stored = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&database_path),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE local_files SET parse_version=16,parsed_stamp=json_set(parsed_stamp,'$[3]',16) WHERE kind IN ('media','cue')")
        .execute(&mut stored)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET metadata=jsonb_remove(metadata,'$.album_tags')")
        .execute(&mut stored)
        .await
        .unwrap();
    stored.close().await.unwrap();
    set_genre(&first, "Folk").expect("change a Track from the old cache");
    source
        .acquire_catalog(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .expect("refresh the changed album and its old cached sibling");
    let changed = updates.try_recv().expect("catalog update");
    assert_eq!(
        album_genres(&database, &changed, albums[0]).await,
        ["Folk", "Jazz"]
    );

    set_genre(&first, "Electronic").expect("change one Track genre");
    source
        .apply_local_change(
            &database,
            initial.0,
            LocalLiveChange::Paths {
                paths: vec![first],
                rename: None,
            },
        )
        .await
        .expect("exact Track change");
    let changed = updates.try_recv().expect("catalog update");
    assert_eq!(
        album_genres(&database, &changed, albums[0]).await,
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
    let database = Database::open(&database_path).await.expect("Database");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source_key = identity.unwrap().1;
        let _ = sender.send((source_key, update));
        std::future::ready(())
    });
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
    database
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    source
        .acquire_catalog(
            &database,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .expect("scan");
    let initial = updates.try_recv().expect("catalog update");
    let order = database
        .selected_track_uris(
            &CatalogQuery::tracks(TrackScope::Tracks(initial.0)).selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .expect("Track order");
    let object_id = library::CatalogQuery::uris(library::CatalogKind::Track, (&order).to_vec())
        .rows(&database, 0, (&order).len(), &ReadCancellation::new())
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!("typed catalog projection"),
        })
        .expect("Track row")[0]
        .object_id
        .clone();
    let media = database
        .local_file_page(initial.0, None, 128, &ReadCancellation::new())
        .await
        .expect("Local observations")
        .into_iter()
        .find(|file| file.kind == library::LocalFileKind::Media)
        .expect("media observation");
    let mut catalog = database
        .catalog(configuration.source_id.as_str(), None)
        .await
        .unwrap();
    catalog
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
    assert!(!catalog.changes().metadata);
    fs::rename(&old, &new).expect("rename file");
    source
        .apply_local_change(
            &database,
            initial.0,
            LocalLiveChange::Paths {
                paths: vec![old.clone(), new.clone()],
                rename: Some((old, new)),
            },
        )
        .await
        .expect("exact rename");
    let changed = updates.try_recv().expect("catalog update");
    let changed_order = database
        .selected_track_uris(
            &CatalogQuery::tracks(TrackScope::Tracks(changed.0)).selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .expect("changed Track order");
    let uri = &changed_order[0];
    let source = Arc::new(source);
    let (editor, _metadata) = sources::FileEditSession::open(
        Some(Arc::clone(&source)),
        &database,
        uri.to_string(),
        sources::SourceEntityKind::Track,
    )
    .await
    .unwrap();
    {
        editor
            .save(
                &database,
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
        library::CatalogQuery::uris(library::CatalogKind::Track, vec![(uri).to_owned()])
            .rows(&database, 0, 256, &ReadCancellation::new())
            .await
            .map(|rows| match rows {
                library::CatalogRows::Tracks(rows) => rows.into_iter().next(),
                _ => unreachable!(),
            })
            .unwrap()
            .unwrap()
            .object_id,
        object_id
    );
}

#[tokio::test]
async fn initial_local_walk_reports_real_total_free_file_progress() {
    let root = tempfile::tempdir().expect("music root");
    write_silent_wav(&root.path().join("first.wav"), 1).expect("first WAV");
    write_silent_wav(&root.path().join("second.wav"), 1).expect("second WAV");
    let store = tempfile::tempdir().expect("Store");
    let database = Database::open(store.path().join("library.sqlite"))
        .await
        .expect("Database");
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
    database
        .reconcile_source(
            &configuration.source_id,
            &configuration.kind,
            &configuration.name,
        )
        .await
        .unwrap();
    let progress = Mutex::new(Vec::new());
    source
        .acquire_catalog(
            &database,
            &|update| progress.lock().expect("progress lock").push(update),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            true,
        )
        .await
        .expect("scan");
    let progress = progress.into_inner().expect("progress");
    let first_files = progress
        .iter()
        .position(|update| update.stage == sources::SourceReadStage::Files)
        .expect("Files progress");
    let first_tracks = progress
        .iter()
        .position(|update| update.stage == sources::SourceReadStage::Tracks)
        .expect("Tracks progress");
    assert!(first_files < first_tracks);
    assert_eq!(progress[first_files].completed, 2);
    assert_eq!(progress[first_files].total, None);
    let artwork = progress
        .iter()
        .filter(|update| update.stage == sources::SourceReadStage::Artwork)
        .collect::<Vec<_>>();
    assert_eq!(artwork.first().expect("artwork started").completed, 0);
    let finished = artwork.last().expect("artwork finished");
    assert!(finished.completed > 0);
    assert_eq!(finished.total, Some(finished.completed));
    assert!(artwork.iter().all(|update| update.total == finished.total));
    assert_eq!(
        progress
            .iter()
            .filter(|update| update.stage == sources::SourceReadStage::Tracks)
            .next_back()
            .expect("final Tracks progress")
            .completed,
        2
    );
}

async fn track_count(
    database: &Database,
    publication: &(library::SourceKey, library::CatalogUpdate),
) -> usize {
    (&CatalogQuery::tracks(TrackScope::Tracks(publication.0)))
        .count(&database, &ReadCancellation::new())
        .await
        .expect("Track count") as usize
}

async fn album_genres(
    database: &Database,
    publication: &(library::SourceKey, library::CatalogUpdate),
    album: library::AlbumKey,
) -> Vec<String> {
    {
        let mut query = library::CatalogQuery::keys(
            library::CatalogKind::Album,
            Some(publication.0),
            (&[album]).iter().map(|key| key.raw()).collect(),
        );
        query.folder = None;
        query
            .rows(&database, 0, (&[album]).len(), &ReadCancellation::new())
            .await
            .map(|rows| match rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!("typed catalog projection"),
            })
    }
    .expect("Album row")[0]
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
