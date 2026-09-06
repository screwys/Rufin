use std::path::Path;

use library::{
    Database, Freshness, LocalFileKind, LocalFileState, LocalFileWrite, ReadCancellation, Scan,
    ScanOutcome,
};
use sqlx::FromRow;
use sqlx::sqlite::SqliteConnection;

const PRIVATE_TRACK_COUNT: usize = 100_000;

#[tokio::test]
async fn local_component_paths_page_beyond_one_batch() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    let ScanOutcome::Changed(publication) =
        write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await
    else {
        panic!("catalog must publish");
    };
    let mut observations = Vec::with_capacity(131);
    observations.push((
        LocalFileWrite {
            path: "/music".to_string(),
            root: "/music".to_string(),
            relative_path: String::new(),
            kind: LocalFileKind::Directory,
            size_bytes: None,
            mtime_ns: 1,
            device_id: None,
            inode: None,
            parse_version: None,
            state: LocalFileState::Observed,
        },
        Vec::new(),
    ));
    observations.extend((0..130).map(|index| {
        (
            LocalFileWrite {
                path: format!("/music/track-{index:03}.flac"),
                root: "/music".to_string(),
                relative_path: format!("track-{index:03}.flac"),
                kind: LocalFileKind::Media,
                size_bytes: Some(100),
                mtime_ns: 1,
                device_id: None,
                inode: None,
                parse_version: Some(1),
                state: LocalFileState::Accepted,
            },
            Vec::new(),
        )
    }));
    let mut observations_scan = Scan::begin_items(&database, "source-one")
        .await
        .expect("begin Local observation scan");
    for page in observations.chunks(128) {
        observations_scan.begin_batch().await.expect("begin batch");
        observations_scan
            .write_local_files(page)
            .await
            .expect("write Local observations");
        observations_scan
            .finish_batch()
            .await
            .expect("finish batch");
    }
    observations_scan
        .finish()
        .await
        .expect("publish Local observations");

    let mut component = Scan::begin_items(&database, "source-one")
        .await
        .expect("begin component scan");
    component
        .begin_batch()
        .await
        .expect("begin component batch");
    component
        .write_local_component_paths(&["/music".to_string()])
        .await
        .expect("seed component");
    component
        .expand_local_component(publication.source)
        .await
        .expect("expand component");
    component
        .finish_batch()
        .await
        .expect("finish component batch");
    let first = component
        .local_component_path_page(None, 128)
        .await
        .expect("first component page");
    let second = component
        .local_component_path_page(first.last().map(String::as_str), 128)
        .await
        .expect("second component page");
    assert_eq!((first.len(), second.len()), (128, 3));
    component.finish().await.expect("finish component scan");
}

async fn connection(path: &Path) -> SqliteConnection {
    super::support::connection(path).await
}

async fn write_small_catalog(
    database: &Database,
    freshness: &str,
    title: &str,
    reverse: bool,
    genre_artwork: &[u8],
) -> ScanOutcome {
    let mut scan = Scan::begin(
        database,
        "source-one",
        "Source One",
        "source one",
        Some(Freshness::new(freshness.as_bytes()).expect("bounded freshness")),
    )
    .await
    .expect("begin scan");
    scan.write_album(
        "album-one",
        "Album One",
        "album one",
        "Artist One",
        "album one",
        Some(2025),
        Some("2025-01-01"),
        Some("2025-01-02"),
        Some("release-one"),
        Some("release-group-one"),
        Some(false),
        Some(b"album-art"),
        false,
        Some(7),
        Some(10),
    )
    .await
    .expect("stage album");
    scan.write_artist(
        "artist-one",
        "Artist One",
        "artist one",
        "artist one",
        Some("artist-mbid-one"),
        Some(b"artist-art"),
        Some(false),
        None,
    )
    .await
    .expect("stage artist");
    scan.write_genre(
        "genre-one",
        "Genre One",
        "genre one",
        "genre one",
        Some(genre_artwork),
    )
    .await
    .expect("stage genre");
    scan.write_mood("mood-one", "Mood One", "mood one", "mood one")
        .await
        .expect("stage mood");
    scan.write_folder("folder-one", "Folder One", "folder one", "folder one", None)
        .await
        .expect("stage folder");
    scan.write_playlist(
        "playlist-one",
        "Playlist One",
        "playlist one",
        "playlist one",
        None,
    )
    .await
    .expect("stage playlist");

    let tracks = [
        ("track-one", title, 0_i64),
        ("track-two", "Track Two", 1_i64),
    ];
    let order: &[usize] = if reverse { &[1, 0] } else { &[0, 1] };
    for &index in order {
        let (object_id, track_title, position) = tracks[index];
        write_track(&mut scan, object_id, track_title, position).await;
    }
    scan.write_album_relations(
        &[("album-one", "artist-one")],
        &[("album-one", "genre-one")],
        &[("album-one", "Album")],
    )
    .await
    .expect("stage album relations");
    scan.finish().await.expect("finish scan")
}

