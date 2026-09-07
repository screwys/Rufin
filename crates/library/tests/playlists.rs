use library::{FavoriteTarget, PlaylistEntrySort, PlaylistSort, ReadCancellation};

use super::support::{connection, fixture};

#[tokio::test]
async fn playlist_destinations_keep_global_rank_without_a_current_source() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO main.source_ids(object_id) VALUES('other')")
        .execute(&mut raw)
        .await
        .unwrap();
    let other: library::SourceKey = sqlx::query_scalar(
        "INSERT INTO sources(object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES('other','Other','other',zeroblob(32),zeroblob(32)) RETURNING source_key",
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
                .create_playlist(owner, name, &[])
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
        .add_playlist_media(
            None,
            keys[0],
            &["https://example.test/unavailable.flac".into()],
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
            &[fixture.track_uris[0].clone()],
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
            &[fixture.track_uris[1].clone()],
        )
        .await
        .expect("create Alpha Playlist")
        .expect("Track exists")
        .0;
    let mut raw = connection(&fixture.path).await;
    let folder = sqlx::query_scalar("INSERT INTO folders(source_key,object_id,name,normalized_name,sort_text) VALUES (?1,'destination-folder','Destination Folder','destination folder','destination folder') RETURNING folder_key")
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
    let all = fixture
        .database
        .playlist_order(
            fixture.source,
            None,
            PlaylistSort::Title,
            false,
            "",
            &cancellation,
        )
        .await
        .expect("all Playlists");
    assert_eq!(all, [alpha, zulu]);
    let scoped = fixture
        .database
        .playlist_order(
            fixture.source,
            Some(folder),
            PlaylistSort::Title,
            false,
            "",
            &cancellation,
        )
        .await
        .expect("folder-scoped Playlists");
    assert_eq!(scoped, [zulu]);
}

#[tokio::test]
async fn playlist_reordering_keeps_one_rank_sequence() {
    let fixture = fixture().await;
    let mut playlists = Vec::new();
    for name in ["One", "Two", "Three", "Four"] {
        playlists.push(
            fixture
                .database
                .create_playlist(Some(fixture.source), name, &[])
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
        fixture
            .database
            .playlist_order(
                fixture.source,
                None,
                PlaylistSort::Position,
                false,
                "",
                &ReadCancellation::new(),
            )
            .await
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
            &[
                fixture.track_uris[0].clone(),
                fixture.track_uris[0].clone(),
                fixture.track_uris[1].clone(),
            ],
        )
        .await
        .expect("create Playlist")
        .expect("all Tracks exist")
        .0;
    let cancel = ReadCancellation::new();
    let initial = fixture
        .database
        .playlist_entry_order(
            playlist,
            None,
            PlaylistEntrySort::Position,
            false,
            "",
            &cancel,
        )
        .await
        .expect("initial Playlist order");
    let rows = fixture
        .database
        .playlist_entry_rows(&initial, &cancel)
        .await
        .expect("Playlist entry rows");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].media_uri, rows[1].media_uri);
    assert_ne!(rows[0].playlist_entry_key, rows[1].playlist_entry_key);
    assert_eq!(
        rows.iter()
            .map(|row| row.media_uri.as_str())
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
            .add_playlist_media(
                Some(fixture.source),
                playlist,
                &[
                    fixture.track_uris[0].clone(),
                    fixture.track_uris[2].clone(),
                    fixture.track_uris[2].clone(),
                ],
                true,
            )
            .await
            .expect("add with duplicate policy"),
        2
    );
    let added = fixture
        .database
        .playlist_entry_order(
            playlist,
            None,
            PlaylistEntrySort::Position,
            false,
            "",
            &cancel,
        )
        .await
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
        .playlist_entry_order(
            playlist,
            None,
            PlaylistEntrySort::Position,
            false,
            "",
            &cancel,
        )
        .await
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
            .add_playlist_media(Some(fixture.source), playlist, &selection, false)
            .await
            .expect("add selected media"),
        520
    );
    assert_eq!(
        fixture
            .database
            .playlist_entry_order(
                playlist,
                None,
                PlaylistEntrySort::Position,
                false,
                "",
                &cancel
            )
            .await
            .expect("complete Playlist order")
            .len(),
        523
    );
    let entries = fixture
        .database
        .playlist_entry_order(
            playlist,
            None,
            PlaylistEntrySort::Position,
            false,
            "",
            &cancel,
        )
        .await
        .unwrap();
    let state = super::support::resolve_queue(
        &fixture.database,
        library::QueueInput::PlaylistEntries {
            order: entries.into(),
            context_id: "playlist".into(),
        },
        Default::default(),
    )
    .await;
    assert!(!state.pending.is_empty());
    assert!(state.occurrences.len() <= 100);
    assert_eq!(state.occurrences[3].media_uri, fixture.track_uris[3]);
    assert_eq!(state.occurrences[95].media_uri, fixture.track_uris[3]);
    assert_ne!(
        state.occurrences[3].occurrence,
        state.occurrences[95].occurrence
    );
}

