use library::{
    ActivityBaseline, ActivityPeriod, ListenDeliveryTarget, ListenWrite, ReadCancellation,
};

use super::support::{connection, fixture};

#[tokio::test]
async fn activity_keeps_one_listen_and_independent_delivery_targets() {
    let fixture = fixture().await;
    let listen = ListenWrite {
        external_id: "play-1".to_string(),
        track_key: Some(fixture.tracks[0]),
        track_object_id: "track-0".to_string(),
        track_title: "Stored Title".to_string(),
        artist_name: "Stored Artist".to_string(),
        album_title: "Stored Album".to_string(),
        started_at: 1_700_000_000,
        duration_millis: 180_000,
        listened_millis: 90_000,
        skipped: false,
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
        .record_listen(fixture.source, &listen, &targets)
        .await
        .expect("record listen");
    assert_eq!(
        fixture
            .database
            .record_listen(fixture.source, &listen, &targets)
            .await
            .expect("record listen idempotently"),
        key
    );
    assert!(
        fixture
            .database
            .set_listen_skipped(key, true)
            .await
            .expect("mark listen skipped")
    );
    let cancel = ReadCancellation::new();
    let history = fixture
        .database
        .activity_history(fixture.source, &cancel)
        .await
        .expect("Activity History");
    assert_eq!(history.len(), 1);
    assert!(history[0].skipped);
    assert_eq!(history[0].title, "Alpha");
    assert_eq!(history[0].duration_millis, 180_000);
    fixture
        .database
        .write_activity_baseline(
            fixture.source,
            "track-0",
            ActivityBaseline {
                play_count: 3,
                skip_count: 1,
                last_played_at: Some(1_600_000_000),
            },
        )
        .await
        .expect("write Activity baseline");
    let tracks = fixture
        .database
        .activity_tracks(
            fixture.source,
            ActivityPeriod::Lifetime,
            1_800_000_000,
            100,
            &cancel,
        )
        .await
        .expect("lifetime Activity");
    assert_eq!(tracks[0].track_key, fixture.tracks[0]);
    assert_eq!(tracks[0].play_count, 4);
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
            .activity_history(fixture.source, &cancel)
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
    let history_plan = sqlx::query_as::<_, (i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT listen_key FROM listens WHERE source_key=?1 ORDER BY started_at DESC,listen_key DESC LIMIT 100")
        .bind(fixture.source).fetch_all(&mut raw).await.expect("History plan").into_iter().map(|row| row.3).collect::<Vec<_>>().join(" | ");
    assert!(
        history_plan.contains("listens_history_idx"),
        "{history_plan}"
    );
    let due_plan = sqlx::query_as::<_, (i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT outbox_key FROM listen_outbox WHERE next_attempt_at IS NOT NULL AND next_attempt_at<=?1 ORDER BY next_attempt_at,outbox_key LIMIT 100")
        .bind(20_i64).fetch_all(&mut raw).await.expect("Activity outbox plan").into_iter().map(|row| row.3).collect::<Vec<_>>().join(" | ");
    assert!(due_plan.contains("listen_outbox_due_idx"), "{due_plan}");
}