async fn write_track(scan: &mut Scan, object_id: &str, title: &str, position: i64) {
    let normalized = format!("{} album one artist one note", title.to_lowercase());
    let media_uri = format!("file:///music/{object_id}.flac");
    scan.write_track(
        object_id,
        Some("album-one"),
        title,
        &normalized,
        "Album One",
        "Artist One",
        &normalized,
        180_000,
        1,
        position + 1,
        Some(2025),
        Some("2025-01-01"),
        Some("2025-01-02"),
        Some(&media_uri),
        Some("FLAC"),
        Some("Note"),
        Some(120),
        Some("recording-one"),
        Some("release-track-one"),
        Some("/music/album.cue"),
        Some(1_000),
        Some(181_000),
        Some(format!("art-{object_id}").as_bytes()),
        false,
        None,
        Some(10 + position),
        Some(20 + position),
        Some(position),
        Some(1_700_000_000 + position),
        None,
        [position as u8 + 1; 32],
    )
    .await
    .expect("stage track");
    scan.write_track_relations(
        &[(object_id, "artist-one")],
        &[(object_id, "genre-one")],
        &[(object_id, "mood-one")],
    )
    .await
    .expect("stage track relations");
    scan.write_track_folders(&[library::ScanLink::new(object_id, "folder-one", 0)])
        .await
        .expect("stage track folder");
    scan.write_track_folders(&[library::ScanLink::new(object_id, "folder-one", 0)])
        .await
        .expect("coalesce repeated track folder");
    scan.write_playlist_entry(
        "playlist-one",
        &format!("entry-{object_id}"),
        object_id,
        position,
    )
    .await
    .expect("stage playlist entry");
}

