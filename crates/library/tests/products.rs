use library::{
    RadioSeed, ReadCancellation, SmartPlaylistActivityPeriod, SmartPlaylistDefinition,
    SmartPlaylistRule, SmartPlaylistRuleField, SmartPlaylistRuleOperator, SmartPlaylistRuleValue,
    SmartPlaylistSort,
};

use super::support::{connection, fixture};

#[tokio::test]
async fn radio_fills_short_album_and_track_seeds_from_the_selected_source() {
    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    // The two albums have distinct artists and no shared genre.
    for sql in [
        "DELETE FROM track_links WHERE role=2",
        "DELETE FROM album_links WHERE role=2",
    ] {
        sqlx::query(sql).execute(&mut raw).await.unwrap();
    }
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let cancel = ReadCancellation::new();
    for variation in [0, 2, 4, 99] {
        for seed in [
            RadioSeed::Album(fixture.albums[0]),
            RadioSeed::Track(fixture.track_uris[0].clone()),
        ] {
            let mut candidates = fixture
                .catalog
                .radio_candidates(
                    fixture.source,
                    seed.clone(),
                    &[],
                    20,
                    false,
                    variation,
                    &cancel,
                )
                .await
                .unwrap();
            if matches!(seed, RadioSeed::Track(_)) {
                assert_eq!(candidates.first(), Some(&fixture.tracks[1]));
            }
            candidates.sort();
            let mut expected = fixture.tracks[if matches!(seed, RadioSeed::Album(_)) {
                2
            } else {
                1
            }..]
                .to_vec();
            expected.sort();
            assert_eq!(candidates, expected, "{seed:?}, variation {variation}");
        }
    }
    sqlx::query("INSERT INTO queue_entries(track_key,rank,provenance) SELECT track_key,X'8000','\"Manual\"' FROM tracks WHERE media_uri=?1")
        .bind(&fixture.track_uris[1])
        .execute(&mut raw)
        .await
        .unwrap();
    for seed in [
        RadioSeed::Album(fixture.albums[0]),
        RadioSeed::Track(fixture.track_uris[0].clone()),
    ] {
        assert_eq!(
            fixture
                .catalog
                .radio_candidates(
                    fixture.source,
                    seed,
                    &[fixture.tracks[2]],
                    20,
                    false,
                    3,
                    &cancel,
                )
                .await
                .unwrap(),
            vec![fixture.tracks[3]]
        );
    }
}

#[tokio::test]
async fn radio_resolves_playlist_ownership_and_filters_native_recommendations() {
    let mut fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO sources(object_id,display_name) VALUES ('other','Other')")
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tracks(source_key,object_id,media_uri,title,sort_text,duration_millis,indexed) SELECT source_key,'track-2','test:other','Other','other',1000,1 FROM sources WHERE object_id='other'")
        .execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO playlists(source_key,owner,object_id,name,writable) VALUES (?1,'source','native','Native',0)")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO playlist_entries(playlist_key,object_id,track_key,rank) SELECT playlist_key,'entry',(SELECT track_key FROM tracks WHERE media_uri=?1),X'8000' FROM playlists WHERE object_id='native'")
        .bind(&fixture.track_uris[0]).execute(&mut raw).await.unwrap();
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let native = fixture
        .catalog
        .playlist_key_by_object(fixture.source, "native", &cancel)
        .await
        .unwrap()
        .unwrap();
    let local = fixture
        .catalog
        .create_playlist(
            "Mixed",
            &library::MemberSelection::Uris(
                ((&[fixture.track_uris[0].clone(), "test:other".into()]).to_vec()).into(),
            ),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
    let (owned, _) = fixture
        .catalog
        .create_playlist(
            "Owned",
            &library::MemberSelection::Uris(((&fixture.track_uris[..1]).to_vec()).into()),
        )
        .await
        .unwrap()
        .unwrap();
    sqlx::query("INSERT INTO queue_entries(track_key,rank,provenance) SELECT track_key,X'8000','\"Manual\"' FROM tracks WHERE media_uri=?1")
        .bind(&fixture.track_uris[1])
        .execute(&mut raw)
        .await
        .unwrap();
    for (seed, object_id) in [
        (RadioSeed::Playlist(native), Some("native")),
        (RadioSeed::Playlist(local), None),
        (RadioSeed::Playlist(owned), None),
        (RadioSeed::Track(fixture.track_uris[0].clone()), None),
        (RadioSeed::Album(fixture.albums[0]), Some("album-a")),
        (RadioSeed::Artist(fixture.artists[0]), Some("artist-a")),
        (RadioSeed::AlbumArtist(fixture.artists[0]), Some("artist-a")),
        (RadioSeed::Genre(fixture.genre), Some("genre")),
    ] {
        assert_eq!(
            fixture.catalog.radio_source(&seed, &cancel).await.unwrap(),
            Some((
                fixture.source,
                library::SourceId::new("source"),
                object_id.map(String::from)
            ))
        );
        let ids = ["track-2", "track-1", "track-2", "missing", "track-3"].map(String::from);
        assert_eq!(
            fixture
                .catalog
                .recommended_track_keys(fixture.source, &seed, &ids, &cancel)
                .await
                .unwrap(),
            fixture.tracks[2..]
        );
    }
    for seed in [
        RadioSeed::Playlist(native),
        RadioSeed::Playlist(local),
        RadioSeed::Track(fixture.track_uris[0].clone()),
        RadioSeed::Album(fixture.albums[0]),
    ] {
        assert!(
            fixture
                .catalog
                .recommended_track_keys(fixture.source, &seed, &["track-0".into()], &cancel)
                .await
                .unwrap()
                .is_empty()
        );
        let result = fixture
            .catalog
            .radio_candidates(fixture.source, seed, &[], 20, false, 0, &cancel)
            .await
            .unwrap();
        assert_eq!(result, fixture.tracks[2..]);
    }
}

