use std::path::Path;

use library::{Catalog, ReadCancellation, SourceId};
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

#[tokio::test]
async fn reopening_without_statistics_preserves_library_order() {
    let fixture = super::support::fixture().await;
    let before = fixture
        .catalog
        .selected_track_uris(
            &library::MemberRead {
                collection: library::Collection::Source(fixture.source),
                sort: Some(library::CatalogSort::Title),
                ..library::MemberRead::new(library::Collection::Source(fixture.source))
            }
            .selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .unwrap();
    let mut raw = super::support::connection(&fixture.path).await;
    sqlx::raw_sql("DROP TABLE IF EXISTS sqlite_stat1;
        INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped)
            SELECT media_uri,'Captured title','Captured artist','Captured album',100,'1970-01',180000,90000,1 FROM tracks LIMIT 1")
        .execute(&mut raw)
        .await
        .unwrap();
    raw.close().await.unwrap();
    fixture.catalog.close().await.unwrap();
    let durable = durable_rows(&fixture.path).await;
    let catalog = Catalog::open(&fixture.path).await.unwrap();
    assert_eq!(durable_rows(&fixture.path).await, durable);
    let after = catalog
        .selected_track_uris(
            &library::MemberRead {
                collection: library::Collection::Source(fixture.source),
                sort: Some(library::CatalogSort::Title),
                ..library::MemberRead::new(library::Collection::Source(fixture.source))
            }
            .selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(after.len(), fixture.tracks.len());
    catalog.close().await.unwrap();
    let mut raw = super::support::connection(&fixture.path).await;
    let schema_version: i64 = sqlx::query_scalar("PRAGMA schema_version")
        .fetch_one(&mut raw)
        .await
        .unwrap();
    raw.close().await.unwrap();
    let reopened = Catalog::open(&fixture.path).await.unwrap();
    assert_eq!(durable_rows(&fixture.path).await, durable);
    let mut raw = super::support::connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA schema_version")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        schema_version
    );
    raw.close().await.unwrap();
    reopened.close().await.unwrap();

    let mut raw = connection(&fixture.path, false).await;
    sqlx::raw_sql("DROP VIEW local_track_access;
        CREATE VIEW local_track_access AS
        SELECT track.track_key,track.source_key,backing.path source_path,owner.path cue_path,
            CASE WHEN track.cue_path IS NULL THEN backing.local_file_key ELSE owner.local_file_key END local_file_key
        FROM tracks track
        JOIN local_files canonical ON canonical.source_key=track.source_key AND canonical.path=track.source_path
        JOIN observed_local_files backing ON backing.local_file_key=canonical.local_file_key
        LEFT JOIN local_files cue ON cue.source_key=track.source_key AND cue.path=track.cue_path
        LEFT JOIN observed_local_files owner ON owner.local_file_key=cue.local_file_key;
        PRAGMA user_version=53;")
        .execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    let migrated = Catalog::open(&fixture.path).await.unwrap();
    assert_eq!(durable_rows(&fixture.path).await, durable);
    let mut raw = connection(&fixture.path, false).await;
    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as("EXPLAIN QUERY PLAN SELECT access.source_path FROM tracks track JOIN local_track_access access USING(track_key) WHERE media_uri=?1")
        .bind(&fixture.track_uris[0]).fetch_all(&mut raw).await.unwrap();
    assert!(
        plan.iter().all(|step| !step.3.contains("MATERIALIZE")),
        "{plan:?}"
    );
    raw.close().await.unwrap();
    migrated.close().await.unwrap();

    let old_paths = [
        fixture.path.with_extension("schema48.sqlite"),
        fixture.path.with_extension("schema49.sqlite"),
    ];
    for path in &old_paths {
        let mut raw = connection(path, true).await;
        super::support::catalog::copy_before_index(&mut raw, &fixture.path).await;
        sqlx::raw_sql("CREATE VIEW local_access_files AS SELECT * FROM local_locators;
            CREATE VIEW local_track_access AS SELECT track_key,source_key,source_path,cue_path,local_file_key FROM tracks;
            ALTER TABLE local_access_metadata ADD COLUMN title TEXT NOT NULL DEFAULT '';
            ALTER TABLE local_access_metadata ADD COLUMN normalized_title TEXT NOT NULL DEFAULT '';
            ALTER TABLE local_access_metadata ADD COLUMN album TEXT NOT NULL DEFAULT '';
            ALTER TABLE local_access_metadata ADD COLUMN normalized_album TEXT NOT NULL DEFAULT '';
            ALTER TABLE local_access_metadata ADD COLUMN artist TEXT NOT NULL DEFAULT '';
            ALTER TABLE local_access_metadata ADD COLUMN normalized_artist TEXT NOT NULL DEFAULT '';
            ALTER TABLE local_access_metadata ADD COLUMN disc_number INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE local_access_metadata ADD COLUMN track_number INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE local_access_metadata ADD COLUMN duration_millis INTEGER NOT NULL DEFAULT 0;
            CREATE INDEX local_access_match_uri_idx ON local_access_metadata(normalized_title,normalized_album,normalized_artist,disc_number,track_number,duration_millis,access_uri);")
            .execute(&mut raw).await.unwrap();
        raw.close().await.unwrap();
    }
    let mut raw = connection(&old_paths[0], false).await;
    sqlx::query("INSERT INTO albums(source_key,object_id,media_uri,indexed) VALUES(?1,'retired-header','https://example.org/retired-header',1)")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO tracks(source_key,media_uri,metadata,indexed) VALUES(?1,'https://example.org/retired-alias',jsonb('{\"title\":\"Retired alias\"}'),1)")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    sqlx::query("PRAGMA user_version=48")
        .execute(&mut raw)
        .await
        .unwrap();
    raw.close().await.unwrap();
    let migrated = Catalog::open(&old_paths[0]).await.unwrap();
    assert_eq!(
        migrated
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Source(fixture.source)).selection(),
                None,
                &ReadCancellation::new(),
            )
            .await
            .unwrap()
            .len(),
        fixture.tracks.len()
    );
    let mut raw = connection(&old_paths[0], false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sqlite_schema WHERE name IN('album_order','effective_album_artists','effective_album_genres','effective_album_release_types')")
            .fetch_one(&mut raw).await.unwrap(),
        0
    );
    raw.close().await.unwrap();
    let home = migrated
        .home_page(
            fixture.source,
            None,
            0,
            0,
            &library::HomeBlockKind::all(),
            &ReadCancellation::new(),
        )
        .await
        .unwrap();
    assert!(home.showcase.is_some());
    for row in home.showcase.iter().chain(&home.explore) {
        assert!(!row.title.is_empty());
        assert!(row.album.summary.track_count > 0);
        assert_ne!(row.album.object_id, "retired-header");
    }
    migrated.close().await.unwrap();

    let mut raw = connection(&old_paths[1], false).await;
    sqlx::raw_sql("DROP VIEW local_access_files;
        INSERT INTO tracks(media_uri,metadata,native_metadata) VALUES('https://media.example/legacy-access.flac',jsonb('{\"title\":\"Canonical title\",\"source_format\":\"flac\",\"future\":{\"field\":true}}'),1);
        INSERT INTO local_locators(media_uri,origin,path,root,relative_path,access_uri) VALUES('https://media.example/legacy-access.flac','download','/downloads/legacy.flac','/downloads','legacy.flac','file:///downloads/legacy.flac');
        INSERT INTO local_access_metadata(access_uri,size_bytes,mtime_ns,device_id,inode,parser_version,loudness_analysis_key,title,album,artist,disc_number,track_number,duration_millis)
        VALUES('file:///downloads/legacy.flac',111,222,3,4,17,zeroblob(32),'Legacy title','Legacy album','Legacy artist',1,4,1234);
        PRAGMA user_version=49;")
        .execute(&mut raw)
        .await
        .unwrap();
    raw.close().await.unwrap();
    let migrated = Catalog::open(&old_paths[1]).await.unwrap();
    let mut raw = connection(&old_paths[1], false).await;
    let legacy = migrated
        .download_metadata("https://media.example/legacy-access.flac")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(legacy.title, "Canonical title");
    assert_eq!(legacy.album, "Legacy album");
    assert_eq!(legacy.artist, "Legacy artist");
    assert_eq!(legacy.duration_millis, 1234);
    assert_eq!(legacy.source_format.as_deref(), Some("flac"));
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT json_extract(metadata,'$.future.field') FROM tracks WHERE media_uri='https://media.example/legacy-access.flac'").fetch_one(&mut raw).await.unwrap(), 1);
    assert_eq!(sqlx::query_as::<_,(i64,i64,i64,i64,i64,Vec<u8>)>("SELECT size_bytes,mtime_ns,device_id,inode,parser_version,loudness_analysis_key FROM local_access_metadata WHERE access_uri='file:///downloads/legacy.flac'").fetch_one(&mut raw).await.unwrap(), (111,222,3,4,17,vec![0;32]));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM pragma_table_info('local_access_metadata')"
        )
        .fetch_one(&mut raw)
        .await
        .unwrap(),
        7
    );
    raw.close().await.unwrap();
    migrated.close().await.unwrap();
}