#[tokio::test]
async fn canonical_publication_preserves_identity_and_user_overrides() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    let ScanOutcome::Changed(first) =
        write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await
    else {
        panic!("first scan must publish");
    };
    assert_eq!(first.catalog_revision, 1);
    assert_eq!(
        database
            .cached_source("source-one", &ReadCancellation::new())
            .await
            .expect("read saved source without provider contact")
            .map(|source| (source.source, source.catalog_revision)),
        Some((first.source, 1))
    );
    let mut outside = connection(&path).await;
    let original_keys = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT
             (SELECT album_key FROM albums WHERE object_id='album-one'),
             (SELECT track_key FROM tracks WHERE object_id='track-one'),
             (SELECT playlist_entry_key FROM playlist_entries
              WHERE object_id='entry-track-one')",
    )
    .fetch_one(&mut outside)
    .await
    .expect("read stable keys");
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, Option<i64>)>(
            "SELECT play_count,skip_count,last_played_at FROM activity_baseline
             WHERE source_key=?1 AND track_object_id='track-one'",
        )
        .bind(first.source)
        .fetch_one(&mut outside)
        .await
        .expect("read initial provider Activity baseline"),
        (20, 0, Some(1_700_000_000))
    );
    sqlx::query(
        "INSERT INTO user_media_state(media_uri,favorite,rating)
         SELECT media_uri,1,90 FROM tracks WHERE object_id='track-one'",
    )
    .execute(&mut outside)
    .await
    .expect("write user overrides");

    assert_eq!(
        write_small_catalog(&database, "fresh-two", "Track One", true, b"genre-art").await,
        ScanOutcome::Identical(first)
    );
    assert_eq!(
        Scan::accept_freshness(
            &database,
            "source-one",
            &Freshness::new(b"fresh-two".to_vec()).expect("bounded freshness"),
            &ReadCancellation::new(),
        )
        .await
        .expect("accept freshness"),
        Some(first)
    );

    let ScanOutcome::Changed(changed) = write_small_catalog(
        &database,
        "fresh-three",
        "Changed Track",
        false,
        b"genre-art",
    )
    .await
    else {
        panic!("changed scan must publish");
    };
    assert_eq!(changed.catalog_revision, 2);
    assert_eq!(changed.source, first.source);
    let changed_facts = sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(
        "SELECT
             (SELECT album_key FROM albums WHERE object_id='album-one'),
             (SELECT track_key FROM tracks WHERE object_id='track-one'),
             (SELECT playlist_entry_key FROM playlist_entries
              WHERE object_id='entry-track-one'),
             (SELECT state.favorite FROM user_media_state state JOIN tracks track USING(media_uri) WHERE track.object_id='track-one'),
             (SELECT state.rating FROM user_media_state state JOIN tracks track USING(media_uri) WHERE track.object_id='track-one')",
    )
    .fetch_one(&mut outside)
    .await
    .expect("read changed facts");
    assert_eq!(
        changed_facts,
        (original_keys.0, original_keys.1, original_keys.2, 1, 90)
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, Option<i64>)>(
            "SELECT play_count,skip_count,last_played_at FROM activity_baseline
             WHERE source_key=?1 AND track_object_id='track-one'",
        )
        .bind(first.source)
        .fetch_one(&mut outside)
        .await
        .expect("read preserved provider Activity baseline"),
        (20, 0, Some(1_700_000_000))
    );
    assert_eq!(
        sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                i64,
                String,
                String,
                String,
                i64,
                i64
            ),
        >(
            "SELECT normalized_search, date_added, source_format, comment, bpm,
                    musicbrainz_recording_id, musicbrainz_release_track_id,
                    cue_path, cue_start_millis, cue_end_millis
             FROM tracks WHERE object_id='track-one'",
        )
        .fetch_one(&mut outside)
        .await
        .expect("read published Track facts"),
        (
            "changed track album one artist one note".to_string(),
            "2025-01-02".to_string(),
            "FLAC".to_string(),
            "Note".to_string(),
            120,
            "recording-one".to_string(),
            "release-track-one".to_string(),
            "/music/album.cue".to_string(),
            1000,
            181000,
        )
    );
    assert_eq!(
        sqlx::query_as::<_, (String, String, String, String, i64)>(
            "SELECT album.date_added, album.musicbrainz_release_id,
                    album.musicbrainz_release_group_id,
                    artist.musicbrainz_artist_id, length(genre.artwork_binding)
             FROM albums AS album, artists AS artist, genres AS genre",
        )
        .fetch_one(&mut outside)
        .await
        .expect("read published collection facts"),
        (
            "2025-01-02".to_string(),
            "release-one".to_string(),
            "release-group-one".to_string(),
            "artist-mbid-one".to_string(),
            9,
        )
    );
    let artwork_before = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT artwork_digest FROM sources
         WHERE source_key=(SELECT source_key FROM sources WHERE object_id='source-one')",
    )
    .fetch_one(&mut outside)
    .await
    .expect("read artwork digest before Genre change");
    let ScanOutcome::ArtworkChanged(artwork_change) = write_small_catalog(
        &database,
        "fresh-four",
        "Changed Track",
        false,
        b"genre-art-two",
    )
    .await
    else {
        panic!("Genre artwork change must publish");
    };
    assert_eq!(artwork_change.catalog_revision, 2);
    let artwork_after = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT artwork_digest FROM sources
         WHERE source_key=(SELECT source_key FROM sources WHERE object_id='source-one')",
    )
    .fetch_one(&mut outside)
    .await
    .expect("read artwork digest after Genre change");
    assert_ne!(artwork_after, artwork_before);
    assert_eq!(artwork_change.artwork_digest.as_slice(), artwork_after);
}