#[tokio::test]
async fn radio_prefers_related_album_and_track_metadata_before_source_fill() {
    let mut fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    // Candidate tracks retain only album genres; the seed retains only a track genre.
    for sql in [
        "DELETE FROM track_links WHERE role IN (0,1)",
        "DELETE FROM album_links WHERE role=0",
    ] {
        sqlx::query(sql).execute(&mut raw).await.unwrap();
    }
    sqlx::query("DELETE FROM album_links WHERE role=2 AND album_key=?1")
        .bind(fixture.albums[0])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_links WHERE role=2 AND track_key<>?1")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .unwrap();
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    assert_eq!(
        fixture
            .catalog
            .radio_candidates(
                fixture.source,
                RadioSeed::Track(fixture.track_uris[0].clone()),
                &[],
                1,
                false,
                0,
                &cancel
            )
            .await
            .unwrap(),
        vec![fixture.tracks[2]]
    );
    // Album radio also recognizes the candidate's direct track genre.
    assert_eq!(
        fixture
            .catalog
            .radio_candidates(
                fixture.source,
                RadioSeed::Album(fixture.albums[1]),
                &[],
                1,
                false,
                2,
                &cancel
            )
            .await
            .unwrap(),
        vec![fixture.tracks[0]]
    );
}

#[tokio::test]
async fn smart_playlists_exclude_known_media_until_it_is_indexed() {
    let mut fixture = fixture().await;
    let direct = "https://media.example/direct.flac".to_string();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO tracks(media_uri,indexed,title,display_artist,display_album,duration_millis) VALUES (?1,0,COALESCE('Direct Signal',''),COALESCE('Outside Artist',''),COALESCE('Outside Album',''),COALESCE(42000,0))")
        .bind(&direct).execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO queue_entries(track_key,rank,provenance) SELECT track_key,X'8000','\"Manual\"' FROM tracks WHERE media_uri=?1")
        .bind(&direct).execute(&mut raw).await.unwrap();
    drop(raw);
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let (playlist, _) = fixture
        .catalog
        .create_playlist(
            "Direct",
            &library::MemberSelection::Uris(((std::slice::from_ref(&direct)).to_vec()).into()),
        )
        .await
        .expect("create global Playlist")
        .expect("Playlist key");
    assert_eq!(
        fixture
            .catalog
            .add_playlist_tracks(
                playlist,
                &library::MemberSelection::Uris(((std::slice::from_ref(&direct)).to_vec()).into()),
                false
            )
            .await
            .expect("append durable media directly"),
        1
    );
    assert_eq!(
        fixture
            .catalog
            .add_playlist_tracks(
                playlist,
                &library::MemberSelection::Uris(((std::slice::from_ref(&direct)).to_vec()).into()),
                true
            )
            .await
            .expect("skip an existing durable identity"),
        0
    );
    let mut definition = SmartPlaylistDefinition {
        source_id: None,
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
        .catalog
        .create_smart_playlist("Direct", &definition, None)
        .await
        .expect("create All Smart Playlist");
    let cancel = ReadCancellation::new();
    let all = fixture
        .catalog
        .selected_track_uris(
            &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                .selection(),
            None,
            &cancel,
        )
        .await
        .expect("evaluate All Smart Playlist");
    assert!(all.is_empty());

    definition.source_id = Some(library::SourceId::new("source"));
    fixture
        .catalog
        .update_smart_playlist(smart, "Direct", &definition, None)
        .await
        .expect("make Smart Playlist Current");
    assert!(
        fixture
            .catalog
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancel
            )
            .await
            .expect("evaluate Current Smart Playlist")
            .is_empty()
    );
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET source_key=?1,object_id='direct',indexed=1 WHERE media_uri=?2")
        .bind(fixture.source)
        .bind(&direct)
        .execute(&mut raw)
        .await
        .unwrap();
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    assert_eq!(
        fixture
            .catalog
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancel
            )
            .await
            .unwrap(),
        std::slice::from_ref(&direct)
    );
    definition.source_id = None;
    fixture
        .catalog
        .update_smart_playlist(smart, "Direct", &definition, None)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .catalog
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancel
            )
            .await
            .unwrap(),
        [direct]
    );
}

