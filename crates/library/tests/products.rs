use library::{
    PlayedFilter, RadioSeed, RandomCriteria, ReadCancellation, SearchRequest,
    SmartPlaylistActivityPeriod, SmartPlaylistDefinition, SmartPlaylistListSort, SmartPlaylistRule,
    SmartPlaylistRuleField, SmartPlaylistRuleOperator, SmartPlaylistRuleValue, SmartPlaylistSort,
};

use super::support::{connection, fixture};

#[tokio::test]
async fn smart_playlist_periods_and_never_played_query_sqlite_directly() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
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
                 artist_name, album_title, started_at, duration_millis, listened_millis, skipped
             ) VALUES (?1, ?2, ?3, 'Track', 'Artist', 'Album',
                       CAST(strftime('%s', ?4) AS INTEGER), 180000, 180000, 0)",
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
            .smart_playlist_order(
                fixture.source,
                None,
                SmartPlaylistListSort::TrackCount,
                true,
                now,
                &cancel,
            )
            .await
            .expect("sorted Smart Playlist order"),
        [smart, second]
    );
    assert_eq!(
        fixture
            .database
            .smart_playlist_order(
                fixture.source,
                None,
                SmartPlaylistListSort::Duration,
                true,
                now,
                &cancel,
            )
            .await
            .expect("duration-sorted Smart Playlist order"),
        [smart, second]
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
    assert!(
        fixture
            .database
            .track_filter_order(fixture.source, None, "artist a", &cancel)
            .await
            .expect("complete Track filter")
            .len()
            >= 2
    );
    assert!(
        fixture
            .database
            .home_most_played_tracks(fixture.source, None, &cancel)
            .await
            .expect("zero-play Home section")
            .is_empty()
    );
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE albums SET date_added=NULL,first_seen_at=CASE album_key WHEN ?1 THEN 200 ELSE 100 END")
        .bind(fixture.albums[0]).execute(&mut raw).await.expect("establish Local first-seen facts");
    let newly_added = fixture
        .database
        .home_newly_added_albums(fixture.source, None, &cancel)
        .await
        .expect("bounded Home Albums");
    assert_eq!(newly_added.len(), 2);
    assert_eq!(newly_added[0].album_key, fixture.albums[0]);
    sqlx::query("UPDATE albums SET release_date=NULL,year=0 WHERE album_key=?1")
        .bind(fixture.albums[1])
        .execute(&mut raw)
        .await
        .expect("remove release fact");
    assert_eq!(
        fixture
            .database
            .home_recently_released_albums(fixture.source, None, &cancel)
            .await
            .expect("released Albums require a release fact")
            .iter()
            .map(|row| row.album_key)
            .collect::<Vec<_>>(),
        [fixture.albums[0]]
    );
    assert_eq!(
        fixture
            .database
            .home_featured_genres(fixture.source, None, &cancel)
            .await
            .expect("bounded Home Genres")
            .len(),
        1
    );
    assert_eq!(
        fixture
            .database
            .provider_home_tracks(fixture.source, "featured", None, 24, &cancel)
            .await
            .expect("provider Home Tracks")
            .len(),
        1
    );

    let random = fixture
        .database
        .random_candidates(
            fixture.source,
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
