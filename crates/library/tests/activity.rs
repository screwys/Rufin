use library::{
    CalendarActivityPeriod, ListenDeliveryTarget, ListenWrite, ReadCancellation, SourceId,
};

use super::support::{connection, fixture, tracks};

#[tokio::test]
async fn overview_uses_full_duration_qualified_plays_and_calendar_comparisons() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    for (period, track, skipped, duration) in [
        ("2025-12", 0, 0, 240_000),
        ("2026-01", 0, 0, 300_000),
        ("2026-01", 0, 0, 300_000),
        ("2026-01", 1, 0, 180_000),
        ("2026-01", 1, 1, 180_000),
        ("2026-02", 1, 0, 180_000),
    ] {
        sqlx::query("INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped)
            VALUES(?1,'Track','Artist','Album',100,?2,?3,1000,?4)")
            .bind(&fixture.track_uris[track]).bind(period).bind(duration).bind(skipped)
            .execute(&mut raw).await.unwrap();
    }
    drop(raw);
    let cancel = ReadCancellation::new();
    let month = fixture
        .catalog
        .activity_overview(
            CalendarActivityPeriod::Month {
                year: 2026,
                month: 1,
            },
            5,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(month.totals.plays, 3);
    assert_eq!(month.totals.duration_millis, 780_000);
    assert_eq!(month.totals.tracks, 2);
    assert_eq!(month.previous.plays, 1);
    assert_eq!(month.previous.duration_millis, 240_000);
    assert_eq!(
        month
            .tracks
            .iter()
            .map(|row| row.state.play_count)
            .collect::<Vec<_>>(),
        [2, 1]
    );
    assert_eq!(month.albums[0].state.play_count, 3);
    assert_eq!(month.artists[0].state.play_count, 3);
    assert_eq!(month.genres[0].plays, 3);
    let year = fixture
        .catalog
        .activity_overview(CalendarActivityPeriod::Year(2026), 5, &cancel)
        .await
        .unwrap();
    assert_eq!(year.totals.plays, 4);
    assert_eq!(year.totals.duration_millis, 960_000);
    assert_eq!(year.previous.plays, 1);
    let empty = fixture
        .catalog
        .activity_overview(CalendarActivityPeriod::Year(2024), 5, &cancel)
        .await
        .unwrap();
    assert_eq!(empty.totals.plays, 0);
    assert!(empty.tracks.is_empty());
}

#[tokio::test]
async fn listens_accept_empty_optional_recording_ids() {
    let fixture = fixture().await;
    let mut observed = fixture.catalog.changes();
    let listen = ListenWrite {
        external_id: Some("optional-recording-ids".into()),
        media_uri: "https://audio.invalid/direct.flac".into(),
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
    let key = fixture
        .catalog
        .record_listen(&listen, None, &[])
        .await
        .unwrap();
    assert_eq!(
        fixture
            .catalog
            .record_listen(&listen, None, &[])
            .await
            .unwrap(),
        key
    );
    assert!(observed.try_recv().is_err());
    let record = library::ActivityRecord {
        version: 1,
        listen_key: 100,
        source_id: None,
        current_media_uri: None,
        listen: ListenWrite {
            external_id: Some("imported-optional-recording-ids".into()),
            ..listen
        },
    };
    let report = fixture
        .catalog
        .import_activity_jsonl(std::io::Cursor::new(serde_json::to_vec(&record).unwrap()))
        .await
        .unwrap();
    assert_eq!((report.accepted, report.skipped), (1, 0));
    let mut exported = Vec::new();
    fixture
        .catalog
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
async fn overview_keeps_monthly_baselines_and_bounds_history_results() {
    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count)
        SELECT object_id,'2025-06','track','track-0',9,2 FROM sources WHERE source_key=?1")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    for index in 0..7 {
        sqlx::query("INSERT INTO tracks(media_uri,indexed,title,display_artist,display_album,duration_millis) VALUES (?1,0,COALESCE(?2,''),COALESCE('Retained artist',''),COALESCE('Retained album',''),COALESCE(180000,0))")
            .bind(format!("file:///retained-{index}.flac")).bind(format!("Retained {index}")).execute(&mut raw).await.unwrap();
        sqlx::query("INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped)
            VALUES(?1,?2,'Retained artist','Retained album',100,'2025-06',180000,1000,0)")
            .bind(format!("file:///retained-{index}.flac")).bind(format!("Retained {index}"))
            .execute(&mut raw).await.unwrap();
    }
    drop(raw);
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let cancel = ReadCancellation::new();
    let overview = fixture
        .catalog
        .activity_overview(
            CalendarActivityPeriod::Month {
                year: 2025,
                month: 6,
            },
            5,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(overview.totals.plays, 16);
    assert_eq!(overview.totals.tracks, 8);
    assert_eq!(overview.tracks.len(), 5);
    assert_eq!(overview.tracks[0].media_uri, fixture.track_uris[0]);
    assert_eq!(overview.tracks[0].state.play_count, 9);
    assert!(
        overview.tracks[1..]
            .iter()
            .all(|row| row.title.starts_with("Retained"))
    );
    assert_eq!(overview.albums[0].state.play_count, 9);
    for (limit, expected) in [(3, 3), (10, 8)] {
        let report = fixture
            .catalog
            .activity_overview(
                CalendarActivityPeriod::Month {
                    year: 2025,
                    month: 6,
                },
                limit,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(report.tracks.len(), expected);
        assert_eq!(report.totals, overview.totals);
    }
    assert_eq!(
        fixture.catalog.activity_months().await.unwrap(),
        ["2025-06"]
    );
}

#[tokio::test]
async fn activity_keeps_one_listen_and_independent_delivery_targets() {
    let mut fixture = fixture().await;
    let mut observed = fixture.catalog.changes();
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
        .catalog
        .record_listen(&listen, None, &targets)
        .await
        .expect("record listen");
    let update = observed.try_recv().expect("new listen changes activity");
    assert!(listened(&update));
    assert_eq!(
        fixture
            .catalog
            .record_listen(&listen, None, &targets)
            .await
            .expect("record listen idempotently"),
        key
    );
    assert!(observed.try_recv().is_err());
    let cancel = ReadCancellation::new();
    let history = tracks(
        &fixture.catalog,
        library::MemberRead {
            descending: true,
            ..library::MemberRead::new(library::Collection::History {
                source: (Some(&SourceId::new("source"))).cloned(),
            })
        },
    )
    .await;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].title, "Stored Title");
    assert_eq!(history[0].duration_millis, 180_000);
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listens WHERE media_uri=?1")
            .bind(&listen.media_uri)
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1,
        "duplicate accepted listens are recorded once"
    );
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT skipped FROM listens WHERE listen_key=?1")
            .bind(key)
            .fetch_one(&mut raw)
            .await
            .expect("read accepted skip")
    );
    sqlx::query("INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at) SELECT object_id,'lifetime','track','track-0',3,1,1600000000 FROM sources WHERE source_key=?1")
        .bind(fixture.source)
        .execute(&mut raw)
        .await
        .expect("seed recovered Activity baseline");
    sqlx::query("UPDATE tracks SET native_play_count=2,native_last_played=1600000000 WHERE source_key=?1 AND object_id='track-1'")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    drop(raw);
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    observed = fixture.catalog.changes();
    let never = fixture
        .catalog
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
            None,
        )
        .await
        .unwrap();
    let unplayed = fixture
        .catalog
        .selected_track_uris(
            &library::MemberRead::new(library::Collection::Smart {
                key: never,
                now: 1800000000,
            })
            .selection(),
            None,
            &cancel,
        )
        .await
        .unwrap();
    // All-time rules read the server's own counts on a server source.
    assert!(!unplayed.contains(&fixture.track_uris[1]));
    assert!(unplayed.contains(&listen.media_uri));
    let catalog = fixture
        .catalog
        .tracks_by_uris(&([(&listen.media_uri).to_string()]), &cancel)
        .await
        .map(|rows| {
            let mut rows = rows;
            rows.pop()
        })
        .unwrap()
        .unwrap();
    let history = tracks(
        &fixture.catalog,
        library::MemberRead {
            descending: true,
            ..library::MemberRead::new(library::Collection::History { source: None })
        },
    )
    .await;
    let row = &history[0];
    assert_eq!(row.links.album_media_uri, catalog.links.album_media_uri);
    assert_eq!(row.links.artists, catalog.links.artists);
    assert_eq!(row.links.album_artists, catalog.links.album_artists);
    // The track's 2024-01-02 date added, as Unix seconds.
    assert_eq!(catalog.metadata.date_added.as_deref(), Some("2024-01-02"));
    assert_eq!(row.date_added, Some(1_704_153_600));
    assert_eq!(row.bpm, catalog.metadata.bpm);
    assert_eq!(row.links.genres[0].name, "Rock");
    // The skipped listen is no play: the 3 carried-over plays remain.
    assert_eq!(row.state.play_count, 3);
    let (playlist, _) = fixture
        .catalog
        .create_playlist(
            "History track",
            &library::MemberSelection::Uris(
                ((std::slice::from_ref(&listen.media_uri)).to_vec()).into(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
    let entries = tracks(
        &fixture.catalog,
        library::MemberRead {
            sort: Some(library::LibraryField::RowIndex),
            ..library::MemberRead::new(library::Collection::Playlist(playlist))
        },
    )
    .await;
    let entry = &entries[0];
    assert_eq!(entry.links.album_media_uri, catalog.links.album_media_uri);
    assert_eq!(entry.links.artists, catalog.links.artists);
    assert_eq!(entry.links.album_artists, catalog.links.album_artists);
    assert_eq!(entry.date_added, row.date_added);
    assert_eq!(entry.bpm, catalog.metadata.bpm);
    assert_eq!(entry.links.genres[0].name, "Rock");
    assert_eq!(entry.state.play_count, catalog.state.play_count);
    assert_eq!(entry.state.last_played, catalog.state.last_played);
    let due = fixture
        .catalog
        .due_listen_deliveries(10, 100, &cancel)
        .await
        .expect("due listen deliveries");
    assert_eq!(due.len(), 12);
    assert_eq!(due[0].duration_millis, 180_000);
    assert_eq!(due[0].listened_millis, 90_000);
    assert!(
        fixture
            .catalog
            .complete_listen_delivery(due[0].outbox_key)
            .await
            .expect("complete one delivery")
    );
    assert!(
        fixture
            .catalog
            .defer_listen_delivery(due[1].outbox_key, 20, Some("offline"))
            .await
            .expect("defer one delivery")
    );
    assert_eq!(
        tracks(
            &fixture.catalog,
            library::MemberRead {
                descending: true,
                ..library::MemberRead::new(library::Collection::History {
                    source: Some(SourceId::new("source"))
                })
            }
        )
        .await
        .len(),
        1
    );
    assert_eq!(
        fixture
            .catalog
            .due_listen_deliveries(10, 100, &cancel)
            .await
            .expect("completed target removed")
            .len(),
        10
    );
    while observed.try_recv().is_ok() {}
    let completed = ListenWrite {
        listened_millis: 120_000,
        skipped: false,
        ..listen.clone()
    };
    assert_eq!(
        fixture
            .catalog
            .record_listen(&completed, None, &[])
            .await
            .unwrap(),
        key
    );
    assert!(listened(&observed.try_recv().unwrap()));
    for repeated in [&listen, &completed] {
        assert_eq!(
            fixture
                .catalog
                .record_listen(repeated, None, &[])
                .await
                .unwrap(),
            key
        );
        assert!(observed.try_recv().is_err());
    }
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_as::<_, (i64, bool)>(
            "SELECT listened_millis,skipped FROM listens WHERE listen_key=?1"
        )
        .bind(key)
        .fetch_one(&mut raw)
        .await
        .unwrap(),
        (120_000, false)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listens")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listen_outbox")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        11
    );
}

