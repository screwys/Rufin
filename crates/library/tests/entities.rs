use library::{
    AlbumMetadataWrite, AlbumReleaseResult, AlbumSort, ArtistMetadataWrite, ArtistSort,
    FavoriteTarget, GenreSort, MoodSort, ReadCancellation, RouteSeedWindow, SearchRequest,
    TrackMetadataWrite, TrackSort,
};

use super::support::{connection, fixture};

#[tokio::test]
async fn track_sorts_keep_nulls_last_and_title_ties_in_both_directions() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET year=CASE object_id WHEN 'track-0' THEN NULL WHEN 'track-1' THEN 0 ELSE 2000 END")
        .execute(&mut raw).await.unwrap();
    sqlx::query("DELETE FROM track_genres WHERE track_key=?1")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .unwrap();
    let empty_genre = sqlx::query_scalar::<_, library::GenreKey>(
        "INSERT INTO genres(source_key,object_id,name,normalized_name,sort_text) VALUES(?1,'empty','','','') RETURNING genre_key",
    ).bind(fixture.source).fetch_one(&mut raw).await.unwrap();
    sqlx::query("UPDATE track_genres SET genre_key=?1 WHERE track_key=?2")
        .bind(empty_genre)
        .bind(fixture.tracks[1])
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    for sort in [TrackSort::Year, TrackSort::Genre] {
        for (descending, expected) in [(false, [1, 3, 2, 0]), (true, [3, 2, 1, 0])] {
            let page = fixture
                .database
                .track_route_page(
                    fixture.source,
                    None,
                    false,
                    "",
                    sort,
                    descending,
                    RouteSeedWindow::top(),
                    &ReadCancellation::new(),
                )
                .await
                .unwrap();
            assert_eq!(
                page.order,
                expected.map(|i| fixture.track_uris[i].clone()),
                "{sort:?}, descending={descending}"
            );
        }
    }
}

#[tokio::test]
async fn repeated_album_cards_keep_scoped_counts_and_legacy_activity_precedence() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::raw_sql("DELETE FROM catalog.activity_baseline;
      INSERT INTO main.legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at)
      VALUES('source','lifetime','track','track-0',7,0,100),('source','lifetime','track','track-1',99,0,900);
      INSERT INTO catalog.activity_baseline(source_key,period,item_kind,track_object_id,play_count,skip_count,last_played_at)
      SELECT source_key,'lifetime','track','track-1',3,0,200 FROM sources;")
        .execute(&mut raw).await.unwrap();
    let cancel = ReadCancellation::new();
    let keys = [fixture.albums[0], fixture.albums[0]];
    let rows = fixture
        .database
        .album_rows(fixture.source, &keys, None, &cancel)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.track_count, 2);
        assert_eq!(row.play_count, 10);
        assert_eq!(row.last_played, Some(200));
        assert_eq!(row.album_artists.len(), 1);
    }
    sqlx::query("DELETE FROM track_folders WHERE track_key=?1")
        .bind(fixture.tracks[1])
        .execute(&mut raw)
        .await
        .unwrap();
    let rows = fixture
        .database
        .album_rows(fixture.source, &keys, Some(fixture.folder), &cancel)
        .await
        .unwrap();
    for row in &rows {
        assert_eq!(row.track_count, 1);
        assert_eq!(row.play_count, 7);
        assert_eq!(row.last_played, Some(100));
    }
    let artist = fixture
        .database
        .artist_rows(
            fixture.source,
            &[fixture.artists[0]],
            false,
            Some(fixture.folder),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(artist[0].play_count, 7);
    assert_eq!(artist[0].last_played, Some(100));
}

