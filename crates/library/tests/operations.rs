use std::sync::Arc;

use library::{
    AcceptedHomeChange, AcceptedLibraryChange, AcceptedPlay, AcceptedSkip, Album, AlbumRelations,
    Artist, ArtistCredit, CandidateBatch, CandidateChange, CandidateFinish, CandidateHeader,
    CueSegment, FavoriteAcceptance, FavoriteItemId, FolderId, Genre, GenreCredit, GenreId,
    HomeFacts, HomeItemId, HomeSectionKind, ImageRef, Libraries, LoadedHomeItem, LocalArtworkRef,
    LocalComponentReplacement, LocalComponentSeed, LocalFile, LocalFileKind, LocalFileSeed,
    LocalFileState, LoudnessItemId, LoudnessMeasurement, LoudnessMeasurementWrite, MoodCredit,
    MoodId, MusicFolder, MusicFolderId, NewScrobble, PendingScrobbleId, PlayedFilter, Playlist,
    PlaylistAcceptance, PlaylistEdit, PlaylistEntry, PlaylistId, PlaylistSnapshot,
    RadioComposition, RadioSeed, RandomComposition, RandomCriteria, ScrobbleService, SearchRequest,
    SmartPlaylistDefinition, SmartPlaylistId, SmartPlaylistRule, SmartPlaylistRuleField,
    SmartPlaylistRuleOperator, SmartPlaylistRuleValue, SmartPlaylistSortField, SourceArtwork,
    SourceHomeSection, SourceHomeSectionKind, SourceId, SourceLibraryUpdate, Track, TrackData,
    TrackRelations, TrackSort,
};

fn created_smart_playlist_id(change: Option<AcceptedLibraryChange>) -> SmartPlaylistId {
    let ids = change
        .expect("creating a smart Playlist must report a change")
        .smart_playlists;
    assert_eq!(ids.len(), 1);
    ids.into_iter().next().expect("created smart Playlist ID")
}

fn created_playlist_id(change: Option<AcceptedLibraryChange>) -> PlaylistId {
    let ids = change
        .expect("creating a Playlist must report a change")
        .playlists;
    assert_eq!(ids.len(), 1);
    ids.into_iter().next().expect("created Playlist ID")
}

#[test]
fn accepted_library_search_matches_substrings_across_item_fields() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let library = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("local:server:search");
    let mut searchable_track = track();
    searchable_track.title = "Orchard Walk".to_string();
    searchable_track.artist = "Apple Trees".to_string();
    searchable_track.album = "Green Fields".to_string();
    let album = album_for_track(&searchable_track, 0);
    let artist = artist_for_track(&searchable_track);

    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id,
            input_digest: digest(100),
        })
        .expect("begin search candidate");
    candidate
        .write(CandidateBatch::Albums(vec![album]))
        .expect("write searchable Album");
    candidate
        .write(CandidateBatch::Artists(vec![artist]))
        .expect("write searchable Artist");
    candidate
        .write(CandidateBatch::Tracks(vec![searchable_track]))
        .expect("write searchable Track");
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept search candidate");

    let apple = accepted
        .library
        .search(&SearchRequest::new("pple"))
        .expect("search accepted library");
    assert_eq!(apple.artists[0].name, "Apple Trees");
    assert_eq!(apple.albums[0].title, "Green Fields");
    assert_eq!(apple.tracks[0].title, "Orchard Walk");

    let combined = accepted
        .library
        .search(&SearchRequest::new("green orch"))
        .expect("search across Track fields");
    assert_eq!(combined.tracks.len(), 1);
    assert!(combined.artists.is_empty());
    assert!(combined.albums.is_empty());

    let mut updated_track = track();
    updated_track.title = "River Walk".to_string();
    updated_track.artist = "Pear Trees".to_string();
    updated_track.album = "Blue Fields".to_string();
    let updated_album = album_for_track(&updated_track, 0);
    let updated_artist = artist_for_track(&updated_track);
    accepted
        .library
        .accept_source_update(SourceLibraryUpdate {
            albums: vec![updated_album],
            tracks: vec![updated_track],
            artists: vec![updated_artist],
            ..SourceLibraryUpdate::default()
        })
        .expect("accept searchable item replacements")
        .expect("searchable item replacements changed");

    assert!(
        accepted
            .library
            .search(&SearchRequest::new("apple"))
            .expect("search removed terms")
            .is_empty()
    );
    let pear = accepted
        .library
        .search(&SearchRequest::new("pear"))
        .expect("search replacement terms");
    assert_eq!(pear.artists.len(), 1);
    assert_eq!(pear.albums.len(), 1);
    assert_eq!(pear.tracks.len(), 1);

    accepted
        .library
        .accept_source_update(SourceLibraryUpdate {
            removed_tracks: vec![library::TrackId::new("local:track:one")],
            ..SourceLibraryUpdate::default()
        })
        .expect("remove searchable Track")
        .expect("searchable Track removal changed");
    assert!(
        accepted
            .library
            .search(&SearchRequest::new("river"))
            .expect("search removed Track")
            .tracks
            .is_empty()
    );
}

#[test]
fn loudness_measurements_survive_reopen_and_invalidate_with_audio_facts() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let libraries = Libraries::open(&path).expect("open Library");
    let source_id = SourceId::new("local:server:loudness");
    let accepted = accept_track(&libraries, source_id.clone(), digest(111), track(), None, 1);
    let snapshot = accepted
        .library
        .loudness_analysis_snapshot()
        .expect("prepare loudness analysis");
    let track_input = snapshot.tracks.first().expect("analysis Track");
    let album_input = snapshot.albums.first().expect("analysis Album");
    let track_measurement = LoudnessMeasurement::new(Some(-20.0), 0.8).expect("Track loudness");
    let album_measurement = LoudnessMeasurement::new(Some(-19.0), 0.9).expect("Album loudness");
    accepted
        .library
        .store_loudness(vec![
            LoudnessMeasurementWrite {
                item: LoudnessItemId::Track(track_input.track.id.clone()),
                analysis_key: track_input.analysis_key,
                measurement: track_measurement,
            },
            LoudnessMeasurementWrite {
                item: LoudnessItemId::Album(album_input.album_id.clone()),
                analysis_key: album_input.analysis_key,
                measurement: album_measurement,
            },
        ])
        .expect("store loudness measurements");
    assert_eq!(
        accepted
            .library
            .loudness_for_track(&track_input.track.id)
            .expect("read loudness"),
        library::TrackLoudness {
            track: Some(track_measurement),
            album: Some(album_measurement),
        }
    );

    drop(accepted);
    drop(libraries);
    let libraries = Libraries::open(&path).expect("reopen Library");
    let reopened = libraries
        .load_source(&source_id)
        .expect("load source")
        .expect("accepted source");
    let track_id = library::TrackId::new("local:track:one");
    assert_eq!(
        reopened
            .loudness_for_track(&track_id)
            .expect("read reopened loudness")
            .album,
        Some(album_measurement)
    );

    let mut changed = track();
    changed.duration_seconds += 1;
    reopened
        .accept_source_update(SourceLibraryUpdate {
            tracks: vec![changed],
            ..SourceLibraryUpdate::default()
        })
        .expect("accept changed audio facts");
    assert_eq!(
        reopened
            .loudness_for_track(&track_id)
            .expect("read invalidated loudness"),
        library::TrackLoudness::default()
    );
}

#[test]
fn artist_artwork_binding_is_reused_until_an_accepted_artwork_change() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let libraries = Libraries::open(&path).expect("open Library");
    let source_id = SourceId::new("local:server:artwork-binding");
    let track = track();
    let artist = artist_for_track(&track);
    let artist_id = artist.id.clone();
    let mut album = album_for_track(&track, 0);
    album.image_ref = Some(ImageRef::new("album-cover-one", None));

    let mut candidate = libraries
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: digest(101),
        })
        .expect("begin artwork candidate");
    candidate
        .write(CandidateBatch::Albums(vec![album.clone()]))
        .expect("write Album");
    candidate
        .write(CandidateBatch::Artists(vec![artist]))
        .expect("write Artist");
    candidate
        .write(CandidateBatch::Tracks(vec![track]))
        .expect("write Track");
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept artwork candidate");

    let first = accepted
        .library
        .artist_artwork(&artist_id)
        .expect("read Artist artwork")
        .expect("bound Artist artwork");
    let unchanged = accepted
        .library
        .artist_artwork(&artist_id)
        .expect("read Artist artwork again")
        .expect("bound Artist artwork");
    assert!(Arc::ptr_eq(
        &first.representative_albums,
        &unchanged.representative_albums
    ));
    assert_eq!(
        first.representative_albums[0]
            .album
            .image_ref
            .as_ref()
            .map(|image| image.item_id.as_str()),
        Some("album-cover-one")
    );

    album.image_ref = Some(ImageRef::new("album-cover-two", None));
    accepted
        .library
        .accept_source_update(SourceLibraryUpdate {
            albums: vec![album],
            ..SourceLibraryUpdate::default()
        })
        .expect("accept artwork update")
        .expect("artwork update changed the Library");
    let changed = accepted
        .library
        .artist_artwork(&artist_id)
        .expect("read changed Artist artwork")
        .expect("changed Artist artwork remains bound");
    assert!(!Arc::ptr_eq(
        &first.representative_albums,
        &changed.representative_albums
    ));
    assert_eq!(
        changed.representative_albums[0]
            .album
            .image_ref
            .as_ref()
            .map(|image| image.item_id.as_str()),
        Some("album-cover-two")
    );

    drop(accepted);
    drop(libraries);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load artwork source")
        .expect("reopened artwork source");
    let restored = reopened
        .artist_artwork(&artist_id)
        .expect("read reopened Artist artwork")
        .expect("reopened Artist artwork remains bound");
    assert_eq!(
        restored.representative_albums[0]
            .album
            .image_ref
            .as_ref()
            .map(|image| image.item_id.as_str()),
        Some("album-cover-two")
    );
}

#[test]
fn source_artwork_uses_album_images_and_keeps_orphan_track_images() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let library = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("subsonic:server:artwork");
    let mut first = track();
    first.image_ref = Some(ImageRef::new("track-alias-one", None));
    let mut second = first.clone();
    second.id = library::TrackId::new("local:track:two");
    second.image_ref = Some(ImageRef::new("track-alias-two", None));
    let mut orphan = first.clone();
    orphan.id = library::TrackId::new("local:track:orphan");
    orphan.album_id = None;
    orphan.album.clear();
    orphan.image_ref = Some(ImageRef::new("orphan-image", None));
    let local_artwork = LocalArtworkRef::File {
        path: "/music/Artist/Album/cover.png".to_string(),
        revision: "cover-revision".to_string(),
    };
    let mut album = album_for_track(&first, 0);
    album.image_ref = Some(ImageRef::new("album-image", Some("album-tag".to_string())));
    album.local_artwork = Some(local_artwork.clone());
    let artist = Artist {
        id: library::ArtistId::new("local:artist:one"),
        name: "Artist".to_string(),
        favorite: false,
        last_played: None,
        play_count: None,
        user_rating: None,
        musicbrainz_artist_id: None,
        image_ref: album.image_ref.clone(),
        local_artwork: Some(local_artwork.clone()),
    };
    let playlist = PlaylistSnapshot {
        playlist: Playlist {
            id: PlaylistId::new("playlist:shared-artwork"),
            name: "Shared artwork".to_string(),
            image_ref: album.image_ref.clone(),
        },
        entries: Vec::new(),
    };

    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id,
            input_digest: digest(89),
        })
        .expect("begin artwork candidate");
    candidate
        .write(CandidateBatch::Albums(vec![album]))
        .expect("write Album");
    candidate
        .write(CandidateBatch::Artists(vec![artist]))
        .expect("write Artist");
    candidate
        .write(CandidateBatch::Tracks(vec![first, second, orphan]))
        .expect("write Tracks");
    candidate
        .write(CandidateBatch::Playlists(vec![playlist]))
        .expect("write Playlist");
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept artwork candidate");

    assert_eq!(
        accepted
            .library
            .source_artwork()
            .expect("project source artwork")
            .as_ref(),
        [
            SourceArtwork::Local(local_artwork),
            SourceArtwork::Native(ImageRef::new("album-image", Some("album-tag".to_string()))),
            SourceArtwork::Native(ImageRef::new("orphan-image", None)),
        ]
    );
}

#[test]
fn candidate_acceptance_rebases_activity_without_rebuilding_the_prepared_library() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let library = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("local:server:prepared");
    let first = accept_track(&library, source_id.clone(), digest(90), track(), None, 1);
    let smart_playlist_id = created_smart_playlist_id(
        first
            .library
            .create_smart_playlist(
                "Played".to_string(),
                SmartPlaylistDefinition {
                    match_all: vec![SmartPlaylistRule {
                        field: SmartPlaylistRuleField::PlayCount,
                        operator: SmartPlaylistRuleOperator::Above,
                        value: Some(SmartPlaylistRuleValue::Number(0)),
                    }],
                    match_any: Vec::new(),
                    sort_field: SmartPlaylistSortField::PlayCount,
                    descending: true,
                    limit: None,
                },
            )
            .expect("create activity smart playlist"),
    );
    let mut updated_track = track();
    updated_track.title = "Prepared replacement".to_string();
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id,
            input_digest: digest(91),
        })
        .expect("begin replacement candidate");
    candidate
        .write(CandidateBatch::Tracks(vec![updated_track.clone()]))
        .expect("write replacement Track");
    let prepared = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 2,
            },
            Some(&first.library),
        )
        .expect("prepare replacement candidate");
    let prepared_loaded = Arc::clone(prepared.library());
    let activity = first
        .library
        .record_play(AcceptedPlay {
            play_id: "play:prepared-overlap".to_string(),
            track_id: updated_track.id.clone(),
            played_at: 1_700_000_000,
            month: "2023-11".to_string(),
        })
        .expect("record overlapping activity")
        .expect("new overlapping activity");
    let replacement = prepared.accept().expect("accept replacement candidate");
    assert!(Arc::ptr_eq(&replacement.library, &prepared_loaded));
    assert_eq!(
        replacement
            .library
            .track(&updated_track.id)
            .expect("read prepared Track")
            .expect("prepared Track")
            .play_count,
        Some(1)
    );
    assert_eq!(
        replacement
            .library
            .history_track_list(None)
            .expect("read rebased History")
            .len(),
        1
    );
    assert!(
        replacement
            .library
            .apply_recorded_activity(&activity)
            .expect("ignore already rebased activity")
            .is_none()
    );
    assert_eq!(
        replacement
            .library
            .history_track_list(None)
            .expect("read History after duplicate publication")
            .len(),
        1
    );
    assert_eq!(
        replacement
            .library
            .smart_playlist_detail(&smart_playlist_id, None)
            .expect("read activity smart playlist")
            .expect("activity smart playlist")
            .tracks
            .len(),
        1
    );
    assert_eq!(
        replacement
            .library
            .track(&updated_track.id)
            .expect("read accepted Track")
            .expect("accepted Track")
            .title,
        "Prepared replacement"
    );
    assert_eq!(
        replacement
            .library
            .track(&updated_track.id)
            .expect("read rebased Track")
            .expect("rebased Track")
            .play_count,
        Some(1)
    );
}

#[test]
fn remote_activity_preserves_provider_statistics_and_drives_rufin_smart_playlists() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let library = Libraries::open(&path).expect("open Library");
    let source_id = SourceId::new("subsonic:server:activity");
    let mut remote_track = track();
    remote_track.id = library::TrackId::new("subsonic:track:one");
    remote_track.source_path = None;
    remote_track.play_count = Some(41);
    remote_track.skip_count = Some(8);
    remote_track.last_played = Some("2024-01-02 03:04:05".to_string());

    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: digest(92),
        })
        .expect("begin remote candidate");
    candidate
        .write(CandidateBatch::Tracks(vec![remote_track.clone()]))
        .expect("write remote Track");
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::Source {
                    sections: Vec::new(),
                },
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept remote candidate");
    let smart_playlist_id = created_smart_playlist_id(
        accepted
            .library
            .create_smart_playlist(
                "Played in Rufin".to_string(),
                SmartPlaylistDefinition {
                    match_all: vec![SmartPlaylistRule {
                        field: SmartPlaylistRuleField::PlayCount,
                        operator: SmartPlaylistRuleOperator::Above,
                        value: Some(SmartPlaylistRuleValue::Number(0)),
                    }],
                    match_any: Vec::new(),
                    sort_field: SmartPlaylistSortField::PlayCount,
                    descending: true,
                    limit: None,
                },
            )
            .expect("create Rufin activity smart playlist"),
    );
    let unrelated_smart_playlist_id = created_smart_playlist_id(
        accepted
            .library
            .create_smart_playlist(
                "Named Track".to_string(),
                SmartPlaylistDefinition {
                    match_all: vec![SmartPlaylistRule {
                        field: SmartPlaylistRuleField::Title,
                        operator: SmartPlaylistRuleOperator::Contains,
                        value: Some(SmartPlaylistRuleValue::Text("Track".to_string())),
                    }],
                    match_any: Vec::new(),
                    sort_field: SmartPlaylistSortField::Title,
                    descending: false,
                    limit: None,
                },
            )
            .expect("create unrelated smart playlist"),
    );
    assert_eq!(
        accepted
            .library
            .smart_playlist_detail(&smart_playlist_id, None)
            .expect("read initial smart playlist")
            .expect("initial smart playlist")
            .tracks
            .len(),
        0
    );

    let update = accepted
        .library
        .record_play(AcceptedPlay {
            play_id: "remote-play:one".to_string(),
            track_id: remote_track.id.clone(),
            played_at: 1_700_000_000,
            month: "2023-11".to_string(),
        })
        .expect("record remote play")
        .expect("new remote play");
    let visible = accepted
        .library
        .apply_recorded_activity(&update)
        .expect("apply remote activity")
        .expect("remote activity must change the library");
    assert!(visible.tracks.is_empty());
    assert!(visible.history_changed);
    assert!(visible.smart_playlists.contains(&smart_playlist_id));
    assert!(
        !visible
            .smart_playlists
            .contains(&unrelated_smart_playlist_id)
    );
    let effective = accepted
        .library
        .track(&remote_track.id)
        .expect("read remote Track")
        .expect("remote Track");
    assert_eq!(effective.play_count, Some(41));
    assert_eq!(effective.skip_count, Some(8));
    assert_eq!(
        effective.last_played.as_deref(),
        Some("2024-01-02 03:04:05")
    );
    assert_eq!(
        accepted
            .library
            .smart_playlist_detail(&smart_playlist_id, None)
            .expect("read updated smart playlist")
            .expect("updated smart playlist")
            .tracks
            .len(),
        1
    );
    assert_eq!(
        accepted
            .library
            .history_track_list(None)
            .expect("read remote History")
            .len(),
        1
    );

    drop(accepted);
    drop(library);
    let reopened_library = Libraries::open(&path).expect("reopen Library");
    let reopened = reopened_library
        .load_source(&source_id)
        .expect("load remote source")
        .expect("reopened remote source");
    assert_eq!(
        reopened
            .track(&remote_track.id)
            .expect("read reopened remote Track")
            .expect("reopened remote Track")
            .play_count,
        Some(41)
    );
    assert_eq!(
        reopened
            .history_track_list(None)
            .expect("read reopened remote History")
            .len(),
        1
    );
    assert_eq!(
        reopened
            .smart_playlist_detail(&smart_playlist_id, None)
            .expect("read reopened smart playlist")
            .expect("reopened smart playlist")
            .tracks
            .len(),
        1
    );
}

#[test]
fn removing_source_data_is_scoped_and_preserves_external_delivery_work() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let library = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let removed_id = SourceId::new("local:server:removed");
    let kept_id = SourceId::new("jellyfin:server:kept");
    let removed = accept_track(&library, removed_id.clone(), digest(91), track(), None, 1);
    accept_track(&library, kept_id.clone(), digest(92), track(), None, 2);
    removed
        .library
        .accept_favorite(FavoriteAcceptance::RufinOwned {
            item: FavoriteItemId::Track(track().id.clone()),
            favorite: true,
        })
        .expect("store Local favorite");
    library
        .queue_scrobbles(vec![NewScrobble {
            id: PendingScrobbleId {
                service: ScrobbleService::LastFm,
                account_id: "listener".to_string(),
                play_id: "qualified-play".to_string(),
            },
            track_title: "Track".to_string(),
            artist_name: "Artist".to_string(),
            album_title: Some("Album".to_string()),
            duration_millis: 180_000,
            started_at: 1,
        }])
        .expect("queue external delivery");

    library
        .remove_source_data(&removed_id)
        .expect("remove source data");

    assert!(
        library
            .load_source(&removed_id)
            .expect("load removed source")
            .is_none()
    );
    assert!(
        library
            .load_source(&kept_id)
            .expect("load retained source")
            .is_some()
    );
    assert_eq!(
        library
            .due_scrobbles(ScrobbleService::LastFm, "listener", 1, 10)
            .expect("load external delivery")
            .len(),
        1
    );

    let rebuilt = accept_track(&library, removed_id, digest(93), track(), None, 3);
    assert!(
        !rebuilt
            .library
            .track(&track().id)
            .unwrap()
            .unwrap()
            .favorite
    );
}

