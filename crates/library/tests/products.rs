use library::{
    ArtistSort, CalendarActivityPeriod, GenreSort, LocalAccessOrigin, LocalAccessWrite,
    PlayedFilter, RadioSeed, RandomCriteria, ReadCancellation, SearchRequest,
    SmartPlaylistActivityPeriod, SmartPlaylistDefinition, SmartPlaylistListSort, SmartPlaylistRule,
    SmartPlaylistRuleField, SmartPlaylistRuleOperator, SmartPlaylistRuleValue, SmartPlaylistSort,
};

use super::support::{connection, fixture};

#[tokio::test]
async fn calendar_activity_summarizes_each_visible_entity_kind() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    for (track, object_id, date) in [
        (fixture.tracks[0], "track-0", "2025-06-03"),
        (fixture.tracks[1], "track-1", "2025-06-14"),
        (fixture.tracks[2], "track-2", "2024-12-31"),
    ] {
        sqlx::query(
            "INSERT INTO listens(source_key,track_key,track_object_id,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped)
             VALUES(?1,?2,?3,'Track','Artist','Album',CAST(strftime('%s',?4) AS INTEGER),substr(?4,1,7),180000,120000,0)",
        )
        .bind(fixture.source)
        .bind(track)
        .bind(object_id)
        .bind(date)
        .execute(&mut raw)
        .await
        .expect("insert calendar listen");
    }
    sqlx::query("INSERT INTO activity_baseline(source_key,track_object_id,play_count,skip_count,last_played_at) VALUES(?1,'track-3',4,0,1)")
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
        .track_rows(fixture.source, &fixture.tracks[..2], &cancel)
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
                fixture.source,
                &LocalAccessWrite {
                    track_object_id: Some(track.object_id.clone()),
                    origin,
                    path: format!("/validated/{}.flac", track.object_id),
                    root: "/validated".to_string(),
                    relative_path: format!("{}.flac", track.object_id),
                    size_bytes: 10,
                    mtime_ns: 1,
                    device_id: Some(1),
                    inode: Some(index as i64 + 1),
                    parser_version: 1,
                    title: track.title.clone(),
                    album: track.display_album.clone(),
                    artist: track.display_artist.clone(),
                    disc_number: track.disc_number,
                    track_number: track.track_number,
                    duration_millis: track.duration_millis,
                    media_uri: format!("file:///validated/{}.flac", track.object_id),
                    loudness_analysis_key: track.loudness_analysis_key,
                },
            )
            .await
            .expect("write accepted Local access provenance");
    }
    let rows = fixture
        .database
        .track_rows(fixture.source, &fixture.tracks[..2], &cancel)
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
        let (order, _) = fixture
            .database
            .genre_route_page(
                fixture.source,
                None,
                "",
                sort,
                false,
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
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET user_favorite=0 WHERE source_key=?1")
        .bind(fixture.source)
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET user_favorite=1 WHERE track_key=?1")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .unwrap();
    let complete = fixture
        .database
        .artist_track_order(
            fixture.source,
            fixture.artists[0],
            false,
            None,
            "",
            library::TrackSort::Title,
            false,
            &cancel,
        )
        .await
        .unwrap();
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
            &cancel,
        )
        .await
        .unwrap()
        .order;
    assert_eq!(favorites, [fixture.tracks[0]]);
    assert!(complete.len() > favorites.len());
}

