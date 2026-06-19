use super::*;

mod cache_lookup;
mod decode_queue;
mod size_helpers;
mod targets;
mod tiles;
mod warming;

use size_helpers::*;
use targets::startup_home_cover_prime_targets;
pub(in crate::ui) use targets::{
    InitialRouteCoverMetrics, row_layout_uses_cover, sidebar_route_visible,
};
pub(in crate::ui) use tiles::{cover_artwork_id_for_key, cover_request_id_for_key};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::ui::root::cover) struct CoverWarmTarget {
    pub(in crate::ui::root::cover) image_ref: ImageRef,
    pub(in crate::ui::root::cover) fetch_size: u32,
    pub(in crate::ui::root::cover) size: i32,
}

struct VisibleCoverRef {
    image_ref: ImageRef,
    fetch_size: u32,
    size: i32,
}

struct VisibleCoverWindow {
    refs: Vec<VisibleCoverRef>,
}

impl Shell {
    pub(in crate::ui::root) fn prime_route_visible_cover_window(
        self: &Rc<Self>,
        route: &Route,
    ) -> usize {
        let window = visible_cover_window(self, route);
        self.prime_visible_cover_window(window)
    }

    fn prime_visible_cover_window(self: &Rc<Self>, window: VisibleCoverWindow) -> usize {
        let mut groups = HashMap::<(u32, i32), Vec<ImageRef>>::new();
        for cover_ref in window.refs {
            groups
                .entry((cover_ref.fetch_size, cover_ref.size))
                .or_default()
                .push(cover_ref.image_ref);
        }
        let mut refs = 0_usize;
        for ((fetch_size, size), image_refs) in groups {
            refs = refs.saturating_add(image_refs.len());
            self.prime_cover_refs_now(image_refs, fetch_size, size);
        }
        refs
    }
}

pub(in crate::ui::root::cover) fn route_visible_cover_targets(
    shell: &Shell,
    route: &Route,
) -> Vec<CoverWarmTarget> {
    visible_cover_window(shell, route)
        .refs
        .into_iter()
        .map(|cover_ref| CoverWarmTarget {
            image_ref: cover_ref.image_ref,
            fetch_size: cover_ref.fetch_size,
            size: cover_ref.size,
        })
        .collect()
}

fn visible_cover_window(shell: &Shell, route: &Route) -> VisibleCoverWindow {
    match route {
        Route::Home => home_visible_cover_window(shell),
        Route::Favorites => track_visible_cover_window(shell, LibraryListKey::FavoriteTracks, true),
        Route::Tracks => track_visible_cover_window(shell, LibraryListKey::Tracks, false),
        Route::Albums => album_visible_cover_window(shell),
        Route::Artists => artist_visible_cover_window(shell, false),
        Route::AlbumArtists => artist_visible_cover_window(shell, true),
        Route::Genres => genre_visible_cover_window(shell),
        Route::Playlists => playlist_visible_cover_window(shell),
        Route::SmartPlaylists => smart_playlist_visible_cover_window(shell),
        _ => VisibleCoverWindow { refs: Vec::new() },
    }
}

fn home_visible_cover_window(shell: &Shell) -> VisibleCoverWindow {
    let refs = startup_home_cover_prime_targets(shell)
        .into_iter()
        .map(|target| VisibleCoverRef {
            image_ref: target.image_ref,
            fetch_size: target.fetch_size,
            size: target.size,
        })
        .collect::<Vec<_>>();
    VisibleCoverWindow { refs }
}

fn track_visible_cover_window(
    shell: &Shell,
    key: LibraryListKey,
    favorite_first: bool,
) -> VisibleCoverWindow {
    let settings = shell.library_settings(key);
    let Some((fetch_size, size)) = cover_prime_sizes(shell, key, &settings) else {
        return VisibleCoverWindow { refs: Vec::new() };
    };
    if matches!(key, LibraryListKey::Tracks | LibraryListKey::FavoriteTracks) {
        let route_refs = shell.state.route_track_refs.borrow();
        if !route_refs.is_empty() {
            return track_visible_cover_window_for_refs(
                shell,
                &route_refs,
                key,
                &settings,
                fetch_size,
                size,
            );
        }
    }
    let mut tracks = match key {
        LibraryListKey::FavoriteTracks => shell.state.library.borrow().favorites.clone(),
        _ => shell.state.library.borrow().tracks.clone(),
    };
    library::sort_tracks(&mut tracks, &settings, favorite_first);
    track_visible_cover_window_for_tracks(shell, &tracks, key, &settings, fetch_size, size)
}

