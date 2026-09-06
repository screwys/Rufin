use library::{
    OccurrenceId, QueueItem, QueueOccurrence, QueueProvenance, QueueRepeatMode, ReadCancellation,
    SmartPlaylistDefinition, TrackSort,
};

use super::support::{connection, fixture, persist_queue};

fn occurrence(
    object_id: impl Into<String>,
    item: QueueItem,
    canonical_position: usize,
) -> QueueOccurrence {
    QueueOccurrence {
        occurrence: OccurrenceId::new(object_id),
        item,
        canonical_position,
        provenance: QueueProvenance::Manual,
    }
}

#[tokio::test]
async fn prepared_queue_page_uses_only_the_new_bounded_snapshots_and_live_favorites() {
    use library::{QueueEdit, QueueInput, QueuePlacement};
    let directory = tempfile::tempdir().unwrap();
    let database = library::Database::open(directory.path().join("queue.sqlite3"))
        .await
        .unwrap();
    database
        .edit_queue_with_preview(
            QueueEdit::Apply {
                input: QueueInput::Items(vec![(
                    QueueItem::direct("old", "Old Queue", "", "", 0),
                    QueueProvenance::Manual,
                )]),
                placement: QueuePlacement::Replace { anchor_index: 0 },
                shuffle_seed: None,
                random_start: false,
                identity: None,
            },
            None,
            QueueRepeatMode::Off,
            false,
            0,
            |_| {},
        )
        .await
        .unwrap();
    let input = QueueInput::Items(
        (0..240)
            .map(|position| {
                (
                    QueueItem::direct(
                        format!("https://example.test/{position}"),
                        if position == 120 {
                            "Été [live].+".into()
                        } else {
                            position.to_string()
                        },
                        "",
                        "",
                        0,
                    ),
                    QueueProvenance::Manual,
                )
            })
            .collect(),
    );
    let prepared = database
        .prepare_queue_window(&input, 120, None, false, "new")
        .await
        .unwrap()
        .unwrap();
    let window = prepared
        .occurrences
        .into_iter()
        .map(std::sync::Arc::new)
        .collect::<Vec<_>>();
    database
        .set_favorite(
            &library::FavoriteTarget::Track("https://example.test/120".into()),
            true,
        )
        .await
        .unwrap();
    let cancellation = ReadCancellation::new();
    let rows = database.prepared_queue_page(&window, "").await.unwrap();
    assert_eq!(rows.len(), 96);
    assert!(
        rows.iter()
            .all(|row| row.occurrence.as_str().starts_with("new:"))
    );
    let filtered = database
        .prepared_queue_page(&window, "été [LIVE].+")
        .await
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].occurrence.as_str(), "new:120");
    assert!(filtered[0].favorite);
    assert!(
        database
            .prepared_queue_page(&window, "Old Queue")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        database
            .queue_page(None, "", 100, &cancellation)
            .await
            .unwrap()[0]
            .title,
        "Old Queue"
    );
}

#[tokio::test]
async fn queue_pages_prepare_ordered_artist_routes_without_changing_occurrences() {
    let fixture = fixture().await;
    let cancellation = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE album_artists SET position=4 WHERE album_key=?1")
        .bind(fixture.albums[0])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO album_artists(album_key,artist_key,position) VALUES(?1,?2,1)")
        .bind(fixture.albums[0])
        .bind(fixture.artists[1])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO track_artists(track_key,artist_key,position) VALUES(?1,?2,2),(?3,?4,9)",
    )
    .bind(fixture.tracks[0])
    .bind(fixture.artists[1])
    .bind(fixture.tracks[2])
    .bind(fixture.artists[0])
    .execute(&mut raw)
    .await
    .unwrap();
    sqlx::query("DELETE FROM track_artists WHERE track_key IN (?1,?2)")
        .bind(fixture.tracks[1])
        .bind(fixture.tracks[3])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM album_artists WHERE album_key=?1")
        .bind(fixture.albums[1])
        .execute(&mut raw)
        .await
        .unwrap();

    let uris = [
        fixture.track_uris[0].as_str(),
        fixture.track_uris[1].as_str(),
        fixture.track_uris[2].as_str(),
        fixture.track_uris[3].as_str(),
        "https://example.test/uncataloged.flac",
        fixture.track_uris[0].as_str(),
    ];
    let artist_uris = [
        Some(fixture.artist_uris[0].as_str()),
        Some(fixture.artist_uris[1].as_str()),
        Some(fixture.artist_uris[1].as_str()),
        None,
        None,
        Some(fixture.artist_uris[0].as_str()),
    ];
    let occurrences = uris
        .iter()
        .enumerate()
        .map(|(position, uri)| {
            occurrence(
                format!("saved-{position}"),
                QueueItem::direct(
                    *uri,
                    format!("Saved title {position}"),
                    "Saved artist",
                    "Saved album",
                    123_000,
                ),
                position,
            )
        })
        .collect::<Vec<_>>();
    let window = occurrences
        .iter()
        .rev()
        .cloned()
        .map(std::sync::Arc::new)
        .collect::<Vec<_>>();
    let prepared = fixture
        .database
        .prepared_queue_page(&window, "Saved")
        .await
        .unwrap();
    assert!(
        fixture
            .database
            .queue_page(None, "", 100, &cancellation)
            .await
            .unwrap()
            .is_empty()
    );
    persist_queue(
        &fixture.database,
        fixture.source,
        &occurrences,
        Some("saved-0"),
        0,
        QueueRepeatMode::Off,
        false,
    )
    .await;
    let persisted = fixture
        .database
        .queue_page(None, "Saved", 100, &cancellation)
        .await
        .unwrap();
    let backwards = fixture
        .database
        .queue_page_direction(None, "Saved", 100, true, &cancellation)
        .await
        .unwrap();
    for page in [prepared, persisted, backwards] {
        assert_eq!(page.len(), occurrences.len());
        for ((row, occurrence), artist_uri) in page.iter().zip(&occurrences).zip(artist_uris) {
            assert_eq!(row.occurrence, occurrence.occurrence);
            assert_eq!(row.position, occurrence.canonical_position as i64);
            assert_eq!(row.item, occurrence.item);
            assert_eq!(row.primary_artist_media_uri.as_deref(), artist_uri);
        }
        assert_eq!(page[0].media_uri, page[5].media_uri);
        assert_ne!(page[0].occurrence, page[5].occurrence);
    }
}

