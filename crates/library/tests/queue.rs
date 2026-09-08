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
        source_index: None,
        playlist_entry_id: None,
        provenance: QueueProvenance::Manual,
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
        restored.current().map(OccurrenceId::as_str),
        Some("offline")
    );
    assert_eq!(restored.progress_millis, 1_500);
    assert_eq!(restored.repeat_mode, QueueRepeatMode::All);
    assert!(restored.shuffled);
    assert_eq!(restored.occurrences[0].title, "Offline");
    assert_eq!(restored.occurrences[0].artwork_binding.as_deref(), None);

    let page = fixture
        .database
        .prepared_queue_page(&restored.occurrences)
        .await
        .unwrap();
    assert_eq!(page[1].media_uri, page[2].media_uri);
    assert_ne!(page[1].occurrence, page[2].occurrence);
    assert_eq!(
        page[1].artwork_binding.as_deref(),
        Some(b"shared-art".as_slice())
    );
    assert_eq!(page[0].title, "Offline");
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
            restored.current().map(OccurrenceId::as_str),
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

async fn source_items(
    database: &library::Database,
    input: library::QueueInput,
    seed: Option<u64>,
) -> Vec<QueueItem> {
    let page = database
        .read_queue(library::QueueReadRequest::Capture {
            input: Box::new(input),
            anchor_index: 0,
            random_start: None,
        })
        .await
        .unwrap();
    let mut entries = page.entries;
    if seed.is_some() {
        entries.reverse();
    }
    let mut items = Vec::new();
    for chunk in entries.chunks(100) {
        let page = database
            .read_queue(library::QueueReadRequest::Hydrate {
                entries: chunk.to_vec(),
            })
            .await
            .unwrap();
        items.extend(page.occurrences.into_iter().map(|row| row.item.clone()));
    }
    items
}

#[tokio::test]
async fn source_membership_matches_track_view_sort_filter_and_null_order() {
    let fixture = fixture().await;
    for sort in [
        TrackSort::Title,
        TrackSort::TrackNumber,
        TrackSort::Artist,
        TrackSort::AlbumArtist,
        TrackSort::Album,
        TrackSort::Year,
        TrackSort::ReleaseDate,
        TrackSort::DateAdded,
        TrackSort::LastPlayed,
        TrackSort::PlayCount,
        TrackSort::UserRating,
        TrackSort::Genre,
        TrackSort::Bpm,
        TrackSort::Duration,
        TrackSort::Favorite,
    ] {
        for descending in [false, true] {
            for filter in ["", "a"] {
                let view = fixture
                    .database
                    .track_route_page(
                        fixture.source,
                        None,
                        false,
                        filter,
                        sort,
                        descending,
                        library::RouteSeedWindow::top(),
                        &ReadCancellation::new(),
                    )
                    .await
                    .unwrap();
                let input = library::QueueInput::Query {
                    query: library::QueueQuery::Tracks {
                        source: fixture.source,
                        favorites_only: false,
                        recursive: false,
                    },
                    folder: None,
                    filter: filter.into(),
                    sort,
                    descending,
                    context_id: "tracks".into(),
                    anchor_uri: None,
                };
                let items = source_items(&fixture.database, input, None).await;
                assert_eq!(
                    items.iter().map(|row| &row.media_uri).collect::<Vec<_>>(),
                    view.order.iter().collect::<Vec<_>>(),
                    "{sort:?}, descending={descending}, filter={filter}"
                );
            }
        }
    }
}

#[tokio::test]
async fn source_playlist_preserves_duplicate_snapshots_across_metadata_order() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO main.playlists(playlist_key,object_id,name,position) VALUES(100,'duplicates','Duplicates',0)").execute(&mut raw).await.unwrap();
    for position in 0..140_i64 {
        sqlx::query("INSERT INTO main.playlist_entries(playlist_key,object_id,media_uri,title,duration_millis,position) VALUES(100,?1,'https://example.test/repeated',?2,?3,?1)")
            .bind(position).bind(format!("Occurrence {position}")).bind(position*1000).execute(&mut raw).await.unwrap();
    }
    for seed in [None, Some(71)] {
        let input = library::QueueInput::Collection {
            collection: library::QueueCollection::Playlist(library::PlaylistKey::from_raw(100)),
            folder: None,
            context_id: "duplicates".into(),
        };
        let items = source_items(&fixture.database, input, seed).await;
        assert_eq!(items.len(), 140);
        let mut positions = items
            .iter()
            .map(|item| {
                let position = item
                    .title
                    .strip_prefix("Occurrence ")
                    .unwrap()
                    .parse::<i64>()
                    .unwrap();
                assert_eq!(item.duration_millis, position * 1000);
                position
            })
            .collect::<Vec<_>>();
        if seed.is_some() {
            positions.sort_unstable();
        }
        assert_eq!(positions, (0..140).collect::<Vec<_>>());
    }
}

