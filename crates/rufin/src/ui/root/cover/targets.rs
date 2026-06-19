use super::*;

#[derive(Clone, Copy)]
pub(in crate::ui) struct InitialRouteCoverMetrics {
    pub(in crate::ui) route_height: i32,
    pub(in crate::ui) app_height: i32,
    pub(in crate::ui) grid_columns: usize,
    pub(in crate::ui) grid_card_size: i32,
    pub(in crate::ui) album_grid_columns: usize,
    pub(in crate::ui) album_grid_card_size: i32,
    pub(in crate::ui) home_showcase_seed: u64,
}
impl InitialRouteCoverMetrics {
    fn initial_visible_count(self, key: LibraryListKey, settings: &LibraryListSettings) -> usize {
        let viewport_height = self.route_height.max(self.app_height).max(1);
        match settings.layout {
            LibraryLayout::Row => {
                let row_height = library::LIBRARY_TABLE_ROW_HEIGHT.max(1);
                (viewport_height / row_height).saturating_add(2).max(1) as usize
            }
            LibraryLayout::Grid | LibraryLayout::Detail => {
                let (columns, card_size) = self.collection_grid_metrics(key, settings);
                let item_extent = library::collection_grid_item_extent(card_size, settings);
                let rows = (viewport_height / item_extent).saturating_add(2).max(1) as usize;
                rows.saturating_mul(columns)
            }
        }
    }

    fn collection_grid_metrics(
        self,
        key: LibraryListKey,
        settings: &LibraryListSettings,
    ) -> (usize, i32) {
        if key == LibraryListKey::Albums && settings.layout == LibraryLayout::Grid {
            (self.album_grid_columns, self.album_grid_card_size)
        } else {
            (self.grid_columns, self.grid_card_size)
        }
    }
}

const STARTUP_QUEUE_ROW_HEIGHT: i32 = 58;
const STARTUP_QUEUE_COVER_SIZE: i32 = 50;
const STARTUP_QUEUE_FALLBACK_APP_HEIGHT: i32 = 900;
const SOURCE_BACKGROUND_COVER_WARM_LIMIT: usize = DECODED_COVER_CACHE_LIMIT;

pub(in crate::ui::root::cover) fn startup_cover_prime_jobs(shell: &Shell) -> Vec<CoverWarmJob> {
    startup_cover_jobs_from_targets(
        shell,
        startup_cover_prime_targets(shell),
        Some(STARTUP_CACHED_COVER_PRIME_LIMIT),
    )
}
fn startup_cover_jobs_from_targets(
    shell: &Shell,
    targets: Vec<CoverWarmTarget>,
    limit: Option<usize>,
) -> Vec<CoverWarmJob> {
    let mut seen = HashSet::new();
    let mut jobs = Vec::new();

    for target in targets {
        let decode_size = cover_decode_size(target.size, target.fetch_size);
        let Some(key) = shell.cover_cache_key(&target.image_ref, target.fetch_size) else {
            continue;
        };
        if !seen.insert(key.clone())
            || shell
                .decoded_cover_for_ref(&target.image_ref, target.fetch_size, decode_size)
                .is_some()
        {
            continue;
        }
        jobs.push(CoverWarmJob {
            key,
            image_ref: target.image_ref,
            fetch_size: target.fetch_size,
            size: decode_size,
        });
        if limit.is_some_and(|limit| jobs.len() >= limit) {
            break;
        }
    }

    jobs
}
pub(in crate::ui) fn sidebar_route_visible(settings: &AppSettings, item: SidebarRouteItem) -> bool {
    settings
        .sidebar
        .route_items
        .iter()
        .any(|entry| entry.item == item && entry.visible)
}
fn startup_cover_prime_targets(shell: &Shell) -> Vec<CoverWarmTarget> {
    let mut targets = startup_home_cover_prime_targets(shell);
    push_startup_playback_targets(&mut targets, &shell.state.player.borrow());
    push_startup_queue_targets(
        &mut targets,
        shell.state.queue.borrow().as_ref(),
        shell.state.queue_filter.borrow().trim(),
        shell.state.resolved_right_sidebar.get().is_visible(),
        shell.state.fullscreen_player_visible.get(),
        startup_queue_app_height(shell),
        shell
            .state
            .library
            .borrow()
            .server
            .as_ref()
            .map(|server| &server.id),
    );
    let route = shell.state.routes.borrow().current().clone();
    if matches!(route, Route::SmartPlaylists) && shell.state.smart_playlists.borrow().is_empty() {
        let playlists = shell
            .controller
            .cached_smart_playlists_page(0, 1_000)
            .map(|page| page.items)
            .unwrap_or_else(|error| {
                warn!(%error, "failed to load cached smart playlists for startup cover prime");
                Vec::new()
            });
        *shell.state.smart_playlists.borrow_mut() = playlists;
        shell.state.smart_playlists_loaded.set(true);
    }
    targets.extend(startup_route_cover_targets(shell, &route));
    let Some(server_id) = shell
        .state
        .library
        .borrow()
        .server
        .as_ref()
        .map(|server| server.id.clone())
    else {
        return targets;
    };
    dedupe_warm_targets(&mut targets, &server_id);
    targets
}

fn push_startup_playback_targets(targets: &mut Vec<CoverWarmTarget>, player: &PlaybackSnapshot) {
    push_startup_cover_target(
        targets,
        player
            .current
            .as_ref()
            .and_then(|entry| entry.image_ref.as_ref()),
        THUMB_COVER_SIZE,
        player::BOTTOM_PLAYER_COVER_SIZE,
    );
}

fn push_startup_queue_targets(
    targets: &mut Vec<CoverWarmTarget>,
    queue: Option<&QueueSnapshot>,
    filter: &str,
    right_visible: bool,
    fullscreen_visible: bool,
    app_height: i32,
    active_server_id: Option<&ServerId>,
) {
    if !right_visible && !fullscreen_visible {
        return;
    }
    let Some(queue) = queue else {
        return;
    };
    if active_server_id.is_some_and(|server_id| server_id != &queue.server_id) {
        return;
    }
    let count = startup_queue_visible_count(app_height);
    let filter = filter.trim().to_lowercase();
    if right_visible {
        push_queue_entry_targets(targets, queue, &filter, count);
    }
    if fullscreen_visible {
        push_queue_entry_targets(targets, queue, "", count);
    }
}

