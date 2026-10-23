use library::{
    CatalogSort, QueueProvenance, QueueRepeatMode, QueueTransferRow, ReadCancellation,
    SmartPlaylistDefinition, TrackReference,
};

use super::support::{connection, fixture, persist_queue};

fn queue_row(item: TrackReference, position: usize) -> QueueTransferRow {
    QueueTransferRow {
        received_from_connect: false,
        item,
        position,
        provenance: QueueProvenance::Manual,
    }
}

#[tokio::test]
async fn continuation_pages_preserve_duplicate_identity_and_do_not_replace_queue_until_commit() {
    let fixture = fixture().await;
    let database = &fixture.database;
    let captured = database
        .queue_entries(
            library::QueueLoad::items(
                (0..205)
                    .map(|index| {
                        (
                            TrackReference::direct(
                                format!("https://example.test/{}", index % 3),
                                format!("Occurrence {index}"),
                                "Artist",
                                "Album",
                                180_000,
                            ),
                            QueueProvenance::Manual,
                        )
                    })
                    .collect(),
            ),
            None,
        )
        .await
        .unwrap();
    let queue = library::QueueRestore {
        order: (0..205u32).rev().collect(),
        entries: captured.entries.into(),
        current_index: Some(12),
        progress_millis: 42_000,
        repeat_mode: QueueRepeatMode::All,
        shuffled: true,
        ..Default::default()
    };
    database.save_queue(&queue).await.unwrap();
    let receiver_directory = tempfile::tempdir().unwrap();
    let receiver = library::Database::open(receiver_directory.path().join("receiver.sqlite3"))
        .await
        .unwrap();
    for offset in (0..205).step_by(library::QUEUE_CONTEXT_LIMIT) {
        let page = database.queue_transfer_page(&queue, offset).await.unwrap();
        assert!(page.rows.len() <= library::QUEUE_CONTEXT_LIMIT);
        receiver
            .stage_queue_transfer_page("transfer", &page)
            .await
            .unwrap();
        // Repeated delivery replaces the page without repeating occurrences.
        receiver
            .stage_queue_transfer_page("transfer", &page)
            .await
            .unwrap();
        assert!(receiver.restore_queue().await.unwrap().entries.is_empty());
    }
    let mut prepared = receiver
        .prepare_queue_transfer("transfer", 205, queue.current_index.unwrap())
        .await
        .unwrap();
    assert_eq!(
        receiver
            .selected_track_uris(
                &library::MemberSelection::Keys {
                    source: None,
                    keys: ((&prepared
                        .entries
                        .iter()
                        .map(|entry| entry.track_key)
                        .collect::<Vec<_>>())
                        .iter()
                        .map(|key| key.raw())
                        .collect::<Vec<i64>>())
                    .into_iter()
                    .map(library::TrackKey::from_raw)
                    .collect::<Vec<_>>()
                    .into()
                },
                None,
                &ReadCancellation::new()
            )
            .await
            .unwrap(),
        database
            .selected_track_uris(
                &library::MemberSelection::Keys {
                    source: None,
                    keys: ((&queue
                        .entries
                        .iter()
                        .map(|entry| entry.track_key)
                        .collect::<Vec<_>>())
                        .iter()
                        .map(|key| key.raw())
                        .collect::<Vec<i64>>())
                    .into_iter()
                    .map(library::TrackKey::from_raw)
                    .collect::<Vec<_>>()
                    .into()
                },
                None,
                &ReadCancellation::new()
            )
            .await
            .unwrap(),
    );
    assert_eq!(
        prepared
            .entries
            .iter()
            .map(|entry| &entry.provenance)
            .collect::<Vec<_>>(),
        queue
            .entries
            .iter()
            .map(|entry| &entry.provenance)
            .collect::<Vec<_>>()
    );
    assert_eq!(prepared.order, queue.order);
    assert_eq!(prepared.current_index, queue.current_index);
    assert_eq!(prepared.rows.len(), 1);
    prepared.progress_millis = queue.progress_millis;
    prepared.repeat_mode = queue.repeat_mode;
    prepared.shuffled = queue.shuffled;
    receiver
        .commit_queue_transfer("transfer", &prepared)
        .await
        .unwrap();
    let restored = receiver.restore_queue().await.unwrap();
    assert_eq!(restored.entries, prepared.entries);
    assert_eq!(restored.order, queue.order);
    assert_eq!(restored.current_index, queue.current_index);
    assert_eq!(restored.progress_millis, 42_000);
    assert_eq!(restored.repeat_mode, QueueRepeatMode::All);
    assert!(restored.shuffled);
    assert!(restored.rows.len() <= library::QUEUE_CONTEXT_LIMIT);
    assert!(
        restored
            .entries
            .iter()
            .all(|entry| entry.received_from_connect)
    );
    assert!(
        queue
            .entries
            .iter()
            .all(|entry| !entry.received_from_connect)
    );
    receiver.save_queue(&restored).await.unwrap();
    assert!(
        receiver
            .restore_queue()
            .await
            .unwrap()
            .entries
            .iter()
            .all(|entry| entry.received_from_connect)
    );
    let final_page = receiver.queue_transfer_page(&restored, 200).await.unwrap();
    assert_eq!(final_page.rows[4].item.metadata.title, "Occurrence 204");
}

