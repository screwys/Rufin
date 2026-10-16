use library::{
    AlbumReleaseResult, CatalogSort, FavoriteTarget, ReadCancellation, RouteSeedWindow,
    SearchRequest,
};

use super::support::{connection, fixture};

#[tokio::test]
async fn album_disc_sections_follow_filtered_order_and_page_boundaries() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    for (index, disc) in [(0, 1), (1, 2), (2, 0)] {
        sqlx::query(
            "UPDATE tracks SET album_key=?1,metadata=jsonb(json_set(metadata,'$.disc_number',?2,'$.track_number',1)) WHERE track_key=?3",
        )
        .bind(fixture.albums[0])
        .bind(disc)
        .bind(fixture.tracks[index])
        .execute(&mut raw)
        .await
        .unwrap();
    }
    for (descending, expected) in [
        (false, vec![(0, 0), (1, 1), (2, 2)]),
        (true, vec![(0, 2), (1, 1), (2, 0)]),
    ] {
        let route = (&library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Album(fixture.albums[0]),
                sort: Some(CatalogSort::TrackNumber),
            },
            descending: descending,
            ..library::CatalogQuery::tracks(library::TrackScope::Album(fixture.albums[0]))
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .unwrap();
        assert_eq!(route.disc_sections, expected);
        assert_eq!(route.count, 3);
        assert_eq!(
            (match &route.rows {
                library::CatalogRows::Tracks(rows) => rows,
                library::CatalogRows::PlaylistEntries(_) => unreachable!(),
                _ => unreachable!(),
            })
            .len(),
            3
        );
        for offset in 0..3 {
            let rows = (&library::CatalogQuery {
                subject: library::CatalogSubject::Tracks {
                    scope: library::TrackScope::Album(fixture.albums[0]),
                    sort: Some(CatalogSort::TrackNumber),
                },
                descending: descending,
                ..library::CatalogQuery::tracks(library::TrackScope::Album(fixture.albums[0]))
            })
                .rows(&fixture.database, offset, 1, &cancel)
                .await
                .map(|rows| match rows {
                    library::CatalogRows::Tracks(rows) => rows,
                    _ => unreachable!("typed song projection"),
                })
                .unwrap();
            assert_eq!(
                (&route.query)
                    .disc_sections(&fixture.database, &cancel)
                    .await
                    .unwrap(),
                expected
            );
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].media_uri,
                (match &route.rows {
                    library::CatalogRows::Tracks(rows) => rows,
                    library::CatalogRows::PlaylistEntries(_) => unreachable!(),
                    _ => unreachable!(),
                })[offset]
                    .media_uri
            );
        }
    }
    for (filter, sort) in [
        ("Alpha", CatalogSort::TrackNumber),
        ("", CatalogSort::Title),
    ] {
        let route = (&library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Album(fixture.albums[0]),
                sort: Some(sort),
            },
            filter: (filter).into(),
            ..library::CatalogQuery::tracks(library::TrackScope::Album(fixture.albums[0]))
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .unwrap();
        assert!(route.disc_sections.is_empty());
    }
    // One known disc, even if numbered 2 or accompanied by unknown tracks, needs no heading.
    sqlx::query("UPDATE tracks SET metadata=jsonb(json_set(metadata,'$.disc_number',2)) WHERE album_key=?1 AND disc_number>0")
        .bind(fixture.albums[0])
        .execute(&mut raw)
        .await
        .unwrap();
    let detail = (&library::CatalogQuery {
        subject: library::CatalogSubject::Tracks {
            scope: library::TrackScope::Album(fixture.albums[0]),
            sort: Some(CatalogSort::TrackNumber),
        },
        ..library::CatalogQuery::tracks(library::TrackScope::Album(fixture.albums[0]))
    })
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .unwrap();
    assert!(detail.disc_sections.is_empty());
}

#[tokio::test]
async fn track_sorts_keep_nulls_last_and_title_ties_in_both_directions() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE tracks SET metadata=jsonb(json_set(metadata,'$.year',CASE object_id WHEN 'track-0' THEN NULL WHEN 'track-1' THEN 0 ELSE 2000 END)) ")
        .execute(&mut raw).await.unwrap();
    sqlx::query("DELETE FROM track_genres WHERE track_key=?1")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .unwrap();
    let empty_genre = sqlx::query_scalar::<_, library::GenreKey>(
        "INSERT INTO genres(source_key,object_id,metadata,media_uri,normalized_name) VALUES (?1,'empty',jsonb(json_object('name','','sort_text','')),'genre:'||COALESCE(?1,0)||':'||'empty',lower(COALESCE(json_extract(jsonb(json_object('name','','sort_text','')),'$.name'),''))) RETURNING genre_key",
    ).bind(fixture.source).fetch_one(&mut raw).await.unwrap();
    sqlx::query("UPDATE track_genres SET genre_key=?1 WHERE track_key=?2")
        .bind(empty_genre)
        .bind(fixture.tracks[1])
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    for sort in [CatalogSort::Year, CatalogSort::Genre] {
        for (descending, expected) in [(false, [1, 3, 2, 0]), (true, [3, 2, 1, 0])] {
            let page = (&library::CatalogQuery {
                subject: library::CatalogSubject::Tracks {
                    scope: library::TrackScope::Tracks(fixture.source),
                    sort: Some(sort),
                },
                descending: descending,
                ..library::CatalogQuery::tracks(library::TrackScope::Tracks(fixture.source))
            })
                .page(
                    &fixture.database,
                    RouteSeedWindow::top(),
                    &ReadCancellation::new(),
                )
                .await
                .unwrap();
            assert_eq!(
                (match &page.rows {
                    library::CatalogRows::Tracks(rows) => rows,
                    library::CatalogRows::PlaylistEntries(_) => unreachable!(),
                    _ => unreachable!(),
                })
                .into_iter()
                .map(|row| row.media_uri.clone())
                .collect::<Vec<_>>(),
                expected.map(|i| fixture.track_uris[i].clone()),
                "{sort:?}, descending={descending}"
            );
        }
    }
}