#[tokio::test]
async fn artwork_publication_keeps_one_effective_binding_per_entity() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");

    write_small_catalog(&database, "shared", "Track One", false, b"genre-art").await;
    let mut reader = connection(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT artwork_binding FROM artists WHERE object_id='artist-one'"
        )
        .fetch_one(&mut reader)
        .await
        .expect("Artist keeps its own image"),
        b"artist-art"
    );
    let shared = sqlx::query_as::<_, (Vec<u8>, Vec<u8>, Vec<u8>)>(
        "SELECT album.artwork_binding,first.artwork_binding,second.artwork_binding
         FROM albums album
         JOIN tracks first ON first.object_id='track-one'
         JOIN tracks second ON second.object_id='track-two'",
    )
    .fetch_one(&mut reader)
    .await
    .expect("read shared effective bindings");
    assert_eq!(
        shared,
        (
            b"album-art".to_vec(),
            b"album-art".to_vec(),
            b"album-art".to_vec()
        )
    );

    database.set_distinct_track_covers(true);
    assert_eq!(
        Scan::accept_freshness(
            &database,
            "source-one",
            &Freshness::new(b"shared".to_vec()).expect("freshness"),
            &ReadCancellation::new(),
        )
        .await
        .expect("check policy freshness"),
        None
    );
    write_small_catalog(&database, "distinct", "Track One", false, b"genre-art").await;
    let distinct = sqlx::query_as::<_, (Vec<u8>, Vec<u8>, Vec<u8>)>(
        "SELECT album.artwork_binding,first.artwork_binding,second.artwork_binding
         FROM albums album
         JOIN tracks first ON first.object_id='track-one'
         JOIN tracks second ON second.object_id='track-two'",
    )
    .fetch_one(&mut reader)
    .await
    .expect("read distinct effective bindings");
    assert_eq!(
        distinct,
        (
            b"album-art".to_vec(),
            b"art-track-one".to_vec(),
            b"art-track-two".to_vec(),
        )
    );
}

#[tokio::test]
async fn point_album_artwork_updates_cached_members_without_replacing_distinct_covers() {
    for distinct in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("library.sqlite");
        let database = Database::open(&path).await.unwrap();
        database.set_distinct_track_covers(distinct);
        write_small_catalog(&database, "first", "Track One", false, b"genre-art").await;
        let mut reader = connection(&path).await;
        for (title, artwork) in [
            ("Album One", b"new-art".as_slice()),
            ("Renamed Album", b"newer-art".as_slice()),
            ("Renamed Album", b"newer-art".as_slice()),
        ] {
            let mut point = Scan::begin_items(&database, "source-one").await.unwrap();
            point
                .write_artist(
                    "artist-one",
                    "Artist One",
                    "artist one",
                    "artist one",
                    Some("artist-mbid-one"),
                    Some(b"artist-art"),
                    Some(false),
                    None,
                )
                .await
                .unwrap();
            point
                .write_genre(
                    "genre-one",
                    "Genre One",
                    "genre one",
                    "genre one",
                    Some(b"genre-art"),
                )
                .await
                .unwrap();
            point
                .write_album(
                    "album-one",
                    title,
                    "album one",
                    "Artist One",
                    "album one",
                    Some(2025),
                    Some("2025-01-01"),
                    Some("2025-01-02"),
                    Some("release-one"),
                    Some("release-group-one"),
                    Some(false),
                    Some(artwork),
                    false,
                    Some(7),
                    Some(10),
                )
                .await
                .unwrap();
            point
                .write_album_relations(
                    &[("album-one", "artist-one")],
                    &[("album-one", "genre-one")],
                    &[("album-one", "Album")],
                )
                .await
                .unwrap();
            let previous = sqlx::query_scalar::<_, Vec<u8>>(
                "SELECT artwork_binding FROM albums WHERE object_id='album-one'",
            )
            .fetch_one(&mut reader)
            .await
            .unwrap();
            let outcome = point.finish().await.unwrap();
            assert!(match outcome {
                ScanOutcome::ArtworkChanged(_) => title == "Album One",
                ScanOutcome::Changed(_) => title == "Renamed Album" && previous != artwork,
                ScanOutcome::Identical(_) => previous == artwork,
                _ => false,
            });
            assert_eq!(
                sqlx::query_scalar::<_, Vec<u8>>(
                    "SELECT artwork_binding FROM tracks ORDER BY object_id"
                )
                .fetch_all(&mut reader)
                .await
                .unwrap(),
                if distinct {
                    vec![b"art-track-one".to_vec(), b"art-track-two".to_vec()]
                } else {
                    vec![artwork.to_vec(), artwork.to_vec()]
                }
            );
        }
    }
}

