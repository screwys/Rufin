use library::{
    CatalogSort, LocalAccessOrigin, LocalAccessWrite, PlayedFilter, RadioSeed, RandomCriteria,
    ReadCancellation, RouteSeedWindow, SearchRequest, SmartPlaylistActivityPeriod,
    SmartPlaylistDefinition, SmartPlaylistRule, SmartPlaylistRuleField, SmartPlaylistRuleOperator,
    SmartPlaylistRuleValue, SmartPlaylistSort,
};

use super::support::{connection, fixture};

#[tokio::test]
async fn artist_genre_and_smart_pages_keep_existing_membership_and_order() {
    use library::CatalogSort;
    let fixture = fixture().await;
    let db = &fixture.database;
    let cancel = ReadCancellation::new();
    for sort in [
        CatalogSort::Title,
        CatalogSort::AlbumCount,
        CatalogSort::TrackCount,
        CatalogSort::LastPlayed,
        CatalogSort::PlayCount,
        CatalogSort::UserRating,
        CatalogSort::Favorite,
    ] {
        for albums in [false, true] {
            for descending in [false, true] {
                for folder in [None, Some(fixture.folder)] {
                    for filter in ["", "artist"] {
                        let (count, _, rows) = library::ListRead {
                            playlist_scope: Default::default(),
                            kind: library::ListKind::Artists {
                                album_artist: albums,
                            },
                            source: Some(fixture.source),
                            sort: sort,
                            folder: folder,
                            filter: (filter).to_string(),
                            descending: descending,
                            favorites_only: false,
                            downloaded_only: false,
                            activity: true,
                        }
                        .page_artists(&db, RouteSeedWindow::top(), &cancel)
                        .await
                        .map(|page| (page.count, page.first_row_position, page.rows))
                        .unwrap();
                        let order = rows.iter().map(|row| row.artist_key).collect::<Vec<_>>();
                        assert_eq!(count, order.len());
                        let mut actual = Vec::new();
                        for offset in 0..=order.len() {
                            actual.extend(
                                library::ListRead {
                                    playlist_scope: Default::default(),
                                    kind: library::ListKind::Artists {
                                        album_artist: albums,
                                    },
                                    source: Some(fixture.source),
                                    sort: sort,
                                    folder: folder,
                                    filter: (filter).to_string(),
                                    descending: descending,
                                    favorites_only: false,
                                    downloaded_only: false,
                                    activity: true,
                                }
                                .artists(&db, offset, 1, &cancel)
                                .await
                                .unwrap()
                                .into_iter()
                                .map(|row| row.artist_key),
                            );
                        }
                        assert_eq!(actual, order);
                    }
                }
            }
        }
    }
    for sort in [
        CatalogSort::Title,
        CatalogSort::AlbumCount,
        CatalogSort::TrackCount,
    ] {
        for folder in [None, Some(fixture.folder)] {
            let (count, _, rows) = library::ListRead {
                playlist_scope: Default::default(),
                kind: library::ListKind::Genres,
                source: Some(fixture.source),
                sort: sort,
                folder: folder,
                filter: ("").to_string(),
                descending: false,
                favorites_only: false,
                downloaded_only: false,
                activity: true,
            }
            .page_genres(&db, RouteSeedWindow::top(), &cancel)
            .await
            .map(|page| (page.count, page.first_row_position, page.rows))
            .unwrap();
            let order = rows.iter().map(|row| row.genre_key).collect::<Vec<_>>();
            assert_eq!(count, order.len());
            let mut actual = Vec::new();
            for offset in 0..=order.len() {
                actual.extend(
                    library::ListRead {
                        playlist_scope: Default::default(),
                        kind: library::ListKind::Genres,
                        source: Some(fixture.source),
                        sort: sort,
                        folder: folder,
                        filter: ("").to_string(),
                        descending: false,
                        favorites_only: false,
                        downloaded_only: false,
                        activity: true,
                    }
                    .genres(&db, offset, 1, &cancel)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|row| row.genre_key),
                );
            }
            assert_eq!(actual, order, "Genre {sort:?}, folder {folder:?}");
        }
    }
    let external = "https://example.test/outside-catalog.flac".to_string();
    let (_, outside_id) = db
        .create_playlist(
            None,
            "Outside",
            &library::MemberSelection::Uris(((std::slice::from_ref(&external)).to_vec()).into()),
        )
        .await
        .unwrap()
        .unwrap();
    let all = db
        .create_smart_playlist("All", &SmartPlaylistDefinition::default())
        .await
        .unwrap();
    db.create_smart_playlist(
        "Source",
        &SmartPlaylistDefinition {
            source_id: Some(library::SourceId::new("source")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let limited = db
        .create_smart_playlist(
            "Limited",
            &SmartPlaylistDefinition {
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let empty = db
        .create_smart_playlist(
            "Empty",
            &SmartPlaylistDefinition {
                source_id: Some(library::SourceId::new("absent")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let other = db
        .reconcile_source(&library::SourceId::new("other"), "local", "Other")
        .await
        .unwrap()
        .source;
    for source in [None, Some(fixture.source), Some(other)] {
        for folder in [None, Some(fixture.folder)] {
            for sort in [
                CatalogSort::Position,
                CatalogSort::Title,
                CatalogSort::TrackCount,
                CatalogSort::Duration,
            ] {
                for descending in [false, true] {
                    let (count, _, rows) = library::ListRead {
                        playlist_scope: Default::default(),
                        kind: library::ListKind::SmartPlaylists { now: 1000 },
                        source: (source).into(),
                        sort: sort,
                        folder: folder,
                        filter: ("").to_string(),
                        descending: descending,
                        favorites_only: false,
                        downloaded_only: false,
                        activity: true,
                    }
                    .page_smart_playlists(&db, RouteSeedWindow::top(), &cancel)
                    .await
                    .map(|page| (page.count, page.first_row_position, page.rows))
                    .unwrap();
                    let order = rows
                        .iter()
                        .map(|row| row.smart_playlist_key)
                        .collect::<Vec<_>>();
                    assert_eq!(count, order.len());
                    assert_eq!(
                        count, 4,
                        "source and folder selection cannot hide a smart playlist"
                    );
                    for row in &rows {
                        let expected = match row.name.as_str() {
                            "Limited" => 1,
                            "Empty" => 0,
                            _ => fixture.tracks.len() as i64,
                        };
                        assert_eq!(row.track_count, expected);
                        if row.smart_playlist_key == limited {
                            assert_eq!(row.definition.limit, Some(1));
                        }
                    }

                    let mut actual = Vec::new();
                    for offset in 0..=order.len() {
                        actual.extend(
                            library::ListRead {
                                playlist_scope: Default::default(),
                                kind: library::ListKind::SmartPlaylists { now: 1000 },
                                source: (source).into(),
                                sort: sort,
                                folder: folder,
                                filter: ("").to_string(),
                                descending: descending,
                                favorites_only: false,
                                downloaded_only: false,
                                activity: true,
                            }
                            .smart_playlists(&db, offset, 1, &cancel)
                            .await
                            .unwrap()
                            .into_iter()
                            .map(|row| row.smart_playlist_key),
                        );
                    }
                    assert_eq!(actual, order, "{sort:?}");
                }
            }
        }
    }
    let album_id = library::source_entity_parts(&fixture.album_uris[0])
        .unwrap()
        .2;
    let artist_id = library::source_entity_parts(&fixture.artist_uris[0])
        .unwrap()
        .2;
    let genre = db
        .genres(&[fixture.genre], None, &cancel)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let smart_rows = db
        .smart_playlist_rows(&[all, limited, empty], 1000, &cancel)
        .await
        .unwrap();
    let regular_folder = db
        .create_playlist_folder("Pinned playlists", false)
        .await
        .unwrap();
    let smart_folder = db
        .create_playlist_folder("Pinned smart playlists", true)
        .await
        .unwrap();
    let outside = db
        .playlist_key_by_identity(None, &outside_id, &cancel)
        .await
        .unwrap()
        .unwrap();
    db.move_playlist_to_folder(
        library::PlaylistFolderMember::Playlist { key: outside },
        Some(regular_folder),
    )
    .await
    .unwrap();
    db.move_playlist_to_folder(
        library::PlaylistFolderMember::SmartPlaylist { key: limited },
        Some(smart_folder),
    )
    .await
    .unwrap();
    db.move_playlist_to_folder(
        library::PlaylistFolderMember::SmartPlaylist { key: all },
        Some(smart_folder),
    )
    .await
    .unwrap();
    let mut raw = connection(&fixture.path).await;
    let folders: Vec<(i64, String)> =
        sqlx::query_as("SELECT folder_key,object_id FROM playlist_folders ORDER BY folder_key")
            .fetch_all(&mut raw)
            .await
            .unwrap();
    let source_id = library::SourceId::new("source");
    let pins = vec![
        library::SidebarPin::Album {
            source_id: source_id.clone(),
            album_id,
        },
        library::SidebarPin::Artist {
            source_id: source_id.clone(),
            artist_id: artist_id.clone(),
            album_artist: false,
        },
        library::SidebarPin::Artist {
            source_id: source_id.clone(),
            artist_id,
            album_artist: true,
        },
        library::SidebarPin::Genre {
            source_id: source_id.clone(),
            genre_id: genre.object_id.clone(),
        },
        library::SidebarPin::Playlist {
            source_id: None,
            playlist_id: outside_id,
        },
        library::SidebarPin::SmartPlaylist {
            playlist_id: smart_rows[0].object_id.clone(),
        },
        library::SidebarPin::SmartPlaylist {
            playlist_id: smart_rows[1].object_id.clone(),
        },
        library::SidebarPin::PlaylistFolder {
            folder_id: folders[0].1.clone(),
            smart: false,
        },
        library::SidebarPin::PlaylistFolder {
            folder_id: folders[1].1.clone(),
            smart: true,
        },
        library::SidebarPin::SmartPlaylist {
            playlist_id: smart_rows[2].object_id.clone(),
        },
        library::SidebarPin::Album {
            source_id,
            album_id: "missing".into(),
        },
    ];
    let headers = db.sidebar_pin_rows(&pins, &cancel).await.unwrap();
    assert_eq!(
        headers.iter().map(|row| &row.pin).collect::<Vec<_>>(),
        pins[..10].iter().collect::<Vec<_>>()
    );
    assert_eq!(headers[4].totals, Some((1, 0)));
    assert!(headers[7].totals.is_none());
    assert!(
        headers[5].totals.is_none() && headers[6].totals.is_none() && headers[8].totals.is_none()
    );
    let rows = db.sidebar_pin_totals(headers, 1000, &cancel).await.unwrap();
    assert_eq!(
        rows[5].totals,
        Some((smart_rows[0].track_count, smart_rows[0].duration_millis))
    );
    assert_eq!(
        rows[6].totals,
        Some((smart_rows[1].track_count, smart_rows[1].duration_millis))
    );
    assert_eq!(
        rows[8].totals,
        Some((
            smart_rows[0].track_count + smart_rows[1].track_count,
            smart_rows[0].duration_millis + smart_rows[1].duration_millis
        ))
    );
    assert_eq!(rows[7].totals, Some((1, 0)));
    assert_eq!(rows[9].totals, Some((0, 0)));
    let album = db
        .album(&fixture.album_uris[0], false, &cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        rows[0].totals,
        Some((album.summary.track_count, album.summary.duration_millis))
    );
    assert_eq!(
        rows[0].artwork,
        album.artwork_binding.iter().cloned().collect::<Vec<_>>()
    );
    for (index, role) in [(1, false), (2, true)] {
        let artist = db
            .artist(&fixture.artist_uris[0], role, false, &cancel)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            rows[index].totals,
            Some((artist.summary.track_count, artist.summary.duration_millis))
        );
    }
    assert_eq!(
        rows[3].totals,
        Some((genre.summary.track_count, genre.summary.duration_millis))
    );
    assert_eq!(
        rows[3].artwork,
        genre
            .artwork_binding
            .as_ref()
            .map(std::slice::from_ref)
            .unwrap_or(&genre.representative_artwork)
    );
    let order = db
        .selected_track_uris(
            &library::MemberRead::new(library::Collection::Smart {
                key: all,
                now: 1000,
            })
            .selection(),
            None,
            &cancel,
        )
        .await
        .unwrap();
    assert!(!order.contains(&external));
    let mut actual = Vec::new();
    for offset in 0..=order.len() {
        actual.extend(
            (&library::MemberRead::new(library::Collection::Smart {
                key: all,
                now: 1000,
            }))
                .rows(&db, offset, 1, &cancel)
                .await
                .map(|rows| {
                    rows.into_iter()
                        .map(library::MemberRow::into_track)
                        .collect::<Vec<_>>()
                })
                .unwrap()
                .into_iter()
                .map(|row| row.media_uri.clone()),
        );
    }
    assert_eq!(actual, order);
    assert!(
        (&library::MemberRead {
            filter: ("Beta").into(),
            ..library::MemberRead::new(library::Collection::Smart {
                key: limited,
                now: 1000
            })
        })
            .rows(&db, 0, 10, &cancel)
            .await
            .map(|rows| rows
                .into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>())
            .unwrap()
            .is_empty()
    );
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET metadata=jsonb(json_set(metadata,'$.title','İSTANBUL')),normalized_search=?2 WHERE media_uri=?1")
        .bind(&fixture.track_uris[0])
        .bind("İSTANBUL".to_lowercase())
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    let filtered = (&library::MemberRead {
        filter: ("i\u{307}st").into(),
        ..library::MemberRead::new(library::Collection::Smart {
            key: all,
            now: 1000,
        })
    })
        .rows(&db, 0, 10, &cancel)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert_eq!(
        filtered
            .iter()
            .map(|row| &row.media_uri)
            .collect::<Vec<_>>(),
        [&fixture.track_uris[0]]
    );
    let captured = super::support::resolve_queue(
        db,
        library::QueueLoad::Tracks(vec![(
            library::MemberRead {
                collection: library::Collection::Smart {
                    key: all,
                    now: 1000,
                },
                sort: Some(library::CatalogSort::Title),
                filter: "i\u{307}st".into(),
                ..library::MemberRead::new(library::Collection::Smart {
                    key: all,
                    now: 1000,
                })
            }
            .selection(),
            library::QueueProvenance::Context {
                context_id: "smart search".into(),
                context_title: None,
                source_rank: 0,
            },
        )]),
        0,
    )
    .await;
    assert_eq!(
        captured
            .entries
            .iter()
            .map(|row| row.track_key)
            .collect::<Vec<_>>(),
        [fixture.tracks[0]]
    );
}

#[tokio::test]
async fn collection_pages_preserve_native_orders_and_duplicate_entries() {
    use library::CatalogSort;
    let fixture = fixture().await;
    let database = &fixture.database;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE albums SET metadata=jsonb(json_set(metadata,'$.year',CASE object_id WHEN 'album-a' THEN 2020 ELSE 1990 END)) ")
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("UPDATE album_artists SET artist_key=?1 WHERE album_key=?2")
        .bind(fixture.artists[0])
        .bind(fixture.albums[1])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO album_artists(album_key,artist_key,position) VALUES(?1,?2,1)")
        .bind(fixture.albums[0])
        .bind(fixture.artists[1])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("UPDATE albums SET year_sort=year,album_artist_sort=(SELECT artist.sort_text FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=albums.album_key ORDER BY credit.position LIMIT 1)").execute(&mut raw).await.unwrap();
    sqlx::query("UPDATE tracks SET (album_title_sort,album_artist_sort,album_year_sort)=(SELECT title_sort,album_artist_sort,year_sort FROM albums WHERE albums.album_key=tracks.album_key)").execute(&mut raw).await.unwrap();
    sqlx::query("DELETE FROM track_folders WHERE track_key IN(SELECT track_key FROM tracks WHERE album_key=?1)")
        .bind(fixture.albums[1]).execute(&mut raw).await.unwrap();
    sqlx::query("UPDATE albums SET year_sort=year,album_artist_sort=(SELECT artist.sort_text FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=albums.album_key ORDER BY credit.position LIMIT 1)").execute(&mut raw).await.unwrap();
    sqlx::query("UPDATE tracks SET (album_title_sort,album_artist_sort,album_year_sort)=(SELECT title_sort,album_artist_sort,year_sort FROM albums WHERE albums.album_key=tracks.album_key)").execute(&mut raw).await.unwrap();
    drop(raw);
    for sort in [
        CatalogSort::Title,
        CatalogSort::AlbumArtist,
        CatalogSort::Year,
        CatalogSort::ReleaseDate,
        CatalogSort::DateAdded,
        CatalogSort::LastPlayed,
        CatalogSort::PlayCount,
        CatalogSort::UserRating,
        CatalogSort::TrackCount,
        CatalogSort::Duration,
        CatalogSort::Favorite,
    ] {
        for descending in [false, true] {
            for folder in [None, Some(fixture.folder)] {
                for filter in ["", "album"] {
                    let (count, _, rows) = library::ListRead {
                        playlist_scope: Default::default(),
                        kind: library::ListKind::Albums,
                        source: Some(fixture.source),
                        sort: sort,
                        folder: folder,
                        filter: (filter).to_string(),
                        descending: descending,
                        favorites_only: false,
                        downloaded_only: false,
                        activity: true,
                    }
                    .page_albums(&database, RouteSeedWindow::top(), &cancel)
                    .await
                    .map(|page| (page.count, page.first_row_position, page.rows))
                    .unwrap();
                    let order = rows.iter().map(|row| row.album_key).collect::<Vec<_>>();
                    assert_eq!(count, order.len());
                    if sort == CatalogSort::AlbumArtist {
                        let expected = if folder.is_some() {
                            vec![fixture.albums[0]]
                        } else {
                            vec![fixture.albums[1], fixture.albums[0]]
                        };
                        assert_eq!(
                            order, expected,
                            "Album artist keeps albums chronological in either direction"
                        );
                        assert_eq!(
                            library::ListRead {
                                playlist_scope: Default::default(),
                                kind: library::ListKind::ArtistAlbums {
                                    artist: fixture.artists[0],
                                    album_artist: true,
                                    filters: None,
                                    release_class: None
                                },
                                source: Some(fixture.source),
                                folder: folder,
                                filter: (filter).to_string(),
                                sort: sort,
                                descending: descending,
                                activity: true,
                                favorites_only: false,
                                downloaded_only: false
                            }
                            .albums(&database, 0, 64, &cancel)
                            .await
                            .unwrap()
                            .into_iter()
                            .map(|row| row.album_key)
                            .collect::<Vec<_>>(),
                            order
                        );
                        assert_eq!(
                            library::ListRead {
                                playlist_scope: Default::default(),
                                kind: library::ListKind::Albums,
                                source: Some(fixture.source),
                                sort: sort,
                                folder: folder,
                                filter: (filter).to_string(),
                                descending: descending,
                                favorites_only: false,
                                activity: true,
                                downloaded_only: false
                            }
                            .album_detail_index(&database, &cancel)
                            .await
                            .unwrap()
                            .rows(&database, 0, 128, &cancel)
                            .await
                            .unwrap()
                            .into_iter()
                            .filter_map(|row| match row {
                                library::AlbumDetailRow::Album { album, .. } =>
                                    Some(album.media_uri),
                                library::AlbumDetailRow::Track { .. } => None,
                            })
                            .collect::<Vec<_>>(),
                            if folder.is_some() {
                                vec![fixture.album_uris[0].clone()]
                            } else {
                                vec![fixture.album_uris[1].clone(), fixture.album_uris[0].clone()]
                            }
                        );
                    }
                    let mut paged = Vec::new();
                    for offset in 0..=order.len() {
                        let page = library::ListRead {
                            playlist_scope: Default::default(),
                            kind: library::ListKind::Albums,
                            source: Some(fixture.source),
                            sort: sort,
                            folder: folder,
                            filter: (filter).to_string(),
                            descending: descending,
                            favorites_only: false,
                            downloaded_only: false,
                            activity: true,
                        }
                        .albums(&database, offset, 1, &cancel)
                        .await
                        .unwrap();
                        assert!(page.len() <= 1);
                        paged.extend(page.into_iter().map(|row| row.album_key));
                    }
                    assert_eq!(paged, order, "album {sort:?}, {descending}, {filter}");
                }
            }
        }
    }
    for album in &fixture.albums {
        for folder in [None, Some(fixture.folder)] {
            let page = (&library::MemberRead {
                collection: library::Collection::Album(*album),
                sort: Some(library::CatalogSort::TrackNumber),
                folder: folder,
                ..library::MemberRead::new(library::Collection::Album(*album))
            })
                .page(&database, RouteSeedWindow::top(), &cancel)
                .await
                .unwrap();
            let mut paged = Vec::new();
            for offset in 0..=page.count {
                paged.extend(
                    (&library::MemberRead {
                        collection: library::Collection::Album(*album),
                        sort: Some(library::CatalogSort::TrackNumber),
                        folder: folder,
                        ..library::MemberRead::new(library::Collection::Album(*album))
                    })
                        .rows(&database, offset, 1, &cancel)
                        .await
                        .map(|rows| {
                            rows.into_iter()
                                .map(library::MemberRow::into_track)
                                .collect::<Vec<_>>()
                        })
                        .unwrap()
                        .into_iter()
                        .map(|row| row.media_uri.clone()),
                );
            }
            assert_eq!(paged.len(), page.count);
            assert_eq!(
                paged,
                ((page.rows)
                    .iter()
                    .map(library::MemberRow::track)
                    .collect::<Vec<_>>())
                .into_iter()
                .map(|row| row.media_uri.clone())
                .collect::<Vec<_>>()
            );
        }
    }
    let uris = [
        fixture.track_uris[0].clone(),
        fixture.track_uris[1].clone(),
        fixture.track_uris[0].clone(),
    ];
    let playlist = database
        .create_playlist(
            None,
            "Global",
            &library::MemberSelection::Uris(((&uris).to_vec()).into()),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
    database
        .create_playlist(
            Some(fixture.source),
            "Source",
            &library::MemberSelection::Uris(((&uris).to_vec()).into()),
        )
        .await
        .unwrap();
    for source in [None, Some(fixture.source)] {
        for sort in [
            CatalogSort::Position,
            CatalogSort::Title,
            CatalogSort::TrackCount,
            CatalogSort::Duration,
        ] {
            for descending in [false, true] {
                for filter in ["", "global"] {
                    let (count, _, rows) = library::ListRead {
                        playlist_scope: Default::default(),
                        kind: library::ListKind::Playlists,
                        source: (source).into(),
                        sort: sort,
                        folder: None,
                        filter: (filter).to_string(),
                        descending: descending,
                        favorites_only: false,
                        downloaded_only: false,
                        activity: true,
                    }
                    .page_playlists(&database, RouteSeedWindow::top(), &cancel)
                    .await
                    .map(|page| (page.count, page.first_row_position, page.rows))
                    .unwrap();
                    let order = rows.iter().map(|row| row.playlist_key).collect::<Vec<_>>();
                    assert_eq!(count, order.len());
                    let mut paged = Vec::new();
                    for offset in 0..=order.len() {
                        paged.extend(
                            library::ListRead {
                                playlist_scope: Default::default(),
                                kind: library::ListKind::Playlists,
                                source: (source).into(),
                                sort: sort,
                                folder: None,
                                filter: (filter).to_string(),
                                descending: descending,
                                favorites_only: false,
                                downloaded_only: false,
                                activity: true,
                            }
                            .playlists(&database, offset, 1, &cancel)
                            .await
                            .unwrap()
                            .into_iter()
                            .map(|row| row.playlist_key),
                        );
                    }
                    assert_eq!(paged, order);
                }
            }
        }
    }
    for sort in [
        CatalogSort::Position,
        CatalogSort::Title,
        CatalogSort::Artist,
        CatalogSort::Album,
        CatalogSort::AlbumArtist,
        CatalogSort::TrackNumber,
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
            for filter in ["", "alpha"] {
                let query = library::MemberRead {
                    collection: library::Collection::Playlist(playlist),
                    sort: Some(sort),
                    filter: filter.into(),
                    descending: descending,
                    ..library::MemberRead::new(library::Collection::Playlist(playlist))
                };
                let order = (&query)
                    .rows(&database, 0, 256, &cancel)
                    .await
                    .map(|rows| {
                        rows.into_iter()
                            .filter_map(library::MemberRow::into_entry)
                            .collect::<Vec<_>>()
                    })
                    .unwrap()
                    .into_iter()
                    .map(|entry| entry.playlist_entry_key)
                    .collect::<Vec<_>>();
                let edit_entries = database
                    .playlist_edit_entries(playlist, &query.selection(), &cancel)
                    .await
                    .unwrap();
                let mut edited = edit_entries
                    .into_iter()
                    .map(|entry| entry.playlist_entry_key)
                    .collect::<Vec<_>>();
                edited.sort();
                let mut selected = order.clone();
                selected.sort();
                assert_eq!(edited, selected);
                let mut paged = Vec::new();
                for offset in 0..=order.len() {
                    paged.extend(
                        (&library::MemberRead {
                            collection: library::Collection::Playlist(playlist),
                            sort: Some(sort),
                            filter: (filter).into(),
                            descending: descending,
                            ..library::MemberRead::new(library::Collection::Playlist(playlist))
                        })
                            .rows(&database, offset, 1, &cancel)
                            .await
                            .map(|rows| {
                                rows.into_iter()
                                    .filter_map(library::MemberRow::into_entry)
                                    .collect::<Vec<_>>()
                            })
                            .unwrap()
                            .into_iter()
                            .map(|row| row.playlist_entry_key),
                    );
                }
                assert_eq!(paged, order);
            }
        }
    }
    for (parent, order) in [
        (None, vec![fixture.folder]),
        (Some(fixture.folder), Vec::new()),
    ] {
        let mut paged = Vec::new();
        for offset in 0..=order.len() {
            paged.extend(
                database
                    .folder_page(fixture.source, parent, offset, 1, &cancel)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|row| row.folder_key),
            );
        }
        assert_eq!(paged, order);
    }
}

#[tokio::test]
async fn track_pages_preserve_sort_filter_and_source_scope() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE albums SET metadata=jsonb(json_set(metadata,'$.year',CASE object_id WHEN 'album-a' THEN 2020 ELSE 1990 END)) ")
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("UPDATE album_artists SET artist_key=?1 WHERE album_key=?2")
        .bind(fixture.artists[0])
        .bind(fixture.albums[1])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("UPDATE albums SET year_sort=year,album_artist_sort=(SELECT artist.sort_text FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=albums.album_key ORDER BY credit.position LIMIT 1)").execute(&mut raw).await.unwrap();
    sqlx::query("UPDATE tracks SET (album_title_sort,album_artist_sort,album_year_sort)=(SELECT title_sort,album_artist_sort,year_sort FROM albums WHERE albums.album_key=tracks.album_key)").execute(&mut raw).await.unwrap();
    drop(raw);
    for sort in [
        library::CatalogSort::Title,
        library::CatalogSort::TrackNumber,
        library::CatalogSort::Artist,
        library::CatalogSort::AlbumArtist,
        library::CatalogSort::Album,
        library::CatalogSort::Year,
        library::CatalogSort::ReleaseDate,
        library::CatalogSort::DateAdded,
        library::CatalogSort::LastPlayed,
        library::CatalogSort::PlayCount,
        library::CatalogSort::UserRating,
        library::CatalogSort::Genre,
        library::CatalogSort::Bpm,
        library::CatalogSort::Duration,
        library::CatalogSort::Favorite,
    ] {
        for descending in [false, true] {
            for folder in [None, Some(fixture.folder)] {
                let order = fixture
                    .database
                    .selected_track_uris(
                        &library::MemberRead {
                            collection: library::Collection::Source(fixture.source),
                            sort: Some(sort),
                            folder: folder,
                            descending: descending,
                            ..library::MemberRead::new(library::Collection::Source(fixture.source))
                        }
                        .selection(),
                        None,
                        &cancel,
                    )
                    .await
                    .unwrap();
                let expected = match sort {
                    library::CatalogSort::AlbumArtist => Some([2, 3, 0, 1]),
                    library::CatalogSort::Artist | library::CatalogSort::Album => {
                        Some(if descending {
                            [2, 3, 0, 1]
                        } else {
                            [0, 1, 2, 3]
                        })
                    }
                    _ => None,
                };
                if let Some(expected) = expected {
                    assert_eq!(
                        order,
                        expected.map(|index| fixture.track_uris[index].clone()),
                        "{sort:?}, descending={descending}"
                    );
                }
                let mut pages = Vec::new();
                for offset in (0..order.len()).step_by(2) {
                    let page = (&library::MemberRead {
                        collection: library::Collection::Source(fixture.source),
                        sort: Some(sort),
                        folder: folder,
                        descending: descending,
                        ..library::MemberRead::new(library::Collection::Source(fixture.source))
                    })
                        .rows(&fixture.database, offset, 2, &cancel)
                        .await
                        .map(|rows| {
                            rows.into_iter()
                                .map(library::MemberRow::into_track)
                                .collect::<Vec<_>>()
                        })
                        .unwrap();
                    assert!(page.len() <= 2);
                    pages.extend(page.into_iter().map(|row| row.media_uri.clone()));
                }
                assert_eq!(pages, order);
            }
        }
    }
    let rows = (&library::MemberRead {
        collection: library::Collection::Source(fixture.source),
        sort: Some(library::CatalogSort::Title),
        filter: ("beta").into(),
        ..library::MemberRead::new(library::Collection::Source(fixture.source))
    })
        .rows(&fixture.database, 0, 10, &cancel)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].metadata.title, "Beta");
    assert!(
        (&library::MemberRead {
            collection: library::Collection::Source(fixture.source),
            sort: Some(library::CatalogSort::Title),
            ..library::MemberRead::new(library::Collection::Source(fixture.source))
        })
            .rows(&fixture.database, usize::MAX, 10, &cancel)
            .await
            .map(|rows| rows
                .into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>())
            .unwrap()
            .is_empty()
    );
    assert!(
        (&library::MemberRead {
            collection: library::Collection::Source(fixture.source),
            sort: Some(library::CatalogSort::Title),
            ..library::MemberRead::new(library::Collection::Source(fixture.source))
        })
            .rows(&fixture.database, 0, 0, &cancel)
            .await
            .map(|rows| rows
                .into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>())
            .unwrap()
            .is_empty()
    );

    let mut raw = connection(&fixture.path).await;
    sqlx::query("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<300) INSERT INTO tracks(source_key,object_id,media_uri,metadata,normalized_search,indexed) SELECT ?1,'extra-'||x,'test:extra-'||x,jsonb(json_object('title',COALESCE('Extra '||x,''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('extra-'||x,''),'duration_millis',COALESCE(1000,0))),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE('Extra '||x,''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('extra-'||x,''),'duration_millis',COALESCE(1000,0))),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Extra '||x,''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('extra-'||x,''),'duration_millis',COALESCE(1000,0))),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Extra '||x,''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('extra-'||x,''),'duration_millis',COALESCE(1000,0))),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Extra '||x,''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('extra-'||x,''),'duration_millis',COALESCE(1000,0))),'$.comment'),'')),1 FROM n")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    let rows = (&library::MemberRead {
        collection: library::Collection::Source(fixture.source),
        sort: Some(library::CatalogSort::Title),
        ..library::MemberRead::new(library::Collection::Source(fixture.source))
    })
        .rows(&fixture.database, 0, usize::MAX, &cancel)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert_eq!(rows.len(), 256);
}

#[tokio::test]
async fn radio_fills_short_album_and_track_seeds_from_the_selected_source() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    // The two albums have distinct artists and no shared genre.
    for sql in ["DELETE FROM track_genres", "DELETE FROM album_genres"] {
        sqlx::query(sql).execute(&mut raw).await.unwrap();
    }
    let cancel = ReadCancellation::new();
    for variation in [0, 2, 4, 99] {
        for seed in [
            RadioSeed::Album(fixture.albums[0]),
            RadioSeed::Track(fixture.track_uris[0].clone()),
        ] {
            let mut candidates = fixture
                .database
                .radio_candidates(
                    fixture.source,
                    seed.clone(),
                    &[],
                    20,
                    false,
                    variation,
                    &cancel,
                )
                .await
                .unwrap();
            if matches!(seed, RadioSeed::Track(_)) {
                assert_eq!(candidates.first(), Some(&fixture.tracks[1]));
            }
            candidates.sort();
            let mut expected = fixture.tracks[if matches!(seed, RadioSeed::Album(_)) {
                2
            } else {
                1
            }..]
                .to_vec();
            expected.sort();
            assert_eq!(candidates, expected, "{seed:?}, variation {variation}");
        }
    }
    sqlx::query("INSERT INTO queue_entries(track_key,provenance) SELECT track_key,'\"Manual\"' FROM tracks WHERE media_uri=?1")
        .bind(&fixture.track_uris[1])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO queue_positions(order_kind,position,entry_id) SELECT kind,row_number() OVER(PARTITION BY kind ORDER BY entry_id)-1,entry_id FROM queue_entries CROSS JOIN (SELECT 0 kind UNION ALL SELECT 1) WHERE true ON CONFLICT(order_kind,position) DO UPDATE SET entry_id=excluded.entry_id")
        .execute(&mut raw).await.unwrap();
    for seed in [
        RadioSeed::Album(fixture.albums[0]),
        RadioSeed::Track(fixture.track_uris[0].clone()),
    ] {
        assert_eq!(
            fixture
                .database
                .radio_candidates(
                    fixture.source,
                    seed,
                    &[fixture.tracks[2]],
                    20,
                    false,
                    3,
                    &cancel,
                )
                .await
                .unwrap(),
            vec![fixture.tracks[3]]
        );
    }
}

#[tokio::test]
async fn radio_resolves_playlist_ownership_and_filters_native_recommendations() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('other','Other','other',zeroblob(32))")
        .execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO tracks(source_key,object_id,media_uri,metadata,normalized_search,indexed) SELECT source_key,'track-2','test:other',jsonb(json_object('title',COALESCE('Other',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('other',''),'duration_millis',COALESCE(1000,0))),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE('Other',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('other',''),'duration_millis',COALESCE(1000,0))),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Other',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('other',''),'duration_millis',COALESCE(1000,0))),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Other',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('other',''),'duration_millis',COALESCE(1000,0))),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Other',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('other',''),'duration_millis',COALESCE(1000,0))),'$.comment'),'')),1 FROM sources WHERE object_id='other'")
        .execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO playlists(source_key,owner,object_id,name,normalized_name,sort_text,writable,position) VALUES(?1,'source','native','Native','native','native',0,0)")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO playlist_entries(playlist_key,object_id,track_key,position) SELECT playlist_key,'entry',(SELECT track_key FROM tracks WHERE media_uri=?1),0 FROM playlists WHERE object_id='native'")
        .bind(&fixture.track_uris[0]).execute(&mut raw).await.unwrap();
    let native = fixture
        .database
        .playlist_key_by_object(fixture.source, "native", &cancel)
        .await
        .unwrap()
        .unwrap();
    let local = fixture
        .database
        .create_playlist(
            None,
            "Mixed",
            &library::MemberSelection::Uris(
                ((&[fixture.track_uris[0].clone(), "test:other".into()]).to_vec()).into(),
            ),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
    let (owned, _) = fixture
        .database
        .create_playlist(
            Some(fixture.source),
            "Owned",
            &library::MemberSelection::Uris(((&fixture.track_uris[..1]).to_vec()).into()),
        )
        .await
        .unwrap()
        .unwrap();
    sqlx::query("INSERT INTO queue_entries(track_key,provenance) SELECT track_key,'\"Manual\"' FROM tracks WHERE media_uri=?1")
        .bind(&fixture.track_uris[1])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO queue_positions(order_kind,position,entry_id) SELECT kind,row_number() OVER(PARTITION BY kind ORDER BY entry_id)-1,entry_id FROM queue_entries CROSS JOIN (SELECT 0 kind UNION ALL SELECT 1) WHERE true ON CONFLICT(order_kind,position) DO UPDATE SET entry_id=excluded.entry_id")
        .execute(&mut raw).await.unwrap();
    for (seed, object_id) in [
        (RadioSeed::Playlist(native), Some("native")),
        (RadioSeed::Playlist(local), None),
        (RadioSeed::Playlist(owned), None),
        (RadioSeed::Track(fixture.track_uris[0].clone()), None),
        (RadioSeed::Album(fixture.albums[0]), Some("album-a")),
        (RadioSeed::Artist(fixture.artists[0]), Some("artist-a")),
        (RadioSeed::AlbumArtist(fixture.artists[0]), Some("artist-a")),
        (RadioSeed::Genre(fixture.genre), Some("genre")),
    ] {
        assert_eq!(
            fixture.database.radio_source(&seed, &cancel).await.unwrap(),
            Some((
                fixture.source,
                library::SourceId::new("source"),
                object_id.map(String::from)
            ))
        );
        let ids = ["track-2", "track-1", "track-2", "missing", "track-3"].map(String::from);
        assert_eq!(
            fixture
                .database
                .recommended_track_keys(fixture.source, &seed, &ids, &cancel)
                .await
                .unwrap(),
            fixture.tracks[2..]
        );
    }
    for seed in [
        RadioSeed::Playlist(native),
        RadioSeed::Playlist(local),
        RadioSeed::Track(fixture.track_uris[0].clone()),
        RadioSeed::Album(fixture.albums[0]),
    ] {
        assert!(
            fixture
                .database
                .recommended_track_keys(fixture.source, &seed, &["track-0".into()], &cancel)
                .await
                .unwrap()
                .is_empty()
        );
        let result = fixture
            .database
            .radio_candidates(fixture.source, seed, &[], 20, false, 0, &cancel)
            .await
            .unwrap();
        assert_eq!(result, fixture.tracks[2..]);
    }
}

