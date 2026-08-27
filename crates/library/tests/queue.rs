use library::{
    LocalAccessWrite, QueueCompactOccurrence, QueueRepeatMode, ReadCancellation,
    SmartPlaylistDefinition,
};

use super::support::{connection, fixture, persist_queue};

#[tokio::test]
async fn queue_restores_exact_orders_duplicates_state_and_offline_media() {
    let fixture = fixture().await;
    fixture
        .database
        .upsert_local_access(
            fixture.source,
            &LocalAccessWrite {
                track_object_id: Some("track-0".to_string()),
                origin: library::LocalAccessOrigin::Mapping,
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
    sqlx::query("UPDATE tracks SET artwork_binding=?2 WHERE track_key=?1")
        .bind(fixture.tracks[0])
        .bind(b"stale-track-art".as_slice())
        .execute(&mut setup)
        .await
        .expect("persist stale Track binding");
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
    assert_eq!(restored.current_occurrence.as_deref(), Some("offline"));
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
    assert_eq!(page[0].album_key, Some(fixture.albums[0]));
    assert_eq!(page[0].primary_artist_key, Some(fixture.artists[0]));
    assert_ne!(page[0].occurrence_key, page[1].occurrence_key);
    assert_eq!(page[2].source_format.as_deref(), Some("FLAC"));
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
            object_id: row.object_id.clone(),
            track_key: row.track_key,
            canonical_position: position as i64,
            traversal_position: position as i64,
            provenance: row.provenance.clone(),
        })
        .collect::<Vec<_>>();
    persist_queue(
        &fixture.database,
        fixture.source,
        &compact,
        Some("offline"),
        Some("duplicate"),
        1_750,
        QueueRepeatMode::All,
        true,
    )
    .await;
    let compact_restore = fixture
        .database
        .restore_queue(fixture.source)
        .await
        .expect("restore compact Queue");
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
        object_id: "new-live-occurrence".to_string(),
        track_key: Some(fixture.tracks[0]),
        canonical_position: 0,
        traversal_position: 0,
        provenance: library::QueueProvenance::Manual,
    };

    for _ in 0..2 {
        persist_queue(
            &fixture.database,
            fixture.source,
            std::slice::from_ref(&occurrence),
            Some(&occurrence.object_id),
            None,
            0,
            QueueRepeatMode::None,
            false,
        )
        .await;
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

#[tokio::test]
async fn compact_queue_persistence_pages_duplicate_occurrences_without_losing_order() {
    let fixture = fixture().await;
    let occurrences = (0..260)
        .map(|position| QueueCompactOccurrence {
            object_id: format!("paged-{position}"),
            track_key: Some(fixture.tracks[position % fixture.tracks.len()]),
            canonical_position: position as i64,
            traversal_position: (259 - position) as i64,
            provenance: library::QueueProvenance::Manual,
        })
        .collect::<Vec<_>>();
    persist_queue(
        &fixture.database,
        fixture.source,
        &occurrences,
        Some("paged-129"),
        Some("paged-128"),
        23_000,
        QueueRepeatMode::All,
        true,
    )
    .await;

    let restored = fixture
        .database
        .restore_queue(fixture.source)
        .await
        .expect("restore paged Queue");
    assert_eq!(restored.occurrences.len(), 260);
    assert_eq!(restored.occurrences[0].object_id, "paged-259");
    assert_eq!(restored.occurrences[259].object_id, "paged-0");
    assert_eq!(restored.progress_millis, 23_000);
    assert_eq!(restored.current_occurrence.as_deref(), Some("paged-129"));
    assert_eq!(
        restored.prepared_next_occurrence.as_deref(),
        Some("paged-128")
    );
}

#[tokio::test]
async fn occurrence_media_and_progress_follow_the_requested_transition() {
    let fixture = fixture().await;
    let mut setup = connection(&fixture.path).await;
    for (position, track) in fixture.tracks.iter().take(3).enumerate() {
        sqlx::query("UPDATE tracks SET media_uri=?2 WHERE track_key=?1")
            .bind(track)
            .bind(format!("file:///transition-{position}.flac"))
            .execute(&mut setup)
            .await
            .expect("set exact transition URI");
        sqlx::query(
            "INSERT INTO loudness_measurements(
                 source_key,entity_kind,entity_key,analysis_key,
                 integrated_lufs,true_peak,origin
             ) SELECT source_key,'track',track_key,loudness_analysis_key,?2,NULL,'analysis'
               FROM tracks WHERE track_key=?1",
        )
        .bind(track)
        .bind(-18.0 + position as f64)
        .execute(&mut setup)
        .await
        .expect("set exact transition loudness");
    }
    drop(setup);
    let occurrences = fixture
        .tracks
        .iter()
        .take(3)
        .enumerate()
        .map(|(position, track)| QueueCompactOccurrence {
            object_id: format!("transition-{position}"),
            track_key: Some(*track),
            canonical_position: position as i64,
            traversal_position: position as i64,
            provenance: library::QueueProvenance::Manual,
        })
        .collect::<Vec<_>>();
    persist_queue(
        &fixture.database,
        fixture.source,
        &occurrences,
        Some("transition-0"),
        Some("transition-1"),
        0,
        QueueRepeatMode::None,
        false,
    )
    .await;

    for (position, occurrence) in occurrences.iter().enumerate() {
        let media = fixture
            .database
            .queue_media_for_occurrence(fixture.source, &occurrence.object_id)
            .await
            .expect("resolve exact occurrence")
            .expect("occurrence media");
        assert_eq!(media.track_key, occurrence.track_key);
        assert_eq!(
            media.media_uri.as_deref(),
            Some(format!("file:///transition-{position}.flac").as_str())
        );
        assert_eq!(media.track_number, Some(position as i64 + 1));
        assert_eq!(media.title, ["Alpha", "Beta", "Gamma"][position]);
        let loudness = fixture
            .database
            .track_loudness(
                fixture.source,
                occurrence.track_key.expect("transition Track"),
                &ReadCancellation::new(),
            )
            .await
            .expect("resolve exact transition loudness")
            .expect("transition loudness");
        assert_eq!(loudness.integrated_lufs, Some(-18.0 + position as f64));
        fixture
            .database
            .persist_queue_progress(
                fixture.source,
                Some(&occurrence.object_id),
                (position as i64 + 1) * 1000,
            )
            .await
            .expect("persist exact progress");
        let restored = fixture
            .database
            .restore_queue(fixture.source)
            .await
            .expect("restore progress");
        assert_eq!(
            restored.current_occurrence.as_deref(),
            Some(occurrence.object_id.as_str())
        );
        assert_eq!(restored.progress_millis, (position as i64 + 1) * 1000);
    }
}

