use library::{
    AlbumReleaseResult, FavoriteTarget, Item, LibraryField, ReadCancellation, SearchRequest,
};

use super::support::{Views, connection, fixture, items, tag, tracks, uris};

#[tokio::test]
async fn repeated_album_cards_keep_whole_counts_and_separate_source_activity() {
    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::raw_sql("UPDATE sources SET kind='subsonic';
      INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at)
      VALUES('source','lifetime','track','track-0',7,0,100),('source','lifetime','track','track-1',99,0,900);
      UPDATE tracks SET native_play_count=3,native_last_played=200 WHERE object_id='track-1';")
        .execute(&mut raw).await.unwrap();
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;

    let cancel = ReadCancellation::new();
    let keys = [fixture.albums[0], fixture.albums[0]];
    let rows = fixture.catalog.albums(&keys, true, &cancel).await.unwrap();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.row.summary.track_count, 2);
        assert_eq!(row.row.state.play_count, 3);
        assert_eq!(row.row.state.last_played, Some(200));
        assert_eq!(row.row.links.album_artists.len(), 1);
    }
    sqlx::query("UPDATE sources SET kind='local'")
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_links WHERE track_key=?1 AND role=4")
        .bind(fixture.tracks[1])
        .execute(&mut raw)
        .await
        .unwrap();
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let rows = fixture.catalog.albums(&keys, true, &cancel).await.unwrap();
    for row in &rows {
        assert_eq!(row.row.summary.track_count, 2);
        assert_eq!(row.row.state.play_count, 106);
        assert_eq!(row.row.state.last_played, Some(900));
    }
    let artist = fixture
        .catalog
        .artists(&[fixture.artists[0]], false, true, &cancel)
        .await
        .unwrap();
    assert_eq!(artist[0].row.state.play_count, 106);
    assert_eq!(artist[0].row.state.last_played, Some(900));
}

