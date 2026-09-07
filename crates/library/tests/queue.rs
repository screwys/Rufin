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
        .prepared_queue_page(&restored.occurrences, "")
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
    let description = format!("{input:?}");
    let mut request = library::QueueReadRequest {
        input,
        cursor: library::QueueCursor {
            seed,
            ..Default::default()
        },
        limit: 3,
        history: false,
        backwards: false,
    };
    let mut items = Vec::new();
    for _ in 0..1000 {
        let page = database.read_queue(request).await.unwrap();
        assert!(page.items.len() <= 3);
        items.extend(page.items.into_iter().map(|row| row.0));
        if page.exhausted {
            return items;
        }
        request = library::QueueReadRequest {
            input: page.input,
            cursor: page.cursor,
            limit: 3,
            history: false,
            backwards: false,
        };
    }
    panic!("source traversal did not finish: {description}");
}

#[tokio::test]
async fn source_continuation_matches_track_view_sort_filter_and_null_order() {
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
async fn source_playlist_preserves_duplicate_snapshots_across_normal_and_shuffled_reads() {
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
        library::QueueCursor {
            anchor: Some(120),
            ..Default::default()
        },
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
    let library::QueueInput::Choices(choices) = &restored.sources[0].input else {
        panic!("compact explicit choices")
    };
    assert_eq!(choices.len(), 500);
    assert!(
        choices
            .iter()
            .flatten()
            .all(|choice| choice.fallback.is_none())
    );
    let rows = fixture
        .database
        .prepared_queue_page(&restored.occurrences, "499")
        .await
        .unwrap();
    assert!(
        rows.is_empty(),
        "queue search cannot read outside its window"
    );
    let rows = fixture
        .database
        .prepared_queue_page(&restored.occurrences, "120")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
}
