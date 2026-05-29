use super::*;

pub(in crate::controller) fn start_sync_thread(context: SyncContext, saved: SavedServer) {
    let server_id = saved.server.id.clone();
    let permit = match context.sync_in_flight.acquire(server_id.clone()) {
        Ok(Some(permit)) => permit,
        Ok(None) => {
            let _sent = context.events.send(ControllerEvent::LoginStatus(
                "Sync already running.".to_string(),
            ));
            return;
        }
        Err(error) => {
            let _sent = context.events.send(ControllerEvent::Error(error));
            return;
        }
    };

    let prefetch_initial_covers = initial_cover_cache_required(&context.store, &server_id);
    let generation = match context
        .store
        .with_store(|store| store.begin_sync(&server_id))
    {
        Ok(generation) => generation,
        Err(error) => {
            let _sent = context.events.send(ControllerEvent::Error(error));
            return;
        }
    };
    emit_snapshot(&context.store, &context.events);

    thread::spawn(move || {
        let provider_name = provider_display_name(&saved.server.provider);
        let _sent = context.events.send(ControllerEvent::LoginStatus(format!(
            "Syncing {provider_name} library..."
        )));
        let sync_result = run_sync_job(&context, &saved, generation, prefetch_initial_covers);
        drop(permit);
        if !sync_target_is_current(&context.store, &server_id) {
            return;
        }
        match sync_result {
            Ok(()) => {
                covers::start_external_metadata_cover_prefetch_thread(
                    covers::ExternalCoverPrefetchRequest {
                        store: context.store.clone(),
                        runtime: Arc::clone(&context.runtime),
                        secrets: Arc::clone(&context.secrets),
                        events: context.events.clone(),
                        cover_in_flight: Arc::clone(&context.cover_in_flight),
                        external_cover_prefetch_in_flight: Arc::clone(
                            &context.external_cover_prefetch_in_flight,
                        ),
                        cover_slots: Arc::clone(&context.cover_slots),
                        saved: saved.clone(),
                    },
                );
                let _sent = context.events.send(ControllerEvent::LoginStatus(
                    "Library sync complete".to_string(),
                ));
                match load_snapshot(&context.store) {
                    Ok(snapshot) => {
                        let _sent = context
                            .events
                            .send(ControllerEvent::Snapshot(Box::new(snapshot)));
                    }
                    Err(error) => {
                        let _sent = context.events.send(ControllerEvent::Error(error));
                    }
                }
            }
            Err(error) => {
                let _failed = context.store.with_store(|store| {
                    store.fail_sync(&saved.server.id, &error)?;
                    Ok(())
                });
                let _sent = context.events.send(ControllerEvent::Error(error));
            }
        }
    });
}

pub(in crate::controller) fn sync_target_is_current(
    store: &StoreHandle,
    server_id: &ServerId,
) -> bool {
    store
        .with_store(|store| {
            Ok(store
                .active_server()?
                .is_some_and(|saved| saved.server.id == *server_id))
        })
        .unwrap_or(false)
}

