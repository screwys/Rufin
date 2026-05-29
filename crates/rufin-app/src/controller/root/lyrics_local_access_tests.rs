use super::*;

#[test]
pub(in crate::controller) fn explicit_favorite_updates_can_unfavorite_persistent_controls() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap(Some(FakeScale::Small));
    let album = snapshot
        .albums
        .iter()
        .find(|album| !album.favorite)
        .expect("non-favorite album")
        .clone();
    controller.set_album_favorite(album.id.clone(), true);
    let (item_id, favorite, snapshot) = wait_for_favorite_changed(&events);
    assert_eq!(item_id, FavoriteItemId::Album(album.id.clone()));
    assert!(favorite);
    assert!(
        snapshot
            .albums
            .iter()
            .find(|candidate| candidate.id == album.id)
            .expect("cached album")
            .favorite
    );
    controller.set_album_favorite(album.id.clone(), false);
    let (item_id, favorite, snapshot) = wait_for_favorite_changed(&events);
    assert_eq!(item_id, FavoriteItemId::Album(album.id.clone()));
    assert!(!favorite);
    assert!(
        !snapshot
            .albums
            .iter()
            .find(|candidate| candidate.id == album.id)
            .expect("cached album")
            .favorite
    );
}
#[test]
pub(in crate::controller) fn fake_playlist_mutations_create_move_and_remove_entries() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap(Some(FakeScale::Small));
    let first = snapshot.tracks[0].clone();
    let second = snapshot.tracks[1].clone();
    let third = snapshot.tracks[2].clone();
    controller.create_playlist(
        "Controller Playlist".to_string(),
        vec![first.clone(), second.clone()],
    );
    let snapshot = wait_for_snapshot(&events);
    let playlist = snapshot
        .playlists
        .iter()
        .find(|playlist| playlist.name == "Controller Playlist")
        .expect("created playlist")
        .clone();
    assert_playlist_order(
        &controller,
        &playlist.id,
        &[first.id.as_str(), second.id.as_str()],
    );
    let detail = controller
        .cached_playlist_detail(&playlist.id)
        .expect("playlist detail")
        .expect("playlist detail");
    controller.move_playlist_entry(playlist.id.clone(), detail.entries[1].entry_id.clone(), 0);
    let (changed_id, _snapshot) = wait_for_playlist_changed(&events);
    assert_eq!(changed_id, playlist.id);
    assert_playlist_order(
        &controller,
        &playlist.id,
        &[second.id.as_str(), first.id.as_str()],
    );
    controller.add_tracks_to_playlist(playlist.id.clone(), vec![third.clone()]);
    let (changed_id, _snapshot) = wait_for_playlist_changed(&events);
    assert_eq!(changed_id, playlist.id);
    assert_playlist_order(
        &controller,
        &playlist.id,
        &[second.id.as_str(), first.id.as_str(), third.id.as_str()],
    );
    let detail = controller
        .cached_playlist_detail(&playlist.id)
        .expect("playlist detail")
        .expect("playlist detail");
    controller.remove_playlist_entry(playlist.id.clone(), detail.entries[0].entry_id.clone());
    let (changed_id, _snapshot) = wait_for_playlist_changed(&events);
    assert_eq!(changed_id, playlist.id);
    assert_playlist_order(
        &controller,
        &playlist.id,
        &[first.id.as_str(), third.id.as_str()],
    );
}
#[test]
pub(in crate::controller) fn fake_lyrics_request_emits_empty_lyrics_event() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap(Some(FakeScale::Small));
    controller.play_now(snapshot.tracks[0].clone());
    let _playback = wait_for_playback_state(&controller, &events, PlaybackState::Playing);
    controller.request_lyrics_for_current();
    assert!(wait_for_lyrics(&events).is_none());
}
#[test]
pub(in crate::controller) fn server_lyrics_request_ignores_cached_remote_lyrics() {
    let (controller, events, snapshot, _queue, _player) =
        AppController::bootstrap(Some(FakeScale::Small));
    let track = snapshot.tracks[0].clone();
    controller.play_now(track.clone());
    let _playback = wait_for_playback_state(&controller, &events, PlaybackState::Playing);
    let server_id = controller
        .store
        .with_store(|store| store.active_server())
        .expect("load active server")
        .expect("active server")
        .server
        .id;
    let remote_lyrics = Lyrics {
        track_id: track.id,
        source: LyricsSource::Remote,
        lines: vec![LyricLine {
            text: "cached remote line".to_string(),
            start_millis: None,
        }],
    };
    controller
        .store
        .with_store(|store| store.save_lyrics(&server_id, &remote_lyrics))
        .expect("save remote lyrics");
    controller.request_server_lyrics_for_current();
    assert!(wait_for_lyrics(&events).is_none());
}
#[test]
pub(in crate::controller) fn restored_queue_request_lyrics_emits_cached_current_lyrics() {
    let store = StoreHandle::open_memory().expect("memory store");
    let saved = SavedServer {
        server: ServerIdentity {
            id: ServerId::new("jellyfin:server:lyrics"),
            provider: "jellyfin".to_string(),
            name: "Lyrics Server".to_string(),
            base_url: "https://music.example".to_string(),
        },
        user_id: "user".to_string(),
        username: "demo".to_string(),
        trust_invalid_cert: false,
    };
    let track = restored_track();
    let mut queue = QueueEngine::new(saved.server.id.clone());
    queue.play_now(&track);
    queue.set_progress_seconds(12);
    let lyrics = Lyrics {
        track_id: track.id.clone(),
        source: LyricsSource::Server,
        lines: vec![LyricLine {
            text: "first line".to_string(),
            start_millis: Some(1_000),
        }],
    };
    store
        .with_store(|store| {
            store.save_server(&saved)?;
            store.set_active_server(&saved.server.id)?;
            store.save_queue_snapshot(&queue.snapshot())?;
            store.save_lyrics(&saved.server.id, &lyrics)?;
            Ok(())
        })
        .expect("seed restored state");
    let (controller, events) = controller_from_store_for_test(store);
    controller.request_lyrics_for_current();
    assert_eq!(wait_for_lyrics(&events), Some(lyrics));
}
#[test]
pub(in crate::controller) fn lyrics_search_respects_private_mode_and_preference() {
    let mut settings = AppSettings {
        external_lyrics_enabled: true,
        ..AppSettings::default()
    };
    assert_eq!(
        super::lyrics_search_for_settings(&settings),
        JellyfinLyricsSearch::ServerThenRemote
    );
    settings.prefer_server_lyrics = false;
    assert_eq!(
        super::lyrics_search_for_settings(&settings),
        JellyfinLyricsSearch::RemoteThenServer
    );
    settings.private_mode = true;
    assert_eq!(
        super::lyrics_search_for_settings(&settings),
        JellyfinLyricsSearch::ServerOnly
    );
    settings.private_mode = false;
    settings.external_lyrics_enabled = false;
    assert_eq!(
        super::lyrics_search_for_settings(&settings),
        JellyfinLyricsSearch::ServerOnly
    );
}
#[test]
pub(in crate::controller) fn saved_lrclib_result_uses_explicit_output_path() {
    let dir = self::unique_test_dir("lyrics-portal-save");
    fs::create_dir_all(&dir).expect("create dir");
    let sidecar = dir.join("Track.lrc");
    let output = dir.join("Chosen Lyrics.lrc");
    let entry = rufin_core::QueueEntry {
        id: rufin_core::QueueEntryId::new("queue-entry:lyrics"),
        track_id: TrackId::new("jellyfin:track:lyrics-save"),
        album_id: None,
        title: "Track".to_string(),
        artist: "Artist".to_string(),
        artist_id: None,
        album: "Album".to_string(),
        year: 0,
        duration_seconds: 180,
        favorite: false,
        image_ref: None,
        local_path: Some(dir.join("Track.flac").to_string_lossy().into_owned()),
        source_format: None,
    };
    let result = super::LyricsSearchResult {
        id: 1,
        track_name: "Track".to_string(),
        artist_name: "Artist".to_string(),
        album_name: "Album".to_string(),
        duration_seconds: 180,
        synced_lyrics: Some("[00:01.00]line one".to_string()),
        plain_lyrics: None,
    };
    let (saved_path, lyrics) = super::save_lrclib_result(
        &ServerId::new("jellyfin:server:lyrics"),
        &entry,
        &result,
        output.clone(),
    )
    .expect("save lyrics");
    assert_eq!(saved_path, output);
    assert_eq!(
        fs::read_to_string(&saved_path).expect("saved lyrics"),
        "[00:01.00]line one"
    );
    assert!(!sidecar.exists());
    assert!(!dir.join("Chosen Lyrics.lrc.tmp").exists());
    assert_eq!(lyrics.track_id, entry.track_id);
    let _cleanup = fs::remove_dir_all(dir);
}
#[test]
pub(in crate::controller) fn local_sidecar_lyrics_use_same_stem_as_audio_file() {
    let store = StoreHandle::open_memory().expect("memory store");
    let saved = self::saved_server();
    let dir = self::unique_test_dir("local-sidecar");
    fs::create_dir_all(&dir).expect("create dir");
    let generation = store
        .with_store(|store| {
            store.save_server(&saved)?;
            store.set_active_server(&saved.server.id)?;
            store.save_server_local_access(&ServerLocalAccess {
                server_id: saved.server.id.clone(),
                root_path: dir.to_string_lossy().into_owned(),
                path_replace_from: None,
                path_replace_to: Some(dir.to_string_lossy().into_owned()),
            })?;
            store.begin_sync(&saved.server.id)
        })
        .expect("begin sync");
    let audio = dir.join("07 I'm feeling lucky.flac");
    let lrc = dir.join("07 I'm feeling lucky.lrc");
    fs::write(&audio, []).expect("audio");
    fs::write(&lrc, "[00:01.00]line one").expect("lrc");
    let mut track = restored_track();
    track.local_path = Some(audio.to_string_lossy().into_owned());
    store
        .with_store(|store| store.upsert_tracks(&saved.server.id, &[track.clone()], generation))
        .expect("upsert track");
    let lyrics =
        super::local_sidecar_lyrics(&store, &saved.server.id, &track.id).expect("sidecar lyrics");
    assert_eq!(lyrics.source, LyricsSource::Local);
    assert_eq!(lyrics.lines[0].text, "line one");
    assert_eq!(lyrics.lines[0].start_millis, Some(1_000));
    let _cleanup = fs::remove_dir_all(dir);
}
#[test]
pub(in crate::controller) fn local_sidecar_lyrics_ignore_oversized_files() {
    let store = StoreHandle::open_memory().expect("memory store");
    let saved = self::saved_server();
    let dir = self::unique_test_dir("local-sidecar-large");
    fs::create_dir_all(&dir).expect("create dir");
    let generation = store
        .with_store(|store| {
            store.save_server(&saved)?;
            store.set_active_server(&saved.server.id)?;
            store.save_server_local_access(&ServerLocalAccess {
                server_id: saved.server.id.clone(),
                root_path: dir.to_string_lossy().into_owned(),
                path_replace_from: None,
                path_replace_to: Some(dir.to_string_lossy().into_owned()),
            })?;
            store.begin_sync(&saved.server.id)
        })
        .expect("begin sync");
    let audio = dir.join("Track.flac");
    let lrc = dir.join("Track.lrc");
    fs::write(&audio, []).expect("audio");
    let file = fs::File::create(&lrc).expect("lrc");
    file.set_len((LOCAL_LYRICS_MAX_BYTES + 1) as u64)
        .expect("lrc length");
    let mut track = restored_track();
    track.local_path = Some(audio.to_string_lossy().into_owned());
    store
        .with_store(|store| store.upsert_tracks(&saved.server.id, &[track.clone()], generation))
        .expect("upsert track");

    let lyrics = super::local_sidecar_lyrics(&store, &saved.server.id, &track.id);

    assert_eq!(lyrics, None);
    let _cleanup = fs::remove_dir_all(dir);
}
#[test]
pub(in crate::controller) fn mapped_local_audio_path_uses_server_prefix_replacement() {
    let store = StoreHandle::open_memory().expect("memory store");
    let saved = self::saved_server();
    let generation = store
        .with_store(|store| {
            store.save_server(&saved)?;
            store.save_server_local_access(&ServerLocalAccess {
                server_id: saved.server.id.clone(),
                root_path: "/unused".to_string(),
                path_replace_from: Some("/server/music".to_string()),
                path_replace_to: Some(
                    self::unique_test_dir("mapped-audio")
                        .to_string_lossy()
                        .into_owned(),
                ),
            })?;
            store.begin_sync(&saved.server.id)
        })
        .expect("begin sync");
    let root = store
        .with_store(|store| store.server_local_access(&saved.server.id))
        .expect("access")
        .expect("access")
        .path_replace_to
        .expect("replace to");
    let root = PathBuf::from(root);
    let audio = root.join("Album/Track.flac");
    fs::create_dir_all(audio.parent().expect("parent")).expect("create dir");
    fs::write(&audio, []).expect("audio");
    let mut track = restored_track();
    track.local_path = Some("/server/music/Album/Track.flac".to_string());
    store
        .with_store(|store| store.upsert_tracks(&saved.server.id, &[track.clone()], generation))
        .expect("upsert track");
    let mapped = super::local_audio_path_for_track(&store, &saved.server.id, &track.id)
        .expect("mapped path");
    assert_eq!(mapped, audio);
    let _cleanup = fs::remove_dir_all(root);
}
#[test]
pub(in crate::controller) fn remote_local_audio_path_requires_configured_access() {
    let store = StoreHandle::open_memory().expect("memory store");
    let saved = self::saved_server();
    let generation = store
        .with_store(|store| {
            store.save_server(&saved)?;
            store.set_active_server(&saved.server.id)?;
            store.begin_sync(&saved.server.id)
        })
        .expect("begin sync");
    let dir = self::unique_test_dir("remote-no-local-access");
    fs::create_dir_all(&dir).expect("create dir");
    let audio = dir.join("Track.flac");
    fs::write(&audio, []).expect("audio");
    let mut track = restored_track();
    track.local_path = Some(audio.to_string_lossy().into_owned());
    store
        .with_store(|store| store.upsert_tracks(&saved.server.id, &[track.clone()], generation))
        .expect("upsert track");
    let mapped = super::local_audio_path_for_track(&store, &saved.server.id, &track.id);
    assert_eq!(mapped, None);
    let _cleanup = fs::remove_dir_all(dir);
}
#[test]
pub(in crate::controller) fn resolve_stream_prefers_local_file_for_remote_server_with_access() {
    let store = StoreHandle::open_memory().expect("memory store");
    let saved = self::saved_server();
    let root = self::unique_test_dir("local-playback-stream");
    let audio = root.join("Album/Track.flac");
    fs::create_dir_all(audio.parent().expect("parent")).expect("create dir");
    fs::write(&audio, []).expect("audio");
    let generation = store
        .with_store(|store| {
            store.save_server(&saved)?;
            store.set_active_server(&saved.server.id)?;
            store.save_server_local_access(&ServerLocalAccess {
                server_id: saved.server.id.clone(),
                root_path: root.to_string_lossy().into_owned(),
                path_replace_from: Some("/server/music".to_string()),
                path_replace_to: Some(root.to_string_lossy().into_owned()),
            })?;
            store.begin_sync(&saved.server.id)
        })
        .expect("begin sync");
    let mut track = restored_track();
    track.local_path = Some("/server/music/Album/Track.flac".to_string());
    store
        .with_store(|store| store.upsert_tracks(&saved.server.id, &[track.clone()], generation))
        .expect("upsert track");
    let runtime = Arc::new(Runtime::new().expect("runtime"));
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let stream = super::resolve_stream(
        &store,
        &runtime,
        &secrets,
        &saved.server.id,
        &track.id,
        &PlaybackSettings::default(),
    )
    .expect("stream");
    assert!(stream.uri().starts_with("file://"));
    assert!(stream.uri().contains("Track.flac"));
    let _cleanup = fs::remove_dir_all(root);
}
#[test]
pub(in crate::controller) fn resolve_stream_uses_cached_local_match_without_server_path() {
    let store = StoreHandle::open_memory().expect("memory store");
    let saved = self::saved_server();
    let root = self::unique_test_dir("cached-local-match-stream");
    let audio = root.join("Album/Track.flac");
    fs::create_dir_all(audio.parent().expect("parent")).expect("create dir");
    fs::write(&audio, []).expect("audio");
    let generation = store
        .with_store(|store| {
            store.save_server(&saved)?;
            store.set_active_server(&saved.server.id)?;
            store.save_server_local_access(&ServerLocalAccess {
                server_id: saved.server.id.clone(),
                root_path: root.to_string_lossy().into_owned(),
                path_replace_from: None,
                path_replace_to: Some(root.to_string_lossy().into_owned()),
            })?;
            store.begin_sync(&saved.server.id)
        })
        .expect("begin sync");
    let track = restored_track();
    store
        .with_store(|store| {
            store.upsert_tracks(&saved.server.id, std::slice::from_ref(&track), generation)?;
            store.replace_track_local_matches(
                &saved.server.id,
                &[(
                    track.id.clone(),
                    audio.to_string_lossy().into_owned(),
                    "metadata".to_string(),
                )],
            )
        })
        .expect("seed track");
    let runtime = Arc::new(Runtime::new().expect("runtime"));
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let stream = super::resolve_stream(
        &store,
        &runtime,
        &secrets,
        &saved.server.id,
        &track.id,
        &PlaybackSettings::default(),
    )
    .expect("stream");
    assert!(stream.uri().starts_with("file://"));
    assert!(stream.uri().contains("Track.flac"));
    let _cleanup = fs::remove_dir_all(root);
}
#[test]
pub(in crate::controller) fn relative_local_audio_path_uses_configured_local_prefix() {
    let store = StoreHandle::open_memory().expect("memory store");
    let saved = self::saved_server();
    let scan_root = self::unique_test_dir("relative-scan-root");
    let local_root = self::unique_test_dir("relative-local-prefix");
    let audio = local_root.join("Album/Track.flac");
    fs::create_dir_all(audio.parent().expect("parent")).expect("create dir");
    fs::write(&audio, []).expect("audio");
    let generation = store
        .with_store(|store| {
            store.save_server(&saved)?;
            store.set_active_server(&saved.server.id)?;
            store.save_server_local_access(&ServerLocalAccess {
                server_id: saved.server.id.clone(),
                root_path: scan_root.to_string_lossy().into_owned(),
                path_replace_from: None,
                path_replace_to: Some(local_root.to_string_lossy().into_owned()),
            })?;
            store.begin_sync(&saved.server.id)
        })
        .expect("begin sync");
    let mut track = restored_track();
    track.local_path = Some("Album/Track.flac".to_string());
    store
        .with_store(|store| store.upsert_tracks(&saved.server.id, &[track.clone()], generation))
        .expect("upsert track");
    let mapped = super::local_audio_path_for_track(&store, &saved.server.id, &track.id)
        .expect("mapped path");
    assert_eq!(mapped, audio);
    let _cleanup = fs::remove_dir_all(scan_root);
    let _cleanup = fs::remove_dir_all(local_root);
}
#[test]
pub(in crate::controller) fn snapshot_local_access_status_counts_cached_mapping_candidates() {
    let store = StoreHandle::open_memory().expect("memory store");
    let saved = self::saved_server();
    let root = self::unique_test_dir("local-access-status");
    let local_prefix = root.join("mapped");
    let direct_audio = root.join("Direct.flac");
    let prefix_audio = local_prefix.join("Album/Mapped.flac");
    let metadata_audio = root.join("Metadata.flac");
    fs::create_dir_all(prefix_audio.parent().expect("parent")).expect("create mapped dir");
    fs::write(&direct_audio, []).expect("direct audio");
    fs::write(&prefix_audio, []).expect("prefix audio");
    fs::write(&metadata_audio, []).expect("metadata audio");
    let generation = store
        .with_store(|store| {
            store.save_server(&saved)?;
            store.set_active_server(&saved.server.id)?;
            store.save_server_local_access(&ServerLocalAccess {
                server_id: saved.server.id.clone(),
                root_path: root.to_string_lossy().into_owned(),
                path_replace_from: Some("/server/music".to_string()),
                path_replace_to: Some(local_prefix.to_string_lossy().into_owned()),
            })?;
            store.begin_sync(&saved.server.id)
        })
        .expect("begin sync");
    let mut direct = restored_track();
    direct.id = TrackId::new("jellyfin:track:direct");
    direct.title = "Direct".to_string();
    direct.local_path = Some(direct_audio.to_string_lossy().into_owned());
    let mut prefix = restored_track();
    prefix.id = TrackId::new("jellyfin:track:prefix");
    prefix.title = "Prefix".to_string();
    prefix.local_path = Some("/server/music/Album/Mapped.flac".to_string());
    let mut metadata = restored_track();
    metadata.id = TrackId::new("jellyfin:track:metadata");
    metadata.title = "Metadata".to_string();
    let mut unmatched = restored_track();
    unmatched.id = TrackId::new("jellyfin:track:unmatched");
    unmatched.title = "Unmatched".to_string();
    unmatched.local_path = Some("/server/music/Album/Missing.flac".to_string());
    store
        .with_store(|store| {
            store.upsert_tracks(
                &saved.server.id,
                &[direct, prefix, metadata.clone(), unmatched],
                generation,
            )?;
            store.replace_track_local_matches(
                &saved.server.id,
                &[(
                    metadata.id.clone(),
                    metadata_audio.to_string_lossy().into_owned(),
                    "metadata".to_string(),
                )],
            )
        })
        .expect("seed tracks");
    let snapshot = super::load_snapshot(&store).expect("load snapshot");
    assert_eq!(snapshot.local_access_status.total_track_count, 4);
    assert_eq!(snapshot.local_access_status.direct_match_count, 1);
    assert_eq!(snapshot.local_access_status.prefix_match_count, 2);
    assert_eq!(snapshot.local_access_status.metadata_match_count, 1);
    assert_eq!(snapshot.local_access_status.unmatched_count, 0);
    assert!(snapshot.local_access_status.sample_server_path.is_some());
    let _cleanup = fs::remove_dir_all(root);
}
#[test]
pub(in crate::controller) fn conservative_local_matches_only_accept_unique_duration_matches() {
    let album = AlbumId::fake(1);
    let mut remote = restored_track();
    remote.album_id = album.clone();
    remote.title = "First Motion".to_string();
    remote.album = "Blue Rooms".to_string();
    remote.artist = "Astral Kin".to_string();
    remote.duration_seconds = 210;
    remote.disc_number = 1;
    remote.track_number = 7;
    let mut local = remote.clone();
    local.id = TrackId::new("local:track:one");
    local.local_path = Some("/home/me/Music/Blue Rooms/07 First Motion.flac".to_string());
    local.duration_seconds = 212;
    let matches = super::conservative_local_matches(&[remote.clone()], &[local.clone()]);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].0, remote.id);
    assert_eq!(
        matches[0].1,
        "/home/me/Music/Blue Rooms/07 First Motion.flac"
    );
    let local_one = local.clone();
    let mut duplicate = local;
    duplicate.id = TrackId::new("local:track:two");
    duplicate.local_path = Some("/home/me/Music/Other/07 First Motion.flac".to_string());
    assert!(super::conservative_local_matches(&[remote], &[local_one, duplicate]).is_empty());
}
#[test]
pub(in crate::controller) fn snapshot_includes_active_server_local_access() {
    let store = StoreHandle::open_memory().expect("memory store");
    let saved = self::saved_server();
    let access = ServerLocalAccess {
        server_id: saved.server.id.clone(),
        root_path: "/home/demo/Music".to_string(),
        path_replace_from: Some("/server/music".to_string()),
        path_replace_to: Some("/home/demo/Music".to_string()),
    };
    store
        .with_store(|store| {
            store.save_server(&saved)?;
            store.save_server_local_access(&access)?;
            store.set_active_server(&saved.server.id)
        })
        .expect("save server");
    let snapshot = super::load_snapshot(&store).expect("load snapshot");
    assert_eq!(snapshot.local_access, Some(access));
}
#[test]
pub(in crate::controller) fn lrclib_result_text_becomes_timed_lyrics() {
    let result = super::LyricsSearchResult {
        id: 7,
        track_name: "Song".to_string(),
        artist_name: "Artist".to_string(),
        album_name: "Album".to_string(),
        duration_seconds: 180,
        synced_lyrics: Some(
            "[00:12.34]first line\n[ar:Artist]\n[00:13.005]second line".to_string(),
        ),
        plain_lyrics: None,
    };
    let lyrics = super::lyrics_from_text(TrackId::new("track-one"), &result);
    assert_eq!(lyrics.lines.len(), 2);
    assert_eq!(lyrics.lines[0].text, "first line");
    assert_eq!(lyrics.lines[0].start_millis, Some(12_340));
    assert_eq!(lyrics.lines[1].text, "second line");
    assert_eq!(lyrics.lines[1].start_millis, Some(13_005));
}
#[test]
pub(in crate::controller) fn lrclib_duration_accepts_fractional_seconds() {
    let json = r#"{
            "id": 7,
            "trackName": "Imagine",
            "artistName": "John Lennon",
            "albumName": "Imagine",
            "duration": 185.0,
            "plainLyrics": "line",
            "syncedLyrics": null
        }"#;
    let dto = serde_json::from_str::<super::LrcLibLyricsDto>(json).expect("deserialize lrclib dto");
    let result = super::LyricsSearchResult::from(dto);
    assert_eq!(result.duration_seconds, 185);
    assert_eq!(result.track_name, "Imagine");
    assert_eq!(result.artist_name, "John Lennon");
}
#[test]
pub(in crate::controller) fn lrclib_manual_search_uses_combined_query_first() {
    let urls = super::lrclib_search_urls("joy", "feel my soul").expect("lrclib search urls");
    let query_pairs = urls[0]
        .query_pairs()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<Vec<_>>();
    assert_eq!(
        query_pairs,
        vec![("q".to_string(), "feel my soul joy".to_string())]
    );
}
#[test]
pub(in crate::controller) fn lrclib_search_body_decodes_feel_my_soul_result() {
    let json = r#"[{
            "id": 9386114,
            "name": "feel my soul",
            "artistName": "joy",
            "albumName": "feel my soul",
            "duration": 223.0,
            "plainLyrics": "plain line",
            "syncedLyrics": "[00:01.00]synced line",
            "lyricsfile": null
        }]"#;
    let results = super::parse_lrclib_search_body(json).expect("parse lrclib response");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, 9_386_114);
    assert_eq!(results[0].track_name, "feel my soul");
    assert_eq!(results[0].artist_name, "joy");
    assert_eq!(results[0].duration_seconds, 223);
    assert!(results[0].synced_lyrics.is_some());
    assert!(results[0].plain_lyrics.is_some());
}
#[test]
pub(in crate::controller) fn lrclib_results_prefer_matching_title_over_album_hit() {
    let mut results = vec![
        super::LyricsSearchResult {
            id: 1,
            track_name: "Crippled Inside".to_string(),
            artist_name: "John Lennon".to_string(),
            album_name: "Imagine".to_string(),
            duration_seconds: 233,
            synced_lyrics: Some("[00:01.00]line".to_string()),
            plain_lyrics: Some("line".to_string()),
        },
        super::LyricsSearchResult {
            id: 2,
            track_name: "Imagine".to_string(),
            artist_name: "John Lennon".to_string(),
            album_name: "Lennon".to_string(),
            duration_seconds: 185,
            synced_lyrics: None,
            plain_lyrics: Some("line".to_string()),
        },
    ];
    super::order_lrclib_results(&mut results, "John Lennon", "Imagine");
    assert_eq!(results[0].track_name, "Imagine");
}
#[test]
pub(in crate::controller) fn controller_events_are_sendable() {
    pub(in crate::controller) fn assert_send<T: Send>() {}
    assert_send::<ControllerEvent>();
}
#[test]
pub(in crate::controller) fn provider_not_found_cover_errors_are_classified() {
    assert!(super::covers::is_provider_not_found_error(
        "provider item was not found"
    ));
    assert!(!super::covers::is_provider_not_found_error(
        "provider network failed: offline"
    ));
}
pub(in crate::controller) fn controller_from_store_for_test(
    store: StoreHandle,
) -> (AppController, Receiver<ControllerEvent>) {
    let test_permit = Some(super::controller_test_permit());
    let (events, receiver) = channel();
    let runtime = Runtime::new()
        .map(Arc::new)
        .unwrap_or_else(|error| panic!("failed to create Tokio runtime: {error}"));
    let snapshot = load_snapshot(&store).expect("load snapshot");
    let settings = load_settings_from_store(&store);
    let queue = restore_queue(&store, snapshot.server.as_ref());
    let playback_snapshot =
        playback_snapshot_from_queue(queue.as_ref(), settings.auto_dj_enabled, &settings.playback);
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let controller = AppController {
        settings: super::settings_controller::SettingsController::new(
            store.clone(),
            secrets.clone(),
        ),
        store,
        runtime,
        secrets,
        queue: Arc::new(Mutex::new(queue)),
        playback: Arc::new(Mutex::new(Box::new(
            rufin_playback::FakePlaybackBackend::new(),
        ))),
        playback_snapshot: Arc::new(Mutex::new(playback_snapshot)),
        auto_dj_enabled: Arc::new(Mutex::new(settings.auto_dj_enabled)),
        last_progress_snapshot: Arc::new(Mutex::new(None)),
        last_report_snapshot: Arc::new(Mutex::new(None)),
        external_scrobble_state: Arc::new(Mutex::new(ExternalScrobbleState::default())),
        events,
        sync_in_flight: InFlightGuards::new("Sync"),
        home_refresh_in_flight: InFlightGuards::new("Home refresh"),
        playlist_refresh_in_flight: InFlightGuards::new("Playlist refresh"),
        explore_prefetch_in_flight: InFlightGuards::new("Explore prefetch"),
        cover_in_flight: Arc::new(Mutex::new(HashSet::new())),
        external_cover_prefetch_in_flight: Arc::new(Mutex::new(HashSet::new())),
        cover_slots: Arc::new((Mutex::new(0), Condvar::new())),
        _test_permit: test_permit,
    };
    (controller, receiver)
}
pub(in crate::controller) fn restored_track() -> Track {
    Track {
        id: TrackId::new("jellyfin:track:lyrics"),
        album_id: AlbumId::fake(1),
        title: "Restored Track".to_string(),
        artist: "Artist".to_string(),
        artist_id: Some(ArtistId::fake(1)),
        artist_credits: Vec::new(),
        album_artist_credits: Vec::new(),
        album: "Album".to_string(),
        year: 2026,
        release_date: None,
        date_added: None,
        last_played: None,
        play_count: None,
        user_rating: None,
        duration_seconds: 180,
        favorite: false,
        disc_number: 1,
        track_number: 1,
        image_ref: None,
        genres: Vec::new(),
        local_path: None,
        source_format: None,
    }
}
pub(in crate::controller) fn saved_server() -> SavedServer {
    SavedServer {
        server: ServerIdentity {
            id: ServerId::new("jellyfin:server:test"),
            provider: "jellyfin".to_string(),
            name: "Test Server".to_string(),
            base_url: "https://music.example".to_string(),
        },
        user_id: "user".to_string(),
        username: "demo".to_string(),
        trust_invalid_cert: false,
    }
}
#[test]
pub(in crate::controller) fn grouped_cover_refs_keep_one_unique_cover_full_size() {
    let cover = test_image_ref(1);
    let albums = vec![library_album(
        1,
        "Example Artist",
        "Example Album",
        Some(cover.clone()),
    )];
    let refs = super::grouped_cover_refs_for_items(&albums, &[]);
    assert_eq!(refs, vec![cover]);
}
#[test]
pub(in crate::controller) fn grouped_cover_refs_deduplicate_and_limit_to_four() {
    let first = test_image_ref(1);
    let second = test_image_ref(2);
    let third = test_image_ref(3);
    let fourth = test_image_ref(4);
    let fifth = test_image_ref(5);
    let albums = vec![
        library_album(1, "Example Artist", "First", Some(first.clone())),
        library_album(2, "Example Artist", "Duplicate", Some(first.clone())),
        library_album(3, "Example Artist", "Second", Some(second.clone())),
    ];
    let mut tracks = vec![
        library_track(1, None, AlbumId::fake(1), "Example Artist", &[]),
        library_track(2, None, AlbumId::fake(2), "Example Artist", &[]),
        library_track(3, None, AlbumId::fake(3), "Example Artist", &[]),
    ];
    tracks[0].image_ref = Some(third.clone());
    tracks[1].image_ref = Some(fourth.clone());
    tracks[2].image_ref = Some(fifth);
    let refs = super::grouped_cover_refs_for_items(&albums, &tracks);
    assert_eq!(refs, vec![first, second, third, fourth]);
}
#[test]
pub(in crate::controller) fn artist_detail_fallback_uses_external_album_image_after_normalization()
{
    let mut detail = CachedArtistDetail {
        artist: rufin_core::Artist {
            id: ArtistId::fake(1),
            name: "Example Artist".to_string(),
            album_count: 1,
            track_count: 0,
            favorite: false,
            last_played: None,
            play_count: None,
            user_rating: None,
            image_ref: None,
        },
        albums: vec![library_album(1, "Example Artist", "Example Album", None)],
        appears_on: Vec::new(),
        tracks: Vec::new(),
    };
    let settings = AppSettings {
        external_metadata_enabled: true,
        ..AppSettings::default()
    };
    super::normalize_artist_detail_image_refs(&mut detail, &settings);
    let image_ref = detail.artist.image_ref.expect("artist fallback image ref");
    assert!(image_ref.item_id.starts_with("external:album:"));
    assert!(
        image_ref
            .item_id
            .contains("Example%20Artist:Example%20Album")
    );
}
#[test]
pub(in crate::controller) fn artist_collection_fallback_uses_external_album_image_after_normalization()
 {
    let artist_id = ArtistId::fake(1);
    let mut artists = vec![rufin_core::Artist {
        id: artist_id.clone(),
        name: "Example Artist".to_string(),
        album_count: 1,
        track_count: 1,
        favorite: false,
        last_played: None,
        play_count: None,
        user_rating: None,
        image_ref: None,
    }];
    let fallback_albums = std::collections::HashMap::from([(
        artist_id,
        library_album(1, "Example Artist", "Example Album", None),
    )]);
    let settings = AppSettings {
        external_metadata_enabled: true,
        ..AppSettings::default()
    };

    super::apply_artist_album_fallback_image_refs(&mut artists, fallback_albums, &settings);

    let image_ref = artists[0]
        .image_ref
        .as_ref()
        .expect("artist fallback image ref");
    assert!(image_ref.item_id.starts_with("external:album:"));
    assert!(
        image_ref
            .item_id
            .contains("Example%20Artist:Example%20Album")
    );
}
pub(in crate::controller) fn unique_test_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "rufin-{label}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ))
}
pub(in crate::controller) fn test_image_ref(number: u32) -> ImageRef {
    ImageRef::new(
        format!("jellyfin:album:{number}"),
        Some(format!("tag-{number}")),
    )
}
pub(in crate::controller) fn library_album(
    number: u32,
    artist: &str,
    title: &str,
    image_ref: Option<ImageRef>,
) -> Album {
    Album {
        id: AlbumId::fake(number),
        title: title.to_string(),
        artist: artist.to_string(),
        artist_id: Some(ArtistId::fake(number)),
        album_artist_credits: Vec::new(),
        artist_credits: Vec::new(),
        year: 2026,
        release_date: None,
        date_added: None,
        last_played: None,
        play_count: None,
        user_rating: None,
        track_count: 1,
        duration_seconds: 180,
        favorite: false,
        color_seed: number,
        image_ref,
        genres: Vec::new(),
    }
}
