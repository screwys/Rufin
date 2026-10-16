use library::{
    CalendarActivityPeriod, ListenDeliveryTarget, ListenWrite, ReadCancellation, SourceId,
};

use super::support::{connection, fixture};

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
        .database
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
        .database
        .activity_overview(CalendarActivityPeriod::Year(2026), 5, &cancel)
        .await
        .unwrap();
    assert_eq!(year.totals.plays, 4);
    assert_eq!(year.totals.duration_millis, 960_000);
    assert_eq!(year.previous.plays, 1);
    let empty = fixture
        .database
        .activity_overview(CalendarActivityPeriod::Year(2024), 5, &cancel)
        .await
        .unwrap();
    assert_eq!(empty.totals.plays, 0);
    assert!(empty.tracks.is_empty());
}

#[tokio::test]
async fn overview_keeps_monthly_baselines_and_bounds_history_results() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count)
        SELECT object_id,'2025-06','track','track-0',9,2 FROM sources WHERE source_key=?1")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    for index in 0..7 {
        sqlx::query("INSERT INTO tracks(media_uri,indexed,metadata,normalized_search) VALUES (?1,0,jsonb(json_object('title',COALESCE(?2,''),'artist',COALESCE('Retained artist',''),'album',COALESCE('Retained album',''),'duration_millis',COALESCE(180000,0))),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE(?2,''),'artist',COALESCE('Retained artist',''),'album',COALESCE('Retained album',''),'duration_millis',COALESCE(180000,0))),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE(?2,''),'artist',COALESCE('Retained artist',''),'album',COALESCE('Retained album',''),'duration_millis',COALESCE(180000,0))),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE(?2,''),'artist',COALESCE('Retained artist',''),'album',COALESCE('Retained album',''),'duration_millis',COALESCE(180000,0))),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE(?2,''),'artist',COALESCE('Retained artist',''),'album',COALESCE('Retained album',''),'duration_millis',COALESCE(180000,0))),'$.comment'),'')))")
            .bind(format!("file:///retained-{index}.flac")).bind(format!("Retained {index}")).execute(&mut raw).await.unwrap();
        sqlx::query("INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped)
            VALUES(?1,?2,'Retained artist','Retained album',100,'2025-06',180000,1000,0)")
            .bind(format!("file:///retained-{index}.flac")).bind(format!("Retained {index}"))
            .execute(&mut raw).await.unwrap();
    }
    drop(raw);
    let cancel = ReadCancellation::new();
    let overview = fixture
        .database
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
            .all(|row| row.metadata.title.starts_with("Retained"))
    );
    assert_eq!(overview.albums[0].state.play_count, 9);
    for (limit, expected) in [(3, 3), (10, 8)] {
        let report = fixture
            .database
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
        fixture.database.activity_months().await.unwrap(),
        ["2025-06"]
    );
}

