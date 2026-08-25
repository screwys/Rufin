use library::{LocalAccessWrite, QueueCompactOccurrence, QueueRepeatMode, ReadCancellation};

use super::support::{connection, fixture};

#[tokio::test]
async fn queue_restores_exact_orders_duplicates_state_and_offline_media() {
    let fixture = fixture().await;
    fixture
        .database
        .upsert_local_access(
            fixture.source,
            &LocalAccessWrite {
                track_object_id: Some("track-0".to_string()),
                path: "/downloads/track-0.flac".to_string(),
                root: "/downloads".to_string(),
                relative_path: "track-0.flac".to_string(),
                size_bytes: 10,
                mtime_ns: 1,
                device_id: None,
                inode: None,
                parser_version: 1,
                title: "Alpha".to_string(),
                album: "Album A".to_string(),
                artist: "Artist A".to_string(),
                disc_number: 1,
                track_number: 1,
                duration_millis: 180_000,
                media_uri: "file:///downloads/track-0.flac".to_string(),
                loudness_analysis_key: [6; 32],
            },
        )
        .await
        .expect("write current Local access");
    let mut setup = connection(&fixture.path).await;
    sqlx::query("DELETE FROM track_artists WHERE track_key=?1")
        .bind(fixture.tracks[0])
        .execute(&mut setup)
        .await
        .expect("leave only Album Artist identity");
    sqlx::query("UPDATE albums SET artwork_binding=?2 WHERE album_key=(SELECT album_key FROM tracks WHERE track_key=?1)")
        .bind(fixture.tracks[0])
        .bind(b"album-art".as_slice())
        .execute(&mut setup)
        .await
        .expect("persist selected Album binding");
    sqlx::query("INSERT INTO queue_occurrences(source_key,object_id,position,traversal_position,provenance_kind,provenance_context_id,provenance_source_rank,track_key,track_object_id,fallback_primary_artist_object_id) VALUES (?1,'first',0,1,'context','album-route',2,?2,'track-0','stale-artist'),(?1,'duplicate',1,2,'manual',NULL,NULL,?2,'track-0',NULL)")
        .bind(fixture.source)
        .bind(fixture.tracks[0])
        .execute(&mut setup)
        .await
        .expect("seed current compact Queue rows");
    sqlx::query("INSERT INTO queue_occurrences(source_key,object_id,position,traversal_position,provenance_kind,track_key,track_object_id,fallback_title,fallback_artist,fallback_album,fallback_album_display_artist,fallback_album_object_id,fallback_primary_artist_object_id,fallback_media_uri,fallback_artwork_binding,fallback_duration_millis,fallback_disc_number,fallback_track_number,fallback_year,fallback_favorite,fallback_source_format,fallback_musicbrainz_recording_id,fallback_cue_path,fallback_cue_start_millis,fallback_cue_end_millis) VALUES (?1,'offline',2,0,'auto-dj',NULL,'offline-track','Offline','Offline Artist','Offline Album','Offline Album Artist','offline-album','offline-artist','file:///offline.flac',?2,42000,1,7,2022,1,'FLAC','offline-recording','/music/offline.cue',1000,43000)")
        .bind(fixture.source)
        .bind(b"local-art".as_slice())
        .execute(&mut setup)
        .await
        .expect("seed persisted offline fallback");
    sqlx::query("INSERT INTO queue_state(source_key,current_occurrence_key,prepared_next_occurrence_key,progress_millis,repeat_mode,shuffled) SELECT ?1,(SELECT queue_occurrence_key FROM queue_occurrences WHERE source_key=?1 AND object_id='offline'),(SELECT queue_occurrence_key FROM queue_occurrences WHERE source_key=?1 AND object_id='first'),1500,'all',1")
        .bind(fixture.source)
        .execute(&mut setup)
        .await
        .expect("seed compact Queue state");
    drop(setup);
    let restored = fixture
        .database
        .restore_queue(fixture.source)
        .await
        .expect("restore Queue");
    assert_eq!(restored.occurrences.len(), 3);
    assert_eq!(
        restored.occurrences[0].occurrence_key,
        restored.state.current
    );
    assert_eq!(restored.current.as_ref().unwrap().title, "Offline");
    assert_eq!(
        restored
            .current
            .as_ref()
            .unwrap()
            .album_display_artist
            .as_deref(),
        Some("Offline Album Artist")
    );
    assert_eq!(
        restored
            .current
            .as_ref()
            .unwrap()
            .album_object_id
            .as_deref(),
        Some("offline-album")
    );
    assert_eq!(
        restored.current.as_ref().unwrap().cue_start_millis,
        Some(1_000)
    );
    assert_eq!(
        restored.prepared_next.as_ref().unwrap().track_key,
        Some(fixture.tracks[0])
    );
    assert_eq!(
        restored
            .prepared_next
            .as_ref()
            .unwrap()
            .media_uri
            .as_deref(),
        Some("file:///downloads/track-0.flac")
    );
    assert_eq!(
        restored
            .prepared_next
            .as_ref()
            .unwrap()
            .primary_artist_object_id
            .as_deref(),
        Some("artist-a")
    );
    assert_eq!(
        restored
            .prepared_next
            .as_ref()
            .unwrap()
            .album_display_artist
            .as_deref(),
        Some("Artist A")
    );
    assert_eq!(
        restored
            .prepared_next
            .as_ref()
            .unwrap()
            .artwork_binding
            .as_deref(),
        Some(b"album-art".as_slice())
    );
    let cancel = ReadCancellation::new();
    assert_eq!(
        fixture
            .database
            .track_rows(fixture.source, &[fixture.tracks[0]], &cancel)
            .await
            .expect("Track projection")
            .pop()
            .unwrap()
            .artwork_binding
            .as_deref(),
        Some(b"album-art".as_slice())
    );
    let page = fixture
        .database
        .queue_page(fixture.source, None, "", 256, &cancel)
        .await
        .expect("Queue page");
    assert_eq!(
        page.iter()
            .map(|row| row.object_id.as_str())
            .collect::<Vec<_>>(),
        ["first", "duplicate", "offline"]
    );
    assert_eq!(page[0].track_key, page[1].track_key);
    assert_eq!(
        page[0].artwork_binding.as_deref(),
        Some(b"album-art".as_slice())
    );
    assert_eq!(
        page[0].primary_artist_object_id.as_deref(),
        Some("artist-a")
    );
    assert_ne!(page[0].occurrence_key, page[1].occurrence_key);
    assert_eq!(page[2].source_format.as_deref(), Some("FLAC"));
    let traversal = [
        page[1].occurrence_key,
        page[2].occurrence_key,
        page[0].occurrence_key,
    ];
    fixture
        .database
        .persist_queue_traversal(fixture.source, &traversal)
        .await
        .expect("persist traversal only");
    let traversal_restore = fixture
        .database
        .restore_queue(fixture.source)
        .await
        .expect("restore traversal-only update");
    assert_eq!(
        traversal_restore
            .occurrences
            .iter()
            .filter_map(|occurrence| occurrence.occurrence_key)
            .collect::<Vec<_>>(),
        traversal
    );
    assert_eq!(traversal_restore.state.current, restored.state.current);
    assert_eq!(
        traversal_restore.state.prepared_next,
        restored.state.prepared_next
    );
    let canonical_after = fixture
        .database
        .queue_page(fixture.source, None, "", 256, &cancel)
        .await
        .expect("canonical Queue after traversal update");
    assert_eq!(
        canonical_after
            .iter()
            .map(|row| row.object_id.as_str())
            .collect::<Vec<_>>(),
        ["first", "duplicate", "offline"]
    );
    assert_eq!(
        canonical_after[2].cue_path.as_deref(),
        Some("/music/offline.cue")
    );
    assert!(
        fixture
            .database
            .queue_page(fixture.source, None, "offline artist", 2, &cancel)
            .await
            .expect("page-local Queue filter")
            .is_empty()
    );
    assert_eq!(
        fixture
            .database
            .queue_page(fixture.source, None, "offline artist", 10, &cancel)
            .await
            .expect("Queue filter")
            .len(),
        1
    );
    let compact = canonical_after
        .iter()
        .rev()
        .enumerate()
        .map(|(position, row)| QueueCompactOccurrence {
            occurrence_key: Some(row.occurrence_key),
            object_id: row.object_id.clone(),
            track_key: row.track_key,
            canonical_position: position as i64,
            traversal_position: position as i64,
            provenance: row.provenance.clone(),
        })
        .collect::<Vec<_>>();
    let compact_restore = fixture
        .database
        .persist_compact_queue(
            fixture.source,
            &compact,
            Some("offline"),
            Some("duplicate"),
            1_750,
            QueueRepeatMode::All,
            true,
        )
        .await
        .expect("persist compact Queue");
    assert_eq!(compact_restore.occurrences.len(), 3);
    assert_eq!(compact_restore.current.as_ref().unwrap().title, "Offline");
    assert_eq!(
        compact_restore
            .current
            .as_ref()
            .unwrap()
            .cue_path
            .as_deref(),
        Some("/music/offline.cue")
    );
    assert_eq!(
        fixture
            .database
            .queue_page(fixture.source, None, "", 10, &cancel)
            .await
            .expect("compact canonical Queue")
            .iter()
            .map(|row| row.object_id.as_str())
            .collect::<Vec<_>>(),
        ["offline", "duplicate", "first"]
    );
    assert!(
        fixture
            .database
            .move_queue_occurrence(fixture.source, page[1].occurrence_key, 0, 0)
            .await
            .expect("move Queue occurrence")
    );
    let moved = fixture
        .database
        .queue_page(fixture.source, None, "", 10, &cancel)
        .await
        .expect("moved Queue page");
    assert_eq!(moved[0].object_id, "duplicate");
    assert!(
        fixture
            .database
            .remove_queue_occurrence(fixture.source, moved[1].occurrence_key)
            .await
            .expect("remove Queue occurrence")
    );
    assert_eq!(
        fixture
            .database
            .restore_queue(fixture.source)
            .await
            .expect("restore edited Queue")
            .occurrences
            .len(),
        2
    );

    let mut raw = connection(&fixture.path).await;
    let canonical_plan = sqlx::query_as::<_, (i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT queue_occurrence_key FROM queue_occurrences WHERE source_key=?1 AND position>?2 ORDER BY position LIMIT 256")
        .bind(fixture.source).bind(-1_i64).fetch_all(&mut raw).await.expect("Queue page plan").into_iter().map(|row| row.3).collect::<Vec<_>>().join(" | ");
    assert!(
        canonical_plan.contains("queue_occurrences_page_idx"),
        "{canonical_plan}"
    );
    let traversal_plan = sqlx::query_as::<_, (i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT queue_occurrence_key FROM queue_occurrences WHERE source_key=?1 ORDER BY traversal_position")
        .bind(fixture.source).fetch_all(&mut raw).await.expect("Queue traversal plan").into_iter().map(|row| row.3).collect::<Vec<_>>().join(" | ");
    assert!(
        traversal_plan.contains("queue_occurrences_traversal_idx"),
        "{traversal_plan}"
    );
}

#[tokio::test]
async fn compact_queue_reuses_object_identity_before_the_persisted_key_is_known() {
    let fixture = fixture().await;
    let occurrence = QueueCompactOccurrence {
        occurrence_key: None,
        object_id: "new-live-occurrence".to_string(),
        track_key: Some(fixture.tracks[0]),
        canonical_position: 0,
        traversal_position: 0,
        provenance: library::QueueProvenance::Manual,
    };

    for _ in 0..2 {
        fixture
            .database
            .persist_compact_queue(
                fixture.source,
                std::slice::from_ref(&occurrence),
                Some(&occurrence.object_id),
                None,
                0,
                QueueRepeatMode::None,
                false,
            )
            .await
            .expect("persist the same live occurrence");
    }

    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM queue_occurrences WHERE source_key=?1 AND object_id=?2",
        )
        .bind(fixture.source)
        .bind(&occurrence.object_id)
        .fetch_one(&mut raw)
        .await
        .expect("count persisted occurrence"),
        1
    );
}