fn push_queue_entry_targets(
    targets: &mut Vec<CoverWarmTarget>,
    queue: &QueueSnapshot,
    filter: &str,
    count: usize,
) {
    let entries = queue
        .entries
        .iter()
        .filter(|entry| queue_entry_matches_startup_filter(entry, filter))
        .collect::<Vec<_>>();
    let current_id = queue
        .current_index
        .and_then(|index| queue.entries.get(index))
        .map(|entry| &entry.id);
    let current_row =
        current_id.and_then(|current_id| entries.iter().position(|entry| &entry.id == current_id));
    let (start, end) = queue_startup_target_range(entries.len(), count, current_row);
    for entry in entries[start..end].iter() {
        push_startup_cover_target(
            targets,
            entry.image_ref.as_ref(),
            THUMB_COVER_SIZE,
            STARTUP_QUEUE_COVER_SIZE,
        );
    }
}

fn queue_startup_target_range(
    total: usize,
    count: usize,
    current_row: Option<usize>,
) -> (usize, usize) {
    if total == 0 || count == 0 {
        return (0, 0);
    }
    let count = count.min(total);
    let Some(current_row) = current_row else {
        return (0, count);
    };
    let lead = ((count as f64) * 0.42).ceil() as usize;
    let start = current_row
        .saturating_sub(lead)
        .min(total.saturating_sub(count));
    (start, start + count)
}

fn queue_entry_matches_startup_filter(entry: &QueueEntry, filter: &str) -> bool {
    filter.is_empty()
        || entry.title.to_lowercase().contains(filter)
        || entry.artist.to_lowercase().contains(filter)
        || entry.album.to_lowercase().contains(filter)
        || (entry.year != 0 && entry.year.to_string().contains(filter))
}

fn startup_queue_visible_count(app_height: i32) -> usize {
    let height = app_height
        .saturating_sub(player::BOTTOM_PLAYER_HEIGHT)
        .max(STARTUP_QUEUE_ROW_HEIGHT);
    (height / STARTUP_QUEUE_ROW_HEIGHT).saturating_add(2) as usize
}

fn startup_queue_app_height(shell: &Shell) -> i32 {
    startup_queue_prime_height(
        shell.app_root.height(),
        shell.window.height(),
        shell.state.settings.borrow().window_height,
    )
}

fn startup_queue_prime_height(
    app_height: i32,
    window_height: i32,
    saved_window_height: Option<i32>,
) -> i32 {
    let min_height = player::BOTTOM_PLAYER_HEIGHT.saturating_add(STARTUP_QUEUE_ROW_HEIGHT);
    if app_height > min_height {
        return app_height;
    }
    if window_height > min_height {
        return window_height;
    }
    if let Some(saved_window_height) = saved_window_height
        && saved_window_height > min_height
    {
        return saved_window_height;
    }
    STARTUP_QUEUE_FALLBACK_APP_HEIGHT
}

fn startup_route_cover_targets(shell: &Shell, route: &Route) -> Vec<CoverWarmTarget> {
    let targets = route_visible_cover_targets(shell, route);
    if !targets.is_empty() {
        return targets;
    }
    startup_route_cover_fallback_targets(shell, route)
}

fn startup_route_cover_fallback_targets(shell: &Shell, route: &Route) -> Vec<CoverWarmTarget> {
    let library = shell.state.library.borrow();
    let settings = shell.state.settings.borrow();
    let metrics = shell.source_route_initial_cover_metrics();
    let mut targets = Vec::new();
    match route {
        Route::Tracks if library.tracks.is_empty() && library.cached_track_count > 0 => {
            let list_settings = settings.library_list(LibraryListKey::Tracks);
            let limit = metrics.initial_visible_count(LibraryListKey::Tracks, &list_settings);
            drop(settings);
            drop(library);
            if let Ok(page) = shell.controller.cached_tracks_page(0, limit) {
                push_track_source_warm_targets(
                    &mut targets,
                    page.items,
                    LibraryListKey::Tracks,
                    &list_settings,
                    false,
                    metrics,
                );
            }
        }
        Route::Albums if library.albums.is_empty() && library.cached_album_count > 0 => {
            let list_settings = settings.library_list(LibraryListKey::Albums);
            let limit = metrics.initial_visible_count(LibraryListKey::Albums, &list_settings);
            drop(settings);
            drop(library);
            if let Ok(page) = shell.controller.cached_albums_page(0, limit) {
                push_album_source_warm_targets(&mut targets, page.items, &list_settings, metrics);
            }
        }
        Route::Artists if library.artists.is_empty() && library.cached_artist_count > 0 => {
            let list_settings = settings.library_list(LibraryListKey::Artists);
            let limit = metrics.initial_visible_count(LibraryListKey::Artists, &list_settings);
            drop(settings);
            drop(library);
            if let Ok(page) = shell.controller.cached_artists_page(false, 0, limit) {
                push_artist_source_warm_targets(
                    &mut targets,
                    page.items,
                    LibraryListKey::Artists,
                    &list_settings,
                    metrics,
                );
            }
        }
        Route::AlbumArtists
            if library.album_artists.is_empty() && library.cached_album_artist_count > 0 =>
        {
            let list_settings = settings.library_list(LibraryListKey::AlbumArtists);
            let limit = metrics.initial_visible_count(LibraryListKey::AlbumArtists, &list_settings);
            drop(settings);
            drop(library);
            if let Ok(page) = shell.controller.cached_artists_page(true, 0, limit) {
                push_artist_source_warm_targets(
                    &mut targets,
                    page.items,
                    LibraryListKey::AlbumArtists,
                    &list_settings,
                    metrics,
                );
            }
        }
        Route::Genres if library.genres.is_empty() && library.cached_genre_count > 0 => {
            let list_settings = settings.library_list(LibraryListKey::Genres);
            let limit = metrics.initial_visible_count(LibraryListKey::Genres, &list_settings);
            drop(settings);
            drop(library);
            if let Ok(page) = shell.controller.cached_genres_page(0, limit) {
                push_genre_source_warm_targets(&mut targets, page.items, &list_settings, metrics);
            }
        }
        Route::Playlists if library.playlists.is_empty() && library.cached_playlist_count > 0 => {
            let list_settings = settings.library_list(LibraryListKey::Playlists);
            let limit = metrics.initial_visible_count(LibraryListKey::Playlists, &list_settings);
            drop(settings);
            drop(library);
            if let Ok(page) = shell.controller.cached_playlists_page(0, limit) {
                push_playlist_source_warm_targets(
                    &mut targets,
                    page.items,
                    &list_settings,
                    metrics,
                );
            }
        }
        Route::SmartPlaylists if shell.state.smart_playlists.borrow().is_empty() => {
            let list_settings = settings.library_list(LibraryListKey::SmartPlaylists);
            drop(settings);
            drop(library);
            let playlists = shell.state.smart_playlists.borrow().clone();
            push_smart_targets(&mut targets, playlists, &list_settings, metrics);
        }
        _ => {}
    }
    targets
}

