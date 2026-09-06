use library::{
    ArtistSort, CalendarActivityPeriod, GenreSort, LocalAccessOrigin, LocalAccessWrite,
    PlayedFilter, RadioSeed, RandomCriteria, ReadCancellation, RouteSeedWindow, SearchRequest,
    SmartPlaylistActivityPeriod, SmartPlaylistDefinition, SmartPlaylistListSort, SmartPlaylistRule,
    SmartPlaylistRuleField, SmartPlaylistRuleOperator, SmartPlaylistRuleValue, SmartPlaylistSort,
};

use super::support::{connection, fixture};

#[tokio::test]
async fn playlist_and_smart_rows_retain_exact_metadata_links_and_unavailable_entries() {
    let fixture = fixture().await;
    let database = &fixture.database;
    let cancel = ReadCancellation::new();
    let local_uri = "file:///music/local.flac";
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO sources(object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES('local','Local','local',zeroblob(32),zeroblob(32))")
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tracks(source_key,object_id,media_uri,title,normalized_search,display_album,display_artist,sort_text,duration_millis) SELECT source_key,'local-track',?1,'Local Track','local track','','','local track',60000 FROM sources WHERE object_id='local'")
        .bind(local_uri)
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    let uris = vec![
        fixture.track_uris[0].clone(),
        "https://example.test/direct.flac".into(),
        fixture.track_uris[0].clone(),
        local_uri.into(),
    ];
    let playlist = database
        .create_playlist(None, "Links", &uris)
        .await
        .unwrap()
        .unwrap()
        .0;
    let order = database
        .playlist_entry_order(
            playlist,
            None,
            library::PlaylistEntrySort::Position,
            false,
            "",
            &cancel,
        )
        .await
        .unwrap();
    let entries = database.playlist_entry_rows(&order, &cancel).await.unwrap();
    let smart = database
        .smart_playlist_track_rows(&uris, &cancel)
        .await
        .unwrap();
    assert_eq!(entries.len(), 4);
    assert_eq!(smart.len(), 4);
    for index in [0, 2] {
        assert_eq!(entries[index].media_uri, uris[index]);
        assert_eq!(entries[index].source_id.as_deref(), Some("source"));
        assert_eq!(
            entries[index].album_media_uri.as_ref(),
            Some(&fixture.album_uris[0])
        );
        assert_eq!(entries[index].artists[0].media_uri, fixture.artist_uris[0]);
        assert_eq!(
            entries[index].album_artists[0].media_uri,
            fixture.artist_uris[0]
        );
        assert_eq!(smart[index].album_media_uri, entries[index].album_media_uri);
        assert_eq!(smart[index].artists, entries[index].artists);
        assert_eq!(smart[index].album_artists, entries[index].album_artists);
    }
    assert_eq!(entries[1].media_uri, uris[1]);
    assert!(entries[1].source_id.is_none());
    assert!(entries[1].album_media_uri.is_none());
    assert!(entries[1].artists.is_empty());
    assert!(entries[1].album_artists.is_empty());
    assert!(smart[1].album_media_uri.is_none());
    assert!(smart[1].artists.is_empty());
    assert!(smart[1].album_artists.is_empty());
    assert_eq!(entries[3].media_uri, local_uri);
    assert_eq!(entries[3].source_id.as_deref(), Some("local"));
}

#[tokio::test]
async fn uri_artwork_is_identical_across_owner_projections_and_current_scopes() {
    let fixture = fixture().await;
    let database = &fixture.database;
    let cancel = ReadCancellation::new();
    let uri = &fixture.track_uris[0];
    let global = database
        .create_playlist(None, "Global artwork", &[uri.clone(), uri.clone()])
        .await
        .unwrap()
        .unwrap()
        .0;
    let current = database
        .create_playlist(
            Some(fixture.source),
            "Current artwork",
            std::slice::from_ref(uri),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
    let smart = database
        .create_smart_playlist(
            "All artwork",
            &SmartPlaylistDefinition {
                current: false,
                ..SmartPlaylistDefinition::default()
            },
        )
        .await
        .unwrap();
    let mut raw = connection(&fixture.path).await;
    let other: library::SourceKey = sqlx::query_scalar(
        "INSERT INTO sources(object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES('other','Other','other',zeroblob(32),zeroblob(32)) RETURNING source_key",
    ).fetch_one(&mut raw).await.unwrap();
    let empty_folder: library::FolderKey = sqlx::query_scalar(
        "INSERT INTO folders(source_key,object_id,name,normalized_name,sort_text) VALUES(?1,'empty','Empty','empty','empty') RETURNING folder_key",
    ).bind(other).fetch_one(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO listens(source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped) VALUES('source',?1,'Historical title','Artist','Album',100,'1970-01',42000,42000,0)")
        .bind(uri).execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO queue_occurrences(object_id,media_uri,position,traversal_position,provenance_kind,title,artist,album,duration_millis) VALUES('artwork-occurrence',?1,0,0,'manual','Queue title','Artist','Album',42000)")
        .bind(uri).execute(&mut raw).await.unwrap();

    // Neither a different Current nor its folder changes A(uri). A later catalog
    // revision, including removal of the binding, is observed by every owner.
    for binding in [Some(b"first".to_vec()), Some(b"revised".to_vec()), None] {
        sqlx::query("UPDATE tracks SET artwork_binding=?1 WHERE media_uri=?2")
            .bind(&binding)
            .bind(uri)
            .execute(&mut raw)
            .await
            .unwrap();
        let expected_sample: Vec<Vec<u8>> = binding.clone().into_iter().collect();
        for (source, folder) in [
            (None, None),
            (Some(fixture.source), Some(fixture.folder)),
            (Some(other), Some(empty_folder)),
        ] {
            for sort in [
                library::PlaylistSort::Position,
                library::PlaylistSort::Title,
                library::PlaylistSort::TrackCount,
                library::PlaylistSort::Duration,
            ] {
                let (order, _, rows) = database
                    .playlist_route_page(
                        source,
                        folder,
                        sort,
                        false,
                        "",
                        RouteSeedWindow::top(),
                        &cancel,
                    )
                    .await
                    .unwrap();
                assert!(order.contains(&global));
                let row = rows.iter().find(|row| row.playlist_key == global).unwrap();
                assert_eq!(row.track_count, 2);
                assert_eq!(
                    row.representative_artwork,
                    [expected_sample.clone(), expected_sample.clone()].concat()
                );
            }
            let detail = database
                .playlist_detail_page(
                    global,
                    folder,
                    library::PlaylistEntrySort::Position,
                    false,
                    RouteSeedWindow::top(),
                    &cancel,
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(detail.order.len(), 2);
            assert_eq!(detail.first_rows.len(), 2);
            for row in detail.first_rows {
                assert_eq!(&row.media_uri, uri);
                assert_eq!(row.artwork_binding, binding);
            }
            let result = database
                .smart_playlist_detail(source, smart, folder, 500, RouteSeedWindow::top(), &cancel)
                .await
                .unwrap()
                .unwrap();
            let row = result
                .first_rows
                .iter()
                .find(|row| &row.media_uri == uri)
                .unwrap();
            assert_eq!(row.artwork_binding, binding);
            assert_eq!(result.summary.artwork_bindings, expected_sample);
        }
        let current_rows = database.playlist_rows(&[current], &cancel).await.unwrap();
        assert_eq!(current_rows[0].representative_artwork, expected_sample);
        let preview = database
            .track_artwork_bindings(
                &[
                    uri.clone(),
                    "https://example.test/no-catalog.flac".into(),
                    uri.clone(),
                ],
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(
            preview,
            [expected_sample.clone(), expected_sample.clone()].concat()
        );
        assert!(
            database
                .track_artwork_bindings(&["https://example.test/no-catalog.flac".into()], &cancel)
                .await
                .unwrap()
                .is_empty(),
            "an unavailable URI never borrows an unrelated catalog cover"
        );
        let catalog = database
            .track_rows(&[fixture.tracks[0]], &cancel)
            .await
            .unwrap();
        assert_eq!(catalog[0].artwork_binding, binding);
        let smart_rows = database
            .smart_playlist_track_rows(std::slice::from_ref(uri), &cancel)
            .await
            .unwrap();
        assert_eq!(smart_rows[0].artwork_binding, binding);
        let queue = database.queue_page(None, "", 100, &cancel).await.unwrap();
        assert_eq!(queue[0].artwork_binding, binding);
        let restored = database.restore_queue().await.unwrap();
        assert_eq!(restored.occurrences[0].artwork_binding, binding);
        let history = database.activity_history(None, "", &cancel).await.unwrap();
        assert_eq!(history[0].artwork_binding, binding);
        let history_window = database
            .history_rows_by_uri(std::slice::from_ref(uri), &cancel)
            .await
            .unwrap();
        assert_eq!(history_window[0].artwork_binding, binding);
        let scoped_samples = database
            .representative_artwork_page(
                fixture.source,
                Some(fixture.folder),
                library::RepresentativeArtworkScope::PlaylistTracks,
                4,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(scoped_samples, expected_sample);
    }
    let scoped = database
        .playlist_entry_order(
            current,
            Some(empty_folder),
            library::PlaylistEntrySort::Position,
            false,
            "",
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        scoped.is_empty(),
        "folder scope still selects Current membership"
    );
    let all_entries = database
        .playlist_entry_order(
            current,
            None,
            library::PlaylistEntrySort::Position,
            false,
            "",
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        database
            .playlist_entry_rows(&all_entries, &cancel)
            .await
            .unwrap()
            .len(),
        1,
        "a known occurrence is projected without Current/folder permission"
    );
}

#[tokio::test]
async fn calendar_activity_summarizes_each_visible_entity_kind() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    for (media_uri, date) in [
        (&fixture.track_uris[0], "2025-06-03"),
        (&fixture.track_uris[1], "2025-06-14"),
        (&fixture.track_uris[2], "2024-12-31"),
    ] {
        sqlx::query(
            "INSERT INTO listens(source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped)
             VALUES('source',?1,'Track','Artist','Album',CAST(strftime('%s',?2) AS INTEGER),substr(?2,1,7),180000,120000,0)",
        )
        .bind(media_uri)
        .bind(date)
        .execute(&mut raw)
        .await
        .expect("insert calendar listen");
    }
    sqlx::query("INSERT INTO catalog.activity_baseline(source_key,track_object_id,play_count,skip_count,last_played_at) VALUES(?1,'track-3',4,0,1)")
        .bind(fixture.source)
        .execute(&mut raw)
        .await
        .expect("insert lifetime baseline");
    drop(raw);
    let cancel = ReadCancellation::new();
    let month = fixture
        .database
        .calendar_activity_summary(
            fixture.source,
            CalendarActivityPeriod::Month {
                year: 2025,
                month: 6,
            },
            20,
            &cancel,
        )
        .await
        .expect("monthly Activity");
    assert_eq!(month.tracks.len(), 2);
    assert_eq!(month.albums.len(), 1);
    assert_eq!(month.artists.len(), 1);
    assert_eq!(month.genres.len(), 1);
    let year = fixture
        .database
        .calendar_activity_summary(
            fixture.source,
            CalendarActivityPeriod::Year(2025),
            20,
            &cancel,
        )
        .await
        .expect("yearly Activity");
    assert_eq!(year.tracks.len(), 2);
    let lifetime = fixture
        .database
        .calendar_activity_summary(
            fixture.source,
            CalendarActivityPeriod::Lifetime,
            20,
            &cancel,
        )
        .await
        .expect("lifetime Activity");
    assert_eq!(lifetime.tracks.len(), 4);
    assert!(lifetime.albums.len() >= 2);
    assert!(lifetime.artists.len() >= 2);
    assert_eq!(lifetime.genres.len(), 1);
}

#[tokio::test]
async fn downloaded_row_facts_require_download_owned_local_access() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let rows = fixture
        .database
        .track_rows(&fixture.tracks[..2], &cancel)
        .await
        .expect("initial Track rows");
    assert!(rows.iter().all(|row| !row.is_downloaded));

    for (index, origin) in [LocalAccessOrigin::Mapping, LocalAccessOrigin::Download]
        .into_iter()
        .enumerate()
    {
        let track = &rows[index];
        fixture
            .database
            .upsert_local_access(
                Some(fixture.source),
                &LocalAccessWrite {
                    media_uri: track.media_uri.clone(),
                    origin,
                    path: format!("/validated/{}.flac", track.object_id.as_str()),
                    root: "/validated".to_string(),
                    relative_path: format!("{}.flac", track.object_id.as_str()),
                    size_bytes: 10,
                    mtime_ns: 1,
                    device_id: Some(1),
                    inode: Some(index as i64 + 1),
                    parser_version: 1,
                    title: track.title.clone(),
                    album: track.album.clone(),
                    artist: track.artist.clone(),
                    disc_number: track.disc_number,
                    track_number: track.track_number,
                    duration_millis: track.duration_millis,
                    access_uri: format!("file:///validated/{}.flac", track.object_id.as_str()),
                    loudness_analysis_key: Some(track.loudness_analysis_key),
                },
            )
            .await
            .expect("write accepted Local access provenance");
    }
    let rows = fixture
        .database
        .track_rows(&fixture.tracks[..2], &cancel)
        .await
        .expect("provenance Track rows");
    assert!(
        !rows[0].is_downloaded,
        "configured mapping is not a Download"
    );
    assert!(
        rows[1].is_downloaded,
        "Download-owned artifact is downloaded"
    );

    let album = fixture
        .database
        .album_rows(fixture.source, &[fixture.albums[0]], None, &cancel)
        .await
        .expect("Album download aggregate")
        .pop()
        .expect("Album row");
    assert_eq!(album.track_count, 2);
    assert_eq!(album.downloaded_count, 1);
    let artist = fixture
        .database
        .artist_rows(fixture.source, &[fixture.artists[0]], false, None, &cancel)
        .await
        .expect("Artist download aggregate")
        .pop()
        .expect("Artist row");
    assert_eq!(artist.downloaded_count, 1);
    let genre = fixture
        .database
        .genre_rows(fixture.source, &[fixture.genre], None, &cancel)
        .await
        .expect("Genre download aggregate")
        .pop()
        .expect("Genre row");
    assert_eq!(genre.downloaded_count, 1);
    let mood = fixture
        .database
        .mood_rows(fixture.source, &[fixture.mood], None, &cancel)
        .await
        .expect("Mood download aggregate")
        .pop()
        .expect("Mood row");
    assert_eq!(mood.downloaded_count, 1);
}

#[tokio::test]
async fn genre_rows_prepare_only_one_representative_cover() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    for (index, album) in fixture.albums.iter().copied().enumerate() {
        sqlx::query("UPDATE albums SET artwork_binding=?2 WHERE album_key=?1")
            .bind(album)
            .bind(vec![index as u8 + 1])
            .execute(&mut raw)
            .await
            .expect("set Album artwork");
    }
    drop(raw);

    let row = fixture
        .database
        .genre_rows(
            fixture.source,
            &[fixture.genre],
            None,
            &ReadCancellation::new(),
        )
        .await
        .expect("read Genre row")
        .pop()
        .expect("Genre row");

    assert_eq!(row.representative_artwork.len(), 1);
}

#[tokio::test]
async fn genre_routes_reject_an_artwork_only_identity_without_track_membership() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    let orphan = sqlx::query_scalar::<_, library::GenreKey>(
        "INSERT INTO genres(source_key,object_id,name,normalized_name,sort_text,artwork_binding) VALUES(?1,'orphan-genre','Orphan Genre','orphan genre','orphan genre',x'01') RETURNING genre_key",
    )
    .bind(fixture.source)
    .fetch_one(&mut raw)
    .await
    .expect("insert artwork-only Genre");
    drop(raw);

    for sort in [GenreSort::Title, GenreSort::TrackCount] {
        let (order, _, _) = fixture
            .database
            .genre_route_page(
                fixture.source,
                None,
                "",
                sort,
                false,
                RouteSeedWindow::top(),
                &ReadCancellation::new(),
            )
            .await
            .expect("Genre route");
        assert!(!order.contains(&orphan));
    }
}

#[tokio::test]
async fn artist_play_order_stays_complete_beside_the_favorite_section() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    for track in &fixture.tracks {
        let media_uri =
            fixture.track_uris[fixture.tracks.iter().position(|key| key == track).unwrap()].clone();
        fixture
            .database
            .set_favorite(&library::FavoriteTarget::Track(media_uri), false)
            .await
            .unwrap();
    }
    fixture
        .database
        .set_favorite(
            &library::FavoriteTarget::Track(fixture.track_uris[0].clone()),
            true,
        )
        .await
        .unwrap();
    let complete = fixture
        .database
        .artist_track_route_page(
            fixture.source,
            fixture.artists[0],
            false,
            None,
            "",
            library::TrackSort::Title,
            false,
            false,
            RouteSeedWindow::top(),
            &cancel,
        )
        .await
        .unwrap()
        .order;
    let favorites = fixture
        .database
        .artist_track_route_page(
            fixture.source,
            fixture.artists[0],
            false,
            None,
            "",
            library::TrackSort::Title,
            false,
            true,
            RouteSeedWindow::top(),
            &cancel,
        )
        .await
        .unwrap()
        .order;
    assert_eq!(favorites, [fixture.track_uris[0].clone()]);
    assert!(complete.len() > favorites.len());
}

#[tokio::test]
async fn track_artist_and_album_artist_roles_keep_exact_membership_and_own_artwork() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    let album_owner_uri =
        library::source_entity_uri(&library::SourceId::new("source"), "artist", "album-owner");
    let album_owner = sqlx::query_scalar::<_, library::ArtistKey>(
        "INSERT INTO artists(source_key,object_id,media_uri,name,normalized_name,sort_text,artwork_binding,source_favorite)
         VALUES(?1,'album-owner',?2,'Album Owner','album owner','album owner',?3,0)
         RETURNING artist_key",
    )
    .bind(fixture.source)
    .bind(album_owner_uri)
    .bind(b"album-artist-art".as_slice())
    .fetch_one(&mut raw)
    .await
    .expect("insert distinct Album Artist");
    sqlx::query("DELETE FROM album_artists WHERE album_key=?1")
        .bind(fixture.albums[0])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO album_artists(album_key,artist_key,position) VALUES(?1,?2,0)")
        .bind(fixture.albums[0])
        .bind(album_owner)
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("UPDATE albums SET artwork_binding=?2 WHERE album_key=?1")
        .bind(fixture.albums[0])
        .bind(b"album-art".as_slice())
        .execute(&mut raw)
        .await
        .unwrap();

    let track_artist = fixture
        .database
        .artist_rows(fixture.source, &[fixture.artists[0]], false, None, &cancel)
        .await
        .expect("Track Artist row")
        .pop()
        .unwrap();
    let album_artist = fixture
        .database
        .artist_rows(fixture.source, &[album_owner], true, None, &cancel)
        .await
        .expect("Album Artist row")
        .pop()
        .unwrap();
    assert_ne!(track_artist.artist_key, album_artist.artist_key);
    assert_eq!((track_artist.album_count, track_artist.track_count), (1, 2));
    assert_eq!((album_artist.album_count, album_artist.track_count), (1, 2));
    assert_eq!(track_artist.artwork_binding, None);
    assert_eq!(
        album_artist.artwork_binding.as_deref(),
        Some(b"album-artist-art".as_slice())
    );

    let track_artist_tracks = fixture
        .database
        .artist_track_route_page(
            fixture.source,
            track_artist.artist_key,
            false,
            None,
            "",
            library::TrackSort::Title,
            false,
            false,
            RouteSeedWindow::top(),
            &cancel,
        )
        .await
        .unwrap()
        .order;
    let album_artist_tracks = fixture
        .database
        .artist_track_route_page(
            fixture.source,
            album_artist.artist_key,
            true,
            None,
            "",
            library::TrackSort::Title,
            false,
            false,
            RouteSeedWindow::top(),
            &cancel,
        )
        .await
        .unwrap()
        .order;
    assert_eq!(track_artist_tracks, album_artist_tracks);
    assert_eq!(track_artist_tracks.len(), 2);
    assert!(
        fixture
            .database
            .artist_track_route_page(
                fixture.source,
                track_artist.artist_key,
                true,
                None,
                "",
                library::TrackSort::Title,
                false,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .unwrap()
            .order
            .is_empty()
    );
    assert!(
        fixture
            .database
            .artist_track_route_page(
                fixture.source,
                album_artist.artist_key,
                false,
                None,
                "",
                library::TrackSort::Title,
                false,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .unwrap()
            .order
            .is_empty()
    );

    assert_eq!(
        fixture
            .database
            .artist_album_projection_order(
                fixture.source,
                track_artist.artist_key,
                false,
                None,
                "",
                library::AlbumSort::Title,
                false,
                &cancel,
            )
            .await
            .unwrap(),
        [fixture.albums[0]]
    );
    assert_eq!(
        fixture
            .database
            .artist_album_projection_order(
                fixture.source,
                album_artist.artist_key,
                true,
                None,
                "",
                library::AlbumSort::Title,
                false,
                &cancel,
            )
            .await
            .unwrap(),
        [fixture.albums[0]]
    );

    sqlx::query("UPDATE albums SET artwork_binding=NULL WHERE album_key=?1")
        .bind(fixture.albums[0])
        .execute(&mut raw)
        .await
        .unwrap();
    let album = fixture
        .database
        .album_rows(fixture.source, &[fixture.albums[0]], None, &cancel)
        .await
        .expect("Album artwork fallback")
        .pop()
        .expect("Album row");
    assert_eq!(album.artwork_binding, None);
}

#[tokio::test]
async fn artist_orders_and_rows_require_the_requested_credit_role() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    let mut role_artists = Vec::new();
    for (object_id, name, rating) in [
        ("role-track", "Performer One", 30),
        ("role-album", "Release Owner", 40),
        ("role-both", "Dual Credit", 50),
    ] {
        let media_uri =
            library::source_entity_uri(&library::SourceId::new("source"), "artist", object_id);
        role_artists.push(
            sqlx::query_scalar::<_, library::ArtistKey>(
                "INSERT INTO artists(source_key,object_id,media_uri,name,normalized_name,sort_text,source_favorite,source_rating)
                 VALUES(?1,?2,?3,?4,lower(?4),lower(?4),1,?5) RETURNING artist_key",
            )
            .bind(fixture.source)
            .bind(object_id)
            .bind(media_uri)
            .bind(name)
            .bind(rating)
            .fetch_one(&mut raw)
            .await
            .expect("insert role fixture Artist"),
        );
    }
    let [track_only, album_only, both] = role_artists.as_slice() else {
        unreachable!()
    };
    let (track_only, album_only, both) = (*track_only, *album_only, *both);
    sqlx::query(
        "INSERT INTO track_artists(track_key,artist_key,position) VALUES(?1,?2,1),(?1,?3,2)",
    )
    .bind(fixture.tracks[0])
    .bind(track_only)
    .bind(both)
    .execute(&mut raw)
    .await
    .expect("insert Track Artist roles");
    sqlx::query(
        "INSERT INTO album_artists(album_key,artist_key,position) VALUES(?1,?2,1),(?1,?3,2)",
    )
    .bind(fixture.albums[0])
    .bind(album_only)
    .bind(both)
    .execute(&mut raw)
    .await
    .expect("insert Album Artist roles");
    drop(raw);

    for sort in [
        ArtistSort::Title,
        ArtistSort::AlbumCount,
        ArtistSort::TrackCount,
        ArtistSort::LastPlayed,
        ArtistSort::PlayCount,
        ArtistSort::Rating,
        ArtistSort::Favorite,
    ] {
        for folder in [None, Some(fixture.folder)] {
            for favorites_only in [false, true] {
                for descending in [false, true] {
                    let track_artists = fixture
                        .database
                        .artist_route_page(
                            fixture.source,
                            folder,
                            false,
                            favorites_only,
                            "",
                            sort,
                            descending,
                            RouteSeedWindow::top(),
                            &cancel,
                        )
                        .await
                        .expect("Track Artist order")
                        .0;
                    assert!(track_artists.contains(&track_only), "{sort:?}");
                    assert!(track_artists.contains(&both), "{sort:?}");
                    assert!(!track_artists.contains(&album_only), "{sort:?}");

                    let album_artists = fixture
                        .database
                        .artist_route_page(
                            fixture.source,
                            folder,
                            true,
                            favorites_only,
                            "",
                            sort,
                            descending,
                            RouteSeedWindow::top(),
                            &cancel,
                        )
                        .await
                        .expect("Album Artist order")
                        .0;
                    assert!(album_artists.contains(&album_only), "{sort:?}");
                    assert!(album_artists.contains(&both), "{sort:?}");
                    assert!(!album_artists.contains(&track_only), "{sort:?}");
                }
            }
        }
    }

    let track_row = fixture
        .database
        .artist_rows(fixture.source, &[track_only], false, None, &cancel)
        .await
        .expect("Track Artist row")
        .pop()
        .expect("role-matched Track Artist");
    assert_eq!((track_row.album_count, track_row.track_count), (1, 1));
    let album_row = fixture
        .database
        .artist_rows(fixture.source, &[album_only], true, None, &cancel)
        .await
        .expect("Album Artist row")
        .pop()
        .expect("role-matched Album Artist");
    assert_eq!((album_row.album_count, album_row.track_count), (1, 2));
    assert!(
        fixture
            .database
            .artist_rows(fixture.source, &[album_only], false, None, &cancel)
            .await
            .expect("mismatched Track Artist row")
            .is_empty()
    );
    assert!(
        fixture
            .database
            .artist_rows(fixture.source, &[track_only], true, None, &cancel)
            .await
            .expect("mismatched Album Artist row")
            .is_empty()
    );
    assert!(
        fixture
            .database
            .artist_detail(fixture.source, album_only, false, None, &cancel)
            .await
            .expect("mismatched Artist detail")
            .is_none()
    );
    assert!(
        fixture
            .database
            .artist_detail(fixture.source, track_only, true, None, &cancel)
            .await
            .expect("mismatched Album Artist detail")
            .is_none()
    );
    assert_eq!(
        fixture
            .database
            .artist_track_route_page(
                fixture.source,
                track_only,
                false,
                None,
                "",
                library::TrackSort::Title,
                false,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .expect("Track Artist Tracks")
            .order,
        [fixture.track_uris[0].clone()]
    );
    assert_eq!(
        fixture
            .database
            .artist_album_projection_order(
                fixture.source,
                album_only,
                true,
                None,
                "",
                library::AlbumSort::Title,
                false,
                &cancel,
            )
            .await
            .expect("Album Artist Albums"),
        [fixture.albums[0]]
    );
}

#[tokio::test]
async fn selected_source_defaults_have_the_three_activity_smart_playlists() {
    let fixture = fixture().await;
    assert!(
        fixture
            .database
            .ensure_default_smart_playlists()
            .await
            .expect("install default Smart Playlists")
    );
    assert!(
        !fixture
            .database
            .ensure_default_smart_playlists()
            .await
            .expect("default Smart Playlists are idempotent")
    );
    let cancellation = ReadCancellation::new();
    let (order, _, rows) = fixture
        .database
        .smart_playlist_route_page(
            Some(fixture.source),
            None,
            SmartPlaylistListSort::Position,
            false,
            0,
            RouteSeedWindow::top(),
            &cancellation,
        )
        .await
        .expect("read default Smart Playlists");
    assert_eq!(order.len(), 3);
    assert_eq!(
        rows.iter()
            .map(|row| (row.object_id.as_str(), row.name.as_str()))
            .collect::<Vec<_>>(),
        [
            ("builtin:most_played", "Most Played"),
            ("builtin:never_played", "Never Played"),
            ("builtin:most_skipped", "Most Skipped"),
        ]
    );
    assert_eq!(rows[0].definition.sort_field, SmartPlaylistSort::PlayCount);
    assert_eq!(
        rows[1].definition.match_all[0].field,
        SmartPlaylistRuleField::Played
    );
    assert_eq!(rows[2].definition.sort_field, SmartPlaylistSort::SkipCount);
}

#[tokio::test]
async fn all_smart_playlists_admit_direct_media_while_current_is_an_exact_subset() {
    let fixture = fixture().await;
    let direct = "https://media.example/direct.flac".to_string();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO queue_occurrences(object_id,media_uri,position,traversal_position,provenance_kind,title,artist,album,duration_millis) VALUES('direct-owner',?1,0,0,'manual','Direct Signal','Outside Artist','Outside Album',42000)")
        .bind(&direct)
        .execute(&mut raw)
        .await
        .expect("retain direct media in Queue");
    drop(raw);
    let (playlist, _) = fixture
        .database
        .create_playlist(None, "Direct", std::slice::from_ref(&direct))
        .await
        .expect("create global Playlist")
        .expect("Playlist key");
    assert_eq!(
        fixture
            .database
            .add_playlist_media(None, playlist, std::slice::from_ref(&direct), false)
            .await
            .expect("append durable media directly"),
        1
    );
    assert_eq!(
        fixture
            .database
            .add_playlist_media(None, playlist, std::slice::from_ref(&direct), true)
            .await
            .expect("skip an existing durable identity"),
        0
    );
    let mut definition = SmartPlaylistDefinition {
        current: false,
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::Title,
            operator: SmartPlaylistRuleOperator::Contains,
            value: Some(SmartPlaylistRuleValue::Text("signal".to_string())),
        }],
        match_any: Vec::new(),
        sort_field: SmartPlaylistSort::Title,
        descending: false,
        activity_period: SmartPlaylistActivityPeriod::Lifetime,
        limit: None,
    };
    let smart = fixture
        .database
        .create_smart_playlist("Direct", &definition)
        .await
        .expect("create All Smart Playlist");
    let cancel = ReadCancellation::new();
    let all = fixture
        .database
        .smart_playlist_media_uri_order(None, smart, None, 0, &cancel)
        .await
        .expect("evaluate All Smart Playlist");
    assert_eq!(all, [direct]);

    definition.current = true;
    fixture
        .database
        .update_smart_playlist(smart, "Direct", &definition)
        .await
        .expect("make Smart Playlist Current");
    assert!(
        fixture
            .database
            .smart_playlist_media_uri_order(Some(fixture.source), smart, None, 0, &cancel)
            .await
            .expect("evaluate Current Smart Playlist")
            .is_empty()
    );
}

