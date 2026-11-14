use std::path::Path;

use library::{Catalog, Freshness, ReadCancellation};
use sqlx::sqlite::SqliteConnection;

#[tokio::test]
async fn publication_keeps_tracks_and_known_credits_beside_missing_related_objects() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.unwrap();

    let mut scan_observation = async {
        if {
            (&catalog).loaded().await;
            (&catalog).source_key(&library::SourceId::new("source"))
        }
        .is_none()
        {
            (&catalog)
                .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
                .await?;
        }
        catalog.observe("source").await
    }
    .await
    .unwrap();
    scan_observation.start_observation().await.unwrap();
    {
        let write = library::CatalogBatch {
            artists: vec![library::ArtistObservation {
                metadata: library::ArtistMetadata {
                    name: "Known Artist".into(),
                    sort_text: Some("known artist".into()),
                    ..Default::default()
                },
                object_id: "artist-one".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    let mut tracks = [
        ("track-one", "One"),
        ("track-two", "Two"),
        ("track-one", "Duplicate"),
    ]
    .into_iter()
    .map(|(object_id, title)| library::TrackObservation {
        metadata: library::TrackMetadata {
            title: title.into(),
            sort_text: title.to_lowercase(),
            album_loudness: Some(library::NativeGain {
                replay_gain_db: Some(-6.0),
                ..Default::default()
            }),
            artists: vec![library::ArtistCredit {
                object_id: "artist-one".into(),
                name: "Known Artist".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        object_id: object_id.into(),
        album_object_id: Some("album-one".into()),
        ..Default::default()
    })
    .collect::<Vec<_>>();
    tracks[0].metadata.artists.clear();
    tracks[2].metadata.source_loudness = Some(library::NativeGain {
        replay_gain_db: Some(-4.0),
        ..Default::default()
    });
    {
        let write = library::CatalogBatch {
            tracks: tracks.to_vec(),
            ..Default::default()
        };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch {
            tracks: vec![tracks[2].clone()],
            ..Default::default()
        };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    scan_observation
        .complete_source_observation(
            &[
                "track", "album", "artist", "genre", "mood", "folder", "playlist",
            ],
            None,
            None,
        )
        .await
        .unwrap();
    scan_observation
        .complete_source_observation(&[], None, None)
        .await
        .unwrap();
    assert!(scan_observation.changed());
    let mut raw = connection(&path).await;
    assert_eq!(
        sqlx::query_as::<_, (String, f64)>(
            "SELECT title,json_extract(details.metadata,'$.source_loudness.replay_gain_db') FROM tracks JOIN track_details details USING(track_key) WHERE object_id='track-one'",
        )
        .fetch_one(&mut raw)
        .await
        .unwrap(),
        ("Duplicate".into(), -4.0)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_links WHERE role=0")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks WHERE album_key IS NOT NULL")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn reopening_preserves_accepted_source_rows_and_noop_refresh() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.unwrap();
    let mut changes = super::support::Changes::of(&catalog);
    let publication = write_small_catalog(&catalog, &mut changes, "same", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/47f24da63989b3586adaa7682d88f1bb","image":{"kind":"native","source_id":"source","item_id":"art","tag":null},"blur_hash":null}]"#).await;
    assert!(publication.1.intersects(super::support::Shown::CATALOG));
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&path).await.unwrap();
    let mut changes = super::support::Changes::of(&reopened);
    assert_eq!(
        reopened.all_source_counts().await.unwrap()[&reopened.source_id(publication.0).unwrap()],
        (1, 2)
    );
    let update = write_small_catalog(&reopened, &mut changes, "same", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/47f24da63989b3586adaa7682d88f1bb","image":{"kind":"native","source_id":"source","item_id":"art","tag":null},"blur_hash":null}]"#).await;
    assert!(!super::support::shown(update.1));
    reopened.close().await.unwrap();
}

async fn connection(path: &Path) -> SqliteConnection {
    super::support::connection(path).await
}

#[tokio::test]
async fn artist_names_and_sort_names_survive_sparse_credits_and_reset_with_complete_records() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.unwrap();
    catalog
        .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
        .await
        .unwrap();
    let mut write = library::CatalogBatch::default();
    {
        write.artists.extend_from_slice(&[
            library::ArtistObservation {
                metadata: library::ArtistMetadata {
                    name: "The Cure".into(),
                    sort_text: Some("cure, the".into()),
                    ..Default::default()
                },
                object_id: "artist".into(),
                ..Default::default()
            },
            library::ArtistObservation {
                metadata: library::ArtistMetadata {
                    name: "Missing Sort".into(),
                    ..Default::default()
                },
                object_id: "fallback".into(),
                ..Default::default()
            },
        ]);
        write.artists.len()
    };
    {
        write.tracks.push(library::TrackObservation {
            metadata: library::TrackMetadata {
                title: "Track".into(),
                artists: vec![library::ArtistCredit {
                    object_id: "artist".into(),
                    name: "the cure".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            object_id: "track".into(),
            ..Default::default()
        });
        write.tracks.len()
    };
    {
        let result = catalog
            .observe("source")
            .await
            .unwrap()
            .apply_catalog(&write, true)
            .await
            .unwrap();
        result
    };
    let mut reader = connection(&path).await;
    assert_eq!(
        sqlx::query_as::<_, (String, String)>(
            "SELECT name,coalesce(sort_text,lower(name)) FROM artists ORDER BY 2"
        )
        .fetch_all(&mut reader)
        .await
        .unwrap(),
        [
            ("The Cure".into(), "cure, the".into()),
            ("Missing Sort".into(), "missing sort".into())
        ]
    );
    for sort in [None, Some("the cure"), Some("cure, the")] {
        let mut write = library::CatalogBatch::default();
        {
            write.artists.push(library::ArtistObservation {
                metadata: library::ArtistMetadata {
                    name: "The Cure".into(),
                    sort_text: sort.map(str::to_owned),
                    ..Default::default()
                },
                object_id: "artist".into(),
                ..Default::default()
            });
            write.artists.len()
        };
        {
            write.tracks.push(library::TrackObservation {
                metadata: library::TrackMetadata {
                    title: "Track".into(),
                    artists: vec![library::ArtistCredit {
                        object_id: "artist".into(),
                        name: "the cure".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                object_id: "track".into(),
                ..Default::default()
            });
            write.tracks.len()
        };
        {
            let result = catalog
                .observe("source")
                .await
                .unwrap()
                .apply_catalog(&write, true)
                .await
                .unwrap();
            result
        };
        assert_eq!(
            sqlx::query_as::<_, (String, String)>(
                "SELECT name,coalesce(sort_text,lower(name)) FROM artists WHERE object_id='artist'"
            )
            .fetch_one(&mut reader)
            .await
            .unwrap(),
            ("The Cure".into(), sort.unwrap_or("the cure").into())
        );
    }
}

#[tokio::test]
async fn sparse_artist_credits_preserve_metadata_and_complete_records_can_clear_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.unwrap();
    catalog
        .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
        .await
        .unwrap();
    let mut write = library::CatalogBatch::default();
    {
        write.artists.push(library::ArtistObservation {
            user_data: library::NativeUserState {
                favorite: Some(false),
                rating: (Some(Some(8))).map(|rating| rating.map(|value| value * 10)),
                ..Default::default()
            },
            metadata: library::ArtistMetadata {
                name: "Artist".into(),
                sort_text: Some("artist".into()),
                musicbrainz_artist_id: Some("mbid".into()),
                ..Default::default()
            },
            object_id: "artist".into(),
            ..Default::default()
        });
        write.artists.len()
    };
    {
        let result = catalog
            .observe("source")
            .await
            .unwrap()
            .apply_catalog(&write, true)
            .await
            .unwrap();
        result
    };
    let mut reader = connection(&path).await;
    for complete in [false, true] {
        let mut write = library::CatalogBatch::default();
        if complete {
            {
                write.artists.push(library::ArtistObservation {
                    metadata: library::ArtistMetadata {
                        name: "Artist".into(),
                        ..Default::default()
                    },
                    object_id: "artist".into(),
                    ..Default::default()
                });
                write.artists.len()
            };
        } else {
            {
                write.tracks.push(library::TrackObservation {
                    metadata: library::TrackMetadata {
                        title: "Track".into(),
                        artists: vec![library::ArtistCredit {
                            object_id: "artist".into(),
                            name: "Artist".into(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    object_id: "track".into(),
                    ..Default::default()
                });
                write.tracks.len()
            };
        }
        {
            let result = catalog
                .observe("source")
                .await
                .unwrap()
                .apply_catalog(&write, true)
                .await
                .unwrap();
            result
        };
        assert_eq!(
            sqlx::query_as::<_, (Option<String>, Option<i64>)>(
                "SELECT musicbrainz_artist_id,native_rating FROM artists WHERE object_id='artist'"
            )
            .fetch_one(&mut reader)
            .await
            .unwrap(),
            (if complete { None } else { Some("mbid".into()) }, Some(80))
        );
    }
    let mut write = library::CatalogBatch::default();
    {
        write.artists.push(library::ArtistObservation {
            user_data: library::NativeUserState {
                rating: Some(None),
                ..Default::default()
            },
            metadata: library::ArtistMetadata {
                name: "Artist".into(),
                ..Default::default()
            },
            object_id: "artist".into(),
            ..Default::default()
        });
        write.artists.len()
    };
    {
        let result = catalog
            .observe("source")
            .await
            .unwrap()
            .apply_catalog(&write, true)
            .await
            .unwrap();
        result
    };
    assert_eq!(
        sqlx::query_scalar::<_, Option<i64>>(
            "SELECT native_rating FROM artists WHERE object_id='artist'"
        )
        .fetch_one(&mut reader)
        .await
        .unwrap(),
        None
    );
}

/// Writes a small catalog and returns what its Writes changed.
async fn write_small_catalog(
    catalog: &Catalog,
    changes: &mut super::support::Changes,
    freshness: &str,
    title: &str,
    reverse: bool,
    genre_artwork: &[u8],
) -> (library::SourceKey, super::support::Shown) {
    changes.facts();
    let mut scan_observation = async {
        if {
            (catalog).loaded().await;
            (catalog).source_key(&library::SourceId::new("source-one"))
        }
        .is_none()
        {
            (catalog)
                .reconcile_source(
                    &library::SourceId::new("source-one"),
                    "subsonic",
                    "Source One",
                )
                .await?;
        }
        catalog.observe("source-one").await
    }
    .await
    .expect("begin scan");
    scan_observation.start_observation().await.unwrap();
    {
        let write = library::CatalogBatch { albums: vec![library::AlbumObservation{user_data:library::NativeUserState{favorite:Some(false),rating:(Some(Some(7))).map(|rating|rating.map(|value|value*10)),..Default::default()},metadata:library::AlbumMetadata{title:"Album One".into(),artist:"Artist One".into(),sort_text:"album one".into(),album_artists:vec![library::ArtistCredit {
                object_id: "artist-one".into(),
                name: "Artist One".into(),
                ..Default::default()
            }],genres:vec![library::TagReference {object_id: "genre-one".into(),name: "Genre One".into(),..Default::default()}],release_types:vec!["Album".into()],year:Some(2025),release_date:Some("2025-01-01".into()),date_added:Some("2025-01-02".into()),musicbrainz_release_id:Some("release-one".into()),musicbrainz_release_group_id:Some("release-group-one".into()),is_compilation:Some(false),..Default::default()},object_id: "album-one".into(),first_seen_at: Some(10),artwork_binding: Some(br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d453ddcc6a2bb4616a7509452b0c0edb","image":{"kind":"native","source_id":"source","item_id":"album-art","tag":null},"blur_hash":null}]"#.to_vec()),..Default::default()}], ..Default::default() };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch { artists: vec![library::ArtistObservation{user_data:library::NativeUserState{favorite:Some(false),..Default::default()},metadata:library::ArtistMetadata{name:"Artist One".into(),sort_text:Some("artist one".into()),musicbrainz_artist_id:Some("artist-mbid-one".into()),..Default::default()},object_id: "artist-one".into(),artwork_binding: Some(br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/8799b34127e8fddd3fdcdff25c21b7dd","image":{"kind":"native","source_id":"source","item_id":"artist-art","tag":null},"blur_hash":null}]"#.to_vec()),..Default::default()}], ..Default::default() };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch {
            tags: vec![library::TagObservation {
                kind: library::TagKind::Genre,
                metadata: library::TagMetadata {
                    name: "Genre One".into(),
                    sort_text: Some("genre one".into()),
                    ..Default::default()
                },
                object_id: "genre-one".into(),
                artwork_binding: Some(genre_artwork.to_vec()),
            }],
            ..Default::default()
        };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch {
            tags: vec![library::TagObservation {
                kind: library::TagKind::Mood,
                metadata: library::TagMetadata {
                    name: "Mood One".into(),
                    sort_text: Some("mood one".into()),
                    ..Default::default()
                },
                object_id: "mood-one".into(),
                artwork_binding: None,
            }],
            ..Default::default()
        };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch {
            folders: vec![library::FolderObservation {
                metadata: library::FolderMetadata {
                    name: Some("Folder One".into()),
                    sort_text: Some("folder one".into()),
                    ..Default::default()
                },
                object_id: "folder-one".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch {
            playlists: vec![library::PlaylistObservation {
                object_id: "playlist-one".into(),
                name: "Playlist One".into(),
                sort_text: "playlist one".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };

    let tracks = [
        ("track-one", title, 0_i64),
        ("track-two", "Track Two", 1_i64),
    ];
    let order: &[usize] = if reverse { &[1, 0] } else { &[0, 1] };
    for &index in order {
        let (object_id, track_title, position) = tracks[index];
        write_track(&mut scan_observation, object_id, track_title, position).await;
    }
    let entries = tracks
        .iter()
        .map(|(object_id, _, _)| library::PlaylistEntryObservation {
            object_id: format!("entry-{object_id}"),
            track_id: (*object_id).into(),
        })
        .collect::<Vec<_>>();
    scan_observation
        .write_source_playlist_page("playlist-one", 0, &entries, true)
        .await
        .expect("write completed playlist");
    scan_observation
        .complete_source_observation(
            &[
                "track", "album", "artist", "genre", "mood", "folder", "playlist",
            ],
            None,
            None,
        )
        .await
        .unwrap();
    scan_observation
        .complete_source_observation(
            &[],
            Some(Freshness::new(freshness.as_bytes()).expect("bounded freshness")).as_ref(),
            None,
        )
        .await
        .unwrap();
    (scan_observation.source, changes.facts())
}

async fn write_track(
    observation: &mut library::Observation<'_>,
    object_id: &str,
    title: &str,
    position: i64,
) {
    let normalized = format!("{} album one artist one note", title.to_lowercase());
    let media_uri = format!("file:///music/{object_id}.flac");
    observation
        .apply_catalog(
            &library::CatalogBatch {
                tracks: vec![library::TrackObservation {
                    user_data: library::NativeUserState {
                        favorite: Some(false),
                        play_count: Some(20 + position),
                        last_played: Some(1_700_000_000 + position),
                        ..Default::default()
                    },
                    metadata: library::TrackMetadata {
                        title: title.to_owned(),
                        album: "Album One".into(),
                        artist: "Artist One".into(),
                        sort_text: normalized,
                        duration_millis: 180_000,
                        disc_number: 1,
                        track_number: position + 1,
                        year: Some(2025),
                        release_date: Some("2025-01-01".into()),
                        date_added: Some("2025-01-02".into()),
                        source_format: Some("FLAC".into()),
                        comment: Some("Note".into()),
                        bpm: Some(120),
                        musicbrainz_recording_id: Some("recording-one".into()),
                        musicbrainz_release_track_id: Some("release-track-one".into()),
                        artists: vec![library::ArtistCredit {
                            object_id: "artist-one".into(),
                            name: "Artist One".into(),
                            sort_text: Some("artist one".into()),
                            musicbrainz_artist_id: Some("artist-mbid-one".into()),
                            ..Default::default()
                        }],
                        genres: vec![library::TagReference {
                            object_id: "genre-one".into(),
                            name: "Genre One".into(),
                            ..Default::default()
                        }],
                        moods: vec![library::TagReference {
                            object_id: "mood-one".into(),
                            name: "Mood One".into(),
                            sort_text: Some("mood one".into()),
                        }],
                        ..Default::default()
                    },
                    object_id: object_id.to_owned(),
                    album_object_id: Some("album-one".into()),
                    media_uri: Some(media_uri),
                    cue_path: Some("/music/album.cue".into()),
                    cue_start_millis: Some(1_000),
                    cue_end_millis: Some(181_000),
                    artwork_binding: Some(
                        serde_json::to_vec(&[library::ArtworkCandidate::new(
                            library::ArtworkImage::Native {
                                source_id: library::SourceId::new("source"),
                                item_id: format!("art-{object_id}"),
                                tag: None,
                            },
                            None,
                        )])
                        .unwrap(),
                    ),
                    first_seen_at: Some(10 + position),
                    audio_revision: Some([position as u8 + 1; 32]),
                    folders: Some(vec!["folder-one".into(), "folder-one".into()]),
                    ..Default::default()
                }],
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn canonical_publication_preserves_identity_and_user_overrides() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.expect("open Library Store");
    let mut changes = super::support::Changes::of(&catalog);
    let first = write_small_catalog(&catalog, &mut changes, "fresh-one", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;
    assert!(first.1.intersects(super::support::Shown::CATALOG));
    assert_eq!(
        {
            catalog.loaded().await;
            catalog.cached_source("source-one")
        }
        .map(|source| source.source),
        Some(first.0)
    );
    let mut outside = connection(&path).await;
    let original_keys = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT
             (SELECT album_key FROM albums WHERE object_id='album-one'),
             (SELECT track_key FROM tracks WHERE object_id='track-one'),
             (SELECT playlist_entry_key FROM playlist_entries
              WHERE object_id='entry-track-one')",
    )
    .fetch_one(&mut outside)
    .await
    .expect("read stable keys");
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, Option<i64>)>(
            "SELECT coalesce(native_play_count,0),0,native_last_played FROM tracks\n             WHERE source_key=?1 AND object_id='track-one'",
        )
        .bind(first.0)
        .fetch_one(&mut outside)
        .await
        .expect("read initial provider activity facts"),
        (20, 0, Some(1_700_000_000))
    );
    sqlx::query(
        "INSERT INTO user_state(media_uri,favorite,rating,changed_at)
         SELECT media_uri,1,90,1 FROM tracks WHERE object_id='track-one'",
    )
    .execute(&mut outside)
    .await
    .expect("write user overrides");

    let unchanged =
        write_small_catalog(&catalog, &mut changes, "fresh-two", "Track One", true, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;
    assert!(!super::support::shown(unchanged.1));
    assert!(
        (&catalog)
            .accept_freshness(
                "source-one",
                &Freshness::new(b"fresh-two".to_vec()).expect("bounded freshness"),
                &ReadCancellation::new()
            )
            .await
            .expect("accept freshness")
    );

    let changed = write_small_catalog(
        &catalog,
        &mut changes,
        "fresh-three",
        "Changed Track",
        false,
        br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#,
    )
    .await;
    assert!(changed.1.intersects(super::support::Shown::CATALOG));
    assert_eq!(changed.0, first.0);
    let changed_facts = sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(
        "SELECT
             (SELECT album_key FROM albums WHERE object_id='album-one'),
             (SELECT track_key FROM tracks WHERE object_id='track-one'),
             (SELECT playlist_entry_key FROM playlist_entries
              WHERE object_id='entry-track-one'),
             (SELECT state.favorite FROM user_state state JOIN tracks track USING(media_uri) WHERE track.object_id='track-one'),
             (SELECT state.rating FROM user_state state JOIN tracks track USING(media_uri) WHERE track.object_id='track-one')",
    )
    .fetch_one(&mut outside)
    .await
    .expect("read changed facts");
    assert_eq!(
        changed_facts,
        (original_keys.0, original_keys.1, original_keys.2, 1, 90)
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, Option<i64>)>(
            "SELECT coalesce(native_play_count,0),0,native_last_played FROM tracks\n             WHERE source_key=?1 AND object_id='track-one'",
        )
        .bind(first.0)
        .fetch_one(&mut outside)
        .await
        .expect("read current provider activity facts"),
        (20, 0, Some(1_700_000_000))
    );
    assert_eq!(
        sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                i64,
                String,
                String,
                String,
                i64,
                i64
            ),
        >(
            "SELECT title, date_added, source_format,comment,bpm,
                    json_extract(details.metadata,'$.musicbrainz_recording_id'),json_extract(details.metadata,'$.musicbrainz_release_track_id'),
                    details.cue_path, details.cue_start_millis, details.cue_end_millis
             FROM tracks JOIN track_details details USING(track_key) WHERE object_id='track-one'",
        )
        .fetch_one(&mut outside)
        .await
        .expect("read published Track facts"),
        (
            "Changed Track".to_string(),
            "2025-01-02".to_string(),
            "FLAC".to_string(),
            "Note".to_string(),
            120,
            "recording-one".to_string(),
            "release-track-one".to_string(),
            "/music/album.cue".to_string(),
            1000,
            181000,
        )
    );
    assert_eq!(
        sqlx::query_as::<_, (String, String, String, String, i64)>(
            "SELECT album.date_added, json_extract(album.metadata,'$.musicbrainz_release_id'),
                    json_extract(album.metadata,'$.musicbrainz_release_group_id'),
                    artist.musicbrainz_artist_id, length(genre.native_artwork)
             FROM albums AS album, artists AS artist, tags AS genre WHERE genre.kind=0",
        )
        .fetch_one(&mut outside)
        .await
        .expect("read published collection facts"),
        (
            "2025-01-02T00:00:00".to_string(),
            "release-one".to_string(),
            "release-group-one".to_string(),
            "artist-mbid-one".to_string(),
            br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#.len() as i64,
        )
    );
    let artwork_before = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT native_artwork FROM tags\n         WHERE kind=0 AND source_key=(SELECT source_key FROM sources WHERE object_id='source-one') ORDER BY tag_key LIMIT 1",
    )
    .fetch_one(&mut outside)
    .await
    .expect("read artwork before Genre change");
    let artwork_change = write_small_catalog(
        &catalog,
        &mut changes,
        "fresh-four",
        "Changed Track",
        false,
        br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/7c93b1fd0795697d6e43b90dfda4dc0b","image":{"kind":"native","source_id":"source","item_id":"genre-art-two","tag":null},"blur_hash":null}]"#,
    )
    .await;
    // A picture is part of the catalog rows that show it.
    assert!(artwork_change.1.contains(super::support::Shown::CATALOG));
    assert!(!artwork_change.1.intersects(super::support::Shown::PLAYLIST));
    let artwork_after = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT native_artwork FROM tags\n         WHERE kind=0 AND source_key=(SELECT source_key FROM sources WHERE object_id='source-one') ORDER BY tag_key LIMIT 1",
    )
    .fetch_one(&mut outside)
    .await
    .expect("read artwork after Genre change");
    assert_ne!(artwork_after, artwork_before);
    assert_eq!(artwork_after, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/7c93b1fd0795697d6e43b90dfda4dc0b","image":{"kind":"native","source_id":"source","item_id":"genre-art-two","tag":null},"blur_hash":null}]"#);
}

#[tokio::test]
async fn artwork_publication_keeps_one_effective_binding_per_entity() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.expect("open Library Store");
    let mut changes = super::support::Changes::of(&catalog);

    write_small_catalog(&catalog, &mut changes, "shared", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;
    let artist = {
        let rows = catalog
            .artist(
                &(library::source_entity_uri(
                    &library::SourceId::new("source-one"),
                    "artist",
                    "artist-one",
                )),
                false,
                false,
                &ReadCancellation::new(),
            )
            .await
            .unwrap();
        rows.unwrap().row.artwork_binding.unwrap()
    };
    let artist: Vec<library::ArtworkCandidate> = serde_json::from_slice(&artist).unwrap();
    assert_eq!(serde_json::to_vec(&artist[..1]).unwrap(), br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/8799b34127e8fddd3fdcdff25c21b7dd","image":{"kind":"native","source_id":"source","item_id":"artist-art","tag":null},"blur_hash":null}]"#);
    assert_eq!(serde_json::to_vec(&artist[1..2]).unwrap(), br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d453ddcc6a2bb4616a7509452b0c0edb","image":{"kind":"native","source_id":"source","item_id":"album-art","tag":null},"blur_hash":null}]"#);
    assert!(matches!(
        artist[2].image,
        library::ArtworkImage::Artist { .. }
    ));
    let shared = {
        let album = {
            let rows = catalog
                .album(
                    &(library::source_entity_uri(
                        &library::SourceId::new("source-one"),
                        "album",
                        "album-one",
                    )),
                    false,
                    &ReadCancellation::new(),
                )
                .await
                .unwrap();
            rows.unwrap()
        };
        let tracks = {
            let rows = catalog
                .tracks_by_uris(
                    &([
                        library::cue_media_uri(
                            "track-one",
                            "file:///music/track-one.flac",
                            1000,
                            181000,
                        ),
                        library::cue_media_uri(
                            "track-two",
                            "file:///music/track-two.flac",
                            1000,
                            181000,
                        ),
                    ]),
                    &ReadCancellation::new(),
                )
                .await
                .unwrap();
            rows
        };
        (
            album.row.artwork_binding.unwrap(),
            tracks[0].artwork_binding.clone().unwrap(),
            tracks[1].artwork_binding.clone().unwrap(),
        )
    };
    assert_eq!(
        shared,
        (
            br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d453ddcc6a2bb4616a7509452b0c0edb","image":{"kind":"native","source_id":"source","item_id":"album-art","tag":null},"blur_hash":null}]"#.to_vec(),
            br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d453ddcc6a2bb4616a7509452b0c0edb","image":{"kind":"native","source_id":"source","item_id":"album-art","tag":null},"blur_hash":null}]"#.to_vec(),
            br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d453ddcc6a2bb4616a7509452b0c0edb","image":{"kind":"native","source_id":"source","item_id":"album-art","tag":null},"blur_hash":null}]"#.to_vec()
        )
    );

    catalog.set_distinct_track_covers(true);
    assert_eq!(
        (&catalog)
            .accept_freshness(
                "source-one",
                &Freshness::new(b"shared".to_vec()).expect("freshness"),
                &ReadCancellation::new()
            )
            .await
            .expect("check policy freshness"),
        true
    );
    write_small_catalog(&catalog, &mut changes, "distinct", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;
    let distinct = {
        let album = {
            let rows = catalog
                .album(
                    &(library::source_entity_uri(
                        &library::SourceId::new("source-one"),
                        "album",
                        "album-one",
                    )),
                    false,
                    &ReadCancellation::new(),
                )
                .await
                .unwrap();
            rows.unwrap()
        };
        let tracks = {
            let rows = catalog
                .tracks_by_uris(
                    &([
                        library::cue_media_uri(
                            "track-one",
                            "file:///music/track-one.flac",
                            1000,
                            181000,
                        ),
                        library::cue_media_uri(
                            "track-two",
                            "file:///music/track-two.flac",
                            1000,
                            181000,
                        ),
                    ]),
                    &ReadCancellation::new(),
                )
                .await
                .unwrap();
            rows
        };
        (
            album.row.artwork_binding.unwrap(),
            tracks[0].artwork_binding.clone().unwrap(),
            tracks[1].artwork_binding.clone().unwrap(),
        )
    };
    assert_eq!(
        distinct,
        (
            br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d453ddcc6a2bb4616a7509452b0c0edb","image":{"kind":"native","source_id":"source","item_id":"album-art","tag":null},"blur_hash":null}]"#.to_vec(),
            br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/c4b5102ebbc0df8d15f13bed5b96ca7f","image":{"kind":"native","source_id":"source","item_id":"art-track-one","tag":null},"blur_hash":null}]"#.to_vec(),
            br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d52e05cfa6bba38cd1c6126637fd1673","image":{"kind":"native","source_id":"source","item_id":"art-track-two","tag":null},"blur_hash":null}]"#.to_vec(),
        )
    );
}

#[tokio::test]
async fn point_album_artwork_updates_cached_members_without_replacing_distinct_covers() {
    for distinct in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("library.sqlite");
        let catalog = Catalog::open(&path).await.unwrap();
        let mut changes = super::support::Changes::of(&catalog);
        catalog.set_distinct_track_covers(distinct);
        write_small_catalog(&catalog, &mut changes, "first", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;
        let mut reader = connection(&path).await;
        for (title, artwork) in [
            ("Album One", br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/248b9a062b6cfb466fd9de98afea60d8","image":{"kind":"native","source_id":"source","item_id":"new-art","tag":null},"blur_hash":null}]"#.as_slice()),
            ("Renamed Album", br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/3c2564f714c17a85ff6b8f5b5f65c092","image":{"kind":"native","source_id":"source","item_id":"newer-art","tag":null},"blur_hash":null}]"#.as_slice()),
            ("Renamed Album", br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/3c2564f714c17a85ff6b8f5b5f65c092","image":{"kind":"native","source_id":"source","item_id":"newer-art","tag":null},"blur_hash":null}]"#.as_slice()),
        ] {
            let previous = sqlx::query_scalar::<_, Vec<u8>>(
                "SELECT native_artwork FROM albums WHERE object_id='album-one'",
            )
            .fetch_one(&mut reader)
            .await
            .unwrap();
            let previous_title=sqlx::query_scalar::<_,String>("SELECT title FROM albums WHERE object_id='album-one'").fetch_one(&mut reader).await.unwrap();

let mut point_observation = catalog.observe("source-one").await.unwrap();
            {
let write = library::CatalogBatch { artists: vec![library::ArtistObservation{user_data:library::NativeUserState{favorite:Some(false),..Default::default()},metadata:library::ArtistMetadata{name:"Artist One".into(),sort_text:Some("artist one".into()),musicbrainz_artist_id:Some("artist-mbid-one".into()),..Default::default()},object_id: "artist-one".into(),artwork_binding: Some(br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/8799b34127e8fddd3fdcdff25c21b7dd","image":{"kind":"native","source_id":"source","item_id":"artist-art","tag":null},"blur_hash":null}]"#.to_vec()),..Default::default()}], ..Default::default() };
        point_observation.apply_catalog(&write, true).await.unwrap();
        };
            {
let write = library::CatalogBatch { tags: vec![library::TagObservation{kind: library::TagKind::Genre,metadata:library::TagMetadata{name: "Genre One".into(),sort_text: Some("genre one".into()),..Default::default()},object_id: "genre-one".into(),artwork_binding: Some(br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#.to_vec())}], ..Default::default() };
        point_observation.apply_catalog(&write, true).await.unwrap();
        };
            {
let write = library::CatalogBatch { albums: vec![library::AlbumObservation{user_data:library::NativeUserState{favorite:Some(false),rating:(Some(Some(7))).map(|rating|rating.map(|value|value*10)),..Default::default()},metadata:library::AlbumMetadata{title:title.to_owned(),artist:"Artist One".into(),sort_text:"album one".into(),year:Some(2025),release_date:Some("2025-01-01".into()),date_added:Some("2025-01-02".into()),musicbrainz_release_id:Some("release-one".into()),musicbrainz_release_group_id:Some("release-group-one".into()),is_compilation:Some(false),album_artists:vec![library::ArtistCredit {
                        object_id: "artist-one".into(),
                        name: "Artist One".into(),
                        ..Default::default()
                    }],genres:vec![library::TagReference {object_id: "genre-one".into(),name: "Genre One".into(),..Default::default()}],release_types:vec!["Album".into()],..Default::default()},object_id: "album-one".into(),artwork_binding: Some(artwork.to_vec()),first_seen_at: Some(10),..Default::default()}], ..Default::default() };
        point_observation.apply_catalog(&write, true).await.unwrap();
        };
            let outcome = changes.facts();
            // A picture is part of the catalog rows that show it.
            assert_eq!(outcome.intersects(super::support::Shown::CATALOG), previous_title != title || previous != artwork);
            assert!(!outcome.intersects(super::support::Shown::PLAYLIST | super::support::Shown::HOME));
            assert_eq!(
                catalog.track_artwork_bindings(&sqlx::query_scalar::<_,String>("SELECT media_uri FROM tracks ORDER BY object_id").fetch_all(&mut reader).await.unwrap(),&ReadCancellation::new()).await.unwrap(),
                if distinct {
                    vec![br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/c4b5102ebbc0df8d15f13bed5b96ca7f","image":{"kind":"native","source_id":"source","item_id":"art-track-one","tag":null},"blur_hash":null}]"#.to_vec(), br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/d52e05cfa6bba38cd1c6126637fd1673","image":{"kind":"native","source_id":"source","item_id":"art-track-two","tag":null},"blur_hash":null}]"#.to_vec()]
                } else {
                    vec![artwork.to_vec(), artwork.to_vec()]
                }
            );
        }
    }
}

#[tokio::test]
async fn incomplete_collection_publishes_valid_pages_without_removal_authority() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.expect("open Library Store");
    let mut changes = super::support::Changes::of(&catalog);
    write_small_catalog(&catalog, &mut changes, "complete", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;

    let mut scan_observation = catalog.observe("source-one").await.unwrap();
    write_track(
        &mut scan_observation,
        "track-one",
        "Changed From Valid Page",
        0,
    )
    .await;
    {
        let write = library::CatalogBatch {
            playlists: vec![library::PlaylistObservation {
                object_id: "completed-playlist".into(),
                name: "Completed".into(),
                sort_text: "completed".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    scan_observation
        .write_source_playlist_page(
            "completed-playlist",
            0,
            &[library::PlaylistEntryObservation {
                object_id: "completed-entry".into(),
                track_id: "track-one".into(),
            }],
            true,
        )
        .await
        .unwrap();
    scan_observation
        .write_source_playlist_page(
            "playlist-one",
            0,
            &[library::PlaylistEntryObservation {
                object_id: "unfinished-entry".into(),
                track_id: "track-two".into(),
            }],
            false,
        )
        .await
        .unwrap();
    assert!(scan_observation.changed());

    let mut outside = connection(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut outside)
            .await
            .expect("count retained Tracks"),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT title FROM tracks WHERE object_id='track-one'")
            .fetch_one(&mut outside)
            .await
            .expect("read accepted page"),
        "Changed From Valid Page"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT title FROM tracks WHERE object_id='track-two'")
            .fetch_one(&mut outside)
            .await
            .expect("read retained unseen Track"),
        "Track Two"
    );
    assert_eq!(sqlx::query_as::<_, (String, i64)>(
        "SELECT playlist.object_id,count(*) FROM playlists playlist JOIN playlist_entries USING(playlist_key) GROUP BY playlist.object_id ORDER BY playlist.object_id"
    ).fetch_all(&mut outside).await.unwrap(), [("completed-playlist".into(),1),("playlist-one".into(),2)]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM playlist_entries WHERE object_id='unfinished-entry'"
        )
        .fetch_one(&mut outside)
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn complete_scans_remove_unseen_items_after_partial_updates() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite");
    let catalog = Catalog::open(&path).await.unwrap();
    let mut changes = super::support::Changes::of(&catalog);
    for (names, authoritative, expected_count) in [
        (["a", "b"].as_slice(), true, 2),
        (["a"].as_slice(), false, 2),
        (["a"].as_slice(), true, 1),
    ] {
        changes.facts();
        let mut scan_observation = async {
            if {
                (&catalog).loaded().await;
                (&catalog).source_key(&library::SourceId::new("source"))
            }
            .is_none()
            {
                (&catalog)
                    .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
                    .await?;
            }
            catalog.observe("source").await
        }
        .await
        .unwrap();
        scan_observation.start_observation().await.unwrap();
        for &name in names {
            {
                let write = library::CatalogBatch {
                    tags: vec![library::TagObservation {
                        kind: library::TagKind::Genre,
                        metadata: library::TagMetadata {
                            name: name.to_owned(),
                            sort_text: Some(name.to_owned()),
                            ..Default::default()
                        },
                        object_id: name.to_owned(),
                        artwork_binding: None,
                    }],
                    ..Default::default()
                };

                scan_observation.apply_catalog(&write, true).await.unwrap();
            };
        }
        if authoritative {
            scan_observation
                .complete_source_observation(
                    &[
                        "track", "album", "artist", "genre", "mood", "folder", "playlist",
                    ],
                    None,
                    None,
                )
                .await
                .unwrap();
            scan_observation
                .complete_source_observation(&[], None, None)
                .await
                .unwrap();
        }
        let outcome = changes.facts();
        assert!(if authoritative {
            outcome.intersects(super::support::Shown::CATALOG)
        } else {
            !super::support::shown(outcome)
        });
        let mut reader = connection(&path).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tags WHERE kind=0 AND indexed")
                .fetch_one(&mut reader)
                .await
                .unwrap(),
            expected_count,
            "a complete scan clears membership omitted after an incomplete predecessor"
        );
    }

    let mut point_observation = catalog.observe("source").await.unwrap();
    {
        let write = library::CatalogBatch {
            tags: vec![library::TagObservation {
                kind: library::TagKind::Genre,
                metadata: library::TagMetadata {
                    name: "Live name".into(),
                    sort_text: Some("live name".into()),
                    ..Default::default()
                },
                object_id: "a".into(),
                artwork_binding: None,
            }],
            ..Default::default()
        };

        point_observation.apply_catalog(&write, true).await.unwrap();
    };

    changes.facts();
    let mut complete_observation = async {
        if {
            (&catalog).loaded().await;
            (&catalog).source_key(&library::SourceId::new("source"))
        }
        .is_none()
        {
            (&catalog)
                .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
                .await?;
        }
        catalog.observe("source").await
    }
    .await
    .unwrap();
    complete_observation.start_observation().await.unwrap();
    {
        let write = library::CatalogBatch {
            tags: vec![library::TagObservation {
                kind: library::TagKind::Genre,
                metadata: library::TagMetadata {
                    name: "a".into(),
                    sort_text: Some("a".into()),
                    ..Default::default()
                },
                object_id: "a".into(),
                artwork_binding: None,
            }],
            ..Default::default()
        };

        complete_observation
            .apply_catalog(&write, true)
            .await
            .unwrap();
    };
    complete_observation
        .complete_source_observation(
            &[
                "track", "album", "artist", "genre", "mood", "folder", "playlist",
            ],
            None,
            None,
        )
        .await
        .unwrap();
    complete_observation
        .complete_source_observation(&[], None, None)
        .await
        .unwrap();
    assert!(changes.facts().intersects(super::support::Shown::CATALOG));
    let mut reader = connection(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT name FROM tags WHERE kind=0 AND object_id='a'")
            .fetch_one(&mut reader)
            .await
            .unwrap(),
        "a",
        "a complete read replaces values from an earlier point update"
    );
}

#[tokio::test]
async fn saved_source_opens_from_cache_without_starting_a_scan() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.expect("open Library Store");
    let mut changes = super::support::Changes::of(&catalog);
    let publication = write_small_catalog(&catalog, &mut changes, "fresh", "Cached Track", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/47f24da63989b3586adaa7682d88f1bb","image":{"kind":"native","source_id":"source","item_id":"art","tag":null},"blur_hash":null}]"#).await;
    assert!(publication.1.intersects(super::support::Shown::CATALOG));
    drop(catalog);

    let reopened = Catalog::open(&path).await.expect("reopen cached Store");
    let cached = {
        reopened.loaded().await;
        reopened.cached_source("source-one")
    }
    .expect("cached source");

    assert_eq!(cached.source, publication.0);
    assert!(cached.catalog_complete);
    assert_eq!(cached.display_name, "Source One");
    assert_eq!(
        reopened.all_source_counts().await.unwrap()[&library::SourceId::new("source-one")],
        (1, 2)
    );
}

#[tokio::test]
async fn completed_batches_survive_later_failure_and_interleaved_changes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.unwrap();
    catalog
        .reconcile_source(&library::SourceId::new("source-two"), "subsonic", "Two")
        .await
        .unwrap();
    catalog
        .reconcile_source(&library::SourceId::new("source-three"), "subsonic", "Three")
        .await
        .unwrap();

    let mut failed_observation = catalog.observe("source-two").await.unwrap();
    failed_observation.start_observation().await.unwrap();
    let mut write = library::CatalogBatch::default();
    {
        write.tags.push(library::TagObservation {
            kind: library::TagKind::Genre,
            metadata: library::TagMetadata {
                name: "Kept".into(),
                ..Default::default()
            },
            object_id: "kept".into(),
            artwork_binding: None,
        });
        write.tags.len()
    };
    {
        write.tracks.push(library::TrackObservation {
            user_data: library::NativeUserState {
                favorite: Some(false),
                play_count: Some(1),
                ..Default::default()
            },
            metadata: library::TrackMetadata {
                title: "Known".into(),
                ..Default::default()
            },
            object_id: "known".into(),
            ..Default::default()
        });
        write.tracks.len()
    };
    failed_observation
        .apply_catalog(&write, true)
        .await
        .unwrap();
    let mut write = library::CatalogBatch::default();
    {
        write.tags.push(library::TagObservation {
            kind: library::TagKind::Genre,
            metadata: library::TagMetadata {
                name: "Rolled back".into(),
                ..Default::default()
            },
            object_id: "rolled-back".into(),
            artwork_binding: None,
        });
        write.tags.len()
    };
    {
        for (kind, id, state) in &[(
            "track",
            "known".to_string(),
            library::NativeUserState {
                favorite: Some(true),
                play_count: Some(9),
                ..Default::default()
            },
        )] {
            failed_observation
                .write_native_user_data(kind, &[(id.clone(), state.clone())])
                .await
                .unwrap();
        }
    };
    drop(write);
    let mut outside = connection(&path).await;
    assert_eq!(
        sqlx::query_as::<_, (bool, i64)>(
            "SELECT coalesce(native_favorite,0),coalesce(native_play_count,0) FROM tracks WHERE object_id='known'"
        )
        .fetch_one(&mut outside)
        .await
        .unwrap(),
        (true, 9)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM tags WHERE kind=0 AND object_id='rolled-back'"
        )
        .fetch_one(&mut outside)
        .await
        .unwrap(),
        0
    );
    let favorite = library::FavoriteTarget::Track(library::source_entity_uri(
        &library::SourceId::new("source-two"),
        "track",
        "known",
    ));
    catalog.set_favorite(&favorite, true, false).await.unwrap();

    {
        for (kind, id, state) in &[(
            "track",
            "known".to_string(),
            library::NativeUserState {
                favorite: Some(true),
                play_count: Some(2),
                ..Default::default()
            },
        )] {
            failed_observation
                .write_native_user_data(kind, &[(id.clone(), state.clone())])
                .await
                .unwrap();
        }
    };

    assert!(failed_observation.changed());
    drop(failed_observation);
    assert!(catalog.favorite(&favorite).await.unwrap());
    assert!(
        !{
            catalog.loaded().await;
            catalog.cached_source("source-two")
        }
        .unwrap()
        .catalog_complete
    );
    assert_eq!(
        sqlx::query_as::<_, (bool, i64)>(
            "SELECT coalesce(native_favorite,0),coalesce(native_play_count,0) FROM tracks WHERE object_id='known'"
        )
        .fetch_one(&mut outside)
        .await
        .unwrap(),
        (true, 2)
    );

    let mut old_observation = catalog.observe("source-two").await.unwrap();
    old_observation.start_observation().await.unwrap();

    let mut third_observation = catalog.observe("source-three").await.unwrap();
    third_observation.start_observation().await.unwrap();
    let tracks = (0..130)
        .map(|index| library::TrackObservation {
            metadata: library::TrackMetadata {
                title: format!("Track {index}"),
                ..Default::default()
            },
            object_id: format!("track-{index}"),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let mut write = library::CatalogBatch::default();
    assert_eq!(
        {
            write.tracks.extend_from_slice(&tracks);
            write.tracks.len()
        },
        130
    );
    third_observation.apply_catalog(&write, true).await.unwrap();
    {
        third_observation
            .complete_source_observation(
                &[
                    "track", "album", "artist", "genre", "mood", "folder", "playlist",
                ],
                None,
                None,
            )
            .await
            .unwrap();

        third_observation
            .complete_source_observation(&[], None, None)
            .await
            .unwrap();
    };
    drop(third_observation);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM tracks WHERE source_key=(SELECT source_key FROM sources WHERE object_id='source-three')").fetch_one(&mut outside).await.unwrap(),130);
    let mut point = library::CatalogBatch::default();
    {
        point.tracks.extend_from_slice(&[library::TrackObservation {
            metadata: library::TrackMetadata {
                title: "Saved edit".into(),
                ..Default::default()
            },
            object_id: "known".into(),
            ..Default::default()
        }]);
        point.tracks.len()
    };
    {
        let result = catalog
            .observe("source-two")
            .await
            .unwrap()
            .apply_catalog(&point, true)
            .await
            .unwrap();
        result
    };
    let mut stale = library::CatalogBatch::default();
    {
        stale.tracks.extend_from_slice(&[library::TrackObservation {
            metadata: library::TrackMetadata {
                title: "Old fetched page".into(),
                ..Default::default()
            },
            object_id: "known".into(),
            ..Default::default()
        }]);
        stale.tracks.len()
    };
    old_observation.apply_catalog(&stale, true).await.unwrap();
    drop(old_observation);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT title FROM tracks WHERE object_id='known'")
            .fetch_one(&mut outside)
            .await
            .unwrap(),
        "Saved edit"
    );
    assert!(catalog.favorite(&favorite).await.unwrap());
    catalog
        .remove_source(&library::SourceId::new("source-two"))
        .await
        .unwrap();
    assert!(catalog.observe("source-two").await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sources WHERE object_id='source-two'")
            .fetch_one(&mut outside)
            .await
            .unwrap(),
        0
    );

    catalog
        .reconcile_source(&library::SourceId::new("source-two"), "subsonic", "Two")
        .await
        .unwrap();
    for title in ["Reacquired", "Refreshed"] {
        let mut observation = catalog.observe("source-two").await.unwrap();
        observation.start_observation().await.unwrap();
        point.tracks[0].metadata.title = title.into();
        observation.apply_catalog(&point, true).await.unwrap();
        assert_eq!(
            sqlx::query_as::<_, (String, bool)>(
                "SELECT title,indexed FROM tracks WHERE object_id='known'",
            )
            .fetch_one(&mut outside)
            .await
            .unwrap(),
            (title.into(), true)
        );
    }

    catalog
        .reconcile_source(&library::SourceId::new("file-restart"), "local", "Files")
        .await
        .unwrap();

    sqlx::query("INSERT INTO albums(source_key,object_id,media_uri,metadata,indexed,cached_local) SELECT source_key,'file-album',?1,jsonb('{\"title\":\"Old header\"}'),1,1 FROM sources WHERE object_id='file-restart'")
        .bind(library::source_entity_uri(&library::SourceId::new("file-restart"), "album", "file-album"))
        .execute(&mut outside).await.unwrap();
    let catalog = super::support::reopen(catalog, &path).await;
    let mut interrupted_observation = catalog.observe("file-restart").await.unwrap();
    interrupted_observation.start_observation().await.unwrap();
    let write = library::CatalogBatch {
        tracks: vec![library::TrackObservation {
            metadata: library::TrackMetadata {
                title: "Track".into(),
                album_tags: Some(library::AlbumTags {
                    sort_text: "albüm".into(),
                    is_compilation: Some(true),
                    release_types: vec!["album".into()],
                    ..Default::default()
                }),
                album: "ALBÜM".into(),
                artist: "Artist".into(),
                album_display_artist: Some("Artist".into()),
                year: Some(2024),
                release_date: Some("2024-05-06".into()),
                musicbrainz_album_id: Some("release".into()),
                album_artists: vec![library::ArtistCredit {
                    object_id: "file-artist".into(),
                    name: "Artist".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            object_id: "file-track".into(),
            album_object_id: Some("file-album".into()),
            source_path: Some(
                directory
                    .path()
                    .join("track.wav")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..Default::default()
        }],
        ..Default::default()
    };
    interrupted_observation
        .apply_catalog(&write, true)
        .await
        .unwrap();
    drop(interrupted_observation);
    assert_eq!(
        sqlx::query_as::<_, (bool, bool)>(
            "SELECT indexed,input_metadata IS NULL FROM albums WHERE object_id='file-album'"
        )
        .fetch_one(&mut outside)
        .await
        .unwrap(),
        (false, true)
    );
    drop(outside);
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&path).await.unwrap();
    let album = {
        let rows = reopened
            .album(
                &(library::source_entity_uri(
                    &library::SourceId::new("file-restart"),
                    "album",
                    "file-album",
                )),
                false,
                &ReadCancellation::new(),
            )
            .await
            .unwrap();
        rows.unwrap()
    };
    assert_eq!(
        (
            album.metadata.title.as_str(),
            album.metadata.sort_text.as_str(),
            album.metadata.year,
            album.metadata.release_date.as_deref(),
            album.metadata.is_compilation
        ),
        ("ALBÜM", "albüm", Some(2024), Some("2024-05-06"), Some(true))
    );
    assert_eq!(album.row.links.album_artists[0].name, "Artist");
    assert_eq!(album.metadata.release_types, ["album"]);
    assert_eq!(
        reopened.all_source_counts().await.unwrap()[&library::SourceId::new("file-restart")],
        (1, 1)
    );
    assert!(
        !{
            reopened.loaded().await;
            reopened.cached_source("file-restart")
        }
        .unwrap()
        .catalog_complete
    );
}

#[tokio::test]
async fn deleting_one_source_playlist_preserves_other_playlist_entries() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.expect("open Library Store");
    let mut changes = super::support::Changes::of(&catalog);
    write_small_catalog(&catalog, &mut changes, "fresh-one", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;

    let mut point_observation = catalog.observe("source-one").await.unwrap();

    {
        let write = library::CatalogBatch {
            playlists: vec![library::PlaylistObservation {
                object_id: "playlist-one".into(),
                name: "Playlist One".into(),
                sort_text: "playlist one".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        point_observation.apply_catalog(&write, true).await.unwrap();
    };
    {
        let write = library::CatalogBatch {
            playlists: vec![library::PlaylistObservation {
                object_id: "playlist-two".into(),
                name: "Other".into(),
                sort_text: "other".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        point_observation.apply_catalog(&write, true).await.unwrap();
    };
    point_observation
        .write_source_playlist_page(
            "playlist-one",
            0,
            &[library::PlaylistEntryObservation {
                object_id: "entry-one".into(),
                track_id: "track-one".into(),
            }],
            true,
        )
        .await
        .unwrap();

    let mut removal_observation = catalog.observe("source-one").await.unwrap();

    removal_observation
        .remove_source_items("playlist", &["playlist-two".into()])
        .await
        .unwrap();

    let mut outside = connection(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM playlist_entries entry
             JOIN playlists playlist USING(playlist_key)
             WHERE playlist.object_id='playlist-one'",
        )
        .fetch_one(&mut outside)
        .await
        .expect("count preserved Playlist entries"),
        1
    );
}

#[tokio::test]
async fn identical_playlist_point_update_is_not_a_catalog_change() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.expect("open Library Store");
    let mut changes = super::support::Changes::of(&catalog);
    let initial =
        write_small_catalog(&catalog, &mut changes, "fresh-one", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;
    assert!(initial.1.intersects(super::support::Shown::CATALOG));

    let mut point_observation = catalog.observe("source-one").await.unwrap();

    {
        let write = library::CatalogBatch {
            playlists: vec![library::PlaylistObservation {
                object_id: "playlist-one".into(),
                name: "Playlist One".into(),
                sort_text: "playlist one".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        point_observation.apply_catalog(&write, true).await.unwrap();
    };
    point_observation
        .write_source_playlist_page(
            "playlist-one",
            0,
            &[library::PlaylistEntryObservation {
                object_id: "entry-track-one".into(),
                track_id: "track-one".into(),
            }],
            false,
        )
        .await
        .unwrap();
    point_observation
        .write_source_playlist_page(
            "playlist-one",
            1,
            &[library::PlaylistEntryObservation {
                object_id: "entry-track-two".into(),
                track_id: "track-two".into(),
            }],
            true,
        )
        .await
        .unwrap();

    assert!(!super::support::shown(changes.facts()));
}

#[tokio::test]
async fn repeated_source_playlist_and_folder_observations_coalesce() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.expect("open Library Store");
    let mut changes = super::support::Changes::of(&catalog);
    write_small_catalog(&catalog, &mut changes, "fresh-one", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;

    let mut scan_observation = catalog.observe("source-one").await.unwrap();

    for _ in 0..2 {
        {
            let write = library::CatalogBatch {
                folders: vec![library::FolderObservation {
                    metadata: library::FolderMetadata {
                        name: Some("Music".into()),
                        sort_text: Some("music".into()),
                        ..Default::default()
                    },
                    object_id: "folder-one".into(),
                    ..Default::default()
                }],
                ..Default::default()
            };

            scan_observation.apply_catalog(&write, true).await.unwrap();
        };
        {
            let write = library::CatalogBatch {
                playlists: vec![library::PlaylistObservation {
                    object_id: "playlist-one".into(),
                    name: "Playlist One".into(),
                    sort_text: "playlist one".into(),
                    ..Default::default()
                }],
                ..Default::default()
            };

            scan_observation.apply_catalog(&write, true).await.unwrap();
        };
        scan_observation
            .write_source_playlist_page(
                "playlist-one",
                0,
                &[library::PlaylistEntryObservation {
                    object_id: "entry-one".into(),
                    track_id: "track-one".into(),
                }],
                true,
            )
            .await
            .expect("stage repeated playlist entry");
    }

    assert!(scan_observation.changed());
}

#[tokio::test]
async fn duplicate_provider_entries_keep_first_occurrence_and_dense_positions() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.expect("open Library Store");
    let mut changes = super::support::Changes::of(&catalog);
    write_small_catalog(&catalog, &mut changes, "fresh-one", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;

    let mut scan_observation = catalog.observe("source-one").await.unwrap();

    {
        let write = library::CatalogBatch {
            playlists: vec![library::PlaylistObservation {
                object_id: "playlist-one".into(),
                name: "Playlist".into(),
                sort_text: "playlist".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_observation.apply_catalog(&write, true).await.unwrap();
    };
    scan_observation
        .write_source_playlist_page(
            "playlist-one",
            0,
            &[
                library::PlaylistEntryObservation {
                    object_id: "duplicate-a".into(),
                    track_id: "track-one".into(),
                },
                library::PlaylistEntryObservation {
                    object_id: "duplicate-a".into(),
                    track_id: "track-two".into(),
                },
                library::PlaylistEntryObservation {
                    object_id: "duplicate-b".into(),
                    track_id: "track-one".into(),
                },
            ],
            true,
        )
        .await
        .expect("write repeated track occurrences");

    let mut outside = connection(&path).await;
    assert_eq!(
        sqlx::query_as::<_, (String, String, i64)>(
            "SELECT entry.object_id,track.object_id,row_number() OVER(ORDER BY entry.rank)-1 FROM playlist_entries entry
             JOIN playlists playlist USING(playlist_key) JOIN tracks track USING(track_key)
             WHERE playlist.object_id='playlist-one' ORDER BY entry.rank",
        )
        .fetch_all(&mut outside)
        .await
        .expect("read normalized occurrence order"),
        [
            ("duplicate-a".into(), "track-one".into(), 0),
            ("duplicate-b".into(), "track-one".into(), 1)
        ]
    );
}

#[tokio::test]
async fn playlist_point_update_preserves_playlist_scope() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let catalog = Catalog::open(&path).await.expect("open Library Store");
    let mut changes = super::support::Changes::of(&catalog);
    write_small_catalog(&catalog, &mut changes, "fresh-one", "Track One", false, br#"[{"key":"native/36cd38f49b9afa08222c0dc9ebfe35eb/242a249a37e1642edb20688243d50616","image":{"kind":"native","source_id":"source","item_id":"genre-art","tag":null},"blur_hash":null}]"#).await;

    let mut point_observation = catalog.observe("source-one").await.unwrap();

    {
        let write = library::CatalogBatch {
            playlists: vec![library::PlaylistObservation {
                object_id: "playlist-one".into(),
                name: "Playlist One".into(),
                sort_text: "playlist one".into(),
                ..Default::default()
            }],
            ..Default::default()
        };

        point_observation.apply_catalog(&write, true).await.unwrap();
    };
    point_observation
        .write_source_playlist_page(
            "playlist-one",
            0,
            &[library::PlaylistEntryObservation {
                object_id: "entry-track-two".into(),
                track_id: "track-two".into(),
            }],
            true,
        )
        .await
        .unwrap();

    let update = changes.facts();
    assert!(update.contains(super::support::Shown::PLAYLIST));
    assert!(!update.intersects(super::support::Shown::CATALOG | super::support::Shown::HOME));
}

#[tokio::test]
async fn artist_credits_preserve_favorites_and_explicit_false_clears_them() {
    let root = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(root.path().join("library.sqlite"))
        .await
        .unwrap();
    let mut changes = super::support::Changes::of(&catalog);
    write_small_catalog(&catalog, &mut changes, "fresh", "Track", false, b"genre").await;
    let uri = library::source_entity_uri(
        &library::SourceId::new("source-one"),
        "artist",
        "artist-one",
    );
    let cancel = ReadCancellation::new();
    for (index, favorite) in [Some(true), None, Some(false), None]
        .into_iter()
        .enumerate()
    {
        let mut scan_observation = catalog.observe("source-one").await.unwrap();
        {
            let write = library::CatalogBatch {
                artists: vec![library::ArtistObservation {
                    user_data: library::NativeUserState {
                        favorite: favorite,
                        ..Default::default()
                    },
                    metadata: library::ArtistMetadata {
                        name: "Artist".into(),
                        sort_text: Some("artist".into()),
                        ..Default::default()
                    },
                    object_id: "artist-one".into(),
                    ..Default::default()
                }],
                ..Default::default()
            };

            scan_observation.apply_catalog(&write, true).await.unwrap();
        };
        // A descriptive credit must leave the native artist's user data alone.
        let mut write = library::CatalogBatch::default();
        {
            write.tracks.push(library::TrackObservation {
                metadata: library::TrackMetadata {
                    title: "Credit track".into(),
                    artists: vec![library::ArtistCredit {
                        object_id: "artist-one".into(),
                        name: "Artist".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                object_id: "credit-track".into(),
                ..Default::default()
            });
            write.tracks.len()
        };
        scan_observation.apply_catalog(&write, true).await.unwrap();
        let row = catalog
            .artist(&(&uri), false, false, &cancel)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.row.state.favorite, index < 2);
    }
}

#[tokio::test]
async fn reference_names_can_change_and_complete_artist_names_take_precedence() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite");
    let catalog = Catalog::open(&path).await.unwrap();
    catalog
        .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
        .await
        .unwrap();
    let mut raw = connection(&path).await;
    for (artist, genre, complete) in [
        ("Artist", "Rock", false),
        ("artist", "rock", false),
        ("Renamed", "Alternative", false),
        ("RENAMED", "ALTERNATIVE", true),
    ] {
        let mut write = library::CatalogBatch::default();
        if complete {
            {
                write.artists.push(library::ArtistObservation {
                    metadata: library::ArtistMetadata {
                        name: artist.into(),
                        ..Default::default()
                    },
                    object_id: "artist".into(),
                    ..Default::default()
                });
                write.artists.len()
            };
        }
        {
            write.tracks.push(library::TrackObservation {
                metadata: library::TrackMetadata {
                    title: "Track".into(),
                    artists: vec![library::ArtistCredit {
                        object_id: "artist".into(),
                        name: if complete { "Ignored credit" } else { artist }.into(),
                        ..Default::default()
                    }],
                    genres: vec![library::TagReference {
                        object_id: "genre".into(),
                        name: genre.into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                object_id: "track".into(),
                ..Default::default()
            });
            write.tracks.len()
        };
        {
            let result = catalog
                .observe("source")
                .await
                .unwrap()
                .apply_catalog(&write, true)
                .await
                .unwrap();
            result
        };
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT name FROM artists")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            artist
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT name FROM tags WHERE kind=0")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            genre
        );
    }
}
