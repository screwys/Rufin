use library::{FavoriteTarget, PlaylistEntrySort, PlaylistSort, ReadCancellation};

use super::support::{connection, fixture};

#[tokio::test]
async fn playlist_destinations_are_title_ordered_and_folder_scoped() {
    let fixture = fixture().await;
    let zulu = fixture
        .database
        .create_playlist(fixture.source, "Zulu", &[fixture.tracks[0]])
        .await
        .expect("create Zulu Playlist")
        .expect("Track exists");
    let alpha = fixture
        .database
        .create_playlist(fixture.source, "Alpha", &[fixture.tracks[1]])
        .await
        .expect("create Alpha Playlist")
        .expect("Track exists");
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
        .playlist_destinations(fixture.source, None, &cancellation)
        .await
        .expect("all Playlist destinations");
    assert_eq!(
        all.iter()
            .map(|destination| (destination.playlist_key, destination.name.as_str()))
            .collect::<Vec<_>>(),
        [(alpha, "Alpha"), (zulu, "Zulu")]
    );
    let scoped = fixture
        .database
        .playlist_destinations(fixture.source, Some(folder), &cancellation)
        .await
        .expect("folder-scoped Playlist destinations");
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].playlist_key, zulu);
    assert_eq!(scoped[0].name, "Zulu");
}

#[tokio::test]
async fn playlist_reordering_swaps_adjacent_positions_without_collisions() {
    let fixture = fixture().await;
    let mut playlists = Vec::new();
    for name in ["One", "Two", "Three", "Four"] {
        playlists.push(
            fixture
                .database
                .create_playlist(fixture.source, name, &[])
                .await
                .expect("create Playlist")
                .expect("Playlist key"),
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
                &ReadCancellation::new()
            )
            .await
            .expect("Playlist order"),
        [playlists[1], playlists[0], playlists[3], playlists[2]]
    );
}