#[test]
fn transient_scrobble_retry_reopens_and_permanent_rejection_leaves_no_work() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let id = PendingScrobbleId {
        service: ScrobbleService::LastFm,
        account_id: "listener".to_string(),
        play_id: "qualified-play".to_string(),
    };
    let library = Libraries::open(&path).expect("open Library");
    library
        .queue_scrobbles(vec![NewScrobble {
            id: id.clone(),
            track_title: "Track".to_string(),
            artist_name: "Artist".to_string(),
            album_title: Some("Album".to_string()),
            duration_millis: 180_000,
            started_at: 1,
        }])
        .expect("queue external delivery");
    library
        .defer_scrobble(id.clone(), 31)
        .expect("defer transient failure");
    drop(library);

    let reopened = Libraries::open(&path).expect("reopen Library");
    assert!(
        reopened
            .due_scrobbles(ScrobbleService::LastFm, "listener", 30, 10)
            .expect("read retry before deadline")
            .is_empty()
    );
    let due = reopened
        .due_scrobbles(ScrobbleService::LastFm, "listener", 31, 10)
        .expect("read due retry");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].attempts, 1);
    assert_eq!(due[0].next_attempt_at, 31);

    reopened
        .complete_scrobble(id)
        .expect("discard permanently rejected delivery");
    drop(reopened);
    assert!(
        Libraries::open(&path)
            .expect("reopen after rejection")
            .due_scrobbles(ScrobbleService::LastFm, "listener", i64::MAX, 10)
            .expect("read work after rejection")
            .is_empty()
    );
}

#[test]
fn accepted_library_reopens_with_sparse_relationships_and_playlist_occurrences() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:test");
    let library = Libraries::open(&path).expect("open Library");

    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: digest(1),
        })
        .expect("begin candidate");
    let track = track();
    candidate
        .write(CandidateBatch::Tracks(vec![track.clone()]))
        .expect("write Track");
    candidate
        .write(CandidateBatch::Playlists(vec![PlaylistSnapshot {
            playlist: Playlist {
                id: PlaylistId::new("local:playlist:one"),
                name: "Duplicates".to_string(),
                image_ref: None,
            },
            entries: vec![
                PlaylistEntry {
                    occurrence_id: "first".to_string(),
                    track_id: track.id.clone(),
                },
                PlaylistEntry {
                    occurrence_id: "second".to_string(),
                    track_id: track.id.clone(),
                },
            ],
        }]))
        .expect("write Playlist");
    let commit = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept candidate");
    assert_eq!(commit.change, CandidateChange::Library);

    let album_id = track.album_id.as_ref().expect("Track Album ID");
    let album = commit
        .library
        .album_detail(album_id, None)
        .expect("read Album")
        .expect("derived Album");
    assert_eq!(album.tracks.len(), 1);
    let artist_id = track.primary_artist_id().expect("Track Artist ID");
    let artist = commit
        .library
        .artist_track_detail(artist_id, None)
        .expect("read Artist")
        .expect("derived Artist");
    assert_eq!(artist.tracks.len(), 1);
    let playlist = commit
        .library
        .playlist_detail(&PlaylistId::new("local:playlist:one"))
        .expect("read Playlist")
        .expect("Playlist");
    assert_eq!(playlist.entries.len(), 2);
    let first_entry = playlist_entry(&playlist.entries, 0);
    let second_entry = playlist_entry(&playlist.entries, 1);
    assert_eq!(first_entry.track.id, second_entry.track.id);
    assert_ne!(first_entry.occurrence_id, second_entry.occurrence_id);

    drop(commit);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load accepted source")
        .expect("accepted source");
    assert_eq!(
        reopened
            .playlist_detail(&PlaylistId::new("local:playlist:one"))
            .expect("read reopened Playlist")
            .expect("reopened Playlist")
            .entries
            .len(),
        2
    );
}

#[test]
fn equal_content_reuses_the_loaded_library_and_home_updates_in_place() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let library = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("jellyfin:server:test");
    let first = accept(
        &library,
        source_id.clone(),
        digest(4),
        "First",
        Some(SourceHomeSectionKind::MostPlayed),
        None,
    );

    let unchanged = accept(
        &library,
        source_id.clone(),
        digest(4),
        "First",
        Some(SourceHomeSectionKind::MostPlayed),
        Some(&first.library),
    );
    assert_eq!(unchanged.change, CandidateChange::None);
    assert!(Arc::ptr_eq(&unchanged.library, &first.library));

    let home_updated = accept(
        &library,
        source_id.clone(),
        digest(4),
        "First",
        Some(SourceHomeSectionKind::RecentlyPlayed),
        Some(&first.library),
    );
    assert_eq!(home_updated.change, CandidateChange::Home);
    assert!(Arc::ptr_eq(&home_updated.library, &first.library));
    let home = home_updated.library.home(None).expect("Home");
    let item = &home
        .section(HomeSectionKind::RecentlyPlayed)
        .expect("Recently played")
        .items[0];
    let LoadedHomeItem::Track(track) = item else {
        panic!("Recently played item is a Track");
    };
    assert_eq!(track.title, "First");

    let changed_input = accept(
        &library,
        source_id,
        digest(7),
        "First",
        Some(SourceHomeSectionKind::RecentlyPlayed),
        Some(&first.library),
    );
    assert_eq!(changed_input.change, CandidateChange::Library);
    assert!(!Arc::ptr_eq(&changed_input.library, &first.library));
}

#[test]
fn equal_candidate_ids_do_not_reuse_a_library_from_another_store() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let left_libraries =
        Libraries::open(directory.path().join("left.db")).expect("open left Store");
    let right_libraries =
        Libraries::open(directory.path().join("right.db")).expect("open right Store");
    let source_id = SourceId::new("jellyfin:server:shared-source-id");
    let left = accept(
        &left_libraries,
        source_id.clone(),
        digest(8),
        "Left Store",
        None,
        None,
    );
    let right = accept(
        &right_libraries,
        source_id.clone(),
        digest(8),
        "Right Store",
        None,
        None,
    );
    assert_eq!(left.library.library_id(), right.library.library_id());

    let unchanged = accept(
        &right_libraries,
        source_id,
        digest(8),
        "Right Store",
        None,
        Some(&left.library),
    );

    assert_eq!(unchanged.change, CandidateChange::None);
    assert!(!Arc::ptr_eq(&unchanged.library, &left.library));
    assert_eq!(
        unchanged
            .library
            .track(&track().id)
            .expect("read right Store Track")
            .expect("right Store Track")
            .title,
        "Right Store"
    );
}

#[test]
fn provider_home_section_replaces_only_that_section_and_reopens() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("jellyfin:server:home-section");
    let library = Libraries::open(&path).expect("open Library");
    let accepted = accept(
        &library,
        source_id.clone(),
        digest(8),
        "First",
        Some(SourceHomeSectionKind::MostPlayed),
        None,
    );
    let current = accepted.library.home(None).expect("initial Home");
    let most_played = Arc::clone(
        current
            .section(HomeSectionKind::MostPlayed)
            .expect("Most played"),
    );

    let next = accepted
        .library
        .accept_home_section(
            None,
            &current,
            SourceHomeSection {
                kind: SourceHomeSectionKind::RecentlyPlayed,
                items: vec![HomeItemId::Track(track().id.clone())],
            },
        )
        .expect("accept one provider Home section");

    assert!(Arc::ptr_eq(
        next.section(HomeSectionKind::MostPlayed)
            .expect("retained Most played"),
        &most_played
    ));
    assert!(
        next.section(HomeSectionKind::RecentlyPlayed)
            .is_some_and(|section| section.items.len() == 1)
    );

    drop(next);
    drop(current);
    drop(accepted);
    drop(library);
    let reopened_library = Libraries::open(&path).expect("reopen Library");
    let reopened = reopened_library
        .load_source(&source_id)
        .expect("load source")
        .expect("accepted source");
    let reopened_home = reopened.home(None).expect("reopened Home");
    assert!(
        reopened_home
            .section(HomeSectionKind::RecentlyPlayed)
            .is_some()
    );

    let removed = reopened
        .accept_home_section(
            None,
            &reopened_home,
            SourceHomeSection {
                kind: SourceHomeSectionKind::RecentlyPlayed,
                items: Vec::new(),
            },
        )
        .expect("remove empty provider Home section");
    assert!(removed.section(HomeSectionKind::RecentlyPlayed).is_none());

    drop(removed);
    drop(reopened_home);
    drop(reopened);
    drop(reopened_library);
    let final_library = Libraries::open(path).expect("reopen Library after removal");
    let final_loaded = final_library
        .load_source(&source_id)
        .expect("load source after removal")
        .expect("accepted source after removal");
    let final_home = final_loaded.home(None).expect("Home after removal");
    assert!(final_home.section(HomeSectionKind::MostPlayed).is_some());
    assert!(
        final_home
            .section(HomeSectionKind::RecentlyPlayed)
            .is_none()
    );
}

#[test]
fn finished_candidate_stays_invisible_until_acceptance() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let library = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("jellyfin:server:prepared");
    let first = accept_track(&library, source_id.clone(), digest(41), track(), None, 1);

    let mut replacement = track();
    replacement.title = "Prepared replacement".to_string();
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: digest(42),
        })
        .expect("begin replacement candidate");
    candidate
        .write(CandidateBatch::Tracks(vec![replacement.clone()]))
        .expect("write replacement Track");
    let prepared = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 2,
            },
            Some(&first.library),
        )
        .expect("finish replacement candidate");
    assert_eq!(prepared.change(), CandidateChange::Library);
    assert_eq!(
        prepared
            .library()
            .track(&replacement.id)
            .expect("read prepared Track")
            .expect("prepared Track")
            .title,
        "Prepared replacement"
    );
    assert_eq!(
        library
            .load_source(&source_id)
            .expect("load accepted source")
            .expect("accepted source")
            .track(&replacement.id)
            .expect("read accepted Track")
            .expect("accepted Track")
            .title,
        "Track"
    );

    drop(prepared);
    assert_eq!(
        library
            .load_source(&source_id)
            .expect("reload accepted source")
            .expect("accepted source")
            .track(&replacement.id)
            .expect("read retained Track")
            .expect("retained Track")
            .title,
        "Track"
    );
}

#[test]
fn remote_favorites_remain_optimistic_across_restart_and_retry() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let libraries = Libraries::open(&path).expect("open Library");
    let source_id = SourceId::new("jellyfin:server:favorite-outbox");
    let source_track = track();
    let accepted = accept_track(
        &libraries,
        source_id.clone(),
        digest(4),
        source_track.clone(),
        None,
        1,
    );
    let item = FavoriteItemId::Track(source_track.id.clone());

    accepted
        .library
        .queue_remote_favorite(item.clone(), true, 10)
        .expect("queue optimistic favorite");
    assert!(
        accepted
            .library
            .track(&source_track.id)
            .expect("read optimistic Track")
            .expect("optimistic Track")
            .favorite
    );
    assert_eq!(
        accepted
            .library
            .due_remote_favorites(10, 10)
            .expect("read due favorite"),
        vec![library::PendingFavorite {
            item: item.clone(),
            favorite: true,
            attempts: 0,
        }]
    );
    accepted
        .library
        .defer_remote_favorite(item.clone(), true, 30)
        .expect("defer favorite");
    assert!(
        accepted
            .library
            .due_remote_favorites(29, 10)
            .expect("read deferred favorites")
            .is_empty()
    );

    drop(accepted);
    drop(libraries);
    let reopened_libraries = Libraries::open(&path).expect("reopen Library");
    let reopened = reopened_libraries
        .load_source(&source_id)
        .expect("load source")
        .expect("accepted source");
    assert!(
        reopened
            .track(&source_track.id)
            .expect("read reopened Track")
            .expect("reopened Track")
            .favorite,
        "pending delivery remains visible after restart"
    );
    assert_eq!(
        reopened
            .due_remote_favorites(30, 10)
            .expect("read retried favorite")[0]
            .attempts,
        1
    );
    reopened
        .complete_remote_favorite(item.clone(), true)
        .expect("complete favorite delivery");
    assert!(
        reopened
            .due_remote_favorites(i64::MAX, 10)
            .expect("read completed outbox")
            .is_empty()
    );

    reopened
        .queue_remote_favorite(item.clone(), false, 40)
        .expect("queue unfavorite");
    let rollback = reopened
        .reject_remote_favorite(item, false)
        .expect("reject unfavorite")
        .expect("rejected value was current");
    assert_eq!(
        rollback
            .favorite
            .expect("rollback acknowledgement")
            .favorite,
        true
    );
    assert!(
        reopened
            .track(&source_track.id)
            .expect("read rolled back Track")
            .expect("rolled back Track")
            .favorite
    );
}

#[test]
fn exact_rating_survives_source_refresh_and_reopen() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("subsonic:server:ratings");
    let libraries = Libraries::open(&path).expect("open Library");
    let mut source_track = track();
    source_track.user_rating = Some(8);
    let accepted = accept_track(
        &libraries,
        source_id.clone(),
        digest(121),
        source_track.clone(),
        None,
        1,
    );
    let track_id = source_track.id.clone();

    let change = accepted
        .library
        .set_rating(FavoriteItemId::Track(track_id.clone()), Some(7))
        .expect("set exact rating");
    assert_eq!(
        change.tracks[0]
            .track
            .as_ref()
            .expect("rated Track")
            .user_rating,
        Some(7)
    );

    accepted
        .library
        .accept_source_update(SourceLibraryUpdate {
            tracks: vec![source_track],
            ..SourceLibraryUpdate::default()
        })
        .expect("accept rounded source rating");
    drop(accepted);
    drop(libraries);

    let reopened = Libraries::open(path)
        .expect("reopen Libraries")
        .load_source(&source_id)
        .expect("load source")
        .expect("rated source");
    assert_eq!(
        reopened
            .track(&track_id)
            .expect("read Track")
            .expect("rated Track")
            .user_rating,
        Some(7)
    );
}

#[test]
fn remote_point_updates_restore_the_complete_refresh_shortcut() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let library = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("jellyfin:server:point-updates");
    let input_digest = digest(5);
    let source_track = track();
    let first = accept_track(
        &library,
        source_id.clone(),
        input_digest,
        source_track.clone(),
        None,
        1,
    );
    let favorite_smart_playlist_id = created_smart_playlist_id(
        first
            .library
            .create_smart_playlist(
                "Favorite Tracks".to_string(),
                SmartPlaylistDefinition {
                    match_all: vec![SmartPlaylistRule {
                        field: SmartPlaylistRuleField::Favorite,
                        operator: SmartPlaylistRuleOperator::Is,
                        value: Some(SmartPlaylistRuleValue::Bool(true)),
                    }],
                    match_any: Vec::new(),
                    sort_field: SmartPlaylistSortField::Title,
                    descending: false,
                    limit: None,
                },
            )
            .expect("create favorite smart playlist"),
    );
    let title_smart_playlist_id = created_smart_playlist_id(
        first
            .library
            .create_smart_playlist(
                "Tracks".to_string(),
                SmartPlaylistDefinition {
                    match_all: vec![SmartPlaylistRule {
                        field: SmartPlaylistRuleField::Title,
                        operator: SmartPlaylistRuleOperator::Contains,
                        value: Some(SmartPlaylistRuleValue::Text("Track".to_string())),
                    }],
                    match_any: Vec::new(),
                    sort_field: SmartPlaylistSortField::Title,
                    descending: false,
                    limit: None,
                },
            )
            .expect("create title smart playlist"),
    );

    let favorite = first
        .library
        .accept_favorite(FavoriteAcceptance::SourceAcknowledged {
            item: FavoriteItemId::Track(source_track.id.clone()),
            favorite: true,
        })
        .expect("accept remote favorite");
    assert_eq!(
        favorite.favorite,
        Some(library::FavoriteAcknowledgement {
            item: FavoriteItemId::Track(source_track.id.clone()),
            favorite: true,
        })
    );
    assert_eq!(favorite.tracks.len(), 1);
    assert!(
        favorite.tracks[0]
            .track
            .as_ref()
            .expect("favorite Track")
            .favorite
    );
    assert!(
        favorite
            .smart_playlists
            .contains(&favorite_smart_playlist_id)
    );
    assert!(!favorite.smart_playlists.contains(&title_smart_playlist_id));
    assert_eq!(
        first
            .library
            .smart_playlist_detail(&favorite_smart_playlist_id, None)
            .expect("read favorite smart playlist")
            .expect("favorite smart playlist")
            .tracks
            .len(),
        1
    );
    assert_eq!(
        first
            .library
            .smart_playlist_detail(&title_smart_playlist_id, None)
            .expect("read title smart playlist")
            .expect("title smart playlist")
            .tracks
            .len(),
        1
    );

    let mut favorited_track = source_track.clone();
    favorited_track.favorite = true;
    let after_favorite = accept_track(
        &library,
        source_id.clone(),
        input_digest,
        favorited_track.clone(),
        Some(&first.library),
        2,
    );
    assert_eq!(after_favorite.change, CandidateChange::None);
    assert!(Arc::ptr_eq(&after_favorite.library, &first.library));

    first
        .library
        .accept_favorite(FavoriteAcceptance::SourceAcknowledged {
            item: FavoriteItemId::Track(source_track.id.clone()),
            favorite: true,
        })
        .expect("accept equal remote favorite");
    let after_equal_favorite = accept_track(
        &library,
        source_id.clone(),
        input_digest,
        favorited_track,
        Some(&first.library),
        3,
    );
    assert_eq!(after_equal_favorite.change, CandidateChange::None);
    assert!(Arc::ptr_eq(&after_equal_favorite.library, &first.library));

    let initial_playlist = PlaylistSnapshot {
        playlist: Playlist {
            id: PlaylistId::new("jellyfin:playlist:one"),
            name: "Before".to_string(),
            image_ref: None,
        },
        entries: vec![PlaylistEntry {
            occurrence_id: "entry-one".to_string(),
            track_id: source_track.id.clone(),
        }],
    };
    let with_playlist = accept_track_and_playlist(
        &library,
        source_id.clone(),
        input_digest,
        source_track.clone(),
        initial_playlist,
        Some(&first.library),
        4,
    );
    assert_eq!(with_playlist.change, CandidateChange::Library);

    let changed_playlist = PlaylistSnapshot {
        playlist: Playlist {
            id: PlaylistId::new("jellyfin:playlist:one"),
            name: "After".to_string(),
            image_ref: None,
        },
        entries: vec![
            PlaylistEntry {
                occurrence_id: "entry-one".to_string(),
                track_id: source_track.id.clone(),
            },
            PlaylistEntry {
                occurrence_id: "entry-two".to_string(),
                track_id: source_track.id.clone(),
            },
        ],
    };
    let changed = with_playlist
        .library
        .accept_playlist(PlaylistAcceptance::SourceSnapshot(changed_playlist.clone()))
        .expect("accept remote playlist readback")
        .expect("changed remote Playlist must report a change");
    assert!(changed.playlists.contains(&changed_playlist.playlist.id));

    let after_playlist = accept_track_and_playlist(
        &library,
        source_id.clone(),
        input_digest,
        source_track.clone(),
        changed_playlist.clone(),
        Some(&with_playlist.library),
        5,
    );
    assert_eq!(after_playlist.change, CandidateChange::None);
    assert!(Arc::ptr_eq(&after_playlist.library, &with_playlist.library));

    let equal = with_playlist
        .library
        .accept_playlist(PlaylistAcceptance::SourceSnapshot(changed_playlist.clone()))
        .expect("accept equal remote playlist readback");
    assert!(equal.is_none());
    let after_equal_playlist = accept_track_and_playlist(
        &library,
        source_id,
        input_digest,
        source_track,
        changed_playlist,
        Some(&with_playlist.library),
        6,
    );
    assert_eq!(after_equal_playlist.change, CandidateChange::None);
    assert!(Arc::ptr_eq(
        &after_equal_playlist.library,
        &with_playlist.library
    ));
}