fn track_visible_cover_window_for_refs(
    shell: &Shell,
    refs: &[Option<ImageRef>],
    key: LibraryListKey,
    settings: &LibraryListSettings,
    fetch_size: u32,
    size: i32,
) -> VisibleCoverWindow {
    let (visible_start, visible_end) = visible_index_range(shell, refs.len(), key, settings);
    let refs = refs[visible_start..visible_end]
        .iter()
        .filter_map(|image_ref| image_ref.clone())
        .map(|image_ref| VisibleCoverRef {
            image_ref,
            fetch_size,
            size,
        })
        .collect::<Vec<_>>();
    VisibleCoverWindow { refs }
}

fn track_visible_cover_window_for_tracks(
    shell: &Shell,
    tracks: &[Track],
    key: LibraryListKey,
    settings: &LibraryListSettings,
    fetch_size: u32,
    size: i32,
) -> VisibleCoverWindow {
    let (visible_start, visible_end) = visible_index_range(shell, tracks.len(), key, settings);
    let visible_tracks = &tracks[visible_start..visible_end];
    let refs = visible_tracks
        .iter()
        .filter_map(|track| track.image_ref.clone())
        .map(|image_ref| VisibleCoverRef {
            image_ref,
            fetch_size,
            size,
        })
        .collect::<Vec<_>>();
    VisibleCoverWindow { refs }
}

fn album_visible_cover_window(shell: &Shell) -> VisibleCoverWindow {
    let settings = shell.library_settings(LibraryListKey::Albums);
    let Some((fetch_size, size)) = cover_prime_sizes(shell, LibraryListKey::Albums, &settings)
    else {
        return VisibleCoverWindow { refs: Vec::new() };
    };
    let mut albums = shell.state.library.borrow().albums.clone();
    library::sort_albums(&mut albums, &settings);
    let (visible_start, visible_end) =
        visible_index_range(shell, albums.len(), LibraryListKey::Albums, &settings);
    let visible_albums = &albums[visible_start..visible_end];
    let refs = visible_albums
        .iter()
        .filter_map(|album| album.image_ref.clone())
        .map(|image_ref| VisibleCoverRef {
            image_ref,
            fetch_size,
            size,
        })
        .collect::<Vec<_>>();
    VisibleCoverWindow { refs }
}

fn artist_visible_cover_window(shell: &Shell, album_artist: bool) -> VisibleCoverWindow {
    let key = if album_artist {
        LibraryListKey::AlbumArtists
    } else {
        LibraryListKey::Artists
    };
    let settings = shell.library_settings(key);
    let Some((fetch_size, size)) = cover_prime_sizes(shell, key, &settings) else {
        return VisibleCoverWindow { refs: Vec::new() };
    };
    let mut artists = if album_artist {
        shell.state.library.borrow().album_artists.clone()
    } else {
        shell.state.library.borrow().artists.clone()
    };
    library::sort_artists(&mut artists, &settings);
    let (visible_start, visible_end) = visible_index_range(shell, artists.len(), key, &settings);
    let visible_artists = &artists[visible_start..visible_end];
    let refs = visible_artists
        .iter()
        .filter_map(|artist| artist.image_ref.clone())
        .map(|image_ref| VisibleCoverRef {
            image_ref,
            fetch_size,
            size,
        })
        .collect::<Vec<_>>();
    VisibleCoverWindow { refs }
}

