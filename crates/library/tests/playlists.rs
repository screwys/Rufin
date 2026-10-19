use library::{CatalogSort, FavoriteTarget, ReadCancellation};

use super::support::{connection, fixture};

#[tokio::test]
async fn playlist_pin_import_reads_only_the_owning_sources_identities() {
    let fixture = fixture().await;
    let database = &fixture.database;
    let (_, authored) = database
        .create_playlist(
            Some(fixture.source),
            "Z saved",
            &library::CatalogQuery::uris(library::CatalogKind::Track, (&[]).to_vec()).selection(),
        )
        .await
        .unwrap()
        .unwrap();
    database
        .create_playlist(
            None,
            "Global",
            &library::CatalogQuery::uris(library::CatalogKind::Track, (&[]).to_vec()).selection(),
        )
        .await
        .unwrap();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO playlists(source_key,owner,position,object_id,name,normalized_name,sort_text) VALUES(?1,'source',(SELECT COALESCE(max(position)+1,0) FROM playlists),'native','A native','a native','a native')")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    let other: library::SourceKey = sqlx::query_scalar("INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('emby','Emby','emby',zeroblob(32)) RETURNING source_key")
        .fetch_one(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO playlists(source_key,owner,position,object_id,name,normalized_name,sort_text) VALUES(?1,'source',(SELECT COALESCE(max(position)+1,0) FROM playlists),'other','Other','other','other')")
        .bind(other).execute(&mut raw).await.unwrap();
    drop(raw);
    let cancel = ReadCancellation::new();
    assert_eq!(
        database
            .source_playlist_object_ids(fixture.source, &cancel)
            .await
            .unwrap(),
        ["native".to_string(), authored]
    );
    assert_eq!(
        database
            .source_playlist_object_ids(other, &cancel)
            .await
            .unwrap(),
        ["other"]
    );
}

#[tokio::test]
async fn playlist_destinations_keep_global_rank_without_a_current_source() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    let other: library::SourceKey = sqlx::query_scalar(
        "INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('other','Other','other',zeroblob(32)) RETURNING source_key",
    ).fetch_one(&mut raw).await.expect("other configured source");
    drop(raw);
    let mut keys = Vec::new();
    for (owner, name) in [
        (None, "Global first"),
        (Some(other), "Other"),
        (Some(fixture.source), "Current"),
        (None, "Global last"),
    ] {
        keys.push(
            fixture
                .database
                .create_playlist(
                    owner,
                    name,
                    &library::CatalogQuery::uris(library::CatalogKind::Track, (&[]).to_vec())
                        .selection(),
                )
                .await
                .expect("create destination")
                .expect("playlist")
                .0,
        );
    }
    let cancel = ReadCancellation::new();
    let without_current = fixture
        .database
        .playlist_destinations(None, &cancel)
        .await
        .expect("global destinations");
    assert_eq!(
        without_current
            .iter()
            .map(|row| row.playlist_key)
            .collect::<Vec<_>>(),
        [keys[0], keys[3]]
    );
    let captured = fixture
        .database
        .playlist_destinations(Some(fixture.source), &cancel)
        .await
        .expect("captured destinations");
    assert_eq!(
        captured
            .iter()
            .map(|row| row.playlist_key)
            .collect::<Vec<_>>(),
        [keys[0], keys[2], keys[3]]
    );
    fixture
        .database
        .move_playlist(fixture.source, keys[3], keys[0])
        .await
        .expect("reorder visible subset");
    let mut raw = connection(&fixture.path).await;
    let order: Vec<library::PlaylistKey> =
        sqlx::query_scalar("SELECT playlist_key FROM playlists ORDER BY position,playlist_key")
            .fetch_all(&mut raw)
            .await
            .expect("global rank");
    assert_eq!(order, [keys[3], keys[1], keys[0], keys[2]]);
    drop(raw);
    fixture
        .database
        .add_playlist_tracks(
            None,
            keys[0],
            &library::CatalogQuery::uris(
                library::CatalogKind::Track,
                (&["https://example.test/unavailable.flac".into()]).to_vec(),
            )
            .selection(),
            false,
        )
        .await
        .expect("global URI edit without Current");
    let rows = fixture
        .database
        .playlist_destinations(None, &cancel)
        .await
        .expect("refreshed global destinations");
    assert_eq!(
        rows.iter()
            .find(|row| row.playlist_key == keys[0])
            .unwrap()
            .track_count,
        1
    );
}

