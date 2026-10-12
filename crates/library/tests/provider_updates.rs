use library::{Database, Freshness, ReadCancellation, Scan, ScanLink};

async fn track(scan: &mut Scan, favorite: Option<bool>, play_count: Option<i64>) {
    scan.write_track(&library::TrackInput {
        object_id: "track".into(),
        album_object_id: Some("album".into()),
        title: "Track".into(),
        display_album: "Album".into(),
        display_artist: "Artist".into(),
        sort_text: "track".into(),
        duration_millis: 180000,
        disc_number: 1,
        track_number: 1,
        year: Some(2025),
        release_date: Some("2025-01-01".into()),
        source_format: Some("FLAC".into()),
        comment: Some("Comment".into()),
        bpm: Some(120),
        musicbrainz_recording_id: Some("recording".into()),
        musicbrainz_release_track_id: Some("release-track".into()),
        favorite,
        rating: Some(8),
        source_play_count: play_count,
        loudness_analysis_key: [1; 32],
        ..Default::default()
    })
    .await
    .unwrap();
}

async fn fixture() -> (tempfile::TempDir, Database) {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(directory.path().join("library.sqlite3"))
        .await
        .unwrap();
    let mut scan = Scan::begin(
        &database,
        "source",
        "Source",
        "source",
        Some(Freshness::new(b"catalog".to_vec()).unwrap()),
    )
    .await
    .unwrap();
    scan.write_album(&library::AlbumInput {
        object_id: "album".into(),
        title: "Album".into(),
        display_artist: "Artist".into(),
        sort_text: "album".into(),
        favorite: Some(true),
        rating: Some(8),
        ..Default::default()
    })
    .await
    .unwrap();
    scan.write_artist(&library::ArtistInput {
        object_id: "artist".into(),
        name: "Artist".into(),
        sort_text: Some("artist".into()),
        favorite: Some(true),
        rating: Some(8),
        ..Default::default()
    })
    .await
    .unwrap();
    track(&mut scan, Some(true), None).await;
    scan.write_folder(&library::FolderInput {
        object_id: "folder".into(),
        name: Some("Folder".into()),
        sort_text: Some("folder".into()),
        ..Default::default()
    })
    .await
    .unwrap();
    scan.write_track_folders(&[ScanLink::new("track", "folder", 0)])
        .await
        .unwrap();
    scan.finish().await.unwrap();
    (directory, database)
}

#[tokio::test]
async fn favorite_and_play_count_changes_do_not_report_new_tracks() {
    let (directory, database) = fixture().await;
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    sqlx::query("INSERT INTO tracks(source_key,object_id,media_uri,indexed,source_favorite,source_play_count)
        SELECT source_key,'unknown',?1,0,0,0 FROM sources WHERE object_id='source'")
        .bind(library::source_entity_uri(&library::SourceId::new("source"), "track", "unknown"))
        .execute(&mut raw).await.unwrap();
    for (favorite, plays, changed) in [(true, 1, true), (true, 1, false), (false, 2, true)] {
        let mut scan = Scan::begin_items(&database, "source").await.unwrap();
        scan.write_source_user_data(&[
            (
                "track",
                "track".into(),
                library::SourceUserData {
                    favorite: Some(favorite),
                    play_count: Some(plays),
                    ..Default::default()
                },
            ),
            (
                "track",
                "unknown".into(),
                library::SourceUserData {
                    favorite: Some(true),
                    play_count: Some(42),
                    ..Default::default()
                },
            ),
        ])
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_as::<_, (bool, i64)>(
                "SELECT source_favorite,source_play_count FROM tracks WHERE object_id='unknown'"
            )
            .fetch_one(&mut raw)
            .await
            .unwrap(),
            (false, 0)
        );
        assert_eq!(
            sqlx::query_as::<_, (bool, Option<i64>, Option<i64>)>(
                "SELECT source_favorite,source_play_count,source_rating FROM tracks WHERE object_id='track'"
            )
            .fetch_one(&mut raw)
            .await
            .unwrap(),
            (favorite, Some(plays), Some(80))
        );
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        scan.finish().await.unwrap();
        let publication = updates.try_recv().unwrap();
        assert_eq!(publication.metadata, changed);
        assert!(!publication.tracks_added);
    }
}

#[tokio::test]
async fn point_metadata_preserves_folders_and_complete_memberships_can_clear_them() {
    let (directory, database) = fixture().await;
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    for replace in [false, true] {
        let mut scan = Scan::begin_items(&database, "source").await.unwrap();
        track(&mut scan, None, None).await;
        if replace {
            scan.replace_track_folders("track", &[]).await.unwrap();
            track(&mut scan, None, None).await;
        }
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        scan.finish().await.unwrap();
        let outcome = updates.try_recv().unwrap();
        assert_eq!(!outcome.changed(), !replace);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_folders")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            i64::from(!replace)
        );
    }
}