#[test]
fn rejected_source_playlist_occurrences_preserve_loaded_and_reopened_order() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let library = Libraries::open(&path).expect("open Library");
    let source_id = SourceId::new("jellyfin:server:playlist-constraint");
    let source_track = track();
    let playlist_id = PlaylistId::new("jellyfin:playlist:constraint");
    let original = PlaylistSnapshot {
        playlist: Playlist {
            id: playlist_id.clone(),
            name: "Accepted order".to_string(),
            image_ref: None,
        },
        entries: vec![
            PlaylistEntry {
                occurrence_id: "first".to_string(),
                track_id: source_track.id.clone(),
            },
            PlaylistEntry {
                occurrence_id: "second".to_string(),
                track_id: source_track.id.clone(),
            },
        ],
    };
    let accepted = accept_track_and_playlist(
        &library,
        source_id.clone(),
        digest(6),
        source_track.clone(),
        original,
        None,
        1,
    );

    let error = accepted
        .library
        .accept_playlist(PlaylistAcceptance::SourceSnapshot(PlaylistSnapshot {
            playlist: Playlist {
                id: playlist_id.clone(),
                name: "Rejected order".to_string(),
                image_ref: None,
            },
            entries: vec![
                PlaylistEntry {
                    occurrence_id: "duplicate".to_string(),
                    track_id: source_track.id.clone(),
                },
                PlaylistEntry {
                    occurrence_id: "duplicate".to_string(),
                    track_id: source_track.id.clone(),
                },
            ],
        }))
        .expect_err("duplicate source occurrence must reject the transaction");
    assert!(matches!(error, library::LibraryError::Persistence(_)));

    let loaded_playlist = accepted
        .library
        .playlist_detail(&playlist_id)
        .expect("read loaded Playlist")
        .expect("loaded Playlist");
    assert_eq!(loaded_playlist.summary.playlist.name, "Accepted order");
    assert_eq!(
        (0..loaded_playlist.entries.len())
            .filter_map(|position| loaded_playlist.entries.occurrence_id(position))
            .collect::<Vec<_>>(),
        ["first", "second"]
    );

    drop(accepted);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load source")
        .expect("accepted source");
    let reopened_playlist = reopened
        .playlist_detail(&playlist_id)
        .expect("read reopened Playlist")
        .expect("reopened Playlist");
    assert_eq!(reopened_playlist.summary.playlist.name, "Accepted order");
    assert_eq!(
        (0..reopened_playlist.entries.len())
            .filter_map(|position| reopened_playlist.entries.occurrence_id(position))
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
}

#[test]
fn smart_playlist_requires_favorite_and_matches_either_artist_after_reopen() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let library = Libraries::open(&path).expect("open Library");
    let source_id = SourceId::new("jellyfin:server:all-and-any");
    let make_track = |id: &str, title: &str, artist: &str, favorite: bool, number: u16| {
        let mut candidate = track();
        candidate.id = library::TrackId::new(id);
        candidate.title = title.to_string();
        candidate.artist = artist.to_string();
        candidate.favorite = favorite;
        candidate.track_number = number;
        candidate
    };
    let accepted = accept_local_tracks(
        &library,
        source_id.clone(),
        vec![
            make_track(
                "jellyfin:track:cannons-favorite",
                "Cannons Favorite",
                "Cannons",
                true,
                1,
            ),
            make_track(
                "jellyfin:track:night-tapes-favorite",
                "Night Tapes Favorite",
                "Night Tapes",
                true,
                2,
            ),
            make_track(
                "jellyfin:track:cannons-not-favorite",
                "Cannons Not Favorite",
                "Cannons",
                false,
                3,
            ),
            make_track(
                "jellyfin:track:other-favorite",
                "Other Favorite",
                "Other Artist",
                true,
                4,
            ),
        ],
        Vec::new(),
        32,
    );
    let smart_playlist_id = created_smart_playlist_id(
        accepted
            .library
            .create_smart_playlist(
                "Favorite Artists".to_string(),
                SmartPlaylistDefinition {
                    match_all: vec![SmartPlaylistRule {
                        field: SmartPlaylistRuleField::Favorite,
                        operator: SmartPlaylistRuleOperator::Is,
                        value: Some(SmartPlaylistRuleValue::Bool(true)),
                    }],
                    match_any: vec![
                        SmartPlaylistRule {
                            field: SmartPlaylistRuleField::Artist,
                            operator: SmartPlaylistRuleOperator::Equals,
                            value: Some(SmartPlaylistRuleValue::Text("Cannons".to_string())),
                        },
                        SmartPlaylistRule {
                            field: SmartPlaylistRuleField::Artist,
                            operator: SmartPlaylistRuleOperator::Equals,
                            value: Some(SmartPlaylistRuleValue::Text("Night Tapes".to_string())),
                        },
                    ],
                    sort_field: SmartPlaylistSortField::Title,
                    descending: false,
                    limit: None,
                },
            )
            .expect("create all-and-any smart Playlist"),
    );
    let matching_ids = |loaded: &Arc<library::Library>| {
        loaded
            .smart_playlist_detail(&smart_playlist_id, None)
            .expect("read smart Playlist")
            .expect("smart Playlist")
            .tracks
            .materialize()
            .expect("materialize smart Playlist Tracks")
            .iter()
            .map(|track| track.id.as_str().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        matching_ids(&accepted.library),
        [
            "jellyfin:track:cannons-favorite",
            "jellyfin:track:night-tapes-favorite",
        ]
    );

    drop(accepted);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load source")
        .expect("source");
    assert_eq!(
        matching_ids(&reopened),
        [
            "jellyfin:track:cannons-favorite",
            "jellyfin:track:night-tapes-favorite",
        ]
    );
}

#[test]
fn identical_source_update_keeps_the_complete_refresh_shortcut() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let library = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("jellyfin:server:equal-source-update");
    let input_digest = digest(31);
    let source_track = track();
    let source_album = album_for_track(&source_track, 99);
    let source_artist = Artist {
        id: source_track.relations.artists[0].id.clone(),
        name: source_track.relations.artists[0].name.clone(),
        favorite: false,
        last_played: None,
        play_count: None,
        user_rating: None,
        musicbrainz_artist_id: None,
        image_ref: None,
        local_artwork: None,
    };
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest,
        })
        .expect("begin source candidate");
    for batch in digest_fixture(&source_track, 99, false) {
        candidate.write(batch).expect("write source facts");
    }
    let first = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept source candidate");

    let accepted = first
        .library
        .accept_source_update(SourceLibraryUpdate {
            albums: vec![source_album],
            tracks: vec![source_track.clone()],
            artists: vec![source_artist],
            removed_tracks: vec![library::TrackId::new("missing-track")],
            ..SourceLibraryUpdate::default()
        })
        .expect("accept identical source update");
    assert!(accepted.is_none());

    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id,
            input_digest,
        })
        .expect("begin equal source candidate");
    for batch in digest_fixture(&source_track, 99, false) {
        candidate.write(batch).expect("write equal source facts");
    }
    let prepared = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 2,
            },
            Some(&first.library),
        )
        .expect("prepare equal source candidate");
    assert_eq!(prepared.change(), CandidateChange::None);
    assert!(Arc::ptr_eq(prepared.library(), &first.library));
}

#[test]
fn canonical_equality_ignores_batch_order_and_projection_counts() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let library = Libraries::open(&path).expect("open Library");
    let source_id = SourceId::new("jellyfin:server:digest");
    let input_digest = digest(30);

    let mut first_track = track();
    first_track.relations.artists.push(ArtistCredit {
        id: library::ArtistId::new("local:artist:two"),
        name: "Second Artist".to_string(),
        musicbrainz_artist_id: None,
    });
    let mut first = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest,
        })
        .expect("begin first candidate");
    for batch in digest_fixture(&first_track, 99, false) {
        first.write(batch).expect("write first candidate");
    }
    let first = first
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept first candidate");
    let album_id = first_track.album_id.clone().expect("Album ID");
    let first_album = first
        .library
        .album(&album_id)
        .expect("read accepted Album")
        .expect("accepted Album");
    assert_ne!(first_album.color_seed, 99);
    let playlist_id = PlaylistId::new("playlist:digest");
    assert_eq!(
        first
            .library
            .playlist_detail(&playlist_id)
            .expect("read accepted Playlist")
            .expect("accepted Playlist")
            .summary
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>(),
        ["Rock"]
    );

    let mut second = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest,
        })
        .expect("begin reordered candidate");
    for batch in digest_fixture(&first_track, 1, true) {
        second.write(batch).expect("write reordered candidate");
    }
    let second = second
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 2,
            },
            Some(&first.library),
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept reordered candidate");
    assert_eq!(second.change, CandidateChange::None);
    assert!(Arc::ptr_eq(&second.library, &first.library));

    let mut reordered_relations = first_track;
    reordered_relations.relations.artists.swap(0, 1);
    let mut changed = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest,
        })
        .expect("begin relationship candidate");
    for batch in digest_fixture(&reordered_relations, 1, false) {
        changed.write(batch).expect("write relationship candidate");
    }
    let changed = changed
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 3,
            },
            Some(&first.library),
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept relationship candidate");
    assert_eq!(changed.change, CandidateChange::Library);
    assert!(!Arc::ptr_eq(&changed.library, &first.library));

    let changed_album = changed
        .library
        .album(&album_id)
        .expect("read changed Album")
        .expect("changed Album");
    let changed_seed = changed_album.color_seed;
    drop(changed);
    drop(second);
    drop(first);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load source")
        .expect("accepted source");
    assert_eq!(
        reopened
            .album(&album_id)
            .expect("read reopened Album")
            .expect("reopened Album")
            .color_seed,
        changed_seed
    );
    assert_eq!(
        reopened
            .playlist_detail(&playlist_id)
            .expect("read reopened Playlist")
            .expect("reopened Playlist")
            .summary
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>(),
        ["Rock"]
    );
}

#[test]
fn failed_candidate_batch_cannot_accept_partially_persisted_rows() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let library = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("local:server:failed-batch");
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: digest(31),
        })
        .expect("begin candidate");
    let track = track();
    let mut albums = (0..500)
        .map(|index| {
            let mut album = album_for_track(&track, 0);
            album.id = library::AlbumId::new(format!("album:{index:03}"));
            album.title = format!("Album {index:03}");
            album
        })
        .collect::<Vec<_>>();
    albums.push(albums[0].clone());
    candidate
        .write(CandidateBatch::Albums(albums))
        .expect_err("second bounded write transaction must reject the duplicate");
    assert!(matches!(
        candidate.finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        ),
        Err(library::LibraryError::CandidateWriteFailed)
    ));

    let accepted = accept(&library, source_id, digest(32), "Recovered", None, None);
    assert_eq!(accepted.change, CandidateChange::Library);
    assert_eq!(accepted.library.albums(None).expect("Albums").len(), 1);
}

#[test]
fn local_favorite_and_playlist_transactions_reopen_without_parallel_truth() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let library = Libraries::open(&path).expect("open Library");
    let source_id = SourceId::new("local:server:user-data");
    let accepted = accept(&library, source_id.clone(), digest(10), "Track", None, None);
    let track_list = accepted
        .library
        .track_list(None, library::TrackSort::Title, false)
        .expect("Tracks");
    let album_id = track_list
        .track(0)
        .expect("read first Track")
        .expect("first Track")
        .album_id
        .clone()
        .expect("Album ID");
    let mounted_home = accepted
        .library
        .home(None)
        .expect("mounted Home before favorite");
    let favorite = accepted
        .library
        .accept_favorite(FavoriteAcceptance::RufinOwned {
            item: FavoriteItemId::Album(album_id.clone()),
            favorite: true,
        })
        .expect("favorite Album");
    assert_eq!(
        favorite.home,
        AcceptedHomeChange::Favorite(FavoriteItemId::Album(album_id.clone()))
    );
    assert!(favorite.download_coverage_changed);
    let next_home = accepted
        .library
        .home_after_accepted_change(None, &mounted_home, &favorite.home)
        .expect("patch favorite in next Home")
        .expect("favorite changes the next Home");
    assert!(
        accepted
            .library
            .home_after_accepted_change(None, &mounted_home, &AcceptedHomeChange::Keep)
            .expect("keep the next Home")
            .is_none()
    );
    let mounted_album = mounted_home
        .section(HomeSectionKind::Explore)
        .expect("mounted Explore")
        .items
        .iter()
        .find_map(|item| match item {
            LoadedHomeItem::Album(album) if album.album.id == album_id => Some(album),
            _ => None,
        })
        .expect("mounted Home Album");
    let next_album = next_home
        .section(HomeSectionKind::Explore)
        .expect("next Explore")
        .items
        .iter()
        .find_map(|item| match item {
            LoadedHomeItem::Album(album) if album.album.id == album_id => Some(album),
            _ => None,
        })
        .expect("next Home Album");
    assert!(!mounted_album.album.favorite);
    assert!(next_album.album.favorite);
    let favorite_album = accepted
        .library
        .album(&album_id)
        .expect("read favorite Album")
        .expect("favorite Album");
    assert!(favorite.tracks.is_empty());
    assert!(favorite.albums.is_empty());
    assert!(favorite.artists.is_empty());
    assert_eq!(
        favorite.favorite,
        Some(library::FavoriteAcknowledgement {
            item: FavoriteItemId::Album(album_id.clone()),
            favorite: true,
        })
    );
    assert!(favorite_album.favorite);
    let playlist_id = created_playlist_id(
        accepted
            .library
            .accept_playlist(PlaylistAcceptance::RufinOwned(PlaylistEdit::Create {
                name: "User list".to_string(),
                track_ids: vec![
                    library::TrackId::new("local:track:one"),
                    library::TrackId::new("local:track:one"),
                ],
            }))
            .expect("save Local Playlist"),
    );
    let empty_playlist_id = created_playlist_id(
        accepted
            .library
            .accept_playlist(PlaylistAcceptance::RufinOwned(PlaylistEdit::Create {
                name: "Empty list".to_string(),
                track_ids: Vec::new(),
            }))
            .expect("save empty Local Playlist"),
    );
    let repeated_incoming = accepted
        .library
        .prepare_playlist_add(library::PlaylistTrackAdd {
            playlist_id: empty_playlist_id,
            track_ids: vec![
                library::TrackId::new("local:track:one"),
                library::TrackId::new("local:track:one"),
            ],
            skip_duplicates: true,
        })
        .expect("prepare repeated incoming tracks")
        .expect("tracks remain");
    assert!(matches!(
        repeated_incoming,
        PlaylistEdit::AddTracks { track_ids, .. } if track_ids.len() == 2
    ));
    assert!(
        accepted
            .library
            .prepare_playlist_add(library::PlaylistTrackAdd {
                playlist_id: playlist_id.clone(),
                track_ids: vec![library::TrackId::new("local:track:one")],
                skip_duplicates: true,
            },)
            .expect("prepare existing track")
            .is_none()
    );

    let unchanged = accept(
        &library,
        source_id.clone(),
        digest(10),
        "Track",
        None,
        Some(&accepted.library),
    );
    assert_eq!(unchanged.change, CandidateChange::None);
    assert!(Arc::ptr_eq(&unchanged.library, &accepted.library));
    assert!(
        unchanged
            .library
            .album(&album_id)
            .expect("read unchanged Album")
            .expect("unchanged Album")
            .favorite
    );
    assert!(
        unchanged
            .library
            .playlist_detail(&playlist_id)
            .expect("read unchanged Playlist")
            .is_some()
    );

    drop(accepted);
    drop(library);
    let library = Libraries::open(&path).expect("reopen Library");
    let reopened = library
        .load_source(&source_id)
        .expect("load source")
        .expect("source");
    assert!(
        reopened
            .album(&album_id)
            .expect("read Album")
            .expect("Album")
            .favorite
    );
    assert_eq!(
        reopened
            .playlist_detail(&playlist_id)
            .expect("read Playlist")
            .expect("Playlist")
            .entries
            .len(),
        2
    );

    reopened
        .accept_favorite(FavoriteAcceptance::RufinOwned {
            item: FavoriteItemId::Album(album_id.clone()),
            favorite: false,
        })
        .expect("unfavorite Album");
    drop(reopened);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen after false")
        .load_source(&source_id)
        .expect("load source")
        .expect("source");
    assert!(
        !reopened
            .album(&album_id)
            .expect("read Album")
            .expect("Album")
            .favorite
    );
}

#[test]
fn local_files_preserve_full_filesystem_identities() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:filesystem-identities");
    let identity_values = [0, i64::MAX as u64, (i64::MAX as u64) + 1, u64::MAX];
    let files = identity_values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let mut file = local_audio_file(
                &format!("/music/Track-{index}.flac"),
                &format!("Track-{index}.flac"),
            );
            file.device_id = Some(value);
            file.inode = Some(value);
            file
        })
        .collect::<Vec<_>>();
    let seeds = files
        .iter()
        .map(|file| LocalFileSeed::Path(file.path.clone()))
        .collect::<Vec<_>>();
    let library = Libraries::open(&path).expect("open Library");
    let accepted = accept_local_tracks(&library, source_id.clone(), Vec::new(), files.clone(), 101);

    assert_eq!(
        accepted
            .library
            .local_file_baseline(&seeds)
            .expect("read accepted filesystem identities")
            .files,
        files
    );

    let mut replaced = files[2].clone();
    replaced.mtime_ns = 2;
    replaced.device_id = Some(u64::MAX);
    replaced.inode = Some((i64::MAX as u64) + 1);
    accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 2,
            files: vec![replaced.clone()],
            ..LocalComponentReplacement::default()
        })
        .expect("replace a high-bit filesystem identity");
    let mut expected = files;
    expected[2] = replaced;
    assert_eq!(
        accepted
            .library
            .local_file_baseline(&seeds)
            .expect("read replaced filesystem identities")
            .files,
        expected
    );

    drop(accepted);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load source")
        .expect("source");
    assert_eq!(
        reopened
            .local_file_baseline(&seeds)
            .expect("read reopened filesystem identities")
            .files,
        expected
    );
}