#[tokio::test]
async fn current_activity_includes_recorded_and_unattributed_local_and_cue_listens() {
    let mut fixture = fixture().await;
    let file = "file:///music/local.flac".to_string();
    let cue = library::cue_media_uri("segment", "file:///music/album.flac", 1000, 181000);
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE sources SET kind='local' WHERE source_key=?1")
        .bind(fixture.source)
        .execute(&mut raw)
        .await
        .unwrap();
    for (key, uri) in [(fixture.tracks[0], &file), (fixture.tracks[1], &cue)] {
        sqlx::query("UPDATE tracks SET media_uri=?2 WHERE track_key=?1")
            .bind(key)
            .bind(uri)
            .execute(&mut raw)
            .await
            .unwrap();
    }
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let mut listen = ListenWrite {
        external_id: None,
        media_uri: "file:///music/../music/local.flac".into(),
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
    let current_track = fixture
        .catalog
        .track_keys(std::slice::from_ref(&file), &ReadCancellation::new())
        .await
        .unwrap()
        .pop();
    let file_key = fixture
        .catalog
        .record_listen(&listen, current_track, &[])
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT media_uri FROM listens WHERE listen_key=?1")
            .bind(file_key)
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        file.clone(),
    );
    listen.media_uri = cue.clone();
    listen.title = "Recorded CUE".into();
    listen.started_at = 200;
    let cue_key = fixture
        .catalog
        .record_listen(&listen, None, &[])
        .await
        .unwrap();
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
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    listen.media_uri = "rufin:source/track/remote/uncached".into();
    listen.title = "Remote".into();
    listen.started_at = 500;
    fixture
        .catalog
        .record_listen(&listen, None, &[])
        .await
        .unwrap();
    let cancel = ReadCancellation::new();
    let current = tracks(
        &fixture.catalog,
        library::MemberRead {
            descending: true,
            ..library::MemberRead::new(library::Collection::History {
                source: (Some(&SourceId::new("source"))).cloned(),
            })
        },
    )
    .await;
    assert_eq!(
        current
            .iter()
            .map(|row| (row.title.as_str(), row.state.play_count))
            .collect::<Vec<_>>(),
        [
            ("Legacy CUE", 2),
            ("Recorded CUE", 2),
            ("Recorded file", 3),
            ("Legacy file", 3),
            ("Legacy file", 3)
        ]
    );
    let matching = tracks(
        &fixture.catalog,
        library::MemberRead {
            filter: ("legacy").into(),
            descending: true,
            ..library::MemberRead::new(library::Collection::History {
                source: (Some(&SourceId::new("source"))).cloned(),
            })
        },
    )
    .await;
    assert_eq!(
        matching
            .iter()
            .map(|row| row.title.as_str())
            .collect::<Vec<_>>(),
        ["Legacy CUE", "Legacy file", "Legacy file"]
    );
    let remote = tracks(
        &fixture.catalog,
        library::MemberRead {
            descending: true,
            ..library::MemberRead::new(library::Collection::History {
                source: (Some(&SourceId::new("remote"))).cloned(),
            })
        },
    )
    .await;
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
            .catalog
            .activity_overview(period, 100, &cancel)
            .await
            .unwrap();
        assert_eq!(
            summary
                .tracks
                .iter()
                .filter(|row| row.media_uri == file || row.media_uri == cue)
                .map(|row| row.state.play_count)
                .collect::<Vec<_>>(),
            [3, 2]
        );
        assert_eq!(summary.albums[0].state.play_count, 5);
        assert_eq!(summary.artists[0].state.play_count, 5);
        assert_eq!(summary.genres[0].plays, 5);
    }
    for (sort, expected) in [
        (library::LibraryField::PlayCount, [&file, &cue]),
        (library::LibraryField::LastPlayed, [&cue, &file]),
    ] {
        let tracks_rows = tracks(
            &fixture.catalog,
            library::MemberRead {
                collection: library::Collection::Source(fixture.source),
                sort: Some(sort),
                descending: true,
                ..library::MemberRead::new(library::Collection::Source(fixture.source))
            },
        )
        .await;
        assert_eq!(
            tracks_rows[..2]
                .iter()
                .map(|row| row.media_uri.as_str())
                .collect::<Vec<_>>(),
            expected.map(String::as_str)
        );
        let album_rows = tracks(
            &fixture.catalog,
            library::MemberRead {
                collection: library::Collection::Album(fixture.albums[0]),
                sort: Some(sort),
                descending: true,
                ..library::MemberRead::new(library::Collection::Album(fixture.albums[0]))
            },
        )
        .await;
        assert_eq!(
            album_rows
                .iter()
                .map(|row| row.media_uri.as_str())
                .collect::<Vec<_>>(),
            expected.map(String::as_str)
        );
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
async fn history_shows_each_listen_from_its_copy_and_source_scope() {
    let fixture = fixture().await;
    let records = [
        (Some("source"), "https://example.org/a", "Old title", 1),
        (Some("source"), "https://example.org/a", "New title", 2),
        (Some("other"), "https://example.org/b", "Other account", 2),
        (
            Some("forgotten"),
            "https://example.org/c",
            "Retained history",
            3,
        ),
        (None, "https://example.org/d", "Direct media", 4),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (source, uri, title, started_at))| {
        serde_json::json!({
            "version":1,"listen_key":index+1,"source_id":source,
            "media_uri":uri,"title":title,"artist":"Artist","album":"Album",
            "started_at":started_at,"local_period":"2023-11",
            "duration_millis":1000,"listened_millis":900,"skipped":false
        })
        .to_string()
    })
    .collect::<Vec<_>>()
    .join("\n");
    assert_eq!(
        fixture
            .catalog
            .import_activity_jsonl(std::io::Cursor::new(records))
            .await
            .unwrap()
            .accepted,
        5
    );
    let all = tracks(
        &fixture.catalog,
        library::MemberRead {
            descending: true,
            ..library::MemberRead::new(library::Collection::History { source: None })
        },
    )
    .await;
    assert_eq!(
        all.iter().map(|row| row.title.as_str()).collect::<Vec<_>>(),
        [
            "Direct media",
            "Retained history",
            "Other account",
            "New title",
            "Old title"
        ]
    );
    let current = tracks(
        &fixture.catalog,
        library::MemberRead {
            descending: true,
            ..library::MemberRead::new(library::Collection::History {
                source: (Some(&SourceId::new("source"))).cloned(),
            })
        },
    )
    .await;
    assert_eq!(current, vec![all[3].clone(), all[4].clone()]);
    let matching = tracks(
        &fixture.catalog,
        library::MemberRead {
            filter: (" old TITLE ").into(),
            descending: true,
            ..library::MemberRead::new(library::Collection::History { source: None })
        },
    )
    .await;
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0].title, "Old title");
    assert_eq!(matching[0].state.last_played, Some(1));
    assert!(
        tracks(
            &fixture.catalog,
            library::MemberRead {
                filter: ("old").into(),
                descending: true,
                ..library::MemberRead::new(library::Collection::History {
                    source: Some(SourceId::new("other"))
                })
            }
        )
        .await
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
        current_media_uri: Some("https://example.org/canonical/music.ogg".into()),
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
        .catalog
        .import_activity_jsonl(std::io::Cursor::new(data))
        .await
        .unwrap();
    assert_eq!(report.accepted, 2);
    assert_eq!(report.skipped, 2);
    let mut exported = Vec::new();
    assert_eq!(
        fixture
            .catalog
            .export_activity_jsonl(&mut exported, None)
            .await
            .unwrap(),
        1
    );
    let restored: library::ActivityRecord = serde_json::from_slice(&exported).unwrap();
    assert_eq!(restored.source_id, input.source_id);
    // A listen keeps the current URI it was imported under, not the one it was captured under.
    assert_eq!(restored.current_media_uri, None);
    assert_eq!(
        restored.listen,
        ListenWrite {
            media_uri: input.current_media_uri.clone().unwrap(),
            ..input.listen.clone()
        }
    );
    let empty = tempfile::tempdir().unwrap();
    let catalog = library::Catalog::open(empty.path().join("store.sqlite"))
        .await
        .unwrap();
    catalog
        .import_activity_jsonl(std::io::Cursor::new(&exported))
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let catalog = library::Catalog::open(empty.path().join("store.sqlite"))
        .await
        .unwrap();
    let history = tracks(
        &catalog,
        library::MemberRead {
            descending: true,
            ..library::MemberRead::new(library::Collection::History { source: None })
        },
    )
    .await;
    assert_eq!(
        history[0].media_uri,
        input.current_media_uri.as_deref().unwrap()
    );
    assert_eq!(history[0].title, input.listen.title);
    let mut again = Vec::new();
    catalog
        .export_activity_jsonl(&mut again, None)
        .await
        .unwrap();
    assert_eq!(again, exported);
    assert!(
        catalog
            .due_listen_deliveries(i64::MAX, 100, &ReadCancellation::new())
            .await
            .unwrap()
            .is_empty()
    );
    let mut csv = Vec::new();
    assert_eq!(
        catalog
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
    catalog
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
            current_media_uri: None,
            listen,
        };
        catalog
            .import_activity_jsonl(std::io::Cursor::new(serde_json::to_vec(&record).unwrap()))
            .await
            .unwrap();
    }
    let source = SourceId::new("forgotten-server");
    let mut scoped = Vec::new();
    assert_eq!(
        catalog
            .export_activity_jsonl(&mut scoped, Some(&source))
            .await
            .unwrap(),
        1
    );
    assert_eq!(scoped, exported);
    assert_eq!(
        catalog
            .export_activity_jsonl(std::io::sink(), None)
            .await
            .unwrap(),
        3
    );
    let missing = SourceId::new("missing");
    assert_eq!(
        catalog
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
            catalog
                .export_activity_csv(&mut scoped, format, Some(&source))
                .await
                .unwrap(),
            1
        );
        let scoped = String::from_utf8(scoped).unwrap();
        assert!(scoped.contains("Björk"));
        assert!(!scoped.contains("Extra"));
        assert_eq!(
            catalog
                .export_activity_csv(std::io::sink(), format, None)
                .await
                .unwrap(),
            3
        );
        assert_eq!(
            catalog
                .export_activity_csv(std::io::sink(), format, Some(&missing))
                .await
                .unwrap(),
            0
        );
    }
}

fn listened(change: &library::Change) -> bool {
    change
        .scopes
        .iter()
        .any(|scope| matches!(scope, library::Scope::Listened { .. }))
}
