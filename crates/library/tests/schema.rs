use std::path::Path;

use library::{CalendarActivityPeriod, Database, ReadCancellation, SourceKey};
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
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut reader)
            .await
            .expect("read final schema version"),
        41
    );
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
async fn schema_40_store_migrates_to_released_schema_41() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    create_released_store(&path).await;
    let database = Database::open(&path)
        .await
        .expect("migrate and open released Store");
    assert!(
        std::fs::read_dir(directory.path())
            .expect("read Store directory")
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("library.sqlite3.schema-40-")),
        "migration preserves the schema-40 Store beside schema 41"
    );
    let mut reader = connection(&path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut reader)
            .await
            .expect("read recovered schema version"),
        41
    );
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
        sqlx::query_scalar::<_, i64>(
            "SELECT first_seen_at FROM tracks WHERE object_id='released-track'"
        )
        .fetch_one(&mut reader)
        .await
        .expect("read migrated first-seen fact"),
        50
    );
    assert_eq!(
        sqlx::query_as::<_, (String, String, i64)>(
            "SELECT playlist.name, playlist.ownership, count(entry.playlist_entry_key)
             FROM playlists AS playlist
             LEFT JOIN playlist_entries AS entry USING (playlist_key)
             GROUP BY playlist.playlist_key ORDER BY playlist.ownership"
        )
        .fetch_all(&mut reader)
        .await
        .expect("read migrated source and user Playlists"),
        [
            ("Source List".to_string(), "source".to_string(), 1),
            ("User List".to_string(), "user".to_string(), 2),
        ]
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT playlist_entries.object_id FROM playlist_entries
             JOIN playlists USING (playlist_key)
             WHERE playlists.object_id='user-list' ORDER BY position"
        )
        .fetch_all(&mut reader)
        .await
        .expect("read migrated duplicate Playlist occurrences"),
        [
            "user-occurrence-one".to_string(),
            "user-occurrence-two".to_string()
        ]
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT object_id FROM smart_playlists")
            .fetch_one(&mut reader)
            .await
            .expect("read migrated Smart Playlist"),
        "smart-list"
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT favorite,attempts,next_attempt_at FROM favorite_outbox"
        )
        .fetch_one(&mut reader)
        .await
        .expect("read migrated Favorite outbox"),
        (1, 2, 200)
    );
    assert_eq!(
        sqlx::query_as::<_, (Option<String>, Option<String>)>(
            "SELECT media_uri,source_path FROM tracks WHERE object_id='released-track'",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered provider path"),
        (None, Some("/music/track.flac".to_string()))
    );
    assert_eq!(
        sqlx::query_as::<_, (String, String, i64)>("SELECT period,item_kind,play_count FROM activity_baseline WHERE source_key=(SELECT source_key FROM sources WHERE object_id='released-source') ORDER BY period")
            .fetch_all(&mut reader).await.expect("read recovered Activity periods"),
        [("2025-06".to_string(), "track".to_string(), 3), ("lifetime".to_string(), "track".to_string(), 4)]
    );
    let month = database
        .calendar_activity_summary(
            SourceKey::from_raw(1),
            CalendarActivityPeriod::Month {
                year: 2025,
                month: 6,
            },
            10,
            &ReadCancellation::new(),
        )
        .await
        .expect("query recovered calendar Activity");
    assert_eq!(month.tracks[0].play_count, 3);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM queue_occurrences")
            .fetch_one(&mut reader)
            .await
            .expect("read intentionally fresh schema-41 Queue"),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM home_entries WHERE source_key=1 AND section_id='most-played'"
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered Home"),
        1
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
            43,
        )
    );
    let genre_binding = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT artwork_binding FROM genres WHERE object_id='released-genre'",
    )
    .fetch_one(&mut reader)
    .await
    .expect("read recovered Genre artwork binding");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&genre_binding)
            .expect("decode recovered native artwork binding"),
        serde_json::json!({"item_id":"genre-image","tag":"genre-tag"})
    );
    let (album_binding, track_binding, artist_binding) =
        sqlx::query_as::<_, (Vec<u8>, Vec<u8>, Vec<u8>)>(
            "SELECT album.artwork_binding,track.artwork_binding,artist.artwork_binding
             FROM albums album,tracks track,artists artist",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered Local artwork bindings");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&album_binding).expect("Album artwork"),
        serde_json::json!({"File":{"path":"/music/cover.jpg","revision":"album-rev"}})
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&track_binding).expect("Track artwork"),
        serde_json::json!({"Embedded":{"path":"/music/track.flac","picture_index":2,"revision":"track-rev"}})
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&artist_binding).expect("Artist artwork"),
        serde_json::json!({"File":{"path":"/music/artist.jpg","revision":"artist-rev"}})
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
        sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64)>(
            "SELECT
                 (SELECT count(*) FROM album_artists),
                 (SELECT count(*) FROM track_moods),
                 (SELECT count(*) FROM album_release_types),
                 (SELECT count(*) FROM local_file_dependencies),
                 (SELECT count(*) FROM local_access_files),
                 (SELECT count(*) FROM listen_outbox)",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered families"),
        (1, 1, 1, 1, 1, 1)
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
        sqlx::query_as::<_, (String, String, String, String)>(
            "SELECT normalized_title,normalized_album,media_uri,origin FROM local_access_files"
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered Local access"),
        (
            "released track".to_string(),
            "released album".to_string(),
            "file:///music/track.flac".to_string(),
            "mapping".to_string()
        )
    );
    drop(database);
    let devel_path = directory.path().join("devel.sqlite3");
    let mut devel = connection(&devel_path, true).await;
    sqlx::raw_sql("PRAGMA application_id=1381320270; PRAGMA user_version=42; CREATE TABLE devel_only(value INTEGER) STRICT;").execute(&mut devel).await.expect("create unreleased schema-42 Store");
    devel.close().await.expect("close Devel Store");
    let _database = Database::open(&devel_path)
        .await
        .expect("shared automatic fallback rebuilds unsupported Store content");
    let mut rebuilt = connection(&devel_path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut rebuilt)
            .await
            .expect("read rebuilt final schema version"),
        41
    );
    assert!(
        devel_path.exists(),
        "automatic fallback reopens a usable Store at the configured path"
    );
    assert!(
        std::fs::read_dir(directory.path())
            .expect("read Store directory")
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("devel.sqlite3.recovered-")),
        "automatic fallback preserves unsupported Store content"
    );
}

