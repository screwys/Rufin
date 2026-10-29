use library::{CalendarActivityPeriod, ListenWrite, ReadCancellation};

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