#[test]
fn local_component_replaces_files_relations_and_dormant_user_data_atomically() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:component");
    let library = Libraries::open(&path).expect("open Library");
    let original = track();
    let original_album_id = original.album_id.clone().expect("original Album ID");
    let original_artist_id = original.relations.artists[0].id.clone();
    let original_genre_id = original.relations.genres[0].id.clone();
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: digest(40),
        })
        .expect("begin Local candidate");
    candidate
        .write(CandidateBatch::Albums(vec![album_for_track(&original, 0)]))
        .expect("write Album");
    candidate
        .write(CandidateBatch::Tracks(vec![original.clone()]))
        .expect("write Track");
    candidate
        .write(CandidateBatch::Artists(vec![Artist {
            id: original_artist_id.clone(),
            name: "Artist".to_string(),
            favorite: false,
            last_played: None,
            play_count: None,
            user_rating: None,
            musicbrainz_artist_id: None,
            image_ref: None,
            local_artwork: None,
        }]))
        .expect("write Artist");
    candidate
        .write(CandidateBatch::Genres(vec![Genre {
            id: original_genre_id.clone(),
            name: "Rock".to_string(),
            image_ref: None,
        }]))
        .expect("write Genre");
    candidate
        .write(CandidateBatch::LocalFiles(vec![local_audio_file(
            "/music/Artist/Album/Track.flac",
            "Artist/Album/Track.flac",
        )]))
        .expect("write Local file");
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept Local candidate");
    accepted
        .library
        .accept_favorite(FavoriteAcceptance::RufinOwned {
            item: FavoriteItemId::Track(original.id.clone()),
            favorite: true,
        })
        .expect("favorite Track");
    let playlist_id = created_playlist_id(
        accepted
            .library
            .accept_playlist(PlaylistAcceptance::RufinOwned(PlaylistEdit::Create {
                name: "Kept list".to_string(),
                track_ids: vec![original.id.clone()],
            }))
            .expect("save Local Playlist"),
    );
    let created_playlist = accepted
        .library
        .playlist_detail(&playlist_id)
        .expect("read created Playlist")
        .expect("created Playlist");
    let kept_occurrence = playlist_entry(&created_playlist.entries, 0).occurrence_id;

    let new_album_id = library::AlbumId::new("local:album:component-new");
    let new_artist_id = library::ArtistId::new("local:artist:component-new");
    let new_genre_id = GenreId::new("local:genre:component-jazz");
    let new_artist_credit = ArtistCredit {
        id: new_artist_id.clone(),
        name: "New Artist".to_string(),
        musicbrainz_artist_id: None,
    };
    let mut changed = original.clone();
    changed.album_id = Some(new_album_id.clone());
    changed.album = "New Album".to_string();
    changed.artist = "New Artist".to_string();
    changed.title = "Changed Track".to_string();
    changed.source_path = Some("/music/New Artist/New Album/Changed.flac".to_string());
    changed.relations = TrackRelations {
        artists: vec![new_artist_credit.clone()],
        album_artists: vec![new_artist_credit],
        genres: vec![GenreCredit {
            id: new_genre_id.clone(),
            name: "Jazz".to_string(),
        }],
        moods: Vec::new(),
        music_folders: Vec::new(),
    };
    let result = accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 3,
            files: vec![local_audio_file(
                "/music/New Artist/New Album/Changed.flac",
                "New Artist/New Album/Changed.flac",
            )],
            removed_paths: vec!["/music/Artist/Album/Track.flac".to_string()],
            albums: vec![album_for_track(&changed, 0)],
            tracks: vec![changed.clone()],
            artists: vec![Artist {
                id: new_artist_id.clone(),
                name: "New Artist".to_string(),
                favorite: false,
                last_played: None,
                play_count: None,
                user_rating: None,
                musicbrainz_artist_id: None,
                image_ref: None,
                local_artwork: None,
            }],
            genres: vec![Genre {
                id: new_genre_id.clone(),
                name: "Jazz".to_string(),
                image_ref: None,
            }],
            removed_album_ids: vec![original_album_id.clone()],
            removed_track_ids: Vec::new(),
            removed_artist_ids: vec![original_artist_id.clone()],
            removed_genre_ids: vec![original_genre_id.clone()],
        })
        .expect("accept Local component")
        .expect("changed Local component must report a change");
    assert_eq!(result.home, AcceptedHomeChange::Rebuild);
    assert!(result.download_coverage_changed);
    assert_eq!(result.tracks.len(), 1);
    assert_eq!(result.tracks[0].id, changed.id);
    assert_eq!(
        result.tracks[0]
            .track
            .as_ref()
            .expect("accepted Local Track")
            .title,
        "Changed Track"
    );
    assert!(result.albums.contains(&original_album_id));
    assert!(result.albums.contains(&new_album_id));
    assert!(result.artists.contains(&original_artist_id));
    assert!(result.artists.contains(&new_artist_id));
    assert!(result.genres.contains(&original_genre_id));
    assert!(result.genres.contains(&new_genre_id));
    assert!(result.local_folders_changed);
    assert!(result.playlists.contains(&playlist_id));

    let changed_track = accepted
        .library
        .track(&changed.id)
        .expect("read changed Track")
        .expect("changed Track");
    assert_eq!(changed_track.title, "Changed Track");
    assert!(changed_track.favorite);
    assert!(
        accepted
            .library
            .album(&original_album_id)
            .expect("read removed Album")
            .is_none()
    );
    assert!(
        accepted
            .library
            .artist(&original_artist_id)
            .expect("read removed Artist")
            .is_none()
    );
    assert!(
        accepted
            .library
            .genre_detail(&original_genre_id, None)
            .expect("read removed Genre")
            .is_none()
    );
    assert_eq!(
        accepted
            .library
            .playable_file(&changed.id)
            .expect("read playable file")
            .expect("playable file")
            .path(),
        std::path::Path::new("/music/New Artist/New Album/Changed.flac")
    );
    let playlist = accepted
        .library
        .playlist_detail(&playlist_id)
        .expect("read Local Playlist")
        .expect("Local Playlist");
    let entry = playlist_entry(&playlist.entries, 0);
    assert_eq!(entry.occurrence_id, kept_occurrence);
    assert_eq!(entry.track.title, "Changed Track");
    assert_eq!(
        playlist
            .summary
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>(),
        ["Jazz"]
    );
    let baseline = accepted
        .library
        .local_component_baseline(&[LocalComponentSeed::DirectoryTree("/music".to_string())])
        .expect("read Local inventory baseline");
    assert_eq!(
        baseline
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["/music/New Artist/New Album/Changed.flac"]
    );

    drop(accepted);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load source")
        .expect("source");
    assert!(
        reopened
            .track(&changed.id)
            .expect("read reopened Track")
            .expect("reopened Track")
            .favorite
    );
    assert!(
        reopened
            .album(&original_album_id)
            .expect("read reopened removed Album")
            .is_none()
    );
    assert_eq!(
        reopened
            .playlist_detail(&playlist_id)
            .expect("read reopened Playlist")
            .expect("reopened Playlist")
            .summary
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>(),
        ["Jazz"]
    );
    assert_eq!(
        reopened
            .playable_file(&changed.id)
            .expect("read reopened playable file")
            .expect("reopened playable file")
            .path(),
        std::path::Path::new("/music/New Artist/New Album/Changed.flac")
    );
}

#[test]
fn local_file_readiness_rebinds_attached_tracks_and_matches_reopen() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:readiness");
    let library = Libraries::open(&path).expect("open Library");
    let track = track();
    let audio_path = track.source_path.clone().expect("audio path");
    let accepted = accept_local_tracks(
        &library,
        source_id.clone(),
        vec![track.clone()],
        vec![local_audio_file(&audio_path, "Artist/Album/Track.flac")],
        41,
    );
    assert!(
        accepted
            .library
            .playable_file(&track.id)
            .expect("read initial playable file")
            .is_some()
    );

    let mut unreadable = local_audio_file(&audio_path, "Artist/Album/Track.flac");
    unreadable.state = LocalFileState::Unreadable;
    accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 2,
            files: vec![unreadable],
            ..LocalComponentReplacement::default()
        })
        .expect("accept unreadable observation");
    assert!(
        accepted
            .library
            .track(&track.id)
            .expect("read retained Track")
            .is_some()
    );
    assert!(
        accepted
            .library
            .playable_file(&track.id)
            .expect("read unavailable file")
            .is_none()
    );

    let accepted_again = local_audio_file(&audio_path, "Artist/Album/Track.flac");
    accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 4,
            files: vec![accepted_again],
            ..LocalComponentReplacement::default()
        })
        .expect("accept readable media again");
    assert!(
        accepted
            .library
            .playable_file(&track.id)
            .expect("read restored playable file")
            .is_some()
    );

    drop(accepted);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load source")
        .expect("source");
    assert!(
        reopened
            .playable_file(&track.id)
            .expect("read reopened playable file")
            .is_some()
    );
}

#[test]
fn full_local_rescan_drops_unreadable_music_until_the_source_recovers() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:unreadable");
    let library = Libraries::open(&path).expect("open Library");
    let original = track();
    let album = album_for_track(&original, 1);
    let artist = Artist {
        id: original.relations.artists[0].id.clone(),
        name: "Artist".to_string(),
        favorite: false,
        last_played: None,
        play_count: None,
        user_rating: None,
        musicbrainz_artist_id: Some("accepted-artist-mbid".to_string()),
        image_ref: None,
        local_artwork: None,
    };
    let genre = Genre {
        id: original.relations.genres[0].id.clone(),
        name: "Rock".to_string(),
        image_ref: None,
    };
    let mut first = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: digest(70),
        })
        .expect("begin initial Local candidate");
    for batch in [
        CandidateBatch::Albums(vec![album.clone()]),
        CandidateBatch::Tracks(vec![original.clone()]),
        CandidateBatch::Artists(vec![artist.clone()]),
        CandidateBatch::Genres(vec![genre.clone()]),
        CandidateBatch::LocalFiles(vec![local_audio_file(
            "/music/Artist/Album/Track.flac",
            "Artist/Album/Track.flac",
        )]),
    ] {
        first.write(batch).expect("write initial Local facts");
    }
    let accepted = first
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept initial Local library");

    let mut previously_accepted =
        local_audio_file("/music/Artist/Album/Track.flac", "Artist/Album/Track.flac");
    previously_accepted.mtime_ns = 2;
    previously_accepted.state = LocalFileState::Unreadable;
    let mut new_unreadable =
        local_audio_file("/music/Artist/Album/New.flac", "Artist/Album/New.flac");
    new_unreadable.state = LocalFileState::Unreadable;
    let mut rescanned = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: digest(70),
        })
        .expect("begin Local rescan");
    rescanned
        .write(CandidateBatch::LocalFiles(vec![
            previously_accepted,
            new_unreadable,
        ]))
        .expect("write unreadable observations");
    let result = rescanned
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 2,
            },
            Some(&accepted.library),
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept Local rescan");

    assert_eq!(result.change, CandidateChange::Library);
    assert!(
        result
            .library
            .track(&original.id)
            .expect("read removed Track")
            .is_none()
    );
    assert!(
        result
            .library
            .album(&album.id)
            .expect("read removed Album")
            .is_none()
    );
    assert!(
        result
            .library
            .artist(&artist.id)
            .expect("read removed Artist")
            .is_none()
    );
    assert!(
        result
            .library
            .genre(&genre.id)
            .expect("read removed Genre")
            .is_none()
    );
    assert_eq!(
        result
            .library
            .track_list(None, TrackSort::Title, false)
            .expect("read Local Tracks")
            .len(),
        0,
        "unreadable files remain observations rather than stale music facts"
    );

    drop(result);
    let empty = Libraries::open(&path)
        .expect("reopen empty Local Library")
        .load_source(&source_id)
        .expect("load Local source")
        .expect("accepted Local source");
    assert!(
        empty
            .track(&original.id)
            .expect("read removed reopened Track")
            .is_none()
    );

    let mut recovered = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: digest(70),
        })
        .expect("begin recovered Local candidate");
    for batch in [
        CandidateBatch::Albums(vec![album]),
        CandidateBatch::Tracks(vec![original.clone()]),
        CandidateBatch::Artists(vec![artist]),
        CandidateBatch::Genres(vec![genre]),
        CandidateBatch::LocalFiles(vec![local_audio_file(
            "/music/Artist/Album/Track.flac",
            "Artist/Album/Track.flac",
        )]),
    ] {
        recovered.write(batch).expect("write recovered Local facts");
    }
    let recovered = recovered
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 3,
            },
            Some(&empty),
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept recovered Local library");
    assert_eq!(
        recovered
            .library
            .track_list(None, TrackSort::Title, false)
            .expect("read recovered Tracks")
            .len(),
        1
    );

    drop(recovered);
    drop(empty);
    drop(accepted);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen recovered Library")
        .load_source(&source_id)
        .expect("load recovered Local source")
        .expect("recovered Local source");
    assert!(
        reopened
            .track(&original.id)
            .expect("read reopened recovered Track")
            .is_some()
    );
}

#[test]
fn local_retag_preserves_activity_and_smart_playlist_membership() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:activity-retag");
    let library = Libraries::open(&path).expect("open Library");
    let original = track();
    let accepted = accept_local_tracks(
        &library,
        source_id.clone(),
        vec![original.clone()],
        Vec::new(),
        42,
    );
    accepted
        .library
        .accept_favorite(FavoriteAcceptance::RufinOwned {
            item: FavoriteItemId::Track(original.id.clone()),
            favorite: true,
        })
        .expect("favorite Local Track");
    let playlist_id = created_playlist_id(
        accepted
            .library
            .accept_playlist(PlaylistAcceptance::RufinOwned(PlaylistEdit::Create {
                name: "Retagged Local Track".to_string(),
                track_ids: vec![original.id.clone()],
            }))
            .expect("create Local playlist"),
    );
    let play = accepted
        .library
        .record_play(AcceptedPlay {
            play_id: "local-play:retag".to_string(),
            track_id: original.id.clone(),
            played_at: 1_700_000_100,
            month: "2023-11".to_string(),
        })
        .expect("record play")
        .expect("new play");
    accepted
        .library
        .apply_recorded_activity(&play)
        .expect("apply play");
    let skip = accepted
        .library
        .record_skip(AcceptedSkip {
            track_id: original.id.clone(),
        })
        .expect("record skip");
    let skip_change = accepted
        .library
        .apply_recorded_activity(&skip)
        .expect("apply skip")
        .expect("accepted skip changes activity");
    assert_eq!(skip_change.home, AcceptedHomeChange::Keep);
    assert!(!skip_change.download_coverage_changed);
    let smart_id = created_smart_playlist_id(
        accepted
            .library
            .create_smart_playlist(
                "Played Local".to_string(),
                SmartPlaylistDefinition {
                    match_all: vec![SmartPlaylistRule {
                        field: SmartPlaylistRuleField::PlayCount,
                        operator: SmartPlaylistRuleOperator::Above,
                        value: Some(SmartPlaylistRuleValue::Number(0)),
                    }],
                    match_any: Vec::new(),
                    sort_field: SmartPlaylistSortField::PlayCount,
                    descending: true,
                    limit: None,
                },
            )
            .expect("create smart playlist"),
    );

    let mut retagged = original.clone();
    retagged.title = "Retagged".to_string();
    retagged.play_count = None;
    retagged.skip_count = None;
    retagged.last_played = None;
    let retag = accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 2,
            tracks: vec![retagged.clone()],
            ..LocalComponentReplacement::default()
        })
        .expect("accept retag")
        .expect("retag must change the library");
    let effective = accepted
        .library
        .track(&retagged.id)
        .expect("read retagged Track")
        .expect("retagged Track");
    let returned = retag
        .tracks
        .iter()
        .find(|replacement| replacement.id == retagged.id)
        .and_then(|replacement| replacement.track.as_ref())
        .expect("returned retagged Track");
    assert!(Track::ptr_eq(&effective, returned));
    assert_eq!(effective.title, "Retagged");
    assert!(effective.favorite);
    assert_eq!(effective.play_count, Some(1));
    assert_eq!(effective.skip_count, Some(1));
    assert!(effective.last_played.is_some());
    let playlist = accepted
        .library
        .playlist_detail(&playlist_id)
        .expect("read Local playlist")
        .expect("Local playlist");
    assert_eq!(playlist.entries.len(), 1);
    assert_eq!(
        playlist
            .entries
            .entry(0)
            .expect("read Local playlist entry")
            .expect("Local playlist entry")
            .track
            .title,
        "Retagged"
    );
    assert_eq!(
        accepted
            .library
            .smart_playlist_detail(&smart_id, None)
            .expect("read smart playlist")
            .expect("smart playlist")
            .tracks
            .len(),
        1
    );
    assert_eq!(
        accepted
            .library
            .album(retagged.album_id.as_ref().expect("retagged Track Album ID"))
            .expect("read sparse Album")
            .expect("sparse Album")
            .play_count,
        Some(1)
    );

    drop(accepted);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load source")
        .expect("source");
    let reopened_track = reopened
        .track(&retagged.id)
        .expect("read reopened Track")
        .expect("reopened Track");
    assert_eq!(reopened_track.title, "Retagged");
    assert!(reopened_track.favorite);
    assert_eq!(reopened_track.play_count, Some(1));
    assert_eq!(reopened_track.skip_count, Some(1));
    assert_eq!(
        reopened
            .playlist_detail(&playlist_id)
            .expect("read reopened Local playlist")
            .expect("reopened Local playlist")
            .entries
            .len(),
        1
    );
    assert_eq!(
        reopened
            .smart_playlist_detail(&smart_id, None)
            .expect("read reopened smart playlist")
            .expect("reopened smart playlist")
            .tracks
            .len(),
        1
    );
}

#[test]
fn local_retag_transfers_aggregate_favorites_to_unique_replacements() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:aggregate-favorite-retag");
    let library = Libraries::open(&path).expect("open Library");
    let collaborator = ArtistCredit {
        id: library::ArtistId::new("local:artist:collaborator"),
        name: "Collaborator".to_string(),
        musicbrainz_artist_id: None,
    };
    let mut original = track();
    original.relations.artists.push(collaborator.clone());
    original.relations.album_artists.push(collaborator.clone());
    let old_album_id = original.album_id.clone().expect("old Album ID");
    let old_artist_id = original.relations.artists[0].id.clone();
    let accepted = accept_local_tracks(
        &library,
        source_id.clone(),
        vec![original.clone()],
        Vec::new(),
        43,
    );
    for item in [
        FavoriteItemId::Album(old_album_id.clone()),
        FavoriteItemId::Artist(old_artist_id.clone()),
    ] {
        accepted
            .library
            .accept_favorite(FavoriteAcceptance::RufinOwned {
                item,
                favorite: true,
            })
            .expect("favorite Local aggregate");
    }

    let new_album_id = library::AlbumId::new("local:album:retagged");
    let new_artist_id = library::ArtistId::new("local:artist:retagged");
    let new_artist = ArtistCredit {
        id: new_artist_id.clone(),
        name: "Retagged Artist".to_string(),
        musicbrainz_artist_id: None,
    };
    let mut retagged = original.clone();
    retagged.album_id = Some(new_album_id.clone());
    retagged.album = "Retagged Album".to_string();
    retagged.artist = "Retagged Artist".to_string();
    retagged.relations.artists = vec![new_artist.clone(), collaborator.clone()];
    retagged.relations.album_artists = vec![new_artist, collaborator];
    accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 2,
            albums: vec![album_for_track(&retagged, 0)],
            tracks: vec![retagged.clone()],
            artists: vec![artist_for_track(&retagged)],
            removed_album_ids: vec![old_album_id.clone()],
            removed_artist_ids: vec![old_artist_id.clone()],
            ..LocalComponentReplacement::default()
        })
        .expect("accept aggregate identity retag")
        .expect("aggregate identity retag must change the library");

    assert!(
        accepted
            .library
            .album(&old_album_id)
            .expect("read removed Album")
            .is_none()
    );
    assert!(
        accepted
            .library
            .artist(&old_artist_id)
            .expect("read removed Artist")
            .is_none()
    );
    assert!(
        accepted
            .library
            .album(&new_album_id)
            .expect("read replacement Album")
            .expect("replacement Album")
            .favorite
    );
    assert!(
        accepted
            .library
            .artist(&new_artist_id)
            .expect("read replacement Artist")
            .expect("replacement Artist")
            .favorite
    );

    drop(accepted);
    drop(library);
    let reopened = Libraries::open(&path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load source")
        .expect("source");
    assert!(
        reopened
            .album(&new_album_id)
            .expect("read reopened replacement Album")
            .expect("reopened replacement Album")
            .favorite
    );
    assert!(
        reopened
            .artist(&new_artist_id)
            .expect("read reopened replacement Artist")
            .expect("reopened replacement Artist")
            .favorite
    );
    drop(reopened);

    let connection = rusqlite::Connection::open(path).expect("inspect Local favorites");
    let mut statement = connection
        .prepare(
            "SELECT item_kind, item_id
             FROM local_favorites
             WHERE source_id = ?1
             ORDER BY item_kind, item_id",
        )
        .expect("prepare Local favorite read");
    let favorites = statement
        .query_map([source_id.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("read Local favorites")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect Local favorites");
    assert_eq!(
        favorites,
        [
            ("album".to_string(), new_album_id.to_string()),
            ("artist".to_string(), new_artist_id.to_string()),
        ]
    );
}

#[test]
fn local_retag_transfers_favorites_to_existing_aggregates() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:existing-aggregate-retag");
    let library = Libraries::open(&path).expect("open Library");
    let original = track();
    let old_album_id = original.album_id.clone().expect("old Album ID");
    let old_artist_id = original.relations.artists[0].id.clone();
    let existing_album_id = library::AlbumId::new("local:album:existing");
    let existing_artist_id = library::ArtistId::new("local:artist:existing");
    let existing_artist = ArtistCredit {
        id: existing_artist_id.clone(),
        name: "Existing Artist".to_string(),
        musicbrainz_artist_id: None,
    };
    let mut existing = original.clone();
    existing.id = library::TrackId::new("local:track:existing");
    existing.album_id = Some(existing_album_id.clone());
    existing.title = "Existing Track".to_string();
    existing.artist = existing_artist.name.clone();
    existing.album = "Existing Album".to_string();
    existing.source_path = Some("/music/Existing/Album/Track.flac".to_string());
    existing.relations.artists = vec![existing_artist.clone()];
    existing.relations.album_artists = vec![existing_artist.clone()];
    let accepted = accept_local_tracks(
        &library,
        source_id.clone(),
        vec![original.clone(), existing],
        Vec::new(),
        45,
    );
    for item in [
        FavoriteItemId::Album(old_album_id.clone()),
        FavoriteItemId::Artist(old_artist_id.clone()),
    ] {
        accepted
            .library
            .accept_favorite(FavoriteAcceptance::RufinOwned {
                item,
                favorite: true,
            })
            .expect("favorite Local aggregate");
    }

    let mut retagged = original;
    retagged.album_id = Some(existing_album_id.clone());
    retagged.artist = existing_artist.name.clone();
    retagged.album = "Existing Album".to_string();
    retagged.relations.artists = vec![existing_artist.clone()];
    retagged.relations.album_artists = vec![existing_artist];
    accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 2,
            albums: vec![album_for_track(&retagged, 0)],
            tracks: vec![retagged.clone()],
            artists: vec![artist_for_track(&retagged)],
            removed_album_ids: vec![old_album_id.clone()],
            removed_artist_ids: vec![old_artist_id.clone()],
            ..LocalComponentReplacement::default()
        })
        .expect("accept retag into existing aggregates")
        .expect("aggregate retag must change the library");

    assert!(
        accepted
            .library
            .album(&existing_album_id)
            .expect("read existing Album")
            .expect("existing Album")
            .favorite
    );
    assert!(
        accepted
            .library
            .artist(&existing_artist_id)
            .expect("read existing Artist")
            .expect("existing Artist")
            .favorite
    );

    drop(accepted);
    drop(library);
    let connection = rusqlite::Connection::open(path).expect("inspect transferred Local favorites");
    for (kind, id) in [
        ("album", old_album_id.as_str()),
        ("artist", old_artist_id.as_str()),
    ] {
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*)
                     FROM local_favorites
                     WHERE source_id = ?1 AND item_kind = ?2 AND item_id = ?3",
                    rusqlite::params![source_id.as_str(), kind, id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("read dormant Local favorite"),
            0
        );
    }
    for (kind, id) in [
        ("album", existing_album_id.as_str()),
        ("artist", existing_artist_id.as_str()),
    ] {
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*)
                     FROM local_favorites
                     WHERE source_id = ?1 AND item_kind = ?2 AND item_id = ?3",
                    rusqlite::params![source_id.as_str(), kind, id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("read existing Local favorite"),
            1
        );
    }
}