#[tokio::test]
async fn failed_schema_40_migration_restores_the_original_store() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    create_released_store(&path).await;
    let mut released = connection(&path, false).await;
    sqlx::query("UPDATE tracks SET duration_seconds=-1 WHERE track_id='released-track'")
        .execute(&mut released)
        .await
        .expect("make one schema-40 value invalid for schema 41");
    released.close().await.expect("close schema-40 fixture");

    assert!(
        Database::open(&path).await.is_err(),
        "invalid copied data must roll back the migration"
    );

    let mut restored = connection(&path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut restored)
            .await
            .expect("read restored schema version"),
        40
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT duration_seconds FROM tracks WHERE track_id='released-track'"
        )
        .fetch_one(&mut restored)
        .await
        .expect("read restored schema-40 Track"),
        -1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='sources'"
        )
        .fetch_one(&mut restored)
        .await
        .expect("check for partial schema-41 tables"),
        0
    );
}

#[tokio::test]
async fn recognizable_released_store_salvages_readable_families_independently() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    create_released_store(&path).await;
    let mut released = connection(&path, false).await;
    sqlx::query("DROP TABLE genres")
        .execute(&mut released)
        .await
        .expect("remove one released family");
    released.close().await.expect("close partial Store");

    let database = Database::open(&path)
        .await
        .expect("salvage recognizable partial released Store");
    let mut reader = connection(&path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sources")
            .fetch_one(&mut reader)
            .await
            .expect("read salvaged source family"),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut reader)
            .await
            .expect("read salvaged Track family"),
        1
    );
    drop(database);
}