#[tokio::test]
async fn album_playlist_and_smart_playlist_materialize_exact_queue_media() {
    let fixture = fixture().await;
    let cancellation = ReadCancellation::new();
    let album = fixture
        .database
        .album_track_order(
            fixture.source,
            fixture.albums[0],
            None,
            "",
            library::TrackSort::TrackNumber,
            false,
            &cancellation,
        )
        .await
        .expect("Album Track order");
    let playlist = fixture
        .database
        .create_playlist(fixture.source, "Collection", &fixture.tracks[..3])
        .await
        .expect("create Playlist")
        .expect("Playlist key");
    let playlist = fixture
        .database
        .playlist_track_order(fixture.source, playlist, None, &cancellation)
        .await
        .expect("Playlist Track order");
    let smart = fixture
        .database
        .create_smart_playlist(
            fixture.source,
            "Everything",
            &SmartPlaylistDefinition::default(),
        )
        .await
        .expect("create Smart Playlist");
    let smart = fixture
        .database
        .smart_playlist_track_order(fixture.source, smart, None, 0, &cancellation)
        .await
        .expect("Smart Playlist Track order");

    for (name, order) in [("album", album), ("playlist", playlist), ("smart", smart)] {
        assert!(!order.is_empty(), "{name} order");
        let occurrences = order
            .iter()
            .enumerate()
            .map(|(position, track)| QueueCompactOccurrence {
                object_id: format!("{name}-{position}"),
                track_key: Some(*track),
                canonical_position: position as i64,
                traversal_position: position as i64,
                provenance: library::QueueProvenance::Context {
                    context_id: name.to_string(),
                    source_rank: position as i64,
                },
            })
            .collect::<Vec<_>>();
        persist_queue(
            &fixture.database,
            fixture.source,
            &occurrences,
            Some(&occurrences[0].object_id),
            occurrences.get(1).map(|entry| entry.object_id.as_str()),
            0,
            QueueRepeatMode::None,
            false,
        )
        .await;
        for (position, occurrence) in occurrences.iter().enumerate() {
            let media = fixture
                .database
                .queue_media_for_occurrence(fixture.source, &occurrence.object_id)
                .await
                .expect("resolve collection occurrence")
                .expect("collection media");
            assert_eq!(media.track_key, Some(order[position]));
            let expected_object = fixture
                .database
                .track_rows(fixture.source, &[order[position]], &cancellation)
                .await
                .expect("expected collection Track")
                .pop()
                .expect("expected Track row")
                .object_id;
            assert_eq!(media.track_object_id, expected_object);
            assert_eq!(media.media_uri.as_deref(), Some("file:///track.flac"));
        }
    }
}
