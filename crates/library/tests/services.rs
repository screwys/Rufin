use library::{
    ListenDeliveryTarget, ListenWrite, LocalAccessWrite, LocalFileKind, LocalFileState,
    LocalFileWrite, LoudnessMeasurement, QueueCompactOccurrence, QueueProvenance, QueueRepeatMode,
    ReadCancellation,
};

use super::support::{connection, fixture, persist_queue};

#[tokio::test]
async fn loudness_selects_one_unit_and_source_facts_win() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let work = fixture
        .database
        .next_missing_track_loudness(fixture.source, &cancel)
        .await
        .expect("missing Track loudness")
        .unwrap();
    assert_eq!(work.track_key, fixture.tracks[0]);
    let analyzed = LoudnessMeasurement {
        analysis_key: work.expected_analysis_key,
        integrated_lufs: Some(-14.0),
        true_peak: Some(0.8),
    };
    assert!(
        fixture
            .database
            .write_track_analyzed_loudness(fixture.source, work.track_key, &analyzed)
            .await
            .expect("write analyzed loudness")
    );
    assert!(
        !fixture
            .database
            .write_track_analyzed_loudness(
                fixture.source,
                work.track_key,
                &LoudnessMeasurement {
                    analysis_key: [2; 32],
                    integrated_lufs: Some(-8.0),
                    true_peak: Some(1.0)
                }
            )
            .await
            .expect("reject a stale work identity")
    );
    let source_fact = LoudnessMeasurement {
        analysis_key: work.expected_analysis_key,
        integrated_lufs: Some(-11.0),
        true_peak: None,
    };
    assert!(
        fixture
            .database
            .write_track_source_loudness(fixture.source, work.track_key, &source_fact)
            .await
            .expect("accepted source gain wins")
    );
    assert_eq!(
        fixture
            .database
            .track_loudness(fixture.source, work.track_key, &cancel)
            .await
            .expect("read Track loudness"),
        Some(source_fact.clone())
    );
    fixture
        .database
        .update_track_metadata(
            fixture.source,
            work.track_key,
            library::TrackMetadataWrite {
                title: "Alpha".to_string(),
                normalized_search: "alpha album a artist a note".to_string(),
                display_album: "Album A".to_string(),
                display_artist: "Artist A".to_string(),
                sort_text: "alpha".to_string(),
                duration_millis: 180_000,
                disc_number: 1,
                track_number: 1,
                year: Some(2020),
                release_date: None,
                date_added: Some("2024-01-02".to_string()),
                media_uri: Some("file:///track.flac".to_string()),
                source_format: Some("FLAC".to_string()),
                comment: Some("Note".to_string()),
                bpm: Some(100),
                musicbrainz_recording_id: None,
                musicbrainz_release_track_id: None,
                cue_path: Some("/music/changed.cue".to_string()),
                cue_start_millis: Some(1_000),
                cue_end_millis: Some(181_000),
                loudness_analysis_key: [42; 32],
            },
        )
        .await
        .expect("change Track CUE identity")
        .unwrap();
    let changed = fixture
        .database
        .next_missing_track_loudness(fixture.source, &cancel)
        .await
        .expect("changed Track loudness work")
        .unwrap();
    assert_eq!(changed.track_key, work.track_key);
    assert_ne!(changed.expected_analysis_key, work.expected_analysis_key);
    assert!(
        !fixture
            .database
            .write_track_source_loudness(fixture.source, changed.track_key, &source_fact)
            .await
            .expect("reject delayed source result")
    );
    assert!(
        !fixture
            .database
            .write_track_analyzed_loudness(
                fixture.source,
                changed.track_key,
                &LoudnessMeasurement {
                    analysis_key: work.expected_analysis_key,
                    integrated_lufs: Some(-9.0),
                    true_peak: Some(0.7)
                }
            )
            .await
            .expect("reject stale analyzer completion")
    );
    assert!(
        !fixture
            .database
            .write_track_analyzed_loudness(
                fixture.source,
                changed.track_key,
                &LoudnessMeasurement {
                    analysis_key: changed.expected_analysis_key,
                    integrated_lufs: Some(-9.0),
                    true_peak: Some(0.7)
                }
            )
            .await
            .expect("do not replace newer source fact")
    );
    let current_source = LoudnessMeasurement {
        analysis_key: changed.expected_analysis_key,
        integrated_lufs: Some(-10.0),
        true_peak: None,
    };
    assert!(
        fixture
            .database
            .write_track_source_loudness(fixture.source, changed.track_key, &current_source)
            .await
            .expect("refresh source gain identity")
    );
    assert_eq!(
        fixture
            .database
            .track_loudness(fixture.source, changed.track_key, &cancel)
            .await
            .unwrap(),
        Some(current_source)
    );

    let album = fixture
        .database
        .next_missing_album_loudness(fixture.source, &cancel)
        .await
        .expect("missing Album loudness")
        .unwrap();
    assert_eq!(album.tracks.len(), 2);
    let album_measurement = LoudnessMeasurement {
        analysis_key: album.expected_analysis_key,
        integrated_lufs: Some(-13.0),
        true_peak: Some(0.85),
    };
    assert!(
        fixture
            .database
            .write_album_analyzed_loudness(fixture.source, album.album_key, &album_measurement)
            .await
            .expect("write Album loudness")
    );
    let old_album_key = album.expected_analysis_key;
    fixture
        .database
        .upsert_local_access(
            fixture.source,
            &LocalAccessWrite {
                track_object_id: Some("track-1".to_string()),
                origin: library::LocalAccessOrigin::Mapping,
                path: "/generic/local-track.flac".to_string(),
                root: "/generic".to_string(),
                relative_path: "local-track.flac".to_string(),
                size_bytes: 10,
                mtime_ns: 1,
                device_id: Some(2),
                inode: Some(3),
                parser_version: 1,
                title: "Beta".to_string(),
                album: "Album A".to_string(),
                artist: "Artist A".to_string(),
                disc_number: 1,
                track_number: 2,
                duration_millis: 181_000,
                media_uri: "file:///generic/local-track.flac".to_string(),
                loudness_analysis_key: [77; 32],
            },
        )
        .await
        .expect("install exact Local audio identity");
    let mut scan = library::Scan::begin(&fixture.database, "source", "Source", "source", None)
        .await
        .expect("begin membership-changing Scan");
    for (id, title, artist, sort, date) in [
        ("album-a", "Album A", "Artist A", "album a", "2024-01-02"),
        ("album-b", "Album B", "Artist B", "album b", "2023-01-02"),
    ] {
        scan.write_album(
            id,
            title,
            sort,
            artist,
            sort,
            Some(2024),
            Some(date),
            Some(date),
            None,
            None,
            Some(id == "album-a"),
            None,
            false,
            None,
            None,
        )
        .await
        .expect("stage Album");
    }
    for (index, album_id, artist, title) in [
        (0, "album-a", "Artist A", "Alpha"),
        (1, "album-a", "Artist A", "Beta"),
        (2, "album-a", "Artist B", "Gamma"),
        (3, "album-b", "Artist B", "Delta"),
    ] {
        scan.write_track(
            &format!("track-{index}"),
            Some(album_id),
            title,
            &format!("{} {} note", title.to_lowercase(), artist.to_lowercase()),
            if album_id == "album-a" {
                "Album A"
            } else {
                "Album B"
            },
            artist,
            &title.to_lowercase(),
            180_000 + index * 1_000,
            1,
            index + 1,
            Some(2020 + index),
            None,
            Some("2024-01-02"),
            Some("file:///track.flac"),
            Some("FLAC"),
            Some("Note"),
            Some(100 + index),
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            [index as u8 + 10; 32],
        )
        .await
        .expect("stage Track");
    }
    scan.write_track_source_loudness("track-1", Some(-15.0), None)
        .await
        .expect("stage Track R128 fact");
    scan.write_track_source_loudness("track-2", Some(-16.0), Some(0.75))
        .await
        .expect("stage current Track R128 fact");
    scan.write_album_source_loudness("album-b", Some(-13.0), None)
        .await
        .expect("stage Album R128 fact");
    assert!(matches!(
        scan.finish().await.expect("publish membership change"),
        library::ScanOutcome::Changed(_)
    ));
    assert!(
        fixture
            .database
            .track_loudness(fixture.source, work.track_key, &cancel)
            .await
            .unwrap()
            .is_none(),
        "unstaged old source fact is removed"
    );
    assert!(
        fixture
            .database
            .track_loudness(fixture.source, fixture.tracks[1], &cancel)
            .await
            .unwrap()
            .is_none(),
        "a source fact cannot claim the current Local audio identity"
    );
    let staged_track = fixture
        .database
        .track_loudness(fixture.source, fixture.tracks[2], &cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(staged_track.integrated_lufs, Some(-16.0));
    assert_eq!(staged_track.true_peak, Some(0.75));
    assert_eq!(
        fixture
            .database
            .album_loudness(fixture.source, fixture.albums[1], &cancel)
            .await
            .unwrap()
            .unwrap()
            .integrated_lufs,
        Some(-13.0)
    );
    let changed_album = fixture
        .database
        .next_missing_album_loudness(fixture.source, &cancel)
        .await
        .expect("changed Album membership work")
        .unwrap();
    assert_eq!(changed_album.album_key, album.album_key);
    assert_ne!(changed_album.expected_analysis_key, old_album_key);
    assert!(
        fixture
            .database
            .write_album_analyzed_loudness(
                fixture.source,
                changed_album.album_key,
                &LoudnessMeasurement {
                    analysis_key: changed_album.expected_analysis_key,
                    integrated_lufs: Some(-12.5),
                    true_peak: Some(0.82)
                }
            )
            .await
            .expect("replace stale analyzed Album value")
    );

    let mut raw = connection(&fixture.path).await;
    let plan=sqlx::query_as::<_,(i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT analysis_key FROM loudness_measurements WHERE source_key=?1 AND entity_kind='track' AND entity_key=?2")
        .bind(fixture.source).bind(work.track_key).fetch_all(&mut raw).await.expect("loudness point plan").into_iter().map(|row|row.3).collect::<Vec<_>>().join(" | ");
    assert!(
        plan.contains("sqlite_autoindex_loudness_measurements_1"),
        "{plan}"
    );
}

#[tokio::test]
async fn loudness_keeps_provider_identity_when_remote_media_has_no_direct_uri() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET media_uri=NULL")
        .execute(&mut raw)
        .await
        .expect("remove direct remote media URIs");

    let track = fixture
        .database
        .next_missing_track_loudness(fixture.source, &cancel)
        .await
        .expect("select provider-resolved Track loudness")
        .expect("provider Track loudness work");
    assert_eq!(track.track_key, fixture.tracks[0]);
    assert_eq!(track.track_object_id, "track-0");
    assert_eq!(track.media_uri, None);

    let album = fixture
        .database
        .next_missing_album_loudness(fixture.source, &cancel)
        .await
        .expect("select provider-resolved Album loudness")
        .expect("provider Album loudness work");
    assert_eq!(album.tracks.len(), 2);
    assert!(album.tracks.iter().all(|track| track.media_uri.is_none()));
    assert_eq!(album.tracks[0].track_object_id, "track-0");
}