#[tokio::test]
async fn not_a_database_is_preserved_and_rebuilt_automatically() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    std::fs::write(&path, b"not a sqlite database").expect("write invalid Store");

    let _database = Database::open(&path)
        .await
        .expect("automatically preserve and rebuild invalid Store content");
    assert!(
        std::fs::read_dir(directory.path())
            .expect("read Store directory")
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("library.sqlite3.recovered-"))
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
             freshness_marker BLOB, home_digest BLOB, home_json TEXT, accepted_at INTEGER
         ) STRICT;
         CREATE TABLE albums(
             library_id INTEGER NOT NULL, album_id TEXT NOT NULL,
             title TEXT NOT NULL, display_artist TEXT NOT NULL,
             year INTEGER, release_date TEXT, date_added TEXT,
             musicbrainz_release_id TEXT, musicbrainz_release_group_id TEXT,
             image_item_id TEXT, image_tag TEXT,
             local_artwork_kind TEXT, local_artwork_path TEXT,
             local_artwork_picture_index INTEGER, local_artwork_revision TEXT,
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
             image_item_id TEXT, image_tag TEXT,
             local_artwork_kind TEXT, local_artwork_path TEXT,
             local_artwork_picture_index INTEGER, local_artwork_revision TEXT,
             favorite INTEGER NOT NULL,
             user_rating INTEGER, relations_json TEXT NOT NULL
         ) STRICT;
         CREATE TABLE artists(
             library_id INTEGER NOT NULL, artist_id TEXT NOT NULL, name TEXT NOT NULL,
             musicbrainz_artist_id TEXT,
             image_item_id TEXT, image_tag TEXT,
             local_artwork_kind TEXT, local_artwork_path TEXT,
             local_artwork_picture_index INTEGER, local_artwork_revision TEXT,
             favorite INTEGER NOT NULL,
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
         CREATE TABLE source_playlists(
             library_id INTEGER NOT NULL, playlist_id TEXT NOT NULL,
             name TEXT NOT NULL, image_item_id TEXT, image_tag TEXT
         ) STRICT;
         CREATE TABLE source_playlist_entries(
             library_id INTEGER NOT NULL, playlist_id TEXT NOT NULL,
             position INTEGER NOT NULL, occurrence_id TEXT NOT NULL,
             track_id TEXT NOT NULL
         ) STRICT;
         CREATE TABLE local_favorites(
             source_id TEXT NOT NULL, item_kind TEXT NOT NULL, item_id TEXT NOT NULL
         ) STRICT;
         CREATE TABLE user_ratings(
             source_id TEXT NOT NULL, item_kind TEXT NOT NULL,
             item_id TEXT NOT NULL, rating INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE pending_favorites(
             source_id TEXT NOT NULL, item_kind TEXT NOT NULL,
             item_id TEXT NOT NULL, favorite INTEGER NOT NULL,
             previous_favorite INTEGER NOT NULL, attempts INTEGER NOT NULL,
             next_attempt_at INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE local_playlists(
             source_id TEXT NOT NULL, playlist_id TEXT NOT NULL, name TEXT NOT NULL
         ) STRICT;
         CREATE TABLE local_playlist_entries(
             source_id TEXT NOT NULL, playlist_id TEXT NOT NULL,
             position INTEGER NOT NULL, occurrence_id TEXT NOT NULL,
             track_id TEXT NOT NULL
         ) STRICT;
         CREATE TABLE smart_playlists(
             source_id TEXT NOT NULL, smart_playlist_id TEXT NOT NULL,
             name TEXT NOT NULL, builtin_key TEXT, definition_json TEXT NOT NULL,
             position INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE local_imports(
             source_id TEXT NOT NULL, track_id TEXT NOT NULL,
             first_seen_at INTEGER NOT NULL
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
             1, 'released-source', zeroblob(32), zeroblob(32), X'01', zeroblob(32),
             '{\"kind\":\"Source\",\"sections\":[{\"kind\":\"MostPlayed\",\"items\":[{\"kind\":\"track\",\"id\":\"released-track\"}]}]}', 1
         );
         INSERT INTO albums VALUES(
             1, 'released-album', 'Released Album', 'Released Artist',
             2024, '2024-01-01', '2024-01-02', 'release-id', 'release-group-id',
             NULL, NULL, 'file', '/music/cover.jpg', NULL, 'album-rev',
             0, NULL, '[\"Album\"]',
             '{\"album_artists\":[{\"id\":\"released-artist\",\"name\":\"Released Artist\"}],\"genres\":[{\"id\":\"released-genre\",\"name\":\"Released Genre\"}]}', 1
         );
         INSERT INTO tracks VALUES(
             1, 'released-track', 'released-album', 'Released Track',
             'Released Album', 'Released Artist', 180, 1, 1, 2024,
             '2024-01-01', '2024-01-02', '/music/track.flac', 'FLAC',
             'Released comment', 120, 'recording-id', 'release-track-id',
             '/music/album.cue', 1000, 181000, NULL, NULL,
             'embedded', '/music/track.flac', 2, 'track-rev', 0, NULL,
             '{\"artists\":[{\"id\":\"released-artist\",\"name\":\"Released Artist\"}],\"genres\":[{\"id\":\"released-genre\",\"name\":\"Released Genre\"}],\"moods\":[{\"id\":\"released-mood\",\"name\":\"Released Mood\"}],\"music_folders\":[\"released-folder\"]}'
         );
         INSERT INTO artists VALUES(
             1, 'released-artist', 'Released Artist', 'artist-id', NULL, NULL,
             'file', '/music/artist.jpg', NULL, 'artist-rev', 0, NULL
         );
         INSERT INTO genres VALUES(
             1, 'released-genre', 'Released Genre', 'genre-image', 'genre-tag'
         );
         INSERT INTO music_folders VALUES(
             1, 'released-folder', 'Released Folder', NULL, NULL
         );
         INSERT INTO source_playlists VALUES(
             1, 'source-list', 'Source List', NULL, NULL
         );
         INSERT INTO source_playlist_entries VALUES(
             1, 'source-list', 0, 'source-occurrence', 'released-track'
         );
         INSERT INTO local_favorites VALUES(
             'released-source', 'track', 'released-track'
         );
         INSERT INTO user_ratings VALUES(
             'released-source', 'track', 'released-track', 8
         );
         INSERT INTO pending_favorites VALUES(
             'released-source', 'track', 'released-track', 1, 0, 2, 200
         );
         INSERT INTO local_playlists VALUES(
             'released-source', 'user-list', 'User List'
         );
         INSERT INTO local_playlist_entries VALUES(
             'released-source', 'user-list', 0, 'user-occurrence-one', 'released-track'
         );
         INSERT INTO local_playlist_entries VALUES(
             'released-source', 'user-list', 1, 'user-occurrence-two', 'released-track'
         );
         INSERT INTO smart_playlists VALUES(
             'released-source', 'smart-list', 'Smart List', NULL,
             '{\"match_all\":[],\"match_any\":[],\"sort_field\":\"Title\",\"descending\":false}', 0
         );
         INSERT INTO local_imports VALUES(
             'released-source', 'released-track', 50
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
         INSERT INTO listening_aggregates VALUES(
             'released-source','released-track','2025-06','track',3,0,NULL
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