#[tokio::test]
async fn favorite_snapshots_replace_only_source_favorites_and_keep_catalog_freshness() {
    let (directory, database) = fixture().await;
    let path = directory.path().join("library.sqlite3");
    let mut raw = super::support::connection(&path).await;
    let uri = library::source_entity_uri(&library::SourceId::new("source"), "track", "track");
    database
        .set_favorite(&library::FavoriteTarget::Track(uri.clone()), true)
        .await
        .unwrap();
    for (favorite, changed) in [(false, true), (false, false), (true, true)] {
        let mut scan = Scan::begin_items(&database, "source").await.unwrap();
        let ids = |id: &str| {
            if favorite {
                vec![id.to_string()]
            } else {
                Vec::new()
            }
        };
        scan.replace_favorites(&ids("track"), &ids("album"), &ids("artist"))
            .await
            .unwrap();
        for table in ["tracks", "albums", "artists"] {
            let sql = format!("SELECT source_favorite,source_rating FROM {table}");
            assert_eq!(
                sqlx::query_as::<_, (bool, Option<i64>)>(sqlx::AssertSqlSafe(sql))
                    .fetch_one(&mut raw)
                    .await
                    .unwrap(),
                (favorite, Some(80))
            );
        }
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        scan.finish().await.unwrap();
        let outcome = updates.try_recv().unwrap();
        assert_eq!(outcome.metadata, changed);
        assert!(
            database
                .favorite(&library::FavoriteTarget::Track(uri.clone()))
                .await
                .unwrap()
        );
        assert!(
            Scan::accept_freshness(
                &database,
                "source",
                &Freshness::new(b"catalog".to_vec()).unwrap(),
                &ReadCancellation::new()
            )
            .await
            .unwrap()
        );
        assert_eq!(
            sqlx::query_as::<_, (String, String, i64)>(
                "SELECT comment,release_date,(SELECT count(*) FROM track_folders) FROM tracks"
            )
            .fetch_one(&mut raw)
            .await
            .unwrap(),
            ("Comment".into(), "2025-01-01".into(), 1)
        );
    }
}

#[tokio::test]
async fn home_sections_refresh_and_clear_without_replacing_their_entities() {
    let (directory, database) = fixture().await;
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    for (in_batch, populated, title, changed) in [
        (false, true, Some("Played"), true),
        (true, true, Some("Played"), false),
        (true, true, Some("Popular"), true),
        (false, false, None, true),
    ] {
        let mut scan = Scan::begin_items(&database, "source").await.unwrap();
        let entries = if populated {
            vec![library::HomeEntryInput {
                position: 0,
                kind: library::HomeEntryKind::Album,
                entity_object_id: "album".into(),
                title: "Album".into(),
                subtitle: "Artist".into(),
            }]
        } else {
            vec![]
        };
        let previous = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT title,section_title FROM home_entries",
        )
        .fetch_all(&mut raw)
        .await
        .unwrap();
        if in_batch {
            scan.begin_batch().await.unwrap();
        }
        scan.replace_home_section("most-played", title, &entries)
            .await
            .unwrap();
        if in_batch {
            assert_eq!(
                sqlx::query_as::<_, (String, Option<String>)>(
                    "SELECT title,section_title FROM home_entries",
                )
                .fetch_all(&mut raw)
                .await
                .unwrap(),
                previous,
            );
            scan.finish_batch().await.unwrap();
        }
        assert_eq!(
            sqlx::query_as::<_, (String, Option<String>)>(
                "SELECT title,section_title FROM home_entries",
            )
            .fetch_all(&mut raw)
            .await
            .unwrap(),
            if populated {
                vec![("Album".into(), title.map(str::to_string))]
            } else {
                vec![]
            },
        );
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        scan.finish().await.unwrap();
        let update = updates.try_recv().unwrap();
        assert_eq!(update.home, changed);
        assert!(!update.metadata && !update.playlists && !update.artwork && !update.inventory);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM home_entries")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            i64::from(populated)
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT title FROM albums")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            "Album"
        );
        assert!(
            Scan::accept_freshness(
                &database,
                "source",
                &Freshness::new(b"catalog".to_vec()).unwrap(),
                &ReadCancellation::new()
            )
            .await
            .unwrap()
        );
    }
}