#[tokio::test]
async fn playlist_order_is_title_ordered_and_folder_scoped() {
    let fixture = fixture().await;
    let zulu = fixture
        .database
        .create_playlist(
            Some(fixture.source),
            "Zulu",
            &library::CatalogQuery::uris(
                library::CatalogKind::Track,
                (&[fixture.track_uris[0].clone()]).to_vec(),
            )
            .selection(),
        )
        .await
        .expect("create Zulu Playlist")
        .expect("Track exists")
        .0;
    let alpha = fixture
        .database
        .create_playlist(
            Some(fixture.source),
            "Alpha",
            &library::CatalogQuery::uris(
                library::CatalogKind::Track,
                (&[fixture.track_uris[1].clone()]).to_vec(),
            )
            .selection(),
        )
        .await
        .expect("create Alpha Playlist")
        .expect("Track exists")
        .0;
    let mut raw = connection(&fixture.path).await;
    let folder = sqlx::query_scalar("INSERT INTO folders(source_key,object_id,metadata,media_uri,normalized_name) VALUES (?1,'destination-folder',jsonb(json_object('name','Destination Folder','sort_text','destination folder')),'folder:'||COALESCE(?1,0)||':'||'destination-folder',lower(COALESCE(json_extract(jsonb(json_object('name','Destination Folder','sort_text','destination folder')),'$.name'),''))) RETURNING folder_key")
        .bind(fixture.source)
        .fetch_one(&mut raw)
        .await
        .expect("insert destination folder");
    sqlx::query("INSERT INTO track_folders(track_key,folder_key,position) VALUES (?1,?2,1)")
        .bind(fixture.tracks[0])
        .bind(folder)
        .execute(&mut raw)
        .await
        .expect("scope Zulu Playlist Track");
    drop(raw);

    let cancellation = ReadCancellation::new();
    let all = library::CatalogQuery {
        subject: library::CatalogSubject::Playlists {
            source: Some(fixture.source),
            sort: CatalogSort::Title,
        },
        folder: None,
        filter: ("").into(),
        descending: false,
        favorites_only: false,
        downloaded_only: false,
        activity: true,
    }
    .rows(&fixture.database, 0, 256, &cancellation)
    .await
    .map(|rows| match rows {
        library::CatalogRows::Playlists(rows) => rows
            .into_iter()
            .map(|row| row.playlist_key)
            .collect::<Vec<_>>(),
        _ => unreachable!(),
    })
    .expect("all Playlists");
    assert_eq!(all, [alpha, zulu]);
    let scoped = library::CatalogQuery {
        subject: library::CatalogSubject::Playlists {
            source: Some(fixture.source),
            sort: CatalogSort::Title,
        },
        folder: Some(folder),
        filter: ("").into(),
        descending: false,
        favorites_only: false,
        downloaded_only: false,
        activity: true,
    }
    .rows(&fixture.database, 0, 256, &cancellation)
    .await
    .map(|rows| match rows {
        library::CatalogRows::Playlists(rows) => rows
            .into_iter()
            .map(|row| row.playlist_key)
            .collect::<Vec<_>>(),
        _ => unreachable!(),
    })
    .expect("folder-scoped Playlists");
    assert_eq!(scoped, [zulu]);
    let destinations = fixture
        .database
        .playlist_destinations(Some(fixture.source), &cancellation)
        .await
        .unwrap();
    assert_eq!(
        destinations
            .iter()
            .find(|row| row.playlist_key == zulu)
            .unwrap()
            .genres[0]
            .name,
        "Rock"
    );
    assert_eq!(
        fixture
            .database
            .playlist_genres(zulu, &cancellation)
            .await
            .unwrap()[0]
            .name,
        "Rock"
    );
}