#[tokio::test]
async fn completed_direct_download_is_an_all_media_owner_without_a_source() {
    let fixture = fixture().await;
    let direct = "https://media.example/download.flac".to_string();
    fixture
        .database
        .upsert_local_access(
            None,
            &LocalAccessWrite {
                media_uri: direct.clone(),
                origin: LocalAccessOrigin::Download,
                path: "/downloads/direct.flac".to_string(),
                root: "/downloads".to_string(),
                relative_path: "direct.flac".to_string(),
                size_bytes: 10,
                mtime_ns: 1,
                device_id: None,
                inode: None,
                parser_version: 5,
                title: "Direct artifact".to_string(),
                album: String::new(),
                artist: String::new(),
                disc_number: 0,
                track_number: 0,
                duration_millis: 42_000,
                access_uri: "file:///downloads/direct.flac".to_string(),
                loudness_analysis_key: None,
            },
        )
        .await
        .expect("record completed direct download");
    assert_eq!(
        fixture
            .database
            .playback_access_uri(&direct)
            .await
            .expect("resolve direct download access")
            .as_deref(),
        Some("file:///downloads/direct.flac")
    );

    let mut definition = SmartPlaylistDefinition {
        current: false,
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::Title,
            operator: SmartPlaylistRuleOperator::Contains,
            value: Some(SmartPlaylistRuleValue::Text("artifact".to_string())),
        }],
        ..SmartPlaylistDefinition::default()
    };
    let smart = fixture
        .database
        .create_smart_playlist("Downloaded direct", &definition)
        .await
        .expect("create direct download Smart Playlist");
    let cancel = ReadCancellation::new();
    assert_eq!(
        fixture
            .database
            .smart_playlist_media_uri_order(None, smart, None, 0, &cancel)
            .await
            .expect("evaluate All direct download"),
        [direct]
    );
    definition.current = true;
    fixture
        .database
        .update_smart_playlist(smart, "Downloaded direct", &definition)
        .await
        .expect("make direct download Smart Playlist Current");
    assert!(
        fixture
            .database
            .smart_playlist_media_uri_order(Some(fixture.source), smart, None, 0, &cancel)
            .await
            .expect("evaluate Current direct download")
            .is_empty()
    );
}