#[tokio::test]
async fn incomplete_collection_publishes_valid_pages_without_removal_authority() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    write_small_catalog(&database, "complete", "Track One", false, b"genre-art").await;

    let mut scan = Scan::begin(
        &database,
        "source-one",
        "Source One",
        "source one",
        Some(Freshness::new(b"incomplete".to_vec()).expect("freshness")),
    )
    .await
    .expect("begin incomplete Scan");
    scan.incomplete();
    write_track(&mut scan, "track-one", "Changed From Valid Page", 0).await;
    assert!(matches!(scan.finish().await, Ok(ScanOutcome::Changed(_))));

    let mut outside = connection(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut outside)
            .await
            .expect("count retained Tracks"),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT title FROM tracks WHERE object_id='track-one'")
            .fetch_one(&mut outside)
            .await
            .expect("read accepted page"),
        "Changed From Valid Page"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT title FROM tracks WHERE object_id='track-two'")
            .fetch_one(&mut outside)
            .await
            .expect("read retained unseen Track"),
        "Track Two"
    );
}

#[tokio::test]
async fn incomplete_and_point_scans_do_not_certify_a_full_catalog_digest() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite");
    let database = Database::open(&path).await.unwrap();
    for (names, authoritative, expected_count) in [
        (["a", "b"].as_slice(), true, 2),
        (["a"].as_slice(), false, 2),
        (["a"].as_slice(), true, 1),
    ] {
        let mut scan = Scan::begin(&database, "source", "Source", "source", None)
            .await
            .unwrap();
        if !authoritative {
            scan.incomplete();
        }
        for name in names {
            scan.write_genre(name, name, name, name, None)
                .await
                .unwrap();
        }
        assert!(matches!(
            scan.finish().await.unwrap(),
            ScanOutcome::Changed(_)
        ));
        let mut reader = connection(&path).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM genres")
                .fetch_one(&mut reader)
                .await
                .unwrap(),
            expected_count,
            "a complete scan must remove items retained by its incomplete predecessor"
        );
    }
    let mut point = Scan::begin_items(&database, "source").await.unwrap();
    point
        .write_genre("a", "Live name", "live name", "live name", None)
        .await
        .unwrap();
    point.finish().await.unwrap();
    let mut complete = Scan::begin(&database, "source", "Source", "source", None)
        .await
        .unwrap();
    complete
        .write_genre("a", "a", "a", "a", None)
        .await
        .unwrap();
    assert!(matches!(
        complete.finish().await.unwrap(),
        ScanOutcome::Changed(_)
    ));
    let mut reader = connection(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT name FROM genres WHERE object_id='a'")
            .fetch_one(&mut reader)
            .await
            .unwrap(),
        "a",
        "a point update cannot leave the previous full-catalog digest valid"
    );
}

#[tokio::test]
async fn identical_and_artwork_only_incomplete_scans_retain_unseen_local_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite");
    let database = Database::open(&path).await.unwrap();
    let files = ["one.flac", "two.flac"].map(|name| {
        (
            LocalFileWrite {
                path: format!("/music/{name}"),
                root: "/music".into(),
                relative_path: name.into(),
                kind: LocalFileKind::Media,
                size_bytes: Some(100),
                mtime_ns: 1,
                device_id: None,
                inode: None,
                parse_version: Some(1),
                state: LocalFileState::Accepted,
            },
            Vec::new(),
        )
    });
    for (index, artwork) in [b"first".as_slice(), b"first", b"second"]
        .into_iter()
        .enumerate()
    {
        let mut scan = Scan::begin(&database, "local", "Local", "local", None)
            .await
            .unwrap();
        scan.write_genre("genre", "Genre", "genre", "genre", Some(artwork))
            .await
            .unwrap();
        if index > 0 {
            scan.incomplete();
        }
        scan.write_local_files(&files[..if index == 0 { 2 } else { 1 }])
            .await
            .unwrap();
        let outcome = scan.finish().await.unwrap();
        assert!(match index {
            0 => matches!(outcome, ScanOutcome::Changed(_)),
            1 => matches!(outcome, ScanOutcome::Identical(_)),
            _ => matches!(outcome, ScanOutcome::ArtworkChanged(_)),
        });
        let mut reader = connection(&path).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM local_files")
                .fetch_one(&mut reader)
                .await
                .unwrap(),
            2
        );
    }
}