#[tokio::test]
async fn detail_folder_bounds_follow_collection_roles_and_selected_folder() {
    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    let paths = [
        "Artist/Album/Disc 1/a.flac",
        "Artist/Album/Disc 2/z.flac",
        "Other/Album/c.flac",
        "Other/Album/d.flac",
    ]
    .map(|path| {
        fixture
            ._directory
            .path()
            .join(path)
            .to_str()
            .unwrap()
            .to_owned()
    });
    for (track, path) in fixture.tracks.iter().zip(&paths) {
        sqlx::query("INSERT INTO track_details(track_key,source_path) VALUES(?2,?1) ON CONFLICT(track_key) DO UPDATE SET source_path=excluded.source_path")
            .bind(path)
            .bind(track)
            .execute(&mut raw)
            .await
            .unwrap();
    }
    // A guest appearance belongs to the artist view, not their album-artist view.
    sqlx::query("INSERT INTO track_links(track_key,role,position,target) VALUES(?1,0,1,?2)")
        .bind(fixture.tracks[2])
        .bind(fixture.artists[0])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_links WHERE role=4 AND track_key<>?1")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let catalog = &fixture.catalog;
    assert_eq!(
        catalog
            .collection_source_path_bounds(&fixture.track_uris[2], false, Some(fixture.folder))
            .await
            .unwrap(),
        Some((paths[2].clone(), paths[2].clone()))
    );
    assert_eq!(
        catalog
            .collection_source_path_bounds(&fixture.album_uris[0], false, None)
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[1].clone()))
    );
    assert_eq!(
        catalog
            .collection_source_path_bounds(&fixture.artist_uris[0], false, None)
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[2].clone()))
    );
    assert_eq!(
        catalog
            .collection_source_path_bounds(&fixture.artist_uris[0], true, None)
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[1].clone()))
    );
    assert_eq!(
        catalog
            .collection_source_path_bounds(&fixture.artist_uris[0], false, Some(fixture.folder))
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[0].clone()))
    );
    assert_eq!(
        catalog
            .collection_source_path_bounds(&fixture.album_uris[1], false, Some(fixture.folder))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn album_disc_sections_follow_filtered_order_and_page_boundaries() {
    let mut fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    for (index, disc) in [(0, 1), (1, 2), (2, 0)] {
        sqlx::query(
            "UPDATE tracks SET album_key=?1,disc_number=?2,track_number=1 WHERE track_key=?3",
        )
        .bind(fixture.albums[0])
        .bind(disc)
        .bind(fixture.tracks[index])
        .execute(&mut raw)
        .await
        .unwrap();
    }
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let sections = |read: library::MemberRead| {
        let catalog = &fixture.catalog;
        let cancel = &cancel;
        async move {
            let order = catalog
                .order(&library::Read::Members(read), cancel)
                .await
                .unwrap();
            catalog.disc_sections(&order)
        }
    };
    let album = library::MemberRead::new(library::Collection::Album(fixture.albums[0]));
    for (descending, expected) in [
        (false, vec![(0, 0), (1, 1), (2, 2)]),
        (true, vec![(0, 2), (1, 1), (2, 0)]),
    ] {
        let read = library::MemberRead {
            sort: Some(LibraryField::TrackNumber),
            descending,
            ..album.clone()
        };
        assert_eq!(sections(read.clone()).await, expected);
        assert_eq!(tracks(&fixture.catalog, read).await.len(), 3);
    }
    for (filter, sort) in [
        ("Alpha", LibraryField::TrackNumber),
        ("", LibraryField::Title),
    ] {
        let read = library::MemberRead {
            sort: Some(sort),
            filter: filter.into(),
            ..album.clone()
        };
        assert!(sections(read).await.is_empty());
    }
    // One known disc, even if numbered 2 or accompanied by unknown tracks, needs no heading.
    sqlx::query("UPDATE tracks SET disc_number=2 WHERE album_key=?1 AND disc_number>0")
        .bind(fixture.albums[0])
        .execute(&mut raw)
        .await
        .unwrap();
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let order = fixture
        .catalog
        .order(&library::Read::Members(album), &cancel)
        .await
        .unwrap();
    assert!(fixture.catalog.disc_sections(&order).is_empty());
}

#[tokio::test]
async fn track_sorts_keep_nulls_last_and_title_ties_in_both_directions() {
    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET year=CASE object_id WHEN 'track-0' THEN NULL WHEN 'track-1' THEN 0 ELSE 2000 END")
        .execute(&mut raw).await.unwrap();
    sqlx::query("DELETE FROM track_links WHERE track_key=?1 AND role=2")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .unwrap();
    let empty_genre = sqlx::query_scalar::<_, i64>(
        "INSERT INTO tags(source_key,kind,object_id,media_uri,name,sort_text) VALUES (?1,0,'empty','genre:empty','','') RETURNING tag_key",
    ).bind(fixture.source).fetch_one(&mut raw).await.unwrap();
    sqlx::query("UPDATE track_links SET target=?1 WHERE track_key=?2 AND role=2")
        .bind(empty_genre)
        .bind(fixture.tracks[1])
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    for sort in [LibraryField::Year, LibraryField::Genre] {
        for (descending, expected) in [(false, [1, 3, 2, 0]), (true, [3, 2, 1, 0])] {
            assert_eq!(
                uris(
                    &fixture.catalog,
                    library::MemberRead {
                        sort: Some(sort),
                        descending,
                        ..library::MemberRead::new(library::Collection::Source(fixture.source))
                    }
                )
                .await,
                expected.map(|i| fixture.track_uris[i].clone()),
                "{sort:?}, descending={descending}"
            );
        }
    }
}

#[tokio::test]
async fn named_card_batches_preserve_requested_rows_and_whole_cover_order() {
    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    let covers = ["first-album", "second-album"].map(|item_id| {
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
    for (index, album) in fixture.albums.iter().enumerate() {
        let objects =
            sqlx::query_scalar::<_, String>("SELECT object_id FROM albums WHERE album_key=?1")
                .bind(album)
                .fetch_all(&mut raw)
                .await
                .unwrap();
        fixture
            .catalog
            .observe("source")
            .await
            .unwrap()
            .set_source_artwork(
                &[],
                &[(
                    "album",
                    &objects
                        .into_iter()
                        .map(|id| (id, Some(covers[index].to_vec())))
                        .collect::<Vec<_>>(),
                )],
            )
            .await
            .unwrap();
    }
    sqlx::query("UPDATE tracks SET date_added=CASE WHEN album_key=?1 THEN '2026-09-08' END")
        .bind(fixture.albums[1])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_links WHERE role=4 AND track_key IN (SELECT track_key FROM tracks WHERE album_key=?1)")
        .bind(fixture.albums[1]).execute(&mut raw).await.unwrap();
    drop(raw);
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    fixture
        .catalog
        .pick_mosaics(&[
            library::MosaicOwner::Tag(fixture.genre.raw()),
            library::MosaicOwner::Tag(fixture.mood.raw()),
        ])
        .await
        .unwrap();
    for item in [Item::Genre(fixture.genre), Item::Mood(fixture.mood)] {
        let row = tag(&fixture.catalog, item).await.unwrap();
        assert_eq!(row.summary.track_count, 4);
        assert_eq!(row.representative_artwork, covers);
    }
}

#[tokio::test]
async fn named_collection_lookups_retain_identity_when_selected_folder_is_empty() {
    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("DELETE FROM track_links WHERE role=4")
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let genre = tag(&fixture.catalog, Item::Genre(fixture.genre))
        .await
        .expect("existing genre");
    assert_eq!(
        (
            genre.summary.track_count,
            genre.summary.album_count,
            genre.summary.duration_millis,
            genre.summary.downloaded_count
        ),
        (4, 2, 726000, 0)
    );
    assert!(genre.representative_artwork.is_empty());
    let mood = tag(&fixture.catalog, Item::Mood(fixture.mood))
        .await
        .expect("existing mood");
    assert_eq!(
        (
            mood.summary.track_count,
            mood.summary.duration_millis,
            mood.summary.downloaded_count
        ),
        (4, 726000, 0)
    );
    assert!(mood.representative_artwork.is_empty());
    for kind in [library::ListKind::Genres, library::ListKind::Moods] {
        for sort in [LibraryField::Title, LibraryField::SongCount] {
            let order = items(
                &fixture.catalog,
                library::ListRead {
                    folder: Some(fixture.folder),
                    activity: true,
                    ..library::ListRead::new(kind.clone(), Some(fixture.source), sort)
                },
            )
            .await;
            assert!(order.is_empty());
        }
    }
}

#[tokio::test]
async fn collection_play_retains_full_order_with_bounded_queue_projection() {
    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    for position in 0..130 {
        let id = format!("extra-{position:03}");
        let uri = library::source_entity_uri(&library::SourceId::new("source"), "track", &id);
        let key: library::TrackKey = sqlx::query_scalar(
            "INSERT INTO tracks(source_key,object_id,album_key,media_uri,title,display_artist,display_album,sort_text,duration_millis,disc_number,track_number,indexed) VALUES (?1,?2,?3,?5,?2,'Artist A','Album A',?2,1000,1,?4,1) RETURNING track_key",
        ).bind(fixture.source).bind(&id).bind(fixture.albums[0]).bind(position + 10).bind(uri)
            .fetch_one(&mut raw).await.expect("extend collection");
        for (role, target) in [
            (0, fixture.artists[0].raw()),
            (2, fixture.genre.raw()),
            (3, fixture.mood.raw()),
        ] {
            sqlx::query("INSERT INTO track_links VALUES(?1,?2,0,?3)")
                .bind(key)
                .bind(role)
                .bind(target)
                .execute(&mut raw)
                .await
                .unwrap();
        }
    }
    drop(raw);
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let cancel = ReadCancellation::new();
    let collections = [
        library::Collection::Album(fixture.albums[0]),
        library::Collection::Artist {
            key: fixture.artists[0],
            album_artist: false,
        },
        library::Collection::Artist {
            key: fixture.artists[0],
            album_artist: true,
        },
        library::Collection::Genre(fixture.genre),
        library::Collection::Mood(fixture.mood),
    ];
    for (index, collection) in collections.into_iter().enumerate() {
        let sort = if index == 0 {
            LibraryField::TrackNumber
        } else {
            LibraryField::Title
        };
        let rows = tracks(
            &fixture.catalog,
            library::MemberRead {
                sort: Some(sort),
                ..library::MemberRead::new(collection.clone())
            },
        )
        .await;
        match index {
            0 => assert!(
                rows.windows(2)
                    .all(|pair| pair[0].track_number < pair[1].track_number)
            ),
            1 => assert!(
                rows.windows(2)
                    .all(|pair| pair[0].title.to_lowercase() <= pair[1].title.to_lowercase())
            ),
            _ => {}
        }
        let total = rows.len();
        assert!(total > 100);
        assert!(
            rows.iter()
                .all(|row| row.source_key == Some(fixture.source))
        );
        let order = fixture
            .catalog
            .selected_track_uris(
                &library::MemberRead::new(collection.clone()).selection(),
                None,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(order.len(), total);
        let state = super::support::resolve_queue(
            &fixture.catalog,
            library::QueueLoad::Tracks(vec![(
                library::MemberRead::new(collection).selection(),
                library::QueueProvenance::Context {
                    context_id: "collection".into(),
                    context_title: None,
                    source_rank: 0,
                },
            )]),
            Default::default(),
        )
        .await;
        assert_eq!(state.entries.len(), total);
        assert_eq!(
            fixture
                .catalog
                .selected_track_uris(
                    &library::MemberSelection::Keys {
                        source: None,
                        keys: state
                            .entries
                            .iter()
                            .map(|entry| entry.track_key)
                            .collect::<Vec<_>>()
                            .into(),
                    },
                    None,
                    &ReadCancellation::new()
                )
                .await
                .unwrap(),
            order
        );
    }
    assert_eq!(
        tracks(
            &fixture.catalog,
            library::MemberRead {
                sort: Some(LibraryField::TrackNumber),
                folder: Some(fixture.folder),
                ..library::MemberRead::new(library::Collection::Album(fixture.albums[0]))
            }
        )
        .await
        .len(),
        2
    );
    assert!(
        uris(
            &fixture.catalog,
            library::MemberRead {
                sort: Some(LibraryField::TrackNumber),
                ..library::MemberRead::new(library::Collection::Album(library::AlbumKey::from_raw(
                    999
                )))
            }
        )
        .await
        .is_empty()
    );
}

#[tokio::test]
async fn favorite_collection_orders_keep_each_favorite_entity_extent() {
    let fixture = fixture().await;
    fixture
        .catalog
        .set_favorite(
            &FavoriteTarget::Album(fixture.album_uris[0].clone()),
            true,
            false,
        )
        .await
        .expect("favorite Album");
    fixture
        .catalog
        .set_favorite(
            &FavoriteTarget::Artist(fixture.artist_uris[1].clone()),
            true,
            false,
        )
        .await
        .expect("favorite Artist");
    let favorites = |kind| library::ListRead {
        favorites_only: true,
        activity: true,
        ..library::ListRead::new(kind, Some(fixture.source), LibraryField::Title)
    };
    assert_eq!(
        items(&fixture.catalog, favorites(library::ListKind::Albums)).await,
        [Item::Album(fixture.albums[0])]
    );
    assert_eq!(
        items(
            &fixture.catalog,
            favorites(library::ListKind::Artists {
                album_artist: false
            })
        )
        .await,
        [Item::Artist {
            key: fixture.artists[1],
            album_artist: false
        }]
    );

    let rows = super::support::rows(
        &fixture.catalog,
        library::Read::AlbumTracks(favorites(library::ListKind::Albums)),
    )
    .await;
    assert_eq!(
        rows.iter()
            .filter_map(|row| match row {
                library::Row::AlbumHeading(album) => Some(album.media_uri.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        [fixture.album_uris[0].as_str()]
    );
    assert!(
        matches!(&rows[0], library::Row::AlbumHeading(album) if !album.links.genres.is_empty())
    );
    assert_eq!(
        rows.iter()
            .filter_map(|row| match row {
                library::Row::AlbumTrack { track, .. } => Some(track.media_uri.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        fixture.track_uris[..2]
    );
}

#[tokio::test]
async fn tracks_and_collections_keep_complete_orders_and_bounded_rows() {
    let mut fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let source = library::MemberRead::new(library::Collection::Source(fixture.source));
    let title = fixture
        .catalog
        .selected_track_uris(
            &library::MemberRead {
                sort: Some(LibraryField::Title),
                ..source.clone()
            }
            .selection(),
            None,
            &cancel,
        )
        .await
        .expect("title Track order");
    assert_eq!(title.len(), 4);
    for (ranges, expected) in [
        (None, vec![title[0].clone(), title[1].clone()]),
        (
            Some(std::iter::once(1..4).collect()),
            vec![title[1].clone(), title[2].clone()],
        ),
        (
            Some(vec![0..1, 2..4]),
            vec![title[0].clone(), title[2].clone()],
        ),
    ] {
        let selection = library::MemberSelection::Members {
            read: source.clone(),
            ranges,
        };
        assert_eq!(
            fixture
                .catalog
                .selected_track_uris(&selection, Some(2), &cancel)
                .await
                .unwrap(),
            expected,
        );
    }
    let combined = library::MemberSelection::Concat(vec![
        library::MemberSelection::Uris(vec![title[3].clone(), title[3].clone()].into()),
        library::MemberSelection::Members {
            read: source.clone(),
            ranges: Some(vec![1..2, 3..4]),
        },
    ]);
    assert_eq!(
        fixture
            .catalog
            .selected_track_uris(&combined, Some(3), &cancel)
            .await
            .unwrap(),
        [title[3].clone(), title[3].clone(), title[1].clone()],
    );
    assert!(
        fixture
            .catalog
            .selected_track_uris(&combined, Some(0), &cancel)
            .await
            .unwrap()
            .is_empty()
    );
    let selected = |read: library::MemberRead| {
        let catalog = &fixture.catalog;
        let cancel = &cancel;
        async move {
            catalog
                .selected_track_uris(&read.selection(), None, cancel)
                .await
                .unwrap()
        }
    };
    assert_eq!(
        selected(library::MemberRead {
            sort: Some(LibraryField::TrackNumber),
            descending: true,
            ..source.clone()
        })
        .await,
        [1, 0, 3, 2].map(|index| fixture.track_uris[index].clone())
    );
    assert_eq!(
        selected(library::MemberRead {
            sort: Some(LibraryField::Title),
            favorites_only: true,
            ..source.clone()
        })
        .await
        .len(),
        1
    );
    assert_eq!(
        selected(library::MemberRead {
            sort: Some(LibraryField::Title),
            folder: Some(fixture.folder),
            ..source.clone()
        })
        .await
        .len(),
        4
    );

    let supplied = [title[2].clone(), title[0].clone()];
    let rows = fixture
        .catalog
        .tracks_by_uris(&supplied, &cancel)
        .await
        .expect("bounded Track rows");
    assert_eq!(
        rows.iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>(),
        supplied
    );
    assert_eq!(rows[0].links.artists[0].name, rows[0].metadata.artist);
    assert_eq!(rows[0].links.album_artists[0].name, rows[0].metadata.artist);
    assert_eq!(rows[0].links.genres[0].name, "Rock");
    assert_eq!(rows[0].state.rating, Some(8));
    assert_eq!(rows[0].state.play_count, 0);
    assert_eq!(rows[0].state.skip_count, 0);
    assert_eq!(rows[0].state.last_played, None);
    let list = |kind, sort, descending| library::ListRead {
        descending,
        activity: true,
        ..library::ListRead::new(kind, Some(fixture.source), sort)
    };
    let albums = fixture
        .albums
        .iter()
        .copied()
        .map(Item::Album)
        .collect::<Vec<_>>();
    let artists = |album_artist| {
        fixture
            .artists
            .iter()
            .map(|key| Item::Artist {
                key: *key,
                album_artist,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        items(
            &fixture.catalog,
            list(library::ListKind::Albums, LibraryField::Title, false)
        )
        .await,
        albums
    );
    assert_eq!(
        items(
            &fixture.catalog,
            list(library::ListKind::Albums, LibraryField::DateAdded, true)
        )
        .await,
        albums
    );
    assert_eq!(
        items(
            &fixture.catalog,
            list(
                library::ListKind::Artists { album_artist: true },
                LibraryField::Title,
                false
            )
        )
        .await,
        artists(true)
    );
    assert_eq!(
        items(
            &fixture.catalog,
            list(
                library::ListKind::Artists {
                    album_artist: false
                },
                LibraryField::Title,
                false
            )
        )
        .await,
        artists(false)
    );
    assert_eq!(
        items(
            &fixture.catalog,
            list(library::ListKind::Genres, LibraryField::Title, false)
        )
        .await,
        [Item::Genre(fixture.genre)]
    );
    assert_eq!(
        items(
            &fixture.catalog,
            list(library::ListKind::Genres, LibraryField::SongCount, true)
        )
        .await,
        [Item::Genre(fixture.genre)]
    );
    assert_eq!(
        items(
            &fixture.catalog,
            list(library::ListKind::Moods, LibraryField::Duration, true)
        )
        .await,
        [Item::Mood(fixture.mood)]
    );
    let album_rows = fixture
        .catalog
        .albums(&fixture.albums, true, &cancel)
        .await
        .expect("bounded Album rows");
    assert_eq!(album_rows[0].row.summary.track_count, 2);
    assert_eq!(album_rows[0].row.links.album_artists[0].name, "Artist A");
    assert_eq!(album_rows[0].row.links.genres[0].name, "Rock");
    let album_tracks = |descending| library::MemberRead {
        sort: Some(LibraryField::TrackNumber),
        descending,
        ..library::MemberRead::new(library::Collection::Album(album_rows[0].row.album_key))
    };
    let album_detail = tracks(&fixture.catalog, album_tracks(false)).await;
    assert_eq!(album_detail.len(), 2);
    assert_eq!(
        tracks(&fixture.catalog, album_tracks(true)).await,
        album_detail.into_iter().rev().collect::<Vec<_>>()
    );
    assert_eq!(
        album_rows[0]
            .row
            .links
            .album_artists
            .iter()
            .map(|artist| artist.artist_key)
            .collect::<Vec<_>>(),
        [fixture.artists[0]]
    );
    assert_eq!(album_rows[0].metadata.is_compilation, Some(true));
    assert_eq!(album_rows[0].metadata.release_types, ["Album".to_string()]);
    let candidates = fixture
        .catalog
        .album_release_candidates(fixture.source, 100, &cancel)
        .await
        .expect("Album release candidates");
    assert_eq!(candidates.len(), 2);
    let first = candidates
        .iter()
        .find(|candidate| candidate.album_key == fixture.albums[0])
        .unwrap();
    assert_eq!(first.lookup_identity, "release-group:group-a");
    assert_eq!(
        fixture
            .catalog
            .accept_album_release_result(
                fixture.source,
                first.album_key,
                &first.lookup_identity,
                AlbumReleaseResult::Missing
            )
            .await
            .expect("accept missing Album release"),
        Some(first.album_key)
    );
    let missing_row = fixture
        .catalog
        .albums(&[first.album_key], true, &cancel)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(missing_row.metadata.release_types, ["Album".to_string()]);

    let mut scan_observation = fixture.catalog.observe("source").await.unwrap();
    let write = library::CatalogBatch {
        albums: vec![library::AlbumObservation {
            user_data: library::NativeUserState {
                favorite: Some(false),
                ..Default::default()
            },
            metadata: library::AlbumMetadata {
                title: "Album A".into(),
                artist: "Artist A".into(),
                sort_text: "album a".into(),
                year: Some(2024),
                release_date: Some("2024-01-02".into()),
                date_added: Some("2024-01-02".into()),
                musicbrainz_release_group_id: Some("group-a2".into()),
                is_compilation: Some(true),
                album_artists: vec![library::ArtistCredit {
                    object_id: "artist-a".into(),
                    name: "Artist A".into(),
                    ..Default::default()
                }],
                genres: vec![library::TagReference {
                    object_id: "genre".into(),
                    name: "Rock".into(),
                    ..Default::default()
                }],
                release_types: vec!["Album".into()],
                ..Default::default()
            },
            object_id: "album-a".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    scan_observation.apply_catalog(&write, true).await.unwrap();
    assert!(scan_observation.changed());
    drop(scan_observation);
    let changed = fixture
        .catalog
        .album_release_candidates(fixture.source, 100, &cancel)
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.album_key == first.album_key)
        .unwrap();
    assert_eq!(changed.lookup_identity, "release-group:group-a2");
    assert_eq!(
        fixture
            .catalog
            .accept_album_release_result(
                fixture.source,
                first.album_key,
                &first.lookup_identity,
                AlbumReleaseResult::Found {
                    release_types: vec!["Stale".to_string()]
                }
            )
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        fixture
            .catalog
            .accept_album_release_result(
                fixture.source,
                changed.album_key,
                &changed.lookup_identity,
                AlbumReleaseResult::Found {
                    release_types: vec!["Compilation".to_string(), "Remix".to_string()]
                }
            )
            .await
            .unwrap(),
        Some(changed.album_key)
    );
    let found = fixture
        .catalog
        .albums(&[changed.album_key], true, &cancel)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        found.metadata.release_types,
        ["Compilation".to_string(), "Remix".to_string()]
    );

    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE track_links SET target=?3 WHERE role=0 AND track_key IN (?1,?2)")
        .bind(fixture.tracks[0])
        .bind(fixture.tracks[1])
        .bind(fixture.artists[1])
        .execute(&mut raw)
        .await
        .expect("separate direct Track Artist from Album Artist");
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;

    let album_artist_uri: String =
        sqlx::query_scalar("SELECT media_uri FROM artists WHERE artist_key=?1")
            .bind(fixture.artists[0])
            .fetch_one(&mut raw)
            .await
            .unwrap();
    let resolved = fixture
        .catalog
        .artist(&album_artist_uri, false, false, &cancel)
        .await
        .expect("resolve an artist credited only on albums")
        .unwrap()
        .row;
    assert_eq!(resolved.artist_key, fixture.artists[0]);
    assert_eq!(resolved.source_key, fixture.source);
    let detail = fixture
        .catalog
        .artists(&[resolved.artist_key], false, true, &cancel)
        .await
        .expect("known artist can open with no tracks in the requested role")
        .pop()
        .unwrap()
        .row;
    assert_eq!(detail.artist_key, resolved.artist_key);
    assert_eq!(detail.summary.track_count, 0);
    let artist_tracks = |album_artist| library::MemberRead {
        sort: Some(LibraryField::Title),
        ..library::MemberRead::new(library::Collection::Artist {
            key: fixture.artists[0],
            album_artist,
        })
    };
    assert_eq!(tracks(&fixture.catalog, artist_tracks(true)).await.len(), 2);
    assert_eq!(
        tracks(&fixture.catalog, artist_tracks(false)).await.len(),
        0
    );
    assert_eq!(
        fixture
            .catalog
            .artists(&[fixture.artists[0]], true, true, &cancel)
            .await
            .expect("Album Artist row")[0]
            .row
            .summary
            .track_count,
        2
    );
    assert_eq!(
        fixture
            .catalog
            .artists(&[fixture.artists[0]], false, true, &cancel)
            .await
            .expect("known artist without direct track credits")[0]
            .row
            .summary
            .track_count,
        0
    );
    assert_eq!(
        tag(&fixture.catalog, Item::Genre(fixture.genre))
            .await
            .expect("scoped Genre detail")
            .summary
            .track_count,
        4
    );
    assert_eq!(
        uris(
            &fixture.catalog,
            library::MemberRead {
                sort: Some(LibraryField::Title),
                folder: Some(fixture.folder),
                filter: "beta".into(),
                ..library::MemberRead::new(library::Collection::Album(fixture.albums[0]))
            }
        )
        .await,
        [fixture.track_uris[1].clone()]
    );
    assert_eq!(
        tracks(
            &fixture.catalog,
            library::MemberRead {
                sort: Some(LibraryField::LastPlayed),
                descending: true,
                ..library::MemberRead::new(library::Collection::Album(fixture.albums[0]))
            }
        )
        .await
        .len(),
        2
    );
    assert_eq!(
        tracks(
            &fixture.catalog,
            library::MemberRead {
                sort: Some(LibraryField::Album),
                folder: Some(fixture.folder),
                filter: "artist b".into(),
                descending: true,
                ..library::MemberRead::new(library::Collection::Genre(fixture.genre))
            }
        )
        .await
        .len(),
        2
    );
    assert_eq!(
        uris(
            &fixture.catalog,
            library::MemberRead {
                sort: Some(LibraryField::Year),
                filter: "2023".into(),
                ..library::MemberRead::new(library::Collection::Mood(fixture.mood))
            }
        )
        .await,
        [fixture.track_uris[3].clone()]
    );
    let artists_named = |album_artist| library::ListRead {
        filter: "artist a".into(),
        activity: true,
        ..library::ListRead::new(
            library::ListKind::Artists { album_artist },
            Some(fixture.source),
            LibraryField::Title,
        )
    };
    assert!(
        items(&fixture.catalog, artists_named(false))
            .await
            .is_empty()
    );
    assert_eq!(
        items(&fixture.catalog, artists_named(true)).await,
        [Item::Artist {
            key: fixture.artists[0],
            album_artist: true
        }]
    );
    assert_eq!(
        fixture
            .catalog
            .search(
                fixture.source,
                None,
                false,
                &SearchRequest::new("artist b"),
                &cancel
            )
            .await
            .expect("multi-term Artist Search")
            .artists
            .into_iter()
            .map(|artist| artist.name)
            .collect::<Vec<_>>(),
        ["Artist B"]
    );
    assert_eq!(
        fixture
            .catalog
            .search(
                fixture.source,
                None,
                true,
                &SearchRequest::new("artist a"),
                &cancel
            )
            .await
            .expect("Album Artist Search")
            .artists[0]
            .artist_key,
        fixture.artists[0]
    );
    assert_eq!(
        tag(&fixture.catalog, Item::Mood(fixture.mood))
            .await
            .expect("scoped Mood detail")
            .summary
            .track_count,
        4
    );
    let empty_folder = sqlx::query_scalar("INSERT INTO folders(source_key,object_id,media_uri,name,sort_text) VALUES (?1,'empty-folder','folder:empty-folder','Empty','empty') RETURNING folder_key")
        .bind(fixture.source).fetch_one(&mut raw).await.expect("insert empty Folder");
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    assert!(
        items(
            &fixture.catalog,
            library::ListRead {
                folder: Some(empty_folder),
                activity: true,
                ..library::ListRead::new(
                    library::ListKind::Albums,
                    Some(fixture.source),
                    LibraryField::Title
                )
            }
        )
        .await
        .is_empty()
    );
}

#[tokio::test]
async fn empty_named_collection_routes_return_empty_pages() {
    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    for statement in [
        "DELETE FROM track_links WHERE role IN (2,3)",
        "DELETE FROM album_links WHERE role=2",
        "DELETE FROM tags",
    ] {
        sqlx::query(statement).execute(&mut raw).await.unwrap();
    }
    drop(raw);
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    for (kind, sort) in [
        (library::ListKind::Genres, LibraryField::SongCount),
        (library::ListKind::Moods, LibraryField::Duration),
    ] {
        assert!(
            items(
                &fixture.catalog,
                library::ListRead {
                    activity: true,
                    ..library::ListRead::new(kind, Some(fixture.source), sort)
                }
            )
            .await
            .is_empty()
        );
    }
}

#[tokio::test]
async fn metadata_refresh_publishes_changes_and_leaves_no_ops_unchanged() {
    let fixture = fixture().await;
    let mut changes = super::support::Changes::of(&fixture.catalog);
    let cancel = ReadCancellation::new();
    let track = fixture.tracks[0];
    for refresh in 0..2 {
        changes.facts();
        let mut scan_observation = fixture.catalog.observe("source").await.unwrap();
        {
            let write = library::CatalogBatch {
                tracks: vec![library::TrackObservation {
                    user_data: library::NativeUserState {
                        favorite: Some(false),
                        rating: (Some(Some(5))).map(|rating| rating.map(|value| value * 10)),
                        ..Default::default()
                    },
                    metadata: library::TrackMetadata {
                        title: "Edited".into(),
                        album: "Album A".into(),
                        artist: "Artist A".into(),
                        sort_text: "edited".into(),
                        duration_millis: 181_000,
                        disc_number: 1,
                        track_number: 1,
                        year: Some(2025),
                        date_added: Some("2025-01-01".into()),
                        source_format: Some("FLAC".into()),
                        comment: Some("Changed".into()),
                        bpm: Some(121),
                        artists: vec![library::ArtistCredit {
                            object_id: "artist-a".into(),
                            name: "Edited Artist".into(),
                            ..Default::default()
                        }],
                        genres: vec![library::TagReference {
                            object_id: "genre".into(),
                            name: "Rock".into(),
                            ..Default::default()
                        }],
                        moods: vec![library::TagReference {
                            object_id: "mood".into(),
                            name: "Energetic".into(),
                            sort_text: Some("energetic".into()),
                        }],
                        ..Default::default()
                    },
                    object_id: "track-0".into(),
                    album_object_id: Some("album-a".into()),
                    audio_revision: Some([1; 32]),
                    ..Default::default()
                }],
                ..Default::default()
            };

            scan_observation.apply_catalog(&write, true).await.unwrap();
        };
        {
            let write = library::CatalogBatch {
                albums: vec![library::AlbumObservation {
                    user_data: library::NativeUserState {
                        favorite: Some(false),
                        ..Default::default()
                    },
                    metadata: library::AlbumMetadata {
                        title: "Edited Album".into(),
                        artist: "Artist A".into(),
                        sort_text: "edited album".into(),
                        year: Some(2025),
                        is_compilation: Some(true),
                        album_artists: vec![library::ArtistCredit {
                            object_id: "artist-a".into(),
                            name: "Edited Artist".into(),
                            ..Default::default()
                        }],
                        genres: vec![library::TagReference {
                            object_id: "genre".into(),
                            name: "Rock".into(),
                            ..Default::default()
                        }],
                        release_types: vec!["Album".into()],
                        ..Default::default()
                    },
                    object_id: "album-a".into(),
                    ..Default::default()
                }],
                ..Default::default()
            };

            scan_observation.apply_catalog(&write, true).await.unwrap();
        };
        {
            let write = library::CatalogBatch {
                artists: vec![library::ArtistObservation {
                    user_data: library::NativeUserState {
                        favorite: Some(false),
                        ..Default::default()
                    },
                    metadata: library::ArtistMetadata {
                        name: "Edited Artist".into(),
                        sort_text: Some("edited artist".into()),
                        ..Default::default()
                    },
                    object_id: "artist-a".into(),
                    ..Default::default()
                }],
                ..Default::default()
            };

            scan_observation.apply_catalog(&write, true).await.unwrap();
        };
        let update = changes.facts();
        assert_eq!(
            update.intersects(super::support::Shown::CATALOG),
            refresh == 0
        );
        if refresh == 1 {
            assert!(
                !update.intersects(super::support::Shown::PLAYLIST | super::support::Shown::HOME)
            );
        }
    }
    let row = fixture
        .catalog
        .tracks(&[track], &cancel)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(row.metadata.title, "Edited");
    assert_eq!(row.metadata.comment.as_deref(), Some("Changed"));
    let filtered = uris(
        &fixture.catalog,
        library::MemberRead {
            collection: library::Collection::Source(fixture.source),
            sort: Some(LibraryField::Title),
            filter: ("changed").into(),
            ..library::MemberRead::new(library::Collection::Source(fixture.source))
        },
    )
    .await;
    assert_eq!(filtered, [fixture.track_uris[0].clone()]);
    assert_eq!(
        uris(
            &fixture.catalog,
            library::MemberRead {
                collection: library::Collection::Source(fixture.source),
                sort: Some(LibraryField::Title),
                filter: ("2025").into(),
                ..library::MemberRead::new(library::Collection::Source(fixture.source))
            }
        )
        .await,
        [fixture.track_uris[0].clone()]
    );
    assert_eq!(
        items(
            &fixture.catalog,
            library::ListRead {
                playlist_scope: Default::default(),
                kind: library::ListKind::Albums,
                source: Some(fixture.source),
                sort: LibraryField::Title,
                folder: None,
                filter: ("rock").to_string(),
                descending: false,
                favorites_only: false,
                activity: true,
                downloaded_only: false
            }
        )
        .await,
        [
            Item::Album(fixture.albums[1]),
            Item::Album(fixture.albums[0])
        ]
    );
    assert_eq!(
        items(
            &fixture.catalog,
            library::ListRead {
                playlist_scope: Default::default(),
                kind: library::ListKind::Albums,
                source: Some(fixture.source),
                sort: LibraryField::Title,
                folder: None,
                filter: ("artist b").to_string(),
                descending: false,
                favorites_only: false,
                activity: true,
                downloaded_only: false
            }
        )
        .await,
        [Item::Album(fixture.albums[1])]
    );
    assert_eq!(
        items(
            &fixture.catalog,
            library::ListRead {
                playlist_scope: Default::default(),
                kind: library::ListKind::Albums,
                source: Some(fixture.source),
                sort: LibraryField::Title,
                folder: None,
                filter: ("2024").to_string(),
                descending: false,
                favorites_only: false,
                activity: true,
                downloaded_only: false
            }
        )
        .await,
        [Item::Album(fixture.albums[1])]
    );
}

#[tokio::test]
async fn album_genres_supply_browsing_and_playback_without_changing_track_tags() {
    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("DELETE FROM album_links WHERE role=2")
        .execute(&mut raw)
        .await
        .unwrap();
    // Keep one track credit overlapping the album credit to check set semantics.
    sqlx::query("DELETE FROM track_links WHERE role=2 AND track_key<>?1")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query(
        "INSERT OR REPLACE INTO album_links(album_key,role,position,target) VALUES(?1,2,0,?2)",
    )
    .bind(fixture.albums[0])
    .bind(fixture.genre)
    .execute(&mut raw)
    .await
    .unwrap();
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let cover = serde_json::to_vec(&[library::ArtworkCandidate::new(
        library::ArtworkImage::Native {
            source_id: library::SourceId::new("source"),
            item_id: "album-cover".into(),
            tag: None,
        },
        None,
    )])
    .unwrap();
    {
        let objects =
            sqlx::query_scalar::<_, String>("SELECT object_id FROM albums WHERE album_key=?1")
                .bind(fixture.albums[0])
                .fetch_all(&mut raw)
                .await
                .unwrap();
        fixture
            .catalog
            .observe("source")
            .await
            .unwrap()
            .set_source_artwork(
                &[],
                &[(
                    "album",
                    &objects
                        .into_iter()
                        .map(|id| (id, Some((&cover).to_vec())))
                        .collect::<Vec<_>>(),
                )],
            )
            .await
            .unwrap();
    };

    let cancel = ReadCancellation::new();
    fixture
        .catalog
        .pick_mosaics(&[library::MosaicOwner::Tag(fixture.genre.raw())])
        .await
        .unwrap();
    for folder in [None, Some(fixture.folder)] {
        let detail = tag(&fixture.catalog, Item::Genre(fixture.genre))
            .await
            .unwrap();
        assert_eq!(detail.summary.track_count, 2);
        assert_eq!(detail.summary.album_count, 1);
        assert_eq!(
            detail.representative_artwork.as_slice(),
            std::slice::from_ref(&cover)
        );
        let tracks = tracks(
            &fixture.catalog,
            library::MemberRead {
                collection: library::Collection::Genre(fixture.genre),
                sort: Some(LibraryField::Title),
                folder: folder,
                ..library::MemberRead::new(library::Collection::Genre(fixture.genre))
            },
        )
        .await;
        assert_eq!(tracks.len(), 2);
        assert!(tracks.iter().any(|track| track.links.genres.is_empty()));
        let home = fixture
            .catalog
            .home_page(
                fixture.source,
                folder,
                0,
                0,
                &[library::HomeBlockKind::Genres],
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(home.genres[0].summary.track_count, 2);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_links WHERE role=2")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1
    );
}