#[tokio::test]
async fn playlist_reordering_keeps_one_rank_sequence() {
    let fixture = fixture().await;
    let mut playlists = Vec::new();
    for name in ["One", "Two", "Three", "Four"] {
        playlists.push(
            fixture
                .database
                .create_playlist(
                    Some(fixture.source),
                    name,
                    &library::CatalogQuery::uris(library::CatalogKind::Track, (&[]).to_vec())
                        .selection(),
                )
                .await
                .expect("create Playlist")
                .expect("Playlist key")
                .0,
        );
    }
    assert!(
        fixture
            .database
            .move_playlist(fixture.source, playlists[1], playlists[0])
            .await
            .expect("move 2 to 1")
    );
    assert!(
        fixture
            .database
            .move_playlist(fixture.source, playlists[3], playlists[2])
            .await
            .expect("move 4 to 3")
    );
    assert_eq!(
        library::CatalogQuery {
            subject: library::CatalogSubject::Playlists {
                source: Some(fixture.source),
                sort: CatalogSort::Position
            },
            folder: None,
            filter: ("").into(),
            descending: false,
            favorites_only: false,
            downloaded_only: false,
            activity: true
        }
        .rows(&fixture.database, 0, 256, &ReadCancellation::new())
        .await
        .map(|rows| match rows {
            library::CatalogRows::Playlists(rows) => rows
                .into_iter()
                .map(|row| row.playlist_key)
                .collect::<Vec<_>>(),
            _ => unreachable!(),
        })
        .expect("Playlist order"),
        [playlists[1], playlists[0], playlists[3], playlists[2]]
    );
}