#[test]
fn local_album_favorite_stays_dormant_when_tracks_split_between_new_and_existing_albums() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:mixed-album-split");
    let library = Libraries::open(&path).expect("open Library");
    let first = track();
    let old_album_id = first.album_id.clone().expect("old Album ID");
    let mut second = first.clone();
    second.id = library::TrackId::new("local:track:two");
    second.title = "Second Track".to_string();
    second.track_number = 2;
    second.source_path = Some("/music/Artist/Album/Second.flac".to_string());

    let existing_album_id = library::AlbumId::new("local:album:existing");
    let mut existing = first.clone();
    existing.id = library::TrackId::new("local:track:existing");
    existing.album_id = Some(existing_album_id.clone());
    existing.title = "Existing Track".to_string();
    existing.album = "Existing Album".to_string();
    existing.source_path = Some("/music/Artist/Existing/Track.flac".to_string());
    let accepted = accept_local_tracks(
        &library,
        source_id.clone(),
        vec![first.clone(), second.clone(), existing],
        Vec::new(),
        46,
    );
    accepted
        .library
        .accept_favorite(FavoriteAcceptance::RufinOwned {
            item: FavoriteItemId::Album(old_album_id.clone()),
            favorite: true,
        })
        .expect("favorite original Local Album");

    let new_album_id = library::AlbumId::new("local:album:new");
    let mut into_new = first;
    into_new.album_id = Some(new_album_id.clone());
    into_new.album = "New Album".to_string();
    let mut into_existing = second;
    into_existing.album_id = Some(existing_album_id.clone());
    into_existing.album = "Existing Album".to_string();
    accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 2,
            albums: vec![
                album_for_track(&into_new, 0),
                album_for_track(&into_existing, 0),
            ],
            tracks: vec![into_new, into_existing],
            removed_album_ids: vec![old_album_id.clone()],
            ..LocalComponentReplacement::default()
        })
        .expect("accept mixed Album split")
        .expect("mixed Album split must change the library");

    for album_id in [&new_album_id, &existing_album_id] {
        assert!(
            !accepted
                .library
                .album(album_id)
                .expect("read split Album")
                .expect("split Album")
                .favorite
        );
    }

    drop(accepted);
    drop(library);
    let connection = rusqlite::Connection::open(path).expect("inspect dormant Local favorite");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*)
                 FROM local_favorites
                 WHERE source_id = ?1 AND item_kind = 'album' AND item_id = ?2",
                rusqlite::params![source_id.as_str(), old_album_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .expect("read dormant Local Album favorite"),
        1
    );
}

#[test]
fn local_retag_keeps_split_favorites_dormant_and_merges_unique_targets() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:ambiguous-aggregate-retag");
    let library = Libraries::open(&path).expect("open Library");
    let first = track();
    let mut second = first.clone();
    second.id = library::TrackId::new("local:track:two");
    second.title = "Second Track".to_string();
    second.track_number = 2;
    second.source_path = Some("/music/Artist/Album/Second.flac".to_string());
    let old_album_id = first.album_id.clone().expect("old Album ID");
    let old_artist_id = first.relations.artists[0].id.clone();
    let accepted = accept_local_tracks(
        &library,
        source_id.clone(),
        vec![first.clone(), second.clone()],
        Vec::new(),
        44,
    );
    for item in [
        FavoriteItemId::Album(old_album_id.clone()),
        FavoriteItemId::Artist(old_artist_id.clone()),
    ] {
        accepted
            .library
            .accept_favorite(FavoriteAcceptance::RufinOwned {
                item,
                favorite: true,
            })
            .expect("favorite Local aggregate");
    }

    let first_album_id = library::AlbumId::new("local:album:split-one");
    let second_album_id = library::AlbumId::new("local:album:split-two");
    let first_artist_id = library::ArtistId::new("local:artist:split-one");
    let second_artist_id = library::ArtistId::new("local:artist:split-two");
    let common_artist_id = library::ArtistId::new("local:artist:split-common");
    let common_artist = ArtistCredit {
        id: common_artist_id.clone(),
        name: "Common Split Artist".to_string(),
        musicbrainz_artist_id: None,
    };
    let mut split_first = first.clone();
    split_first.album_id = Some(first_album_id.clone());
    split_first.album = "First Split Album".to_string();
    split_first.artist = "First Split Artist".to_string();
    let first_artist = ArtistCredit {
        id: first_artist_id.clone(),
        name: split_first.artist.clone(),
        musicbrainz_artist_id: None,
    };
    split_first.relations.artists = vec![first_artist.clone(), common_artist.clone()];
    split_first.relations.album_artists = vec![first_artist, common_artist.clone()];
    let mut split_second = second.clone();
    split_second.album_id = Some(second_album_id.clone());
    split_second.album = "Second Split Album".to_string();
    split_second.artist = "Second Split Artist".to_string();
    let second_artist = ArtistCredit {
        id: second_artist_id.clone(),
        name: split_second.artist.clone(),
        musicbrainz_artist_id: None,
    };
    split_second.relations.artists = vec![second_artist.clone(), common_artist.clone()];
    split_second.relations.album_artists = vec![second_artist, common_artist.clone()];
    let common_artist_row = Artist {
        id: common_artist.id,
        name: common_artist.name,
        favorite: false,
        last_played: None,
        play_count: None,
        user_rating: None,
        musicbrainz_artist_id: None,
        image_ref: None,
        local_artwork: None,
    };
    accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 2,
            albums: vec![
                album_for_track(&split_first, 0),
                album_for_track(&split_second, 0),
            ],
            tracks: vec![split_first.clone(), split_second.clone()],
            artists: vec![
                artist_for_track(&split_first),
                artist_for_track(&split_second),
                common_artist_row,
            ],
            removed_album_ids: vec![old_album_id.clone()],
            removed_artist_ids: vec![old_artist_id.clone()],
            ..LocalComponentReplacement::default()
        })
        .expect("accept aggregate split");
    for album_id in [&first_album_id, &second_album_id] {
        assert!(
            !accepted
                .library
                .album(album_id)
                .expect("read split Album")
                .expect("split Album")
                .favorite
        );
    }
    for artist_id in [&first_artist_id, &second_artist_id] {
        assert!(
            !accepted
                .library
                .artist(artist_id)
                .expect("read split Artist")
                .expect("split Artist")
                .favorite
        );
    }
    assert!(
        !accepted
            .library
            .artist(&common_artist_id)
            .expect("read common split Artist")
            .expect("common split Artist")
            .favorite
    );

    for item in [
        FavoriteItemId::Album(first_album_id.clone()),
        FavoriteItemId::Album(second_album_id.clone()),
        FavoriteItemId::Artist(first_artist_id.clone()),
        FavoriteItemId::Artist(second_artist_id.clone()),
    ] {
        accepted
            .library
            .accept_favorite(FavoriteAcceptance::RufinOwned {
                item,
                favorite: true,
            })
            .expect("favorite split aggregate");
    }
    let merged_album_id = library::AlbumId::new("local:album:merged");
    let merged_artist_id = library::ArtistId::new("local:artist:merged");
    let merged_artist = ArtistCredit {
        id: merged_artist_id.clone(),
        name: "Merged Artist".to_string(),
        musicbrainz_artist_id: None,
    };
    let mut merged_first = split_first;
    merged_first.album_id = Some(merged_album_id.clone());
    merged_first.album = "Merged Album".to_string();
    merged_first.artist = "Merged Artist".to_string();
    merged_first.relations.artists = vec![merged_artist.clone()];
    merged_first.relations.album_artists = vec![merged_artist.clone()];
    let mut merged_second = split_second;
    merged_second.album_id = Some(merged_album_id.clone());
    merged_second.album = "Merged Album".to_string();
    merged_second.artist = "Merged Artist".to_string();
    merged_second.relations.artists = vec![merged_artist.clone()];
    merged_second.relations.album_artists = vec![merged_artist];
    let merged_artist_row = artist_for_track(&merged_first);
    accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 3,
            albums: vec![album_for_track(&merged_first, 0)],
            tracks: vec![merged_first, merged_second],
            artists: vec![merged_artist_row],
            removed_album_ids: vec![first_album_id.clone(), second_album_id.clone()],
            removed_artist_ids: vec![
                first_artist_id.clone(),
                second_artist_id.clone(),
                common_artist_id,
            ],
            ..LocalComponentReplacement::default()
        })
        .expect("accept aggregate merge");
    assert!(
        accepted
            .library
            .album(&merged_album_id)
            .expect("read merged Album")
            .expect("merged Album")
            .favorite
    );
    assert!(
        accepted
            .library
            .artist(&merged_artist_id)
            .expect("read merged Artist")
            .expect("merged Artist")
            .favorite
    );

    drop(accepted);
    drop(library);
    let connection = rusqlite::Connection::open(path).expect("inspect dormant Local favorites");
    for (kind, id) in [
        ("album", old_album_id.as_str()),
        ("artist", old_artist_id.as_str()),
    ] {
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*)
                     FROM local_favorites
                     WHERE source_id = ?1 AND item_kind = ?2 AND item_id = ?3",
                    rusqlite::params![source_id.as_str(), kind, id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("read dormant split favorite"),
            1
        );
    }
    for (kind, id) in [
        ("album", first_album_id.as_str()),
        ("album", second_album_id.as_str()),
        ("artist", first_artist_id.as_str()),
        ("artist", second_artist_id.as_str()),
    ] {
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*)
                     FROM local_favorites
                     WHERE source_id = ?1 AND item_kind = ?2 AND item_id = ?3",
                    rusqlite::params![source_id.as_str(), kind, id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("read transferred Local favorite"),
            0
        );
    }
    for (kind, id) in [
        ("album", merged_album_id.as_str()),
        ("artist", merged_artist_id.as_str()),
    ] {
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*)
                     FROM local_favorites
                     WHERE source_id = ?1 AND item_kind = ?2 AND item_id = ?3",
                    rusqlite::params![source_id.as_str(), kind, id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("read merged Local favorite"),
            1
        );
    }
}

#[test]
fn sparse_local_favorites_stay_dormant_without_becoming_source_rows() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:sparse-favorites");
    let library = Libraries::open(&path).expect("open Library");
    let track = track();
    let album_id = track.album_id.clone().expect("Album ID");
    let artist_id = track.relations.artists[0].id.clone();
    let accepted = accept_local_tracks(
        &library,
        source_id.clone(),
        vec![track.clone()],
        Vec::new(),
        43,
    );
    accepted
        .library
        .accept_favorite(FavoriteAcceptance::RufinOwned {
            item: FavoriteItemId::Album(album_id.clone()),
            favorite: true,
        })
        .expect("favorite sparse Album");
    let artist_favorite = accepted
        .library
        .accept_favorite(FavoriteAcceptance::RufinOwned {
            item: FavoriteItemId::Artist(artist_id.clone()),
            favorite: true,
        })
        .expect("favorite sparse Artist");
    assert!(artist_favorite.albums.is_empty());
    assert!(artist_favorite.artists.is_empty());
    assert_eq!(
        artist_favorite.favorite,
        Some(library::FavoriteAcknowledgement {
            item: FavoriteItemId::Artist(artist_id.clone()),
            favorite: true,
        })
    );

    drop(accepted);
    let loaded = library
        .load_source(&source_id)
        .expect("reload source")
        .expect("source");
    loaded
        .accept_local_component(LocalComponentReplacement {
            observed_at: 2,
            removed_track_ids: vec![track.id.clone()],
            ..LocalComponentReplacement::default()
        })
        .expect("remove final relation");
    assert!(
        loaded
            .album(&album_id)
            .expect("read removed Album")
            .is_none()
    );
    assert!(
        loaded
            .artist(&artist_id)
            .expect("read removed Artist")
            .is_none()
    );

    loaded
        .accept_local_component(LocalComponentReplacement {
            observed_at: 3,
            tracks: vec![track.clone()],
            ..LocalComponentReplacement::default()
        })
        .expect("restore relation");
    assert!(
        loaded
            .album(&album_id)
            .expect("read restored Album")
            .expect("restored Album")
            .favorite
    );
    assert!(
        loaded
            .artist(&artist_id)
            .expect("read restored Artist")
            .expect("restored Artist")
            .favorite
    );

    drop(loaded);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load source")
        .expect("source");
    assert!(
        reopened
            .album(&album_id)
            .expect("read reopened Album")
            .expect("reopened Album")
            .favorite
    );
    assert!(
        reopened
            .artist(&artist_id)
            .expect("read reopened Artist")
            .expect("reopened Artist")
            .favorite
    );
}

#[test]
fn local_cue_component_and_baseline_follow_backing_file_transitions() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:cue-component");
    let library = Libraries::open(&path).expect("open Library");
    let raw = track();
    let audio_path = raw.source_path.clone().expect("audio path");
    let cue_path = "/music/Artist/Album/Track.cue";
    let mut unrelated = raw.clone();
    unrelated.id = library::TrackId::new("local:track:unrelated");
    unrelated.title = "Unrelated".to_string();
    unrelated.album_id = Some(library::AlbumId::new("local:album:unrelated"));
    unrelated.album = "Unrelated Album".to_string();
    unrelated.source_path = Some("/music/Other/Unrelated.flac".to_string());
    let accepted = accept_local_tracks(
        &library,
        source_id.clone(),
        vec![raw.clone(), unrelated.clone()],
        vec![
            local_audio_file(&audio_path, "Artist/Album/Track.flac"),
            local_audio_file(
                unrelated.source_path.as_deref().expect("unrelated path"),
                "Other/Unrelated.flac",
            ),
        ],
        44,
    );
    let playlist_id = created_playlist_id(
        accepted
            .library
            .accept_playlist(PlaylistAcceptance::RufinOwned(PlaylistEdit::Create {
                name: "Raw occurrence".to_string(),
                track_ids: vec![raw.id.clone()],
            }))
            .expect("save raw playlist occurrence"),
    );
    let raw_playlist = accepted
        .library
        .playlist_detail(&playlist_id)
        .expect("read raw Playlist")
        .expect("raw Playlist");
    let raw_occurrence = playlist_entry(&raw_playlist.entries, 0).occurrence_id;

    let mut first = raw.clone();
    first.id = library::TrackId::new("local:cue:one");
    first.title = "Part One".to_string();
    first.duration_seconds = 90;
    first.cue = Some(CueSegment {
        cue_path: cue_path.to_string(),
        start_millis: 0,
        end_millis: 90_000,
    });
    let mut second = first.clone();
    second.id = library::TrackId::new("local:cue:two");
    second.title = "Part Two".to_string();
    second.track_number = 2;
    second.cue = Some(CueSegment {
        cue_path: cue_path.to_string(),
        start_millis: 90_000,
        end_millis: 180_000,
    });
    accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 2,
            files: vec![
                local_audio_file(&audio_path, "Artist/Album/Track.flac"),
                local_cue_file(cue_path, vec![audio_path.clone()], LocalFileState::Accepted),
            ],
            tracks: vec![first.clone(), second.clone()],
            removed_track_ids: vec![raw.id.clone()],
            ..LocalComponentReplacement::default()
        })
        .expect("accept valid CUE");
    assert!(
        accepted
            .library
            .playlist_detail(&playlist_id)
            .expect("read dormant playlist")
            .expect("dormant playlist")
            .entries
            .is_empty()
    );
    assert!(
        accepted
            .library
            .playable_file(&first.id)
            .expect("read first CUE playable file")
            .is_some()
    );
    let baseline = accepted
        .library
        .local_component_baseline(&[LocalComponentSeed::Path(audio_path.clone())])
        .expect("read CUE component baseline");
    assert_eq!(
        baseline
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        [cue_path, audio_path.as_str()]
    );
    assert_eq!(
        baseline
            .tracks
            .iter()
            .map(|track| track.id.as_str())
            .collect::<Vec<_>>(),
        [first.id.as_str(), second.id.as_str()]
    );

    let missing_audio_path = "/music/Artist/Album/Missing.flac";
    let missing_cue_path = "/music/Artist/Album/Missing.cue";
    accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 3,
            files: vec![local_cue_file(
                missing_cue_path,
                vec![missing_audio_path.to_string()],
                LocalFileState::Rejected,
            )],
            ..LocalComponentReplacement::default()
        })
        .expect("accept CUE with a missing backing file");
    let missing_baseline = accepted
        .library
        .local_component_baseline(&[LocalComponentSeed::Path(missing_audio_path.to_string())])
        .expect("read missing CUE dependency baseline");
    assert_eq!(
        missing_baseline
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        [missing_cue_path]
    );

    let mut unreadable = local_audio_file(&audio_path, "Artist/Album/Track.flac");
    unreadable.state = LocalFileState::Unreadable;
    accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 4,
            files: vec![unreadable],
            ..LocalComponentReplacement::default()
        })
        .expect("accept unreadable CUE backing");
    assert!(
        accepted
            .library
            .playable_file(&first.id)
            .expect("read unavailable CUE file")
            .is_none()
    );
    assert!(
        accepted
            .library
            .playable_file(&second.id)
            .expect("read unavailable second CUE file")
            .is_none()
    );

    accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 5,
            files: vec![
                local_audio_file(&audio_path, "Artist/Album/Track.flac"),
                local_cue_file(cue_path, vec![audio_path.clone()], LocalFileState::Rejected),
            ],
            tracks: vec![raw.clone()],
            removed_track_ids: vec![first.id.clone(), second.id.clone()],
            ..LocalComponentReplacement::default()
        })
        .expect("restore raw Track for invalid CUE");
    let playlist = accepted
        .library
        .playlist_detail(&playlist_id)
        .expect("read restored playlist")
        .expect("restored playlist");
    assert_eq!(playlist.entries.len(), 1);
    let entry = playlist_entry(&playlist.entries, 0);
    assert_eq!(entry.occurrence_id, raw_occurrence);
    assert_eq!(entry.track.id, raw.id);
    assert!(
        accepted
            .library
            .track(&first.id)
            .expect("read removed first segment")
            .is_none()
    );

    drop(accepted);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load source")
        .expect("source");
    assert!(
        reopened
            .playable_file(&raw.id)
            .expect("read reopened raw file")
            .is_some()
    );
    let reopened_playlist = reopened
        .playlist_detail(&playlist_id)
        .expect("read reopened playlist")
        .expect("reopened playlist");
    assert_eq!(
        playlist_entry(&reopened_playlist.entries, 0).occurrence_id,
        raw_occurrence
    );
}