#[tokio::test]
async fn completed_direct_download_keeps_access_without_smart_membership() {
    let fixture = fixture().await;
    let direct = "https://media.example/download.flac".to_string();
    fixture
        .catalog
        .write_direct_track_metadata(
            &direct,
            &library::TrackMetadata {
                title: "Direct artifact".into(),
                duration_millis: 42_000,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture
        .catalog
        .save_download(&library::DownloadFile {
            media_uri: direct.clone(),
            encoding: "original".into(),
            path: "file:///downloads/direct.flac".into(),
            storage_root: "/downloads".into(),
            relative_path: "direct.flac".into(),
            completed_size: Some(10),
            managed: true,
            ..Default::default()
        })
        .await
        .expect("record completed direct download");
    assert_eq!(
        fixture
            .catalog
            .playback_access(&direct)
            .await
            .expect("resolve direct download access")
            .map(|(uri, _)| uri)
            .as_deref(),
        Some("file:///downloads/direct.flac")
    );

    let mut definition = SmartPlaylistDefinition {
        source_id: None,
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::Title,
            operator: SmartPlaylistRuleOperator::Contains,
            value: Some(SmartPlaylistRuleValue::Text("artifact".to_string())),
        }],
        ..SmartPlaylistDefinition::default()
    };
    let smart = fixture
        .catalog
        .create_smart_playlist("Downloaded direct", &definition, None)
        .await
        .expect("create direct download Smart Playlist");
    let cancel = ReadCancellation::new();
    assert_eq!(
        fixture
            .catalog
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancel
            )
            .await
            .expect("evaluate All direct download"),
        Vec::<String>::new()
    );
    definition.source_id = Some(library::SourceId::new("source"));
    fixture
        .catalog
        .update_smart_playlist(smart, "Downloaded direct", &definition, None)
        .await
        .expect("make direct download Smart Playlist Current");
    assert!(
        fixture
            .catalog
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancel
            )
            .await
            .expect("evaluate Current direct download")
            .is_empty()
    );
}

