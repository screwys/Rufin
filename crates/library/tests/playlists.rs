use library::{Delivery, DeliveryKind, FavoriteTarget, ReadCancellation};

use super::support::{connection, fixture};

#[tokio::test]
async fn playlist_pin_import_reads_only_the_owning_sources_identities() {
    let mut fixture = fixture().await;
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
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let catalog = &fixture.catalog;
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
async fn playlist_mosaics_pick_distinct_covers_and_keep_them() {
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
    let (incomplete, _) = fixture
        .catalog
        .pick_mosaics(&[library::MosaicOwner::Playlist(playlist)])
        .await
        .unwrap();
    assert_eq!(incomplete, [library::MosaicOwner::Playlist(playlist)]);
    let mosaic = || async {
        let row = fixture
            .catalog
            .playlists(&[playlist], &ReadCancellation::new())
            .await
            .expect("Playlist cover samples")
            .pop()
            .expect("Playlist row");
        library::automatic_playlist_artwork(&row.artwork_binding.unwrap()).unwrap()
    };
    let saved = mosaic().await.expect("mosaic");
    let candidate = serde_json::from_slice::<Vec<library::ArtworkCandidate>>(&saved)
        .unwrap()
        .remove(0);
    let library::ArtworkImage::Playlist {
        mosaic: Some(picks),
        ..
    } = candidate.image
    else {
        panic!("mosaic playlist image")
    };
    assert_eq!(picks.len(), 2);
    let actual = fixture
        .catalog
        .track_artwork_bindings(&picks, &ReadCancellation::new())
        .await
        .unwrap()
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(actual, [first, second].into_iter().collect());
    fixture
        .catalog
        .update_playlist(
            playlist,
            None,
            Some(Some((b"manual image", Some("image/png")))),
        )
        .await
        .unwrap();
    assert_eq!(mosaic().await, Some(saved.clone()));
    fixture
        .catalog
        .update_playlist(playlist, None, Some(None))
        .await
        .unwrap();
    assert_eq!(mosaic().await, Some(saved));
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
async fn deliveries_wait_until_accepted_and_then_drop_rufins_copy() {
    let mut fixture = fixture().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let target = FavoriteTarget::Track(fixture.track_uris[0].clone());
    let cancellation = ReadCancellation::new();
    assert_eq!(
        fixture
            .catalog
            .user_state(target.media_uri(), &cancellation)
            .await
            .unwrap(),
        None
    );
    fixture
        .catalog
        .set_rating(&target, Some(8), false)
        .await
        .unwrap();
    fixture
        .catalog
        .set_favorite(&target, true, false)
        .await
        .unwrap();
    let row = fixture
        .catalog
        .tracks(&[fixture.tracks[0]], &cancellation)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(row.state.rating, Some(8));
    assert!(row.state.favorite);
    assert!(
        fixture
            .catalog
            .due_deliveries(now + 60, 64)
            .await
            .unwrap()
            .is_empty()
    );

    // A favorite for a server waits in the outbox; Rufin shows it meanwhile.
    fixture
        .catalog
        .set_favorite(&target, false, true)
        .await
        .unwrap();
    let album = FavoriteTarget::Album(fixture.album_uris[0].clone());
    fixture
        .catalog
        .set_favorite(&album, true, true)
        .await
        .unwrap();
    let track_delivery = Delivery {
        media_uri: target.media_uri().into(),
        kind: DeliveryKind::Favorite,
        value: Some(0),
    };
    let album_delivery = Delivery {
        media_uri: album.media_uri().into(),
        kind: DeliveryKind::Favorite,
        value: Some(1),
    };
    let mut due = fixture.catalog.due_deliveries(now + 60, 64).await.unwrap();
    due.sort_by(|a, b| a.media_uri.cmp(&b.media_uri));
    let mut expected = vec![track_delivery.clone(), album_delivery.clone()];
    expected.sort_by(|a, b| a.media_uri.cmp(&b.media_uri));
    assert_eq!(due, expected);
    assert!(!fixture.catalog.favorite(&target).await.unwrap());

    // Accepted: the value becomes the source's and Rufin's copy goes.
    assert!(
        fixture
            .catalog
            .delivered(&album_delivery, false)
            .await
            .unwrap()
    );
    assert!(fixture.catalog.favorite(&album).await.unwrap());
    assert_eq!(
        fixture
            .catalog
            .user_state(album.media_uri(), &cancellation)
            .await
            .unwrap(),
        None
    );

    // A deferred delivery waits for its time, across a restart.
    fixture
        .catalog
        .defer_delivery(&track_delivery, now + 300)
        .await
        .unwrap();
    fixture.catalog.clone().close().await.unwrap();
    fixture.catalog = library::Catalog::open(&fixture.path).await.unwrap();
    assert!(
        fixture
            .catalog
            .due_deliveries(now + 60, 64)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture.catalog.next_delivery_at().await.unwrap(),
        Some(now + 300)
    );

    // A newer value replaces the waiting one; the older one's acceptance changes nothing.
    fixture
        .catalog
        .set_favorite(&target, true, true)
        .await
        .unwrap();
    assert!(
        !fixture
            .catalog
            .delivered(&track_delivery, false)
            .await
            .unwrap()
    );
    let newer = Delivery {
        value: Some(1),
        ..track_delivery
    };
    assert_eq!(
        fixture.catalog.due_deliveries(now + 60, 64).await.unwrap(),
        std::slice::from_ref(&newer)
    );

    // Refused: the delivery and Rufin's copy go, so the source's own value shows.
    assert!(fixture.catalog.drop_delivery(&newer, false).await.unwrap());
    assert!(!fixture.catalog.favorite(&target).await.unwrap());
    assert_eq!(
        fixture
            .catalog
            .user_state(target.media_uri(), &cancellation)
            .await
            .unwrap(),
        Some((None, Some(8)))
    );
    assert_eq!(fixture.catalog.next_delivery_at().await.unwrap(), None);
}