#[cfg(test)]
fn startup_cover_targets(
    library: &LibrarySnapshot,
    settings: &AppSettings,
    home_showcase_seed: u64,
) -> Vec<CoverWarmTarget> {
    startup_prime_targets(library, settings, home_showcase_seed)
}
pub(in crate::ui::root::cover) fn source_warm_targets(
    library: &LibrarySnapshot,
    smart_playlists: &[SmartPlaylist],
    settings: &AppSettings,
    route_metrics: InitialRouteCoverMetrics,
) -> Vec<CoverWarmTarget> {
    let mut targets = Vec::new();
    push_startup_home_prime_targets(
        &mut targets,
        library,
        settings,
        route_metrics.home_showcase_seed,
    );
    let Some(server_id) = library.server.as_ref().map(|server| &server.id) else {
        return targets;
    };
    push_source_route_warm_targets(
        &mut targets,
        server_id,
        library,
        smart_playlists,
        settings,
        route_metrics,
    );
    dedupe_warm_targets(&mut targets, server_id);
    targets
}
pub(in crate::ui::root::cover) fn startup_home_cover_prime_targets(
    shell: &Shell,
) -> Vec<CoverWarmTarget> {
    startup_prime_targets(
        &shell.state.library.borrow(),
        &shell.state.settings.borrow(),
        shell.state.home_showcase_seed.get(),
    )
}
fn startup_prime_targets(
    library: &LibrarySnapshot,
    settings: &AppSettings,
    home_showcase_seed: u64,
) -> Vec<CoverWarmTarget> {
    let mut targets = Vec::new();
    push_startup_home_prime_targets(&mut targets, library, settings, home_showcase_seed);
    targets
}
fn push_startup_home_prime_targets(
    targets: &mut Vec<CoverWarmTarget>,
    library: &LibrarySnapshot,
    settings: &AppSettings,
    home_showcase_seed: u64,
) {
    let mut section_blocks = 0_usize;
    for block in &settings.home_blocks {
        match block {
            HomeBlockKind::Showcase => {
                if let Some(album) = home::showcase_album(library, home_showcase_seed) {
                    push_startup_cover_target(
                        targets,
                        album.image_ref.as_ref(),
                        GRID_COVER_SIZE,
                        GRID_COVER_SIZE as i32,
                    );
                }
            }
            HomeBlockKind::Genres => {}
            _ => {
                if section_blocks >= STARTUP_HOME_SECTION_LIMIT {
                    continue;
                }
                let Some(kind) = block.section_kind() else {
                    continue;
                };
                let Some(section) = library
                    .home_sections
                    .iter()
                    .find(|section| section.kind == kind)
                else {
                    continue;
                };

                section_blocks = section_blocks.saturating_add(1);
                for album in section.albums.iter().take(STARTUP_HOME_SECTION_COVER_LIMIT) {
                    push_startup_cover_target(
                        targets,
                        album.image_ref.as_ref(),
                        GRID_COVER_SIZE,
                        GRID_COVER_SIZE as i32,
                    );
                }
                for track in section.tracks.iter().take(STARTUP_HOME_SECTION_COVER_LIMIT) {
                    push_startup_cover_target(
                        targets,
                        track.image_ref.as_ref(),
                        GRID_COVER_SIZE,
                        GRID_COVER_SIZE as i32,
                    );
                }
            }
        }
    }
}
pub(in crate::ui) fn row_layout_uses_cover(settings: &LibraryListSettings) -> bool {
    settings
        .row_fields
        .iter()
        .any(|field| matches!(field, LibraryField::Image | LibraryField::TitleMerged))
}
pub(in crate::ui::root) fn push_startup_cover_target(
    targets: &mut Vec<CoverWarmTarget>,
    image_ref: Option<&ImageRef>,
    fetch_size: u32,
    size: i32,
) {
    let Some(image_ref) = image_ref else {
        return;
    };
    targets.push(CoverWarmTarget {
        image_ref: image_ref.clone(),
        fetch_size,
        size,
    });
}
fn push_source_route_warm_targets(
    targets: &mut Vec<CoverWarmTarget>,
    server_id: &ServerId,
    library: &LibrarySnapshot,
    smart_playlists: &[SmartPlaylist],
    settings: &AppSettings,
    route_metrics: InitialRouteCoverMetrics,
) {
    if sidebar_route_visible(settings, SidebarRouteItem::Tracks) {
        let list_settings = settings.library_list(LibraryListKey::Tracks);
        push_track_source_warm_targets(
            targets,
            library.tracks.clone(),
            LibraryListKey::Tracks,
            &list_settings,
            false,
            route_metrics,
        );
        push_track_targets(
            targets,
            library.tracks.clone(),
            &list_settings,
            route_metrics,
        );
    }
    if sidebar_route_visible(settings, SidebarRouteItem::Albums) {
        let list_settings = settings.library_list(LibraryListKey::Albums);
        push_album_source_warm_targets(
            targets,
            library.albums.clone(),
            &list_settings,
            route_metrics,
        );
    }
    if sidebar_route_visible(settings, SidebarRouteItem::Artists) {
        let list_settings = settings.library_list(LibraryListKey::Artists);
        push_artist_source_warm_targets(
            targets,
            library.artists.clone(),
            LibraryListKey::Artists,
            &list_settings,
            route_metrics,
        );
    }
    if sidebar_route_visible(settings, SidebarRouteItem::AlbumArtists) {
        let list_settings = settings.library_list(LibraryListKey::AlbumArtists);
        push_artist_source_warm_targets(
            targets,
            library.album_artists.clone(),
            LibraryListKey::AlbumArtists,
            &list_settings,
            route_metrics,
        );
    }
    if sidebar_route_visible(settings, SidebarRouteItem::Genres) {
        let list_settings = settings.library_list(LibraryListKey::Genres);
        push_genre_source_warm_targets(
            targets,
            library.genres.clone(),
            &list_settings,
            route_metrics,
        );
    }
    if sidebar_route_visible(settings, SidebarRouteItem::Favorites) {
        let list_settings = settings.library_list(LibraryListKey::FavoriteTracks);
        push_track_source_warm_targets(
            targets,
            library.favorites.clone(),
            LibraryListKey::FavoriteTracks,
            &list_settings,
            true,
            route_metrics,
        );
    }
    if sidebar_route_visible(settings, SidebarRouteItem::Playlists) {
        let list_settings = settings.library_list(LibraryListKey::Playlists);
        push_playlist_source_warm_targets(
            targets,
            library.playlists.clone(),
            &list_settings,
            route_metrics,
        );
    }
    if sidebar_route_visible(settings, SidebarRouteItem::SmartPlaylists) {
        let list_settings = settings.library_list(LibraryListKey::SmartPlaylists);
        push_smart_targets(
            targets,
            smart_playlists.to_vec(),
            &list_settings,
            route_metrics,
        );
    }
    push_source_background_warm_targets(targets, library, smart_playlists, settings, route_metrics);
    dedupe_warm_targets(targets, server_id);
}
fn push_track_source_warm_targets(
    targets: &mut Vec<CoverWarmTarget>,
    mut tracks: Vec<Track>,
    key: LibraryListKey,
    settings: &LibraryListSettings,
    favorite_first: bool,
    route_metrics: InitialRouteCoverMetrics,
) {
    let Some((fetch_size, size)) = source_route_cover_size(key, settings, route_metrics) else {
        return;
    };
    library::sort_tracks(&mut tracks, settings, favorite_first);
    for track in tracks
        .iter()
        .take(route_metrics.initial_visible_count(key, settings))
    {
        push_startup_cover_target(targets, track.image_ref.as_ref(), fetch_size, size);
    }
}
fn push_track_targets(
    targets: &mut Vec<CoverWarmTarget>,
    mut tracks: Vec<Track>,
    settings: &LibraryListSettings,
    route_metrics: InitialRouteCoverMetrics,
) {
    let Some((fetch_size, size)) =
        source_route_cover_size(LibraryListKey::Tracks, settings, route_metrics)
    else {
        return;
    };
    library::sort_tracks(&mut tracks, settings, false);
    let total = tracks.len();
    if total == 0 {
        return;
    }
    let visible_rows = route_metrics
        .initial_visible_count(LibraryListKey::Tracks, settings)
        .max(1)
        .min(total);
    for numerator in [1_usize, 2, 3, 4] {
        let start = total.saturating_sub(visible_rows).saturating_mul(numerator) / 4;
        let end = start.saturating_add(visible_rows).min(total);
        for track in &tracks[start..end] {
            push_startup_cover_target(targets, track.image_ref.as_ref(), fetch_size, size);
        }
    }
}
fn push_album_source_warm_targets(
    targets: &mut Vec<CoverWarmTarget>,
    mut albums: Vec<Album>,
    settings: &LibraryListSettings,
    route_metrics: InitialRouteCoverMetrics,
) {
    let Some((fetch_size, size)) =
        source_route_cover_size(LibraryListKey::Albums, settings, route_metrics)
    else {
        return;
    };
    library::sort_albums(&mut albums, settings);
    for album in albums
        .iter()
        .take(route_metrics.initial_visible_count(LibraryListKey::Albums, settings))
    {
        push_startup_cover_target(targets, album.image_ref.as_ref(), fetch_size, size);
    }
}
fn push_artist_source_warm_targets(
    targets: &mut Vec<CoverWarmTarget>,
    mut artists: Vec<Artist>,
    key: LibraryListKey,
    settings: &LibraryListSettings,
    route_metrics: InitialRouteCoverMetrics,
) {
    let Some((fetch_size, size)) = source_route_cover_size(key, settings, route_metrics) else {
        return;
    };
    library::sort_artists(&mut artists, settings);
    for artist in artists
        .iter()
        .take(route_metrics.initial_visible_count(key, settings))
    {
        push_startup_cover_target(targets, artist.image_ref.as_ref(), fetch_size, size);
    }
}
fn push_genre_source_warm_targets(
    targets: &mut Vec<CoverWarmTarget>,
    mut genres: Vec<Genre>,
    settings: &LibraryListSettings,
    route_metrics: InitialRouteCoverMetrics,
) {
    let Some((fetch_size, size)) = source_collection_route_cover_size(settings) else {
        return;
    };
    library::sort_genres(&mut genres, settings);
    for genre in genres
        .iter()
        .take(route_metrics.initial_visible_count(LibraryListKey::Genres, settings))
    {
        for image_ref in &genre.image_refs {
            push_startup_cover_target(targets, Some(image_ref), fetch_size, size);
        }
        push_startup_cover_target(targets, genre.image_ref.as_ref(), fetch_size, size);
    }
}
fn push_playlist_source_warm_targets(
    targets: &mut Vec<CoverWarmTarget>,
    mut playlists: Vec<Playlist>,
    settings: &LibraryListSettings,
    route_metrics: InitialRouteCoverMetrics,
) {
    let Some((fetch_size, size)) = source_collection_route_cover_size(settings) else {
        return;
    };
    library::sort_playlists(&mut playlists, settings);
    for playlist in playlists
        .iter()
        .take(route_metrics.initial_visible_count(LibraryListKey::Playlists, settings))
    {
        for image_ref in &playlist.image_refs {
            push_startup_cover_target(targets, Some(image_ref), fetch_size, size);
        }
    }
}
fn push_smart_targets(
    targets: &mut Vec<CoverWarmTarget>,
    mut playlists: Vec<SmartPlaylist>,
    settings: &LibraryListSettings,
    route_metrics: InitialRouteCoverMetrics,
) {
    let Some((fetch_size, size)) = source_collection_route_cover_size(settings) else {
        return;
    };
    library::sort_smart_playlists(&mut playlists, settings);
    for playlist in playlists
        .iter()
        .take(route_metrics.initial_visible_count(LibraryListKey::SmartPlaylists, settings))
    {
        for image_ref in &playlist.image_refs {
            push_startup_cover_target(targets, Some(image_ref), fetch_size, size);
        }
        push_startup_cover_target(targets, playlist.image_ref.as_ref(), fetch_size, size);
    }
}
fn push_source_background_warm_targets(
    targets: &mut Vec<CoverWarmTarget>,
    library: &LibrarySnapshot,
    smart_playlists: &[SmartPlaylist],
    settings: &AppSettings,
    route_metrics: InitialRouteCoverMetrics,
) {
    let mut seen = HashSet::new();
    let mut remaining = SOURCE_BACKGROUND_COVER_WARM_LIMIT;

    if sidebar_route_visible(settings, SidebarRouteItem::Albums) {
        let list_settings = settings.library_list(LibraryListKey::Albums);
        if let Some((fetch_size, size)) =
            source_route_cover_size(LibraryListKey::Albums, &list_settings, route_metrics)
        {
            push_background_cover_refs(
                targets,
                &mut seen,
                &mut remaining,
                library
                    .albums
                    .iter()
                    .filter_map(|album| album.image_ref.as_ref()),
                fetch_size,
                size,
            );
        }
    }
    if sidebar_route_visible(settings, SidebarRouteItem::Artists) {
        let list_settings = settings.library_list(LibraryListKey::Artists);
        if let Some((fetch_size, size)) =
            source_route_cover_size(LibraryListKey::Artists, &list_settings, route_metrics)
        {
            push_background_cover_refs(
                targets,
                &mut seen,
                &mut remaining,
                library
                    .artists
                    .iter()
                    .filter_map(|artist| artist.image_ref.as_ref()),
                fetch_size,
                size,
            );
        }
    }
    if sidebar_route_visible(settings, SidebarRouteItem::AlbumArtists) {
        let list_settings = settings.library_list(LibraryListKey::AlbumArtists);
        if let Some((fetch_size, size)) =
            source_route_cover_size(LibraryListKey::AlbumArtists, &list_settings, route_metrics)
        {
            push_background_cover_refs(
                targets,
                &mut seen,
                &mut remaining,
                library
                    .album_artists
                    .iter()
                    .filter_map(|artist| artist.image_ref.as_ref()),
                fetch_size,
                size,
            );
        }
    }
    if sidebar_route_visible(settings, SidebarRouteItem::Genres) {
        let list_settings = settings.library_list(LibraryListKey::Genres);
        if let Some((fetch_size, size)) = source_collection_route_cover_size(&list_settings) {
            push_background_cover_ref_values(
                targets,
                &mut seen,
                &mut remaining,
                library.genres.iter().flat_map(|genre| {
                    crate::cover_art_policy::selected_genre_artwork(genre).image_refs
                }),
                fetch_size,
                size,
            );
        }
    }
    if sidebar_route_visible(settings, SidebarRouteItem::Playlists) {
        let list_settings = settings.library_list(LibraryListKey::Playlists);
        if let Some((fetch_size, size)) = source_collection_route_cover_size(&list_settings) {
            push_background_cover_ref_values(
                targets,
                &mut seen,
                &mut remaining,
                library.playlists.iter().flat_map(|playlist| {
                    crate::cover_art_policy::selected_playlist_artwork(playlist, settings)
                        .image_refs
                }),
                fetch_size,
                size,
            );
        }
    }
    if sidebar_route_visible(settings, SidebarRouteItem::SmartPlaylists) {
        let list_settings = settings.library_list(LibraryListKey::SmartPlaylists);
        if let Some((fetch_size, size)) = source_collection_route_cover_size(&list_settings) {
            push_background_cover_ref_values(
                targets,
                &mut seen,
                &mut remaining,
                smart_playlists.iter().flat_map(|playlist| {
                    crate::cover_art_policy::selected_smart_playlist_artwork(playlist).image_refs
                }),
                fetch_size,
                size,
            );
        }
    }
    if sidebar_route_visible(settings, SidebarRouteItem::Tracks) {
        let list_settings = settings.library_list(LibraryListKey::Tracks);
        if let Some((fetch_size, size)) =
            source_route_cover_size(LibraryListKey::Tracks, &list_settings, route_metrics)
        {
            push_background_cover_refs(
                targets,
                &mut seen,
                &mut remaining,
                library
                    .tracks
                    .iter()
                    .filter_map(|track| track.image_ref.as_ref()),
                fetch_size,
                size,
            );
        }
    }
    if sidebar_route_visible(settings, SidebarRouteItem::Favorites) {
        let list_settings = settings.library_list(LibraryListKey::FavoriteTracks);
        if let Some((fetch_size, size)) = source_route_cover_size(
            LibraryListKey::FavoriteTracks,
            &list_settings,
            route_metrics,
        ) {
            push_background_cover_refs(
                targets,
                &mut seen,
                &mut remaining,
                library
                    .favorites
                    .iter()
                    .filter_map(|track| track.image_ref.as_ref()),
                fetch_size,
                size,
            );
        }
    }
}
fn push_background_cover_refs<'a>(
    targets: &mut Vec<CoverWarmTarget>,
    seen: &mut HashSet<String>,
    remaining: &mut usize,
    image_refs: impl IntoIterator<Item = &'a ImageRef>,
    fetch_size: u32,
    size: i32,
) {
    for image_ref in image_refs {
        push_background_cover_target(targets, seen, remaining, image_ref, fetch_size, size);
        if *remaining == 0 {
            break;
        }
    }
}