#[tokio::test]
async fn repeated_album_cards_keep_whole_counts_and_separate_source_activity() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::raw_sql("UPDATE sources SET kind='subsonic';\n      INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at)\n      VALUES('source','lifetime','track','track-0',7,0,100),('source','lifetime','track','track-1',99,0,900);\n      INSERT INTO native_media_state(media_uri,play_count,last_played) SELECT media_uri,3,200 FROM tracks WHERE object_id='track-1' ON CONFLICT(media_uri) DO UPDATE SET play_count=excluded.play_count,last_played=excluded.last_played;")
        .execute(&mut raw).await.unwrap();

    let cancel = ReadCancellation::new();
    let keys = [fixture.albums[0], fixture.albums[0]];
    let rows = {
        let mut query = library::CatalogQuery::keys(
            library::CatalogKind::Album,
            Some(fixture.source),
            (&keys).iter().map(|key| key.raw()).collect(),
        );
        query.activity = true;
        query.folder = None;
        query
            .rows(&fixture.database, 0, (&keys).len(), &cancel)
            .await
            .map(|rows| match rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!("typed catalog projection"),
            })
    }
    .unwrap();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.summary.track_count, 2);
        assert_eq!(row.state.play_count, 3);
        assert_eq!(row.state.last_played, Some(200));
        assert_eq!(row.links.album_artists.len(), 1);
    }
    sqlx::query("UPDATE sources SET kind='local'")
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_folders WHERE track_key=?1")
        .bind(fixture.tracks[1])
        .execute(&mut raw)
        .await
        .unwrap();
    let rows = {
        let mut query = library::CatalogQuery::keys(
            library::CatalogKind::Album,
            Some(fixture.source),
            (&keys).iter().map(|key| key.raw()).collect(),
        );
        query.activity = true;
        query.folder = Some(fixture.folder);
        query
            .rows(&fixture.database, 0, (&keys).len(), &cancel)
            .await
            .map(|rows| match rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!("typed catalog projection"),
            })
    }
    .unwrap();
    for row in &rows {
        assert_eq!(row.summary.track_count, 2);
        assert_eq!(row.state.play_count, 106);
        assert_eq!(row.state.last_played, Some(900));
    }
    let artist = {
        let mut query = library::CatalogQuery::keys(
            library::CatalogKind::Artist,
            Some(fixture.source),
            (&[fixture.artists[0]])
                .iter()
                .map(|key| key.raw())
                .collect(),
        );
        query.activity = true;
        query.folder = Some(fixture.folder);
        if let library::CatalogSubject::Keys { album_artist, .. } = &mut query.subject {
            *album_artist = false;
        }
        query
            .rows(&fixture.database, 0, (&[fixture.artists[0]]).len(), &cancel)
            .await
            .map(|rows| match rows {
                library::CatalogRows::Artists(rows) => rows,
                _ => unreachable!("typed catalog projection"),
            })
    }
    .unwrap();
    assert_eq!(artist[0].state.play_count, 106);
    assert_eq!(artist[0].state.last_played, Some(900));
}

#[tokio::test]
async fn named_card_batches_preserve_requested_rows_and_whole_cover_order() {
    let fixture = fixture().await;
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
                        .map(|id| (id, Some((&covers[index]).to_vec())))
                        .collect::<Vec<_>>(),
                )
                .await
                .unwrap();
        };
    }
    sqlx::query("UPDATE tracks SET metadata=jsonb(json_set(metadata,'$.date_added',CASE WHEN album_key=?1 THEN '2026-09-08' END)) ")
        .bind(fixture.albums[1])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_folders WHERE track_key IN (SELECT track_key FROM tracks WHERE album_key=?1)")
        .bind(fixture.albums[1]).execute(&mut raw).await.unwrap();
    drop(raw);
    let cancel = ReadCancellation::new();
    for (folder, count) in [(None, 4), (Some(fixture.folder), 4)] {
        let genres = {
            let mut query = library::CatalogQuery::keys(
                library::CatalogKind::Genre,
                Some(fixture.source),
                (&[
                    fixture.genre,
                    library::GenreKey::from_raw(-100),
                    fixture.genre,
                ])
                    .iter()
                    .map(|key| key.raw())
                    .collect(),
            );
            query.activity = true;
            query.folder = folder;
            query
                .rows(
                    &fixture.database,
                    0,
                    (&[
                        fixture.genre,
                        library::GenreKey::from_raw(-100),
                        fixture.genre,
                    ])
                        .len(),
                    &cancel,
                )
                .await
                .map(|rows| match rows {
                    library::CatalogRows::Genres(rows) => rows,
                    _ => unreachable!("typed catalog projection"),
                })
        }
        .unwrap();
        assert_eq!(genres.len(), 2);
        assert_eq!(genres[0], genres[1]);
        assert_eq!(genres[0].summary.track_count, count);
        assert_eq!(genres[0].representative_artwork, covers);
        let single = {
            let mut query = library::CatalogQuery::keys(
                library::CatalogKind::Genre,
                Some(fixture.source),
                (&[fixture.genre]).iter().map(|key| key.raw()).collect(),
            );
            query.activity = true;
            query.folder = folder;
            query
                .rows(&fixture.database, 0, (&[fixture.genre]).len(), &cancel)
                .await
                .map(|rows| match rows {
                    library::CatalogRows::Genres(rows) => rows,
                    _ => unreachable!("typed catalog projection"),
                })
        }
        .unwrap();
        assert_eq!(
            genres[0].summary.duration_millis,
            single[0].summary.duration_millis
        );
        let moods = {
            let mut query = library::CatalogQuery::keys(
                library::CatalogKind::Mood,
                Some(fixture.source),
                (&[fixture.mood, fixture.mood])
                    .iter()
                    .map(|key| key.raw())
                    .collect(),
            );
            query.activity = true;
            query.folder = folder;
            query
        }
        .rows(
            &fixture.database,
            0,
            (&[fixture.mood, fixture.mood]).len(),
            &cancel,
        )
        .await
        .map(|rows| match rows {
            library::CatalogRows::Moods(rows) => rows,
            _ => unreachable!(),
        })
        .unwrap();
        assert_eq!(moods.len(), 2);
        assert_eq!(moods[0], moods[1]);
        assert_eq!(moods[0].summary.track_count, count);
        assert_eq!(moods[0].representative_artwork, covers);
        let single = {
            let mut query = library::CatalogQuery::keys(
                library::CatalogKind::Mood,
                Some(fixture.source),
                (&[fixture.mood]).iter().map(|key| key.raw()).collect(),
            );
            query.activity = true;
            query.folder = folder;
            query
        }
        .rows(&fixture.database, 0, (&[fixture.mood]).len(), &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Moods(rows) => rows,
            _ => unreachable!(),
        })
        .unwrap();
        assert_eq!(
            moods[0].summary.duration_millis,
            single[0].summary.duration_millis
        );
    }
}

