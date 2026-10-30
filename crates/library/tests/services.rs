use library::{
    ListenDeliveryTarget, ListenWrite, LoudnessMeasurement, QueueProvenance, QueueRepeatMode,
    QueueTransferRow, ReadCancellation,
};

use super::support::{PlaybackLoudness, connection, fixture, persist_queue};

/// Observes one fixture track again with this album, audio revision and source gain.
async fn observe_track(
    catalog: &library::Catalog,
    index: i64,
    album: &str,
    revision: [u8; 32],
    gain: Option<f64>,
) {
    let mut observation = catalog.observe("source").await.unwrap();
    let batch = library::CatalogBatch {
        tracks: vec![library::TrackObservation {
            metadata: library::TrackMetadata {
                title: format!("Track {index}"),
                sort_text: format!("track {index}"),
                duration_millis: 180_000,
                disc_number: 1,
                track_number: index + 1,
                source_loudness: gain.map(|lufs| library::NativeGain {
                    integrated_lufs: Some(lufs),
                    ..Default::default()
                }),
                ..Default::default()
            },
            object_id: format!("track-{index}"),
            album_object_id: Some(album.into()),
            audio_revision: Some(revision),
            ..Default::default()
        }],
        ..Default::default()
    };
    observation.apply_catalog(&batch, true).await.unwrap();
}

#[tokio::test]
async fn loudness_is_keyed_by_audio_hash_and_source_facts_win() {
    let fixture = fixture().await;
    let catalog = &fixture.catalog;
    let source = fixture.source;
    let tracks = &fixture.tracks;
    let uris = &fixture.track_uris;
    let cancel = ReadCancellation::new();
    let measured = |lufs: Option<f64>, peak: f64| LoudnessMeasurement {
        integrated_lufs: lufs,
        true_peak: Some(peak),
        replay_gain_db: None,
        replay_gain_peak: None,
    };
    let lufs = async |uri: &str| {
        let (track, album) = catalog.playback_loudness(uri, &cancel).await.unwrap();
        (
            track.and_then(|value| value.integrated_lufs),
            album.and_then(|value| value.integrated_lufs),
        )
    };

    // No hash is known yet, so every track is work.
    assert_eq!(
        catalog.loudness_work(source, None, 64).await.unwrap(),
        *tracks
    );
    // A hash is recorded only against the revision that was hashed.
    assert!(
        !catalog
            .put_audio_hash(tracks[0], [9; 32], [50; 32])
            .await
            .unwrap()
    );
    assert!(
        catalog
            .put_audio_hash(tracks[0], [1; 32], [50; 32])
            .await
            .unwrap()
    );
    assert!(
        !catalog
            .put_audio_hash(tracks[0], [1; 32], [50; 32])
            .await
            .unwrap()
    );

    // A measurement below the gate is stored; it is no tag to write.
    assert!(
        catalog
            .put_loudness([50; 32], &measured(None, 1.04))
            .await
            .unwrap()
    );
    let (track, _) = catalog.playback_loudness(&uris[0], &cancel).await.unwrap();
    assert_eq!(track.and_then(|value| value.true_peak), Some(1.04));
    assert_eq!(
        catalog.loudness_work(source, None, 64).await.unwrap(),
        tracks[1..]
    );
    assert!(
        catalog
            .r128_tag_write_page(source, None, 64, &cancel)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        catalog
            .put_loudness([50; 32], &measured(Some(-14.0), 0.8))
            .await
            .unwrap()
    );
    let page = catalog
        .r128_tag_write_page(source, None, 64, &cancel)
        .await
        .unwrap();
    assert_eq!(
        page.iter()
            .map(|row| (row.track_key, row.track_lufs))
            .collect::<Vec<_>>(),
        [(tracks[0], Some(-14.0))]
    );
    // Another track of the same audio shares the result.
    assert!(
        catalog
            .put_audio_hash(tracks[2], [3; 32], [50; 32])
            .await
            .unwrap()
    );
    assert_eq!(lufs(&uris[2]).await.0, Some(-14.0));

    // A source gain wins over analysis, and an unchanged revision keeps the hash.
    observe_track(catalog, 0, "album-a", [1; 32], Some(-11.0)).await;
    assert_eq!(lufs(&uris[0]).await.0, Some(-11.0));
    let page = catalog
        .r128_tag_write_page(source, None, 64, &cancel)
        .await
        .unwrap();
    assert!(page.iter().all(|row| row.track_key != tracks[0]));
    observe_track(catalog, 0, "album-a", [1; 32], None).await;
    assert_eq!(lufs(&uris[0]).await.0, Some(-14.0));
    // A new revision forgets the hash, so the audio is hashed again.
    observe_track(catalog, 0, "album-a", [99; 32], None).await;
    assert_eq!(lufs(&uris[0]).await.0, None);
    assert!(
        catalog
            .loudness_work(source, None, 64)
            .await
            .unwrap()
            .contains(&tracks[0])
    );

    // An album result needs every indexed member's hash.
    let work = catalog.album_loudness_work(source, None, 16).await.unwrap();
    let (album, members) = work[0].clone();
    assert_eq!(members, tracks[..2]);
    assert!(
        !catalog
            .put_album_loudness(album, &measured(Some(-13.0), 0.85))
            .await
            .unwrap()
    );
    assert!(
        catalog
            .put_audio_hash(tracks[0], [99; 32], [60; 32])
            .await
            .unwrap()
    );
    assert!(
        catalog
            .put_audio_hash(tracks[1], [2; 32], [61; 32])
            .await
            .unwrap()
    );
    assert!(
        catalog
            .put_album_loudness(album, &measured(Some(-13.0), 0.85))
            .await
            .unwrap()
    );
    assert_eq!(lufs(&uris[1]).await.1, Some(-13.0));
    let work = catalog.album_loudness_work(source, None, 16).await.unwrap();
    assert!(work.iter().all(|(key, _)| *key != album));

    // A membership change misses the stored album result.
    observe_track(catalog, 2, "album-a", [3; 32], None).await;
    assert_eq!(lufs(&uris[1]).await.1, None);
    assert_eq!(lufs(&uris[2]).await.0, Some(-14.0));
    let work = catalog.album_loudness_work(source, None, 16).await.unwrap();
    assert_eq!(work[0], (album, vec![tracks[0], tracks[1], tracks[2]]));
}