#[tokio::test]
async fn canonical_rows_keep_uri_order_after_smart_membership_changes() {
    let mut fixture = fixture().await;
    let direct = "https://media.example/window.flac".to_string();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO tracks(media_uri,indexed,title,display_artist,display_album,duration_millis) VALUES (?1,0,COALESCE('Known title',''),COALESCE('Artist',''),COALESCE('Album',''),COALESCE(42000,0))")
        .bind(&direct).execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped) VALUES(?1,'Historical title','Artist','Album',100,'1970-01',42000,42000,0)")
        .bind(&direct).execute(&mut raw).await.unwrap();
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let mut definition = SmartPlaylistDefinition::default();
    let key = fixture
        .catalog
        .create_smart_playlist("Window", &definition, None)
        .await
        .unwrap();
    let cancel = ReadCancellation::new();
    let order = fixture
        .catalog
        .selected_track_uris(
            &library::MemberRead::new(library::Collection::Smart { key: key, now: 500 })
                .selection(),
            None,
            &cancel,
        )
        .await
        .unwrap();
    assert!(!order.contains(&direct));
    let detail = fixture
        .catalog
        .smart_playlist_rows(&[key], 500, &cancel)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(detail.track_count as usize, order.len());
    definition.match_all.push(SmartPlaylistRule {
        field: SmartPlaylistRuleField::Title,
        operator: SmartPlaylistRuleOperator::Equals,
        value: Some(SmartPlaylistRuleValue::Text("No match".into())),
    });
    fixture
        .catalog
        .update_smart_playlist(key, "Window", &definition, None)
        .await
        .unwrap();
    let requested = vec![direct.clone(), fixture.track_uris[0].clone(), direct];
    let rows = fixture
        .catalog
        .tracks_by_uris(&((&requested).to_vec()), &cancel)
        .await
        .unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>(),
        requested
    );
    assert_eq!(rows[0].metadata.title, "Known title");
    assert_eq!(rows[1].metadata.title, "Alpha");
    assert_eq!(rows[2].metadata.duration_millis, 42000);
    assert_eq!(rows[0].state.play_count, 1);
    sqlx::query("UPDATE tracks SET indexed=0 WHERE media_uri=?1")
        .bind(&fixture.track_uris[0])
        .execute(&mut raw)
        .await
        .unwrap();
    let rows = fixture
        .catalog
        .tracks_by_uris(&((&requested).to_vec()), &cancel)
        .await
        .unwrap();
    assert_eq!(rows[1].metadata.title, "Alpha");
}

#[tokio::test]
async fn global_playlist_keeps_unindexed_media_outside_smart_membership() {
    let mut fixture = fixture().await;
    let retained = fixture.track_uris[0].clone();
    let (playlist, _) = fixture
        .catalog
        .create_playlist(
            "Retained",
            &library::MemberSelection::Uris(((std::slice::from_ref(&retained)).to_vec()).into()),
        )
        .await
        .expect("create global Playlist")
        .expect("Playlist key");
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET indexed=0 WHERE media_uri=?1")
        .bind(&retained)
        .execute(&mut raw)
        .await
        .expect("remove retained media from the catalog");
    drop(raw);
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let mut definition = SmartPlaylistDefinition {
        source_id: None,
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::Title,
            operator: SmartPlaylistRuleOperator::Equals,
            value: Some(SmartPlaylistRuleValue::Text("Alpha".to_string())),
        }],
        ..SmartPlaylistDefinition::default()
    };
    let smart = fixture
        .catalog
        .create_smart_playlist("Retained", &definition, None)
        .await
        .expect("create retained-source Smart Playlist");
    let cancellation = ReadCancellation::new();
    assert_eq!(
        fixture
            .catalog
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancellation
            )
            .await
            .expect("evaluate All while source is retained"),
        Vec::<String>::new()
    );
    definition.source_id = Some(library::SourceId::new("source"));
    fixture
        .catalog
        .update_smart_playlist(smart, "Retained", &definition, None)
        .await
        .expect("make retained-source Smart Playlist Current");
    assert_eq!(
        fixture
            .catalog
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancellation
            )
            .await
            .expect("evaluate Current while source is retained"),
        Vec::<String>::new()
    );

    fixture
        .catalog
        .remove_source(&library::SourceId::new("source"))
        .await
        .expect("forget source");
    assert!(
        fixture
            .catalog
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancellation
            )
            .await
            .unwrap()
            .is_empty()
    );
    definition.source_id = None;
    fixture
        .catalog
        .update_smart_playlist(smart, "Retained", &definition, None)
        .await
        .expect("make retained-source Smart Playlist All");
    assert_eq!(
        fixture
            .catalog
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancellation
            )
            .await
            .expect("evaluate All after Forget"),
        Vec::<String>::new()
    );
    assert_eq!(
        fixture
            .catalog
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Playlist(playlist)).selection(),
                None,
                &cancellation
            )
            .await
            .expect("read retained Playlist reference"),
        [retained]
    );
}