#[tokio::test]
async fn continuation_uses_local_artwork_before_starting_the_current_track() {
    let fixture = fixture().await;
    let mut connection = connection(&fixture.path).await;
    let [local_binding, transferred_binding] =
        ["local-cover", "transferred-cover"].map(|item_id| {
            serde_json::to_vec(&[library::ArtworkCandidate::new(
                library::ArtworkImage::Native {
                    source_id: library::SourceId::new("source"),
                    item_id: item_id.into(),
                    tag: None,
                },
                None,
            )])
            .unwrap()
        });
    {
        let objects =
            sqlx::query_scalar::<_, String>("SELECT object_id FROM tracks WHERE media_uri=?1")
                .bind(&fixture.track_uris[0])
                .fetch_all(&mut connection)
                .await
                .unwrap();
        fixture
            .database
            .catalog("source", None)
            .await
            .unwrap()
            .set_source_artwork(
                "track",
                &objects
                    .into_iter()
                    .map(|id| (id, Some((&local_binding).to_vec())))
                    .collect::<Vec<_>>(),
            )
            .await
            .unwrap();
    };
    let mut item = TrackReference::direct(
        &fixture.track_uris[0],
        "Transferred title",
        "Artist",
        "Album",
        180_000,
    );
    item.artwork_binding = Some(transferred_binding);
    let page = library::QueueTransferPage {
        offset: 0,
        rows: vec![queue_row(item, 0)],
        order: vec![0],
    };
    fixture
        .database
        .stage_queue_transfer_page("artwork", &page)
        .await
        .unwrap();
    let prepared = fixture
        .database
        .prepare_queue_transfer("artwork", 1, 0)
        .await
        .unwrap();
    assert_eq!(
        prepared.rows[0].track.artwork_binding.as_ref(),
        Some(&local_binding)
    );
    assert_eq!(prepared.rows[0].track.metadata.title, "Alpha");
}

#[tokio::test]
async fn interrupted_or_invalid_continuation_retains_saved_queue() {
    let fixture = fixture().await;
    let database = &fixture.database;
    let before = database.restore_queue().await.unwrap();
    let row = queue_row(
        TrackReference::direct(
            "https://example.test/track",
            "Track",
            "Artist",
            "Album",
            180_000,
        ),
        0,
    );
    let page = library::QueueTransferPage {
        offset: 0,
        rows: vec![row],
        order: vec![99],
    };
    database
        .stage_queue_transfer_page("incomplete", &page)
        .await
        .unwrap();
    assert!(
        database
            .prepare_queue_transfer("incomplete", 101, 0)
            .await
            .is_err()
    );
    assert!(
        database
            .prepare_queue_transfer("incomplete", 1, 0)
            .await
            .is_err()
    );
    assert_eq!(database.restore_queue().await.unwrap(), before);
    database.discard_queue_transfer("incomplete").await.unwrap();
}