#[tokio::test]
async fn named_card_batches_preserve_requested_rows_and_scoped_cover_order() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    for (index, album) in fixture.albums.iter().enumerate() {
        sqlx::query("UPDATE albums SET artwork_binding=?1 WHERE album_key=?2")
            .bind(vec![index as u8 + 1])
            .bind(album)
            .execute(&mut raw)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE tracks SET date_added=CASE WHEN album_key=?1 THEN '2026-09-08' END")
        .bind(fixture.albums[1])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_folders WHERE track_key IN (SELECT track_key FROM tracks WHERE album_key=?1)")
        .bind(fixture.albums[1]).execute(&mut raw).await.unwrap();
    drop(raw);
    let cancel = ReadCancellation::new();
    for (folder, covers, count) in [
        (None, vec![vec![2], vec![1]], 4),
        (Some(fixture.folder), vec![vec![1]], 2),
    ] {
        let genres = fixture
            .database
            .genre_rows(
                fixture.source,
                &[
                    fixture.genre,
                    library::GenreKey::from_raw(-100),
                    fixture.genre,
                ],
                folder,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(genres.len(), 2);
        assert_eq!(genres[0], genres[1]);
        assert_eq!(genres[0].track_count, count);
        assert_eq!(genres[0].representative_artwork, covers[..1]);
        let single = fixture
            .database
            .genre_rows(fixture.source, &[fixture.genre], folder, &cancel)
            .await
            .unwrap();
        assert_eq!(genres[0].duration_millis, single[0].duration_millis);
        let moods = fixture
            .database
            .mood_rows(
                fixture.source,
                &[fixture.mood, fixture.mood],
                folder,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(moods.len(), 2);
        assert_eq!(moods[0], moods[1]);
        assert_eq!(moods[0].track_count, count);
        assert_eq!(moods[0].representative_artwork, covers);
        let single = fixture
            .database
            .mood_rows(fixture.source, &[fixture.mood], folder, &cancel)
            .await
            .unwrap();
        assert_eq!(moods[0].duration_millis, single[0].duration_millis);
    }
}

#[tokio::test]
async fn named_collection_lookups_retain_identity_when_selected_folder_is_empty() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("DELETE FROM track_folders")
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    let folder = Some(fixture.folder);
    let genre = fixture
        .database
        .genre_rows(fixture.source, &[fixture.genre], folder, &cancel)
        .await
        .unwrap()
        .pop()
        .expect("existing genre");
    assert_eq!(
        (
            genre.track_count,
            genre.album_count,
            genre.duration_millis,
            genre.downloaded_count
        ),
        (0, 0, 0, 0)
    );
    assert!(genre.representative_artwork.is_empty());
    let mood = fixture
        .database
        .mood_rows(fixture.source, &[fixture.mood], folder, &cancel)
        .await
        .unwrap()
        .pop()
        .expect("existing mood");
    assert_eq!(
        (
            mood.track_count,
            mood.duration_millis,
            mood.downloaded_count
        ),
        (0, 0, 0)
    );
    assert!(mood.representative_artwork.is_empty());
    let genre = fixture
        .database
        .genre_detail(fixture.source, fixture.genre, folder, &cancel)
        .await
        .unwrap()
        .expect("existing genre detail");
    assert_eq!(
        (
            genre.genre.track_count,
            genre.genre.album_count,
            genre.genre.duration_millis
        ),
        (0, 0, 0)
    );
    assert!(genre.representative_albums.is_empty());
    let mood = fixture
        .database
        .mood_detail(fixture.source, fixture.mood, folder, &cancel)
        .await
        .unwrap()
        .expect("existing mood detail");
    assert_eq!((mood.mood.track_count, mood.mood.duration_millis), (0, 0));
    assert!(mood.representative_albums.is_empty());
    for sort in [GenreSort::Title, GenreSort::TrackCount] {
        let (order, _, _) = fixture
            .database
            .genre_route_page(
                fixture.source,
                folder,
                "",
                sort,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .unwrap();
        assert!(order.is_empty());
    }
    for sort in [MoodSort::Title, MoodSort::TrackCount] {
        let (order, _, _) = fixture
            .database
            .mood_route_page(
                fixture.source,
                folder,
                "",
                sort,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .unwrap();
        assert!(order.is_empty());
    }
}

#[tokio::test]
async fn detail_folder_bounds_follow_collection_roles_and_selected_folder() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    let paths = [
        "Artist/Album/Disc 1/a.flac",
        "Artist/Album/Disc 2/z.flac",
        "Other/Album/c.flac",
        "Other/Album/d.flac",
    ]
    .map(|path| {
        fixture
            ._directory
            .path()
            .join(path)
            .to_str()
            .unwrap()
            .to_owned()
    });
    for (track, path) in fixture.tracks.iter().zip(&paths) {
        sqlx::query("UPDATE tracks SET source_path=?1 WHERE track_key=?2")
            .bind(path)
            .bind(track)
            .execute(&mut raw)
            .await
            .unwrap();
    }
    // A guest appearance belongs to the artist view, not their album-artist view.
    sqlx::query("INSERT INTO track_artists(track_key,artist_key,position) VALUES(?1,?2,1)")
        .bind(fixture.tracks[2])
        .bind(fixture.artists[0])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_folders WHERE track_key<>?1")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    let database = &fixture.database;
    assert_eq!(
        database
            .collection_source_path_bounds(&fixture.album_uris[0], false, None)
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[1].clone()))
    );
    assert_eq!(
        database
            .collection_source_path_bounds(&fixture.artist_uris[0], false, None)
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[2].clone()))
    );
    assert_eq!(
        database
            .collection_source_path_bounds(&fixture.artist_uris[0], true, None)
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[1].clone()))
    );
    assert_eq!(
        database
            .collection_source_path_bounds(&fixture.artist_uris[0], false, Some(fixture.folder))
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[0].clone()))
    );
    assert_eq!(
        database
            .collection_source_path_bounds(&fixture.album_uris[1], false, Some(fixture.folder))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn collection_play_retains_full_order_with_bounded_queue_projection() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    for position in 0..130 {
        let id = format!("extra-{position:03}");
        let uri = library::source_entity_uri(&library::SourceId::new("source"), "track", &id);
        let key: library::TrackKey = sqlx::query_scalar(
            "INSERT INTO tracks(source_key,object_id,album_key,title,normalized_search,display_album,display_artist,sort_text,duration_millis,disc_number,track_number,media_uri) VALUES(?1,?2,?3,?2,?2,'Album A','Artist A',?2,1000,1,?4,?5) RETURNING track_key",
        ).bind(fixture.source).bind(&id).bind(fixture.albums[0]).bind(position + 10).bind(uri)
            .fetch_one(&mut raw).await.expect("extend collection");
        sqlx::query("INSERT INTO track_artists VALUES(?1,?2,0)")
            .bind(key)
            .bind(fixture.artists[0])
            .execute(&mut raw)
            .await
            .unwrap();
        sqlx::query("INSERT INTO track_genres VALUES(?1,?2,0)")
            .bind(key)
            .bind(fixture.genre)
            .execute(&mut raw)
            .await
            .unwrap();
        sqlx::query("INSERT INTO track_moods VALUES(?1,?2,0)")
            .bind(key)
            .bind(fixture.mood)
            .execute(&mut raw)
            .await
            .unwrap();
    }
    drop(raw);
    let cancel = ReadCancellation::new();
    let pages = [
        fixture
            .database
            .album_track_route_page(
                fixture.source,
                fixture.albums[0],
                None,
                "",
                TrackSort::TrackNumber,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .unwrap(),
        fixture
            .database
            .artist_track_route_page(
                fixture.source,
                fixture.artists[0],
                false,
                None,
                "",
                TrackSort::Title,
                false,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .unwrap(),
        fixture
            .database
            .artist_track_route_page(
                fixture.source,
                fixture.artists[0],
                true,
                None,
                "",
                TrackSort::Title,
                false,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .unwrap(),
        fixture
            .database
            .genre_track_route_page(
                fixture.source,
                fixture.genre,
                None,
                "",
                TrackSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .unwrap(),
        fixture
            .database
            .mood_track_route_page(
                fixture.source,
                fixture.mood,
                None,
                "",
                TrackSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .unwrap(),
    ];
    assert!(
        pages[0]
            .first_rows
            .windows(2)
            .all(|pair| pair[0].track_number < pair[1].track_number)
    );
    assert!(
        pages[1]
            .first_rows
            .windows(2)
            .all(|pair| pair[0].title.to_lowercase() <= pair[1].title.to_lowercase())
    );
    let collections = [
        library::QueueCollection::AlbumKey(fixture.albums[0]),
        library::QueueCollection::ArtistKey {
            key: fixture.artists[0],
            album_artist: false,
        },
        library::QueueCollection::ArtistKey {
            key: fixture.artists[0],
            album_artist: true,
        },
        library::QueueCollection::Genre(fixture.genre),
        library::QueueCollection::Mood(fixture.mood),
    ];
    for (page, collection) in pages.into_iter().zip(collections) {
        let total = page.order.len();
        assert!(total > 100);
        assert!(
            page.first_rows
                .iter()
                .all(|row| row.source_key == fixture.source)
        );
        assert_eq!(
            fixture
                .database
                .collection_source(&collection, &cancel)
                .await
                .unwrap(),
            Some(fixture.source)
        );
        assert_eq!(
            fixture
                .database
                .collection_media_uri_order(&collection, &cancel)
                .await
                .unwrap(),
            page.order
        );
        let state = super::support::resolve_queue(
            &fixture.database,
            library::QueueInput::Collection {
                collection,
                folder: None,
                context_id: "collection".into(),
            },
            Default::default(),
        )
        .await;
        assert_eq!(state.entries.len(), total);
        assert_eq!(
            state
                .entries
                .iter()
                .map(|entry| entry.media_uri.as_ref())
                .collect::<Vec<_>>(),
            page.order.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert!(state.occurrences.len() <= 100);
    }
    assert_eq!(
        fixture
            .database
            .album_track_route_page(
                fixture.source,
                fixture.albums[0],
                Some(fixture.folder),
                "",
                TrackSort::TrackNumber,
                false,
                RouteSeedWindow::top(),
                &cancel
            )
            .await
            .unwrap()
            .order
            .len(),
        2
    );
    assert!(
        fixture
            .database
            .album_track_route_page(
                library::SourceKey::from_raw(999),
                fixture.albums[0],
                None,
                "",
                TrackSort::TrackNumber,
                false,
                RouteSeedWindow::top(),
                &cancel
            )
            .await
            .unwrap()
            .order
            .is_empty()
    );
}

#[tokio::test]
async fn favorite_collection_orders_keep_each_favorite_entity_extent() {
    let fixture = fixture().await;
    let cancellation = ReadCancellation::new();
    fixture
        .database
        .set_favorite(&FavoriteTarget::Album(fixture.album_uris[0].clone()), true)
        .await
        .expect("favorite Album");
    fixture
        .database
        .set_favorite(
            &FavoriteTarget::Artist(fixture.artist_uris[1].clone()),
            true,
        )
        .await
        .expect("favorite Artist");

    assert_eq!(
        fixture
            .database
            .album_route_page(
                fixture.source,
                None,
                true,
                "",
                AlbumSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancellation,
            )
            .await
            .expect("favorite Album order")
            .0,
        [fixture.albums[0]]
    );
    assert_eq!(
        fixture
            .database
            .artist_route_page(
                fixture.source,
                None,
                false,
                true,
                "",
                ArtistSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancellation,
            )
            .await
            .expect("favorite Artist order")
            .0,
        [fixture.artists[1]]
    );

    let (order, albums, albums_with_genres) = fixture
        .database
        .album_detail_route_order(
            fixture.source,
            None,
            true,
            "",
            AlbumSort::Title,
            false,
            &cancellation,
        )
        .await
        .expect("favorite Album detail order");

    assert_eq!(albums_with_genres, [fixture.album_uris[0].clone()]);
    assert_eq!(albums, [fixture.album_uris[0].clone()]);
    assert_eq!(
        order
            .iter()
            .filter(|media_uri| {
                library::source_entity_parts(media_uri).is_some_and(|(_, kind, _)| kind == "album")
            })
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [fixture.album_uris[0].as_str()]
    );
    assert_eq!(
        order
            .iter()
            .filter(|media_uri| {
                library::source_entity_parts(media_uri).is_some_and(|(_, kind, _)| kind == "track")
            })
            .map(String::as_str)
            .collect::<Vec<_>>(),
        fixture.track_uris[..2]
    );
}

#[tokio::test]
async fn tracks_and_collections_keep_complete_orders_and_bounded_rows() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let title = fixture
        .database
        .track_order(
            fixture.source,
            None,
            false,
            TrackSort::Title,
            false,
            &cancel,
        )
        .await
        .expect("title Track order");
    assert_eq!(title.len(), 4);
    let numbered = fixture
        .database
        .track_order(
            fixture.source,
            None,
            false,
            TrackSort::TrackNumber,
            true,
            &cancel,
        )
        .await
        .expect("numbered Track order");
    assert_eq!(
        numbered,
        fixture.track_uris.iter().cloned().rev().collect::<Vec<_>>()
    );
    let favorites = fixture
        .database
        .track_order(fixture.source, None, true, TrackSort::Title, false, &cancel)
        .await
        .expect("favorite Track order");
    assert_eq!(favorites.len(), 1);
    let folder = fixture
        .database
        .track_order(
            fixture.source,
            Some(fixture.folder),
            false,
            TrackSort::Title,
            false,
            &cancel,
        )
        .await
        .expect("Folder Track order");
    assert_eq!(folder.len(), 4);

    let supplied = [title[2].clone(), title[0].clone()];
    let rows = fixture
        .database
        .track_rows_by_uri(&supplied, &cancel)
        .await
        .expect("bounded Track rows");
    assert_eq!(
        rows.iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>(),
        supplied
    );
    assert_eq!(rows[0].artists[0].name, rows[0].artist);
    assert_eq!(rows[0].album_artists[0].name, rows[0].artist);
    assert_eq!(rows[0].genres[0].name, "Rock");
    assert_eq!(rows[0].rating, Some(8));
    assert_eq!(rows[0].play_count, 0);
    assert_eq!(rows[0].skip_count, 0);
    assert_eq!(rows[0].last_played, None);
    assert!(
        fixture
            .database
            .track_rows_by_uri(&vec![title[0].clone(); 257], &cancel)
            .await
            .is_err(),
        "Track row bound was not enforced"
    );
    assert_eq!(
        fixture
            .database
            .album_route_page(
                fixture.source,
                None,
                false,
                "",
                AlbumSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel
            )
            .await
            .expect("Album order")
            .0,
        fixture.albums
    );
    assert_eq!(
        fixture
            .database
            .album_route_page(
                fixture.source,
                None,
                false,
                "",
                AlbumSort::DateAdded,
                true,
                RouteSeedWindow::top(),
                &cancel
            )
            .await
            .expect("sorted Album order")
            .0,
        fixture.albums
    );
    assert_eq!(
        fixture
            .database
            .artist_route_page(
                fixture.source,
                None,
                true,
                false,
                "",
                ArtistSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel
            )
            .await
            .expect("Album Artist order")
            .0,
        fixture.artists
    );
    assert_eq!(
        fixture
            .database
            .artist_route_page(
                fixture.source,
                None,
                false,
                false,
                "",
                ArtistSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel
            )
            .await
            .expect("sorted Artist order")
            .0,
        fixture.artists
    );
    assert_eq!(
        fixture
            .database
            .genre_route_page(
                fixture.source,
                None,
                "",
                GenreSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .expect("Genre order")
            .0,
        [fixture.genre]
    );
    assert_eq!(
        fixture
            .database
            .genre_route_page(
                fixture.source,
                None,
                "",
                GenreSort::TrackCount,
                true,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .expect("sorted Genre order")
            .0,
        [fixture.genre]
    );
    assert_eq!(
        fixture
            .database
            .mood_route_page(
                fixture.source,
                None,
                "",
                MoodSort::Duration,
                true,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .expect("sorted Mood order")
            .0,
        [fixture.mood]
    );
    let album_rows = fixture
        .database
        .album_rows(fixture.source, &fixture.albums, None, &cancel)
        .await
        .expect("bounded Album rows");
    assert_eq!(album_rows[0].track_count, 2);
    assert_eq!(album_rows[0].album_artists[0].name, "Artist A");
    assert_eq!(album_rows[0].genres[0].name, "Rock");
    let album_detail = fixture
        .database
        .album_detail(
            &album_rows[0].media_uri,
            library::TrackSort::TrackNumber,
            false,
            &cancel,
        )
        .await
        .expect("Album detail")
        .expect("existing Album");
    assert_eq!(album_detail.track_order.len(), 2);
    let reversed = fixture
        .database
        .album_detail(
            &album_rows[0].media_uri,
            library::TrackSort::TrackNumber,
            true,
            &cancel,
        )
        .await
        .expect("descending Album detail")
        .expect("existing Album");
    assert_eq!(
        reversed.track_order,
        album_detail
            .track_order
            .iter()
            .rev()
            .cloned()
            .collect::<Vec<_>>()
    );
    assert_eq!(album_detail.artists, [fixture.artists[0]]);
    assert_eq!(album_rows[0].is_compilation, Some(true));
    assert_eq!(album_rows[0].release_types, ["Album".to_string()]);
    let candidates = fixture
        .database
        .album_release_candidates(fixture.source, 100, &cancel)
        .await
        .expect("Album release candidates");
    assert_eq!(candidates.len(), 2);
    let first = candidates
        .iter()
        .find(|candidate| candidate.album_key == fixture.albums[0])
        .unwrap();
    assert_eq!(first.lookup_identity, "release-group:group-a");
    let mut release_raw = connection(&fixture.path).await;
    let revision =
        sqlx::query_scalar::<_, i64>("SELECT catalog_revision FROM sources WHERE source_key=?1")
            .bind(fixture.source)
            .fetch_one(&mut release_raw)
            .await
            .unwrap();
    assert_eq!(
        fixture
            .database
            .accept_album_release_result(
                fixture.source,
                first.album_key,
                &first.lookup_identity,
                AlbumReleaseResult::Missing
            )
            .await
            .expect("accept missing Album release"),
        Some(first.album_key)
    );
    let missing_row = fixture
        .database
        .album_rows(fixture.source, &[first.album_key], None, &cancel)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(missing_row.release_types, ["Album".to_string()]);
    assert_eq!(
        missing_row.release_lookup_identity.as_deref(),
        Some("release-group:group-a")
    );
    assert!(matches!(
        fixture
            .database
            .update_album_metadata(
                fixture.source,
                first.album_key,
                AlbumMetadataWrite {
                    title: "Album A".to_string(),
                    normalized_title: "album a".to_string(),
                    display_artist: "Artist A".to_string(),
                    sort_text: "album a".to_string(),
                    year: Some(2024),
                    release_date: Some("2024-01-02".to_string()),
                    date_added: Some("2024-01-02".to_string()),
                    musicbrainz_release_id: None,
                    musicbrainz_release_group_id: Some("group-a2".to_string()),
                    is_compilation: Some(true)
                }
            )
            .await
            .unwrap(),
        library::ScanOutcome::Changed(_)
    ));
    let changed = fixture
        .database
        .album_release_candidates(fixture.source, 100, &cancel)
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.album_key == first.album_key)
        .unwrap();
    assert_eq!(changed.lookup_identity, "release-group:group-a2");
    assert_eq!(
        fixture
            .database
            .accept_album_release_result(
                fixture.source,
                first.album_key,
                &first.lookup_identity,
                AlbumReleaseResult::Found {
                    release_types: vec!["Stale".to_string()]
                }
            )
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        fixture
            .database
            .accept_album_release_result(
                fixture.source,
                changed.album_key,
                &changed.lookup_identity,
                AlbumReleaseResult::Found {
                    release_types: vec!["Compilation".to_string(), "Remix".to_string()]
                }
            )
            .await
            .unwrap(),
        Some(changed.album_key)
    );
    let found = fixture
        .database
        .album_rows(fixture.source, &[changed.album_key], None, &cancel)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        found.release_types,
        ["Compilation".to_string(), "Remix".to_string()]
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT catalog_revision FROM sources WHERE source_key=?1")
            .bind(fixture.source)
            .fetch_one(&mut release_raw)
            .await
            .unwrap(),
        revision + 1 // The metadata edit publishes once; release annotations do not republish it.
    );

    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE track_artists SET artist_key=?3 WHERE track_key IN (?1,?2)")
        .bind(fixture.tracks[0])
        .bind(fixture.tracks[1])
        .bind(fixture.artists[1])
        .execute(&mut raw)
        .await
        .expect("separate direct Track Artist from Album Artist");
    let album_artist_uri: String =
        sqlx::query_scalar("SELECT media_uri FROM artists WHERE artist_key=?1")
            .bind(fixture.artists[0])
            .fetch_one(&mut raw)
            .await
            .unwrap();
    let resolved = fixture
        .database
        .artist_row_by_media_uri(&album_artist_uri, &cancel)
        .await
        .expect("resolve an artist credited only on albums")
        .unwrap();
    assert_eq!(resolved.artist_key, fixture.artists[0]);
    assert_eq!(resolved.source_key, fixture.source);
    let detail = fixture
        .database
        .artist_detail(fixture.source, resolved.artist_key, false, None, &cancel)
        .await
        .expect("known artist can open with no tracks in the requested role")
        .unwrap();
    assert_eq!(detail.artist.artist_key, resolved.artist_key);
    assert_eq!(detail.artist.track_count, 0);
    assert_eq!(
        fixture
            .database
            .artist_track_route_page(
                fixture.source,
                fixture.artists[0],
                true,
                None,
                "",
                TrackSort::Title,
                false,
                false,
                RouteSeedWindow::top(),
                &cancel
            )
            .await
            .expect("Album Artist Tracks")
            .order
            .len(),
        2
    );
    assert_eq!(
        fixture
            .database
            .artist_track_route_page(
                fixture.source,
                fixture.artists[0],
                false,
                None,
                "",
                TrackSort::Title,
                false,
                false,
                RouteSeedWindow::top(),
                &cancel
            )
            .await
            .expect("direct Artist Tracks")
            .order
            .len(),
        0
    );
    assert_eq!(
        fixture
            .database
            .artist_rows(fixture.source, &[fixture.artists[0]], true, None, &cancel)
            .await
            .expect("Album Artist row")[0]
            .track_count,
        2
    );
    assert_eq!(
        fixture
            .database
            .artist_rows(fixture.source, &[fixture.artists[0]], false, None, &cancel)
            .await
            .expect("known artist without direct track credits")[0]
            .track_count,
        0
    );
    assert_eq!(
        fixture
            .database
            .artist_detail(fixture.source, fixture.artists[0], true, None, &cancel)
            .await
            .expect("Album Artist detail")
            .unwrap()
            .artist
            .track_count,
        2
    );
    assert_eq!(
        fixture
            .database
            .genre_detail(fixture.source, fixture.genre, Some(fixture.folder), &cancel)
            .await
            .expect("scoped Genre detail")
            .unwrap()
            .genre
            .track_count,
        4
    );
    assert_eq!(
        fixture
            .database
            .album_track_route_page(
                fixture.source,
                fixture.albums[0],
                Some(fixture.folder),
                "beta",
                TrackSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel
            )
            .await
            .expect("scoped filtered Album Tracks")
            .order,
        [fixture.track_uris[1].clone()]
    );
    assert_eq!(
        fixture
            .database
            .album_track_route_page(
                fixture.source,
                fixture.albums[0],
                None,
                "",
                TrackSort::LastPlayed,
                true,
                RouteSeedWindow::top(),
                &cancel
            )
            .await
            .expect("Activity-sorted Album Tracks")
            .order
            .len(),
        2
    );
    assert_eq!(
        fixture
            .database
            .genre_track_route_page(
                fixture.source,
                fixture.genre,
                Some(fixture.folder),
                "artist b",
                TrackSort::Album,
                true,
                RouteSeedWindow::top(),
                &cancel
            )
            .await
            .expect("scoped filtered Genre Tracks")
            .order
            .len(),
        2
    );
    assert_eq!(
        fixture
            .database
            .mood_track_route_page(
                fixture.source,
                fixture.mood,
                None,
                "2023",
                TrackSort::Year,
                false,
                RouteSeedWindow::top(),
                &cancel
            )
            .await
            .expect("filtered Mood Tracks")
            .order,
        [fixture.track_uris[3].clone()]
    );
    assert!(
        fixture
            .database
            .artist_route_page(
                fixture.source,
                None,
                false,
                false,
                "artist a",
                ArtistSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .expect("direct Artist filter")
            .0
            .is_empty()
    );
    assert_eq!(
        fixture
            .database
            .artist_route_page(
                fixture.source,
                None,
                true,
                false,
                "artist a",
                ArtistSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .expect("Album Artist filter")
            .0,
        [fixture.artists[0]]
    );
    assert_eq!(
        fixture
            .database
            .search(
                fixture.source,
                None,
                false,
                &SearchRequest::new("artist b"),
                &cancel
            )
            .await
            .expect("multi-term Artist Search")
            .artists
            .into_iter()
            .map(|artist| artist.name)
            .collect::<Vec<_>>(),
        ["Artist B"]
    );
    assert_eq!(
        fixture
            .database
            .search(
                fixture.source,
                None,
                true,
                &SearchRequest::new("artist a"),
                &cancel
            )
            .await
            .expect("Album Artist Search")
            .artists[0]
            .artist_key,
        fixture.artists[0]
    );
    assert_eq!(
        fixture
            .database
            .mood_detail(fixture.source, fixture.mood, Some(fixture.folder), &cancel)
            .await
            .expect("scoped Mood detail")
            .unwrap()
            .mood
            .track_count,
        4
    );
    let empty_folder = sqlx::query_scalar("INSERT INTO folders(source_key,object_id,name,normalized_name,sort_text) VALUES (?1,'empty-folder','Empty','empty','empty') RETURNING folder_key")
        .bind(fixture.source).fetch_one(&mut raw).await.expect("insert empty Folder");
    assert!(
        fixture
            .database
            .album_route_page(
                fixture.source,
                Some(empty_folder),
                false,
                "",
                AlbumSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel
            )
            .await
            .expect("empty Folder scope")
            .0
            .is_empty()
    );
}

#[tokio::test]
async fn empty_named_collection_routes_return_empty_pages() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    for statement in [
        "DELETE FROM track_genres",
        "DELETE FROM album_genres",
        "DELETE FROM genres",
        "DELETE FROM track_moods",
        "DELETE FROM moods",
    ] {
        sqlx::query(statement).execute(&mut raw).await.unwrap();
    }
    drop(raw);
    let source = fixture.source;
    let cancellation = ReadCancellation::new();

    let genres = fixture
        .database
        .genre_route_page(
            source,
            None,
            "",
            GenreSort::TrackCount,
            false,
            RouteSeedWindow::top(),
            &cancellation,
        )
        .await
        .expect("empty Genre route");
    let moods = fixture
        .database
        .mood_route_page(
            source,
            None,
            "",
            MoodSort::Duration,
            false,
            RouteSeedWindow::top(),
            &cancellation,
        )
        .await
        .expect("empty Mood route");

    assert_eq!(genres, (Vec::new(), 0, Vec::new()));
    assert_eq!(moods, (Vec::new(), 0, Vec::new()));
}

#[tokio::test]
async fn metadata_point_writes_publish_changes_and_leave_no_ops_unchanged() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let track = fixture.tracks[0];
    let outcome = fixture
        .database
        .update_track_metadata(
            fixture.source,
            track,
            TrackMetadataWrite {
                title: "Edited".to_string(),
                normalized_search: "edited album a artist a changed".to_string(),
                display_album: "Album A".to_string(),
                display_artist: "Artist A".to_string(),
                sort_text: "edited".to_string(),
                duration_millis: 181_000,
                disc_number: 1,
                track_number: 1,
                year: Some(2025),
                release_date: None,
                date_added: Some("2025-01-01".to_string()),
                source_format: Some("FLAC".to_string()),
                comment: Some("Changed".to_string()),
                bpm: Some(121),
                musicbrainz_recording_id: None,
                musicbrainz_release_track_id: None,
                cue_path: None,
                cue_start_millis: None,
                cue_end_millis: None,
            },
        )
        .await
        .expect("update Track metadata");
    assert!(matches!(outcome, library::ScanOutcome::Changed(_)));
    let row = fixture
        .database
        .track_rows(&[track], &cancel)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(row.title, "Edited");
    assert_eq!(row.comment.as_deref(), Some("Changed"));

    assert!(matches!(
        fixture
            .database
            .update_album_metadata(
                fixture.source,
                fixture.albums[0],
                AlbumMetadataWrite {
                    title: "Edited Album".to_string(),
                    normalized_title: "edited album".to_string(),
                    display_artist: "Artist A".to_string(),
                    sort_text: "edited album".to_string(),
                    year: Some(2025),
                    release_date: None,
                    date_added: None,
                    musicbrainz_release_id: None,
                    musicbrainz_release_group_id: None,
                    is_compilation: Some(true),
                },
            )
            .await
            .expect("update Album metadata"),
        library::ScanOutcome::Changed(_)
    ));
    let artist_write = ArtistMetadataWrite {
        name: "Edited Artist".to_string(),
        normalized_name: "edited artist".to_string(),
        sort_text: "edited artist".to_string(),
        musicbrainz_artist_id: None,
    };
    let outcome = fixture
        .database
        .update_artist_metadata(fixture.source, fixture.artists[0], artist_write.clone())
        .await
        .expect("update Artist metadata");
    let library::ScanOutcome::Changed(publication) = outcome else {
        panic!("changed artist must publish");
    };
    let unchanged = fixture
        .database
        .update_artist_metadata(fixture.source, fixture.artists[0], artist_write)
        .await
        .unwrap();
    let library::ScanOutcome::Identical(unchanged) = unchanged else {
        panic!("identical artist must not invalidate");
    };
    assert_eq!(publication.catalog_revision, unchanged.catalog_revision);
    let filtered = fixture
        .database
        .track_route_page(
            fixture.source,
            None,
            false,
            "changed",
            library::TrackSort::Title,
            false,
            RouteSeedWindow::top(),
            &cancel,
        )
        .await
        .expect("filter edited Track")
        .order;
    assert_eq!(filtered, [fixture.track_uris[0].clone()]);
    assert_eq!(
        fixture
            .database
            .track_route_page(
                fixture.source,
                None,
                false,
                "2025",
                library::TrackSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .expect("Track Year filter")
            .order,
        [fixture.track_uris[0].clone()]
    );
    assert_eq!(
        fixture
            .database
            .album_route_page(
                fixture.source,
                None,
                false,
                "rock",
                AlbumSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .expect("Album Genre filter")
            .0,
        [fixture.albums[1], fixture.albums[0]]
    );
    assert_eq!(
        fixture
            .database
            .album_route_page(
                fixture.source,
                None,
                false,
                "artist b",
                AlbumSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .expect("Album Artist filter")
            .0,
        [fixture.albums[1]]
    );
    assert_eq!(
        fixture
            .database
            .album_route_page(
                fixture.source,
                None,
                false,
                "2024",
                AlbumSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .expect("Album Year filter")
            .0,
        [fixture.albums[1]]
    );

    let mut raw = connection(&fixture.path).await;
    let plan = sqlx::query_as::<_, (i64, i64, i64, String)>(
        "EXPLAIN QUERY PLAN
         SELECT track_key FROM tracks
         WHERE source_key=?1 ORDER BY sort_text, track_key",
    )
    .bind(fixture.source)
    .fetch_one(&mut raw)
    .await
    .expect("Track order plan")
    .3;
    assert!(plan.contains("tracks_order_idx"), "{plan}");
    let album_plan = sqlx::query_as::<_, (i64, i64, i64, String)>(
        "EXPLAIN QUERY PLAN
         SELECT album_key FROM albums
         WHERE source_key=?1 ORDER BY sort_text, album_key",
    )
    .bind(fixture.source)
    .fetch_one(&mut raw)
    .await
    .expect("Album order plan")
    .3;
    assert!(album_plan.contains("albums_order_idx"), "{album_plan}");
    for (sql, expected) in [
        (
            "EXPLAIN QUERY PLAN SELECT track.track_key FROM tracks track WHERE track.source_key=?1 AND (?2 OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=track.media_uri),track.source_favorite)=1) AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders relation WHERE relation.track_key=track.track_key AND relation.folder_key=?3)) ORDER BY track.sort_text,track.track_key",
            "tracks_order_idx",
        ),
        (
            "EXPLAIN QUERY PLAN SELECT album.album_key FROM albums album WHERE album.source_key=?1 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=album.album_key AND scope.folder_key=?3)) AND (?2=0 OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite)=1) ORDER BY album.sort_text,album.album_key",
            "albums_order_idx",
        ),
        (
            "EXPLAIN QUERY PLAN SELECT genre.genre_key FROM genres genre WHERE genre.source_key=?1 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_genres credit JOIN track_folders scope USING(track_key) WHERE credit.genre_key=genre.genre_key AND scope.folder_key=?3)) ORDER BY genre.sort_text,genre.genre_key",
            "genres_order_idx",
        ),
        (
            "EXPLAIN QUERY PLAN SELECT mood.mood_key FROM moods mood WHERE mood.source_key=?1 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_moods credit JOIN track_folders scope USING(track_key) WHERE credit.mood_key=mood.mood_key AND scope.folder_key=?3)) ORDER BY mood.sort_text,mood.mood_key",
            "moods_order_idx",
        ),
    ] {
        let details = sqlx::query_as::<_, (i64, i64, i64, String)>(sql)
            .bind(fixture.source)
            .bind(false)
            .bind(Option::<i64>::None)
            .fetch_all(&mut raw)
            .await
            .expect("production default order plan")
            .into_iter()
            .map(|row| row.3)
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(details.contains(expected), "{expected}: {details}");
        assert!(
            !details.contains("USE TEMP B-TREE FOR ORDER BY"),
            "{details}"
        );
    }
    let artist_plan = sqlx::query_as::<_, (i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT artist.artist_key FROM artists artist WHERE artist.source_key=?1 AND (?2 OR EXISTS (SELECT 1 FROM album_artists relation WHERE relation.artist_key=artist.artist_key)) AND (?3 OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_favorite)=1) AND (?4 IS NULL OR ((?5 AND EXISTS (SELECT 1 FROM track_artists credit JOIN track_folders scope USING(track_key) WHERE credit.artist_key=artist.artist_key AND scope.folder_key=?6)) OR (?7 AND EXISTS (SELECT 1 FROM album_artists credit JOIN tracks track USING(album_key) JOIN track_folders scope USING(track_key) WHERE credit.artist_key=artist.artist_key AND scope.folder_key=?8)))) ORDER BY artist.sort_text,artist.artist_key")
        .bind(fixture.source).bind(true).bind(true).bind(Option::<i64>::None).bind(true).bind(Option::<i64>::None).bind(false).bind(Option::<i64>::None).fetch_all(&mut raw).await.expect("production Artist title plan").into_iter().map(|row| row.3).collect::<Vec<_>>().join(" | ");
    assert!(artist_plan.contains("artists_order_idx"), "{artist_plan}");
    assert!(
        !artist_plan.contains("USE TEMP B-TREE FOR ORDER BY"),
        "{artist_plan}"
    );
    let direct_album_plan = sqlx::query_as::<_, (i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT album.album_key FROM albums album WHERE album.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=album.album_key AND scope.folder_key=?2)) AND (?3 OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite)=1) ORDER BY album.year DESC NULLS LAST,album.sort_text,album.album_key")
        .bind(fixture.source).bind(Option::<i64>::None).bind(true).fetch_all(&mut raw).await.expect("actual direct Album sort plan").into_iter().map(|row| row.3).collect::<Vec<_>>().join(" | ");
    assert!(
        !direct_album_plan.contains("listens"),
        "{direct_album_plan}"
    );
    assert!(
        !direct_album_plan.contains("activity_baseline"),
        "{direct_album_plan}"
    );
    let counted_album_plan = sqlx::query_as::<_, (i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT album.album_key FROM albums album LEFT JOIN tracks track ON track.album_key=album.album_key AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?2)) WHERE album.source_key=?1 AND (?3 OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite)=1) GROUP BY album.album_key ORDER BY count(track.track_key) DESC,album.sort_text,album.album_key")
        .bind(fixture.source).bind(Option::<i64>::None).bind(true).fetch_all(&mut raw).await.expect("actual counted Album sort plan").into_iter().map(|row| row.3).collect::<Vec<_>>().join(" | ");
    assert!(
        counted_album_plan.contains("tracks_album_idx"),
        "{counted_album_plan}"
    );
    assert!(
        !counted_album_plan.contains("listens"),
        "{counted_album_plan}"
    );
    assert!(
        !counted_album_plan.contains("activity_baseline"),
        "{counted_album_plan}"
    );
    let point_plan = sqlx::query_as::<_, (i64, i64, i64, String)>(
        "EXPLAIN QUERY PLAN
         SELECT * FROM tracks WHERE source_key=?1 AND track_key=?2",
    )
    .bind(fixture.source)
    .bind(track)
    .fetch_one(&mut raw)
    .await
    .expect("bounded Track row plan")
    .3;
    assert!(point_plan.contains("INTEGER PRIMARY KEY"), "{point_plan}");
}