#[tokio::test]
async fn saved_source_opens_from_cache_without_starting_a_scan() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    let ScanOutcome::Changed(publication) =
        write_small_catalog(&database, "fresh", "Cached Track", false, b"art").await
    else {
        panic!("initial scan must publish");
    };
    drop(database);

    let reopened = Database::open(&path).await.expect("reopen cached Store");
    let cached = reopened
        .cached_source("source-one", &ReadCancellation::new())
        .await
        .expect("resolve configured source from SQLite")
        .expect("cached source");

    assert_eq!(cached.source, publication.source);
    assert_eq!(cached.display_name, "Source One");
    assert_eq!(cached.catalog_revision, 1);
}

#[tokio::test]
async fn failed_and_stale_publications_are_atomic() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    let ScanOutcome::Changed(first) =
        write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await
    else {
        panic!("first scan must publish");
    };

    let mut failed = Scan::begin(&database, "source-two", "Two", "two", None)
        .await
        .expect("begin failed scan");
    failed
        .write_track(
            "", None, "Bad", "bad", "", "", "bad", 0, 0, 0, None, None, None, None, None, None,
            None, None, None, None, None, None, None, false, None, None, None, None, None, None,
            [0; 32],
        )
        .await
        .expect_err("empty identity fails staging");
    assert_eq!(
        failed.finish().await.expect("finish failed scan"),
        ScanOutcome::Failed
    );

    let mut oversized = Scan::begin(&database, "source-three", "Three", "three", None)
        .await
        .expect("begin oversized scan");
    let oversized_name = "x".repeat(8 * 1024 * 1024 + 1);
    oversized
        .write_genre("genre", &oversized_name, "genre", "genre", None)
        .await
        .expect_err("oversized row fails staging");
    assert_eq!(
        oversized.finish().await.expect("finish oversized scan"),
        ScanOutcome::Failed
    );

    let mut invalid = Scan::begin(&database, "source-four", "Four", "four", None)
        .await
        .expect("begin invalid scan");
    invalid
        .write_track(
            "orphan-track",
            Some("missing-album"),
            "Orphan",
            "orphan",
            "Missing",
            "Artist",
            "orphan",
            1,
            1,
            1,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            [1; 32],
        )
        .await
        .expect("stage song with unavailable album relationship");
    assert!(matches!(
        invalid.finish().await.expect("publish useful song"),
        ScanOutcome::Changed(_)
    ));
    let song = database
        .track_row_by_uri(
            &library::source_entity_uri(
                &library::SourceId::new("source-four"),
                "track",
                "orphan-track",
            ),
            &ReadCancellation::new(),
        )
        .await
        .expect("project song")
        .expect("song admitted");
    assert_eq!(song.album, "Missing");
    assert!(song.album_key.is_none());

    let stale = Scan::begin(
        &database,
        "source-one",
        "Source One",
        "source one",
        Some(Freshness::new(b"later".to_vec()).expect("freshness")),
    )
    .await
    .expect("begin stale scan");
    let mut outside = connection(&path).await;
    sqlx::query(
        "UPDATE sources SET catalog_revision=catalog_revision+1
         WHERE source_key=(SELECT source_key FROM sources WHERE object_id='source-one')",
    )
    .execute(&mut outside)
    .await
    .expect("advance accepted revision");
    assert_eq!(
        stale.finish().await.expect("finish stale scan"),
        ScanOutcome::Stale
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT catalog_revision FROM sources
             WHERE source_key=(SELECT source_key FROM sources WHERE object_id='source-one')",
        )
        .fetch_one(&mut outside)
        .await
        .expect("read revision"),
        (first.catalog_revision + 1) as i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM sources WHERE object_id='source-two' AND catalog_revision>0",
        )
        .fetch_one(&mut outside)
        .await
        .expect("count rejected sources"),
        0
    );
}

