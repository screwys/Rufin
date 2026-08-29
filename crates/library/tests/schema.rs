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
        42
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
            "replay_gain_measurements",
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
async fn schema_41_migrates_in_place_to_schema_42() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("create current Store");
    drop(database);

    let mut schema_41 = connection(&path, false).await;
    sqlx::raw_sql(
        "INSERT INTO sources(
             source_key,object_id,display_name,normalized_name,
             catalog_digest,artwork_digest
         ) VALUES(1,'source','Source','source',zeroblob(32),zeroblob(32));
         INSERT INTO tracks(
             track_key,source_key,object_id,title,normalized_search,
             display_album,display_artist,sort_text,duration_millis
         ) VALUES(1,1,'track','Track','track','Album','Artist','track',180000);
         INSERT INTO loudness_measurements(
             source_key,entity_kind,entity_key,analysis_key,
             integrated_lufs,true_peak,origin
         ) VALUES(1,'track',1,zeroblob(32),-18.0,0.9,'analysis');
         DROP TABLE replay_gain_measurements;
         PRAGMA user_version=41;",
    )
    .execute(&mut schema_41)
    .await
    .expect("create schema-41 fixture");
    schema_41.close().await.expect("close schema-41 fixture");

    let _database = Database::open(&path)
        .await
        .expect("migrate schema 41 in place");
    let mut reader = connection(&path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut reader)
            .await
            .expect("read migrated version"),
        42
    );
    assert_eq!(
        sqlx::query_scalar::<_, f64>(
            "SELECT integrated_lufs FROM loudness_measurements WHERE entity_kind='track'"
        )
        .fetch_one(&mut reader)
        .await
        .expect("read preserved R128 measurement"),
        -18.0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='replay_gain_measurements'"
        )
        .fetch_one(&mut reader)
        .await
        .expect("read ReplayGain table"),
        1
    );
    assert_eq!(
        std::fs::read_dir(directory.path())
            .expect("read Store directory")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("recovered"))
            .count(),
        0,
        "an ordinary schema migration must not replace the Store"
    );
}