#[tokio::test]
async fn radio_prefers_related_album_and_track_metadata_before_source_fill() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    // Candidate tracks retain only album genres; the seed retains only a track genre.
    for sql in ["DELETE FROM track_artists", "DELETE FROM album_artists"] {
        sqlx::query(sql).execute(&mut raw).await.unwrap();
    }
    sqlx::query("DELETE FROM album_genres WHERE album_key=?1")
        .bind(fixture.albums[0])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_genres WHERE track_key<>?1")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .database
            .radio_candidates(
                fixture.source,
                RadioSeed::Track(fixture.track_uris[0].clone()),
                &[],
                1,
                false,
                0,
                &cancel
            )
            .await
            .unwrap(),
        vec![fixture.tracks[2]]
    );
    // Album radio also recognizes the candidate's direct track genre.
    assert_eq!(
        fixture
            .database
            .radio_candidates(
                fixture.source,
                RadioSeed::Album(fixture.albums[1]),
                &[],
                1,
                false,
                2,
                &cancel
            )
            .await
            .unwrap(),
        vec![fixture.tracks[0]]
    );
}

#[tokio::test]
async fn smart_toolbar_sorts_and_filters_eleven_members_without_changing_definition_limits() {
    use library::CatalogSort;
    let fixture = fixture().await;
    let database = &fixture.database;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE sources SET kind='local' WHERE source_key=?1")
        .bind(fixture.source)
        .execute(&mut raw)
        .await
        .unwrap();
    let mut uris = Vec::new();
    for index in 0..11 {
        let uri = format!("test:toolbar:{:02}", 10 - index);
        sqlx::query("INSERT INTO tracks(source_key,object_id,media_uri,album_key,metadata,normalized_search,indexed) VALUES (?1,?2,?2,?3,jsonb(json_object('title',COALESCE(?4,''),'artist',COALESCE(?5,''),'album',COALESCE('Toolbar album',''),'sort_text',COALESCE(?4,''),'duration_millis',COALESCE(?6,0),'disc_number',COALESCE(1,0),'track_number',COALESCE(?7,0),'year',?8,'release_date','2026-01-01','date_added','2026-01-01','bpm',?9)),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE(?4,''),'artist',COALESCE(?5,''),'album',COALESCE('Toolbar album',''),'sort_text',COALESCE(?4,''),'duration_millis',COALESCE(?6,0),'disc_number',COALESCE(1,0),'track_number',COALESCE(?7,0),'year',?8,'release_date','2026-01-01','date_added','2026-01-01','bpm',?9)),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE(?4,''),'artist',COALESCE(?5,''),'album',COALESCE('Toolbar album',''),'sort_text',COALESCE(?4,''),'duration_millis',COALESCE(?6,0),'disc_number',COALESCE(1,0),'track_number',COALESCE(?7,0),'year',?8,'release_date','2026-01-01','date_added','2026-01-01','bpm',?9)),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE(?4,''),'artist',COALESCE(?5,''),'album',COALESCE('Toolbar album',''),'sort_text',COALESCE(?4,''),'duration_millis',COALESCE(?6,0),'disc_number',COALESCE(1,0),'track_number',COALESCE(?7,0),'year',?8,'release_date','2026-01-01','date_added','2026-01-01','bpm',?9)),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE(?4,''),'artist',COALESCE(?5,''),'album',COALESCE('Toolbar album',''),'sort_text',COALESCE(?4,''),'duration_millis',COALESCE(?6,0),'disc_number',COALESCE(1,0),'track_number',COALESCE(?7,0),'year',?8,'release_date','2026-01-01','date_added','2026-01-01','bpm',?9)),'$.comment'),'')),1)")
            .bind(fixture.source).bind(&uri).bind(fixture.albums[0])
            .bind(format!("Toolbar {index:02}{}", if index == 9 { " İstanbul" } else { "" }))
            .bind(format!("Artist {:02}", 10-index)).bind(1000+index).bind(index)
            .bind(2020+index).bind(100+index).bind(index*10).bind(index%2)
            .bind(format!("Toolbar {index:02}{} Artist {:02} Toolbar album {}", if index == 9 { " İstanbul" } else { "" },10-index,2020+index).to_lowercase())
            .execute(&mut raw).await.unwrap();
        sqlx::query("INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped) VALUES(?1,'Toolbar','Artist','Album',?2,'2026-01',1000,1000,0)")
            .bind(&uri).bind(100+index).execute(&mut raw).await.unwrap();
        sqlx::query("INSERT INTO native_media_state(media_uri,rating,favorite) VALUES(?1,?2,?3)")
            .bind(&uri)
            .bind(index * 10)
            .bind(index % 2)
            .execute(&mut raw)
            .await
            .unwrap();
        sqlx::query("UPDATE tracks SET normalized_search=?2 WHERE media_uri=?1")
            .bind(&uri)
            .bind(
                format!(
                    "Toolbar {index:02}{} Artist {:02} Toolbar album {}",
                    if index == 9 { " İstanbul" } else { "" },
                    10 - index,
                    2020 + index
                )
                .to_lowercase(),
            )
            .execute(&mut raw)
            .await
            .unwrap();
        uris.push(uri);
    }
    let playlist = database
        .create_playlist(
            None,
            "Toolbar order",
            &library::MemberSelection::Uris(((&uris).to_vec()).into()),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
    let mut definition = SmartPlaylistDefinition {
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::Title,
            operator: SmartPlaylistRuleOperator::Contains,
            value: Some(SmartPlaylistRuleValue::Text("Toolbar".into())),
        }],
        descending: true,
        ..SmartPlaylistDefinition::default()
    };
    let key = database
        .create_smart_playlist("Toolbar", &definition)
        .await
        .unwrap();
    let members = database
        .selected_track_uris(
            &library::MemberRead::new(library::Collection::Smart { key: key, now: 500 })
                .selection(),
            None,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(members, uris.iter().rev().cloned().collect::<Vec<_>>());
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
        let ascending = (&library::MemberRead {
            collection: library::Collection::Smart { key: key, now: 500 },
            sort: Some(sort),
            ..library::MemberRead::new(library::Collection::Smart { key: key, now: 500 })
        })
            .rows(&database, 0, 64, &cancel)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(library::MemberRow::into_track)
                    .collect::<Vec<_>>()
            })
            .unwrap()
            .into_iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>();
        let descending = (&library::MemberRead {
            collection: library::Collection::Smart { key: key, now: 500 },
            sort: Some(sort),
            descending: true,
            ..library::MemberRead::new(library::Collection::Smart { key: key, now: 500 })
        })
            .rows(&database, 0, 64, &cancel)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(library::MemberRow::into_track)
                    .collect::<Vec<_>>()
            })
            .unwrap()
            .into_iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>();
        for (reverse, expected) in [(false, &ascending), (true, &descending)] {
            let tracks = (&library::MemberRead {
                collection: library::Collection::Source(fixture.source),
                sort: Some(sort),
                filter: ("Toolbar").into(),
                descending: reverse,
                ..library::MemberRead::new(library::Collection::Source(fixture.source))
            })
                .rows(&database, 0, 64, &cancel)
                .await
                .map(|rows| {
                    rows.into_iter()
                        .map(library::MemberRow::into_track)
                        .collect::<Vec<_>>()
                })
                .unwrap()
                .into_iter()
                .map(|row| row.media_uri.clone())
                .collect::<Vec<_>>();
            let entries = (&library::MemberRead {
                collection: library::Collection::Playlist(playlist),
                sort: Some(sort),
                descending: reverse,
                ..library::MemberRead::new(library::Collection::Playlist(playlist))
            })
                .rows(&database, 0, 64, &cancel)
                .await
                .map(|rows| {
                    rows.into_iter()
                        .filter_map(library::MemberRow::into_entry)
                        .collect::<Vec<_>>()
                })
                .unwrap()
                .into_iter()
                .map(|row| row.track.media_uri)
                .collect::<Vec<_>>();
            assert_eq!(
                &tracks, expected,
                "tracks and smart: {sort:?}, descending={reverse}"
            );
            assert_eq!(
                &entries, expected,
                "saved and smart: {sort:?}, descending={reverse}"
            );
        }
        assert_eq!(ascending.len(), 11, "{sort:?}");
        let expected_descending = match sort {
            CatalogSort::AlbumArtist
            | CatalogSort::Album
            | CatalogSort::ReleaseDate
            | CatalogSort::DateAdded
            | CatalogSort::PlayCount
            | CatalogSort::Genre => ascending.clone(),
            CatalogSort::Favorite => (0..11)
                .filter(|index| index % 2 == 1)
                .chain((0..11).filter(|index| index % 2 == 0))
                .map(|index| uris[index].clone())
                .collect(),
            _ => ascending.iter().rev().cloned().collect(),
        };
        assert_eq!(descending, expected_descending, "{sort:?}");
        if matches!(
            sort,
            CatalogSort::Title
                | CatalogSort::TrackNumber
                | CatalogSort::Year
                | CatalogSort::Bpm
                | CatalogSort::Duration
                | CatalogSort::UserRating
                | CatalogSort::LastPlayed
        ) {
            assert_eq!(ascending, uris, "{sort:?}");
        }
        if sort == CatalogSort::Artist {
            assert_eq!(ascending, members);
        }
    }
    for (query, expected) in [
        ("toolbar 09", vec![uris[9].clone()]),
        ("i\u{307}st", vec![uris[9].clone()]),
        ("202", uris[..10].to_vec()),
        ("no match", vec![]),
        ("", uris.clone()),
    ] {
        let order = (&library::MemberRead {
            collection: library::Collection::Smart { key: key, now: 500 },
            sort: Some(CatalogSort::Title),
            filter: (query).into(),
            ..library::MemberRead::new(library::Collection::Smart { key: key, now: 500 })
        })
            .rows(&database, 0, 64, &cancel)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(library::MemberRow::into_track)
                    .collect::<Vec<_>>()
            })
            .unwrap()
            .into_iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>();
        assert_eq!(order, expected, "{query}");
    }
    let rows = database
        .tracks_by_uris(&((&uris).to_vec()), &cancel)
        .await
        .unwrap();
    assert!(rows.iter().all(|row| row.links.album_media_uri.as_ref()
        == Some(&fixture.album_uris[0])
        && !row.links.album_artists.is_empty()));
    definition.limit = Some(3);
    database
        .update_smart_playlist(key, "Toolbar", &definition, None)
        .await
        .unwrap();
    let limited = database
        .selected_track_uris(
            &library::MemberRead::new(library::Collection::Smart { key: key, now: 500 })
                .selection(),
            None,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(limited, members[..3]);
    let order = (&library::MemberRead {
        collection: library::Collection::Smart { key: key, now: 500 },
        sort: Some(CatalogSort::TrackNumber),
        ..library::MemberRead::new(library::Collection::Smart { key: key, now: 500 })
    })
        .rows(&database, 0, 64, &cancel)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .unwrap()
        .into_iter()
        .map(|row| row.media_uri.clone())
        .collect::<Vec<_>>();
    assert_eq!(order, uris[8..]);
    definition.limit = None;
    database
        .update_smart_playlist(key, "Toolbar", &definition, None)
        .await
        .unwrap();
    sqlx::query("DELETE FROM listens WHERE media_uri=?1")
        .bind(&uris[0])
        .execute(&mut raw)
        .await
        .unwrap();
    for descending in [false, true] {
        let order = (&library::MemberRead {
            collection: library::Collection::Smart { key: key, now: 500 },
            sort: Some(CatalogSort::LastPlayed),
            descending: descending,
            ..library::MemberRead::new(library::Collection::Smart { key: key, now: 500 })
        })
            .rows(&database, 0, 64, &cancel)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(library::MemberRow::into_track)
                    .collect::<Vec<_>>()
            })
            .unwrap()
            .into_iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>();
        assert_eq!(order.last(), Some(&uris[0]));
    }
    assert_eq!(
        database.tracks_by_uris(&uris[..1], &cancel).await.unwrap()[0]
            .state
            .last_played,
        None
    );
    sqlx::query("UPDATE tracks SET metadata=jsonb(json_set(metadata,'$.title','Toolbar Intro','$.sort_text','toolbar intro')),normalized_search='toolbar intro' WHERE media_uri IN (?1,?2)")
        .bind(&uris[0]).bind(&uris[1]).execute(&mut raw).await.unwrap();
    let tracks = (&library::MemberRead {
        collection: library::Collection::Source(fixture.source),
        sort: Some(CatalogSort::Title),
        filter: ("Toolbar Intro").into(),
        ..library::MemberRead::new(library::Collection::Source(fixture.source))
    })
        .rows(&database, 0, 64, &cancel)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .unwrap()
        .into_iter()
        .map(|row| row.media_uri.clone())
        .collect::<Vec<_>>();
    let entries = (&library::MemberRead {
        collection: library::Collection::Playlist(playlist),
        sort: Some(CatalogSort::Title),
        filter: ("Toolbar Intro").into(),
        ..library::MemberRead::new(library::Collection::Playlist(playlist))
    })
        .rows(&database, 0, 64, &cancel)
        .await
        .map(|rows| {
            rows.into_iter()
                .filter_map(library::MemberRow::into_entry)
                .collect::<Vec<_>>()
        })
        .unwrap()
        .into_iter()
        .map(|row| row.track.media_uri)
        .collect::<Vec<_>>();
    let smart = (&library::MemberRead {
        collection: library::Collection::Smart { key: key, now: 500 },
        sort: Some(CatalogSort::Title),
        filter: ("Toolbar Intro").into(),
        ..library::MemberRead::new(library::Collection::Smart { key: key, now: 500 })
    })
        .rows(&database, 0, 64, &cancel)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .unwrap()
        .into_iter()
        .map(|row| row.media_uri.clone())
        .collect::<Vec<_>>();
    assert_eq!(tracks, uris[..2]);
    assert_eq!(entries, tracks);
    assert_eq!(smart, tracks);
}

