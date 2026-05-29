use super::*;

pub(in crate::controller) fn promote_prefetched_home_section(
    store: &StoreHandle,
    server_id: &ServerId,
    section: &HomeSection,
) -> Result<(), String> {
    let generation =
        store.with_store(|store| store.sync_state(server_id).map(|state| state.generation))?;
    cache_home_section(store, server_id, section, generation)?;
    store.with_store(|store| store.clear_home_section_prefetch(server_id, section.kind))?;
    Ok(())
}
pub(in crate::controller) fn cache_home_sections(
    store: &StoreHandle,
    server_id: &ServerId,
    sections: &[HomeSection],
    generation: i64,
) -> Result<(), String> {
    for section in sections {
        cache_home_section_items(store, server_id, section, generation)?;
    }
    store.with_store(|store| store.upsert_home_sections(server_id, sections, generation))?;
    Ok(())
}
pub(in crate::controller) fn cache_home_section(
    store: &StoreHandle,
    server_id: &ServerId,
    section: &HomeSection,
    generation: i64,
) -> Result<(), String> {
    cache_home_section_items(store, server_id, section, generation)?;
    store.with_store(|store| store.upsert_home_section(server_id, section, generation))?;
    Ok(())
}
pub(in crate::controller) fn cache_home_section_items(
    store: &StoreHandle,
    server_id: &ServerId,
    section: &HomeSection,
    generation: i64,
) -> Result<(), String> {
    if !section.albums.is_empty() {
        store.with_store(|store| store.upsert_albums(server_id, &section.albums, generation))?;
    }
    if !section.tracks.is_empty() {
        store.with_store(|store| store.upsert_tracks(server_id, &section.tracks, generation))?;
    }
    Ok(())
}
pub(in crate::controller) fn sync_page_finished(
    item_count: usize,
    total: usize,
    offset: usize,
) -> bool {
    item_count == 0 || (total > 0 && offset >= total) || (total == 0 && item_count < PAGE_SIZE)
}
#[cfg(test)]
pub(in crate::controller) fn home_refresh_section_kinds() -> [HomeSectionKind; 5] {
    [
        HomeSectionKind::Explore,
        HomeSectionKind::MostPlayed,
        HomeSectionKind::NewlyAdded,
        HomeSectionKind::RecentlyPlayed,
        HomeSectionKind::RecentlyReleased,
    ]
}
pub(in crate::controller) fn load_snapshot(store: &StoreHandle) -> Result<LibrarySnapshot, String> {
    let source_settings = load_settings_from_store(store);
    let saved_servers = store.with_store(|store| store.list_servers())?;
    let remote_saved_servers = saved_servers
        .iter()
        .filter(|saved| saved.server.provider != LOCAL_PROVIDER_ID)
        .cloned()
        .collect::<Vec<_>>();
    let servers = remote_saved_servers
        .iter()
        .map(|saved| saved.server.clone())
        .collect::<Vec<_>>();
    let server_local_access = remote_saved_servers
        .iter()
        .map(|saved| {
            let access = store.with_store(|store| store.server_local_access(&saved.server.id))?;
            let status = local_access_status_for_server(store, &saved.server, access.as_ref())?;
            let sync_state = store
                .with_store(|store| store.sync_state(&saved.server.id))
                .ok();
            let sync_status = sync_state
                .as_ref()
                .map(sync_status_text)
                .unwrap_or_else(|| "Cached library ready".to_string());
            let cached_album_count = store
                .with_store(|store| {
                    store
                        .load_albums(&saved.server.id, 0, 1)
                        .map(|page| page.total)
                })
                .unwrap_or_default();
            let cached_track_count = store
                .with_store(|store| {
                    store
                        .load_tracks(&saved.server.id, 0, 1)
                        .map(|page| page.total)
                })
                .unwrap_or_default();
            let selected_music_folder_name = store
                .with_store(|store| {
                    let selected = store.selected_music_folder_id(&saved.server.id)?;
                    let folders = store.list_music_folders(&saved.server.id)?;
                    Ok(selected.and_then(|selected| {
                        folders
                            .into_iter()
                            .find(|folder| folder.id == selected)
                            .map(|folder| folder.name)
                    }))
                })
                .unwrap_or_default();
            Ok(ServerLocalAccessSnapshot {
                server_id: saved.server.id.clone(),
                access,
                status,
                selected_music_folder_name,
                username: Some(saved.username.clone()),
                trust_invalid_cert: saved.trust_invalid_cert,
                sync_status,
                cached_album_count,
                cached_track_count,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let Some(reconciled_source) =
        reconcile_snapshot_source(store, &source_settings, &remote_saved_servers)?
    else {
        let mut snapshot = LibrarySnapshot::first_run();
        snapshot.servers = servers;
        snapshot.local_folders = source_settings.sources.local_folders.clone();
        snapshot.server_local_access = server_local_access;
        return Ok(snapshot);
    };
    let SnapshotSourceReconciliation {
        selected_source,
        saved,
    } = reconciled_source;
    let local_access = store.with_store(|store| store.server_local_access(&saved.server.id))?;
    let local_access_status =
        local_access_status_for_server(store, &saved.server, local_access.as_ref())?;
    let music_folders = store.with_store(|store| store.list_music_folders(&saved.server.id))?;
    let selected_music_folder_id =
        store.with_store(|store| store.selected_music_folder_id(&saved.server.id))?;
    let metadata_settings = load_settings_for_saved(store, &saved);
    let sync_state = store
        .with_store(|store| store.sync_state(&saved.server.id))
        .ok();
    let mut home_sections = store.with_store(|store| store.load_home_sections(&saved.server.id))?;
    let mut prefetched_explore = store.with_store(|store| {
        store.load_home_section_prefetch(&saved.server.id, HomeSectionKind::Explore)
    })?;
    let album_page =
        store.with_store(|store| store.load_albums(&saved.server.id, 0, SNAPSHOT_GRID_LIMIT))?;
    let track_page =
        store.with_store(|store| store.load_tracks(&saved.server.id, 0, SNAPSHOT_TRACK_LIMIT))?;
    let cached_album_count = album_page.total;
    let cached_track_count = track_page.total;
    let mut albums = album_page.items;
    let mut tracks = track_page.items;
    let artist_page = store
        .with_store(|store| store.load_artists(&saved.server.id, false, 0, SNAPSHOT_GRID_LIMIT))?;
    let album_artist_page = store
        .with_store(|store| store.load_artists(&saved.server.id, true, 0, SNAPSHOT_GRID_LIMIT))?;
    let genre_page =
        store.with_store(|store| store.load_genres(&saved.server.id, 0, SNAPSHOT_GRID_LIMIT))?;
    let playlist_page =
        store.with_store(|store| store.load_playlists(&saved.server.id, 0, SNAPSHOT_GRID_LIMIT))?;
    let cached_artist_count = artist_page.total;
    let cached_album_artist_count = album_artist_page.total;
    let cached_genre_count = genre_page.total;
    let cached_playlist_count = playlist_page.total;
    let mut artists = artist_page.items;
    let mut album_artists = album_artist_page.items;
    let genres = genre_page.items;
    let playlists = playlist_page.items;
    let mut favorites = store.with_store(|store| store.load_favorite_tracks(&saved.server.id))?;
    external_metadata::normalize_home_sections(&mut home_sections, &metadata_settings);
    if let Some(section) = &mut prefetched_explore {
        external_metadata::normalize_home_section(section, &metadata_settings);
    }
    external_metadata::normalize_albums(&mut albums, &metadata_settings);
    external_metadata::normalize_tracks(&mut tracks, &metadata_settings);
    normalize_artist_collection_image_refs(store, &saved, &mut artists, false, &metadata_settings)?;
    normalize_artist_collection_image_refs(
        store,
        &saved,
        &mut album_artists,
        true,
        &metadata_settings,
    )?;
    external_metadata::normalize_tracks(&mut favorites, &metadata_settings);
    let status = sync_state
        .as_ref()
        .map(sync_status_text)
        .unwrap_or_else(|| "Cached library ready".to_string());
    let last_error = sync_state.and_then(|state| state.last_error);

    Ok(LibrarySnapshot {
        server: Some(saved.server),
        servers,
        selected_source: Some(selected_source),
        local_folders: source_settings.sources.local_folders,
        server_local_access,
        local_access,
        local_access_status,
        music_folders,
        selected_music_folder_id,
        username: Some(saved.username),
        first_run: false,
        sync_status: status,
        last_error,
        cached_album_count,
        cached_track_count,
        cached_artist_count,
        cached_album_artist_count,
        cached_genre_count,
        cached_playlist_count,
        home_sections,
        prefetched_explore,
        albums,
        tracks,
        artists,
        album_artists,
        genres,
        playlists,
        favorites,
        search: SearchResults::default(),
    })
}
pub(crate) fn grouped_cover_refs_for_items(albums: &[Album], tracks: &[Track]) -> Vec<ImageRef> {
    let mut image_refs = Vec::new();
    for album in albums {
        push_unique_cover_ref(&mut image_refs, album.image_ref.as_ref());
    }
    for track in tracks {
        push_unique_cover_ref(&mut image_refs, track.image_ref.as_ref());
    }
    image_refs
}
pub(crate) fn track_cover_refs_for_items(tracks: &[Track]) -> Vec<ImageRef> {
    let mut image_refs = Vec::new();
    for track in tracks {
        push_unique_cover_ref(&mut image_refs, track.image_ref.as_ref());
    }
    image_refs
}
pub(in crate::controller) fn normalize_artist_detail_image_refs(
    detail: &mut CachedArtistDetail,
    settings: &AppSettings,
) {
    external_metadata::normalize_artist(&mut detail.artist, settings);
    external_metadata::normalize_albums(&mut detail.albums, settings);
    external_metadata::normalize_albums(&mut detail.appears_on, settings);
    external_metadata::normalize_tracks(&mut detail.tracks, settings);
    if detail.artist.image_ref.is_none() {
        detail.artist.image_ref =
            artist_fallback_image_ref(&detail.albums, &detail.appears_on, &detail.tracks);
    }
    external_metadata::normalize_artist(&mut detail.artist, settings);
}
pub(in crate::controller) fn normalize_artist_collection_image_refs(
    store: &StoreHandle,
    saved: &SavedServer,
    artists: &mut [Artist],
    album_artist: bool,
    settings: &AppSettings,
) -> Result<(), String> {
    external_metadata::normalize_artists(artists, settings);
    let missing_artist_ids = artists
        .iter()
        .filter(|artist| artist.image_ref.is_none())
        .map(|artist| artist.id.clone())
        .collect::<Vec<_>>();
    if missing_artist_ids.is_empty() {
        return Ok(());
    }

    let fallback_albums = store.with_store(|store| {
        store.load_artist_fallback_albums(&saved.server.id, album_artist, &missing_artist_ids)
    })?;
    apply_artist_album_fallback_image_refs(artists, fallback_albums, settings);
    Ok(())
}
pub(in crate::controller) fn apply_artist_album_fallback_image_refs(
    artists: &mut [Artist],
    mut fallback_albums: HashMap<ArtistId, Album>,
    settings: &AppSettings,
) {
    for artist in artists {
        if artist.image_ref.is_some() {
            continue;
        }
        let Some(mut album) = fallback_albums.remove(&artist.id) else {
            continue;
        };
        external_metadata::normalize_album(&mut album, settings);
        artist.image_ref = album.image_ref;
        external_metadata::normalize_artist(artist, settings);
    }
}
pub(in crate::controller) fn artist_fallback_image_ref(
    albums: &[Album],
    appears_on: &[Album],
    tracks: &[Track],
) -> Option<ImageRef> {
    albums
        .iter()
        .chain(appears_on.iter())
        .filter_map(|album| album.image_ref.clone())
        .next()
        .or_else(|| tracks.iter().find_map(|track| track.image_ref.clone()))
}
pub(in crate::controller) fn push_unique_cover_ref(
    image_refs: &mut Vec<ImageRef>,
    image_ref: Option<&ImageRef>,
) {
    if image_refs.len() >= GROUPED_COVER_REF_LIMIT {
        return;
    }
    let Some(image_ref) = image_ref else {
        return;
    };
    if !image_refs.iter().any(|existing| existing == image_ref) {
        image_refs.push(image_ref.clone());
    }
}
pub(in crate::controller) fn sync_status_text(state: &SyncState) -> String {
    match state.status.as_str() {
        "running" => "Syncing library...".to_string(),
        "error" => "Sync needs attention".to_string(),
        _ => "Cached library ready".to_string(),
    }
}
pub(in crate::controller) fn seed_fake_cache(
    store: &StoreHandle,
    scale: FakeScale,
) -> Result<(), String> {
    let provider = FakeProvider::new(scale);
    let server = provider.identity().server.clone();
    let saved = SavedServer {
        server: server.clone(),
        user_id: "fake-user".to_string(),
        username: "fake".to_string(),
        trust_invalid_cert: false,
    };
    store.with_store(|store| {
        store.save_server(&saved)?;
        store.set_active_server(&server.id)?;
        Ok(())
    })?;
    let generation = store.with_store(|store| store.begin_sync(&server.id))?;

    let runtime = Runtime::new().map_err(|error| error.to_string())?;
    let album_limit = match scale {
        FakeScale::Small => provider.album_count(),
        FakeScale::Large => 1_000,
    };
    let track_limit = match scale {
        FakeScale::Small => provider.track_count(),
        FakeScale::Large => 2_000,
    };
    runtime.block_on(async {
        let albums = provider
            .albums(PagedRequest::new(0, album_limit))
            .await
            .map_err(|error| error.to_string())?;
        let tracks = provider
            .tracks(PagedRequest::new(0, track_limit))
            .await
            .map_err(|error| error.to_string())?;
        let artists = provider
            .artists(PagedRequest::new(0, PAGE_SIZE))
            .await
            .map_err(|error| error.to_string())?;
        let album_artists = provider
            .album_artists(PagedRequest::new(0, PAGE_SIZE))
            .await
            .map_err(|error| error.to_string())?;
        let genres = provider
            .genres(PagedRequest::new(0, PAGE_SIZE))
            .await
            .map_err(|error| error.to_string())?;
        let playlists = provider
            .playlists(PagedRequest::new(0, PAGE_SIZE))
            .await
            .map_err(|error| error.to_string())?;
        let home_sections = provider
            .home_sections()
            .await
            .map_err(|error| error.to_string())?;

        store.with_store(|store| {
            store.upsert_albums(&server.id, &albums.items, generation)?;
            store.upsert_tracks(&server.id, &tracks.items, generation)?;
            store.upsert_artists(&server.id, &artists.items, false, generation)?;
            store.upsert_artists(&server.id, &album_artists.items, true, generation)?;
            store.refresh_library_counts(&server.id)?;
            store.upsert_genres(&server.id, &genres.items, generation)?;
            store.upsert_playlists(&server.id, &playlists.items, generation)?;
            store.upsert_home_sections(&server.id, &home_sections, generation)?;
            store.complete_sync(&server.id, generation)?;
            Ok(())
        })
    })?;
    Ok(())
}
pub(in crate::controller) fn restore_queue(
    store: &StoreHandle,
    server: Option<&ServerIdentity>,
) -> Option<QueueEngine> {
    let server = server?;
    let settings = load_settings_for_server(store, server);
    match store.with_store(|store| store.load_queue_snapshot(&server.id)) {
        Ok(Some(mut snapshot)) => {
            external_metadata::normalize_queue_snapshot(&mut snapshot, &settings);
            Some(QueueEngine::restore(snapshot))
        }
        Ok(None) => Some(QueueEngine::new(server.id.clone())),
        Err(error) => {
            warn!(%error, "failed to restore queue snapshot");
            Some(QueueEngine::new(server.id.clone()))
        }
    }
}

pub(in crate::controller) struct LoginActivationContext<'a> {
    pub(in crate::controller) store: &'a StoreHandle,
    pub(in crate::controller) queue: &'a Arc<Mutex<Option<QueueEngine>>>,
    pub(in crate::controller) playback: &'a Arc<Mutex<Box<dyn PlaybackBackend>>>,
    pub(in crate::controller) playback_snapshot: &'a Arc<Mutex<PlaybackSnapshot>>,
    pub(in crate::controller) auto_dj_enabled: &'a Arc<Mutex<bool>>,
    pub(in crate::controller) events: &'a Sender<ControllerEvent>,
}

#[derive(Clone, Copy)]
pub(in crate::controller) struct LoginActivationRequest<'a> {
    pub(in crate::controller) session: &'a ProviderSession,
    pub(in crate::controller) trust_invalid_cert: bool,
    pub(in crate::controller) local_access_root: Option<&'a Path>,
    pub(in crate::controller) path_replace_from: Option<&'a str>,
}

pub(in crate::controller) fn activate_logged_in_server(
    context: &LoginActivationContext<'_>,
    request: LoginActivationRequest<'_>,
) -> Result<SavedServer, String> {
    let session = request.session;
    let saved = SavedServer {
        server: session.server.clone(),
        user_id: session.user_id.clone(),
        username: session.username.clone(),
        trust_invalid_cert: request.trust_invalid_cert,
    };
    context.store.with_store(|store| {
        store.save_server(&saved)?;
        if let Some(root) = request.local_access_root.and_then(Path::to_str) {
            store.save_server_local_access(&ServerLocalAccess {
                server_id: saved.server.id.clone(),
                root_path: root.to_string(),
                path_replace_from: trimmed_optional(request.path_replace_from),
                path_replace_to: Some(root.to_string()),
            })?;
        }
        store.set_active_server(&saved.server.id)?;
        Ok(())
    })?;
    let mut settings = load_settings_from_store(context.store);
    settings.sources.selected = Some(LibrarySourceSelection::Server(saved.server.id.clone()));
    settings.migrate_defaults();
    context.store.save_settings(&settings)?;

    activate_queue_for_saved_and_emit(
        context.store,
        context.queue,
        context.playback,
        context.playback_snapshot,
        context.auto_dj_enabled,
        context.events,
        &saved,
    )?;
    let _sent = context.events.send(ControllerEvent::LoginStatus(
        "Connected. Loading cached library...".to_string(),
    ));
    emit_snapshot(context.store, context.events);
    Ok(saved)
}

pub(in crate::controller) fn save_token_and_activate_logged_in_server(
    context: &LoginActivationContext<'_>,
    secrets: &Arc<dyn SecretStore>,
    request: LoginActivationRequest<'_>,
) -> Result<SavedServer, String> {
    let session = request.session;
    secrets
        .save_token(&session.server.id, &session.access_token)
        .map_err(|error| error.to_string())?;
    match activate_logged_in_server(context, request) {
        Ok(saved) => Ok(saved),
        Err(error) => {
            if let Err(delete_error) = secrets.delete_token(&session.server.id) {
                warn!(
                    %delete_error,
                    server_id = %session.server.id,
                    "failed to delete token after login activation failed"
                );
            }
            Err(error)
        }
    }
}
pub(in crate::controller) fn activate_queue_for_saved_and_emit(
    store: &StoreHandle,
    queue: &Arc<Mutex<Option<QueueEngine>>>,
    playback: &Arc<Mutex<Box<dyn PlaybackBackend>>>,
    playback_snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    auto_dj_enabled: &Arc<Mutex<bool>>,
    events: &Sender<ControllerEvent>,
    saved: &SavedServer,
) -> Result<(), String> {
    let Some((queue_snapshot, player)) =
        activate_queue_for_saved(store, queue, playback_snapshot, auto_dj_enabled, saved)?
    else {
        return Ok(());
    };
    stop_playback_backend(playback, events);
    let _sent = events.send(ControllerEvent::Queue(Box::new(Some(queue_snapshot))));
    let _sent = events.send(ControllerEvent::Playback(Box::new(player)));
    Ok(())
}
pub(in crate::controller) fn activate_queue_for_saved(
    store: &StoreHandle,
    queue: &Arc<Mutex<Option<QueueEngine>>>,
    playback_snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    auto_dj_enabled: &Arc<Mutex<bool>>,
    saved: &SavedServer,
) -> Result<Option<(QueueSnapshot, PlaybackSnapshot)>, String> {
    let mut queue = queue
        .lock()
        .map_err(|_| "queue lock was poisoned".to_string())?;
    let current_server_id = queue.as_ref().map(|queue| queue.snapshot().server_id);
    if current_server_id.as_ref() == Some(&saved.server.id) {
        return Ok(None);
    }

    let restored = restore_queue(store, Some(&saved.server))
        .unwrap_or_else(|| QueueEngine::new(saved.server.id.clone()));
    let queue_snapshot = restored.snapshot();
    let auto_dj_enabled = auto_dj_enabled
        .lock()
        .map(|enabled| *enabled)
        .unwrap_or_default();
    let player = playback_snapshot_from_queue(
        Some(&restored),
        auto_dj_enabled,
        &load_settings_for_saved(store, saved).playback,
    );
    *queue = Some(restored);
    drop(queue);

    if let Ok(mut snapshot) = playback_snapshot.lock() {
        *snapshot = player.clone();
    }

    Ok(Some((queue_snapshot, player)))
}
pub(in crate::controller) fn stop_playback_backend(
    playback: &Arc<Mutex<Box<dyn PlaybackBackend>>>,
    events: &Sender<ControllerEvent>,
) {
    if let Err(error) = playback
        .lock()
        .map_err(|_| "playback lock was poisoned".to_string())
        .and_then(|mut playback| {
            playback
                .send(PlaybackCommand::Stop)
                .map_err(|error| error.to_string())
        })
    {
        let _sent = events.send(ControllerEvent::Error(error));
    }
}
pub(in crate::controller) fn emit_snapshot(store: &StoreHandle, events: &Sender<ControllerEvent>) {
    match load_snapshot(store) {
        Ok(snapshot) => {
            let _sent = events.send(ControllerEvent::Snapshot(Box::new(snapshot)));
        }
        Err(error) => {
            let _sent = events.send(ControllerEvent::Error(error));
        }
    }
}
#[derive(Clone, Debug)]
struct SnapshotSourceReconciliation {
    selected_source: LibrarySourceSelection,
    saved: SavedServer,
}
fn reconcile_snapshot_source(
    store: &StoreHandle,
    settings: &AppSettings,
    remote_saved_servers: &[SavedServer],
) -> Result<Option<SnapshotSourceReconciliation>, String> {
    let selected_source = resolve_selected_source(
        settings,
        remote_saved_servers,
        store.with_store(|store| store.active_server())?,
    );
    let Some(selected_source) = selected_source else {
        return Ok(None);
    };

    let saved = saved_server_for_snapshot_source(store, remote_saved_servers, &selected_source)?;

    // Keep active_server aligned for follow-up cache, sync, and queue work.
    store.with_store(|store| store.set_active_server(&saved.server.id))?;
    Ok(Some(SnapshotSourceReconciliation {
        selected_source,
        saved,
    }))
}
fn saved_server_for_snapshot_source(
    store: &StoreHandle,
    remote_saved_servers: &[SavedServer],
    selected_source: &LibrarySourceSelection,
) -> Result<SavedServer, String> {
    match selected_source {
        LibrarySourceSelection::Local => ensure_local_source_server(store),
        LibrarySourceSelection::Server(server_id) => remote_saved_servers
            .iter()
            .find(|saved| &saved.server.id == server_id)
            .cloned()
            .ok_or_else(|| "The selected source is no longer saved.".to_string()),
    }
}
pub(in crate::controller) fn resolve_selected_source(
    settings: &AppSettings,
    remote_saved_servers: &[SavedServer],
    active_server: Option<SavedServer>,
) -> Option<LibrarySourceSelection> {
    match &settings.sources.selected {
        Some(LibrarySourceSelection::Local) => return Some(LibrarySourceSelection::Local),
        Some(LibrarySourceSelection::Server(server_id))
            if remote_saved_servers
                .iter()
                .any(|saved| saved.server.id == *server_id) =>
        {
            return Some(LibrarySourceSelection::Server(server_id.clone()));
        }
        _ => {}
    }

    if let Some(saved) = active_server
        && saved.server.provider != LOCAL_PROVIDER_ID
    {
        return Some(LibrarySourceSelection::Server(saved.server.id));
    }
    if !settings.sources.local_folders.is_empty() {
        return Some(LibrarySourceSelection::Local);
    }
    remote_saved_servers
        .first()
        .map(|saved| LibrarySourceSelection::Server(saved.server.id.clone()))
}
pub(in crate::controller) fn active_server_needs_sync(
    store: &StoreHandle,
    server_id: &ServerId,
) -> bool {
    store
        .with_store(|store| store.sync_completed_age_seconds(server_id))
        .ok()
        .flatten()
        .is_none_or(|age| age > STARTUP_CACHE_STALE_SECONDS)
}
pub(in crate::controller) fn trimmed_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}
pub(in crate::controller) fn load_settings_from_store(store: &StoreHandle) -> AppSettings {
    let mut settings = store.load_settings().unwrap_or_default();
    settings.migrate_defaults();
    settings
}
pub(in crate::controller) fn local_folder_paths(settings: &AppSettings) -> Vec<PathBuf> {
    settings
        .sources
        .local_folders
        .iter()
        .map(|folder| PathBuf::from(&folder.path))
        .collect()
}
pub(in crate::controller) fn local_source_server() -> ServerIdentity {
    ServerIdentity {
        id: ServerId::new(LOCAL_SOURCE_SERVER_ID),
        provider: LOCAL_PROVIDER_ID.to_string(),
        name: "Local".to_string(),
        base_url: String::new(),
    }
}
pub(in crate::controller) fn local_source_saved() -> SavedServer {
    SavedServer {
        server: local_source_server(),
        user_id: "local".to_string(),
        username: "Local".to_string(),
        trust_invalid_cert: false,
    }
}
pub(in crate::controller) fn ensure_local_source_server(
    store: &StoreHandle,
) -> Result<SavedServer, String> {
    let saved = local_source_saved();
    store.with_store(|store| store.save_server(&saved))?;
    Ok(saved)
}
pub(in crate::controller) fn load_settings_for_active_server(store: &StoreHandle) -> AppSettings {
    let settings = load_settings_from_store(store);
    match store.with_store(|store| store.active_server()) {
        Ok(Some(saved)) => settings_for_server(settings, &saved.server),
        _ => settings,
    }
}
pub(in crate::controller) fn load_settings_for_saved(
    store: &StoreHandle,
    saved: &SavedServer,
) -> AppSettings {
    settings_for_server(load_settings_from_store(store), &saved.server)
}
pub(in crate::controller) fn load_settings_for_server(
    store: &StoreHandle,
    server: &ServerIdentity,
) -> AppSettings {
    settings_for_server(load_settings_from_store(store), server)
}
pub(in crate::controller) fn settings_for_server(
    mut settings: AppSettings,
    server: &ServerIdentity,
) -> AppSettings {
    if server.provider == "fake" {
        settings.external_metadata_enabled = false;
    }
    settings
}
pub(in crate::controller) fn playback_snapshot_from_queue(
    queue: Option<&QueueEngine>,
    auto_dj_enabled: bool,
    playback_settings: &PlaybackSettings,
) -> PlaybackSnapshot {
    queue
        .map(|queue| PlaybackSnapshot {
            current: queue.current().cloned(),
            state: PlaybackState::Stopped,
            position_seconds: queue.progress_seconds(),
            position_millis: u64::from(queue.progress_seconds()) * 1_000,
            duration_seconds: queue
                .current()
                .map(|entry| entry.duration_seconds)
                .unwrap_or_default(),
            volume: playback_settings.volume,
            muted: playback_settings.muted,
            repeat_mode: queue.repeat_mode(),
            shuffle_enabled: queue.shuffle().enabled,
            auto_dj_enabled,
            buffering_percent: None,
            last_error: None,
        })
        .unwrap_or_else(|| PlaybackSnapshot {
            auto_dj_enabled,
            volume: playback_settings.volume,
            muted: playback_settings.muted,
            ..PlaybackSnapshot::default()
        })
}
pub(in crate::controller) fn next_queue_entry_after_current(
    queue: &QueueEngine,
) -> Option<QueueEntry> {
    let mut preview = QueueEngine::restore(queue.snapshot());
    preview.advance_after_end_of_stream().cloned()
}

#[derive(Clone, Debug)]
pub(in crate::controller) struct NextPreloadRequest {
    pub(in crate::controller) server_id: ServerId,
    pub(in crate::controller) current_entry_id: QueueEntryId,
    pub(in crate::controller) next_entry_id: QueueEntryId,
    pub(in crate::controller) next_entry: QueueEntry,
    pub(in crate::controller) playback_settings: PlaybackSettings,
}

fn queue_state_matches_next_preload_request(
    queue: Option<&QueueEngine>,
    request: &NextPreloadRequest,
) -> bool {
    let Some(queue) = queue else {
        return false;
    };
    let Some(current) = queue.current() else {
        return false;
    };
    if current.id != request.current_entry_id {
        return false;
    }
    next_queue_entry_after_current(queue).is_some_and(|entry| entry.id == request.next_entry_id)
}
pub(in crate::controller) fn shuffle_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(1)
}
pub(in crate::controller) fn auto_dj_candidates(
    tracks: &[Track],
    current: &QueueEntry,
    queued_track_ids: &HashSet<TrackId>,
    seed: u64,
) -> Vec<Track> {
    let current_genres = tracks
        .iter()
        .find(|track| track.id == current.track_id)
        .map(|track| {
            track
                .genres
                .iter()
                .map(|genre| genre.to_lowercase())
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();

    let mut candidates = tracks
        .iter()
        .filter(|track| !queued_track_ids.contains(&track.id))
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_by_key(|track| {
        (
            std::cmp::Reverse(auto_dj_score(track, current, &current_genres)),
            auto_dj_shuffle_key(seed, track.id.as_str()),
        )
    });
    candidates.truncate(AUTO_DJ_ITEM_COUNT);
    candidates
}
pub(in crate::controller) fn auto_dj_score(
    track: &Track,
    current: &QueueEntry,
    current_genres: &HashSet<String>,
) -> u8 {
    let mut score = 0;
    if !current_genres.is_empty()
        && track
            .genres
            .iter()
            .any(|genre| current_genres.contains(&genre.to_lowercase()))
    {
        score += 80;
    }
    if current
        .artist_id
        .as_ref()
        .is_some_and(|artist_id| track.artist_id.as_ref() == Some(artist_id))
    {
        score += 60;
    } else if !current.artist.trim().is_empty()
        && track.artist.eq_ignore_ascii_case(current.artist.as_str())
    {
        score += 50;
    }
    if current
        .album_id
        .as_ref()
        .is_some_and(|album_id| track.album_id == *album_id)
    {
        score += 25;
    }
    score
}
pub(in crate::controller) fn auto_dj_shuffle_key(seed: u64, value: &str) -> u64 {
    value
        .bytes()
        .fold(seed ^ 0xa24b_aed4_963e_e407, |hash, byte| {
            hash.rotate_left(7) ^ u64::from(byte)
        })
}
pub(in crate::controller) fn playback_backend(fake: bool) -> Box<dyn PlaybackBackend> {
    if fake {
        return Box::new(FakePlaybackBackend::new());
    }
    Box::new(LazyGStreamerPlaybackBackend::new())
}
pub(in crate::controller) fn platform_secret_store() -> Arc<dyn SecretStore> {
    #[cfg(unix)]
    {
        Arc::new(CachedSecretStore::new(Arc::new(SecretServiceStore::new())))
    }
    #[cfg(not(unix))]
    {
        Arc::new(CachedSecretStore::new(Arc::new(MemorySecretStore::new())))
    }
}
pub(in crate::controller) fn playback_track_from_entry(entry: &QueueEntry) -> PlaybackTrack {
    PlaybackTrack {
        id: entry.track_id.clone(),
        title: entry.title.clone(),
        artist: entry.artist.clone(),
        album: entry.album.clone(),
        duration_seconds: entry.duration_seconds,
    }
}
pub(in crate::controller) fn prepared_item_from_entry(
    entry: &QueueEntry,
    stream: StreamDescriptor,
) -> PreparedPlaybackItem {
    PreparedPlaybackItem::new(playback_track_from_entry(entry), stream)
}
pub(in crate::controller) fn resolve_prepared_item(
    store: &StoreHandle,
    runtime: &Runtime,
    secrets: &Arc<dyn SecretStore>,
    server_id: &ServerId,
    entry: &QueueEntry,
    playback_settings: &PlaybackSettings,
) -> Result<PreparedPlaybackItem, String> {
    let stream = resolve_stream(
        store,
        runtime,
        secrets,
        server_id,
        &entry.track_id,
        playback_settings,
    )?;
    Ok(prepared_item_from_entry(entry, stream))
}
pub(in crate::controller) fn send_prepared_next_if_queue_matches(
    playback: &Arc<Mutex<Box<dyn PlaybackBackend>>>,
    queue: &Arc<Mutex<Option<QueueEngine>>>,
    events: &Sender<ControllerEvent>,
    request: &NextPreloadRequest,
    prepared: PreparedPlaybackItem,
) -> bool {
    let Ok(queue) = queue.lock() else {
        return false;
    };
    if !queue_state_matches_next_preload_request(queue.as_ref(), request) {
        return false;
    }
    if let Err(error) = playback
        .lock()
        .map_err(|_| "playback lock was poisoned".to_string())
        .and_then(|mut playback| {
            playback
                .send(PlaybackCommand::PrepareNext(Some(prepared)))
                .map_err(|error| error.to_string())
        })
    {
        let _sent = events.send(ControllerEvent::Error(error));
        return false;
    }
    true
}
pub(in crate::controller) fn prepare_next_stream_from_handles(
    store: StoreHandle,
    runtime: Arc<Runtime>,
    secrets: Arc<dyn SecretStore>,
    playback: Arc<Mutex<Box<dyn PlaybackBackend>>>,
    queue: Arc<Mutex<Option<QueueEngine>>>,
    events: Sender<ControllerEvent>,
) {
    let playback_settings = load_settings_from_store(&store).playback;
    let Some(request) = next_preload_request_from_queue(&queue, playback_settings) else {
        if let Err(error) = playback
            .lock()
            .map_err(|_| "playback lock was poisoned".to_string())
            .and_then(|mut playback| {
                playback
                    .send(PlaybackCommand::PrepareNext(None))
                    .map_err(|error| error.to_string())
            })
        {
            let _sent = events.send(ControllerEvent::Error(error));
        }
        return;
    };

    thread::spawn(move || {
        let prepared = match resolve_prepared_item(
            &store,
            &runtime,
            &secrets,
            &request.server_id,
            &request.next_entry,
            &request.playback_settings,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                let _sent = events.send(ControllerEvent::Error(error));
                return;
            }
        };
        let _sent =
            send_prepared_next_if_queue_matches(&playback, &queue, &events, &request, prepared);
    });
}
pub(in crate::controller) fn next_preload_request_from_queue(
    queue: &Arc<Mutex<Option<QueueEngine>>>,
    playback_settings: PlaybackSettings,
) -> Option<NextPreloadRequest> {
    queue.lock().ok().and_then(|queue| {
        let queue = queue.as_ref()?;
        let server_id = queue.snapshot().server_id;
        let current_entry_id = queue.current()?.id.clone();
        let next_entry = next_queue_entry_after_current(queue)?;
        let next_entry_id = next_entry.id.clone();
        Some(NextPreloadRequest {
            server_id,
            current_entry_id,
            next_entry_id,
            next_entry,
            playback_settings,
        })
    })
}