#[tokio::test]
async fn schema_40_store_migrates_into_current_schema() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    create_legacy_store(&path).await;
    let database = Database::open(&path)
        .await
        .expect("migrate and open schema-40 Store");
    assert!(
        std::fs::read_dir(directory.path())
            .expect("read Store directory")
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("library.sqlite3.schema-40-")),
        "migration preserves the schema-40 input"
    );
    let mut reader = connection(&path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut reader)
            .await
            .expect("read recovered schema version"),
        42
    );
    assert_eq!(
        sqlx::query_as::<_, (String, i64)>("SELECT object_id, catalog_revision FROM sources",)
            .fetch_one(&mut reader)
            .await
            .expect("read recovered source"),
        ("legacy-source".to_string(), 1)
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT user_favorite, user_rating FROM tracks
             WHERE object_id='legacy-track'",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered user facts"),
        (1, 80)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT first_seen_at FROM tracks WHERE object_id='legacy-track'"
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
            "SELECT media_uri,source_path FROM tracks WHERE object_id='legacy-track'",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered provider path"),
        (None, Some("/music/track.flac".to_string()))
    );
    assert_eq!(
        sqlx::query_as::<_, (String, String, i64)>("SELECT period,item_kind,play_count FROM activity_baseline WHERE source_key=(SELECT source_key FROM sources WHERE object_id='legacy-source') ORDER BY period")
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
        sqlx::query_scalar::<_, String>(
            "SELECT section_id FROM home_entries WHERE source_key=1 ORDER BY section_id"
        )
        .fetch_all(&mut reader)
        .await
        .expect("read recovered Home"),
        [
            "most-played".to_string(),
            "newly-added".to_string(),
            "recently-played".to_string(),
            "recently-released".to_string(),
        ]
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
             FROM tracks WHERE object_id='legacy-track'",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered Track facts"),
        (
            "2024-01-02".to_string(),
            "FLAC".to_string(),
            "Legacy comment".to_string(),
            120,
            "recording-id".to_string(),
            "release-track-id".to_string(),
            "/music/album.cue".to_string(),
            1000,
            181000,
            "legacy track legacy album legacy artist legacy comment".to_string(),
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
        "SELECT artwork_binding FROM genres WHERE object_id='legacy-genre'",
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
        sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64)>(
            "SELECT
                 (SELECT count(*) FROM artists),
                 (SELECT count(*) FROM genres),
                 (SELECT count(*) FROM album_artists),
                 (SELECT count(*) FROM track_artists),
                 (SELECT count(*) FROM album_genres),
                 (SELECT count(*) FROM track_genres),
                 (SELECT count(*) FROM track_moods),
                 (SELECT count(*) FROM album_release_types),
                 (SELECT count(*) FROM local_file_dependencies),
                 (SELECT count(*) FROM local_access_files),
                 (SELECT count(*) FROM listen_outbox)",
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered families"),
        (3, 3, 2, 2, 2, 2, 1, 1, 1, 1, 1)
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
    assert_eq!(sqlx::query_as::<_,(i64,i64,Option<i64>)>("SELECT play_count,skip_count,last_played_at FROM activity_baseline WHERE track_object_id='legacy-track'").fetch_one(&mut reader).await.expect("read recovered Activity baseline"),(4,2,Some(90)));
    assert_eq!(
        sqlx::query_as::<_, (String, i64)>(
            "SELECT track_title,started_at FROM listens WHERE external_id='legacy-play'"
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered listen"),
        ("Legacy Track".to_string(), 95)
    );
    assert_eq!(
        sqlx::query_as::<_, (String, String, String, String)>(
            "SELECT normalized_title,normalized_album,media_uri,origin FROM local_access_files"
        )
        .fetch_one(&mut reader)
        .await
        .expect("read recovered Local access"),
        (
            "legacy track".to_string(),
            "legacy album".to_string(),
            "file:///music/track.flac".to_string(),
            "mapping".to_string()
        )
    );
    drop(database);
    let devel_path = directory.path().join("devel.sqlite3");
    let mut devel = connection(&devel_path, true).await;
    sqlx::raw_sql("PRAGMA application_id=1381320270; PRAGMA user_version=43; CREATE TABLE devel_only(value INTEGER) STRICT;").execute(&mut devel).await.expect("create unsupported schema-43 Store");
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
        42
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
async fn schema_39_store_runs_the_committed_chain_into_current_schema() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    create_legacy_store(&path).await;
    let mut schema_39 = connection(&path, false).await;
    sqlx::raw_sql(
        "DROP TABLE user_ratings;
         UPDATE smart_playlists SET definition_json=
           '{\"match_all\":[{\"field\":\"Rating\",\"operator\":\"Above\",\"value\":{\"Number\":4}}],\"match_any\":[],\"sort_field\":\"Title\",\"descending\":false}';
         PRAGMA user_version=39;",
    )
    .execute(&mut schema_39)
    .await
    .expect("restore schema-39 facts");
    schema_39.close().await.expect("close schema-39 fixture");

    let _database = Database::open(&path)
        .await
        .expect("run the committed migration chain before opening the current Store");
    let store_files = std::fs::read_dir(directory.path())
        .expect("read Store directory")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(
        store_files
            .iter()
            .any(|name| name.starts_with("library.sqlite3.schema-40-")),
        "migration preserves its normalized schema-40 input"
    );
    assert!(
        !store_files
            .iter()
            .any(|name| name.starts_with("library.sqlite3.recovered-")),
        "an intact known schema must not be routed through recovery"
    );
    let mut reader = connection(&path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut reader)
            .await
            .expect("read current schema version"),
        42
    );
    let definition = sqlx::query_scalar::<_, String>(
        "SELECT definition_json FROM smart_playlists WHERE object_id='smart-list'",
    )
    .fetch_one(&mut reader)
    .await
    .expect("read migrated Smart Playlist");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&definition)
            .expect("parse migrated Smart Playlist")["match_all"][0]["value"]["Number"],
        8
    );
}

