use library::{CatalogSort, FavoriteTarget, ReadCancellation};

use super::support::{connection, fixture};

#[tokio::test]
async fn playlist_pin_import_reads_only_the_owning_sources_identities() {
    let fixture = fixture().await;
    let catalog = &fixture.catalog;
    let (_, authored) = catalog
        .create_playlist(
            "Z saved",
            &library::MemberSelection::Uris(((&[]).to_vec()).into()),
        )
        .await
        .unwrap()
        .unwrap();
    catalog
        .create_playlist(
            "Global",
            &library::MemberSelection::Uris(((&[]).to_vec()).into()),
        )
        .await
        .unwrap();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO playlists(source_key,owner,object_id,name) VALUES (?1,'source','native','A native')")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    let other: library::SourceKey = sqlx::query_scalar(
        "INSERT INTO sources(object_id,display_name) VALUES ('emby','Emby') RETURNING source_key",
    )
    .fetch_one(&mut raw)
    .await
    .unwrap();
    sqlx::query("INSERT INTO playlists(source_key,owner,object_id,name) VALUES (?1,'source','other','Other')")
        .bind(other).execute(&mut raw).await.unwrap();
    drop(raw);
    let cancel = ReadCancellation::new();
    assert_eq!(
        catalog
            .source_playlist_object_ids(fixture.source, &cancel)
            .await
            .unwrap(),
        // Rufin playlists hold tracks of every source, so no source owns them.
        ["native".to_string()]
    );
    assert!(!authored.is_empty());
    assert_eq!(
        catalog
            .source_playlist_object_ids(other, &cancel)
            .await
            .unwrap(),
        ["other"]
    );
}

#[tokio::test]
async fn playlist_order_is_title_ordered_and_folder_scoped() {
    let mut fixture = fixture().await;
    let zulu = fixture
        .catalog
        .create_playlist(
            "Zulu",
            &library::MemberSelection::Uris(((&[fixture.track_uris[0].clone()]).to_vec()).into()),
        )
        .await
        .expect("create Zulu Playlist")
        .expect("Track exists")
        .0;
    let alpha = fixture
        .catalog
        .create_playlist(
            "Alpha",
            &library::MemberSelection::Uris(((&[fixture.track_uris[1].clone()]).to_vec()).into()),
        )
        .await
        .expect("create Alpha Playlist")
        .expect("Track exists")
        .0;
    let mut raw = connection(&fixture.path).await;
    let folder = sqlx::query_scalar("INSERT INTO folders(source_key,object_id,name,sort_text,media_uri) VALUES (?1,'destination-folder','Destination Folder','destination folder','folder:'||COALESCE(?1,0)||':'||'destination-folder') RETURNING folder_key")
        .bind(fixture.source)
        .fetch_one(&mut raw)
        .await
        .expect("insert destination folder");
    sqlx::query("INSERT INTO track_links(track_key,role,position,target) VALUES (?1,4,1,?2)")
        .bind(fixture.tracks[0])
        .bind(folder)
        .execute(&mut raw)
        .await
        .expect("scope Zulu Playlist Track");
    drop(raw);
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;

    let cancellation = ReadCancellation::new();
    let all = library::ListRead {
        playlist_scope: Default::default(),
        kind: library::ListKind::Playlists,
        source: Some(fixture.source),
        sort: CatalogSort::Title,
        folder: None,
        filter: ("").into(),
        descending: false,
        favorites_only: false,
        downloaded_only: false,
        activity: true,
    }
    .playlists(&fixture.catalog, 0, 256, &cancellation)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|row| row.playlist_key)
            .collect::<Vec<_>>()
    })
    .expect("all Playlists");
    assert_eq!(all, [alpha, zulu]);
    let scoped = library::ListRead {
        playlist_scope: Default::default(),
        kind: library::ListKind::Playlists,
        source: Some(fixture.source),
        sort: CatalogSort::Title,
        folder: Some(folder),
        filter: ("").into(),
        descending: false,
        favorites_only: false,
        downloaded_only: false,
        activity: true,
    }
    .playlists(&fixture.catalog, 0, 256, &cancellation)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|row| row.playlist_key)
            .collect::<Vec<_>>()
    })
    .expect("folder-scoped Playlists");
    // A music folder scopes source playlists; Rufin playlists hold tracks of every source.
    assert_eq!(scoped, [alpha, zulu]);
    let destinations = fixture
        .catalog
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
            .catalog
            .playlists(&[zulu], &cancellation)
            .await
            .unwrap()[0]
            .genres[0]
            .name,
        "Rock"
    );
}