#[tokio::test]
async fn queue_search_matches_unicode_case_and_literal_punctuation_beyond_the_first_page() {
    let directory = tempfile::tempdir().unwrap();
    let database = library::Database::open(directory.path().join("queue.sqlite3"))
        .await
        .unwrap();
    let mut raw = connection(&directory.path().join("queue.sqlite3")).await;
    sqlx::query("WITH RECURSIVE numbers(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM numbers WHERE n<499)
        INSERT INTO queue_occurrences(object_id,media_uri,position,traversal_position,provenance_kind,title,artist,album,duration_millis)
        SELECT printf('unicode-%d',n),printf('https://example.test/%d',n),n,n,'manual',CASE WHEN n=400 THEN 'Été [live].+' ELSE 'Other' END,CASE WHEN n=450 THEN 'MÜNCHEN' ELSE '' END,'',0 FROM numbers")
        .execute(&mut raw).await.unwrap();
    let cancellation = ReadCancellation::new();
    for (filter, expected) in [("été [LIVE].+", 400), ("münchen", 450)] {
        let rows = database
            .queue_page(None, filter, 100, &cancellation)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].position, expected);
    }
    assert!(
        database
            .queue_page(None, ".*", 100, &cancellation)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn collection_preview_preserves_playlist_occurrences_and_explicit_shuffle() {
    use library::{PlaylistKey, QueueCollection, QueueEdit, QueueInput, QueuePlacement};
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO main.playlists(playlist_key,object_id,name,position) VALUES(100,'duplicates','Duplicates',0)")
        .execute(&mut raw).await.unwrap();
    for position in 0..140_i64 {
        sqlx::query("INSERT INTO main.playlist_entries(playlist_key,object_id,media_uri,title,duration_millis,position) VALUES(100,?1,?2,?3,?4,?5)")
            .bind(position.to_string())
            .bind(if position % 2 == 0 { "https://example.test/stream" } else { "file:///music/repeated.flac" })
            .bind(format!("Occurrence {position}"))
            .bind(position * 1_000)
            .bind(position).execute(&mut raw).await.unwrap();
    }
    for seed in [None, Some(71)] {
        let preview = std::sync::Mutex::new(None);
        let state = fixture
            .database
            .edit_queue_with_preview(
                QueueEdit::Apply {
                    input: QueueInput::Collection {
                        collection: QueueCollection::Playlist(PlaylistKey::from_raw(100)),
                        folder: None,
                        context_id: "duplicates".into(),
                    },
                    placement: QueuePlacement::Replace { anchor_index: 0 },
                    shuffle_seed: seed,
                    random_start: seed.is_some(),
                    identity: Some("duplicates".into()),
                },
                None,
                QueueRepeatMode::Off,
                false,
                0,
                |window| {
                    *preview.lock().unwrap() = Some(window);
                },
            )
            .await
            .unwrap();
        assert_eq!(preview.into_inner().unwrap().unwrap(), state);
        assert_eq!(state.total, 140);
        assert_eq!(state.shuffled, seed.is_some());
        assert!(state.occurrences.len() <= 96);
        for occurrence in &state.occurrences {
            assert_eq!(
                occurrence.title,
                format!("Occurrence {}", occurrence.canonical_position)
            );
            assert_eq!(
                occurrence.duration_millis,
                occurrence.canonical_position as i64 * 1_000
            );
        }
        if seed.is_none() {
            assert!(
                state
                    .occurrences
                    .iter()
                    .enumerate()
                    .all(|(position, row)| row.canonical_position == position)
            );
        }
    }
}

#[tokio::test]
async fn captured_windows_match_persisted_original_shuffle_at_every_boundary() {
    use library::{QueueEdit, QueueInput, QueuePlacement};
    let directory = tempfile::tempdir().unwrap();
    let database = library::Database::open(directory.path().join("queue.sqlite3"))
        .await
        .unwrap();
    for count in [1, 2, 3, 17, 96, 99, 257, 1_024] {
        let order: std::sync::Arc<[String]> = (0..count)
            .map(|position| format!("https://example.test/{position}"))
            .collect::<Vec<_>>()
            .into();
        for (anchor, seed) in [(0, 0), (count / 2, 71), (count - 1, u64::MAX)] {
            let input = QueueInput::Uris {
                order: order.clone(),
                context_id: "captured".into(),
                source_start: 20,
            };
            let preview = database
                .prepare_queue_window(&input, anchor, Some(seed), false, "captured")
                .await
                .unwrap()
                .unwrap();
            let persisted = database
                .edit_queue_with_preview(
                    QueueEdit::Apply {
                        input,
                        placement: QueuePlacement::Replace {
                            anchor_index: anchor,
                        },
                        shuffle_seed: Some(seed),
                        random_start: false,
                        identity: Some("captured".into()),
                    },
                    None,
                    QueueRepeatMode::Off,
                    false,
                    0,
                    |_| {},
                )
                .await
                .unwrap();
            assert_eq!(
                preview, persisted,
                "count={count}, anchor={anchor}, seed={seed}"
            );
        }
    }
}

#[tokio::test]
async fn grouped_collection_inputs_insert_in_selected_order_at_the_addressed_occurrence() {
    use library::{QueueEdit, QueueInput, QueuePlacement, QueueReorderTarget};
    let directory = tempfile::tempdir().unwrap();
    let database = library::Database::open(directory.path().join("queue.sqlite3"))
        .await
        .unwrap();
    let items = |names: &[&str]| {
        QueueInput::Items(
            names
                .iter()
                .map(|name| {
                    (
                        QueueItem::direct(
                            format!("https://example.test/{name}"),
                            *name,
                            "",
                            "",
                            1_000,
                        ),
                        QueueProvenance::Manual,
                    )
                })
                .collect(),
        )
    };
    let initial = database
        .edit_queue_with_preview(
            QueueEdit::Apply {
                input: items(&["a", "b", "c"]),
                placement: QueuePlacement::Now,
                shuffle_seed: None,
                random_start: false,
                identity: None,
            },
            None,
            QueueRepeatMode::Off,
            false,
            0,
            |_| {},
        )
        .await
        .unwrap();
    let target = initial.occurrences[1].occurrence.clone();
    let state = database
        .edit_queue_with_preview(
            QueueEdit::Insert {
                input: QueueInput::Groups(vec![items(&["d", "d"]), items(&["e", "f"])]),
                target: QueueReorderTarget::After(target),
            },
            initial.current_occurrence.as_ref(),
            QueueRepeatMode::Off,
            false,
            4321,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        state
            .occurrences
            .iter()
            .map(|entry| entry.title.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "d", "d", "e", "f", "c"]
    );
    assert_eq!(state.current_occurrence, initial.current_occurrence);
    assert_eq!(state.progress_millis, 4321);
}

#[tokio::test]
async fn queue_selection_moves_are_occurrence_blocks_at_exact_drop_positions() {
    use library::{QueueEdit, QueueReorderTarget};
    let f = fixture().await;
    let media = QueueItem::direct("https://example.test/repeated", "Repeated", "", "", 0);
    for (selected, target, expected) in [
        (
            vec!["b", "d"],
            QueueReorderTarget::Before("f".into()),
            vec!["a", "c", "e", "b", "d", "f"],
        ),
        (
            vec!["b", "d"],
            QueueReorderTarget::After("f".into()),
            vec!["a", "c", "e", "f", "b", "d"],
        ),
        (
            vec!["d", "f"],
            QueueReorderTarget::After("a".into()),
            vec!["a", "d", "f", "b", "c", "e"],
        ),
        (
            vec!["b", "d"],
            QueueReorderTarget::End,
            vec!["a", "c", "e", "f", "b", "d"],
        ),
        (
            vec!["b", "d"],
            QueueReorderTarget::Before("d".into()),
            vec!["a", "b", "c", "d", "e", "f"],
        ),
    ] {
        let entries = ["a", "b", "c", "d", "e", "f"]
            .into_iter()
            .enumerate()
            .map(|(rank, id)| occurrence(id, media.clone(), rank))
            .collect::<Vec<_>>();
        persist_queue(
            &f.database,
            f.source,
            &entries,
            Some("c"),
            4321,
            QueueRepeatMode::All,
            false,
        )
        .await;
        let state = f
            .database
            .edit_queue_with_preview(
                QueueEdit::Reorder {
                    occurrences: selected.into_iter().map(OccurrenceId::new).collect(),
                    target,
                },
                Some(&OccurrenceId::new("c")),
                QueueRepeatMode::All,
                false,
                4321,
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(state.total, 6);
        assert_eq!(state.current_occurrence, Some(OccurrenceId::new("c")));
        assert_eq!(state.progress_millis, 4321);
        let rows = f
            .database
            .queue_page(None, "", 100, &ReadCancellation::new())
            .await
            .unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row.occurrence.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            rows.iter().map(|row| row.position).collect::<Vec<_>>(),
            [0, 1, 2, 3, 4, 5]
        );
    }
}

#[tokio::test]
async fn queue_restores_uri_owned_duplicates_state_and_unavailable_media() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut media = fixture
        .database
        .queue_items_for_uris(&fixture.track_uris[..2], &cancel)
        .await
        .expect("materialize catalog media");
    media[0].artwork_binding = Some(b"shared-art".to_vec());
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET artwork_binding=?1 WHERE media_uri=?2")
        .bind(b"shared-art".as_slice())
        .bind(&media[0].media_uri)
        .execute(&mut raw)
        .await
        .expect("publish effective catalog artwork");
    let mut unavailable = media[1].clone();
    unavailable.media_uri = "https://example.invalid/offline.flac".to_string();
    unavailable.title = "Offline".to_string();
    unavailable.artwork_binding = Some(b"saved-art".to_vec());

    let occurrences = vec![
        occurrence("offline", unavailable, 2),
        occurrence("first", media[0].clone(), 0),
        occurrence("duplicate", media[0].clone(), 1),
    ];
    persist_queue(
        &fixture.database,
        fixture.source,
        &occurrences,
        Some("offline"),
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
            .occurrences
            .iter()
            .map(|row| row.occurrence.as_str())
            .collect::<Vec<_>>(),
        ["offline", "first", "duplicate"]
    );
    assert_eq!(
        restored
            .current_occurrence
            .as_ref()
            .map(OccurrenceId::as_str),
        Some("offline")
    );
    assert_eq!(restored.progress_millis, 1_500);
    assert_eq!(restored.repeat_mode, QueueRepeatMode::All);
    assert!(restored.shuffled);
    assert_eq!(restored.occurrences[0].title, "Offline");
    assert_eq!(restored.occurrences[0].artwork_binding.as_deref(), None);

    let page = fixture
        .database
        .queue_page(None, "", 256, &cancel)
        .await
        .expect("Queue page");
    assert_eq!(
        page.iter()
            .map(|row| row.occurrence.as_str())
            .collect::<Vec<_>>(),
        ["first", "duplicate", "offline"]
    );
    assert_eq!(page[0].media_uri, page[1].media_uri);
    assert_ne!(page[0].occurrence, page[1].occurrence);
    assert_eq!(
        page[0].artwork_binding.as_deref(),
        Some(b"shared-art".as_slice())
    );
    assert_eq!(page[2].title, "Offline");

    let mut raw = connection(&fixture.path).await;
    let canonical_plan = sqlx::query_as::<_, (i64, i64, i64, String)>(
        "EXPLAIN QUERY PLAN SELECT queue_occurrence_key FROM queue_occurrences WHERE position>?1 ORDER BY position LIMIT 256",
    )
    .bind(-1_i64)
    .fetch_all(&mut raw)
    .await
    .expect("Queue page plan")
    .into_iter()
    .map(|row| row.3)
    .collect::<Vec<_>>()
    .join(" | ");
    assert!(
        canonical_plan.contains("queue_occurrences_page_idx"),
        "{canonical_plan}"
    );
}