#[tokio::test]
async fn smart_visible_window_keeps_uri_order_and_latest_snapshot_without_rechecking_rules() {
    let fixture = fixture().await;
    let direct = "https://media.example/window.flac".to_string();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO queue_occurrences(object_id,media_uri,position,traversal_position,provenance_kind,title,artist,album,duration_millis,snapshot_at) VALUES('window-owner',?1,0,0,'manual','Latest snapshot','Artist','Album',42000,300)")
        .bind(&direct).execute(&mut raw).await.expect("retain Queue snapshot");
    sqlx::query("INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped) VALUES(?1,'Old snapshot','Artist','Album',100,'1970-01',42000,42000,0)")
        .bind(&direct).execute(&mut raw).await.expect("retain historical snapshot");
    drop(raw);
    let mut definition = SmartPlaylistDefinition {
        current: false,
        ..SmartPlaylistDefinition::default()
    };
    let key = fixture
        .database
        .create_smart_playlist("Window", &definition)
        .await
        .unwrap();
    let cancel = ReadCancellation::new();
    let order = fixture
        .database
        .smart_playlist_media_uri_order(None, key, None, 500, &cancel)
        .await
        .unwrap();
    assert!(order.contains(&direct));
    let detail = fixture
        .database
        .smart_playlist_detail(None, key, None, 500, RouteSeedWindow::top(), &cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(detail.tracks, order);
    assert_eq!(detail.summary.track_count as usize, detail.tracks.len());
    assert_eq!(
        detail
            .first_rows
            .iter()
            .map(|row| &row.media_uri)
            .collect::<Vec<_>>(),
        detail.tracks.iter().take(64).collect::<Vec<_>>()
    );
    definition.match_all.push(SmartPlaylistRule {
        field: SmartPlaylistRuleField::Title,
        operator: SmartPlaylistRuleOperator::Equals,
        value: Some(SmartPlaylistRuleValue::Text("No match".into())),
    });
    fixture
        .database
        .update_smart_playlist(key, "Window", &definition)
        .await
        .unwrap();
    let requested = vec![direct.clone(), fixture.track_uris[0].clone(), direct];
    let rows = fixture
        .database
        .smart_playlist_track_rows(&requested, &cancel)
        .await
        .unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>(),
        requested
    );
    assert_eq!(rows[0].title, "Latest snapshot");
    assert_eq!(rows[1].title, "Alpha");
    assert_eq!(rows[2].duration_millis, 42000);
    assert_eq!(rows[0].play_count, 1);
}