fn genre_visible_cover_window(shell: &Shell) -> VisibleCoverWindow {
    let settings = shell.library_settings(LibraryListKey::Genres);
    let Some((fetch_size, size)) = collection_cover_prime_sizes(&settings) else {
        return VisibleCoverWindow { refs: Vec::new() };
    };
    let library = shell.state.library.borrow();
    let mut genres = library.genres.clone();
    library::sort_genres(&mut genres, &settings);
    let (visible_start, visible_end) =
        visible_index_range(shell, genres.len(), LibraryListKey::Genres, &settings);
    let mut image_refs = Vec::new();
    for genre in &genres[visible_start..visible_end] {
        image_refs.extend(crate::cover_art_policy::selected_genre_artwork(genre).image_refs);
    }
    let refs = visible_cover_refs(image_refs, fetch_size, size);
    VisibleCoverWindow { refs }
}

fn playlist_visible_cover_window(shell: &Shell) -> VisibleCoverWindow {
    let settings = shell.library_settings(LibraryListKey::Playlists);
    let Some((fetch_size, size)) = collection_cover_prime_sizes(&settings) else {
        return VisibleCoverWindow { refs: Vec::new() };
    };
    let mut playlists = shell.state.library.borrow().playlists.clone();
    library::sort_playlists(&mut playlists, &settings);
    let (visible_start, visible_end) =
        visible_index_range(shell, playlists.len(), LibraryListKey::Playlists, &settings);
    let visible_playlists = &playlists[visible_start..visible_end];
    let mut image_refs = Vec::new();
    for playlist in visible_playlists {
        image_refs.extend(crate::cover_art_policy::selected_collection_refs(
            &playlist.image_refs,
            None,
            false,
        ));
    }
    let refs = visible_cover_refs(image_refs, fetch_size, size);
    VisibleCoverWindow { refs }
}

fn smart_playlist_visible_cover_window(shell: &Shell) -> VisibleCoverWindow {
    let settings = shell.library_settings(LibraryListKey::SmartPlaylists);
    let Some((fetch_size, size)) = collection_cover_prime_sizes(&settings) else {
        return VisibleCoverWindow { refs: Vec::new() };
    };
    let mut playlists = shell.state.smart_playlists.borrow().clone();
    library::sort_smart_playlists(&mut playlists, &settings);
    let (visible_start, visible_end) = visible_index_range(
        shell,
        playlists.len(),
        LibraryListKey::SmartPlaylists,
        &settings,
    );
    let visible_playlists = &playlists[visible_start..visible_end];
    let mut image_refs = Vec::new();
    for playlist in visible_playlists {
        image_refs
            .extend(crate::cover_art_policy::selected_smart_playlist_artwork(playlist).image_refs);
    }
    let refs = visible_cover_refs(image_refs, fetch_size, size);
    VisibleCoverWindow { refs }
}

fn visible_cover_refs(
    image_refs: Vec<ImageRef>,
    fetch_size: u32,
    size: i32,
) -> Vec<VisibleCoverRef> {
    image_refs
        .into_iter()
        .map(|image_ref| VisibleCoverRef {
            image_ref,
            fetch_size,
            size,
        })
        .collect()
}

fn collection_cover_prime_sizes(settings: &LibraryListSettings) -> Option<(u32, i32)> {
    match settings.layout {
        LibraryLayout::Grid | LibraryLayout::Detail => {
            Some((THUMB_COVER_SIZE, THUMB_COVER_SIZE as i32))
        }
        LibraryLayout::Row if row_layout_uses_cover(settings) => Some((THUMB_COVER_SIZE, 48)),
        LibraryLayout::Row => None,
    }
}

#[derive(Clone)]
pub(in crate::ui) struct CoverBinding {
    pub(in crate::ui) tile: ArtworkTileWeak,
    pub(in crate::ui) generation: u64,
    pub(in crate::ui) clear_on_failure: bool,
}

#[derive(Clone)]
pub(in crate::ui) struct DecodedCover {
    pub(in crate::ui) pixbuf: Pixbuf,
    pub(in crate::ui) size: i32,
    pub(in crate::ui) bytes: usize,
    pub(in crate::ui) last_used: u64,
    pub(in crate::ui) priority: CoverDecodePriority,
}

