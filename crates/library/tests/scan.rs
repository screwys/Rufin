use std::path::Path;

use library::{
    Database, Freshness, LocalFileKind, LocalFileState, LocalFileWrite, ReadCancellation, Scan,
    ScanOutcome,
};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{Connection, FromRow};

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
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false),
    )
    .await
    .expect("open test connection")
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
        false,
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
        Some("file:///music/track.flac"),
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
    sqlx::query("UPDATE tracks SET user_favorite=1, user_rating=90 WHERE object_id='track-one'")
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
             (SELECT user_favorite FROM tracks WHERE object_id='track-one'),
             (SELECT user_rating FROM tracks WHERE object_id='track-one')",
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
        "SELECT artwork_digest FROM sources WHERE object_id='source-one'",
    )
    .fetch_one(&mut outside)
    .await
    .expect("read artwork digest before Genre change");
    let ScanOutcome::Changed(artwork_change) = write_small_catalog(
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
    assert_eq!(artwork_change.catalog_revision, 3);
    let artwork_after = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT artwork_digest FROM sources WHERE object_id='source-one'",
    )
    .fetch_one(&mut outside)
    .await
    .expect("read artwork digest after Genre change");
    assert_ne!(artwork_after, artwork_before);
    assert_eq!(artwork_change.artwork_digest.as_slice(), artwork_after);
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
        .expect("stage referentially invalid row");
    invalid
        .finish()
        .await
        .expect_err("invalid publication rolls back");

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
        "UPDATE sources SET catalog_revision=catalog_revision+1 WHERE object_id='source-one'",
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
            "SELECT catalog_revision FROM sources WHERE object_id='source-one'",
        )
        .fetch_one(&mut outside)
        .await
        .expect("read revision"),
        (first.catalog_revision + 1) as i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM sources WHERE object_id IN ('source-two', 'source-four')",
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
            Some("file:///music/track.flac"),
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