#[tokio::test]
async fn saved_queue_contains_only_the_window_and_retains_compact_explicit_choices() {
    let fixture = fixture().await;
    let mut state = super::support::resolve_queue(
        &fixture.database,
        library::QueueInput::Uris {
            order: (0..500)
                .map(|i| format!("https://example.test/{i}"))
                .collect(),
            context_id: "explicit".into(),
            source_start: 0,
        },
        120,
    )
    .await;
    state.progress_millis = 42000;
    state.repeat_mode = QueueRepeatMode::All;
    fixture.database.save_queue(&state).await.unwrap();
    let restored = fixture.database.restore_queue().await.unwrap();
    assert_eq!(restored, state);
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM queue_occurrences")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        100
    );
    assert_eq!(restored.entries.len(), 500);
    let rows = fixture
        .database
        .prepared_queue_page(&restored.occurrences)
        .await
        .unwrap();
    assert_eq!(rows.len(), restored.occurrences.len());
    for (row, occurrence) in rows.iter().zip(&restored.occurrences) {
        assert_eq!(row.occurrence, occurrence.occurrence);
        assert_eq!(row.title, occurrence.title);
    }
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
async fn saved_membership_keeps_provenance_snapshots_and_source_identity_across_formats() {
    let fixture = fixture().await;
    let mut unavailable = QueueItem::direct(
        "https://example.test/İstanbul/%2Fduplicate?name=\"a\"",
        "Unavailable title",
        "Saved artist",
        "Saved album",
        12345,
    );
    unavailable.album_display_artist = Some("Saved album artist".into());
    unavailable.disc_number = Some(2);
    unavailable.track_number = Some(7);
    unavailable.year = Some(1998);
    unavailable.release_date = Some("1998-04-03".into());
    unavailable.source_format = Some("flac".into());
    unavailable.musicbrainz_recording_id = Some("recording".into());
    unavailable.musicbrainz_release_track_id = Some("release-track".into());
    unavailable.musicbrainz_album_id = Some("album".into());
    unavailable.musicbrainz_release_group_id = Some("release-group".into());
    unavailable.primary_artist_musicbrainz_id = Some("artist".into());
    let rows = [
        QueueProvenance::Context {
            context_id: "shared context".into(),
            source_rank: 37,
        },
        QueueProvenance::Context {
            context_id: "shared context".into(),
            source_rank: 2,
        },
        QueueProvenance::Context {
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
        let mut row = occurrence(format!("queue:shared:{index}"), unavailable.clone(), index);
        row.provenance = provenance;
        row.playlist_entry_id =
            (index % 2 == 0).then(|| format!("[\"playlist\",\"entry-{index}\"]"));
        row
    })
    .collect::<Vec<_>>();
    persist_queue(
        &fixture.database,
        fixture.source,
        &rows,
        Some("queue:shared:4"),
        42000,
        QueueRepeatMode::All,
        true,
    )
    .await;
    let mut state = fixture.database.restore_queue().await.unwrap();
    let mut entries = state.entries.to_vec();
    entries[6].media_uri = fixture.track_uris[0].clone().into();
    entries[7].media_uri = library::source_entity_uri(
        &library::SourceId::new("source"),
        "track",
        "missing-catalog-track",
    )
    .into();
    state.entries = entries.into();
    state.order = vec![2, 1, 0, 3, 4, 5, 7, 6].into();
    state.next_id = 987;
    let mut raw = connection(&fixture.path).await;
    for compact in [false, true] {
        fixture.database.save_queue(&state).await.unwrap();
        if !compact {
            sqlx::query("UPDATE queue_saved SET state=?1")
                .bind(serde_json::to_string(&state).unwrap())
                .execute(&mut raw)
                .await
                .unwrap();
            sqlx::raw_sql("DELETE FROM queue_order;DELETE FROM queue_state")
                .execute(&mut raw)
                .await
                .unwrap();
        }
        let restored = fixture.database.restore_queue().await.unwrap();
        assert_eq!(restored.entries, state.entries);
        assert_eq!(restored.order, state.order);
        assert_eq!(restored.current(), state.current());
        assert_eq!(restored.next_id, state.next_id);
        assert_eq!(restored.progress_millis, 42000);
        assert_eq!(restored.repeat_mode, QueueRepeatMode::All);
        assert!(restored.shuffled);
        for restored in &restored.occurrences {
            if restored.media_uri == unavailable.media_uri {
                assert_eq!(restored.item, unavailable);
            }
        }
        let mut source_members = fixture
            .database
            .queue_occurrences_for_source(fixture.source)
            .await
            .unwrap();
        source_members.sort();
        assert_eq!(source_members, ["queue:shared:6", "queue:shared:7"]);
        fixture
            .database
            .read_queue(library::QueueReadRequest::Capture {
                input: Box::new(library::QueueInput::Items(vec![(
                    unavailable.clone(),
                    QueueProvenance::Manual,
                )])),
                anchor_index: 0,
                random_start: None,
            })
            .await
            .unwrap();
        fixture
            .database
            .discard_queue_capture("queue:")
            .await
            .unwrap();
        let retained: Vec<String> =
            sqlx::query_scalar("SELECT object_id FROM queue_occurrences ORDER BY object_id")
                .fetch_all(&mut raw)
                .await
                .unwrap();
        assert_eq!(
            retained,
            rows.iter()
                .map(|row| row.occurrence.to_string())
                .collect::<Vec<_>>()
        );
        let mut exported = Vec::new();
        fixture
            .database
            .export_queue_jsonl(&mut exported)
            .await
            .unwrap();
        let header: serde_json::Value = serde_json::from_slice(&exported).unwrap();
        assert_eq!(header["version"], 3);
        let exported_state: library::QueueRestore =
            serde_json::from_value(header["queue"].clone()).unwrap();
        assert_eq!(exported_state.entries, state.entries);
        let target = super::support::fixture().await;
        target
            .database
            .import_queue_jsonl(std::io::Cursor::new(exported))
            .await
            .unwrap();
        assert_eq!(target.database.restore_queue().await.unwrap(), restored);
    }
    state.entries = state.entries[..6].to_vec().into();
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
    assert_eq!(removed.current(), state.current());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM queue_occurrences")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        6
    );
    let membership: String = sqlx::query_scalar("SELECT state FROM queue_saved")
        .fetch_one(&mut raw)
        .await
        .unwrap();
    fixture
        .database
        .persist_queue_settings(None, 0, QueueRepeatMode::One, false)
        .await
        .unwrap();
    let stopped = fixture.database.restore_queue().await.unwrap();
    assert_eq!(stopped.entries, state.entries);
    assert_eq!(stopped.current(), None);
    assert_eq!(stopped.progress_millis, 0);
    assert_eq!(stopped.repeat_mode, QueueRepeatMode::One);
    assert!(!stopped.shuffled);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM queue_saved")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        membership
    );
}