pub(in crate::ui) struct DecodedCoverOrderEntry {
    pub(in crate::ui) key: String,
    pub(in crate::ui) last_used: u64,
}

pub(in crate::ui) struct CoverDecodeJob {
    pub(in crate::ui) key: String,
    pub(in crate::ui) path: PathBuf,
    pub(in crate::ui) size: i32,
    pub(in crate::ui) priority: CoverDecodePriority,
    pub(in crate::ui) requires_live_binding: bool,
}

pub(in crate::ui::root::cover) struct CoverWarmJob {
    pub(in crate::ui::root::cover) key: String,
    pub(in crate::ui::root::cover) image_ref: ImageRef,
    pub(in crate::ui::root::cover) fetch_size: u32,
    pub(in crate::ui::root::cover) size: i32,
}

pub(in crate::ui::root) struct CoverWorkStats {
    pub(in crate::ui::root) prime_pending: usize,
    pub(in crate::ui::root) path_lookups: usize,
    pub(in crate::ui::root) fetches: usize,
    pub(in crate::ui::root) visible_requests: usize,
    pub(in crate::ui::root) bindings: usize,
    pub(in crate::ui::root) decode_queue: usize,
    pub(in crate::ui::root) decodes: usize,
    pub(in crate::ui::root) decoded: usize,
    pub(in crate::ui::root) warm_pending: bool,
    pub(in crate::ui::root) warm_started: bool,
}

#[derive(Clone)]
pub(in crate::ui::root::cover) struct CoverPathLookupRequest {
    pub(in crate::ui::root::cover) key: String,
    pub(in crate::ui::root::cover) image_ref: ImageRef,
    pub(in crate::ui::root::cover) fetch_size: u32,
    pub(in crate::ui::root::cover) size: i32,
    pub(in crate::ui::root::cover) intent: CoverPathLookupIntent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui::root::cover) enum CoverRequestState {
    PathLookup,
    Fetching,
    Decoding,
    Deferred,
    Ready,
    FinalMissing,
}

pub(in crate::ui::root) struct CoverVisibleRequests {
    entries: RefCell<HashMap<String, CoverRequestRecord>>,
}

#[derive(Clone)]
struct CoverRequestRecord {
    request: CoverPathLookupRequest,
    state: CoverRequestState,
    decode_failures: u8,
}

impl CoverVisibleRequests {
    pub(in crate::ui::root) fn new() -> Self {
        Self {
            entries: RefCell::new(HashMap::new()),
        }
    }

    pub(in crate::ui::root::cover) fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    pub(in crate::ui::root::cover) fn clear(&self) {
        self.entries.borrow_mut().clear();
    }

    pub(in crate::ui::root::cover) fn record(&self, request: CoverPathLookupRequest) {
        let mut entries = self.entries.borrow_mut();
        if let Some(existing) = entries.get_mut(&request.key) {
            existing.merge_request(request);
        } else {
            entries.insert(request.key.clone(), CoverRequestRecord::new(request));
        }
    }

    pub(in crate::ui::root::cover) fn remove(&self, key: &str) {
        self.entries.borrow_mut().remove(key);
    }

    pub(in crate::ui::root::cover) fn mark(&self, key: &str, state: CoverRequestState) {
        if let Some(record) = self.entries.borrow_mut().get_mut(key) {
            record.state = state;
        }
    }

    pub(in crate::ui::root::cover) fn request(&self, key: &str) -> Option<CoverPathLookupRequest> {
        self.entries
            .borrow()
            .get(key)
            .map(|record| record.request.clone())
    }

    pub(in crate::ui::root::cover) fn retry_after_decode_failure(
        &self,
        key: &str,
    ) -> Option<CoverPathLookupRequest> {
        self.entries.borrow_mut().get_mut(key).and_then(|record| {
            if record.decode_failures > 0 {
                record.state = CoverRequestState::FinalMissing;
                return None;
            }
            record.decode_failures = record.decode_failures.saturating_add(1);
            record.state = CoverRequestState::PathLookup;
            Some(record.request.clone())
        })
    }
}

