use super::*;

pub(in crate::controller) fn library_track(
    number: u32,
    artist_id: Option<ArtistId>,
    album_id: AlbumId,
    artist: &str,
    genres: &[&str],
) -> Track {
    Track {
        id: TrackId::fake(number),
        album_id,
        title: format!("Track {number}"),
        artist: artist.to_string(),
        artist_id,
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
        track_number: number as u16,
        image_ref: None,
        genres: genres.iter().map(|genre| genre.to_string()).collect(),
        musicbrainz_recording_id: None,
        musicbrainz_release_track_id: None,
        local_path: None,
        source_format: None,
        comment: None,
        skip_count: None,
    }
}
pub(in crate::controller) fn wait_for_snapshot(
    events: &Receiver<ControllerEvent>,
) -> LibrarySnapshot {
    loop {
        match events
            .recv_timeout(Duration::from_secs(5))
            .expect("controller event")
        {
            ControllerEvent::Snapshot(snapshot)
            | ControllerEvent::HomeSectionsUpdated { snapshot, .. }
            | ControllerEvent::PlaylistChanged { snapshot, .. }
            | ControllerEvent::SmartPlaylistChanged { snapshot, .. } => return *snapshot,
            ControllerEvent::LibrarySyncStatus(_)
            | ControllerEvent::LibraryDelta(_)
            | ControllerEvent::Queue(_)
            | ControllerEvent::FavoriteChanged { .. }
            | ControllerEvent::Playback(_)
            | ControllerEvent::Visualizer(_)
            | ControllerEvent::Lyrics(_)
            | ControllerEvent::LyricsSearchResults { .. }
            | ControllerEvent::LyricsSearchFailed { .. }
            | ControllerEvent::LyricsSaved { .. }
            | ControllerEvent::FolderLoaded { .. }
            | ControllerEvent::FolderLoadFailed { .. }
            | ControllerEvent::HomeSectionPrefetched { .. }
            | ControllerEvent::ServerDiscovery { .. }
            | ControllerEvent::CoverReady { .. }
            | ControllerEvent::CoverUnavailable { .. }
            | ControllerEvent::CoverDeferred { .. } => {}
            ControllerEvent::LoginStatus(_) => {}
            ControllerEvent::Error(error) => panic!("controller error: {error}"),
        }
    }
}
pub(in crate::controller) fn wait_for_favorite_changed(
    events: &Receiver<ControllerEvent>,
) -> (FavoriteItemId, bool, LibrarySnapshot) {
    loop {
        match events
            .recv_timeout(Duration::from_secs(5))
            .expect("controller event")
        {
            ControllerEvent::FavoriteChanged {
                item_id,
                favorite,
                snapshot,
            } => return (item_id, favorite, *snapshot),
            ControllerEvent::Snapshot(_)
            | ControllerEvent::LibrarySyncStatus(_)
            | ControllerEvent::LibraryDelta(_)
            | ControllerEvent::HomeSectionsUpdated { .. }
            | ControllerEvent::PlaylistChanged { .. }
            | ControllerEvent::SmartPlaylistChanged { .. }
            | ControllerEvent::Queue(_)
            | ControllerEvent::Playback(_)
            | ControllerEvent::Visualizer(_)
            | ControllerEvent::Lyrics(_)
            | ControllerEvent::LyricsSearchResults { .. }
            | ControllerEvent::LyricsSearchFailed { .. }
            | ControllerEvent::LyricsSaved { .. }
            | ControllerEvent::FolderLoaded { .. }
            | ControllerEvent::FolderLoadFailed { .. }
            | ControllerEvent::HomeSectionPrefetched { .. }
            | ControllerEvent::ServerDiscovery { .. }
            | ControllerEvent::CoverReady { .. }
            | ControllerEvent::CoverUnavailable { .. }
            | ControllerEvent::CoverDeferred { .. } => {}
            ControllerEvent::LoginStatus(_) => {}
            ControllerEvent::Error(error) => panic!("controller error: {error}"),
        }
    }
}
pub(in crate::controller) fn wait_for_playlist_changed(
    events: &Receiver<ControllerEvent>,
) -> (PlaylistId, LibrarySnapshot) {
    loop {
        match events
            .recv_timeout(Duration::from_secs(5))
            .expect("controller event")
        {
            ControllerEvent::PlaylistChanged {
                playlist_id,
                snapshot,
            } => return (playlist_id, *snapshot),
            ControllerEvent::Snapshot(_)
            | ControllerEvent::LibrarySyncStatus(_)
            | ControllerEvent::LibraryDelta(_)
            | ControllerEvent::HomeSectionsUpdated { .. }
            | ControllerEvent::SmartPlaylistChanged { .. }
            | ControllerEvent::FavoriteChanged { .. }
            | ControllerEvent::Queue(_)
            | ControllerEvent::Playback(_)
            | ControllerEvent::Visualizer(_)
            | ControllerEvent::Lyrics(_)
            | ControllerEvent::LyricsSearchResults { .. }
            | ControllerEvent::LyricsSearchFailed { .. }
            | ControllerEvent::LyricsSaved { .. }
            | ControllerEvent::FolderLoaded { .. }
            | ControllerEvent::FolderLoadFailed { .. }
            | ControllerEvent::HomeSectionPrefetched { .. }
            | ControllerEvent::ServerDiscovery { .. }
            | ControllerEvent::CoverReady { .. }
            | ControllerEvent::CoverUnavailable { .. }
            | ControllerEvent::CoverDeferred { .. } => {}
            ControllerEvent::LoginStatus(_) => {}
            ControllerEvent::Error(error) => panic!("controller error: {error}"),
        }
    }
}
pub(in crate::controller) fn wait_for_status(events: &Receiver<ControllerEvent>) -> String {
    loop {
        match events
            .recv_timeout(Duration::from_secs(5))
            .expect("controller event")
        {
            ControllerEvent::LoginStatus(status) => return status,
            ControllerEvent::Snapshot(_)
            | ControllerEvent::LibrarySyncStatus(_)
            | ControllerEvent::LibraryDelta(_)
            | ControllerEvent::HomeSectionsUpdated { .. }
            | ControllerEvent::PlaylistChanged { .. }
            | ControllerEvent::SmartPlaylistChanged { .. }
            | ControllerEvent::FavoriteChanged { .. }
            | ControllerEvent::Queue(_)
            | ControllerEvent::Playback(_)
            | ControllerEvent::Visualizer(_)
            | ControllerEvent::Lyrics(_)
            | ControllerEvent::LyricsSearchResults { .. }
            | ControllerEvent::LyricsSearchFailed { .. }
            | ControllerEvent::LyricsSaved { .. }
            | ControllerEvent::FolderLoaded { .. }
            | ControllerEvent::FolderLoadFailed { .. }
            | ControllerEvent::HomeSectionPrefetched { .. }
            | ControllerEvent::ServerDiscovery { .. }
            | ControllerEvent::CoverReady { .. }
            | ControllerEvent::CoverUnavailable { .. }
            | ControllerEvent::CoverDeferred { .. } => {}
            ControllerEvent::Error(error) => panic!("controller error: {error}"),
        }
    }
}
pub(in crate::controller) fn wait_for_queue(
    events: &Receiver<ControllerEvent>,
) -> Option<rufin_core::QueueSnapshot> {
    loop {
        match events
            .recv_timeout(Duration::from_secs(5))
            .expect("controller event")
        {
            ControllerEvent::Queue(queue) => return *queue,
            ControllerEvent::Snapshot(_)
            | ControllerEvent::LibrarySyncStatus(_)
            | ControllerEvent::LibraryDelta(_)
            | ControllerEvent::HomeSectionsUpdated { .. }
            | ControllerEvent::PlaylistChanged { .. }
            | ControllerEvent::SmartPlaylistChanged { .. }
            | ControllerEvent::FavoriteChanged { .. }
            | ControllerEvent::Playback(_)
            | ControllerEvent::Visualizer(_)
            | ControllerEvent::LoginStatus(_)
            | ControllerEvent::Lyrics(_)
            | ControllerEvent::LyricsSearchResults { .. }
            | ControllerEvent::LyricsSearchFailed { .. }
            | ControllerEvent::LyricsSaved { .. }
            | ControllerEvent::FolderLoaded { .. }
            | ControllerEvent::FolderLoadFailed { .. }
            | ControllerEvent::HomeSectionPrefetched { .. }
            | ControllerEvent::ServerDiscovery { .. }
            | ControllerEvent::CoverReady { .. }
            | ControllerEvent::CoverUnavailable { .. }
            | ControllerEvent::CoverDeferred { .. } => {}
            ControllerEvent::Error(error) => panic!("controller error: {error}"),
        }
    }
}
pub(in crate::controller) fn random_request(
    action: RandomPlayAction,
    limit: usize,
) -> RandomPlayRequest {
    RandomPlayRequest {
        action,
        limit,
        min_year: None,
        max_year: None,
        genre_id: None,
        genre_name: None,
        played_filter: PlayedFilter::All,
    }
}
pub(in crate::controller) fn random_track_ids(tracks: &[Track], limit: usize) -> Vec<TrackId> {
    let mut ids = tracks
        .iter()
        .map(|track| track.id.clone())
        .collect::<Vec<_>>();
    ids.sort_by_key(|id| id.as_str().to_string());
    ids.truncate(limit);
    ids
}
pub(in crate::controller) fn wait_for_cover_ready(
    events: &Receiver<ControllerEvent>,
    expected_key: &str,
) -> PathBuf {
    loop {
        match events
            .recv_timeout(Duration::from_secs(5))
            .expect("controller event")
        {
            ControllerEvent::CoverReady { key, path } if key == expected_key => return path,
            ControllerEvent::Snapshot(_)
            | ControllerEvent::LibrarySyncStatus(_)
            | ControllerEvent::LibraryDelta(_)
            | ControllerEvent::HomeSectionsUpdated { .. }
            | ControllerEvent::PlaylistChanged { .. }
            | ControllerEvent::SmartPlaylistChanged { .. }
            | ControllerEvent::FavoriteChanged { .. }
            | ControllerEvent::Queue(_)
            | ControllerEvent::Playback(_)
            | ControllerEvent::Visualizer(_)
            | ControllerEvent::LoginStatus(_)
            | ControllerEvent::Lyrics(_)
            | ControllerEvent::LyricsSearchResults { .. }
            | ControllerEvent::LyricsSearchFailed { .. }
            | ControllerEvent::LyricsSaved { .. }
            | ControllerEvent::FolderLoaded { .. }
            | ControllerEvent::FolderLoadFailed { .. }
            | ControllerEvent::HomeSectionPrefetched { .. }
            | ControllerEvent::ServerDiscovery { .. }
            | ControllerEvent::CoverReady { .. }
            | ControllerEvent::CoverUnavailable { .. }
            | ControllerEvent::CoverDeferred { .. } => {}
            ControllerEvent::Error(error) => panic!("controller error: {error}"),
        }
    }
}
pub(in crate::controller) fn wait_for_lyrics(
    events: &Receiver<ControllerEvent>,
) -> Option<rufin_provider::Lyrics> {
    loop {
        match events
            .recv_timeout(Duration::from_secs(5))
            .expect("controller event")
        {
            ControllerEvent::Lyrics(lyrics) => return *lyrics,
            ControllerEvent::Snapshot(_)
            | ControllerEvent::LibrarySyncStatus(_)
            | ControllerEvent::LibraryDelta(_)
            | ControllerEvent::HomeSectionsUpdated { .. }
            | ControllerEvent::PlaylistChanged { .. }
            | ControllerEvent::SmartPlaylistChanged { .. }
            | ControllerEvent::FavoriteChanged { .. }
            | ControllerEvent::Queue(_)
            | ControllerEvent::Playback(_)
            | ControllerEvent::Visualizer(_)
            | ControllerEvent::LoginStatus(_)
            | ControllerEvent::LyricsSearchResults { .. }
            | ControllerEvent::LyricsSearchFailed { .. }
            | ControllerEvent::LyricsSaved { .. }
            | ControllerEvent::FolderLoaded { .. }
            | ControllerEvent::FolderLoadFailed { .. }
            | ControllerEvent::HomeSectionPrefetched { .. }
            | ControllerEvent::ServerDiscovery { .. }
            | ControllerEvent::CoverReady { .. }
            | ControllerEvent::CoverUnavailable { .. }
            | ControllerEvent::CoverDeferred { .. } => {}
            ControllerEvent::Error(error) => panic!("controller error: {error}"),
        }
    }
}
pub(in crate::controller) fn wait_for_recorded_command(
    commands: &Arc<Mutex<Vec<PlaybackCommand>>>,
    predicate: impl Fn(&PlaybackCommand) -> bool,
) -> PlaybackCommand {
    for _ in 0..50 {
        if let Some(command) = commands
            .lock()
            .expect("commands")
            .iter()
            .find(|command| predicate(command))
            .cloned()
        {
            return command;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for playback command");
}
pub(in crate::controller) fn wait_for_playback_state(
    controller: &AppController,
    events: &Receiver<ControllerEvent>,
    state: PlaybackState,
) -> super::PlaybackSnapshot {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for playback state"
        );
        controller.poll_playback_events();
        match events.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => match event {
                ControllerEvent::Playback(playback) if playback.state == state => {
                    return *playback;
                }
                ControllerEvent::Playback(_)
                | ControllerEvent::Visualizer(_)
                | ControllerEvent::Queue(_)
                | ControllerEvent::Lyrics(_)
                | ControllerEvent::LyricsSearchResults { .. }
                | ControllerEvent::LyricsSearchFailed { .. }
                | ControllerEvent::LyricsSaved { .. }
                | ControllerEvent::FolderLoaded { .. }
                | ControllerEvent::FolderLoadFailed { .. }
                | ControllerEvent::HomeSectionPrefetched { .. }
                | ControllerEvent::ServerDiscovery { .. }
                | ControllerEvent::CoverReady { .. }
                | ControllerEvent::CoverUnavailable { .. }
                | ControllerEvent::CoverDeferred { .. } => {}
                ControllerEvent::Snapshot(_)
                | ControllerEvent::LibrarySyncStatus(_)
                | ControllerEvent::LibraryDelta(_)
                | ControllerEvent::HomeSectionsUpdated { .. }
                | ControllerEvent::PlaylistChanged { .. }
                | ControllerEvent::SmartPlaylistChanged { .. }
                | ControllerEvent::FavoriteChanged { .. }
                | ControllerEvent::LoginStatus(_) => {}
                ControllerEvent::Error(error) => panic!("controller error: {error}"),
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("controller event channel closed")
            }
        }
    }
}
pub(in crate::controller) fn wait_for_playback_track_position(
    controller: &AppController,
    events: &Receiver<ControllerEvent>,
    track_id: &TrackId,
    position_millis: u64,
) -> super::PlaybackSnapshot {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for playback track position"
        );
        controller.poll_playback_events();
        match events.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => match event {
                ControllerEvent::Playback(playback)
                    if playback.position_millis == position_millis
                        && playback
                            .current
                            .as_ref()
                            .is_some_and(|entry| &entry.track_id == track_id) =>
                {
                    return *playback;
                }
                ControllerEvent::Playback(_)
                | ControllerEvent::Visualizer(_)
                | ControllerEvent::Queue(_)
                | ControllerEvent::Lyrics(_)
                | ControllerEvent::LyricsSearchResults { .. }
                | ControllerEvent::LyricsSearchFailed { .. }
                | ControllerEvent::LyricsSaved { .. }
                | ControllerEvent::FolderLoaded { .. }
                | ControllerEvent::FolderLoadFailed { .. }
                | ControllerEvent::HomeSectionPrefetched { .. }
                | ControllerEvent::ServerDiscovery { .. }
                | ControllerEvent::CoverReady { .. }
                | ControllerEvent::CoverUnavailable { .. }
                | ControllerEvent::CoverDeferred { .. } => {}
                ControllerEvent::Snapshot(_)
                | ControllerEvent::LibrarySyncStatus(_)
                | ControllerEvent::LibraryDelta(_)
                | ControllerEvent::HomeSectionsUpdated { .. }
                | ControllerEvent::PlaylistChanged { .. }
                | ControllerEvent::SmartPlaylistChanged { .. }
                | ControllerEvent::FavoriteChanged { .. }
                | ControllerEvent::LoginStatus(_) => {}
                ControllerEvent::Error(error) => panic!("controller error: {error}"),
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("controller event channel closed")
            }
        }
    }
}
pub(in crate::controller) fn wait_for_playback_auto_dj(
    events: &Receiver<ControllerEvent>,
    enabled: bool,
) -> super::PlaybackSnapshot {
    loop {
        match events
            .recv_timeout(Duration::from_secs(5))
            .expect("controller event")
        {
            ControllerEvent::Playback(playback) if playback.auto_dj_enabled == enabled => {
                return *playback;
            }
            ControllerEvent::Playback(_)
            | ControllerEvent::Visualizer(_)
            | ControllerEvent::Queue(_)
            | ControllerEvent::Lyrics(_)
            | ControllerEvent::LyricsSearchResults { .. }
            | ControllerEvent::LyricsSearchFailed { .. }
            | ControllerEvent::LyricsSaved { .. }
            | ControllerEvent::FolderLoaded { .. }
            | ControllerEvent::FolderLoadFailed { .. }
            | ControllerEvent::HomeSectionPrefetched { .. }
            | ControllerEvent::ServerDiscovery { .. }
            | ControllerEvent::CoverReady { .. }
            | ControllerEvent::CoverUnavailable { .. }
            | ControllerEvent::CoverDeferred { .. } => {}
            ControllerEvent::Snapshot(_)
            | ControllerEvent::LibrarySyncStatus(_)
            | ControllerEvent::LibraryDelta(_)
            | ControllerEvent::HomeSectionsUpdated { .. }
            | ControllerEvent::PlaylistChanged { .. }
            | ControllerEvent::SmartPlaylistChanged { .. }
            | ControllerEvent::FavoriteChanged { .. }
            | ControllerEvent::LoginStatus(_) => {}
            ControllerEvent::Error(error) => panic!("controller error: {error}"),
        }
    }
}
pub(in crate::controller) fn wait_for_playback_repeat(
    events: &Receiver<ControllerEvent>,
    repeat_mode: RepeatMode,
) -> super::PlaybackSnapshot {
    loop {
        match events
            .recv_timeout(Duration::from_secs(5))
            .expect("controller event")
        {
            ControllerEvent::Playback(playback) if playback.repeat_mode == repeat_mode => {
                return *playback;
            }
            ControllerEvent::Playback(_)
            | ControllerEvent::Visualizer(_)
            | ControllerEvent::Queue(_)
            | ControllerEvent::Lyrics(_)
            | ControllerEvent::LyricsSearchResults { .. }
            | ControllerEvent::LyricsSearchFailed { .. }
            | ControllerEvent::LyricsSaved { .. }
            | ControllerEvent::FolderLoaded { .. }
            | ControllerEvent::FolderLoadFailed { .. }
            | ControllerEvent::HomeSectionPrefetched { .. }
            | ControllerEvent::ServerDiscovery { .. }
            | ControllerEvent::CoverReady { .. }
            | ControllerEvent::CoverUnavailable { .. }
            | ControllerEvent::CoverDeferred { .. } => {}
            ControllerEvent::Snapshot(_)
            | ControllerEvent::LibrarySyncStatus(_)
            | ControllerEvent::LibraryDelta(_)
            | ControllerEvent::HomeSectionsUpdated { .. }
            | ControllerEvent::PlaylistChanged { .. }
            | ControllerEvent::SmartPlaylistChanged { .. }
            | ControllerEvent::FavoriteChanged { .. }
            | ControllerEvent::LoginStatus(_) => {}
            ControllerEvent::Error(error) => panic!("controller error: {error}"),
        }
    }
}
pub(in crate::controller) fn wait_for_playback_current_favorite(
    controller: &AppController,
    events: &Receiver<ControllerEvent>,
    favorite: bool,
) -> super::PlaybackSnapshot {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for playback favorite"
        );
        controller.poll_playback_events();
        match events.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => match event {
                ControllerEvent::Playback(playback)
                    if playback
                        .current
                        .as_ref()
                        .is_some_and(|entry| entry.favorite == favorite) =>
                {
                    return *playback;
                }
                ControllerEvent::Playback(_)
                | ControllerEvent::Visualizer(_)
                | ControllerEvent::Queue(_)
                | ControllerEvent::Lyrics(_)
                | ControllerEvent::LyricsSearchResults { .. }
                | ControllerEvent::LyricsSearchFailed { .. }
                | ControllerEvent::LyricsSaved { .. }
                | ControllerEvent::FolderLoaded { .. }
                | ControllerEvent::FolderLoadFailed { .. }
                | ControllerEvent::HomeSectionPrefetched { .. }
                | ControllerEvent::ServerDiscovery { .. }
                | ControllerEvent::CoverReady { .. }
                | ControllerEvent::CoverUnavailable { .. }
                | ControllerEvent::CoverDeferred { .. } => {}
                ControllerEvent::Snapshot(_)
                | ControllerEvent::LibrarySyncStatus(_)
                | ControllerEvent::LibraryDelta(_)
                | ControllerEvent::HomeSectionsUpdated { .. }
                | ControllerEvent::PlaylistChanged { .. }
                | ControllerEvent::SmartPlaylistChanged { .. }
                | ControllerEvent::FavoriteChanged { .. }
                | ControllerEvent::LoginStatus(_) => {}
                ControllerEvent::Error(error) => panic!("controller error: {error}"),
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("controller event channel closed")
            }
        }
    }
}
pub(in crate::controller) fn assert_playlist_order(
    controller: &AppController,
    playlist_id: &PlaylistId,
    ids: &[&str],
) {
    let detail = controller
        .cached_playlist_detail(playlist_id)
        .expect("playlist detail")
        .expect("playlist detail");
    assert_eq!(
        detail
            .entries
            .iter()
            .map(|entry| entry.track.id.as_str())
            .collect::<Vec<_>>(),
        ids
    );
}