#[tokio::test]
async fn playlist_edits_preserve_occurrence_identity_order_and_duplicates() {
    let fixture = fixture().await;
    let playlist = fixture
        .database
        .create_playlist(
            fixture.source,
            "Duplicates",
            &[fixture.tracks[0], fixture.tracks[0], fixture.tracks[1]],
        )
        .await
        .expect("create Playlist")
        .expect("all Tracks exist");
    let cancel = ReadCancellation::new();
    let initial = fixture
        .database
        .playlist_entry_order(
            fixture.source,
            playlist,
            None,
            PlaylistEntrySort::Position,
            false,
            "",
            &cancel,
        )
        .await
        .expect("initial Playlist order");
    assert_eq!(initial.len(), 3);
    assert_eq!(
        initial.tracks,
        [fixture.tracks[0], fixture.tracks[0], fixture.tracks[1]]
    );
    assert_eq!(initial.track_positions, [0, 1, 2]);
    assert_eq!(
        fixture
            .database
            .playlist_entry_order(
                fixture.source,
                playlist,
                None,
                PlaylistEntrySort::Position,
                false,
                "alpha",
                &cancel
            )
            .await
            .expect("filtered duplicate occurrences")
            .len(),
        2
    );
    assert_eq!(
        fixture
            .database
            .playlist_entry_order(
                fixture.source,
                playlist,
                None,
                PlaylistEntrySort::Artist,
                false,
                "artist a",
                &cancel
            )
            .await
            .expect("Artist-filtered Playlist entries")
            .len(),
        3
    );
    assert_eq!(
        fixture
            .database
            .playlist_entry_order(
                fixture.source,
                playlist,
                None,
                PlaylistEntrySort::Album,
                false,
                "album a",
                &cancel
            )
            .await
            .expect("Album-filtered Playlist entries")
            .len(),
        3
    );
    assert_eq!(
        fixture
            .database
            .playlist_order(
                fixture.source,
                None,
                PlaylistSort::TrackCount,
                true,
                "",
                &cancel
            )
            .await
            .expect("sorted Playlist order"),
        [playlist]
    );
    let title_order = fixture
        .database
        .playlist_entry_order(
            fixture.source,
            playlist,
            None,
            PlaylistEntrySort::Title,
            false,
            "",
            &cancel,
        )
        .await
        .expect("sorted Playlist entries");
    assert_eq!(title_order.len(), 3);
    let mut raw = connection(&fixture.path).await;
    let scoped_folder = sqlx::query_scalar("INSERT INTO folders(source_key,object_id,name,normalized_name,sort_text) VALUES (?1,'playlist-folder','Playlist Folder','playlist folder','playlist folder') RETURNING folder_key")
        .bind(fixture.source).fetch_one(&mut raw).await.expect("insert Playlist Folder scope");
    sqlx::query("INSERT INTO track_folders(track_key,folder_key,position) VALUES (?1,?2,1)")
        .bind(fixture.tracks[0])
        .bind(scoped_folder)
        .execute(&mut raw)
        .await
        .expect("scope one Playlist Track");
    let scoped_row = fixture
        .database
        .playlist_rows(fixture.source, &[playlist], Some(scoped_folder), &cancel)
        .await
        .expect("scoped Playlist facts")
        .pop()
        .unwrap();
    assert_eq!(scoped_row.track_count, 2);
    assert_eq!(scoped_row.duration_millis, 360_000);
    assert_eq!(
        fixture
            .database
            .playlist_order(
                fixture.source,
                Some(scoped_folder),
                PlaylistSort::TrackCount,
                true,
                "",
                &cancel
            )
            .await
            .expect("scoped Playlist count order"),
        [playlist]
    );
    let plan = sqlx::query_as::<_, (i64, i64, i64, String)>(
        "EXPLAIN QUERY PLAN
         SELECT playlist_entry_key FROM playlist_entries
         WHERE playlist_key=?1 ORDER BY position",
    )
    .bind(playlist)
    .fetch_one(&mut raw)
    .await
    .expect("Playlist entry order plan")
    .3;
    assert!(plan.contains("playlist_entries_order_idx"), "{plan}");
    let playlist_plan = sqlx::query_as::<_, (i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT playlist_key FROM playlists WHERE source_key=?1 ORDER BY sort_text,playlist_key")
        .bind(fixture.source).fetch_all(&mut raw).await.expect("production Playlist title plan").into_iter().map(|row| row.3).collect::<Vec<_>>().join(" | ");
    assert!(
        playlist_plan.contains("playlists_title_idx"),
        "{playlist_plan}"
    );
    assert!(
        !playlist_plan.contains("USE TEMP B-TREE FOR ORDER BY"),
        "{playlist_plan}"
    );
    let rows = fixture
        .database
        .playlist_entry_rows(fixture.source, &initial, None, &cancel)
        .await
        .expect("Playlist entry rows");
    assert_eq!(rows[0].track_key, rows[1].track_key);
    assert_ne!(rows[0].playlist_entry_key, rows[1].playlist_entry_key);
    assert_eq!(
        rows[0].track.as_ref().expect("final Track row").track_key,
        rows[0].track_key.unwrap()
    );
    assert!(!rows[0].track.as_ref().unwrap().artists.is_empty());

    assert_eq!(
        fixture
            .database
            .add_playlist_tracks(
                fixture.source,
                playlist,
                &[fixture.tracks[0], fixture.tracks[2], fixture.tracks[2]],
                true,
            )
            .await
            .expect("add with duplicate policy"),
        2
    );
    let added = fixture
        .database
        .playlist_entry_order(
            fixture.source,
            playlist,
            None,
            PlaylistEntrySort::Position,
            false,
            "",
            &cancel,
        )
        .await
        .expect("added Playlist order");
    assert_eq!(added.len(), 5);
    assert!(
        fixture
            .database
            .move_playlist_entry(fixture.source, playlist, added[4], 0)
            .await
            .expect("move Playlist entry")
    );
    let moved = fixture
        .database
        .playlist_entry_order(
            fixture.source,
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
            .remove_playlist_entries(fixture.source, playlist, &[moved[1], moved[2]])
            .await
            .expect("remove Playlist entries"),
        2
    );
    let remaining = fixture
        .database
        .playlist_entry_order(
            fixture.source,
            playlist,
            None,
            PlaylistEntrySort::Position,
            false,
            "",
            &cancel,
        )
        .await
        .expect("remaining Playlist order");
    assert_eq!(remaining.len(), 3);

    let selection = vec![fixture.tracks[3]; 520];
    assert_eq!(
        fixture
            .database
            .add_playlist_tracks(fixture.source, playlist, &selection, false)
            .await
            .expect("add complete selected Tracks"),
        selection.len()
    );
    let complete = fixture
        .database
        .playlist_entry_order(
            fixture.source,
            playlist,
            None,
            PlaylistEntrySort::Position,
            false,
            "",
            &cancel,
        )
        .await
        .expect("non-truncated Playlist membership");
    assert_eq!(complete.len(), 523);
    assert_eq!(
        fixture
            .database
            .remove_playlist_entries(fixture.source, playlist, &complete[..501])
            .await
            .expect("remove complete selected occurrences"),
        501
    );
}

#[tokio::test]
async fn playlist_cover_samples_first_four_occurrences_without_deduplication() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    let first = vec![1_u8, 2, 3];
    let second = vec![4_u8, 5, 6];
    for (album, binding) in [
        (fixture.albums[0], first.clone()),
        (fixture.albums[1], second.clone()),
    ] {
        sqlx::query("UPDATE albums SET artwork_binding=?2 WHERE album_key=?1")
            .bind(album)
            .bind(binding)
            .execute(&mut raw)
            .await
            .expect("set Album artwork");
    }
    drop(raw);

    let playlist = fixture
        .database
        .create_playlist(
            fixture.source,
            "Positional covers",
            &[
                fixture.tracks[0],
                fixture.tracks[0],
                fixture.tracks[2],
                fixture.tracks[1],
                fixture.tracks[3],
            ],
        )
        .await
        .expect("create Playlist")
        .expect("all Tracks exist");
    let row = fixture
        .database
        .playlist_rows(fixture.source, &[playlist], None, &ReadCancellation::new())
        .await
        .expect("read Playlist cover samples")
        .pop()
        .expect("Playlist row");

    assert_eq!(
        row.representative_artwork,
        [first.clone(), first.clone(), second, first]
    );
}