impl CoverRequestRecord {
    fn new(request: CoverPathLookupRequest) -> Self {
        Self {
            request,
            state: CoverRequestState::PathLookup,
            decode_failures: 0,
        }
    }

    fn merge_request(&mut self, request: CoverPathLookupRequest) {
        self.request.intent = self.request.intent.coalesce(request.intent);
        self.request.size = self.request.size.max(request.size);
        if request.fetch_size > self.request.fetch_size {
            self.request.fetch_size = request.fetch_size;
            self.request.image_ref = request.image_ref;
        }
        self.state = CoverRequestState::PathLookup;
    }
}

pub(in crate::ui::root) struct CoverPathLookups {
    entries: RefCell<HashMap<String, CoverPathLookupIntent>>,
}

impl CoverPathLookups {
    pub(in crate::ui::root) fn new() -> Self {
        Self {
            entries: RefCell::new(HashMap::new()),
        }
    }

    pub(in crate::ui::root) fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    pub(in crate::ui::root::cover) fn clear(&self) {
        self.entries.borrow_mut().clear();
    }

    pub(in crate::ui::root::cover) fn record(
        &self,
        key: String,
        intent: CoverPathLookupIntent,
    ) -> bool {
        let mut entries = self.entries.borrow_mut();
        if let Some(existing) = entries.get_mut(&key) {
            *existing = existing.coalesce(intent);
            false
        } else {
            entries.insert(key, intent);
            true
        }
    }

    pub(in crate::ui::root::cover) fn remove(&self, key: &str) -> Option<CoverPathLookupIntent> {
        self.entries.borrow_mut().remove(key)
    }

    pub(in crate::ui::root::cover) fn contains_key(&self, key: &str) -> bool {
        self.entries.borrow().contains_key(key)
    }

    fn retain_current_priority(&self, keep: &HashSet<String>) {
        self.entries.borrow_mut().retain(|key, intent| {
            !matches!(
                intent,
                CoverPathLookupIntent::Priority | CoverPathLookupIntent::StartupPrime
            ) || keep.contains(key)
        });
    }

    fn retain_warm(&self) {
        self.entries
            .borrow_mut()
            .retain(|_, intent| *intent == CoverPathLookupIntent::Warm);
    }

    #[cfg(test)]
    fn contains_intent(&self, key: &str, intent: CoverPathLookupIntent) -> bool {
        self.entries.borrow().get(key) == Some(&intent)
    }

