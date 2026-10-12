use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use library::{Database, ReadCancellation, SongQuery, SongScope};
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;
use lofty::tag::{Tag, TagType};
use sources::{LocalFolderHostInput, LocalLiveChange, Source, SourceId, SourceSetupInput};

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
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    let (configuration, source, _) = Source::connect(
        SourceId::new("test-source"),
        SourceSetupInput::Local(LocalFolderHostInput {
            roots: vec![root.clone()],
        }),
    )
    .await
    .unwrap()
    .into_parts();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    fs::rename(&root, &offline).unwrap();
    assert!(matches!(
        source
            .manual_refresh(&database, &configuration.name, &|_| {}, cancelled.clone())
            .await,
        Err(sources::SourceError::Other(_))
    ));
    fs::rename(&offline, &root).unwrap();
    source
        .manual_refresh(&database, &configuration.name, &|_| {}, cancelled.clone())
        .await
        .unwrap();
    let initial = updates.try_recv().expect("catalog update");
    assert!(initial.metadata);
    fs::rename(&root, &offline).unwrap();
    assert!(
        source
            .manual_refresh(&database, &configuration.name, &|_| {}, cancelled.clone())
            .await
            .is_err()
    );
    assert!(
        source
            .catch_up_local(&database, initial.source, &|_| {}, cancelled.clone())
            .await
            .is_err()
    );
    assert!(
        source
            .apply_local_change(
                &database,
                initial.source,
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
        .manual_refresh(&database, &configuration.name, &|_| {}, cancelled)
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
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetupInput::Local(LocalFolderHostInput {
            roots: vec![root.path().to_path_buf()],
        }),
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    source
        .manual_refresh(
            &database,
            &configuration.name,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("initial scan");
    let first_publication = updates.try_recv().expect("catalog update");
    let before = database
        .selected_track_uris(
            &SongQuery::new(SongScope::Tracks(first_publication.source)).selection(),
            &ReadCancellation::new(),
        )
        .await
        .expect("before order");
    assert_eq!(before.len(), 2);

    write_silent_wav(&second, 2).expect("edit second WAV");
    source
        .apply_local_change(
            &database,
            first_publication.source,
            LocalLiveChange::Paths {
                paths: vec![second.clone()],
                rename: None,
            },
        )
        .await
        .expect("exact change");
    let changed = updates.try_recv().expect("catalog update");
    assert!(changed.metadata);
    let after = database
        .selected_track_uris(
            &SongQuery::new(SongScope::Tracks(changed.source)).selection(),
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
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetupInput::Local(LocalFolderHostInput {
            roots: vec![root.path().to_path_buf()],
        }),
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    source
        .manual_refresh(
            &database,
            &configuration.name,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("scan");
    let initial = updates.try_recv().expect("catalog update");
    let album = database
        .album_route_page(
            initial.source,
            None,
            false,
            "",
            library::AlbumSort::Title,
            false,
            library::RouteSeedWindow::top(),
            &ReadCancellation::new(),
        )
        .await
        .expect("Album order")
        .2[0]
        .album_key;
    let before = database
        .album_rows(initial.source, &[album], None, &ReadCancellation::new())
        .await
        .expect("initial Album artwork")[0]
        .artwork_binding
        .clone();
    assert!(before.is_some());
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = Arc::clone(&cancelled);
    let interrupted = source
        .manual_refresh(
            &database,
            &configuration.name,
            &move |progress| {
                if progress.stage == sources::SourceReadStage::Tracks {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            },
            cancelled,
        )
        .await;
    assert!(matches!(interrupted, Err(sources::SourceError::Cancelled)));
    assert_eq!(
        database
            .album_rows(initial.source, &[album], None, &ReadCancellation::new())
            .await
            .expect("Album cover after cancelled discovery")[0]
            .artwork_binding,
        before
    );
    let interrupted_update = updates.try_recv().expect("interrupted catalog update");
    assert!(!interrupted_update.metadata && !interrupted_update.artwork);
    fs::write(&image, b"changed-image").expect("change image");
    source
        .apply_local_change(
            &database,
            initial.source,
            LocalLiveChange::Paths {
                paths: vec![image],
                rename: None,
            },
        )
        .await
        .expect("image change");
    let publication = updates.try_recv().expect("catalog update");
    assert!(publication.artwork);
    assert!(!publication.metadata && !publication.playlists && !publication.home);
    assert_eq!(
        database
            .cached_source(
                &configuration.source_id.to_string(),
                &ReadCancellation::new()
            )
            .await
            .expect("cached Local source")
            .expect("Local source")
            .artwork_digest,
        publication.artwork_digest
    );
    assert_ne!(
        database
            .album_rows(initial.source, &[album], None, &ReadCancellation::new())
            .await
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
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetupInput::Local(LocalFolderHostInput {
            roots: vec![root.path().to_path_buf()],
        }),
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    source
        .manual_refresh(
            &database,
            &configuration.name,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("scan");
    let initial = updates.try_recv().expect("catalog update");
    let order = database
        .selected_track_uris(
            &SongQuery::new(SongScope::Tracks(initial.source)).selection(),
            &ReadCancellation::new(),
        )
        .await
        .expect("Track order");
    assert_eq!(
        database
            .track_rows_by_uri(&order, &ReadCancellation::new())
            .await
            .expect("initial Track row")[0]
            .duration_millis,
        1_000
    );

    database.close().await.expect("close before file edit");
    write_silent_wav(&track, 2).expect("edit WAV in place");
    let database = Database::open(&database_path)
        .await
        .expect("reopen after edit");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    source
        .catch_up_local(
            &database,
            initial.source,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("restart catch-up");
    let changed = updates.try_recv().expect("catalog update");
    assert!(changed.metadata);
    assert_eq!(
        database
            .track_rows_by_uri(&order, &ReadCancellation::new())
            .await
            .expect("changed Track row")[0]
            .duration_millis,
        2_000
    );
    database.close().await.expect("close before file removal");
    fs::remove_file(&track).expect("remove cached file while stopped");
    let database = Database::open(&database_path)
        .await
        .expect("reopen after removal");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    source
        .catch_up_local(
            &database,
            initial.source,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("remove missing file during catch-up");
    let removed = updates.try_recv().expect("catalog update");
    assert_eq!(
        database
            .source_counts(removed.source, &ReadCancellation::new())
            .await
            .unwrap()
            .1,
        0
    );
}

#[tokio::test]
async fn restart_catch_up_accepts_file_bookkeeping_without_catalog_change() {
    let root = tempfile::tempdir().expect("music root");
    let track = root.path().join("track.wav");
    write_silent_wav(&track, 1).expect("WAV");
    let store = tempfile::tempdir().expect("Store");
    let database_path = store.path().join("library.sqlite");
    let database = Database::open(&database_path).await.expect("Database");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetupInput::Local(LocalFolderHostInput {
            roots: vec![root.path().to_path_buf()],
        }),
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    source
        .manual_refresh(
            &database,
            &configuration.name,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("scan");
    let initial = updates.try_recv().expect("catalog update");

    fs::create_dir(root.path().join("empty")).expect("change only Local inventory bookkeeping");
    source
        .catch_up_local(
            &database,
            initial.source,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("restart catch-up");

    let update = updates.try_recv().expect("catalog update");
    assert!(!update.changed());

    for index in 0..130 {
        fs::create_dir_all(root.path().join(format!("pending-{index:03}/nested"))).unwrap();
    }
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = Arc::clone(&cancelled);
    assert!(matches!(
        source
            .manual_refresh(
                &database,
                &configuration.name,
                &move |progress| {
                    if progress.stage == sources::SourceReadStage::Files {
                        stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                },
                cancelled,
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
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    source
        .catch_up_local(
            &database,
            initial.source,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(!updates.try_recv().unwrap().changed());
    let mut files = Vec::new();
    loop {
        let page = database
            .local_file_page(
                initial.source,
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
    source
        .catch_up_local(
            &database,
            initial.source,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(!updates.try_recv().unwrap().changed());
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
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetupInput::Local(LocalFolderHostInput {
            roots: vec![root.path().to_path_buf()],
        }),
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    source
        .manual_refresh(
            &database,
            &configuration.name,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("scan");
    let initial = updates.try_recv().expect("catalog update");
    let albums = database
        .album_route_page(
            initial.source,
            None,
            false,
            "",
            library::AlbumSort::Title,
            false,
            library::RouteSeedWindow::top(),
            &ReadCancellation::new(),
        )
        .await
        .expect("Album order")
        .2;
    assert_eq!(albums.len(), 2);
    write_silent_wav(&media, 3).expect("edit CUE backing media");
    source
        .catch_up_local(
            &database,
            initial.source,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("first catch-up");
    let changed = updates.try_recv().expect("catalog update");
    source
        .catch_up_local(
            &database,
            changed.source,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("second catch-up");

    let update = updates.try_recv().expect("catalog update");
    assert!(!update.changed());
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
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    let connected = Source::connect(
        SourceId::new("local-art"),
        SourceSetupInput::Local(LocalFolderHostInput { roots: vec![root] }),
    )
    .await
    .unwrap();
    let (configuration, source, _) = connected.into_parts();
    source
        .manual_refresh(
            &database,
            &configuration.name,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
    let row = database
        .track_row_by_uri(&uri, &ReadCancellation::new())
        .await
        .unwrap()
        .unwrap();
    let binding: sources::LocalImageRef =
        serde_json::from_slice(row.artwork_binding.as_ref().unwrap()).unwrap();
    assert_eq!(binding.source_id(), source.source_id());
    assert!(
        matches!(binding, sources::LocalImageRef::File { path, .. } if path == cover.canonicalize().unwrap().to_string_lossy())
    );
    source
        .manual_refresh(
            &database,
            &configuration.name,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    let update = updates.try_recv().expect("catalog update");
    assert!(!update.changed());
    fs::write(&cover, b"changed image bytes").unwrap();
    source
        .apply_local_change(
            &database,
            first.source,
            LocalLiveChange::Paths {
                paths: vec![cover],
                rename: None,
            },
        )
        .await
        .unwrap();
    let update = updates.try_recv().expect("catalog update");
    assert!(update.artwork && !update.metadata && !update.playlists && !update.home);
    let changed = database
        .track_row_by_uri(&uri, &ReadCancellation::new())
        .await
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
        SourceSetupInput::Local(LocalFolderHostInput {
            roots: vec![root.path().to_path_buf()],
        }),
    )
    .await
    .unwrap();
    let (configuration, source, _) = connected.into_parts();
    let mut expected_group = None;
    for distinct in [false, true, false] {
        database.set_distinct_track_covers(distinct);
        source
            .manual_refresh(
                &database,
                &configuration.name,
                &|_| {},
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        for (index, path) in paths.iter().enumerate() {
            let uri = url::Url::from_file_path(path).unwrap().to_string();
            let row = database
                .track_row_by_uri(&uri, &ReadCancellation::new())
                .await
                .unwrap()
                .unwrap();
            let bytes = row.artwork_binding.unwrap();
            let binding: sources::LocalImageRef = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(binding.source_id(), source.source_id());
            let expected = if distinct && index == 2 {
                &paths[2]
            } else {
                &paths[1]
            };
            assert!(
                matches!(&binding, sources::LocalImageRef::Embedded { path, .. } if path == &expected.to_string_lossy()),
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
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    let connected = Source::connect(
        SourceId::new("local-art"),
        SourceSetupInput::Local(LocalFolderHostInput {
            roots: vec![root.path().to_path_buf()],
        }),
    )
    .await
    .unwrap();
    let (configuration, source, _) = connected.into_parts();
    source
        .manual_refresh(
            &database,
            &configuration.name,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    let initial = updates.try_recv().expect("catalog update");
    let artist = database
        .artist_route_page(
            initial.source,
            None,
            false,
            false,
            "",
            library::ArtistSort::Title,
            false,
            library::RouteSeedWindow::top(),
            &ReadCancellation::new(),
        )
        .await
        .unwrap()
        .2
        .remove(0);
    for changed in [&covers[1], &covers[0]] {
        fs::write(changed, b"replacement artwork").unwrap();
        source
            .apply_local_change(
                &database,
                initial.source,
                LocalLiveChange::Paths {
                    paths: vec![changed.clone()],
                    rename: None,
                },
            )
            .await
            .unwrap();
        let update = updates.try_recv().expect("catalog update");
        assert!(update.artwork && !update.metadata && !update.playlists && !update.home);
        let current = database
            .artist_row_by_media_uri(&artist.media_uri, &ReadCancellation::new())
            .await
            .unwrap()
            .unwrap();
        let artwork: library::ArtistArtworkBinding =
            serde_json::from_slice(current.artwork_binding.as_ref().unwrap()).unwrap();
        let binding: sources::LocalImageRef =
            serde_json::from_slice(artwork.fallback.as_ref().unwrap()).unwrap();
        assert_eq!(binding.source_id(), source.source_id());
        assert!(
            matches!(binding, sources::LocalImageRef::File { path, .. } if path == covers[0].to_string_lossy())
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
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    let connected = Source::connect(
        SourceId::new("local-art"),
        SourceSetupInput::Local(LocalFolderHostInput {
            roots: vec![root.path().to_path_buf()],
        }),
    )
    .await
    .unwrap();
    let (configuration, source, _) = connected.into_parts();
    source
        .manual_refresh(
            &database,
            &configuration.name,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    let initial = updates.try_recv().expect("catalog update");
    let borrower_uri = url::Url::from_file_path(&paths[1]).unwrap().to_string();
    let before = database
        .track_row_by_uri(&borrower_uri, &ReadCancellation::new())
        .await
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
                initial.source,
                LocalLiveChange::Paths {
                    paths: vec![cover.clone()],
                    rename: None,
                },
            )
            .await
            .unwrap();
        let update = updates.try_recv().expect("catalog update");
        assert!(update.artwork && !update.metadata && !update.playlists && !update.home);
        let borrower = database
            .track_row_by_uri(&borrower_uri, &ReadCancellation::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(borrower.track_key, before.track_key);
        assert_ne!(borrower.artwork_binding, before.artwork_binding);
        assert_eq!(borrower.artwork_binding.is_none(), remove);
        for path in &paths {
            let uri = url::Url::from_file_path(path).unwrap().to_string();
            assert_eq!(
                database
                    .track_row_by_uri(&uri, &ReadCancellation::new())
                    .await
                    .unwrap()
                    .unwrap()
                    .artwork_binding,
                borrower.artwork_binding
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
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    let connected = Source::connect(
        SourceId::new("local-metadata"),
        SourceSetupInput::Local(LocalFolderHostInput {
            roots: vec![root.path().to_path_buf()],
        }),
    )
    .await
    .unwrap();
    let (configuration, source, _) = connected.into_parts();
    source
        .manual_refresh(
            &database,
            &configuration.name,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    let initial = updates.try_recv().expect("catalog update");
    let uri = url::Url::from_file_path(&path).unwrap().to_string();
    let track = database
        .track_row_by_uri(&uri, &ReadCancellation::new())
        .await
        .unwrap()
        .unwrap();
    let album = database
        .album_rows(
            initial.source,
            &[track.album_key.unwrap()],
            None,
            &ReadCancellation::new(),
        )
        .await
        .unwrap()
        .remove(0);
    let mut metadata = source
        .read_album_metadata(&database, &album.media_uri)
        .await
        .unwrap();
    metadata.values.title = "After".to_string();
    source
        .write_metadata(
            &database,
            &album.media_uri,
            metadata.revision.as_deref().unwrap(),
            None,
            sources::MetadataEdit::Album(sources::AlbumMetadataEdit {
                extra: Default::default(),
                artwork: None,
                values: metadata.values,
                changed: sources::AlbumMetadataWritable {
                    title: true,
                    ..Default::default()
                },
            }),
        )
        .await
        .unwrap();
    assert!(updates.try_recv().expect("catalog update").metadata);
    let current = database
        .track_row_by_uri(&uri, &ReadCancellation::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.track_key, track.track_key);
    assert_eq!(current.album, "After");
    let metadata = source.read_track_metadata(&database, &uri).await.unwrap();
    source
        .write_metadata(
            &database,
            &uri,
            metadata.revision.as_deref().unwrap(),
            None,
            sources::MetadataEdit::Track(sources::TrackMetadataEdit {
                extra: Default::default(),
                artwork: None,
                values: metadata.values,
                changed: Default::default(),
            }),
        )
        .await
        .unwrap();
    let update = updates.try_recv().expect("catalog update");
    assert!(!update.changed());
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
        let album = database
            .album_rows(
                initial.source,
                &[current.album_key.unwrap()],
                None,
                &ReadCancellation::new(),
            )
            .await
            .unwrap()
            .remove(0);
        let metadata = source
            .read_album_metadata(&database, &album.media_uri)
            .await
            .unwrap();
        source
            .write_metadata(
                &database,
                &album.media_uri,
                metadata.revision.as_deref().unwrap(),
                None,
                sources::MetadataEdit::Album(sources::AlbumMetadataEdit {
                    extra: [
                        ("recording_date".to_string(), date.to_string()),
                        ("release_date".to_string(), release.to_string()),
                        ("original_release_date".to_string(), original.to_string()),
                    ]
                    .into(),
                    artwork: None,
                    values: metadata.values,
                    changed: Default::default(),
                }),
            )
            .await
            .unwrap();
        let album = database
            .album_rows(
                initial.source,
                &[current.album_key.unwrap()],
                None,
                &ReadCancellation::new(),
            )
            .await
            .unwrap()
            .remove(0);
        for path in [&path, &second_path] {
            let uri = url::Url::from_file_path(path).unwrap().to_string();
            let metadata = source.read_track_metadata(&database, &uri).await.unwrap();
            for (key, value) in [
                ("recording_date", date),
                ("release_date", release),
                ("original_release_date", original),
            ] {
                assert_eq!(
                    metadata
                        .extra
                        .iter()
                        .find(|field| field.key == key)
                        .unwrap()
                        .value,
                    value
                );
            }
            let track = database
                .track_row_by_uri(&uri, &ReadCancellation::new())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(track.release_date.as_deref(), expected);
            assert_eq!(track.year, year);
        }
        assert_eq!(album.release_date.as_deref(), expected);
        assert_eq!(album.year, year);
        for descending in [false, true] {
            let albums = database
                .album_page(
                    initial.source,
                    None,
                    false,
                    "",
                    library::AlbumSort::ReleaseDate,
                    descending,
                    0,
                    10,
                    &ReadCancellation::new(),
                )
                .await
                .unwrap();
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
    let database = Database::open(store.path().join("library.sqlite"))
        .await
        .expect("Database");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetupInput::Local(LocalFolderHostInput {
            roots: vec![root.path().to_path_buf()],
        }),
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    source
        .manual_refresh(
            &database,
            &configuration.name,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("scan");
    let initial = updates.try_recv().expect("catalog update");
    assert_eq!(track_count(&database, &initial).await, 1);

    fs::write(&media, b"temporarily unreadable media content").expect("damage backing media");
    source
        .apply_local_change(
            &database,
            initial.source,
            LocalLiveChange::Paths {
                paths: vec![cue.clone()],
                rename: None,
            },
        )
        .await
        .expect("transient CUE change");
    let retained = updates.try_recv().expect("catalog update");
    assert_eq!(track_count(&database, &retained).await, 1);

    fs::write(&cue, "this is not a cue sheet").expect("reject CUE");
    source
        .apply_local_change(
            &database,
            retained.source,
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
    let database = Database::open(store.path().join("library.sqlite"))
        .await
        .expect("Database");
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetupInput::Local(LocalFolderHostInput {
            roots: vec![root.path().to_path_buf()],
        }),
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    source
        .manual_refresh(
            &database,
            &configuration.name,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("scan");
    let initial = updates.try_recv().expect("catalog update");
    let albums = database
        .album_route_page(
            initial.source,
            None,
            false,
            "",
            library::AlbumSort::Title,
            false,
            library::RouteSeedWindow::top(),
            &ReadCancellation::new(),
        )
        .await
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

    set_genre(&first, "Electronic").expect("change one Track genre");
    source
        .apply_local_change(
            &database,
            initial.source,
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
    database.observe_catalog(move |update| {
        let _ = sender.send(update);
        std::future::ready(())
    });
    let connected = Source::connect(
        SourceId::new("test-source"),
        SourceSetupInput::Local(LocalFolderHostInput {
            roots: vec![root.path().to_path_buf()],
        }),
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    source
        .manual_refresh(
            &database,
            &configuration.name,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("scan");
    let initial = updates.try_recv().expect("catalog update");
    let order = database
        .selected_track_uris(
            &SongQuery::new(SongScope::Tracks(initial.source)).selection(),
            &ReadCancellation::new(),
        )
        .await
        .expect("Track order");
    let object_id = database
        .track_rows_by_uri(&order, &ReadCancellation::new())
        .await
        .expect("Track row")[0]
        .object_id
        .clone();
    let media = database
        .local_file_page(initial.source, None, 128, &ReadCancellation::new())
        .await
        .expect("Local observations")
        .into_iter()
        .find(|file| file.kind == library::LocalFileKind::Media)
        .expect("media observation");
    let mut scan = library::Scan::begin_items(&database, configuration.source_id.as_str())
        .await
        .expect("begin Local observation update");
    scan.begin_batch().await.expect("begin observation batch");
    scan.write_local_files(&[(
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
    )])
    .await
    .expect("remove native identity enrichment");
    scan.finish_batch().await.expect("finish observation batch");
    scan.finish().await.expect("publish observation update");
    let observation = updates.try_recv().expect("observation update");
    assert!(!observation.metadata);

    fs::rename(&old, &new).expect("rename file");
    source
        .apply_local_change(
            &database,
            initial.source,
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
            &SongQuery::new(SongScope::Tracks(changed.source)).selection(),
            &ReadCancellation::new(),
        )
        .await
        .expect("changed Track order");
    assert_eq!(
        database
            .track_rows_by_uri(&changed_order, &ReadCancellation::new())
            .await
            .expect("changed Track row")[0]
            .object_id,
        object_id
    );
    let uri = &changed_order[0];
    let mut metadata = source.read_track_metadata(&database, uri).await.unwrap();
    metadata.values.title = "After rename".into();
    source
        .write_metadata(
            &database,
            uri,
            metadata.revision.as_deref().unwrap(),
            None,
            sources::MetadataEdit::Track(sources::TrackMetadataEdit {
                extra: Default::default(),
                artwork: None,
                values: metadata.values,
                changed: sources::TrackMetadataWritable {
                    title: true,
                    ..Default::default()
                },
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        database
            .track_row_by_uri(uri, &ReadCancellation::new())
            .await
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
        SourceSetupInput::Local(LocalFolderHostInput {
            roots: vec![root.path().to_path_buf()],
        }),
    )
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    let progress = Mutex::new(Vec::new());
    source
        .manual_refresh(
            &database,
            &configuration.name,
            &|update| progress.lock().expect("progress lock").push(update),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
    assert!(progress[first_files].completed >= 3);
    assert_eq!(progress[first_files].total, None);
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

async fn track_count(database: &Database, publication: &library::CatalogUpdate) -> usize {
    database
        .song_count(
            &SongQuery::new(SongScope::Tracks(publication.source)),
            &ReadCancellation::new(),
        )
        .await
        .expect("Track count") as usize
}

async fn album_genres(
    database: &Database,
    publication: &library::CatalogUpdate,
    album: library::AlbumKey,
) -> Vec<String> {
    database
        .album_rows(publication.source, &[album], None, &ReadCancellation::new())
        .await
        .expect("Album row")[0]
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
