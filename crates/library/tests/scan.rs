use std::path::Path;

use library::{
    Database, Freshness, LocalFileKind, LocalFileState, LocalFileWrite, ReadCancellation,
};
use sqlx::sqlite::SqliteConnection;

#[tokio::test]
async fn publication_keeps_tracks_and_known_credits_beside_missing_related_objects() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.unwrap();

    let mut scan_catalog = async {
        if (&database)
            .source_identity_key(&library::SourceId::new("source"))
            .await?
            .is_none()
        {
            (&database)
                .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
                .await?;
        }
        database.catalog("source", None).await
    }
    .await
    .unwrap();
    let scan = scan_catalog.start_observation().await.unwrap();
    {
        let write = library::CatalogBatch {
            artists: vec![library::ArtistObservation {
                metadata: library::ArtistMetadata {
                    name: "Known Artist".into(),
                    sort_text: Some("known artist".into()),
                    ..Default::default()
                },
                object_id: "artist-one".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    let mut tracks = [
        ("track-one", "One"),
        ("track-two", "Two"),
        ("track-one", "Duplicate"),
    ]
    .into_iter()
    .map(|(object_id, title)| library::TrackObservation {
        metadata: library::TrackMetadata {
            title: title.into(),
            sort_text: title.to_lowercase(),
            album_loudness: Some(library::NativeGain {
                replay_gain_db: Some(-6.0),
                ..Default::default()
            }),
            artists: vec![library::ArtistCredit {
                object_id: "artist-one".into(),
                name: "Known Artist".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        object_id: object_id.into(),
        album_object_id: Some("album-one".into()),
        ..Default::default()
    })
    .collect::<Vec<_>>();
    tracks[0].metadata.artists.clear();
    tracks[2].metadata.source_loudness = Some(library::NativeGain {
        replay_gain_db: Some(-4.0),
        ..Default::default()
    });
    {
        let write = library::CatalogBatch {
            tracks: tracks.to_vec(),
            ..Default::default()
        };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch {
            tracks: vec![tracks[2].clone()],
            ..Default::default()
        };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |_, update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    {
        let update = {
            scan_catalog
                .complete_source_observation(
                    scan,
                    &[
                        "track", "album", "artist", "genre", "mood", "folder", "playlist",
                    ],
                    None,
                )
                .await
                .unwrap();

            scan_catalog
                .complete_source_observation(scan, &[], None)
                .await
                .unwrap();

            scan_catalog.into_changes()
        };
        (&database).publish_catalog_update("source", update).await;
    };
    let update = updates.try_recv().unwrap();
    assert!(update.metadata);
    let mut raw = connection(&path).await;
    assert_eq!(
        sqlx::query_as::<_, (String, f64)>(
            "SELECT title,json_extract(metadata,'$.source_loudness.replay_gain_db') FROM tracks WHERE object_id='track-one'",
        )
        .fetch_one(&mut raw)
        .await
        .unwrap(),
        ("Duplicate".into(), -4.0)
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
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks WHERE album_key IS NOT NULL")
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
        let mut scan_catalog = async {
            if (&database)
                .source_identity_key(&library::SourceId::new(source_id.as_str()))
                .await?
                .is_none()
            {
                (&database)
                    .reconcile_source(&library::SourceId::new(source_id.as_str()), "webdav", "DAV")
                    .await?;
            }
            database.catalog(source_id.as_str(), None).await
        }
        .await
        .unwrap();
        let scan = scan_catalog.start_observation().await.unwrap();
        {
            let write = library::CatalogBatch {
                tracks: vec![library::TrackObservation {
                    user_data: library::NativeUserState {
                        favorite: Some(false),
                        ..Default::default()
                    },
                    metadata: library::TrackMetadata {
                        title: "A track".into(),
                        album: "Album".into(),
                        artist: "Artist".into(),
                        sort_text: "a track".into(),
                        duration_millis: 2000,
                        disc_number: 1,
                        track_number: 1,
                        source_format: Some("FLAC".into()),
                        ..Default::default()
                    },
                    object_id: "stable-track".into(),
                    source_path: Some(observation.path.to_owned()),
                    loudness_analysis_key: [1; 32],
                    ..Default::default()
                }],
                ..Default::default()
            };

            scan_catalog.apply_catalog(&write, true).await.unwrap();
        };
        scan_catalog
            .write_file_observations(&[(observation.clone(), Vec::new())], Vec::new(), true)
            .await
            .unwrap();
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |_, update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        {
            let update = {
                scan_catalog
                    .complete_source_observation(
                        scan,
                        &[
                            "track", "album", "artist", "genre", "mood", "folder", "playlist",
                        ],
                        None,
                    )
                    .await
                    .unwrap();

                scan_catalog
                    .complete_source_observation(scan, &[], None)
                    .await
                    .unwrap();

                scan_catalog.into_changes()
            };
            (&database)
                .publish_catalog_update(source_id.as_str(), update)
                .await;
        };
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
            let candidates = database
                .source_file_reuse_candidates(source_id.as_str(), &[observation.clone()])
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
    let publication = write_small_catalog(&database, "same", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/47f24da63989b3586adaa7682d88f1bb","image":{"kind":"native","source_id":"source","item_id":"art","tag":null},"blur_hash":null}]"#).await;
    assert!(publication.1.metadata);
    database.close().await.unwrap();
    let reopened = Database::open(&path).await.unwrap();
    assert_eq!(
        reopened
            .source_counts(publication.0, &ReadCancellation::default())
            .await
            .unwrap(),
        (1, 2)
    );
    let update = write_small_catalog(&reopened, "same", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/47f24da63989b3586adaa7682d88f1bb","image":{"kind":"native","source_id":"source","item_id":"art","tag":null},"blur_hash":null}]"#).await;
    assert!(!update.1.changed());
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn local_play_counts_survive_import_source_refresh_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.unwrap();
    write_small_catalog(&database, "before", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/47f24da63989b3586adaa7682d88f1bb","image":{"kind":"native","source_id":"source","item_id":"art","tag":null},"blur_hash":null}]"#).await;
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
    write_small_catalog(&database, "after", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/47f24da63989b3586adaa7682d88f1bb","image":{"kind":"native","source_id":"source","item_id":"art","tag":null},"blur_hash":null}]"#).await;
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
        write_small_catalog(&database, "fresh-one", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;
    assert!(publication.1.metadata);
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
    for page in observations.chunks(128) {
        database
            .catalog("source-one", None)
            .await
            .unwrap()
            .write_file_observations(page, Vec::new(), true)
            .await
            .expect("write Local observations");
    }
    let first = database
        .source_component_page("source-one", &["/music".into()], &["/music/".into()], None)
        .await
        .unwrap();
    let second = database
        .source_component_page(
            "source-one",
            &["/music".into()],
            &["/music/".into()],
            first.last().map(String::as_str),
        )
        .await
        .unwrap();
    assert_eq!((first.len(), second.len()), (128, 3));
}

async fn connection(path: &Path) -> SqliteConnection {
    super::support::connection(path).await
}

#[tokio::test]
async fn artist_names_and_sort_names_survive_sparse_credits_and_reset_with_complete_records() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.unwrap();
    database
        .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
        .await
        .unwrap();
    let mut write = library::CatalogBatch::default();
    {
        write.artists.extend_from_slice(&[
            library::ArtistObservation {
                metadata: library::ArtistMetadata {
                    name: "The Cure".into(),
                    sort_text: Some("cure, the".into()),
                    ..Default::default()
                },
                object_id: "artist".into(),
                ..Default::default()
            },
            library::ArtistObservation {
                metadata: library::ArtistMetadata {
                    name: "Missing Sort".into(),
                    ..Default::default()
                },
                object_id: "fallback".into(),
                ..Default::default()
            },
        ]);
        write.artists.len()
    };
    {
        write.tracks.push(library::TrackObservation {
            metadata: library::TrackMetadata {
                title: "Track".into(),
                artists: vec![library::ArtistCredit {
                    object_id: "artist".into(),
                    name: "the cure".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            object_id: "track".into(),
            ..Default::default()
        });
        write.tracks.len()
    };
    {
        let result = database
            .catalog("source", None)
            .await
            .unwrap()
            .apply_catalog(&write, true)
            .await
            .unwrap();
        result
    };
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
        let mut write = library::CatalogBatch::default();
        {
            write.artists.push(library::ArtistObservation {
                metadata: library::ArtistMetadata {
                    name: "The Cure".into(),
                    sort_text: sort.map(str::to_owned),
                    ..Default::default()
                },
                object_id: "artist".into(),
                ..Default::default()
            });
            write.artists.len()
        };
        {
            write.tracks.push(library::TrackObservation {
                metadata: library::TrackMetadata {
                    title: "Track".into(),
                    artists: vec![library::ArtistCredit {
                        object_id: "artist".into(),
                        name: "the cure".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                object_id: "track".into(),
                ..Default::default()
            });
            write.tracks.len()
        };
        {
            let result = database
                .catalog("source", None)
                .await
                .unwrap()
                .apply_catalog(&write, true)
                .await
                .unwrap();
            result
        };
        assert_eq!(
            sqlx::query_as::<_, (String, String)>(
                "SELECT name,sort_text FROM artists WHERE object_id='artist'"
            )
            .fetch_one(&mut reader)
            .await
            .unwrap(),
            ("The Cure".into(), sort.unwrap_or("the cure").into())
        );
    }
}

#[tokio::test]
async fn sparse_artist_credits_preserve_metadata_and_complete_records_can_clear_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.unwrap();
    database
        .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
        .await
        .unwrap();
    let mut write = library::CatalogBatch::default();
    {
        write.artists.push(library::ArtistObservation {
            user_data: library::NativeUserState {
                favorite: Some(false),
                rating: (Some(Some(8))).map(|rating| rating.map(|value| value * 10)),
                ..Default::default()
            },
            metadata: library::ArtistMetadata {
                name: "Artist".into(),
                sort_text: Some("artist".into()),
                musicbrainz_artist_id: Some("mbid".into()),
                ..Default::default()
            },
            object_id: "artist".into(),
            ..Default::default()
        });
        write.artists.len()
    };
    {
        let result = database
            .catalog("source", None)
            .await
            .unwrap()
            .apply_catalog(&write, true)
            .await
            .unwrap();
        result
    };
    let mut reader = connection(&path).await;
    for complete in [false, true] {
        let mut write = library::CatalogBatch::default();
        if complete {
            {
                write.artists.push(library::ArtistObservation {
                    metadata: library::ArtistMetadata {
                        name: "Artist".into(),
                        ..Default::default()
                    },
                    object_id: "artist".into(),
                    ..Default::default()
                });
                write.artists.len()
            };
        } else {
            {
                write.tracks.push(library::TrackObservation {
                    metadata: library::TrackMetadata {
                        title: "Track".into(),
                        artists: vec![library::ArtistCredit {
                            object_id: "artist".into(),
                            name: "Artist".into(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    object_id: "track".into(),
                    ..Default::default()
                });
                write.tracks.len()
            };
        }
        {
            let result = database
                .catalog("source", None)
                .await
                .unwrap()
                .apply_catalog(&write, true)
                .await
                .unwrap();
            result
        };
        assert_eq!(
            sqlx::query_as::<_, (Option<String>, Option<i64>)>(
                "SELECT json_extract(artists.metadata,'$.musicbrainz_artist_id'),(SELECT rating FROM native_media_state WHERE media_uri=artists.media_uri) FROM artists WHERE object_id='artist'"
            )
            .fetch_one(&mut reader)
            .await
            .unwrap(),
            (if complete { None } else { Some("mbid".into()) }, Some(80))
        );
    }
    let mut write = library::CatalogBatch::default();
    {
        write.artists.push(library::ArtistObservation {
            user_data: library::NativeUserState {
                rating: Some(None),
                ..Default::default()
            },
            metadata: library::ArtistMetadata {
                name: "Artist".into(),
                ..Default::default()
            },
            object_id: "artist".into(),
            ..Default::default()
        });
        write.artists.len()
    };
    {
        let result = database
            .catalog("source", None)
            .await
            .unwrap()
            .apply_catalog(&write, true)
            .await
            .unwrap();
        result
    };
    assert_eq!(
        sqlx::query_scalar::<_, Option<i64>>(
            "SELECT (SELECT rating FROM native_media_state WHERE media_uri=artists.media_uri) FROM artists WHERE object_id='artist'"
        )
        .fetch_one(&mut reader)
        .await
        .unwrap(),
        None
    );
}

async fn write_small_catalog(
    database: &Database,
    freshness: &str,
    title: &str,
    reverse: bool,
    genre_artwork: &[u8],
) -> (library::SourceKey, library::CatalogUpdate) {
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |identity, update| {
        let source = identity.unwrap().1;
        let _ = updates_tx.send((source, update));
        std::future::ready(())
    });

    let mut scan_catalog = async {
        if (database)
            .source_identity_key(&library::SourceId::new("source-one"))
            .await?
            .is_none()
        {
            (database)
                .reconcile_source(
                    &library::SourceId::new("source-one"),
                    "subsonic",
                    "Source One",
                )
                .await?;
        }
        database.catalog("source-one", None).await
    }
    .await
    .expect("begin scan");
    let scan = scan_catalog.start_observation().await.unwrap();
    {
        let write = library::CatalogBatch { albums: vec![library::AlbumObservation{user_data:library::NativeUserState{favorite:Some(false),rating:(Some(Some(7))).map(|rating|rating.map(|value|value*10)),..Default::default()},metadata:library::AlbumMetadata{title:"Album One".into(),artist:"Artist One".into(),sort_text:"album one".into(),album_artists:vec![library::ArtistCredit {
                object_id: "artist-one".into(),
                name: "Artist One".into(),
                ..Default::default()
            }],genres:vec![library::TagReference {object_id: "genre-one".into(),name: "Genre One".into(),..Default::default()}],release_types:vec!["Album".into()],year:Some(2025),release_date:Some("2025-01-01".into()),date_added:Some("2025-01-02".into()),musicbrainz_release_id:Some("release-one".into()),musicbrainz_release_group_id:Some("release-group-one".into()),is_compilation:Some(false),..Default::default()},object_id: "album-one".into(),first_seen_at: Some(10),artwork_binding: Some(br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d453ddcc6a2bb4616a7509452b0c0edb","image":{"kind":"native","source_id":"source","item_id":"album-art","tag":null},"blur_hash":null}]"#.to_vec()),..Default::default()}], ..Default::default() };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch { artists: vec![library::ArtistObservation{user_data:library::NativeUserState{favorite:Some(false),..Default::default()},metadata:library::ArtistMetadata{name:"Artist One".into(),sort_text:Some("artist one".into()),musicbrainz_artist_id:Some("artist-mbid-one".into()),..Default::default()},object_id: "artist-one".into(),artwork_binding: Some(br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/8799b34127e8fddd3fdcdff25c21b7dd","image":{"kind":"native","source_id":"source","item_id":"artist-art","tag":null},"blur_hash":null}]"#.to_vec()),..Default::default()}], ..Default::default() };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch {
            genres: vec![library::GenreObservation {
                metadata: library::TagMetadata {
                    name: "Genre One".into(),
                    sort_text: Some("genre one".into()),
                    ..Default::default()
                },
                object_id: "genre-one".into(),
                artwork_binding: Some(genre_artwork.to_vec()),
            }],
            ..Default::default()
        };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch {
            moods: vec![library::MoodObservation {
                metadata: library::TagMetadata {
                    name: "Mood One".into(),
                    sort_text: Some("mood one".into()),
                    ..Default::default()
                },
                object_id: "mood-one".into(),
            }],
            ..Default::default()
        };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch {
            folders: vec![library::FolderObservation {
                metadata: library::FolderMetadata {
                    name: Some("Folder One".into()),
                    sort_text: Some("folder one".into()),
                    ..Default::default()
                },
                object_id: "folder-one".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch {
            playlists: vec![library::PlaylistObservation {
                object_id: "playlist-one".into(),
                name: "Playlist One".into(),
                sort_text: "playlist one".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };

    let tracks = [
        ("track-one", title, 0_i64),
        ("track-two", "Track Two", 1_i64),
    ];
    let order: &[usize] = if reverse { &[1, 0] } else { &[0, 1] };
    for &index in order {
        let (object_id, track_title, position) = tracks[index];
        write_track(&mut scan_catalog, object_id, track_title, position).await;
    }
    let entries = tracks
        .iter()
        .map(|(object_id, _, _)| library::PlaylistEntryObservation {
            object_id: format!("entry-{object_id}"),
            track_id: (*object_id).into(),
        })
        .collect::<Vec<_>>();
    scan_catalog
        .write_source_playlist_page("playlist-one", 0, &entries, true)
        .await
        .expect("write completed playlist");
    let update = {
        let update = {
            scan_catalog
                .complete_source_observation(
                    scan,
                    &[
                        "track", "album", "artist", "genre", "mood", "folder", "playlist",
                    ],
                    None,
                )
                .await
                .unwrap();

            scan_catalog
                .complete_source_observation(
                    scan,
                    &[],
                    Some(Freshness::new(freshness.as_bytes()).expect("bounded freshness")).as_ref(),
                )
                .await
                .unwrap();

            scan_catalog.into_changes()
        };
        (database)
            .publish_catalog_update("source-one", update.clone())
            .await;
        update
    };
    if update.changed() || !update.track_rekeys.is_empty() {
        let (source, published) = updates.try_recv().expect("catalog update");
        assert_eq!(published, update);
        (source, update)
    } else {
        assert!(updates.try_recv().is_err());
        (
            database
                .source_identity_key(&library::SourceId::new("source-one"))
                .await
                .unwrap()
                .unwrap(),
            update,
        )
    }
}

async fn write_track(
    catalog: &mut library::Catalog<'_>,
    object_id: &str,
    title: &str,
    position: i64,
) {
    let normalized = format!("{} album one artist one note", title.to_lowercase());
    let media_uri = format!("file:///music/{object_id}.flac");
    catalog
        .apply_catalog(
            &library::CatalogBatch {
                tracks: vec![library::TrackObservation {
                    user_data: library::NativeUserState {
                        favorite: Some(false),
                        play_count: Some(20 + position),
                        skip_count: Some(position),
                        last_played: Some(1_700_000_000 + position),
                        ..Default::default()
                    },
                    metadata: library::TrackMetadata {
                        title: title.to_owned(),
                        album: "Album One".into(),
                        artist: "Artist One".into(),
                        sort_text: normalized,
                        duration_millis: 180_000,
                        disc_number: 1,
                        track_number: position + 1,
                        year: Some(2025),
                        release_date: Some("2025-01-01".into()),
                        date_added: Some("2025-01-02".into()),
                        source_format: Some("FLAC".into()),
                        comment: Some("Note".into()),
                        bpm: Some(120),
                        musicbrainz_recording_id: Some("recording-one".into()),
                        musicbrainz_release_track_id: Some("release-track-one".into()),
                        artists: vec![library::ArtistCredit {
                            object_id: "artist-one".into(),
                            name: "Artist One".into(),
                            sort_text: Some("artist one".into()),
                            musicbrainz_artist_id: Some("artist-mbid-one".into()),
                            ..Default::default()
                        }],
                        genres: vec![library::TagReference {
                            object_id: "genre-one".into(),
                            name: "Genre One".into(),
                            ..Default::default()
                        }],
                        moods: vec![library::TagReference {
                            object_id: "mood-one".into(),
                            name: "Mood One".into(),
                            sort_text: Some("mood one".into()),
                        }],
                        ..Default::default()
                    },
                    object_id: object_id.to_owned(),
                    album_object_id: Some("album-one".into()),
                    media_uri: Some(media_uri),
                    cue_path: Some("/music/album.cue".into()),
                    cue_start_millis: Some(1_000),
                    cue_end_millis: Some(181_000),
                    artwork_binding: Some(
                        serde_json::to_vec(&[library::ArtworkCandidate::new(
                            library::ArtworkImage::Native {
                                source_id: library::SourceId::new("source"),
                                item_id: format!("art-{object_id}"),
                                tag: None,
                            },
                            None,
                        )])
                        .unwrap(),
                    ),
                    first_seen_at: Some(10 + position),
                    loudness_analysis_key: [position as u8 + 1; 32],
                    folders: Some(vec!["folder-one".into(), "folder-one".into()]),
                    ..Default::default()
                }],
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn canonical_publication_preserves_identity_and_user_overrides() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    let first = write_small_catalog(&database, "fresh-one", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;
    assert!(first.1.metadata);
    let first_revision = sqlx::query_scalar::<_, i64>(
        "SELECT catalog_revision FROM sources WHERE object_id='source-one'",
    )
    .fetch_one(&mut connection(&path).await)
    .await
    .unwrap();
    assert!(first_revision > 0);
    assert_eq!(
        database
            .cached_source("source-one", &ReadCancellation::new())
            .await
            .expect("read saved source without provider contact")
            .map(|source| source.source),
        Some(first.0)
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
            "SELECT COALESCE((SELECT play_count FROM native_media_state WHERE media_uri=tracks.media_uri),0),COALESCE((SELECT skip_count FROM native_media_state WHERE media_uri=tracks.media_uri),0),(SELECT last_played FROM native_media_state WHERE media_uri=tracks.media_uri) FROM tracks\n             WHERE source_key=?1 AND object_id='track-one'",
        )
        .bind(first.0)
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
        write_small_catalog(&database, "fresh-two", "Track One", true, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;
    assert!(!unchanged.1.changed());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT catalog_revision FROM sources WHERE object_id='source-one'"
        )
        .fetch_one(&mut outside)
        .await
        .unwrap(),
        first_revision
    );
    assert!(!unchanged.1.changed());
    assert!(
        (&database)
            .accept_freshness(
                "source-one",
                &Freshness::new(b"fresh-two".to_vec()).expect("bounded freshness"),
                &ReadCancellation::new()
            )
            .await
            .expect("accept freshness")
    );

    let changed = write_small_catalog(
        &database,
        "fresh-three",
        "Changed Track",
        false,
        br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#,
    )
    .await;
    assert!(changed.1.metadata);
    let changed_revision = sqlx::query_scalar::<_, i64>(
        "SELECT catalog_revision FROM sources WHERE object_id='source-one'",
    )
    .fetch_one(&mut outside)
    .await
    .unwrap();
    assert!(changed_revision > first_revision);
    assert_eq!(changed.0, first.0);
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
            "SELECT COALESCE((SELECT play_count FROM native_media_state WHERE media_uri=tracks.media_uri),0),COALESCE((SELECT skip_count FROM native_media_state WHERE media_uri=tracks.media_uri),0),(SELECT last_played FROM native_media_state WHERE media_uri=tracks.media_uri) FROM tracks\n             WHERE source_key=?1 AND object_id='track-one'",
        )
        .bind(first.0)
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
            "SELECT normalized_search, date_added, source_format,json_extract(metadata,'$.comment'),bpm,
                    json_extract(metadata,'$.musicbrainz_recording_id'),json_extract(metadata,'$.musicbrainz_release_track_id'),
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
                    artist.musicbrainz_artist_id, length(genre.native_artwork)
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
            br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#.len() as i64,
        )
    );
    let artwork_before = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT native_artwork FROM genres\n         WHERE source_key=(SELECT source_key FROM sources WHERE object_id='source-one') ORDER BY genre_key LIMIT 1",
    )
    .fetch_one(&mut outside)
    .await
    .expect("read artwork before Genre change");
    let artwork_change = write_small_catalog(
        &database,
        "fresh-four",
        "Changed Track",
        false,
        br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/7c93b1fd0795697d6e43b90dfda4dc0b","image":{"kind":"native","source_id":"source","item_id":"genre-art-two","tag":null},"blur_hash":null}]"#,
    )
    .await;
    assert!(artwork_change.1.artwork);
    assert!(!artwork_change.1.metadata && !artwork_change.1.playlists);
    let artwork_after = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT native_artwork FROM genres\n         WHERE source_key=(SELECT source_key FROM sources WHERE object_id='source-one') ORDER BY genre_key LIMIT 1",
    )
    .fetch_one(&mut outside)
    .await
    .expect("read artwork after Genre change");
    assert_ne!(artwork_after, artwork_before);
    assert_eq!(artwork_after, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/7c93b1fd0795697d6e43b90dfda4dc0b","image":{"kind":"native","source_id":"source","item_id":"genre-art-two","tag":null},"blur_hash":null}]"#);
}

#[tokio::test]
async fn artwork_publication_keeps_one_effective_binding_per_entity() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");

    write_small_catalog(&database, "shared", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;
    let artist = match library::CatalogQuery::uris(
        library::CatalogKind::Artist,
        vec![library::source_entity_uri(
            &library::SourceId::new("source-one"),
            "artist",
            "artist-one",
        )],
    )
    .rows(&database, 0, 1, &ReadCancellation::new())
    .await
    .unwrap()
    {
        library::CatalogRows::Artists(mut rows) => rows.pop().unwrap().artwork_binding.unwrap(),
        _ => unreachable!(),
    };
    let artist: Vec<library::ArtworkCandidate> = serde_json::from_slice(&artist).unwrap();
    assert_eq!(serde_json::to_vec(&artist[..1]).unwrap(), br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/8799b34127e8fddd3fdcdff25c21b7dd","image":{"kind":"native","source_id":"source","item_id":"artist-art","tag":null},"blur_hash":null}]"#);
    assert_eq!(serde_json::to_vec(&artist[1..2]).unwrap(), br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d453ddcc6a2bb4616a7509452b0c0edb","image":{"kind":"native","source_id":"source","item_id":"album-art","tag":null},"blur_hash":null}]"#);
    assert!(matches!(
        artist[2].image,
        library::ArtworkImage::Artist { .. }
    ));
    let shared = {
        let album = match library::CatalogQuery::uris(
            library::CatalogKind::Album,
            vec![library::source_entity_uri(
                &library::SourceId::new("source-one"),
                "album",
                "album-one",
            )],
        )
        .rows(&database, 0, 1, &ReadCancellation::new())
        .await
        .unwrap()
        {
            library::CatalogRows::Albums(mut rows) => rows.pop().unwrap(),
            _ => unreachable!(),
        };
        let tracks = match library::CatalogQuery::uris(
            library::CatalogKind::Track,
            vec![
                library::cue_media_uri("track-one", "file:///music/track-one.flac", 1000, 181000),
                library::cue_media_uri("track-two", "file:///music/track-two.flac", 1000, 181000),
            ],
        )
        .rows(&database, 0, 2, &ReadCancellation::new())
        .await
        .unwrap()
        {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!(),
        };
        (
            album.artwork_binding.unwrap(),
            tracks[0].artwork_binding.clone().unwrap(),
            tracks[1].artwork_binding.clone().unwrap(),
        )
    };
    assert_eq!(
        shared,
        (
            br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d453ddcc6a2bb4616a7509452b0c0edb","image":{"kind":"native","source_id":"source","item_id":"album-art","tag":null},"blur_hash":null}]"#.to_vec(),
            br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d453ddcc6a2bb4616a7509452b0c0edb","image":{"kind":"native","source_id":"source","item_id":"album-art","tag":null},"blur_hash":null}]"#.to_vec(),
            br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d453ddcc6a2bb4616a7509452b0c0edb","image":{"kind":"native","source_id":"source","item_id":"album-art","tag":null},"blur_hash":null}]"#.to_vec()
        )
    );

    database.set_distinct_track_covers(true).await.unwrap();
    assert_eq!(
        (&database)
            .accept_freshness(
                "source-one",
                &Freshness::new(b"shared".to_vec()).expect("freshness"),
                &ReadCancellation::new()
            )
            .await
            .expect("check policy freshness"),
        true
    );
    write_small_catalog(&database, "distinct", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;
    let distinct = {
        let album = match library::CatalogQuery::uris(
            library::CatalogKind::Album,
            vec![library::source_entity_uri(
                &library::SourceId::new("source-one"),
                "album",
                "album-one",
            )],
        )
        .rows(&database, 0, 1, &ReadCancellation::new())
        .await
        .unwrap()
        {
            library::CatalogRows::Albums(mut rows) => rows.pop().unwrap(),
            _ => unreachable!(),
        };
        let tracks = match library::CatalogQuery::uris(
            library::CatalogKind::Track,
            vec![
                library::cue_media_uri("track-one", "file:///music/track-one.flac", 1000, 181000),
                library::cue_media_uri("track-two", "file:///music/track-two.flac", 1000, 181000),
            ],
        )
        .rows(&database, 0, 2, &ReadCancellation::new())
        .await
        .unwrap()
        {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!(),
        };
        (
            album.artwork_binding.unwrap(),
            tracks[0].artwork_binding.clone().unwrap(),
            tracks[1].artwork_binding.clone().unwrap(),
        )
    };
    assert_eq!(
        distinct,
        (
            br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d453ddcc6a2bb4616a7509452b0c0edb","image":{"kind":"native","source_id":"source","item_id":"album-art","tag":null},"blur_hash":null}]"#.to_vec(),
            br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/c4b5102ebbc0df8d15f13bed5b96ca7f","image":{"kind":"native","source_id":"source","item_id":"art-track-one","tag":null},"blur_hash":null}]"#.to_vec(),
            br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d52e05cfa6bba38cd1c6126637fd1673","image":{"kind":"native","source_id":"source","item_id":"art-track-two","tag":null},"blur_hash":null}]"#.to_vec(),
        )
    );
}

#[tokio::test]
async fn point_album_artwork_updates_cached_members_without_replacing_distinct_covers() {
    for distinct in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("library.sqlite");
        let database = Database::open(&path).await.unwrap();
        database.set_distinct_track_covers(distinct).await.unwrap();
        write_small_catalog(&database, "first", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;
        let mut reader = connection(&path).await;
        for (title, artwork) in [
            ("Album One", br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/248b9a062b6cfb466fd9de98afea60d8","image":{"kind":"native","source_id":"source","item_id":"new-art","tag":null},"blur_hash":null}]"#.as_slice()),
            ("Renamed Album", br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/3c2564f714c17a85ff6b8f5b5f65c092","image":{"kind":"native","source_id":"source","item_id":"newer-art","tag":null},"blur_hash":null}]"#.as_slice()),
            ("Renamed Album", br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/3c2564f714c17a85ff6b8f5b5f65c092","image":{"kind":"native","source_id":"source","item_id":"newer-art","tag":null},"blur_hash":null}]"#.as_slice()),
        ] {
            let previous = sqlx::query_scalar::<_, Vec<u8>>(
                "SELECT native_artwork FROM albums WHERE object_id='album-one'",
            )
            .fetch_one(&mut reader)
            .await
            .unwrap();
            let previous_title=sqlx::query_scalar::<_,String>("SELECT title FROM albums WHERE object_id='album-one'").fetch_one(&mut reader).await.unwrap();

let mut point_catalog = database.catalog("source-one", None).await.unwrap();
            {
let write = library::CatalogBatch { artists: vec![library::ArtistObservation{user_data:library::NativeUserState{favorite:Some(false),..Default::default()},metadata:library::ArtistMetadata{name:"Artist One".into(),sort_text:Some("artist one".into()),musicbrainz_artist_id:Some("artist-mbid-one".into()),..Default::default()},object_id: "artist-one".into(),artwork_binding: Some(br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/8799b34127e8fddd3fdcdff25c21b7dd","image":{"kind":"native","source_id":"source","item_id":"artist-art","tag":null},"blur_hash":null}]"#.to_vec()),..Default::default()}], ..Default::default() };
        point_catalog.apply_catalog(&write, true).await.unwrap();
        };
            {
let write = library::CatalogBatch { genres: vec![library::GenreObservation{metadata:library::TagMetadata{name: "Genre One".into(),sort_text: Some("genre one".into()),..Default::default()},object_id: "genre-one".into(),artwork_binding: Some(br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#.to_vec())}], ..Default::default() };
        point_catalog.apply_catalog(&write, true).await.unwrap();
        };
            {
let write = library::CatalogBatch { albums: vec![library::AlbumObservation{user_data:library::NativeUserState{favorite:Some(false),rating:(Some(Some(7))).map(|rating|rating.map(|value|value*10)),..Default::default()},metadata:library::AlbumMetadata{title:title.to_owned(),artist:"Artist One".into(),sort_text:"album one".into(),year:Some(2025),release_date:Some("2025-01-01".into()),date_added:Some("2025-01-02".into()),musicbrainz_release_id:Some("release-one".into()),musicbrainz_release_group_id:Some("release-group-one".into()),is_compilation:Some(false),album_artists:vec![library::ArtistCredit {
                        object_id: "artist-one".into(),
                        name: "Artist One".into(),
                        ..Default::default()
                    }],genres:vec![library::TagReference {object_id: "genre-one".into(),name: "Genre One".into(),..Default::default()}],release_types:vec!["Album".into()],..Default::default()},object_id: "album-one".into(),artwork_binding: Some(artwork.to_vec()),first_seen_at: Some(10),..Default::default()}], ..Default::default() };
        point_catalog.apply_catalog(&write, true).await.unwrap();
        };
            let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
            database.observe_catalog(move |_, update| {
                let _ = updates_tx.send(update);
                std::future::ready(())
            });
            {
let update = point_catalog.into_changes();
        (&database).publish_catalog_update("source-one", update).await;};
            let outcome = updates.try_recv().map(|update| { assert!(update.changed() || !update.track_rekeys.is_empty()); update }).unwrap_or_default();
            assert_eq!(outcome.metadata,previous_title!=title);
            assert_eq!(outcome.artwork,previous!=artwork);
            assert!(!outcome.playlists&&!outcome.home);
            assert_eq!(
                database.track_artwork_bindings(&sqlx::query_scalar::<_,String>("SELECT media_uri FROM tracks ORDER BY object_id").fetch_all(&mut reader).await.unwrap(),&ReadCancellation::new()).await.unwrap(),
                if distinct {
                    vec![br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/c4b5102ebbc0df8d15f13bed5b96ca7f","image":{"kind":"native","source_id":"source","item_id":"art-track-one","tag":null},"blur_hash":null}]"#.to_vec(), br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d52e05cfa6bba38cd1c6126637fd1673","image":{"kind":"native","source_id":"source","item_id":"art-track-two","tag":null},"blur_hash":null}]"#.to_vec()]
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
    write_small_catalog(&database, "complete", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;

    let mut scan_catalog = database.catalog("source-one", None).await.unwrap();
    write_track(&mut scan_catalog, "track-one", "Changed From Valid Page", 0).await;
    {
        let write = library::CatalogBatch {
            playlists: vec![library::PlaylistObservation {
                object_id: "completed-playlist".into(),
                name: "Completed".into(),
                sort_text: "completed".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    scan_catalog
        .write_source_playlist_page(
            "completed-playlist",
            0,
            &[library::PlaylistEntryObservation {
                object_id: "completed-entry".into(),
                track_id: "track-one".into(),
            }],
            true,
        )
        .await
        .unwrap();
    scan_catalog
        .write_source_playlist_page(
            "playlist-one",
            0,
            &[library::PlaylistEntryObservation {
                object_id: "unfinished-entry".into(),
                track_id: "track-two".into(),
            }],
            false,
        )
        .await
        .unwrap();
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |_, update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    {
        let update = scan_catalog.into_changes();
        (&database)
            .publish_catalog_update("source-one", update)
            .await;
    };
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
        let mut scan_catalog = async {
            if (&database)
                .source_identity_key(&library::SourceId::new("source"))
                .await?
                .is_none()
            {
                (&database)
                    .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
                    .await?;
            }
            database.catalog("source", None).await
        }
        .await
        .unwrap();
        let observed = scan_catalog.start_observation().await.unwrap();
        for &name in names {
            {
                let write = library::CatalogBatch {
                    genres: vec![library::GenreObservation {
                        metadata: library::TagMetadata {
                            name: name.to_owned(),
                            sort_text: Some(name.to_owned()),
                            ..Default::default()
                        },
                        object_id: name.to_owned(),
                        ..Default::default()
                    }],
                    ..Default::default()
                };

                scan_catalog.apply_catalog(&write, true).await.unwrap();
            };
        }
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |_, update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        {
            if authoritative {
                scan_catalog
                    .complete_source_observation(
                        observed,
                        &[
                            "track", "album", "artist", "genre", "mood", "folder", "playlist",
                        ],
                        None,
                    )
                    .await
                    .unwrap();
                scan_catalog
                    .complete_source_observation(observed, &[], None)
                    .await
                    .unwrap();
            }
            let update = scan_catalog.into_changes();
            (&database).publish_catalog_update("source", update).await;
        };
        let outcome = updates
            .try_recv()
            .map(|update| {
                assert!(update.changed() || !update.track_rekeys.is_empty());
                update
            })
            .unwrap_or_default();
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

    let mut point_catalog = database.catalog("source", None).await.unwrap();
    {
        let write = library::CatalogBatch {
            genres: vec![library::GenreObservation {
                metadata: library::TagMetadata {
                    name: "Live name".into(),
                    sort_text: Some("live name".into()),
                    ..Default::default()
                },
                object_id: "a".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        point_catalog.apply_catalog(&write, true).await.unwrap();
    };
    {
        let update = point_catalog.into_changes();
        (&database).publish_catalog_update("source", update).await;
    };

    let mut complete_catalog = async {
        if (&database)
            .source_identity_key(&library::SourceId::new("source"))
            .await?
            .is_none()
        {
            (&database)
                .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
                .await?;
        }
        database.catalog("source", None).await
    }
    .await
    .unwrap();
    let complete = complete_catalog.start_observation().await.unwrap();
    {
        let write = library::CatalogBatch {
            genres: vec![library::GenreObservation {
                metadata: library::TagMetadata {
                    name: "a".into(),
                    sort_text: Some("a".into()),
                    ..Default::default()
                },
                object_id: "a".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        complete_catalog.apply_catalog(&write, true).await.unwrap();
    };
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |_, update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    {
        let update = {
            complete_catalog
                .complete_source_observation(
                    complete,
                    &[
                        "track", "album", "artist", "genre", "mood", "folder", "playlist",
                    ],
                    None,
                )
                .await
                .unwrap();

            complete_catalog
                .complete_source_observation(complete, &[], None)
                .await
                .unwrap();

            complete_catalog.into_changes()
        };
        (&database).publish_catalog_update("source", update).await;
    };
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
        let mut scan_catalog = async {
            if (&database)
                .source_identity_key(&library::SourceId::new("local"))
                .await?
                .is_none()
            {
                (&database)
                    .reconcile_source(&library::SourceId::new("local"), "local", "Local")
                    .await?;
            }
            database.catalog("local", None).await
        }
        .await
        .unwrap();
        let observed = scan_catalog.start_observation().await.unwrap();
        {
            let write = library::CatalogBatch {
                genres: vec![library::GenreObservation {
                    metadata: library::TagMetadata {
                        name: "Genre".into(),
                        sort_text: Some("genre".into()),
                        ..Default::default()
                    },
                    object_id: "genre".into(),
                    artwork_binding: artwork.map(<[u8]>::to_vec),
                }],
                ..Default::default()
            };

            scan_catalog.apply_catalog(&write, true).await.unwrap();
        };
        scan_catalog
            .write_file_observations(&files[..if index == 0 { 2 } else { 1 }], Vec::new(), true)
            .await
            .unwrap();
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |_, update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        {
            if index == 0 {
                scan_catalog
                    .complete_source_observation(
                        observed,
                        &[
                            "track", "album", "artist", "genre", "mood", "folder", "playlist",
                        ],
                        None,
                    )
                    .await
                    .unwrap();
                scan_catalog
                    .complete_source_observation(observed, &[], None)
                    .await
                    .unwrap();
            }
            let update = scan_catalog.into_changes();
            (&database).publish_catalog_update("local", update).await;
        };
        let outcome = updates
            .try_recv()
            .map(|update| {
                assert!(update.changed() || !update.track_rekeys.is_empty());
                update
            })
            .unwrap_or_default();
        assert!(match index {
            0 => outcome.metadata,
            1 => !outcome.changed(),
            _ => outcome.artwork,
        });
        let mut reader = connection(&path).await;
        assert_eq!(
            sqlx::query_scalar::<_, Option<Vec<u8>>>(
                "SELECT native_artwork FROM genres WHERE object_id='genre'"
            )
            .fetch_one(&mut reader)
            .await
            .unwrap(),
            match index {
                0 | 1 => Some(b"first".to_vec()),
                2 => None,
                _ => Some(b"second".to_vec()),
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
    let publication = write_small_catalog(&database, "fresh", "Cached Track", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/47f24da63989b3586adaa7682d88f1bb","image":{"kind":"native","source_id":"source","item_id":"art","tag":null},"blur_hash":null}]"#).await;
    assert!(publication.1.metadata);
    drop(database);

    let reopened = Database::open(&path).await.expect("reopen cached Store");
    let cached = reopened
        .cached_source("source-one", &ReadCancellation::new())
        .await
        .expect("resolve configured source from SQLite")
        .expect("cached source");

    assert_eq!(cached.source, publication.0);
    assert!(cached.catalog_complete);
    assert_eq!(cached.display_name, "Source One");
    assert_eq!(
        reopened.all_source_counts().await.unwrap()[&library::SourceId::new("source-one")],
        (1, 2)
    );
}

#[tokio::test]
async fn completed_batches_survive_later_failure_and_interleaved_changes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.unwrap();
    database
        .reconcile_source(&library::SourceId::new("source-two"), "subsonic", "Two")
        .await
        .unwrap();
    database
        .reconcile_source(&library::SourceId::new("source-three"), "subsonic", "Three")
        .await
        .unwrap();

    let mut failed_catalog = database.catalog("source-two", None).await.unwrap();
    failed_catalog.start_observation().await.unwrap();
    let mut write = library::CatalogBatch::default();
    {
        write.genres.push(library::GenreObservation {
            metadata: library::TagMetadata {
                name: "Kept".into(),
                ..Default::default()
            },
            object_id: "kept".into(),
            ..Default::default()
        });
        write.genres.len()
    };
    {
        write.tracks.push(library::TrackObservation {
            user_data: library::NativeUserState {
                favorite: Some(false),
                play_count: Some(1),
                ..Default::default()
            },
            metadata: library::TrackMetadata {
                title: "Known".into(),
                ..Default::default()
            },
            object_id: "known".into(),
            ..Default::default()
        });
        write.tracks.len()
    };
    failed_catalog.apply_catalog(&write, true).await.unwrap();
    let mut write = library::CatalogBatch::default();
    {
        write.genres.push(library::GenreObservation {
            metadata: library::TagMetadata {
                name: "Rolled back".into(),
                ..Default::default()
            },
            object_id: "rolled-back".into(),
            ..Default::default()
        });
        write.genres.len()
    };
    {
        for (kind, id, state) in &[(
            "track",
            "known".to_string(),
            library::NativeUserState {
                favorite: Some(true),
                play_count: Some(9),
                ..Default::default()
            },
        )] {
            failed_catalog
                .write_native_user_data(kind, &[(id.clone(), state.clone())])
                .await
                .unwrap();
        }
    };
    drop(write);
    let mut outside = connection(&path).await;
    assert_eq!(
        sqlx::query_as::<_, (bool, i64)>(
            "SELECT COALESCE((SELECT favorite FROM native_media_state WHERE media_uri=tracks.media_uri),0),COALESCE((SELECT play_count FROM native_media_state WHERE media_uri=tracks.media_uri),0) FROM tracks WHERE object_id='known'"
        )
        .fetch_one(&mut outside)
        .await
        .unwrap(),
        (true, 9)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM genres WHERE object_id='rolled-back'")
            .fetch_one(&mut outside)
            .await
            .unwrap(),
        0
    );
    let favorite = library::FavoriteTarget::Track(library::source_entity_uri(
        &library::SourceId::new("source-two"),
        "track",
        "known",
    ));
    database.set_favorite(&favorite, true).await.unwrap();

    {
        for (kind, id, state) in &[(
            "track",
            "known".to_string(),
            library::NativeUserState {
                favorite: Some(true),
                play_count: Some(2),
                ..Default::default()
            },
        )] {
            failed_catalog
                .write_native_user_data(kind, &[(id.clone(), state.clone())])
                .await
                .unwrap();
        }
    };

    let update = failed_catalog.into_changes();
    assert!(update.metadata);
    assert!(database.favorite(&favorite).await.unwrap());
    assert!(
        !database
            .cached_source("source-two", &ReadCancellation::new())
            .await
            .unwrap()
            .unwrap()
            .catalog_complete
    );
    assert_eq!(
        sqlx::query_as::<_, (bool, i64)>(
            "SELECT COALESCE((SELECT favorite FROM native_media_state WHERE media_uri=tracks.media_uri),0),COALESCE((SELECT play_count FROM native_media_state WHERE media_uri=tracks.media_uri),0) FROM tracks WHERE object_id='known'"
        )
        .fetch_one(&mut outside)
        .await
        .unwrap(),
        (true, 2)
    );

    let mut old_catalog = database.catalog("source-two", None).await.unwrap();
    old_catalog.start_observation().await.unwrap();

    let mut third_catalog = database.catalog("source-three", None).await.unwrap();
    let third = third_catalog.start_observation().await.unwrap();
    let tracks = (0..130)
        .map(|index| library::TrackObservation {
            metadata: library::TrackMetadata {
                title: format!("Track {index}"),
                ..Default::default()
            },
            object_id: format!("track-{index}"),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let mut write = library::CatalogBatch::default();
    assert_eq!(
        {
            write.tracks.extend_from_slice(&tracks);
            write.tracks.len()
        },
        130
    );
    third_catalog.apply_catalog(&write, true).await.unwrap();
    {
        third_catalog
            .complete_source_observation(
                third,
                &[
                    "track", "album", "artist", "genre", "mood", "folder", "playlist",
                ],
                None,
            )
            .await
            .unwrap();

        third_catalog
            .complete_source_observation(third, &[], None)
            .await
            .unwrap();

        third_catalog.into_changes()
    };
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM tracks WHERE source_key=(SELECT source_key FROM sources WHERE object_id='source-three')").fetch_one(&mut outside).await.unwrap(),130);
    let mut point = library::CatalogBatch::default();
    {
        point.tracks.extend_from_slice(&[library::TrackObservation {
            metadata: library::TrackMetadata {
                title: "Saved edit".into(),
                ..Default::default()
            },
            object_id: "known".into(),
            ..Default::default()
        }]);
        point.tracks.len()
    };
    {
        let result = database
            .catalog("source-two", None)
            .await
            .unwrap()
            .apply_catalog(&point, true)
            .await
            .unwrap();
        result
    };
    let mut stale = library::CatalogBatch::default();
    {
        stale.tracks.extend_from_slice(&[library::TrackObservation {
            metadata: library::TrackMetadata {
                title: "Old fetched page".into(),
                ..Default::default()
            },
            object_id: "known".into(),
            ..Default::default()
        }]);
        stale.tracks.len()
    };
    old_catalog.apply_catalog(&stale, true).await.unwrap();
    {
        old_catalog.into_changes()
    };
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT title FROM tracks WHERE object_id='known'")
            .fetch_one(&mut outside)
            .await
            .unwrap(),
        "Saved edit"
    );
    assert!(database.favorite(&favorite).await.unwrap());
    database
        .remove_source(&library::SourceId::new("source-two"))
        .await
        .unwrap();
    assert!(database.catalog("source-two", None).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sources WHERE object_id='source-two'")
            .fetch_one(&mut outside)
            .await
            .unwrap(),
        0
    );

    database
        .reconcile_source(&library::SourceId::new("file-restart"), "local", "Files")
        .await
        .unwrap();

    let mut interrupted_catalog = database.catalog("file-restart", None).await.unwrap();
    interrupted_catalog.start_observation().await.unwrap();
    let write = library::CatalogBatch {
        tracks: vec![library::TrackObservation {
            metadata: library::TrackMetadata {
                title: "Track".into(),
                album_tags: Some(library::AlbumTags {
                    sort_text: "albüm".into(),
                    is_compilation: Some(true),
                    release_types: vec!["album".into()],
                }),
                album: "ALBÜM".into(),
                artist: "Artist".into(),
                album_display_artist: Some("Artist".into()),
                year: Some(2024),
                release_date: Some("2024-05-06".into()),
                musicbrainz_album_id: Some("release".into()),
                album_artists: vec![library::ArtistCredit {
                    object_id: "file-artist".into(),
                    name: "Artist".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            object_id: "file-track".into(),
            album_object_id: Some("file-album".into()),
            source_path: Some(
                directory
                    .path()
                    .join("track.wav")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..Default::default()
        }],
        ..Default::default()
    };
    interrupted_catalog
        .apply_catalog(&write, true)
        .await
        .unwrap();
    drop(outside);
    database.close().await.unwrap();
    let reopened = Database::open(&path).await.unwrap();
    let album = match library::CatalogQuery::uris(
        library::CatalogKind::Album,
        vec![library::source_entity_uri(
            &library::SourceId::new("file-restart"),
            "album",
            "file-album",
        )],
    )
    .rows(&reopened, 0, 1, &ReadCancellation::new())
    .await
    .unwrap()
    {
        library::CatalogRows::Albums(mut rows) => rows.pop().unwrap(),
        _ => unreachable!(),
    };
    assert_eq!(
        (
            album.metadata.title.as_str(),
            album.metadata.sort_text.as_str(),
            album.metadata.year,
            album.metadata.release_date.as_deref(),
            album.metadata.is_compilation
        ),
        ("ALBÜM", "albüm", Some(2024), Some("2024-05-06"), Some(true))
    );
    assert_eq!(album.links.album_artists[0].name, "Artist");
    assert_eq!(album.metadata.release_types, ["album"]);
    assert_eq!(
        reopened.all_source_counts().await.unwrap()[&library::SourceId::new("file-restart")],
        (1, 1)
    );
    assert!(
        !reopened
            .cached_source("file-restart", &ReadCancellation::new())
            .await
            .unwrap()
            .unwrap()
            .catalog_complete
    );
}

#[tokio::test]
async fn deleting_one_source_playlist_preserves_other_playlist_entries() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    write_small_catalog(&database, "fresh-one", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;

    let mut point_catalog = database.catalog("source-one", None).await.unwrap();

    {
        let write = library::CatalogBatch {
            playlists: vec![library::PlaylistObservation {
                object_id: "playlist-one".into(),
                name: "Playlist One".into(),
                sort_text: "playlist one".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        point_catalog.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch {
            playlists: vec![library::PlaylistObservation {
                object_id: "playlist-two".into(),
                name: "Other".into(),
                sort_text: "other".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        point_catalog.apply_catalog(&write, true).await.unwrap();
    };
    point_catalog
        .write_source_playlist_page(
            "playlist-one",
            0,
            &[library::PlaylistEntryObservation {
                object_id: "entry-one".into(),
                track_id: "track-one".into(),
            }],
            true,
        )
        .await
        .unwrap();

    {
        let update = point_catalog.into_changes();
        (&database)
            .publish_catalog_update("source-one", update)
            .await;
    };

    let mut removal_catalog = database.catalog("source-one", None).await.unwrap();

    removal_catalog
        .remove_source_items("playlist", &["playlist-two".into()])
        .await
        .unwrap();

    {
        let update = removal_catalog.into_changes();
        (&database)
            .publish_catalog_update("source-one", update)
            .await;
    };

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
        write_small_catalog(&database, "fresh-one", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;
    assert!(initial.1.metadata);

    let mut point_catalog = database.catalog("source-one", None).await.unwrap();

    {
        let write = library::CatalogBatch {
            playlists: vec![library::PlaylistObservation {
                object_id: "playlist-one".into(),
                name: "Playlist One".into(),
                sort_text: "playlist one".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        point_catalog.apply_catalog(&write, true).await.unwrap();
    };
    point_catalog
        .write_source_playlist_page(
            "playlist-one",
            0,
            &[library::PlaylistEntryObservation {
                object_id: "entry-track-one".into(),
                track_id: "track-one".into(),
            }],
            false,
        )
        .await
        .unwrap();
    point_catalog
        .write_source_playlist_page(
            "playlist-one",
            1,
            &[library::PlaylistEntryObservation {
                object_id: "entry-track-two".into(),
                track_id: "track-two".into(),
            }],
            true,
        )
        .await
        .unwrap();

    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |_, update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    {
        let update = point_catalog.into_changes();
        (&database)
            .publish_catalog_update("source-one", update)
            .await;
    };
    let replayed = updates
        .try_recv()
        .map(|update| {
            assert!(update.changed() || !update.track_rekeys.is_empty());
            update
        })
        .unwrap_or_default();
    assert!(!replayed.changed());
    assert!(updates.try_recv().is_err());
}

#[tokio::test]
async fn repeated_source_playlist_and_folder_observations_coalesce() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    write_small_catalog(&database, "fresh-one", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;

    let mut scan_catalog = database.catalog("source-one", None).await.unwrap();

    for _ in 0..2 {
        {
            let write = library::CatalogBatch {
                folders: vec![library::FolderObservation {
                    metadata: library::FolderMetadata {
                        name: Some("Music".into()),
                        sort_text: Some("music".into()),
                        ..Default::default()
                    },
                    object_id: "folder-one".into(),
                    ..Default::default()
                }],
                ..Default::default()
            };

            scan_catalog.apply_catalog(&write, true).await.unwrap();
        };
        {
            let write = library::CatalogBatch {
                playlists: vec![library::PlaylistObservation {
                    object_id: "playlist-one".into(),
                    name: "Playlist One".into(),
                    sort_text: "playlist one".into(),
                    ..Default::default()
                }],
                ..Default::default()
            };

            scan_catalog.apply_catalog(&write, true).await.unwrap();
        };
        scan_catalog
            .write_source_playlist_page(
                "playlist-one",
                0,
                &[library::PlaylistEntryObservation {
                    object_id: "entry-one".into(),
                    track_id: "track-one".into(),
                }],
                true,
            )
            .await
            .expect("stage repeated playlist entry");
    }

    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |_, update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    {
        let update = scan_catalog.into_changes();
        (&database)
            .publish_catalog_update("source-one", update)
            .await;
    };
    let update = updates.try_recv().unwrap();
    assert!(update.metadata);
}

#[tokio::test]
async fn duplicate_provider_entries_keep_first_occurrence_and_dense_positions() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    write_small_catalog(&database, "fresh-one", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;

    let mut scan_catalog = database.catalog("source-one", None).await.unwrap();

    {
        let write = library::CatalogBatch {
            playlists: vec![library::PlaylistObservation {
                object_id: "playlist-one".into(),
                name: "Playlist".into(),
                sort_text: "playlist".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    scan_catalog
        .write_source_playlist_page(
            "playlist-one",
            0,
            &[
                library::PlaylistEntryObservation {
                    object_id: "duplicate-a".into(),
                    track_id: "track-one".into(),
                },
                library::PlaylistEntryObservation {
                    object_id: "duplicate-a".into(),
                    track_id: "track-two".into(),
                },
                library::PlaylistEntryObservation {
                    object_id: "duplicate-b".into(),
                    track_id: "track-one".into(),
                },
            ],
            true,
        )
        .await
        .expect("write repeated track occurrences");

    {
        let update = scan_catalog.into_changes();
        (&database)
            .publish_catalog_update("source-one", update)
            .await;
    };

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
            ("duplicate-a".into(), "track-two".into(), 0),
            ("duplicate-b".into(), "track-one".into(), 1)
        ]
    );
}

#[tokio::test]
async fn playlist_point_update_preserves_playlist_scope() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let database = Database::open(&path).await.expect("open Library Store");
    write_small_catalog(&database, "fresh-one", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;

    let mut point_catalog = database.catalog("source-one", None).await.unwrap();

    {
        let write = library::CatalogBatch {
            playlists: vec![library::PlaylistObservation {
                object_id: "playlist-one".into(),
                name: "Playlist One".into(),
                sort_text: "playlist one".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        point_catalog.apply_catalog(&write, true).await.unwrap();
    };
    point_catalog
        .write_source_playlist_page(
            "playlist-one",
            0,
            &[library::PlaylistEntryObservation {
                object_id: "entry-track-two".into(),
                track_id: "track-two".into(),
            }],
            true,
        )
        .await
        .unwrap();

    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |_, update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    {
        let update = point_catalog.into_changes();
        (&database)
            .publish_catalog_update("source-one", update)
            .await;
    };
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
        let mut scan_catalog = database.catalog("source-one", None).await.unwrap();
        {
            let write = library::CatalogBatch {
                artists: vec![library::ArtistObservation {
                    user_data: library::NativeUserState {
                        favorite: favorite,
                        ..Default::default()
                    },
                    metadata: library::ArtistMetadata {
                        name: "Artist".into(),
                        sort_text: Some("artist".into()),
                        ..Default::default()
                    },
                    object_id: "artist-one".into(),
                    ..Default::default()
                }],
                ..Default::default()
            };

            scan_catalog.apply_catalog(&write, true).await.unwrap();
        };
        // A descriptive credit must leave the native artist's user data alone.
        let mut write = library::CatalogBatch::default();
        {
            write.tracks.push(library::TrackObservation {
                metadata: library::TrackMetadata {
                    title: "Credit track".into(),
                    artists: vec![library::ArtistCredit {
                        object_id: "artist-one".into(),
                        name: "Artist".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                object_id: "credit-track".into(),
                ..Default::default()
            });
            write.tracks.len()
        };
        scan_catalog.apply_catalog(&write, true).await.unwrap();
        {
            let update = scan_catalog.into_changes();
            (&database)
                .publish_catalog_update("source-one", update)
                .await;
        };
        let row =
            library::CatalogQuery::uris(library::CatalogKind::Artist, vec![(&uri).to_string()])
                .rows(&database, 0, 1, &cancel)
                .await
                .map(|rows| match rows {
                    library::CatalogRows::Artists(mut rows) => rows.pop(),
                    _ => unreachable!("typed catalog projection"),
                })
                .unwrap()
                .unwrap();
        assert_eq!(row.state.favorite, index < 2);
    }
}

#[tokio::test]
async fn reference_names_can_change_and_complete_artist_names_take_precedence() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite");
    let database = Database::open(&path).await.unwrap();
    database
        .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
        .await
        .unwrap();
    let mut raw = connection(&path).await;
    for (artist, genre, complete) in [
        ("Artist", "Rock", false),
        ("artist", "rock", false),
        ("Renamed", "Alternative", false),
        ("RENAMED", "ALTERNATIVE", true),
    ] {
        let mut write = library::CatalogBatch::default();
        if complete {
            {
                write.artists.push(library::ArtistObservation {
                    metadata: library::ArtistMetadata {
                        name: artist.into(),
                        ..Default::default()
                    },
                    object_id: "artist".into(),
                    ..Default::default()
                });
                write.artists.len()
            };
        }
        {
            write.tracks.push(library::TrackObservation {
                metadata: library::TrackMetadata {
                    title: "Track".into(),
                    artists: vec![library::ArtistCredit {
                        object_id: "artist".into(),
                        name: if complete { "Ignored credit" } else { artist }.into(),
                        ..Default::default()
                    }],
                    genres: vec![library::TagReference {
                        object_id: "genre".into(),
                        name: genre.into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                object_id: "track".into(),
                ..Default::default()
            });
            write.tracks.len()
        };
        {
            let result = database
                .catalog("source", None)
                .await
                .unwrap()
                .apply_catalog(&write, true)
                .await
                .unwrap();
            result
        };
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT name FROM artists")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            artist
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT name FROM genres")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            genre
        );
    }
}