#[tokio::test]
async fn named_collection_lookups_retain_identity_when_selected_folder_is_empty() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut raw = connection(&fixture.path).await;
    sqlx::query("DELETE FROM track_folders")
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    let folder = Some(fixture.folder);
    let genre = {
        let mut query = library::CatalogQuery::keys(
            library::CatalogKind::Genre,
            Some(fixture.source),
            (&[fixture.genre]).iter().map(|key| key.raw()).collect(),
        );
        query.activity = true;
        query.folder = folder;
        query
            .rows(&fixture.database, 0, (&[fixture.genre]).len(), &cancel)
            .await
            .map(|rows| match rows {
                library::CatalogRows::Genres(rows) => rows,
                _ => unreachable!("typed catalog projection"),
            })
    }
    .unwrap()
    .pop()
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
    let mood = {
        let mut query = library::CatalogQuery::keys(
            library::CatalogKind::Mood,
            Some(fixture.source),
            (&[fixture.mood]).iter().map(|key| key.raw()).collect(),
        );
        query.activity = true;
        query.folder = folder;
        query
    }
    .rows(&fixture.database, 0, (&[fixture.mood]).len(), &cancel)
    .await
    .map(|rows| match rows {
        library::CatalogRows::Moods(rows) => rows,
        _ => unreachable!(),
    })
    .unwrap()
    .pop()
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
    let genre = {
        let mut query = library::CatalogQuery::keys(
            library::CatalogKind::Genre,
            Some(fixture.source),
            vec![(fixture.genre).raw()],
        );
        query.activity = true;
        query.folder = folder;
        query
    }
    .rows(&fixture.database, 0, 1, &cancel)
    .await
    .map(|rows| match rows {
        library::CatalogRows::Genres(mut rows) => rows.pop(),
        _ => unreachable!(),
    })
    .unwrap()
    .expect("existing genre detail");
    assert_eq!(
        (
            genre.summary.track_count,
            genre.summary.album_count,
            genre.summary.duration_millis
        ),
        (4, 2, 726000)
    );
    assert!(genre.representative_artwork.is_empty());
    let mood = {
        let mut query = library::CatalogQuery::keys(
            library::CatalogKind::Mood,
            Some(fixture.source),
            vec![(fixture.mood).raw()],
        );
        query.activity = true;
        query.folder = folder;
        query
    }
    .rows(&fixture.database, 0, 1, &cancel)
    .await
    .map(|rows| match rows {
        library::CatalogRows::Moods(mut rows) => rows.pop(),
        _ => unreachable!(),
    })
    .unwrap()
    .expect("existing mood detail");
    assert_eq!(
        (mood.summary.track_count, mood.summary.duration_millis),
        (4, 726000)
    );
    assert!(mood.representative_artwork.is_empty());
    for sort in [CatalogSort::Title, CatalogSort::TrackCount] {
        let (count, _, rows) = library::CatalogQuery {
            subject: library::CatalogSubject::Genres {
                source: fixture.source,
                sort: sort,
            },
            folder: folder,
            filter: ("").to_string(),
            descending: false,
            favorites_only: false,
            downloaded_only: false,
            activity: true,
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| {
            (
                page.count,
                page.first_row_position,
                match page.rows {
                    library::CatalogRows::Genres(rows) => rows,
                    _ => unreachable!(),
                },
            )
        })
        .unwrap();
        let order = rows.iter().map(|row| row.genre_key).collect::<Vec<_>>();
        assert_eq!(count, order.len());
        assert!(order.is_empty());
    }
    for sort in [CatalogSort::Title, CatalogSort::TrackCount] {
        let (count, _, rows) = library::CatalogQuery {
            subject: library::CatalogSubject::Moods {
                source: fixture.source,
                sort: sort,
            },
            folder: folder,
            filter: ("").to_string(),
            descending: false,
            favorites_only: false,
            downloaded_only: false,
            activity: true,
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| {
            (
                page.count,
                page.first_row_position,
                match page.rows {
                    library::CatalogRows::Moods(rows) => rows,
                    _ => unreachable!(),
                },
            )
        })
        .unwrap();
        let order = rows.iter().map(|row| row.mood_key).collect::<Vec<_>>();
        assert_eq!(count, order.len());
        assert!(order.is_empty());
    }
}