#[tokio::test]
async fn playlist_edits_preserve_uri_occurrences_order_and_duplicates() {
    let fixture = fixture().await;
    let playlist = fixture
        .database
        .create_playlist(
            Some(fixture.source),
            "Duplicates",
            &library::CatalogQuery::uris(
                library::CatalogKind::Track,
                (&[
                    fixture.track_uris[0].clone(),
                    fixture.track_uris[0].clone(),
                    fixture.track_uris[1].clone(),
                ])
                    .to_vec(),
            )
            .selection(),
        )
        .await
        .expect("create Playlist")
        .expect("all Tracks exist")
        .0;
    let cancel = ReadCancellation::new();
    let initial = fixture
        .database
        .playlist_edit_entries(
            playlist,
            &library::CatalogQuery {
                subject: library::CatalogSubject::Tracks {
                    scope: library::TrackScope::Playlist(playlist),
                    sort: Some(CatalogSort::Position),
                },
                ..library::CatalogQuery::tracks(library::TrackScope::Playlist(playlist))
            }
            .selection(),
            &cancel,
        )
        .await
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| entry.playlist_entry_key)
                .collect::<Vec<_>>()
        })
        .expect("initial Playlist order");
    let rows = library::CatalogQuery::playlist_entries((&initial).to_vec())
        .rows(&fixture.database, 0, (&initial).len(), &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::PlaylistEntries(rows) => rows,
            _ => unreachable!(),
        })
        .expect("Playlist entry rows");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].track.media_uri, rows[1].track.media_uri);
    assert_ne!(rows[0].playlist_entry_key, rows[1].playlist_entry_key);
    let edit_entries = fixture
        .database
        .playlist_edit_entries(
            playlist,
            &library::CatalogQuery::playlist_entries(
                initial.iter().rev().copied().collect::<Vec<_>>(),
            )
            .selection(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        edit_entries
            .iter()
            .map(|entry| entry.playlist_entry_key)
            .collect::<Vec<_>>(),
        initial
    );
    assert_eq!(
        edit_entries
            .iter()
            .map(|entry| entry.position)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(edit_entries[0].track_key, edit_entries[1].track_key);
    assert!(
        fixture
            .database
            .playlist_edit_entries(
                playlist,
                &library::CatalogQuery::keys(
                    library::CatalogKind::Track,
                    None,
                    vec![edit_entries[0].track_key.raw()]
                )
                .selection(),
                &cancel,
            )
            .await
            .is_err()
    );
    let selected = fixture
        .database
        .playlist_edit_entries(
            playlist,
            &library::TrackSelection::Query {
                query: library::CatalogQuery {
                    descending: true,
                    ..library::CatalogQuery::tracks(library::TrackScope::Playlist(playlist))
                },
                ranges: Some(vec![0..1, 2..3]),
            },
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|entry| entry.playlist_entry_key)
            .collect::<Vec<_>>(),
        [initial[0], initial[2]]
    );
    assert_eq!(
        rows.iter()
            .map(|row| row.track.media_uri.as_str())
            .collect::<Vec<_>>(),
        [
            fixture.track_uris[0].as_str(),
            fixture.track_uris[0].as_str(),
            fixture.track_uris[1].as_str(),
        ]
    );

    assert_eq!(
        fixture
            .database
            .add_playlist_tracks(
                Some(fixture.source),
                playlist,
                &library::CatalogQuery::uris(
                    library::CatalogKind::Track,
                    (&[
                        fixture.track_uris[0].clone(),
                        fixture.track_uris[2].clone(),
                        fixture.track_uris[2].clone(),
                    ])
                        .to_vec()
                )
                .selection(),
                true
            )
            .await
            .expect("add with duplicate policy"),
        2
    );
    let added = fixture
        .database
        .playlist_edit_entries(
            playlist,
            &library::CatalogQuery {
                subject: library::CatalogSubject::Tracks {
                    scope: library::TrackScope::Playlist(playlist),
                    sort: Some(CatalogSort::Position),
                },
                ..library::CatalogQuery::tracks(library::TrackScope::Playlist(playlist))
            }
            .selection(),
            &cancel,
        )
        .await
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| entry.playlist_entry_key)
                .collect::<Vec<_>>()
        })
        .expect("added Playlist order");
    assert!(
        fixture
            .database
            .move_playlist_entry(Some(fixture.source), playlist, added[4], 0)
            .await
            .expect("move entry")
    );
    let moved = fixture
        .database
        .playlist_edit_entries(
            playlist,
            &library::CatalogQuery {
                subject: library::CatalogSubject::Tracks {
                    scope: library::TrackScope::Playlist(playlist),
                    sort: Some(CatalogSort::Position),
                },
                ..library::CatalogQuery::tracks(library::TrackScope::Playlist(playlist))
            }
            .selection(),
            &cancel,
        )
        .await
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| entry.playlist_entry_key)
                .collect::<Vec<_>>()
        })
        .expect("moved Playlist order");
    assert_eq!(moved[0], added[4]);
    assert_eq!(
        fixture
            .database
            .remove_playlist_entries(Some(fixture.source), playlist, &moved[1..3])
            .await
            .expect("remove entries"),
        2
    );

    let selection = vec![fixture.track_uris[3].clone(); 520];
    assert_eq!(
        fixture
            .database
            .add_playlist_tracks(
                Some(fixture.source),
                playlist,
                &library::CatalogQuery::uris(library::CatalogKind::Track, (&selection).to_vec())
                    .selection(),
                false
            )
            .await
            .expect("add selected media"),
        520
    );
    assert_eq!(
        fixture
            .database
            .playlist_edit_entries(
                playlist,
                &library::CatalogQuery {
                    subject: library::CatalogSubject::Tracks {
                        scope: library::TrackScope::Playlist(playlist),
                        sort: Some(CatalogSort::Position)
                    },
                    ..library::CatalogQuery::tracks(library::TrackScope::Playlist(playlist))
                }
                .selection(),
                &cancel
            )
            .await
            .map(|entries| entries
                .into_iter()
                .map(|entry| entry.playlist_entry_key)
                .collect::<Vec<_>>())
            .expect("complete Playlist order")
            .len(),
        523
    );
    let entries = fixture
        .database
        .playlist_edit_entries(
            playlist,
            &library::CatalogQuery {
                subject: library::CatalogSubject::Tracks {
                    scope: library::TrackScope::Playlist(playlist),
                    sort: Some(CatalogSort::Position),
                },
                ..library::CatalogQuery::tracks(library::TrackScope::Playlist(playlist))
            }
            .selection(),
            &cancel,
        )
        .await
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| entry.playlist_entry_key)
                .collect::<Vec<_>>()
        })
        .unwrap();
    let state = super::support::resolve_queue(
        &fixture.database,
        library::QueueLoad::Tracks(vec![(
            library::CatalogQuery::playlist_entries(entries).selection(),
            library::QueueProvenance::Context {
                context_id: "playlist".into(),
                context_title: None,
                source_rank: 0,
            },
        )]),
        Default::default(),
    )
    .await;
    assert_eq!(state.entries.len(), 523);
    assert!(state.rows.len() <= 100);
    assert_eq!(state.rows[3].track.media_uri, fixture.track_uris[3]);
    assert_eq!(state.rows[95].track.media_uri, fixture.track_uris[3]);
    assert_ne!(state.rows[3].position, state.rows[95].position);
}