#[tokio::test]
async fn source_artwork_pages_distinct_bindings() {
    let mut fixture = fixture().await;
    let binding = |id: &str| {
        serde_json::to_vec(&[library::ArtworkCandidate::new(
            library::ArtworkImage::Native {
                source_id: library::SourceId::new("source"),
                item_id: id.into(),
                tag: None,
            },
            None,
        )])
        .unwrap()
    };
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE artists SET native_artwork=NULL ")
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO track_details(track_key,native_artwork) VALUES(?1,?2) ON CONFLICT(track_key) DO UPDATE SET native_artwork=excluded.native_artwork")
        .bind(fixture.tracks[0])
        .bind(binding("binding-a").as_slice())
        .execute(&mut raw)
        .await
        .expect("hydrate Track artwork");
    sqlx::query("UPDATE albums SET native_artwork=?2 WHERE album_key=?1")
        .bind(fixture.albums[0])
        .bind(binding("binding-a").as_slice())
        .execute(&mut raw)
        .await
        .expect("hydrate duplicate Album artwork");
    sqlx::query("UPDATE artists SET native_artwork=?2 WHERE artist_key=?1")
        .bind(fixture.artists[0])
        .bind(binding("binding-b").as_slice())
        .execute(&mut raw)
        .await
        .expect("hydrate Artist artwork");
    let cancel = ReadCancellation::new();
    let first = fixture
        .catalog
        .source_artwork_page(fixture.source, None, 1, &cancel)
        .await
        .expect("first artwork page");
    assert_eq!(first.len(), 1);
    let second = fixture
        .catalog
        .source_artwork_page(fixture.source, Some(&first[0]), 128, &cancel)
        .await
        .expect("resume artwork page");
    let mut expected_first = [binding("binding-a"), binding("binding-b")];
    expected_first.sort();
    assert_eq!(first, expected_first[..1]);
    assert_eq!(second, expected_first[1..]);
    sqlx::query("WITH RECURSIVE numbers(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM numbers WHERE n<259)\n        INSERT INTO tracks(source_key,object_id,media_uri,title,display_album,display_artist,sort_text,duration_millis,indexed) SELECT ?1,'artwork-'||n,'test:artwork-'||n,COALESCE('',''),COALESCE('',''),COALESCE('',''),COALESCE('',''),COALESCE(0,0),1 FROM numbers")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    fixture
        .catalog
        .observe("source")
        .await
        .unwrap()
        .set_source_artwork(
            &[],
            &[(
                "track",
                &(0..260)
                    .map(|n| {
                        (
                            format!("artwork-{n}"),
                            Some(binding(&format!("binding-{n:04}"))),
                        )
                    })
                    .collect::<Vec<_>>(),
            )],
        )
        .await
        .unwrap();
    sqlx::query("UPDATE tags SET native_artwork=NULL WHERE tag_key=?1")
        .bind(fixture.genre)
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("UPDATE folders SET native_artwork=?2 WHERE folder_key=?1")
        .bind(fixture.folder)
        .bind(binding("binding-folder").as_slice())
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO playlists(source_key,owner,object_id,name,artwork_binding) VALUES (?1,'source','artwork','Artwork',?2)")
        .bind(fixture.source).bind(binding("binding-playlist").as_slice())
        .execute(&mut raw).await.unwrap();
    let mut expected = Vec::new();
    expected.extend((0..260).map(|n| binding(&format!("binding-{n:04}"))));
    expected.extend([
        binding("binding-a"),
        binding("binding-b"),
        binding("binding-folder"),
        binding("binding-playlist"),
    ]);
    expected.sort();
    let mut actual = Vec::<Vec<u8>>::new();
    loop {
        let page = fixture
            .catalog
            .source_artwork_page(
                fixture.source,
                actual.last().map(Vec::as_slice),
                128,
                &cancel,
            )
            .await
            .unwrap();
        assert!(page.len() <= 128);
        if page.is_empty() {
            break;
        }
        actual.extend(page);
        assert!(actual.len() <= expected.len());
    }
    assert_eq!(actual, expected);
    sqlx::query("INSERT INTO downloads(media_uri,quality,revision,path,managed) SELECT media_uri,'original','',media_uri,1 FROM tracks WHERE object_id LIKE 'artwork-%' AND CAST(substr(object_id,9) AS INTEGER)%2=0")
        .execute(&mut raw).await.unwrap();
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let mut retention = fixture
        .catalog
        .artwork_cache_retention(&["test:artwork-1".into()])
        .await
        .unwrap();
    let mut retained = Vec::new();
    loop {
        let page = retention.next_bindings().await.unwrap();
        assert!(page.len() <= 128);
        if page.is_empty() {
            break;
        }
        retained.extend(page);
    }
    assert_eq!(retained.len(), 131);
    assert!(retained.contains(&binding("binding-0001")));
    assert!(!retained.contains(&binding("binding-a")));
    retention
        .retain(&["native/source/cover".into(), "external/cover".into()])
        .await
        .unwrap();
    assert_eq!(
        retention
            .unretained(&[
                "native/source/cover".into(),
                "external/other".into(),
                "external/cover".into()
            ])
            .await
            .unwrap(),
        [false, true, false]
    );
    retention.finish().await.unwrap();
}

