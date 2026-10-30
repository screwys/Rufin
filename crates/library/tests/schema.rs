use std::path::Path;

use library::{Catalog, SourceId};
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

#[tokio::test]
async fn malformed_library_schema_returns_error_without_replacing_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    Catalog::open(&path).await.unwrap().close().await.unwrap();
    let mut raw = connection(&path, false).await;
    sqlx::raw_sql("PRAGMA writable_schema=ON; UPDATE sqlite_schema SET sql=replace(sql,'CREATE TABLE','BROKEN TABLE') WHERE name='sources';").execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(Catalog::open(&path).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
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
            "Durable duplicate list",
            &library::MemberSelection::Uris((vec![uri.clone(), uri.clone()]).into()),
        )
        .await
        .unwrap();
    super::support::restore(
        &fixture.catalog,
        &[
            (
                "user-state.jsonl",
                format!(
                    "{{\"version\":1}}\n{{\"media_uri\":{},\"favorite\":true,\"rating\":87}}\n",
                    serde_json::to_string(uri).unwrap()
                )
                .as_bytes(),
            ),
            (
                "local-locators.jsonl",
                format!("{{\"version\":1}}\n{{\"source_id\":\"source\",\"media_uri\":{},\"origin\":\"mapping\",\"path\":\"/music/original.flac\",\"root\":\"/music\",\"relative_path\":\"original.flac\",\"access_uri\":\"file:///music/original.flac\"}}\n", serde_json::to_string(uri).unwrap())
                .as_bytes(),
            ),
        ],
    )
    .await;
    let mut raw = connection(&path, false).await;
    sqlx::query("INSERT INTO listens(external_id,source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped) VALUES(NULL,'source',?1,'Snapshot','Artist','Album',100,'1970-01',100000,90000,0)").bind(uri).execute(&mut raw).await.unwrap();
    sqlx::raw_sql("INSERT INTO listen_outbox(listen_key,service,account_id,attempts,next_attempt_at,last_error) SELECT listen_key,'lastfm','account',3,200,'retry' FROM listens; INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at) VALUES('source','lifetime','track','track-a',4,2,90);").execute(&mut raw).await.unwrap();
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
            sqlx::query_scalar::<_, String>("SELECT access_uri FROM local_locators")
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
        observation.start_observation().await.unwrap();
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
            .complete_source_observation(&["track"], None, None)
            .await
            .unwrap();
        drop(observation);
        let mut raw = connection(&path, false).await;
        assert_eq!(sqlx::query_as::<_,(bool,i64)>("SELECT state.favorite,state.rating FROM tracks track JOIN user_state state USING(media_uri) WHERE track.media_uri=?1").bind(uri).fetch_one(&mut raw).await.unwrap(),(true,87));
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
            "Snapshot",
            &library::MemberSelection::Uris((vec![fixture.track_uris[0].clone()]).into()),
        )
        .await
        .unwrap();
    let mut raw = connection(&fixture.path, false).await;
    for kind in ["track", "album", "artist"] {
        let uri = library::source_entity_uri(&SourceId::new("source"), kind, "owned");
        sqlx::query(
            "INSERT INTO user_state(media_uri,favorite,rating,changed_at) VALUES(?1,1,87,1)",
        )
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
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_state")
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
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    Catalog::open(&path).await.unwrap().close().await.unwrap();
    let mut raw = connection(&path, false).await;
    sqlx::raw_sql("INSERT INTO sources(source_key,object_id,kind) VALUES (1,'source','local');
        INSERT INTO tracks(track_key,source_key,object_id,media_uri,title,indexed) VALUES(1,1,'one','file:///one.flac','One',1);
        INSERT INTO playlists(playlist_key,object_id,name) VALUES (1,'keep','Keep me');
        INSERT INTO playlist_entries(playlist_key,object_id,track_key,rank) VALUES(1,'kept',1,X'8000');
        INSERT INTO user_state(media_uri,favorite,changed_at) VALUES('file:///one.flac',1,1);")
        .execute(&mut raw)
        .await
        .unwrap();
    raw.close().await.unwrap();
    let supported = durable_rows(&path).await;
    // A newer or damaged Store can name rows that no longer exist.
    let mut raw = connection(&path, false).await;
    sqlx::raw_sql("PRAGMA foreign_keys=OFF;
        INSERT INTO playlist_entries(playlist_key,object_id,track_key,rank) VALUES(1,'dangling',99,X'8001');
        INSERT INTO tracks(track_key,source_key,object_id,media_uri,album_key) VALUES(2,1,'two','file:///two.flac',77);
        INSERT INTO queue_entries(entry_id,track_key,rank) VALUES(1,98,X'8000');
        ALTER TABLE playlists ADD COLUMN irrelevant TEXT; ALTER TABLE playlists DROP COLUMN artwork_mime;
        CREATE TABLE extra(value TEXT); INSERT INTO extra VALUES('keep in the original');
        PRAGMA user_version=999;")
        .execute(&mut raw)
        .await
        .unwrap();
    raw.close().await.unwrap();
    let before = durable_rows(&path).await;
    assert!(Catalog::open(&path).await.is_err());
    assert_eq!(durable_rows(&path).await, before);
    let recovered = Catalog::open_installation(&path, &path.with_extension("catalog.sqlite"))
        .await
        .unwrap();
    recovered.close().await.unwrap();
    // The entry and queue row that name no track go; the track loses the album it named.
    let mut expected = supported;
    for (table, rows) in &mut expected {
        if table == "tracks" {
            rows.push(r#"[2,1,"two","file:///two.flac",null,0,null,0,"","","","",0,0,0,null,null,null,0,null,null,null,null,null,0,null,null,null,null]"#.into());
        }
    }
    assert_eq!(durable_rows(&path).await, expected);
    let preserved = std::fs::read_dir(path.parent().unwrap())
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
async fn released_store_imports_old_playlist_artwork_and_files() {
    let directory = tempfile::tempdir().unwrap();
    let store = directory.path().join("rufin-store.sqlite");
    let catalog = directory.path().join("rufin-catalog.sqlite");
    let mut raw = connection(&store, true).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TABLE source_ids(source_key INTEGER PRIMARY KEY,object_id TEXT NOT NULL);
        INSERT INTO source_ids VALUES(1,'source');
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
        INSERT INTO sources(source_key,object_id) VALUES (7,'source');
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
    // A file waiting for a parse is read again on the next walk.
    assert_eq!(
        sqlx::query_as::<_, (String, String, bool)>(
            "SELECT path,state,parse_version IS NULL FROM files"
        )
        .fetch_all(&mut raw)
        .await
        .unwrap(),
        vec![("/music/a.flac".into(), "accepted".into(), true)]
    );
    assert_eq!(sqlx::query_scalar::<_, String>("SELECT json_extract(artwork_binding,'$[0].image.item_id') FROM playlists WHERE object_id='list'").fetch_one(&mut raw).await.unwrap(), "list-art");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT play_count FROM legacy_activity WHERE track_object_id='played'"
        )
        .fetch_one(&mut raw)
        .await
        .unwrap(),
        3
    );
    raw.close().await.unwrap();
}

/// A Store and catalog file as a released version left them upgrade into one Store that keeps
/// every piece of user data and the library's facts.
#[tokio::test]
async fn released_stores_upgrade_with_their_user_data() {
    for version in ["0.15.0", "0.16.6"] {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("rufin-store.sqlite");
        let catalog = directory.path().join("rufin-catalog.sqlite");
        for (path, name) in [(&store, "store"), (&catalog, "catalog")] {
            let fixture = std::fs::read_to_string(format!(
                "{}/tests/fixtures/{name}-{version}.sql",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap();
            let mut raw = connection(path, true).await;
            sqlx::raw_sql(sqlx::AssertSqlSafe(fixture))
                .execute(&mut raw)
                .await
                .unwrap();
            raw.close().await.unwrap();
        }
        Catalog::open_installation(&store, &catalog)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
        let mut raw = connection(&store, false).await;
        let all = |sql: &'static str| sqlx::query_as::<_, (String,)>(sql);
        let values = async |raw: &mut SqliteConnection, sql: &'static str| -> Vec<String> {
            all(sql)
                .fetch_all(&mut *raw)
                .await
                .unwrap()
                .into_iter()
                .map(|(value,)| value)
                .collect()
        };
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            60,
            "{version}"
        );
        // Favorites, ratings, listens, scrobbles and pending deliveries stay as they were.
        assert_eq!(
            values(&mut raw, "SELECT json_array(media_uri,favorite,rating) FROM user_state WHERE changed_at>0 ORDER BY media_uri").await,
            [
                r#"["file:///music/Album/01.flac",1,60]"#,
                r#"["file:///music/Gone/old.flac",1,null]"#,
                r#"["rufin:source/album/local-source/album-1",1,null]"#,
            ],
            "{version}"
        );
        assert_eq!(
            values(&mut raw, "SELECT json_array(listen_key,media_uri,track_title,listened_millis) FROM listens ORDER BY listen_key").await,
            [
                r#"[1,"file:///music/Album/01.flac","First",200000]"#,
                r#"[2,"file:///music/Gone/old.flac","Gone Song",100000]"#,
                r#"[3,"rufin:source/track/server/vanished","Vanished",90000]"#,
            ],
            "{version}"
        );
        assert_eq!(
            values(
                &mut raw,
                "SELECT json_array(listen_key,service,attempts) FROM listen_outbox"
            )
            .await,
            [r#"[1,"lastfm",2]"#],
            "{version}"
        );
        assert_eq!(
            values(
                &mut raw,
                "SELECT json_array(media_uri,kind,value) FROM outbox"
            )
            .await,
            [r#"["rufin:source/track/server/s1","favorite",0]"#],
            "{version}"
        );
        // Carried-over counts keep their names, also for tracks the catalog no longer has.
        assert_eq!(
            values(&mut raw, "SELECT json_array(period,item_kind,track_object_id,play_count,title,artist,album) FROM legacy_activity ORDER BY period,item_kind").await,
            [
                r#"["2017-07","track","vanished",2,"Vanished","Lost Artist","Lost Album"]"#,
                r#"["lifetime","artist","ar2",9,"Server Artist","Server Artist",""]"#,
                r#"["lifetime","track","s2",5,"Server Two","Server Artist","Server Album"]"#,
            ],
            "{version}"
        );
        // Rufin playlists are global and keep their entries, also for missing tracks. The
        // list order is not kept.
        assert_eq!(
            values(&mut raw, "SELECT json_array(object_id,owner,source_key IS NULL,name) FROM playlists ORDER BY object_id").await,
            [
                r#"["p100","source",0,"Server Mix"]"#,
                r#"["rufin:playlist:global","rufin",1,"Global Mix"]"#,
                r#"["rufin:playlist:local","rufin",1,"Local Mix"]"#,
            ],
            "{version}"
        );
        assert_eq!(
            values(&mut raw, "SELECT json_array(playlist.object_id,entry.object_id,track.media_uri,track.title) FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) JOIN tracks track USING(track_key) ORDER BY playlist.object_id,entry.rank").await,
            [
                r#"["p100","pe1","rufin:source/track/server/s1","Server One"]"#,
                r#"["p100","pe2","rufin:source/track/server/s2","Server Two"]"#,
                r#"["rufin:playlist:global","e3","rufin:source/track/server/s1","Server One"]"#,
                r#"["rufin:playlist:global","e4","file:///music/Album/01.flac","First"]"#,
                r#"["rufin:playlist:local","e1","file:///music/Album/01.flac","First"]"#,
                r#"["rufin:playlist:local","e2","file:///music/Gone/old.flac","Gone Song"]"#,
            ],
            "{version}"
        );
        // Entry order becomes ranks a list built by appends has.
        assert_eq!(
            values(&mut raw, "SELECT json_array(entry.object_id,hex(entry.rank)) FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) WHERE playlist.object_id='rufin:playlist:local' ORDER BY entry.rank").await,
            [r#"["e1","8000"]"#, r#"["e2","8001"]"#],
            "{version}"
        );
        assert_eq!(
            values(
                &mut raw,
                "SELECT json_array(object_id,name) FROM smart_playlists"
            )
            .await,
            [r#"["rufin:smart:recent","Recently Played"]"#],
            "{version}"
        );
        // A missing track stays as a hidden row with the names the playlist kept, and its
        // 60-day expiry starts at the upgrade.
        assert_eq!(
            values(&mut raw, "SELECT json_array(indexed,hidden_since IS NOT NULL,title,display_artist,display_album,duration_millis,json_extract(details.metadata,'$.musicbrainz_recording_id')) FROM tracks JOIN track_details details USING(track_key) WHERE media_uri='file:///music/Gone/old.flac'").await,
            [r#"[0,1,"Gone Song","Old Artist","Old Album",123000,"mb-gone"]"#],
            "{version}"
        );
        // The queue keeps its order, position and repeat mode.
        assert_eq!(
            values(&mut raw, "SELECT json_group_array(track.media_uri) FROM (SELECT * FROM queue_entries ORDER BY rank) entry JOIN tracks track USING(track_key)").await,
            [r#"["file:///music/Album/01.flac","file:///music/Gone/old.flac","rufin:source/track/server/s1"]"#],
            "{version}"
        );
        assert_eq!(
            values(&mut raw, "SELECT json_array(current_index IS NOT NULL,progress_millis,repeat_mode) FROM queue_state").await,
            [r#"[1,5000,"all"]"#],
            "{version}"
        );
        // Downloads stay attached to their track, as one record each.
        assert_eq!(
            values(&mut raw, "SELECT json_array(media_uri,quality,path,source_id,size,root,managed) FROM downloads").await,
            [if version == "0.16.6" {
                r#"["rufin:source/track/server/s1","original","file:///downloads/s1.flac","server",900,"/downloads",1]"#
            } else {
                r#"["rufin:source/track/server/s1","unknown","file:///downloads/s1.flac","server",900,"/downloads",1]"#
            }],
            "{version}"
        );
        assert_eq!(
            values(&mut raw, "SELECT json_array(count(*)) FROM local_locators").await,
            ["[0]"],
            "{version}"
        );
        // Library facts: typed columns, details, links, the date a track was added and the
        // favorites a server reported.
        assert_eq!(
            values(&mut raw, "SELECT json_array(track.media_uri,track.indexed,track.title,track.date_added,track.first_seen_at,track.flags,track.native_favorite,track.native_rating,details.source_path,details.cue_path,details.cue_end_millis) FROM tracks track JOIN track_details details USING(track_key) WHERE track.indexed=1 ORDER BY track.track_key").await,
            [
                r#"["file:///music/Album/01.flac",1,"First","2020-01-02T03:04:05Z",1577934245,1,0,null,"/music/Album/01.flac",null,null]"#,
                r#"["rufin:cue/album-cue%3A1/file%3A%2F%2F%2Fmusic%2FAlbum%2Falbum.flac/0/100000",1,"Cue One",null,1577934245,2,0,null,"/music/Album/album.flac","/music/Album/album.cue",100000]"#,
                r#"["rufin:source/track/server/s1",1,"Server One","2021-06-07T00:00:00Z",null,1,1,80,null,null,null]"#,
                r#"["rufin:source/track/server/s2",1,"Server Two",null,null,0,0,null,null,null,null]"#,
            ],
            "{version}"
        );
        assert_eq!(
            values(&mut raw, "SELECT json_array(json_extract(metadata,'$.musicbrainz_recording_id'),json_extract(metadata,'$.source_loudness.integrated_lufs'),json_extract(metadata,'$.artists[0].name'),json_extract(metadata,'$.moods[0].name')) FROM track_details WHERE track_key=1").await,
            [r#"["mb-recording-1",-10.5,"Artist A","Calm"]"#],
            "{version}"
        );
        assert_eq!(
            values(&mut raw, "SELECT json_array(link.role,link.position,coalesce(artist.name,tag.name,folder.name),tag.kind) FROM track_links link LEFT JOIN artists artist ON link.role IN(0,1) AND artist.artist_key=link.target LEFT JOIN tags tag ON link.role IN(2,3) AND tag.tag_key=link.target LEFT JOIN folders folder ON link.role=4 AND folder.folder_key=link.target WHERE link.track_key=1 ORDER BY link.role,link.position").await,
            [
                r#"[0,0,"Artist A",null]"#,
                r#"[2,0,"Rock",0]"#,
                r#"[3,0,"Calm",1]"#,
                r#"[4,0,"Album",null]"#,
            ],
            "{version}"
        );
        assert_eq!(
            values(&mut raw, "SELECT json_array(album.title,album.date_added,json_extract(album.metadata,'$.release_types'),json_extract(album.metadata,'$.musicbrainz_release_group_id'),(SELECT count(*) FROM album_links WHERE album_key=album.album_key)) FROM albums album WHERE album.media_uri='rufin:source/album/local-source/album-1'").await,
            [r#"["Album","2020-01-02T03:04:05",["album"],"mb-group",2]"#],
            "{version}"
        );
        assert_eq!(
            values(
                &mut raw,
                "SELECT json_array(media_uri,authority) FROM lyrics_cache"
            )
            .await,
            [r#"["file:///music/Album/01.flac","source"]"#],
            "{version}"
        );
        assert_eq!(
            values(
                &mut raw,
                "SELECT json_array(section_id,entity_kind,title) FROM home_entries"
            )
            .await,
            [r#"["recent","album","Server Album"]"#],
            "{version}"
        );
        // The file index keeps its rows and the inputs of its accepted CUE sheet.
        assert_eq!(
            values(&mut raw, "SELECT json_array((SELECT json_group_array(json_array(file_key,kind,relative_path,state)) FROM files),(SELECT json_group_array(json_array(file_key,position,path)) FROM file_inputs))").await,
            [r#"[[[1,"directory","Album","accepted"],[2,"media","Album/01.flac","accepted"],[3,"cue","Album/album.cue","accepted"],[4,"media","Album/album.flac","accepted"]],[[3,0,"/music/Album/album.flac"]]]"#],
            "{version}"
        );
        // Analysis results are measured again; nothing derived is stored.
        assert_eq!(
            values(&mut raw, "SELECT json_array((SELECT count(*) FROM loudness),(SELECT count(*) FROM album_loudness),(SELECT count(*) FROM waveform),(SELECT count(*) FROM fingerprint),(SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND tbl_name<>'documents'))").await,
            ["[0,0,0,0,0]"],
            "{version}"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT schema_version FROM connect_profile")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            0,
            "{version}"
        );
        raw.close().await.unwrap();
    }
}
