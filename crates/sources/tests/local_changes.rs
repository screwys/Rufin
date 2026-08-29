use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use library::{Database, ReadCancellation, ScanOutcome, TrackSort};
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;
use lofty::tag::{Tag, TagType};
use sources::{LocalFolderHostInput, LocalLiveChange, Source, SourceSetupInput};

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
    let connected = Source::connect(SourceSetupInput::Local(LocalFolderHostInput {
        roots: vec![root.path().to_path_buf()],
    }))
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    let first_publication = publication(
        source
            .manual_refresh(
                &database,
                &configuration.name,
                &|_| {},
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .expect("initial scan"),
    );
    let before = database
        .track_order(
            first_publication.source,
            None,
            false,
            TrackSort::Title,
            false,
            &ReadCancellation::new(),
        )
        .await
        .expect("before order");
    assert_eq!(before.len(), 2);

    write_silent_wav(&first, 2).expect("edit one WAV");
    let changed = source
        .apply_local_change(
            &database,
            first_publication.source,
            LocalLiveChange::Paths {
                paths: vec![first.clone()],
                rename: None,
            },
        )
        .await
        .expect("exact change")
        .expect("point outcome");
    let changed = publication(changed);
    assert_eq!(
        changed.catalog_revision,
        first_publication.catalog_revision + 1
    );
    let after = database
        .track_order(
            changed.source,
            None,
            false,
            TrackSort::Title,
            false,
            &ReadCancellation::new(),
        )
        .await
        .expect("after order");
    assert_eq!(after, before);
}