#[tokio::test]
async fn local_mapping_selects_one_exact_or_representative_track_without_a_library_walk() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO track_details(track_key,source_path) VALUES(?1,'/server/music/track.flac') ON CONFLICT(track_key) DO UPDATE SET source_path=excluded.source_path")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .expect("install provider source path");
    let representative = fixture
        .catalog
        .mapping_track_page(fixture.source, None, None, 1, &cancel)
        .await
        .expect("representative mapping Track")
        .pop()
        .expect("mapping Track");
    assert_eq!(representative.source_path, "/server/music/track.flac");
}

#[tokio::test]
async fn local_mapping_counts_formula_coverage_without_probing_local_files() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    sqlx::query(
        "INSERT INTO track_details(track_key,source_path) SELECT track_key,CASE track_key WHEN ?1 THEN '/music/Artist/Album/Track.flac' ELSE 'Other/Album/Track.flac' END FROM tracks WHERE source_key=?2 ON CONFLICT(track_key) DO UPDATE SET source_path=excluded.source_path",
    )
    .bind(fixture.tracks[0])
    .bind(fixture.source)
    .execute(&mut raw)
    .await
    .expect("install provider source paths");

    assert_eq!(
        fixture
            .catalog
            .mapping_formula_match_count(fixture.source, "/local/music", Some("/music"), &cancel)
            .await
            .expect("count prefixed paths"),
        1
    );
    assert_eq!(
        fixture
            .catalog
            .mapping_formula_match_count(fixture.source, "/local/music", None, &cancel)
            .await
            .expect("count relative paths"),
        fixture.tracks.len().saturating_sub(1)
    );
}