#[tokio::test]
async fn playlist_and_smart_rows_retain_exact_metadata_links_and_unavailable_entries() {
    let fixture = fixture().await;
    let database = &fixture.database;
    let cancel = ReadCancellation::new();
    let local_uri = "file:///music/local.flac";
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('local','Local','local',zeroblob(32))")
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tracks(source_key,object_id,media_uri,first_seen_at,metadata,normalized_search,indexed) SELECT source_key,'local-track',?1,86400,jsonb(json_object('title',COALESCE('Local Track',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('local track',''),'duration_millis',COALESCE(60000,0))),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE('Local Track',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('local track',''),'duration_millis',COALESCE(60000,0))),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Local Track',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('local track',''),'duration_millis',COALESCE(60000,0))),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Local Track',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('local track',''),'duration_millis',COALESCE(60000,0))),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Local Track',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('local track',''),'duration_millis',COALESCE(60000,0))),'$.comment'),'')),1 FROM sources WHERE object_id='local'")
        .bind(local_uri)
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    let uris = vec![
        fixture.track_uris[0].clone(),
        "https://example.test/direct.flac".into(),
        fixture.track_uris[0].clone(),
        local_uri.into(),
    ];
    let playlist = database
        .create_playlist(
            None,
            "Links",
            &library::MemberSelection::Uris(((&uris).to_vec()).into()),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
    let order = database
        .playlist_edit_entries(
            playlist,
            &library::MemberRead {
                collection: library::Collection::Playlist(playlist),
                sort: Some(library::CatalogSort::Position),
                ..library::MemberRead::new(library::Collection::Playlist(playlist))
            }
            .selection(),
            &cancel,
        )
        .await
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| entry.playlist_entry_key)
                .collect::<Vec<_>>()
        })
        .unwrap();
    let entries = database
        .playlist_entries(&((&order).to_vec()), &cancel)
        .await
        .unwrap();
    let smart = database
        .tracks_by_uris(&((&uris).to_vec()), &cancel)
        .await
        .unwrap();
    let captured = super::support::resolve_queue(
        database,
        library::QueueLoad::media_uris(
            uris.clone(),
            library::QueueProvenance::Context {
                context_id: "metadata-links".into(),
                context_title: None,
                source_rank: 0,
            },
        ),
        0,
    )
    .await;
    let queued = &captured.rows;
    assert_eq!(entries.len(), 4);
    assert_eq!(smart.len(), 4);
    assert_eq!(queued.len(), 4);
    for index in [0, 2] {
        assert_eq!(entries[index].track.media_uri, uris[index]);
        assert_eq!(entries[index].track.source_id.as_deref(), Some("source"));
        assert_eq!(
            entries[index].track.links.album_media_uri.as_ref(),
            Some(&fixture.album_uris[0])
        );
        assert_eq!(
            entries[index].track.links.artists[0].media_uri,
            fixture.artist_uris[0]
        );
        assert_eq!(
            entries[index].track.links.album_artists[0].media_uri,
            fixture.artist_uris[0]
        );
        assert_eq!(
            smart[index].links.album_media_uri,
            entries[index].track.links.album_media_uri
        );
        assert_eq!(
            smart[index].links.artists,
            entries[index].track.links.artists
        );
        assert_eq!(
            smart[index].links.album_artists,
            entries[index].track.links.album_artists
        );
        assert_eq!(
            queued[index]
                .track
                .links
                .artists
                .first()
                .map(|artist| &artist.media_uri),
            Some(&fixture.artist_uris[0])
        );
    }
    assert_eq!(entries[1].track.media_uri, uris[1]);
    assert!(entries[1].track.source_id.is_none());
    assert!(entries[1].track.links.album_media_uri.is_none());
    assert!(entries[1].track.links.artists.is_empty());
    assert!(entries[1].track.links.album_artists.is_empty());
    assert!(smart[1].links.album_media_uri.is_none());
    assert!(smart[1].links.artists.is_empty());
    assert!(smart[1].links.album_artists.is_empty());
    assert!(queued[1].track.links.artists.is_empty());
    assert_eq!(entries[3].track.media_uri, local_uri);
    assert_eq!(entries[3].track.source_id.as_deref(), Some("local"));
    assert_eq!(
        entries[3].track.metadata.date_added.as_deref(),
        Some("1970-01-02")
    );
    assert_eq!(
        smart[3].metadata.date_added,
        entries[3].track.metadata.date_added
    );
    assert!(entries[1].track.metadata.date_added.is_none());
    let tracks = database
        .tracks_by_uris(&[local_uri.to_owned()], &cancel)
        .await
        .unwrap();
    assert_eq!(
        tracks[0].metadata.date_added,
        entries[3].track.metadata.date_added
    );
    let added = database
        .create_smart_playlist(
            "Added",
            &SmartPlaylistDefinition {
                source_id: None,
                match_all: vec![SmartPlaylistRule {
                    field: SmartPlaylistRuleField::DateAdded,
                    operator: SmartPlaylistRuleOperator::Equals,
                    value: Some(SmartPlaylistRuleValue::Date("1970-01-02".into())),
                }],
                ..SmartPlaylistDefinition::default()
            },
        )
        .await
        .unwrap();
    let dated = database
        .selected_track_uris(
            &library::MemberRead::new(library::Collection::Smart {
                key: added,
                now: 86400,
            })
            .selection(),
            None,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(dated, [local_uri.to_owned()]);
}