#[tokio::test]
async fn detail_folder_bounds_follow_collection_roles_and_selected_folder() {
    let fixture = fixture().await;
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
        sqlx::query("UPDATE tracks SET source_path=?1 WHERE track_key=?2")
            .bind(path)
            .bind(track)
            .execute(&mut raw)
            .await
            .unwrap();
    }
    // A guest appearance belongs to the artist view, not their album-artist view.
    sqlx::query("INSERT INTO track_artists(track_key,artist_key,position) VALUES(?1,?2,1)")
        .bind(fixture.tracks[2])
        .bind(fixture.artists[0])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_folders WHERE track_key<>?1")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    let database = &fixture.database;
    assert_eq!(
        database
            .collection_source_path_bounds(&fixture.album_uris[0], false, None)
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[1].clone()))
    );
    assert_eq!(
        database
            .collection_source_path_bounds(&fixture.artist_uris[0], false, None)
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[2].clone()))
    );
    assert_eq!(
        database
            .collection_source_path_bounds(&fixture.artist_uris[0], true, None)
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[1].clone()))
    );
    assert_eq!(
        database
            .collection_source_path_bounds(&fixture.artist_uris[0], false, Some(fixture.folder))
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[0].clone()))
    );
    assert_eq!(
        database
            .collection_source_path_bounds(&fixture.album_uris[1], false, Some(fixture.folder))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn collection_play_retains_full_order_with_bounded_queue_projection() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    for position in 0..130 {
        let id = format!("extra-{position:03}");
        let uri = library::source_entity_uri(&library::SourceId::new("source"), "track", &id);
        let key: library::TrackKey = sqlx::query_scalar(
            "INSERT INTO tracks(source_key,object_id,album_key,media_uri,metadata,normalized_search,indexed) VALUES (?1,?2,?3,?5,jsonb(json_object('title',COALESCE(?2,''),'album',COALESCE('Album A',''),'artist',COALESCE('Artist A',''),'sort_text',COALESCE(?2,''),'duration_millis',COALESCE(1000,0),'disc_number',COALESCE(1,0),'track_number',COALESCE(?4,0))),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE(?2,''),'album',COALESCE('Album A',''),'artist',COALESCE('Artist A',''),'sort_text',COALESCE(?2,''),'duration_millis',COALESCE(1000,0),'disc_number',COALESCE(1,0),'track_number',COALESCE(?4,0))),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE(?2,''),'album',COALESCE('Album A',''),'artist',COALESCE('Artist A',''),'sort_text',COALESCE(?2,''),'duration_millis',COALESCE(1000,0),'disc_number',COALESCE(1,0),'track_number',COALESCE(?4,0))),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE(?2,''),'album',COALESCE('Album A',''),'artist',COALESCE('Artist A',''),'sort_text',COALESCE(?2,''),'duration_millis',COALESCE(1000,0),'disc_number',COALESCE(1,0),'track_number',COALESCE(?4,0))),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE(?2,''),'album',COALESCE('Album A',''),'artist',COALESCE('Artist A',''),'sort_text',COALESCE(?2,''),'duration_millis',COALESCE(1000,0),'disc_number',COALESCE(1,0),'track_number',COALESCE(?4,0))),'$.comment'),'')),1) RETURNING track_key",
        ).bind(fixture.source).bind(&id).bind(fixture.albums[0]).bind(position + 10).bind(uri)
            .fetch_one(&mut raw).await.expect("extend collection");
        sqlx::query("INSERT INTO track_artists VALUES(?1,?2,0)")
            .bind(key)
            .bind(fixture.artists[0])
            .execute(&mut raw)
            .await
            .unwrap();
        sqlx::query("INSERT INTO track_genres VALUES(?1,?2,0)")
            .bind(key)
            .bind(fixture.genre)
            .execute(&mut raw)
            .await
            .unwrap();
        sqlx::query("INSERT INTO track_moods VALUES(?1,?2,0)")
            .bind(key)
            .bind(fixture.mood)
            .execute(&mut raw)
            .await
            .unwrap();
    }
    drop(raw);
    let cancel = ReadCancellation::new();
    let pages = [
        (&library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Album(fixture.albums[0]),
                sort: Some(CatalogSort::TrackNumber),
            },
            ..library::CatalogQuery::tracks(library::TrackScope::Album(fixture.albums[0]))
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .unwrap(),
        (&library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Artist {
                    key: fixture.artists[0],
                    album_artist: false,
                },
                sort: Some(CatalogSort::Title),
            },
            ..library::CatalogQuery::tracks(library::TrackScope::Artist {
                key: fixture.artists[0],
                album_artist: false,
            })
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .unwrap(),
        (&library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Artist {
                    key: fixture.artists[0],
                    album_artist: true,
                },
                sort: Some(CatalogSort::Title),
            },
            ..library::CatalogQuery::tracks(library::TrackScope::Artist {
                key: fixture.artists[0],
                album_artist: true,
            })
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .unwrap(),
        (&library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Genre(fixture.genre),
                sort: Some(CatalogSort::Title),
            },
            ..library::CatalogQuery::tracks(library::TrackScope::Genre(fixture.genre))
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .unwrap(),
        (&library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Mood(fixture.mood),
                sort: Some(CatalogSort::Title),
            },
            ..library::CatalogQuery::tracks(library::TrackScope::Mood(fixture.mood))
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .unwrap(),
    ];
    assert!(
        (match &pages[0].rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!(),
        })
        .windows(2)
        .all(|pair| pair[0].metadata.track_number < pair[1].metadata.track_number)
    );
    assert!(
        (match &pages[1].rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!(),
        })
        .windows(2)
        .all(|pair| pair[0].metadata.title.to_lowercase() <= pair[1].metadata.title.to_lowercase())
    );
    let collections = [
        library::TrackScope::Album(fixture.albums[0]),
        library::TrackScope::Artist {
            key: fixture.artists[0],
            album_artist: false,
        },
        library::TrackScope::Artist {
            key: fixture.artists[0],
            album_artist: true,
        },
        library::TrackScope::Genre(fixture.genre),
        library::TrackScope::Mood(fixture.mood),
    ];
    for (page, collection) in pages.into_iter().zip(collections) {
        let total = page.count;
        assert!(total > 100);
        assert!(
            (match &page.rows {
                library::CatalogRows::Tracks(rows) => rows,
                library::CatalogRows::PlaylistEntries(_) => unreachable!(),
                _ => unreachable!(),
            })
            .iter()
            .all(|row| row.source_key == Some(fixture.source))
        );
        assert_eq!(
            fixture
                .database
                .collection_source(&collection, &cancel)
                .await
                .unwrap(),
            Some(fixture.source)
        );
        let order = fixture
            .database
            .selected_track_uris(
                &library::CatalogQuery::tracks(collection.clone()).selection(),
                None,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(order.len(), total);
        assert_eq!(
            &order[page.first_row_position
                ..page.first_row_position
                    + (match &page.rows {
                        library::CatalogRows::Tracks(rows) => rows,
                        library::CatalogRows::PlaylistEntries(_) => unreachable!(),
                        _ => unreachable!(),
                    })
                    .len()],
            (match &page.rows {
                library::CatalogRows::Tracks(rows) => rows,
                library::CatalogRows::PlaylistEntries(_) => unreachable!(),
                _ => unreachable!(),
            })
            .iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>()
        );
        let state = super::support::resolve_queue(
            &fixture.database,
            library::QueueLoad::Tracks(vec![(
                library::CatalogQuery::tracks(collection).selection(),
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
                .database
                .selected_track_uris(
                    &library::CatalogQuery::keys(
                        library::CatalogKind::Track,
                        None,
                        (&state
                            .entries
                            .iter()
                            .map(|entry| entry.track_key)
                            .collect::<Vec<_>>())
                            .iter()
                            .map(|key| key.raw())
                            .collect()
                    )
                    .selection(),
                    None,
                    &ReadCancellation::new()
                )
                .await
                .unwrap(),
            order
        );
        assert!(state.rows.len() <= 100);
    }
    assert_eq!(
        (&library::CatalogQuery {
            folder: Some(fixture.folder),
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Album(fixture.albums[0]),
                sort: Some(CatalogSort::TrackNumber)
            },
            ..library::CatalogQuery::tracks(library::TrackScope::Album(fixture.albums[0]))
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .unwrap()
            .count,
        2
    );
    assert!(
        (match (&library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Album(library::AlbumKey::from_raw(999)),
                sort: Some(CatalogSort::TrackNumber)
            },
            ..library::CatalogQuery::tracks(library::TrackScope::Album(
                library::AlbumKey::from_raw(999)
            ))
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .unwrap()
            .rows
        {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!(),
        })
        .into_iter()
        .map(|row| row.media_uri.clone())
        .collect::<Vec<_>>()
        .is_empty()
    );
}

#[tokio::test]
async fn favorite_collection_orders_keep_each_favorite_entity_extent() {
    let fixture = fixture().await;
    let cancellation = ReadCancellation::new();
    fixture
        .database
        .set_favorite(&FavoriteTarget::Album(fixture.album_uris[0].clone()), true)
        .await
        .expect("favorite Album");
    fixture
        .database
        .set_favorite(
            &FavoriteTarget::Artist(fixture.artist_uris[1].clone()),
            true,
        )
        .await
        .expect("favorite Artist");

    assert_eq!(
        library::CatalogQuery {
            folder: None,
            filter: ("").to_string(),
            descending: false,
            favorites_only: true,
            subject: library::CatalogSubject::Albums {
                source: fixture.source,
                sort: CatalogSort::Title,
                inline_tracks: None
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancellation)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("favorite Album order")
        .2
        .into_iter()
        .map(|row| row.album_key)
        .collect::<Vec<_>>(),
        [fixture.albums[0]]
    );
    assert_eq!(
        library::CatalogQuery {
            folder: None,
            filter: ("").to_string(),
            descending: false,
            favorites_only: true,
            subject: library::CatalogSubject::Artists {
                source: fixture.source,
                sort: CatalogSort::Title,
                album_artist: false
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancellation)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Artists(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("favorite Artist order")
        .2
        .into_iter()
        .map(|row| row.artist_key)
        .collect::<Vec<_>>(),
        [fixture.artists[1]]
    );

    let rows = library::CatalogQuery {
        subject: library::CatalogSubject::Albums {
            source: fixture.source,
            sort: CatalogSort::Title,
            inline_tracks: Some(0),
        },
        folder: None,
        filter: ("").to_string(),
        descending: false,
        favorites_only: true,
        downloaded_only: false,
        activity: true,
    }
    .rows(&fixture.database, 0, 64, &cancellation)
    .await
    .map(|rows| match rows {
        library::CatalogRows::AlbumDetails(rows) => rows,
        _ => unreachable!(),
    })
    .expect("favorite Album detail page");
    assert_eq!(
        rows.iter()
            .filter_map(|row| match row {
                library::AlbumDetailRow::Album { album, .. } => Some(album.media_uri.as_str()),
                library::AlbumDetailRow::Track { .. } => None,
            })
            .collect::<Vec<_>>(),
        [fixture.album_uris[0].as_str()]
    );
    assert!(
        matches!(&rows[0], library::AlbumDetailRow::Album { album, .. } if !album.links.genres.is_empty())
    );
    assert_eq!(
        rows.iter()
            .filter_map(|row| match row {
                library::AlbumDetailRow::Track { track, .. } => Some(track.media_uri.as_str()),
                library::AlbumDetailRow::Album { .. } => None,
            })
            .collect::<Vec<_>>(),
        fixture.track_uris[..2]
    );
}

#[tokio::test]
async fn tracks_and_collections_keep_complete_orders_and_bounded_rows() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let title = fixture
        .database
        .selected_track_uris(
            &library::CatalogQuery {
                subject: library::CatalogSubject::Tracks {
                    scope: library::TrackScope::Tracks(fixture.source),
                    sort: Some(CatalogSort::Title),
                },
                ..library::CatalogQuery::tracks(library::TrackScope::Tracks(fixture.source))
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
        let selection = library::TrackSelection::Query {
            query: library::CatalogQuery::tracks(library::TrackScope::Tracks(fixture.source)),
            ranges,
        };
        assert_eq!(
            fixture
                .database
                .selected_track_uris(&selection, Some(2), &cancel)
                .await
                .unwrap(),
            expected,
        );
    }
    let combined = library::TrackSelection::Concat(vec![
        library::CatalogQuery::uris(
            library::CatalogKind::Track,
            vec![title[3].clone(), title[3].clone()],
        )
        .selection(),
        library::TrackSelection::Query {
            query: library::CatalogQuery::tracks(library::TrackScope::Tracks(fixture.source)),
            ranges: Some(vec![1..2, 3..4]),
        },
    ]);
    assert_eq!(
        fixture
            .database
            .selected_track_uris(&combined, Some(3), &cancel)
            .await
            .unwrap(),
        [title[3].clone(), title[3].clone(), title[1].clone()],
    );
    assert!(
        fixture
            .database
            .selected_track_uris(&combined, Some(0), &cancel)
            .await
            .unwrap()
            .is_empty()
    );
    let numbered = fixture
        .database
        .selected_track_uris(
            &library::CatalogQuery {
                subject: library::CatalogSubject::Tracks {
                    scope: library::TrackScope::Tracks(fixture.source),
                    sort: Some(CatalogSort::TrackNumber),
                },
                descending: true,
                ..library::CatalogQuery::tracks(library::TrackScope::Tracks(fixture.source))
            }
            .selection(),
            None,
            &cancel,
        )
        .await
        .expect("numbered Track order");
    assert_eq!(
        numbered,
        fixture.track_uris.iter().cloned().rev().collect::<Vec<_>>()
    );
    let favorites = fixture
        .database
        .selected_track_uris(
            &library::CatalogQuery {
                subject: library::CatalogSubject::Tracks {
                    scope: library::TrackScope::Tracks(fixture.source),
                    sort: Some(CatalogSort::Title),
                },
                favorites_only: true,
                ..library::CatalogQuery::tracks(library::TrackScope::Tracks(fixture.source))
            }
            .selection(),
            None,
            &cancel,
        )
        .await
        .expect("favorite Track order");
    assert_eq!(favorites.len(), 1);
    let folder = fixture
        .database
        .selected_track_uris(
            &library::CatalogQuery {
                subject: library::CatalogSubject::Tracks {
                    scope: library::TrackScope::Tracks(fixture.source),
                    sort: Some(CatalogSort::Title),
                },
                folder: Some(fixture.folder),
                ..library::CatalogQuery::tracks(library::TrackScope::Tracks(fixture.source))
            }
            .selection(),
            None,
            &cancel,
        )
        .await
        .expect("Folder Track order");
    assert_eq!(folder.len(), 4);

    let supplied = [title[2].clone(), title[0].clone()];
    let rows = library::CatalogQuery::uris(library::CatalogKind::Track, (&supplied).to_vec())
        .rows(&fixture.database, 0, (&supplied).len(), &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!("typed catalog projection"),
        })
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
    assert_eq!(
        library::CatalogQuery::uris(
            library::CatalogKind::Track,
            (&vec![title[0].clone(); 257]).to_vec()
        )
        .rows(
            &fixture.database,
            0,
            (&vec![title[0].clone(); 257]).len(),
            &cancel
        )
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!(),
        })
        .unwrap()
        .len(),
        256,
        "Track row window remains bounded"
    );
    assert_eq!(
        library::CatalogQuery {
            folder: None,
            filter: ("").to_string(),
            descending: false,
            favorites_only: false,
            subject: library::CatalogSubject::Albums {
                source: fixture.source,
                sort: CatalogSort::Title,
                inline_tracks: None
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("Album order")
        .2
        .into_iter()
        .map(|row| row.album_key)
        .collect::<Vec<_>>(),
        fixture.albums
    );
    assert_eq!(
        library::CatalogQuery {
            folder: None,
            filter: ("").to_string(),
            descending: true,
            favorites_only: false,
            subject: library::CatalogSubject::Albums {
                source: fixture.source,
                sort: CatalogSort::DateAdded,
                inline_tracks: None
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("sorted Album order")
        .2
        .into_iter()
        .map(|row| row.album_key)
        .collect::<Vec<_>>(),
        fixture.albums
    );
    assert_eq!(
        library::CatalogQuery {
            folder: None,
            filter: ("").to_string(),
            descending: false,
            favorites_only: false,
            subject: library::CatalogSubject::Artists {
                source: fixture.source,
                sort: CatalogSort::Title,
                album_artist: true
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Artists(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("Album Artist order")
        .2
        .into_iter()
        .map(|row| row.artist_key)
        .collect::<Vec<_>>(),
        fixture.artists
    );
    assert_eq!(
        library::CatalogQuery {
            folder: None,
            filter: ("").to_string(),
            descending: false,
            favorites_only: false,
            subject: library::CatalogSubject::Artists {
                source: fixture.source,
                sort: CatalogSort::Title,
                album_artist: false
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Artists(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("sorted Artist order")
        .2
        .into_iter()
        .map(|row| row.artist_key)
        .collect::<Vec<_>>(),
        fixture.artists
    );
    assert_eq!(
        library::CatalogQuery {
            folder: None,
            filter: ("").to_string(),
            descending: false,
            favorites_only: false,
            subject: library::CatalogSubject::Genres {
                source: fixture.source,
                sort: CatalogSort::Title
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Genres(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("Genre order")
        .2
        .into_iter()
        .map(|row| row.genre_key)
        .collect::<Vec<_>>(),
        [fixture.genre]
    );
    assert_eq!(
        library::CatalogQuery {
            folder: None,
            filter: ("").to_string(),
            descending: true,
            favorites_only: false,
            subject: library::CatalogSubject::Genres {
                source: fixture.source,
                sort: CatalogSort::TrackCount
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Genres(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("sorted Genre order")
        .2
        .into_iter()
        .map(|row| row.genre_key)
        .collect::<Vec<_>>(),
        [fixture.genre]
    );
    assert_eq!(
        library::CatalogQuery {
            folder: None,
            filter: ("").to_string(),
            descending: true,
            favorites_only: false,
            subject: library::CatalogSubject::Moods {
                source: fixture.source,
                sort: CatalogSort::Duration
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Moods(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("sorted Mood order")
        .2
        .into_iter()
        .map(|row| row.mood_key)
        .collect::<Vec<_>>(),
        [fixture.mood]
    );
    let album_rows = {
        let mut query = library::CatalogQuery::keys(
            library::CatalogKind::Album,
            Some(fixture.source),
            (&fixture.albums).iter().map(|key| key.raw()).collect(),
        );
        query.activity = true;
        query.folder = None;
        query
            .rows(&fixture.database, 0, (&fixture.albums).len(), &cancel)
            .await
            .map(|rows| match rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!("typed catalog projection"),
            })
    }
    .expect("bounded Album rows");
    assert_eq!(album_rows[0].summary.track_count, 2);
    assert_eq!(album_rows[0].links.album_artists[0].name, "Artist A");
    assert_eq!(album_rows[0].links.genres[0].name, "Rock");
    let album_detail = (&library::CatalogQuery {
        subject: library::CatalogSubject::Tracks {
            scope: library::TrackScope::Album(album_rows[0].album_key),
            sort: Some(library::CatalogSort::TrackNumber),
        },
        ..library::CatalogQuery::tracks(library::TrackScope::Album(album_rows[0].album_key))
    })
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .expect("Album detail");
    assert_eq!(album_detail.count, 2);
    let reversed = (&library::CatalogQuery {
        subject: library::CatalogSubject::Tracks {
            scope: library::TrackScope::Album(album_rows[0].album_key),
            sort: Some(library::CatalogSort::TrackNumber),
        },
        descending: true,
        ..library::CatalogQuery::tracks(library::TrackScope::Album(album_rows[0].album_key))
    })
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .expect("descending Album detail");
    assert_eq!(
        (match reversed.rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!(),
        }),
        (match &album_detail.rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!(),
        })
        .iter()
        .rev()
        .cloned()
        .collect::<Vec<_>>()
    );
    assert_eq!(
        album_rows[0]
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
        .database
        .album_release_candidates(fixture.source, 100, &cancel)
        .await
        .expect("Album release candidates");
    assert_eq!(candidates.len(), 2);
    let first = candidates
        .iter()
        .find(|candidate| candidate.album_key == fixture.albums[0])
        .unwrap();
    assert_eq!(first.lookup_identity, "release-group:group-a");
    let mut release_raw = connection(&fixture.path).await;
    let revision =
        sqlx::query_scalar::<_, i64>("SELECT catalog_revision FROM sources WHERE source_key=?1")
            .bind(fixture.source)
            .fetch_one(&mut release_raw)
            .await
            .unwrap();
    assert_eq!(
        fixture
            .database
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
    let missing_row = {
        let mut query = library::CatalogQuery::keys(
            library::CatalogKind::Album,
            Some(fixture.source),
            (&[first.album_key]).iter().map(|key| key.raw()).collect(),
        );
        query.activity = true;
        query.folder = None;
        query
            .rows(&fixture.database, 0, (&[first.album_key]).len(), &cancel)
            .await
            .map(|rows| match rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!("typed catalog projection"),
            })
    }
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(missing_row.metadata.release_types, ["Album".to_string()]);
    assert_eq!(
        missing_row.release_lookup_identity.as_deref(),
        Some("release-group:group-a")
    );

    let mut scan_catalog = fixture.database.catalog("source", None).await.unwrap();
    {
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

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    fixture.database.observe_catalog(move |_, update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    {
        let update = scan_catalog.into_changes();
        (&fixture.database)
            .publish_catalog_update("source", update)
            .await;
    };
    let update = updates.try_recv().unwrap();
    assert!(update.metadata);
    let changed = fixture
        .database
        .album_release_candidates(fixture.source, 100, &cancel)
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.album_key == first.album_key)
        .unwrap();
    assert_eq!(changed.lookup_identity, "release-group:group-a2");
    assert_eq!(
        fixture
            .database
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
            .database
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
    let found = {
        let mut query = library::CatalogQuery::keys(
            library::CatalogKind::Album,
            Some(fixture.source),
            (&[changed.album_key]).iter().map(|key| key.raw()).collect(),
        );
        query.activity = true;
        query.folder = None;
        query
            .rows(&fixture.database, 0, (&[changed.album_key]).len(), &cancel)
            .await
            .map(|rows| match rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!("typed catalog projection"),
            })
    }
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(
        found.metadata.release_types,
        ["Compilation".to_string(), "Remix".to_string()]
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT catalog_revision FROM sources WHERE source_key=?1")
            .bind(fixture.source)
            .fetch_one(&mut release_raw)
            .await
            .unwrap(),
        revision + 1 // The metadata edit publishes once; release annotations do not republish it.
    );

    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE track_artists SET artist_key=?3 WHERE track_key IN (?1,?2)")
        .bind(fixture.tracks[0])
        .bind(fixture.tracks[1])
        .bind(fixture.artists[1])
        .execute(&mut raw)
        .await
        .expect("separate direct Track Artist from Album Artist");

    let album_artist_uri: String =
        sqlx::query_scalar("SELECT media_uri FROM artists WHERE artist_key=?1")
            .bind(fixture.artists[0])
            .fetch_one(&mut raw)
            .await
            .unwrap();
    let resolved = library::CatalogQuery::uris(
        library::CatalogKind::Artist,
        vec![(&album_artist_uri).to_string()],
    )
    .rows(&fixture.database, 0, 1, &cancel)
    .await
    .map(|rows| match rows {
        library::CatalogRows::Artists(mut rows) => rows.pop(),
        _ => unreachable!("typed catalog projection"),
    })
    .expect("resolve an artist credited only on albums")
    .unwrap();
    assert_eq!(resolved.artist_key, fixture.artists[0]);
    assert_eq!(resolved.source_key, fixture.source);
    let detail = {
        let mut query = library::CatalogQuery::keys(
            library::CatalogKind::Artist,
            Some(fixture.source),
            (&[resolved.artist_key])
                .iter()
                .map(|key| key.raw())
                .collect(),
        );
        query.activity = true;
        query.folder = None;
        if let library::CatalogSubject::Keys { album_artist, .. } = &mut query.subject {
            *album_artist = false;
        }
        query
            .rows(
                &fixture.database,
                0,
                (&[resolved.artist_key]).len(),
                &cancel,
            )
            .await
            .map(|rows| match rows {
                library::CatalogRows::Artists(rows) => rows,
                _ => unreachable!("typed catalog projection"),
            })
    }
    .expect("known artist can open with no tracks in the requested role")
    .pop()
    .unwrap();
    assert_eq!(detail.artist_key, resolved.artist_key);
    assert_eq!(detail.summary.track_count, 0);
    assert_eq!(
        (&library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Artist {
                    key: fixture.artists[0],
                    album_artist: true
                },
                sort: Some(CatalogSort::Title)
            },
            ..library::CatalogQuery::tracks(library::TrackScope::Artist {
                key: fixture.artists[0],
                album_artist: true
            })
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .expect("Album Artist Tracks")
            .count,
        2
    );
    assert_eq!(
        (&library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Artist {
                    key: fixture.artists[0],
                    album_artist: false
                },
                sort: Some(CatalogSort::Title)
            },
            ..library::CatalogQuery::tracks(library::TrackScope::Artist {
                key: fixture.artists[0],
                album_artist: false
            })
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .expect("direct Artist Tracks")
            .count,
        0
    );
    assert_eq!(
        {
            let mut query = library::CatalogQuery::keys(
                library::CatalogKind::Artist,
                Some(fixture.source),
                (&[fixture.artists[0]])
                    .iter()
                    .map(|key| key.raw())
                    .collect(),
            );
            query.activity = true;
            query.folder = None;
            if let library::CatalogSubject::Keys { album_artist, .. } = &mut query.subject {
                *album_artist = true;
            }
            query
        }
        .rows(&fixture.database, 0, (&[fixture.artists[0]]).len(), &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Artists(rows) => rows,
            _ => unreachable!(),
        })
        .expect("Album Artist row")[0]
            .summary
            .track_count,
        2
    );
    assert_eq!(
        {
            let mut query = library::CatalogQuery::keys(
                library::CatalogKind::Artist,
                Some(fixture.source),
                (&[fixture.artists[0]])
                    .iter()
                    .map(|key| key.raw())
                    .collect(),
            );
            query.activity = true;
            query.folder = None;
            if let library::CatalogSubject::Keys { album_artist, .. } = &mut query.subject {
                *album_artist = false;
            }
            query
        }
        .rows(&fixture.database, 0, (&[fixture.artists[0]]).len(), &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Artists(rows) => rows,
            _ => unreachable!(),
        })
        .expect("known artist without direct track credits")[0]
            .summary
            .track_count,
        0
    );
    assert_eq!(
        {
            let mut query = library::CatalogQuery::keys(
                library::CatalogKind::Artist,
                Some(fixture.source),
                (&[fixture.artists[0]])
                    .iter()
                    .map(|key| key.raw())
                    .collect(),
            );
            query.activity = true;
            query.folder = None;
            if let library::CatalogSubject::Keys { album_artist, .. } = &mut query.subject {
                *album_artist = true;
            }
            query
        }
        .rows(&fixture.database, 0, (&[fixture.artists[0]]).len(), &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Artists(rows) => rows,
            _ => unreachable!(),
        })
        .expect("Album Artist detail")
        .pop()
        .unwrap()
        .summary
        .track_count,
        2
    );
    assert_eq!(
        {
            let mut query = library::CatalogQuery::keys(
                library::CatalogKind::Genre,
                Some(fixture.source),
                vec![(fixture.genre).raw()],
            );
            query.activity = true;
            query.folder = Some(fixture.folder);
            query
        }
        .rows(&fixture.database, 0, 1, &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Genres(mut rows) => rows.pop(),
            _ => unreachable!(),
        })
        .expect("scoped Genre detail")
        .unwrap()
        .summary
        .track_count,
        4
    );
    assert_eq!(
        (match (&library::CatalogQuery {
            folder: Some(fixture.folder),
            filter: ("beta").into(),
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Album(fixture.albums[0]),
                sort: Some(CatalogSort::Title)
            },
            ..library::CatalogQuery::tracks(library::TrackScope::Album(fixture.albums[0]))
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .expect("scoped filtered Album Tracks")
            .rows
        {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!(),
        })
        .into_iter()
        .map(|row| row.media_uri.clone())
        .collect::<Vec<_>>(),
        [fixture.track_uris[1].clone()]
    );
    assert_eq!(
        (&library::CatalogQuery {
            descending: true,
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Album(fixture.albums[0]),
                sort: Some(CatalogSort::LastPlayed)
            },
            ..library::CatalogQuery::tracks(library::TrackScope::Album(fixture.albums[0]))
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .expect("Activity-sorted Album Tracks")
            .count,
        2
    );
    assert_eq!(
        (&library::CatalogQuery {
            folder: Some(fixture.folder),
            filter: ("artist b").into(),
            descending: true,
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Genre(fixture.genre),
                sort: Some(CatalogSort::Album)
            },
            ..library::CatalogQuery::tracks(library::TrackScope::Genre(fixture.genre))
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .expect("scoped filtered Genre Tracks")
            .count,
        2
    );
    assert_eq!(
        (match (&library::CatalogQuery {
            filter: ("2023").into(),
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Mood(fixture.mood),
                sort: Some(CatalogSort::Year)
            },
            ..library::CatalogQuery::tracks(library::TrackScope::Mood(fixture.mood))
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .expect("filtered Mood Tracks")
            .rows
        {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!(),
        })
        .into_iter()
        .map(|row| row.media_uri.clone())
        .collect::<Vec<_>>(),
        [fixture.track_uris[3].clone()]
    );
    assert!(
        library::CatalogQuery {
            folder: None,
            filter: ("artist a").to_string(),
            descending: false,
            favorites_only: false,
            subject: library::CatalogSubject::Artists {
                source: fixture.source,
                sort: CatalogSort::Title,
                album_artist: false
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Artists(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("direct Artist filter")
        .2
        .into_iter()
        .map(|row| row.artist_key)
        .collect::<Vec<_>>()
        .is_empty()
    );
    assert_eq!(
        library::CatalogQuery {
            folder: None,
            filter: ("artist a").to_string(),
            descending: false,
            favorites_only: false,
            subject: library::CatalogSubject::Artists {
                source: fixture.source,
                sort: CatalogSort::Title,
                album_artist: true
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Artists(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("Album Artist filter")
        .2
        .into_iter()
        .map(|row| row.artist_key)
        .collect::<Vec<_>>(),
        [fixture.artists[0]]
    );
    assert_eq!(
        fixture
            .database
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
            .map(|artist| artist.metadata.name)
            .collect::<Vec<_>>(),
        ["Artist B"]
    );
    assert_eq!(
        fixture
            .database
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
        {
            let mut query = library::CatalogQuery::keys(
                library::CatalogKind::Mood,
                Some(fixture.source),
                vec![(fixture.mood).raw()],
            );
            query.activity = true;
            query.folder = Some(fixture.folder);
            query
        }
        .rows(&fixture.database, 0, 1, &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Moods(mut rows) => rows.pop(),
            _ => unreachable!(),
        })
        .expect("scoped Mood detail")
        .unwrap()
        .summary
        .track_count,
        4
    );
    let empty_folder = sqlx::query_scalar("INSERT INTO folders(source_key,object_id,metadata,media_uri,normalized_name) VALUES (?1,'empty-folder',jsonb(json_object('name','Empty','sort_text','empty')),'folder:'||COALESCE(?1,0)||':'||'empty-folder',lower(COALESCE(json_extract(jsonb(json_object('name','Empty','sort_text','empty')),'$.name'),''))) RETURNING folder_key")
        .bind(fixture.source).fetch_one(&mut raw).await.expect("insert empty Folder");
    assert!(
        library::CatalogQuery {
            folder: Some(empty_folder),
            filter: ("").to_string(),
            descending: false,
            favorites_only: false,
            subject: library::CatalogSubject::Albums {
                source: fixture.source,
                sort: CatalogSort::Title,
                inline_tracks: None
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("empty Folder scope")
        .2
        .into_iter()
        .map(|row| row.album_key)
        .collect::<Vec<_>>()
        .is_empty()
    );
}

#[tokio::test]
async fn empty_named_collection_routes_return_empty_pages() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    for statement in [
        "DELETE FROM track_genres",
        "DELETE FROM album_genres",
        "DELETE FROM genres",
        "DELETE FROM track_moods",
        "DELETE FROM moods",
    ] {
        sqlx::query(statement).execute(&mut raw).await.unwrap();
    }
    drop(raw);
    let source = fixture.source;
    let cancellation = ReadCancellation::new();

    let genres = library::CatalogQuery {
        subject: library::CatalogSubject::Genres {
            source: source,
            sort: CatalogSort::TrackCount,
        },
        folder: None,
        filter: ("").to_string(),
        descending: false,
        favorites_only: false,
        downloaded_only: false,
        activity: true,
    }
    .page(&fixture.database, RouteSeedWindow::top(), &cancellation)
    .await
    .map(|page| {
        (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Genres(rows) => rows,
                _ => unreachable!(),
            },
        )
    })
    .expect("empty Genre route");
    let moods = library::CatalogQuery {
        subject: library::CatalogSubject::Moods {
            source: source,
            sort: CatalogSort::Duration,
        },
        folder: None,
        filter: ("").to_string(),
        descending: false,
        favorites_only: false,
        downloaded_only: false,
        activity: true,
    }
    .page(&fixture.database, RouteSeedWindow::top(), &cancellation)
    .await
    .map(|page| {
        (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Moods(rows) => rows,
                _ => unreachable!(),
            },
        )
    })
    .expect("empty Mood route");

    assert_eq!(genres, (0, 0, Vec::new()));
    assert_eq!(moods, (0, 0, Vec::new()));
}

#[tokio::test]
async fn metadata_refresh_publishes_changes_and_leaves_no_ops_unchanged() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let track = fixture.tracks[0];
    for refresh in 0..2 {
        let mut scan_catalog = fixture.database.catalog("source", None).await.unwrap();
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
                    loudness_analysis_key: [1; 32],
                    ..Default::default()
                }],
                ..Default::default()
            };

            scan_catalog.apply_catalog(&write, true).await.unwrap();
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

            scan_catalog.apply_catalog(&write, true).await.unwrap();
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

            scan_catalog.apply_catalog(&write, true).await.unwrap();
        };
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        fixture.database.observe_catalog(move |_, update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        {
            let update = scan_catalog.into_changes();
            (&fixture.database)
                .publish_catalog_update("source", update)
                .await;
        };
        let update = updates
            .try_recv()
            .map(|update| {
                assert!(update.changed() || !update.track_rekeys.is_empty());
                update
            })
            .unwrap_or_default();
        assert_eq!(update.metadata, refresh == 0);
        if refresh == 1 {
            assert!(!update.playlists && !update.artwork && !update.home);
        }
    }
    let row = library::CatalogQuery::keys(
        library::CatalogKind::Track,
        None,
        (&[track]).iter().map(|key| key.raw()).collect(),
    )
    .rows(&fixture.database, 0, (&[track]).len(), &cancel)
    .await
    .map(|rows| match rows {
        library::CatalogRows::Tracks(rows) => rows,
        _ => unreachable!("typed catalog projection"),
    })
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(row.metadata.title, "Edited");
    assert_eq!(row.metadata.comment.as_deref(), Some("Changed"));
    let filtered = (match (&library::CatalogQuery {
        subject: library::CatalogSubject::Tracks {
            scope: library::TrackScope::Tracks(fixture.source),
            sort: Some(library::CatalogSort::Title),
        },
        filter: ("changed").into(),
        ..library::CatalogQuery::tracks(library::TrackScope::Tracks(fixture.source))
    })
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .expect("filter edited Track")
        .rows
    {
        library::CatalogRows::Tracks(rows) => rows,
        _ => unreachable!(),
    })
    .into_iter()
    .map(|row| row.media_uri.clone())
    .collect::<Vec<_>>();
    assert_eq!(filtered, [fixture.track_uris[0].clone()]);
    assert_eq!(
        (match (&library::CatalogQuery {
            filter: ("2025").into(),
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Tracks(fixture.source),
                sort: Some(library::CatalogSort::Title)
            },
            ..library::CatalogQuery::tracks(library::TrackScope::Tracks(fixture.source))
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .expect("Track Year filter")
            .rows
        {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!(),
        })
        .into_iter()
        .map(|row| row.media_uri.clone())
        .collect::<Vec<_>>(),
        [fixture.track_uris[0].clone()]
    );
    assert_eq!(
        library::CatalogQuery {
            folder: None,
            filter: ("rock").to_string(),
            descending: false,
            favorites_only: false,
            subject: library::CatalogSubject::Albums {
                source: fixture.source,
                sort: CatalogSort::Title,
                inline_tracks: None
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("Album Genre filter")
        .2
        .into_iter()
        .map(|row| row.album_key)
        .collect::<Vec<_>>(),
        [fixture.albums[1], fixture.albums[0]]
    );
    assert_eq!(
        library::CatalogQuery {
            folder: None,
            filter: ("artist b").to_string(),
            descending: false,
            favorites_only: false,
            subject: library::CatalogSubject::Albums {
                source: fixture.source,
                sort: CatalogSort::Title,
                inline_tracks: None
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("Album Artist filter")
        .2
        .into_iter()
        .map(|row| row.album_key)
        .collect::<Vec<_>>(),
        [fixture.albums[1]]
    );
    assert_eq!(
        library::CatalogQuery {
            folder: None,
            filter: ("2024").to_string(),
            descending: false,
            favorites_only: false,
            subject: library::CatalogSubject::Albums {
                source: fixture.source,
                sort: CatalogSort::Title,
                inline_tracks: None
            },
            activity: true,
            downloaded_only: false
        }
        .page(&fixture.database, RouteSeedWindow::top(), &cancel)
        .await
        .map(|page| (
            page.count,
            page.first_row_position,
            match page.rows {
                library::CatalogRows::Albums(rows) => rows,
                _ => unreachable!(),
            }
        ))
        .expect("Album Year filter")
        .2
        .into_iter()
        .map(|row| row.album_key)
        .collect::<Vec<_>>(),
        [fixture.albums[1]]
    );
}

#[tokio::test]
async fn album_genres_supply_browsing_and_playback_without_changing_track_tags() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("DELETE FROM album_genres")
        .execute(&mut raw)
        .await
        .unwrap();
    // Keep one track credit overlapping the album credit to check set semantics.
    sqlx::query("DELETE FROM track_genres WHERE track_key<>?1")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query(
        "INSERT OR REPLACE INTO album_genres(album_key,genre_key,position) VALUES(?1,?2,0)",
    )
    .bind(fixture.albums[0])
    .bind(fixture.genre)
    .execute(&mut raw)
    .await
    .unwrap();
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
            .database
            .catalog("source", None)
            .await
            .unwrap()
            .set_source_artwork(
                "album",
                &objects
                    .into_iter()
                    .map(|id| (id, Some((&cover).to_vec())))
                    .collect::<Vec<_>>(),
            )
            .await
            .unwrap();
    };

    let cancel = ReadCancellation::new();
    for folder in [None, Some(fixture.folder)] {
        let detail = {
            let mut query = library::CatalogQuery::keys(
                library::CatalogKind::Genre,
                Some(fixture.source),
                vec![(fixture.genre).raw()],
            );
            query.activity = true;
            query.folder = folder;
            query
        }
        .rows(&fixture.database, 0, 1, &cancel)
        .await
        .map(|rows| match rows {
            library::CatalogRows::Genres(mut rows) => rows.pop(),
            _ => unreachable!(),
        })
        .unwrap()
        .unwrap();
        assert_eq!(detail.summary.track_count, 2);
        assert_eq!(detail.summary.album_count, 1);
        assert_eq!(
            detail.representative_artwork.as_slice(),
            std::slice::from_ref(&cover)
        );
        let route = (&library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Genre(fixture.genre),
                sort: Some(CatalogSort::Title),
            },
            folder: folder,
            ..library::CatalogQuery::tracks(library::TrackScope::Genre(fixture.genre))
        })
            .page(&fixture.database, RouteSeedWindow::top(), &cancel)
            .await
            .unwrap();
        assert_eq!(route.count, 2);
        let tracks = (&library::CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope: library::TrackScope::Genre(fixture.genre),
                sort: Some(CatalogSort::Title),
            },
            folder: folder,
            ..library::CatalogQuery::tracks(library::TrackScope::Genre(fixture.genre))
        })
            .rows(&fixture.database, 0, 10, &cancel)
            .await
            .map(|rows| match rows {
                library::CatalogRows::Tracks(rows) => rows,
                _ => unreachable!("typed song projection"),
            })
            .unwrap();
        assert_eq!(tracks.len(), 2);
        assert!(tracks.iter().any(|track| track.links.genres.is_empty()));
        let home = fixture
            .database
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
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_genres")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1
    );
}