#[tokio::test]
async fn failed_known_schema_migration_repairs_readable_families_and_opens() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    create_legacy_store(&path).await;
    let mut legacy = connection(&path, false).await;
    sqlx::raw_sql(
        "DROP TABLE user_ratings;
         UPDATE tracks SET duration_seconds=-1 WHERE track_id='legacy-track';
         PRAGMA user_version=39;",
    )
    .execute(&mut legacy)
    .await
    .expect("make one known-schema value invalid for the current schema");
    legacy.close().await.expect("close known-schema fixture");

    let _database = Database::open(&path)
        .await
        .expect("failed normal migration must fall through to automatic repair");

    let mut restored = connection(&path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut restored)
            .await
            .expect("read repaired schema version"),
        42
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sources")
            .fetch_one(&mut restored)
            .await
            .expect("read repaired sources"),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut restored)
            .await
            .expect("read skipped invalid Tracks family"),
        0
    );
    assert!(
        std::fs::read_dir(directory.path())
            .expect("read Store directory")
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("library.sqlite3.recovered-")),
        "automatic repair preserves the failed migration input"
    );
}

#[tokio::test]
async fn recognizable_legacy_store_salvages_readable_families_independently() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    create_legacy_store(&path).await;
    let mut legacy = connection(&path, false).await;
    sqlx::query("DROP TABLE genres")
        .execute(&mut legacy)
        .await
        .expect("remove one legacy family");
    legacy.close().await.expect("close partial Store");

    let database = Database::open(&path)
        .await
        .expect("salvage recognizable partial legacy Store");
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