    #[cfg(test)]
    fn snapshot(&self) -> HashMap<String, CoverPathLookupIntent> {
        self.entries.borrow().clone()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui::root::cover) enum CoverPathLookupIntent {
    Visible,
    Priority,
    StartupPrime,
    Warm,
}

impl CoverPathLookupIntent {
    fn coalesce(self, next: Self) -> Self {
        match (self, next) {
            (Self::Visible, _) | (_, Self::Visible) => Self::Visible,
            (Self::Priority, _) | (_, Self::Priority) => Self::Priority,
            (Self::StartupPrime, _) | (_, Self::StartupPrime) => Self::StartupPrime,
            _ => Self::Warm,
        }
    }
}

struct FirstRunCoverPrimeJob {
    key: String,
    image_ref: ImageRef,
    fetch_size: u32,
    size: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum CoverDecodePriority {
    Visible,
    Warm,
}

impl CoverDecodePriority {
    pub(in crate::ui) fn glib_priority(self) -> glib::Priority {
        match self {
            Self::Visible => glib::Priority::DEFAULT_IDLE,
            Self::Warm => glib::Priority::LOW,
        }
    }
}

pub(in crate::ui::root::cover) fn retain_current_priority_cover_work(
    lookups: &CoverPathLookups,
    queue: &mut VecDeque<CoverDecodeJob>,
    keep: &HashSet<String>,
) {
    lookups.retain_current_priority(keep);
    queue.retain(|job| {
        job.priority != CoverDecodePriority::Visible
            || job.requires_live_binding
            || keep.contains(&job.key)
    });
}

pub(in crate::ui::root::cover) fn clear_queued_route_cover_work(
    lookups: &CoverPathLookups,
    queue: &mut VecDeque<CoverDecodeJob>,
) {
    lookups.retain_warm();
    queue.retain(|job| job.priority == CoverDecodePriority::Warm);
}

pub(in crate::ui) fn queue_cover_decode_job(
    queue: &mut VecDeque<CoverDecodeJob>,
    job: CoverDecodeJob,
) {
    if job.priority == CoverDecodePriority::Visible {
        let insertion_index = queue
            .iter()
            .position(|queued| queued.priority == CoverDecodePriority::Warm)
            .unwrap_or(queue.len());
        queue.insert(insertion_index, job);
    } else {
        queue.push_back(job);
    }
}

pub(in crate::ui) fn cover_decode_priority_for_interaction(
    priority: CoverDecodePriority,
    requires_live_binding: bool,
    interaction_paused: bool,
) -> CoverDecodePriority {
    if interaction_paused && priority == CoverDecodePriority::Visible && !requires_live_binding {
        CoverDecodePriority::Warm
    } else {
        priority
    }
}

pub(in crate::ui) fn visible_cover_decode_startable(
    job: &CoverDecodeJob,
    visible_paused: bool,
) -> bool {
    job.priority == CoverDecodePriority::Visible && !visible_paused
}

pub(in crate::ui) fn cover_decode_has_capacity(
    active: &HashMap<String, CoverDecodePriority>,
    priority: CoverDecodePriority,
) -> bool {
    match priority {
        CoverDecodePriority::Visible => {
            active
                .values()
                .filter(|active_priority| **active_priority == CoverDecodePriority::Visible)
                .count()
                < COVER_DECODE_LIMIT
        }
        CoverDecodePriority::Warm => active.len() < COVER_DECODE_MAX_IN_FLIGHT,
    }
}

#[cfg(test)]
mod priority_work_tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn path_lookup_intent_coalesces() {
        let lookups = CoverPathLookups::new();

        assert!(lookups.record("album-art".to_string(), CoverPathLookupIntent::Warm));
        assert!(!lookups.record("album-art".to_string(), CoverPathLookupIntent::Visible));
        assert!(lookups.contains_intent("album-art", CoverPathLookupIntent::Visible));

        assert!(lookups.record("now-playing".to_string(), CoverPathLookupIntent::Visible));
        assert!(!lookups.record("now-playing".to_string(), CoverPathLookupIntent::Warm));
        assert!(lookups.contains_intent("now-playing", CoverPathLookupIntent::Visible));
    }

    #[test]
    fn visible_drop_backlog() {
        let lookups = CoverPathLookups::new();
        lookups.record("old-priority".to_string(), CoverPathLookupIntent::Priority);
        lookups.record(
            "current-priority".to_string(),
            CoverPathLookupIntent::Priority,
        );
        lookups.record("live-visible".to_string(), CoverPathLookupIntent::Visible);
        lookups.record("background-warm".to_string(), CoverPathLookupIntent::Warm);
        let mut queue = VecDeque::from([
            CoverDecodeJob {
                key: "old-priority".to_string(),
                path: PathBuf::from("/tmp/old-cover.jpg"),
                size: 96,
                priority: CoverDecodePriority::Visible,
                requires_live_binding: false,
            },
            CoverDecodeJob {
                key: "current-priority".to_string(),
                path: PathBuf::from("/tmp/current-cover.jpg"),
                size: 96,
                priority: CoverDecodePriority::Visible,
                requires_live_binding: false,
            },
            CoverDecodeJob {
                key: "live-visible".to_string(),
                path: PathBuf::from("/tmp/live-cover.jpg"),
                size: 96,
                priority: CoverDecodePriority::Visible,
                requires_live_binding: true,
            },
            CoverDecodeJob {
                key: "background-warm".to_string(),
                path: PathBuf::from("/tmp/warm-cover.jpg"),
                size: 96,
                priority: CoverDecodePriority::Warm,
                requires_live_binding: false,
            },
        ]);
        let keep = HashSet::from(["current-priority".to_string()]);

        retain_current_priority_cover_work(&lookups, &mut queue, &keep);

        assert!(!lookups.contains_key("old-priority"));
        assert!(lookups.contains_key("current-priority"));
        assert!(lookups.contains_key("live-visible"));
        assert!(lookups.contains_key("background-warm"));

        let queued_keys = queue.iter().map(|job| job.key.as_str()).collect::<Vec<_>>();
        assert_eq!(
            queued_keys,
            vec!["current-priority", "live-visible", "background-warm"]
        );
    }

