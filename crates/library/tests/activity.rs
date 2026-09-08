use library::{
    CalendarActivityPeriod, ListenDeliveryTarget, ListenWrite, ReadCancellation, SourceId,
};

use super::support::{connection, fixture};

#[tokio::test]
async fn listens_accept_empty_optional_recording_ids() {
    let fixture = fixture().await;
    let listen = ListenWrite {
        external_id: Some("optional-recording-ids".into()),
        media_uri: fixture.track_uris[0].clone(),
        title: "Playable song".into(),
        artist: "Artist".into(),
        album: "Album".into(),
        duration_millis: 1000,
        disc_number: None,
        track_number: None,
        year: None,
        release_date: None,
        source_format: None,
        musicbrainz_recording_id: Some(String::new()),
        musicbrainz_release_track_id: Some(String::new()),
        started_at: 1_700_000_000,
        local_period: "2023-11".into(),
        listened_millis: 1000,
        skipped: false,
    };
    fixture.database.record_listen(&listen, &[]).await.unwrap();
    let record = library::ActivityRecord {
        version: 1,
        listen_key: 100,
        source_id: None,
        listen: ListenWrite {
            external_id: Some("imported-optional-recording-ids".into()),
            ..listen
        },
    };
    let report = fixture
        .database
        .import_activity_jsonl(std::io::Cursor::new(serde_json::to_vec(&record).unwrap()))
        .await
        .unwrap();
    assert_eq!((report.accepted, report.skipped), (1, 0));
    let mut exported = Vec::new();
    fixture
        .database
        .export_activity_jsonl(&mut exported, None)
        .await
        .unwrap();
    let rows = String::from_utf8(exported)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<library::ActivityRecord>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(row.listen.title, "Playable song");
        assert_eq!(row.listen.musicbrainz_recording_id, None);
        assert_eq!(row.listen.musicbrainz_release_track_id, None);
    }
}