#[tokio::test]
async fn uri_artwork_is_identical_across_owner_projections_and_current_scopes() {
    let fixture = fixture().await;
    let database = &fixture.database;
    let cancel = ReadCancellation::new();
    let uri = &fixture.track_uris[0];
    let global = database
        .create_playlist(
            None,
            "Global artwork",
            &library::MemberSelection::Uris(((&[uri.clone(), uri.clone()]).to_vec()).into()),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
    let current = database
        .create_playlist(
            Some(fixture.source),
            "Current artwork",
            &library::MemberSelection::Uris(((std::slice::from_ref(uri)).to_vec()).into()),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
    let smart = database
        .create_smart_playlist(
            "All artwork",
            &SmartPlaylistDefinition {
                source_id: None,
                match_all: vec![SmartPlaylistRule {
                    field: SmartPlaylistRuleField::Title,
                    operator: SmartPlaylistRuleOperator::Equals,
                    value: Some(SmartPlaylistRuleValue::Text("Alpha".into())),
                }],
                ..SmartPlaylistDefinition::default()
            },
        )
        .await
        .unwrap();
    let mut raw = connection(&fixture.path).await;
    let other: library::SourceKey = sqlx::query_scalar(
        "INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('other','Other','other',zeroblob(32)) RETURNING source_key",
    ).fetch_one(&mut raw).await.unwrap();
    let empty_folder: library::FolderKey = sqlx::query_scalar(
        "INSERT INTO folders(source_key,object_id,metadata,media_uri,normalized_name) VALUES (?1,'empty',jsonb(json_object('name','Empty','sort_text','empty')),'folder:'||COALESCE(?1,0)||':'||'empty',lower(COALESCE(json_extract(jsonb(json_object('name','Empty','sort_text','empty')),'$.name'),''))) RETURNING folder_key",
    ).bind(other).fetch_one(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO listens(source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped) VALUES('source',?1,'Historical title','Artist','Album',100,'1970-01',42000,42000,0)")
        .bind(uri).execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO queue_entries(track_key,provenance) SELECT track_key,'\"Manual\"' FROM tracks WHERE media_uri=?1")
        .bind(uri)
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO queue_positions(order_kind,position,entry_id) SELECT kind,row_number() OVER(PARTITION BY kind ORDER BY entry_id)-1,entry_id FROM queue_entries CROSS JOIN (SELECT 0 kind UNION ALL SELECT 1) WHERE true ON CONFLICT(order_kind,position) DO UPDATE SET entry_id=excluded.entry_id")
        .execute(&mut raw).await.unwrap();

    // Neither a different Current nor its folder changes A(uri). A later catalog
    // revision, including removal of the binding, is observed by every owner.
    let revisions = ["first", "revised"].map(|tag| {
        Some(
            serde_json::to_vec(&[library::ArtworkCandidate::new(
                library::ArtworkImage::Native {
                    source_id: library::SourceId::new("source"),
                    item_id: "cover".into(),
                    tag: Some(tag.into()),
                },
                None,
            )])
            .unwrap(),
        )
    });
    for binding in revisions.into_iter().chain([None]) {
        {
            let objects =
                sqlx::query_scalar::<_, String>("SELECT object_id FROM tracks WHERE media_uri=?1")
                    .bind(uri)
                    .fetch_all(&mut raw)
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
                        .map(|id| (id, binding.clone()))
                        .collect::<Vec<_>>(),
                )
                .await
                .unwrap();
        };
        let expected_binding = if binding.is_some() {
            binding.clone()
        } else {
            let view = {
                let mut rows = database
                    .tracks_by_uris(std::slice::from_ref(&uri), &cancel)
                    .await
                    .unwrap();
                rows.pop().unwrap()
            };
            if let Some(art) = &view.artwork_binding {
                let candidates: Vec<library::ArtworkCandidate> =
                    serde_json::from_slice(art).unwrap();
                assert!(candidates.iter().all(|candidate| matches!(
                    candidate.image,
                    library::ArtworkImage::Artist { .. }
                )));
            }
            view.artwork_binding
        };
        let expected_sample: Vec<Vec<u8>> = expected_binding.clone().into_iter().collect();
        for (source, folder) in [
            (None, None),
            (Some(fixture.source), Some(fixture.folder)),
            (Some(other), Some(empty_folder)),
        ] {
            for sort in [
                library::CatalogSort::Position,
                library::CatalogSort::Title,
                library::CatalogSort::TrackCount,
                library::CatalogSort::Duration,
            ] {
                let (count, _, rows) = library::ListRead {
                    playlist_scope: Default::default(),
                    kind: library::ListKind::Playlists,
                    source: (source).into(),
                    sort: sort,
                    folder: folder,
                    filter: ("").to_string(),
                    descending: false,
                    favorites_only: false,
                    downloaded_only: false,
                    activity: true,
                }
                .page_playlists(&database, RouteSeedWindow::top(), &cancel)
                .await
                .map(|page| (page.count, page.first_row_position, page.rows))
                .unwrap();
                let order = rows.iter().map(|row| row.playlist_key).collect::<Vec<_>>();
                assert_eq!(count, order.len());
                assert!(order.contains(&global));
                let row = rows.iter().find(|row| row.playlist_key == global).unwrap();
                assert_eq!(row.track_count, 2);
                assert!(row.artwork_binding.is_some());
            }
            let detail = library::MemberRead {
                folder,
                ..library::MemberRead::new(library::Collection::Playlist(global))
            }
            .page(&database, RouteSeedWindow::top(), &cancel)
            .await
            .unwrap();
            assert_eq!(detail.count, 2);
            let entries = detail
                .rows
                .into_iter()
                .map(|row| row.into_entry().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(entries.len(), 2);
            for row in &entries {
                assert_eq!(&row.track.media_uri, uri);
                assert_eq!(row.track.artwork_binding, expected_binding);
            }
            let summary = database
                .smart_playlist_rows(&[smart], 500, &cancel)
                .await
                .unwrap()
                .pop()
                .unwrap();
            let rows = (&library::MemberRead {
                collection: library::Collection::Smart {
                    key: smart,
                    now: 500,
                },
                sort: Some(library::CatalogSort::Title),
                folder: folder,
                ..library::MemberRead::new(library::Collection::Smart {
                    key: smart,
                    now: 500,
                })
            })
                .rows(&database, 0, 64, &cancel)
                .await
                .map(|rows| {
                    rows.into_iter()
                        .map(library::MemberRow::into_track)
                        .collect::<Vec<_>>()
                })
                .unwrap();
            let row = rows.iter().find(|row| &row.media_uri == uri).unwrap();
            assert_eq!(row.artwork_binding, expected_binding);
            let candidate = serde_json::from_slice::<Vec<library::ArtworkCandidate>>(
                &summary.artwork_binding.unwrap(),
            )
            .unwrap()
            .remove(0);
            let candidate = database
                .resolve_playlist_artwork(candidate)
                .await
                .unwrap()
                .unwrap();
            let library::ArtworkImage::Playlist {
                resolved: Some(recipes),
                ..
            } = candidate.image
            else {
                panic!("automatic playlist image")
            };
            assert_eq!(
                recipes
                    .iter()
                    .map(|recipe| serde_json::to_vec(recipe).unwrap())
                    .collect::<Vec<_>>(),
                expected_sample
            );
        }
        let current_rows = database.playlists(&[current], &cancel).await.unwrap();
        assert!(
            library::authored_playlist_artwork(current_rows[0].artwork_binding.as_ref().unwrap())
                .unwrap()
                .is_none()
        );
        let preview = database
            .track_artwork_bindings(
                &[
                    uri.clone(),
                    "https://example.test/no-catalog.flac".into(),
                    uri.clone(),
                ],
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(
            preview,
            [expected_sample.clone(), expected_sample.clone()].concat()
        );
        assert!(
            database
                .track_artwork_bindings(&["https://example.test/no-catalog.flac".into()], &cancel)
                .await
                .unwrap()
                .is_empty(),
            "an unavailable URI never borrows an unrelated catalog cover"
        );
        let catalog = database
            .tracks(&[fixture.tracks[0]], &cancel)
            .await
            .unwrap();
        assert_eq!(catalog[0].artwork_binding, expected_binding);
        let smart_rows = database
            .tracks_by_uris(std::slice::from_ref(uri), &cancel)
            .await
            .unwrap();
        assert_eq!(smart_rows[0].artwork_binding, expected_binding);
        let queue = database.restore_queue().await.unwrap().rows;
        assert_eq!(queue[0].track.artwork_binding, expected_binding);
        let restored = database.restore_queue().await.unwrap();
        assert_eq!(restored.rows[0].track.artwork_binding, expected_binding);
        let history = (&library::MemberRead {
            descending: true,
            ..library::MemberRead::new(library::Collection::History { source: None })
        })
            .rows(&database, 0, 100, &cancel)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(library::MemberRow::into_track)
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(history[0].artwork_binding, expected_binding);
        let scoped_samples = database
            .representative_artwork_page(
                fixture.source,
                Some(fixture.folder),
                library::RepresentativeArtworkScope::PlaylistTracks,
                4,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(scoped_samples, expected_sample);
    }
    let scoped = database
        .playlist_edit_entries(
            current,
            &library::MemberRead {
                collection: library::Collection::Playlist(current),
                sort: Some(library::CatalogSort::Position),
                folder: Some(empty_folder),
                ..library::MemberRead::new(library::Collection::Playlist(current))
            }
            .selection(),
            &cancel,
        )
        .await
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| entry.playlist_entry_key)
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert!(
        scoped.is_empty(),
        "folder scope still selects Current membership"
    );
    let all_entries = database
        .playlist_edit_entries(
            current,
            &library::MemberRead {
                collection: library::Collection::Playlist(current),
                sort: Some(library::CatalogSort::Position),
                ..library::MemberRead::new(library::Collection::Playlist(current))
            }
            .selection(),
            &cancel,
        )
        .await
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| entry.playlist_entry_key)
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert_eq!(
        database
            .playlist_entries(&((&all_entries).to_vec()), &cancel)
            .await
            .unwrap()
            .len(),
        1,
        "a known occurrence is projected without Current/folder permission"
    );
}

#[tokio::test]
async fn downloaded_row_facts_require_download_owned_local_access() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let rows = fixture
        .database
        .tracks(&fixture.tracks[..2], &cancel)
        .await
        .expect("initial Track rows");
    assert!(rows.iter().all(|row| !row.access.is_downloaded));

    for (index, origin) in [LocalAccessOrigin::Mapping, LocalAccessOrigin::Download]
        .into_iter()
        .enumerate()
    {
        let track = &rows[index];
        fixture
            .database
            .write_local_access(
                Some(fixture.source),
                &[&LocalAccessWrite {
                    media_uri: track.media_uri.clone(),
                    origin,
                    path: format!(
                        "/validated/{}.flac",
                        track.object_id.as_deref().expect("indexed track object")
                    ),
                    root: "/validated".to_string(),
                    relative_path: format!(
                        "{}.flac",
                        track.object_id.as_deref().expect("indexed track object")
                    ),
                    size_bytes: 10,
                    mtime_ns: 1,
                    device_id: Some(1),
                    inode: Some(index as i64 + 1),
                    parser_version: 1,
                    access_uri: format!(
                        "file:///validated/{}.flac",
                        track.object_id.as_deref().expect("indexed track object")
                    ),
                    loudness_analysis_key: Some(track.access.loudness_analysis_key),
                }],
            )
            .await
            .expect("write accepted Local access provenance");
    }
    let rows = fixture
        .database
        .tracks(&fixture.tracks[..2], &cancel)
        .await
        .expect("provenance Track rows");
    assert!(
        !rows[0].access.is_downloaded,
        "configured mapping is not a Download"
    );
    assert!(
        rows[1].access.is_downloaded,
        "Download-owned artifact is downloaded"
    );

    let album = fixture
        .database
        .albums(&[fixture.albums[0]], true, &cancel)
        .await
        .expect("Album download aggregate")
        .pop()
        .expect("Album row");
    assert_eq!(album.summary.track_count, 2);
    assert_eq!(album.summary.downloaded_count, 1);
    let artist = fixture
        .database
        .artists(&[fixture.artists[0]], false, true, &cancel)
        .await
        .expect("Artist download aggregate")
        .pop()
        .expect("Artist row");
    assert_eq!(artist.summary.downloaded_count, 1);
    let genre = fixture
        .database
        .genres(&[fixture.genre], None, &cancel)
        .await
        .expect("Genre download aggregate")
        .pop()
        .expect("Genre row");
    assert_eq!(genre.summary.downloaded_count, 1);
    let mood = fixture
        .database
        .moods(&[fixture.mood], None, &cancel)
        .await
        .expect("Mood download aggregate")
        .pop()
        .expect("Mood row");
    assert_eq!(mood.summary.downloaded_count, 1);
}

#[tokio::test]
async fn collection_rows_carry_their_own_stable_source_identity() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let authored = fixture
        .database
        .create_playlist(
            Some(fixture.source),
            "Authored",
            &library::MemberSelection::Uris(((&[]).to_vec()).into()),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
    let mut raw = connection(&fixture.path).await;
    sqlx::raw_sql("INSERT INTO sources(source_key,object_id,display_name,normalized_name,artwork_digest) VALUES (55,'other-source','Other','other',zeroblob(32)),(56,'absent-source','','',zeroblob(32));\n        INSERT INTO genres(genre_key,source_key,object_id,metadata,media_uri,normalized_name) VALUES (55,55,'genre',jsonb(json_object('name','Other genre','sort_text','other genre')),'genre:'||COALESCE(55,0)||':'||'genre',lower(COALESCE(json_extract(jsonb(json_object('name','Other genre','sort_text','other genre')),'$.name'),'')));\n        INSERT INTO playlists(playlist_key,source_key,owner,object_id,name,position) VALUES(55,55,'source','native','Native',55),(56,56,'rufin','offline-authored','Offline authored',56),(57,NULL,'rufin','global','Global',57);")
        .execute(&mut raw).await.unwrap();

    let other = library::SourceKey::from_raw(55);
    let genre = library::GenreKey::from_raw(55);
    let row = fixture
        .database
        .genres(&[genre], None, &cancel)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(row.source_id, "other-source");
    let detail = fixture
        .database
        .genres(&[genre], None, &cancel)
        .await
        .map(|rows| {
            let mut rows = rows;
            rows.pop()
        })
        .unwrap()
        .unwrap();
    assert_eq!(detail.source_id, row.source_id);
    let original = fixture
        .database
        .genres(&[fixture.genre], None, &cancel)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(original.source_id, "source");

    let keys = [
        library::PlaylistKey::from_raw(55),
        library::PlaylistKey::from_raw(56),
        library::PlaylistKey::from_raw(57),
        authored,
    ];
    let rows = fixture.database.playlists(&keys, &cancel).await.unwrap();
    assert_eq!(
        rows.iter().map(|row| row.playlist_key).collect::<Vec<_>>(),
        keys
    );
    assert_eq!(
        rows.iter()
            .map(|row| row.source_id.as_deref())
            .collect::<Vec<_>>(),
        [
            Some("other-source"),
            Some("absent-source"),
            None,
            Some("source")
        ]
    );
    assert_eq!(rows[0].source_key, Some(other));
    assert_eq!(rows[1].source_key, Some(library::SourceKey::from_raw(56)));
    assert_eq!(rows[2].source_key, None);
    assert_eq!(rows[3].source_key, Some(fixture.source));
    for row in &rows {
        let source = row
            .source_id
            .as_ref()
            .map(|id| library::SourceId::new(id.clone()));
        assert_eq!(
            fixture
                .database
                .playlist_key_by_identity(source.as_ref(), &row.object_id, &cancel)
                .await
                .unwrap(),
            Some(row.playlist_key)
        );
    }
    assert_eq!(
        fixture
            .database
            .playlist_key_by_identity(None, "offline-authored", &cancel)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        fixture
            .database
            .playlist_key_by_identity(
                Some(&library::SourceId::new("missing-source")),
                "global",
                &cancel,
            )
            .await
            .unwrap(),
        None
    );
    sqlx::raw_sql("INSERT INTO tracks(track_key,media_uri,indexed,metadata,normalized_search) VALUES (55,'https://example.test/unavailable',0,jsonb(json_object('title',COALESCE('Known',''))),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE('Known',''))),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Known',''))),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Known',''))),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Known',''))),'$.comment'),'')));\n        INSERT INTO playlist_entries(playlist_entry_key,playlist_key,object_id,track_key,position) VALUES\n        (550,55,'first',55,0),(551,55,'second',55,1),(560,56,'first',55,0),(561,56,'second',55,1);")
        .execute(&mut raw).await.unwrap();
    for (key, entry_keys) in [(keys[0], [550, 551]), (keys[1], [560, 561])] {
        let collection = library::Collection::Playlist(key);
        assert_eq!(
            fixture
                .database
                .selected_track_uris(
                    &library::MemberRead::new(collection.clone()).selection(),
                    None,
                    &cancel
                )
                .await
                .unwrap(),
            [
                "https://example.test/unavailable",
                "https://example.test/unavailable"
            ]
        );
        for input in [
            library::QueueLoad::Tracks(vec![(
                library::MemberRead::new(collection).selection(),
                library::QueueProvenance::Context {
                    context_id: "playlist".into(),
                    context_title: None,
                    source_rank: 0,
                },
            )]),
            library::QueueLoad::Tracks(vec![(
                library::MemberSelection::Entries(
                    entry_keys
                        .map(library::PlaylistEntryKey::from_raw)
                        .to_vec()
                        .into(),
                ),
                library::QueueProvenance::Context {
                    context_id: "selected entries".into(),
                    context_title: None,
                    source_rank: 0,
                },
            )]),
        ] {
            let captured = super::support::resolve_queue(&fixture.database, input, 0).await;
            assert_eq!(captured.entries.len(), 2);
            assert!(
                captured
                    .entries
                    .iter()
                    .all(|entry| entry.track_key == library::TrackKey::from_raw(55))
            );
            for (rank, entry) in captured.entries.iter().enumerate() {
                assert!(
                    matches!(&entry.provenance,library::QueueProvenance::Context {source_rank,..} if *source_rank==rank)
                );
            }
            assert_eq!(
                captured
                    .rows
                    .iter()
                    .map(|row| row.track.metadata.title.as_str())
                    .collect::<Vec<_>>(),
                ["Known", "Known"]
            );
        }
    }
}

#[tokio::test]
async fn genre_rows_prepare_one_cover_per_member_album() {
    let fixture = fixture().await;
    let covers = (0..2)
        .map(|index| {
            serde_json::to_vec(&[library::ArtworkCandidate::new(
                library::ArtworkImage::Native {
                    source_id: library::SourceId::new("source"),
                    item_id: format!("genre-album-{index}"),
                    tag: None,
                },
                None,
            )])
            .unwrap()
        })
        .collect::<Vec<_>>();
    let mut raw = connection(&fixture.path).await;
    for (index, album) in fixture.albums.iter().copied().enumerate() {
        {
            let objects =
                sqlx::query_scalar::<_, String>("SELECT object_id FROM albums WHERE album_key=?1")
                    .bind(album)
                    .fetch_all(&mut raw)
                    .await
                    .unwrap();
            fixture
                .database
                .catalog("source", None)
                .await
                .unwrap()
                .set_source_artwork(
                    "album",
                    &objects
                        .into_iter()
                        .map(|id| (id, Some(covers[index].clone())))
                        .collect::<Vec<_>>(),
                )
                .await
                .unwrap();
        };
    }
    drop(raw);

    let row = fixture
        .database
        .genres(&[fixture.genre], None, &ReadCancellation::new())
        .await
        .expect("read Genre row")
        .pop()
        .expect("Genre row");

    assert_eq!(row.representative_artwork.len(), 2);
}

#[tokio::test]
async fn genre_routes_reject_an_artwork_only_identity_without_track_membership() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    let orphan = sqlx::query_scalar::<_, library::GenreKey>(
        "INSERT INTO genres(source_key,object_id,native_artwork,metadata,media_uri,normalized_name) VALUES (?1,'orphan-genre',x'01',jsonb(json_object('name','Orphan Genre','sort_text','orphan genre')),'genre:'||COALESCE(?1,0)||':'||'orphan-genre',lower(COALESCE(json_extract(jsonb(json_object('name','Orphan Genre','sort_text','orphan genre')),'$.name'),''))) RETURNING genre_key",
    )
    .bind(fixture.source)
    .fetch_one(&mut raw)
    .await
    .expect("insert artwork-only Genre");
    drop(raw);

    for sort in [CatalogSort::Title, CatalogSort::TrackCount] {
        let (count, _, rows) = library::ListRead {
            playlist_scope: Default::default(),
            kind: library::ListKind::Genres,
            source: Some(fixture.source),
            sort: sort,
            folder: None,
            filter: String::new(),
            descending: false,
            favorites_only: false,
            downloaded_only: false,
            activity: true,
        }
        .page_genres(
            &fixture.database,
            RouteSeedWindow::top(),
            &ReadCancellation::new(),
        )
        .await
        .map(|page| (page.count, page.first_row_position, page.rows))
        .expect("Genre route");
        let order = rows.iter().map(|row| row.genre_key).collect::<Vec<_>>();
        assert_eq!(count, order.len());
        assert!(!order.contains(&orphan));
    }
}

#[tokio::test]
async fn artist_play_order_stays_complete_beside_the_favorite_section() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    for track in &fixture.tracks {
        let media_uri =
            fixture.track_uris[fixture.tracks.iter().position(|key| key == track).unwrap()].clone();
        fixture
            .database
            .set_favorite(&library::FavoriteTarget::Track(media_uri), false)
            .await
            .unwrap();
    }
    fixture
        .database
        .set_favorite(
            &library::FavoriteTarget::Track(fixture.track_uris[0].clone()),
            true,
        )
        .await
        .unwrap();
    let complete = (((&library::MemberRead {
        collection: library::Collection::Artist {
            key: fixture.artists[0],
            album_artist: false,
        },
        sort: Some(library::CatalogSort::Title),
        ..library::MemberRead::new(library::Collection::Artist {
            key: fixture.artists[0],
            album_artist: false,
        })
    })
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .unwrap()
        .rows)
        .into_iter()
        .map(library::MemberRow::into_track)
        .collect::<Vec<_>>())
    .into_iter()
    .map(|row| row.media_uri.clone())
    .collect::<Vec<_>>();
    let favorites = (((&library::MemberRead {
        collection: library::Collection::Artist {
            key: fixture.artists[0],
            album_artist: false,
        },
        sort: Some(library::CatalogSort::Title),
        favorites_only: true,
        ..library::MemberRead::new(library::Collection::Artist {
            key: fixture.artists[0],
            album_artist: false,
        })
    })
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .unwrap()
        .rows)
        .into_iter()
        .map(library::MemberRow::into_track)
        .collect::<Vec<_>>())
    .into_iter()
    .map(|row| row.media_uri.clone())
    .collect::<Vec<_>>();
    assert_eq!(favorites, [fixture.track_uris[0].clone()]);
    assert!(complete.len() > favorites.len());
}