#[tokio::test]
async fn smart_membership_excludes_retained_queue_and_activity() {
    let mut fixture = fixture().await;
    let queue_uri =
        library::source_entity_uri(&library::SourceId::new("absent"), "track", "queued");
    let listen_uri =
        library::source_entity_uri(&library::SourceId::new("absent"), "track", "listened");
    let mut raw = connection(&fixture.path).await;
    sqlx::query(
        "INSERT INTO tracks(media_uri,indexed,title) VALUES (?1,0,COALESCE('Queued','')),(?2,0,COALESCE('Listened',''))",
    )
    .bind(&queue_uri)
    .bind(&listen_uri)
    .execute(&mut raw)
    .await
    .unwrap();
    sqlx::query("INSERT INTO queue_entries(track_key,rank,provenance) SELECT track_key,X'8000','\"Manual\"' FROM tracks WHERE media_uri=?1")
        .bind(&queue_uri).execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO listens(external_id,source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis) VALUES('retained','absent',?1,'Listened','Artist','Album',1,'1970-01',1000,1000)")
        .bind(&listen_uri).execute(&mut raw).await.unwrap();
    sqlx::query("UPDATE tracks SET indexed=0 ")
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let key = fixture
        .catalog
        .create_smart_playlist(
            "All",
            &SmartPlaylistDefinition {
                source_id: None,
                ..SmartPlaylistDefinition::default()
            },
            None,
        )
        .await
        .unwrap();
    let cancellation = ReadCancellation::new();
    assert_eq!(
        fixture
            .catalog
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: key, now: 10 })
                    .selection(),
                None,
                &cancellation
            )
            .await
            .unwrap(),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn artist_and_home_activity_follow_membership_even_without_listen_source_ids() {
    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE sources SET kind='local' WHERE source_key=?1")
        .bind(fixture.source)
        .execute(&mut raw)
        .await
        .unwrap();
    for (uri, time) in [
        (fixture.track_uris[0].as_str(), 100),
        (fixture.track_uris[0].as_str(), 200),
        (fixture.track_uris[2].as_str(), 300),
        ("file:///outside-current.flac", 400),
    ] {
        sqlx::query("INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis) VALUES(?1,'Track','Artist','Album',?2,'2026-09',1000,1000)")
            .bind(uri).bind(time).execute(&mut raw).await.unwrap();
    }
    sqlx::query("DELETE FROM track_links WHERE role=4 AND track_key=?1")
        .bind(fixture.tracks[2])
        .execute(&mut raw)
        .await
        .unwrap();
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let cancel = ReadCancellation::new();
    for album_artist in [false, true] {
        let rows = fixture
            .catalog
            .artists(&[fixture.artists[0]], album_artist, true, &cancel)
            .await
            .unwrap();
        assert_eq!(rows[0].state.play_count, 2);
        assert_eq!(rows[0].state.last_played, Some(200));
    }
    let home = fixture
        .catalog
        .home_page(
            fixture.source,
            Some(fixture.folder),
            0,
            0,
            &library::HomeBlockKind::all(),
            &cancel,
        )
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

    sqlx::query("INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at)
        SELECT source.object_id,'lifetime','track',track.object_id,
            CASE track.track_key WHEN ?2 THEN 4 WHEN ?3 THEN 1 ELSE 20 END,0,
            CASE track.track_key WHEN ?2 THEN 50 WHEN ?3 THEN 500 ELSE 600 END
        FROM tracks track JOIN sources source USING(source_key)
        WHERE track.source_key=?1 AND track.track_key IN(?2,?3,?4)")
        .bind(fixture.source).bind(fixture.tracks[0]).bind(fixture.tracks[1]).bind(fixture.tracks[2])
        .execute(&mut raw).await.unwrap();
    sqlx::query("UPDATE tracks SET native_play_count=9,native_last_played=700 WHERE track_key=?1")
        .bind(fixture.tracks[1])
        .execute(&mut raw)
        .await
        .unwrap();
    for kind in ["local", "files", "webdav", "smb", "documents", "subsonic"] {
        sqlx::query("UPDATE sources SET kind=?2 WHERE source_key=?1")
            .bind(fixture.source)
            .bind(kind)
            .execute(&mut raw)
            .await
            .unwrap();
        fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
        let home = fixture
            .catalog
            .home_page(
                fixture.source,
                Some(fixture.folder),
                0,
                0,
                &[
                    library::HomeBlockKind::MostPlayed,
                    library::HomeBlockKind::RecentlyPlayed,
                ],
                &cancel,
            )
            .await
            .unwrap();
        let most = home
            .most_played
            .tracks
            .iter()
            .map(|row| (row.track.track_key, row.track.state.play_count))
            .collect::<Vec<_>>();
        let recent = home
            .recently_played
            .tracks
            .iter()
            .map(|row| (row.track.track_key, row.track.state.last_played))
            .collect::<Vec<_>>();
        if kind == "subsonic" {
            assert_eq!(most, [(fixture.tracks[1], 9)]);
            assert_eq!(recent, [(fixture.tracks[1], Some(700))]);
        } else {
            assert_eq!(most, [(fixture.tracks[0], 6), (fixture.tracks[1], 1)]);
            assert_eq!(
                recent,
                [
                    (fixture.tracks[1], Some(500)),
                    (fixture.tracks[0], Some(200))
                ]
            );
        }
    }
}

#[tokio::test]
async fn home_reads_only_enabled_blocks_and_preserves_provider_sections() {
    use library::{HomeBlockKind, HomePage};

    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE sources SET kind='local' WHERE source_key=?1")
        .bind(fixture.source)
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis) VALUES(?1,'Track','Artist','Album',100,'2026-09',1000,1000)")
        .bind(&fixture.track_uris[0])
        .execute(&mut raw)
        .await
        .unwrap();
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let cancel = ReadCancellation::new();
    let all = fixture
        .catalog
        .home_page(fixture.source, None, 0, 0, &HomeBlockKind::all(), &cancel)
        .await
        .unwrap();
    assert!(!all.provider_sections.is_empty());
    assert!(all.showcase.is_some());
    assert!(!all.explore.is_empty());
    assert!(!all.most_played.tracks.is_empty());
    assert!(!all.newly_added.albums.is_empty());
    assert!(!all.recently_played.tracks.is_empty());
    assert!(!all.recently_released.albums.is_empty());
    assert!(!all.genres.is_empty());

    for block in HomeBlockKind::all() {
        let selected = fixture
            .catalog
            .home_page(fixture.source, None, 0, 0, &[block], &cancel)
            .await
            .unwrap();
        let mut expected = HomePage {
            provider_sections: all.provider_sections.clone(),
            ..HomePage::default()
        };
        match block {
            HomeBlockKind::Showcase => expected.showcase = all.showcase.clone(),
            HomeBlockKind::Explore => expected.explore = all.explore.clone(),
            HomeBlockKind::MostPlayed => expected.most_played = all.most_played.clone(),
            HomeBlockKind::NewlyAdded => expected.newly_added = all.newly_added.clone(),
            HomeBlockKind::RecentlyPlayed => expected.recently_played = all.recently_played.clone(),
            HomeBlockKind::RecentlyReleased => {
                expected.recently_released = all.recently_released.clone()
            }
            HomeBlockKind::Genres => expected.genres = all.genres.clone(),
        }
        assert_eq!(
            selected, expected,
            "only {block:?} and provider sections are read"
        );
    }
    let disabled = fixture
        .catalog
        .home_page(fixture.source, None, 0, 0, &[], &cancel)
        .await
        .unwrap();
    assert_eq!(
        disabled,
        HomePage {
            provider_sections: all.provider_sections.clone(),
            ..HomePage::default()
        }
    );
    let enabled_again = fixture
        .catalog
        .home_page(fixture.source, None, 0, 0, &HomeBlockKind::all(), &cancel)
        .await
        .unwrap();
    assert_eq!(
        enabled_again, all,
        "disabling presentation keeps stored Home facts"
    );
}