#[test]
fn local_folders_follow_directory_facts_track_moves_and_reopen() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:folders");
    let library = Libraries::open(&path).expect("open Library");

    let mut first = track();
    first.id = library::TrackId::new("local:cue:first");
    first.title = "First section".to_string();
    first.source_path = Some("/music/Artist/Album/Disc.flac".to_string());
    first.cue = Some(CueSegment {
        cue_path: "/music/Artist/Album/Disc.cue".to_string(),
        start_millis: 0,
        end_millis: 90_000,
    });
    let mut second = first.clone();
    second.id = library::TrackId::new("local:cue:second");
    second.title = "Second section".to_string();
    second.track_number = 2;
    second.cue = Some(CueSegment {
        cue_path: "/music/Artist/Album/Disc.cue".to_string(),
        start_millis: 90_000,
        end_millis: 180_000,
    });
    let mut loose = track();
    loose.id = library::TrackId::new("local:track:loose");
    loose.title = "Loose".to_string();
    loose.track_number = 3;
    loose.source_path = Some("/music/Loose.flac".to_string());
    loose.cue = None;

    let accepted = accept_local_tracks(
        &library,
        source_id.clone(),
        vec![second.clone(), loose.clone(), first.clone()],
        vec![
            local_directory_file("/music", "/music"),
            local_directory_file("/music/Artist", "/music"),
            local_directory_file("/music/Artist/Album", "/music"),
            local_directory_file("/other", "/other"),
            local_directory_file("/other/Empty", "/other"),
            local_audio_file("/music/Artist/Album/Disc.flac", "Artist/Album/Disc.flac"),
            local_audio_file("/music/Loose.flac", "Loose.flac"),
            local_cue_file(
                "/music/Artist/Album/Disc.cue",
                vec!["/music/Artist/Album/Disc.flac".to_string()],
                LocalFileState::Accepted,
            ),
        ],
        47,
    );

    let root = accepted
        .library
        .local_folder_contents(None)
        .expect("read Local folder root")
        .expect("Local folder root");
    assert_eq!(
        root.tracks
            .iter()
            .map(|track| track.id.as_str())
            .collect::<Vec<_>>(),
        [loose.id.as_str()]
    );
    assert_eq!(
        root.folders
            .iter()
            .map(|folder| folder.name.as_str())
            .collect::<Vec<_>>(),
        ["music", "other"]
    );

    let music_id = test_local_folder_id("/music");
    let album_id = test_local_folder_id("/music/Artist/Album");
    let other_id = test_local_folder_id("/other");
    let empty_id = test_local_folder_id("/other/Empty");
    let music = accepted
        .library
        .local_folder_contents(Some(&music_id))
        .expect("read music folder")
        .expect("music folder");
    assert_eq!(
        music
            .folders
            .iter()
            .map(|folder| folder.name.as_str())
            .collect::<Vec<_>>(),
        ["Artist"]
    );
    assert_eq!(
        music
            .tracks
            .iter()
            .map(|track| track.id.as_str())
            .collect::<Vec<_>>(),
        [loose.id.as_str()]
    );
    let album = accepted
        .library
        .local_folder_contents(Some(&album_id))
        .expect("read album folder")
        .expect("album folder");
    assert_eq!(
        album
            .tracks
            .iter()
            .map(|track| track.id.as_str())
            .collect::<Vec<_>>(),
        [first.id.as_str(), second.id.as_str()]
    );
    assert!(
        accepted
            .library
            .local_folder_contents(Some(&empty_id))
            .expect("read empty folder")
            .expect("empty folder")
            .tracks
            .is_empty()
    );

    let initial_inventory = accepted
        .library
        .local_component_baseline(&[
            LocalComponentSeed::DirectoryTree("/music".to_string()),
            LocalComponentSeed::DirectoryTree("/other".to_string()),
        ])
        .expect("read Local inventory");
    assert_eq!(
        initial_inventory
            .files
            .iter()
            .find(|file| file.path == "/music/Artist/Album/Disc.cue")
            .expect("CUE inventory row")
            .dependencies
            .as_ref(),
        ["/music/Artist/Album/Disc.flac"]
    );
    assert_eq!(
        initial_inventory
            .files
            .iter()
            .find(|file| file.path == "/other/Empty")
            .expect("empty folder inventory row")
            .kind,
        LocalFileKind::Directory
    );

    let mut moved = loose.clone();
    moved.source_path = Some("/other/Moved/Loose.flac".to_string());
    let moved_id = test_local_folder_id("/other/Moved");
    let moved_result = accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 2,
            files: vec![
                local_directory_file("/other/Moved", "/other"),
                local_audio_file_in_root("/other/Moved/Loose.flac", "/other", "Moved/Loose.flac"),
            ],
            removed_paths: vec!["/music/Loose.flac".to_string()],
            tracks: vec![moved.clone()],
            ..LocalComponentReplacement::default()
        })
        .expect("move Local Track")
        .expect("moving a Local Track must report a change");
    assert!(moved_result.local_folders_changed);
    assert!(
        accepted
            .library
            .local_folder_contents(Some(&music_id))
            .expect("read old parent")
            .expect("old parent")
            .tracks
            .is_empty()
    );
    assert_eq!(
        accepted
            .library
            .local_folder_contents(Some(&moved_id))
            .expect("read moved folder")
            .expect("moved folder")
            .tracks[0]
            .id,
        moved.id
    );

    let removed = accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 3,
            removed_paths: vec!["/other/Empty".to_string()],
            ..LocalComponentReplacement::default()
        })
        .expect("remove empty Local folder")
        .expect("removing a Local folder must report a change");
    assert!(removed.local_folders_changed);
    assert!(
        accepted
            .library
            .local_folder_contents(Some(&empty_id))
            .expect("read removed folder")
            .is_none()
    );
    let final_inventory = accepted
        .library
        .local_component_baseline(&[
            LocalComponentSeed::DirectoryTree("/music".to_string()),
            LocalComponentSeed::DirectoryTree("/other".to_string()),
        ])
        .expect("read final Local inventory");

    drop(accepted);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load source")
        .expect("source");
    assert_eq!(
        reopened
            .local_component_baseline(&[
                LocalComponentSeed::DirectoryTree("/music".to_string()),
                LocalComponentSeed::DirectoryTree("/other".to_string()),
            ])
            .expect("read reopened Local inventory"),
        final_inventory
    );
    let reopened_other = reopened
        .local_folder_contents(Some(&other_id))
        .expect("read reopened other folder")
        .expect("reopened other folder");
    assert_eq!(
        reopened_other
            .folders
            .iter()
            .map(|folder| folder.id.clone())
            .collect::<Vec<_>>(),
        [moved_id]
    );
    assert_eq!(
        reopened
            .local_folder_contents(Some(&album_id))
            .expect("read reopened album folder")
            .expect("reopened album folder")
            .tracks
            .iter()
            .map(|track| track.id.as_str())
            .collect::<Vec<_>>(),
        [first.id.as_str(), second.id.as_str()]
    );
}

#[test]
fn local_point_updates_keep_exact_totals_and_playlist_occurrences_across_reopen() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:exact-totals");
    let library = Libraries::open(&path).expect("open Library");
    let mood_id = MoodId::new("local:mood:focus");

    let mut long = track();
    long.id = library::TrackId::new("local:track:long");
    long.title = "Long".to_string();
    long.duration_seconds = u32::MAX;
    long.relations.moods = vec![MoodCredit {
        id: mood_id.clone(),
        name: "Focus".to_string(),
    }];

    let mut short = long.clone();
    short.id = library::TrackId::new("local:track:short");
    short.title = "Short".to_string();
    short.duration_seconds = 10;
    short.track_number = 2;
    short.source_path = Some("/music/Artist/Album/Short.flac".to_string());
    short.relations.genres = vec![GenreCredit {
        id: GenreId::new("local:genre:jazz"),
        name: "Jazz".to_string(),
    }];

    let accepted = accept_local_tracks(
        &library,
        source_id.clone(),
        vec![long.clone(), short.clone()],
        Vec::new(),
        45,
    );
    let playlist_id = created_playlist_id(
        accepted
            .library
            .accept_playlist(PlaylistAcceptance::RufinOwned(PlaylistEdit::Create {
                name: "Repeated short Track".to_string(),
                track_ids: vec![long.id.clone(), short.id.clone(), short.id.clone()],
            }))
            .expect("save repeated playlist"),
    );

    let mut retagged = short.clone();
    retagged.duration_seconds = 9;
    retagged.relations.genres = vec![GenreCredit {
        id: GenreId::new("local:genre:blues"),
        name: "Blues".to_string(),
    }];
    accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 2,
            tracks: vec![retagged.clone()],
            ..LocalComponentReplacement::default()
        })
        .expect("accept exact duration replacement");
    assert_eq!(
        accepted
            .library
            .album_detail(retagged.album_id.as_ref().expect("Album ID"), None)
            .expect("read saturated Album")
            .expect("saturated Album")
            .summary
            .duration_seconds,
        u32::MAX
    );
    assert_eq!(
        accepted
            .library
            .mood_detail(&mood_id, None)
            .expect("read saturated Mood")
            .expect("saturated Mood")
            .summary
            .duration_seconds,
        u32::MAX
    );
    let playlist = accepted
        .library
        .playlist_detail(&playlist_id)
        .expect("read saturated playlist")
        .expect("saturated playlist");
    assert_eq!(playlist.summary.track_count, 3);
    assert_eq!(playlist.summary.duration_seconds, u32::MAX);
    assert_eq!(
        playlist
            .summary
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>(),
        ["Blues", "Rock"]
    );

    accepted
        .library
        .accept_local_component(LocalComponentReplacement {
            observed_at: 3,
            removed_track_ids: vec![long.id.clone()],
            ..LocalComponentReplacement::default()
        })
        .expect("remove long Track");
    assert_eq!(
        accepted
            .library
            .album_detail(retagged.album_id.as_ref().expect("Album ID"), None)
            .expect("read exact Album")
            .expect("exact Album")
            .summary
            .duration_seconds,
        9
    );
    assert_eq!(
        accepted
            .library
            .mood_detail(&mood_id, None)
            .expect("read exact Mood")
            .expect("exact Mood")
            .summary
            .duration_seconds,
        9
    );
    let playlist = accepted
        .library
        .playlist_detail(&playlist_id)
        .expect("read exact playlist")
        .expect("exact playlist");
    assert_eq!(playlist.summary.track_count, 2);
    assert_eq!(playlist.summary.duration_seconds, 18);
    assert_eq!(
        playlist
            .summary
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>(),
        ["Blues"]
    );
    assert_eq!(playlist.entries.len(), 2);

    drop(accepted);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&source_id)
        .expect("load source")
        .expect("source");
    assert_eq!(
        reopened
            .album_detail(retagged.album_id.as_ref().expect("Album ID"), None)
            .expect("read reopened Album")
            .expect("reopened Album")
            .summary
            .duration_seconds,
        9
    );
    let playlist = reopened
        .playlist_detail(&playlist_id)
        .expect("read reopened playlist")
        .expect("reopened playlist");
    assert_eq!(playlist.summary.track_count, 2);
    assert_eq!(playlist.summary.duration_seconds, 18);
    assert_eq!(
        playlist
            .summary
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>(),
        ["Blues"]
    );
    assert_eq!(playlist.entries.len(), 2);
}

#[test]
fn accepted_activity_replaces_only_the_next_local_home_and_reopens() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("local:server:activity");
    let library = Libraries::open(&path).expect("open Library");
    let accepted = accept(&library, source_id.clone(), digest(20), "Track", None, None);
    let mounted = accepted.library.home(None).expect("mounted Home");
    assert!(mounted.section(HomeSectionKind::MostPlayed).is_none());

    let play = AcceptedPlay {
        play_id: "play:one".to_string(),
        track_id: library::TrackId::new("local:track:one"),
        played_at: 1_700_000_000,
        month: "2023-11".to_string(),
    };
    let accepted_play = accepted
        .library
        .record_play(play.clone())
        .expect("record accepted play")
        .expect("new accepted play");
    let accepted_play = accepted
        .library
        .apply_recorded_activity(&accepted_play)
        .expect("apply accepted play")
        .expect("accepted play must change the library");
    assert_eq!(
        accepted_play.home,
        AcceptedHomeChange::Played(play.track_id.clone())
    );
    assert!(!accepted_play.download_coverage_changed);
    assert_eq!(accepted_play.tracks.len(), 1);
    assert_eq!(
        accepted_play.tracks[0]
            .track
            .as_ref()
            .expect("accepted activity Track")
            .play_count,
        Some(1)
    );
    let next = accepted
        .library
        .home_after_accepted_change(None, &mounted, &accepted_play.home)
        .expect("prepare Home after accepted play")
        .expect("accepted play changes the next Home");
    assert!(!Arc::ptr_eq(&mounted, &next));
    assert!(mounted.section(HomeSectionKind::MostPlayed).is_none());
    assert!(Arc::ptr_eq(
        mounted
            .section(HomeSectionKind::Explore)
            .expect("mounted Explore"),
        next.section(HomeSectionKind::Explore)
            .expect("next Explore")
    ));
    assert_eq!(
        next.section(HomeSectionKind::MostPlayed)
            .expect("Most played")
            .items
            .len(),
        1
    );
    assert_eq!(
        accepted
            .library
            .track(&play.track_id)
            .expect("read effective Track")
            .expect("effective Track")
            .play_count,
        Some(1)
    );

    let duplicate = accepted
        .library
        .record_play(play)
        .expect("ignore duplicate play");
    assert!(duplicate.is_none());
    let after_duplicate = Arc::clone(&next);
    assert_eq!(
        home_track_title(&next, HomeSectionKind::MostPlayed),
        home_track_title(&after_duplicate, HomeSectionKind::MostPlayed)
    );

    let second_play = AcceptedPlay {
        play_id: "play:two".to_string(),
        track_id: library::TrackId::new("local:track:one"),
        played_at: 1_700_000_001,
        month: "2023-11".to_string(),
    };
    let second_update = accepted
        .library
        .record_play(second_play)
        .expect("record second accepted play")
        .expect("second play occurrence");
    let second_change = accepted
        .library
        .apply_recorded_activity(&second_update)
        .expect("apply second accepted play")
        .expect("second accepted play must change activity");
    assert!(second_change.history_changed);
    let history = accepted
        .library
        .history_track_list(None)
        .expect("read bounded History");
    assert_eq!(history.len(), 2);
    assert_eq!(history.played_at(0), Some(1_700_000_001));
    assert_eq!(history.played_at(1), Some(1_700_000_000));
    assert_eq!(
        history.track_ids().expect("History Track IDs").as_ref(),
        [
            library::TrackId::new("local:track:one"),
            library::TrackId::new("local:track:one"),
        ]
    );

    drop(accepted);
    drop(library);
    let reopened_library = Libraries::open(&path).expect("reopen Library");
    let reopened = reopened_library
        .load_source(&source_id)
        .expect("load source")
        .expect("accepted source");
    assert_eq!(
        reopened
            .track(&library::TrackId::new("local:track:one"))
            .expect("read reopened Track")
            .expect("reopened Track")
            .play_count,
        Some(2)
    );
    assert_eq!(
        reopened
            .history_track_list(None)
            .expect("reopened History")
            .len(),
        2
    );
    let home = reopened.home(None).expect("reopened Home");
    assert!(home.section(HomeSectionKind::MostPlayed).is_some());
    assert!(home.section(HomeSectionKind::RecentlyPlayed).is_some());
    assert!(home.section(HomeSectionKind::NewlyAdded).is_some());
}