#[tokio::test]
async fn local_image_change_publishes_artwork_without_catalog_revision() {
    let root = tempfile::tempdir().expect("music root");
    let track = root.path().join("track.wav");
    let image = root.path().join("cover.jpg");
    write_silent_wav(&track, 1).expect("WAV");
    fs::write(&image, b"image-change").expect("image observation");
    let store = tempfile::tempdir().expect("Store");
    let database = Database::open(store.path().join("library.sqlite"))
        .await
        .expect("Database");
    let connected = Source::connect(SourceSetupInput::Local(LocalFolderHostInput {
        roots: vec![root.path().to_path_buf()],
    }))
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    let initial = publication(
        source
            .manual_refresh(
                &database,
                &configuration.name,
                &|_| {},
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .expect("scan"),
    );
    let album = database
        .album_route_page(
            initial.source,
            None,
            false,
            "",
            library::AlbumSort::Title,
            false,
            &ReadCancellation::new(),
        )
        .await
        .expect("Album order")
        .0[0];
    database
        .write_album_artwork_bindings(initial.source, &[(album, b"accepted-art".to_vec())])
        .await
        .expect("binding");
    fs::write(&image, b"changed-image").expect("change image");
    let outcome = source
        .apply_local_change(
            &database,
            initial.source,
            LocalLiveChange::Paths {
                paths: vec![image],
                rename: None,
            },
        )
        .await
        .expect("image change")
        .expect("point outcome");
    let ScanOutcome::ArtworkChanged(publication) = outcome else {
        panic!("image-only change must publish artwork identity");
    };
    assert_eq!(publication.catalog_revision, initial.catalog_revision);
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
    assert_eq!(
        database
            .album_rows(initial.source, &[album], None, &ReadCancellation::new())
            .await
            .expect("Album row")[0]
            .artwork_binding,
        None
    );
}

#[tokio::test]
async fn restart_catch_up_detects_an_in_place_file_edit() {
    let root = tempfile::tempdir().expect("music root");
    let track = root.path().join("track.wav");
    write_silent_wav(&track, 1).expect("WAV");
    let store = tempfile::tempdir().expect("Store");
    let database = Database::open(store.path().join("library.sqlite"))
        .await
        .expect("Database");
    let connected = Source::connect(SourceSetupInput::Local(LocalFolderHostInput {
        roots: vec![root.path().to_path_buf()],
    }))
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    let initial = publication(
        source
            .manual_refresh(
                &database,
                &configuration.name,
                &|_| {},
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .expect("scan"),
    );
    let order = database
        .track_order(
            initial.source,
            None,
            false,
            TrackSort::Title,
            false,
            &ReadCancellation::new(),
        )
        .await
        .expect("Track order");
    assert_eq!(
        database
            .track_rows(initial.source, &order, &ReadCancellation::new())
            .await
            .expect("initial Track row")[0]
            .duration_millis,
        1_000
    );

    write_silent_wav(&track, 2).expect("edit WAV in place");
    let changed = publication(
        source
            .catch_up_local(
                &database,
                initial.source,
                &|_| {},
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .expect("restart catch-up"),
    );
    assert_eq!(changed.catalog_revision, initial.catalog_revision + 1);
    assert_eq!(
        database
            .track_rows(changed.source, &order, &ReadCancellation::new())
            .await
            .expect("changed Track row")[0]
            .duration_millis,
        2_000
    );
}

#[tokio::test]
async fn restart_catch_up_accepts_file_bookkeeping_without_catalog_change() {
    let root = tempfile::tempdir().expect("music root");
    let track = root.path().join("track.wav");
    write_silent_wav(&track, 1).expect("WAV");
    let store = tempfile::tempdir().expect("Store");
    let database = Database::open(store.path().join("library.sqlite"))
        .await
        .expect("Database");
    let connected = Source::connect(SourceSetupInput::Local(LocalFolderHostInput {
        roots: vec![root.path().to_path_buf()],
    }))
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    let initial = publication(
        source
            .manual_refresh(
                &database,
                &configuration.name,
                &|_| {},
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .expect("scan"),
    );

    fs::create_dir(root.path().join("empty")).expect("change only Local inventory bookkeeping");
    let outcome = source
        .catch_up_local(
            &database,
            initial.source,
            &|_| {},
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("restart catch-up");

    let ScanOutcome::Identical(publication) = outcome else {
        panic!("unchanged parsed music facts must not publish: {outcome:?}");
    };
    assert_eq!(publication.catalog_revision, initial.catalog_revision);
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
    let connected = Source::connect(SourceSetupInput::Local(LocalFolderHostInput {
        roots: vec![root.path().to_path_buf()],
    }))
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    let initial = publication(
        source
            .manual_refresh(
                &database,
                &configuration.name,
                &|_| {},
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .expect("scan"),
    );
    assert_eq!(track_count(&database, initial).await, 1);

    fs::write(&media, b"temporarily unreadable media content").expect("damage backing media");
    let retained = publication(
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
            .expect("transient CUE change")
            .expect("point outcome"),
    );
    assert_eq!(track_count(&database, retained).await, 1);

    fs::write(&cue, "this is not a cue sheet").expect("reject CUE");
    let rejected = publication(
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
            .expect("rejected CUE change")
            .expect("point outcome"),
    );
    assert_eq!(track_count(&database, rejected).await, 0);
}

#[tokio::test]
async fn exact_track_change_rebuilds_relations_from_the_complete_album() {
    let root = tempfile::tempdir().expect("music root");
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
    let connected = Source::connect(SourceSetupInput::Local(LocalFolderHostInput {
        roots: vec![root.path().to_path_buf()],
    }))
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    let initial = publication(
        source
            .manual_refresh(
                &database,
                &configuration.name,
                &|_| {},
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .expect("scan"),
    );
    let albums = database
        .album_route_page(
            initial.source,
            None,
            false,
            "",
            library::AlbumSort::Title,
            false,
            &ReadCancellation::new(),
        )
        .await
        .expect("Album order")
        .0;
    assert_eq!(albums.len(), 1);
    assert_eq!(
        album_genres(&database, initial, albums[0]).await,
        ["Rock", "Jazz"]
    );

    set_genre(&first, "Electronic").expect("change one Track genre");
    let changed = publication(
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
            .expect("exact Track change")
            .expect("point outcome"),
    );
    assert_eq!(
        album_genres(&database, changed, albums[0]).await,
        ["Electronic", "Jazz"]
    );
}

#[tokio::test]
async fn explicit_rename_preserves_track_identity_without_native_file_identity() {
    let root = tempfile::tempdir().expect("music root");
    let old = root.path().join("old.wav");
    let new = root.path().join("new.wav");
    write_tagged_wav(&old, "Track", "Artist", "Album", "Genre").expect("tagged WAV");
    let store = tempfile::tempdir().expect("Store");
    let database_path = store.path().join("library.sqlite");
    let database = Database::open(&database_path).await.expect("Database");
    let connected = Source::connect(SourceSetupInput::Local(LocalFolderHostInput {
        roots: vec![root.path().to_path_buf()],
    }))
    .await
    .expect("Local");
    let (configuration, source, _) = connected.into_parts();
    let initial = publication(
        source
            .manual_refresh(
                &database,
                &configuration.name,
                &|_| {},
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .expect("scan"),
    );
    let order = database
        .track_order(
            initial.source,
            None,
            false,
            TrackSort::Title,
            false,
            &ReadCancellation::new(),
        )
        .await
        .expect("Track order");
    let object_id = database
        .track_rows(initial.source, &order, &ReadCancellation::new())
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
            parse_version: media.parse_version,
            state: media.state,
        },
        media.dependencies,
    )])
    .await
    .expect("remove native identity enrichment");
    scan.finish_batch().await.expect("finish observation batch");
    scan.finish().await.expect("publish observation update");

    fs::rename(&old, &new).expect("rename file");
    let changed = publication(
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
            .expect("exact rename")
            .expect("point outcome"),
    );
    let changed_order = database
        .track_order(
            changed.source,
            None,
            false,
            TrackSort::Title,
            false,
            &ReadCancellation::new(),
        )
        .await
        .expect("changed Track order");
    assert_eq!(
        database
            .track_rows(changed.source, &changed_order, &ReadCancellation::new())
            .await
            .expect("changed Track row")[0]
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
    let connected = Source::connect(SourceSetupInput::Local(LocalFolderHostInput {
        roots: vec![root.path().to_path_buf()],
    }))
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

async fn track_count(database: &Database, publication: library::Publication) -> usize {
    database
        .track_order(
            publication.source,
            None,
            false,
            TrackSort::Title,
            false,
            &ReadCancellation::new(),
        )
        .await
        .expect("Track order")
        .len()
}

async fn album_genres(
    database: &Database,
    publication: library::Publication,
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

fn publication(outcome: ScanOutcome) -> library::Publication {
    match outcome {
        ScanOutcome::Changed(publication)
        | ScanOutcome::PlaylistsChanged(publication)
        | ScanOutcome::ArtworkChanged(publication)
        | ScanOutcome::Identical(publication) => publication,
        other => panic!("unexpected outcome: {other:?}"),
    }
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