#[tokio::test]
async fn lyrics_cache_identity_and_evicts_bounded_rows() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let capacity_payload = "x".repeat(8 * 1024 * 1024);
    fixture
        .catalog
        .write_lyrics_cache(
            &fixture.track_uris[0],
            "source",
            "lyrics",
            "en",
            "Latn",
            "First",
            10,
        )
        .await
        .expect("write lyrics");
    fixture
        .catalog
        .write_lyrics_cache(
            &fixture.track_uris[0],
            "external",
            "lyrics",
            "en",
            "Latn",
            "External",
            20,
        )
        .await
        .expect("write external lyrics");
    let row = fixture
        .catalog
        .lyrics_cache_for_role(
            &fixture.track_uris[0],
            "original",
            "",
            "",
            "source",
            &cancel,
        )
        .await
        .expect("read lyrics")
        .unwrap();
    assert_eq!(row.lyrics, "First");
    let external = fixture
        .catalog
        .lyrics_cache_for_role(
            &fixture.track_uris[0],
            "lyrics",
            "en",
            "Latn",
            "external",
            &cancel,
        )
        .await
        .expect("read preferred external lyrics")
        .unwrap();
    assert_eq!(external.lyrics, "External");
    assert!(
        fixture
            .catalog
            .lyrics_cache_for_role(
                &fixture.track_uris[0],
                "original",
                "",
                "",
                "external",
                &cancel,
            )
            .await
            .expect("external cache keeps its display key")
            .is_none()
    );
    fixture
        .catalog
        .write_lyrics_cache(
            &fixture.track_uris[1],
            "source",
            "lyrics",
            "",
            "",
            &capacity_payload,
            20,
        )
        .await
        .expect("write second lyrics");
    assert_eq!(
        fixture
            .catalog
            .lyrics_cache_for_role(&fixture.track_uris[1], "lyrics", "", "", "source", &cancel)
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
            .catalog
            .write_lyrics_cache(
                &fixture.track_uris[2],
                "source",
                "lyrics",
                "",
                "",
                &oversized,
                30
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .catalog
            .remove_lyrics_cache(&fixture.track_uris[1], "source", "lyrics", "", "")
            .await
            .expect("remove lyrics")
    );
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO downloads(media_uri,quality,revision,path,managed) VALUES(?1,'original','',?1,1)")
        .bind(&fixture.track_uris[0])
        .execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO local_locators(media_uri,origin,path,root,relative_path,access_uri) VALUES(?1,'mapping',?1,'','',?1)")
        .bind(&fixture.track_uris[2])
        .execute(&mut raw).await.unwrap();
    for index in [1, 2] {
        for authority in ["source", "external"] {
            fixture
                .catalog
                .write_lyrics_cache(
                    &fixture.track_uris[index],
                    authority,
                    "lyrics",
                    "en",
                    "Latn",
                    "Cached",
                    30,
                )
                .await
                .unwrap();
        }
    }
    fixture
        .catalog
        .clear_lyrics_cache(&[fixture.track_uris[1].clone()])
        .await
        .unwrap();
    for (index, retained) in [(0, true), (1, true), (2, false)] {
        for authority in ["source", "external"] {
            assert_eq!(
                fixture
                    .catalog
                    .lyrics_cache_for_role(
                        &fixture.track_uris[index],
                        "lyrics",
                        "en",
                        "Latn",
                        authority,
                        &cancel
                    )
                    .await
                    .unwrap()
                    .is_some(),
                retained
            );
        }
    }
    let imported = library::LyricsCacheRow {
        media_uri: fixture.track_uris[2].clone(),
        authority: "source".into(),
        role: "original".into(),
        language: String::new(),
        script: String::new(),
        lyrics: "Peer".into(),
        updated_at: 40,
    };
    assert_eq!(
        fixture
            .catalog
            .import_lyrics_cache(vec![imported], false)
            .await
            .unwrap()
            .len(),
        1
    );
    fixture
        .catalog
        .write_lyrics_cache(
            &fixture.track_uris[2],
            "source",
            "original",
            "",
            "",
            "Local edit",
            41,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .catalog
            .lyrics_cache_for_role(
                &fixture.track_uris[2],
                "original",
                "",
                "",
                "source",
                &cancel
            )
            .await
            .unwrap()
            .unwrap()
            .lyrics,
        "Local edit"
    );
}