#[test]
fn source_item_replacement_survives_removal_and_reattaches_dormant_consumers() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("jellyfin:server:exact");
    let library = Libraries::open(&path).expect("open Library");
    let mut original = track();
    let old_mood_id = MoodId::new("mood:quiet");
    let old_folder_id = MusicFolderId::new("folder:one");
    let new_folder_id = MusicFolderId::new("folder:two");
    original.relations.moods.push(MoodCredit {
        id: old_mood_id.clone(),
        name: "Quiet".to_string(),
    });
    original.relations.music_folders.push(old_folder_id.clone());

    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: digest(40),
        })
        .expect("begin candidate");
    for batch in digest_fixture(&original, 99, false) {
        candidate.write(batch).expect("write candidate");
    }
    candidate
        .write(CandidateBatch::MusicFolders(vec![MusicFolder {
            id: new_folder_id.clone(),
            name: "Other music".to_string(),
            image_ref: Some(ImageRef::new(
                "jellyfin:music-folder:other",
                Some("other-cover".to_string()),
            )),
        }]))
        .expect("write second music folder");
    candidate
        .write(CandidateBatch::LocalFiles(vec![
            local_audio_file("/music/Artist/Album/Track.flac", "Track.flac"),
            local_audio_file("/music/New Artist/New Album/Changed.flac", "Changed.flac"),
        ]))
        .expect("write Local files");
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::Source {
                    sections: vec![SourceHomeSection {
                        kind: SourceHomeSectionKind::RecentlyPlayed,
                        items: vec![HomeItemId::Track(original.id.clone())],
                    }],
                },
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept candidate");
    let loaded = Arc::clone(&accepted.library);
    let playlist_id = PlaylistId::new("playlist:digest");
    let smart_playlist_id = created_smart_playlist_id(
        loaded
            .create_smart_playlist(
                "Changed tracks".to_string(),
                SmartPlaylistDefinition {
                    match_all: vec![SmartPlaylistRule {
                        field: SmartPlaylistRuleField::Title,
                        operator: SmartPlaylistRuleOperator::Equals,
                        value: Some(SmartPlaylistRuleValue::Text("Changed".to_string())),
                    }],
                    match_any: Vec::new(),
                    sort_field: SmartPlaylistSortField::Title,
                    descending: false,
                    limit: None,
                },
            )
            .expect("create smart playlist"),
    );
    assert!(
        loaded
            .smart_playlist_detail(&smart_playlist_id, None)
            .expect("read smart playlist")
            .expect("smart playlist")
            .tracks
            .is_empty()
    );
    let mounted_home = loaded.home(None).expect("mounted Home");
    assert_eq!(
        home_track_title(&mounted_home, HomeSectionKind::RecentlyPlayed),
        Some("Track")
    );

    let new_album_id = library::AlbumId::new("local:album:two");
    let new_artist_id = library::ArtistId::new("local:artist:two");
    let new_genre_id = GenreId::new("local:genre:jazz");
    let new_mood_id = MoodId::new("mood:bright");
    let mut changed = original.clone();
    changed.album_id = Some(new_album_id.clone());
    changed.title = "Changed".to_string();
    changed.artist = "New Artist".to_string();
    changed.album = "New Album".to_string();
    changed.duration_seconds = 240;
    changed.source_path = Some("/music/New Artist/New Album/Changed.flac".to_string());
    let new_artist = ArtistCredit {
        id: new_artist_id.clone(),
        name: "New Artist".to_string(),
        musicbrainz_artist_id: None,
    };
    changed.relations = TrackRelations {
        artists: vec![new_artist.clone()],
        album_artists: vec![new_artist],
        genres: vec![GenreCredit {
            id: new_genre_id.clone(),
            name: "Jazz".to_string(),
        }],
        moods: vec![MoodCredit {
            id: new_mood_id.clone(),
            name: "Bright".to_string(),
        }],
        music_folders: vec![new_folder_id.clone()],
    };

    let replacement = loaded
        .accept_source_update(SourceLibraryUpdate {
            tracks: vec![changed.clone()],
            ..SourceLibraryUpdate::default()
        })
        .expect("replace Track")
        .expect("replacement Track must change the library");
    assert_eq!(replacement.home, AcceptedHomeChange::Rebuild);
    assert!(replacement.download_coverage_changed);
    assert_eq!(replacement.tracks.len(), 1);
    assert_eq!(
        replacement.tracks[0]
            .track
            .as_ref()
            .expect("replacement Track")
            .title,
        "Changed"
    );
    assert!(replacement.playlists.contains(&playlist_id));
    assert!(replacement.smart_playlists.contains(&smart_playlist_id));
    assert!(
        replacement
            .albums
            .contains(original.album_id.as_ref().expect("old Album ID"))
    );
    assert!(replacement.albums.contains(&new_album_id));
    assert!(
        replacement
            .artists
            .contains(&original.relations.artists[0].id)
    );
    assert!(replacement.artists.contains(&new_artist_id));
    assert!(
        replacement
            .genres
            .contains(&original.relations.genres[0].id)
    );
    assert!(replacement.genres.contains(&new_genre_id));
    assert!(replacement.moods.contains(&old_mood_id));
    assert!(replacement.moods.contains(&new_mood_id));

    let current = loaded
        .track(&changed.id)
        .expect("read changed Track")
        .expect("changed Track");
    assert_eq!(current.title, "Changed");
    assert_eq!(current.duration_seconds, 240);
    let old_album = loaded
        .album_detail(original.album_id.as_ref().expect("old Album ID"), None)
        .expect("read old Album")
        .expect("old explicit Album");
    assert_eq!(old_album.summary.track_count, 0);
    assert_eq!(old_album.summary.duration_seconds, 0);
    let new_album = loaded
        .album_detail(&new_album_id, None)
        .expect("read new Album")
        .expect("new sparse Album");
    assert_eq!(new_album.summary.track_count, 1);
    assert_eq!(new_album.summary.duration_seconds, 240);
    assert_eq!(
        new_album
            .tracks
            .track(0)
            .expect("read new Album Track")
            .expect("new Album Track")
            .title,
        "Changed"
    );
    assert_eq!(
        loaded
            .artist_overview(&new_artist_id, None)
            .expect("read new Artist")
            .expect("new Artist")
            .summary
            .track_count,
        1
    );
    let new_genre = loaded
        .genre_detail(&new_genre_id, None)
        .expect("read new Genre")
        .expect("new Genre");
    assert_eq!(new_genre.summary.track_count, 1);
    assert_eq!(new_genre.summary.duration_seconds, 240);
    assert_eq!(
        loaded
            .mood_detail(&new_mood_id, None)
            .expect("read new Mood")
            .expect("new Mood")
            .tracks
            .len(),
        1
    );
    assert!(
        loaded
            .mood_detail(&old_mood_id, None)
            .expect("read old Mood")
            .is_none()
    );
    assert!(
        loaded
            .track_list(Some(&old_folder_id), TrackSort::Title, false)
            .expect("read old music folder Tracks")
            .is_empty()
    );
    assert_eq!(
        loaded
            .track_list(Some(&new_folder_id), TrackSort::Title, false)
            .expect("read new music folder Tracks")
            .len(),
        1
    );
    let folders = loaded.music_folders().expect("read music folders");
    let new_folder = folders
        .iter()
        .find(|folder| folder.id == new_folder_id)
        .expect("new music folder");
    assert_eq!(
        new_folder
            .image_ref
            .as_ref()
            .map(|image| (image.item_id.as_str(), image.tag.as_deref())),
        Some(("jellyfin:music-folder:other", Some("other-cover")))
    );
    let scoped_albums = loaded
        .albums(Some(&new_folder_id))
        .expect("read scoped Albums");
    assert_eq!(scoped_albums.len(), 1);
    assert_eq!(scoped_albums[0].track_count, 1);
    assert_eq!(scoped_albums[0].duration_seconds, 240);
    assert_eq!(
        scoped_albums[0]
            .artwork
            .representative_track
            .as_ref()
            .expect("Album representative Track")
            .id,
        changed.id
    );
    let scoped_artists = loaded
        .artists(Some(&new_folder_id))
        .expect("read scoped Artists");
    assert_eq!(scoped_artists.len(), 1);
    assert_eq!(scoped_artists[0].album_count, 1);
    assert_eq!(scoped_artists[0].track_count, 1);
    assert_eq!(scoped_artists[0].duration_seconds, 240);
    assert_eq!(
        scoped_artists[0].artwork.representative_albums[0].album.id,
        new_album_id
    );
    let scoped_genres = loaded
        .genres(Some(&new_folder_id))
        .expect("read scoped Genres");
    assert_eq!(scoped_genres.len(), 1);
    assert_eq!(scoped_genres[0].album_count, 1);
    assert_eq!(scoped_genres[0].track_count, 1);
    assert_eq!(scoped_genres[0].duration_seconds, 240);
    let scoped_moods = loaded
        .moods(Some(&new_folder_id))
        .expect("read scoped Moods");
    assert_eq!(scoped_moods.len(), 1);
    assert_eq!(scoped_moods[0].track_count, 1);
    assert_eq!(scoped_moods[0].duration_seconds, 240);
    assert!(
        loaded
            .artists(Some(&old_folder_id))
            .expect("read empty old-folder Artists")
            .is_empty()
    );
    assert!(
        loaded
            .home(Some(&old_folder_id))
            .expect("read empty old-folder Home")
            .section(HomeSectionKind::RecentlyPlayed)
            .is_none()
    );
    assert_eq!(
        home_track_title(
            &loaded
                .home(Some(&new_folder_id))
                .expect("read new-folder Home"),
            HomeSectionKind::RecentlyPlayed,
        ),
        Some("Changed")
    );
    let playlist = loaded
        .playlist_detail(&playlist_id)
        .expect("read playlist")
        .expect("playlist");
    assert_eq!(playlist.summary.track_count, 1);
    assert_eq!(playlist.summary.duration_seconds, 240);
    assert_eq!(
        playlist
            .summary
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>(),
        ["Jazz"]
    );
    assert_eq!(playlist_entry(&playlist.entries, 0).track.title, "Changed");
    let smart = loaded
        .smart_playlist_detail(&smart_playlist_id, Some(&new_folder_id))
        .expect("read smart playlist")
        .expect("smart playlist");
    assert_eq!(smart.summary.track_count, 1);
    assert_eq!(smart.summary.duration_seconds, 240);
    assert_eq!(
        smart
            .tracks
            .track(0)
            .expect("read smart playlist Track")
            .expect("smart playlist Track")
            .title,
        "Changed"
    );
    assert!(
        loaded
            .smart_playlist_detail(&smart_playlist_id, Some(&old_folder_id))
            .expect("read old-folder smart playlist")
            .expect("old-folder smart playlist")
            .tracks
            .is_empty()
    );
    assert_eq!(
        loaded
            .playable_file(&changed.id)
            .expect("read playable file")
            .expect("playable file")
            .path(),
        std::path::Path::new("/music/New Artist/New Album/Changed.flac")
    );
    let next_home = loaded
        .home_after_accepted_change(None, &mounted_home, &replacement.home)
        .expect("prepare next Home")
        .expect("source item replacement rebuilds Home");
    assert!(!Arc::ptr_eq(&mounted_home, &next_home));
    assert_eq!(
        home_track_title(&mounted_home, HomeSectionKind::RecentlyPlayed),
        Some("Track")
    );
    assert_eq!(
        home_track_title(&next_home, HomeSectionKind::RecentlyPlayed),
        Some("Changed")
    );

    let previous_playlist_track = playlist_entry(&playlist.entries, 0).track;
    let mut same_duration = changed.clone();
    same_duration.comment = Some("updated source comment".to_string());
    let same_duration_replacement = loaded
        .accept_source_update(SourceLibraryUpdate {
            tracks: vec![same_duration.clone()],
            ..SourceLibraryUpdate::default()
        })
        .expect("replace same-duration Track")
        .expect("same-duration Track replacement must change the library");
    assert!(same_duration_replacement.playlists.contains(&playlist_id));
    let playlist = loaded
        .playlist_detail(&playlist_id)
        .expect("read same-duration playlist")
        .expect("same-duration playlist");
    assert_eq!(playlist.summary.duration_seconds, 240);
    let entry = playlist_entry(&playlist.entries, 0);
    assert!(!Track::ptr_eq(&previous_playlist_track, &entry.track));
    assert_eq!(
        entry.track.comment.as_deref(),
        Some("updated source comment")
    );
    changed = same_duration;

    let removed = loaded
        .accept_source_update(SourceLibraryUpdate {
            removed_tracks: vec![changed.id.clone()],
            ..SourceLibraryUpdate::default()
        })
        .expect("remove Track")
        .expect("Track removal must change the library");
    assert!(removed.tracks[0].track.is_none());
    assert!(
        loaded
            .track(&changed.id)
            .expect("read removed Track")
            .is_none()
    );
    assert!(
        loaded
            .album_detail(&new_album_id, None)
            .expect("read removed sparse Album")
            .is_none()
    );
    assert!(
        loaded
            .artist_overview(&new_artist_id, None)
            .expect("read removed sparse Artist")
            .is_none()
    );
    assert!(
        loaded
            .genre_detail(&new_genre_id, None)
            .expect("read removed sparse Genre")
            .is_none()
    );
    assert!(
        loaded
            .mood_detail(&new_mood_id, None)
            .expect("read removed Mood")
            .is_none()
    );
    let dormant = loaded
        .playlist_detail(&playlist_id)
        .expect("read dormant playlist")
        .expect("dormant playlist");
    assert_eq!(dormant.summary.track_count, 0);
    assert_eq!(dormant.summary.duration_seconds, 0);
    assert!(dormant.summary.genres.is_empty());
    assert!(dormant.entries.is_empty());
    assert!(
        loaded
            .smart_playlist_detail(&smart_playlist_id, None)
            .expect("read empty smart playlist")
            .expect("empty smart playlist")
            .tracks
            .is_empty()
    );
    assert!(
        loaded
            .playable_file(&changed.id)
            .expect("read removed playable file")
            .is_none()
    );
    assert!(
        loaded
            .home(None)
            .expect("Home after removal")
            .section(HomeSectionKind::RecentlyPlayed)
            .is_none()
    );

    drop(accepted);
    drop(loaded);
    drop(library);
    let library = Libraries::open(&path).expect("reopen Library");
    let reopened = library
        .load_source(&source_id)
        .expect("load source")
        .expect("accepted source");
    assert!(
        reopened
            .track(&changed.id)
            .expect("read reopened Track")
            .is_none()
    );
    let dormant = reopened
        .playlist_detail(&playlist_id)
        .expect("read reopened playlist")
        .expect("reopened playlist");
    assert_eq!(dormant.summary.track_count, 0);
    assert!(dormant.entries.is_empty());

    reopened
        .accept_source_update(SourceLibraryUpdate {
            tracks: vec![changed.clone()],
            ..SourceLibraryUpdate::default()
        })
        .expect("re-add Track");
    let reattached = reopened
        .playlist_detail(&playlist_id)
        .expect("read reattached playlist")
        .expect("reattached playlist");
    assert_eq!(reattached.summary.track_count, 1);
    assert_eq!(reattached.summary.duration_seconds, 240);
    assert_eq!(reattached.entries.len(), 1);
    let entry = playlist_entry(&reattached.entries, 0);
    assert_eq!(entry.occurrence_id, "one");
    assert_eq!(entry.track.title, "Changed");
    assert_eq!(
        reopened
            .smart_playlist_detail(&smart_playlist_id, None)
            .expect("read reattached smart playlist")
            .expect("reattached smart playlist")
            .tracks
            .len(),
        1
    );
    assert_eq!(
        reopened
            .playable_file(&changed.id)
            .expect("read reattached playable file")
            .expect("reattached playable file")
            .path(),
        std::path::Path::new("/music/New Artist/New Album/Changed.flac")
    );
    assert_eq!(
        home_track_title(
            &reopened.home(None).expect("Home after re-add"),
            HomeSectionKind::RecentlyPlayed
        ),
        Some("Changed")
    );
}

#[test]
fn replacing_album_artwork_facts_replaces_unchanged_tracks() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let library = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_track = track();
    let accepted = accept_track(
        &library,
        SourceId::new("jellyfin:server:album-replacement"),
        digest(94),
        source_track.clone(),
        None,
        1,
    );
    let previous_artwork = accepted
        .library
        .track(&source_track.id)
        .expect("read Track")
        .expect("Track")
        .album_artwork_facts()
        .cloned()
        .expect("sparse Album artwork");

    let mut album = album_for_track(&source_track, 1);
    album.title = "Accepted Album".to_string();
    let replacement = accepted
        .library
        .accept_source_update(SourceLibraryUpdate {
            albums: vec![album],
            ..SourceLibraryUpdate::default()
        })
        .expect("replace Album")
        .expect("Album replacement must change the library");

    assert_eq!(replacement.tracks.len(), 1);
    assert_eq!(replacement.tracks[0].id, source_track.id);
    let current_track = replacement.tracks[0]
        .track
        .as_ref()
        .expect("published Track");
    assert_eq!(
        current_track
            .album_artwork_facts()
            .map(|album| album.title.as_str()),
        Some("Accepted Album")
    );
    assert_ne!(
        current_track
            .album_artwork_facts()
            .expect("accepted Album artwork"),
        &previous_artwork
    );
}

#[test]
fn source_update_commits_tracks_and_playlist_readback_as_one_reopenable_value() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("jellyfin:server:exact:user:listener");
    let library = Libraries::open(&path).expect("open Library");
    let original = track();
    let playlist_id = PlaylistId::new("jellyfin:playlist:one");
    let original_playlist = PlaylistSnapshot {
        playlist: Playlist {
            id: playlist_id.clone(),
            name: "Before".to_string(),
            image_ref: None,
        },
        entries: vec![PlaylistEntry {
            occurrence_id: "entry-original".to_string(),
            track_id: original.id.clone(),
        }],
    };
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: digest(60),
        })
        .expect("begin source candidate");
    candidate
        .write(CandidateBatch::Tracks(vec![original.clone()]))
        .expect("write original Track");
    candidate
        .write(CandidateBatch::Playlists(vec![original_playlist]))
        .expect("write original Playlist");
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept source candidate");

    let mut changed = original.clone();
    changed.title = "Changed".to_string();
    let mut added = original.clone();
    added.id = library::TrackId::new("jellyfin:track:added");
    added.title = "Added".to_string();
    added.track_number = 2;
    let replacement_playlist = PlaylistSnapshot {
        playlist: Playlist {
            id: playlist_id.clone(),
            name: "After".to_string(),
            image_ref: None,
        },
        entries: vec![
            PlaylistEntry {
                occurrence_id: "entry-original".to_string(),
                track_id: changed.id.clone(),
            },
            PlaylistEntry {
                occurrence_id: "entry-added".to_string(),
                track_id: added.id.clone(),
            },
        ],
    };
    let update = accepted
        .library
        .accept_source_update(SourceLibraryUpdate {
            tracks: vec![changed.clone(), added.clone()],
            playlists: vec![replacement_playlist],
            ..SourceLibraryUpdate::default()
        })
        .expect("accept Track and Playlist update")
        .expect("Track and Playlist update must change the library");
    assert_eq!(update.home, AcceptedHomeChange::Rebuild);
    assert!(update.download_coverage_changed);
    assert!(update.playlists.contains(&playlist_id));
    let playlist = accepted
        .library
        .playlist_detail(&playlist_id)
        .expect("read updated Playlist")
        .expect("updated Playlist");
    assert_eq!(playlist.summary.playlist.name, "After");
    assert_eq!(
        playlist_entries(&playlist.entries)
            .iter()
            .map(|entry| entry.track.title.as_str())
            .collect::<Vec<_>>(),
        ["Changed", "Added"]
    );

    drop(accepted);
    drop(library);
    let library = Libraries::open(&path).expect("reopen Library");
    let reopened = library
        .load_source(&source_id)
        .expect("load source")
        .expect("accepted source");
    let playlist = reopened
        .playlist_detail(&playlist_id)
        .expect("read reopened Playlist")
        .expect("reopened Playlist");
    assert_eq!(playlist.summary.playlist.name, "After");
    assert_eq!(playlist.entries.len(), 2);
    assert_eq!(
        reopened
            .track(&added.id)
            .expect("read added Track")
            .expect("added Track")
            .title,
        "Added"
    );

    reopened
        .accept_source_update(SourceLibraryUpdate {
            removed_tracks: vec![added.id.clone()],
            removed_playlists: vec![playlist_id.clone()],
            ..SourceLibraryUpdate::default()
        })
        .expect("remove Track and Playlist");
    assert!(
        reopened
            .track(&added.id)
            .expect("read removed Track")
            .is_none()
    );
    assert!(
        reopened
            .playlist_detail(&playlist_id)
            .expect("read removed Playlist")
            .is_none()
    );

    drop(reopened);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen after removal")
        .load_source(&source_id)
        .expect("load source")
        .expect("accepted source");
    assert!(
        reopened
            .track(&added.id)
            .expect("read reopened removed Track")
            .is_none()
    );
    assert!(
        reopened
            .playlist_detail(&playlist_id)
            .expect("read reopened removed Playlist")
            .is_none()
    );
}

#[test]
fn album_release_results_follow_exact_identity_across_replacement_and_reopen() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let source_id = SourceId::new("jellyfin:server:release");
    let library = Libraries::open(&path).expect("open Library");
    let mut track = track();
    let primary_artist_id = track.relations.album_artists[0].id.clone();
    let appearing_artist_id = library::ArtistId::new("local:artist:appearing");
    track.make_mut().relations.artists = vec![ArtistCredit {
        id: appearing_artist_id.clone(),
        name: "Appearing Artist".to_string(),
        musicbrainz_artist_id: None,
    }];
    let mut album = album_for_track(&track, 0);
    album.relations.artists.clear();
    album.musicbrainz_release_group_id = Some("release-group-one".to_string());
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: digest(50),
        })
        .expect("begin candidate");
    candidate
        .write(CandidateBatch::Albums(vec![album.clone()]))
        .expect("write Album");
    candidate
        .write(CandidateBatch::Tracks(vec![track]))
        .expect("write Track");
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept candidate");
    let first_lookup = accepted
        .library
        .take_album_release_lookups(10)
        .expect("take release lookup")
        .pop()
        .expect("release lookup");
    assert_eq!(
        accepted
            .library
            .take_album_release_lookups(10)
            .expect("repeat unaccepted release lookup"),
        std::slice::from_ref(&first_lookup)
    );
    let release = accepted
        .library
        .accept_album_release_result(
            first_lookup,
            library::AlbumReleaseResult::Found {
                release_types: vec!["album".to_string()],
            },
        )
        .expect("accept found release")
        .expect("release patch");
    assert!(
        accepted
            .library
            .take_album_release_lookups(10)
            .expect("read accepted release lookup queue")
            .is_empty()
    );
    let mut expected_artist_releases = vec![primary_artist_id, appearing_artist_id];
    expected_artist_releases.sort();
    assert_eq!(release.album_releases, [album.id.clone()]);
    assert_eq!(release.artist_releases, expected_artist_releases);
    assert!(release.artists.is_empty());
    assert!(release.tracks.is_empty());
    assert_eq!(
        accepted
            .library
            .album(&album.id)
            .expect("read enriched Album")
            .expect("enriched Album")
            .release_types,
        ["album"]
    );

    accepted
        .library
        .accept_source_update(SourceLibraryUpdate {
            albums: vec![album.clone()],
            ..SourceLibraryUpdate::default()
        })
        .expect("replace same Album identity");
    assert_eq!(
        accepted
            .library
            .album(&album.id)
            .expect("read replaced Album")
            .expect("replaced Album")
            .release_types,
        ["album"]
    );

    let mut changed_identity = album.clone();
    changed_identity.musicbrainz_release_group_id = Some("release-group-two".to_string());
    accepted
        .library
        .accept_source_update(SourceLibraryUpdate {
            albums: vec![changed_identity.clone()],
            ..SourceLibraryUpdate::default()
        })
        .expect("replace changed Album identity");
    assert!(
        accepted
            .library
            .album(&album.id)
            .expect("read changed Album")
            .expect("changed Album")
            .release_types
            .is_empty()
    );
    let changed_lookup = accepted
        .library
        .take_album_release_lookups(10)
        .expect("take changed release lookup")
        .pop()
        .expect("changed release lookup");
    assert_eq!(
        changed_lookup.identity,
        library::AlbumReleaseIdentity::ReleaseGroup("release-group-two".to_string())
    );
    assert!(
        accepted
            .library
            .accept_album_release_result(changed_lookup, library::AlbumReleaseResult::Missing,)
            .expect("accept missing release")
            .is_none()
    );
    accepted
        .library
        .accept_source_update(SourceLibraryUpdate {
            albums: vec![changed_identity],
            ..SourceLibraryUpdate::default()
        })
        .expect("replace known missing Album");
    assert!(
        accepted
            .library
            .take_album_release_lookups(10)
            .expect("read lookup queue")
            .is_empty()
    );

    drop(accepted);
    drop(library);
    let library = Libraries::open(path).expect("reopen Library");
    let reopened = library
        .load_source(&source_id)
        .expect("load source")
        .expect("accepted source");
    assert!(
        reopened
            .album(&album.id)
            .expect("read reopened Album")
            .expect("reopened Album")
            .release_types
            .is_empty()
    );
    assert!(
        reopened
            .take_album_release_lookups(10)
            .expect("read reopened lookup queue")
            .is_empty()
    );
}

#[test]
fn album_release_candidates_continue_after_the_first_bounded_store_page() {
    let libraries = Libraries::memory().expect("open Library");
    let source_id = SourceId::new("jellyfin:server:release-pages");
    let template_track = track();
    let albums = (0..501)
        .map(|index| {
            let mut album = album_for_track(&template_track, index);
            album.id = library::AlbumId::new(format!("jellyfin:album:{index:04}"));
            album.title = format!("Album {index:04}");
            album.musicbrainz_release_group_id = Some(format!("release-group-{index:04}"));
            album
        })
        .collect::<Vec<_>>();
    let mut candidate = libraries
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: digest(62),
        })
        .expect("begin release-page candidate");
    candidate
        .write(CandidateBatch::Albums(albums))
        .expect("write release-page Albums");
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept release-page candidate");

    let first = accepted
        .library
        .take_album_release_lookups(500)
        .expect("read first release page");
    assert_eq!(first.len(), 500);
    for candidate in first {
        accepted
            .library
            .accept_album_release_result(candidate, library::AlbumReleaseResult::Missing)
            .expect("accept missing release result");
    }
    let second = accepted
        .library
        .take_album_release_lookups(500)
        .expect("read second release page");

    assert_eq!(second.len(), 1);
}