#[tokio::test]
async fn incomplete_catalog_keeps_home_sections_when_their_refresh_fails() {
    let (directory, database) = fixture().await;
    let mut scan = Scan::begin_items(&database, "source").await.unwrap();
    scan.replace_home_section(
        "most-played",
        None,
        &[library::HomeEntryInput {
            position: 0,
            kind: library::HomeEntryKind::Album,
            entity_object_id: "album".into(),
            title: "Album".into(),
            subtitle: "Artist".into(),
        }],
    )
    .await
    .unwrap();
    scan.finish().await.unwrap();
    let mut scan = Scan::begin(&database, "source", "Source", "source", None)
        .await
        .unwrap();
    scan.write_album(&library::AlbumInput {
        object_id: "album".into(),
        title: "Album".into(),
        display_artist: "Artist".into(),
        sort_text: "album".into(),
        favorite: Some(true),
        rating: Some(8),
        ..Default::default()
    })
    .await
    .unwrap();
    scan.replace_home_section(
        "newly-added",
        None,
        &[library::HomeEntryInput {
            position: 0,
            kind: library::HomeEntryKind::Album,
            entity_object_id: "album".into(),
            title: "New Album".into(),
            subtitle: "Artist".into(),
        }],
    )
    .await
    .unwrap();
    scan.incomplete();
    scan.retain_home_section("most-played").await.unwrap();
    scan.finish().await.unwrap();
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    assert_eq!(
        sqlx::query_as::<_, (String, String)>(
            "SELECT section_id,title FROM home_entries ORDER BY section_id"
        )
        .fetch_all(&mut raw)
        .await
        .unwrap(),
        vec![
            ("most-played".into(), "Album".into()),
            ("newly-added".into(), "New Album".into())
        ]
    );
}

#[tokio::test]
async fn growing_and_reordering_a_playlist_preserves_occurrence_keys() {
    let (directory, database) = fixture().await;
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    let mut original = std::collections::HashMap::new();
    for ids in [
        &["a", "b"][..],
        &["x", "y", "z", "a", "b"][..],
        &["b", "a"][..],
        &[][..],
    ] {
        let mut scan = Scan::begin_items(&database, "source").await.unwrap();
        scan.write_playlist(&library::PlaylistInput {
            object_id: "playlist".into(),
            name: "Playlist".into(),
            sort_text: "playlist".into(),
            ..Default::default()
        })
        .await
        .unwrap();
        let entries = ids
            .iter()
            .map(|id| library::PlaylistEntryInput {
                object_id: (*id).into(),
                track_id: "track".into(),
            })
            .collect::<Vec<_>>();
        scan.write_playlist_page("playlist", 0, &entries, true)
            .await
            .unwrap();
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        scan.finish().await.unwrap();
        let update = updates.try_recv().unwrap();
        assert!(update.playlists && !update.metadata && !update.home);
        let rows = sqlx::query_as::<_, (String, i64)>(
            "SELECT object_id,playlist_entry_key FROM playlist_entries ORDER BY position",
        )
        .fetch_all(&mut raw)
        .await
        .unwrap();
        assert_eq!(
            rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ids
        );
        for (id, key) in rows {
            assert_eq!(*original.entry(id).or_insert(key), key);
        }
    }
}

#[tokio::test]
async fn unchanged_point_reads_do_not_rewrite_tracks_or_albums() {
    let (directory, database) = fixture().await;
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    sqlx::raw_sql("CREATE TRIGGER no_track_rewrite BEFORE UPDATE ON tracks BEGIN SELECT RAISE(FAIL,'unchanged track rewritten'); END;
        CREATE TRIGGER no_album_rewrite BEFORE UPDATE ON albums BEGIN SELECT RAISE(FAIL,'unchanged album rewritten'); END;")
        .execute(&mut raw).await.unwrap();
    let mut scan = Scan::begin_items(&database, "source").await.unwrap();
    track(&mut scan, None, None).await;
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    database.observe_catalog(move |update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    scan.finish().await.unwrap();
    let update = updates.try_recv().unwrap();
    assert!(!update.changed());
}

#[tokio::test]
async fn first_activity_observation_publishes_without_invalidating_catalog_freshness() {
    let (_directory, database) = fixture().await;
    for changed in [true, false] {
        let mut scan = Scan::begin_items(&database, "source").await.unwrap();
        track(&mut scan, None, Some(5)).await;
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        scan.finish().await.unwrap();
        let update = updates.try_recv().unwrap();
        assert_eq!(update.metadata, changed);
        let cancellation = ReadCancellation::new();
        let uri = library::source_entity_uri(&library::SourceId::new("source"), "track", "track");
        assert_eq!(
            database
                .track_row_by_uri(&uri, &cancellation)
                .await
                .unwrap()
                .unwrap()
                .play_count,
            5
        );
        assert!(
            Scan::accept_freshness(
                &database,
                "source",
                &Freshness::new(b"catalog".to_vec()).unwrap(),
                &cancellation
            )
            .await
            .unwrap()
        );
    }
}

#[tokio::test]
async fn absent_favorites_preserve_known_state_and_explicit_false_clears_it() {
    let (directory, database) = fixture().await;
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    for (favorite, expected) in [(None, true), (Some(false), false), (None, false)] {
        let mut scan = Scan::begin_items(&database, "source").await.unwrap();
        track(&mut scan, favorite, None).await;
        scan.finish().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, bool>("SELECT source_favorite FROM tracks")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            expected
        );
    }
}