#[tokio::test]
async fn global_playlist_media_remains_in_all_when_its_source_is_forgotten() {
    let fixture = fixture().await;
    let retained = fixture.track_uris[0].clone();
    let (playlist, _) = fixture
        .database
        .create_playlist(None, "Retained", std::slice::from_ref(&retained))
        .await
        .expect("create global Playlist")
        .expect("Playlist key");
    let mut raw = connection(&fixture.path).await;
    sqlx::query("DELETE FROM tracks WHERE media_uri=?1")
        .bind(&retained)
        .execute(&mut raw)
        .await
        .expect("remove retained media from the catalog");
    drop(raw);
    let mut definition = SmartPlaylistDefinition {
        current: false,
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::Title,
            operator: SmartPlaylistRuleOperator::Equals,
            value: Some(SmartPlaylistRuleValue::Text("Alpha".to_string())),
        }],
        ..SmartPlaylistDefinition::default()
    };
    let smart = fixture
        .database
        .create_smart_playlist("Retained", &definition)
        .await
        .expect("create retained-source Smart Playlist");
    let cancellation = ReadCancellation::new();
    assert_eq!(
        fixture
            .database
            .smart_playlist_media_uri_order(None, smart, None, 0, &cancellation)
            .await
            .expect("evaluate All while source is retained"),
        std::slice::from_ref(&retained)
    );
    definition.current = true;
    fixture
        .database
        .update_smart_playlist(smart, "Retained", &definition)
        .await
        .expect("make retained-source Smart Playlist Current");
    assert_eq!(
        fixture
            .database
            .smart_playlist_media_uri_order(Some(fixture.source), smart, None, 0, &cancellation,)
            .await
            .expect("evaluate Current while source is retained"),
        std::slice::from_ref(&retained)
    );

    fixture
        .database
        .remove_source(&library::SourceId::new("source"))
        .await
        .expect("forget source");
    assert!(
        fixture
            .database
            .smart_playlist_media_uri_order(Some(fixture.source), smart, None, 0, &cancellation)
            .await
            .unwrap()
            .is_empty()
    );
    definition.current = false;
    fixture
        .database
        .update_smart_playlist(smart, "Retained", &definition)
        .await
        .expect("make retained-source Smart Playlist All");
    assert_eq!(
        fixture
            .database
            .smart_playlist_media_uri_order(None, smart, None, 0, &cancellation)
            .await
            .expect("evaluate All after Forget"),
        std::slice::from_ref(&retained)
    );
    assert_eq!(
        fixture
            .database
            .playlist_media_uri_order(playlist, None, &cancellation)
            .await
            .expect("read retained Playlist snapshot"),
        [retained]
    );
}