async fn create_legacy_store(path: &Path) {
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
             1, 'legacy-source', zeroblob(32), zeroblob(32), X'01', zeroblob(32),
             '{\"kind\":\"Source\",\"sections\":[{\"kind\":\"MostPlayed\",\"items\":[{\"kind\":\"track\",\"id\":\"legacy-track\"}]},{\"kind\":\"NewlyAdded\",\"items\":[{\"kind\":\"track\",\"id\":\"legacy-track\"}]},{\"kind\":\"RecentlyPlayed\",\"items\":[{\"kind\":\"track\",\"id\":\"legacy-track\"}]},{\"kind\":\"RecentlyReleased\",\"items\":[{\"kind\":\"track\",\"id\":\"legacy-track\"}]}]}', 1
         );
         INSERT INTO albums VALUES(
             1, 'legacy-album', 'Legacy Album', 'Legacy Artist',
             2024, '2024-01-01', '2024-01-02', 'release-id', 'release-group-id',
             NULL, NULL, 'file', '/music/cover.jpg', NULL, 'album-rev',
             0, NULL, '[\"Album\"]',
             '{\"album_artists\":[{\"id\":\"legacy-artist\",\"name\":\"Legacy Artist\"},{\"id\":\"album-credit-only\",\"name\":\"Album Credit\"}],\"genres\":[{\"id\":\"legacy-genre\",\"name\":\"Legacy Genre\"},{\"id\":\"album-genre-only\",\"name\":\"Album Genre\"}]}', 1
         );
         INSERT INTO tracks VALUES(
             1, 'legacy-track', 'legacy-album', 'Legacy Track',
             'Legacy Album', 'Legacy Artist', 180, 1, 1, 2024,
             '2024-01-01', '2024-01-02', '/music/track.flac', 'FLAC',
             'Legacy comment', 120, 'recording-id', 'release-track-id',
             '/music/album.cue', 1000, 181000, NULL, NULL,
             'embedded', '/music/track.flac', 2, 'track-rev', 0, NULL,
             '{\"artists\":[{\"id\":\"legacy-artist\",\"name\":\"Legacy Artist\"},{\"id\":\"track-credit-only\",\"name\":\"Track Credit\"}],\"genres\":[{\"id\":\"legacy-genre\",\"name\":\"Legacy Genre\"},{\"id\":\"track-genre-only\",\"name\":\"Track Genre\"}],\"moods\":[{\"id\":\"legacy-mood\",\"name\":\"Legacy Mood\"}],\"music_folders\":[\"legacy-folder\"]}'
         );
         INSERT INTO artists VALUES(
             1, 'legacy-artist', 'Legacy Artist', 'artist-id', NULL, NULL,
             'file', '/music/artist.jpg', NULL, 'artist-rev', 0, NULL
         );
         INSERT INTO genres VALUES(
             1, 'legacy-genre', 'Legacy Genre', 'genre-image', 'genre-tag'
         );
         INSERT INTO music_folders VALUES(
             1, 'legacy-folder', 'Legacy Folder', NULL, NULL
         );
         INSERT INTO source_playlists VALUES(
             1, 'source-list', 'Source List', NULL, NULL
         );
         INSERT INTO source_playlist_entries VALUES(
             1, 'source-list', 0, 'source-occurrence', 'legacy-track'
         );
         INSERT INTO local_favorites VALUES(
             'legacy-source', 'track', 'legacy-track'
         );
         INSERT INTO user_ratings VALUES(
             'legacy-source', 'track', 'legacy-track', 8
         );
         INSERT INTO pending_favorites VALUES(
             'legacy-source', 'track', 'legacy-track', 1, 0, 2, 200
         );
         INSERT INTO local_playlists VALUES(
             'legacy-source', 'user-list', 'User List'
         );
         INSERT INTO local_playlist_entries VALUES(
             'legacy-source', 'user-list', 0, 'user-occurrence-one', 'legacy-track'
         );
         INSERT INTO local_playlist_entries VALUES(
             'legacy-source', 'user-list', 1, 'user-occurrence-two', 'legacy-track'
         );
         INSERT INTO smart_playlists VALUES(
             'legacy-source', 'smart-list', 'Smart List', NULL,
             '{\"match_all\":[],\"match_any\":[],\"sort_field\":\"Title\",\"descending\":false}', 0
         );
         INSERT INTO local_imports VALUES(
             'legacy-source', 'legacy-track', 50
         );
         INSERT INTO playback_queues VALUES(
             'legacy-source', 1,
             '{\"occurrences\":[{\"id\":\"occ-context\",\"track_id\":\"legacy-track\",\"provenance\":{\"Context\":{\"context_id\":\"route-album\",\"source_rank\":7}}},{\"id\":\"occ-auto\",\"track_id\":\"legacy-track\",\"provenance\":\"AutoDj\"}],\"fallback_tracks\":[{\"id\":\"legacy-track\",\"title\":\"Legacy Track\",\"artist\":\"Legacy Artist\",\"album\":\"Legacy Album\",\"album_id\":\"legacy-album\",\"primary_artist_id\":\"legacy-artist\",\"local_artwork\":\"/music/local-cover.jpg\",\"year\":2024,\"duration_seconds\":180,\"favorite\":true,\"track_number\":1,\"disc_number\":1,\"source_format\":\"FLAC\",\"source_path\":\"/music/track.flac\",\"musicbrainz_recording_id\":\"recording-id\",\"cue\":{\"cue_path\":\"/music/album.cue\",\"start_millis\":1000,\"end_millis\":181000}}]}',
             '[\"occ-auto\",\"occ-context\"]'
         );
         INSERT INTO playback_state VALUES('legacy-source', 1, 'occ-context', 1200);
         INSERT INTO local_files VALUES(
             1, '/music/track.flac', '/music', 'track.flac', 'media', 100,
             10, 1, 2, 1, 'accepted', '[\"/music/image.jpg\"]'
         );
         INSERT INTO local_access_files VALUES(
             'legacy-source', '/music/track.flac', '/music', 'track.flac',
             100, 10, 1, 2, 1, 'Legacy Track', 'Legacy Album',
             'Legacy Artist', 1, 1, 180
         );
         INSERT INTO pending_scrobbles VALUES(
             'lastfm', 'account', 'pending-play', 'Pending Track',
             'Pending Artist', NULL, 180000, 100, 0, 200, NULL
         );
         INSERT INTO lyrics_cache VALUES(
             'legacy-source', 'legacy-track', 'Lyrics', 'en', 'Latn',
             'source', 1, zeroblob(32), 'Legacy lyrics', 100
         );
         INSERT INTO loudness_measurements VALUES(
             'legacy-source','track','legacy-track',zeroblob(32),-14.0,NULL
         );
         INSERT INTO listening_aggregates VALUES(
             'legacy-source','legacy-track','lifetime','track',4,2,90
         );
         INSERT INTO listening_aggregates VALUES(
             'legacy-source','legacy-track','2025-06','track',3,0,NULL
         );
         INSERT INTO recent_plays VALUES(
             'legacy-play','legacy-source','legacy-track','Legacy Track',
             'Legacy Artist','Legacy Album',95
         );
         INSERT INTO album_release_info VALUES(
             'legacy-source','legacy-album','release-group:release-group-id',
             'found','[\"Remix\"]',NULL
         );",
    )
    .execute(&mut connection)
    .await
    .expect("create legacy Store fixture");
    connection.close().await.expect("close legacy fixture");
}