#[tokio::test]
async fn track_artist_and_album_artist_roles_keep_exact_membership_and_cover_fallback() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    let album_owner = sqlx::query_scalar::<_, library::ArtistKey>(
        "INSERT INTO artists(source_key,object_id,name,normalized_name,sort_text,artwork_binding,source_favorite)
         VALUES(?1,'album-owner','Album Owner','album owner','album owner',?2,0)
         RETURNING artist_key",
    )
    .bind(fixture.source)
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
    assert_eq!(
        track_artist.artwork_binding.as_deref(),
        Some(b"album-art".as_slice())
    );
    assert_eq!(
        album_artist.artwork_binding.as_deref(),
        Some(b"album-artist-art".as_slice())
    );

    let track_artist_tracks = fixture
        .database
        .artist_track_order(
            fixture.source,
            track_artist.artist_key,
            false,
            None,
            "",
            library::TrackSort::Title,
            false,
            &cancel,
        )
        .await
        .unwrap();
    let album_artist_tracks = fixture
        .database
        .artist_track_order(
            fixture.source,
            album_artist.artist_key,
            true,
            None,
            "",
            library::TrackSort::Title,
            false,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(track_artist_tracks, album_artist_tracks);
    assert_eq!(track_artist_tracks.len(), 2);
    assert!(
        fixture
            .database
            .artist_track_order(
                fixture.source,
                track_artist.artist_key,
                true,
                None,
                "",
                library::TrackSort::Title,
                false,
                &cancel,
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .database
            .artist_track_order(
                fixture.source,
                album_artist.artist_key,
                false,
                None,
                "",
                library::TrackSort::Title,
                false,
                &cancel,
            )
            .await
            .unwrap()
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
    assert_eq!(
        album.artwork_binding.as_deref(),
        Some(b"album-artist-art".as_slice())
    );
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
        role_artists.push(
            sqlx::query_scalar::<_, library::ArtistKey>(
                "INSERT INTO artists(source_key,object_id,name,normalized_name,sort_text,source_favorite,source_rating)
                 VALUES(?1,?2,?3,lower(?3),lower(?3),1,?4) RETURNING artist_key",
            )
            .bind(fixture.source)
            .bind(object_id)
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
            .artist_track_order(
                fixture.source,
                track_only,
                false,
                None,
                "",
                library::TrackSort::Title,
                false,
                &cancel,
            )
            .await
            .expect("Track Artist Tracks"),
        [fixture.tracks[0]]
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
async fn download_access_coexists_with_and_precedes_mapping_access() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let track = fixture
        .database
        .track_rows(fixture.source, &fixture.tracks[..1], &cancel)
        .await
        .expect("Track row")
        .pop()
        .expect("fixture Track");
    let access = |origin, name: &str| LocalAccessWrite {
        track_object_id: Some(track.object_id.clone()),
        origin,
        path: format!("/validated/{name}.flac"),
        root: "/validated".to_string(),
        relative_path: format!("{name}.flac"),
        size_bytes: 10,
        mtime_ns: 1,
        device_id: Some(1),
        inode: Some(if origin == LocalAccessOrigin::Download {
            2
        } else {
            1
        }),
        parser_version: 1,
        title: track.title.clone(),
        album: track.display_album.clone(),
        artist: track.display_artist.clone(),
        disc_number: track.disc_number,
        track_number: track.track_number,
        duration_millis: track.duration_millis,
        media_uri: format!("file:///validated/{name}.flac"),
        loudness_analysis_key: track.loudness_analysis_key,
    };
    let mapping = fixture
        .database
        .upsert_local_access(
            fixture.source,
            &access(LocalAccessOrigin::Mapping, "mapping"),
        )
        .await
        .expect("mapping access");
    let download = fixture
        .database
        .upsert_local_access(
            fixture.source,
            &access(LocalAccessOrigin::Download, "download"),
        )
        .await
        .expect("download access");

    let preferred = fixture
        .database
        .resolve_local_access(
            fixture.source,
            Some(&track.object_id),
            &track.title,
            &track.display_album,
            &track.display_artist,
            track.disc_number,
            track.track_number,
            track.duration_millis,
        )
        .await
        .expect("preferred access")
        .expect("download or mapping");
    assert_eq!(preferred.local_access_file_key, download);
    assert_eq!(preferred.origin, LocalAccessOrigin::Download);

    assert!(
        fixture
            .database
            .remove_local_access(fixture.source, download)
            .await
            .expect("remove Download only")
    );
    let fallback = fixture
        .database
        .resolve_local_access(
            fixture.source,
            Some(&track.object_id),
            &track.title,
            &track.display_album,
            &track.display_artist,
            track.disc_number,
            track.track_number,
            track.duration_millis,
        )
        .await
        .expect("mapping fallback")
        .expect("mapping remains");
    assert_eq!(fallback.local_access_file_key, mapping);
    assert_eq!(fallback.origin, LocalAccessOrigin::Mapping);
    assert!(
        !fixture
            .database
            .track_rows(fixture.source, &[track.track_key], &cancel)
            .await
            .expect("badge after Download deletion")[0]
            .is_downloaded
    );
}

#[tokio::test]
async fn selected_source_defaults_have_the_three_activity_smart_playlists() {
    let fixture = fixture().await;
    assert!(
        fixture
            .database
            .ensure_default_smart_playlists(fixture.source)
            .await
            .expect("install default Smart Playlists")
    );
    assert!(
        !fixture
            .database
            .ensure_default_smart_playlists(fixture.source)
            .await
            .expect("default Smart Playlists are idempotent")
    );
    let cancellation = ReadCancellation::new();
    let (order, rows) = fixture
        .database
        .smart_playlist_route_page(
            fixture.source,
            None,
            SmartPlaylistListSort::Position,
            false,
            0,
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
        .create_smart_playlist(fixture.source, "Never Played", &never)
        .await
        .expect("create Never Played");
    let policy_definition = SmartPlaylistDefinition {
        match_all: SmartPlaylistRuleField::ALL
            .into_iter()
            .map(SmartPlaylistRuleField::default_rule)
            .collect(),
        ..SmartPlaylistDefinition::default()
    };
    let policy = fixture
        .database
        .create_smart_playlist(fixture.source, "Policy Defaults", &policy_definition)
        .await
        .expect("Library-owned editor defaults pass Library validation");
    assert_eq!(
        fixture
            .database
            .smart_playlist_track_order(fixture.source, smart, None, 0, &cancel)
            .await
            .expect("Never Played membership")
            .len(),
        4
    );

    let mut raw = connection(&fixture.path).await;
    sqlx::query(
        "INSERT INTO activity_baseline(
             source_key, track_object_id, play_count, skip_count, last_played_at
         ) VALUES (?1, 'track-0', 2, 0, 1)",
    )
    .bind(fixture.source)
    .execute(&mut raw)
    .await
    .expect("insert lifetime baseline");
    for (track, object_id, date) in [
        (fixture.tracks[0], "track-0", "2025-06-12"),
        (fixture.tracks[1], "track-1", "2025-06-18"),
        (fixture.tracks[2], "track-2", "2025-05-25"),
        (fixture.tracks[3], "track-3", "2024-06-19"),
    ] {
        sqlx::query(
            "INSERT INTO listens(
                 source_key, track_key, track_object_id, track_title,
                 artist_name, album_title, started_at, local_period, duration_millis, listened_millis, skipped
             ) VALUES (?1, ?2, ?3, 'Track', 'Artist', 'Album',
                       CAST(strftime('%s', ?4) AS INTEGER), substr(?4,1,7), 180000, 180000, 0)",
        )
        .bind(fixture.source)
        .bind(track)
        .bind(object_id)
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
                .update_smart_playlist(fixture.source, smart, "Played", &played)
                .await
                .expect("update period template")
        );
        assert_eq!(
            fixture
                .database
                .smart_playlist_track_order(fixture.source, smart, None, now, &cancel)
                .await
                .expect("period membership")
                .len(),
            expected
        );
    }
    let facts = fixture
        .database
        .smart_playlist_rows(fixture.source, &[smart], None, now, &cancel)
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
        .create_smart_playlist(fixture.source, "Optional", &optional)
        .await
        .expect("create second Smart Playlist");
    assert_eq!(
        fixture
            .database
            .smart_playlist_track_order(fixture.source, second, None, now, &cancel)
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
            .update_smart_playlist(fixture.source, second, "Optional", &optional)
            .await
            .expect("update optional rule")
    );
    assert_eq!(
        fixture
            .database
            .smart_playlist_track_order(fixture.source, second, None, now, &cancel)
            .await
            .expect("missing optional value matches IsEmpty"),
        [fixture.tracks[0]]
    );
    let smart_rows = fixture
        .database
        .smart_playlist_rows(fixture.source, &[smart, second], None, now, &cancel)
        .await
        .expect("consistent Smart Playlist rows");
    assert_eq!(
        smart_rows[0].track_count as usize,
        fixture
            .database
            .smart_playlist_track_order(fixture.source, smart, None, now, &cancel)
            .await
            .expect("first Smart membership")
            .len()
    );
    assert_eq!(
        smart_rows[1].track_count as usize,
        fixture
            .database
            .smart_playlist_track_order(fixture.source, second, None, now, &cancel)
            .await
            .expect("second Smart membership")
            .len()
    );
    assert_eq!(
        fixture
            .database
            .smart_playlist_route_page(
                fixture.source,
                None,
                SmartPlaylistListSort::TrackCount,
                true,
                now,
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
                fixture.source,
                None,
                SmartPlaylistListSort::Duration,
                true,
                now,
                &cancel,
            )
            .await
            .expect("duration-sorted Smart Playlist order")
            .0,
        [smart, second, policy]
    );
    let smart_plan = sqlx::query_as::<_, (i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT smart_playlist_key FROM smart_playlists WHERE source_key=?1 ORDER BY position,smart_playlist_key")
        .bind(fixture.source).fetch_all(&mut raw).await.expect("production Smart Playlist order plan").into_iter().map(|row| row.3).collect::<Vec<_>>().join(" | ");
    assert!(
        smart_plan.contains("sqlite_autoindex_smart_playlists_2"),
        "{smart_plan}"
    );
    assert!(
        !smart_plan.contains("USE TEMP B-TREE FOR ORDER BY"),
        "{smart_plan}"
    );
    let window_plan = sqlx::query_as::<_, (i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT track_key FROM listens WHERE source_key=?1 AND started_at>=?2 AND started_at<=?3 ORDER BY started_at DESC")
        .bind(fixture.source).bind(now-31_536_000).bind(now).fetch_all(&mut raw).await.expect("rolling Activity window plan").into_iter().map(|row| row.3).collect::<Vec<_>>().join(" | ");
    assert!(window_plan.contains("listens_history_idx"), "{window_plan}");
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
        .home_page(fixture.source, None, 0, &cancel)
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
    let alternate_showcase = fixture
        .database
        .home_page(fixture.source, None, 1, &cancel)
        .await
        .expect("alternate launch Home page")
        .showcase
        .expect("alternate Showcase");
    assert!(matches!(
        initial_home.showcase,
        Some(library::HomeShowcaseRow::Album(_))
    ));
    assert!(matches!(
        alternate_showcase,
        library::HomeShowcaseRow::Track(_)
    ));
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE albums SET date_added=NULL,first_seen_at=CASE album_key WHEN ?1 THEN 200 ELSE 100 END")
        .bind(fixture.albums[0]).execute(&mut raw).await.expect("establish Local first-seen facts");
    let newly_added = fixture
        .database
        .home_page(fixture.source, None, 0, &cancel)
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
            .home_page(fixture.source, None, 0, &cancel)
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
            .home_page(fixture.source, None, 0, &cancel)
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
            &[fixture.tracks[0]],
            2,
            &cancel,
        )
        .await
        .expect("bounded random candidates");
    assert_eq!(random.len(), 2);
    assert!(!random.contains(&fixture.tracks[0]));
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
    sqlx::query("INSERT INTO queue_occurrences(source_key,object_id,position,traversal_position,provenance_kind,track_key,track_object_id) VALUES (?1,'radio-queued',0,0,'manual',?2,'track-0')")
        .bind(fixture.source).bind(fixture.tracks[0]).execute(&mut raw).await.expect("persist complete Radio queue exclusion");
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
                &vec![fixture.tracks[1]; 501],
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
            .all(|track| fixture.tracks[2..].contains(track))
    );
    let playlist = fixture
        .database
        .create_playlist(fixture.source, "Seed", &[fixture.tracks[0]])
        .await
        .expect("create Radio seed Playlist")
        .unwrap();
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
    assert!(!playlist_radio.contains(&fixture.tracks[0]));
    assert_ne!(playlist_radio, [fixture.tracks[0]]);
    for sql in [
        "EXPLAIN QUERY PLAN SELECT track_key FROM tracks WHERE source_key=?1 AND track_key>=?2 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=tracks.track_key AND scope.folder_key=?3)) ORDER BY track_key LIMIT 24",
        "EXPLAIN QUERY PLAN SELECT album_key FROM albums WHERE source_key=?1 AND album_key>=?2 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=albums.album_key AND scope.folder_key=?3)) ORDER BY album_key LIMIT 1",
        "EXPLAIN QUERY PLAN SELECT track.track_key FROM tracks track WHERE track.source_key=?1 AND track.track_key>?2 AND EXISTS (SELECT 1 FROM track_genres relation WHERE relation.track_key=track.track_key AND relation.genre_key=?3) AND NOT EXISTS (SELECT 1 FROM queue_occurrences queued WHERE queued.source_key=?1 AND queued.track_key=track.track_key) ORDER BY track.track_key LIMIT 500",
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
        if sql.contains("queue_occurrences") {
            assert!(details.contains("queue_occurrences_track_idx"), "{details}");
        }
    }
}