#[tokio::test]
async fn all_includes_retained_queue_and_activity_without_a_cached_source() {
    let fixture = fixture().await;
    let queue_uri =
        library::source_entity_uri(&library::SourceId::new("absent"), "track", "queued");
    let listen_uri =
        library::source_entity_uri(&library::SourceId::new("absent"), "track", "listened");
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO queue_occurrences(object_id,media_uri,position,traversal_position,provenance_kind,title,artist,album,duration_millis) VALUES('retained',?1,0,0,'manual','Queued','Artist','Album',1000)")
        .bind(&queue_uri).execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO listens(external_id,source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis) VALUES('retained','absent',?1,'Listened','Artist','Album',1,'1970-01',1000,1000)")
        .bind(&listen_uri).execute(&mut raw).await.unwrap();
    sqlx::query("DELETE FROM catalog.sources")
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    let key = fixture
        .database
        .create_smart_playlist(
            "All",
            &SmartPlaylistDefinition {
                current: false,
                ..SmartPlaylistDefinition::default()
            },
        )
        .await
        .unwrap();
    let cancellation = ReadCancellation::new();
    assert_eq!(
        fixture
            .database
            .smart_playlist_media_uri_order(None, key, None, 10, &cancellation)
            .await
            .unwrap(),
        [listen_uri, queue_uri]
    );
}