#[tokio::test]
async fn ordinary_private_library_scan_is_bounded() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    let mut scan = Scan::begin(&database, "private-source", "Private", "private", None)
        .await
        .expect("begin private scan");
    for number in 0..PRIVATE_TRACK_COUNT {
        let object_id = format!("track-{number:06}");
        scan.write_track(
            &object_id,
            None,
            "Track",
            "track",
            "",
            "Artist",
            &object_id,
            180_000,
            1,
            number as i64,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            [(number % 251) as u8 + 1; 32],
        )
        .await
        .expect("stage one bounded Track row");
    }
    assert!(matches!(
        scan.finish().await.expect("publish private scan"),
        ScanOutcome::Changed(_)
    ));
    let mut outside = connection(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut outside)
            .await
            .expect("count Tracks"),
        PRIVATE_TRACK_COUNT as i64
    );
}

#[derive(FromRow)]
struct QueryPlan {
    detail: String,
}

#[tokio::test]
async fn ordinary_orders_and_point_rows_use_named_indexes() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    let ScanOutcome::Changed(publication) =
        write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await
    else {
        panic!("catalog must publish");
    };
    let mut outside = connection(&path).await;
    let order = sqlx::query_as::<_, QueryPlan>(
        "EXPLAIN QUERY PLAN
         SELECT track_key FROM tracks
         WHERE source_key=?1 ORDER BY sort_text, track_key",
    )
    .bind(publication.source.raw())
    .fetch_all(&mut outside)
    .await
    .expect("read order plan");
    assert_eq!(
        order
            .iter()
            .map(|row| row.detail.as_str())
            .collect::<Vec<_>>(),
        ["SEARCH tracks USING COVERING INDEX tracks_order_idx (source_key=?)"]
    );
    let point = sqlx::query_as::<_, QueryPlan>(
        "EXPLAIN QUERY PLAN SELECT * FROM tracks WHERE track_key=?1",
    )
    .bind(1_i64)
    .fetch_all(&mut outside)
    .await
    .expect("read point plan");
    assert_eq!(
        point
            .iter()
            .map(|row| row.detail.as_str())
            .collect::<Vec<_>>(),
        ["SEARCH tracks USING INTEGER PRIMARY KEY (rowid=?)"]
    );
}

#[tokio::test]
async fn deleting_one_source_playlist_preserves_other_playlist_entries() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await;

    let mut point = Scan::begin_items(&database, "source-one")
        .await
        .expect("begin Playlist point scan");
    point.begin_batch().await.unwrap();
    point
        .write_playlist(
            "playlist-one",
            "Playlist One",
            "playlist one",
            "playlist one",
            None,
        )
        .await
        .unwrap();
    point
        .write_playlist("playlist-two", "Other", "other", "other", None)
        .await
        .unwrap();
    point
        .write_playlist_entry("playlist-one", "entry-one", "track-one", 0)
        .await
        .unwrap();
    point.finish_batch().await.unwrap();
    point.finish().await.unwrap();

    let mut removal = Scan::begin_items(&database, "source-one")
        .await
        .expect("begin Playlist removal");
    removal.begin_batch().await.unwrap();
    removal.remove_playlist("playlist-two").await.unwrap();
    removal.finish_batch().await.unwrap();
    removal.finish().await.unwrap();

    let mut outside = connection(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM playlist_entries entry
             JOIN playlists playlist USING(playlist_key)
             WHERE playlist.object_id='playlist-one'",
        )
        .fetch_one(&mut outside)
        .await
        .expect("count preserved Playlist entries"),
        1
    );
}

#[tokio::test]
async fn identical_playlist_point_update_is_not_a_catalog_change() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    let ScanOutcome::Changed(initial) =
        write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await
    else {
        panic!("initial catalog must publish");
    };

    let mut point = Scan::begin_items(&database, "source-one")
        .await
        .expect("begin Playlist point scan");
    point.begin_batch().await.unwrap();
    point
        .write_playlist(
            "playlist-one",
            "Playlist One",
            "playlist one",
            "playlist one",
            None,
        )
        .await
        .unwrap();
    point
        .write_playlist_entry("playlist-one", "entry-track-one", "track-one", 0)
        .await
        .unwrap();
    point
        .write_playlist_entry("playlist-one", "entry-track-two", "track-two", 1)
        .await
        .unwrap();
    point.finish_batch().await.unwrap();

    let ScanOutcome::Identical(replayed) = point.finish().await.unwrap() else {
        panic!("an identical Playlist readback must be ignored");
    };
    assert_eq!(replayed.catalog_revision, initial.catalog_revision);
}