#[tokio::test]
async fn artwork_pages_distinct_opaque_bindings_by_digest() {
    let fixture = fixture().await;
    let digest = [9; 32];
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET artwork_binding=?2 WHERE track_key=?1")
        .bind(fixture.tracks[0])
        .bind(b"binding-a".as_slice())
        .execute(&mut raw)
        .await
        .expect("hydrate Track artwork");
    sqlx::query("UPDATE albums SET artwork_binding=?2 WHERE album_key=?1")
        .bind(fixture.albums[0])
        .bind(b"binding-a".as_slice())
        .execute(&mut raw)
        .await
        .expect("hydrate duplicate Album artwork");
    sqlx::query("UPDATE artists SET artwork_binding=?2 WHERE artist_key=?1")
        .bind(fixture.artists[0])
        .bind(b"binding-b".as_slice())
        .execute(&mut raw)
        .await
        .expect("hydrate Artist artwork");
    sqlx::query("UPDATE sources SET artwork_digest=?2 WHERE source_key=?1")
        .bind(fixture.source)
        .bind(digest.as_slice())
        .execute(&mut raw)
        .await
        .expect("hydrate artwork digest");
    let cancel = ReadCancellation::new();
    let first = fixture
        .database
        .artwork_preparation_page(fixture.source, None, 1, &cancel)
        .await
        .expect("first artwork page");
    assert_eq!(first.len(), 1);
    let second = fixture
        .database
        .artwork_preparation_page(fixture.source, Some(&first[0]), 128, &cancel)
        .await
        .expect("resume artwork page");
    assert_eq!(second, [b"binding-b".to_vec()]);
    let plan=sqlx::query_as::<_,(i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT artwork_binding FROM tracks WHERE source_key=?1 AND artwork_binding IS NOT NULL AND artwork_binding>?2 ORDER BY artwork_binding LIMIT 128")
        .bind(fixture.source).bind(b"binding-a".as_slice()).fetch_all(&mut raw).await.expect("artwork preparation plan").into_iter().map(|row|row.3).collect::<Vec<_>>().join(" | ");
    assert!(plan.contains("tracks_artwork_idx"), "{plan}");
}

#[tokio::test]
async fn local_rows_keep_dependencies_and_point_resolution_precedence() {
    let fixture = fixture().await;
    let cue = LocalFileWrite {
        path: "/music/album.cue".to_string(),
        root: "/music".to_string(),
        relative_path: "album.cue".to_string(),
        kind: LocalFileKind::Cue,
        size_bytes: Some(100),
        mtime_ns: 20,
        device_id: Some(1),
        inode: Some(2),
        parse_version: Some(1),
        state: LocalFileState::Accepted,
    };
    let dependencies = (0..300)
        .map(|index| format!("/generic/component-{index:03}.bin"))
        .collect::<Vec<_>>();
    let mut raw = connection(&fixture.path).await;
    let key = sqlx::query_scalar::<_, library::LocalFileKey>(
        "INSERT INTO local_files(source_key,path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,parse_version,state)
         VALUES(?1,?2,?3,?4,'cue',?5,?6,?7,?8,?9,'accepted') RETURNING local_file_key",
    )
    .bind(fixture.source)
    .bind(&cue.path)
    .bind(&cue.root)
    .bind(&cue.relative_path)
    .bind(cue.size_bytes)
    .bind(cue.mtime_ns)
    .bind(cue.device_id)
    .bind(cue.inode)
    .bind(cue.parse_version)
    .fetch_one(&mut raw)
    .await
    .expect("write Local CUE observation");
    for (position, dependency) in dependencies.iter().enumerate() {
        sqlx::query("INSERT INTO local_file_dependencies(local_file_key,dependency_path,position) VALUES(?1,?2,?3)")
            .bind(key)
            .bind(dependency)
            .bind(position as i64)
            .execute(&mut raw)
            .await
            .expect("write Local CUE dependency");
    }
    drop(raw);
    let cancel = ReadCancellation::new();
    let page = fixture
        .database
        .local_file_page(fixture.source, None, 128, &cancel)
        .await
        .expect("Local file page");
    assert_eq!(page[0].local_file_key, key);
    assert_eq!(page[0].dependencies, dependencies);
    let identity = fixture
        .database
        .local_file_reuse_candidates(fixture.source, std::slice::from_ref(&cue), &cancel)
        .await
        .expect("Local identity")
        .remove(0);
    assert_eq!(identity.path, "/music/album.cue");
    assert_eq!(identity.dependencies, dependencies);
    let metadata = LocalAccessWrite {
        track_object_id: None,
        origin: library::LocalAccessOrigin::Mapping,
        path: "/downloads/metadata.flac".to_string(),
        root: "/downloads".to_string(),
        relative_path: "metadata.flac".to_string(),
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
        media_uri: "file:///downloads/metadata.flac".to_string(),
        loudness_analysis_key: [7; 32],
    };
    fixture
        .database
        .upsert_local_access(fixture.source, &metadata)
        .await
        .expect("write metadata Local access");
    let exact = LocalAccessWrite {
        track_object_id: Some("track-0".to_string()),
        origin: library::LocalAccessOrigin::Mapping,
        path: "/downloads/exact.flac".to_string(),
        media_uri: "file:///downloads/exact.flac".to_string(),
        loudness_analysis_key: [8; 32],
        ..metadata.clone()
    };
    fixture
        .database
        .upsert_local_access(fixture.source, &exact)
        .await
        .expect("write exact Local access");
    let local_loudness = fixture
        .database
        .next_missing_track_loudness(fixture.source, &cancel)
        .await
        .expect("Local loudness identity")
        .unwrap();
    assert_eq!(local_loudness.track_key, fixture.tracks[0]);
    assert_eq!(local_loudness.expected_analysis_key, [8; 32]);
    let resolved = fixture
        .database
        .resolve_local_access(
            fixture.source,
            Some("track-0"),
            "Alpha",
            "Album A",
            "Artist A",
            1,
            1,
            180_000,
        )
        .await
        .expect("resolve exact Local access")
        .unwrap();
    assert_eq!(resolved.media_uri, "file:///downloads/exact.flac");
    let matched = fixture
        .database
        .resolve_local_access(
            fixture.source,
            None,
            "Alpha",
            "Album A",
            "Artist A",
            1,
            1,
            180_000,
        )
        .await
        .expect("resolve metadata Local access")
        .unwrap();
    assert_eq!(matched.media_uri, "file:///downloads/metadata.flac");

    let mut raw = connection(&fixture.path).await;
    let identity_plan=sqlx::query_as::<_,(i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT local_file_key FROM local_files WHERE source_key=?1 AND device_id=?2 AND inode=?3")
        .bind(fixture.source).bind(1_i64).bind(2_i64).fetch_all(&mut raw).await.expect("Local identity plan").into_iter().map(|row|row.3).collect::<Vec<_>>().join(" | ");
    assert!(
        identity_plan.contains("local_files_identity_idx"),
        "{identity_plan}"
    );
    let access_plan=sqlx::query_as::<_,(i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT local_access_file_key FROM local_access_files WHERE source_key=?1 AND track_object_id=?2")
        .bind(fixture.source).bind("track-0").fetch_all(&mut raw).await.expect("Local access plan").into_iter().map(|row|row.3).collect::<Vec<_>>().join(" | ");
    assert!(
        access_plan.contains("local_access_precedence_idx"),
        "{access_plan}"
    );
    let match_plan=sqlx::query_as::<_,(i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT local_access_file_key FROM local_access_files WHERE source_key=?1 AND normalized_title=?2 AND normalized_album=?3 AND normalized_artist=?4 AND disc_number=?5 AND track_number=?6 AND duration_millis=?7")
        .bind(fixture.source).bind("alpha").bind("album a").bind("artist a").bind(1_i64).bind(1_i64).bind(180_000_i64).fetch_all(&mut raw).await.expect("Local metadata match plan").into_iter().map(|row|row.3).collect::<Vec<_>>().join(" | ");
    assert!(
        match_plan.contains("local_access_match_idx"),
        "{match_plan}"
    );
}

#[tokio::test]
async fn local_mapping_selects_one_exact_or_representative_track_without_a_library_walk() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET source_path='/server/music/track.flac' WHERE track_key=?1")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .expect("install provider source path");
    let representative = fixture
        .database
        .mapping_track_page(fixture.source, None, None, 1, &cancel)
        .await
        .expect("representative mapping Track")
        .pop()
        .expect("mapping Track");
    let exact = fixture
        .database
        .mapping_track_page(
            fixture.source,
            None,
            Some(&representative.source_path),
            1,
            &cancel,
        )
        .await
        .expect("exact mapping Track")
        .pop()
        .expect("exact mapping Track");

    assert_eq!(exact.track_key, representative.track_key);
    assert_eq!(exact.object_id, representative.object_id);
}

#[tokio::test]
async fn lyrics_cache_identity_includes_input_digest_and_evicts_bounded_rows() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let capacity_payload = "x".repeat(8 * 1024 * 1024);
    fixture
        .database
        .write_lyrics_cache(
            fixture.source,
            fixture.tracks[0],
            "source",
            "lyrics",
            "en",
            "Latn",
            [1; 32],
            "First",
            10,
        )
        .await
        .expect("write lyrics");
    let row = fixture
        .database
        .lyrics_cache_for_role(
            fixture.source,
            fixture.tracks[0],
            "lyrics",
            "en",
            "Latn",
            [1; 32],
            &cancel,
        )
        .await
        .expect("read lyrics")
        .unwrap();
    assert_eq!(row.lyrics, "First");
    assert!(
        fixture
            .database
            .lyrics_cache_for_role(
                fixture.source,
                fixture.tracks[0],
                "lyrics",
                "en",
                "Latn",
                [2; 32],
                &cancel
            )
            .await
            .expect("reject stale lyrics")
            .is_none()
    );
    fixture
        .database
        .write_lyrics_cache(
            fixture.source,
            fixture.tracks[1],
            "source",
            "lyrics",
            "",
            "",
            [2; 32],
            &capacity_payload,
            20,
        )
        .await
        .expect("write second lyrics");
    assert_eq!(
        fixture
            .database
            .lyrics_cache_for_role(
                fixture.source,
                fixture.tracks[1],
                "lyrics",
                "",
                "",
                [2; 32],
                &cancel
            )
            .await
            .expect("read capacity lyrics")
            .unwrap()
            .lyrics
            .len(),
        8 * 1024 * 1024
    );
    let mut oversized = capacity_payload.clone();
    oversized.push('x');
    assert!(
        fixture
            .database
            .write_lyrics_cache(
                fixture.source,
                fixture.tracks[2],
                "source",
                "lyrics",
                "",
                "",
                [3; 32],
                &oversized,
                30
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .database
            .remove_lyrics_cache(
                fixture.source,
                fixture.tracks[1],
                "source",
                "lyrics",
                "",
                ""
            )
            .await
            .expect("remove lyrics")
    );
}

#[tokio::test]
async fn source_removal_preserves_listens_and_pending_delivery_only() {
    let fixture = fixture().await;
    fixture
        .database
        .record_listen(
            fixture.source,
            &ListenWrite {
                external_id: "remove-play".to_string(),
                track_key: Some(fixture.tracks[0]),
                track_object_id: "track-0".to_string(),
                track_title: "Track".to_string(),
                artist_name: "Artist".to_string(),
                album_title: "Album".to_string(),
                started_at: 100,
                local_period: "1970-01".to_string(),
                duration_millis: 180_000,
                listened_millis: 1000,
                skipped: false,
            },
            &[ListenDeliveryTarget {
                service: "lastfm".to_string(),
                account_id: "account".to_string(),
                next_attempt_at: Some(200),
            }],
        )
        .await
        .expect("record retained listen");
    fixture
        .database
        .write_lyrics_cache(
            fixture.source,
            fixture.tracks[0],
            "source",
            "lyrics",
            "",
            "",
            [1; 32],
            "Lyrics",
            1,
        )
        .await
        .expect("write source lyrics");
    fixture
        .database
        .write_track_source_loudness(
            fixture.source,
            fixture.tracks[0],
            &LoudnessMeasurement {
                analysis_key: [1; 32],
                integrated_lufs: Some(-12.0),
                true_peak: Some(0.8),
            },
        )
        .await
        .expect("write source loudness");
    persist_queue(
        &fixture.database,
        fixture.source,
        &[QueueCompactOccurrence {
            object_id: "remove-queue".to_string(),
            track_key: Some(fixture.tracks[0]),
            canonical_position: 0,
            traversal_position: 0,
            provenance: QueueProvenance::Manual,
        }],
        Some("remove-queue"),
        None,
        0,
        QueueRepeatMode::None,
        false,
    )
    .await;
    assert!(
        fixture
            .database
            .remove_source(fixture.source)
            .await
            .expect("remove source")
    );
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
            "SELECT source_key,track_key FROM listens WHERE external_id='remove-play'"
        )
        .fetch_one(&mut raw)
        .await
        .expect("retained listen identity"),
        (None, None)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listen_outbox")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1
    );
    assert_eq!(sqlx::query_as::<_,(i64,i64,i64,i64,i64)>("SELECT (SELECT count(*) FROM sources),(SELECT count(*) FROM tracks),(SELECT count(*) FROM queue_occurrences),(SELECT count(*) FROM lyrics_cache),(SELECT count(*) FROM loudness_measurements)").fetch_one(&mut raw).await.unwrap(),(0,0,0,0,0));
}