#[tokio::test]
async fn missing_indexed_column_returns_error_without_replacing_the_library() {
    let fixture = super::support::fixture().await;
    fixture.catalog.close().await.unwrap();
    let mut raw = super::support::connection(&fixture.path).await;
    let triggers: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type='trigger' AND sql LIKE '%local_play_count%'",
    )
    .fetch_all(&mut raw)
    .await
    .unwrap();
    for trigger in triggers {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TRIGGER {trigger}")))
            .execute(&mut raw)
            .await
            .unwrap();
    }
    sqlx::raw_sql(
        "DROP INDEX tracks_local_play_order_idx; DROP INDEX tracks_global_play_order_idx;
        DROP INDEX tracks_local_shuffle_idx; DROP INDEX tracks_global_local_shuffle_idx;
        ALTER TABLE tracks DROP COLUMN local_play_count;",
    )
    .execute(&mut raw)
    .await
    .unwrap();
    raw.close().await.unwrap();
    let before = durable_rows(&fixture.path).await;
    assert!(Catalog::open(&fixture.path).await.is_err());
    assert_eq!(durable_rows(&fixture.path).await, before);
}

#[tokio::test]
async fn index_setup_failure_returns_error_without_replacing_the_library() {
    let fixture = super::support::fixture().await;
    fixture
        .catalog
        .create_playlist(
            None,
            "Keep me",
            &library::MemberSelection::Uris((vec![]).into()),
        )
        .await
        .unwrap();
    fixture.catalog.close().await.unwrap();
    let mut raw = super::support::connection(&fixture.path).await;
    sqlx::raw_sql(
        "DROP INDEX tracks_local_shuffle_idx; CREATE TABLE tracks_local_shuffle_idx(value TEXT);",
    )
    .execute(&mut raw)
    .await
    .unwrap();
    raw.close().await.unwrap();
    let before = durable_rows(&fixture.path).await;
    assert!(Catalog::open(&fixture.path).await.is_err());
    assert_eq!(durable_rows(&fixture.path).await, before);
}