pub(in crate::controller) fn start_home_refresh_thread(
    context: HomeRefreshContext,
    saved: SavedServer,
    target: HomeRefreshTarget,
) {
    if saved.server.provider == "fake" {
        return;
    }

    let server_id = saved.server.id.clone();
    if sync_is_running(&context.sync_in_flight, &server_id) {
        return;
    }
    let permit = match context.home_refresh_in_flight.acquire(server_id) {
        Ok(Some(permit)) => permit,
        Ok(None) => return,
        Err(error) => {
            let _sent = context.events.send(ControllerEvent::Error(error));
            return;
        }
    };

    thread::spawn(move || {
        let result = match target {
            HomeRefreshTarget::Section(kind) => refresh_home_section_for_saved(
                &context.store,
                &context.runtime,
                &context.secrets,
                &saved,
                kind,
            ),
        }
        .and_then(|()| load_snapshot(&context.store).map(Box::new));
        drop(permit);
        match result {
            Ok(snapshot) => {
                let _sent = context
                    .events
                    .send(home_refresh_completed_event(target, snapshot));
            }
            Err(error) => {
                warn!(%error, "failed to refresh home sections");
            }
        }
    });
}
pub(in crate::controller) fn start_playlist_refresh_thread(
    context: PlaylistRefreshContext,
    saved: SavedServer,
) {
    if saved.server.provider == "fake" || saved.server.provider == LOCAL_PROVIDER_ID {
        return;
    }

    let server_id = saved.server.id.clone();
    if sync_is_running(&context.sync_in_flight, &server_id) {
        return;
    }
    let permit = match context.playlist_refresh_in_flight.acquire(server_id) {
        Ok(Some(permit)) => permit,
        Ok(None) => return,
        Err(error) => {
            let _sent = context.events.send(ControllerEvent::Error(error));
            return;
        }
    };

    thread::spawn(move || {
        let result =
            refresh_playlists_for_saved(&context.store, &context.runtime, &context.secrets, &saved)
                .and_then(|()| load_snapshot(&context.store).map(Box::new));
        drop(permit);
        match result {
            Ok(snapshot) => {
                let _sent = context.events.send(ControllerEvent::Snapshot(snapshot));
            }
            Err(error) => {
                warn!(%error, "failed to refresh playlists");
            }
        }
    });
}
pub(in crate::controller) fn home_refresh_completed_event(
    target: HomeRefreshTarget,
    snapshot: Box<LibrarySnapshot>,
) -> ControllerEvent {
    ControllerEvent::HomeSectionsUpdated {
        snapshot,
        include_explore: matches!(target, HomeRefreshTarget::Section(HomeSectionKind::Explore)),
    }
}
pub(in crate::controller) fn start_explore_prefetch_thread(
    context: ExplorePrefetchContext,
    saved: SavedServer,
) {
    if saved.server.provider == "fake" {
        return;
    }

    let server_id = saved.server.id.clone();
    if sync_is_running(&context.sync_in_flight, &server_id) {
        return;
    }
    let permit = match context
        .explore_prefetch_in_flight
        .acquire(server_id.clone())
    {
        Ok(Some(permit)) => permit,
        Ok(None) => return,
        Err(error) => {
            let _sent = context.events.send(ControllerEvent::Error(error));
            return;
        }
    };

    thread::spawn(move || {
        let result = prefetch_home_section_for_saved(
            &context.store,
            &context.runtime,
            &context.secrets,
            &saved,
            HomeSectionKind::Explore,
        );
        drop(permit);
        match result {
            Ok(section) => {
                let _sent = context
                    .events
                    .send(ControllerEvent::HomeSectionPrefetched { server_id, section });
            }
            Err(error) => {
                warn!(%error, "failed to prefetch Explore section");
            }
        }
    });
}
pub(in crate::controller) fn start_prefetched_home_section_promotion_thread(
    store: StoreHandle,
    events: Sender<ControllerEvent>,
    server_id: ServerId,
    section: HomeSection,
) {
    thread::spawn(move || {
        let result = promote_prefetched_home_section(&store, &server_id, &section)
            .and_then(|()| load_snapshot(&store).map(Box::new));
        match result {
            Ok(snapshot) => {
                let _sent = events.send(ControllerEvent::HomeSectionsUpdated {
                    snapshot,
                    include_explore: false,
                });
            }
            Err(error) => {
                warn!(%error, "failed to promote prefetched home section");
            }
        }
    });
}

pub(in crate::controller) fn initial_cover_cache_required(
    store: &StoreHandle,
    server_id: &ServerId,
) -> bool {
    store
        .with_store(|store| {
            let albums = store.load_albums(server_id, 0, 1)?;
            let tracks = store.load_tracks(server_id, 0, 1)?;
            Ok(albums.total == 0 && tracks.total == 0)
        })
        .unwrap_or(true)
}