#[tokio::test]
async fn queue_restores_uri_owned_duplicates_state_and_unavailable_media() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut media = fixture
        .database
        .track_references_for_uris(&fixture.track_uris[..2], &cancel)
        .await
        .expect("materialize catalog media");
    media[0].artwork_binding = Some(br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/72fc22518d84ba750d9fd4179a8fd2c4","image":{"kind":"native","source_id":"source","item_id":"shared-art","tag":null},"blur_hash":null}]"#.to_vec());
    let mut raw = connection(&fixture.path).await;
    {
        let objects =
            sqlx::query_scalar::<_, String>("SELECT object_id FROM tracks WHERE media_uri=?1")
                .bind(&media[0].media_uri)
                .fetch_all(&mut raw)
                .await
                .unwrap();
        fixture.database.catalog("source", None).await.unwrap().set_source_artwork("track", &objects.into_iter().map(|id|(id,Some((br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/72fc22518d84ba750d9fd4179a8fd2c4","image":{"kind":"native","source_id":"source","item_id":"shared-art","tag":null},"blur_hash":null}]"#.as_slice()).to_vec()))).collect::<Vec<_>>()).await.unwrap();
    };
    let mut unavailable = media[1].clone();
    unavailable.media_uri = "https://example.invalid/offline.flac".to_string();
    unavailable.metadata.title = "Offline".to_string();
    unavailable.artwork_binding = Some(br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/071c303459c767b7d3d7208c71f5bbf6","image":{"kind":"native","source_id":"source","item_id":"saved-art","tag":null},"blur_hash":null}]"#.to_vec());

    let occurrences = vec![
        queue_row(unavailable, 2),
        queue_row(media[0].clone(), 0),
        queue_row(media[0].clone(), 1),
    ];
    persist_queue(
        &fixture.database,
        &occurrences,
        Some(0),
        1_500,
        QueueRepeatMode::All,
        true,
    )
    .await;

    let restored = fixture
        .database
        .restore_queue()
        .await
        .expect("restore Queue");
    assert_eq!(
        restored
            .rows
            .iter()
            .map(|row| row.position)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(restored.current_index, Some(0));
    assert_eq!(restored.progress_millis, 1_500);
    assert_eq!(restored.repeat_mode, QueueRepeatMode::All);
    assert!(restored.shuffled);
    assert_eq!(restored.rows[0].track.metadata.title, "Offline");
    assert_eq!(
        restored.rows[0].track.artwork_binding,
        occurrences[0].item.artwork_binding
    );

    let page = &restored.rows;
    assert_eq!(page[1].track.media_uri, page[2].track.media_uri);
    assert_ne!(page[1].position, page[2].position);
    assert_eq!(
        page[1].track.artwork_binding.as_deref(),
        Some(br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/72fc22518d84ba750d9fd4179a8fd2c4","image":{"kind":"native","source_id":"source","item_id":"shared-art","tag":null},"blur_hash":null}]"#.as_slice())
    );
    assert_eq!(page[0].track.metadata.title, "Offline");
}

#[tokio::test]
async fn occurrence_media_and_progress_follow_the_exact_uri() {
    let fixture = fixture().await;
    let mut media = fixture
        .database
        .track_references_for_uris(&fixture.track_uris[..3], &ReadCancellation::new())
        .await
        .expect("materialize media");
    for (position, media) in media.iter_mut().enumerate() {
        media.media_uri = format!("file:///transition-{position}.flac");
    }
    let occurrences = media
        .into_iter()
        .enumerate()
        .map(|(position, media)| queue_row(media, position))
        .collect::<Vec<_>>();
    persist_queue(
        &fixture.database,
        &occurrences,
        Some(0),
        0,
        QueueRepeatMode::Off,
        false,
    )
    .await;

    for position in 0..occurrences.len() {
        fixture
            .database
            .apply_queue_update(&library::QueueUpdate {
                current_index: Some(position),
                progress_millis: (position as i64 + 1) * 1_000,
                ..Default::default()
            })
            .await
            .expect("persist progress");
        let restored = fixture
            .database
            .restore_queue()
            .await
            .expect("restore progress");
        let restored_occurrence = restored
            .rows
            .iter()
            .find(|candidate| candidate.position == position)
            .expect("restore occurrence");
        assert_eq!(
            restored_occurrence.track.media_uri,
            format!("file:///transition-{position}.flac")
        );
        assert_eq!(
            restored_occurrence.track.metadata.title,
            ["Alpha", "Beta", "Gamma"][position]
        );
        assert_eq!(restored.current_index, Some(position));
        assert_eq!(restored.progress_millis, (position as i64 + 1) * 1_000);
    }
}

#[tokio::test]
async fn album_playlist_and_smart_playlist_materialize_the_same_uri_identity() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let album = ({
        let rows = ((&library::MemberRead {
            collection: library::Collection::Album(fixture.albums[0]),
            sort: Some(CatalogSort::TrackNumber),
            ..library::MemberRead::new(library::Collection::Album(fixture.albums[0]))
        })
            .page(&fixture.database, library::RouteSeedWindow::top(), &cancel)
            .await
            .expect("Album Track order")
            .rows)
            .into_iter()
            .map(library::MemberRow::into_track)
            .collect::<Vec<_>>();
        rows
    })
    .into_iter()
    .map(|row| row.media_uri)
    .collect::<Vec<_>>();
    let playlist_key = fixture
        .database
        .create_playlist(
            Some(fixture.source),
            "Collection",
            &library::MemberSelection::Uris(((&fixture.track_uris[..3]).to_vec()).into()),
        )
        .await
        .expect("create Playlist")
        .expect("Playlist key")
        .0;
    let playlist = fixture
        .database
        .selected_track_uris(
            &library::MemberRead::new(library::Collection::Playlist(playlist_key)).selection(),
            None,
            &cancel,
        )
        .await
        .expect("Playlist URI order");
    let smart_key = fixture
        .database
        .create_smart_playlist("Everything", &SmartPlaylistDefinition::default())
        .await
        .expect("create Smart Playlist");
    let smart = fixture
        .database
        .selected_track_uris(
            &library::MemberRead::new(library::Collection::Smart {
                key: smart_key,
                now: 0,
            })
            .selection(),
            None,
            &cancel,
        )
        .await
        .expect("Smart Playlist URI order");

    for (name, order) in [("album", album), ("playlist", playlist), ("smart", smart)] {
        assert!(!order.is_empty(), "{name} order");
        let media = fixture
            .database
            .track_references_for_uris(&order, &cancel)
            .await
            .expect("materialize URI order");
        assert_eq!(
            media
                .iter()
                .map(|item| item.media_uri.as_str())
                .collect::<Vec<_>>(),
            order.iter().map(String::as_str).collect::<Vec<_>>()
        );
    }
}

async fn source_items(
    database: &library::Database,
    input: library::QueueLoad,
    seed: Option<u64>,
) -> Vec<TrackReference> {
    let page = database.queue_entries(input, None).await.unwrap();
    let mut entries = page.entries;
    if seed.is_some() {
        entries.reverse();
    }
    let mut items = Vec::new();
    for chunk in entries.chunks(100) {
        let page = database
            .queue_rows_for_entries(
                &chunk.iter().cloned().enumerate().collect::<Vec<_>>(),
                &ReadCancellation::new(),
            )
            .await
            .unwrap();
        items.extend(
            page.into_iter()
                .map(|row| library::TrackReference::from(row.track.clone())),
        );
    }
    items
}

#[tokio::test]
async fn source_membership_matches_track_view_sort_filter_and_null_order() {
    let fixture = fixture().await;
    for sort in [
        CatalogSort::Title,
        CatalogSort::TrackNumber,
        CatalogSort::Artist,
        CatalogSort::AlbumArtist,
        CatalogSort::Album,
        CatalogSort::Year,
        CatalogSort::ReleaseDate,
        CatalogSort::DateAdded,
        CatalogSort::LastPlayed,
        CatalogSort::PlayCount,
        CatalogSort::UserRating,
        CatalogSort::Genre,
        CatalogSort::Bpm,
        CatalogSort::Duration,
        CatalogSort::Favorite,
    ] {
        for descending in [false, true] {
            for filter in ["", "a"] {
                let view = (&library::MemberRead {
                    collection: library::Collection::Source(fixture.source),
                    sort: Some(sort),
                    filter: (filter).into(),
                    descending: descending,
                    ..library::MemberRead::new(library::Collection::Source(fixture.source))
                })
                    .page(
                        &fixture.database,
                        library::RouteSeedWindow::top(),
                        &ReadCancellation::new(),
                    )
                    .await
                    .unwrap();
                let input = library::QueueLoad::Tracks(vec![(
                    library::MemberRead {
                        collection: library::Collection::Source(fixture.source),
                        sort: Some(sort),
                        filter: filter.into(),
                        descending: descending,
                        ..library::MemberRead::new(library::Collection::Source(fixture.source))
                    }
                    .selection(),
                    library::QueueProvenance::Context {
                        context_id: "tracks".into(),
                        context_title: None,
                        source_rank: 0,
                    },
                )]);
                let items = source_items(&fixture.database, input, None).await;
                assert_eq!(
                    items.iter().map(|row| &row.media_uri).collect::<Vec<_>>(),
                    ({
                        let rows = view
                            .rows
                            .iter()
                            .map(library::MemberRow::track)
                            .collect::<Vec<_>>();
                        rows
                    })
                    .iter()
                    .map(|row| &row.media_uri)
                    .collect::<Vec<_>>(),
                    "{sort:?}, descending={descending}, filter={filter}"
                );
            }
        }
    }
}

#[tokio::test]
async fn source_playlist_preserves_duplicate_members_with_shared_uri_metadata() {
    let fixture = fixture().await;
    let file = fixture._directory.path().join("duplicates.m3u8");
    let content = format!(
        "#EXTM3U\n#PLAYLIST:Duplicates\n{}",
        "#EXTINF:3,Repeated\nhttps://example.test/repeated\n".repeat(140)
    );
    let imported = fixture
        .database
        .import_playlist_file(std::io::Cursor::new(content), &file, None, |_| None)
        .await
        .unwrap();
    let mut raw = connection(&fixture.path).await;
    for seed in [None, Some(71)] {
        let input = library::QueueLoad::Tracks(vec![(
            library::MemberRead::new(library::Collection::Playlist(imported.playlist)).selection(),
            library::QueueProvenance::Context {
                context_id: "duplicates".into(),
                context_title: None,
                source_rank: 0,
            },
        )]);
        let selected_index = seed.unwrap_or(0) as usize;
        let selected = sqlx::query_as::<_, (library::TrackKey, library::PlaylistEntryKey)>(
            "SELECT track_key,playlist_entry_key FROM playlist_entries WHERE playlist_key=?1 AND position=?2",
        )
        .bind(imported.playlist)
        .bind(selected_index as i64)
        .fetch_one(&mut raw)
        .await
        .unwrap();
        let page = fixture
            .database
            .queue_entries(input.clone(), Some((selected.0, Some(selected.1))))
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 140);
        assert_eq!(page.anchor_index, Some(selected_index));
        assert!(
            page.entries
                .iter()
                .all(|entry| entry.track_key == page.entries[0].track_key)
        );
        let items = source_items(&fixture.database, input, seed).await;
        assert_eq!(items.len(), 140);
        assert!(items.iter().all(|item| item == &items[0]));
        assert_eq!(items[0].metadata.title, "Repeated");
        assert_eq!(items[0].metadata.duration_millis, 3000);
        let queue = library::QueueRestore {
            order: (0..page.entries.len() as u32).rev().collect(),
            entries: page.entries.into(),
            ..Default::default()
        };
        let (positions, count) = fixture
            .database
            .queue_search_page(&queue, &items[0].metadata.title, 100, 40)
            .await
            .unwrap();
        assert_eq!(positions, (100..140).collect::<Vec<_>>());
        assert_eq!(count, 140);
    }
}

#[tokio::test]
async fn saved_queue_keeps_compact_choices_and_reads_only_the_window() {
    let fixture = fixture().await;
    let mut state = super::support::resolve_queue(
        &fixture.database,
        library::QueueLoad::media_uris(
            (0..500).map(|i| format!("https://example.test/{i}")),
            library::QueueProvenance::Context {
                context_id: "explicit".into(),
                context_title: None,
                source_rank: 0,
            },
        ),
        120,
    )
    .await;
    state.progress_millis = 42000;
    state.repeat_mode = QueueRepeatMode::All;
    state.ids = (1..=state.entries.len() as u64).collect();
    for row in &mut state.rows {
        std::sync::Arc::make_mut(row).occurrence_id = Some(state.ids[row.position]);
    }
    fixture.database.save_queue(&state).await.unwrap();
    let restored = fixture.database.restore_queue().await.unwrap();
    assert_eq!(restored, state);
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM queue_entries")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        500
    );
    assert_eq!(restored.entries.len(), 500);
    let mut exported = Vec::new();
    fixture
        .database
        .export_queue_jsonl(&mut exported)
        .await
        .unwrap();
    let target = super::support::fixture().await;
    target
        .database
        .import_queue_jsonl(std::io::Cursor::new(exported))
        .await
        .unwrap();
    assert_eq!(target.database.restore_queue().await.unwrap(), state);
}

#[tokio::test]
async fn saved_membership_keeps_provenance_metadata_and_source_identity_across_formats() {
    let fixture = fixture().await;
    let mut unavailable = TrackReference::direct(
        "https://example.test/İstanbul/%2Fduplicate?name=\"a\"",
        "Unavailable title",
        "Saved artist",
        "Saved album",
        12345,
    );
    unavailable.metadata.album_display_artist = Some("Saved album artist".into());
    unavailable.metadata.disc_number = 2;
    unavailable.metadata.track_number = 7;
    unavailable.metadata.year = Some(1998);
    unavailable.metadata.release_date = Some("1998-04-03".into());
    unavailable.metadata.source_format = Some("flac".into());
    unavailable.metadata.musicbrainz_recording_id = Some("recording".into());
    unavailable.metadata.musicbrainz_release_track_id = Some("release-track".into());
    unavailable.metadata.musicbrainz_album_id = Some("album".into());
    unavailable.metadata.musicbrainz_release_group_id = Some("release-group".into());
    unavailable.metadata.primary_artist_musicbrainz_id = Some("artist".into());
    let rows = [
        QueueProvenance::Context {
            context_title: None,
            context_id: "shared context".into(),
            source_rank: 37,
        },
        QueueProvenance::Context {
            context_title: None,
            context_id: "shared context".into(),
            source_rank: 2,
        },
        QueueProvenance::Context {
            context_title: Some(std::sync::Arc::new(library::QueueContextTitle {
                kind: Some("Playlist".into()),
                title: "Saved playlist".into(),
            })),
            context_id: "other \"context\"".into(),
            source_rank: 900,
        },
        QueueProvenance::Manual,
        QueueProvenance::Random,
        QueueProvenance::Radio,
        QueueProvenance::AutoDj,
        QueueProvenance::Legacy,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, provenance)| {
        let mut row = queue_row(unavailable.clone(), index);
        row.provenance = provenance;
        row
    })
    .collect::<Vec<_>>();
    persist_queue(
        &fixture.database,
        &rows,
        Some(4),
        42000,
        QueueRepeatMode::All,
        true,
    )
    .await;
    let mut state = fixture.database.restore_queue().await.unwrap();
    let mut entries = state.entries.to_vec();
    entries[6].track_key = fixture.tracks[0];
    let missing_uri = library::source_entity_uri(
        &library::SourceId::new("source"),
        "track",
        "missing-catalog-track",
    );
    let missing = fixture
        .database
        .queue_entries(
            library::QueueLoad::items(vec![(
                TrackReference::direct(&missing_uri, "Missing", "", "", 0),
                QueueProvenance::Legacy,
            )]),
            None,
        )
        .await
        .unwrap();
    entries[7].track_key = missing.entries[0].track_key;
    state.entries = entries.into();
    state.order = vec![2, 1, 0, 3, 4, 5, 7, 6].into();

    let mut raw = connection(&fixture.path).await;
    {
        fixture.database.save_queue(&state).await.unwrap();
        let restored = fixture.database.restore_queue().await.unwrap();
        assert_eq!(restored.entries, state.entries);
        assert_eq!(restored.order, state.order);
        assert_eq!(restored.current_index, state.current_index);
        assert_eq!(restored.progress_millis, 42000);
        assert_eq!(restored.repeat_mode, QueueRepeatMode::All);
        assert!(restored.shuffled);
        for restored in &restored.rows {
            assert_eq!(
                restored.source_rank(),
                [Some(900), Some(2), Some(37), None, None, None, None, None][restored.position]
            );
            if restored.track.media_uri == unavailable.media_uri {
                assert_eq!(
                    library::TrackReference::from(restored.track.clone()),
                    unavailable
                );
            }
        }
        let mut source_members = fixture
            .database
            .queue_positions_for_source(&state, fixture.source)
            .await
            .unwrap();
        source_members.sort();
        assert_eq!(source_members, [6, 7]);
        fixture
            .database
            .queue_entries(
                library::QueueLoad::items(vec![(unavailable.clone(), QueueProvenance::Manual)]),
                None,
            )
            .await
            .unwrap();
        let retained: Vec<String> =
            sqlx::query_scalar("SELECT media_uri FROM tracks WHERE media_uri=?1")
                .bind(&unavailable.media_uri)
                .fetch_all(&mut raw)
                .await
                .unwrap();
        assert_eq!(retained, [unavailable.media_uri.clone()]);
        let mut exported = Vec::new();
        fixture
            .database
            .export_queue_jsonl(&mut exported)
            .await
            .unwrap();
        let header: serde_json::Value =
            serde_json::from_slice(exported.split(|byte| *byte == b'\n').next().unwrap()).unwrap();
        assert_eq!(header["version"], 4);
        assert_eq!(
            header["queue"]["entries"].as_array().unwrap().len(),
            state.entries.len()
        );
        assert!(
            header["queue"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|entry| entry.get("track_key").is_none() && entry["media_uri"].is_string())
        );
        let target = super::support::fixture().await;
        target
            .database
            .import_queue_jsonl(std::io::Cursor::new(exported))
            .await
            .unwrap();
        assert_eq!(target.database.restore_queue().await.unwrap(), restored);
        let mut released = header["queue"].clone();
        for (index, entry) in released["entries"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            entry["occurrence"] = format!("queue:batch{}:{}", index % 2, index / 2).into();
            entry["playlist_entry_id"] = serde_json::Value::Null;
            entry
                .as_object_mut()
                .unwrap()
                .remove("received_from_connect");
        }
        released["next_id"] = 0.into();
        let snapshots = restored
            .rows
            .iter()
            .map(|row| {
                let canonical = state.order[row.position] as usize;
                serde_json::json!({
                    "occurrence":released["entries"][canonical]["occurrence"],
                    "item":TrackReference::from(row.track.clone()),
                    "canonical_position":canonical,
                    "playlist_entry_id":null,
                    "source_index":null,
                    "provenance":row.provenance,
                })
            })
            .collect::<Vec<_>>();
        target
            .database
            .import_queue_jsonl(std::io::Cursor::new(
                serde_json::to_vec(&serde_json::json!({
                    "version":3,"queue":released,"snapshots":snapshots,
                }))
                .unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(target.database.restore_queue().await.unwrap(), restored);
    }
    state.entries = state.entries[..6].to_vec().into();
    state.ids = state.ids[..6].to_vec().into();
    state.order = state
        .order
        .iter()
        .copied()
        .filter(|index| *index < 6)
        .collect();
    fixture.database.save_queue(&state).await.unwrap();
    let removed = fixture.database.restore_queue().await.unwrap();
    assert_eq!(removed.entries, state.entries);
    assert_eq!(removed.order, state.order);
    assert_eq!(removed.current_index, state.current_index);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks WHERE media_uri=?1")
            .bind(&unavailable.media_uri)
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1
    );
    let membership: String = sqlx::query_scalar("SELECT json_group_array(json_object('id',entry_id,'track',track_key,'provenance',provenance,'context',context_key,'rank',source_rank)) FROM queue_entries")
        .fetch_one(&mut raw)
        .await
        .unwrap();
    fixture
        .database
        .apply_queue_update(&library::QueueUpdate {
            repeat_mode: QueueRepeatMode::One,
            ..Default::default()
        })
        .await
        .unwrap();
    let stopped = fixture.database.restore_queue().await.unwrap();
    assert_eq!(stopped.entries, state.entries);
    assert_eq!(stopped.current_index, None);
    assert_eq!(stopped.progress_millis, 0);
    assert_eq!(stopped.repeat_mode, QueueRepeatMode::One);
    assert!(!stopped.shuffled);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT json_group_array(json_object('id',entry_id,'track',track_key,'provenance',provenance,'context',context_key,'rank',source_rank)) FROM queue_entries")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        membership
    );
}

#[tokio::test]
async fn imported_metadata_batches_keep_member_order_and_known_song_facts() {
    let fixture = fixture().await;
    let mut rows = (0..205)
        .map(|index| {
            let mut row = queue_row(
                TrackReference::direct(
                    format!("https://example.test/duplicate/{}", index % 3),
                    format!("Saved title {index}"),
                    "Artist",
                    "Album",
                    index as i64,
                ),
                index,
            );
            row.provenance = QueueProvenance::Context {
                context_title: None,
                context_id: "original".into(),
                source_rank: 700 + index,
            };
            row
        })
        .collect::<Vec<_>>();
    persist_queue(
        &fixture.database,
        &rows,
        Some(150),
        900,
        QueueRepeatMode::All,
        true,
    )
    .await;
    let mut exported = Vec::new();
    fixture
        .database
        .export_queue_jsonl(&mut exported)
        .await
        .unwrap();
    let records = String::from_utf8(exported).unwrap();
    let metadata = records
        .lines()
        .skip(1)
        .map(|line| serde_json::from_str::<TrackReference>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(metadata.len(), 3);
    assert_eq!(
        metadata
            .iter()
            .map(|item| item.metadata.title.as_str())
            .collect::<Vec<_>>(),
        ["Saved title 204", "Saved title 202", "Saved title 203"]
    );
    rows.reverse();
    for (position, row) in rows.iter_mut().enumerate() {
        row.position = position;
        row.item.metadata.title = "Changed supplied metadata".into();
        row.provenance = QueueProvenance::Radio;
    }
    persist_queue(
        &fixture.database,
        &rows,
        Some(54),
        1200,
        QueueRepeatMode::One,
        false,
    )
    .await;
    let restored = fixture.database.restore_queue().await.unwrap();
    assert_eq!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberSelection::Keys {
                    source: None,
                    keys: ((&restored
                        .entries
                        .iter()
                        .map(|entry| entry.track_key)
                        .collect::<Vec<_>>())
                        .iter()
                        .map(|key| key.raw())
                        .collect::<Vec<i64>>())
                    .into_iter()
                    .map(library::TrackKey::from_raw)
                    .collect::<Vec<_>>()
                    .into()
                },
                None,
                &ReadCancellation::new()
            )
            .await
            .unwrap(),
        rows.iter()
            .map(|row| row.item.media_uri.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        restored
            .entries
            .iter()
            .all(|entry| entry.provenance == QueueProvenance::Radio)
    );
    assert_eq!(restored.current_index, Some(54));
    assert_eq!(restored.progress_millis, 1200);
    assert!(
        restored
            .rows
            .iter()
            .all(|row| row.track.metadata.title == "Changed supplied metadata")
    );
}

#[tokio::test]
async fn installed_cursor_queue_retains_displaced_entries_and_play_last() {
    let fixture = fixture().await;
    let row = |id: &str, uri: &str, source| serde_json::json!({"occurrence":id,"item":TrackReference::direct(uri,uri,"","",1000),"canonical_position":0,"source_index":source,"playlist_entry_id":null,"provenance":"Manual"});
    let rows = vec![row("old-a", "a", 0), row("old-duplicate", "a", 1)];
    let choice = |uri: &str, origin: Option<(usize, usize)>| serde_json::json!({"origin":origin,"media_uri":uri,"fallback":null,"provenance":"Manual"});
    let instruction = |choices: Vec<serde_json::Value>, repeat| serde_json::json!({"input":{"Choices":choices},"repeat":repeat,"seed":null});
    let saved = serde_json::json!({"occurrences":rows,"current_index":1,"progress_millis":42000,"repeat_mode":"All","shuffled":false,"next_id":20,
        "sources":[instruction(vec![choice("a",None),choice("b",None),choice("c",None),choice("d",None)],true),instruction(vec![choice("a",None)],true),instruction(vec![choice("b",Some((0,1))),choice("c",Some((0,2)))],false),instruction(vec![choice("z",None)],true)],
        "pending":[{"source":2,"offset":0},{"source":0,"offset":3},{"source":3,"offset":0}]});
    fixture
        .database
        .import_queue_jsonl(std::io::Cursor::new(
            serde_json::to_vec(&serde_json::json!({"version":2,"queue":saved})).unwrap(),
        ))
        .await
        .unwrap();
    let restored = fixture.database.restore_queue().await.unwrap();
    assert_eq!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberSelection::Keys {
                    source: None,
                    keys: ((&restored
                        .order
                        .iter()
                        .map(|index| restored.entries[*index as usize].track_key)
                        .collect::<Vec<_>>())
                        .iter()
                        .map(|key| key.raw())
                        .collect::<Vec<i64>>())
                    .into_iter()
                    .map(library::TrackKey::from_raw)
                    .collect::<Vec<_>>()
                    .into()
                },
                None,
                &ReadCancellation::new()
            )
            .await
            .unwrap(),
        ["a", "a", "b", "c", "d", "z"]
    );
    assert_eq!(restored.current_index, Some(1));
    assert_eq!(restored.progress_millis, 42000);
    assert_eq!(restored.repeat_mode, QueueRepeatMode::All);
}

#[tokio::test]
async fn queue_replacement_retains_uri_metadata_without_writes_on_order_changes() {
    let fixture = fixture().await;
    let mut latest = library::QueueRestore::default();
    for activation in 0..12 {
        let page = fixture
            .database
            .queue_entries(
                library::QueueLoad::items(
                    (0..2)
                        .map(|index| {
                            (
                                TrackReference::direct(
                                    format!("https://example.test/{activation}/{index}"),
                                    format!("Title {index}"),
                                    "",
                                    "",
                                    1000,
                                ),
                                library::QueueProvenance::Manual,
                            )
                        })
                        .collect(),
                ),
                None,
            )
            .await
            .unwrap();
        latest = library::QueueRestore {
            entries: page.entries.into(),
            order: vec![0, 1].into(),
            current_index: Some(0),
            ..Default::default()
        };
    }
    fixture.database.save_queue(&latest).await.unwrap();
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM tracks WHERE media_uri LIKE 'https://example.test/%'"
        )
        .fetch_one(&mut raw)
        .await
        .unwrap(),
        24
    );
    let membership: String = sqlx::query_scalar("SELECT json_group_array(json_object('id',entry_id,'track',track_key,'provenance',provenance,'context',context_key,'rank',source_rank)) FROM queue_entries")
        .fetch_one(&mut raw)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE TABLE song_writes(value INTEGER);CREATE TRIGGER song_updated AFTER UPDATE ON tracks BEGIN INSERT INTO song_writes VALUES(1); END;CREATE TRIGGER song_inserted AFTER INSERT ON tracks BEGIN INSERT INTO song_writes VALUES(1); END;").execute(&mut raw).await.unwrap();
    latest.order = vec![1, 0].into();
    latest.current_index = Some(1);
    latest.shuffled = true;

    latest.progress_millis = 1234;
    latest.repeat_mode = QueueRepeatMode::All;
    fixture
        .database
        .apply_queue_update(&library::QueueUpdate {
            order: Some(library::QueueOrderUpdate {
                rows: latest
                    .order
                    .iter()
                    .enumerate()
                    .map(|(position, index)| (position, *index as u64 + 1))
                    .collect(),
                total: latest.order.len(),
            }),
            current_index: latest.current_index,
            progress_millis: latest.progress_millis,
            repeat_mode: latest.repeat_mode,
            shuffled: latest.shuffled,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT json_group_array(json_object('id',entry_id,'track',track_key,'provenance',provenance,'context',context_key,'rank',source_rank)) FROM queue_entries")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        membership
    );
    fixture.database.save_queue(&latest).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM song_writes")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        0
    );
    let restored = fixture.database.restore_queue().await.unwrap();
    assert_eq!(restored.entries, latest.entries);
    assert_eq!(restored.order, latest.order);
    assert_eq!(restored.current_index, latest.current_index);
    assert_eq!(restored.progress_millis, latest.progress_millis);
    assert_eq!(restored.repeat_mode, latest.repeat_mode);
    assert_eq!(restored.shuffled, latest.shuffled);
}