#[test]
fn artist_routes_keep_relationship_roles_and_album_level_tracks() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let library = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("jellyfin:server:artist-roles");
    let mut track = track();
    let guest = ArtistCredit {
        id: library::ArtistId::new("artist:guest"),
        name: "Guest".to_string(),
        musicbrainz_artist_id: None,
    };
    let track_album_artist = ArtistCredit {
        id: library::ArtistId::new("artist:track-album"),
        name: "Track Album Artist".to_string(),
        musicbrainz_artist_id: None,
    };
    let album_only_artist = ArtistCredit {
        id: library::ArtistId::new("artist:album-only"),
        name: "Album Only Artist".to_string(),
        musicbrainz_artist_id: None,
    };
    track.relations.artists = vec![guest.clone()];
    track.relations.album_artists = vec![track_album_artist.clone()];
    let mut second_track = track.clone();
    second_track.id = library::TrackId::new("local:track:artist-roles-two");
    second_track.title = "Second".to_string();
    second_track.relations.artists.clear();
    let mut album = album_for_track(&track, 0);
    album.relations.album_artists.clear();
    album.relations.artists = vec![album_only_artist.clone()];

    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id,
            input_digest: digest(94),
        })
        .expect("begin artist-role candidate");
    candidate
        .write(CandidateBatch::Albums(vec![album.clone()]))
        .expect("write partial Album");
    candidate
        .write(CandidateBatch::Tracks(vec![
            track.clone(),
            second_track.clone(),
        ]))
        .expect("write Tracks");
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept artist-role candidate");

    assert_eq!(
        accepted
            .library
            .artists(None)
            .expect("read Artists")
            .iter()
            .map(|item| item.artist.id.clone())
            .collect::<Vec<_>>(),
        std::slice::from_ref(&guest.id)
    );
    assert_eq!(
        accepted
            .library
            .album_artists(None)
            .expect("read Album Artists")
            .iter()
            .map(|item| item.artist.id.clone())
            .collect::<Vec<_>>(),
        std::slice::from_ref(&track_album_artist.id)
    );
    let album_artist = accepted
        .library
        .artist_discography(&track_album_artist.id, None)
        .expect("read Album Artist releases")
        .expect("Album Artist releases");
    assert_eq!(album_artist.albums.len(), 1);
    assert!(album_artist.appears_on.is_empty());
    let album_contributor = accepted
        .library
        .artist_discography(&album_only_artist.id, None)
        .expect("read Album contributor releases")
        .expect("Album contributor releases");
    assert!(album_contributor.albums.is_empty());
    assert_eq!(album_contributor.appears_on.len(), 1);
    let guest_tracks = accepted
        .library
        .artist_track_detail(&guest.id, None)
        .expect("read guest Artist")
        .expect("guest Artist");
    let guest_releases = accepted
        .library
        .artist_discography(&guest.id, None)
        .expect("read guest releases")
        .expect("guest releases");
    assert_eq!(
        guest_tracks
            .tracks
            .materialize()
            .expect("resolve guest Tracks")
            .iter()
            .map(|track| track.id.clone())
            .collect::<Vec<_>>(),
        vec![track.id.clone()]
    );
    assert!(guest_releases.albums.is_empty());
    assert_eq!(guest_releases.appears_on.len(), 1);

    let replacement_artist = ArtistCredit {
        id: library::ArtistId::new("artist:replacement-album"),
        name: "Replacement Album Artist".to_string(),
        musicbrainz_artist_id: None,
    };
    album.relations.artists = vec![replacement_artist.clone()];
    accepted
        .library
        .accept_source_update(SourceLibraryUpdate {
            albums: vec![album],
            ..SourceLibraryUpdate::default()
        })
        .expect("replace Album relationship");
    let replacement_tracks = accepted
        .library
        .artist_track_detail(&replacement_artist.id, None)
        .expect("read replacement Artist")
        .expect("replacement Artist");
    let replacement_releases = accepted
        .library
        .artist_discography(&replacement_artist.id, None)
        .expect("read replacement Artist releases")
        .expect("replacement Artist releases");
    assert_eq!(replacement_tracks.tracks.len(), 2);
    assert!(replacement_releases.albums.is_empty());
    assert_eq!(replacement_releases.appears_on.len(), 1);
}

#[test]
fn full_and_point_relationships_match_after_reopen() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.db");
    let library = Libraries::open(&path).expect("open Library");
    let full_source = SourceId::new("jellyfin:server:full-relationships");
    let point_source = SourceId::new("jellyfin:server:point-relationships");

    let artist = ArtistCredit {
        id: library::ArtistId::new("artist:relationship"),
        name: "Relationship Artist".to_string(),
        musicbrainz_artist_id: None,
    };
    let track_genre = GenreCredit {
        id: GenreId::new("genre:track-relationship"),
        name: "Track Genre".to_string(),
    };
    let album_genre = GenreCredit {
        id: GenreId::new("genre:album-relationship"),
        name: "Album Genre".to_string(),
    };
    let mood = MoodCredit {
        id: MoodId::new("mood:relationship"),
        name: "Focused".to_string(),
    };
    let mut first = track();
    first.relations.artists = vec![artist.clone()];
    first.relations.genres = vec![track_genre];
    first.relations.moods = vec![mood];
    let mut second = first.clone();
    second.id = library::TrackId::new("local:track:relationship-two");
    second.title = "Second".to_string();
    second.relations.artists.clear();
    second.relations.genres.clear();
    second.relations.moods.clear();
    let mut album = album_for_track(&first, 0);
    album.relations.artists = vec![artist];
    album.relations.genres = vec![album_genre.clone()];

    let mut full = library
        .begin_source_candidate(CandidateHeader {
            source_id: full_source,
            input_digest: digest(95),
        })
        .expect("begin full relationship candidate");
    full.write(CandidateBatch::Albums(vec![album.clone()]))
        .expect("write full Album");
    full.write(CandidateBatch::Tracks(vec![first.clone(), second.clone()]))
        .expect("write full Tracks");
    let full = full
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept full relationships");
    let album_genre_detail = full
        .library
        .genre_detail(&album_genre.id, None)
        .expect("read Album Genre")
        .expect("Album Genre");
    assert_eq!(album_genre_detail.summary.album_count, 1);
    assert_eq!(album_genre_detail.summary.track_count, 2);
    assert_eq!(album_genre_detail.tracks.len(), 2);
    assert_eq!(
        full.library
            .compose_radio(RadioComposition {
                seed: RadioSeed::Genre {
                    id: album_genre.id.clone(),
                    name: album_genre.name.clone(),
                },
                native: None,
                excluded_track_ids: Vec::new(),
                limit: 2,
                include_seed_track: false,
                require_local_playback: false,
                variation: 0,
            })
            .expect("compose Album Genre radio")
            .len(),
        2
    );
    let random = full
        .library
        .compose_random(RandomComposition {
            native: Vec::new(),
            criteria: RandomCriteria {
                limit: 2,
                min_year: None,
                max_year: None,
                genre_id: Some(album_genre.id.clone()),
                genre_name: None,
                played_filter: PlayedFilter::All,
            },
            music_folder_id: None,
            variation: 0,
        })
        .expect("compose Album Genre random play");
    assert_eq!(random.len(), 2);
    assert!(random.iter().all(|track| {
        full.library
            .track(&track.id)
            .expect("read canonical random Track")
            .is_some_and(|canonical| Track::ptr_eq(&canonical, track))
    }));
    let native_first = full
        .library
        .compose_radio(RadioComposition {
            seed: RadioSeed::Genre {
                id: album_genre.id.clone(),
                name: album_genre.name.clone(),
            },
            native: Some(vec![second.clone()]),
            excluded_track_ids: Vec::new(),
            limit: 2,
            include_seed_track: false,
            require_local_playback: false,
            variation: 0,
        })
        .expect("underfill native Album Genre radio");
    assert_eq!(
        native_first
            .iter()
            .map(|track| track.id.clone())
            .collect::<Vec<_>>(),
        vec![second.id.clone(), first.id.clone()]
    );

    let point = library
        .begin_source_candidate(CandidateHeader {
            source_id: point_source.clone(),
            input_digest: digest(96),
        })
        .expect("begin empty point candidate");
    let point = point
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept empty point candidate");
    point
        .library
        .accept_source_update(SourceLibraryUpdate {
            albums: vec![album],
            tracks: vec![first, second],
            ..SourceLibraryUpdate::default()
        })
        .expect("accept equivalent point relationships");

    let projection = |loaded: &Arc<library::Library>| {
        (
            loaded
                .albums(None)
                .expect("read Albums")
                .iter()
                .map(|item| (item.album.id.clone(), item.track_count))
                .collect::<Vec<_>>(),
            loaded
                .artists(None)
                .expect("read Artists")
                .iter()
                .map(|item| {
                    (
                        item.artist.id.clone(),
                        item.album_count,
                        item.track_count,
                        item.duration_seconds,
                    )
                })
                .collect::<Vec<_>>(),
            loaded
                .genres(None)
                .expect("read Genres")
                .iter()
                .map(|item| (item.genre.id.clone(), item.album_count, item.track_count))
                .collect::<Vec<_>>(),
            loaded
                .moods(None)
                .expect("read Moods")
                .iter()
                .map(|item| (item.mood.id.clone(), item.track_count))
                .collect::<Vec<_>>(),
        )
    };
    let expected = projection(&full.library);
    assert_eq!(projection(&point.library), expected);

    drop(full);
    drop(point);
    drop(library);
    let reopened = Libraries::open(path)
        .expect("reopen Library")
        .load_source(&point_source)
        .expect("load point source")
        .expect("accepted point source");
    assert_eq!(projection(&reopened), expected);
}

#[test]
fn radio_varies_its_bounded_window_and_passes_excluded_tracks() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let library = Libraries::open(directory.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new("local:server:radio-window");
    let candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id,
            input_digest: digest(97),
        })
        .expect("begin empty radio candidate");
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept empty radio candidate");
    let tracks = (0..640)
        .map(|index| {
            let mut track = track();
            track.id = library::TrackId::new(format!("local:track:radio-{index:04}"));
            track.title = format!("Radio Track {index}");
            track.track_number = index + 1;
            track.source_path = Some(format!("/music/Artist/Album/{index:04}.flac"));
            track
        })
        .collect::<Vec<_>>();
    accepted
        .library
        .accept_source_update(SourceLibraryUpdate {
            tracks: tracks.clone(),
            ..SourceLibraryUpdate::default()
        })
        .expect("accept radio Tracks");

    let downloaded = directory.path().join("radio-0001.flac");
    std::fs::write(&downloaded, b"available").expect("write downloaded radio Track");
    accepted
        .library
        .set_downloaded_file(tracks[1].id.clone(), downloaded)
        .expect("attach downloaded radio Track");
    let offline = accepted
        .library
        .compose_radio(RadioComposition {
            seed: RadioSeed::Track(tracks[0].id.clone()),
            native: None,
            excluded_track_ids: Vec::new(),
            limit: 20,
            include_seed_track: false,
            require_local_playback: true,
            variation: 0,
        })
        .expect("compose offline radio");
    assert_eq!(
        offline.iter().map(|track| &track.id).collect::<Vec<_>>(),
        vec![&tracks[1].id]
    );

    let compose = |variation, excluded_track_ids| {
        accepted
            .library
            .compose_radio(RadioComposition {
                seed: RadioSeed::Track(tracks[0].id.clone()),
                native: None,
                excluded_track_ids,
                limit: 1,
                include_seed_track: false,
                require_local_playback: false,
                variation,
            })
            .expect("compose bounded radio")
            .pop()
            .expect("one radio Track")
    };
    let varied = compose(600, Vec::new());
    assert!((600..608).any(|index| varied.id == tracks[index].id));

    let after_excluded = compose(
        0,
        tracks
            .iter()
            .take(620)
            .map(|track| track.id.clone())
            .collect(),
    );
    assert!((620..628).any(|index| after_excluded.id == tracks[index].id));
}

fn accept(
    library: &Libraries,
    source_id: SourceId,
    input_digest: [u8; 32],
    title: &str,
    home_kind: Option<SourceHomeSectionKind>,
    current: Option<&Arc<library::Library>>,
) -> library::CandidateCommit {
    let mut track = track();
    track.title = title.to_string();
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id,
            input_digest,
        })
        .expect("begin candidate");
    candidate
        .write(CandidateBatch::Tracks(vec![track.clone()]))
        .expect("write Track");
    candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: match home_kind {
                    Some(kind) => HomeFacts::Source {
                        sections: vec![SourceHomeSection {
                            kind,
                            items: vec![HomeItemId::Track(track.id.clone())],
                        }],
                    },
                    None => HomeFacts::RufinDefined,
                },
                accepted_at: 1,
            },
            current,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept candidate")
}

fn accept_track(
    library: &Libraries,
    source_id: SourceId,
    input_digest: [u8; 32],
    track: Track,
    current: Option<&Arc<library::Library>>,
    accepted_at: i64,
) -> library::CandidateCommit {
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id,
            input_digest,
        })
        .expect("begin Track candidate");
    candidate
        .write(CandidateBatch::Tracks(vec![track]))
        .expect("write Track");
    candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at,
            },
            current,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept Track candidate")
}

fn accept_track_and_playlist(
    library: &Libraries,
    source_id: SourceId,
    input_digest: [u8; 32],
    track: Track,
    playlist: PlaylistSnapshot,
    current: Option<&Arc<library::Library>>,
    accepted_at: i64,
) -> library::CandidateCommit {
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id,
            input_digest,
        })
        .expect("begin Track and Playlist candidate");
    candidate
        .write(CandidateBatch::Tracks(vec![track]))
        .expect("write Track");
    candidate
        .write(CandidateBatch::Playlists(vec![playlist]))
        .expect("write Playlist");
    candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at,
            },
            current,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept Track and Playlist candidate")
}

fn accept_local_tracks(
    library: &Libraries,
    source_id: SourceId,
    tracks: Vec<Track>,
    files: Vec<LocalFile>,
    digest_byte: u8,
) -> library::CandidateCommit {
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id,
            input_digest: digest(digest_byte),
        })
        .expect("begin Local candidate");
    if !tracks.is_empty() {
        candidate
            .write(CandidateBatch::Tracks(tracks))
            .expect("write Local Tracks");
    }
    if !files.is_empty() {
        candidate
            .write(CandidateBatch::LocalFiles(files))
            .expect("write Local files");
    }
    candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept Local candidate")
}

fn track() -> Track {
    let artist = ArtistCredit {
        id: library::ArtistId::new("local:artist:one"),
        name: "Artist".to_string(),
        musicbrainz_artist_id: None,
    };
    Track::new(TrackData {
        id: library::TrackId::new("local:track:one"),
        album_id: Some(library::AlbumId::new("local:album:one")),
        title: "Track".to_string(),
        artist: "Artist".to_string(),
        album: "Album".to_string(),
        album_artwork: None,
        year: 2024,
        release_date: Some("2024-01-01".to_string()),
        date_added: None,
        last_played: None,
        play_count: None,
        user_rating: None,
        duration_seconds: 180,
        favorite: false,
        disc_number: 1,
        track_number: 1,
        image_ref: None,
        local_artwork: None,
        musicbrainz_recording_id: None,
        musicbrainz_release_track_id: None,
        source_path: Some("/music/Artist/Album/Track.flac".to_string()),
        cue: None,
        source_format: Some("flac".to_string()),
        comment: None,
        skip_count: None,
        bpm: None,
        relations: TrackRelations {
            artists: vec![artist.clone()],
            album_artists: vec![artist],
            genres: vec![GenreCredit {
                id: GenreId::new("local:genre:rock"),
                name: "Rock".to_string(),
            }],
            moods: Vec::new(),
            music_folders: Vec::new(),
        },
    })
}

fn digest_fixture(track: &Track, projection_count: u32, reverse: bool) -> Vec<CandidateBatch> {
    let artist = track.relations.artists[0].clone();
    let mut batches = vec![
        CandidateBatch::Albums(vec![album_for_track(track, projection_count)]),
        CandidateBatch::Tracks(vec![track.clone()]),
        CandidateBatch::Artists(vec![Artist {
            id: artist.id,
            name: artist.name,
            favorite: false,
            last_played: None,
            play_count: None,
            user_rating: None,
            musicbrainz_artist_id: artist.musicbrainz_artist_id,
            image_ref: None,
            local_artwork: None,
        }]),
        CandidateBatch::Genres(vec![Genre {
            id: GenreId::new("local:genre:rock"),
            name: "Rock".to_string(),
            image_ref: None,
        }]),
        CandidateBatch::MusicFolders(vec![MusicFolder {
            id: MusicFolderId::new("folder:one"),
            name: "Music".to_string(),
            image_ref: None,
        }]),
        CandidateBatch::Playlists(vec![PlaylistSnapshot {
            playlist: Playlist {
                id: PlaylistId::new("playlist:digest"),
                name: "Digest".to_string(),
                image_ref: None,
            },
            entries: vec![PlaylistEntry {
                occurrence_id: "one".to_string(),
                track_id: track.id.clone(),
            }],
        }]),
    ];
    if reverse {
        batches.reverse();
    }
    batches
}

fn album_for_track(track: &Track, projection_count: u32) -> Album {
    Album {
        id: track.album_id.clone().expect("fixture Album ID"),
        title: track.album.clone(),
        artist: track.artist.clone(),
        year: track.year,
        release_date: track.release_date.clone(),
        date_added: None,
        last_played: None,
        play_count: None,
        user_rating: None,
        favorite: false,
        color_seed: projection_count,
        image_ref: None,
        local_artwork: None,
        release_types: Vec::new(),
        is_compilation: None,
        musicbrainz_album_id: None,
        musicbrainz_release_group_id: None,
        relations: AlbumRelations {
            album_artists: track.relations.album_artists.clone(),
            artists: track.relations.artists.clone(),
            genres: track.relations.genres.clone(),
        },
    }
}

fn artist_for_track(track: &Track) -> Artist {
    Artist {
        id: track.relations.artists[0].id.clone(),
        name: track.artist.clone(),
        favorite: false,
        last_played: None,
        play_count: None,
        user_rating: None,
        musicbrainz_artist_id: None,
        image_ref: None,
        local_artwork: None,
    }
}

fn digest(byte: u8) -> [u8; 32] {
    [byte; 32]
}

fn local_audio_file(path: &str, relative_path: &str) -> LocalFile {
    local_audio_file_in_root(path, "/music", relative_path)
}

fn local_audio_file_in_root(path: &str, root: &str, relative_path: &str) -> LocalFile {
    LocalFile {
        path: path.to_string(),
        root: root.to_string(),
        relative_path: relative_path.to_string(),
        kind: LocalFileKind::Media,
        size_bytes: Some(1_024),
        mtime_ns: 1,
        device_id: None,
        inode: None,
        parse_version: Some(1),
        state: LocalFileState::Accepted,
        dependencies: Vec::new(),
    }
}

fn local_directory_file(path: &str, root: &str) -> LocalFile {
    LocalFile {
        path: path.to_string(),
        root: root.to_string(),
        relative_path: path
            .strip_prefix(root)
            .unwrap_or(path)
            .trim_start_matches('/')
            .to_string(),
        kind: LocalFileKind::Directory,
        size_bytes: None,
        mtime_ns: 1,
        device_id: None,
        inode: None,
        parse_version: None,
        state: LocalFileState::Observed,
        dependencies: Vec::new(),
    }
}

fn local_cue_file(path: &str, dependencies: Vec<String>, state: LocalFileState) -> LocalFile {
    LocalFile {
        path: path.to_string(),
        root: "/music".to_string(),
        relative_path: path.strip_prefix("/music/").unwrap_or(path).to_string(),
        kind: LocalFileKind::Cue,
        size_bytes: Some(256),
        mtime_ns: 1,
        device_id: None,
        inode: None,
        parse_version: Some(1),
        state,
        dependencies,
    }
}

fn test_local_folder_id(path: &str) -> FolderId {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in path.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    FolderId::new(format!("local:folder:{hash:016x}"))
}

fn home_track_title(home: &library::HomeSnapshot, kind: HomeSectionKind) -> Option<&str> {
    let LoadedHomeItem::Track(track) = home.section(kind)?.items.first()? else {
        return None;
    };
    Some(track.title.as_str())
}

fn playlist_entry(
    entries: &library::PlaylistEntryList,
    position: usize,
) -> library::PlaylistEntryItem {
    entries
        .entry(position)
        .expect("read Playlist entry")
        .expect("Playlist entry")
}

fn playlist_entries(entries: &library::PlaylistEntryList) -> Vec<library::PlaylistEntryItem> {
    (0..entries.len())
        .map(|position| playlist_entry(entries, position))
        .collect()
}