#[tokio::test]
async fn listens_accept_empty_optional_recording_ids() {
    let fixture = fixture().await;
    let (published, mut observed) = tokio::sync::mpsc::unbounded_channel();
    fixture.database.observe_catalog(move |source, update| {
        let _ = published.send((source, update));
        std::future::ready(())
    });
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
        .database
        .record_listen(&listen, None, &[])
        .await
        .unwrap();
    let (source, update) = observed.try_recv().unwrap();
    assert!(source.is_none());
    assert!(update.activity && !update.metadata);
    assert_eq!(
        fixture
            .database
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
    let (updates, mut observed) = tokio::sync::mpsc::unbounded_channel();
    fixture.database.observe_catalog(move |_, update| {
        updates.send(update).unwrap();
        std::future::ready(())
    });
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
        .record_listen(&listen, None, &targets)
        .await
        .expect("record listen");
    let update = observed.try_recv().expect("new listen changes activity");
    assert!(update.activity);
    assert!(!update.metadata);
    assert_eq!(
        fixture
            .database
            .record_listen(&listen, None, &targets)
            .await
            .expect("record listen idempotently"),
        key
    );
    assert!(observed.try_recv().is_err());
    let cancel = ReadCancellation::new();
    let history = (&library::CatalogQuery {
        descending: true,
        ..library::CatalogQuery::tracks(library::TrackScope::History {
            source: (Some(&SourceId::new("source"))).cloned(),
        })
    })
        .rows(&fixture.database, 0, 100, &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!("typed song projection"),
        })
        .expect("Activity History");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].metadata.title, "Stored Title");
    assert_eq!(history[0].metadata.duration_millis, 180_000);
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
    sqlx::query("INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at) SELECT object_id,'lifetime','track','track-0',3,1,1600000000 FROM sources WHERE source_key=?1")
        .bind(fixture.source)
        .execute(&mut raw)
        .await
        .expect("seed recovered Activity baseline");
    sqlx::query("INSERT INTO native_media_state(media_uri,play_count,last_played) SELECT media_uri,2,1600000000 FROM tracks WHERE source_key=?1 AND object_id='track-1' ON CONFLICT(media_uri) DO UPDATE SET play_count=excluded.play_count,last_played=excluded.last_played")
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
        .selected_track_uris(
            &library::CatalogQuery::tracks(library::TrackScope::Smart {
                key: never,
                source: Some(fixture.source),
                now: 1800000000,
            })
            .selection(),
            None,
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        unplayed.contains(&fixture.track_uris[1]),
        "remote play totals do not count as Rufin listens"
    );
    assert!(!unplayed.contains(&listen.media_uri));
    let catalog = library::CatalogQuery::uris(
        library::CatalogKind::Track,
        vec![(&listen.media_uri).to_string()],
    )
    .rows(&fixture.database, 0, 1, &cancel)
    .await
    .map(|rows| match rows {
        library::CatalogRows::Tracks(mut rows) => rows.pop(),
        _ => unreachable!("typed catalog projection"),
    })
    .unwrap()
    .unwrap();
    let history = (&library::CatalogQuery {
        descending: true,
        ..library::CatalogQuery::tracks(library::TrackScope::History { source: None })
    })
        .rows(&fixture.database, 0, 100, &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!("typed song projection"),
        })
        .unwrap();
    let row = &history[0];
    assert_eq!(row.links.album_media_uri, catalog.links.album_media_uri);
    assert_eq!(row.links.artists, catalog.links.artists);
    assert_eq!(row.links.album_artists, catalog.links.album_artists);
    assert_eq!(row.metadata.date_added, catalog.metadata.date_added);
    assert_eq!(row.metadata.bpm, catalog.metadata.bpm);
    assert_eq!(row.metadata.genres[0].name, "Rock");
    assert_eq!(row.state.play_count, 4);
    let (playlist, _) = fixture
        .database
        .create_playlist(
            None,
            "History track",
            &library::CatalogQuery::uris(
                library::CatalogKind::Track,
                (std::slice::from_ref(&listen.media_uri)).to_vec(),
            )
            .selection(),
        )
        .await
        .unwrap()
        .unwrap();
    let entries = fixture
        .database
        .playlist_edit_entries(
            playlist,
            &library::CatalogQuery {
                subject: library::CatalogSubject::Tracks {
                    scope: library::TrackScope::Playlist(playlist),
                    sort: Some(library::CatalogSort::Position),
                },
                ..library::CatalogQuery::tracks(library::TrackScope::Playlist(playlist))
            }
            .selection(),
            &cancel,
        )
        .await
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| entry.playlist_entry_key)
                .collect::<Vec<_>>()
        })
        .unwrap();
    let entries = library::CatalogQuery::playlist_entries((&entries).to_vec())
        .rows(&fixture.database, 0, (&entries).len(), &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::PlaylistEntries(rows) => rows,
            _ => unreachable!(),
        })
        .unwrap();
    let entry = &entries[0];
    assert_eq!(
        entry.track.links.album_media_uri,
        catalog.links.album_media_uri
    );
    assert_eq!(entry.track.links.artists, catalog.links.artists);
    assert_eq!(entry.track.links.album_artists, catalog.links.album_artists);
    assert_eq!(entry.track.metadata.date_added, catalog.metadata.date_added);
    assert_eq!(entry.track.metadata.bpm, catalog.metadata.bpm);
    assert_eq!(entry.track.metadata.genres[0].name, "Rock");
    assert_eq!(entry.track.state.play_count, catalog.state.play_count);
    assert_eq!(entry.track.state.last_played, catalog.state.last_played);
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
        (&library::CatalogQuery {
            descending: true,
            ..library::CatalogQuery::tracks(library::TrackScope::History {
                source: (Some(&SourceId::new("source"))).cloned()
            })
        })
            .rows(&fixture.database, 0, 100, &cancel)
            .await
            .map(|rows| match rows {
                library::CatalogRows::Tracks(rows) => rows,
                _ => unreachable!(),
            })
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
}