#[tokio::test]
async fn activity_keeps_one_listen_and_independent_delivery_targets() {
    let fixture = fixture().await;
    let listen = ListenWrite {
        external_id: Some("play-1".to_string()),
        media_uri: fixture.track_uris[0].clone(),
        title: "Stored Title".to_string(),
        artist: "Stored Artist".to_string(),
        album: "Stored Album".to_string(),
        duration_millis: 180_000,
        disc_number: None,
        track_number: None,
        year: None,
        release_date: None,
        source_format: None,
        musicbrainz_recording_id: None,
        musicbrainz_release_track_id: None,
        started_at: 1_700_000_000,
        local_period: "2023-11".to_string(),
        listened_millis: 90_000,
        skipped: true,
    };
    let targets = (0..12)
        .map(|index| ListenDeliveryTarget {
            service: match index % 3 {
                0 => "lastfm",
                1 => "listenbrainz",
                _ => "librefm",
            }
            .to_string(),
            account_id: format!("account-{index}"),
            next_attempt_at: Some(10),
        })
        .collect::<Vec<_>>();
    let key = fixture
        .database
        .record_listen(&listen, &targets)
        .await
        .expect("record listen");
    assert_eq!(
        fixture
            .database
            .record_listen(&listen, &targets)
            .await
            .expect("record listen idempotently"),
        key
    );
    let cancel = ReadCancellation::new();
    let history = fixture
        .database
        .activity_history(Some(&SourceId::new("source")), "", &cancel)
        .await
        .expect("Activity History");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].title, "Stored Title");
    assert_eq!(history[0].duration_millis, 180_000);
    let window = fixture
        .database
        .history_rows_by_uri(
            &[listen.media_uri.clone(), listen.media_uri.clone()],
            &cancel,
        )
        .await
        .expect("read repeated History URIs");
    assert_eq!(window.len(), 2);
    assert_eq!(window[0].title, "Stored Title");
    assert_eq!(window[1].media_uri, listen.media_uri);
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT local_play_count FROM tracks WHERE media_uri=?1")
            .bind(&listen.media_uri)
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1,
        "duplicate accepted listens must not increment the indexed local count"
    );
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT skipped FROM listens WHERE listen_key=?1")
            .bind(key)
            .fetch_one(&mut raw)
            .await
            .expect("read accepted skip")
    );
    sqlx::query("INSERT INTO catalog.activity_baseline(source_key,period,item_kind,track_object_id,play_count,skip_count,last_played_at) VALUES (?1,'lifetime','track','track-0',3,1,1600000000)")
        .bind(fixture.source)
        .execute(&mut raw)
        .await
        .expect("seed recovered Activity baseline");
    sqlx::query("INSERT INTO catalog.activity_baseline(source_key,period,item_kind,track_object_id,play_count,skip_count,last_played_at) VALUES (?1,'lifetime','track','track-1',2,0,1600000000)")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    drop(raw);
    let never = fixture
        .database
        .create_smart_playlist(
            "Local never played",
            &library::SmartPlaylistDefinition {
                match_all: vec![
                    library::SmartPlaylistRule {
                        field: library::SmartPlaylistRuleField::Played,
                        operator: library::SmartPlaylistRuleOperator::Is,
                        value: Some(library::SmartPlaylistRuleValue::Bool(false)),
                    },
                    library::SmartPlaylistRule {
                        field: library::SmartPlaylistRuleField::PlayCount,
                        operator: library::SmartPlaylistRuleOperator::Equals,
                        value: Some(library::SmartPlaylistRuleValue::Number(0)),
                    },
                ],
                ..library::SmartPlaylistDefinition::default()
            },
        )
        .await
        .unwrap();
    let unplayed = fixture
        .database
        .smart_playlist_media_uri_order(Some(fixture.source), never, None, 1800000000, &cancel)
        .await
        .unwrap();
    assert!(
        unplayed.contains(&fixture.track_uris[1]),
        "remote play totals do not count as Rufin listens"
    );
    assert!(!unplayed.contains(&listen.media_uri));
    let catalog = fixture
        .database
        .track_row_by_uri(&listen.media_uri, &cancel)
        .await
        .unwrap()
        .unwrap();
    let history = fixture
        .database
        .activity_history(None, "", &cancel)
        .await
        .unwrap();
    let row = &history[0];
    assert_eq!(row.album_media_uri, catalog.album_media_uri);
    assert_eq!(row.artists, catalog.artists);
    assert_eq!(row.album_artists, catalog.album_artists);
    assert_eq!(row.date_added, catalog.date_added);
    assert_eq!(row.bpm, catalog.bpm);
    assert_eq!(row.genre, "Rock");
    assert_eq!(row.play_count, 4);
    assert_eq!(
        fixture
            .database
            .history_rows_by_uri(std::slice::from_ref(&listen.media_uri), &cancel)
            .await
            .unwrap(),
        history
    );
    let (playlist, _) = fixture
        .database
        .create_playlist(
            None,
            "History track",
            std::slice::from_ref(&listen.media_uri),
        )
        .await
        .unwrap()
        .unwrap();
    let entries = fixture
        .database
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
    let entries = fixture
        .database
        .playlist_entry_rows(&entries, &cancel)
        .await
        .unwrap();
    let entry = &entries[0];
    assert_eq!(entry.album_media_uri, catalog.album_media_uri);
    assert_eq!(entry.artists, catalog.artists);
    assert_eq!(entry.album_artists, catalog.album_artists);
    assert_eq!(entry.date_added, catalog.date_added);
    assert_eq!(entry.bpm, catalog.bpm);
    assert_eq!(entry.genre, "Rock");
    assert_eq!(entry.play_count, catalog.play_count);
    assert_eq!(entry.last_played, catalog.last_played);
    let summary = fixture
        .database
        .calendar_activity_summary(
            fixture.source,
            CalendarActivityPeriod::Lifetime,
            100,
            &cancel,
        )
        .await
        .expect("lifetime Activity");
    assert_eq!(summary.tracks[0].track_key, fixture.tracks[0]);
    assert_eq!(summary.tracks[0].play_count, 4);
    let due = fixture
        .database
        .due_listen_deliveries(10, 100, &cancel)
        .await
        .expect("due listen deliveries");
    assert_eq!(due.len(), 12);
    assert_eq!(due[0].duration_millis, 180_000);
    assert_eq!(due[0].listened_millis, 90_000);
    assert!(
        fixture
            .database
            .complete_listen_delivery(due[0].outbox_key)
            .await
            .expect("complete one delivery")
    );
    assert!(
        fixture
            .database
            .defer_listen_delivery(due[1].outbox_key, 20, Some("offline"))
            .await
            .expect("defer one delivery")
    );
    assert_eq!(
        fixture
            .database
            .activity_history(Some(&SourceId::new("source")), "", &cancel)
            .await
            .expect("History survives delivery")
            .len(),
        1
    );
    assert_eq!(
        fixture
            .database
            .due_listen_deliveries(10, 100, &cancel)
            .await
            .expect("completed target removed")
            .len(),
        10
    );

    let mut raw = connection(&fixture.path).await;
    let history_plan = sqlx::query_as::<_, (i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT listen_key FROM listens WHERE source_id=?1 ORDER BY started_at DESC,listen_key DESC LIMIT 100")
        .bind("source").fetch_all(&mut raw).await.expect("History plan").into_iter().map(|row| row.3).collect::<Vec<_>>().join(" | ");
    assert!(
        history_plan.contains("listens_history_idx"),
        "{history_plan}"
    );
    let due_plan = sqlx::query_as::<_, (i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT outbox_key FROM listen_outbox WHERE next_attempt_at IS NOT NULL AND next_attempt_at<=?1 ORDER BY next_attempt_at,outbox_key LIMIT 100")
        .bind(20_i64).fetch_all(&mut raw).await.expect("Activity outbox plan").into_iter().map(|row| row.3).collect::<Vec<_>>().join(" | ");
    assert!(due_plan.contains("listen_outbox_due_idx"), "{due_plan}");
}

#[tokio::test]
async fn current_activity_includes_recorded_and_unattributed_local_and_cue_listens() {
    let fixture = fixture().await;
    let file = "file:///music/local.flac".to_string();
    let cue = library::cue_media_uri("segment", "file:///music/album.flac", 1000, 181000);
    let mut raw = connection(&fixture.path).await;
    for (key, uri) in [(fixture.tracks[0], &file), (fixture.tracks[1], &cue)] {
        sqlx::query("UPDATE tracks SET media_uri=?2 WHERE track_key=?1")
            .bind(key)
            .bind(uri)
            .execute(&mut raw)
            .await
            .unwrap();
    }
    let mut listen = ListenWrite {
        external_id: None,
        media_uri: file.clone(),
        title: "Recorded file".into(),
        artist: "Artist".into(),
        album: "Album".into(),
        duration_millis: 180000,
        disc_number: None,
        track_number: None,
        year: None,
        release_date: None,
        source_format: None,
        musicbrainz_recording_id: None,
        musicbrainz_release_track_id: None,
        started_at: 100,
        local_period: "2023-11".into(),
        listened_millis: 90000,
        skipped: false,
    };
    let file_key = fixture.database.record_listen(&listen, &[]).await.unwrap();
    listen.media_uri = cue.clone();
    listen.title = "Recorded CUE".into();
    listen.started_at = 200;
    let cue_key = fixture.database.record_listen(&listen, &[]).await.unwrap();
    for key in [file_key, cue_key] {
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT source_id FROM listens WHERE listen_key=?1")
                .bind(key)
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            "source"
        );
    }
    for (uri, title, time) in [
        (file.as_str(), "Legacy file", 50),
        (file.as_str(), "Legacy file", 60),
        (cue.as_str(), "Legacy CUE", 300),
        ("file:///outside.flac", "Outside", 400),
    ] {
        sqlx::query("INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis) VALUES(?1,?2,'Artist','Album',?3,'2023-11',180000,90000)")
            .bind(uri).bind(title).bind(time).execute(&mut raw).await.unwrap();
    }
    listen.media_uri = "rufin:source/track/remote/uncached".into();
    listen.title = "Remote".into();
    listen.started_at = 500;
    fixture.database.record_listen(&listen, &[]).await.unwrap();
    let cancel = ReadCancellation::new();
    let current = fixture
        .database
        .activity_history(Some(&SourceId::new("source")), "", &cancel)
        .await
        .unwrap();
    assert_eq!(
        current
            .iter()
            .map(|row| (row.title.as_str(), row.play_count))
            .collect::<Vec<_>>(),
        [("Legacy CUE", 2), ("Recorded file", 3)]
    );
    let matching = fixture
        .database
        .activity_history(Some(&SourceId::new("source")), "legacy", &cancel)
        .await
        .unwrap();
    assert_eq!(
        matching
            .iter()
            .map(|row| row.title.as_str())
            .collect::<Vec<_>>(),
        ["Legacy CUE", "Legacy file"]
    );
    let remote = fixture
        .database
        .activity_history(Some(&SourceId::new("remote")), "", &cancel)
        .await
        .unwrap();
    assert_eq!(remote.len(), 1);
    assert_eq!(remote[0].title, "Remote");
    for period in [
        CalendarActivityPeriod::Lifetime,
        CalendarActivityPeriod::Year(2023),
        CalendarActivityPeriod::Month {
            year: 2023,
            month: 11,
        },
    ] {
        let summary = fixture
            .database
            .calendar_activity_summary(fixture.source, period, 100, &cancel)
            .await
            .unwrap();
        assert_eq!(
            summary
                .tracks
                .iter()
                .map(|row| row.play_count)
                .collect::<Vec<_>>(),
            [3, 2]
        );
        assert_eq!(summary.albums[0].play_count, 5);
        assert_eq!(summary.artists[0].play_count, 5);
        assert_eq!(summary.genres[0].play_count, 5);
    }
    for (sort, expected) in [
        (library::TrackSort::PlayCount, [&file, &cue]),
        (library::TrackSort::LastPlayed, [&cue, &file]),
    ] {
        let tracks = fixture
            .database
            .track_route_page(
                fixture.source,
                None,
                false,
                "",
                sort,
                true,
                library::RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(&tracks.order[..2], expected.map(String::as_str));
        let album = fixture
            .database
            .album_track_route_page(
                fixture.source,
                fixture.albums[0],
                None,
                "",
                sort,
                true,
                library::RouteSeedWindow::top(),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(album.order, expected.map(String::as_str));
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listens WHERE source_id IS NULL")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        4
    );
}

#[tokio::test]
async fn history_preserves_latest_matching_facts_and_source_scope() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO listens(source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped)
        VALUES ('source','https://example.org/a','Old title','Artist','Album',1,'2023-11',1000,900,0),
               ('source','https://example.org/a','New title','Artist','Album',2,'2023-11',1000,900,0),
               ('other','https://example.org/b','Other account','Artist','Album',2,'2023-11',1000,900,0),
               ('forgotten','https://example.org/c','Retained history','Artist','Album',3,'2023-11',1000,900,0),
               (NULL,'https://example.org/d','Direct media','Artist','Album',4,'2023-11',1000,900,0)")
        .execute(&mut raw).await.unwrap();
    drop(raw);
    let cancel = ReadCancellation::new();
    let all = fixture
        .database
        .activity_history(None, "", &cancel)
        .await
        .unwrap();
    assert_eq!(
        all.iter().map(|row| row.title.as_str()).collect::<Vec<_>>(),
        [
            "Direct media",
            "Retained history",
            "Other account",
            "New title"
        ]
    );
    assert_eq!(all[3].play_count, 2);
    let current = fixture
        .database
        .activity_history(Some(&SourceId::new("source")), "", &cancel)
        .await
        .unwrap();
    assert_eq!(current, vec![all[3].clone()]);
    let matching = fixture
        .database
        .activity_history(None, " old TITLE ", &cancel)
        .await
        .unwrap();
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0].title, "Old title");
    assert_eq!(matching[0].last_played, Some(1));
    assert_eq!(matching[0].play_count, 2);
    assert!(
        fixture
            .database
            .activity_history(Some(&SourceId::new("other")), "old", &cancel)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn semantic_activity_round_trip_preserves_facts_without_delivery() {
    let fixture = fixture().await;
    let input = library::ActivityRecord {
        version: 1,
        listen_key: 1,
        source_id: Some("forgotten-server".into()),
        listen: ListenWrite {
            external_id: Some("accepted-play-exact-id".into()),
            media_uri: "https://example.org/music.ogg".into(),
            title: "Şarkı, \"one\"\nsecond line".into(),
            artist: "Björk".into(),
            album: "日本語".into(),
            duration_millis: 123456,
            disc_number: Some(2),
            track_number: Some(7),
            year: Some(2024),
            release_date: Some("2024-02-29".into()),
            source_format: Some("ogg".into()),
            musicbrainz_recording_id: Some("recording-id".into()),
            musicbrainz_release_track_id: Some("release-track-id".into()),
            started_at: 1_700_000_000,
            local_period: "2023-11".into(),
            listened_millis: 90123,
            skipped: true,
        },
    };
    let valid = serde_json::to_string(&input).unwrap();
    let data = format!("malformed\n{valid}\n{{\"version\":999}}\n{valid}\n");
    let report = fixture
        .database
        .import_activity_jsonl(std::io::Cursor::new(data))
        .await
        .unwrap();
    assert_eq!(report.accepted, 2);
    assert_eq!(report.skipped, 2);
    let mut exported = Vec::new();
    assert_eq!(
        fixture
            .database
            .export_activity_jsonl(&mut exported, None)
            .await
            .unwrap(),
        1
    );
    let restored: library::ActivityRecord = serde_json::from_slice(&exported).unwrap();
    assert_eq!(restored.source_id, input.source_id);
    assert_eq!(restored.listen, input.listen);
    let empty = tempfile::tempdir().unwrap();
    let database = library::Database::open(empty.path().join("store.sqlite"))
        .await
        .unwrap();
    database
        .import_activity_jsonl(std::io::Cursor::new(&exported))
        .await
        .unwrap();
    let mut again = Vec::new();
    database
        .export_activity_jsonl(&mut again, None)
        .await
        .unwrap();
    assert_eq!(again, exported);
    assert!(
        database
            .due_listen_deliveries(i64::MAX, 100, &ReadCancellation::new())
            .await
            .unwrap()
            .is_empty()
    );
    let mut csv = Vec::new();
    assert_eq!(
        database
            .export_activity_csv(&mut csv, library::ActivityCsvFormat::LastFm, None)
            .await
            .unwrap(),
        1
    );
    let csv = String::from_utf8(csv).unwrap();
    assert_eq!(
        csv,
        "\"Björk\",\"日本語\",\"Şarkı, \"\"one\"\"\nsecond line\",\"2023-11-14T22:13:20Z\"\r\n"
    );
    assert!(csv.contains("\"2023-11-14T22:13:20Z\""));
    assert!(csv.contains("\"Şarkı, \"\"one\"\"\nsecond line\""));
    assert!(!csv.contains("forgotten-server"));
    assert!(!csv.contains("example.org"));
    let mut csv = Vec::new();
    database
        .export_activity_csv(&mut csv, library::ActivityCsvFormat::ListenBrainz, None)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(csv).unwrap(),
        "artist,track,album,time\r\n\"Björk\",\"Şarkı, \"\"one\"\"\nsecond line\",\"日本語\",\"1700000000\"\r\n"
    );
    for (index, source) in [Some("another-server"), None].into_iter().enumerate() {
        let mut listen = input.listen.clone();
        listen.external_id = Some(format!("extra-{index}"));
        listen.title = format!("Extra {index}");
        let record = library::ActivityRecord {
            version: 1,
            listen_key: index as i64 + 2,
            source_id: source.map(str::to_string),
            listen,
        };
        database
            .import_activity_jsonl(std::io::Cursor::new(serde_json::to_vec(&record).unwrap()))
            .await
            .unwrap();
    }
    let source = SourceId::new("forgotten-server");
    let mut scoped = Vec::new();
    assert_eq!(
        database
            .export_activity_jsonl(&mut scoped, Some(&source))
            .await
            .unwrap(),
        1
    );
    assert_eq!(scoped, exported);
    assert_eq!(
        database
            .export_activity_jsonl(std::io::sink(), None)
            .await
            .unwrap(),
        3
    );
    let missing = SourceId::new("missing");
    assert_eq!(
        database
            .export_activity_jsonl(std::io::sink(), Some(&missing))
            .await
            .unwrap(),
        0
    );
    for format in [
        library::ActivityCsvFormat::LastFm,
        library::ActivityCsvFormat::ListenBrainz,
    ] {
        let mut scoped = Vec::new();
        assert_eq!(
            database
                .export_activity_csv(&mut scoped, format, Some(&source))
                .await
                .unwrap(),
            1
        );
        let scoped = String::from_utf8(scoped).unwrap();
        assert!(scoped.contains("Björk"));
        assert!(!scoped.contains("Extra"));
        assert_eq!(
            database
                .export_activity_csv(std::io::sink(), format, None)
                .await
                .unwrap(),
            3
        );
        assert_eq!(
            database
                .export_activity_csv(std::io::sink(), format, Some(&missing))
                .await
                .unwrap(),
            0
        );
    }
}