#[tokio::test]
async fn queue_persistence_reuses_occurrence_identity_and_pages_duplicates() {
    let fixture = fixture().await;
    let media = fixture
        .database
        .queue_items_for_uris(&fixture.track_uris, &ReadCancellation::new())
        .await
        .expect("materialize catalog media");
    let occurrences = (0..260)
        .rev()
        .map(|position| {
            occurrence(
                format!("paged-{position}"),
                media[position % media.len()].clone(),
                position,
            )
        })
        .collect::<Vec<_>>();

    for _ in 0..2 {
        persist_queue(
            &fixture.database,
            fixture.source,
            &occurrences,
            Some("paged-129"),
            23_000,
            QueueRepeatMode::All,
            true,
        )
        .await;
    }

    let restored = fixture
        .database
        .restore_queue()
        .await
        .expect("restore Queue");
    assert_eq!(restored.total, 260);
    assert!(restored.occurrences.len() <= 100);
    assert_eq!(restored.current_index, Some(130));
    assert_eq!(
        restored.occurrences[130 - restored.window_start]
            .occurrence
            .as_str(),
        "paged-129"
    );
    let last = fixture
        .database
        .queue_window_at(259)
        .await
        .expect("last window");
    assert_eq!(
        last.occurrences.last().unwrap().occurrence.as_str(),
        "paged-0"
    );
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM queue_occurrences WHERE object_id='paged-129'",
        )
        .fetch_one(&mut raw)
        .await
        .expect("count occurrence"),
        1
    );
}