#[tokio::test]
async fn installed_shuffled_source_keeps_its_exact_pending_order() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    let seed = 1_200_000_000_u64;
    let ordered:Vec<String>=sqlx::query_scalar("SELECT media_uri FROM tracks ORDER BY (((track_key*1103515245)%2147483647)<?1),(track_key*1103515245)%2147483647,track_key").bind(seed as i64).fetch_all(&mut raw).await.unwrap();
    let rows = ordered
        .iter()
        .take(2)
        .enumerate()
        .map(|(index, uri)| serde_json::json!({"occurrence":format!("old:{index}"),"item":TrackReference::direct(uri.clone(),uri.clone(),"","",1000),"canonical_position":index,"source_index":0,"playlist_entry_id":null,"provenance":{"Context":{"context_id":"source","context_title":null,"source_rank":index}}}))
        .collect::<Vec<_>>();
    let reference = serde_json::json!({
        "scope": {"Tracks": {"source":"source","folder":null,"downloaded_only":false,"favorites_only":false,"recursive":false}},
        "filter":"","sort":"Title","descending":false,"anchor_uri":null
    });
    let saved = serde_json::json!({"occurrences":rows,"current_index":1,"progress_millis":42,"repeat_mode":"All","shuffled":true,"next_id":20,
        "sources":[{"input":{"Source":{"reference":reference,"context_id":"source"}},"repeat":true,"seed":seed}],
        "pending":[{"source":0,"offset":2,"seed":seed}]});
    fixture
        .database
        .import_queue_jsonl(std::io::Cursor::new(
            serde_json::to_vec(&serde_json::json!({"version":2,"queue":saved})).unwrap(),
        ))
        .await
        .unwrap();
    let restored = fixture.database.restore_queue().await.unwrap();
    assert_eq!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberSelection::Keys {
                    source: None,
                    keys: ((&restored
                        .order
                        .iter()
                        .map(|index| restored.entries[*index as usize].track_key)
                        .collect::<Vec<_>>())
                        .iter()
                        .map(|key| key.raw())
                        .collect::<Vec<i64>>())
                    .into_iter()
                    .map(library::TrackKey::from_raw)
                    .collect::<Vec<_>>()
                    .into()
                },
                None,
                &ReadCancellation::new()
            )
            .await
            .unwrap(),
        ordered
    );
    assert_eq!(restored.current_index, Some(1));
    assert_eq!(restored.progress_millis, 42);
}