fn push_background_cover_ref_values(
    targets: &mut Vec<CoverWarmTarget>,
    seen: &mut HashSet<String>,
    remaining: &mut usize,
    image_refs: impl IntoIterator<Item = ImageRef>,
    fetch_size: u32,
    size: i32,
) {
    for image_ref in image_refs {
        push_background_cover_target(targets, seen, remaining, &image_ref, fetch_size, size);
        if *remaining == 0 {
            break;
        }
    }
}
fn push_background_cover_target(
    targets: &mut Vec<CoverWarmTarget>,
    seen: &mut HashSet<String>,
    remaining: &mut usize,
    image_ref: &ImageRef,
    fetch_size: u32,
    size: i32,
) {
    if *remaining == 0 {
        return;
    }
    if !seen.insert(background_warm_key(image_ref)) {
        return;
    }
    targets.push(CoverWarmTarget {
        image_ref: image_ref.clone(),
        fetch_size,
        size,
    });
    *remaining = (*remaining).saturating_sub(1);
}
fn background_warm_key(image_ref: &ImageRef) -> String {
    format!(
        "{}\u{1f}{}",
        image_ref.item_id,
        image_ref.tag.as_deref().unwrap_or(IMAGE_TAG_UNTAGGED),
    )
}
fn source_route_cover_size(
    key: LibraryListKey,
    settings: &LibraryListSettings,
    route_metrics: InitialRouteCoverMetrics,
) -> Option<(u32, i32)> {
    match settings.layout {
        LibraryLayout::Grid => Some((
            GRID_COVER_SIZE,
            route_metrics.collection_grid_metrics(key, settings).1,
        )),
        LibraryLayout::Detail => Some((GRID_COVER_SIZE, GRID_COVER_SIZE as i32)),
        LibraryLayout::Row if row_layout_uses_cover(settings) => Some((THUMB_COVER_SIZE, 48)),
        LibraryLayout::Row => None,
    }
}
fn source_collection_route_cover_size(settings: &LibraryListSettings) -> Option<(u32, i32)> {
    match settings.layout {
        LibraryLayout::Grid | LibraryLayout::Detail => {
            Some((THUMB_COVER_SIZE, THUMB_COVER_SIZE as i32))
        }
        LibraryLayout::Row if row_layout_uses_cover(settings) => Some((THUMB_COVER_SIZE, 48)),
        LibraryLayout::Row => None,
    }
}
fn dedupe_warm_targets(targets: &mut Vec<CoverWarmTarget>, server_id: &ServerId) {
    let mut positions = HashMap::<String, usize>::new();
    let mut deduped = Vec::<CoverWarmTarget>::new();
    for target in targets.drain(..) {
        let key = warm_dedupe_key(server_id, &target.image_ref);
        if let Some(index) = positions.get(&key).copied() {
            let existing = &mut deduped[index];
            let existing_decode_size = cover_decode_size(existing.size, existing.fetch_size);
            let target_decode_size = cover_decode_size(target.size, target.fetch_size);
            if (target.fetch_size, target_decode_size) > (existing.fetch_size, existing_decode_size)
            {
                existing.fetch_size = target.fetch_size;
                existing.size = target.size;
            }
            continue;
        }
        positions.insert(key, deduped.len());
        deduped.push(target);
    }
    *targets = deduped;
}
fn warm_dedupe_key(server_id: &ServerId, image_ref: &ImageRef) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}",
        server_id.as_str(),
        image_ref.item_id,
        image_ref.tag.as_deref().unwrap_or(IMAGE_TAG_UNTAGGED),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::root::shell_tests::{
        test_album, test_image_ref, test_initial_route_metrics, test_library_snapshot,
        test_playlist, test_queue_entry, test_server, test_smart_playlist, test_track,
    };
    use domain::{GenreId, RepeatMode, ShuffleState};

    #[test]
    fn startup_home_targets_ignore_route_sources() {
        let mut library = test_library_snapshot();
        let home_ref = test_image_ref("home");
        let mut home_album = test_album("Home Artist", Some(ArtistId::fake(90)));
        home_album.image_ref = Some(home_ref.clone());
        library.home_sections = vec![HomeSection {
            kind: HomeSectionKind::Explore,
            albums: vec![home_album],
            tracks: Vec::new(),
        }];

        let first_track_ref = test_image_ref("track-a");
        let mut first_track = test_track("Route Artist", Some(ArtistId::fake(1)));
        first_track.title = "A route track".to_string();
        first_track.image_ref = Some(first_track_ref.clone());
        let mut second_track = test_track("Route Artist", Some(ArtistId::fake(1)));
        second_track.id = TrackId::fake(2);
        second_track.title = "B route track".to_string();
        second_track.image_ref = Some(test_image_ref("track-b"));
        library.tracks = vec![second_track, first_track];

        let first_album_ref = test_image_ref("album-a");
        let mut first_album = test_album("Route Artist", Some(ArtistId::fake(2)));
        first_album.title = "A route album".to_string();
        first_album.image_ref = Some(first_album_ref.clone());
        let mut second_album = test_album("Route Artist", Some(ArtistId::fake(2)));
        second_album.id = AlbumId::fake(2);
        second_album.title = "B route album".to_string();
        second_album.image_ref = Some(test_image_ref("album-b"));
        library.albums = vec![second_album, first_album];

        let settings = AppSettings {
            home_blocks: vec![HomeBlockKind::Explore],
            ..Default::default()
        };
        let targets = startup_cover_targets(&library, &settings, 0);
        let target_refs = targets
            .iter()
            .map(|target| target.image_ref.item_id.as_str())
            .collect::<Vec<_>>();

        assert!(target_refs.contains(&home_ref.item_id.as_str()));
        assert!(!target_refs.contains(&first_track_ref.item_id.as_str()));
        assert!(!target_refs.contains(&first_album_ref.item_id.as_str()));

        let home_targets = startup_prime_targets(&library, &settings, 0);
        let home_target_refs = home_targets
            .iter()
            .map(|target| target.image_ref.item_id.as_str())
            .collect::<Vec<_>>();
        assert!(home_target_refs.contains(&home_ref.item_id.as_str()));
        assert!(!home_target_refs.contains(&first_track_ref.item_id.as_str()));
        assert!(!home_target_refs.contains(&first_album_ref.item_id.as_str()));
    }

    #[test]
    fn startup_playback_cover_target() {
        let playback_ref = test_image_ref("playback");
        let mut targets = Vec::new();
        let player = PlaybackSnapshot {
            current: Some(test_queue_entry("Now", playback_ref.clone())),
            ..PlaybackSnapshot::default()
        };

        push_startup_playback_targets(&mut targets, &player);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].image_ref, playback_ref);
        assert_eq!(targets[0].fetch_size, THUMB_COVER_SIZE);
        assert_eq!(targets[0].size, player::BOTTOM_PLAYER_COVER_SIZE);
    }

    #[test]
    fn startup_queue_cover_targets() {
        let first_ref = test_image_ref("queue-first");
        let second_ref = test_image_ref("queue-second");
        let current_ref = test_image_ref("queue-current");
        let skipped_ref = test_image_ref("queue-0");
        let queue = QueueSnapshot {
            server_id: ServerId::new("server:active"),
            entries: vec![
                test_queue_entry("Visible Song", first_ref.clone()),
                test_queue_entry("Hidden Song", second_ref.clone()),
            ],
            current_index: Some(0),
            repeat_mode: RepeatMode::Off,
            shuffle: ShuffleState::default(),
            shuffle_order: Vec::new(),
            progress_seconds: 0,
        };
        let mut targets = Vec::new();

        push_startup_queue_targets(
            &mut targets,
            Some(&queue),
            "visible",
            true,
            false,
            720,
            Some(&ServerId::new("server:active")),
        );

        let target_refs = targets
            .iter()
            .map(|target| target.image_ref.item_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(target_refs, vec![first_ref.item_id.as_str()]);

        targets.clear();
        push_startup_queue_targets(
            &mut targets,
            Some(&queue),
            "",
            false,
            false,
            720,
            Some(&ServerId::new("server:active")),
        );
        assert!(targets.is_empty());

        push_startup_queue_targets(
            &mut targets,
            Some(&queue),
            "",
            true,
            false,
            720,
            Some(&ServerId::new("server:stale")),
        );
        assert!(targets.is_empty());

        let entries = (0..12)
            .map(|index| {
                let image_ref = if index == 8 {
                    current_ref.clone()
                } else if index == 0 {
                    skipped_ref.clone()
                } else {
                    test_image_ref(&format!("queue-{index}"))
                };
                test_queue_entry(&format!("Track {index}"), image_ref)
            })
            .collect::<Vec<_>>();
        let current_queue = QueueSnapshot {
            server_id: ServerId::new("server:active"),
            entries,
            current_index: Some(8),
            repeat_mode: RepeatMode::Off,
            shuffle: ShuffleState::default(),
            shuffle_order: Vec::new(),
            progress_seconds: 0,
        };
        push_startup_queue_targets(
            &mut targets,
            Some(&current_queue),
            "",
            true,
            false,
            280,
            Some(&ServerId::new("server:active")),
        );

        let target_refs = targets
            .iter()
            .map(|target| target.image_ref.item_id.as_str())
            .collect::<Vec<_>>();
        assert!(target_refs.contains(&current_ref.item_id.as_str()));
        assert!(!target_refs.contains(&skipped_ref.item_id.as_str()));

        targets.clear();
        let tall_first_ref = test_image_ref("queue-0");
        let tall_entries = (0..30)
            .map(|index| {
                let image_ref = if index == 0 {
                    tall_first_ref.clone()
                } else {
                    test_image_ref(&format!("queue-{index}"))
                };
                test_queue_entry(&format!("Track {index}"), image_ref)
            })
            .collect::<Vec<_>>();
        let tall_queue = QueueSnapshot {
            server_id: ServerId::new("server:active"),
            entries: tall_entries,
            current_index: Some(8),
            repeat_mode: RepeatMode::Off,
            shuffle: ShuffleState::default(),
            shuffle_order: Vec::new(),
            progress_seconds: 0,
        };
        push_startup_queue_targets(
            &mut targets,
            Some(&tall_queue),
            "",
            true,
            false,
            1028,
            Some(&ServerId::new("server:active")),
        );
        let target_refs = targets
            .iter()
            .map(|target| target.image_ref.item_id.as_str())
            .collect::<Vec<_>>();
        assert!(target_refs.contains(&tall_first_ref.item_id.as_str()));
    }

    #[test]
    fn startup_queue_height_falls_back_before_allocation() {
        assert_eq!(startup_queue_prime_height(0, 720, Some(640)), 720);
        assert_eq!(startup_queue_prime_height(0, 0, Some(640)), 640);
        assert_eq!(startup_queue_prime_height(0, 0, None), 900);
    }

    #[test]
    fn source_warm_includes_route_matrix_once() {
        let mut library = test_library_snapshot();
        library.server = Some(test_server("source"));
        let first_track_ref = test_image_ref("track-a");
        let mut first_track = test_track("Route Artist", Some(ArtistId::fake(1)));
        first_track.title = "A route track".to_string();
        first_track.image_ref = Some(first_track_ref.clone());
        let mut second_track = test_track("Route Artist", Some(ArtistId::fake(1)));
        second_track.id = TrackId::fake(2);
        second_track.title = "B route track".to_string();
        second_track.image_ref = Some(test_image_ref("track-b"));
        library.tracks = vec![second_track, first_track];

        let first_album_ref = test_image_ref("album-a");
        let mut first_album = test_album("Route Artist", Some(ArtistId::fake(2)));
        first_album.title = "A route album".to_string();
        first_album.image_ref = Some(first_album_ref.clone());
        let mut second_album = test_album("Route Artist", Some(ArtistId::fake(2)));
        second_album.id = AlbumId::fake(2);
        second_album.title = "B route album".to_string();
        second_album.image_ref = Some(test_image_ref("album-b"));
        library.albums = vec![second_album, first_album];

        let settings = AppSettings::default();
        let targets = source_warm_targets(&library, &[], &settings, test_initial_route_metrics());
        let target_refs = targets
            .iter()
            .map(|target| target.image_ref.item_id.as_str())
            .collect::<Vec<_>>();

        assert!(target_refs.contains(&first_track_ref.item_id.as_str()));
        assert!(target_refs.contains(&first_album_ref.item_id.as_str()));
        assert_eq!(
            target_refs
                .iter()
                .filter(|item_id| **item_id == first_track_ref.item_id)
                .count(),
            1
        );
        assert!(
            target_refs
                .iter()
                .position(|item_id| *item_id == first_track_ref.item_id)
                < target_refs
                    .iter()
                    .position(|item_id| *item_id == first_album_ref.item_id)
        );
    }

    #[test]
    fn source_warm_includes_group_refs() {
        let shared = test_image_ref("shared-art");
        let genre_only = test_image_ref("genre-only");
        let mut library = test_library_snapshot();
        library.server = Some(test_server("source"));
        let mut track = test_track("Route Artist", Some(ArtistId::fake(1)));
        track.image_ref = Some(shared.clone());
        library.tracks = vec![track];
        library.genres = vec![Genre {
            id: GenreId::fake(1),
            name: "Genre".to_string(),
            album_count: 1,
            track_count: 1,
            duration_seconds: 180,
            image_refs: vec![shared.clone(), genre_only.clone()],
            image_ref: Some(shared.clone()),
        }];

        let settings = AppSettings::default();
        let targets = source_warm_targets(&library, &[], &settings, test_initial_route_metrics());

        assert_eq!(
            targets
                .iter()
                .filter(|target| target.image_ref.item_id == shared.item_id)
                .count(),
            1
        );
        assert!(
            targets
                .iter()
                .any(|target| target.image_ref.item_id == genre_only.item_id)
        );
    }

    #[test]
    fn source_warm_includes_playlists() {
        let playlist_ref = test_image_ref("playlist-group");
        let smart_ref = test_image_ref("smart-group");
        let mut library = test_library_snapshot();
        library.server = Some(test_server("source"));
        library.playlists = vec![test_playlist("Regular", playlist_ref.clone())];
        let smart_playlists = vec![test_smart_playlist("Smart", smart_ref.clone())];

        let settings = AppSettings::default();
        let targets = source_warm_targets(
            &library,
            &smart_playlists,
            &settings,
            test_initial_route_metrics(),
        );
        let target_refs = targets
            .iter()
            .map(|target| target.image_ref.item_id.as_str())
            .collect::<Vec<_>>();

        assert!(target_refs.contains(&playlist_ref.item_id.as_str()));
        assert!(target_refs.contains(&smart_ref.item_id.as_str()));
    }

    #[test]
    fn source_warm_includes_background_refs() {
        let mut library = test_library_snapshot();
        library.server = Some(test_server("source"));
        let background_ref = test_image_ref("background-album");
        library.albums = (0..24)
            .map(|index| {
                let mut album = test_album("Route Artist", Some(ArtistId::fake(index + 1)));
                album.id = AlbumId::fake(index + 1);
                album.title = format!("Album {index:02}");
                album.image_ref = Some(if index == 23 {
                    background_ref.clone()
                } else {
                    test_image_ref(&format!("album-{index:02}"))
                });
                album
            })
            .collect();

        let targets = source_warm_targets(
            &library,
            &[],
            &AppSettings::default(),
            test_initial_route_metrics(),
        );

        assert!(
            targets
                .iter()
                .any(|target| target.image_ref.item_id == background_ref.item_id)
        );
    }

    #[test]
    fn album_grid_warm_uses_album_metrics() {
        let metrics = test_initial_route_metrics();
        let album_settings = LibraryListSettings {
            layout: LibraryLayout::Grid,
            ..LibraryListSettings::for_key(LibraryListKey::Albums)
        };
        let track_settings = LibraryListSettings {
            layout: LibraryLayout::Grid,
            ..LibraryListSettings::for_key(LibraryListKey::Tracks)
        };

        assert_eq!(
            source_route_cover_size(LibraryListKey::Albums, &album_settings, metrics),
            Some((GRID_COVER_SIZE, metrics.album_grid_card_size))
        );
        assert_eq!(
            source_route_cover_size(LibraryListKey::Tracks, &track_settings, metrics),
            Some((GRID_COVER_SIZE, metrics.grid_card_size))
        );
        assert_ne!(metrics.album_grid_card_size, metrics.grid_card_size);
    }

    #[test]
    fn source_warm_skips_hidden_routes() {
        let genre_ref = test_image_ref("hidden-genre");
        let playlist_ref = test_image_ref("hidden-playlist");
        let mut library = test_library_snapshot();
        library.server = Some(test_server("source"));
        library.genres = vec![Genre {
            id: GenreId::fake(1),
            name: "Genre".to_string(),
            album_count: 1,
            track_count: 1,
            duration_seconds: 180,
            image_refs: vec![genre_ref.clone()],
            image_ref: None,
        }];
        library.playlists = vec![test_playlist("Regular", playlist_ref.clone())];
        let mut settings = AppSettings::default();
        for entry in &mut settings.sidebar.route_items {
            if matches!(
                entry.item,
                SidebarRouteItem::Genres | SidebarRouteItem::Playlists
            ) {
                entry.visible = false;
            }
        }

        let targets = source_warm_targets(&library, &[], &settings, test_initial_route_metrics());
        let target_refs = targets
            .iter()
            .map(|target| target.image_ref.item_id.as_str())
            .collect::<Vec<_>>();

        assert!(!target_refs.contains(&genre_ref.item_id.as_str()));
        assert!(!target_refs.contains(&playlist_ref.item_id.as_str()));
    }
}