#[tokio::test]
async fn occurrence_media_and_progress_follow_the_exact_uri() {
    let fixture = fixture().await;
    let mut media = fixture
        .database
        .queue_items_for_uris(&fixture.track_uris[..3], &ReadCancellation::new())
        .await
        .expect("materialize media");
    for (position, media) in media.iter_mut().enumerate() {
        media.media_uri = format!("file:///transition-{position}.flac");
    }
    let occurrences = media
        .into_iter()
        .enumerate()
        .map(|(position, media)| occurrence(format!("transition-{position}"), media, position))
        .collect::<Vec<_>>();
    persist_queue(
        &fixture.database,
        fixture.source,
        &occurrences,
        Some("transition-0"),
        0,
        QueueRepeatMode::Off,
        false,
    )
    .await;

    for (position, occurrence) in occurrences.iter().enumerate() {
        fixture
            .database
            .persist_queue_progress(Some(&occurrence.occurrence), (position as i64 + 1) * 1_000)
            .await
            .expect("persist progress");
        let restored = fixture
            .database
            .restore_queue()
            .await
            .expect("restore progress");
        let restored_occurrence = restored
            .occurrences
            .iter()
            .find(|candidate| candidate.occurrence == occurrence.occurrence)
            .expect("restore occurrence");
        assert_eq!(
            restored_occurrence.media_uri,
            format!("file:///transition-{position}.flac")
        );
        assert_eq!(
            restored_occurrence.title,
            ["Alpha", "Beta", "Gamma"][position]
        );
        assert_eq!(
            restored
                .current_occurrence
                .as_ref()
                .map(OccurrenceId::as_str),
            Some(occurrence.occurrence.as_str())
        );
        assert_eq!(restored.progress_millis, (position as i64 + 1) * 1_000);
    }
}

