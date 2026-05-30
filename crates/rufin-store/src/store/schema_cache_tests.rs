use std::fs;

use super::test_support::*;
#[test]
fn current_schema_initializes_empty_database() {
    let store = Store::open_memory().expect("open store");
    assert_eq!(store.schema_version().expect("schema version"), 10);
    assert!(store.foreign_keys_enabled().expect("foreign keys"));
    assert!(store.fts5_available().expect("fts5 table"));
    assert!(
        !store.table_exists("app_settings").expect("table lookup"),
        "settings are persisted outside the SQLite store"
    );
}
#[test]
fn current_schema_creates_library_route_indexes() {
    let store = Store::open_memory().expect("open store");
    for (table, index) in [
        ("albums", "albums_server_title_nocase_idx"),
        ("albums", "albums_server_artist_idx"),
        ("tracks", "tracks_server_artist_idx"),
        ("artists", "artists_server_name_nocase_idx"),
        ("album_artists", "album_artists_server_name_nocase_idx"),
        ("genres", "genres_server_name_nocase_idx"),
        ("playlists", "playlists_server_name_nocase_idx"),
        ("album_genres", "album_genres_server_genre_idx"),
        ("track_genres", "track_genres_server_genre_idx"),
        ("album_artist_links", "album_artist_links_server_artist_idx"),
        ("track_artist_links", "track_artist_links_server_artist_idx"),
        ("track_music_folders", "track_music_folders_folder_idx"),
        ("track_music_folders", "track_music_folders_track_idx"),
        ("track_local_matches", "track_local_matches_track_idx"),
    ] {
        assert!(index_exists(&store, table, index), "{index} should exist");
    }
}
#[test]
fn unsupported_file_store_resets_cache_database() {
    let path = std::env::temp_dir().join(format!(
        "rufin-store-test-{}-{}.sqlite",
        std::process::id(),
        "reset"
    ));
    let _cleanup = fs::remove_file(&path);
    let connection = rusqlite::Connection::open(&path).expect("open old connection");
    connection
        .execute_batch(
            "
                CREATE TABLE schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                INSERT INTO schema_migrations (version) VALUES (10);
                CREATE TABLE stale_cache (value TEXT NOT NULL);
                INSERT INTO stale_cache VALUES ('old row');
                CREATE TABLE servers (
                    server_id TEXT PRIMARY KEY,
                    provider TEXT NOT NULL,
                    name TEXT NOT NULL,
                    base_url TEXT NOT NULL,
                    user_id TEXT NOT NULL,
                    username TEXT NOT NULL,
                    trust_invalid_cert INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                INSERT INTO servers (
                    server_id, provider, name, base_url, user_id, username, trust_invalid_cert
                )
                VALUES (
                    'jellyfin:server:old', 'jellyfin', 'Old Server',
                    'https://music.example', 'user', 'demo', 0
                );
                ",
        )
        .expect("seed old schema");
    drop(connection);
    let store = Store::open(&path).expect("open reset store");
    assert_eq!(store.schema_version().expect("schema version"), 10);
    assert!(store.foreign_keys_enabled().expect("foreign keys"));
    assert!(store.fts5_available().expect("fts5 table"));
    assert!(
        !store
            .table_exists("schema_migrations")
            .expect("table lookup")
    );
    assert!(!store.table_exists("stale_cache").expect("table lookup"));
    assert!(store.table_exists("servers").expect("table lookup"));
    assert!(store.list_servers().expect("list servers").is_empty());
    drop(store);
    let _cleanup = fs::remove_file(&path);
    let _cleanup = fs::remove_file(sqlite_sidecar_path(&path, "-wal"));
    let _cleanup = fs::remove_file(sqlite_sidecar_path(&path, "-shm"));
}
#[test]
fn incomplete_user_version_ten_file_store_resets_cache_database() {
    let path = std::env::temp_dir().join(format!(
        "rufin-store-test-{}-{}.sqlite",
        std::process::id(),
        "incomplete"
    ));
    let _cleanup = fs::remove_file(&path);
    let connection = rusqlite::Connection::open(&path).expect("open incomplete connection");
    connection
        .execute_batch(
            "
                PRAGMA user_version = 10;
                CREATE TABLE servers (
                    server_id TEXT PRIMARY KEY,
                    provider TEXT NOT NULL,
                    name TEXT NOT NULL,
                    base_url TEXT NOT NULL,
                    user_id TEXT NOT NULL,
                    username TEXT NOT NULL,
                    trust_invalid_cert INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                INSERT INTO servers (
                    server_id, provider, name, base_url, user_id, username, trust_invalid_cert
                )
                VALUES (
                    'jellyfin:server:old', 'jellyfin', 'Old Server',
                    'https://music.example', 'user', 'demo', 0
                );
                ",
        )
        .expect("seed incomplete schema");
    drop(connection);
    let store = Store::open(&path).expect("open reset store");
    assert_eq!(store.schema_version().expect("schema version"), 10);
    assert!(store.table_exists("tracks").expect("table lookup"));
    assert!(store.list_servers().expect("list servers").is_empty());
    drop(store);
    let _cleanup = fs::remove_file(&path);
    let _cleanup = fs::remove_file(sqlite_sidecar_path(&path, "-wal"));
    let _cleanup = fs::remove_file(sqlite_sidecar_path(&path, "-shm"));
}
#[test]
fn current_file_store_reopens_without_dropping_servers() {
    let path = std::env::temp_dir().join(format!(
        "rufin-store-test-{}-{}.sqlite",
        std::process::id(),
        "preserve-current"
    ));
    let _cleanup = fs::remove_file(&path);
    let saved = saved_server();
    {
        let store = Store::open(&path).expect("open store");
        store.save_server(&saved).expect("save server");
        store
            .set_active_server(&saved.server.id)
            .expect("set active server");
    }

    let store = Store::open(&path).expect("reopen store");
    assert_eq!(store.schema_version().expect("schema version"), 10);
    assert_eq!(
        store.list_servers().expect("list servers"),
        vec![saved.clone()]
    );
    assert_eq!(store.active_server().expect("active server"), Some(saved));
    drop(store);
    let _cleanup = fs::remove_file(&path);
    let _cleanup = fs::remove_file(sqlite_sidecar_path(&path, "-wal"));
    let _cleanup = fs::remove_file(sqlite_sidecar_path(&path, "-shm"));
}
#[test]
fn future_user_version_file_store_resets_cache_database() {
    let path = std::env::temp_dir().join(format!(
        "rufin-store-test-{}-{}.sqlite",
        std::process::id(),
        "future"
    ));
    let _cleanup = fs::remove_file(&path);
    let saved = saved_server();
    {
        let store = Store::open(&path).expect("open store");
        store.save_server(&saved).expect("save server");
    }
    let connection = rusqlite::Connection::open(&path).expect("open future connection");
    connection
        .pragma_update(None, "user_version", 11)
        .expect("set future schema version");
    drop(connection);

    let store = Store::open(&path).expect("open reset store");
    assert_eq!(store.schema_version().expect("schema version"), 10);
    assert!(store.list_servers().expect("list servers").is_empty());
    drop(store);
    let _cleanup = fs::remove_file(&path);
    let _cleanup = fs::remove_file(sqlite_sidecar_path(&path, "-wal"));
    let _cleanup = fs::remove_file(sqlite_sidecar_path(&path, "-shm"));
}
#[test]
fn file_store_uses_wal_journal_mode() {
    let path = std::env::temp_dir().join(format!(
        "rufin-store-test-{}-{}.sqlite",
        std::process::id(),
        "wal"
    ));
    let _cleanup = fs::remove_file(&path);
    let store = Store::open(&path).expect("open file store");
    assert_eq!(store.journal_mode().expect("journal mode"), "wal");
    drop(store);
    let _cleanup = fs::remove_file(path);
}
#[test]
fn queue_snapshot_round_trip_by_server() {
    let store = Store::open_memory().expect("open store");
    let server_id = ServerId::fake(1);
    let mut queue = QueueEngine::new(server_id.clone());
    queue.append(&track(1, &album(1)));
    store
        .save_queue_snapshot(&queue.snapshot())
        .expect("save queue snapshot");
    assert_eq!(
        store
            .load_queue_snapshot(&server_id)
            .expect("load queue snapshot"),
        Some(queue.snapshot())
    );
    assert_eq!(
        store
            .load_queue_snapshot(&ServerId::fake(2))
            .expect("load queue snapshot"),
        None
    );
}
#[test]
fn active_server_round_trips_without_token() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    store
        .set_active_server(&saved.server.id)
        .expect("set active server");
    assert_eq!(store.active_server().expect("active server"), Some(saved));
}
#[test]
fn server_local_access_round_trips() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let access = ServerLocalAccess {
        server_id: saved.server.id.clone(),
        root_path: "/home/me/Music".to_string(),
        path_replace_from: Some("/media/music".to_string()),
        path_replace_to: Some("/home/me/Music".to_string()),
    };
    store
        .save_server_local_access(&access)
        .expect("save local access");
    assert_eq!(
        store
            .server_local_access(&saved.server.id)
            .expect("load local access"),
        Some(access)
    );
}
#[test]
fn track_local_path_round_trips() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    let album = album(1);
    let mut track = track(1, &album);
    track.local_path = Some("/home/me/Music/Track 1.flac".to_string());
    track.source_format = Some("flac".to_string());
    store
        .upsert_tracks(&saved.server.id, std::slice::from_ref(&track), generation)
        .expect("upsert track");
    assert_eq!(
        store
            .track_local_path(&saved.server.id, &track.id)
            .expect("track local path"),
        track.local_path
    );
    assert_eq!(
        store
            .track_source_format(&saved.server.id, &track.id)
            .expect("track source format"),
        track.source_format
    );
}
#[test]
fn albums_without_image_ref_can_be_loaded_for_external_art_prefetch() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    store
        .upsert_albums(
            &saved.server.id,
            &[album(1), album_with_image(2), album(3)],
            generation,
        )
        .expect("upsert albums");
    let albums = store
        .load_albums_without_image_ref(&saved.server.id, 0, 10)
        .expect("load albums without image ref");
    assert_eq!(
        albums.into_iter().map(|album| album.id).collect::<Vec<_>>(),
        vec![AlbumId::fake(1), AlbumId::fake(3)]
    );
}
#[test]
fn artists_without_image_ref_can_be_loaded_for_external_art_prefetch() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    store
        .upsert_artists(
            &saved.server.id,
            &[
                artist(1, None),
                artist(2, Some(image_ref("artist-two", "tag-two"))),
            ],
            false,
            generation,
        )
        .expect("upsert artists");
    let artists = store
        .load_artists_without_image_ref(&saved.server.id, false, 0, 10)
        .expect("load artists without image ref");
    assert_eq!(
        artists
            .into_iter()
            .map(|artist| artist.id)
            .collect::<Vec<_>>(),
        vec![ArtistId::fake(1)]
    );
}
#[test]
fn artists_without_provider_images_use_album_cover_fallback() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    let album = album_with_image(1);
    let track = track(1, &album);
    let artist = artist(1, None);
    store
        .upsert_albums(&saved.server.id, std::slice::from_ref(&album), generation)
        .expect("upsert album");
    store
        .upsert_tracks(&saved.server.id, std::slice::from_ref(&track), generation)
        .expect("upsert track");
    store
        .upsert_artists(
            &saved.server.id,
            std::slice::from_ref(&artist),
            false,
            generation,
        )
        .expect("upsert artist");
    let loaded = store
        .load_artists(&saved.server.id, false, 0, 10)
        .expect("load artists")
        .items
        .remove(0);
    let matching = store
        .load_artists_matching(&saved.server.id, false, "Artist 1", 0, 10)
        .expect("search artists")
        .items
        .remove(0);
    let global_search = store
        .search_library(&saved.server.id, "Artist 1", 10)
        .expect("search library");
    let detail = store
        .load_artist_detail(&saved.server.id, &artist.id)
        .expect("load artist detail")
        .expect("artist detail");
    assert_eq!(loaded.image_ref, album.image_ref);
    assert_eq!(matching.image_ref, album.image_ref);
    assert_eq!(global_search.artists[0].image_ref, album.image_ref);
    assert_eq!(detail.artist.image_ref, album.image_ref);
}
#[test]
fn artist_provider_image_wins_over_album_cover_fallback() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    let album = album_with_image(1);
    let track = track(1, &album);
    let artist_image = image_ref("artist-one", "artist-tag-one");
    let artist = artist(1, Some(artist_image.clone()));
    store
        .upsert_albums(&saved.server.id, std::slice::from_ref(&album), generation)
        .expect("upsert album");
    store
        .upsert_tracks(&saved.server.id, std::slice::from_ref(&track), generation)
        .expect("upsert track");
    store
        .upsert_artists(
            &saved.server.id,
            std::slice::from_ref(&artist),
            false,
            generation,
        )
        .expect("upsert artist");
    let loaded = store
        .load_artists(&saved.server.id, false, 0, 10)
        .expect("load artists")
        .items
        .remove(0);
    let detail = store
        .load_artist_detail(&saved.server.id, &artist.id)
        .expect("load artist detail")
        .expect("artist detail");
    assert_eq!(loaded.image_ref, Some(artist_image.clone()));
    assert_eq!(detail.artist.image_ref, Some(artist_image));
}
#[test]
fn album_artists_without_provider_images_use_album_cover_fallback() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    let album_artist_id = ArtistId::fake(8);
    let mut album = album_with_image(8);
    album.artist_id = Some(ArtistId::fake(99));
    album.album_artist_credits = vec![credit(album_artist_id.clone(), "Linked Album Artist")];
    let mut album_artist = artist(8, None);
    album_artist.name = "Linked Album Artist".to_string();
    store
        .upsert_albums(&saved.server.id, std::slice::from_ref(&album), generation)
        .expect("upsert album");
    store
        .upsert_artists(
            &saved.server.id,
            std::slice::from_ref(&album_artist),
            true,
            generation,
        )
        .expect("upsert album artist");
    let loaded = store
        .load_artists(&saved.server.id, true, 0, 10)
        .expect("load album artists")
        .items
        .into_iter()
        .find(|artist| artist.id == album_artist_id)
        .expect("album artist");
    let matching = store
        .load_artists_matching(&saved.server.id, true, "Linked Album Artist", 0, 10)
        .expect("search album artists")
        .items
        .remove(0);
    assert_eq!(loaded.image_ref, album.image_ref);
    assert_eq!(matching.image_ref, album.image_ref);
}
#[test]
fn track_local_matches_round_trip_and_replace() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let track_id = TrackId::fake(1);
    store
        .replace_track_local_matches(
            &saved.server.id,
            &[(
                track_id.clone(),
                "/home/me/Music/Track 1.flac".to_string(),
                "metadata".to_string(),
            )],
        )
        .expect("replace local matches");
    assert_eq!(
        store
            .track_local_match_path(&saved.server.id, &track_id)
            .expect("match path")
            .as_deref(),
        Some("/home/me/Music/Track 1.flac")
    );
    assert_eq!(
        store
            .track_local_match_paths(&saved.server.id)
            .expect("match paths"),
        vec![(track_id.clone(), "/home/me/Music/Track 1.flac".to_string())]
    );
    store
        .replace_track_local_matches(&saved.server.id, &[])
        .expect("clear local matches");
    assert_eq!(
        store
            .track_local_match_path(&saved.server.id, &track_id)
            .expect("match path"),
        None
    );
    assert!(
        store
            .track_local_match_paths(&saved.server.id)
            .expect("match paths")
            .is_empty()
    );
}
#[test]
fn selected_music_folder_filters_cached_tracks_and_search() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    let album = album(1);
    let tracks = vec![track(1, &album), track(2, &album)];
    let folder = MusicFolder {
        id: MusicFolderId::fake(1),
        name: "Music".to_string(),
    };
    store
        .upsert_albums(&saved.server.id, std::slice::from_ref(&album), generation)
        .expect("upsert album");
    store
        .upsert_tracks(&saved.server.id, &tracks, generation)
        .expect("upsert tracks");
    store
        .upsert_music_folders(&saved.server.id, std::slice::from_ref(&folder), generation)
        .expect("upsert folder");
    store
        .upsert_track_music_folder_memberships(
            &saved.server.id,
            &folder.id,
            std::slice::from_ref(&tracks[1]),
            generation,
        )
        .expect("upsert membership");
    store
        .set_selected_music_folder_id(&saved.server.id, Some(&folder.id))
        .expect("select folder");
    let page = store
        .load_tracks(&saved.server.id, 0, 10)
        .expect("load tracks");
    let search = store
        .load_tracks_matching(&saved.server.id, "Track", 0, 10)
        .expect("search tracks");
    let favorites = store
        .load_favorite_tracks(&saved.server.id)
        .expect("load favorites");
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, tracks[1].id);
    assert_eq!(search.total, 1);
    assert_eq!(search.items[0].id, tracks[1].id);
    assert!(favorites.is_empty());
}
#[test]
fn load_track_by_id_ignores_selected_music_folder_filter() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    let album = album(1);
    let tracks = vec![track(1, &album), track(2, &album)];
    let folder = MusicFolder {
        id: MusicFolderId::fake(1),
        name: "Music".to_string(),
    };
    store
        .upsert_albums(&saved.server.id, std::slice::from_ref(&album), generation)
        .expect("upsert album");
    store
        .upsert_tracks(&saved.server.id, &tracks, generation)
        .expect("upsert tracks");
    store
        .upsert_music_folders(&saved.server.id, std::slice::from_ref(&folder), generation)
        .expect("upsert folder");
    store
        .upsert_track_music_folder_memberships(
            &saved.server.id,
            &folder.id,
            std::slice::from_ref(&tracks[1]),
            generation,
        )
        .expect("upsert membership");
    store
        .set_selected_music_folder_id(&saved.server.id, Some(&folder.id))
        .expect("select folder");
    let loaded = store
        .load_track(&saved.server.id, &tracks[0].id)
        .expect("load track")
        .expect("track");
    assert_eq!(loaded.id, tracks[0].id);
}
#[test]
fn stale_selected_music_folder_is_cleared_after_sync() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let folder = MusicFolder {
        id: MusicFolderId::fake(1),
        name: "Music".to_string(),
    };
    let first_generation = store.begin_sync(&saved.server.id).expect("begin sync");
    store
        .upsert_music_folders(
            &saved.server.id,
            std::slice::from_ref(&folder),
            first_generation,
        )
        .expect("upsert folder");
    store
        .set_selected_music_folder_id(&saved.server.id, Some(&folder.id))
        .expect("select folder");
    store
        .complete_sync(&saved.server.id, first_generation)
        .expect("complete first sync");
    let second_generation = store.begin_sync(&saved.server.id).expect("begin next sync");
    store
        .complete_sync(&saved.server.id, second_generation)
        .expect("complete second sync");
    assert!(
        store
            .list_music_folders(&saved.server.id)
            .expect("list folders")
            .is_empty()
    );
    assert_eq!(
        store
            .selected_music_folder_id(&saved.server.id)
            .expect("selected folder"),
        None
    );
}
#[test]
fn cached_album_and_track_pages_round_trip() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    let album = album(1);
    let tracks = vec![track(1, &album), track(2, &album)];
    store
        .upsert_albums(&saved.server.id, std::slice::from_ref(&album), generation)
        .expect("upsert album");
    store
        .upsert_tracks(&saved.server.id, &tracks, generation)
        .expect("upsert tracks");
    store
        .complete_sync(&saved.server.id, generation)
        .expect("complete sync");
    let albums = store
        .load_albums(&saved.server.id, 0, 25)
        .expect("load albums");
    let detail = store
        .load_album_detail(&saved.server.id, &album.id)
        .expect("load detail")
        .expect("detail");
    assert_eq!(albums.total, 1);
    assert_eq!(albums.items, vec![album.clone()]);
    assert_eq!(detail.0, album);
    assert_eq!(detail.1, tracks);
}
#[test]
fn image_refs_round_trip_for_cached_library_models() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    let artist = artist(1, Some(image_ref("artist-one", "artist-tag")));
    let genre = genre(1, Some(image_ref("genre-one", "genre-tag")));
    let mut album = album_with_image(1);
    album.genres = vec![genre.name.clone()];
    let track = track(1, &album);
    let playlist = playlist(1, Some(image_ref("playlist-one", "playlist-tag")));
    store
        .upsert_albums(&saved.server.id, std::slice::from_ref(&album), generation)
        .expect("upsert album");
    store
        .upsert_tracks(&saved.server.id, std::slice::from_ref(&track), generation)
        .expect("upsert track");
    store
        .upsert_artists(
            &saved.server.id,
            std::slice::from_ref(&artist),
            false,
            generation,
        )
        .expect("upsert artist");
    store
        .upsert_genres(&saved.server.id, std::slice::from_ref(&genre), generation)
        .expect("upsert genre");
    store
        .upsert_playlists(
            &saved.server.id,
            std::slice::from_ref(&playlist),
            generation,
        )
        .expect("upsert playlist");
    assert_eq!(
        store
            .load_albums(&saved.server.id, 0, 1)
            .expect("load albums")
            .items[0]
            .image_ref,
        album.image_ref
    );
    assert_eq!(
        store
            .load_tracks(&saved.server.id, 0, 1)
            .expect("load tracks")
            .items[0]
            .image_ref,
        track.image_ref
    );
    assert_eq!(
        store
            .load_artists(&saved.server.id, false, 0, 1)
            .expect("load artists")
            .items[0]
            .image_ref,
        artist.image_ref
    );
    assert_eq!(
        store
            .load_genres(&saved.server.id, 0, 1)
            .expect("load genres")
            .items[0]
            .image_ref,
        genre.image_ref
    );
    assert_eq!(
        store
            .load_playlists(&saved.server.id, 0, 1)
            .expect("load playlists")
            .items[0]
            .image_ref,
        playlist.image_ref
    );
}
#[test]
fn album_reads_use_track_cover_when_album_cover_is_missing() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    let album = album(1);
    let fallback_image = image_ref("album-track-cover", "album-track-tag");
    let mut first_track = track(1, &album);
    first_track.image_ref = Some(fallback_image.clone());
    let second_track = track(2, &album);
    store
        .upsert_albums(&saved.server.id, std::slice::from_ref(&album), generation)
        .expect("upsert album");
    store
        .upsert_tracks(
            &saved.server.id,
            &[first_track.clone(), second_track.clone()],
            generation,
        )
        .expect("upsert tracks");

    let albums = store
        .load_albums(&saved.server.id, 0, 25)
        .expect("load albums");
    let detail = store
        .load_album_detail(&saved.server.id, &album.id)
        .expect("load detail")
        .expect("detail");

    assert_eq!(albums.items[0].image_ref, Some(fallback_image.clone()));
    assert_eq!(detail.0.image_ref, Some(fallback_image));
    assert_eq!(detail.1, vec![first_track, second_track]);
}
#[test]
fn paged_reads_return_items_beyond_previous_snapshot_caps() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    let albums = (1..=505).map(album).collect::<Vec<_>>();
    let tracks = (1..=1005)
        .map(|number| track(number, &albums[(number as usize - 1) % albums.len()]))
        .collect::<Vec<_>>();
    store
        .upsert_albums(&saved.server.id, &albums, generation)
        .expect("upsert albums");
    store
        .upsert_tracks(&saved.server.id, &tracks, generation)
        .expect("upsert tracks");
    let album_page = store
        .load_albums(&saved.server.id, 500, 10)
        .expect("load album page");
    let track_page = store
        .load_tracks(&saved.server.id, 1000, 10)
        .expect("load track page");
    assert_eq!(album_page.total, 505);
    assert_eq!(album_page.items.len(), 5);
    assert_eq!(track_page.total, 1005);
    assert_eq!(track_page.items.len(), 5);
}
#[test]
fn sorted_track_pages_keep_global_order_across_page_boundaries() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    let mut first_album = album(1);
    first_album.title = "Alpha Album".to_string();
    let mut second_album = album(2);
    second_album.title = "Beta Album".to_string();
    let mut tracks = vec![
        track(1, &second_album),
        track(2, &first_album),
        track(3, &first_album),
        track(4, &second_album),
    ];
    for track in &mut tracks {
        track.title = format!("Needle {}", track.track_number);
    }
    store
        .upsert_albums(&saved.server.id, &[first_album, second_album], generation)
        .expect("upsert albums");
    store
        .upsert_tracks(&saved.server.id, &tracks, generation)
        .expect("upsert tracks");

    let full_page = store
        .load_tracks_sorted(&saved.server.id, LibraryField::Album, false, 0, 10)
        .expect("load full sorted page");
    let first_page = store
        .load_tracks_sorted(&saved.server.id, LibraryField::Album, false, 0, 2)
        .expect("load first sorted page");
    let second_page = store
        .load_tracks_sorted(&saved.server.id, LibraryField::Album, false, 2, 2)
        .expect("load second sorted page");
    let combined_ids = first_page
        .items
        .iter()
        .chain(second_page.items.iter())
        .map(|track| track.id.clone())
        .collect::<Vec<_>>();
    let full_ids = full_page
        .items
        .iter()
        .map(|track| track.id.clone())
        .collect::<Vec<_>>();

    assert_eq!(
        full_ids,
        vec![
            tracks[1].id.clone(),
            tracks[2].id.clone(),
            tracks[0].id.clone(),
            tracks[3].id.clone()
        ]
    );
    assert_eq!(combined_ids, full_ids);

    let search_page = store
        .load_tracks_matching_sorted(
            &saved.server.id,
            "Needle",
            LibraryField::Album,
            false,
            0,
            10,
        )
        .expect("load sorted search page");
    assert_eq!(
        search_page
            .items
            .iter()
            .map(|track| track.id.clone())
            .collect::<Vec<_>>(),
        full_ids
    );
}
#[test]
fn paged_search_reads_items_beyond_previous_snapshot_caps() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    let mut albums = (1..=505).map(album).collect::<Vec<_>>();
    albums[504].genres = vec!["Needle Genre".to_string()];
    let tracks = (1..=1005)
        .map(|number| track(number, &albums[(number as usize - 1) % albums.len()]))
        .collect::<Vec<_>>();
    let artists = (1..=505)
        .map(|number| artist(number, None))
        .collect::<Vec<_>>();
    let album_artists = artists.clone();
    let mut genres = (1..=505)
        .map(|number| genre(number, None))
        .collect::<Vec<_>>();
    genres[504].name = "Needle Genre".to_string();
    genres[504].track_count = 1;
    let playlists = (1..=505)
        .map(|number| playlist(number, None))
        .collect::<Vec<_>>();
    store
        .upsert_albums(&saved.server.id, &albums, generation)
        .expect("upsert albums");
    store
        .upsert_tracks(&saved.server.id, &tracks, generation)
        .expect("upsert tracks");
    store
        .upsert_artists(&saved.server.id, &artists, false, generation)
        .expect("upsert artists");
    store
        .upsert_artists(&saved.server.id, &album_artists, true, generation)
        .expect("upsert album artists");
    store
        .upsert_genres(&saved.server.id, &genres, generation)
        .expect("upsert genres");
    store
        .upsert_playlists(&saved.server.id, &playlists, generation)
        .expect("upsert playlists");
    let album_page = store
        .load_albums_matching(&saved.server.id, "Needle Genre", 0, 10)
        .expect("search albums");
    let track_page = store
        .load_tracks_matching(&saved.server.id, "Track 1005", 0, 10)
        .expect("search tracks");
    let artist_page = store
        .load_artists_matching(&saved.server.id, false, "Artist 505", 0, 10)
        .expect("search artists");
    let album_artist_page = store
        .load_artists_matching(&saved.server.id, true, "Artist 505", 0, 10)
        .expect("search album artists");
    let genre_page = store
        .load_genres_matching(&saved.server.id, "Needle Genre", 0, 10)
        .expect("search genres");
    let playlist_page = store
        .load_playlists_matching(&saved.server.id, "Playlist 505", 0, 10)
        .expect("search playlists");
    assert_eq!(album_page.items, vec![albums[504].clone()]);
    assert_eq!(track_page.items, vec![tracks[1004].clone()]);
    assert_eq!(artist_page.items, vec![artists[504].clone()]);
    assert_eq!(album_artist_page.items, vec![album_artists[504].clone()]);
    assert_eq!(genre_page.items, vec![genres[504].clone()]);
    assert_eq!(playlist_page.items, vec![playlists[504].clone()]);
}
#[test]
fn playlist_detail_stores_ordered_tracks() {
    let store = Store::open_memory().expect("open store");
    let saved = saved_server();
    store.save_server(&saved).expect("save server");
    let generation = store.begin_sync(&saved.server.id).expect("begin sync");
    let album = album(1);
    let track_one = track(1, &album);
    let track_two = track(2, &album);
    let playlist = playlist(1, None);
    store
        .upsert_albums(&saved.server.id, std::slice::from_ref(&album), generation)
        .expect("upsert album");
    store
        .upsert_tracks(
            &saved.server.id,
            &[track_one.clone(), track_two.clone()],
            generation,
        )
        .expect("upsert tracks");
    store
        .upsert_playlists(
            &saved.server.id,
            std::slice::from_ref(&playlist),
            generation,
        )
        .expect("upsert playlist");
    store
        .upsert_playlist_tracks(
            &saved.server.id,
            &playlist.id,
            &[track_two.clone(), track_one.clone()],
            generation,
        )
        .expect("upsert playlist tracks");
    let detail = store
        .load_playlist_detail(&saved.server.id, &playlist.id)
        .expect("load playlist detail")
        .expect("playlist detail");
    assert_eq!(detail.playlist, playlist);
    assert_eq!(detail.tracks, vec![track_two, track_one]);
}
