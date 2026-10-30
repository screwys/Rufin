use library::{Catalog, Freshness, ReadCancellation};

async fn track(
    observation: &mut library::Observation<'_>,
    favorite: Option<bool>,
    play_count: Option<i64>,
) {
    observation
        .apply_catalog(
            &library::CatalogBatch {
                tracks: vec![library::TrackObservation {
                    user_data: library::NativeUserState {
                        favorite: favorite,
                        rating: (Some(Some(8))).map(|rating| rating.map(|value| value * 10)),
                        play_count: play_count,
                        ..Default::default()
                    },
                    metadata: library::TrackMetadata {
                        title: "Track".into(),
                        album: "Album".into(),
                        artist: "Artist".into(),
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
                        ..Default::default()
                    },
                    object_id: "track".into(),
                    album_object_id: Some("album".into()),
                    audio_revision: Some([1; 32]),
                    ..Default::default()
                }],
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
}

async fn fixture() -> (tempfile::TempDir, Catalog) {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("library.sqlite3"))
        .await
        .unwrap();

    let mut scan_observation = async {
        if {
            (&catalog).loaded().await;
            (&catalog).source_key(&library::SourceId::new("source"))
        }
        .is_none()
        {
            (&catalog)
                .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
                .await?;
        }
        catalog.observe("source").await
    }
    .await
    .unwrap();
    scan_observation.start_observation().await.unwrap();
    {
        let write = library::CatalogBatch {
            albums: vec![library::AlbumObservation {
                user_data: library::NativeUserState {
                    favorite: Some(true),
                    rating: (Some(Some(8))).map(|rating| rating.map(|value| value * 10)),
                    ..Default::default()
                },
                metadata: library::AlbumMetadata {
                    title: "Album".into(),
                    artist: "Artist".into(),
                    sort_text: "album".into(),
                    ..Default::default()
                },
                object_id: "album".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch {
            artists: vec![library::ArtistObservation {
                user_data: library::NativeUserState {
                    favorite: Some(true),
                    rating: (Some(Some(8))).map(|rating| rating.map(|value| value * 10)),
                    ..Default::default()
                },
                metadata: library::ArtistMetadata {
                    name: "Artist".into(),
                    sort_text: Some("artist".into()),
                    ..Default::default()
                },
                object_id: "artist".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    track(&mut scan_observation, Some(true), None).await;
    {
        let write = library::CatalogBatch {
            folders: vec![library::FolderObservation {
                metadata: library::FolderMetadata {
                    name: Some("Folder".into()),
                    sort_text: Some("folder".into()),
                    ..Default::default()
                },
                object_id: "folder".into(),
                tracks: vec!["track".into()],
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    scan_observation
        .complete_source_observation(
            &[
                "track", "album", "artist", "genre", "mood", "folder", "playlist",
            ],
            None,
            None,
        )
        .await
        .unwrap();
    scan_observation
        .complete_source_observation(
            &[],
            Some(Freshness::new(b"catalog".to_vec()).unwrap()).as_ref(),
            None,
        )
        .await
        .unwrap();
    drop(scan_observation);
    (directory, catalog)
}

#[tokio::test]
async fn favorite_and_play_count_changes_do_not_report_new_tracks() {
    let (directory, catalog) = fixture().await;
    let path = directory.path().join("library.sqlite3");
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    sqlx::query("INSERT INTO tracks(source_key,object_id,media_uri,indexed) SELECT source_key,'unknown',?1,0 FROM sources WHERE object_id='source'")
        .bind(library::source_entity_uri(&library::SourceId::new("source"), "track", "unknown"))
        .execute(&mut raw).await.unwrap();
    let catalog = super::support::reopen(catalog, &path).await;
    let mut changes = super::support::Changes::of(&catalog);
    // Native favorites are state and native play counts activity; neither changes catalog rows.
    for (favorite, plays, changed, played) in [
        (true, 1, true, true),
        (true, 1, false, false),
        (true, 2, false, true),
        (false, 2, true, false),
    ] {
        changes.facts();
        let mut scan_observation = catalog.observe("source").await.unwrap();
        {
            for (kind, id, state) in &[
                (
                    "track",
                    "track".to_string(),
                    library::NativeUserState {
                        favorite: Some(favorite),
                        play_count: Some(plays),
                        ..Default::default()
                    },
                ),
                (
                    "track",
                    "unknown".to_string(),
                    library::NativeUserState {
                        favorite: Some(true),
                        play_count: Some(42),
                        ..Default::default()
                    },
                ),
            ] {
                scan_observation
                    .write_native_user_data(
                        kind,
                        &[
                            (
                                id.clone(),
                                library::NativeUserState {
                                    favorite: state.favorite,
                                    ..Default::default()
                                },
                            ),
                            (
                                id.clone(),
                                library::NativeUserState {
                                    favorite: None,
                                    ..state.clone()
                                },
                            ),
                        ],
                    )
                    .await
                    .unwrap();
            }
        };
        assert_eq!(
            sqlx::query_as::<_, (bool, i64)>(
                "SELECT coalesce(native_favorite,0),coalesce(native_play_count,0) FROM tracks WHERE object_id='unknown'"
            )
            .fetch_one(&mut raw)
            .await
            .unwrap(),
            (true, 42)
        );
        assert_eq!(
            sqlx::query_as::<_, (bool, Option<i64>, Option<i64>)>(
                "SELECT coalesce(native_favorite,0),coalesce(native_play_count,0),native_rating FROM tracks WHERE object_id='track'"
            )
            .fetch_one(&mut raw)
            .await
            .unwrap(),
            (favorite, Some(plays), Some(80))
        );
        let publication = changes.facts();
        assert_eq!(
            publication.intersects(super::support::Shown::STATE),
            changed
        );
        assert_eq!(publication.contains(super::support::Shown::STATS), played);
    }
}

#[tokio::test]
async fn point_metadata_preserves_folders_and_complete_memberships_can_clear_them() {
    let (directory, catalog) = fixture().await;
    let mut changes = super::support::Changes::of(&catalog);
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    for replace in [false, true] {
        changes.facts();
        let mut scan_observation = catalog.observe("source").await.unwrap();
        track(&mut scan_observation, None, None).await;
        if replace {
            {
                let write = library::CatalogBatch {
                    tracks: vec![library::TrackObservation {
                        object_id: "track".into(),
                        folders: Some(Vec::new()),
                        ..Default::default()
                    }],
                    ..Default::default()
                };

                scan_observation.apply_catalog(&write, true).await.unwrap();
            };
            track(&mut scan_observation, None, None).await;
        }
        let outcome = changes.facts();
        assert_eq!(!super::support::shown(outcome), !replace);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_links WHERE role=4")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            i64::from(!replace)
        );
    }
}

#[tokio::test]
async fn favorite_snapshots_replace_only_source_favorites_and_keep_catalog_freshness() {
    let (directory, catalog) = fixture().await;
    let mut changes = super::support::Changes::of(&catalog);
    let path = directory.path().join("library.sqlite3");
    let mut raw = super::support::connection(&path).await;
    let uri = library::source_entity_uri(&library::SourceId::new("source"), "track", "track");
    catalog
        .set_favorite(&library::FavoriteTarget::Track(uri.clone()), true, false)
        .await
        .unwrap();
    for (favorite, changed) in [(false, true), (false, false), (true, true)] {
        changes.facts();
        let mut scan_observation = catalog.observe("source").await.unwrap();
        let ids = |id: &str| {
            if favorite {
                vec![id.to_string()]
            } else {
                Vec::new()
            }
        };
        for kind in ["track", "album", "artist"] {
            scan_observation
                .replace_source_favorites(kind, &ids(kind))
                .await
                .unwrap();
        }
        for table in ["tracks", "albums", "artists"] {
            let sql = format!("SELECT native_favorite,native_rating FROM {table}");
            assert_eq!(
                sqlx::query_as::<_, (bool, Option<i64>)>(sqlx::AssertSqlSafe(sql))
                    .fetch_one(&mut raw)
                    .await
                    .unwrap(),
                (favorite, Some(80))
            );
        }
        let outcome = changes.facts();
        assert!(!outcome.intersects(super::support::Shown::CATALOG));
        assert_eq!(outcome.intersects(super::support::Shown::STATE), changed);
        assert!(
            catalog
                .favorite(&library::FavoriteTarget::Track(uri.clone()))
                .await
                .unwrap()
        );
        assert!(
            (&catalog)
                .accept_freshness(
                    "source",
                    &Freshness::new(b"catalog".to_vec()).unwrap(),
                    &ReadCancellation::new()
                )
                .await
                .unwrap()
        );
        assert_eq!(
            sqlx::query_as::<_, (String, String, i64)>(
                "SELECT comment,release_date,(SELECT count(*) FROM track_links WHERE role=4) FROM tracks"
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
    let (directory, catalog) = fixture().await;
    let mut changes = super::support::Changes::of(&catalog);
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    for (populated, title, changed) in [
        (true, Some("Played"), true),
        (true, Some("Played"), false),
        (true, Some("Popular"), true),
        (false, None, true),
    ] {
        changes.facts();
        let mut scan_observation = catalog.observe("source").await.unwrap();
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

        scan_observation
            .replace_source_home_section("most-played", title, &entries)
            .await
            .unwrap();

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
        let update = changes.facts();
        assert_eq!(update.contains(super::support::Shown::HOME), changed);
        assert!(
            !update.intersects(super::support::Shown::CATALOG)
                && !update.contains(super::support::Shown::PLAYLIST)
        );
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
            (&catalog)
                .accept_freshness(
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
    let (directory, catalog) = fixture().await;

    let mut scan_observation = catalog.observe("source").await.unwrap();
    scan_observation
        .replace_source_home_section(
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

    let mut scan_observation = async {
        if {
            (&catalog).loaded().await;
            (&catalog).source_key(&library::SourceId::new("source"))
        }
        .is_none()
        {
            (&catalog)
                .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
                .await?;
        }
        catalog.observe("source").await
    }
    .await
    .unwrap();
    scan_observation.start_observation().await.unwrap();
    {
        let write = library::CatalogBatch {
            albums: vec![library::AlbumObservation {
                user_data: library::NativeUserState {
                    favorite: Some(true),
                    rating: (Some(Some(8))).map(|rating| rating.map(|value| value * 10)),
                    ..Default::default()
                },
                metadata: library::AlbumMetadata {
                    title: "Album".into(),
                    artist: "Artist".into(),
                    sort_text: "album".into(),
                    ..Default::default()
                },
                object_id: "album".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    scan_observation
        .replace_source_home_section(
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
    let (directory, catalog) = fixture().await;
    let mut changes = super::support::Changes::of(&catalog);
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    let mut original = std::collections::HashMap::new();
    for ids in [
        &["a", "b"][..],
        &["x", "y", "z", "a", "b"][..],
        &["b", "a"][..],
        &[][..],
    ] {
        changes.facts();
        let mut scan_observation = catalog.observe("source").await.unwrap();
        {
            let write = library::CatalogBatch {
                playlists: vec![library::PlaylistObservation {
                    object_id: "playlist".into(),
                    name: "Playlist".into(),
                    sort_text: "playlist".into(),
                    ..Default::default()
                }],
                ..Default::default()
            };

            scan_observation.apply_catalog(&write, true).await.unwrap();
        };
        let entries = ids
            .iter()
            .map(|id| library::PlaylistEntryObservation {
                object_id: (*id).into(),
                track_id: "track".into(),
            })
            .collect::<Vec<_>>();
        scan_observation
            .write_source_playlist_page("playlist", 0, &entries, true)
            .await
            .unwrap();
        let update = changes.facts();
        assert!(
            update.contains(super::support::Shown::PLAYLIST)
                && !update.intersects(super::support::Shown::CATALOG)
                && !update.contains(super::support::Shown::HOME)
        );
        let rows = sqlx::query_as::<_, (String, i64)>(
            "SELECT object_id,playlist_entry_key FROM playlist_entries ORDER BY rank,playlist_entry_key",
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
    let (directory, catalog) = fixture().await;
    let mut changes = super::support::Changes::of(&catalog);
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    sqlx::raw_sql("CREATE TRIGGER no_track_rewrite BEFORE UPDATE OF metadata,native_artwork,source_path,cue_path,cue_start_millis,cue_end_millis ON tracks BEGIN SELECT RAISE(FAIL,'unchanged track rewritten'); END;
        CREATE TRIGGER no_album_rewrite BEFORE UPDATE OF metadata,native_artwork ON albums BEGIN SELECT RAISE(FAIL,'unchanged album rewritten'); END;")
        .execute(&mut raw).await.unwrap();

    changes.facts();
    let mut scan_observation = catalog.observe("source").await.unwrap();
    track(&mut scan_observation, None, None).await;
    let update = changes.facts();
    assert!(!super::support::shown(update));
}

#[tokio::test]
async fn first_activity_observation_publishes_without_invalidating_catalog_freshness() {
    let (_directory, catalog) = fixture().await;
    let mut changes = super::support::Changes::of(&catalog);
    for changed in [true, false] {
        changes.facts();
        let mut scan_observation = catalog.observe("source").await.unwrap();
        track(&mut scan_observation, None, Some(5)).await;
        let update = changes.facts();
        // Server play counts are activity, not catalog rows.
        assert!(!update.intersects(super::support::Shown::CATALOG));
        assert_eq!(update.contains(super::support::Shown::STATS), changed);
        let cancellation = ReadCancellation::new();
        let uri = library::source_entity_uri(&library::SourceId::new("source"), "track", "track");
        assert_eq!(
            catalog
                .tracks_by_uris(&([(&uri).to_string()]), &cancellation)
                .await
                .map(|rows| {
                    let mut rows = rows;
                    rows.pop()
                })
                .unwrap()
                .unwrap()
                .state
                .play_count,
            5
        );
        assert!(
            (&catalog)
                .accept_freshness(
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
    let (directory, catalog) = fixture().await;
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    for (favorite, expected) in [(None, true), (Some(false), false), (None, false)] {
        let mut scan_observation = catalog.observe("source").await.unwrap();
        track(&mut scan_observation, favorite, None).await;
        assert_eq!(
            sqlx::query_scalar::<_, bool>("SELECT coalesce(native_favorite,0) FROM tracks")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            expected
        );
    }
}