#[tokio::test]
async fn album_playlist_and_smart_playlist_materialize_the_same_uri_identity() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let album = fixture
        .database
        .album_track_route_page(
            fixture.source,
            fixture.albums[0],
            None,
            "",
            TrackSort::TrackNumber,
            false,
            library::RouteSeedWindow::top(),
            &cancel,
        )
        .await
        .expect("Album Track order")
        .order;
    let playlist_key = fixture
        .database
        .create_playlist(Some(fixture.source), "Collection", &fixture.track_uris[..3])
        .await
        .expect("create Playlist")
        .expect("Playlist key")
        .0;
    let playlist = fixture
        .database
        .playlist_media_uri_order(playlist_key, None, &cancel)
        .await
        .expect("Playlist URI order");
    let smart_key = fixture
        .database
        .create_smart_playlist("Everything", &SmartPlaylistDefinition::default())
        .await
        .expect("create Smart Playlist");
    let smart = fixture
        .database
        .smart_playlist_media_uri_order(Some(fixture.source), smart_key, None, 0, &cancel)
        .await
        .expect("Smart Playlist URI order");

    for (name, order) in [("album", album), ("playlist", playlist), ("smart", smart)] {
        assert!(!order.is_empty(), "{name} order");
        let media = fixture
            .database
            .queue_items_for_uris(&order, &cancel)
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

#[tokio::test]
async fn durable_queue_edits_preserve_duplicates_progress_and_canonical_order() {
    use library::{QueueEdit, QueueInput, QueuePlacement, QueueReorderTarget};
    let f = fixture().await;
    let input = QueueInput::Uris {
        order: vec![f.track_uris[0].clone(); 6].into(),
        context_id: "duplicate-test".into(),
        source_start: 0,
    };
    let mut state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::Apply {
                input,
                placement: QueuePlacement::Replace { anchor_index: 2 },
                shuffle_seed: None,
                random_start: false,
                identity: None,
            },
            None,
            QueueRepeatMode::All,
            false,
            0,
            |_| {},
        )
        .await
        .unwrap();
    let ids = state
        .occurrences
        .iter()
        .map(|r| r.occurrence.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        6
    );
    state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::Shuffle {
                enabled: true,
                seed: 42,
            },
            Some(&ids[2]),
            QueueRepeatMode::All,
            false,
            12345,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(state.current_occurrence, Some(ids[2].clone()));
    assert_eq!(state.progress_millis, 12345);
    let shuffled = state
        .occurrences
        .iter()
        .map(|r| r.occurrence.clone())
        .collect::<Vec<_>>();
    state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::Reorder {
                occurrences: vec![ids[5].clone()],
                target: QueueReorderTarget::Before(ids[0].clone()),
            },
            Some(&ids[2]),
            QueueRepeatMode::All,
            true,
            12345,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        state
            .occurrences
            .iter()
            .map(|r| r.occurrence.clone())
            .collect::<Vec<_>>(),
        shuffled
    );
    state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::Shuffle {
                enabled: false,
                seed: 0,
            },
            Some(&ids[2]),
            QueueRepeatMode::All,
            true,
            12345,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        state
            .occurrences
            .iter()
            .map(|r| r.occurrence.clone())
            .collect::<Vec<_>>(),
        [
            ids[5].clone(),
            ids[0].clone(),
            ids[1].clone(),
            ids[2].clone(),
            ids[3].clone(),
            ids[4].clone()
        ]
    );
    state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::Remove(vec![ids[2].clone(), ids[3].clone()]),
            Some(&ids[2]),
            QueueRepeatMode::All,
            false,
            12345,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(state.current_occurrence, Some(ids[4].clone()));
    assert_eq!(state.progress_millis, 0);
    state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::Remove(vec![ids[4].clone()]),
            Some(&ids[4]),
            QueueRepeatMode::All,
            false,
            300,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(state.current_occurrence, Some(ids[5].clone()));
    state.removed_successors.clear();
    let mut output = Vec::new();
    f.database.export_queue_jsonl(&mut output).await.unwrap();
    f.database
        .import_queue_jsonl(std::io::Cursor::new(output))
        .await
        .unwrap();
    assert_eq!(state, f.database.restore_queue().await.unwrap());
}