#[tokio::test]
async fn source_playlist_payload_queries_are_bounded_and_preserve_requested_order() {
    let fixture = fixture().await;
    let playlist = fixture
        .database
        .create_playlist(fixture.source, "Provider list", &[fixture.tracks[0]])
        .await
        .expect("seed source Playlist")
        .unwrap();
    let mut raw = connection(&fixture.path).await;
    sqlx::query(
        "UPDATE playlists SET ownership='source',object_id='provider:playlist' WHERE playlist_key=?1",
    )
    .bind(playlist)
    .execute(&mut raw)
    .await
    .expect("make Playlist source-owned");
    drop(raw);

    assert_eq!(
        fixture
            .database
            .source_playlist_object_id(fixture.source, playlist, &ReadCancellation::new())
            .await
            .expect("source Playlist identity")
            .as_deref(),
        Some("provider:playlist")
    );
    assert_eq!(
        fixture
            .database
            .source_playlist_track_object_ids(
                fixture.source,
                Some(playlist),
                &[fixture.tracks[0], fixture.tracks[1], fixture.tracks[1]],
                true,
                &ReadCancellation::new(),
            )
            .await
            .expect("source Playlist Track payload"),
        ["track-1", "track-1"]
    );
    let entry = fixture
        .database
        .playlist_entry_order(
            fixture.source,
            playlist,
            None,
            PlaylistEntrySort::Position,
            false,
            "",
            &ReadCancellation::new(),
        )
        .await
        .expect("source Playlist entries")[0];
    let entry_object = fixture
        .database
        .source_playlist_entry_object_ids(
            fixture.source,
            playlist,
            &[entry],
            &ReadCancellation::new(),
        )
        .await
        .expect("source Playlist entry payload");
    assert_eq!(entry_object.len(), 1);
    assert!(entry_object[0].starts_with("rufin:entry:"));
    let selected = vec![fixture.tracks[2]; 300];
    let mut resolved = 0;
    for page in selected.chunks(256) {
        resolved += fixture
            .database
            .source_playlist_track_object_ids(
                fixture.source,
                None,
                page,
                false,
                &ReadCancellation::new(),
            )
            .await
            .expect("paged source Playlist payload")
            .len();
    }
    assert_eq!(resolved, 300);
}