#[tokio::test]
async fn track_artist_and_album_artist_roles_keep_exact_membership_and_own_artwork() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    let album_owner_uri =
        library::source_entity_uri(&library::SourceId::new("source"), "artist", "album-owner");
    let album_owner = sqlx::query_scalar::<_, library::ArtistKey>(
        "INSERT INTO artists(source_key,object_id,media_uri,native_artwork,metadata,normalized_name) VALUES (?1,'album-owner',?2,?3,jsonb(json_object('name','Album Owner','sort_text','album owner')),lower(COALESCE(json_extract(jsonb(json_object('name','Album Owner','sort_text','album owner')),'$.name'),'')))\n         RETURNING artist_key",
    )
    .bind(fixture.source)
    .bind(album_owner_uri)
    .bind(br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/f19f105616fb750567aaaa048c3e6b98","image":{"kind":"native","source_id":"source","item_id":"album-artist-art","tag":null},"blur_hash":null}]"#.as_slice())
    .fetch_one(&mut raw)
    .await
    .expect("insert distinct Album Artist");
    sqlx::query("DELETE FROM album_artists WHERE album_key=?1")
        .bind(fixture.albums[0])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO album_artists(album_key,artist_key,position) VALUES(?1,?2,0)")
        .bind(fixture.albums[0])
        .bind(album_owner)
        .execute(&mut raw)
        .await
        .unwrap();
    {
        let objects =
            sqlx::query_scalar::<_, String>("SELECT object_id FROM albums WHERE album_key=?1")
                .bind(fixture.albums[0])
                .fetch_all(&mut raw)
                .await
                .unwrap();
        fixture.database.catalog("source", None).await.unwrap().set_source_artwork("album", &objects.into_iter().map(|id|(id,Some((br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d453ddcc6a2bb4616a7509452b0c0edb","image":{"kind":"native","source_id":"source","item_id":"album-art","tag":null},"blur_hash":null}]"#.as_slice()).to_vec()))).collect::<Vec<_>>()).await.unwrap();
    };

    let track_artist = fixture
        .database
        .artists(&[fixture.artists[0]], false, true, &cancel)
        .await
        .expect("Track Artist row")
        .pop()
        .unwrap();
    let album_artist = fixture
        .database
        .artists(&[album_owner], true, true, &cancel)
        .await
        .expect("Album Artist row")
        .pop()
        .unwrap();
    assert_ne!(track_artist.artist_key, album_artist.artist_key);
    assert_eq!(
        (
            track_artist.summary.album_count,
            track_artist.summary.track_count
        ),
        (1, 2)
    );
    assert_eq!(
        (
            album_artist.summary.album_count,
            album_artist.summary.track_count
        ),
        (1, 2)
    );
    let track_artwork: Vec<library::ArtworkCandidate> =
        serde_json::from_slice(track_artist.artwork_binding.as_ref().unwrap()).unwrap();
    assert!(!track_artwork.is_empty());
    assert!(matches!(
        &track_artwork[0].image,
        library::ArtworkImage::Native { item_id, .. } if item_id == "album-art"
    ));
    assert!(matches!(
        track_artwork.last().unwrap().image,
        library::ArtworkImage::Artist { .. }
    ));
    let artist_art: Vec<library::ArtworkCandidate> =
        serde_json::from_slice(album_artist.artwork_binding.as_ref().unwrap()).unwrap();
    assert!(
        matches!(&artist_art[0].image,library::ArtworkImage::Native{item_id,..}if item_id=="album-artist-art")
    );

    let track_artist_tracks = (((&library::MemberRead {
        collection: library::Collection::Artist {
            key: track_artist.artist_key,
            album_artist: false,
        },
        sort: Some(library::CatalogSort::Title),
        ..library::MemberRead::new(library::Collection::Artist {
            key: track_artist.artist_key,
            album_artist: false,
        })
    })
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .unwrap()
        .rows)
        .into_iter()
        .map(library::MemberRow::into_track)
        .collect::<Vec<_>>())
    .into_iter()
    .map(|row| row.media_uri.clone())
    .collect::<Vec<_>>();
    let album_artist_tracks = (((&library::MemberRead {
        collection: library::Collection::Artist {
            key: album_artist.artist_key,
            album_artist: true,
        },
        sort: Some(library::CatalogSort::Title),
        ..library::MemberRead::new(library::Collection::Artist {
            key: album_artist.artist_key,
            album_artist: true,
        })
    })
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .unwrap()
        .rows)
        .into_iter()
        .map(library::MemberRow::into_track)
        .collect::<Vec<_>>())
    .into_iter()
    .map(|row| row.media_uri.clone())
    .collect::<Vec<_>>();
    assert_eq!(track_artist_tracks, album_artist_tracks);
    assert_eq!(track_artist_tracks.len(), 2);
    assert!(
        (((&library::MemberRead {
            collection: library::Collection::Artist {
                key: track_artist.artist_key,
                album_artist: true
            },
            sort: Some(library::CatalogSort::Title),
            ..library::MemberRead::new(library::Collection::Artist {
                key: track_artist.artist_key,
                album_artist: true
            })
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .unwrap()
            .rows)
            .into_iter()
            .map(library::MemberRow::into_track)
            .collect::<Vec<_>>())
        .into_iter()
        .map(|row| row.media_uri.clone())
        .collect::<Vec<_>>()
        .is_empty()
    );
    assert!(
        (((&library::MemberRead {
            collection: library::Collection::Artist {
                key: album_artist.artist_key,
                album_artist: false
            },
            sort: Some(library::CatalogSort::Title),
            ..library::MemberRead::new(library::Collection::Artist {
                key: album_artist.artist_key,
                album_artist: false
            })
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .unwrap()
            .rows)
            .into_iter()
            .map(library::MemberRow::into_track)
            .collect::<Vec<_>>())
        .into_iter()
        .map(|row| row.media_uri.clone())
        .collect::<Vec<_>>()
        .is_empty()
    );

    assert_eq!(
        library::ListRead {
            playlist_scope: Default::default(),
            kind: library::ListKind::ArtistAlbums {
                artist: track_artist.artist_key,
                album_artist: false,
                filters: None,
                release_class: None
            },
            source: Some(fixture.source),
            folder: None,
            filter: ("").to_string(),
            sort: library::CatalogSort::Title,
            descending: false,
            activity: true,
            favorites_only: false,
            downloaded_only: false
        }
        .albums(&fixture.database, 0, 64, &cancel)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.album_key)
        .collect::<Vec<_>>(),
        [fixture.albums[0]]
    );
    assert_eq!(
        library::ListRead {
            playlist_scope: Default::default(),
            kind: library::ListKind::ArtistAlbums {
                artist: album_artist.artist_key,
                album_artist: true,
                filters: None,
                release_class: None
            },
            source: Some(fixture.source),
            folder: None,
            filter: ("").to_string(),
            sort: library::CatalogSort::Title,
            descending: false,
            activity: true,
            favorites_only: false,
            downloaded_only: false
        }
        .albums(&fixture.database, 0, 64, &cancel)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.album_key)
        .collect::<Vec<_>>(),
        [fixture.albums[0]]
    );

    {
        let objects =
            sqlx::query_scalar::<_, String>("SELECT object_id FROM albums WHERE album_key=?1")
                .bind(fixture.albums[0])
                .fetch_all(&mut raw)
                .await
                .unwrap();
        fixture
            .database
            .catalog("source", None)
            .await
            .unwrap()
            .set_source_artwork(
                "album",
                &objects.into_iter().map(|id| (id, None)).collect::<Vec<_>>(),
            )
            .await
            .unwrap();
    };
    let album = fixture
        .database
        .albums(&[fixture.albums[0]], true, &cancel)
        .await
        .expect("Album artwork fallback")
        .pop()
        .expect("Album row");
    assert!(album.artwork_binding.is_some());
}

#[tokio::test]
async fn artist_orders_filter_roles_but_known_artists_remain_addressable() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    let mut role_artists = Vec::new();
    for (object_id, name, rating) in [
        ("role-track", "Performer One", 30),
        ("role-album", "Release Owner", 40),
        ("role-both", "Dual Credit", 50),
        ("role-none", "No Credits", 20),
    ] {
        let media_uri =
            library::source_entity_uri(&library::SourceId::new("source"), "artist", object_id);
        role_artists.push(
            sqlx::query_scalar::<_, library::ArtistKey>(
                "INSERT INTO artists(source_key,object_id,media_uri,metadata,normalized_name) VALUES (?1,?2,?3,jsonb(json_object('name',?4,'sort_text',lower(?4))),lower(COALESCE(json_extract(jsonb(json_object('name',?4,'sort_text',lower(?4))),'$.name'),''))) RETURNING artist_key",
            )
            .bind(fixture.source)
            .bind(object_id)
            .bind(media_uri)
            .bind(name)
            .bind(rating)
            .fetch_one(&mut raw)
            .await
            .expect("insert role fixture Artist"),
        );
        fixture
            .database
            .catalog("source", None)
            .await
            .unwrap()
            .write_native_user_data(
                "artist",
                &[(
                    object_id.to_string(),
                    library::NativeUserState {
                        favorite: Some(true),
                        rating: Some(Some(rating * 10)),
                        ..Default::default()
                    },
                )],
            )
            .await
            .unwrap();
    }
    let [track_only, album_only, both, uncredited] = role_artists.as_slice() else {
        unreachable!()
    };
    let (track_only, album_only, both) = (*track_only, *album_only, *both);
    let uncredited = *uncredited;
    sqlx::query(
        "INSERT INTO track_artists(track_key,artist_key,position) VALUES(?1,?2,1),(?1,?3,2)",
    )
    .bind(fixture.tracks[0])
    .bind(track_only)
    .bind(both)
    .execute(&mut raw)
    .await
    .expect("insert Track Artist roles");
    sqlx::query(
        "INSERT INTO album_artists(album_key,artist_key,position) VALUES(?1,?2,1),(?1,?3,2)",
    )
    .bind(fixture.albums[0])
    .bind(album_only)
    .bind(both)
    .execute(&mut raw)
    .await
    .expect("insert Album Artist roles");
    drop(raw);

    for sort in [
        CatalogSort::Title,
        CatalogSort::AlbumCount,
        CatalogSort::TrackCount,
        CatalogSort::LastPlayed,
        CatalogSort::PlayCount,
        CatalogSort::UserRating,
        CatalogSort::Favorite,
    ] {
        for folder in [None, Some(fixture.folder)] {
            for favorites_only in [false, true] {
                for descending in [false, true] {
                    let track_artists = library::ListRead {
                        playlist_scope: Default::default(),
                        kind: library::ListKind::Artists {
                            album_artist: false,
                        },
                        source: Some(fixture.source),
                        sort: sort,
                        folder: folder,
                        filter: ("").to_string(),
                        descending: descending,
                        favorites_only: favorites_only,
                        downloaded_only: false,
                        activity: true,
                    }
                    .page_artists(&fixture.database, RouteSeedWindow::top(), &cancel)
                    .await
                    .map(|page| (page.count, page.first_row_position, page.rows))
                    .expect("Track Artist order")
                    .2
                    .into_iter()
                    .map(|row| row.artist_key)
                    .collect::<Vec<_>>();
                    assert!(track_artists.contains(&track_only), "{sort:?}");
                    assert!(track_artists.contains(&both), "{sort:?}");
                    assert!(!track_artists.contains(&album_only), "{sort:?}");

                    let album_artists = library::ListRead {
                        playlist_scope: Default::default(),
                        kind: library::ListKind::Artists { album_artist: true },
                        source: Some(fixture.source),
                        sort: sort,
                        folder: folder,
                        filter: ("").to_string(),
                        descending: descending,
                        favorites_only: favorites_only,
                        downloaded_only: false,
                        activity: true,
                    }
                    .page_artists(&fixture.database, RouteSeedWindow::top(), &cancel)
                    .await
                    .map(|page| (page.count, page.first_row_position, page.rows))
                    .expect("Album Artist order")
                    .2
                    .into_iter()
                    .map(|row| row.artist_key)
                    .collect::<Vec<_>>();
                    assert!(album_artists.contains(&album_only), "{sort:?}");
                    assert!(album_artists.contains(&both), "{sort:?}");
                    assert!(!album_artists.contains(&track_only), "{sort:?}");
                }
            }
        }
    }

    let track_row = fixture
        .database
        .artists(&[track_only], false, true, &cancel)
        .await
        .expect("Track Artist row")
        .pop()
        .expect("role-matched Track Artist");
    assert_eq!(
        (track_row.summary.album_count, track_row.summary.track_count),
        (1, 1)
    );
    let album_row = fixture
        .database
        .artists(&[album_only], true, true, &cancel)
        .await
        .expect("Album Artist row")
        .pop()
        .expect("role-matched Album Artist");
    assert_eq!(
        (album_row.summary.album_count, album_row.summary.track_count),
        (1, 2)
    );
    for (artist, album_artist) in [
        (album_only, false),
        (track_only, true),
        (uncredited, false),
        (uncredited, true),
    ] {
        let row = fixture
            .database
            .artists(&[artist], album_artist, true, &cancel)
            .await
            .expect("known artist row")
            .pop()
            .unwrap();
        assert_eq!(row.artist_key, artist);
        assert_eq!(row.summary.track_count, 0);
        let detail = fixture
            .database
            .artists(&[artist], album_artist, true, &cancel)
            .await
            .expect("known &[artist] detail")
            .pop()
            .unwrap();
        assert_eq!(detail.artist_key, artist);
        assert_eq!(detail.summary.track_count, 0);
    }
    assert_eq!(
        (((&library::MemberRead {
            collection: library::Collection::Artist {
                key: track_only,
                album_artist: false
            },
            sort: Some(library::CatalogSort::Title),
            ..library::MemberRead::new(library::Collection::Artist {
                key: track_only,
                album_artist: false
            })
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .expect("Track Artist Tracks")
            .rows)
            .into_iter()
            .map(library::MemberRow::into_track)
            .collect::<Vec<_>>())
        .into_iter()
        .map(|row| row.media_uri.clone())
        .collect::<Vec<_>>(),
        [fixture.track_uris[0].clone()]
    );
    assert_eq!(
        library::ListRead {
            playlist_scope: Default::default(),
            kind: library::ListKind::ArtistAlbums {
                artist: album_only,
                album_artist: true,
                filters: None,
                release_class: None
            },
            source: Some(fixture.source),
            folder: None,
            filter: ("").to_string(),
            sort: library::CatalogSort::Title,
            descending: false,
            activity: true,
            favorites_only: false,
            downloaded_only: false
        }
        .albums(&fixture.database, 0, 64, &cancel)
        .await
        .expect("Album Artist Albums")
        .into_iter()
        .map(|row| row.album_key)
        .collect::<Vec<_>>(),
        [fixture.albums[0]]
    );
}

#[tokio::test]
async fn default_activity_smart_playlists_stay_deleted_after_reopening() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let database = library::Database::open(&path).await.unwrap();
    let source = database
        .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
        .await
        .unwrap()
        .source;
    let cancellation = ReadCancellation::new();
    let (count, _, rows) = library::ListRead {
        playlist_scope: Default::default(),
        kind: library::ListKind::SmartPlaylists { now: 0 },
        source: (Some(source)).into(),
        sort: CatalogSort::Position,
        folder: None,
        filter: ("").to_string(),
        descending: false,
        favorites_only: false,
        downloaded_only: false,
        activity: true,
    }
    .page_smart_playlists(&database, RouteSeedWindow::top(), &cancellation)
    .await
    .map(|page| (page.count, page.first_row_position, page.rows))
    .expect("read default Smart Playlists");
    let order = rows
        .iter()
        .map(|row| row.smart_playlist_key)
        .collect::<Vec<_>>();
    assert_eq!(count, order.len());
    assert_eq!(order.len(), 3);
    assert_eq!(
        rows.iter()
            .map(|row| (row.object_id.as_str(), row.name.as_str()))
            .collect::<Vec<_>>(),
        [
            ("builtin:most_played", "Most Played"),
            ("builtin:never_played", "Never Played"),
            ("builtin:most_skipped", "Most Skipped"),
        ]
    );
    assert_eq!(rows[0].definition.sort_field, SmartPlaylistSort::PlayCount);
    assert_eq!(
        rows[1].definition.match_all[0].field,
        SmartPlaylistRuleField::Played
    );
    assert_eq!(rows[2].definition.sort_field, SmartPlaylistSort::SkipCount);
    assert!(database.delete_smart_playlist(order[2]).await.unwrap());
    database.close().await.unwrap();
    let database = library::Database::open(&path).await.unwrap();
    let mut raw = connection(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT object_id FROM smart_playlists ORDER BY position")
            .fetch_all(&mut raw)
            .await
            .unwrap(),
        ["builtin:most_played", "builtin:never_played"]
    );
    for key in &order[..2] {
        assert!(database.delete_smart_playlist(*key).await.unwrap());
    }
    database.close().await.unwrap();
    let database = library::Database::open(&path).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM smart_playlists")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        0
    );
    database.close().await.unwrap();
}

#[tokio::test]
async fn smart_playlists_exclude_known_media_until_it_is_indexed() {
    let fixture = fixture().await;
    let direct = "https://media.example/direct.flac".to_string();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO tracks(media_uri,indexed,metadata,normalized_search) VALUES (?1,0,jsonb(json_object('title',COALESCE('Direct Signal',''),'artist',COALESCE('Outside Artist',''),'album',COALESCE('Outside Album',''),'duration_millis',COALESCE(42000,0))),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE('Direct Signal',''),'artist',COALESCE('Outside Artist',''),'album',COALESCE('Outside Album',''),'duration_millis',COALESCE(42000,0))),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Direct Signal',''),'artist',COALESCE('Outside Artist',''),'album',COALESCE('Outside Album',''),'duration_millis',COALESCE(42000,0))),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Direct Signal',''),'artist',COALESCE('Outside Artist',''),'album',COALESCE('Outside Album',''),'duration_millis',COALESCE(42000,0))),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Direct Signal',''),'artist',COALESCE('Outside Artist',''),'album',COALESCE('Outside Album',''),'duration_millis',COALESCE(42000,0))),'$.comment'),'')))")
        .bind(&direct).execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO queue_entries(track_key,provenance) SELECT track_key,'\"Manual\"' FROM tracks WHERE media_uri=?1")
        .bind(&direct).execute(&mut raw).await.unwrap();
    drop(raw);
    let (playlist, _) = fixture
        .database
        .create_playlist(
            None,
            "Direct",
            &library::MemberSelection::Uris(((std::slice::from_ref(&direct)).to_vec()).into()),
        )
        .await
        .expect("create global Playlist")
        .expect("Playlist key");
    assert_eq!(
        fixture
            .database
            .add_playlist_tracks(
                None,
                playlist,
                &library::MemberSelection::Uris(((std::slice::from_ref(&direct)).to_vec()).into()),
                false
            )
            .await
            .expect("append durable media directly"),
        1
    );
    assert_eq!(
        fixture
            .database
            .add_playlist_tracks(
                None,
                playlist,
                &library::MemberSelection::Uris(((std::slice::from_ref(&direct)).to_vec()).into()),
                true
            )
            .await
            .expect("skip an existing durable identity"),
        0
    );
    let mut definition = SmartPlaylistDefinition {
        source_id: None,
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::Title,
            operator: SmartPlaylistRuleOperator::Contains,
            value: Some(SmartPlaylistRuleValue::Text("signal".to_string())),
        }],
        match_any: Vec::new(),
        sort_field: SmartPlaylistSort::Title,
        descending: false,
        activity_period: SmartPlaylistActivityPeriod::Lifetime,
        limit: None,
    };
    let smart = fixture
        .database
        .create_smart_playlist("Direct", &definition)
        .await
        .expect("create All Smart Playlist");
    let cancel = ReadCancellation::new();
    let all = fixture
        .database
        .selected_track_uris(
            &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                .selection(),
            None,
            &cancel,
        )
        .await
        .expect("evaluate All Smart Playlist");
    assert!(all.is_empty());

    definition.source_id = Some(library::SourceId::new("source"));
    fixture
        .database
        .update_smart_playlist(smart, "Direct", &definition, None)
        .await
        .expect("make Smart Playlist Current");
    assert!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancel
            )
            .await
            .expect("evaluate Current Smart Playlist")
            .is_empty()
    );
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET source_key=?1,object_id='direct',indexed=1 WHERE media_uri=?2")
        .bind(fixture.source)
        .bind(&direct)
        .execute(&mut raw)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancel
            )
            .await
            .unwrap(),
        std::slice::from_ref(&direct)
    );
    definition.source_id = None;
    fixture
        .database
        .update_smart_playlist(smart, "Direct", &definition, None)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancel
            )
            .await
            .unwrap(),
        [direct]
    );
}

