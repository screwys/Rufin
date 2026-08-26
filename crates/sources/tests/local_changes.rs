use std::fs;
use std::path::Path;
use std::sync::Arc;

use library::{Database, ReadCancellation, ScanOutcome, TrackSort};
use sources::{LocalFolderHostInput, LocalLiveChange, Source, SourceSetupInput};

#[tokio::test]
async fn one_local_path_republishes_only_its_component() {
    let root = tempfile::tempdir().expect("music root");
    let first = root.path().join("first.wav");
    let second = root.path().join("second.wav");
    write_silent_wav(&first, 1).expect("first WAV");
    write_silent_wav(&second, 1).expect("second WAV");
    let store = tempfile::tempdir().expect("Store root");
    let database = Database::open(store.path().join("library.sqlite"))
        .await
        .expect("Database");
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
            LocalLiveChange::Paths(vec![first.clone()]),
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
async fn local_component_larger_than_one_sql_batch_converges_atomically() {
    let root = tempfile::tempdir().expect("music root");
    let mut paths = Vec::new();
    for index in 0..130 {
        let path = root.path().join(format!("track-{index:03}.wav"));
        write_silent_wav(&path, 1).expect("WAV");
        paths.push(path);
    }
    let store = tempfile::tempdir().expect("Store root");
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
            .expect("initial scan"),
    );
    for path in &paths {
        write_silent_wav(path, 2).expect("edit WAV");
    }
    let changed = source
        .apply_local_change(
            &database,
            initial.source,
            LocalLiveChange::Paths(vec![root.path().to_path_buf()]),
        )
        .await
        .expect("large component")
        .expect("point outcome");
    let changed = publication(changed);
    assert_eq!(changed.catalog_revision, initial.catalog_revision + 1);
    assert_eq!(
        database
            .track_order(
                changed.source,
                None,
                false,
                TrackSort::Title,
                false,
                &ReadCancellation::new()
            )
            .await
            .expect("Track order")
            .len(),
        130
    );
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
        .album_order(
            initial.source,
            None,
            false,
            library::AlbumSort::Title,
            false,
            &ReadCancellation::new(),
        )
        .await
        .expect("Album order")[0];
    database
        .write_album_artwork_binding(initial.source, album, Some(b"accepted-art"), [9; 32])
        .await
        .expect("binding");
    fs::write(&image, b"changed-image").expect("change image");
    let outcome = source
        .apply_local_change(
            &database,
            initial.source,
            LocalLiveChange::Paths(vec![image]),
        )
        .await
        .expect("image change")
        .expect("point outcome");
    assert!(
        matches!(outcome,ScanOutcome::ArtworkChanged(publication) if publication.catalog_revision==initial.catalog_revision)
    );
    assert_eq!(
        database
            .album_artwork_binding(initial.source, album, &ReadCancellation::new())
            .await
            .expect("Album binding"),
        None
    );
}

fn publication(outcome: ScanOutcome) -> library::Publication {
    match outcome {
        ScanOutcome::Changed(publication)
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