#[tokio::test]
async fn global_playlist_adds_rufin_media_without_provider_admission() {
    let fixture = fixture().await;
    let (playlist, _) = fixture
        .database
        .create_playlist(
            None,
            "Global",
            &library::CatalogQuery::uris(
                library::CatalogKind::Track,
                (&[fixture.track_uris[0].clone()]).to_vec(),
            )
            .selection(),
        )
        .await
        .expect("create global Playlist")
        .expect("Playlist key");
    assert_eq!(
        fixture
            .database
            .add_playlist_tracks(
                None,
                playlist,
                &library::CatalogQuery::uris(
                    library::CatalogKind::Track,
                    (&[
                        fixture.track_uris[1].clone(),
                        fixture.track_uris[1].clone(),
                        fixture.track_uris[2].clone(),
                    ])
                        .to_vec()
                )
                .selection(),
                false
            )
            .await
            .expect("add global Playlist media"),
        3
    );
    let order = fixture
        .database
        .selected_track_uris(
            &library::CatalogQuery::tracks(library::TrackScope::Playlist(playlist)).selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .expect("global Playlist order");
    assert_eq!(
        order,
        [
            fixture.track_uris[0].clone(),
            fixture.track_uris[1].clone(),
            fixture.track_uris[1].clone(),
            fixture.track_uris[2].clone(),
        ]
    );

    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET indexed=0 WHERE media_uri=?1")
        .bind(&fixture.track_uris[1])
        .execute(&mut raw)
        .await
        .expect("remove source membership");
    drop(raw);
    let entries = fixture
        .database
        .playlist_edit_entries(
            playlist,
            &library::CatalogQuery {
                subject: library::CatalogSubject::Tracks {
                    scope: library::TrackScope::Playlist(playlist),
                    sort: Some(CatalogSort::Position),
                },
                ..library::CatalogQuery::tracks(library::TrackScope::Playlist(playlist))
            }
            .selection(),
            &ReadCancellation::new(),
        )
        .await
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| entry.playlist_entry_key)
                .collect::<Vec<_>>()
        })
        .expect("unavailable entries");
    let rows = library::CatalogQuery::playlist_entries((&entries).to_vec())
        .rows(
            &fixture.database,
            0,
            (&entries).len(),
            &ReadCancellation::new(),
        )
        .await
        .map(|rows| match rows {
            library::CatalogRows::PlaylistEntries(rows) => rows,
            _ => unreachable!(),
        })
        .expect("unavailable Playlist rows");
    assert_eq!(rows[1].track.metadata.title, "Beta");
    assert_eq!(rows[1].track.media_uri, fixture.track_uris[1]);
    let playback = super::support::resolve_queue(
        &fixture.database,
        library::QueueLoad::Tracks(vec![(
            library::CatalogQuery::playlist_entries(entries).selection(),
            library::QueueProvenance::Context {
                context_id: "unavailable".into(),
                context_title: None,
                source_rank: 0,
            },
        )]),
        Default::default(),
    )
    .await;
    assert_eq!(playback.rows[1].track.metadata.title, "Beta");
    assert_eq!(playback.rows[1].track.media_uri, fixture.track_uris[1]);

    assert_eq!(
        fixture
            .database
            .playlist_access(playlist, &ReadCancellation::new())
            .await
            .expect("read global Playlist owner")
            .map(|access| (access.source_key, access.source_id, access.source_owned)),
        Some((None, None, false))
    );
    assert!(
        fixture
            .database
            .update_playlist(None, playlist, Some("Renamed global"), None)
            .await
            .expect("rename global Playlist")
    );
    assert!(
        fixture
            .database
            .delete_playlist(None, playlist)
            .await
            .expect("delete global Playlist")
    );
    assert_eq!(
        fixture
            .database
            .playlist_access(playlist, &ReadCancellation::new())
            .await
            .expect("deleted Playlist owner"),
        None
    );
}