#[tokio::test]
async fn repeated_source_playlist_and_folder_observations_coalesce() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await;
    let mut scan = Scan::begin_items(&database, "source-one")
        .await
        .expect("begin Scan");

    for _ in 0..2 {
        scan.begin_batch().await.expect("begin repeated batch");
        scan.write_folder("folder-one", "Music", "music", "music", None)
            .await
            .expect("stage repeated folder");
        scan.write_playlist(
            "playlist-one",
            "Playlist One",
            "playlist one",
            "playlist one",
            None,
        )
        .await
        .expect("stage repeated playlist");
        scan.write_playlist_entry("playlist-one", "entry-one", "track-one", 0)
            .await
            .expect("stage repeated playlist entry");
        scan.finish_batch().await.expect("finish repeated batch");
    }

    assert!(matches!(
        scan.finish().await.expect("publish repeated observations"),
        ScanOutcome::Changed(_)
    ));
}

#[tokio::test]
async fn duplicate_provider_positions_are_normalized_without_rejecting_occurrences() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await;
    let mut scan = Scan::begin_items(&database, "source-one")
        .await
        .expect("begin Playlist point Scan");
    scan.begin_batch().await.expect("begin batch");
    scan.write_playlist("playlist-one", "Playlist", "playlist", "playlist", None)
        .await
        .expect("stage Playlist");
    scan.write_playlist_entry("playlist-one", "duplicate-a", "track-one", 7)
        .await
        .expect("stage first occurrence");
    scan.write_playlist_entry("playlist-one", "duplicate-b", "track-two", 7)
        .await
        .expect("stage second occurrence");
    scan.finish_batch().await.expect("finish batch");
    scan.finish().await.expect("publish duplicate positions");

    let mut outside = connection(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT entry.position FROM playlist_entries entry
             JOIN playlists playlist USING(playlist_key)
             WHERE playlist.object_id='playlist-one' ORDER BY entry.position",
        )
        .fetch_all(&mut outside)
        .await
        .expect("read normalized occurrence order"),
        [0, 1]
    );
}

#[tokio::test]
async fn playlist_point_update_preserves_playlist_scope() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await;

    let mut point = Scan::begin_items(&database, "source-one")
        .await
        .expect("begin Playlist point scan");
    point.begin_batch().await.unwrap();
    point
        .write_playlist(
            "playlist-one",
            "Playlist One",
            "playlist one",
            "playlist one",
            None,
        )
        .await
        .unwrap();
    point
        .write_playlist_entry("playlist-one", "entry-track-two", "track-two", 0)
        .await
        .unwrap();
    point.finish_batch().await.unwrap();

    assert!(matches!(
        point.finish().await.unwrap(),
        ScanOutcome::PlaylistsChanged(_)
    ));
}

#[tokio::test]
async fn artist_credits_preserve_favorites_and_explicit_false_clears_them() {
    let root = tempfile::tempdir().unwrap();
    let database = Database::open(root.path().join("library.sqlite"))
        .await
        .unwrap();
    write_small_catalog(&database, "fresh", "Track", false, b"genre").await;
    let uri = library::source_entity_uri(
        &library::SourceId::new("source-one"),
        "artist",
        "artist-one",
    );
    let cancel = ReadCancellation::new();
    for (index, favorite) in [Some(true), None, Some(false), None]
        .into_iter()
        .enumerate()
    {
        let mut scan = Scan::begin_items(&database, "source-one").await.unwrap();
        scan.write_artist(
            "artist-one",
            "Artist",
            "artist",
            "artist",
            None,
            None,
            favorite,
            None,
        )
        .await
        .unwrap();
        // A credit arriving after a complete record must not overwrite its state.
        scan.write_artist(
            "artist-one",
            "Artist",
            "artist",
            "artist",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        scan.finish().await.unwrap();
        let row = database
            .artist_row_by_media_uri(&uri, &cancel)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.favorite, index < 2);
    }
}