#[tokio::test]
async fn current_activity_includes_recorded_and_unattributed_local_and_cue_listens() {
    let fixture = fixture().await;
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
        .database
        .catalog_identity(library::CatalogKind::Track, &file, &ReadCancellation::new())
        .await
        .map(|identity| identity.map(|identity| library::TrackKey::from_raw(identity.key)))
        .unwrap();
    let file_key = fixture
        .database
        .record_listen(&listen, current_track, &[])
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_as::<_, (String, String)>(
            "SELECT media_uri,captured_media_uri FROM listens WHERE listen_key=?1"
        )
        .bind(file_key)
        .fetch_one(&mut raw)
        .await
        .unwrap(),
        (file.clone(), listen.media_uri.clone()),
    );
    listen.media_uri = cue.clone();
    listen.title = "Recorded CUE".into();
    listen.started_at = 200;
    let cue_key = fixture
        .database
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
    listen.media_uri = "rufin:source/track/remote/uncached".into();
    listen.title = "Remote".into();
    listen.started_at = 500;
    fixture
        .database
        .record_listen(&listen, None, &[])
        .await
        .unwrap();
    let cancel = ReadCancellation::new();
    let current = (&library::CatalogQuery {
        descending: true,
        ..library::CatalogQuery::tracks(library::TrackScope::History {
            source: (Some(&SourceId::new("source"))).cloned(),
        })
    })
        .rows(&fixture.database, 0, 100, &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!("typed song projection"),
        })
        .unwrap();
    assert_eq!(
        current
            .iter()
            .map(|row| (row.metadata.title.as_str(), row.state.play_count))
            .collect::<Vec<_>>(),
        [("Legacy CUE", 2), ("Recorded file", 3)]
    );
    let matching = (&library::CatalogQuery {
        filter: ("legacy").into(),
        descending: true,
        ..library::CatalogQuery::tracks(library::TrackScope::History {
            source: (Some(&SourceId::new("source"))).cloned(),
        })
    })
        .rows(&fixture.database, 0, 100, &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!("typed song projection"),
        })
        .unwrap();
    assert_eq!(
        matching
            .iter()
            .map(|row| row.metadata.title.as_str())
            .collect::<Vec<_>>(),
        ["Legacy CUE", "Legacy file"]
    );
    let remote = (&library::CatalogQuery {
        descending: true,
        ..library::CatalogQuery::tracks(library::TrackScope::History {
            source: (Some(&SourceId::new("remote"))).cloned(),
        })
    })
        .rows(&fixture.database, 0, 100, &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!("typed song projection"),
        })
        .unwrap();
    assert_eq!(remote.len(), 1);
    assert_eq!(remote[0].metadata.title, "Remote");
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
        (library::CatalogSort::PlayCount, [&file, &cue]),
        (library::CatalogSort::LastPlayed, [&cue, &file]),
    ] {
        let tracks = (&library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Tracks(fixture.source),
                sort: Some(sort),
            },
            descending: true,
            ..library::CatalogQuery::tracks(library::TrackScope::Tracks(fixture.source))
        })
            .page(&fixture.database, library::RouteSeedWindow::top(), &cancel)
            .await
            .unwrap();
        assert_eq!(
            (match &tracks.rows {
                library::CatalogRows::Tracks(rows) => rows,
                _ => unreachable!(),
            })[..2]
                .iter()
                .map(|row| row.media_uri.as_str())
                .collect::<Vec<_>>(),
            expected.map(String::as_str)
        );
        let album = (&library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Album(fixture.albums[0]),
                sort: Some(sort),
            },
            descending: true,
            ..library::CatalogQuery::tracks(library::TrackScope::Album(fixture.albums[0]))
        })
            .page(&fixture.database, library::RouteSeedWindow::top(), &cancel)
            .await
            .unwrap();
        assert_eq!(
            (match &album.rows {
                library::CatalogRows::Tracks(rows) => rows,
                _ => unreachable!(),
            })
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
async fn history_preserves_latest_matching_facts_and_source_scope() {
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
            .database
            .import_activity_jsonl(std::io::Cursor::new(records))
            .await
            .unwrap()
            .accepted,
        5
    );
    let cancel = ReadCancellation::new();
    let all = (&library::CatalogQuery {
        descending: true,
        ..library::CatalogQuery::tracks(library::TrackScope::History { source: None })
    })
        .rows(&fixture.database, 0, 100, &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!("typed song projection"),
        })
        .unwrap();
    assert_eq!(
        all.iter()
            .map(|row| row.metadata.title.as_str())
            .collect::<Vec<_>>(),
        [
            "Direct media",
            "Retained history",
            "Other account",
            "New title"
        ]
    );
    assert_eq!(all[3].state.play_count, 2);
    let current = (&library::CatalogQuery {
        descending: true,
        ..library::CatalogQuery::tracks(library::TrackScope::History {
            source: (Some(&SourceId::new("source"))).cloned(),
        })
    })
        .rows(&fixture.database, 0, 100, &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!("typed song projection"),
        })
        .unwrap();
    assert_eq!(current, vec![all[3].clone()]);
    let matching = (&library::CatalogQuery {
        filter: (" old TITLE ").into(),
        descending: true,
        ..library::CatalogQuery::tracks(library::TrackScope::History { source: None })
    })
        .rows(&fixture.database, 0, 100, &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!("typed song projection"),
        })
        .unwrap();
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0].metadata.title, "Old title");
    assert_eq!(matching[0].state.last_played, Some(1));
    assert_eq!(matching[0].state.play_count, 2);
    assert!(
        (&library::CatalogQuery {
            filter: ("old").into(),
            descending: true,
            ..library::CatalogQuery::tracks(library::TrackScope::History {
                source: (Some(&SourceId::new("other"))).cloned()
            })
        })
            .rows(&fixture.database, 0, 100, &cancel)
            .await
            .map(|rows| match rows {
                library::CatalogRows::Tracks(rows) => rows,
                _ => unreachable!(),
            })
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
    assert_eq!(restored.current_media_uri, input.current_media_uri);
    assert_eq!(restored.listen, input.listen);
    let empty = tempfile::tempdir().unwrap();
    let database = library::Database::open(empty.path().join("store.sqlite"))
        .await
        .unwrap();
    database
        .import_activity_jsonl(std::io::Cursor::new(&exported))
        .await
        .unwrap();
    database.close().await.unwrap();
    let database = library::Database::open(empty.path().join("store.sqlite"))
        .await
        .unwrap();
    let history = (&library::CatalogQuery {
        descending: true,
        ..library::CatalogQuery::tracks(library::TrackScope::History { source: None })
    })
        .rows(&database, 0, 100, &ReadCancellation::new())
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!("typed song projection"),
        })
        .unwrap();
    assert_eq!(
        history[0].media_uri,
        input.current_media_uri.as_deref().unwrap()
    );
    assert_eq!(history[0].metadata.title, input.listen.title);
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
            current_media_uri: None,
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