#[tokio::test]
async fn completed_direct_download_keeps_access_without_smart_membership() {
    let fixture = fixture().await;
    let direct = "https://media.example/download.flac".to_string();
    fixture
        .database
        .write_direct_track_metadata(
            &direct,
            &library::TrackMetadata {
                title: "Direct artifact".into(),
                duration_millis: 42_000,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture
        .database
        .write_local_access(
            None,
            &[&LocalAccessWrite {
                media_uri: direct.clone(),
                origin: LocalAccessOrigin::Download,
                path: "/downloads/direct.flac".to_string(),
                root: "/downloads".to_string(),
                relative_path: "direct.flac".to_string(),
                size_bytes: 10,
                mtime_ns: 1,
                device_id: None,
                inode: None,
                parser_version: 5,
                access_uri: "file:///downloads/direct.flac".to_string(),
                loudness_analysis_key: None,
            }],
        )
        .await
        .expect("record completed direct download");
    assert_eq!(
        fixture
            .database
            .playback_access(&direct)
            .await
            .expect("resolve direct download access")
            .map(|(uri, _)| uri)
            .as_deref(),
        Some("file:///downloads/direct.flac")
    );

    let mut definition = SmartPlaylistDefinition {
        source_id: None,
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::Title,
            operator: SmartPlaylistRuleOperator::Contains,
            value: Some(SmartPlaylistRuleValue::Text("artifact".to_string())),
        }],
        ..SmartPlaylistDefinition::default()
    };
    let smart = fixture
        .database
        .create_smart_playlist("Downloaded direct", &definition)
        .await
        .expect("create direct download Smart Playlist");
    let cancel = ReadCancellation::new();
    assert_eq!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancel
            )
            .await
            .expect("evaluate All direct download"),
        Vec::<String>::new()
    );
    definition.source_id = Some(library::SourceId::new("source"));
    fixture
        .database
        .update_smart_playlist(smart, "Downloaded direct", &definition, None)
        .await
        .expect("make direct download Smart Playlist Current");
    assert!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancel
            )
            .await
            .expect("evaluate Current direct download")
            .is_empty()
    );
}

