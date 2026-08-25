use std::path::Path;

use library::Database;
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

async fn connection(path: &Path, create: bool) -> SqliteConnection {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(create),
    )
    .await
    .expect("open schema test connection")
}

#[tokio::test]
async fn fresh_schema_has_exact_whitelisted_tables() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let _database = Database::open(&path).await.expect("open fresh Store");
    let mut reader = connection(&path, false).await;
    let tables = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_schema
         WHERE type='table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )
    .fetch_all(&mut reader)
    .await
    .expect("read table inventory");
    assert_eq!(
        tables,
        [
            "activity_baseline",
            "album_artists",
            "album_genres",
            "album_release_types",
            "albums",
            "artists",
            "favorite_outbox",
            "folders",
            "genres",
            "home_entries",
            "listen_outbox",
            "listens",
            "local_access_files",
            "local_file_dependencies",
            "local_files",
            "loudness_measurements",
            "lyrics_cache",
            "moods",
            "playlist_entries",
            "playlists",
            "queue_occurrences",
            "queue_state",
            "smart_playlists",
            "sources",
            "track_artists",
            "track_folders",
            "track_genres",
            "track_moods",
            "tracks",
        ]
    );
}

#[tokio::test]
async fn released_store_is_preserved_and_salvaged_into_fresh_schema() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    create_released_store(&path).await;
    assert!(Database::open(&path).await.is_err());
    assert!(
        path.exists(),
        "ordinary open preserves the released Store in place"
    );
    assert!(
        Database::released_repair_available(&path)
            .await
            .expect("inspect released Store")
    );
    let report = Database::repair_released(&path)
        .await
        .expect("explicitly repair released Store");
    assert!(report.preserved_store.exists());
    let database = Database::open(&path).await.expect("open repaired Store");
    let mut reader = connection(&path, false).await;
    assert_eq!(
        sqlx::query_as::<_, (String, i64)>("SELECT object_id, catalog_revision FROM sources",)
            .fetch_one(&mut reader)
            .await
            .expect("read recovered source"),
        ("released-source".to_string(), 1)
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT user_favorite, user_rating FROM tracks
             WHERE object_id='released-track'",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered user facts"),
        (1, 80)
    );
    assert_eq!(
        sqlx::query_as::<_, (String, i64, i64, String, Option<String>, Option<i64>)>(
            "SELECT object_id, position, traversal_position, provenance_kind,
                    provenance_context_id, provenance_source_rank
             FROM queue_occurrences ORDER BY position",
        )
        .fetch_all(&mut reader)
        .await
        .expect("read recovered queue orders and provenance"),
        [
            (
                "occ-context".to_string(),
                0,
                1,
                "context".to_string(),
                Some("route-album".to_string()),
                Some(7),
            ),
            (
                "occ-auto".to_string(),
                1,
                0,
                "auto-dj".to_string(),
                None,
                None,
            ),
        ]
    );
    assert_eq!(
        sqlx::query_as::<_, (String, i64)>(
            "SELECT occurrence.object_id, state.shuffled
             FROM queue_state AS state
             JOIN queue_occurrences AS occurrence
               ON occurrence.queue_occurrence_key=state.current_occurrence_key",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered selected shuffled occurrence"),
        ("occ-context".to_string(), 1)
    );
    let source_key = sqlx::query_scalar::<_, i64>(
        "SELECT source_key FROM sources WHERE object_id='released-source'",
    )
    .fetch_one(&mut reader)
    .await
    .expect("read recovered source key");
    let canonical_plan = sqlx::query_as::<_, (i64, i64, i64, String)>(
        "EXPLAIN QUERY PLAN
         SELECT queue_occurrence_key FROM queue_occurrences
         WHERE source_key=?1 ORDER BY position",
    )
    .bind(source_key)
    .fetch_one(&mut reader)
    .await
    .expect("read canonical queue plan")
    .3;
    assert!(
        canonical_plan.contains("queue_occurrences_page_idx"),
        "{canonical_plan}"
    );
    let traversal_plan = sqlx::query_as::<_, (i64, i64, i64, String)>(
        "EXPLAIN QUERY PLAN
         SELECT queue_occurrence_key FROM queue_occurrences
         WHERE source_key=?1 ORDER BY traversal_position",
    )
    .bind(source_key)
    .fetch_one(&mut reader)
    .await
    .expect("read traversal queue plan")
    .3;
    assert!(
        traversal_plan.contains("queue_occurrences_traversal_idx"),
        "{traversal_plan}"
    );
    assert_eq!(
        sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                i64,
                String,
                String,
                String,
                i64,
                i64,
                String
            ),
        >(
            "SELECT date_added, source_format, comment, bpm,
                    musicbrainz_recording_id, musicbrainz_release_track_id,
                    cue_path, cue_start_millis, cue_end_millis, normalized_search
             FROM tracks WHERE object_id='released-track'",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered Track facts"),
        (
            "2024-01-02".to_string(),
            "FLAC".to_string(),
            "Released comment".to_string(),
            120,
            "recording-id".to_string(),
            "release-track-id".to_string(),
            "/music/album.cue".to_string(),
            1000,
            181000,
            "released track released album released artist released comment".to_string(),
        )
    );
    assert_eq!(
        sqlx::query_as::<_, (Option<String>, Option<String>, Option<Vec<u8>>)>(
            "SELECT fallback_album_object_id, fallback_primary_artist_object_id,
                    fallback_artwork_binding
             FROM queue_occurrences WHERE object_id='occ-context'",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered queue identity and Local artwork fallback"),
        (
            Some("released-album".to_string()),
            Some("released-artist".to_string()),
            Some(b"/music/local-cover.jpg".to_vec()),
        )
    );
    assert_eq!(
        sqlx::query_as::<_, (String, String, String, String, i64)>(
            "SELECT album.date_added, album.musicbrainz_release_id,
                    album.musicbrainz_release_group_id,
                    artist.musicbrainz_artist_id, length(genre.artwork_binding)
             FROM albums AS album, artists AS artist, genres AS genre",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered collection facts"),
        (
            "2024-01-02".to_string(),
            "release-id".to_string(),
            "release-group-id".to_string(),
            "artist-id".to_string(),
            21,
        )
    );
    assert_eq!(
        sqlx::query_as::<_, (String, String, String, String, String)>(
            "SELECT authority, role, language, script, hex(cache_input_digest)
             FROM lyrics_cache",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered lyrics identity"),
        (
            "source".to_string(),
            "Lyrics".to_string(),
            "en".to_string(),
            "Latn".to_string(),
            "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        )
    );
    assert_eq!(
        sqlx::query_as::<_, (bool, Option<String>)>(
            "SELECT is_compilation,release_lookup_identity FROM albums"
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered Album release facts"),
        (true, Some("release-group:release-group-id".to_string()))
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT release_type FROM album_release_types ORDER BY position"
        )
        .fetch_all(&mut reader)
        .await
        .expect("read recovered release lookup types"),
        ["Remix".to_string()]
    );
    assert_eq!(
        sqlx::query_as::<
            _,
            (
                i64,
                i64,
                i64,
                i64,
                i64,
                String,
                String,
                String,
                String,
                i64,
                i64
            ),
        >(
            "SELECT fallback_duration_millis, fallback_disc_number,
                    fallback_track_number, fallback_year, fallback_favorite,
                    fallback_media_uri,
                    fallback_source_format, fallback_musicbrainz_recording_id,
                    fallback_cue_path, fallback_cue_start_millis,
                    fallback_cue_end_millis
             FROM queue_occurrences WHERE object_id='occ-context'",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered queue fallback"),
        (
            180000,
            1,
            1,
            2024,
            1,
            "/music/track.flac".to_string(),
            "FLAC".to_string(),
            "recording-id".to_string(),
            "/music/album.cue".to_string(),
            1000,
            181000,
        )
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64, i64)>(
            "SELECT
                 (SELECT count(*) FROM album_artists),
                 (SELECT count(*) FROM track_moods),
                 (SELECT count(*) FROM album_release_types),
                 (SELECT count(*) FROM queue_occurrences),
                 (SELECT count(*) FROM local_file_dependencies),
                 (SELECT count(*) FROM local_access_files),
                 (SELECT count(*) FROM listen_outbox)",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered families"),
        (1, 1, 1, 2, 1, 1, 1)
    );
    assert_eq!(
        sqlx::query_as::<_, (f64, Option<f64>)>(
            "SELECT integrated_lufs,true_peak FROM loudness_measurements WHERE entity_kind='track'"
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered loudness without peak"),
        (-14.0, None)
    );
    assert_eq!(sqlx::query_as::<_,(i64,i64,Option<i64>)>("SELECT play_count,skip_count,last_played_at FROM activity_baseline WHERE track_object_id='released-track'").fetch_one(&mut reader).await.expect("read recovered Activity baseline"),(4,2,Some(90)));
    assert_eq!(
        sqlx::query_as::<_, (String, i64)>(
            "SELECT track_title,started_at FROM listens WHERE external_id='released-play'"
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered listen"),
        ("Released Track".to_string(), 95)
    );
    assert_eq!(
        sqlx::query_as::<_, (String, String, String)>(
            "SELECT normalized_title,normalized_album,media_uri FROM local_access_files"
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered Local access"),
        (
            "released track".to_string(),
            "released album".to_string(),
            "/music/track.flac".to_string()
        )
    );
    drop(database);
    let devel_path = directory.path().join("devel.sqlite3");
    let mut devel = connection(&devel_path, true).await;
    sqlx::raw_sql("PRAGMA application_id=1381320270; PRAGMA user_version=99; CREATE TABLE devel_only(value INTEGER) STRICT;").execute(&mut devel).await.expect("create intermediate Devel Store");
    devel.close().await.expect("close Devel Store");
    assert!(
        !Database::released_repair_available(&devel_path)
            .await
            .expect("inspect Devel Store")
    );
    assert!(Database::repair_released(&devel_path).await.is_err());
    assert!(
        devel_path.exists(),
        "explicit repair leaves Devel Store untouched"
    );
}

async fn create_released_store(path: &Path) {
    let mut connection = connection(path, true).await;
    sqlx::raw_sql(
        "PRAGMA application_id=1381320270;
         PRAGMA user_version=40;
         CREATE TABLE source_libraries(
             library_id INTEGER PRIMARY KEY, source_id TEXT NOT NULL,
             input_digest BLOB NOT NULL, content_digest BLOB,
             freshness_marker BLOB, accepted_at INTEGER
         ) STRICT;
         CREATE TABLE albums(
             library_id INTEGER NOT NULL, album_id TEXT NOT NULL,
             title TEXT NOT NULL, display_artist TEXT NOT NULL,
             year INTEGER, release_date TEXT, date_added TEXT,
             musicbrainz_release_id TEXT, musicbrainz_release_group_id TEXT,
             image_item_id TEXT, image_tag TEXT,
             favorite INTEGER NOT NULL, user_rating INTEGER,
             release_types_json TEXT NOT NULL, relations_json TEXT NOT NULL,
             is_compilation INTEGER
         ) STRICT;
         CREATE TABLE tracks(
             library_id INTEGER NOT NULL, track_id TEXT NOT NULL,
             album_id TEXT, title TEXT NOT NULL, display_album TEXT NOT NULL,
             display_artist TEXT NOT NULL, duration_seconds INTEGER NOT NULL,
             disc_number INTEGER NOT NULL, track_number INTEGER NOT NULL,
             year INTEGER, release_date TEXT, date_added TEXT, source_path TEXT,
             source_format TEXT, comment TEXT, bpm INTEGER,
             musicbrainz_recording_id TEXT, musicbrainz_release_track_id TEXT,
             cue_path TEXT, cue_start_millis INTEGER, cue_end_millis INTEGER,
             image_item_id TEXT, image_tag TEXT, favorite INTEGER NOT NULL,
             user_rating INTEGER, relations_json TEXT NOT NULL
         ) STRICT;
         CREATE TABLE artists(
             library_id INTEGER NOT NULL, artist_id TEXT NOT NULL, name TEXT NOT NULL,
             musicbrainz_artist_id TEXT,
             image_item_id TEXT, image_tag TEXT, favorite INTEGER NOT NULL,
             user_rating INTEGER
         ) STRICT;
         CREATE TABLE genres(
             library_id INTEGER NOT NULL, genre_id TEXT NOT NULL, name TEXT NOT NULL,
             image_item_id TEXT, image_tag TEXT
         ) STRICT;
         CREATE TABLE music_folders(
             library_id INTEGER NOT NULL, folder_id TEXT NOT NULL, name TEXT NOT NULL,
             image_item_id TEXT, image_tag TEXT
         ) STRICT;
         CREATE TABLE local_favorites(
             source_id TEXT NOT NULL, item_kind TEXT NOT NULL, item_id TEXT NOT NULL
         ) STRICT;
         CREATE TABLE user_ratings(
             source_id TEXT NOT NULL, item_kind TEXT NOT NULL,
             item_id TEXT NOT NULL, rating INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE playback_queues(
             source_id TEXT PRIMARY KEY, revision INTEGER NOT NULL,
             rows_json TEXT NOT NULL, traversal_json TEXT NOT NULL
         ) STRICT;
         CREATE TABLE playback_state(
             source_id TEXT PRIMARY KEY, revision INTEGER NOT NULL,
             selected_occurrence_id TEXT, progress_millis INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE local_files(
             library_id INTEGER NOT NULL, path TEXT NOT NULL, root TEXT NOT NULL,
             relative_path TEXT NOT NULL, kind TEXT NOT NULL, size_bytes INTEGER,
             mtime_ns INTEGER NOT NULL, device_id INTEGER, inode INTEGER,
             parse_version INTEGER, state TEXT NOT NULL, dependencies_json TEXT NOT NULL
         ) STRICT;
         CREATE TABLE local_access_files(
             source_id TEXT NOT NULL, path TEXT NOT NULL, root TEXT NOT NULL,
             relative_path TEXT NOT NULL, size_bytes INTEGER NOT NULL,
             mtime_ns INTEGER NOT NULL, device_id INTEGER, inode INTEGER,
             parser_version INTEGER NOT NULL, title TEXT NOT NULL,
             album TEXT NOT NULL, artist TEXT NOT NULL, disc_number INTEGER NOT NULL,
             track_number INTEGER NOT NULL, duration_seconds INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE pending_scrobbles(
             service TEXT NOT NULL, account_id TEXT NOT NULL, play_id TEXT NOT NULL,
             track_title TEXT NOT NULL, artist_name TEXT NOT NULL, album_title TEXT,
             duration_millis INTEGER NOT NULL, started_at INTEGER NOT NULL,
             attempts INTEGER NOT NULL, next_attempt_at INTEGER, last_error TEXT
         ) STRICT;
         CREATE TABLE lyrics_cache(
             source_id TEXT NOT NULL, track_id TEXT NOT NULL, role TEXT NOT NULL,
             language TEXT NOT NULL, script TEXT NOT NULL, origin TEXT NOT NULL,
             input_version INTEGER NOT NULL, input_digest BLOB NOT NULL,
             payload TEXT NOT NULL, cached_at INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE loudness_measurements(
             source_id TEXT NOT NULL, scope TEXT NOT NULL, item_id TEXT NOT NULL,
             analysis_key BLOB NOT NULL, integrated_lufs REAL, true_peak REAL
         ) STRICT;
         CREATE TABLE listening_aggregates(
             source_id TEXT NOT NULL, item_id TEXT NOT NULL, period TEXT NOT NULL,
             item_kind TEXT NOT NULL, play_count INTEGER NOT NULL,
             skip_count INTEGER NOT NULL, last_played_at INTEGER
         ) STRICT;
         CREATE TABLE recent_plays(
             play_id TEXT NOT NULL, source_id TEXT NOT NULL, track_id TEXT NOT NULL,
             track_title TEXT NOT NULL, artist_name TEXT NOT NULL,
             album_title TEXT, played_at INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE album_release_info(
             source_id TEXT NOT NULL,album_id TEXT NOT NULL,
             exact_identity_key TEXT NOT NULL,lookup_state TEXT NOT NULL,
             release_types_json TEXT,is_compilation INTEGER
         ) STRICT;
         INSERT INTO source_libraries VALUES(
             1, 'released-source', zeroblob(32), zeroblob(32), X'01', 1
         );
         INSERT INTO albums VALUES(
             1, 'released-album', 'Released Album', 'Released Artist',
             2024, '2024-01-01', '2024-01-02', 'release-id', 'release-group-id',
             NULL, NULL, 0, NULL, '[\"Album\"]',
             '{\"album_artists\":[{\"id\":\"released-artist\",\"name\":\"Released Artist\"}],\"genres\":[{\"id\":\"released-genre\",\"name\":\"Released Genre\"}]}', 1
         );
         INSERT INTO tracks VALUES(
             1, 'released-track', 'released-album', 'Released Track',
             'Released Album', 'Released Artist', 180, 1, 1, 2024,
             '2024-01-01', '2024-01-02', '/music/track.flac', 'FLAC',
             'Released comment', 120, 'recording-id', 'release-track-id',
             '/music/album.cue', 1000, 181000, NULL, NULL, 0, NULL,
             '{\"artists\":[{\"id\":\"released-artist\",\"name\":\"Released Artist\"}],\"genres\":[{\"id\":\"released-genre\",\"name\":\"Released Genre\"}],\"moods\":[{\"id\":\"released-mood\",\"name\":\"Released Mood\"}],\"music_folders\":[\"released-folder\"]}'
         );
         INSERT INTO artists VALUES(
             1, 'released-artist', 'Released Artist', 'artist-id', NULL, NULL, 0, NULL
         );
         INSERT INTO genres VALUES(
             1, 'released-genre', 'Released Genre', 'genre-image', 'genre-tag'
         );
         INSERT INTO music_folders VALUES(
             1, 'released-folder', 'Released Folder', NULL, NULL
         );
         INSERT INTO local_favorites VALUES(
             'released-source', 'track', 'released-track'
         );
         INSERT INTO user_ratings VALUES(
             'released-source', 'track', 'released-track', 8
         );
         INSERT INTO playback_queues VALUES(
             'released-source', 1,
             '{\"occurrences\":[{\"id\":\"occ-context\",\"track_id\":\"released-track\",\"provenance\":{\"Context\":{\"context_id\":\"route-album\",\"source_rank\":7}}},{\"id\":\"occ-auto\",\"track_id\":\"released-track\",\"provenance\":\"AutoDj\"}],\"fallback_tracks\":[{\"id\":\"released-track\",\"title\":\"Released Track\",\"artist\":\"Released Artist\",\"album\":\"Released Album\",\"album_id\":\"released-album\",\"primary_artist_id\":\"released-artist\",\"local_artwork\":\"/music/local-cover.jpg\",\"year\":2024,\"duration_seconds\":180,\"favorite\":true,\"track_number\":1,\"disc_number\":1,\"source_format\":\"FLAC\",\"source_path\":\"/music/track.flac\",\"musicbrainz_recording_id\":\"recording-id\",\"cue\":{\"cue_path\":\"/music/album.cue\",\"start_millis\":1000,\"end_millis\":181000}}]}',
             '[\"occ-auto\",\"occ-context\"]'
         );
         INSERT INTO playback_state VALUES('released-source', 1, 'occ-context', 1200);
         INSERT INTO local_files VALUES(
             1, '/music/track.flac', '/music', 'track.flac', 'media', 100,
             10, 1, 2, 1, 'accepted', '[\"/music/image.jpg\"]'
         );
         INSERT INTO local_access_files VALUES(
             'released-source', '/music/track.flac', '/music', 'track.flac',
             100, 10, 1, 2, 1, 'Released Track', 'Released Album',
             'Released Artist', 1, 1, 180
         );
         INSERT INTO pending_scrobbles VALUES(
             'lastfm', 'account', 'pending-play', 'Pending Track',
             'Pending Artist', NULL, 180000, 100, 0, 200, NULL
         );
         INSERT INTO lyrics_cache VALUES(
             'released-source', 'released-track', 'Lyrics', 'en', 'Latn',
             'source', 1, zeroblob(32), 'Released lyrics', 100
         );
         INSERT INTO loudness_measurements VALUES(
             'released-source','track','released-track',zeroblob(32),-14.0,NULL
         );
         INSERT INTO listening_aggregates VALUES(
             'released-source','released-track','lifetime','track',4,2,90
         );
         INSERT INTO recent_plays VALUES(
             'released-play','released-source','released-track','Released Track',
             'Released Artist','Released Album',95
         );
         INSERT INTO album_release_info VALUES(
             'released-source','released-album','release-group:release-group-id',
             'found','[\"Remix\"]',NULL
         );",
    )
    .execute(&mut connection)
    .await
    .expect("create released Store fixture");
    connection.close().await.expect("close released fixture");
}