#[tokio::test]
async fn source_removal_preserves_listens_and_pending_delivery_only() {
    let fixture = fixture().await;
    fixture
        .catalog
        .record_listen(
            &ListenWrite {
                external_id: Some("remove-play".to_string()),
                media_uri: fixture.track_uris[0].clone(),
                title: "Track".to_string(),
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                duration_millis: 180_000,
                disc_number: None,
                track_number: None,
                year: None,
                release_date: None,
                source_format: None,
                musicbrainz_recording_id: None,
                musicbrainz_release_track_id: None,
                started_at: 100,
                local_period: "1970-01".to_string(),
                listened_millis: 1000,
                skipped: false,
            },
            None,
            &[ListenDeliveryTarget {
                service: "lastfm".to_string(),
                account_id: "account".to_string(),
                next_attempt_at: Some(200),
            }],
        )
        .await
        .expect("record retained listen");
    fixture
        .catalog
        .write_lyrics_cache(
            &fixture.track_uris[0],
            "source",
            "lyrics",
            "",
            "",
            "Lyrics",
            1,
        )
        .await
        .expect("write source lyrics");

    let mut scan_observation = fixture.catalog.observe("source").await.unwrap();
    {
        let write = library::CatalogBatch {
            tracks: vec![library::TrackObservation {
                metadata: library::TrackMetadata {
                    title: "Alpha".into(),
                    sort_text: "alpha".into(),
                    source_loudness: Some(library::NativeGain {
                        integrated_lufs: Some(-12.0),
                        true_peak: Some(0.8),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                object_id: "track-0".into(),
                audio_revision: Some([1; 32]),
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    let queue_media = fixture
        .catalog
        .track_references_for_uris(&fixture.track_uris[..1], &ReadCancellation::new())
        .await
        .expect("materialize Queue media")
        .remove(0);
    persist_queue(
        &fixture.catalog,
        &[QueueTransferRow {
            received_from_connect: false,
            item: queue_media,
            position: 0,
            provenance: QueueProvenance::Manual,
        }],
        Some(0),
        0,
        QueueRepeatMode::Off,
        false,
    )
    .await;
    assert!(
        fixture
            .catalog
            .remove_source(&library::SourceId::new("source"))
            .await
            .expect("remove source")
    );
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_as::<_, (Option<String>, String)>(
            "SELECT source_id,media_uri FROM listens WHERE external_id='remove-play'"
        )
        .fetch_one(&mut raw)
        .await
        .expect("retained listen identity"),
        (Some("source".to_string()), fixture.track_uris[0].clone())
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listen_outbox")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1
    );
    assert_eq!(sqlx::query_as::<_,(i64,i64,i64,i64,i64)>("SELECT (SELECT count(*) FROM sources),(SELECT count(*) FROM tracks WHERE indexed),(SELECT count(*) FROM queue_entries),(SELECT count(*) FROM lyrics_cache),(SELECT count(*) FROM lyrics_fts)").fetch_one(&mut raw).await.unwrap(),(0,0,0,0,0));
}