#[tokio::test]
async fn imported_snapshot_batches_keep_order_and_existing_fallback_facts() {
    let fixture = fixture().await;
    let mut rows = (0..205)
        .map(|index| {
            let mut row = occurrence(
                format!("snapshot:{index}"),
                QueueItem::direct(
                    format!("https://example.test/duplicate/{}", index % 3),
                    format!("Saved title {index}"),
                    "Artist",
                    "Album",
                    index as i64,
                ),
                500 + index,
            );
            row.source_index = Some(index % 2);
            row.playlist_entry_id = Some(format!("[null,\"playlist\",\"entry-{index}\"]"));
            row.provenance = QueueProvenance::Context {
                context_id: "original".into(),
                source_rank: 700 + index,
            };
            row
        })
        .collect::<Vec<_>>();
    persist_queue(
        &fixture.database,
        fixture.source,
        &rows,
        Some("snapshot:150"),
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
    let value: serde_json::Value = serde_json::from_slice(&exported).unwrap();
    assert_eq!(
        serde_json::from_value::<Vec<QueueOccurrence>>(value["snapshots"].clone()).unwrap(),
        rows
    );

    let original = rows.clone();
    rows.reverse();
    for row in &mut rows {
        row.item.title = "Changed supplied metadata".into();
        row.canonical_position = 9000;
        row.source_index = None;
        row.playlist_entry_id = None;
        row.provenance = QueueProvenance::Radio;
    }
    persist_queue(
        &fixture.database,
        fixture.source,
        &rows,
        Some("snapshot:150"),
        1200,
        QueueRepeatMode::One,
        false,
    )
    .await;
    let restored = fixture.database.restore_queue().await.unwrap();
    assert_eq!(
        restored
            .entries
            .iter()
            .map(|entry| &entry.occurrence)
            .collect::<Vec<_>>(),
        rows.iter().map(|row| &row.occurrence).collect::<Vec<_>>()
    );
    assert_eq!(restored.current().unwrap().as_str(), "snapshot:150");
    assert_eq!(restored.progress_millis, 1200);
    exported.clear();
    fixture
        .database
        .export_queue_jsonl(&mut exported)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&exported).unwrap();
    let snapshots: Vec<QueueOccurrence> =
        serde_json::from_value(value["snapshots"].clone()).unwrap();
    let expected = original
        .into_iter()
        .rev()
        .map(|mut row| {
            row.provenance = QueueProvenance::Radio;
            row
        })
        .collect::<Vec<_>>();
    assert_eq!(snapshots, expected);
}