#[tokio::test]
async fn playlist_edits_preserve_uri_occurrences_order_and_duplicates() {
    let fixture = fixture().await;
    let playlist = fixture
        .catalog
        .create_playlist(
            "Duplicates",
            &library::MemberSelection::Uris(
                ((&[
                    fixture.track_uris[0].clone(),
                    fixture.track_uris[0].clone(),
                    fixture.track_uris[1].clone(),
                ])
                    .to_vec())
                .into(),
            ),
        )
        .await
        .expect("create Playlist")
        .expect("all Tracks exist")
        .0;
    let cancel = ReadCancellation::new();
    let initial = fixture
        .catalog
        .playlist_edit_entries(
            playlist,
            &library::MemberRead {
                collection: library::Collection::Playlist(playlist),
                sort: Some(CatalogSort::Position),
                ..library::MemberRead::new(library::Collection::Playlist(playlist))
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
    let rows = fixture
        .catalog
        .playlist_entries(&((&initial).to_vec()), &cancel)
        .await
        .expect("Playlist entry rows");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].track.media_uri, rows[1].track.media_uri);
    assert_ne!(rows[0].playlist_entry_key, rows[1].playlist_entry_key);
    let edit_entries = fixture
        .catalog
        .playlist_edit_entries(
            playlist,
            &library::MemberSelection::Entries(
                (initial.iter().rev().copied().collect::<Vec<_>>()).into(),
            ),
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
            .catalog
            .playlist_edit_entries(
                playlist,
                &library::MemberSelection::Keys {
                    source: None,
                    keys: (vec![edit_entries[0].track_key.raw()])
                        .into_iter()
                        .map(library::TrackKey::from_raw)
                        .collect::<Vec<_>>()
                        .into()
                },
                &cancel,
            )
            .await
            .is_err()
    );
    let selected = fixture
        .catalog
        .playlist_edit_entries(
            playlist,
            &library::MemberSelection::Members {
                read: library::MemberRead {
                    descending: true,
                    ..library::MemberRead::new(library::Collection::Playlist(playlist))
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
            .catalog
            .add_playlist_tracks(
                playlist,
                &library::MemberSelection::Uris(
                    ((&[
                        fixture.track_uris[0].clone(),
                        fixture.track_uris[2].clone(),
                        fixture.track_uris[2].clone(),
                    ])
                        .to_vec())
                    .into()
                ),
                true
            )
            .await
            .expect("add with duplicate policy"),
        2
    );
    let added = fixture
        .catalog
        .playlist_edit_entries(
            playlist,
            &library::MemberRead {
                collection: library::Collection::Playlist(playlist),
                sort: Some(CatalogSort::Position),
                ..library::MemberRead::new(library::Collection::Playlist(playlist))
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
            .catalog
            .move_playlist_entry(playlist, added[4], 0)
            .await
            .expect("move entry")
    );
    let moved = fixture
        .catalog
        .playlist_edit_entries(
            playlist,
            &library::MemberRead {
                collection: library::Collection::Playlist(playlist),
                sort: Some(CatalogSort::Position),
                ..library::MemberRead::new(library::Collection::Playlist(playlist))
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
            .catalog
            .remove_playlist_entries(playlist, &moved[1..3])
            .await
            .expect("remove entries"),
        2
    );

    let selection = vec![fixture.track_uris[3].clone(); 520];
    assert_eq!(
        fixture
            .catalog
            .add_playlist_tracks(
                playlist,
                &library::MemberSelection::Uris(((&selection).to_vec()).into()),
                false
            )
            .await
            .expect("add selected media"),
        520
    );
    assert_eq!(
        fixture
            .catalog
            .playlist_edit_entries(
                playlist,
                &library::MemberRead {
                    collection: library::Collection::Playlist(playlist),
                    sort: Some(CatalogSort::Position),
                    ..library::MemberRead::new(library::Collection::Playlist(playlist))
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
        .catalog
        .playlist_edit_entries(
            playlist,
            &library::MemberRead {
                collection: library::Collection::Playlist(playlist),
                sort: Some(CatalogSort::Position),
                ..library::MemberRead::new(library::Collection::Playlist(playlist))
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
        &fixture.catalog,
        library::QueueLoad::Tracks(vec![(
            library::MemberSelection::Entries((entries).into()),
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
        .catalog
        .create_playlist(
            "Global",
            &library::MemberSelection::Uris(((&[fixture.track_uris[0].clone()]).to_vec()).into()),
        )
        .await
        .expect("create global Playlist")
        .expect("Playlist key");
    assert_eq!(
        fixture
            .catalog
            .add_playlist_tracks(
                playlist,
                &library::MemberSelection::Uris(
                    ((&[
                        fixture.track_uris[1].clone(),
                        fixture.track_uris[1].clone(),
                        fixture.track_uris[2].clone(),
                    ])
                        .to_vec())
                    .into()
                ),
                false
            )
            .await
            .expect("add global Playlist media"),
        3
    );
    let order = fixture
        .catalog
        .selected_track_uris(
            &library::MemberRead::new(library::Collection::Playlist(playlist)).selection(),
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
        .catalog
        .playlist_edit_entries(
            playlist,
            &library::MemberRead {
                collection: library::Collection::Playlist(playlist),
                sort: Some(CatalogSort::Position),
                ..library::MemberRead::new(library::Collection::Playlist(playlist))
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
    let rows = fixture
        .catalog
        .playlist_entries(&((&entries).to_vec()), &ReadCancellation::new())
        .await
        .expect("unavailable Playlist rows");
    assert_eq!(rows[1].track.metadata.title, "Beta");
    assert_eq!(rows[1].track.media_uri, fixture.track_uris[1]);
    let playback = super::support::resolve_queue(
        &fixture.catalog,
        library::QueueLoad::Tracks(vec![(
            library::MemberSelection::Entries((entries).into()),
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
            .catalog
            .playlist_access(playlist, &ReadCancellation::new())
            .await
            .expect("read global Playlist owner")
            .map(|access| (access.source_key, access.source_id, access.source_owned)),
        Some((None, None, false))
    );
    assert!(
        fixture
            .catalog
            .update_playlist(playlist, Some("Renamed global"), None)
            .await
            .expect("rename global Playlist")
    );
    assert!(
        fixture
            .catalog
            .delete_playlist(playlist)
            .await
            .expect("delete global Playlist")
    );
    assert_eq!(
        fixture
            .catalog
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
        .catalog
        .observe("source", None)
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
        .catalog
        .create_playlist(
            "Positional covers",
            &library::MemberSelection::Uris(
                ((&[
                    fixture.track_uris[0].clone(),
                    fixture.track_uris[0].clone(),
                    fixture.track_uris[2].clone(),
                    fixture.track_uris[1].clone(),
                    fixture.track_uris[3].clone(),
                ])
                    .to_vec())
                .into(),
            ),
        )
        .await
        .expect("create Playlist")
        .expect("all Tracks exist")
        .0;
    let row = fixture
        .catalog
        .playlists(&[playlist], &ReadCancellation::new())
        .await
        .expect("Playlist cover samples")
        .pop()
        .expect("Playlist row");
    let candidate =
        serde_json::from_slice::<Vec<library::ArtworkCandidate>>(&row.artwork_binding.unwrap())
            .unwrap()
            .remove(0);
    let candidate = fixture
        .catalog
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
        .catalog
        .update_playlist(
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
        .catalog
        .update_playlist(playlist, None, Some(None))
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
        .catalog
        .create_playlist(
            "Provider list",
            &library::MemberSelection::Uris(((&[fixture.track_uris[0].clone()]).to_vec()).into()),
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
            .catalog
            .source_playlist_track_ids(
                fixture.source,
                Some(playlist),
                &library::MemberSelection::Uris(
                    ((&[
                        fixture.track_uris[0].clone(),
                        fixture.track_uris[1].clone(),
                        fixture.track_uris[1].clone(),
                    ])
                        .to_vec())
                    .into()
                ),
                true
            )
            .await
            .expect("provider payload"),
        ["track-1", "track-1"]
    );
}

#[tokio::test]
async fn source_owned_playlist_additions_report_the_destination_without_partial_writes() {
    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    let destination: library::SourceKey = sqlx::query_scalar(
        "INSERT INTO sources(object_id,display_name) VALUES ('destination','Destination') RETURNING source_key",
    ).fetch_one(&mut raw).await.unwrap();
    let own_uri = "test:destination-track".to_string();
    sqlx::query("INSERT INTO tracks(source_key,object_id,media_uri,title,display_artist,display_album,sort_text,duration_millis,indexed) VALUES (?1,'destination-track',?2,COALESCE('Destination Track',''),COALESCE('',''),COALESCE('',''),COALESCE('destination track',''),COALESCE(1000,0),1)")
        .bind(destination).bind(&own_uri).execute(&mut raw).await.unwrap();
    drop(raw);
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let catalog = &fixture.catalog;
    let cancel = ReadCancellation::new();
    let playlist = catalog
        .create_playlist(
            "Destination",
            &library::MemberSelection::Uris(((&[]).to_vec()).into()),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(
        catalog
            .source_playlist_track_ids(
                destination,
                Some(playlist),
                &library::MemberSelection::Uris(((std::slice::from_ref(&own_uri)).to_vec()).into()),
                false
            )
            .await
            .unwrap(),
        ["destination-track"]
    );
    assert_eq!(
        catalog
            .add_playlist_tracks(
                playlist,
                &library::MemberSelection::Uris(((std::slice::from_ref(&own_uri)).to_vec()).into()),
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
        let provider_error = catalog
            .source_playlist_track_ids(
                destination,
                Some(playlist),
                &library::MemberSelection::Uris(((&selection).to_vec()).into()),
                true,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(provider_error, library::LibraryError::PlaylistSourceMismatch(source)
            if source.as_str() == "destination")
        );
    }
    let missing = vec![own_uri.clone(), "test:missing-track".to_string()];
    assert!(matches!(
        catalog
            .source_playlist_track_ids(destination,Some(playlist),&library::MemberSelection::Uris(missing.clone().into()),false)
            .await
            .unwrap_err(),
        library::LibraryError::InvalidRequest(message) if message == "Playlist Track is no longer current"
    ));
    assert_eq!(
        catalog
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Playlist(playlist)).selection(),
                None,
                &cancel
            )
            .await
            .unwrap(),
        std::slice::from_ref(&own_uri)
    );
    let global = catalog
        .create_playlist(
            "Global",
            &library::MemberSelection::Uris(((&[]).to_vec()).into()),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
    let mixed = vec![own_uri, fixture.track_uris[0].clone()];
    assert_eq!(
        catalog
            .add_playlist_tracks(
                global,
                &library::MemberSelection::Uris(((&mixed).to_vec()).into()),
                false
            )
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        catalog
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Playlist(global)).selection(),
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
            .catalog
            .user_media_state(target.media_uri(), &ReadCancellation::new())
            .await
            .expect("read absent user state"),
        None
    );
    assert!(
        fixture
            .catalog
            .set_rating(&target, Some(8))
            .await
            .expect("set Rating")
    );
    assert!(
        fixture
            .catalog
            .set_favorite(&target, true)
            .await
            .expect("set Favorite")
    );
    let row = fixture
        .catalog
        .tracks(&[fixture.tracks[0]], &ReadCancellation::new())
        .await
        .expect("read Track")
        .remove(0);
    assert_eq!(row.state.rating, Some(8));
    assert!(row.state.favorite);
    assert_eq!(
        fixture
            .catalog
            .user_media_state(target.media_uri(), &ReadCancellation::new())
            .await
            .expect("read URI-owned user state"),
        Some((Some(true), Some(8)))
    );

    assert!(
        fixture
            .catalog
            .queue_remote_favorite(&target, false, 100)
            .await
            .expect("queue delivery")
    );
    assert!(
        fixture
            .catalog
            .due_remote_favorites(99, 64, &cancellation)
            .await
            .unwrap()
            .is_empty()
    );
    let album = FavoriteTarget::Album(fixture.album_uris[0].clone());
    fixture
        .catalog
        .queue_remote_favorite(&album, true, 100)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .catalog
            .due_remote_favorites(100, 1, &cancellation)
            .await
            .unwrap(),
        [(target.media_uri().to_string(), false)]
    );
    assert_eq!(
        fixture
            .catalog
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
            .catalog
            .acknowledge_remote_favorite(&album, true)
            .await
            .unwrap()
    );
    assert!(fixture.catalog.favorite(&album).await.unwrap());
    assert!(
        fixture
            .catalog
            .defer_remote_favorite(&target, false, 200)
            .await
            .expect("defer delivery")
    );
    fixture.catalog.clone().close().await.unwrap();
    fixture.catalog = library::Catalog::open(&fixture.path).await.unwrap();
    assert!(!fixture.catalog.favorite(&target).await.unwrap());
    assert!(
        fixture
            .catalog
            .due_remote_favorites(199, 64, &cancellation)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture
            .catalog
            .due_remote_favorites(200, 64, &cancellation)
            .await
            .unwrap(),
        [(target.media_uri().to_string(), false)]
    );
    fixture
        .catalog
        .queue_remote_favorite(&target, true, 201)
        .await
        .unwrap();
    assert!(
        !fixture
            .catalog
            .acknowledge_remote_favorite(&target, false)
            .await
            .unwrap()
    );
    assert!(
        !fixture
            .catalog
            .defer_remote_favorite(&target, false, 300)
            .await
            .unwrap()
    );
    assert_eq!(
        fixture
            .catalog
            .reject_remote_favorite(&target, false)
            .await
            .unwrap(),
        None
    );
    assert!(fixture.catalog.favorite(&target).await.unwrap());
    assert_eq!(
        fixture
            .catalog
            .due_remote_favorites(201, 64, &cancellation)
            .await
            .unwrap(),
        [(target.media_uri().to_string(), true)]
    );
    fixture
        .catalog
        .queue_remote_favorite(&target, false, 202)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .catalog
            .reject_remote_favorite(&target, false)
            .await
            .expect("reject delivery"),
        Some(true)
    );
    assert!(
        fixture
            .catalog
            .due_remote_favorites(300, 64, &cancellation)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .catalog
            .tracks(&[fixture.tracks[0]], &ReadCancellation::new())
            .await
            .expect("restored Track")[0]
            .state
            .favorite
    );
}