#[tokio::test]
async fn canonical_rows_keep_uri_order_after_smart_membership_changes() {
    let fixture = fixture().await;
    let direct = "https://media.example/window.flac".to_string();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO tracks(media_uri,indexed,metadata,normalized_search) VALUES (?1,0,jsonb(json_object('title',COALESCE('Known title',''),'artist',COALESCE('Artist',''),'album',COALESCE('Album',''),'duration_millis',COALESCE(42000,0))),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE('Known title',''),'artist',COALESCE('Artist',''),'album',COALESCE('Album',''),'duration_millis',COALESCE(42000,0))),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Known title',''),'artist',COALESCE('Artist',''),'album',COALESCE('Album',''),'duration_millis',COALESCE(42000,0))),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Known title',''),'artist',COALESCE('Artist',''),'album',COALESCE('Album',''),'duration_millis',COALESCE(42000,0))),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Known title',''),'artist',COALESCE('Artist',''),'album',COALESCE('Album',''),'duration_millis',COALESCE(42000,0))),'$.comment'),'')))")
        .bind(&direct).execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped) VALUES(?1,'Historical title','Artist','Album',100,'1970-01',42000,42000,0)")
        .bind(&direct).execute(&mut raw).await.unwrap();
    let mut definition = SmartPlaylistDefinition::default();
    let key = fixture
        .database
        .create_smart_playlist("Window", &definition)
        .await
        .unwrap();
    let cancel = ReadCancellation::new();
    let order = fixture
        .database
        .selected_track_uris(
            &library::MemberRead::new(library::Collection::Smart { key: key, now: 500 })
                .selection(),
            None,
            &cancel,
        )
        .await
        .unwrap();
    assert!(!order.contains(&direct));
    let detail = fixture
        .database
        .smart_playlist_rows(&[key], 500, &cancel)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(detail.track_count as usize, order.len());
    definition.match_all.push(SmartPlaylistRule {
        field: SmartPlaylistRuleField::Title,
        operator: SmartPlaylistRuleOperator::Equals,
        value: Some(SmartPlaylistRuleValue::Text("No match".into())),
    });
    fixture
        .database
        .update_smart_playlist(key, "Window", &definition, None)
        .await
        .unwrap();
    let requested = vec![direct.clone(), fixture.track_uris[0].clone(), direct];
    let rows = fixture
        .database
        .tracks_by_uris(&((&requested).to_vec()), &cancel)
        .await
        .unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>(),
        requested
    );
    assert_eq!(rows[0].metadata.title, "Known title");
    assert_eq!(rows[1].metadata.title, "Alpha");
    assert_eq!(rows[2].metadata.duration_millis, 42000);
    assert_eq!(rows[0].state.play_count, 1);
    sqlx::query("UPDATE tracks SET indexed=0 WHERE media_uri=?1")
        .bind(&fixture.track_uris[0])
        .execute(&mut raw)
        .await
        .unwrap();
    let rows = fixture
        .database
        .tracks_by_uris(&((&requested).to_vec()), &cancel)
        .await
        .unwrap();
    assert_eq!(rows[1].metadata.title, "Alpha");
}

#[tokio::test]
async fn global_playlist_keeps_unindexed_media_outside_smart_membership() {
    let fixture = fixture().await;
    let retained = fixture.track_uris[0].clone();
    let (playlist, _) = fixture
        .database
        .create_playlist(
            None,
            "Retained",
            &library::MemberSelection::Uris(((std::slice::from_ref(&retained)).to_vec()).into()),
        )
        .await
        .expect("create global Playlist")
        .expect("Playlist key");
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET indexed=0 WHERE media_uri=?1")
        .bind(&retained)
        .execute(&mut raw)
        .await
        .expect("remove retained media from the catalog");
    drop(raw);
    let mut definition = SmartPlaylistDefinition {
        source_id: None,
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::Title,
            operator: SmartPlaylistRuleOperator::Equals,
            value: Some(SmartPlaylistRuleValue::Text("Alpha".to_string())),
        }],
        ..SmartPlaylistDefinition::default()
    };
    let smart = fixture
        .database
        .create_smart_playlist("Retained", &definition)
        .await
        .expect("create retained-source Smart Playlist");
    let cancellation = ReadCancellation::new();
    assert_eq!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancellation
            )
            .await
            .expect("evaluate All while source is retained"),
        Vec::<String>::new()
    );
    definition.source_id = Some(library::SourceId::new("source"));
    fixture
        .database
        .update_smart_playlist(smart, "Retained", &definition, None)
        .await
        .expect("make retained-source Smart Playlist Current");
    assert_eq!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancellation
            )
            .await
            .expect("evaluate Current while source is retained"),
        Vec::<String>::new()
    );

    fixture
        .database
        .remove_source(&library::SourceId::new("source"))
        .await
        .expect("forget source");
    assert!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancellation
            )
            .await
            .unwrap()
            .is_empty()
    );
    definition.source_id = None;
    fixture
        .database
        .update_smart_playlist(smart, "Retained", &definition, None)
        .await
        .expect("make retained-source Smart Playlist All");
    assert_eq!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancellation
            )
            .await
            .expect("evaluate All after Forget"),
        Vec::<String>::new()
    );
    assert_eq!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Playlist(playlist)).selection(),
                None,
                &cancellation
            )
            .await
            .expect("read retained Playlist reference"),
        [retained]
    );
}

#[tokio::test]
async fn smart_membership_excludes_retained_queue_and_activity() {
    let fixture = fixture().await;
    let queue_uri =
        library::source_entity_uri(&library::SourceId::new("absent"), "track", "queued");
    let listen_uri =
        library::source_entity_uri(&library::SourceId::new("absent"), "track", "listened");
    let mut raw = connection(&fixture.path).await;
    sqlx::query(
        "INSERT INTO tracks(media_uri,indexed,metadata,normalized_search) VALUES (?1,0,jsonb(json_object('title',COALESCE('Queued',''))),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE('Queued',''))),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Queued',''))),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Queued',''))),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Queued',''))),'$.comment'),''))),(?2,0,jsonb(json_object('title',COALESCE('Listened',''))),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE('Listened',''))),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Listened',''))),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Listened',''))),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Listened',''))),'$.comment'),'')))",
    )
    .bind(&queue_uri)
    .bind(&listen_uri)
    .execute(&mut raw)
    .await
    .unwrap();
    sqlx::query("INSERT INTO queue_entries(track_key,provenance) SELECT track_key,'\"Manual\"' FROM tracks WHERE media_uri=?1")
        .bind(&queue_uri).execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO listens(external_id,source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis) VALUES('retained','absent',?1,'Listened','Artist','Album',1,'1970-01',1000,1000)")
        .bind(&listen_uri).execute(&mut raw).await.unwrap();
    sqlx::query("UPDATE tracks SET indexed=0 ")
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    let key = fixture
        .database
        .create_smart_playlist(
            "All",
            &SmartPlaylistDefinition {
                source_id: None,
                ..SmartPlaylistDefinition::default()
            },
        )
        .await
        .unwrap();
    let cancellation = ReadCancellation::new();
    assert_eq!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: key, now: 10 })
                    .selection(),
                None,
                &cancellation
            )
            .await
            .unwrap(),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn smart_overview_keeps_the_full_order_and_only_the_requested_summary_window() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    let cover = serde_json::to_vec(&[library::ArtworkCandidate::new(
        library::ArtworkImage::Native {
            source_id: library::SourceId::new("source"),
            item_id: "overview-cover".into(),
            tag: None,
        },
        None,
    )])
    .unwrap();
    {
        let objects = sqlx::query_scalar::<_, String>("SELECT object_id FROM tracks")
            .fetch_all(&mut raw)
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
                    .map(|id| (id, Some((cover).to_vec())))
                    .collect::<Vec<_>>(),
            )
            .await
            .unwrap();
    };
    sqlx::query("INSERT INTO local_locators(media_uri,origin,path,root,relative_path,access_uri) VALUES(?1,'download','/music/track.flac','/music','track.flac','file:///music/track.flac')")
        .bind(&fixture.track_uris[0]).execute(&mut raw).await.unwrap();
    for index in 0..70 {
        let definition = SmartPlaylistDefinition {
            source_id: (index % 2 == 0).then(|| library::SourceId::new("source")),
            limit: Some(index % 4 + 1),
            ..SmartPlaylistDefinition::default()
        };
        fixture
            .database
            .create_smart_playlist(&format!("List {index:02}"), &definition)
            .await
            .unwrap();
    }
    for (folder, sort) in [
        (None, CatalogSort::TrackCount),
        (None, CatalogSort::Duration),
        (Some(fixture.folder), CatalogSort::Title),
    ] {
        for descending in [false, true] {
            let window = RouteSeedWindow::relative(1.0);
            let (count, start, rows) = library::ListRead {
                playlist_scope: Default::default(),
                kind: library::ListKind::SmartPlaylists { now: 0 },
                source: (Some(fixture.source)).into(),
                sort: sort,
                folder: folder,
                filter: ("").to_string(),
                descending: descending,
                favorites_only: false,
                downloaded_only: false,
                activity: true,
            }
            .page_smart_playlists(&fixture.database, window, &cancel)
            .await
            .map(|page| (page.count, page.first_row_position, page.rows))
            .unwrap();
            let order = rows
                .iter()
                .map(|row| row.smart_playlist_key)
                .collect::<Vec<_>>();
            assert_eq!(count, 70);
            assert_eq!(start, 64);
            assert_eq!(rows.len(), 6);
            let expected = fixture
                .database
                .smart_playlist_rows(&order, 0, &cancel)
                .await
                .unwrap();
            assert_eq!(rows, expected);
        }
    }
    let objects =
        sqlx::query_scalar::<_, String>("SELECT object_id FROM smart_playlists ORDER BY position")
            .fetch_all(&mut raw)
            .await
            .unwrap();
    let pins = objects
        .into_iter()
        .map(|playlist_id| library::SidebarPin::SmartPlaylist { playlist_id })
        .collect::<Vec<_>>();
    let headers = fixture
        .database
        .sidebar_pin_rows(&pins, &cancel)
        .await
        .unwrap();
    assert_eq!(headers.len(), 70);
    assert!(headers.iter().all(|row| row.totals.is_none()));
    let rows = fixture
        .database
        .sidebar_pin_totals(headers, 0, &cancel)
        .await
        .unwrap();
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row.pin, pins[index]);
        assert_eq!(
            row.totals.unwrap().0,
            ((index % 4 + 1).min(fixture.tracks.len())) as i64
        );
    }
}

#[tokio::test]
async fn smart_playlist_reordering_preserves_unique_positions() {
    let fixture = fixture().await;
    let definition = SmartPlaylistDefinition::default();
    let first = fixture
        .database
        .create_smart_playlist("First", &definition)
        .await
        .expect("create first Smart Playlist");
    let second = fixture
        .database
        .create_smart_playlist("Second", &definition)
        .await
        .expect("create second Smart Playlist");
    let third = fixture
        .database
        .create_smart_playlist("Third", &definition)
        .await
        .expect("create third Smart Playlist");

    assert!(
        fixture
            .database
            .move_smart_playlist(third, first)
            .await
            .expect("move Smart Playlist upward")
    );
    assert!(
        fixture
            .database
            .move_smart_playlist(third, second)
            .await
            .expect("move Smart Playlist downward")
    );
    assert_eq!(
        library::ListRead {
            playlist_scope: Default::default(),
            kind: library::ListKind::SmartPlaylists { now: 0 },
            source: (Some(fixture.source)).into(),
            sort: CatalogSort::Position,
            folder: None,
            filter: ("").to_string(),
            descending: false,
            favorites_only: false,
            activity: true,
            downloaded_only: false
        }
        .page_smart_playlists(
            &fixture.database,
            RouteSeedWindow::top(),
            &ReadCancellation::new()
        )
        .await
        .map(|page| (page.count, page.first_row_position, page.rows))
        .expect("read reordered Smart Playlists")
        .2
        .into_iter()
        .map(|row| row.smart_playlist_key)
        .collect::<Vec<_>>(),
        [first, second, third]
    );
}

#[tokio::test]
async fn smart_playlist_periods_and_never_played_query_sqlite_directly() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let suggestions = fixture
        .database
        .smart_playlist_value_suggestions(fixture.source, None, &cancel)
        .await
        .expect("bounded Smart Playlist value suggestions");
    assert_eq!(suggestions.genres, ["Rock".to_string()]);
    assert_eq!(suggestions.moods, ["Energetic".to_string()]);
    let never = SmartPlaylistDefinition {
        source_id: None,
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::Played,
            operator: SmartPlaylistRuleOperator::Is,
            value: Some(SmartPlaylistRuleValue::Bool(false)),
        }],
        match_any: Vec::new(),
        sort_field: SmartPlaylistSort::Title,
        descending: false,
        activity_period: SmartPlaylistActivityPeriod::Weekly,
        limit: None,
    };
    let smart = fixture
        .database
        .create_smart_playlist("Never Played", &never)
        .await
        .expect("create Never Played");
    let policy_definition = SmartPlaylistDefinition {
        source_id: None,
        match_all: SmartPlaylistRuleField::ALL
            .into_iter()
            .map(SmartPlaylistRuleField::default_rule)
            .collect(),
        ..SmartPlaylistDefinition::default()
    };
    let policy = fixture
        .database
        .create_smart_playlist("Policy Defaults", &policy_definition)
        .await
        .expect("Library-owned editor defaults pass Library validation");
    assert_eq!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart { key: smart, now: 0 })
                    .selection(),
                None,
                &cancel
            )
            .await
            .expect("Never Played membership")
            .len(),
        4
    );

    let mut raw = connection(&fixture.path).await;
    sqlx::query(
        "INSERT INTO native_media_state(media_uri,play_count,last_played) SELECT media_uri,2,1 FROM tracks WHERE source_key=?1 AND object_id='track-0' ON CONFLICT(media_uri) DO UPDATE SET play_count=excluded.play_count,last_played=excluded.last_played",
    )
    .bind(fixture.source)
    .execute(&mut raw)
    .await
    .expect("record current server statistics");
    for (media_uri, date) in [
        (&fixture.track_uris[0], "2025-06-12"),
        (&fixture.track_uris[1], "2025-06-18"),
        (&fixture.track_uris[2], "2025-05-25"),
        (&fixture.track_uris[3], "2024-06-19"),
    ] {
        sqlx::query(
            "INSERT INTO listens(
                 source_id, media_uri, track_title,
                 artist_name, album_title, started_at, local_period, duration_millis, listened_millis, skipped
             ) VALUES ('source', ?1, 'Track', 'Artist', 'Album',
                       CAST(strftime('%s', ?2) AS INTEGER), substr(?2,1,7), 180000, 180000, 0)",
        )
        .bind(media_uri)
        .bind(date)
        .execute(&mut raw)
        .await
        .expect("insert period listen");
    }
    let now = sqlx::query_scalar::<_, i64>("SELECT CAST(strftime('%s', '2025-06-18') AS INTEGER)")
        .fetch_one(&mut raw)
        .await
        .expect("read fixed current time");
    let mut played = SmartPlaylistDefinition {
        source_id: None,
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::PlayCount,
            operator: SmartPlaylistRuleOperator::Above,
            value: Some(SmartPlaylistRuleValue::Number(0)),
        }],
        match_any: Vec::new(),
        sort_field: SmartPlaylistSort::PlayCount,
        descending: true,
        activity_period: SmartPlaylistActivityPeriod::Weekly,
        limit: None,
    };
    for (period, expected) in [
        (SmartPlaylistActivityPeriod::Weekly, 2),
        (SmartPlaylistActivityPeriod::Monthly, 3),
        (SmartPlaylistActivityPeriod::Yearly, 4),
        (SmartPlaylistActivityPeriod::Lifetime, 4),
    ] {
        played.activity_period = period;
        assert!(
            fixture
                .database
                .update_smart_playlist(smart, "Played", &played, None)
                .await
                .expect("update period template")
        );
        assert_eq!(
            fixture
                .database
                .selected_track_uris(
                    &library::MemberRead::new(library::Collection::Smart {
                        key: smart,
                        now: now
                    })
                    .selection(),
                    None,
                    &cancel
                )
                .await
                .expect("period membership")
                .len(),
            expected
        );
    }
    let facts = fixture
        .database
        .smart_playlist_rows(&[smart], now, &cancel)
        .await
        .expect("Smart Playlist row")
        .pop()
        .expect("row exists");
    assert_eq!(facts.track_count, 4);
    assert!(facts.duration_millis > 0);
    sqlx::query(
        "UPDATE tracks SET metadata=jsonb(json_set(metadata,'$.comment',NULL)) WHERE track_key=?1",
    )
    .bind(fixture.tracks[0])
    .execute(&mut raw)
    .await
    .expect("remove optional Comment");
    let mut optional = SmartPlaylistDefinition {
        source_id: None,
        match_all: vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::Comment,
            operator: SmartPlaylistRuleOperator::NotContains,
            value: Some(SmartPlaylistRuleValue::Text("absent".to_string())),
        }],
        match_any: Vec::new(),
        sort_field: SmartPlaylistSort::Title,
        descending: false,
        activity_period: SmartPlaylistActivityPeriod::Lifetime,
        limit: None,
    };
    let second = fixture
        .database
        .create_smart_playlist("Optional", &optional)
        .await
        .expect("create second Smart Playlist");
    assert_eq!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart {
                    key: second,
                    now: now
                })
                .selection(),
                None,
                &cancel
            )
            .await
            .expect("missing optional value does not match NotContains")
            .len(),
        3
    );
    optional.match_all[0].operator = SmartPlaylistRuleOperator::IsEmpty;
    optional.match_all[0].value = None;
    assert!(
        fixture
            .database
            .update_smart_playlist(second, "Optional", &optional, None)
            .await
            .expect("update optional rule")
    );
    assert_eq!(
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart {
                    key: second,
                    now: now
                })
                .selection(),
                None,
                &cancel
            )
            .await
            .expect("missing optional value matches IsEmpty"),
        [fixture.track_uris[0].clone()]
    );
    let smart_rows = fixture
        .database
        .smart_playlist_rows(&[smart, second], now, &cancel)
        .await
        .expect("consistent Smart Playlist rows");
    assert_eq!(
        smart_rows[0].track_count as usize,
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart {
                    key: smart,
                    now: now
                })
                .selection(),
                None,
                &cancel
            )
            .await
            .expect("first Smart membership")
            .len()
    );
    assert_eq!(
        smart_rows[1].track_count as usize,
        fixture
            .database
            .selected_track_uris(
                &library::MemberRead::new(library::Collection::Smart {
                    key: second,
                    now: now
                })
                .selection(),
                None,
                &cancel
            )
            .await
            .expect("second Smart membership")
            .len()
    );
    assert_eq!(
        library::ListRead {
            playlist_scope: Default::default(),
            kind: library::ListKind::SmartPlaylists { now: now },
            source: (Some(fixture.source)).into(),
            sort: CatalogSort::TrackCount,
            folder: None,
            filter: ("").to_string(),
            descending: true,
            favorites_only: false,
            activity: true,
            downloaded_only: false
        }
        .page_smart_playlists(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (page.count, page.first_row_position, page.rows))
        .expect("sorted Smart Playlist order")
        .2
        .into_iter()
        .map(|row| row.smart_playlist_key)
        .collect::<Vec<_>>(),
        [smart, second, policy]
    );
    assert_eq!(
        library::ListRead {
            playlist_scope: Default::default(),
            kind: library::ListKind::SmartPlaylists { now: now },
            source: (Some(fixture.source)).into(),
            sort: CatalogSort::Duration,
            folder: None,
            filter: ("").to_string(),
            descending: true,
            favorites_only: false,
            activity: true,
            downloaded_only: false
        }
        .page_smart_playlists(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (page.count, page.first_row_position, page.rows))
        .expect("duration-sorted Smart Playlist order")
        .2
        .into_iter()
        .map(|row| row.smart_playlist_key)
        .collect::<Vec<_>>(),
        [smart, second, policy]
    );
}