#[tokio::test]
async fn rating_favorite_and_delivery_writes_keep_accepted_semantics() {
    let fixture = fixture().await;
    let track = fixture.tracks[0];
    assert!(
        fixture
            .database
            .set_rating(fixture.source, FavoriteTarget::Track(track), Some(8))
            .await
            .expect("set Track Rating")
    );
    assert!(
        fixture
            .database
            .set_rating(fixture.source, FavoriteTarget::Track(track), Some(0))
            .await
            .expect("clear Rating explicitly")
    );
    let cancel = ReadCancellation::new();
    assert_eq!(
        fixture
            .database
            .track_rows(fixture.source, &[track], &cancel)
            .await
            .expect("read zero Rating")[0]
            .rating,
        Some(0)
    );
    assert!(
        fixture
            .database
            .set_rating(fixture.source, FavoriteTarget::Track(track), None)
            .await
            .expect("inherit source Rating")
    );
    assert_eq!(
        fixture
            .database
            .track_rows(fixture.source, &[track], &cancel)
            .await
            .expect("read inherited Rating")[0]
            .rating,
        Some(5)
    );
    assert!(
        fixture
            .database
            .set_rating(fixture.source, FavoriteTarget::Track(track), Some(8))
            .await
            .expect("set Rating again")
    );
    assert!(
        fixture
            .database
            .set_favorite(fixture.source, FavoriteTarget::Track(track), true)
            .await
            .expect("set Track Favorite")
    );
    let row = fixture
        .database
        .track_rows(fixture.source, &[track], &cancel)
        .await
        .expect("read accepted Track")
        .remove(0);
    assert_eq!(row.rating, Some(8));
    assert!(row.favorite);

    let target = FavoriteTarget::Track(track);
    assert!(
        fixture
            .database
            .queue_remote_favorite(fixture.source, target, false, 100)
            .await
            .expect("queue remote Favorite")
    );
    assert!(
        fixture
            .database
            .defer_remote_favorite(fixture.source, target, false, 200)
            .await
            .expect("defer Favorite")
    );
    assert_eq!(
        fixture
            .database
            .reject_remote_favorite(fixture.source, target, false)
            .await
            .expect("reject Favorite"),
        Some(true)
    );
    let restored = fixture
        .database
        .track_rows(fixture.source, &[track], &cancel)
        .await
        .expect("read restored Track")
        .remove(0);
    assert!(restored.favorite);

    assert!(
        fixture
            .database
            .queue_remote_favorite(fixture.source, target, false, 300)
            .await
            .expect("queue old Favorite")
    );
    assert!(
        fixture
            .database
            .queue_remote_favorite(fixture.source, target, true, 301)
            .await
            .expect("replace with newer Favorite")
    );
    assert!(
        !fixture
            .database
            .acknowledge_remote_favorite(fixture.source, target, false)
            .await
            .expect("ignore stale acknowledgement")
    );
    assert!(
        fixture
            .database
            .track_rows(fixture.source, &[track], &cancel)
            .await
            .expect("read after stale acknowledgement")[0]
            .favorite
    );
}