#[tokio::test]
async fn installed_cursor_queue_retains_displaced_entries_and_play_last() {
    let fixture = fixture().await;
    let row = |id: &str, uri: &str, source| library::QueueOccurrence {
        occurrence: id.into(),
        item: QueueItem::direct(uri, uri, "", "", 1000),
        canonical_position: 0,
        source_index: Some(source),
        playlist_entry_id: None,
        provenance: library::QueueProvenance::Manual,
    };
    super::support::persist_queue(
        &fixture.database,
        fixture.source,
        &[row("old-a", "a", 0), row("old-x", "x", 1)],
        Some("old-a"),
        42000,
        QueueRepeatMode::All,
        false,
    )
    .await;
    let choice = |uri: &str, origin: Option<(usize, usize)>| serde_json::json!({"origin":origin,"media_uri":uri,"fallback":null,"provenance":"Manual"});
    let instruction = |choices: Vec<serde_json::Value>, repeat| serde_json::json!({"input":{"Choices":choices},"repeat":repeat,"seed":null});
    let saved = serde_json::json!({"occurrences":[],"current_index":0,"progress_millis":42000,"repeat_mode":"All","shuffled":false,"next_id":20,
        "sources":[instruction(vec![choice("a",None),choice("b",None),choice("c",None),choice("d",None)],true),instruction(vec![choice("x",None)],true),instruction(vec![choice("b",Some((0,1))),choice("c",Some((0,2)))],false),instruction(vec![choice("z",None)],true)],
        "pending":[{"source":2,"offset":0},{"source":0,"offset":3},{"source":3,"offset":0}]});
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE queue_saved SET state=?1")
        .bind(saved.to_string())
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM queue_order")
        .execute(&mut raw)
        .await
        .unwrap();
    let restored = fixture.database.restore_queue().await.unwrap();
    assert_eq!(
        restored
            .order
            .iter()
            .map(|index| restored.entries[*index as usize].media_uri.as_ref())
            .collect::<Vec<_>>(),
        ["a", "x", "b", "c", "d", "z"]
    );
    assert_eq!(restored.current().unwrap().as_str(), "old-a");
    assert_eq!(restored.progress_millis, 42000);
    assert_eq!(restored.repeat_mode, QueueRepeatMode::All);
}