#[tokio::test]
#[allow(clippy::print_stderr)]
async fn million_queue_occurrences_have_bounded_windows_and_indexed_anchors() {
    use library::{QueueEdit, QueueInput, QueuePlacement};
    let f = fixture().await;
    let mut raw = connection(&f.path).await;
    sqlx::query("WITH RECURSIVE numbers(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM numbers WHERE n<999999)
        INSERT INTO queue_occurrences(object_id,media_uri,position,traversal_position,provenance_kind,provenance_context_id,provenance_source_rank,title,artist,album,duration_millis)
        SELECT printf('large-%d',n),?1,n,n,'context','large',n,printf('Track %06d',n),'Artist','Album',1000 FROM numbers")
        .bind(&f.track_uris[0]).execute(&mut raw).await.unwrap();
    let mut state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::SelectIndex(500_000),
            None,
            QueueRepeatMode::All,
            false,
            0,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(state.total, 1_000_000);
    assert_eq!(state.current_index, Some(500_000));
    assert!(state.occurrences.len() <= 100);
    let selected = state.current_occurrence.clone().unwrap();
    assert_eq!(
        f.database
            .queue_context_occurrence("large", Some("https://example.test/unrelated"), 500_000)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        f.database
            .queue_context_occurrence("large", None, 500_000)
            .await
            .unwrap(),
        Some(selected.clone())
    );
    for _ in 0..10 {
        let input = QueueInput::Uris {
            order: vec![f.track_uris[0].clone(); 100].into(),
            context_id: "add".into(),
            source_start: 0,
        };
        state = f
            .database
            .edit_queue_with_preview(
                QueueEdit::Apply {
                    input,
                    placement: QueuePlacement::End,
                    shuffle_seed: None,
                    random_start: false,
                    identity: None,
                },
                Some(&selected),
                QueueRepeatMode::All,
                false,
                321,
                |_| {},
            )
            .await
            .unwrap();
        assert!(state.occurrences.len() <= 100);
        assert_eq!(state.progress_millis, 321);
    }
    assert_eq!(state.total, 1_001_000);
    for index in [0, 97, 98, 499_949, 500_050, 1_000_999] {
        let page = f.database.queue_window_at(index).await.unwrap();
        assert!(
            page.occurrences.len()
                + usize::from(page.wrap_next.is_some())
                + usize::from(page.wrap_previous.is_some())
                <= 100
        );
        assert!(index >= page.window_start && index < page.window_start + page.occurrences.len());
    }
    let reopened = library::Database::open(&f.path).await.unwrap();
    assert_eq!(reopened.restore_queue().await.unwrap(), state);
    let mut raw = connection(&f.path).await;
    let plan=sqlx::query_as::<_,(i64,i64,i64,String)>("EXPLAIN QUERY PLAN SELECT object_id FROM queue_occurrences WHERE provenance_context_id='large' AND provenance_source_rank=50000 LIMIT 1").fetch_all(&mut raw).await.unwrap();
    assert!(
        plan.iter()
            .any(|row| row.3.contains("queue_occurrences_context_idx")),
        "{plan:?}"
    );
    let started = std::time::Instant::now();
    let matches = f
        .database
        .queue_page(None, "track 999", 100, &ReadCancellation::new())
        .await
        .unwrap();
    assert_eq!(matches.len(), 100);
    assert_eq!(matches[0].position, 999_000);
    assert_eq!(matches[99].position, 999_099);
    let next = f
        .database
        .queue_page(
            Some(matches[99].position),
            "track 999",
            100,
            &ReadCancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(next[0].position, 999_100);
    let previous = f
        .database
        .queue_page_direction(
            Some(next[0].position),
            "track 999",
            100,
            true,
            &ReadCancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(previous, matches);
    eprintln!(
        "million Queue filtered forward/backward reads: {:?}; maximum page {}",
        started.elapsed(),
        matches.len()
    );
}

#[tokio::test]
#[allow(clippy::print_stderr)]
async fn smart_play_and_shuffle_keep_queue_only_media_before_replacing_the_queue() {
    use library::{QueueEdit, QueueInput, QueuePlacement};
    let directory = tempfile::tempdir().unwrap();
    let database = library::Database::open(directory.path().join("queue.sqlite3"))
        .await
        .unwrap();
    let mut raw = connection(&directory.path().join("queue.sqlite3")).await;
    sqlx::query("WITH RECURSIVE numbers(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM numbers WHERE n<999999)
        INSERT INTO queue_occurrences(object_id,media_uri,position,traversal_position,provenance_kind,title,artist,album,duration_millis)
        SELECT printf('seed-%d',n),printf('https://example.test/%06d.flac',n),n,n,'manual','','','',0 FROM numbers")
        .execute(&mut raw).await.unwrap();
    let (playlist, _) = database
        .create_playlist(None, "Million", &[])
        .await
        .unwrap()
        .unwrap();
    sqlx::query("INSERT INTO main.playlist_entries(playlist_key,object_id,media_uri,position) SELECT ?1,object_id,media_uri,position FROM queue_occurrences")
        .bind(playlist).execute(&mut raw).await.unwrap();
    let requested = (500_000..500_096)
        .map(|position| format!("https://example.test/{position:06}.flac"))
        .collect::<Vec<_>>();
    let point_started = std::time::Instant::now();
    assert_eq!(
        database
            .queue_items_for_uris(&requested, &ReadCancellation::new())
            .await
            .unwrap()
            .len(),
        96
    );
    eprintln!(
        "million existing Queue/Playlist bounded known-media first window: {:?}; hydrated 96",
        point_started.elapsed()
    );
    let point_started = std::time::Instant::now();
    assert_eq!(
        database
            .smart_playlist_track_rows(&requested, &ReadCancellation::new())
            .await
            .unwrap()
            .len(),
        96
    );
    eprintln!(
        "million existing Queue/Playlist bounded Smart rows: {:?}; hydrated 96",
        point_started.elapsed()
    );
    let input = QueueInput::Collection {
        collection: library::QueueCollection::Playlist(playlist),
        folder: None,
        context_id: "original".into(),
    };
    let collection_started = std::time::Instant::now();
    let collection_ready = std::sync::Mutex::new(None);
    let collection = database
        .edit_queue_with_preview(
            QueueEdit::Apply {
                input,
                placement: QueuePlacement::Now,
                shuffle_seed: None,
                random_start: false,
                identity: Some("million-collection".into()),
            },
            None,
            QueueRepeatMode::All,
            false,
            0,
            |window| {
                collection_ready
                    .lock()
                    .unwrap()
                    .replace((collection_started.elapsed(), window));
            },
        )
        .await
        .unwrap();
    assert_eq!(collection.total, 1_000_000);
    assert!(collection.occurrences.len() <= 98);
    eprintln!(
        "million ordinary Playlist SQL Play: {:?}",
        collection_started.elapsed()
    );
    let (ready_after, ready) = collection_ready.into_inner().unwrap().unwrap();
    assert_eq!(ready.total, 1_000_000);
    assert_eq!(ready.current_occurrence, collection.current_occurrence);
    assert_eq!(
        ready
            .occurrences
            .iter()
            .map(|row| &row.occurrence)
            .collect::<Vec<_>>(),
        collection
            .occurrences
            .iter()
            .map(|row| &row.occurrence)
            .collect::<Vec<_>>()
    );
    eprintln!(
        "million ordinary Playlist first playback window: {ready_after:?}; hydrated {}",
        ready.occurrences.len()
            + usize::from(ready.wrap_previous.is_some())
            + usize::from(ready.wrap_next.is_some())
    );
    database.delete_playlist(None, playlist).await.unwrap();
    let smart = database
        .create_smart_playlist("Queue media", &SmartPlaylistDefinition::default())
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let smart_ready = std::sync::Mutex::new(None);
    let window = database
        .edit_queue_with_preview(
            QueueEdit::Apply {
                input: QueueInput::Smart {
                    key: smart,
                    source: None,
                    folder: None,
                    now: 0,
                    context_id: "smart".into(),
                },
                placement: QueuePlacement::Replace {
                    anchor_index: 500_000,
                },
                shuffle_seed: Some(71),
                random_start: false,
                identity: Some("million-smart".into()),
            },
            None,
            QueueRepeatMode::All,
            false,
            0,
            |window| {
                smart_ready
                    .lock()
                    .unwrap()
                    .replace((started.elapsed(), window));
            },
        )
        .await
        .unwrap();
    assert_eq!(window.total, 1_000_000);
    let (ready_after, ready) = smart_ready.into_inner().unwrap().unwrap();
    assert_eq!(ready.current_occurrence, window.current_occurrence);
    assert_eq!(
        ready
            .occurrences
            .iter()
            .map(|row| &row.occurrence)
            .collect::<Vec<_>>(),
        window
            .occurrences
            .iter()
            .map(|row| &row.occurrence)
            .collect::<Vec<_>>()
    );
    eprintln!(
        "million Smart first playback window: {ready_after:?}; hydrated {}",
        ready.occurrences.len()
            + usize::from(ready.wrap_previous.is_some())
            + usize::from(ready.wrap_next.is_some())
    );
    assert!(window.occurrences.len() <= 100);
    assert_eq!(
        window.occurrences[window.current_index.unwrap() - window.window_start].media_uri,
        "https://example.test/500000.flac"
    );
    let selected = window.current_occurrence.clone().unwrap();
    assert_eq!(
        database
            .queue_context_occurrence("smart", None, 500_000)
            .await
            .unwrap(),
        Some(selected.clone())
    );
    let shuffled_elapsed = started.elapsed();
    let traversal_started = std::time::Instant::now();
    for index in [0, 45, 94, 95, 96, 97, 100_000, 500_000, 999_998, 999_999] {
        let page = database
            .edit_queue_with_preview(
                QueueEdit::SelectIndex(index),
                Some(&selected),
                QueueRepeatMode::All,
                true,
                0,
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(page.current_index, Some(index));
        assert!(
            page.occurrences.len()
                + usize::from(page.wrap_next.is_some())
                + usize::from(page.wrap_previous.is_some())
                <= 98
        );
    }
    let selection_elapsed = traversal_started.elapsed();
    let traversal_started = std::time::Instant::now();
    let mut seen = 0;
    let mut cursor = 0;
    let mut windows = 0;
    let mut max_hydrated = 0;
    while cursor < 1_000_000 {
        let page = database
            .queue_window_at((cursor + 50).min(999_999))
            .await
            .unwrap();
        assert!(
            page.occurrences.len()
                + usize::from(page.wrap_next.is_some())
                + usize::from(page.wrap_previous.is_some())
                <= 98
        );
        windows += 1;
        max_hydrated = max_hydrated.max(
            page.occurrences.len()
                + usize::from(page.wrap_next.is_some())
                + usize::from(page.wrap_previous.is_some()),
        );
        for (local, row) in page.occurrences.iter().enumerate() {
            if page.window_start + local < cursor {
                continue;
            }
            let QueueProvenance::Context { source_rank, .. } = row.provenance else {
                panic!("Smart provenance")
            };
            seen += 1;
            assert_eq!(
                row.media_uri,
                format!("https://example.test/{source_rank:06}.flac")
            );
            assert_eq!(row.canonical_position, source_rank);
            if source_rank % 10_000 == 0 {
                assert_eq!(
                    database
                        .queue_context_occurrence("smart", None, source_rank)
                        .await
                        .unwrap(),
                    Some(row.occurrence.clone())
                );
            }
        }
        cursor = page.window_start + page.occurrences.len();
    }
    assert_eq!(seen, 1_000_000);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(DISTINCT provenance_source_rank) FROM queue_occurrences"
        )
        .fetch_one(&mut raw)
        .await
        .unwrap(),
        1_000_000
    );
    let expected = database.restore_queue().await.unwrap();
    let reopened = library::Database::open(directory.path().join("queue.sqlite3"))
        .await
        .unwrap();
    assert_eq!(reopened.restore_queue().await.unwrap(), expected);
    eprintln!(
        "million Smart SQL Play/Shuffle: {shuffled_elapsed:?}; ten indexed selections: {selection_elapsed:?}; complete million traversal: {:?}; {windows} window reads; maximum {max_hydrated} hydrated occurrences",
        traversal_started.elapsed()
    );
}

#[tokio::test]
async fn queue_only_snapshots_and_auto_dj_history_survive_owner_edits() {
    use library::{QueueEdit, QueueInput, QueuePlacement};
    let f = fixture().await;
    let mut items = (0..15)
        .map(|index| {
            (
                QueueItem::direct(
                    format!("https://example.test/{index}"),
                    format!("Snapshot {index}"),
                    "Artist",
                    "Album",
                    123,
                ),
                QueueProvenance::AutoDj,
            )
        })
        .collect::<Vec<_>>();
    items.insert(
        2,
        (
            QueueItem::direct("https://example.test/manual", "Manual", "", "", 0),
            QueueProvenance::Manual,
        ),
    );
    let state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::Apply {
                input: QueueInput::Items(items),
                placement: QueuePlacement::Replace { anchor_index: 15 },
                shuffle_seed: None,
                random_start: false,
                identity: None,
            },
            None,
            QueueRepeatMode::Off,
            false,
            0,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(state.total, 12); // ten prior AutoDJ, selected AutoDJ and the manual item
    let direct = state.occurrences.last().unwrap().media_uri.clone();
    let state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::Apply {
                input: QueueInput::Uris {
                    order: vec![direct].into(),
                    context_id: "again".into(),
                    source_start: 0,
                },
                placement: QueuePlacement::Now,
                shuffle_seed: None,
                random_start: false,
                identity: None,
            },
            None,
            QueueRepeatMode::Off,
            false,
            0,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(state.occurrences[0].title, "Snapshot 14");
    assert_eq!(state.occurrences[0].duration_millis, 123);
}

#[tokio::test]
async fn positional_insert_move_and_refill_preserve_the_playing_occurrence() {
    use library::{QueueEdit, QueueInput, QueuePlacement, QueueReorderTarget};
    let f = fixture().await;
    let state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::Apply {
                input: QueueInput::Uris {
                    order: vec![f.track_uris[0].clone(); 4].into(),
                    context_id: "positions".into(),
                    source_start: 0,
                },
                placement: QueuePlacement::Replace { anchor_index: 1 },
                shuffle_seed: None,
                random_start: false,
                identity: None,
            },
            None,
            QueueRepeatMode::All,
            false,
            0,
            |_| {},
        )
        .await
        .unwrap();
    let current = state.current_occurrence.unwrap();
    let last = state.occurrences.last().unwrap().occurrence.clone();
    let state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::Insert {
                input: QueueInput::Items(vec![(
                    QueueItem::direct("https://example.test/inserted", "Inserted", "", "", 0),
                    QueueProvenance::Manual,
                )]),
                target: QueueReorderTarget::Before(current.clone()),
            },
            Some(&current),
            QueueRepeatMode::All,
            false,
            12345,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(state.current_index, Some(2));
    assert_eq!(state.occurrences[1].title, "Inserted");
    assert_eq!(state.progress_millis, 12345);
    let state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::MoveAfterCurrent(last.clone()),
            Some(&current),
            QueueRepeatMode::All,
            false,
            12345,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        state.occurrences[state.current_index.unwrap() + 1].occurrence,
        last
    );
    let state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::SelectIndex(state.current_index.unwrap()),
            Some(&current),
            QueueRepeatMode::All,
            false,
            12345,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(state.current_occurrence, Some(current));
    assert_eq!(state.progress_millis, 12345);
    assert_eq!(f.database.restore_queue().await.unwrap(), state);
}

#[tokio::test]
async fn shuffled_insert_and_move_preserve_canonical_order_and_progress() {
    use library::{QueueEdit, QueueInput, QueuePlacement, QueueReorderTarget};
    let f = fixture().await;
    let input = QueueInput::MediaUris {
        order: (0..4)
            .map(|rank| format!("https://example.test/{rank}"))
            .collect::<Vec<_>>()
            .into(),
        provenance: QueueProvenance::Manual,
    };
    let state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::Apply {
                input,
                placement: QueuePlacement::Replace { anchor_index: 1 },
                shuffle_seed: None,
                random_start: false,
                identity: None,
            },
            None,
            QueueRepeatMode::All,
            false,
            0,
            |_| {},
        )
        .await
        .unwrap();
    let current = state.current_occurrence.unwrap();
    let last = state.occurrences[3].occurrence.clone();
    f.database
        .edit_queue_with_preview(
            QueueEdit::Shuffle {
                enabled: true,
                seed: 19,
            },
            Some(&current),
            QueueRepeatMode::All,
            false,
            42000,
            |_| {},
        )
        .await
        .unwrap();
    let state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::Insert {
                input: QueueInput::MediaUris {
                    order: vec!["https://example.test/extra".into()].into(),
                    provenance: QueueProvenance::Manual,
                },
                target: QueueReorderTarget::Before(last.clone()),
            },
            Some(&current),
            QueueRepeatMode::All,
            true,
            42000,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(state.current_occurrence.as_ref(), Some(&current));
    assert_eq!(state.progress_millis, 42000);
    let state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::MoveAfterCurrent(last.clone()),
            Some(&current),
            QueueRepeatMode::All,
            true,
            42000,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        state.occurrences[state.current_index.unwrap() + 1].occurrence,
        last
    );
    let state = f
        .database
        .edit_queue_with_preview(
            QueueEdit::Shuffle {
                enabled: false,
                seed: 0,
            },
            Some(&current),
            QueueRepeatMode::All,
            true,
            42000,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        state
            .occurrences
            .iter()
            .map(|row| row.media_uri.rsplit('/').next().unwrap())
            .collect::<Vec<_>>(),
        ["0", "1", "3", "2", "extra"]
    );
    assert_eq!(state.current_occurrence, Some(current));
    assert_eq!(state.progress_millis, 42000);
}