#[tokio::test]
async fn global_playlist_adds_rufin_media_without_provider_admission() {
    let fixture = fixture().await;
    let (playlist, _) = fixture
        .database
        .create_playlist(None, "Global", &[fixture.track_uris[0].clone()])
        .await
        .expect("create global Playlist")
        .expect("Playlist key");
    assert_eq!(
        fixture
            .database
            .add_playlist_media(
                None,
                playlist,
                &[
                    fixture.track_uris[1].clone(),
                    fixture.track_uris[1].clone(),
                    fixture.track_uris[2].clone(),
                ],
                false,
            )
            .await
            .expect("add global Playlist media"),
        3
    );
    let order = fixture
        .database
        .playlist_media_uri_order(playlist, None, &ReadCancellation::new())
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
    sqlx::query("DELETE FROM tracks WHERE media_uri=?1")
        .bind(&fixture.track_uris[1])
        .execute(&mut raw)
        .await
        .expect("remove catalog row");
    drop(raw);
    let entries = fixture
        .database
        .playlist_entry_order(
            playlist,
            None,
            PlaylistEntrySort::Position,
            false,
            "",
            &ReadCancellation::new(),
        )
        .await
        .expect("unavailable entries");
    let rows = fixture
        .database
        .playlist_entry_rows(&entries, &ReadCancellation::new())
        .await
        .expect("unavailable Playlist rows");
    assert_eq!(rows[1].title, "Beta");
    assert_eq!(rows[1].media_uri, fixture.track_uris[1]);
    let playback = super::support::resolve_queue(
        &fixture.database,
        library::QueueInput::PlaylistEntries {
            order: entries.into(),
            context_id: "unavailable".into(),
        },
        Default::default(),
    )
    .await;
    assert_eq!(playback.occurrences[1].title, "Beta");
    assert_eq!(playback.occurrences[1].media_uri, fixture.track_uris[1]);

    assert_eq!(
        fixture
            .database
            .playlist_owner(playlist, &ReadCancellation::new())
            .await
            .expect("read global Playlist owner"),
        Some((None, None))
    );
    assert!(
        fixture
            .database
            .rename_playlist(None, playlist, "Renamed global")
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
            .playlist_owner(playlist, &ReadCancellation::new())
            .await
            .expect("deleted Playlist owner"),
        None
    );
}

#[tokio::test]
async fn playlist_cover_samples_follow_occurrences_and_effective_bindings() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    let first = vec![1_u8, 2, 3];
    let second = vec![4_u8, 5, 6];
    sqlx::query("UPDATE tracks SET artwork_binding=CASE WHEN album_key=?1 THEN ?3 ELSE ?4 END")
        .bind(fixture.albums[0])
        .bind(fixture.albums[1])
        .bind(&first)
        .bind(&second)
        .execute(&mut raw)
        .await
        .expect("set effective artwork");
    drop(raw);
    let playlist = fixture
        .database
        .create_playlist(
            Some(fixture.source),
            "Positional covers",
            &[
                fixture.track_uris[0].clone(),
                fixture.track_uris[0].clone(),
                fixture.track_uris[2].clone(),
                fixture.track_uris[1].clone(),
                fixture.track_uris[3].clone(),
            ],
        )
        .await
        .expect("create Playlist")
        .expect("all Tracks exist")
        .0;
    let row = fixture
        .database
        .playlist_rows(&[playlist], &ReadCancellation::new())
        .await
        .expect("Playlist cover samples")
        .pop()
        .expect("Playlist row");
    assert_eq!(
        row.representative_artwork,
        [first.clone(), first.clone(), second, first]
    );
}

#[tokio::test]
async fn provider_playlist_payload_resolves_uri_only_at_the_protocol_boundary() {
    let fixture = fixture().await;
    let playlist = fixture
        .database
        .create_playlist(
            Some(fixture.source),
            "Provider list",
            &[fixture.track_uris[0].clone()],
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
            .source_playlist_media_object_ids(
                fixture.source,
                Some(playlist),
                &[
                    fixture.track_uris[0].clone(),
                    fixture.track_uris[1].clone(),
                    fixture.track_uris[1].clone(),
                ],
                true,
                &ReadCancellation::new(),
            )
            .await
            .expect("provider payload"),
        ["track-1", "track-1"]
    );
}

#[tokio::test]
async fn rating_favorite_and_delivery_are_uri_owned() {
    let fixture = fixture().await;
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
    let row = fixture
        .database
        .track_rows(&[fixture.tracks[0]], &ReadCancellation::new())
        .await
        .expect("read Track")
        .remove(0);
    assert_eq!(row.rating, Some(8));
    assert!(row.favorite);
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
            .defer_remote_favorite(&target, false, 200)
            .await
            .expect("defer delivery")
    );
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
            .track_rows(&[fixture.tracks[0]], &ReadCancellation::new())
            .await
            .expect("restored Track")[0]
            .favorite
    );
}