#[tokio::test]
async fn coalesced_replacements_retire_snapshots_without_rewriting_them_on_order_changes() {
    let fixture = fixture().await;
    let mut latest = library::QueueRestore::default();
    for activation in 0..12 {
        let page = fixture
            .database
            .read_queue(library::QueueReadRequest::Capture {
                input: Box::new(library::QueueInput::Items(
                    (0..2)
                        .map(|index| {
                            (
                                QueueItem::direct(
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
                )),
                anchor_index: 0,
                random_start: None,
            })
            .await
            .unwrap();
        assert_eq!(page.occurrences.len(), 1);
        latest = library::QueueRestore {
            entries: page.entries.into(),
            order: vec![0, 1].into(),
            occurrences: page.occurrences,
            current_index: Some(0),
            ..Default::default()
        };
    }
    fixture.database.save_queue(&latest).await.unwrap();
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM queue_occurrences")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        2
    );
    let membership: String = sqlx::query_scalar("SELECT state FROM queue_saved")
        .fetch_one(&mut raw)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE TABLE queue_snapshot_writes(value INTEGER);CREATE TRIGGER queue_snapshot_updated AFTER UPDATE ON queue_occurrences BEGIN INSERT INTO queue_snapshot_writes VALUES(1); END;CREATE TRIGGER queue_snapshot_inserted AFTER INSERT ON queue_occurrences BEGIN INSERT INTO queue_snapshot_writes VALUES(1); END;").execute(&mut raw).await.unwrap();
    latest.order = vec![1, 0].into();
    latest.current_index = Some(1);
    latest.shuffled = true;
    latest.next_id = 37;
    latest.progress_millis = 1234;
    latest.repeat_mode = QueueRepeatMode::All;
    fixture.database.save_queue_order(&latest).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM queue_saved")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        membership
    );
    fixture.database.save_queue(&latest).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM queue_snapshot_writes")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        0
    );
    let restored = fixture.database.restore_queue().await.unwrap();
    assert_eq!(restored.entries, latest.entries);
    assert_eq!(restored.order, latest.order);
    assert_eq!(restored.current(), latest.current());
    assert_eq!(restored.next_id, latest.next_id);
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
        .map(|(index, uri)| library::QueueOccurrence {
            occurrence: format!("old:{index}").into(),
            item: QueueItem::direct(uri.clone(), uri.clone(), "", "", 1000),
            canonical_position: index,
            source_index: Some(0),
            playlist_entry_id: None,
            provenance: library::QueueProvenance::Context {
                context_id: "source".into(),
                source_rank: index,
            },
        })
        .collect::<Vec<_>>();
    super::support::persist_queue(
        &fixture.database,
        fixture.source,
        &rows,
        Some("old:1"),
        42,
        QueueRepeatMode::All,
        true,
    )
    .await;
    let reference = library::QueueSource {
        scope: library::QueueScope::Tracks {
            source: library::SourceId::new("source"),
            folder: None,
            favorites_only: false,
            recursive: false,
        },
        filter: String::new(),
        sort: TrackSort::Title,
        descending: false,
        anchor_uri: None,
    };
    let saved = serde_json::json!({"occurrences":[],"current_index":1,"progress_millis":42,"repeat_mode":"All","shuffled":true,"next_id":20,
        "sources":[{"input":library::QueueInput::Source {reference,context_id:"source".into()},"repeat":true,"seed":seed}],
        "pending":[{"source":0,"offset":2,"seed":seed}]});
    sqlx::query("UPDATE queue_saved SET state=?1")
        .bind(saved.to_string())
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM queue_order")
        .execute(&mut raw)
        .await
        .unwrap();
    let restored = fixture.database.restore_queue().await.unwrap();
    assert_eq!(
        restored
            .order
            .iter()
            .map(|index| restored.entries[*index as usize].media_uri.to_string())
            .collect::<Vec<_>>(),
        ordered
    );
    assert_eq!(restored.current().unwrap().as_str(), "old:1");
    assert_eq!(restored.progress_millis, 42);
}

#[tokio::test]
async fn repeated_large_source_starts_resolve_only_the_selected_window() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("WITH RECURSIVE n(i) AS(VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<20000)
        INSERT INTO tracks(source_key,object_id,media_uri,title,normalized_search,display_album,display_artist,sort_text,duration_millis)
        SELECT ?1,'bulk-'||i,'https://example.test/bulk/'||i,'Bulk '||i,'bulk','','',printf('%06d',i),1000 FROM n")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    for activation in 0..6_u64 {
        let page = fixture
            .database
            .read_queue(library::QueueReadRequest::Capture {
                input: Box::new(library::QueueInput::Query {
                    query: library::QueueQuery::Tracks {
                        source: fixture.source,
                        favorites_only: false,
                        recursive: false,
                    },
                    folder: None,
                    filter: "bulk".into(),
                    sort: TrackSort::Title,
                    descending: false,
                    context_id: "bulk".into(),
                    anchor_uri: None,
                }),
                anchor_index: 0,
                random_start: Some(activation * 7919),
            })
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 20000);
        assert_eq!(page.occurrences.len(), 1);
        assert_eq!(
            page.occurrences[0].occurrence,
            page.entries[page.current_index].occurrence
        );
        let mut order = (0..20000_u32).collect::<Vec<_>>();
        order.rotate_left(page.current_index);
        let projection = fixture
            .database
            .read_queue(library::QueueReadRequest::Hydrate {
                entries: order
                    .iter()
                    .take(100)
                    .map(|index| page.entries[*index as usize].clone())
                    .collect(),
            })
            .await
            .unwrap();
        assert_eq!(projection.occurrences.len(), 100);
        let mut state = library::QueueRestore {
            entries: page.entries.into(),
            order: order.into(),
            occurrences: projection.occurrences,
            current_index: Some(0),
            shuffled: true,
            ..Default::default()
        };
        fixture.database.save_queue(&state).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM queue_occurrences")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            100
        );
        let membership: String = sqlx::query_scalar("SELECT state FROM queue_saved")
            .fetch_one(&mut raw)
            .await
            .unwrap();
        let mut order = state.order.to_vec();
        order[1..].reverse();
        state.order = order.into();
        fixture.database.save_queue_order(&state).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT state FROM queue_saved")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            membership
        );
    }
}