#[tokio::test]
async fn large_source_membership_keeps_metadata_reads_bounded() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("WITH RECURSIVE n(i) AS(VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<20000)\n        INSERT INTO tracks(source_key,object_id,media_uri,metadata,normalized_search,indexed) SELECT ?1,'bulk-'||i,'https://example.test/bulk/'||i,jsonb(json_object('title',COALESCE('Bulk '||i,''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE(printf('%06d',i),''),'duration_millis',COALESCE(1000,0))),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE('Bulk '||i,''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE(printf('%06d',i),''),'duration_millis',COALESCE(1000,0))),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Bulk '||i,''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE(printf('%06d',i),''),'duration_millis',COALESCE(1000,0))),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Bulk '||i,''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE(printf('%06d',i),''),'duration_millis',COALESCE(1000,0))),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Bulk '||i,''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE(printf('%06d',i),''),'duration_millis',COALESCE(1000,0))),'$.comment'),'')),1 FROM n")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    for activation in 0..6_usize {
        let selected_index = (activation * 7919) % 20000;
        let selected = sqlx::query_scalar::<_, library::TrackKey>(
            "SELECT track_key FROM tracks WHERE source_key=?1 AND object_id=?2",
        )
        .bind(fixture.source)
        .bind(format!("bulk-{}", selected_index + 1))
        .fetch_one(&mut raw)
        .await
        .unwrap();
        let page = fixture
            .database
            .queue_entries(
                library::QueueLoad::Tracks(vec![(
                    library::MemberRead {
                        collection: library::Collection::Source(fixture.source),
                        sort: Some(CatalogSort::Title),
                        filter: "bulk".into(),
                        ..library::MemberRead::new(library::Collection::Source(fixture.source))
                    }
                    .selection(),
                    library::QueueProvenance::Context {
                        context_id: "bulk".into(),
                        context_title: None,
                        source_rank: 0,
                    },
                )]),
                Some((selected, None)),
            )
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 20000);
        assert_eq!(page.anchor_index, Some(selected_index));
        let mut order = (0..20000_u32).collect::<Vec<_>>();
        order.rotate_left(selected_index);
        let projection = fixture
            .database
            .queue_rows_for_entries(
                &order
                    .iter()
                    .take(100)
                    .enumerate()
                    .map(|(position, index)| (position, page.entries[*index as usize].clone()))
                    .collect::<Vec<_>>(),
                &ReadCancellation::new(),
            )
            .await
            .unwrap();
        assert_eq!(projection.len(), 100);
        let mut state = library::QueueRestore {
            entries: page.entries.into(),
            order: order.into(),
            rows: projection,
            current_index: Some(0),
            shuffled: true,
            ..Default::default()
        };
        fixture.database.save_queue(&state).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM queue_entries")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            20000
        );
        let membership: String = sqlx::query_scalar("SELECT json_group_array(json_object('id',entry_id,'track',track_key,'provenance',provenance,'context',context_key,'rank',source_rank)) FROM queue_entries")
            .fetch_one(&mut raw)
            .await
            .unwrap();
        let mut order = state.order.to_vec();
        order[1..].reverse();
        state.order = order.into();
        fixture
            .database
            .apply_queue_update(&library::QueueUpdate {
                order: Some(library::QueueOrderUpdate {
                    rows: state
                        .order
                        .iter()
                        .enumerate()
                        .map(|(position, index)| (position, *index as u64 + 1))
                        .collect(),
                    total: state.order.len(),
                }),
                current_index: state.current_index,
                progress_millis: state.progress_millis,
                repeat_mode: state.repeat_mode,
                shuffled: state.shuffled,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT json_group_array(json_object('id',entry_id,'track',track_key,'provenance',provenance,'context',context_key,'rank',source_rank)) FROM queue_entries")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            membership
        );
    }
}