pub(in crate::controller) fn run_sync_job(
    context: &SyncContext,
    saved: &SavedServer,
    generation: i64,
    prefetch_initial_covers: bool,
) -> Result<(), String> {
    let provider = provider_for_saved(&context.store, &context.runtime, &context.secrets, saved)?;
    context.runtime.block_on(sync_provider_generation(
        &context.store,
        &saved.server.id,
        provider.as_music_provider(),
        generation,
    ))?;
    if prefetch_initial_covers {
        let _sent = context.events.send(ControllerEvent::LoginStatus(
            "Caching library artwork...".to_string(),
        ));
        covers::prefetch_initial_provider_cover_cache(
            &context.store,
            &context.runtime,
            &context.secrets,
            &context.events,
            &context.cover_in_flight,
            &context.cover_slots,
            saved,
        )?;
    }
    Ok(())
}
pub(in crate::controller) fn refresh_playlists_for_saved(
    store: &StoreHandle,
    runtime: &Runtime,
    secrets: &Arc<dyn SecretStore>,
    saved: &SavedServer,
) -> Result<(), String> {
    let provider = provider_for_saved(store, runtime, secrets, saved)?;
    runtime.block_on(refresh_playlist_pages(
        store,
        &saved.server.id,
        provider.as_music_provider(),
    ))
}
pub(in crate::controller) fn refresh_home_section_for_saved(
    store: &StoreHandle,
    runtime: &Runtime,
    secrets: &Arc<dyn SecretStore>,
    saved: &SavedServer,
    kind: HomeSectionKind,
) -> Result<(), String> {
    let provider = provider_for_saved(store, runtime, secrets, saved)?;
    runtime.block_on(refresh_home_section(
        store,
        &saved.server.id,
        provider.as_music_provider(),
        kind,
    ))
}
pub(in crate::controller) fn prefetch_home_section_for_saved(
    store: &StoreHandle,
    runtime: &Runtime,
    secrets: &Arc<dyn SecretStore>,
    saved: &SavedServer,
    kind: HomeSectionKind,
) -> Result<HomeSection, String> {
    let provider = provider_for_saved(store, runtime, secrets, saved)?;
    runtime.block_on(prefetch_home_section(
        store,
        &saved.server.id,
        provider.as_music_provider(),
        kind,
    ))
}
#[cfg(test)]
#[instrument(skip(store, provider), fields(server_id = %server_id.as_str()))]
pub(in crate::controller) async fn sync_provider(
    store: &StoreHandle,
    server_id: &ServerId,
    provider: &(impl MusicProvider + ?Sized),
) -> Result<(), String> {
    let generation = store.with_store(|store| store.begin_sync(server_id))?;
    sync_provider_generation(store, server_id, provider, generation).await
}
#[instrument(skip(store, provider), fields(server_id = %server_id.as_str(), generation))]
async fn sync_provider_generation(
    store: &StoreHandle,
    server_id: &ServerId,
    provider: &(impl MusicProvider + ?Sized),
    generation: i64,
) -> Result<(), String> {
    info!(generation, "started provider cache sync");
    sync_album_pages(store, server_id, provider, generation).await?;
    sync_track_pages(store, server_id, provider, generation).await?;
    sync_music_folders(store, server_id, provider, generation).await?;
    sync_artist_pages(store, server_id, provider, generation, false).await?;
    sync_artist_pages(store, server_id, provider, generation, true).await?;
    sync_genre_pages(store, server_id, provider, generation).await?;
    sync_playlist_pages(store, server_id, provider, generation).await?;
    sync_home_sections(store, server_id, provider, generation).await?;
    store.with_store(|store| store.refresh_library_counts(server_id))?;
    store.with_store(|store| store.complete_sync(server_id, generation))?;
    if let Err(error) = refresh_local_track_matches(store, server_id).await {
        warn!(%error, "failed to refresh local track matches");
    }
    info!(generation, "completed provider cache sync");
    Ok(())
}
async fn sync_album_pages(
    store: &StoreHandle,
    server_id: &ServerId,
    provider: &(impl MusicProvider + ?Sized),
    generation: i64,
) -> Result<(), String> {
    let mut offset = 0;
    loop {
        let page = provider
            .albums(PagedRequest::new(offset, PAGE_SIZE))
            .await
            .map_err(|error| error.to_string())?;
        store.with_store(|store| store.upsert_albums(server_id, &page.items, generation))?;
        let item_count = page.items.len();
        offset += item_count;
        if sync_page_finished(item_count, page.total, offset) {
            return Ok(());
        }
    }
}
async fn sync_track_pages(
    store: &StoreHandle,
    server_id: &ServerId,
    provider: &(impl MusicProvider + ?Sized),
    generation: i64,
) -> Result<(), String> {
    let mut offset = 0;
    loop {
        let page = provider
            .tracks(PagedRequest::new(offset, PAGE_SIZE))
            .await
            .map_err(|error| error.to_string())?;
        store.with_store(|store| store.upsert_tracks(server_id, &page.items, generation))?;
        let item_count = page.items.len();
        offset += item_count;
        if sync_page_finished(item_count, page.total, offset) {
            return Ok(());
        }
    }
}
async fn sync_music_folders(
    store: &StoreHandle,
    server_id: &ServerId,
    provider: &(impl MusicProvider + ?Sized),
    generation: i64,
) -> Result<(), String> {
    if !provider.capabilities().music_folders {
        return Ok(());
    }
    let folders = provider
        .music_folders()
        .await
        .map_err(|error| error.to_string())?;
    store.with_store(|store| store.upsert_music_folders(server_id, &folders, generation))?;
    for folder in folders {
        let mut offset = 0;
        loop {
            let page = provider
                .tracks_in_music_folder(&folder.id, PagedRequest::new(offset, PAGE_SIZE))
                .await
                .map_err(|error| error.to_string())?;
            store.with_store(|store| store.upsert_tracks(server_id, &page.items, generation))?;
            store.with_store(|store| {
                store.upsert_track_music_folder_memberships(
                    server_id,
                    &folder.id,
                    &page.items,
                    generation,
                )
            })?;
            let item_count = page.items.len();
            offset += item_count;
            if sync_page_finished(item_count, page.total, offset) {
                break;
            }
        }
    }
    Ok(())
}
pub(in crate::controller) async fn refresh_local_track_matches(
    store: &StoreHandle,
    server_id: &ServerId,
) -> Result<usize, String> {
    let Some(access) = store.with_store(|store| store.server_local_access(server_id))? else {
        return Ok(0);
    };
    let saved = store
        .with_store(|store| {
            store.list_servers().map(|servers| {
                servers
                    .into_iter()
                    .find(|saved| saved.server.id == *server_id)
            })
        })?
        .ok_or_else(|| "The server is no longer saved.".to_string())?;
    if saved.server.provider == "local" {
        return Ok(0);
    }
    let remote_tracks =
        store.with_store(|store| store.load_tracks_for_local_matching(server_id))?;
    if remote_tracks.is_empty() {
        store.with_store(|store| store.replace_track_local_matches(server_id, &[]))?;
        return Ok(0);
    }
    let local_provider = LocalProvider::from_root(PathBuf::from(&access.root_path))
        .map_err(|error| error.to_string())?;
    let local_tracks = load_all_local_tracks_for_matching(&local_provider).await?;
    let matches = conservative_local_matches(&remote_tracks, &local_tracks);
    let count = matches.len();
    store.with_store(|store| store.replace_track_local_matches(server_id, &matches))?;
    debug!(server_id = %server_id, count, "refreshed local track matches");
    Ok(count)
}
async fn load_all_local_tracks_for_matching(
    provider: &LocalProvider,
) -> Result<Vec<Track>, String> {
    let mut tracks = Vec::new();
    let mut offset = 0;
    loop {
        let page = provider
            .tracks(PagedRequest::new(offset, PAGE_SIZE))
            .await
            .map_err(|error| error.to_string())?;
        let item_count = page.items.len();
        tracks.extend(page.items);
        offset += item_count;
        if sync_page_finished(item_count, page.total, offset) {
            return Ok(tracks);
        }
    }
}
pub(in crate::controller) fn local_access_status_for_server(
    store: &StoreHandle,
    server: &ServerIdentity,
    access: Option<&ServerLocalAccess>,
) -> Result<LocalAccessStatus, String> {
    let Some(access) = access else {
        return Ok(LocalAccessStatus::default());
    };
    if server.provider == "local" {
        return Ok(LocalAccessStatus::default());
    }

    let remote_tracks =
        store.with_store(|store| store.load_tracks_for_local_matching(&server.id))?;
    let metadata_matches = store.with_store(|store| store.track_local_match_paths(&server.id))?;
    let metadata_by_track = metadata_matches
        .into_iter()
        .collect::<HashMap<TrackId, String>>();

    let sample_track = remote_tracks
        .iter()
        .find(|track| {
            track
                .local_path
                .as_deref()
                .is_some_and(|path| !path.trim().is_empty())
                && metadata_by_track.contains_key(&track.id)
        })
        .or_else(|| {
            remote_tracks.iter().find(|track| {
                track
                    .local_path
                    .as_deref()
                    .is_some_and(|path| !path.trim().is_empty())
            })
        });
    let sample_server_path = sample_track.and_then(|track| track.local_path.clone());
    let sample_local_path = sample_track.and_then(|track| {
        metadata_by_track.get(&track.id).cloned().or_else(|| {
            track
                .local_path
                .as_deref()
                .and_then(|raw| potential_local_path_text(raw, access))
        })
    });

    let mut effective_matches = HashSet::<TrackId>::new();
    let mut direct_match_count = 0;
    let mut prefix_match_count = 0;
    for track in &remote_tracks {
        let Some(raw) = track.local_path.as_deref() else {
            continue;
        };
        if map_server_path_to_local(raw, access).is_some() {
            prefix_match_count += 1;
            effective_matches.insert(track.id.clone());
        } else if Path::new(raw).is_absolute() {
            direct_match_count += 1;
            effective_matches.insert(track.id.clone());
        }
    }

    let metadata_match_count = metadata_by_track.len();
    for track_id in metadata_by_track.into_keys() {
        effective_matches.insert(track_id);
    }

    let total_track_count = remote_tracks.len();
    let unmatched_count = total_track_count.saturating_sub(effective_matches.len());
    Ok(LocalAccessStatus {
        sample_server_path,
        sample_local_path,
        direct_match_count,
        prefix_match_count,
        metadata_match_count,
        unmatched_count,
        total_track_count,
    })
}
pub(in crate::controller) fn potential_local_path_text(
    raw: &str,
    access: &ServerLocalAccess,
) -> Option<String> {
    if raw.trim().is_empty() {
        return None;
    }
    if let Some(mapped) = map_server_path_to_local(raw, access) {
        return Some(mapped.to_string_lossy().into_owned());
    }
    let direct = Path::new(raw);
    if direct.is_absolute() {
        return Some(direct.to_string_lossy().into_owned());
    }
    None
}
#[derive(Hash, Eq, PartialEq)]
pub(in crate::controller) struct LocalMatchKey {
    title: String,
    album: String,
    artist: String,
    disc_number: u16,
    track_number: u16,
}
pub(in crate::controller) fn conservative_local_matches(
    remote_tracks: &[Track],
    local_tracks: &[Track],
) -> Vec<(TrackId, String, String)> {
    let mut index = HashMap::<LocalMatchKey, Vec<&Track>>::new();
    for track in local_tracks {
        if track.local_path.is_none() {
            continue;
        }
        index.entry(local_match_key(track)).or_default().push(track);
    }

    let mut matches = Vec::new();
    for remote in remote_tracks {
        let Some(candidates) = index.get(&local_match_key(remote)) else {
            continue;
        };
        let matched = candidates
            .iter()
            .copied()
            .filter(|candidate| {
                durations_close(remote.duration_seconds, candidate.duration_seconds)
            })
            .collect::<Vec<_>>();
        if matched.len() != 1 {
            continue;
        }
        let Some(local_path) = matched[0].local_path.clone() else {
            continue;
        };
        matches.push((remote.id.clone(), local_path, "metadata".to_string()));
    }
    matches
}
pub(in crate::controller) fn local_match_key(track: &Track) -> LocalMatchKey {
    LocalMatchKey {
        title: normalize_match_text(&track.title),
        album: normalize_match_text(&track.album),
        artist: normalize_match_text(&track.artist),
        disc_number: track.disc_number,
        track_number: track.track_number,
    }
}
pub(in crate::controller) fn durations_close(left: u32, right: u32) -> bool {
    left == 0 || right == 0 || left.abs_diff(right) <= 3
}
pub(in crate::controller) fn normalize_match_text(value: &str) -> String {
    let mut normalized = String::new();
    for character in value.chars() {
        if character.is_alphanumeric() {
            normalized.extend(character.to_lowercase());
        } else {
            normalized.push(' ');
        }
    }
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}
async fn sync_artist_pages(
    store: &StoreHandle,
    server_id: &ServerId,
    provider: &(impl MusicProvider + ?Sized),
    generation: i64,
    album_artist: bool,
) -> Result<(), String> {
    let mut offset = 0;
    loop {
        let page = if album_artist {
            provider
                .album_artists(PagedRequest::new(offset, PAGE_SIZE))
                .await
        } else {
            provider.artists(PagedRequest::new(offset, PAGE_SIZE)).await
        }
        .map_err(|error| error.to_string())?;
        store.with_store(|store| {
            store.upsert_artists(server_id, &page.items, album_artist, generation)
        })?;
        let item_count = page.items.len();
        offset += item_count;
        if sync_page_finished(item_count, page.total, offset) {
            return Ok(());
        }
    }
}
async fn sync_genre_pages(
    store: &StoreHandle,
    server_id: &ServerId,
    provider: &(impl MusicProvider + ?Sized),
    generation: i64,
) -> Result<(), String> {
    let mut offset = 0;
    loop {
        let page = provider
            .genres(PagedRequest::new(offset, PAGE_SIZE))
            .await
            .map_err(|error| error.to_string())?;
        store.with_store(|store| store.upsert_genres(server_id, &page.items, generation))?;
        let item_count = page.items.len();
        offset += item_count;
        if sync_page_finished(item_count, page.total, offset) {
            return Ok(());
        }
    }
}
async fn sync_playlist_pages(
    store: &StoreHandle,
    server_id: &ServerId,
    provider: &(impl MusicProvider + ?Sized),
    generation: i64,
) -> Result<(), String> {
    let mut offset = 0;
    loop {
        let page = provider
            .playlists(PagedRequest::new(offset, PAGE_SIZE))
            .await
            .map_err(|error| error.to_string())?;
        store.with_store(|store| store.upsert_playlists(server_id, &page.items, generation))?;
        for playlist in &page.items {
            let detail = provider
                .playlist_detail(&playlist.id)
                .await
                .map_err(|error| error.to_string())?;
            store.with_store(|store| {
                store.upsert_tracks(server_id, &detail.tracks, generation)?;
                store.upsert_playlist_entries(
                    server_id,
                    &detail.playlist.id,
                    &detail.entries,
                    generation,
                )?;
                Ok(())
            })?;
        }
        let item_count = page.items.len();
        offset += item_count;
        if sync_page_finished(item_count, page.total, offset) {
            return Ok(());
        }
    }
}
pub(in crate::controller) async fn refresh_playlist_pages(
    store: &StoreHandle,
    server_id: &ServerId,
    provider: &(impl MusicProvider + ?Sized),
) -> Result<(), String> {
    let generation =
        store.with_store(|store| store.sync_state(server_id).map(|state| state.generation))?;
    let mut playlist_ids = Vec::new();
    let mut offset = 0;
    loop {
        let page = provider
            .playlists(PagedRequest::new(offset, PAGE_SIZE))
            .await
            .map_err(|error| error.to_string())?;
        for playlist in &page.items {
            playlist_ids.push(playlist.id.clone());
        }
        store.with_store(|store| store.upsert_playlists(server_id, &page.items, generation))?;
        for playlist in &page.items {
            let detail = provider
                .playlist_detail(&playlist.id)
                .await
                .map_err(|error| error.to_string())?;
            store.with_store(|store| {
                store.upsert_tracks(server_id, &detail.tracks, generation)?;
                store.upsert_playlist_entries(
                    server_id,
                    &detail.playlist.id,
                    &detail.entries,
                    generation,
                )?;
                Ok(())
            })?;
        }
        let item_count = page.items.len();
        offset += item_count;
        if sync_page_finished(item_count, page.total, offset) {
            store.with_store(|store| store.prune_playlists_except(server_id, &playlist_ids))?;
            return Ok(());
        }
    }
}
async fn sync_home_sections(
    store: &StoreHandle,
    server_id: &ServerId,
    provider: &(impl MusicProvider + ?Sized),
    generation: i64,
) -> Result<(), String> {
    let sections = provider
        .home_sections()
        .await
        .map_err(|error| error.to_string())?;
    cache_home_sections(store, server_id, &sections, generation)
}
#[cfg(test)]
pub(in crate::controller) async fn refresh_home_sections(
    store: &StoreHandle,
    server_id: &ServerId,
    provider: &(impl MusicProvider + ?Sized),
) -> Result<(), String> {
    let generation =
        store.with_store(|store| store.sync_state(server_id).map(|state| state.generation))?;
    let sections = provider
        .home_sections()
        .await
        .map_err(|error| error.to_string())?;
    cache_home_sections(store, server_id, &sections, generation)
}
#[cfg(test)]
pub(in crate::controller) async fn refresh_home_sections_without_explore(
    store: &StoreHandle,
    server_id: &ServerId,
    provider: &(impl MusicProvider + ?Sized),
) -> Result<(), String> {
    for kind in home_refresh_section_kinds()
        .into_iter()
        .filter(|kind| *kind != HomeSectionKind::Explore)
    {
        refresh_home_section(store, server_id, provider, kind).await?;
    }
    Ok(())
}
pub(in crate::controller) async fn refresh_home_section(
    store: &StoreHandle,
    server_id: &ServerId,
    provider: &(impl MusicProvider + ?Sized),
    kind: HomeSectionKind,
) -> Result<(), String> {
    let generation =
        store.with_store(|store| store.sync_state(server_id).map(|state| state.generation))?;
    let section = provider
        .home_section(kind)
        .await
        .map_err(|error| error.to_string())?;
    cache_home_section(store, server_id, &section, generation)
}
pub(in crate::controller) async fn prefetch_home_section(
    store: &StoreHandle,
    server_id: &ServerId,
    provider: &(impl MusicProvider + ?Sized),
    kind: HomeSectionKind,
) -> Result<HomeSection, String> {
    let generation =
        store.with_store(|store| store.sync_state(server_id).map(|state| state.generation))?;
    let section = provider
        .home_section(kind)
        .await
        .map_err(|error| error.to_string())?;
    cache_home_section_items(store, server_id, &section, generation)?;
    store
        .with_store(|store| store.upsert_home_section_prefetch(server_id, &section, generation))?;
    Ok(section)
}