#[tokio::test]
async fn smart_overview_keeps_the_full_order_and_only_the_requested_summary_window() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET artwork_binding=x'010203'")
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO local_locators(media_uri,origin,path,root,relative_path,access_uri) VALUES(?1,'download','/music/track.flac','/music','track.flac','file:///music/track.flac')")
        .bind(&fixture.track_uris[0]).execute(&mut raw).await.unwrap();
    for index in 0..70 {
        let definition = SmartPlaylistDefinition {
            current: index % 2 == 0,
            limit: Some(index % 4 + 1),
            ..SmartPlaylistDefinition::default()
        };
        fixture
            .database
            .create_smart_playlist(&format!("List {index:02}"), &definition)
            .await
            .unwrap();
    }
    for (folder, sort) in [
        (None, SmartPlaylistListSort::TrackCount),
        (None, SmartPlaylistListSort::Duration),
        (Some(fixture.folder), SmartPlaylistListSort::Title),
    ] {
        for descending in [false, true] {
            let window = RouteSeedWindow::relative(1.0);
            let (order, start, rows) = fixture
                .database
                .smart_playlist_route_page(
                    Some(fixture.source),
                    folder,
                    sort,
                    descending,
                    0,
                    window,
                    &cancel,
                )
                .await
                .unwrap();
            assert_eq!(order.len(), 70);
            assert_eq!(start, 64);
            assert_eq!(rows.len(), 6);
            let expected = fixture
                .database
                .smart_playlist_rows(
                    Some(fixture.source),
                    &order[window.range(order.len())],
                    folder,
                    0,
                    &cancel,
                )
                .await
                .unwrap();
            assert_eq!(rows, expected);
        }
    }
}

#[tokio::test]
async fn smart_playlist_reordering_preserves_unique_positions() {
    let fixture = fixture().await;
    let definition = SmartPlaylistDefinition::default();
    let first = fixture
        .database
        .create_smart_playlist("First", &definition)
        .await
        .expect("create first Smart Playlist");
    let second = fixture
        .database
        .create_smart_playlist("Second", &definition)
        .await
        .expect("create second Smart Playlist");
    let third = fixture
        .database
        .create_smart_playlist("Third", &definition)
        .await
        .expect("create third Smart Playlist");

    assert!(
        fixture
            .database
            .move_smart_playlist(third, first)
            .await
            .expect("move Smart Playlist upward")
    );
    assert!(
        fixture
            .database
            .move_smart_playlist(third, second)
            .await
            .expect("move Smart Playlist downward")
    );
    assert_eq!(
        fixture
            .database
            .smart_playlist_route_page(
                Some(fixture.source),
                None,
                SmartPlaylistListSort::Position,
                false,
                0,
                RouteSeedWindow::top(),
                &ReadCancellation::new(),
            )
            .await
            .expect("read reordered Smart Playlists")
            .0,
        [first, second, third]
    );
}