#[tokio::test]
async fn artist_and_home_activity_follow_membership_even_without_listen_source_ids() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE sources SET kind='local' WHERE source_key=?1")
        .bind(fixture.source)
        .execute(&mut raw)
        .await
        .unwrap();
    for (uri, time) in [
        (fixture.track_uris[0].as_str(), 100),
        (fixture.track_uris[0].as_str(), 200),
        (fixture.track_uris[2].as_str(), 300),
        ("file:///outside-current.flac", 400),
    ] {
        sqlx::query("INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis) VALUES(?1,'Track','Artist','Album',?2,'2026-09',1000,1000)")
            .bind(uri).bind(time).execute(&mut raw).await.unwrap();
    }
    sqlx::query("DELETE FROM track_folders WHERE track_key=?1")
        .bind(fixture.tracks[2])
        .execute(&mut raw)
        .await
        .unwrap();
    let cancel = ReadCancellation::new();
    for album_artist in [false, true] {
        let rows = fixture
            .database
            .artists(&[fixture.artists[0]], album_artist, true, &cancel)
            .await
            .unwrap();
        assert_eq!(rows[0].state.play_count, 2);
        assert_eq!(rows[0].state.last_played, Some(200));
    }
    let home = fixture
        .database
        .home_page(
            fixture.source,
            Some(fixture.folder),
            0,
            0,
            &library::HomeBlockKind::all(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        home.most_played
            .tracks
            .iter()
            .map(|row| row.track.media_uri.as_str())
            .collect::<Vec<_>>(),
        [fixture.track_uris[0].as_str()]
    );
    assert_eq!(
        home.recently_played
            .tracks
            .iter()
            .map(|row| row.track.media_uri.as_str())
            .collect::<Vec<_>>(),
        [fixture.track_uris[0].as_str()]
    );

    sqlx::query("INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at)
        SELECT source.object_id,'lifetime','track',track.object_id,
            CASE track.track_key WHEN ?2 THEN 4 WHEN ?3 THEN 1 ELSE 20 END,0,
            CASE track.track_key WHEN ?2 THEN 50 WHEN ?3 THEN 500 ELSE 600 END
        FROM tracks track JOIN sources source USING(source_key)
        WHERE track.source_key=?1 AND track.track_key IN(?2,?3,?4)")
        .bind(fixture.source).bind(fixture.tracks[0]).bind(fixture.tracks[1]).bind(fixture.tracks[2])
        .execute(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO native_media_state(media_uri,play_count,last_played) SELECT media_uri,9,700 FROM tracks WHERE track_key=?1 ON CONFLICT(media_uri) DO UPDATE SET play_count=excluded.play_count,last_played=excluded.last_played")
        .bind(fixture.tracks[1])
        .execute(&mut raw)
        .await
        .unwrap();
    for kind in ["local", "files", "webdav", "smb", "documents", "subsonic"] {
        sqlx::query("UPDATE sources SET kind=?2 WHERE source_key=?1")
            .bind(fixture.source)
            .bind(kind)
            .execute(&mut raw)
            .await
            .unwrap();
        let home = fixture
            .database
            .home_page(
                fixture.source,
                Some(fixture.folder),
                0,
                0,
                &[
                    library::HomeBlockKind::MostPlayed,
                    library::HomeBlockKind::RecentlyPlayed,
                ],
                &cancel,
            )
            .await
            .unwrap();
        let most = home
            .most_played
            .tracks
            .iter()
            .map(|row| (row.track.track_key, row.track.state.play_count))
            .collect::<Vec<_>>();
        let recent = home
            .recently_played
            .tracks
            .iter()
            .map(|row| (row.track.track_key, row.track.state.last_played))
            .collect::<Vec<_>>();
        if kind == "subsonic" {
            assert_eq!(most, [(fixture.tracks[1], 9)]);
            assert_eq!(recent, [(fixture.tracks[1], Some(700))]);
        } else {
            assert_eq!(most, [(fixture.tracks[0], 6), (fixture.tracks[1], 1)]);
            assert_eq!(
                recent,
                [
                    (fixture.tracks[1], Some(500)),
                    (fixture.tracks[0], Some(200))
                ]
            );
        }
    }
}

#[tokio::test]
async fn home_reads_only_enabled_blocks_and_preserves_provider_sections() {
    use library::{HomeBlockKind, HomePage};

    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE sources SET kind='local' WHERE source_key=?1")
        .bind(fixture.source)
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis) VALUES(?1,'Track','Artist','Album',100,'2026-09',1000,1000)")
        .bind(&fixture.track_uris[0])
        .execute(&mut raw)
        .await
        .unwrap();
    let cancel = ReadCancellation::new();
    let all = fixture
        .database
        .home_page(fixture.source, None, 0, 0, &HomeBlockKind::all(), &cancel)
        .await
        .unwrap();
    assert!(!all.provider_sections.is_empty());
    assert!(all.showcase.is_some());
    assert!(!all.explore.is_empty());
    assert!(!all.most_played.tracks.is_empty());
    assert!(!all.newly_added.albums.is_empty());
    assert!(!all.recently_played.tracks.is_empty());
    assert!(!all.recently_released.albums.is_empty());
    assert!(!all.genres.is_empty());

    for block in HomeBlockKind::all() {
        let selected = fixture
            .database
            .home_page(fixture.source, None, 0, 0, &[block], &cancel)
            .await
            .unwrap();
        let mut expected = HomePage {
            provider_sections: all.provider_sections.clone(),
            ..HomePage::default()
        };
        match block {
            HomeBlockKind::Showcase => expected.showcase = all.showcase.clone(),
            HomeBlockKind::Explore => expected.explore = all.explore.clone(),
            HomeBlockKind::MostPlayed => expected.most_played = all.most_played.clone(),
            HomeBlockKind::NewlyAdded => expected.newly_added = all.newly_added.clone(),
            HomeBlockKind::RecentlyPlayed => expected.recently_played = all.recently_played.clone(),
            HomeBlockKind::RecentlyReleased => {
                expected.recently_released = all.recently_released.clone()
            }
            HomeBlockKind::Genres => expected.genres = all.genres.clone(),
        }
        assert_eq!(
            selected, expected,
            "only {block:?} and provider sections are read"
        );
    }
    let disabled = fixture
        .database
        .home_page(fixture.source, None, 0, 0, &[], &cancel)
        .await
        .unwrap();
    assert_eq!(
        disabled,
        HomePage {
            provider_sections: all.provider_sections.clone(),
            ..HomePage::default()
        }
    );
    let enabled_again = fixture
        .database
        .home_page(fixture.source, None, 0, 0, &HomeBlockKind::all(), &cancel)
        .await
        .unwrap();
    assert_eq!(
        enabled_again, all,
        "disabling presentation keeps stored Home facts"
    );
}

#[tokio::test]
async fn home_search_and_radio_results_stay_bounded() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let search = fixture
        .database
        .search(
            fixture.source,
            None,
            false,
            &SearchRequest::with_limit("note", 2),
            &cancel,
        )
        .await
        .expect("cached Search");
    assert_eq!(search.tracks.len(), 2);
    assert!(!search.tracks[0].links.artists.is_empty());
    assert!(search.albums.is_empty());
    let object_ids = search
        .tracks
        .iter()
        .rev()
        .map(|track| track.object_id.clone().expect("indexed track object"))
        .collect::<Vec<_>>();
    let resolved = fixture
        .database
        .search_rows_by_objects(fixture.source, None, false, &object_ids, &[], &[], &cancel)
        .await
        .expect("live Search identity rows");
    assert_eq!(
        resolved
            .tracks
            .iter()
            .map(|track| track.object_id.as_deref().expect("indexed track object"))
            .collect::<Vec<_>>(),
        object_ids.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert!(
        (&library::MemberRead {
            collection: library::Collection::Source(fixture.source),
            sort: Some(library::CatalogSort::Title),
            filter: ("artist a").into(),
            ..library::MemberRead::new(library::Collection::Source(fixture.source))
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .expect("complete Track filter")
            .count
            >= 2
    );
    let initial_home = fixture
        .database
        .home_page(
            fixture.source,
            None,
            0,
            0,
            &library::HomeBlockKind::all(),
            &cancel,
        )
        .await
        .expect("initial bounded Home page");
    assert!(initial_home.most_played.tracks.is_empty());
    assert_eq!(
        initial_home
            .provider_sections
            .iter()
            .find(|section| section.section_id == "featured")
            .expect("provider Home section")
            .rows
            .tracks
            .len(),
        1
    );
    let alternate_showcase_home = fixture
        .database
        .home_page(
            fixture.source,
            None,
            1,
            0,
            &library::HomeBlockKind::all(),
            &cancel,
        )
        .await
        .expect("alternate Showcase Home page");
    assert_eq!(initial_home.explore, alternate_showcase_home.explore);
    let alternate_showcase = alternate_showcase_home
        .showcase
        .expect("alternate Showcase");
    assert_eq!(
        initial_home
            .showcase
            .as_ref()
            .expect("initial Showcase")
            .album
            .album_key,
        fixture.albums[0]
    );
    assert_eq!(alternate_showcase.album.album_key, fixture.albums[1]);
    let alternate_explore_home = fixture
        .database
        .home_page(
            fixture.source,
            None,
            0,
            2,
            &library::HomeBlockKind::all(),
            &cancel,
        )
        .await
        .expect("alternate Explore Home page");
    assert_eq!(initial_home.showcase, alternate_explore_home.showcase);
    assert_eq!(initial_home.explore.len(), fixture.albums.len());
    assert_eq!(alternate_explore_home.explore.len(), fixture.albums.len());
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE albums SET first_seen_at=CASE album_key WHEN ?1 THEN 200 ELSE 100 END,metadata=jsonb(json_set(metadata,'$.date_added',NULL)) ")
        .bind(fixture.albums[0]).execute(&mut raw).await.expect("establish Local first-seen facts");
    sqlx::query("UPDATE albums SET added_sort=first_seen_at,input_metadata=jsonb(json_set(input_metadata,'$.date_added',NULL)),metadata=jsonb(json_set(metadata,'$.date_added',date(first_seen_at,'unixepoch')))")
        .execute(&mut raw)
        .await
        .unwrap();
    let albums = fixture
        .database
        .albums(&fixture.albums, true, &cancel)
        .await
        .unwrap();
    assert!(
        albums
            .iter()
            .all(|album| album.metadata.date_added.as_deref() == Some("1970-01-01"))
    );
    for descending in [false, true] {
        let sorted = library::ListRead {
            playlist_scope: Default::default(),
            kind: library::ListKind::Albums,
            source: Some(fixture.source),
            sort: library::CatalogSort::DateAdded,
            folder: None,
            filter: ("").to_string(),
            descending: descending,
            favorites_only: false,
            downloaded_only: false,
            activity: true,
        }
        .albums(&fixture.database, 0, 10, &cancel)
        .await
        .unwrap();
        let expected = if descending {
            fixture.albums.clone()
        } else {
            fixture.albums.iter().rev().copied().collect()
        };
        assert_eq!(
            sorted
                .iter()
                .map(|album| album.album_key)
                .collect::<Vec<_>>(),
            expected
        );
    }
    let newly_added = fixture
        .database
        .home_page(
            fixture.source,
            None,
            0,
            0,
            &library::HomeBlockKind::all(),
            &cancel,
        )
        .await
        .expect("bounded Home Albums")
        .newly_added
        .albums;
    assert_eq!(newly_added.len(), 2);
    assert_eq!(newly_added[0].album.album_key, fixture.albums[0]);
    sqlx::query("UPDATE albums SET metadata=jsonb(json_set(metadata,'$.release_date',NULL,'$.year',0)) WHERE album_key=?1")
        .bind(fixture.albums[1])
        .execute(&mut raw)
        .await
        .expect("remove release fact");
    sqlx::query("UPDATE albums SET release_sort=NULL,year_sort=0 WHERE album_key=?1")
        .bind(fixture.albums[1])
        .execute(&mut raw)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .database
            .home_page(
                fixture.source,
                None,
                0,
                0,
                &library::HomeBlockKind::all(),
                &cancel
            )
            .await
            .expect("released Albums require a release fact")
            .recently_released
            .albums
            .iter()
            .map(|row| row.album.album_key)
            .collect::<Vec<_>>(),
        [fixture.albums[0]]
    );
    assert_eq!(
        fixture
            .database
            .home_page(
                fixture.source,
                None,
                0,
                0,
                &library::HomeBlockKind::all(),
                &cancel
            )
            .await
            .expect("bounded Home Genres")
            .genres
            .len(),
        1
    );

    let random = fixture
        .database
        .random_candidates(
            fixture.source,
            None,
            &RandomCriteria {
                min_year: Some(2020),
                max_year: Some(2030),
                genre: Some(fixture.genre),
                played: PlayedFilter::All,
                require_media: true,
                variation: 7,
            },
            &[fixture.tracks[0]],
            2,
            &cancel,
        )
        .await
        .expect("bounded random candidates");
    assert_eq!(random.len(), 2);
    assert!(!random.contains(&fixture.tracks[0]));
    let radio = fixture
        .database
        .radio_candidates(
            fixture.source,
            RadioSeed::Genre(fixture.genre),
            &[],
            3,
            false,
            9,
            &cancel,
        )
        .await
        .expect("bounded Radio candidates");
    assert_eq!(radio.len(), 3);
    sqlx::query("INSERT INTO queue_entries(track_key,provenance) SELECT track_key,'\"Manual\"' FROM tracks WHERE media_uri=?1")
        .bind(&fixture.track_uris[0])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO queue_positions(order_kind,position,entry_id) SELECT kind,row_number() OVER(PARTITION BY kind ORDER BY entry_id)-1,entry_id FROM queue_entries CROSS JOIN (SELECT 0 kind UNION ALL SELECT 1) WHERE true ON CONFLICT(order_kind,position) DO UPDATE SET entry_id=excluded.entry_id")
        .execute(&mut raw).await.unwrap();
    assert_eq!(
        fixture
            .database
            .radio_candidates(
                fixture.source,
                RadioSeed::Genre(fixture.genre),
                &[],
                2,
                false,
                9,
                &cancel
            )
            .await
            .expect("complete queue exclusion")
            .len(),
        2
    );
    assert!(
        fixture
            .database
            .radio_candidates(
                fixture.source,
                RadioSeed::Genre(fixture.genre),
                &vec![fixture.tracks[1]; 501],
                2,
                false,
                9,
                &cancel
            )
            .await
            .is_err()
    );

    let album_radio = fixture
        .database
        .radio_candidates(
            fixture.source,
            RadioSeed::Album(fixture.albums[0]),
            &[],
            2,
            false,
            0,
            &cancel,
        )
        .await
        .expect("Album Radio context");
    assert!(
        album_radio
            .iter()
            .all(|track| fixture.tracks[2..].contains(track))
    );
    let playlist = fixture
        .database
        .create_playlist(
            Some(fixture.source),
            "Seed",
            &library::MemberSelection::Uris(((&[fixture.track_uris[0].clone()]).to_vec()).into()),
        )
        .await
        .expect("create Radio seed Playlist")
        .unwrap()
        .0;
    let playlist_radio = fixture
        .database
        .radio_candidates(
            fixture.source,
            RadioSeed::Playlist(playlist),
            &[],
            2,
            false,
            0,
            &cancel,
        )
        .await
        .expect("Playlist first-Track Radio context");
    assert!(!playlist_radio.contains(&fixture.tracks[0]));
    assert_ne!(playlist_radio, [fixture.tracks[0]]);
}