#[tokio::test]
async fn playlist_automatic_covers_keep_distinct_saved_choices() {
    let fixture = fixture().await;
    let [first, second] = ["first-album", "second-album"].map(|item_id| {
        serde_json::to_vec(&[library::ArtworkCandidate::new(
            library::ArtworkImage::Native {
                source_id: library::SourceId::new("source"),
                item_id: item_id.into(),
                tag: None,
            },
            None,
        )])
        .unwrap()
    });
    fixture
        .database
        .catalog("source", None)
        .await
        .unwrap()
        .set_source_artwork(
            "album",
            &[
                ("album-a".into(), Some(first.clone())),
                ("album-b".into(), Some(second.clone())),
            ],
        )
        .await
        .unwrap();
    let playlist = fixture
        .database
        .create_playlist(
            Some(fixture.source),
            "Positional covers",
            &library::CatalogQuery::uris(
                library::CatalogKind::Track,
                (&[
                    fixture.track_uris[0].clone(),
                    fixture.track_uris[0].clone(),
                    fixture.track_uris[2].clone(),
                    fixture.track_uris[1].clone(),
                    fixture.track_uris[3].clone(),
                ])
                    .to_vec(),
            )
            .selection(),
        )
        .await
        .expect("create Playlist")
        .expect("all Tracks exist")
        .0;
    let row = library::CatalogQuery::keys(
        library::CatalogKind::Playlist,
        None,
        (&[playlist]).iter().map(|key| key.raw()).collect(),
    )
    .rows(
        &fixture.database,
        0,
        (&[playlist]).len(),
        &ReadCancellation::new(),
    )
    .await
    .map(|rows| match rows {
        library::CatalogRows::Playlists(rows) => rows,
        _ => unreachable!(),
    })
    .expect("Playlist cover samples")
    .pop()
    .expect("Playlist row");
    let candidate =
        serde_json::from_slice::<Vec<library::ArtworkCandidate>>(&row.artwork_binding.unwrap())
            .unwrap()
            .remove(0);
    let candidate = fixture
        .database
        .resolve_playlist_artwork(candidate)
        .await
        .unwrap()
        .unwrap();
    let library::ArtworkImage::Playlist {
        automatic: Some(manifest),
        resolved: Some(recipes),
        ..
    } = candidate.image
    else {
        panic!("automatic playlist image")
    };
    assert_eq!(manifest.chosen.len(), 2);
    assert_eq!(recipes.len(), 2);
    let actual = recipes
        .iter()
        .map(|recipe| serde_json::to_vec(recipe).unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(actual, [first, second].into_iter().collect());
    let saved: Vec<u8> =
        sqlx::query_scalar("SELECT artwork_binding FROM playlists WHERE playlist_key=?1")
            .bind(playlist)
            .fetch_one(&mut connection(&fixture.path).await)
            .await
            .unwrap();
    fixture
        .database
        .update_playlist(
            Some(fixture.source),
            playlist,
            None,
            Some(Some((b"manual image", Some("image/png")))),
        )
        .await
        .unwrap();
    let replaced: Vec<u8> =
        sqlx::query_scalar("SELECT artwork_binding FROM playlists WHERE playlist_key=?1")
            .bind(playlist)
            .fetch_one(&mut connection(&fixture.path).await)
            .await
            .unwrap();
    assert!(
        library::authored_playlist_artwork(&replaced)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        library::automatic_playlist_artwork(&replaced).unwrap(),
        Some(saved.clone())
    );
    fixture
        .database
        .update_playlist(Some(fixture.source), playlist, None, Some(None))
        .await
        .unwrap();
    let restored: Vec<u8> =
        sqlx::query_scalar("SELECT artwork_binding FROM playlists WHERE playlist_key=?1")
            .bind(playlist)
            .fetch_one(&mut connection(&fixture.path).await)
            .await
            .unwrap();
    assert_eq!(restored, saved);
}

#[tokio::test]
async fn provider_playlist_payload_resolves_uri_only_at_the_protocol_boundary() {
    let fixture = fixture().await;
    let playlist = fixture
        .database
        .create_playlist(
            Some(fixture.source),
            "Provider list",
            &library::CatalogQuery::uris(
                library::CatalogKind::Track,
                (&[fixture.track_uris[0].clone()]).to_vec(),
            )
            .selection(),
        )
        .await
        .expect("seed Playlist")
        .expect("Playlist key")
        .0;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE main.playlists SET object_id='provider:playlist' WHERE playlist_key=?1")
        .bind(playlist)
        .execute(&mut raw)
        .await
        .expect("make source-owned Playlist");
    drop(raw);
    assert_eq!(
        fixture
            .database
            .source_playlist_track_ids(
                fixture.source,
                Some(playlist),
                &library::CatalogQuery::uris(
                    library::CatalogKind::Track,
                    (&[
                        fixture.track_uris[0].clone(),
                        fixture.track_uris[1].clone(),
                        fixture.track_uris[1].clone(),
                    ])
                        .to_vec()
                )
                .selection(),
                true
            )
            .await
            .expect("provider payload"),
        ["track-1", "track-1"]
    );
}

#[tokio::test]
async fn source_owned_playlist_additions_report_the_destination_without_partial_writes() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    let destination: library::SourceKey = sqlx::query_scalar(
        "INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('destination','Destination','destination',zeroblob(32)) RETURNING source_key",
    ).fetch_one(&mut raw).await.unwrap();
    let own_uri = "test:destination-track".to_string();
    sqlx::query("INSERT INTO tracks(source_key,object_id,media_uri,metadata,normalized_search,indexed) VALUES (?1,'destination-track',?2,jsonb(json_object('title',COALESCE('Destination Track',''),'artist',COALESCE('',''),'album',COALESCE('',''),'sort_text',COALESCE('destination track',''),'duration_millis',COALESCE(1000,0))),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE('Destination Track',''),'artist',COALESCE('',''),'album',COALESCE('',''),'sort_text',COALESCE('destination track',''),'duration_millis',COALESCE(1000,0))),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Destination Track',''),'artist',COALESCE('',''),'album',COALESCE('',''),'sort_text',COALESCE('destination track',''),'duration_millis',COALESCE(1000,0))),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Destination Track',''),'artist',COALESCE('',''),'album',COALESCE('',''),'sort_text',COALESCE('destination track',''),'duration_millis',COALESCE(1000,0))),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Destination Track',''),'artist',COALESCE('',''),'album',COALESCE('',''),'sort_text',COALESCE('destination track',''),'duration_millis',COALESCE(1000,0))),'$.comment'),'')),1)")
        .bind(destination).bind(&own_uri).execute(&mut raw).await.unwrap();
    drop(raw);
    let database = &fixture.database;
    let cancel = ReadCancellation::new();
    let playlist = database
        .create_playlist(
            Some(destination),
            "Destination",
            &library::CatalogQuery::uris(library::CatalogKind::Track, (&[]).to_vec()).selection(),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(
        database
            .source_playlist_track_ids(
                destination,
                Some(playlist),
                &library::CatalogQuery::uris(
                    library::CatalogKind::Track,
                    (std::slice::from_ref(&own_uri)).to_vec()
                )
                .selection(),
                false
            )
            .await
            .unwrap(),
        ["destination-track"]
    );
    assert_eq!(
        database
            .add_playlist_tracks(
                Some(destination),
                playlist,
                &library::CatalogQuery::uris(
                    library::CatalogKind::Track,
                    (std::slice::from_ref(&own_uri)).to_vec()
                )
                .selection(),
                false
            )
            .await
            .unwrap(),
        1
    );
    for selection in [
        vec![fixture.track_uris[0].clone()],
        vec![own_uri.clone(), fixture.track_uris[0].clone()],
    ] {
        let provider_error = database
            .source_playlist_track_ids(
                destination,
                Some(playlist),
                &library::CatalogQuery::uris(library::CatalogKind::Track, (&selection).to_vec())
                    .selection(),
                true,
            )
            .await
            .unwrap_err();
        let authored_error = database
            .add_playlist_tracks(
                Some(destination),
                playlist,
                &library::CatalogQuery::uris(library::CatalogKind::Track, (&selection).to_vec())
                    .selection(),
                false,
            )
            .await
            .unwrap_err();
        for error in [provider_error, authored_error] {
            assert!(
                matches!(error, library::LibraryError::PlaylistSourceMismatch(source)
                if source.as_str() == "destination")
            );
        }
    }
    let missing = vec![own_uri.clone(), "test:missing-track".to_string()];
    assert!(matches!(
        database
            .source_playlist_track_ids(destination,Some(playlist),&library::CatalogQuery::uris(library::CatalogKind::Track,(&missing).to_vec()).selection(),false)
            .await
            .unwrap_err(),
        library::LibraryError::InvalidRequest(message) if message == "Playlist Track is no longer current"
    ));
    assert_eq!(
        database
            .add_playlist_tracks(
                Some(destination),
                playlist,
                &library::CatalogQuery::uris(library::CatalogKind::Track, (&missing).to_vec())
                    .selection(),
                false
            )
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        database
            .selected_track_uris(
                &library::CatalogQuery::tracks(library::TrackScope::Playlist(playlist)).selection(),
                None,
                &cancel
            )
            .await
            .unwrap(),
        std::slice::from_ref(&own_uri)
    );
    let global = database
        .create_playlist(
            None,
            "Global",
            &library::CatalogQuery::uris(library::CatalogKind::Track, (&[]).to_vec()).selection(),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
    let mixed = vec![own_uri, fixture.track_uris[0].clone()];
    assert_eq!(
        database
            .add_playlist_tracks(
                None,
                global,
                &library::CatalogQuery::uris(library::CatalogKind::Track, (&mixed).to_vec())
                    .selection(),
                false
            )
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        database
            .selected_track_uris(
                &library::CatalogQuery::tracks(library::TrackScope::Playlist(global)).selection(),
                None,
                &cancel
            )
            .await
            .unwrap(),
        mixed
    );
}

#[tokio::test]
async fn rating_favorite_and_delivery_are_uri_owned() {
    let mut fixture = fixture().await;
    let cancellation = ReadCancellation::new();
    let target = FavoriteTarget::Track(fixture.track_uris[0].clone());
    assert_eq!(
        fixture
            .database
            .user_media_state(target.media_uri(), &ReadCancellation::new())
            .await
            .expect("read absent user state"),
        None
    );
    assert!(
        fixture
            .database
            .set_rating(&target, Some(8))
            .await
            .expect("set Rating")
    );
    assert!(
        fixture
            .database
            .set_favorite(&target, true)
            .await
            .expect("set Favorite")
    );
    let row = library::CatalogQuery::keys(
        library::CatalogKind::Track,
        None,
        (&[fixture.tracks[0]]).iter().map(|key| key.raw()).collect(),
    )
    .rows(
        &fixture.database,
        0,
        (&[fixture.tracks[0]]).len(),
        &ReadCancellation::new(),
    )
    .await
    .map(|rows| match rows {
        library::CatalogRows::Tracks(rows) => rows,
        _ => unreachable!("typed catalog projection"),
    })
    .expect("read Track")
    .remove(0);
    assert_eq!(row.state.rating, Some(8));
    assert!(row.state.favorite);
    assert_eq!(
        fixture
            .database
            .user_media_state(target.media_uri(), &ReadCancellation::new())
            .await
            .expect("read URI-owned user state"),
        Some((Some(true), Some(8)))
    );

    assert!(
        fixture
            .database
            .queue_remote_favorite(&target, false, 100)
            .await
            .expect("queue delivery")
    );
    assert!(
        fixture
            .database
            .due_remote_favorites(99, 64, &cancellation)
            .await
            .unwrap()
            .is_empty()
    );
    let album = FavoriteTarget::Album(fixture.album_uris[0].clone());
    fixture
        .database
        .queue_remote_favorite(&album, true, 100)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .database
            .due_remote_favorites(100, 1, &cancellation)
            .await
            .unwrap(),
        [(target.media_uri().to_string(), false)]
    );
    assert_eq!(
        fixture
            .database
            .due_remote_favorites(100, 64, &cancellation)
            .await
            .unwrap(),
        [
            (target.media_uri().to_string(), false),
            (album.media_uri().to_string(), true)
        ]
    );
    assert!(
        fixture
            .database
            .acknowledge_remote_favorite(&album, true)
            .await
            .unwrap()
    );
    assert!(fixture.database.favorite(&album).await.unwrap());
    assert!(
        fixture
            .database
            .defer_remote_favorite(&target, false, 200)
            .await
            .expect("defer delivery")
    );
    fixture.database.clone().close().await.unwrap();
    fixture.database = library::Database::open(&fixture.path).await.unwrap();
    assert!(!fixture.database.favorite(&target).await.unwrap());
    assert!(
        fixture
            .database
            .due_remote_favorites(199, 64, &cancellation)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture
            .database
            .due_remote_favorites(200, 64, &cancellation)
            .await
            .unwrap(),
        [(target.media_uri().to_string(), false)]
    );
    fixture
        .database
        .queue_remote_favorite(&target, true, 201)
        .await
        .unwrap();
    assert!(
        !fixture
            .database
            .acknowledge_remote_favorite(&target, false)
            .await
            .unwrap()
    );
    assert!(
        !fixture
            .database
            .defer_remote_favorite(&target, false, 300)
            .await
            .unwrap()
    );
    assert_eq!(
        fixture
            .database
            .reject_remote_favorite(&target, false)
            .await
            .unwrap(),
        None
    );
    assert!(fixture.database.favorite(&target).await.unwrap());
    assert_eq!(
        fixture
            .database
            .due_remote_favorites(201, 64, &cancellation)
            .await
            .unwrap(),
        [(target.media_uri().to_string(), true)]
    );
    fixture
        .database
        .queue_remote_favorite(&target, false, 202)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .database
            .reject_remote_favorite(&target, false)
            .await
            .expect("reject delivery"),
        Some(true)
    );
    assert!(
        fixture
            .database
            .due_remote_favorites(300, 64, &cancellation)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        library::CatalogQuery::keys(
            library::CatalogKind::Track,
            None,
            (&[fixture.tracks[0]]).iter().map(|key| key.raw()).collect()
        )
        .rows(
            &fixture.database,
            0,
            (&[fixture.tracks[0]]).len(),
            &ReadCancellation::new()
        )
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!(),
        })
        .expect("restored Track")[0]
            .state
            .favorite
    );
}