#[tokio::test]
async fn malformed_library_schema_returns_error_without_replacing_the_file() {
    let fixture = super::support::fixture().await;
    fixture.catalog.close().await.unwrap();
    let mut raw = super::support::connection(&fixture.path).await;
    sqlx::raw_sql("PRAGMA writable_schema=ON; UPDATE sqlite_schema SET sql=replace(sql,'CREATE TABLE','BROKEN TABLE') WHERE name='sources';").execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    let before = std::fs::read(&fixture.path).unwrap();
    assert!(Catalog::open(&fixture.path).await.is_err());
    assert_eq!(std::fs::read(&fixture.path).unwrap(), before);
}

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
async fn unreadable_library_returns_error_and_preserves_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    std::fs::write(&path, b"not a sqlite database").unwrap();
    assert!(Catalog::open(&path).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"not a sqlite database");
}
#[tokio::test]
async fn deleting_or_corrupting_catalog_preserves_exact_durable_records_and_locators() {
    let fixture = super::support::fixture().await;
    let path = fixture.path.clone();
    let catalog_file = path.with_extension("catalog.sqlite");
    let uri = &fixture.track_uris[0];
    fixture
        .catalog
        .create_playlist(
            None,
            "Durable duplicate list",
            &library::MemberSelection::Uris((vec![uri.clone(), uri.clone()]).into()),
        )
        .await
        .unwrap();
    fixture
        .catalog
        .import_user_media_state_jsonl(std::io::Cursor::new(format!(
            "{{\"version\":1}}\n{{\"media_uri\":{},\"favorite\":true,\"rating\":87}}\n",
            serde_json::to_string(uri).unwrap()
        )))
        .await
        .unwrap();
    fixture.catalog.import_local_locators_jsonl(std::io::Cursor::new(format!("{{\"version\":1}}\n{{\"source_id\":\"source\",\"media_uri\":{},\"origin\":\"mapping\",\"path\":\"/music/original.flac\",\"root\":\"/music\",\"relative_path\":\"original.flac\",\"access_uri\":\"file:///music/original.flac\"}}\n",serde_json::to_string(uri).unwrap()))).await.unwrap();
    let mut raw = connection(&path, false).await;
    sqlx::query("INSERT INTO listens(external_id,source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped) VALUES(NULL,'source',?1,'Snapshot','Artist','Album',100,'1970-01',100000,90000,0)").bind(uri).execute(&mut raw).await.unwrap();
    sqlx::raw_sql("INSERT INTO listen_outbox(listen_key,service,account_id,attempts,next_attempt_at,last_error) SELECT listen_key,'lastfm','account',3,200,'retry' FROM listens; INSERT INTO legacy_activity VALUES('source','lifetime','track','track-a',4,2,90);").execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    fixture.catalog.close().await.unwrap();
    let mut before = durable_rows(&path).await;
    for damage in 0..3 {
        for suffix in ["", "-wal", "-shm"] {
            let mut file = catalog_file.as_os_str().to_os_string();
            file.push(suffix);
            match std::fs::remove_file(file) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("{error}"),
            }
        }
        match damage {
            1 => std::fs::write(&catalog_file, b"damaged catalog").unwrap(),
            2 => {
                let mut old = SqliteConnection::connect_with(
                    &SqliteConnectOptions::new()
                        .filename(&catalog_file)
                        .create_if_missing(true),
                )
                .await
                .unwrap();
                sqlx::raw_sql("PRAGMA user_version=1; CREATE TABLE obsolete(value TEXT)")
                    .execute(&mut old)
                    .await
                    .unwrap();
                old.close().await.unwrap();
            }
            _ => {}
        }
        let catalog = Catalog::open(&path).await.unwrap();
        assert_eq!(durable_rows(&path).await, before);
        let mut raw = connection(&path, false).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            4
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT access_uri FROM local_access_files")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            "file:///music/original.flac"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            "ok"
        );
        raw.close().await.unwrap();
        // Ordinary rescan rejoins effective favorites/ratings by the unchanged media URI.
        let (_, _, object) = library::source_entity_parts(uri).unwrap();
        catalog
            .reconcile_source(&SourceId::new("source"), "subsonic", "Source")
            .await
            .unwrap();
        let mut observation = catalog.observe("source", None).await.unwrap();
        let observed = observation.start_observation().await.unwrap();
        observation
            .apply_catalog(
                &library::CatalogBatch {
                    tracks: vec![library::TrackObservation {
                        object_id: object.to_owned(),
                        metadata: library::TrackMetadata {
                            title: "Changed tags".into(),
                            artist: "Artist".into(),
                            sort_text: "changed tags".into(),
                            duration_millis: 100000,
                            disc_number: 1,
                            track_number: 1,
                            ..Default::default()
                        },
                        user_data: library::NativeUserState {
                            favorite: Some(false),
                            rating: Some(Some(99)),
                            ..Default::default()
                        },
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
        observation
            .complete_source_observation(observed, &["track"], None)
            .await
            .unwrap();
        let mut raw = connection(&path, false).await;
        assert_eq!(sqlx::query_as::<_,(bool,i64)>("SELECT state.favorite,state.rating FROM tracks track JOIN user_media_state state USING(media_uri) WHERE track.media_uri=?1").bind(uri).fetch_one(&mut raw).await.unwrap(),(true,87));
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT title FROM tracks WHERE media_uri=?1")
                .bind(uri)
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            "Changed tags"
        );
        raw.close().await.unwrap();
        before = durable_rows(&path).await;
        catalog.close().await.unwrap();
    }
}

async fn durable_rows(path: &Path) -> Vec<(String, Vec<String>)> {
    let mut raw = connection(path, false).await;
    let tables=sqlx::query_scalar::<_,String>("SELECT name FROM main.sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(&mut raw).await.unwrap();
    let mut result = Vec::new();
    for table in tables {
        let columns = sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info(?1)")
            .bind(&table)
            .fetch_all(&mut raw)
            .await
            .unwrap();
        let values = columns
            .iter()
            .map(|column| {
                format!(
                    "CASE WHEN typeof(\"{0}\")='blob' THEN hex(\"{0}\") ELSE \"{0}\" END",
                    column.replace('"', "\"\"")
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let query = format!("SELECT json_array({values}) FROM main.\"{table}\" ORDER BY {values}");
        let rows = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(query))
            .fetch_all(&mut raw)
            .await
            .unwrap();
        result.push((table, rows));
    }
    raw.close().await.unwrap();
    result
}

#[tokio::test]
async fn forgetting_source_retains_canonical_metadata_for_global_playlists() {
    let fixture = super::support::fixture().await;
    let catalog = &fixture.catalog;
    catalog
        .create_playlist(
            None,
            "Snapshot",
            &library::MemberSelection::Uris((vec![fixture.track_uris[0].clone()]).into()),
        )
        .await
        .unwrap();
    let mut raw = connection(&fixture.path, false).await;
    for kind in ["track", "album", "artist"] {
        let uri = library::source_entity_uri(&SourceId::new("source"), kind, "owned");
        sqlx::query("INSERT INTO user_media_state(media_uri,favorite,rating) VALUES(?1,1,87)")
            .bind(uri)
            .execute(&mut raw)
            .await
            .unwrap();
    }
    raw.close().await.unwrap();
    assert!(
        catalog
            .remove_source(&library::SourceId::new("source"))
            .await
            .unwrap()
    );
    let mut raw = connection(&fixture.path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_media_state")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM main.playlist_entries")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn unsupported_version_preserves_library_data() {
    let fixture = super::support::fixture().await;
    fixture
        .catalog
        .create_playlist(
            None,
            "Keep me",
            &library::MemberSelection::Uris((vec![]).into()),
        )
        .await
        .unwrap();
    fixture.catalog.close().await.unwrap();
    let supported = durable_rows(&fixture.path).await;
    let mut raw = connection(&fixture.path, false).await;
    sqlx::raw_sql("ALTER TABLE playlists ADD COLUMN irrelevant TEXT; ALTER TABLE playlists DROP COLUMN artwork_mime; CREATE TABLE extra(value TEXT); INSERT INTO extra VALUES('keep in the original'); PRAGMA user_version=999;").execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    let before = durable_rows(&fixture.path).await;
    assert!(Catalog::open(&fixture.path).await.is_err());
    assert_eq!(durable_rows(&fixture.path).await, before);
    let recovered = Catalog::open_installation(
        &fixture.path,
        &fixture.path.with_extension("catalog.sqlite"),
    )
    .await
    .unwrap();
    recovered.close().await.unwrap();
    let facts = |tables: Vec<(String, Vec<String>)>| {
        tables
            .into_iter()
            .filter(|(table, _)| {
                !matches!(
                    table.as_str(),
                    "folder_children"
                        | "collection_album_index"
                        | "collection_totals"
                        | "playlist_genres"
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(facts(durable_rows(&fixture.path).await), facts(supported));
    let preserved = std::fs::read_dir(fixture.path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension().is_some_and(|extension| {
                extension.to_string_lossy().starts_with("before-migration-")
            }) && !path.to_string_lossy().ends_with("-wal")
                && !path.to_string_lossy().ends_with("-shm")
        })
        .unwrap();
    assert_eq!(durable_rows(&preserved).await, before);
}

#[tokio::test]
async fn released_store_imports_old_playlist_artwork_and_known_roots() {
    let directory = tempfile::tempdir().unwrap();
    let store = directory.path().join("rufin-store.sqlite");
    let catalog = directory.path().join("rufin-catalog.sqlite");
    let root_id = blake3::hash(b"/music").to_hex().to_string();
    let mut raw = connection(&store, true).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TABLE source_ids(source_key INTEGER PRIMARY KEY,object_id TEXT NOT NULL);
        INSERT INTO source_ids VALUES(1,'source');
        CREATE TABLE connect_roots(source_id TEXT NOT NULL,id TEXT NOT NULL,label TEXT NOT NULL,PRIMARY KEY(source_id,id)) STRICT;
        INSERT INTO connect_roots VALUES('source','{root_id}','Music');
        CREATE TABLE legacy_activity(source_id TEXT NOT NULL,period TEXT NOT NULL,item_kind TEXT NOT NULL,track_object_id TEXT NOT NULL,play_count INTEGER NOT NULL,skip_count INTEGER NOT NULL,last_played_at INTEGER);
        INSERT INTO legacy_activity VALUES('source','lifetime','track','played',3,0,90);
        CREATE TABLE local_locators(local_access_file_key INTEGER PRIMARY KEY,media_uri TEXT NOT NULL,origin TEXT NOT NULL,access_uri TEXT NOT NULL);
        CREATE TABLE queue_occurrences(queue_occurrence_key INTEGER PRIMARY KEY,object_id TEXT,media_uri TEXT,position INTEGER,traversal_position INTEGER,provenance_kind TEXT,provenance_context_id TEXT,provenance_source_rank INTEGER,snapshot_at INTEGER);
        CREATE TABLE queue_state(singleton INTEGER PRIMARY KEY,current_occurrence_id TEXT,progress_millis INTEGER,repeat_mode TEXT,shuffled INTEGER);
        PRAGMA user_version=46;"))).execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    let mut raw = connection(&catalog, true).await;
    let mut tables = String::new();
    for (table, key) in [
        ("albums", "album_key"),
        ("artists", "artist_key"),
        ("tracks", "track_key"),
        ("genres", "genre_key"),
        ("moods", "mood_key"),
        ("folders", "folder_key"),
    ] {
        tables += &format!(
            "CREATE TABLE {table}({key} INTEGER PRIMARY KEY,source_key INTEGER,object_id TEXT);"
        );
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("{tables}
        CREATE TABLE sources(source_key INTEGER PRIMARY KEY,object_id TEXT NOT NULL,catalog_complete INTEGER NOT NULL DEFAULT 1);
        INSERT INTO sources(source_key,object_id) VALUES(7,'source');
        CREATE TABLE local_files(local_file_key INTEGER PRIMARY KEY,source_key INTEGER NOT NULL,path TEXT NOT NULL,root TEXT NOT NULL,relative_path TEXT NOT NULL,kind TEXT NOT NULL,size_bytes INTEGER,mtime_ns INTEGER NOT NULL,state TEXT NOT NULL);
        INSERT INTO local_files VALUES(1,7,'/music/a.flac','/music','a.flac','media',1,1,'observed');
        CREATE TABLE local_access_metadata(access_uri TEXT PRIMARY KEY,title TEXT,artist TEXT,album TEXT,duration_millis INTEGER,disc_number INTEGER,track_number INTEGER);
        CREATE TABLE native_playlists(playlist_key INTEGER PRIMARY KEY,source_key INTEGER NOT NULL,object_id TEXT NOT NULL,name TEXT NOT NULL,normalized_name TEXT NOT NULL,sort_text TEXT NOT NULL,artwork_binding BLOB);
        INSERT INTO native_playlists VALUES(1,7,'list','List','list','list',CAST('{{\"source_id\":\"source\",\"image\":{{\"item_id\":\"list-art\",\"tag\":\"t\"}}}}' AS BLOB));
        CREATE TABLE native_playlist_entries(playlist_entry_key INTEGER PRIMARY KEY,playlist_key INTEGER NOT NULL,object_id TEXT NOT NULL,media_uri TEXT NOT NULL,title TEXT,snapshot_at INTEGER NOT NULL DEFAULT 0,position INTEGER NOT NULL);
        PRAGMA user_version=51;"))).execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();

    Catalog::open_installation(&store, &catalog)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let mut raw = connection(&store, false).await;
    assert_eq!(
        sqlx::query_as::<_, (String, String)>("SELECT id,label FROM connect_roots")
            .fetch_all(&mut raw)
            .await
            .unwrap(),
        vec![(root_id, String::new())]
    );
    assert_eq!(sqlx::query_scalar::<_, String>("SELECT json_extract(artwork_binding,'$[0].image.item_id') FROM playlists WHERE object_id='list'").fetch_one(&mut raw).await.unwrap(), "list-art");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks WHERE object_id='played'")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1
    );
    raw.close().await.unwrap();
}

#[tokio::test]
async fn missing_listen_identity_returns_error_without_replacing_user_data() {
    let fixture = super::support::fixture().await;
    fixture
        .catalog
        .create_playlist(
            None,
            "Keep me",
            &library::MemberSelection::Uris((vec![]).into()),
        )
        .await
        .unwrap();
    fixture.catalog.close().await.unwrap();
    let mut raw = connection(&fixture.path, false).await;
    let triggers: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type='trigger' AND sql LIKE '%listens%'",
    )
    .fetch_all(&mut raw)
    .await
    .unwrap();
    for trigger in triggers {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TRIGGER {trigger}")))
            .execute(&mut raw)
            .await
            .unwrap();
    }
    sqlx::raw_sql("DROP INDEX listens_media_activity_idx; DROP INDEX listens_connect_key_idx; ALTER TABLE listens DROP COLUMN media_uri;").execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    let before = durable_rows(&fixture.path).await;
    assert!(Catalog::open(&fixture.path).await.is_err());
    assert_eq!(durable_rows(&fixture.path).await, before);
}