    #[test]
    fn visible_warm_work() {
        let mut queue = VecDeque::from([decode_job("warm-old", CoverDecodePriority::Warm)]);

        queue_cover_decode_job(
            &mut queue,
            decode_job("visible-first", CoverDecodePriority::Visible),
        );
        queue_cover_decode_job(
            &mut queue,
            decode_job("visible-second", CoverDecodePriority::Visible),
        );
        queue_cover_decode_job(
            &mut queue,
            decode_job("warm-new", CoverDecodePriority::Warm),
        );

        let queued_keys = queue.iter().map(|job| job.key.as_str()).collect::<Vec<_>>();
        assert_eq!(
            queued_keys,
            vec!["visible-first", "visible-second", "warm-old", "warm-new"]
        );
    }

    #[test]
    fn visible_warm_lane() {
        let active = (0..COVER_DECODE_MAX_IN_FLIGHT)
            .map(|index| (format!("warm-{index}"), CoverDecodePriority::Warm))
            .collect::<HashMap<_, _>>();

        assert!(cover_decode_has_capacity(
            &active,
            CoverDecodePriority::Visible
        ));
        assert!(!cover_decode_has_capacity(
            &active,
            CoverDecodePriority::Warm
        ));
    }

    #[test]
    fn scroll_pause_defers_visible_decode_start() {
        assert_eq!(
            cover_decode_priority_for_interaction(CoverDecodePriority::Visible, false, true),
            CoverDecodePriority::Warm
        );
        assert_eq!(
            cover_decode_priority_for_interaction(CoverDecodePriority::Visible, true, true),
            CoverDecodePriority::Visible
        );

        let mut unbound = decode_job("scroll-prime", CoverDecodePriority::Visible);
        let mut live = decode_job("live-tile", CoverDecodePriority::Visible);
        live.requires_live_binding = true;

        assert!(!visible_cover_decode_startable(&unbound, true));
        assert!(!visible_cover_decode_startable(&live, true));
        assert!(visible_cover_decode_startable(&unbound, false));
        assert!(visible_cover_decode_startable(&live, false));

        unbound.priority =
            cover_decode_priority_for_interaction(CoverDecodePriority::Visible, false, true);
        assert!(!visible_cover_decode_startable(&unbound, false));
        live.priority =
            cover_decode_priority_for_interaction(CoverDecodePriority::Visible, true, true);
        assert!(visible_cover_decode_startable(&live, false));
    }

    #[test]
    fn route_warm_work() {
        let lookups = CoverPathLookups::new();
        lookups.record("old-visible".to_string(), CoverPathLookupIntent::Visible);
        lookups.record("old-priority".to_string(), CoverPathLookupIntent::Priority);
        lookups.record("background-warm".to_string(), CoverPathLookupIntent::Warm);
        let mut queue = VecDeque::from([
            decode_job("old-visible", CoverDecodePriority::Visible),
            decode_job("background-warm", CoverDecodePriority::Warm),
        ]);

        clear_queued_route_cover_work(&lookups, &mut queue);

        assert_eq!(
            lookups.snapshot(),
            HashMap::from([("background-warm".to_string(), CoverPathLookupIntent::Warm)])
        );
        let queued_keys = queue.iter().map(|job| job.key.as_str()).collect::<Vec<_>>();
        assert_eq!(queued_keys, vec!["background-warm"]);
    }

    fn decode_job(key: &str, priority: CoverDecodePriority) -> CoverDecodeJob {
        CoverDecodeJob {
            key: key.to_string(),
            path: PathBuf::from("/tmp/cached-cover.jpg"),
            size: 96,
            priority,
            requires_live_binding: false,
        }
    }
}