#[tokio::test]
async fn smart_playlist_periods_and_never_played_query_sqlite_directly() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let suggestions = fixture
        .database
        .smart_playlist_value_suggestions(fixture.source, None, &cancel)
        .await
        .expect("bounded Smart Playlist value suggestions");
    assert_eq!(suggestions.genres, ["Rock".to_string()]);
    assert_eq!(suggestions.moods, ["Energetic".to_string()]);
    let never = SmartPlaylistDefinition {
        current: false,
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::Played,
            operator: SmartPlaylistRuleOperator::Is,
            value: Some(SmartPlaylistRuleValue::Bool(false)),
        }],
        match_any: Vec::new(),
        sort_field: SmartPlaylistSort::Title,
        descending: false,
        activity_period: SmartPlaylistActivityPeriod::Weekly,
        limit: None,
    };
    let smart = fixture
        .database
        .create_smart_playlist("Never Played", &never)
        .await
        .expect("create Never Played");
    let policy_definition = SmartPlaylistDefinition {
        current: false,
        match_all: SmartPlaylistRuleField::ALL
            .into_iter()
            .map(SmartPlaylistRuleField::default_rule)
            .collect(),
        ..SmartPlaylistDefinition::default()
    };
    let policy = fixture
        .database
        .create_smart_playlist("Policy Defaults", &policy_definition)
        .await
        .expect("Library-owned editor defaults pass Library validation");
    assert_eq!(
        fixture
            .database
            .smart_playlist_media_uri_order(Some(fixture.source), smart, None, 0, &cancel)
            .await
            .expect("Never Played membership")
            .len(),
        4
    );

    let mut raw = connection(&fixture.path).await;
    sqlx::query(
        "INSERT INTO catalog.activity_baseline(
             source_key, track_object_id, play_count, skip_count, last_played_at
         ) VALUES (?1, 'track-0', 2, 0, 1)",
    )
    .bind(fixture.source)
    .execute(&mut raw)
    .await
    .expect("insert lifetime baseline");
    for (media_uri, date) in [
        (&fixture.track_uris[0], "2025-06-12"),
        (&fixture.track_uris[1], "2025-06-18"),
        (&fixture.track_uris[2], "2025-05-25"),
        (&fixture.track_uris[3], "2024-06-19"),
    ] {
        sqlx::query(
            "INSERT INTO listens(
                 source_id, media_uri, track_title,
                 artist_name, album_title, started_at, local_period, duration_millis, listened_millis, skipped
             ) VALUES ('source', ?1, 'Track', 'Artist', 'Album',
                       CAST(strftime('%s', ?2) AS INTEGER), substr(?2,1,7), 180000, 180000, 0)",
        )
        .bind(media_uri)
        .bind(date)
        .execute(&mut raw)
        .await
        .expect("insert period listen");
    }
    let now = sqlx::query_scalar::<_, i64>("SELECT CAST(strftime('%s', '2025-06-18') AS INTEGER)")
        .fetch_one(&mut raw)
        .await
        .expect("read fixed current time");
    let mut played = SmartPlaylistDefinition {
        current: false,
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::PlayCount,
            operator: SmartPlaylistRuleOperator::Above,
            value: Some(SmartPlaylistRuleValue::Number(0)),
        }],
        match_any: Vec::new(),
        sort_field: SmartPlaylistSort::PlayCount,
        descending: true,
        activity_period: SmartPlaylistActivityPeriod::Weekly,
        limit: None,
    };
    for (period, expected) in [
        (SmartPlaylistActivityPeriod::Weekly, 2),
        (SmartPlaylistActivityPeriod::Monthly, 3),
        (SmartPlaylistActivityPeriod::Yearly, 4),
        (SmartPlaylistActivityPeriod::Lifetime, 4),
    ] {
        played.activity_period = period;
        assert!(
            fixture
                .database
                .update_smart_playlist(smart, "Played", &played)
                .await
                .expect("update period template")
        );
        assert_eq!(
            fixture
                .database
                .smart_playlist_media_uri_order(Some(fixture.source), smart, None, now, &cancel)
                .await
                .expect("period membership")
                .len(),
            expected
        );
    }
    let facts = fixture
        .database
        .smart_playlist_rows(Some(fixture.source), &[smart], None, now, &cancel)
        .await
        .expect("Smart Playlist row")
        .pop()
        .expect("row exists");
    assert_eq!(facts.track_count, 4);
    assert!(facts.duration_millis > 0);
    sqlx::query("UPDATE tracks SET comment=NULL WHERE track_key=?1")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .expect("remove optional Comment");
    let mut optional = SmartPlaylistDefinition {
        current: false,
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::Comment,
            operator: SmartPlaylistRuleOperator::NotContains,
            value: Some(SmartPlaylistRuleValue::Text("absent".to_string())),
        }],
        match_any: Vec::new(),
        sort_field: SmartPlaylistSort::Title,
        descending: false,
        activity_period: SmartPlaylistActivityPeriod::Lifetime,
        limit: None,
    };
    let second = fixture
        .database
        .create_smart_playlist("Optional", &optional)
        .await
        .expect("create second Smart Playlist");
    assert_eq!(
        fixture
            .database
            .smart_playlist_media_uri_order(Some(fixture.source), second, None, now, &cancel)
            .await
            .expect("missing optional value does not match NotContains")
            .len(),
        3
    );
    optional.match_all[0].operator = SmartPlaylistRuleOperator::IsEmpty;
    optional.match_all[0].value = None;
    assert!(
        fixture
            .database
            .update_smart_playlist(second, "Optional", &optional)
            .await
            .expect("update optional rule")
    );
    assert_eq!(
        fixture
            .database
            .smart_playlist_media_uri_order(Some(fixture.source), second, None, now, &cancel)
            .await
            .expect("missing optional value matches IsEmpty"),
        [fixture.track_uris[0].clone()]
    );
    let smart_rows = fixture
        .database
        .smart_playlist_rows(Some(fixture.source), &[smart, second], None, now, &cancel)
        .await
        .expect("consistent Smart Playlist rows");
    assert_eq!(
        smart_rows[0].track_count as usize,
        fixture
            .database
            .smart_playlist_media_uri_order(Some(fixture.source), smart, None, now, &cancel)
            .await
            .expect("first Smart membership")
            .len()
    );
    assert_eq!(
        smart_rows[1].track_count as usize,
        fixture
            .database
            .smart_playlist_media_uri_order(Some(fixture.source), second, None, now, &cancel)
            .await
            .expect("second Smart membership")
            .len()
    );
    assert_eq!(
        fixture
            .database
            .smart_playlist_route_page(
                Some(fixture.source),
                None,
                SmartPlaylistListSort::TrackCount,
                true,
                now,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .expect("sorted Smart Playlist order")
            .0,
        [smart, second, policy]
    );
    assert_eq!(
        fixture
            .database
            .smart_playlist_route_page(
                Some(fixture.source),
                None,
                SmartPlaylistListSort::Duration,
                true,
                now,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .expect("duration-sorted Smart Playlist order")
            .0,
        [smart, second, policy]
    );
    let smart_plan = sqlx::query_as::<_, (i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT smart_playlist_key FROM smart_playlists ORDER BY position,smart_playlist_key")
        .fetch_all(&mut raw).await.expect("production Smart Playlist order plan").into_iter().map(|row| row.3).collect::<Vec<_>>().join(" | ");
    assert!(
        smart_plan.contains("sqlite_autoindex_smart_playlists_2"),
        "{smart_plan}"
    );
    assert!(
        !smart_plan.contains("USE TEMP B-TREE FOR ORDER BY"),
        "{smart_plan}"
    );
    let window_plan = sqlx::query_as::<_, (i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT media_uri FROM listens WHERE source_id=?1 AND started_at>=?2 AND started_at<=?3 ORDER BY started_at DESC")
        .bind("source").bind(now-31_536_000).bind(now).fetch_all(&mut raw).await.expect("rolling Activity window plan").into_iter().map(|row| row.3).collect::<Vec<_>>().join(" | ");
    assert!(window_plan.contains("listens_history_idx"), "{window_plan}");
}

#[tokio::test]
async fn artist_and_home_activity_follow_membership_even_without_listen_source_ids() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    for (uri, time) in [
        (fixture.track_uris[0].as_str(), 100),
        (fixture.track_uris[0].as_str(), 200),
        (fixture.track_uris[2].as_str(), 300),
        ("file:///outside-current.flac", 400),
    ] {
        sqlx::query("INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis) VALUES(?1,'Track','Artist','Album',?2,'2026-09',1000,1000)")
            .bind(uri).bind(time).execute(&mut raw).await.unwrap();
    }
    sqlx::query("DELETE FROM track_folders WHERE track_key=?1")
        .bind(fixture.tracks[2])
        .execute(&mut raw)
        .await
        .unwrap();
    let cancel = ReadCancellation::new();
    for album_artist in [false, true] {
        let rows = fixture
            .database
            .artist_rows(
                fixture.source,
                &[fixture.artists[0]],
                album_artist,
                None,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(rows[0].play_count, 2);
        assert_eq!(rows[0].last_played, Some(200));
    }
    let home = fixture
        .database
        .home_page(fixture.source, Some(fixture.folder), 0, 0, &cancel)
        .await
        .unwrap();
    assert_eq!(
        home.most_played
            .tracks
            .iter()
            .map(|row| row.track.media_uri.as_str())
            .collect::<Vec<_>>(),
        [fixture.track_uris[0].as_str()]
    );
    assert_eq!(
        home.recently_played
            .tracks
            .iter()
            .map(|row| row.track.media_uri.as_str())
            .collect::<Vec<_>>(),
        [fixture.track_uris[0].as_str()]
    );
}

#[tokio::test]
async fn home_search_and_radio_results_stay_bounded() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let search = fixture
        .database
        .search(
            fixture.source,
            None,
            false,
            &SearchRequest::with_limit("note", 2),
            &cancel,
        )
        .await
        .expect("cached Search");
    assert_eq!(search.tracks.len(), 2);
    assert!(!search.tracks[0].artists.is_empty());
    assert!(search.albums.is_empty());
    let object_ids = search
        .tracks
        .iter()
        .rev()
        .map(|track| track.object_id.clone())
        .collect::<Vec<_>>();
    let resolved = fixture
        .database
        .search_rows_by_objects(fixture.source, None, false, &object_ids, &[], &[], &cancel)
        .await
        .expect("live Search identity rows");
    assert_eq!(
        resolved
            .tracks
            .iter()
            .map(|track| track.object_id.as_str())
            .collect::<Vec<_>>(),
        object_ids.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert!(
        fixture
            .database
            .track_route_page(
                fixture.source,
                None,
                false,
                "artist a",
                library::TrackSort::Title,
                false,
                RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .expect("complete Track filter")
            .order
            .len()
            >= 2
    );
    let initial_home = fixture
        .database
        .home_page(fixture.source, None, 0, 0, &cancel)
        .await
        .expect("initial bounded Home page");
    assert!(initial_home.most_played.tracks.is_empty());
    assert_eq!(
        initial_home
            .provider_sections
            .iter()
            .find(|section| section.section_id == "featured")
            .expect("provider Home section")
            .rows
            .tracks
            .len(),
        1
    );
    let alternate_showcase_home = fixture
        .database
        .home_page(fixture.source, None, 1, 0, &cancel)
        .await
        .expect("alternate Showcase Home page");
    assert_eq!(initial_home.explore, alternate_showcase_home.explore);
    let alternate_showcase = alternate_showcase_home
        .showcase
        .expect("alternate Showcase");
    assert_eq!(
        initial_home
            .showcase
            .as_ref()
            .expect("initial Showcase")
            .album
            .album_key,
        fixture.albums[0]
    );
    assert_eq!(alternate_showcase.album.album_key, fixture.albums[1]);
    let alternate_explore_home = fixture
        .database
        .home_page(fixture.source, None, 0, 2, &cancel)
        .await
        .expect("alternate Explore Home page");
    assert_eq!(initial_home.showcase, alternate_explore_home.showcase);
    assert_ne!(initial_home.explore, alternate_explore_home.explore);
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE albums SET date_added=NULL,first_seen_at=CASE album_key WHEN ?1 THEN 200 ELSE 100 END")
        .bind(fixture.albums[0]).execute(&mut raw).await.expect("establish Local first-seen facts");
    let newly_added = fixture
        .database
        .home_page(fixture.source, None, 0, 0, &cancel)
        .await
        .expect("bounded Home Albums")
        .newly_added
        .albums;
    assert_eq!(newly_added.len(), 2);
    assert_eq!(newly_added[0].album.album_key, fixture.albums[0]);
    sqlx::query("UPDATE albums SET release_date=NULL,year=0 WHERE album_key=?1")
        .bind(fixture.albums[1])
        .execute(&mut raw)
        .await
        .expect("remove release fact");
    assert_eq!(
        fixture
            .database
            .home_page(fixture.source, None, 0, 0, &cancel)
            .await
            .expect("released Albums require a release fact")
            .recently_released
            .albums
            .iter()
            .map(|row| row.album.album_key)
            .collect::<Vec<_>>(),
        [fixture.albums[0]]
    );
    assert_eq!(
        fixture
            .database
            .home_page(fixture.source, None, 0, 0, &cancel)
            .await
            .expect("bounded Home Genres")
            .genres
            .len(),
        1
    );

    let random = fixture
        .database
        .random_candidates(
            fixture.source,
            None,
            &RandomCriteria {
                min_year: Some(2020),
                max_year: Some(2030),
                genre: Some(fixture.genre),
                played: PlayedFilter::All,
                require_media: true,
                variation: 7,
            },
            &[fixture.track_uris[0].clone()],
            2,
            &cancel,
        )
        .await
        .expect("bounded random candidates");
    assert_eq!(random.len(), 2);
    assert!(!random.contains(&fixture.track_uris[0]));
    let radio = fixture
        .database
        .radio_candidates(
            fixture.source,
            RadioSeed::Genre(fixture.genre),
            &[],
            3,
            false,
            9,
            &cancel,
        )
        .await
        .expect("bounded Radio candidates");
    assert_eq!(radio.len(), 3);
    sqlx::query("INSERT INTO queue_occurrences(object_id,media_uri,position,traversal_position,provenance_kind,title,artist,album,duration_millis) VALUES ('radio-queued',?1,0,0,'manual','Track 0','Artist','Album',1000)")
        .bind(&fixture.track_uris[0]).execute(&mut raw).await.expect("persist complete Radio queue exclusion");
    assert_eq!(
        fixture
            .database
            .radio_candidates(
                fixture.source,
                RadioSeed::Genre(fixture.genre),
                &[],
                2,
                false,
                9,
                &cancel
            )
            .await
            .expect("complete queue exclusion")
            .len(),
        2
    );
    assert!(
        fixture
            .database
            .radio_candidates(
                fixture.source,
                RadioSeed::Genre(fixture.genre),
                &vec![fixture.track_uris[1].clone(); 501],
                2,
                false,
                9,
                &cancel
            )
            .await
            .is_err()
    );

    let album_radio = fixture
        .database
        .radio_candidates(
            fixture.source,
            RadioSeed::Album(fixture.albums[0]),
            &[],
            2,
            false,
            0,
            &cancel,
        )
        .await
        .expect("Album Radio context");
    assert!(
        album_radio
            .iter()
            .all(|track| fixture.track_uris[2..].contains(track))
    );
    let playlist = fixture
        .database
        .create_playlist(
            Some(fixture.source),
            "Seed",
            &[fixture.track_uris[0].clone()],
        )
        .await
        .expect("create Radio seed Playlist")
        .unwrap()
        .0;
    let playlist_radio = fixture
        .database
        .radio_candidates(
            fixture.source,
            RadioSeed::Playlist(playlist),
            &[],
            2,
            false,
            0,
            &cancel,
        )
        .await
        .expect("Playlist first-Track Radio context");
    assert!(!playlist_radio.contains(&fixture.track_uris[0]));
    assert_ne!(playlist_radio, [fixture.track_uris[0].clone()]);
    for sql in [
        "EXPLAIN QUERY PLAN SELECT track_key FROM tracks WHERE source_key=?1 AND track_key>=?2 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=tracks.track_key AND scope.folder_key=?3)) ORDER BY track_key LIMIT 24",
        "EXPLAIN QUERY PLAN SELECT album_key FROM albums WHERE source_key=?1 AND album_key>=?2 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=albums.album_key AND scope.folder_key=?3)) ORDER BY album_key LIMIT 1",
    ] {
        let details = sqlx::query_as::<_, (i64, i64, i64, String)>(sql)
            .bind(fixture.source)
            .bind(0_i64)
            .bind(fixture.genre)
            .fetch_all(&mut raw)
            .await
            .expect("bounded pivot plan")
            .into_iter()
            .map(|row| row.3)
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            !details.contains("USE TEMP B-TREE FOR ORDER BY"),
            "{sql}: {details}"
        );
        assert!(
            details.contains("_key_idx") || details.contains("INTEGER PRIMARY KEY"),
            "{sql}: {details}"
        );
    }
}
