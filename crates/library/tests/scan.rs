use std::path::Path;

use library::{
    Database, Freshness, LocalFileKind, LocalFileState, LocalFileWrite, ReadCancellation, Scan,
};
use sqlx::FromRow;
use sqlx::sqlite::SqliteConnection;

#[tokio::test]
async fn publication_keeps_tracks_and_known_credits_beside_missing_related_objects() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.unwrap();
    let mut scan = Scan::begin(&database, "source", "Source", "source", None)
        .await
        .unwrap();
    scan.write_artists(&[&library::ArtistInput {
        object_id: "artist-one".into(),
        name: "Known Artist".into(),
        sort_text: Some("known artist".into()),
        ..Default::default()
    }])
    .await
    .unwrap();
    let mut tracks = [
        ("track-one", "One"),
        ("track-two", "Two"),
        ("track-one", "Duplicate"),
    ]
    .into_iter()
    .map(|(object_id, title)| library::TrackInput {
        object_id: object_id.into(),
        album_object_id: Some("album-one".into()),
        title: title.into(),
        sort_text: title.to_lowercase(),
        album_loudness: Some(library::LoudnessInput {
            replay_gain_db: Some(-6.0),
            ..Default::default()
        }),
        relations: Some(library::TrackRelationsInput {
            artists: vec![library::ArtistInput {
                object_id: "artist-one".into(),
                name: "Known Artist".into(),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    })
    .collect::<Vec<_>>();
    tracks[0].relations = None;
    tracks[2].source_loudness = Some(library::LoudnessInput {
        replay_gain_db: Some(-4.0),
        ..Default::default()
    });
    assert_eq!(
        scan.write_tracks(&tracks.iter().map(|track| (track, None)).collect::<Vec<_>>())
            .await
            .unwrap(),
        2
    );
    assert_eq!(scan.write_tracks(&[(&tracks[2], None)]).await.unwrap(), 0);
    scan.write_track_relations(&[("track-one", "missing-artist")], &[], &[])
        .await
        .unwrap();
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    scan.finish().await.unwrap();
    let update = updates.try_recv().unwrap();
    assert!(update.metadata);
    let mut raw = connection(&path).await;
    assert_eq!(
        sqlx::query_as::<_, (String, f64)>(
            "SELECT title,gain_db FROM tracks JOIN replay_gain_measurements
             ON entity_kind='track' AND entity_key=track_key WHERE object_id='track-one'",
        )
        .fetch_one(&mut raw)
        .await
        .unwrap(),
        ("One".into(), -4.0)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_artists")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks WHERE album_key IS NULL")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn remote_relocation_keeps_media_identity_and_updates_observed_access() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.unwrap();
    let source_id = library::SourceId::new("webdav:test");
    let uri = library::source_entity_uri(&source_id, "track", "stable-track");
    let mut observation = LocalFileWrite {
        path: "https://music.example/dav/original.flac".into(),
        root: "https://music.example/dav/".into(),
        relative_path: "original.flac".into(),
        kind: LocalFileKind::Media,
        size_bytes: Some(1234),
        mtime_ns: 0,
        device_id: None,
        inode: None,
        native_id: Some("oc-fileid:123".into()),
        picture_index: None,
        artist_pictures: None,
        revision: Some("etag-1".into()),
        parse_version: Some(1),
        state: LocalFileState::Accepted,
    };
    for renamed in [false, true] {
        let mut scan = Scan::begin(&database, source_id.as_str(), "DAV", "dav", None)
            .await
            .unwrap();
        scan.write_tracks(&[(
            &library::TrackInput {
                object_id: "stable-track".into(),
                title: "A track".into(),
                display_album: "Album".into(),
                display_artist: "Artist".into(),
                sort_text: "a track".into(),
                duration_millis: 2000,
                disc_number: 1,
                track_number: 1,
                source_format: Some("FLAC".into()),
                favorite: Some(false),
                source_path: Some(observation.path.to_owned()),
                loudness_analysis_key: [1; 32],
                ..Default::default()
            },
            None,
        )])
        .await
        .unwrap();
        scan.write_local_files(&[(observation.clone(), Vec::new())])
            .await
            .unwrap();
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        scan.finish().await.unwrap();
        let publication = updates.try_recv().unwrap();
        assert!(publication.metadata);
        let observed = database.observed_media_file(&uri).await.unwrap().unwrap();
        assert_eq!(observed.path, observation.path);
        assert_eq!(observed.revision, observation.revision);
        if !renamed {
            sqlx::query("INSERT INTO user_media_state(media_uri,favorite) VALUES(?1,1)")
                .bind(&uri)
                .execute(&mut connection(&path).await)
                .await
                .unwrap();
            observation.path = "https://music.example/dav/renamed.flac".into();
            observation.relative_path = "renamed.flac".into();
            let mut next_scan = Scan::begin_items(&database, source_id.as_str())
                .await
                .unwrap();
            let candidates = next_scan
                .local_file_reuse_candidates(&[observation.clone()])
                .await
                .unwrap();
            assert_eq!(candidates.len(), 1);
            assert_eq!(
                candidates[0].track_object_id.as_deref(),
                Some("stable-track")
            );
            assert_eq!(candidates[0].path, observed.path);
        }
    }
    let favorite: bool =
        sqlx::query_scalar("SELECT favorite FROM user_media_state WHERE media_uri=?1")
            .bind(&uri)
            .fetch_one(&mut connection(&path).await)
            .await
            .unwrap();
    assert!(favorite);
    database.close().await.unwrap();
}

#[tokio::test]
async fn reopening_preserves_accepted_source_rows_and_noop_refresh() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.unwrap();
    let publication = write_small_catalog(&database, "same", "Track One", false, b"art").await;
    assert!(publication.metadata);
    database.close().await.unwrap();
    let reopened = Database::open(&path).await.unwrap();
    assert_eq!(
        reopened
            .source_counts(publication.source, &ReadCancellation::default())
            .await
            .unwrap(),
        (1, 2)
    );
    let update = write_small_catalog(&reopened, "same", "Track One", false, b"art").await;
    assert!(!update.changed());
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn local_play_counts_survive_import_source_refresh_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.unwrap();
    write_small_catalog(&database, "before", "Track One", false, b"art").await;
    let uri =
        sqlx::query_scalar::<_, String>("SELECT media_uri FROM tracks WHERE object_id='track-one'")
            .fetch_one(&mut connection(&path).await)
            .await
            .unwrap();
    let listen = serde_json::json!({"version":1,"listen_key":500,"source_id":null,"media_uri":uri,"title":"Track One","artist":"Artist","album":"Album","duration_millis":1000,"started_at":1700000000,"local_period":"2023-11","listened_millis":1000,"skipped":false});
    for _ in 0..2 {
        let report = database
            .import_activity_jsonl(std::io::Cursor::new(serde_json::to_vec(&listen).unwrap()))
            .await
            .unwrap();
        assert_eq!(report.accepted, 1);
    }
    let counts =
        || sqlx::query_scalar::<_, i64>("SELECT local_play_count FROM tracks ORDER BY object_id");
    assert_eq!(
        counts()
            .fetch_all(&mut connection(&path).await)
            .await
            .unwrap(),
        [1, 0]
    );
    sqlx::query("DELETE FROM sources")
        .execute(&mut connection(&path).await)
        .await
        .unwrap();
    write_small_catalog(&database, "after", "Track One", false, b"art").await;
    assert_eq!(
        counts()
            .fetch_all(&mut connection(&path).await)
            .await
            .unwrap(),
        [1, 0]
    );
    database.close().await.unwrap();
    let reopened = Database::open(&path).await.unwrap();
    assert_eq!(
        counts()
            .fetch_all(&mut connection(&path).await)
            .await
            .unwrap(),
        [1, 0]
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn local_component_paths_page_beyond_one_batch() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    let publication =
        write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await;
    assert!(publication.metadata);
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
            native_id: None,
            picture_index: None,
            artist_pictures: None,
            revision: None,
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
                native_id: None,
                picture_index: None,
                artist_pictures: None,
                revision: None,
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

#[tokio::test]
async fn artist_names_and_sort_names_survive_sparse_credits_and_reset_with_complete_records() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.unwrap();
    let mut scan = Scan::begin(&database, "source", "Source", "source", None)
        .await
        .unwrap();
    scan.write_artists(&[
        &library::ArtistInput {
            object_id: "artist".into(),
            name: "The Cure".into(),
            sort_text: Some("cure, the".into()),
            favorite: Some(false),
            ..Default::default()
        },
        &library::ArtistInput {
            object_id: "artist".into(),
            name: "the cure".into(),
            ..Default::default()
        },
        &library::ArtistInput {
            object_id: "fallback".into(),
            name: "Missing Sort".into(),
            ..Default::default()
        },
    ])
    .await
    .unwrap();
    scan.finish().await.unwrap();
    let mut reader = connection(&path).await;
    assert_eq!(
        sqlx::query_as::<_, (String, String)>(
            "SELECT name,sort_text FROM artists ORDER BY sort_text"
        )
        .fetch_all(&mut reader)
        .await
        .unwrap(),
        [
            ("The Cure".into(), "cure, the".into()),
            ("Missing Sort".into(), "missing sort".into())
        ]
    );

    for sort in [None, Some("the cure"), Some("cure, the")] {
        let mut scan = Scan::begin_items(&database, "source").await.unwrap();
        scan.write_artists(&[&library::ArtistInput {
            object_id: "artist".into(),
            name: "The Cure".into(),
            sort_text: sort.map(str::to_owned),
            ..Default::default()
        }])
        .await
        .unwrap();
        scan.write_artists(&[&library::ArtistInput {
            object_id: "artist".into(),
            name: "The Cure".into(),
            ..Default::default()
        }])
        .await
        .unwrap();
        scan.finish().await.unwrap();
        let actual: String =
            sqlx::query_scalar("SELECT sort_text FROM artists WHERE object_id='artist'")
                .fetch_one(&mut reader)
                .await
                .unwrap();
        assert_eq!(actual, sort.unwrap_or("cure, the"));
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT name FROM artists WHERE object_id=\'artist\'")
                .fetch_one(&mut reader)
                .await
                .unwrap(),
            "The Cure"
        );
    }
}

#[tokio::test]
async fn sparse_artist_credits_preserve_metadata_and_complete_records_can_clear_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite");
    let database = Database::open(&path).await.unwrap();
    let mut scan = Scan::begin(&database, "source", "Source", "source", None)
        .await
        .unwrap();
    scan.write_artists(&[&library::ArtistInput {
        object_id: "artist".into(),
        name: "Artist".into(),
        sort_text: Some("artist".into()),
        musicbrainz_artist_id: Some("mbid".into()),
        favorite: Some(false),
        rating: Some(8),
        ..Default::default()
    }])
    .await
    .unwrap();
    scan.finish().await.unwrap();
    for point in [true, false] {
        let mut scan = if point {
            Scan::begin_items(&database, "source").await.unwrap()
        } else {
            Scan::begin(&database, "source", "Source", "source", None)
                .await
                .unwrap()
        };
        scan.write_artists(&[&library::ArtistInput {
            object_id: "artist".into(),
            name: "Artist".into(),
            ..Default::default()
        }])
        .await
        .unwrap();
        if !point {
            scan.write_artists(&[&library::ArtistInput {
                object_id: "artist".into(),
                name: "Artist".into(),
                sort_text: Some("artist".into()),
                favorite: Some(false),
                ..Default::default()
            }])
            .await
            .unwrap();
            scan.write_artists(&[&library::ArtistInput {
                object_id: "artist".into(),
                name: "Artist".into(),
                ..Default::default()
            }])
            .await
            .unwrap();
        }
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        scan.finish().await.unwrap();
        let outcome = updates.try_recv().unwrap();
        if point {
            assert!(!outcome.changed());
        }
        let mut reader = connection(&path).await;
        let metadata = sqlx::query_as::<_, (Option<String>, Option<i64>)>(
            "SELECT musicbrainz_artist_id,source_rating FROM artists WHERE object_id='artist'",
        )
        .fetch_one(&mut reader)
        .await
        .unwrap();
        assert_eq!(
            metadata,
            if point {
                (Some("mbid".into()), Some(80))
            } else {
                (None, None)
            }
        );
    }
}

async fn write_small_catalog(
    database: &Database,
    freshness: &str,
    title: &str,
    reverse: bool,
    genre_artwork: &[u8],
) -> library::CatalogUpdate {
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    let mut scan = Scan::begin(
        database,
        "source-one",
        "Source One",
        "source one",
        Some(Freshness::new(freshness.as_bytes()).expect("bounded freshness")),
    )
    .await
    .expect("begin scan");
    scan.write_albums(&[
        &library::AlbumInput {
            object_id: "album-one".into(),
            title: "Album One".into(),
            display_artist: "Artist One".into(),
            sort_text: "album one".into(),
            first_seen_at: Some(10),
            ..Default::default()
        },
        &library::AlbumInput {
            object_id: "album-one".into(),
            title: "Later file title".into(),
            year: Some(2025),
            release_date: Some("2025-01-01".into()),
            date_added: Some("2025-01-02".into()),
            musicbrainz_release_id: Some("release-one".into()),
            musicbrainz_release_group_id: Some("release-group-one".into()),
            is_compilation: Some(false),
            artwork_binding: Some(b"album-art".to_vec()),
            favorite: Some(false),
            rating: Some(7),
            first_seen_at: Some(20),
            ..Default::default()
        },
    ])
    .await
    .expect("stage album");
    scan.write_artists(&[&library::ArtistInput {
        object_id: "artist-one".into(),
        name: "Artist One".into(),
        sort_text: Some("artist one".into()),
        musicbrainz_artist_id: Some("artist-mbid-one".into()),
        artwork_binding: Some(b"artist-art".to_vec()),
        favorite: Some(false),
        ..Default::default()
    }])
    .await
    .expect("stage artist");
    scan.write_genres(&[&library::GenreInput {
        object_id: "genre-one".into(),
        name: "Genre One".into(),
        sort_text: Some("genre one".into()),
        artwork_binding: Some(genre_artwork.to_vec()),
    }])
    .await
    .expect("stage genre");
    scan.write_moods(&[&library::MoodInput {
        object_id: "mood-one".into(),
        name: "Mood One".into(),
        sort_text: "mood one".into(),
    }])
    .await
    .expect("stage mood");
    scan.write_folders(&[&library::FolderInput {
        object_id: "folder-one".into(),
        name: Some("Folder One".into()),
        sort_text: Some("folder one".into()),
        ..Default::default()
    }])
    .await
    .expect("stage folder");
    scan.write_playlist(&library::PlaylistInput {
        object_id: "playlist-one".into(),
        name: "Playlist One".into(),
        sort_text: "playlist one".into(),
        ..Default::default()
    })
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
    let entries = tracks
        .iter()
        .map(|(object_id, _, _)| library::PlaylistEntryInput {
            object_id: format!("entry-{object_id}"),
            track_id: (*object_id).into(),
        })
        .collect::<Vec<_>>();
    scan.write_playlist_page("playlist-one", 0, &entries, true)
        .await
        .expect("write completed playlist");
    scan.write_album_relations(
        &[("album-one", "artist-one")],
        &[("album-one", "genre-one")],
        &[("album-one", "Album")],
    )
    .await
    .expect("stage album relations");
    scan.finish().await.expect("finish scan");
    updates.try_recv().expect("catalog update")
}

async fn write_track(scan: &mut Scan, object_id: &str, title: &str, position: i64) {
    let normalized = format!("{} album one artist one note", title.to_lowercase());
    let media_uri = format!("file:///music/{object_id}.flac");
    scan.write_tracks(&[(
        &library::TrackInput {
            object_id: object_id.to_owned(),
            album_object_id: Some("album-one".into()),
            title: title.to_owned(),
            display_album: "Album One".into(),
            display_artist: "Artist One".into(),
            sort_text: normalized,
            duration_millis: 180_000,
            disc_number: 1,
            track_number: position + 1,
            year: Some(2025),
            release_date: Some("2025-01-01".into()),
            date_added: Some("2025-01-02".into()),
            media_uri: Some(media_uri),
            source_format: Some("FLAC".into()),
            comment: Some("Note".into()),
            bpm: Some(120),
            musicbrainz_recording_id: Some("recording-one".into()),
            musicbrainz_release_track_id: Some("release-track-one".into()),
            cue_path: Some("/music/album.cue".into()),
            cue_start_millis: Some(1_000),
            cue_end_millis: Some(181_000),
            artwork_binding: Some(format!("art-{object_id}").into_bytes()),
            favorite: Some(false),
            first_seen_at: Some(10 + position),
            source_play_count: Some(20 + position),
            source_skip_count: Some(position),
            source_last_played: Some(1_700_000_000 + position),
            loudness_analysis_key: [position as u8 + 1; 32],
            ..Default::default()
        },
        None,
    )])
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
}

#[tokio::test]
async fn canonical_publication_preserves_identity_and_user_overrides() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    let first = write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await;
    assert!(first.metadata);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT catalog_revision FROM sources WHERE object_id='source-one'"
        )
        .fetch_one(&mut connection(&path).await)
        .await
        .unwrap(),
        1
    );
    assert!(first.tracks_added && first.inventory);
    assert_eq!(first.source_id, library::SourceId::new("source-one"));
    assert_eq!(
        database
            .cached_source("source-one", &ReadCancellation::new())
            .await
            .expect("read saved source without provider contact")
            .map(|source| source.source),
        Some(first.source)
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
            "SELECT source_play_count,source_skip_count,source_last_played FROM tracks
             WHERE source_key=?1 AND object_id='track-one'",
        )
        .bind(first.source)
        .fetch_one(&mut outside)
        .await
        .expect("read initial provider activity facts"),
        (20, 0, Some(1_700_000_000))
    );
    sqlx::query(
        "INSERT INTO user_media_state(media_uri,favorite,rating)
         SELECT media_uri,1,90 FROM tracks WHERE object_id='track-one'",
    )
    .execute(&mut outside)
    .await
    .expect("write user overrides");

    let unchanged =
        write_small_catalog(&database, "fresh-two", "Track One", true, b"genre-art").await;
    assert!(!unchanged.changed());
    assert!(!unchanged.tracks_added && unchanged.inventory);
    assert_eq!(unchanged.artwork_digest, first.artwork_digest);
    assert!(
        Scan::accept_freshness(
            &database,
            "source-one",
            &Freshness::new(b"fresh-two".to_vec()).expect("bounded freshness"),
            &ReadCancellation::new(),
        )
        .await
        .expect("accept freshness")
    );

    let changed = write_small_catalog(
        &database,
        "fresh-three",
        "Changed Track",
        false,
        b"genre-art",
    )
    .await;
    assert!(changed.metadata);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT catalog_revision FROM sources WHERE object_id='source-one'"
        )
        .fetch_one(&mut outside)
        .await
        .unwrap(),
        2
    );
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
            "SELECT source_play_count,source_skip_count,source_last_played FROM tracks
             WHERE source_key=?1 AND object_id='track-one'",
        )
        .bind(first.source)
        .fetch_one(&mut outside)
        .await
        .expect("read current provider activity facts"),
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
            "changed track artist one album one note 2025".to_string(),
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
    let artwork_change = write_small_catalog(
        &database,
        "fresh-four",
        "Changed Track",
        false,
        b"genre-art-two",
    )
    .await;
    assert!(artwork_change.artwork);
    assert!(!artwork_change.metadata && !artwork_change.playlists);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT catalog_revision FROM sources WHERE object_id='source-one'"
        )
        .fetch_one(&mut outside)
        .await
        .unwrap(),
        2
    );
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
    let artist = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT artwork_binding FROM artists WHERE object_id='artist-one'",
    )
    .fetch_one(&mut reader)
    .await
    .expect("Artist keeps its own image");
    let artist: library::ArtistArtworkBinding = serde_json::from_slice(&artist).unwrap();
    assert_eq!(artist.source.as_deref(), Some(b"artist-art".as_slice()));
    assert_eq!(artist.fallback.as_deref(), Some(b"album-art".as_slice()));
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
        false
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
                .write_artists(&[&library::ArtistInput {
                    object_id: "artist-one".into(),
                    name: "Artist One".into(),
                    sort_text: Some("artist one".into()),
                    musicbrainz_artist_id: Some("artist-mbid-one".into()),
                    artwork_binding: Some(b"artist-art".to_vec()),
                    favorite: Some(false),
                    ..Default::default()
                }])
                .await
                .unwrap();
            point
                .write_genres(&[&library::GenreInput {
                    object_id: "genre-one".into(),
                    name: "Genre One".into(),
                    sort_text: Some("genre one".into()),
                    artwork_binding: Some(b"genre-art".to_vec()),
                }])
                .await
                .unwrap();
            point
                .write_albums(&[&library::AlbumInput {
                    object_id: "album-one".into(),
                    title: title.to_owned(),
                    display_artist: "Artist One".into(),
                    sort_text: "album one".into(),
                    year: Some(2025),
                    release_date: Some("2025-01-01".into()),
                    date_added: Some("2025-01-02".into()),
                    musicbrainz_release_id: Some("release-one".into()),
                    musicbrainz_release_group_id: Some("release-group-one".into()),
                    is_compilation: Some(false),
                    artwork_binding: Some(artwork.to_vec()),
                    favorite: Some(false),
                    rating: Some(7),
                    first_seen_at: Some(10),
                    ..Default::default()
                }])
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
            let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
            database.observe_catalog(move |update| {
                let _ = updates_tx.send(update);
                std::future::ready(())
            });
            point.finish().await.unwrap();
            let outcome = updates.try_recv().unwrap();
            assert!(if outcome.metadata {
                title == "Renamed Album" && previous != artwork
            } else if outcome.artwork {
                title == "Album One"
            } else {
                !outcome.playlists && !outcome.home && previous == artwork
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
    scan.write_playlist(&library::PlaylistInput {
        object_id: "completed-playlist".into(),
        name: "Completed".into(),
        sort_text: "completed".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    scan.write_playlist_page(
        "completed-playlist",
        0,
        &[library::PlaylistEntryInput {
            object_id: "completed-entry".into(),
            track_id: "track-one".into(),
        }],
        true,
    )
    .await
    .unwrap();
    scan.write_playlist_page(
        "playlist-one",
        0,
        &[library::PlaylistEntryInput {
            object_id: "unfinished-entry".into(),
            track_id: "track-two".into(),
        }],
        false,
    )
    .await
    .unwrap();
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    scan.finish().await.unwrap();
    assert!(updates.try_recv().unwrap().metadata);

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
    assert_eq!(sqlx::query_as::<_, (String, i64)>(
        "SELECT playlist.object_id,count(*) FROM playlists playlist JOIN playlist_entries USING(playlist_key) GROUP BY playlist.object_id ORDER BY playlist.object_id"
    ).fetch_all(&mut outside).await.unwrap(), [("completed-playlist".into(),1),("playlist-one".into(),2)]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM playlist_entries WHERE object_id='unfinished-entry'"
        )
        .fetch_one(&mut outside)
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn complete_scans_remove_unseen_items_after_partial_updates() {
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
        for &name in names {
            scan.write_genres(&[&library::GenreInput {
                object_id: name.to_owned(),
                name: name.to_owned(),
                sort_text: Some(name.to_owned()),
                ..Default::default()
            }])
            .await
            .unwrap();
        }
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        scan.finish().await.unwrap();
        let outcome = updates.try_recv().unwrap();
        assert!(if authoritative {
            outcome.metadata
        } else {
            !outcome.changed()
        });
        let mut reader = connection(&path).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM genres WHERE indexed")
                .fetch_one(&mut reader)
                .await
                .unwrap(),
            expected_count,
            "a complete scan clears membership omitted after an incomplete predecessor"
        );
    }
    let mut point = Scan::begin_items(&database, "source").await.unwrap();
    point
        .write_genres(&[&library::GenreInput {
            object_id: "a".into(),
            name: "Live name".into(),
            sort_text: Some("live name".into()),
            ..Default::default()
        }])
        .await
        .unwrap();
    point.finish().await.unwrap();
    let mut complete = Scan::begin(&database, "source", "Source", "source", None)
        .await
        .unwrap();
    complete
        .write_genres(&[&library::GenreInput {
            object_id: "a".into(),
            name: "a".into(),
            sort_text: Some("a".into()),
            ..Default::default()
        }])
        .await
        .unwrap();
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    complete.finish().await.unwrap();
    let update = updates.try_recv().unwrap();
    assert!(update.metadata);
    let mut reader = connection(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT name FROM genres WHERE object_id='a'")
            .fetch_one(&mut reader)
            .await
            .unwrap(),
        "a",
        "a complete read replaces values from an earlier point update"
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
                native_id: None,
                picture_index: None,
                artist_pictures: None,
                revision: None,
                parse_version: Some(1),
                state: LocalFileState::Accepted,
            },
            Vec::new(),
        )
    });
    for (index, artwork) in [
        Some(b"first".as_slice()),
        Some(b"first"),
        None,
        Some(b"second"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut scan = Scan::begin(&database, "local", "Local", "local", None)
            .await
            .unwrap();
        scan.write_genres(&[&library::GenreInput {
            object_id: "genre".into(),
            name: "Genre".into(),
            sort_text: Some("genre".into()),
            artwork_binding: artwork.map(<[u8]>::to_vec),
        }])
        .await
        .unwrap();
        if index > 0 {
            scan.incomplete();
        }
        scan.write_local_files(&files[..if index == 0 { 2 } else { 1 }])
            .await
            .unwrap();
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        scan.finish().await.unwrap();
        let outcome = updates.try_recv().unwrap();
        assert!(match index {
            0 => outcome.metadata,
            1 | 2 => !outcome.changed(),
            _ => outcome.artwork,
        });
        let mut reader = connection(&path).await;
        assert_eq!(
            sqlx::query_scalar::<_, Vec<u8>>(
                "SELECT artwork_binding FROM genres WHERE object_id='genre'"
            )
            .fetch_one(&mut reader)
            .await
            .unwrap(),
            if index < 3 {
                b"first".as_slice()
            } else {
                b"second".as_slice()
            }
        );
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
    let publication = write_small_catalog(&database, "fresh", "Cached Track", false, b"art").await;
    assert!(publication.metadata);
    drop(database);

    let reopened = Database::open(&path).await.expect("reopen cached Store");
    let cached = reopened
        .cached_source("source-one", &ReadCancellation::new())
        .await
        .expect("resolve configured source from SQLite")
        .expect("cached source");

    assert_eq!(cached.source, publication.source);
    assert!(cached.catalog_complete);
    assert_eq!(cached.display_name, "Source One");
    assert_eq!(cached.artwork_digest, publication.artwork_digest);
    assert_eq!(
        reopened.all_source_counts().await.unwrap()[&library::SourceId::new("source-one")],
        (1, 2)
    );
}

#[tokio::test]
async fn completed_batches_survive_later_failure_and_interleaved_changes() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    let first = write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await;
    assert!(first.metadata);

    let mut failed = Scan::begin(&database, "source-two", "Two", "two", None)
        .await
        .expect("begin failed scan");
    failed.begin_batch().await.expect("begin first batch");
    failed
        .write_genres(&[&library::GenreInput {
            object_id: "kept".into(),
            name: "Kept".into(),
            sort_text: Some("kept".into()),
            artwork_binding: Some(b"kept-art".to_vec()),
            ..Default::default()
        }])
        .await
        .expect("write a useful record");
    failed
        .write_tracks(&[(
            &library::TrackInput {
                object_id: "known".into(),
                title: "Known".into(),
                sort_text: "known".into(),
                favorite: Some(false),
                source_play_count: Some(1),
                ..Default::default()
            },
            None,
        )])
        .await
        .unwrap();
    failed.finish_batch().await.expect("commit first batch");
    let partial = database
        .cached_source("source-two", &ReadCancellation::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(partial.source, failed.source());
    assert!(!partial.catalog_complete);
    failed.begin_batch().await.expect("begin later batch");
    failed
        .write_genres(&[&library::GenreInput {
            object_id: "rolled-back".into(),
            name: "Rolled back".into(),
            sort_text: Some("rolled back".into()),
            ..Default::default()
        }])
        .await
        .expect("write before the invalid record");
    failed
        .write_source_user_data(&[(
            "track",
            "known".into(),
            library::SourceUserData {
                favorite: Some(true),
                play_count: Some(9),
                ..Default::default()
            },
        )])
        .await
        .unwrap();
    failed
        .write_tracks(&[(
            &library::TrackInput {
                object_id: "".into(),
                title: "Bad".into(),
                display_album: "".into(),
                display_artist: "".into(),
                sort_text: "bad".into(),
                duration_millis: 0,
                disc_number: 0,
                track_number: 0,
                favorite: Some(false),
                loudness_analysis_key: [0; 32],
                ..Default::default()
            },
            None,
        )])
        .await
        .expect_err("empty identity fails the current batch");
    failed
        .finish_batch()
        .await
        .expect("settle the failed batch");
    let mut outside = connection(&path).await;
    assert_eq!(sqlx::query_as::<_, (bool, i64)>(
        "SELECT source_favorite,source_play_count FROM tracks WHERE source_key=?1 AND object_id='known'"
    ).bind(failed.source()).fetch_one(&mut outside).await.unwrap(), (false, 1));
    failed
        .begin_batch()
        .await
        .expect("begin independent accepted facts");
    failed
        .write_source_user_data(&[(
            "track",
            "known".into(),
            library::SourceUserData {
                favorite: Some(true),
                play_count: Some(2),
                ..Default::default()
            },
        )])
        .await
        .unwrap();
    failed
        .finish_batch()
        .await
        .expect("commit independent accepted facts");
    assert_eq!(sqlx::query_as::<_, (bool, i64)>(
        "SELECT source_favorite,source_play_count FROM tracks WHERE source_key=?1 AND object_id='known'"
    ).bind(failed.source()).fetch_one(&mut outside).await.unwrap(), (true, 2));
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    failed.finish().await.expect("finish failed scan");
    let update = updates.try_recv().unwrap();
    assert!(update.metadata && update.artwork);
    assert!(
        !database
            .cached_source("source-two", &ReadCancellation::new())
            .await
            .unwrap()
            .unwrap()
            .catalog_complete
    );
    assert_eq!(
        sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT artwork_binding FROM genres WHERE source_key=?1 AND object_id='kept'"
        )
        .bind(update.source)
        .fetch_one(&mut outside)
        .await
        .unwrap(),
        b"kept-art"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT object_id FROM genres WHERE source_key=(SELECT source_key FROM sources WHERE object_id='source-two') ORDER BY object_id",
        )
        .fetch_all(&mut outside)
        .await
        .expect("read completed records after failure"),
        vec!["kept"]
    );

    let mut cancelled = Scan::begin_items(&database, "source-two")
        .await
        .expect("begin cancelled update");
    cancelled
        .begin_batch()
        .await
        .expect("begin completed batch");
    cancelled
        .write_genres(&[&library::GenreInput {
            object_id: "also-kept".into(),
            name: "Also kept".into(),
            sort_text: Some("also kept".into()),
            ..Default::default()
        }])
        .await
        .expect("write before cancellation");
    cancelled
        .finish_batch()
        .await
        .expect("commit before cancellation");
    cancelled
        .begin_batch()
        .await
        .expect("begin interrupted batch");
    cancelled
        .write_genres(&[&library::GenreInput {
            object_id: "cancelled".into(),
            name: "Cancelled".into(),
            sort_text: Some("cancelled".into()),
            ..Default::default()
        }])
        .await
        .expect("write interrupted record");
    drop(cancelled);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT object_id FROM genres WHERE source_key=(SELECT source_key FROM sources WHERE object_id='source-two') ORDER BY object_id",
        )
        .fetch_all(&mut outside)
        .await
        .expect("read completed records after cancellation"),
        vec!["also-kept", "kept"]
    );

    let mut oversized = Scan::begin(&database, "source-three", "Three", "three", None)
        .await
        .expect("begin oversized scan");
    let oversized_name = "x".repeat(8 * 1024 * 1024 + 1);
    oversized
        .write_genres(&[&library::GenreInput {
            object_id: "genre".into(),
            name: oversized_name,
            sort_text: Some("genre".into()),
            ..Default::default()
        }])
        .await
        .expect_err("oversized row fails staging");
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    oversized.finish().await.expect("finish oversized scan");
    assert!(updates.try_recv().is_err());

    let mut paged = Scan::begin(&database, "source-three", "Three", "three", None)
        .await
        .expect("begin paged scan");
    let mut tracks = (0..130)
        .map(|index| library::TrackInput {
            object_id: format!("page-{index}"),
            title: format!("Track {index}"),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    tracks[129].source_play_count = Some(-1);
    paged
        .write_tracks(&tracks.iter().map(|track| (track, None)).collect::<Vec<_>>())
        .await
        .expect_err("invalid activity rolls back its page");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks WHERE source_key=?1")
            .bind(paged.source())
            .fetch_one(&mut outside)
            .await
            .unwrap(),
        128
    );
    tracks[129].source_play_count = Some(1);
    assert_eq!(
        paged
            .write_tracks(&[(&tracks[128], None), (&tracks[129], None)])
            .await
            .expect("rolled-back observations remain writable"),
        2
    );
    paged.finish().await.expect("publish accepted pages");
    assert!(
        !database
            .cached_source("source-three", &ReadCancellation::new())
            .await
            .unwrap()
            .unwrap()
            .catalog_complete
    );

    let mut invalid = Scan::begin(&database, "source-four", "Four", "four", None)
        .await
        .expect("begin invalid scan");
    invalid
        .write_tracks(&[(
            &library::TrackInput {
                object_id: "orphan-track".into(),
                album_object_id: Some("missing-album".into()),
                title: "Orphan".into(),
                display_album: "Missing".into(),
                display_artist: "Artist".into(),
                sort_text: "orphan".into(),
                duration_millis: 1,
                disc_number: 1,
                track_number: 1,
                favorite: Some(false),
                loudness_analysis_key: [1; 32],
                ..Default::default()
            },
            None,
        )])
        .await
        .expect("stage song with unavailable album relationship");
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    invalid.finish().await.expect("publish useful song");
    let update = updates.try_recv().unwrap();
    assert!(update.metadata);
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

    sqlx::query("INSERT INTO replay_gain_measurements(source_key,entity_kind,entity_key,analysis_key,gain_db) SELECT source_key,'album',album_key,loudness_analysis_key,-6.0 FROM albums WHERE source_key=?1 AND object_id='album-one'")
        .bind(first.source.raw()).execute(&mut outside).await.expect("seed known album gain");
    let mut interleaved = Scan::begin_items(&database, "source-one")
        .await
        .expect("begin update alongside a favorite change");
    interleaved
        .write_genres(&[&library::GenreInput {
            object_id: "new-genre".into(),
            name: "New genre".into(),
            sort_text: Some("new genre".into()),
            ..Default::default()
        }])
        .await
        .expect("write newly acquired metadata");
    interleaved
        .write_albums(&[&library::AlbumInput {
            object_id: "album-one".into(),
            title: "Album One".into(),
            display_artist: "Artist One".into(),
            sort_text: "album one".into(),
            ..Default::default()
        }])
        .await
        .expect("read the first file of an album without its dates");
    interleaved
        .write_album_relations(&[], &[("album-one", "new-genre")], &[("album-one", "Live")])
        .await
        .expect("read one file's album credits");
    interleaved.incomplete();
    let favorite = library::FavoriteTarget::Track(library::source_entity_uri(
        &library::SourceId::new("source-one"),
        "track",
        "track-one",
    ));
    database
        .set_favorite(&favorite, true)
        .await
        .expect("favorite during acquisition");
    sqlx::query(
        "UPDATE sources SET catalog_revision=catalog_revision+1
         WHERE source_key=(SELECT source_key FROM sources WHERE object_id='source-one')",
    )
    .execute(&mut outside)
    .await
    .expect("advance accepted revision");
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    interleaved.finish().await.expect("finish valid update");
    let update = updates.try_recv().unwrap();
    assert!(update.metadata);
    assert!(
        database
            .favorite(&favorite)
            .await
            .expect("read retained favorite")
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, String)>(
            "SELECT year,release_date FROM albums WHERE source_key=?1 AND object_id='album-one'"
        )
        .bind(first.source.raw())
        .fetch_one(&mut outside)
        .await
        .expect("keep unread album dates"),
        (2025, "2025-01-01".into())
    );
    assert_eq!(
        sqlx::query_scalar::<_, f64>("SELECT gain_db FROM replay_gain_measurements WHERE source_key=?1 AND entity_kind='album'")
            .bind(first.source.raw()).fetch_one(&mut outside).await.expect("keep unread album gain"),
        -6.0
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT genre.object_id FROM album_genres relation JOIN albums album USING(album_key) JOIN genres genre USING(genre_key) WHERE album.source_key=?1 ORDER BY relation.position")
            .bind(first.source.raw()).fetch_all(&mut outside).await.expect("keep known genres and apply observed ones"),
        ["new-genre", "genre-one"]
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT release_type FROM album_release_types relation JOIN albums album USING(album_key) WHERE album.source_key=?1 ORDER BY position")
            .bind(first.source.raw()).fetch_all(&mut outside).await.expect("keep known release types"),
        ["Live", "Album"]
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT catalog_revision FROM sources
             WHERE source_key=(SELECT source_key FROM sources WHERE object_id='source-one')",
        )
        .fetch_one(&mut outside)
        .await
        .expect("read revision"),
        3
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM sources WHERE object_id='source-two' AND catalog_revision>0",
        )
        .fetch_one(&mut outside)
        .await
        .expect("count sources with completed records"),
        1
    );
    let removed = Scan::begin_items(&database, "source-one")
        .await
        .expect("begin update before source removal");
    database
        .remove_source(&library::SourceId::new("source-one"))
        .await
        .expect("remove source during acquisition");
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    removed.finish().await.expect("finish removed source");
    assert!(updates.try_recv().is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sources WHERE object_id='source-one'")
            .fetch_one(&mut outside)
            .await
            .expect("source stays removed"),
        0
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
    let publication =
        write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await;
    assert!(publication.metadata);
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
        .write_playlist(&library::PlaylistInput {
            object_id: "playlist-one".into(),
            name: "Playlist One".into(),
            sort_text: "playlist one".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    point
        .write_playlist(&library::PlaylistInput {
            object_id: "playlist-two".into(),
            name: "Other".into(),
            sort_text: "other".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    point
        .write_playlist_page(
            "playlist-one",
            0,
            &[library::PlaylistEntryInput {
                object_id: "entry-one".into(),
                track_id: "track-one".into(),
            }],
            true,
        )
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
    let initial =
        write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await;
    assert!(initial.metadata);

    let mut point = Scan::begin_items(&database, "source-one")
        .await
        .expect("begin Playlist point scan");
    point.begin_batch().await.unwrap();
    point
        .write_playlist(&library::PlaylistInput {
            object_id: "playlist-one".into(),
            name: "Playlist One".into(),
            sort_text: "playlist one".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    point
        .write_playlist_page(
            "playlist-one",
            0,
            &[library::PlaylistEntryInput {
                object_id: "entry-track-one".into(),
                track_id: "track-one".into(),
            }],
            false,
        )
        .await
        .unwrap();
    point
        .write_playlist_page(
            "playlist-one",
            1,
            &[library::PlaylistEntryInput {
                object_id: "entry-track-two".into(),
                track_id: "track-two".into(),
            }],
            true,
        )
        .await
        .unwrap();
    point.finish_batch().await.unwrap();

    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    point.finish().await.unwrap();
    let replayed = updates.try_recv().unwrap();
    assert!(!replayed.changed());
    assert_eq!(replayed.artwork_digest, initial.artwork_digest);
    assert!(!replayed.inventory);
    assert!(updates.try_recv().is_err());
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
        scan.write_folders(&[&library::FolderInput {
            object_id: "folder-one".into(),
            name: Some("Music".into()),
            sort_text: Some("music".into()),
            ..Default::default()
        }])
        .await
        .expect("stage repeated folder");
        scan.write_playlist(&library::PlaylistInput {
            object_id: "playlist-one".into(),
            name: "Playlist One".into(),
            sort_text: "playlist one".into(),
            ..Default::default()
        })
        .await
        .expect("stage repeated playlist");
        scan.write_playlist_page(
            "playlist-one",
            0,
            &[library::PlaylistEntryInput {
                object_id: "entry-one".into(),
                track_id: "track-one".into(),
            }],
            true,
        )
        .await
        .expect("stage repeated playlist entry");
        scan.finish_batch().await.expect("finish repeated batch");
    }

    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    scan.finish().await.expect("publish repeated observations");
    let update = updates.try_recv().unwrap();
    assert!(update.metadata);
}

#[tokio::test]
async fn duplicate_provider_entries_keep_first_occurrence_and_dense_positions() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    write_small_catalog(&database, "fresh-one", "Track One", false, b"genre-art").await;
    let mut scan = Scan::begin_items(&database, "source-one")
        .await
        .expect("begin Playlist point Scan");
    scan.begin_batch().await.expect("begin batch");
    scan.write_playlist(&library::PlaylistInput {
        object_id: "playlist-one".into(),
        name: "Playlist".into(),
        sort_text: "playlist".into(),
        ..Default::default()
    })
    .await
    .expect("stage Playlist");
    scan.write_playlist_page(
        "playlist-one",
        0,
        &[
            library::PlaylistEntryInput {
                object_id: "duplicate-a".into(),
                track_id: "track-one".into(),
            },
            library::PlaylistEntryInput {
                object_id: "duplicate-a".into(),
                track_id: "track-two".into(),
            },
            library::PlaylistEntryInput {
                object_id: "duplicate-b".into(),
                track_id: "track-one".into(),
            },
        ],
        true,
    )
    .await
    .expect("write repeated track occurrences");
    scan.finish_batch().await.expect("finish batch");
    scan.finish().await.expect("publish duplicate positions");

    let mut outside = connection(&path).await;
    assert_eq!(
        sqlx::query_as::<_, (String, String, i64)>(
            "SELECT entry.object_id,track.object_id,entry.position FROM playlist_entries entry
             JOIN playlists playlist USING(playlist_key) JOIN tracks track USING(track_key)
             WHERE playlist.object_id='playlist-one' ORDER BY entry.position",
        )
        .fetch_all(&mut outside)
        .await
        .expect("read normalized occurrence order"),
        [
            ("duplicate-a".into(), "track-one".into(), 0),
            ("duplicate-b".into(), "track-one".into(), 1)
        ]
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
        .write_playlist(&library::PlaylistInput {
            object_id: "playlist-one".into(),
            name: "Playlist One".into(),
            sort_text: "playlist one".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    point
        .write_playlist_page(
            "playlist-one",
            0,
            &[library::PlaylistEntryInput {
                object_id: "entry-track-two".into(),
                track_id: "track-two".into(),
            }],
            true,
        )
        .await
        .unwrap();
    point.finish_batch().await.unwrap();

    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    point.finish().await.unwrap();
    let update = updates.try_recv().unwrap();
    assert!(update.playlists && !update.metadata && !update.home);
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
        scan.write_artists(&[&library::ArtistInput {
            object_id: "artist-one".into(),
            name: "Artist".into(),
            sort_text: Some("artist".into()),
            favorite,
            ..Default::default()
        }])
        .await
        .unwrap();
        // A credit arriving after a complete record must not overwrite its state.
        scan.write_artists(&[&library::ArtistInput {
            object_id: "artist-one".into(),
            name: "Artist".into(),
            sort_text: Some("artist".into()),
            ..Default::default()
        }])
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

#[tokio::test]
async fn sparse_names_keep_casing_but_do_not_require_complete_metadata_for_renames() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite");
    let database = Database::open(&path).await.unwrap();
    let mut scan = Scan::begin(&database, "source", "Source", "source", None)
        .await
        .unwrap();
    scan.write_genres(&[&library::GenreInput {
        object_id: "genre".into(),
        name: "Rock".into(),
        sort_text: Some("rock".into()),
        ..Default::default()
    }])
    .await
    .unwrap();
    scan.write_artists(&[&library::ArtistInput {
        object_id: "artist".into(),
        name: "Artist".into(),
        sort_text: Some("artist".into()),
        ..Default::default()
    }])
    .await
    .unwrap();
    scan.finish().await.unwrap();
    let mut raw = connection(&path).await;
    for (artist, genre, complete, expected_artist, expected_genre) in [
        ("artist", "rock", false, "Artist", "Rock"),
        ("Renamed", "Alternative", false, "Renamed", "Alternative"),
        ("RENAMED", "ALTERNATIVE", true, "RENAMED", "ALTERNATIVE"),
    ] {
        let mut scan = Scan::begin_items(&database, "source").await.unwrap();
        let artist_sort = artist.to_lowercase();
        let genre_sort = genre.to_lowercase();
        scan.write_artists(&[&library::ArtistInput {
            object_id: "artist".into(),
            name: artist.to_owned(),
            sort_text: complete.then_some(artist_sort),
            ..Default::default()
        }])
        .await
        .unwrap();
        scan.write_genres(&[&library::GenreInput {
            object_id: "genre".into(),
            name: genre.to_owned(),
            sort_text: complete.then_some(genre_sort),
            ..Default::default()
        }])
        .await
        .unwrap();
        scan.finish().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT name FROM artists")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            expected_artist
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT name FROM genres")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            expected_genre
        );
    }
}
